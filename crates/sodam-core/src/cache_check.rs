//! 缓存维护「这一拍要不要跑」的判断（#316）。
//!
//! 接线在 `crates/sodam/src/app_actions.rs` 的 `Root::run_cache_maintenance`：
//! 心跳每拍拿账本问一句越线没有，每 60 秒按到期规则问一句，用户改上限这种必须
//! 立刻收口的另算。问出来的结果只有三种：现在跑、先跳过、记下回头补。
//!
//! 这里保持纯判断：不碰磁盘、不碰 GPUI、不读全局账本，时间也由调用方给，
//! 所以整张组合表都能在单测里跑完，不依赖本机真实的缓存目录。

use crate::cache::{MaintenanceLimits, MaintenanceMark};
use std::time::{Duration, SystemTime};

/// 常规维护间隔：缓存是「别无限涨」的软约束，按小时级检查就够，不必每分钟重算
/// 一遍树。取 6 小时是和 Kodi 的图片缓存同一档：它的「无活可干」下一次
/// 检查也要 6 小时起步，再密就回到「每分钟扫一次」的老问题。
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// 把检查调密的间隔，两种情况共用：已用 ≥ 90%，或上一轮有子树读不全。
///
/// 配额是软上限：贴着额度时还在下载，按常规间隔一查就等于超着一整天；读不全的
/// 字节又只是下界，同样不能按常规间隔放走。其余情况仍按 [`MAINTENANCE_INTERVAL`]。
pub const MAINTENANCE_NEAR_LIMIT_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// 已知超限后的重试下限：播放中的文件被租约挡着时这一轮可能一个都没删掉，
/// 不设下限就会每拍重扫，等于把尖峰按分钟复现。
pub const MAINTENANCE_PRESSURE_RETRY: Duration = Duration::from_secs(10 * 60);

/// 这一拍是谁来问的。
///
/// 三种来源的「被挡住怎么办」不同，用一个枚举写清楚，别在调用点传布尔。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceTrigger {
    /// 心跳 / 启动：按到期规则问一句。
    Due,
    /// 用户改了上限：必须马上收口，被挡下也要记着回头补。
    Forced,
    /// 下载记账把占用推过九成：不等例行检查。
    Ledger,
}

/// 这一拍被哪些事挡着。
///
/// 三件事互不排斥，任一件没结束就这一拍先不跑。分开写而不是合成一个布尔，
/// 是为了在测试里能把每一种挡路理由单独摆出来。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Blockers {
    /// 上一轮维护还没跑完。
    pub running: bool,
    /// 用户正在清空缓存。
    pub clearing: bool,
    /// 正在批量把歌升级成更高音质。
    pub upgrading: bool,
}

impl Blockers {
    /// 有没有挡路的。
    pub fn any(&self) -> bool {
        self.running || self.clearing || self.upgrading
    }
}

/// 这一拍问出来的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceDecision {
    /// 起后台任务，真跑一轮。
    Run,
    /// 什么都不做，下一拍再说。
    Skip,
    /// 先不跑，但记下这一笔：挡路的结束后由下一拍心跳补上。
    Defer,
}

/// 问一次需要的全部输入。
///
/// 账本增量由调用方传进来，判断本身不读全局原子，测试才能随便造数。
pub struct MaintenanceAsk<'a> {
    pub trigger: MaintenanceTrigger,
    pub blockers: Blockers,
    /// 上次维护的结论；第一次跑、记录坏了、用户清空过缓存根时是 `None`。
    pub mark: Option<&'a MaintenanceMark>,
    pub limits: MaintenanceLimits,
    /// 上次看盘之后又写进来的音频净增量（可为负）。
    pub pending_audio: i64,
    pub now: SystemTime,
}

/// 「该不该清」的判断，外加一笔「被挡下的强制请求」的欠账。
///
/// 欠账只记在这里：被挡下的强制请求记一笔，挡路的结束后的下一拍不管到没到期都
/// 补跑，跑起来就销账。放在这里而不是界面里，是为了让「会不会补跑」也能被测到。
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheCheck {
    owed: bool,
}

impl CacheCheck {
    /// 有没有一笔被挡下的强制请求还欠着。设置页拿它判断旧结论还作不作数。
    pub fn owed(&self) -> bool {
        self.owed
    }

    /// 这一拍该怎么办。
    pub fn ask(&mut self, ask: MaintenanceAsk<'_>) -> MaintenanceDecision {
        // 欠着的强制请求按强制算：补跑不等到期。
        let forced = ask.trigger == MaintenanceTrigger::Forced || self.owed;
        if ask.blockers.any() {
            if forced {
                // 挡路的还没结束：账继续挂着，下一拍再来问。重复记是幂等的。
                self.owed = true;
                return MaintenanceDecision::Defer;
            }
            return MaintenanceDecision::Skip;
        }
        let run = match ask.trigger {
            MaintenanceTrigger::Forced => true,
            MaintenanceTrigger::Due => maintenance_due(ask.mark, ask.limits, ask.now),
            MaintenanceTrigger::Ledger => ledger_pressure(ask.mark, ask.limits, ask.pending_audio),
        };
        // 欠着的强制请求不看各自的到期规则：挡路的刚结束，这一拍就补。
        if !run && !self.owed {
            return MaintenanceDecision::Skip;
        }
        // 真跑一轮就销账：这一轮读的是新上限，欠的强制请求已经兑现。
        self.owed = false;
        MaintenanceDecision::Run
    }
}

/// 账本有没有把音频占用推过九成、而上次看盘还没到九成。
pub fn ledger_pressure(
    mark: Option<&MaintenanceMark>,
    limits: MaintenanceLimits,
    pending_audio: i64,
) -> bool {
    let Some(mark) = mark else {
        // 没有结论时 `maintenance_due` 本来就要求跑一轮，这里不重复排。
        return false;
    };
    ledger_says_near_limit(mark.audio_bytes, pending_audio, limits.audio_bytes)
}

/// 上一轮扫描的数字加上这本账，够不够把检查提前。
///
/// 只在「上次看盘还没到九成、这次写进来的字节把它推上去」时成立：上次看盘本身
/// 就在九成以上时交给 [`maintenance_due`] 的密集档——一轮扫完账本就清零，
/// 停在高位由那里的 30 分钟间隔兜住，不会写一首就扫一遍盘。
fn ledger_says_near_limit(mark_audio_bytes: u64, pending: i64, limit_bytes: u64) -> bool {
    if pending <= 0 {
        return false;
    }
    near_limit(mark_audio_bytes.saturating_add_signed(pending), limit_bytes)
        && !near_limit(mark_audio_bytes, limit_bytes)
}

/// 这一拍要不要真跑一轮维护。
///
/// 越接近上限查得越密：
/// - 已知超限：过了 [`MAINTENANCE_PRESSURE_RETRY`] 就重试（挡住删除时不能每拍重扫）；
/// - 上一轮读不全：过了 [`MAINTENANCE_NEAR_LIMIT_INTERVAL`] 就重查（下界不能当真）；
/// - 已用 ≥ 90%：同样过了 [`MAINTENANCE_NEAR_LIMIT_INTERVAL`] 就查一次；
/// - 其余：过了 [`MAINTENANCE_INTERVAL`] 才查。
///
/// 没有记录（首次运行、记录损坏、用户清空过缓存根）按到期处理，跑一轮就能重新立账。
pub fn maintenance_due(
    mark: Option<&MaintenanceMark>,
    limits: MaintenanceLimits,
    now: SystemTime,
) -> bool {
    let Some(mark) = mark else {
        return true;
    };
    let Ok(elapsed) = now.duration_since(mark.completed_at) else {
        // 时钟回拨：记录看起来在未来。当成到期跑一次，落盘时会改回当前时刻。
        return true;
    };
    let over_limit = mark.audio_bytes > limits.audio_bytes || mark.cover_bytes > limits.cover_bytes;
    if over_limit {
        return elapsed >= MAINTENANCE_PRESSURE_RETRY;
    }
    // 读不全的字节数只是下界：不能按常规间隔等，也不能当成刚刚完整盘点过。
    // 但也不能按超限的 10 分钟来——目录长期读不到时会变成每 10 分钟扫一遍。
    if mark.incomplete {
        return elapsed >= MAINTENANCE_NEAR_LIMIT_INTERVAL;
    }
    if elapsed >= MAINTENANCE_INTERVAL {
        return true;
    }
    let near = near_limit(mark.audio_bytes, limits.audio_bytes)
        || near_limit(mark.cover_bytes, limits.cover_bytes);
    near && elapsed >= MAINTENANCE_NEAR_LIMIT_INTERVAL
}

/// 已用是否摸到上限的九成。
fn near_limit(known: u64, limit: u64) -> bool {
    known.saturating_mul(10) >= limit.saturating_mul(9)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn limits() -> MaintenanceLimits {
        MaintenanceLimits::from_mb(10, 512)
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000)
    }

    fn mark(completed_at: SystemTime, audio_bytes: u64) -> MaintenanceMark {
        MaintenanceMark {
            completed_at,
            audio_bytes,
            cover_bytes: 0,
            incomplete: false,
        }
    }

    /// 造一个「到期 / 没到期」的结论：字节停在五成，别让占用自己触发密集档。
    fn state_mark(due: bool, now: SystemTime) -> MaintenanceMark {
        let age = if due {
            MAINTENANCE_INTERVAL
        } else {
            Duration::from_secs(60)
        };
        mark(now - age, 5 * MIB)
    }

    fn input<'a>(
        trigger: MaintenanceTrigger,
        blockers: Blockers,
        mark: &'a MaintenanceMark,
        pending_audio: i64,
    ) -> MaintenanceAsk<'a> {
        MaintenanceAsk {
            trigger,
            blockers,
            mark: Some(mark),
            limits: limits(),
            pending_audio,
            now: now(),
        }
    }

    fn blocked() -> Blockers {
        Blockers {
            upgrading: true,
            ..Blockers::default()
        }
    }

    /// 到期判定只看落盘结论——没到期不扫，已知超限不等到期，但也不能每拍重扫。
    #[test]
    fn maintenance_due_follows_interval_and_pressure() {
        let limits = limits();
        let now = now();
        assert!(maintenance_due(None, limits, now), "没有记录要跑一轮立账");

        let fresh = MaintenanceMark {
            completed_at: now - Duration::from_secs(60),
            audio_bytes: 1,
            cover_bytes: 1,
            incomplete: false,
        };
        assert!(
            !maintenance_due(Some(&fresh), limits, now),
            "刚跑完、没超限：这一拍什么都不做"
        );

        let stale = MaintenanceMark {
            completed_at: now - MAINTENANCE_INTERVAL,
            audio_bytes: 1,
            cover_bytes: 1,
            incomplete: false,
        };
        assert!(maintenance_due(Some(&stale), limits, now), "到点要扫");

        let near = MaintenanceMark {
            completed_at: now - MAINTENANCE_NEAR_LIMIT_INTERVAL,
            audio_bytes: limits.audio_bytes / 10 * 9 + 1,
            cover_bytes: 0,
            incomplete: false,
        };
        assert!(
            maintenance_due(Some(&near), limits, now),
            "已经贴着上限：不能按常规间隔等一整天"
        );
        let near_but_fresh = MaintenanceMark {
            completed_at: now - Duration::from_secs(60),
            audio_bytes: limits.audio_bytes / 10 * 9 + 1,
            cover_bytes: 0,
            incomplete: false,
        };
        assert!(
            !maintenance_due(Some(&near_but_fresh), limits, now),
            "刚查过就别再查"
        );

        let incomplete = MaintenanceMark {
            completed_at: now - Duration::from_secs(60),
            audio_bytes: 1,
            cover_bytes: 1,
            incomplete: true,
        };
        assert!(
            !maintenance_due(Some(&incomplete), limits, now),
            "读不全的盘点按密集档重试，别每拍重扫"
        );
        let incomplete_retry = MaintenanceMark {
            completed_at: now - MAINTENANCE_NEAR_LIMIT_INTERVAL,
            audio_bytes: 1,
            cover_bytes: 1,
            incomplete: true,
        };
        assert!(
            maintenance_due(Some(&incomplete_retry), limits, now),
            "读不全的字节只是下界，不能把下一次检查推到常规间隔"
        );

        let over = MaintenanceMark {
            completed_at: now - MAINTENANCE_PRESSURE_RETRY,
            audio_bytes: limits.audio_bytes + 1,
            cover_bytes: 0,
            incomplete: false,
        };
        assert!(
            maintenance_due(Some(&over), limits, now),
            "已知超限不等到期"
        );

        let just_retried = MaintenanceMark {
            completed_at: now - Duration::from_secs(30),
            audio_bytes: limits.audio_bytes + 1,
            cover_bytes: 0,
            incomplete: false,
        };
        assert!(
            !maintenance_due(Some(&just_retried), limits, now),
            "播放中的文件挡住删除时，也不能每拍重扫"
        );

        let rewound = MaintenanceMark {
            completed_at: now + Duration::from_secs(3600),
            audio_bytes: 0,
            cover_bytes: 0,
            incomplete: false,
        };
        assert!(
            maintenance_due(Some(&rewound), limits, now),
            "时钟回拨当成到期，别让维护永久停摆"
        );
    }

    /// 账本只在「上次看盘还没到九成、这次写进来的字节把它推上去」时才提前排。
    #[test]
    fn ledger_pressure_only_fires_when_crossing_from_below() {
        let limits = limits();
        let limit = limits.audio_bytes;
        let fresh = mark(now() - Duration::from_secs(60), 5 * MIB);
        assert!(
            ledger_pressure(Some(&fresh), limits, 4 * MIB as i64),
            "从五成推到九成"
        );
        assert!(
            !ledger_pressure(Some(&fresh), limits, 3 * MIB as i64),
            "还不到九成不跑"
        );
        let already_near = mark(now() - Duration::from_secs(60), 9 * MIB);
        assert!(
            !ledger_pressure(Some(&already_near), limits, MIB as i64),
            "上次看盘就贴着上限：交给 maintenance_due 的密集档，别写一首扫一遍"
        );
        assert!(!ledger_pressure(Some(&fresh), limits, 0));
        assert!(
            !ledger_pressure(Some(&fresh), limits, -(MIB as i64)),
            "账本缩了不能触发"
        );
        assert!(
            !ledger_pressure(None, limits, limit as i64),
            "没有结论时交给 maintenance_due，这里不重复排"
        );
    }

    /// 触发来源 × 挡路条件（八种组合）× 到期与否 × 账本越线与否，逐个对照期望。
    #[test]
    fn every_combination_lands_on_the_expected_decision() {
        let limits = limits();
        let now = now();
        for due in [false, true] {
            for pressure in [false, true] {
                let mark = state_mark(due, now);
                let pending_audio = if pressure {
                    limits.audio_bytes as i64
                } else {
                    0
                };
                // 先确认造出来的状态真的落在预期的格子里，免得下面的表跟着错。
                assert_eq!(maintenance_due(Some(&mark), limits, now), due);
                assert_eq!(
                    ledger_pressure(Some(&mark), limits, pending_audio),
                    pressure
                );

                for mask in 0..8u8 {
                    let blockers = Blockers {
                        running: mask & 1 != 0,
                        clearing: mask & 2 != 0,
                        upgrading: mask & 4 != 0,
                    };
                    for trigger in [
                        MaintenanceTrigger::Due,
                        MaintenanceTrigger::Forced,
                        MaintenanceTrigger::Ledger,
                    ] {
                        let got = CacheCheck::default().ask(input(
                            trigger,
                            blockers,
                            &mark,
                            pending_audio,
                        ));
                        let want = if blockers.any() {
                            if trigger == MaintenanceTrigger::Forced {
                                MaintenanceDecision::Defer
                            } else {
                                MaintenanceDecision::Skip
                            }
                        } else {
                            match trigger {
                                MaintenanceTrigger::Forced => MaintenanceDecision::Run,
                                MaintenanceTrigger::Due if due => MaintenanceDecision::Run,
                                MaintenanceTrigger::Ledger if pressure => MaintenanceDecision::Run,
                                _ => MaintenanceDecision::Skip,
                            }
                        };
                        assert_eq!(
                            got, want,
                            "trigger={trigger:?} blockers={blockers:?} due={due} pressure={pressure}"
                        );
                    }
                }
            }
        }
    }

    /// 第一次跑没有上次结论：按到期处理，跑一轮就能重新立账。
    #[test]
    fn first_run_without_a_mark_runs() {
        let decision = CacheCheck::default().ask(MaintenanceAsk {
            trigger: MaintenanceTrigger::Due,
            blockers: Blockers::default(),
            mark: None,
            limits: limits(),
            pending_audio: 0,
            now: now(),
        });
        assert_eq!(
            decision,
            MaintenanceDecision::Run,
            "没有结论时不能因为「没到点」就永远不扫"
        );
    }

    /// 被挡下的强制请求记一笔，挡路的结束后的下一拍就补跑，不等下一轮到期。
    #[test]
    fn blocked_forced_request_is_retried_on_the_next_tick() {
        let fresh = state_mark(false, now());
        let mut check = CacheCheck::default();

        assert_eq!(
            check.ask(input(MaintenanceTrigger::Forced, blocked(), &fresh, 0)),
            MaintenanceDecision::Defer
        );
        assert!(check.owed(), "被挡下的强制请求要记一笔，否则没人补跑");

        // 挡路的还在：账继续挂着，别丢。
        assert_eq!(
            check.ask(input(MaintenanceTrigger::Due, blocked(), &fresh, 0)),
            MaintenanceDecision::Defer
        );
        assert!(check.owed());

        // 挡路的结束：下一拍心跳（按到期规则问的）就得补跑，不用等到期。
        assert_eq!(
            check.ask(input(
                MaintenanceTrigger::Due,
                Blockers::default(),
                &fresh,
                0
            )),
            MaintenanceDecision::Run
        );
        assert!(!check.owed(), "跑起来就销账");

        // 补跑完回到正常节奏：没到期就跳过。
        assert_eq!(
            check.ask(input(
                MaintenanceTrigger::Due,
                Blockers::default(),
                &fresh,
                0
            )),
            MaintenanceDecision::Skip
        );
    }

    /// 三种挡路理由各自都算数；例行检查被挡下不记账，下一拍自己会再来问。
    #[test]
    fn only_forced_requests_leave_a_debt() {
        let fresh = state_mark(false, now());
        for blockers in [
            Blockers {
                running: true,
                ..Blockers::default()
            },
            Blockers {
                clearing: true,
                ..Blockers::default()
            },
            Blockers {
                upgrading: true,
                ..Blockers::default()
            },
        ] {
            let mut forced = CacheCheck::default();
            assert_eq!(
                forced.ask(input(MaintenanceTrigger::Forced, blockers, &fresh, 0)),
                MaintenanceDecision::Defer
            );
            assert!(forced.owed(), "{blockers:?}");

            for trigger in [MaintenanceTrigger::Due, MaintenanceTrigger::Ledger] {
                let mut routine = CacheCheck::default();
                assert_eq!(
                    routine.ask(input(trigger, blockers, &fresh, 0)),
                    MaintenanceDecision::Skip,
                    "trigger={trigger:?} blockers={blockers:?}"
                );
                assert!(!routine.owed(), "例行检查不值得补跑");
            }
        }
    }

    /// 越过九成只提前排一次：扫完停在九成以上、账本清零后，再写歌也不按账本反复触发。
    #[test]
    fn ledger_schedules_the_early_sweep_only_once() {
        let now = now();
        let mut check = CacheCheck::default();
        let below = mark(now - Duration::from_secs(60), 5 * MIB);
        assert_eq!(
            check.ask(input(
                MaintenanceTrigger::Ledger,
                Blockers::default(),
                &below,
                5 * MIB as i64
            )),
            MaintenanceDecision::Run,
            "从五成推到十成：提前跑一轮"
        );

        let after = mark(now - Duration::from_secs(60), 9 * MIB + 1);
        assert_eq!(
            check.ask(input(
                MaintenanceTrigger::Ledger,
                Blockers::default(),
                &after,
                MIB as i64
            )),
            MaintenanceDecision::Skip,
            "停在九成以上交给密集档，不能每写一首就扫一遍盘"
        );
    }
}
