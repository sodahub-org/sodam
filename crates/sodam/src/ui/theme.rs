//! 主题：颜色、尺寸、字号、圆角、图标尺寸。
//!
//! 颜色参考 sonora 的分层方式：内置 Dark/Light 两套 palette，并把当前播放
//! 封面的主色轻轻混入背景/面板，让全局背景形成随歌曲变化的渐变。
//!
//! 1. **所有尺寸从同一个基准值派生**，行高/控件/缩略图之间保持固定比例；
//! 2. **层级用同一色系的小步进表达**：底色 → 面板 → 悬浮 → 选中，每级只差一点，
//!    再配 1px 低透明度描边，而不是堆一堆不同颜色。

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use gpui::{linear_color_stop, linear_gradient, px, Background, Pixels, Rgba};

pub const fn hex(value: u32) -> Rgba {
    Rgba {
        r: ((value >> 16) & 0xFF) as f32 / 255.0,
        g: ((value >> 8) & 0xFF) as f32 / 255.0,
        b: (value & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Dark,
    Light,
}

impl ThemeKind {
    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    fn from_appearance(appearance: gpui::WindowAppearance) -> Self {
        match appearance {
            gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => Self::Light,
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark => Self::Dark,
        }
    }
}

static CURRENT_THEME: AtomicU8 = AtomicU8::new(0);
// bit31 表示存在主色；低 24 位是 RGB。渲染线程每帧设置，后台只传值不读全局。
static AMBIENT_COLOR: AtomicU32 = AtomicU32::new(0);

const SURFACE_TINT: f32 = 0.5;
const BORDER_TINT: f32 = 0.4;
const TEXT_TINT: f32 = 0.12;
const MAX_WASH_SATURATION: f32 = 0.7;
const MIN_ACCENT_SATURATION: f32 = 0.6;
const MAX_ACCENT_SATURATION: f32 = 0.85;

/// 深色主题的底色接近纯黑（HSL 亮度只有 0.04~0.19），只换色相、不抬亮度的话
/// 彩度空间被 `(1 - |2L - 1|)` 卡死，封面再鲜艳背景也看不出颜色。
/// 有主色时给这类底色抬一截亮度，色相才有表达空间。
const AMBIENT_LIFT: f32 = 0.07;
/// 只抬比这个更暗的底色：文字/次级文字本身够亮，不该被抬。
const AMBIENT_LIFT_GATE: f32 = 0.25;
/// 抬升上限，避免把深色主题抬灰。
const AMBIENT_LIFT_CEIL: f32 = 0.32;
/// 浅色主题对称的问题：底色接近纯白（亮度 0.83~1.0），同样没有彩度空间。
/// 有主色时把这类很亮的底色压暗一截。
const AMBIENT_DROP: f32 = 0.09;
/// 只压比这个更亮的底色：浅色主题的文字是近黑，不会被压。
const AMBIENT_DROP_GATE: f32 = 0.75;
/// 压暗下限，避免把浅色主题压脏。
const AMBIENT_DROP_FLOOR: f32 = 0.72;

pub fn set_theme(theme: ThemeKind) {
    CURRENT_THEME.store(theme as u8, Ordering::Relaxed);
}

pub fn current_theme() -> ThemeKind {
    match CURRENT_THEME.load(Ordering::Relaxed) {
        1 => ThemeKind::Light,
        _ => ThemeKind::Dark,
    }
}

pub fn theme_from_appearance(appearance: gpui::WindowAppearance) -> ThemeKind {
    ThemeKind::from_appearance(appearance)
}

pub fn set_ambient_rgb(color: Option<(u8, u8, u8)>) {
    let packed = color
        .map(|(r, g, b)| 0x8000_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b));
    AMBIENT_COLOR.store(packed.unwrap_or(0), Ordering::Relaxed);
}

fn ambient_color() -> Option<Rgba> {
    let packed = AMBIENT_COLOR.load(Ordering::Relaxed);
    if packed & 0x8000_0000 == 0 {
        return None;
    }
    Some(Rgba {
        r: ((packed >> 16) & 0xFF) as f32 / 255.0,
        g: ((packed >> 8) & 0xFF) as f32 / 255.0,
        b: (packed & 0xFF) as f32 / 255.0,
        a: 1.0,
    })
}

type Hsl = (f32, f32, f32);

fn rgba_to_hsl(color: Rgba) -> Hsl {
    let max = color.r.max(color.g).max(color.b);
    let min = color.r.min(color.g).min(color.b);
    let delta = max - min;
    let lightness = (max + min) / 2.0;
    let saturation = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * lightness - 1.0).abs())
    };
    let hue = if delta == 0.0 {
        0.0
    } else if max == color.r {
        ((color.g - color.b) / delta).rem_euclid(6.0) / 6.0
    } else if max == color.g {
        ((color.b - color.r) / delta + 2.0) / 6.0
    } else {
        ((color.r - color.g) / delta + 4.0) / 6.0
    };
    (hue, saturation, lightness)
}

fn hsl_to_rgba(hue: f32, saturation: f32, lightness: f32, alpha: f32) -> Rgba {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue = (hue * 6.0).rem_euclid(6.0);
    let x = chroma * (1.0 - ((hue % 2.0) - 1.0).abs());
    let (red, green, blue) = match hue as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let shift = lightness - chroma / 2.0;
    Rgba {
        r: (red + shift).clamp(0.0, 1.0),
        g: (green + shift).clamp(0.0, 1.0),
        b: (blue + shift).clamp(0.0, 1.0),
        a: alpha,
    }
}

fn mix_rgba(from: Rgba, to: Rgba, delta: f32) -> Rgba {
    let delta = delta.clamp(0.0, 1.0);
    Rgba {
        r: from.r + (to.r - from.r) * delta,
        g: from.g + (to.g - from.g) * delta,
        b: from.b + (to.b - from.b) * delta,
        a: from.a + (to.a - from.a) * delta,
    }
}

/// sonora 的 wash：保留每个填充色原本的亮度/层级，只把色相换成封面主色，
/// 并把主色饱和度按强度并入背景。这样主题不会因封面明暗被破坏。
///
/// 额外做一步亮度补偿：底色太黑（深色 L≈0.04）或太（浅色 L≈0.98）时，
/// 彩度空间都被 `(1 - |2L - 1|)` 压到接近 0，只换色相根本显不出来。
/// 所以有主色时深色把暗底色抬亮、浅色把亮底色压暗（见 `AMBIENT_LIFT*` / `AMBIENT_DROP*`）。
fn tint(base: Rgba, strength: f32) -> Rgba {
    let Some(ambient) = ambient_color() else {
        return base;
    };
    let (_, base_saturation, base_lightness) = rgba_to_hsl(base);
    let (ambient_hue, ambient_saturation, _) = rgba_to_hsl(ambient);
    let saturation = (base_saturation + ambient_saturation * strength).min(MAX_WASH_SATURATION);
    // 近黑/近白的底色都压不出彩度；深色抬亮、浅色压暗，色相才有空间显出来。
    let lightness = if is_dark() {
        if base_lightness < AMBIENT_LIFT_GATE {
            let lift = AMBIENT_LIFT * (0.6 + 0.8 * ambient_saturation);
            (base_lightness + lift).min(AMBIENT_LIFT_CEIL)
        } else {
            base_lightness
        }
    } else if base_lightness > AMBIENT_DROP_GATE {
        let drop = AMBIENT_DROP * (0.6 + 0.8 * ambient_saturation);
        (base_lightness - drop).max(AMBIENT_DROP_FLOOR)
    } else {
        base_lightness
    };
    hsl_to_rgba(ambient_hue, saturation, lightness, base.a)
}

fn tinted_accent(lightness: f32) -> Rgba {
    let Some(ambient) = ambient_color() else {
        return if is_dark() {
            hex(0xFAFAFA)
        } else {
            hex(0x171717)
        };
    };
    let (hue, saturation, _) = rgba_to_hsl(ambient);
    hsl_to_rgba(
        hue,
        saturation.clamp(MIN_ACCENT_SATURATION, MAX_ACCENT_SATURATION),
        lightness,
        1.0,
    )
}

fn is_dark() -> bool {
    current_theme() == ThemeKind::Dark
}

// ---------------------------------------------------------------------------
// 颜色
// ---------------------------------------------------------------------------

/// 窗口/侧边/播放条底色。
pub fn bg() -> Rgba {
    tint(
        if is_dark() {
            hex(0x0A0A0A)
        } else {
            hex(0xFAFAFA)
        },
        SURFACE_TINT,
    )
}

/// 内容区背景。
pub fn surface() -> Rgba {
    tint(
        if is_dark() {
            hex(0x101010)
        } else {
            hex(0xF5F5F5)
        },
        SURFACE_TINT,
    )
}

/// 抬升面：搜索框、卡片、徽章。
pub fn surface_elevated() -> Rgba {
    tint(
        if is_dark() {
            hex(0x171717)
        } else {
            hex(0xFFFFFF)
        },
        SURFACE_TINT,
    )
}

/// 行悬停。
pub fn surface_hover() -> Rgba {
    tint(
        if is_dark() {
            hex(0x232323)
        } else {
            hex(0xE5E5E5)
        },
        SURFACE_TINT,
    )
}

/// 选中行。
pub fn surface_selected() -> Rgba {
    tint(
        if is_dark() {
            hex(0x303030)
        } else {
            hex(0xD4D4D4)
        },
        SURFACE_TINT,
    )
}

/// 1px 描边。
pub fn border() -> Rgba {
    tint(
        if is_dark() {
            hex(0x262626)
        } else {
            hex(0xD4D4D4)
        },
        BORDER_TINT,
    )
}

/// 主文本。
pub fn text() -> Rgba {
    tint(
        if is_dark() {
            hex(0xFAFAFA)
        } else {
            hex(0x171717)
        },
        TEXT_TINT,
    )
}

/// 次要文本。
pub fn text_muted() -> Rgba {
    tint(hex(0x737373), TEXT_TINT)
}

/// 三级文本。
pub fn text_faint() -> Rgba {
    tint(
        if is_dark() {
            hex(0x525252)
        } else {
            hex(0x737373)
        },
        TEXT_TINT,
    )
}

/// 强调色：无封面时是 sonora neutral；有封面时由封面色相生成。
pub fn accent() -> Rgba {
    tinted_accent(if is_dark() { 0.72 } else { 0.42 })
}

/// 主色上的前景：对齐 sonora 的 primary_foreground。
pub fn accent_foreground() -> Rgba {
    let Some(ambient) = ambient_color() else {
        return if is_dark() {
            hex(0x171717)
        } else {
            hex(0xFAFAFA)
        };
    };
    let (hue, saturation, _) = rgba_to_hsl(ambient);
    hsl_to_rgba(
        hue,
        saturation.min(0.25),
        if is_dark() { 0.08 } else { 0.98 },
        1.0,
    )
}

/// 强调色的低透明底。
pub fn accent_soft() -> Rgba {
    let mut color = accent();
    color.a = 0.16;
    color
}

/// 歌词的"未点亮"颜色：**非高亮句**与**当前句还没唱到的部分**共用这一个颜色。
///
/// 歌词高亮只需要两种颜色（官方客户端也是这么做的：`.line` 用 `--color-base-4`，
/// 只在 `active` 时把已走字的部分换成 `--color-base-1`）：
///
/// * [`accent`] —— 当前句已唱到的字；
/// * 本函数 —— 其余一切：非当前行、当前行未唱到的部分、已唱过的行。
///
/// 早先非当前行用 `text_muted`、当前行未走字用另一个压暗色，两者亮度差 0.17，
/// 于是没在唱的句子反而比当前句还没唱到的部分更重，层级是反的。
/// 现在统一取一个颜色，视觉重量自然一致，也不会再出现"灰比彩色更重"。
///
/// 颜色取强调色的色相（与高亮部分同色系，切换时不割裂），饱和度压到极低，
/// 亮度贴近背景但与背景保持可读对比。
pub fn lyric_dim() -> Rgba {
    let lit = accent();
    let (hue, saturation, lightness) = rgba_to_hsl(lit);
    hsl_to_rgba(
        hue,
        // 只留一点点色味，接近中性灰但不会与高亮部分脱节。
        (saturation * 0.55).min(0.6) * 0.16,
        if is_dark() {
            // 深色主题：比背景亮，保证可读。
            (lightness * 0.62).max(0.24)
        } else {
            // 浅色主题：比背景暗（文字在浅底上越暗越重）。
            //
            // 这个偏移决定"未点亮"有多淡。实测 `+0.34` 时对比度只有 1.5，
            // 歌词几乎读不出来（WCAG 正文要求 4.5）；取 `+0.14` 约 2.8，
            // 既明显弱于已唱到的字（5.3），又保持可读。
            (lightness + 0.14).min(0.78)
        },
        lit.a,
    )
}

/// 警示色：message 的 warning 级别使用；不替代主色，也不用于普通按钮。
pub fn warning() -> Rgba {
    if is_dark() {
        hex(0xF5A524)
    } else {
        hex(0xB7791F)
    }
}

/// 错误色：message 的 error 级别使用。
pub fn danger() -> Rgba {
    if is_dark() {
        hex(0xF31260)
    } else {
        hex(0xC41D4A)
    }
}

/// 全局自适应背景：封面主色 wash 后的同色相明暗渐变。
pub fn ambient_background() -> Background {
    let neutral = if is_dark() {
        hex(0x0A0A0A)
    } else {
        hex(0xFAFAFA)
    };
    let from = tint(neutral, SURFACE_TINT);
    let to = mix_rgba(
        from,
        if is_dark() {
            hex(0x050505)
        } else {
            hex(0xFFFFFF)
        },
        0.48,
    );
    linear_gradient(
        135.0,
        linear_color_stop(from, 0.0),
        linear_color_stop(to, 1.0),
    )
}

// ---------------------------------------------------------------------------
// 尺寸
// ---------------------------------------------------------------------------

/// 尺寸基准：所有控件尺寸都由它推导。
pub const BASE: f32 = 14.0;

/// 侧边栏宽度。
pub const SIDEBAR_W: f32 = 216.0;
/// 顶部工具条 / 抽屉头高度。
pub const TOOLBAR_H: f32 = 48.0;
/// 底部播放条高度。
pub const PLAYER_H: f32 = 76.0;

/// 图标尺寸：导航与按钮统一 16，播放控制 20。
pub const ICON_SM: f32 = 14.0;
pub const ICON: f32 = 16.0;
pub const ICON_LG: f32 = 20.0;
pub const ICON_XL: f32 = 24.0;

/// 间距梯度：4 / 8 / 12 / 16 / 24 / 32，只用这几档。
pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
}

/// 行高与控件尺寸梯度。
pub mod size {
    /// 导航行。
    pub const NAV_ROW: f32 = 40.0;
    /// 列表行（歌曲/歌单）。
    pub const LIST_ROW: f32 = 56.0;
    /// 小控件（图标按钮）。
    pub const CONTROL_SM: f32 = 32.0;
    /// 常规控件（按钮/输入框）。
    pub const CONTROL: f32 = 36.0;
    /// 列表行封面。
    pub const ROW_ART: f32 = 40.0;
    /// 播放条封面。
    pub const PLAYER_THUMB: f32 = 48.0;
    /// 歌单卡片封面。
    pub const CARD_COVER: f32 = 156.0;
}

/// 圆角梯度：6 / 8 / 12 / 999。
pub mod radius {
    /// 封面（小图）。
    pub const ART: f32 = 6.0;
    /// 列表行 / 输入框。
    pub const ROW: f32 = 8.0;
    /// 卡片。
    pub const CARD: f32 = 12.0;
    /// 胶囊按钮。
    pub const PILL: f32 = 999.0;
}

/// 字号梯度（相对 `BASE` 的比例）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Text {
    Tiny,
    Small,
    Body,
    Large,
    Title,
}

impl Text {
    pub fn ratio(self) -> f32 {
        match self {
            Text::Tiny => 0.77,
            Text::Small => 0.85,
            Text::Body => 1.0,
            Text::Large => 1.38,
            Text::Title => 1.69,
        }
    }

    pub fn size(self) -> Pixels {
        px((BASE * self.ratio()).round())
    }
}

pub fn nav_row() -> Pixels {
    px(size::NAV_ROW)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 相对亮度（WCAG），用来比较两种颜色在感知上的"重量"。
    fn luminance(color: Rgba) -> f32 {
        let channel = |c: f32| {
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }

    /// 歌词只有两种颜色：未点亮共用 `lyric_dim`，已唱到用 `accent`。
    ///
    /// 未点亮色必须明显弱于已唱到的字，否则看不出高亮推进。
    ///
    /// "非高亮句"与"当前句未走字部分"在视图里取的是同一个 `lyric_dim()`，
    /// 一致性由调用点保证；这里守住两者的**层级差**。
    #[test]
    fn dimmed_color_is_weaker_than_lit() {
        // 视图里两者都取 `lyric_dim()`，这里直接验证它与 `accent` 的层级关系：
        // 未点亮必须比已唱到更弱，且差距足够看得出来。
        let dim = luminance(lyric_dim());
        let lit = luminance(accent());
        let delta = (dim - lit).abs();
        assert!(
            delta > 0.10,
            "未点亮与已唱到的亮度差 {} 太小，看不出高亮",
            delta
        );
    }

    /// 未点亮色必须能与纯背景区分开，否则歌词读不清。
    #[test]
    fn dimmed_lyric_stays_readable() {
        let (_, _, lightness) = rgba_to_hsl(lyric_dim());
        if is_dark() {
            assert!(lightness > 0.20, "深色主题下未点亮色太暗：{}", lightness);
        } else {
            assert!(lightness < 0.85, "浅色主题下未点亮色太亮：{}", lightness);
        }
    }
}
