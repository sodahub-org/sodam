//! 音质意图：配置里只存用户的选择，实际拿到哪一档由服务端决定。
//!
//! 三条对外行为：
//! * 意图归一化——空串、空白、`auto` / `best` 都是「自动」，未知值也按不设上限处理
//!   （与 libresoda 的 `preference_rank` 一致），免得用户手写一个错字就被锁进低档；
//! * 缓存标签——同一个意图永远落到同一个文件名，`auto` 与空串共享 `auto`；
//! * 降档判定——实际档位低于所选档位时为真；`auto` 永不降档，实际档位未知（旧缓存）
//!   也不算降档。
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
        self.max_rank().is_some_and(|rank| rank >= 88)
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

/// 实际档位是否低于所选档位。
///
/// `actual` 缺失（旧缓存没有记录档位）时返回 `false`：不知道就不能断言降档，
/// 也不能因此触发重新下载。
pub fn is_downgraded(intent: &QualityIntent, actual: Option<i64>) -> bool {
    let (Some(max), Some(actual)) = (intent.max_rank(), actual) else {
        return false;
    };
    actual < max
}

/// 是否值得向用户显示降档提示。
///
/// 分数确实降了、但实际档位与所选档位落在同一个显示名时（例如所选「流畅」50、
/// 实际 10/20 也显示「流畅」），照实说只会得到「所选 流畅，实际 流畅」。
/// 这种提示读起来像没降档，干脆不显示；判定本身仍由 [`is_downgraded`] 负责，
/// 不改变缓存升级等内部决策。
pub fn is_visible_downgrade(intent: &QualityIntent, actual: Option<i64>) -> bool {
    let Some(actual) = actual else {
        return false;
    };
    if !is_downgraded(intent, Some(actual)) {
        return false;
    }
    match intent.label_key() {
        Some(selected) => selected != tier_label(actual),
        None => true,
    }
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

/// 实际档位是否低于目标档位分数。
///
/// `actual` 缺失（旧缓存未知档位）时返回 `false`。
pub fn is_below_target(actual: Option<i64>, target: i64) -> bool {
    match actual {
        Some(rank) => rank < target,
        None => false,
    }
}

/// 命中缓存时，当前档位是否低于所选（可升级判定）。
///
/// * 所选为具体档位：实际档位低于该档位即为真；
/// * 所选为「自动」且为 VIP：实际档位低于无损（100）即为真（非会员时期缓存的低档流在会员后可升级）；
/// * 实际档位未知（旧缓存没有记录分数）时返回 `false`：不知道就不能断言低于所选，也不触发重下。
pub fn is_tier_below_selected(
    actual: Option<i64>,
    intent: &QualityIntent,
    is_vip: Option<bool>,
) -> bool {
    let Some(target) = target_rank(intent, is_vip) else {
        return false;
    };
    is_below_target(actual, target)
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

    /// 降档提示按显示名去重：分数降了但显示名相同就不提示。
    ///
    /// 合并 #85 的分数拆档后，表里已不存在「同名却判降档」的组合（50 分档叫流畅、
    /// 更低分叫省流），所以真实降档都会提示；这条用例同时钉住「不降档不提示」与
    /// 「显示名不同才提示」两个口径，将来改表时仍能抓住回归。
    #[test]
    fn visible_downgrade_follows_display_names() {
        let low = QualityIntent::parse("low");
        let medium = QualityIntent::parse("medium");
        assert!(!is_visible_downgrade(&low, Some(50)), "同分不算降档");
        assert_eq!(tier_label(20), "省流");
        assert!(
            is_visible_downgrade(&low, Some(20)),
            "流畅 ≠ 省流，应当提示"
        );
        assert!(is_visible_downgrade(&medium, Some(50)));
        assert!(!is_visible_downgrade(&medium, None));
        assert!(!is_visible_downgrade(&QualityIntent::Auto, Some(10)));
        assert!(is_downgraded(&low, Some(20)), "内部降档判定保持不变");
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
            (info("", "m4a", 0), "未知", true),
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
        assert!(is_tier_below_selected(Some(80), &lossless, None));
        assert!(is_tier_below_selected(Some(50), &lossless, Some(false)));
        assert!(!is_tier_below_selected(Some(100), &lossless, None));
        assert!(!is_tier_below_selected(Some(110), &lossless, None));
        assert!(!is_tier_below_selected(None, &lossless, None));

        // 具体档位（极高，80）：
        assert!(is_tier_below_selected(Some(70), &highest, None));
        assert!(!is_tier_below_selected(Some(80), &highest, None));
        assert!(!is_tier_below_selected(Some(100), &highest, None));

        // 自动模式：
        // 1. VIP 用户：目标收敛到无损（100），非会员时期缓存的 50/70/80 均判断为可升级
        assert!(is_tier_below_selected(Some(50), &auto, Some(true)));
        assert!(is_tier_below_selected(Some(80), &auto, Some(true)));
        assert!(!is_tier_below_selected(Some(100), &auto, Some(true)));
        assert!(!is_tier_below_selected(None, &auto, Some(true)));

        // 2. 非 VIP 或未知：不指定上限，不盲目判断为低于所选
        assert!(!is_tier_below_selected(Some(50), &auto, Some(false)));
        assert!(!is_tier_below_selected(Some(50), &auto, None));

        // 目标分数与直接比较测试
        assert_eq!(target_rank(&lossless, None), Some(100));
        assert_eq!(target_rank(&highest, None), Some(80));
        assert_eq!(target_rank(&auto, Some(true)), Some(100));
        assert_eq!(target_rank(&auto, Some(false)), None);
        assert_eq!(target_rank(&auto, None), None);

        assert!(is_below_target(Some(70), 100));
        assert!(!is_below_target(Some(100), 100));
        assert!(!is_below_target(Some(110), 100));
        assert!(!is_below_target(None, 100));
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
