//! 音质意图：配置里只存用户的选择，实际拿到哪一档由服务端决定。
//!
//! 四条对外行为：
//! * 意图归一化——空串、空白、`auto` / `best` 都是「自动」，未知值也按不设上限处理
//!   （与 libresoda 的 `preference_rank` 一致），免得用户手写一个错字就被锁进低档；
//! * 缓存标签——同一个意图永远落到同一个文件名，`auto` 与空串共享 `auto`；
//! * 不符判定——实际档位**低于**或**高于**所选档位时都要说出来；`auto` 永不提示，
//!   实际档位未知（旧缓存没记分数，或分数算成 0）也不提示。
//! * 升级判定——这份缓存该不该重新取流。与不符判定相反，档位未知不能算「不用重下」，
//!   只有旧标签能证明已达目标时才抑制（见 [`legacy_label_floor_rank`]）。
//!
//! 这里的档位分数直接沿用 libresoda 的 [`quality_rank`] / [`preference_rank`]：
//! 取流时的筛选就发生在那边，界面若另算一套，两边迟早会对同一个文件给出不同结论。

use libresoda::soda::quality::{preference_rank, quality_rank};
use serde::{Deserialize, Serialize};

/// 音质偏好来源：记录档位由谁决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualitySource {
    /// 用户在设置页或播放条上手动选择。
    User,
    /// 程序推导写入。
    Derived,
    /// 未知（旧配置未记录来源，按「可能是任一种」处理）。
    #[default]
    #[serde(other)]
    Unknown,
}

impl QualitySource {
    /// 「来源未知」在 i18n 词典中的稳定 key。
    pub const LABEL_UNKNOWN: &'static str = "来源未知";
    /// 「手动选择」在 i18n 词典中的稳定 key。
    pub const LABEL_USER: &'static str = "手动选择";
    /// 「程序推导」在 i18n 词典中的稳定 key。
    pub const LABEL_DERIVED: &'static str = "程序推导";

    /// 来源在配置中的原始字符串表示。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::User => "user",
            Self::Derived => "derived",
        }
    }

    /// 来源的稳定中文名（i18n 的 key）。
    ///
    /// 注：设置页视图为了配合 i18n 静态测试扫描器提取字面量，在 match 分支中显式
    /// 调用了 `.tr("...")` 字面量；本方法保留给核心层及其他不需要字面量扫描的场景。
    pub fn label_key(&self) -> &'static str {
        match self {
            Self::Unknown => Self::LABEL_UNKNOWN,
            Self::User => Self::LABEL_USER,
            Self::Derived => Self::LABEL_DERIVED,
        }
    }
}

impl std::fmt::Display for QualitySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 「自动」的存储写法。空串按它读，新写入一律写它。
pub const AUTO: &str = "auto";

/// 需要会员权益的档位分数下限（无损、杜比及更高）。
pub const MEMBERSHIP_MIN_RANK: i64 = 88;

/// 设置页与播放条共用的档位顺序（从高到低，也是菜单里的展示顺序）。
///
/// 新增档位时必须同时检查三处边界：[`TIERS`]、[`QualityIntent::label_key`] 与
/// [`tier_label`]——菜单能选、会显示名字、实际档位能被正确命名，缺一处就会出现
/// 「选中却看不到」或「显示同名却判降档」。设置页的批量升级也直接派生自这里。
///
/// libresoda 的取流筛选认识这些 key；服务端是否真的下发 Hi-Res / 杜比标签仍取决于
/// 账号权益与曲目可用性，候选不足时它会回退成全量候选（可能高于所选上限）。
pub const TIERS: [&str; 6] = ["hires", "lossless", "dolby", "highest", "medium", "low"];

/// 归一化后的音质意图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualityIntent {
    /// 不设上限：由服务端按账号权益与曲目可用性收敛。
    Auto,
    /// 一个具体档位（已小写化并归一成菜单写法）；未知值也走这里，按原样保留。
    Tier(String),
}

/// 把 libresoda 认识的档位别名归一成菜单里的规范写法。
///
/// 别名只该影响“怎么读旧配置 / 环境变量”，不该影响菜单高亮或缓存文件身份：
/// 否则 `hi-res` 与 `hires` 会显示同一个名字却不高亮同一行，还会各存一份缓存。
fn canonical_tier(value: &str) -> &str {
    match value {
        "hi_res" | "hi-res" => "hires",
        "flac" => "lossless",
        "dolby_atmos" => "dolby",
        "hq" | "high" => "highest",
        "standard" | "320k" => "medium",
        "128k" | "saver" => "low",
        other => other,
    }
}

impl QualityIntent {
    /// 从配置里的原始字符串解析。空串 / 空白 / `auto` / `best` → 自动；
    /// 已知别名归一成菜单写法（`standard` → `medium`，与 libresoda 的 70 分档一致），
    /// 未知档位按原样保留。
    pub fn parse(raw: &str) -> Self {
        let value = raw.trim().to_ascii_lowercase();
        if value.is_empty() || value == AUTO || value == "best" {
            Self::Auto
        } else {
            Self::Tier(canonical_tier(&value).to_string())
        }
    }

    /// 落盘写法：自动写 `auto`，具体档位写它自己。
    pub fn as_setting(&self) -> &str {
        match self {
            Self::Auto => AUTO,
            Self::Tier(tier) => tier,
        }
    }

    /// 缓存文件名后缀。自动固定 `auto`，与旧配置的空串落到同一个文件；
    /// 其余取意图本身，只保留文件名安全字符——换意图就换路径，命中的一定是同档位。
    pub fn cache_tag(&self) -> String {
        match self {
            Self::Auto => AUTO.to_string(),
            Self::Tier(tier) => {
                let cleaned: String = tier
                    .chars()
                    .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
                    .take(16)
                    .collect();
                if cleaned.is_empty() {
                    "custom".to_string()
                } else {
                    cleaned
                }
            }
        }
    }

    /// 这个意图允许的最高档位分数；`None` = 不设上限。
    pub fn max_rank(&self) -> Option<i64> {
        preference_rank(self.as_setting())
    }

    /// 该意图是否依赖账号会员权益（无损及更高、杜比全景声）。
    ///
    /// 仅用于批量升级这类会消耗流量的操作预检；单个播放档位仍允许用户选择，
    /// 实际能不能拿到由服务端按账号权益与曲目可用性决定。
    pub fn requires_membership(&self) -> bool {
        self.max_rank()
            .is_some_and(|rank| rank >= MEMBERSHIP_MIN_RANK)
    }

    /// 档位的稳定中文名（i18n 的 key）；认不出的档位返回 `None`，由调用方原样显示。
    ///
    /// 放在核心层是因为设置页、播放条菜单与状态行都要它：三处各写一遍 match，
    /// 迟早会有一处漏掉新档位。
    pub fn label_key(&self) -> Option<&'static str> {
        match self {
            Self::Auto => Some("自动"),
            Self::Tier(tier) => match tier.as_str() {
                "hires" | "hi_res" | "hi-res" => Some("Hi-Res 无损"),
                "lossless" | "flac" => Some("无损"),
                "dolby" | "dolby_atmos" => Some("杜比全景声"),
                "highest" | "hq" | "high" => Some("极高"),
                "medium" | "standard" | "320k" => Some("较高"),
                "low" | "128k" | "saver" => Some("流畅"),
                _ => None,
            },
        }
    }
}

/// 认得出来的档位分数才算一次测量。
///
/// `quality_rank` 在档位标签与码率都对不上时给 0：那不是「比谁都低」，是「没认出来」。
/// 拿它当分数比大小，就是向用户断言一件查不到的事（#96）。负数同样不在这套刻度上。
/// 两侧判定与展示共用这一道闸，免得对同一个文件给出不同结论。
fn readable_rank(actual: Option<i64>) -> Option<i64> {
    actual.filter(|rank| *rank > 0)
}

/// 实际档位是否低于所选档位。
///
/// 档位未知时返回 `false`——旧缓存没记分数（`None`），或记了 0 分（认不出来，
/// 见 `readable_rank`）：不知道就不能断言降档。
pub fn is_downgraded(intent: &QualityIntent, actual: Option<i64>) -> bool {
    let (Some(max), Some(actual)) = (intent.max_rank(), readable_rank(actual)) else {
        return false;
    };
    actual < max
}

/// 实际档位是否高于所选档位。
///
/// 这是 [`is_downgraded`] 的另一侧（issue #82）：取流先按上限过滤候选，**过滤后一条不剩
/// 就退回全部候选**再取最优，于是「所选档位不在这条流的候选集里」会给出一条更高的档位。
/// 试听片段最容易碰到——片段往往只下发少数几条流，且没有低于「流畅」的档位。
///
/// 档位未知（`None` 或 0 分，见 `readable_rank`）时返回 `false`：不知道就不能断言。
pub fn is_above_selected(intent: &QualityIntent, actual: Option<i64>) -> bool {
    let (Some(max), Some(actual)) = (intent.max_rank(), readable_rank(actual)) else {
        return false;
    };
    actual > max
}

/// 实际档位与所选意图不符的方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierMismatch {
    /// 实际低于所选：拿到的比要的差。
    Below,
    /// 实际高于所选：所选档位不在候选集里，服务端回了更高一档。
    Above,
}

impl TierMismatch {
    /// 说明不符的文案模板（i18n 的 key，两个占位符依次是所选与实际）。
    ///
    /// 状态行与档位菜单共用这一条：同一件事两处各写一句，迟早会漂成两种叫法。
    /// 方向本身仍由 [`visible_tier_mismatch`] 判定，这里只管怎么说。
    pub fn notice_key(self) -> &'static str {
        match self {
            Self::Below => "已降档：所选 {}，实际 {}",
            Self::Above => "高于所选：所选 {}，实际 {}",
        }
    }
}

/// 是否值得向用户说明「实际与所选不符」，并给出方向。
///
/// 三个不说的条件：意图为「自动」（不设上限）、实际档位未知（旧缓存没有记录分数，
/// 或分数算成 0，见 `readable_rank`）、以及两侧落在同一个显示名上
/// ——后者照实说只会得到「所选 流畅，实际 流畅」，读起来像没不符。判定不落盘，每次现算：
/// 用户改了意图，旧结论就不该继续成立。
///
/// 徽章、状态行与档位菜单都只读这一个出处，三边共用同一套刻度与同一道显示名闸。
pub fn visible_tier_mismatch(intent: &QualityIntent, actual: Option<i64>) -> Option<TierMismatch> {
    let actual = readable_rank(actual)?;
    let direction = if is_downgraded(intent, Some(actual)) {
        TierMismatch::Below
    } else if is_above_selected(intent, Some(actual)) {
        TierMismatch::Above
    } else {
        return None;
    };
    // 两侧同名（例如所选 50、实际 65 都显示「流畅」）就不说：照实读出来是
    // 「所选 流畅，实际 流畅」，等于没说。
    if intent.label_key() == Some(tier_label(actual)) {
        return None;
    }
    Some(direction)
}

/// 不符时两侧的显示名，供常驻承载面使用。
///
/// 徽章与状态行只需要方向；档位菜单要一次写全「所选 X，实际 Y」，否则用户还是得
/// 把两处的名字自己比（#97）。两侧名字与判定同源：名字走这里，菜单不再自己拼一套刻度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierComparison {
    /// 实际低于还是高于所选。
    pub direction: TierMismatch,
    /// 所选档位的显示名（i18n 的 key）。
    pub selected: &'static str,
    /// 实际档位的显示名（i18n 的 key）。
    pub actual: &'static str,
}

/// 不符时给出方向与两侧显示名；自动、档位未知、两侧同名时返回 `None`。
pub fn visible_tier_comparison(
    intent: &QualityIntent,
    actual: Option<i64>,
) -> Option<TierComparison> {
    let direction = visible_tier_mismatch(intent, actual)?;
    Some(TierComparison {
        direction,
        selected: intent.label_key()?,
        actual: tier_label(actual?),
    })
}

/// 获取音质意图在给定账号 VIP 状态下的目标档位分数。
///
/// 具体档位返回其对应的分数（如 Hi-Res 110、无损 100、杜比 88、极高 80、
/// 较高 70、流畅 50）；「自动」在意图上不设上限，但在已知为 VIP 时目标为无损（100）。
/// 若在「自动」下且非 VIP 或 VIP 状态未知，则返回 `None`（不指定确定目标）。
pub fn target_rank(intent: &QualityIntent, is_vip: Option<bool>) -> Option<i64> {
    match intent {
        QualityIntent::Tier(_) => intent.max_rank(),
        QualityIntent::Auto => {
            if is_vip.unwrap_or(false) {
                Some(100)
            } else {
                None
            }
        }
    }
}

/// 旧三字段缓存的档位**下限**：标签能证明「至少无损」时给出无损的分数，否则给 `None`。
///
/// 只有这一条证得起来。两代旧写入者写「无损」的条件分别是
/// `quality_rank >= 100`（按分数命名的那代）与
/// `is_lossless(info) || format == "flac" || quality 含 "lossless"`（按码率命名的那代），
/// 而这三者在当前刻度里每一条都评到 100 及以上（libresoda 的 `quality_rank`：
/// 无损标签、flac/alac/wav 格式、码率 ≥900 都是 100）——所以「无损」开头的标签是一个下限，
/// 不是一个猜测。更低的标签证明不了任何下限：老边界里「极高」= 码率 ≥300
/// （按当前刻度可能只有 70），「标准」横跨 1..65 好几种流，按显示名倒推分数就是把猜测当测量。
///
/// 因此它只有两个用途，都是**不做**某件事：抑制一次注定没有收益的重下（升级判定），
/// 以及拦住「拿一条不比它好的流把旧文件顶掉」（换文件判定，见 `session::is_better_stream`）。
/// 它不作为实际档位展示，不参与降档判定、徽章与状态行——那些地方档位未知就说未知。
/// 档名跟着 [`tier_label`] 走：改名时这条判定跟着变，不会悄悄失效。
pub fn legacy_label_floor_rank(label: &str) -> Option<i64> {
    if !label.trim().starts_with(tier_label(100)) {
        return None;
    }
    preference_rank("lossless")
}

/// 旧 sidecar 的档位文本里记着的码率（kbps），如「极高 320k」→ 320；没写码率则 `None`。
///
/// 这是当年 `describe_quality` 直接写进文件的原始数字，不是档位：档位未知时，它是唯一
/// 能诚实回答「新取的流是不是比本地这份更差」的依据（用在 `session::is_better_stream`）。
pub fn legacy_label_bitrate_kbps(label: &str) -> Option<i64> {
    let tail = label.trim().rsplit(' ').next()?;
    let digits = tail.trim_end_matches('k').trim_end_matches('K');
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i64>().ok().filter(|bitrate| *bitrate > 0)
}

/// 这份缓存要不要重新取一次更高的流：档位已知就比分数，未知（旧三字段）看标签能否证明已达目标。
///
/// 未知不再算「不用重下」（#89 要修的就是这一条）：既然证明不了达标，就重下一次，
/// 取完流会把实际档位写进 sidecar，之后走已知分支。唯一例外是标签已经证明达标，
/// 那次重下注定没有收益。
pub fn cache_needs_higher_stream(actual: Option<i64>, label: &str, target: i64) -> bool {
    match actual {
        Some(rank) => rank < target,
        None => !legacy_label_floor_rank(label).is_some_and(|floor| floor >= target),
    }
}

/// 命中缓存时，当前档位是否低于所选（可升级判定）。
///
/// * 所选为具体档位：实际档位低于该档位即为真；
/// * 所选为「自动」且为 VIP：实际档位低于无损（100）即为真（非会员时期缓存的低档流在会员后可升级）；
/// * 实际档位未知（旧三字段缓存）时看 `label`：标签证明不了达标就要重下
///   （见 [`cache_needs_higher_stream`]），不再一律判「不需要升级」。
pub fn is_tier_below_selected(
    actual: Option<i64>,
    label: &str,
    intent: &QualityIntent,
    is_vip: Option<bool>,
) -> bool {
    let Some(target) = target_rank(intent, is_vip) else {
        return false;
    };
    cache_needs_higher_stream(actual, label, target)
}

/// 批量升级是否要求先确认账号权益。
///
/// 设置页的禁用态与执行处的拦截必须同判：一处放行一处拒绝，按钮就是骗人的。
/// 认不出权益（账号没加载，`is_vip == None`）也算不放行——那时既取不到会员档位的流，
/// 也记不下「没有更高档位」（见 [`can_record_unavailable`]），跑一轮只会整曲下一遍再丢掉。
pub fn batch_upgrade_needs_confirmed_entitlement(
    intent: &QualityIntent,
    is_vip: Option<bool>,
) -> bool {
    intent.requires_membership() && is_vip != Some(true)
}

/// 这个目标档位的「没有更高流」结论能否持久化。
///
/// 账号信息只有一个 VIP 布尔：低于会员门槛的档位无需权益，无损（100）能由 VIP 确认；
/// 杜比与 Hi-Res 需要更高等级，布尔分不清，宁可下次再探，也不能把权益不足写成曲库没有。
pub fn can_record_unavailable(target_rank: Option<i64>, is_vip: Option<bool>) -> bool {
    match target_rank {
        Some(rank) if rank < MEMBERSHIP_MIN_RANK => true,
        Some(rank) if preference_rank("lossless") == Some(rank) => is_vip == Some(true),
        _ => false,
    }
}

/// 一条流（`DownloadInfo`）的档位分数，用于写进缓存边车。
pub fn stream_rank(info: &libresoda::soda::types::DownloadInfo) -> i64 {
    quality_rank(&info.quality, &info.format, info.bitrate)
}

/// 实际档位的显示名。
///
/// 必须与 [`is_downgraded`] 用同一套刻度：两者若各按各的边界算，就会出现
/// 「徽章写着极高、降档判定却按较高比」这种自相矛盾的显示。
///
/// 分数与显示名的对应（每个区间都对应 libresoda 里一个不能互相混淆的档位）：
///
/// | 分数 | 显示名 | 说明 |
/// | --- | --- | --- |
/// | >= 110 | Hi-Res 无损 | `quality_rank` 给 Hi-Res 无损 110 |
/// | 100..110 | 无损 | 普通无损（100） |
/// | 90..100 | Hi-Res | 有 Hi-Res 标签但没有无损格式；不是真无损，不能叫「无损」 |
/// | 88..90 | 杜比全景声 | 杜比 / Atmos / Spatial 固定 88 |
/// | 80..88 | 极高 | 极高 / HQ（80） |
/// | 70..80 | 较高 | 较高 / 320k（70） |
/// | 50..70 | 流畅 | 50 分档与码率兜底 65 / 55；**不能叫「标准」**——那是 libresoda 意图的 70 分档 |
/// | 1..50 | 省流 | 低码率兜底 20 / 10 |
/// | 0 | 未知 | 没有可用信息 |
///
/// 区间按 [`preference_rank`] 的边界切开：**所选档位的显示名与实际档位在数值不同时
/// 必须不同名**。一旦把 `hires` / `dolby` 加进 [`TIERS`]，88 和 80 若还都叫「极高」，
/// 选中极高、实际 88 就会重现「显示同名却判降档」。新增档位时必须同步检查
/// [`TIERS`]、[`QualityIntent::label_key`] 与本函数三处边界。
pub fn tier_label(rank: i64) -> &'static str {
    if rank >= 110 {
        "Hi-Res 无损"
    } else if rank >= 100 {
        "无损"
    } else if rank >= 90 {
        "Hi-Res"
    } else if rank >= 88 {
        "杜比全景声"
    } else if rank >= 80 {
        "极高"
    } else if rank >= 70 {
        "较高"
    } else if rank >= 50 {
        "流畅"
    } else if rank > 0 {
        "省流"
    } else {
        "未知"
    }
}

/// [`tier_label`] 可能返回的全部显示名。
///
/// UI 把实际档位翻成当前语言时按这张表整体替换；顺序按长度从长到短，
/// 避免短名先替换掉长名的一部分（如「无损」吃掉「Hi-Res 无损」）。
pub const TIER_LABELS: [&str; 9] = [
    "Hi-Res 无损",
    "Hi-Res",
    "杜比全景声",
    "无损",
    "极高",
    "较高",
    "流畅",
    "省流",
    "未知",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_empty_auto_and_unknown() {
        for raw in ["", "   ", "auto", " AUTO ", "best"] {
            assert_eq!(QualityIntent::parse(raw), QualityIntent::Auto, "{raw:?}");
        }
        assert_eq!(
            QualityIntent::parse(" LossLess "),
            QualityIntent::Tier("lossless".into())
        );
        // 未知值按「不设上限」处理，与 libresoda 的 preference_rank 一致
        assert_eq!(QualityIntent::parse("ultra").max_rank(), None);
    }

    #[test]
    fn auto_and_empty_share_one_cache_tag_and_intents_do_not() {
        assert_eq!(
            QualityIntent::parse("").cache_tag(),
            QualityIntent::parse("auto").cache_tag()
        );
        assert_ne!(
            QualityIntent::parse("lossless").cache_tag(),
            QualityIntent::parse("highest").cache_tag()
        );
        // 文件名安全：路径分隔符与点号不能漏进名字
        assert_eq!(
            QualityIntent::parse("../../x").cache_tag(),
            "x".to_string(),
            "不安全字符被过滤"
        );
    }

    #[test]
    fn downgrade_only_when_actual_is_below_intent() {
        let lossless = QualityIntent::parse("lossless");
        // 所选无损 + 实际极高（80）→ 降档
        assert!(is_downgraded(&lossless, Some(80)));
        // 实际就是无损 → 不算
        assert!(!is_downgraded(&lossless, Some(100)));
        // 自动不设上限 → 任何实际档位都不算降档
        assert!(!is_downgraded(&QualityIntent::Auto, Some(0)));
        // 所选较低 + 实际更高 → 不是降档
        assert!(!is_downgraded(&QualityIntent::parse("low"), Some(100)));
        // 旧缓存没记档位 → 未知，不算降档
        assert!(!is_downgraded(&lossless, None));
        // 记了 0 分 → 认不出来，同样是未知，不算降档（#96）
        assert!(!is_downgraded(&lossless, Some(0)));
    }

    #[test]
    fn setting_writes_auto_never_empty() {
        assert_eq!(QualityIntent::parse("").as_setting(), AUTO);
        assert_eq!(QualityIntent::parse("lossless").as_setting(), "lossless");
    }

    /// 档位名只有一处映射：设置页、播放条菜单与状态行都查它，认不出的不硬翻。
    #[test]
    fn label_key_covers_known_tiers_and_returns_none_for_unknown() {
        assert_eq!(QualityIntent::Auto.label_key(), Some("自动"));
        for (tier, label) in [
            ("hires", "Hi-Res 无损"),
            ("hi-res", "Hi-Res 无损"),
            ("lossless", "无损"),
            ("flac", "无损"),
            ("dolby", "杜比全景声"),
            ("dolby_atmos", "杜比全景声"),
            ("highest", "极高"),
            ("medium", "较高"),
            ("low", "流畅"),
        ] {
            assert_eq!(
                QualityIntent::parse(tier).label_key(),
                Some(label),
                "{tier}"
            );
        }
        assert_eq!(QualityIntent::parse("ultra").label_key(), None);
    }

    /// 50 分档不能借用 libresoda 意图里的 `standard`（70）之名，否则降档提示会读反。
    #[test]
    fn low_tier_label_is_not_named_after_libresoda_standard() {
        assert_eq!(preference_rank("standard"), Some(70));
        assert_eq!(preference_rank("low"), Some(50));
        assert_eq!(QualityIntent::parse("low").label_key(), Some("流畅"));
        assert_eq!(tier_label(50), "流畅");
        assert_eq!(tier_label(70), "较高");
        assert_ne!(tier_label(50), "标准");
        assert_eq!(
            QualityIntent::parse("standard"),
            QualityIntent::Tier("medium".into()),
            "standard 与 medium 同属 70 分档，必须归一到同一行"
        );
        assert_eq!(QualityIntent::parse("standard").label_key(), Some("较高"));
    }

    /// 四种判定：实际高于所选 / 等于 / 低于 / 意图为自动。
    ///
    /// 「高于所选」不是降档：设置里选了上限，取流那一段却因为候选集里没有
    /// 等于或低于上限的流而回退到全量，最终给出一条更高的档位（issue #82）。
    #[test]
    fn mismatch_reports_both_directions_but_never_auto() {
        let low = QualityIntent::parse("low");

        // 实际高于所选：选「流畅」拿到 258k 的「较高」
        let above = visible_tier_mismatch(&low, Some(70));
        assert_eq!(above, Some(TierMismatch::Above));
        assert!(is_above_selected(&low, Some(70)));
        assert!(!is_downgraded(&low, Some(70)), "高于所选不能算降档");

        // 实际等于所选
        let equal = visible_tier_mismatch(&low, Some(50));
        assert_eq!(equal, None);
        assert!(!is_above_selected(&low, Some(50)));

        // 实际低于所选
        let below = visible_tier_mismatch(&low, Some(20));
        assert_eq!(below, Some(TierMismatch::Below));

        // 意图为自动：不设上限，任何实际档位都不提示
        for actual in [0, 20, 50, 70, 100, 110] {
            let auto = visible_tier_mismatch(&QualityIntent::Auto, Some(actual));
            assert_eq!(auto, None, "自动不提示：实际 {actual}");
            assert!(!is_above_selected(&QualityIntent::Auto, Some(actual)));
        }

        // 实际档位未知（旧缓存没有记录分数）：不知道就不能断言
        let unknown = visible_tier_mismatch(&low, None);
        assert_eq!(unknown, None);
        assert!(!is_above_selected(&low, None));
    }

    /// #96：分数 0 是 `quality_rank` 认不出这条流，不是最低档。
    ///
    /// 认不出来就不该说「不符」——档位菜单、徽章、状态行共用这一道闸，
    /// 任何承载面都不出现「不符·未知」。两侧判定本身也不认 0 分，不是只有展示层挡了一道。
    #[test]
    fn unreadable_zero_rank_is_unknown_not_a_mismatch() {
        for tier in TIERS {
            let intent = QualityIntent::parse(tier);
            assert_eq!(
                visible_tier_mismatch(&intent, Some(0)),
                None,
                "所选 {tier}、实际分数 0 不能说成不符"
            );
            assert!(
                !is_downgraded(&intent, Some(0)),
                "所选 {tier}、实际 0 分不算降档"
            );
            assert!(
                !is_above_selected(&intent, Some(0)),
                "所选 {tier}、实际 0 分不算高于所选"
            );
        }
        assert_eq!(visible_tier_mismatch(&QualityIntent::Auto, Some(0)), None);
        assert_eq!(
            visible_tier_comparison(&QualityIntent::parse("lossless"), Some(0)),
            None
        );
    }

    /// 常驻承载面一次读全两侧：方向与名字同源，菜单不再自己拼一套刻度。
    #[test]
    fn comparison_names_both_sides_for_the_persistent_surface() {
        let low = QualityIntent::parse("low");

        let above = visible_tier_comparison(&low, Some(70)).expect("流畅 拿到较高应说明");
        assert_eq!(above.direction, TierMismatch::Above);
        assert_eq!(above.selected, "流畅");
        assert_eq!(above.actual, "较高");
        assert_eq!(above.direction.notice_key(), "高于所选：所选 {}，实际 {}");

        let below = visible_tier_comparison(&QualityIntent::parse("medium"), Some(50))
            .expect("较高 拿到流畅应说明");
        assert_eq!(below.direction, TierMismatch::Below);
        assert_eq!(below.selected, "较高");
        assert_eq!(below.actual, "流畅");
        assert_eq!(below.direction.notice_key(), "已降档：所选 {}，实际 {}");

        // 自动、档位未知、两侧同名：这一行不出现
        assert_eq!(
            visible_tier_comparison(&QualityIntent::Auto, Some(70)),
            None
        );
        assert_eq!(visible_tier_comparison(&low, None), None);
        assert_eq!(visible_tier_comparison(&low, Some(50)), None);
    }

    /// 不符提示按显示名去重：分数不同但两侧同名就不提示。
    ///
    /// 抬档确实会撞名（50..70 都显示「流畅」），降档在 #85 拆档后不会（50 流畅、
    /// 更低叫省流）。方向与「自动」的口径由 `mismatch_reports_both_directions_but_never_auto`
    /// 负责，这里只钉显示名这一道闸。
    #[test]
    fn visible_mismatch_follows_display_names() {
        assert_eq!(tier_label(65), "流畅");
        let saver = QualityIntent::parse("128k");
        assert!(is_above_selected(&saver, Some(65)), "65 高于所选上限 50");
        let same_label = visible_tier_mismatch(&saver, Some(65));
        assert_eq!(same_label, None, "所选与实际同名就不提示");

        // 显示名不同就提示：所选 较高(70)，实际 流畅(50)
        let medium = QualityIntent::parse("medium");
        let medium_to_low = visible_tier_mismatch(&medium, Some(50));
        assert_eq!(medium_to_low, Some(TierMismatch::Below));
    }

    /// 每一档在实际档位明显更高时都要报「高于所选」。
    ///
    /// 认不出的意图不设上限、因此从不报，这条已由
    /// `normalizes_empty_auto_and_unknown` 钉住，不在这里重复。
    #[test]
    fn every_tier_reports_above_when_a_much_higher_stream_is_delivered() {
        for tier in TIERS {
            let intent = QualityIntent::parse(tier);
            let cap = intent.max_rank().expect("菜单档位必须有分数");
            // Hi-Res 无损 是菜单里的最高档，没有比它更高的实际档位
            if cap >= 110 {
                continue;
            }
            let above = visible_tier_mismatch(&intent, Some(110));
            assert_eq!(above, Some(TierMismatch::Above), "所选 {tier} 拿到 Hi-Res");
            // 高于所选绝不算降档
            assert!(!is_downgraded(&intent, Some(110)));
        }
    }

    /// 菜单里的每一档都必须同时有分数与显示名，否则会出现能选却显示不出名字的档位。
    #[test]
    fn every_menu_tier_has_preference_rank_and_label() {
        for tier in TIERS {
            let intent = QualityIntent::parse(tier);
            assert!(intent.max_rank().is_some(), "{tier} 缺少 preference_rank");
            assert!(intent.label_key().is_some(), "{tier} 缺少 label_key");
        }
    }

    /// 每个可选档位在自己的分数边界上显示的名字必须与菜单里的名字一致。
    #[test]
    fn tier_label_matches_each_menu_tier_at_its_preference_rank() {
        for tier in TIERS {
            let intent = QualityIntent::parse(tier);
            let rank = intent.max_rank().expect("菜单档位必须有分数");
            let label = intent.label_key().expect("菜单档位必须有显示名");
            assert_eq!(tier_label(rank), label, "所选 {tier}（{rank}）");
        }
    }

    /// `TIER_LABELS` 必须恰好是 `tier_label` 的输出集合；新增档名时漏登记会被这里抓住。
    #[test]
    fn tier_label_values_match_the_documented_label_set() {
        use std::collections::BTreeSet;

        let mut seen = BTreeSet::new();
        for rank in -10..=1000 {
            let label = tier_label(rank);
            assert!(
                TIER_LABELS.contains(&label),
                "tier_label({rank}) = {label} 未登记到 TIER_LABELS"
            );
            seen.insert(label);
        }
        let expected: BTreeSet<_> = TIER_LABELS.into_iter().collect();
        assert_eq!(seen, expected, "TIER_LABELS 与 tier_label 的值域不一致");
    }

    /// 表驱动覆盖「所选 N、实际 M」：只要判了降档，两个显示名就不能同名。
    ///
    /// 这是 issue #85 的回归测试：旧实现里 88 和 80 都叫「极高」，
    /// 若把 `dolby` 加进菜单，选中 highest 拿到 88 就会显示同名却判降档。
    #[test]
    fn downgraded_pairs_never_share_a_display_name() {
        for tier in TIERS {
            let intent = QualityIntent::parse(tier);
            let selected_name = intent.label_key().expect("菜单档位必须有显示名");
            let selected_rank = intent.max_rank().expect("菜单档位必须有分数");

            // 同分必须同名：徽章显示的就是菜单选的那一档。
            assert_eq!(tier_label(selected_rank), selected_name, "所选 {tier}");

            // 遍历整个可能分数区间，而不是硬编码当前 libresoda 的 12 个返回值：
            // 上游将来新增分数时，这里仍会覆盖到。
            for actual in 0..=200 {
                let downgraded = is_downgraded(&intent, Some(actual));
                let actual_name = tier_label(actual);
                assert!(
                    !(downgraded && actual_name == selected_name),
                    "所选 {tier}（{selected_rank}）实际 {actual}，判降档却同名「{actual_name}」"
                );
            }
        }
    }

    #[test]
    fn aliases_normalize_to_menu_tiers() {
        for (alias, canonical) in [
            ("hi-res", "hires"),
            ("hi_res", "hires"),
            ("flac", "lossless"),
            ("dolby_atmos", "dolby"),
            ("hq", "highest"),
            ("high", "highest"),
            ("standard", "medium"),
            ("320k", "medium"),
            ("128k", "low"),
            ("saver", "low"),
        ] {
            let intent = QualityIntent::parse(alias);
            assert_eq!(
                intent,
                QualityIntent::Tier(canonical.to_string()),
                "别名 {alias} 应归一为 {canonical}"
            );
            assert_eq!(intent.as_setting(), canonical, "别名 {alias} 的落盘写法");
            assert_eq!(intent.cache_tag(), canonical, "别名 {alias} 的缓存 tag");
        }
    }

    #[test]
    fn membership_required_for_lossless_and_above() {
        for (tier, expected) in [
            ("hires", true),
            ("lossless", true),
            ("dolby", true),
            ("highest", false),
            ("medium", false),
            ("low", false),
        ] {
            assert_eq!(
                QualityIntent::parse(tier).requires_membership(),
                expected,
                "{tier}"
            );
        }
        assert!(!QualityIntent::Auto.requires_membership());
        assert!(!QualityIntent::parse("ultra").requires_membership());
    }

    /// 批量升级的权益闸：认不出账号（`None`）与「非会员」一样挡住会员档位，会员才放行；
    /// 不依赖权益的档位与「自动」不受此限。设置页与执行处都读这一条。
    #[test]
    fn batch_upgrade_gate_opens_only_for_confirmed_vip() {
        for tier in ["hires", "lossless", "dolby"] {
            let intent = QualityIntent::parse(tier);
            assert!(
                batch_upgrade_needs_confirmed_entitlement(&intent, None),
                "{tier}：账号没加载就白花流量，挡住"
            );
            assert!(
                batch_upgrade_needs_confirmed_entitlement(&intent, Some(false)),
                "{tier}：非会员挡住"
            );
            assert!(
                !batch_upgrade_needs_confirmed_entitlement(&intent, Some(true)),
                "{tier}：会员放行"
            );
        }
        for tier in ["highest", "medium", "low"] {
            let intent = QualityIntent::parse(tier);
            for is_vip in [None, Some(false), Some(true)] {
                assert!(
                    !batch_upgrade_needs_confirmed_entitlement(&intent, is_vip),
                    "{tier} 不依赖权益，{is_vip:?} 都要放行"
                );
            }
        }
        assert!(!batch_upgrade_needs_confirmed_entitlement(
            &QualityIntent::Auto,
            None
        ));
    }

    /// 负结果只在能确认账号权益时落盘：非会员档位无需权益，无损需要 VIP，
    /// 更高档位无法由一个布尔确认，宁可下次再探。
    #[test]
    fn unavailable_results_only_recorded_with_confirmed_entitlement() {
        assert!(can_record_unavailable(Some(50), None));
        assert!(can_record_unavailable(Some(80), Some(false)));
        assert!(can_record_unavailable(Some(100), Some(true)));
        assert!(!can_record_unavailable(Some(100), Some(false)));
        assert!(!can_record_unavailable(Some(100), None));
        // Hi-Res 与杜比需要更高等级，VIP 布尔分不清，不写负结果。
        assert!(!can_record_unavailable(Some(110), Some(true)));
        assert!(!can_record_unavailable(Some(88), Some(true)));
        assert!(!can_record_unavailable(None, Some(true)));
    }

    /// 显示名与降档判定必须同源：真实流样本上，徽章写的档位就是判定用的那一档。
    ///
    /// 这两者曾经各算一套（命名字按码率阈值、判定按 libresoda 档位分数），
    /// 于是会出现「所选 极高，实际 极高」这种同词却仍判降档的自相矛盾。
    #[test]
    fn tier_label_and_downgrade_agree_on_real_stream_samples() {
        use libresoda::soda::types::DownloadInfo;

        let info = |quality: &str, format: &str, bitrate: i64| DownloadInfo {
            quality: quality.to_string(),
            format: format.to_string(),
            bitrate,
            ..Default::default()
        };

        // (样本, 期望显示档位, 期望所选无损时算不算降档)
        let cases = [
            (info("hires", "flac", 871_000), "Hi-Res 无损", false),
            (info("lossless", "flac", 871_000), "无损", false),
            (info("hires", "m4a", 320_000), "Hi-Res", true),
            (info("dolby", "m4a", 320_000), "杜比全景声", true),
            (info("highest", "m4a", 320_000), "极高", true),
            (info("higher", "m4a", 320_000), "较高", true),
            // libresoda 把实际流里的 `standard` 评为 50（与意图 `standard`=70 不同），
            // 显示名用 50 分档的「流畅」，不复用意图档的「标准」。
            (info("standard", "m4a", 128_000), "流畅", true),
            (info("low", "m4a", 96_000), "省流", true),
            (info("", "m4a", 96_000), "省流", true),
            // 标签与码率都认不出来 → 0 分 → 显示「未知」。未知不是一档更低的音质，
            // 不能判降档（#96）：这条样本原先钉的正是那个误报。
            (info("", "m4a", 0), "未知", false),
            // 码率与标签冲突时以档位分数为准，不再是「码率超过 300 就叫极高」
            (info("higher", "m4a", 320_000), "较高", true),
        ];
        for (sample, expected_name, expected_downgraded) in cases {
            let rank = stream_rank(&sample);
            assert_eq!(
                tier_label(rank),
                expected_name,
                "样本 quality={} bitrate={} 的显示名",
                sample.quality,
                sample.bitrate
            );
            assert_eq!(
                is_downgraded(&QualityIntent::parse("lossless"), Some(rank)),
                expected_downgraded,
                "样本 quality={} bitrate={} 的降档判定",
                sample.quality,
                sample.bitrate
            );
        }
    }

    #[test]
    fn tier_below_selected_and_upgrade_decision() {
        let lossless = QualityIntent::parse("lossless");
        let highest = QualityIntent::parse("highest");
        let auto = QualityIntent::Auto;

        // 具体档位（无损，100）：低于 100 即可升级，等于或高于不算
        assert!(is_tier_below_selected(
            Some(80),
            "流畅 128k",
            &lossless,
            None
        ));
        assert!(is_tier_below_selected(
            Some(50),
            "流畅 128k",
            &lossless,
            Some(false)
        ));
        assert!(!is_tier_below_selected(
            Some(100),
            "无损 871k",
            &lossless,
            None
        ));
        assert!(!is_tier_below_selected(
            Some(110),
            "Hi-Res 无损 871k",
            &lossless,
            None
        ));

        // 具体档位（极高，80）：
        assert!(is_tier_below_selected(
            Some(70),
            "较高 320k",
            &highest,
            None
        ));
        assert!(!is_tier_below_selected(
            Some(80),
            "极高 320k",
            &highest,
            None
        ));
        assert!(!is_tier_below_selected(
            Some(100),
            "无损 871k",
            &highest,
            None
        ));

        // 自动模式：
        // 1. VIP 用户：目标收敛到无损（100），非会员时期缓存的 50/70/80 均判断为可升级
        assert!(is_tier_below_selected(
            Some(50),
            "流畅 128k",
            &auto,
            Some(true)
        ));
        assert!(is_tier_below_selected(
            Some(80),
            "极高 320k",
            &auto,
            Some(true)
        ));
        assert!(!is_tier_below_selected(
            Some(100),
            "无损 871k",
            &auto,
            Some(true)
        ));
        assert!(!is_tier_below_selected(
            None,
            "无损 871k",
            &auto,
            Some(true)
        ));

        // 2. 非 VIP 或未知：不指定上限，不盲目判断为低于所选
        assert!(!is_tier_below_selected(
            Some(50),
            "流畅 128k",
            &auto,
            Some(false)
        ));
        assert!(!is_tier_below_selected(Some(50), "流畅 128k", &auto, None));

        // 目标分数与直接比较测试
        assert_eq!(target_rank(&lossless, None), Some(100));
        assert_eq!(target_rank(&highest, None), Some(80));
        assert_eq!(target_rank(&auto, Some(true)), Some(100));
        assert_eq!(target_rank(&auto, Some(false)), None);
        assert_eq!(target_rank(&auto, None), None);

        assert!(cache_needs_higher_stream(Some(70), "无损 871k", 100));
        assert!(!cache_needs_higher_stream(Some(100), "流畅 128k", 100));
        assert!(!cache_needs_higher_stream(Some(110), "流畅 128k", 100));
        // 档位未知时按标签判定，不再恒为 false（#89 修的就是这一条）
        assert!(cache_needs_higher_stream(None, "极高 320k", 100));
    }

    /// #89：档位未知的旧缓存要能参与升级，只有标签证明达标才抑制重下。
    #[test]
    fn unknown_rank_cache_needs_redownload_unless_label_proves_lossless() {
        // 未知 + 证明不了下限 → 重下（旧行为是恒 false，存量缓存永远不参与升级）
        for label in ["极高 320k", "标准 128k", "流畅 128k", "较高 256k", "未知"] {
            assert!(
                cache_needs_higher_stream(None, label, 100),
                "档位未知的「{label}」不能假装已达目标"
            );
        }
        // 未知 + 标签证明至少无损 → 目标不高于无损时不必重下（注定没有收益的一次流量）
        assert!(!cache_needs_higher_stream(None, "无损 871k", 100));
        assert!(!cache_needs_higher_stream(None, "无损 871k", 80));
        // 但 Hi-Res 目标（110）仍然要探：标签只证明到无损，无损标签里也混过 110 的流
        assert!(cache_needs_higher_stream(None, "无损 871k", 110));
        // 已知分数时标签不参与判定，仍按分数比
        assert!(!cache_needs_higher_stream(Some(100), "流畅 128k", 100));
        assert!(cache_needs_higher_stream(Some(70), "无损 871k", 100));
    }

    /// 旧标签的下限只认「无损」：更低的标签在两代旧刻度里边界不同，证不出分数。
    #[test]
    fn legacy_label_floor_only_trusts_lossless() {
        assert_eq!(legacy_label_floor_rank("无损 871k"), Some(100));
        assert_eq!(legacy_label_floor_rank("无损"), Some(100));
        // 早期按码率命名的档位：300k 以上一律写「极高」，按当前刻度可能只有 70。
        assert_eq!(legacy_label_floor_rank("极高 320k"), None);
        assert_eq!(legacy_label_floor_rank("较高 192k"), None);
        assert_eq!(legacy_label_floor_rank("标准 128k"), None);
        assert_eq!(legacy_label_floor_rank("未知"), None);
        assert_eq!(legacy_label_floor_rank(""), None);
        // 「Hi-Res 无损」是补上第 4 字段之后才出现的档名，档位未知的记录不可能带它；
        // 真带上也不认——它的下限是 110，不是无损的 100。
        assert_eq!(legacy_label_floor_rank("Hi-Res 无损 871k"), None);
        // 老格式的「· 试听」拼在档位里：认档位前缀，标记留在后面不影响判定。
        assert_eq!(legacy_label_floor_rank("无损 · 试听"), Some(100));
        assert_eq!(legacy_label_floor_rank("试听 128k"), None);
    }

    /// 下限跟着档名走：`tier_label` 改名时这条判定一起改，不会悄悄失效。
    #[test]
    fn legacy_label_floor_follows_the_tier_label() {
        let lossless = tier_label(100);
        assert_eq!(
            legacy_label_floor_rank(&format!("{lossless} 871k")),
            preference_rank("lossless"),
            "无损档名开头的标签必须给出无损的分数"
        );
        assert_eq!(
            legacy_label_floor_rank(&format!("{} 871k", tier_label(110))),
            None,
            "比无损更高的档名不以「{lossless}」开头，交给带分数的记录自己说明"
        );
    }

    /// 旧标签里的码率是当年写进文件的原始数字，用来判断新流是不是更差。
    #[test]
    fn legacy_label_bitrate_reads_the_recorded_number_and_nothing_else() {
        assert_eq!(legacy_label_bitrate_kbps("极高 320k"), Some(320));
        assert_eq!(legacy_label_bitrate_kbps("无损 871k"), Some(871));
        assert_eq!(legacy_label_bitrate_kbps("标准 128k"), Some(128));
        assert_eq!(legacy_label_bitrate_kbps("未知 96k"), Some(96));
        // 没有码率的写法（早期档位文本单独成串）与认不出的尾巴都给 None，不参与比较。
        assert_eq!(legacy_label_bitrate_kbps("极高"), None);
        assert_eq!(legacy_label_bitrate_kbps("无损 · 试听"), None);
        assert_eq!(legacy_label_bitrate_kbps("较高 k"), None);
        assert_eq!(legacy_label_bitrate_kbps("较高 0k"), None);
        assert_eq!(legacy_label_bitrate_kbps(""), None);
    }

    #[test]
    fn quality_source_serde_and_fallback() {
        assert_eq!(QualitySource::default(), QualitySource::Unknown);
        assert_eq!(QualitySource::User.as_str(), "user");
        assert_eq!(QualitySource::Derived.as_str(), "derived");
        assert_eq!(QualitySource::Unknown.as_str(), "unknown");

        // 正常序列化与反序列化
        assert_eq!(
            serde_json::to_string(&QualitySource::User).unwrap(),
            "\"user\""
        );
        assert_eq!(
            serde_json::to_string(&QualitySource::Derived).unwrap(),
            "\"derived\""
        );
        assert_eq!(
            serde_json::to_string(&QualitySource::Unknown).unwrap(),
            "\"unknown\""
        );

        assert_eq!(
            serde_json::from_str::<QualitySource>("\"user\"").unwrap(),
            QualitySource::User
        );
        assert_eq!(
            serde_json::from_str::<QualitySource>("\"derived\"").unwrap(),
            QualitySource::Derived
        );
        assert_eq!(
            serde_json::from_str::<QualitySource>("\"unknown\"").unwrap(),
            QualitySource::Unknown
        );

        // 未知字符串安全回退为 Unknown，不导致反序列化失败
        assert_eq!(
            serde_json::from_str::<QualitySource>("\"legacy\"").unwrap(),
            QualitySource::Unknown
        );

        // label_key 覆盖三种来源
        assert_eq!(QualitySource::User.label_key(), "手动选择");
        assert_eq!(QualitySource::Derived.label_key(), "程序推导");
        assert_eq!(QualitySource::Unknown.label_key(), "来源未知");
    }
}
