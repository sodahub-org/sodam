//! 一次播放尝试的日志轨迹（Issue #59）。
//!
//! 一次「播这首歌」在日志里留下三类事件：开始时一行、每次失败的判定一行、最终成功一行。
//! 放弃不再单独补一行——最后一次失败行带 `decision=give_up` 与 `reason=`，本身就是
//! 「为什么放弃」的那一行。
//!
//! 字段顺序固定，便于 grep：
//!
//! ```text
//! INFO  player attempt_start  attempt=1 track_id=…
//! WARN  player attempt_failed attempt=1 track_id=… stage=… retryability=… action=… decision=retry    wait_ms=…
//! ERROR player attempt_failed attempt=1 track_id=… stage=… retryability=… action=… decision=give_up  reason=…
//! INFO  player attempt_ok     attempt=1 track_id=… elapsed_ms=…
//! ```

use crate::failure::{jitter_from_elapsed, PlayBudget, PlaybackFailure, Retryability};
use crate::log_outlet::{self, LogLevel, LogOutlet, LogTopic};
use std::time::{Duration, Instant};

/// 一次失败后的判定：重试（附等待时长）还是放弃（附原因）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Retry { wait: Duration },
    GiveUp { reason: &'static str },
}

/// 只按「这次失败值不值得再来一次」做判定，预算与分类规则都在 [`PlayBudget`] 里。
pub fn decide(
    budget: &PlayBudget,
    attempt: u8,
    failure: &PlaybackFailure,
    jitter: f32,
) -> RetryDecision {
    match budget.next_delay(attempt, failure, jitter) {
        Some(wait) => RetryDecision::Retry { wait },
        None => RetryDecision::GiveUp {
            reason: if failure.retryability == Retryability::Never {
                "non_retryable"
            } else {
                "budget_exhausted"
            },
        },
    }
}

/// 把一次播放尝试的事件写进诊断日志。
pub struct PlaybackTrace<'a> {
    outlet: &'a LogOutlet,
    track_id: &'a str,
}

impl<'a> PlaybackTrace<'a> {
    pub fn new(outlet: &'a LogOutlet, track_id: &'a str) -> Self {
        Self { outlet, track_id }
    }

    /// 使用全局日志出口；未初始化时返回 `None`，播放不受影响。
    pub fn global(track_id: &'a str) -> Option<Self> {
        log_outlet::global().map(|outlet| Self::new(outlet, track_id))
    }

    pub fn attempt_started(&self, attempt: u8) {
        let attempt = attempt.to_string();
        self.outlet.log(
            LogLevel::Info,
            LogTopic::Player,
            "attempt_start",
            &[("attempt", &attempt), ("track_id", self.track_id)],
        );
    }

    pub fn attempt_failed(&self, attempt: u8, failure: &PlaybackFailure, decision: RetryDecision) {
        let attempt = attempt.to_string();
        let wait_ms = match decision {
            RetryDecision::Retry { wait } => Some(wait.as_millis().to_string()),
            RetryDecision::GiveUp { .. } => None,
        };
        let reason = match decision {
            RetryDecision::GiveUp { reason } => Some(reason.to_string()),
            RetryDecision::Retry { .. } => None,
        };
        let level = match decision {
            RetryDecision::Retry { .. } => LogLevel::Warn,
            RetryDecision::GiveUp { .. } => LogLevel::Error,
        };

        // 字段顺序固定：attempt / track_id / stage / retryability / action / decision /
        // wait_ms 或 reason / detail。
        let mut fields = vec![
            ("attempt", attempt.clone()),
            ("track_id", self.track_id.to_string()),
            ("stage", failure.stage.as_str().to_string()),
            ("retryability", failure.retryability.as_str().to_string()),
            ("action", failure.action.as_str().to_string()),
        ];
        match wait_ms {
            Some(wait_ms) => {
                fields.push(("decision", "retry".to_string()));
                fields.push(("wait_ms", wait_ms));
            }
            None => {
                fields.push(("decision", "give_up".to_string()));
                fields.push(("reason", reason.unwrap_or_default()));
            }
        }
        fields.push(("detail", failure.detail.clone()));

        let refs: Vec<(&str, &str)> = fields
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        self.outlet
            .log(level, LogTopic::Player, "attempt_failed", &refs);
    }

    pub fn attempt_ok(&self, attempt: u8, elapsed: Duration) {
        let attempt = attempt.to_string();
        let elapsed_ms = elapsed.as_millis().to_string();
        self.outlet.log(
            LogLevel::Info,
            LogTopic::Player,
            "attempt_ok",
            &[
                ("attempt", &attempt),
                ("track_id", self.track_id),
                ("elapsed_ms", &elapsed_ms),
            ],
        );
    }

    /// 把当前轨迹立即落盘：一次播放尝试有了结论，别等下一次事件或进程退出。
    pub fn flush(&self) {
        self.outlet.flush();
    }
}

/// 按预算驱动一次播放尝试的重试循环，并在每一步写下轨迹。
///
/// `attempt_fn` 的入参是本次是第几次尝试（从 1 开始）；`sleep` 只负责等待，
/// 便于单测替换成空实现。返回第一次成功或最终放弃的失败。
pub fn drive_play_attempt<T, A, S>(
    budget: PlayBudget,
    trace: Option<&PlaybackTrace<'_>>,
    started: Instant,
    mut attempt_fn: A,
    mut sleep: S,
) -> Result<T, PlaybackFailure>
where
    A: FnMut(u8) -> Result<T, PlaybackFailure>,
    S: FnMut(Duration),
{
    let mut attempt = 0u8;
    loop {
        attempt += 1;
        if let Some(trace) = trace {
            trace.attempt_started(attempt);
        }
        match attempt_fn(attempt) {
            Ok(value) => {
                if let Some(trace) = trace {
                    trace.attempt_ok(attempt, started.elapsed());
                    trace.flush();
                }
                return Ok(value);
            }
            Err(failure) => {
                let jitter = jitter_from_elapsed(started.elapsed());
                let decision = decide(&budget, attempt, &failure, jitter);
                if let Some(trace) = trace {
                    trace.attempt_failed(attempt, &failure, decision);
                }
                match decision {
                    RetryDecision::Retry { wait } => sleep(wait),
                    RetryDecision::GiveUp { .. } => {
                        if let Some(trace) = trace {
                            trace.flush();
                        }
                        return Err(failure);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::{RecoveryAction, Stage};
    use std::cell::Cell;

    fn permanent() -> PlaybackFailure {
        PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::CheckSigner,
            "signer error: https://sign.test/sign?token=SECRET",
        )
    }

    fn transient() -> PlaybackFailure {
        PlaybackFailure::new(
            Stage::Transfer,
            Retryability::Transient,
            RecoveryAction::Nothing,
            "io: timed out reading response",
        )
    }

    fn lines_of(outlet: &LogOutlet) -> Vec<String> {
        outlet.flush();
        std::fs::read_to_string(outlet.log_file_path())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn permanent_failure_runs_once_and_ends_with_a_give_up_line() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().join("logs"));
        let trace = PlaybackTrace::new(&outlet, "track-1");
        let calls = Cell::new(0);

        let result = drive_play_attempt(
            PlayBudget::default(),
            Some(&trace),
            Instant::now(),
            |_| {
                calls.set(calls.get() + 1);
                Err::<(), _>(permanent())
            },
            |_| panic!("永久失败不该等待重试"),
        );

        assert!(result.is_err());
        assert_eq!(calls.get(), 1, "永久失败只允许一次远程尝试");

        let lines = lines_of(&outlet);
        assert_eq!(lines.len(), 2, "一次尝试只留开始与放弃两行：{lines:#?}");
        assert!(
            lines[0].contains("INFO player attempt_start attempt=1 track_id=track-1"),
            "{}",
            lines[0]
        );
        let failed = &lines[1];
        assert!(failed.contains("ERROR player attempt_failed attempt=1 track_id=track-1"));
        assert!(failed.contains("stage=resolve_stream"));
        assert!(failed.contains("retryability=never"));
        assert!(failed.contains("action=check_signer"));
        assert!(failed.contains("decision=give_up"));
        assert!(failed.contains("reason=non_retryable"));
        assert!(failed.contains("detail="), "技术原文必须留下：{failed}");
        assert!(
            !failed.contains("token=SECRET") && !failed.contains("SECRET"),
            "detail 里的查询串必须写盘前切断：{failed}"
        );
    }

    #[test]
    fn transient_failures_retry_until_budget_exhausted_in_order() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().join("logs"));
        let trace = PlaybackTrace::new(&outlet, "track-2");
        let calls = Cell::new(0);
        let mut waits = Vec::new();

        let result = drive_play_attempt(
            PlayBudget::default(),
            Some(&trace),
            Instant::now(),
            |_| {
                calls.set(calls.get() + 1);
                Err::<(), _>(transient())
            },
            |wait| waits.push(wait),
        );

        assert!(result.is_err());
        assert_eq!(calls.get(), 3, "默认预算恰好三次尝试");

        let lines = lines_of(&outlet);
        assert_eq!(lines.len(), 6, "每次尝试两行：{lines:#?}");
        for attempt in 1..=3 {
            assert!(
                lines[(attempt - 1) * 2].contains(&format!(
                    "INFO player attempt_start attempt={attempt} track_id=track-2"
                )),
                "第 {attempt} 次开始行错位：{}",
                lines[(attempt - 1) * 2]
            );
        }
        assert!(lines[1].contains("WARN player attempt_failed attempt=1"));
        assert!(lines[1].contains("decision=retry"));
        assert!(lines[1].contains("wait_ms="));
        assert!(!lines[1].contains("reason="));
        assert!(lines[3].contains("WARN player attempt_failed attempt=2"));
        assert!(lines[3].contains("decision=retry"));
        assert!(lines[3].contains("wait_ms="));
        assert!(lines[5].contains("ERROR player attempt_failed attempt=3"));
        assert!(lines[5].contains("decision=give_up"));
        assert!(lines[5].contains("reason=budget_exhausted"));
        assert!(!lines[5].contains("wait_ms="));
        // 前两次失败决定重试（各等一次），第三次放弃不再等。
        assert_eq!(waits.len(), 2, "只应在决定重试时等待");
    }

    #[test]
    fn success_after_one_retry_ends_with_attempt_ok() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().join("logs"));
        let trace = PlaybackTrace::new(&outlet, "track-3");
        let calls = Cell::new(0);

        let result = drive_play_attempt(
            PlayBudget::default(),
            Some(&trace),
            Instant::now(),
            |_| {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    Err(transient())
                } else {
                    Ok("cached")
                }
            },
            |_| {},
        );

        assert_eq!(result.unwrap(), "cached");
        let lines = lines_of(&outlet);
        assert_eq!(lines.len(), 4, "失败一次再成功应留四行：{lines:#?}");
        assert!(lines[0].contains("attempt_start attempt=1"));
        assert!(lines[1].contains("attempt_failed attempt=1"));
        assert!(lines[1].contains("decision=retry"));
        assert!(lines[2].contains("attempt_start attempt=2"));
        assert!(lines[3].contains("INFO player attempt_ok attempt=2 track_id=track-3"));
        assert!(lines[3].contains("elapsed_ms="));
    }

    #[test]
    fn throttled_and_never_map_to_stable_log_values() {
        let throttled = PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Throttled,
            RecoveryAction::Nothing,
            "status 429",
        );
        assert_eq!(Retryability::Throttled.as_str(), "throttled");
        assert_eq!(Stage::ResolveStream.as_str(), "resolve_stream");
        assert_eq!(RecoveryAction::Nothing.as_str(), "nothing");
        assert!(matches!(
            decide(&PlayBudget::default(), 1, &throttled, 0.5),
            RetryDecision::Retry { .. }
        ));
        assert!(matches!(
            decide(&PlayBudget::default(), 3, &throttled, 0.5),
            RetryDecision::GiveUp {
                reason: "budget_exhausted"
            }
        ));
    }
}
