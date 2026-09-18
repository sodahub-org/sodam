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
fn tint(base: Rgba, strength: f32) -> Rgba {
    let Some(ambient) = ambient_color() else {
        return base;
    };
    let (_, base_saturation, base_lightness) = rgba_to_hsl(base);
    let (ambient_hue, ambient_saturation, _) = rgba_to_hsl(ambient);
    let saturation = (base_saturation + ambient_saturation * strength).min(MAX_WASH_SATURATION);
    hsl_to_rgba(ambient_hue, saturation, base_lightness, base.a)
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
