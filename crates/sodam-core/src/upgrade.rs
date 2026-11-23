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

/// 一份缓存流的样子：档位分 + 标签能证明的档位下限 + 码率 + 是不是试听。
///
/// 只比较档位分不够：试听换成整曲时分数可能一样（同档位），但盘上那份文件确实变好了；
/// 旧三字段缓存的档位分是 `None`，得靠标签下限和码率兜底，否则「重新量了一次」
/// 会被当成升级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamShape {
    /// 这条流的档位分；`None` = 旧缓存没记。
    pub rank: Option<i64>,
    /// 旧标签能证明的档位下限（见 [`crate::quality::legacy_label_floor_rank`]）；证不出为 `None`。
    pub label_floor_rank: Option<i64>,
    /// 旧标签里写的档名（`极高` / `无损` 等）；新流和认不出的标签为 `None`。
    pub label_tier: Option<&'static str>,
    /// 码率（kbps）；不知道为 `None`。新流总有，旧缓存看标签里有没有写。
    pub bitrate_kbps: Option<i64>,
    /// 是不是试听片段。
    pub preview: bool,
}

impl From<&crate::session::CachedTrack> for StreamShape {
    fn from(cached: &crate::session::CachedTrack) -> Self {
        Self {
            rank: cached.quality_rank,
            label_floor_rank: crate::quality::legacy_label_floor_rank(&cached.quality),
            label_tier: crate::quality::TIER_LABELS
                .iter()
                .copied()
                .find(|tier| cached.quality.trim().starts_with(tier)),
            bitrate_kbps: crate::quality::legacy_label_bitrate_kbps(&cached.quality),
            preview: cached.preview,
        }
    }
}

impl StreamShape {
    /// 刚从服务端取到、还没落盘的一条流。
    pub fn from_download_info(info: &libresoda::soda::types::DownloadInfo) -> Self {
        Self {
            rank: Some(crate::quality::stream_rank(info)),
            label_floor_rank: None,
            label_tier: None,
            bitrate_kbps: Some(libresoda::soda::quality::normalize_bitrate(
                info.bitrate.max(0),
            )),
            preview: info.is_preview,
        }
    }
}

/// 新流是不是真的比本地这份好。只回答「算不算一次升级」，不回答「要不要换文件」。
///
/// 档位已知就只认档位提升；档位未知的旧缓存先看标签能证明的下限，再比码率——
/// 同样码率只是把档位重新量了一遍，不算更好；码率变低时只有新流已经够到用户要的
/// 目标档位才算（无损的 300k 换掉有损的 320k）。旧标签没写码率时证明不了新流更好，
/// 不算升级。
pub fn stream_is_better(before: StreamShape, after: StreamShape, target_rank: Option<i64>) -> bool {
    if before.preview && !after.preview {
        return true;
    }
    let Some(after_rank) = crate::quality::readable_rank(after.rank) else {
        return false;
    };
    if let Some(old_rank) = crate::quality::readable_rank(before.rank).or(before.label_floor_rank) {
        return after_rank > old_rank;
    }
    let reached_target = target_rank.is_some_and(|target| after_rank >= target);
    match (before.bitrate_kbps, after.bitrate_kbps) {
        (Some(old), Some(new)) if new > old => true,
        // 用码率换档位：只有新流码率更低、已经够到目标、且档名确实和旧标签不同才算
        // （无损的 300k 换掉有损的 320k）。同码率只是同一条流换了叫法，同名档位的
        // 低码率流只是更差，都不算。
        (Some(old), Some(new)) if new < old && reached_target => before
            .label_tier
            .is_some_and(|tier| tier != crate::quality::tier_label(after_rank)),
        // 同码率，或旧标签没写码率：证明不了更好。
        _ => false,
    }
}

/// 新流值不值得写回缓存。换文件和「算不算升级」是两个问题：
///
/// 同样码率的流也会换一次，把旧缓存没记下来的档位量进 sidecar；不换的话这一首
/// 每轮批量都会重下一次再丢掉（档位一直补不上）。换完由 [`stream_is_better`] 判定，
/// 这种「重新量了一遍」不算升级。
pub fn should_replace_stream(
    before: StreamShape,
    after: StreamShape,
    target_rank: Option<i64>,
) -> bool {
    if stream_is_better(before, after, target_rank) {
        return true;
    }
    if before.rank.is_some() || before.label_floor_rank.is_some() || after.preview {
        return false;
    }
    let Some(after_rank) = crate::quality::readable_rank(after.rank) else {
        return false;
    };
    // 旧缓存没记档位、标签也证不出下限：新流不比标签里写的码率差就换，把档位补进 sidecar。
    match (before.bitrate_kbps, after.bitrate_kbps) {
        (Some(old), Some(new)) => new >= old,
        _ => target_rank.is_some_and(|target| after_rank >= target),
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
    /// 按升级前后两份流的样子判定：试听换整曲算提升，档位真变高算提升，其余不算。
    ///
    /// 判定和 `session::is_better_stream` 共用 [`stream_is_better`]：旧三字段缓存
    /// （`rank = None`）不能再因为「量出了分数」就报成功，同码率只是把档位重新量了一遍。
    pub fn of_shapes(before: StreamShape, after: StreamShape, target_rank: Option<i64>) -> Self {
        if stream_is_better(before, after, target_rank) {
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

    fn check(start: u64, current: u64, cancelled: bool, logging_out: bool) -> StopConditions {
        StopConditions {
            start_epoch: start,
            current_epoch: current,
            cancelled,
            logging_out,
        }
    }

    /// 换号之后，下一首之前就停；报的是身份变了。
    #[test]
    fn identity_change_stops_before_the_next_track() {
        assert_eq!(stop_reason(check(7, 7, false, false)), None);
        assert_eq!(
            stop_reason(check(7, 8, false, false)),
            Some(StopReason::IdentityChanged)
        );
        // 取消和换号同时到：报身份那次，那才是用户要处理的事。
        assert_eq!(
            stop_reason(check(7, 8, true, false)),
            Some(StopReason::IdentityChanged)
        );
    }

    /// 取消之后不再发下一首。
    #[test]
    fn cancel_stops_before_the_next_track() {
        assert_eq!(
            stop_reason(check(3, 3, true, false)),
            Some(StopReason::Cancelled)
        );
        assert_eq!(stop_reason(check(3, 3, false, false)), None);
    }

    /// 正在退出登录压过另外两条：落盘可能失败、账号还在，这时候说「账号已变更」没把握。
    #[test]
    fn logging_out_wins_over_identity_and_cancel() {
        assert_eq!(
            stop_reason(check(7, 7, false, true)),
            Some(StopReason::LoggingOut)
        );
        assert_eq!(
            stop_reason(check(7, 8, false, true)),
            Some(StopReason::LoggingOut)
        );
        assert_eq!(
            stop_reason(check(7, 8, true, true)),
            Some(StopReason::LoggingOut)
        );
    }

    fn shape(
        rank: Option<i64>,
        floor: Option<i64>,
        bitrate: Option<i64>,
        preview: bool,
    ) -> StreamShape {
        StreamShape {
            rank,
            label_floor_rank: floor,
            label_tier: None,
            bitrate_kbps: bitrate,
            preview,
        }
    }

    fn with_label_tier(mut stream: StreamShape, tier: &'static str) -> StreamShape {
        stream.label_tier = Some(tier);
        stream
    }

    /// 档位没真提升不算成功。
    #[test]
    fn an_equal_or_lower_tier_is_not_an_upgrade() {
        let lossless = shape(Some(100), None, Some(871), false);
        let highest = shape(Some(80), None, Some(320), false);
        assert_eq!(
            Outcome::of_shapes(highest, lossless, None),
            Outcome::Upgraded
        );
        assert_eq!(
            Outcome::of_shapes(lossless, lossless, None),
            Outcome::NoHigherTier,
            "同一条流拿回来两次不算升级"
        );
        assert_eq!(
            Outcome::of_shapes(lossless, highest, None),
            Outcome::NoHigherTier,
            "拿回更差的流不算升级"
        );
        // 旧缓存没记分数、码率确实变高：实打实的提升。
        assert_eq!(
            Outcome::of_shapes(
                shape(None, None, Some(192), false),
                shape(Some(80), None, Some(320), false),
                None
            ),
            Outcome::Upgraded
        );
        // 旧缓存没记分数、换成同样 320k 的「极高」流：只是重新量了一遍档位，不算成功。
        assert_eq!(
            Outcome::of_shapes(
                shape(None, None, Some(320), false),
                shape(Some(80), None, Some(320), false),
                Some(80)
            ),
            Outcome::NoHigherTier,
            "同码率重写一遍文件不是升级"
        );
        // 旧标签是无损：按下限比，同档位的新流不算升级，哪怕码率更高。
        assert_eq!(
            Outcome::of_shapes(
                shape(None, Some(100), Some(300), false),
                shape(Some(100), None, Some(871), false),
                Some(100)
            ),
            Outcome::NoHigherTier
        );
        // 旧标签没写码率：证明不了新流更好，不算升级（换文件补档位另算）。
        assert_eq!(
            Outcome::of_shapes(
                shape(None, None, None, false),
                shape(Some(100), None, None, false),
                Some(100)
            ),
            Outcome::NoHigherTier
        );
        // 旧缓存没记分数、这次量出来 0 分（认不出）：不算已知档位，也不算升级。
        assert_eq!(
            Outcome::of_shapes(
                shape(None, None, Some(320), false),
                shape(Some(0), None, Some(320), false),
                None
            ),
            Outcome::NoHigherTier
        );
        // 试听换整曲：同档位也算，盘上那份文件确实变完整了。
        let preview = shape(Some(80), None, Some(320), true);
        let full = shape(Some(80), None, Some(320), false);
        assert_eq!(Outcome::of_shapes(preview, full, None), Outcome::Upgraded);
        // 整曲换成试听什么时候都不算升级（这条路本来就另有护栏挡着）。
        assert_eq!(
            Outcome::of_shapes(full, preview, None),
            Outcome::NoHigherTier
        );
    }

    /// 换文件比算升级宽一档：同码率也换一次把档位量出来，但不算升级。
    #[test]
    fn an_equal_bitrate_stream_is_replaced_only_to_measure_the_tier() {
        let legacy_high = with_label_tier(shape(None, None, Some(320), false), "极高");
        let same = shape(Some(80), None, Some(320), false);
        assert!(!stream_is_better(legacy_high, same, Some(80)));
        assert!(should_replace_stream(legacy_high, same, Some(80)));

        // 历史标签「极高 320k」按当前刻度只算「较高」：同一条 320k 流取回来，
        // 只是档名从「极高」变成「较高」，码率没变，不算升级。
        let remeasured = shape(Some(70), None, Some(320), false);
        assert!(!stream_is_better(legacy_high, remeasured, Some(70)));
        assert!(should_replace_stream(legacy_high, remeasured, Some(70)));

        // 同档位、更低码率：目标档位也够到了，但只是把 320k 换成 192k，不换。
        let worse_same_tier = shape(Some(80), None, Some(192), false);
        assert!(!stream_is_better(legacy_high, worse_same_tier, Some(80)));
        assert!(!should_replace_stream(
            legacy_high,
            worse_same_tier,
            Some(80)
        ));

        let lossless_at_lower_bitrate = shape(Some(100), None, Some(300), false);
        assert!(stream_is_better(
            legacy_high,
            lossless_at_lower_bitrate,
            Some(100)
        ));
        assert!(should_replace_stream(
            legacy_high,
            lossless_at_lower_bitrate,
            Some(100)
        ));

        // 已经记过档位的不再为了补测量而重写：只有真提升才换。
        let known = shape(Some(80), None, Some(320), false);
        assert!(!should_replace_stream(known, same, Some(80)));

        // 标签能证明无损下限时，同档无损流也不换：下限管着，不靠补测。
        let legacy_lossless = with_label_tier(shape(None, Some(100), Some(871), false), "无损");
        let lossless_again = shape(Some(100), None, Some(871), false);
        assert!(!stream_is_better(
            legacy_lossless,
            lossless_again,
            Some(110)
        ));
        assert!(!should_replace_stream(
            legacy_lossless,
            lossless_again,
            Some(110)
        ));
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
