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
pub const TIERS: [&str; 4] = ["lossless", "highest", "medium", "low"];

/// 归一化后的音质意图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualityIntent {
    /// 不设上限：由服务端按账号权益与曲目可用性收敛。
    Auto,
    /// 一个具体档位（已小写化）；未知值也走这里，按原样保留。
    Tier(String),
}

impl QualityIntent {
    /// 从配置里的原始字符串解析。空串 / 空白 / `auto` / `best` → 自动。
    ///
    /// libresoda 把 `standard` 当作 70 分档（与 `medium` 同档），这里归一成 `medium`：
    /// 否则设置页选不中任何一行，状态行还会把原始 token 当显示名。
    pub fn parse(raw: &str) -> Self {
        let value = raw.trim().to_ascii_lowercase();
        if value.is_empty() || value == AUTO || value == "best" {
            Self::Auto
        } else if value == "standard" {
            Self::Tier("medium".to_string())
        } else {
            Self::Tier(value)
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

    /// 档位的稳定中文名（i18n 的 key）；认不出的档位返回 `None`，由调用方原样显示。
    ///
    /// 放在核心层是因为设置页、播放条菜单与状态行都要它：三处各写一遍 match，
    /// 迟早会有一处漏掉新档位。
    pub fn label_key(&self) -> Option<&'static str> {
        match self {
            Self::Auto => Some("自动"),
            Self::Tier(tier) => match tier.as_str() {
                "lossless" => Some("无损"),
                "highest" => Some("极高"),
                "medium" | "standard" => Some("较高"),
                "low" => Some("流畅"),
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
/// 具体档位返回其对应的分数（如无损 100、极高 80、较高 70、流畅 50）；
/// 「自动」在意图上不设上限，但在已知为 VIP 时目标为无损（100）。
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
pub fn tier_label(rank: i64) -> &'static str {
    if rank >= 100 {
        "无损"
    } else if rank >= 80 {
        "极高"
    } else if rank >= 70 {
        "较高"
    } else if rank > 0 {
        "流畅"
    } else {
        "未知"
    }
}

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
            ("lossless", "无损"),
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

    /// 同名的「降档」不该提示：所选流畅、实际更低分也显示流畅时，句子会自相矛盾。
    #[test]
    fn visible_downgrade_requires_a_different_display_name() {
        let low = QualityIntent::parse("low");
        let medium = QualityIntent::parse("medium");
        assert!(!is_visible_downgrade(&low, Some(50)));
        assert!(!is_visible_downgrade(&low, Some(20)));
        assert!(!is_visible_downgrade(&low, Some(10)));
        assert!(is_visible_downgrade(&medium, Some(50)));
        assert!(is_downgraded(&low, Some(20)), "内部降档判定保持不变");
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
            (info("lossless", "flac", 871_000), "无损", false),
            (info("highest", "m4a", 320_000), "极高", true),
            (info("higher", "m4a", 320_000), "较高", true),
            // libresoda 把实际流里的 `standard` 评为 50（与意图 `standard`=70 不同），
            // 显示名必须用 50 分档的「流畅」，不能复用意图档的「标准」。
            (info("standard", "m4a", 128_000), "流畅", true),
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
