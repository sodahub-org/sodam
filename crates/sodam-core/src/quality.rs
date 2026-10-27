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
    pub fn parse(raw: &str) -> Self {
        let value = raw.trim().to_ascii_lowercase();
        if value.is_empty() || value == AUTO || value == "best" {
            Self::Auto
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
                "medium" => Some("较高"),
                "low" => Some("标准"),
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
        "标准"
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
            ("low", "标准"),
        ] {
            assert_eq!(
                QualityIntent::parse(tier).label_key(),
                Some(label),
                "{tier}"
            );
        }
        assert_eq!(QualityIntent::parse("ultra").label_key(), None);
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
            (info("standard", "m4a", 128_000), "标准", true),
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
}
