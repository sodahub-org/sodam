//! 配置文件热重载的状态机：指纹轮询 + 防抖 + 自写过滤（#185）。
//!
//! 纯逻辑、可单独测试：读文件与解析都在 [`ReloadTracker::poll`] 里完成，
//! UI 侧应放到后台线程调用，拿到事件回 UI 线程应用。
//!
//! 防抖要处理的正是外部编辑器的「先截断再写入」：截断态解析失败不报错，
//! 等文件稳定且能解析了才应用；稳定损坏（改完放在那不动）才提示一次。

use crate::config::{LoadOutcome, Settings};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

/// 文件指纹：修改时间 + 长度。截断再写全会同时变化，足够识别「变过了」。
pub type Fingerprint = (SystemTime, u64);

/// 解析成功后，指纹保持不变的这段时间才应用——跨过编辑器的写入过程。
const APPLY_AFTER: Duration = Duration::from_millis(600);
/// 指纹稳定且持续解析失败超过这段时间，才当作「真的坏了」提示一次。
const CORRUPT_AFTER: Duration = Duration::from_secs(3);

/// 一次轮询的结论，UI 侧按此处理。
#[derive(Debug)]
pub enum ReloadEvent {
    /// 无事发生（没变化 / 自己写的 / 防抖中 / 已提示过）。
    Quiet,
    /// 文件变了、解析成功且与内存不同：应用这份新设置（已迁移、已钳制）。
    Apply(Box<Settings>),
    /// 文件变了、解析成功但内容与应用内一致：不提示、不回写。
    Unchanged,
    /// 文件稳定损坏：提示一次（内存设置不动，文件不覆盖）。
    Corrupt(String),
}

fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[derive(Default)]
struct PendingChange {
    /// 防抖针对的那次变化。
    fingerprint: Option<Fingerprint>,
    /// 第一次看到这个指纹的时刻。
    first_seen: Option<Instant>,
    /// 第一次解析成功的时刻（指纹不变跨过 [`APPLY_AFTER`] 才应用）。
    good_since: Option<Instant>,
    /// 已经为这个指纹报过损坏提示（同一来源只提示一条）。
    corrupt_reported: bool,
}

/// 热重载状态机。跨线程共享时包在 `Arc<Mutex<>>` 里。
#[derive(Default)]
pub struct ReloadTracker {
    /// 上次确认过的指纹（`None` = 文件不存在）。应用自己的保存不改它：
    /// 自写窗口过去后下一次轮询会重读内容，与应用内一致就静默按 `Unchanged` 收尾。
    seen: Option<Fingerprint>,
    pending: PendingChange,
}

impl ReloadTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 把某个时刻的文件状态当作已确认基线（启动加载完成后调用）。
    pub fn acknowledge(&mut self, fingerprint: Option<Fingerprint>) {
        self.seen = fingerprint;
        self.pending = PendingChange::default();
    }

    /// 轮询一次。读盘与解析都在本调用线程完成。
    ///
    /// `self_write_recent` 由调用方传入 [`crate::config::recent_self_write`]，
    /// 测试可注入固定值，避免并行测试互相干扰。
    pub fn poll(
        &mut self,
        path: &Path,
        current: &Settings,
        now: Instant,
        self_write_recent: bool,
    ) -> ReloadEvent {
        let fingerprint = fingerprint(path);
        if fingerprint == self.seen {
            return ReloadEvent::Quiet;
        }
        let Some(fingerprint) = fingerprint else {
            // 文件被外部删掉：不动内存设置，等它回来再重载。
            self.acknowledge(None);
            return ReloadEvent::Quiet;
        };
        // 应用自己刚保存过：这次静默跳过，不确认指纹——窗口过后内容与应用内
        // 一致，自然按 Unchanged 收尾，不会产生提示或回弹。
        if self_write_recent {
            return ReloadEvent::Quiet;
        }
        // 指纹变了：进入（或延续）这次变化的防抖。
        if self.pending.fingerprint != Some(fingerprint) {
            self.pending = PendingChange {
                fingerprint: Some(fingerprint),
                first_seen: Some(now),
                good_since: None,
                corrupt_reported: false,
            };
        }

        match Settings::try_load_from(path) {
            LoadOutcome::Corrupt { error } => {
                // 读取内容前后文件可能又被写（编辑器第二次落盘）：对不上就重开防抖，
                // 不拿旧的稳定时间给新内容背书。
                if fingerprint(path) != Some(fingerprint) {
                    self.pending = PendingChange {
                        fingerprint: Some(fingerprint),
                        first_seen: Some(now),
                        good_since: None,
                        corrupt_reported: false,
                    };
                    return ReloadEvent::Quiet;
                }
                let first_seen = self.pending.first_seen.unwrap_or(now);
                let stable = now.duration_since(first_seen) >= CORRUPT_AFTER;
                if stable && !self.pending.corrupt_reported {
                    self.pending.corrupt_reported = true;
                    return ReloadEvent::Corrupt(error);
                }
                ReloadEvent::Quiet
            }
            LoadOutcome::NotFound(_) => {
                // metadata 时还在、读取时已被删：不能把默认值当外部配置应用，
                // 否则一次删除竞争就会把音量/主题/凭据全部重置。
                self.pending = PendingChange::default();
                ReloadEvent::Quiet
            }
            LoadOutcome::Loaded { settings, .. } => {
                // 读取内容前后文件可能又被写：对不上就重开防抖，新内容要重新等稳定。
                if fingerprint(path) != Some(fingerprint) {
                    self.pending = PendingChange {
                        fingerprint: Some(fingerprint),
                        first_seen: Some(now),
                        good_since: None,
                        corrupt_reported: false,
                    };
                    return ReloadEvent::Quiet;
                }
                // 每次外部重载后重新补环境变量，优先级与启动时一致（#185）。
                // 这里故意不做 ensure_login_device：文件没带设备身份时不能在轮询里
                // 现生成一份（会悄悄轮换身份），交给应用侧沿用当前身份。
                let settings = settings.fill_empty_from_env();
                // 第一次解析成功只记账：等指纹稳定跨过 APPLY_AFTER 再应用，
                // 编辑器「截断 → 写全」的中间态不会被当成损坏，也不会应用半份。
                let good_since = *self.pending.good_since.get_or_insert(now);
                if now.duration_since(good_since) < APPLY_AFTER {
                    return ReloadEvent::Quiet;
                }
                self.acknowledge(Some(fingerprint));
                if settings == *current {
                    ReloadEvent::Unchanged
                } else {
                    ReloadEvent::Apply(Box::new(settings))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 外部改动（内容与内存不同）：防抖窗口过后应用一次。
    #[test]
    fn external_change_applies_after_stability_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut tracker = ReloadTracker::new();
        let current = Settings::default();
        let now = Instant::now();

        // 没有文件：安静。
        assert!(matches!(
            tracker.poll(&path, &current, now, false),
            ReloadEvent::Quiet
        ));

        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        // 第一次看到变化：只进入防抖，不立刻应用。
        assert!(matches!(
            tracker.poll(&path, &current, now, false),
            ReloadEvent::Quiet
        ));

        std::thread::sleep(APPLY_AFTER + Duration::from_millis(100));
        match tracker.poll(&path, &current, Instant::now(), false) {
            ReloadEvent::Apply(settings) => assert_eq!(settings.theme, "dark"),
            other => panic!("稳定后应应用新内容，实际 {other:?}"),
        }
        // 已确认：再轮询安静。
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet
        ));
    }

    /// 内容与应用内一致：不提示、不应用（编辑器回存同一份内容不打扰用户）。
    #[test]
    fn identical_content_settles_silently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let current = Settings::default();
        std::fs::write(&path, serde_json::to_string(&current).unwrap()).unwrap();
        let mut tracker = ReloadTracker::new();

        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet
        ));
        std::thread::sleep(APPLY_AFTER + Duration::from_millis(100));
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Unchanged
        ));
    }

    /// 应用自己刚保存过：窗口期内静默跳过，且不确认指纹——窗口过后内容一致，
    /// 自然按 Unchanged 收尾，不会丢掉任何东西。
    #[test]
    fn self_write_is_skipped_then_settles_as_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let current = Settings::default();
        std::fs::write(&path, serde_json::to_string(&current).unwrap()).unwrap();
        let mut tracker = ReloadTracker::new();

        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), true),
            ReloadEvent::Quiet
        ));
        // 自写窗口过后：先进入防抖（第一次解析成功只记账），再按 Unchanged 收尾。
        std::thread::sleep(APPLY_AFTER + Duration::from_millis(100));
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet
        ));
        std::thread::sleep(APPLY_AFTER + Duration::from_millis(100));
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Unchanged
        ));
    }

    /// 编辑器「先截断再写全」：截断态解析失败不报损坏，写全后照常应用。
    #[test]
    fn truncated_then_full_write_is_not_treated_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let current = Settings::default();
        let mut tracker = ReloadTracker::new();

        std::fs::write(&path, r#"{"theme":"dar"#).unwrap();
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet,
        ));
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet,
        ));

        std::thread::sleep(APPLY_AFTER + Duration::from_millis(100));
        match tracker.poll(&path, &current, Instant::now(), false) {
            ReloadEvent::Apply(settings) => assert_eq!(settings.theme, "dark"),
            other => panic!("写全后应应用新内容，实际 {other:?}"),
        }
    }

    /// 稳定损坏：提示一次（不重复刷屏）；内存设置由上层保持，文件不动。
    #[test]
    fn stable_corruption_reports_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let current = Settings::default();
        let mut tracker = ReloadTracker::new();

        std::fs::write(&path, "{oops").unwrap();
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet,
        ));

        std::thread::sleep(CORRUPT_AFTER + Duration::from_millis(100));
        match tracker.poll(&path, &current, Instant::now(), false) {
            ReloadEvent::Corrupt(error) => assert!(error.contains("line"), "{error}"),
            other => panic!("稳定损坏应提示一次，实际 {other:?}"),
        }
        // 同一份坏文件不再重复提示。
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet
        ));
        // 用户修好之后照常应用。
        std::fs::write(&path, r#"{"theme":"light"}"#).unwrap();
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet,
        ));
        std::thread::sleep(APPLY_AFTER + Duration::from_millis(100));
        match tracker.poll(&path, &current, Instant::now(), false) {
            ReloadEvent::Apply(settings) => assert_eq!(settings.theme, "light"),
            other => panic!("修好后应应用新内容，实际 {other:?}"),
        }
    }

    /// 文件被外部删掉：不动内存设置，等它回来再重载。
    #[test]
    fn missing_file_is_ignored_until_it_returns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let current = Settings::default();
        let mut tracker = ReloadTracker::new();

        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet
        ));
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet
        ));
        std::thread::sleep(APPLY_AFTER + Duration::from_millis(100));
        std::fs::write(&path, r#"{"theme":"light"}"#).unwrap();
        assert!(matches!(
            tracker.poll(&path, &current, Instant::now(), false),
            ReloadEvent::Quiet
        ));
        std::thread::sleep(APPLY_AFTER + Duration::from_millis(100));
        match tracker.poll(&path, &current, Instant::now(), false) {
            ReloadEvent::Apply(settings) => assert_eq!(settings.theme, "light"),
            other => panic!("文件回来后应应用新内容，实际 {other:?}"),
        }
    }
}
