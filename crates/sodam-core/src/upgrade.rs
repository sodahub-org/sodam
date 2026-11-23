//! 缓存音质升级要的纯逻辑：这一轮该不该接着跑、一首跑完算不算成功、试过的记账。
//!
//! 应用层单首与批量共用同一段「跑一首」，两条路都从这里取判断：
//! [`stop_reason`]（下一首之前要不要停）、[`Outcome`]（这一首落在哪一档）、
//! [`Tally`]（批量收尾的三个数）、[`UpgradeAttempts`]（本会话试过哪几首，单首那条用）。
//! 身份代数与取消标志由界面线程读出来再喂进来——后台任务碰不到 `Root`。
//!
//! 身份变了**不**自动拿新号接着跑：目标档位是按旧号定的，新号权益可能撑不住，替人花流量不合适。

use std::collections::HashSet;

/// 批量为什么不再发下一首。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// 登录身份变了：落盘完成的退出登录、换号，或会员状态刷新推进了代数。
    IdentityChanged,
    /// 正在退出登录、落盘还没回来。`auth_epoch` 要等落盘成功才推进，慢盘上那几秒里批量还能
    /// 拿旧 Cookie 再发一首，所以不等落盘就停；可落盘也可能失败、账号还在，那时候说「账号已
    /// 变更」是撒谎，只能说「正在退出登录」。
    LoggingOut,
    /// 用户点了取消。只停在下一首之前，正在下的那一首不打断。
    Cancelled,
}

impl StopReason {
    /// 日志字段 `stop=` 的稳定取值（小写下划线，不随界面语言变化）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::IdentityChanged => "identity_changed",
            Self::LoggingOut => "logging_out",
            Self::Cancelled => "cancelled",
        }
    }
}

/// 判断批量该不该停的输入。
///
/// 字段带名字：两个布尔挨着当参数，调用点把 `cancelled` 和 `logging_out` 写反了照样编译，
/// 症状是点取消却弹「正在退出登录」。
#[derive(Debug, Clone, Copy)]
pub struct StopConditions {
    pub start_epoch: u64,
    pub current_epoch: u64,
    pub cancelled: bool,
    pub logging_out: bool,
}

/// 下一首之前该不该停。
///
/// 优先级：正在退出登录 > 身份变了 > 用户取消。登出还在落盘也停，理由见
/// [`StopReason::LoggingOut`]；身份和取消同时来也报身份变了，用户先要处理换号。
pub fn stop_reason(conditions: StopConditions) -> Option<StopReason> {
    if conditions.logging_out {
        return Some(StopReason::LoggingOut);
    }
    if conditions.current_epoch != conditions.start_epoch {
        return Some(StopReason::IdentityChanged);
    }
    conditions.cancelled.then_some(StopReason::Cancelled)
}

/// 批量因身份代数推进停下时，到底是真换了号，还是同一账号的登录状态变化。
///
/// 会员状态刷新也会推进 `auth_epoch`，但用户没换号，文案不能说「账号已变更」；
/// 两端账号 id 有一端没读到，也不下这个判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityChange {
    /// 两端账号 id 都读到了，而且不一样：真换了号。
    AccountChanged,
    /// 账号 id 没变、有一端没读到、或本来就没登录：只说登录状态变了。
    LoginStateChanged,
}

/// 对齐起始与停下那一刻的账号 id，判断是不是真换了号。
///
/// 空串和只有空格的值按「没读到」处理：接口给的 user_id 可能为空，不能拿它和真账号
/// 比出「换了号」。
pub fn identity_change(
    start_user_id: Option<&str>,
    current_user_id: Option<&str>,
) -> IdentityChange {
    fn read(id: Option<&str>) -> Option<&str> {
        id.map(str::trim).filter(|value| !value.is_empty())
    }
    match (read(start_user_id), read(current_user_id)) {
        (Some(before), Some(now)) if before != now => IdentityChange::AccountChanged,
        _ => IdentityChange::LoginStateChanged,
    }
}

/// 一首跑完落在哪一档。
///
/// 由升级那条路在换文件那一刻定下来，不在事后拿升级前后的两份缓存比分数：
/// 档位未知的旧缓存没有分数可比，「量出一个正分」并不等于真的升了一档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 盘上那份确实换成了更好的流。
    Upgraded,
    /// 跑完了，但没换成更好的流：服务端能给的最高就这些，或者回来的流并不更好。
    NoHigherTier,
    /// 这次没跑成：网络、磁盘、权益，或者跑之前那一首已经被删了。
    Failed,
}

impl Outcome {
    /// 把「这次有没有换成更好的流」翻成对外的结论。界面上的成功与否只认这一个映射。
    pub fn from_improved(improved: bool) -> Self {
        if improved {
            Self::Upgraded
        } else {
            Self::NoHigherTier
        }
    }
}

/// 一轮批量的三类计数：成功 / 失败 / 确实没更高档位。
///
/// 中间那一档是「这一首没跑成」的总数，不只网络：磁盘写不进去、权益不够、曲目被删都落在这里，
/// 所以界面上的说法也是「失败」，不冒充「网络失败」。
///
/// 收尾那行日志和界面小结都读它，两边不各数一遍。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub upgraded: usize,
    pub failed: usize,
    pub no_higher: usize,
}

impl Tally {
    pub fn record(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Upgraded => self.upgraded += 1,
            Outcome::Failed => self.failed += 1,
            Outcome::NoHigherTier => self.no_higher += 1,
        }
    }

    /// 这一轮真跑了几首。提前停时小于扫出来的总数。
    pub fn total(&self) -> usize {
        self.upgraded + self.failed + self.no_higher
    }
}

/// 本会话「这一首、这个档位已经试过」的记账。
///
/// 表跟着身份走：换号 / 登出后整份作废，同一首同一档位在新账号下重新可试。
/// 身份代数由调用方在 [`UpgradeAttempts::observe_epoch`] 里喂进来，这里不自己数。
///
/// 一条记号记下就一直留着：网络抖一下、或者这首确实没有更高档位，都不该让它在每次播放时
/// 重跑一遍。作废只有两个来源——换了身份，或者盘上那份缓存被清掉。
#[derive(Debug, Default)]
pub struct UpgradeAttempts {
    epoch: u64,
    keys: HashSet<String>,
}

impl UpgradeAttempts {
    /// 记一次尝试；已经记过返回 `false`，这次不用再跑。
    pub fn begin(&mut self, key: String) -> bool {
        self.keys.insert(key)
    }

    /// 身份代数变了就整份作废。
    pub fn observe_epoch(&mut self, epoch: u64) {
        if epoch == self.epoch {
            return;
        }
        self.epoch = epoch;
        self.keys.clear();
    }

    /// 盘上的缓存没了：记号跟着作废（清缓存走这条路）。
    pub fn clear(&mut self) {
        self.keys.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 换号之后，下一首之前就停；报的是身份变了。
    #[test]
    fn identity_change_stops_before_the_next_track() {
        let same = StopConditions {
            start_epoch: 7,
            current_epoch: 7,
            cancelled: false,
            logging_out: false,
        };
        assert_eq!(stop_reason(same), None);
        assert_eq!(
            stop_reason(StopConditions {
                current_epoch: 8,
                ..same
            }),
            Some(StopReason::IdentityChanged)
        );
        // 取消和换号同时到：报身份那次，那才是用户要处理的事。
        assert_eq!(
            stop_reason(StopConditions {
                current_epoch: 8,
                cancelled: true,
                ..same
            }),
            Some(StopReason::IdentityChanged)
        );
    }

    /// 取消之后不再发下一首。
    #[test]
    fn cancel_stops_before_the_next_track() {
        let running = StopConditions {
            start_epoch: 3,
            current_epoch: 3,
            cancelled: false,
            logging_out: false,
        };
        assert_eq!(stop_reason(running), None);
        assert_eq!(
            stop_reason(StopConditions {
                cancelled: true,
                ..running
            }),
            Some(StopReason::Cancelled)
        );
    }

    /// 正在退出登录压过另外两条：落盘可能失败、账号还在，这时候说「账号已变更」没把握。
    #[test]
    fn logging_out_wins_over_identity_and_cancel() {
        let logging_out = StopConditions {
            start_epoch: 7,
            current_epoch: 7,
            cancelled: false,
            logging_out: true,
        };
        assert_eq!(stop_reason(logging_out), Some(StopReason::LoggingOut));
        assert_eq!(
            stop_reason(StopConditions {
                current_epoch: 8,
                ..logging_out
            }),
            Some(StopReason::LoggingOut)
        );
        assert_eq!(
            stop_reason(StopConditions {
                current_epoch: 8,
                cancelled: true,
                ..logging_out
            }),
            Some(StopReason::LoggingOut)
        );
    }

    /// 只有两端账号 id 都读到且不同，才算真换号。
    #[test]
    fn identity_change_is_only_account_change_when_ids_differ() {
        assert_eq!(
            identity_change(Some("8801"), Some("8802")),
            IdentityChange::AccountChanged
        );
        // 同账号的会员状态刷新：代数变了，但号没变。
        assert_eq!(
            identity_change(Some("8801"), Some("8801")),
            IdentityChange::LoginStateChanged
        );
        // 有一端没读到账号：不下「换了号」的结论。
        assert_eq!(
            identity_change(None, Some("8801")),
            IdentityChange::LoginStateChanged
        );
        assert_eq!(
            identity_change(Some("8801"), None),
            IdentityChange::LoginStateChanged
        );
        assert_eq!(
            identity_change(None, None),
            IdentityChange::LoginStateChanged
        );
        assert_eq!(
            identity_change(Some("8801"), Some("")),
            IdentityChange::LoginStateChanged
        );
        assert_eq!(
            identity_change(Some("8801"), Some("  ")),
            IdentityChange::LoginStateChanged
        );
        // 两端只有空白差异：同一个号。
        assert_eq!(
            identity_change(Some(" 8801 "), Some("8801")),
            IdentityChange::LoginStateChanged
        );
    }

    /// 「有没有换成更好的流」是界面报成功的唯一出口，钉住它别被改宽。
    #[test]
    fn improved_maps_to_the_reported_outcome() {
        assert_eq!(Outcome::from_improved(true), Outcome::Upgraded);
        assert_eq!(Outcome::from_improved(false), Outcome::NoHigherTier);
    }

    /// 换号以后去重表整份作废：同一首同一档位在新号下能再试一次。
    #[test]
    fn identity_change_voids_the_attempt_table() {
        let mut attempts = UpgradeAttempts::default();
        let key = "8801:lossless".to_string();
        assert!(attempts.begin(key.clone()));
        assert!(!attempts.begin(key.clone()), "同一次会话里不重复跑");

        attempts.observe_epoch(1);
        assert!(attempts.begin(key.clone()), "新账号下重新可试");
        // 同一个代数再喂一遍：什么都不用清。
        attempts.observe_epoch(1);
        assert!(!attempts.begin(key.clone()));
    }

    /// 清缓存把记号一起作废：文件没了，同一首当然要能重新跑。
    #[test]
    fn clearing_drops_the_marks() {
        let mut attempts = UpgradeAttempts::default();
        let key = "8801:lossless".to_string();
        assert!(attempts.begin(key.clone()));
        assert!(!attempts.begin(key.clone()));

        attempts.clear();
        assert!(attempts.begin(key), "清掉缓存后同一首要能重新试");
    }

    /// 收尾的三个数分开记，总数只算真跑过的。
    #[test]
    fn tally_keeps_the_three_numbers_apart() {
        let mut tally = Tally::default();
        assert_eq!(tally.total(), 0);

        tally.record(Outcome::Upgraded);
        tally.record(Outcome::Failed);
        tally.record(Outcome::Failed);
        tally.record(Outcome::NoHigherTier);

        assert_eq!(tally.upgraded, 1);
        assert_eq!(tally.failed, 2);
        assert_eq!(tally.no_higher, 1);
        assert_eq!(tally.total(), 4);
    }

    /// 日志取值稳定：小写下划线，不随界面语言变化。
    #[test]
    fn stop_log_values_are_stable() {
        assert_eq!(StopReason::IdentityChanged.as_str(), "identity_changed");
        assert_eq!(StopReason::LoggingOut.as_str(), "logging_out");
        assert_eq!(StopReason::Cancelled.as_str(), "cancelled");
    }
}
