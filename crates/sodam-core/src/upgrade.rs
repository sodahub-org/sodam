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
    /// 登录身份变了：退出登录、换号，或会员状态刷新推进了代数。
    IdentityChanged,
    /// 用户点了取消。只停在下一首之前，正在下的那一首不打断。
    Cancelled,
}

impl StopReason {
    /// 日志字段 `stop=` 的稳定取值（小写下划线，不随界面语言变化）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::IdentityChanged => "identity_changed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// 下一首之前该不该停。
///
/// 两件事同时到达时报身份那次：用户要处理的是「换号了」，取消已经不重要。
pub fn stop_reason(start_epoch: u64, current_epoch: u64, cancelled: bool) -> Option<StopReason> {
    if current_epoch != start_epoch {
        return Some(StopReason::IdentityChanged);
    }
    cancelled.then_some(StopReason::Cancelled)
}

/// 一份缓存流的样子：档位分 + 是不是试听片段。
///
/// 只比较档位分不够：试听换成整曲时分数可能一样（同档位），但盘上那份文件确实变好了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamShape {
    /// 这条流的档位分；`None` = 旧缓存没记。
    pub rank: Option<i64>,
    /// 是不是试听片段。
    pub preview: bool,
}

impl From<&crate::session::CachedTrack> for StreamShape {
    fn from(cached: &crate::session::CachedTrack) -> Self {
        Self {
            rank: cached.quality_rank,
            preview: cached.preview,
        }
    }
}

/// 一首跑完落在哪一档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 盘上那份确实变好了。
    Upgraded,
    /// 跑完了，但服务端能给的最高就这些：不算成功。
    NoHigherTier,
    /// 这次没跑成：网络、磁盘、权益，或者跑之前那一首已经被删了。
    Failed,
}

impl Outcome {
    /// 按升级前后两份流的样子判定：试听换整曲算提升，档位分真变高算提升，其余不算。
    ///
    /// 档位分走 [`crate::quality::readable_rank`] 那一道闸：0 分是「量过但认不出」，
    /// 和旧缓存没记分数的 `None` 一样不算已知档位——否则「没记分 → 量出 0 分」
    /// 会被当成一次升级。
    pub fn of_shapes(before: StreamShape, after: StreamShape) -> Self {
        if before.preview && !after.preview {
            return Self::Upgraded;
        }
        if crate::quality::readable_rank(after.rank) > crate::quality::readable_rank(before.rank) {
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
/// 表跟着身份走：换号 / 登出后整份作废，同一首同一档位在新账号下重新可试（#254）。
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
        assert_eq!(stop_reason(7, 7, false), None);
        assert_eq!(stop_reason(7, 8, false), Some(StopReason::IdentityChanged));
        // 取消和换号同时到：报身份那次，那才是用户要处理的事。
        assert_eq!(stop_reason(7, 8, true), Some(StopReason::IdentityChanged));
    }

    /// 取消之后不再发下一首。
    #[test]
    fn cancel_stops_before_the_next_track() {
        assert_eq!(stop_reason(3, 3, true), Some(StopReason::Cancelled));
        assert_eq!(stop_reason(3, 3, false), None);
    }

    /// 档位没真提升不算成功。
    #[test]
    fn an_equal_or_lower_tier_is_not_an_upgrade() {
        let lossless = StreamShape {
            rank: Some(100),
            preview: false,
        };
        let highest = StreamShape {
            rank: Some(80),
            preview: false,
        };
        assert_eq!(Outcome::of_shapes(highest, lossless), Outcome::Upgraded);
        assert_eq!(
            Outcome::of_shapes(lossless, lossless),
            Outcome::NoHigherTier,
            "同一条流拿回来两次不算升级"
        );
        assert_eq!(
            Outcome::of_shapes(lossless, highest),
            Outcome::NoHigherTier,
            "拿回更差的流不算升级"
        );
        // 旧缓存没记分数：这次量出来一个分数，就是实打实的提升。
        assert_eq!(
            Outcome::of_shapes(
                StreamShape {
                    rank: None,
                    preview: false
                },
                highest
            ),
            Outcome::Upgraded
        );
        // 旧缓存没记分数、这次量出来 0 分（认不出）：不算已知档位，也不算升级。
        assert_eq!(
            Outcome::of_shapes(
                StreamShape {
                    rank: None,
                    preview: false
                },
                StreamShape {
                    rank: Some(0),
                    preview: false
                }
            ),
            Outcome::NoHigherTier
        );
        // 试听换整曲：同档位也算，盘上那份文件确实变完整了。
        let preview = StreamShape {
            rank: Some(80),
            preview: true,
        };
        let full = StreamShape {
            rank: Some(80),
            preview: false,
        };
        assert_eq!(Outcome::of_shapes(preview, full), Outcome::Upgraded);
        // 整曲换成试听什么时候都不算升级（这条路本来就另有护栏挡着）。
        assert_eq!(Outcome::of_shapes(full, preview), Outcome::NoHigherTier);
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
        assert_eq!(StopReason::Cancelled.as_str(), "cancelled");
    }
}
