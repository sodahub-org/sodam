//! Windows 自绘标题栏：拖拽区 + 最小化/最大化/关闭按钮。
//!
//! gpui Windows 后端在 `TitlebarOptions::appears_transparent` 时会移除系统
//! 标题栏（WM_NCCALCSIZE），由应用通过 `window_control_area` 提供控制区：
//! WM_NCHITTEST 把这些区域映射为 HTCAPTION / HTMINBUTTON / HTMAXBUTTON /
//! HTCLOSE，拖拽、双击最大化、Win11 贴靠布局全部由系统处理。
//!
//! 命中匹配取「第一个包含光标的 hitbox」，因此按钮必须与拖拽区是
//! 兄弟节点，不能嵌套 —— 否则拖拽区先匹配，按钮会失效。
//! Linux（Wayland 会话）下 gpui 只能提供客户端装饰，系统不会画标题栏；而
//! `on_hit_test_window_control` 在 Linux 后端是空实现，`window_control_area`
//! 不生效。因此 Linux 下按钮点击与标题栏拖拽由本模块直接调用窗口 API 处理。
//!
//! 拖拽**不能**在按下时就 `start_window_move()`：合成器会立刻对指针做交互移动
//! 抓取，双击的第二下被吞掉，`click_count >= 2` 永远收不到（实测）。所以按下只
//! 记录锚点，移动超过 [`DRAG_THRESHOLD`] 才交给合成器。
//!
//! macOS 走系统红绿灯，不使用本组件。

use gpui::prelude::*;
#[cfg(target_os = "linux")]
use gpui::MouseButton;
use gpui::{
    div, hsla, px, svg, AnyElement, App, Div, Hsla, Pixels, Point, SharedString, Window,
    WindowControlArea,
};
use std::{cell::Cell, rc::Rc};

use crate::ui::{icons, theme};

/// 标题栏条高度（逻辑像素，与 Windows 惯例一致）。
pub const STRIP_HEIGHT: f32 = 48.0;
/// 单个标题栏按钮宽度。
const BUTTON_WIDTH: f32 = 46.0;
/// 控制图标边长。
const GLYPH_SIZE: f32 = 12.0;
/// 关闭按钮悬停色（Windows 惯例红）。
const CLOSE_HOVER: Hsla = hsla(4.0 / 360.0, 0.83, 0.49, 1.0);
/// 拖拽阈值（逻辑像素）：移动超过它才把指针交给合成器移动窗口。
/// 太小会让手抖误触发、太大拖拽发涩，参考各桌面工具包的 3~4px。
#[cfg(target_os = "linux")]
const DRAG_THRESHOLD: f32 = 4.0;

/// 标题栏拖拽的按下锚点（由 `Root` 持有，跨帧保留）。
/// Windows/macOS 由系统处理拖拽，这里只是保持签名一致。
pub type DragAnchor = Rc<Cell<Option<Point<Pixels>>>>;

pub struct NavArrows {
    pub back_enabled: bool,
    pub forward_enabled: bool,
    pub on_back: Box<dyn Fn(&mut Window, &mut App)>,
    pub on_forward: Box<dyn Fn(&mut Window, &mut App)>,
}

/// 顶栏右侧的账号区：会员徽章 + 头像。
pub struct HeaderAccount {
    pub badge: Option<SharedString>,
    pub avatar: Option<std::path::PathBuf>,
}

/// 顶栏里除窗口按钮之外的内容：前进/后退、搜索框与账号区都是可选的。
pub struct HeaderExtras {
    pub nav: Option<NavArrows>,
    pub search: Option<AnyElement>,
    pub account: Option<HeaderAccount>,
    /// 点 SVIP 胶囊：打开会员购买页。
    pub on_vip_click: Option<Box<dyn Fn(&mut Window, &mut App)>>,
    /// 点头像：弹出账号菜单。
    pub on_account_click: Option<Box<dyn Fn(&mut Window, &mut App)>>,
    /// 未登录时的「登录」按钮。
    pub on_login_click: Option<Box<dyn Fn(&mut Window, &mut App)>>,
}

impl Default for HeaderExtras {
    fn default() -> Self {
        Self {
            nav: None,
            search: None,
            account: None,
            on_vip_click: None,
            on_account_click: None,
            on_login_click: None,
        }
    }
}

/// 覆盖在窗口顶部，不占布局高度：侧边栏/内容列各自为控件留出空间，
/// 背景仍延伸到标题栏下方，避免独立标题栏横条与面板之间出现色差。
/// 左段拖拽、中段搜索、右段账号/歌词开关/窗口按钮。
pub fn render(
    window: &Window,
    anchor: &DragAnchor,
    lyrics_visible: bool,
    on_toggle_lyrics: impl Fn(&mut Window, &mut App) + 'static,
    extras: HeaderExtras,
) -> Div {
    render_with_maximize_icon(
        maximize_icon(window.is_maximized()),
        anchor,
        lyrics_visible,
        on_toggle_lyrics,
        extras,
    )
}

/// 最大化后按钮换成「还原」图标，和系统窗口的语义一致。
fn maximize_icon(is_maximized: bool) -> &'static str {
    if is_maximized {
        "restore"
    } else {
        "square"
    }
}

/// 与真实 `Window` 解耦：单测不必构造平台窗口，也能覆盖普通/最大化两条分支。
fn render_with_maximize_icon(
    maximize_icon: &'static str,
    anchor: &DragAnchor,
    lyrics_visible: bool,
    on_toggle_lyrics: impl Fn(&mut Window, &mut App) + 'static,
    mut extras: HeaderExtras,
) -> Div {
    let mut row = div()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .h(px(STRIP_HEIGHT))
        .flex_none()
        // 左侧越过侧栏的一段只做拖拽，顶栏内容从侧栏右边开始。
        .child(drag_area(anchor.clone(), Some(theme::SIDEBAR_W)));

    if let Some(nav) = extras.nav.take() {
        row = row
            .child(arrow_button(
                "titlebar-back",
                "chevron-left",
                nav.back_enabled,
                nav.on_back,
            ))
            .child(arrow_button(
                "titlebar-forward",
                "chevron-right",
                nav.forward_enabled,
                nav.on_forward,
            ));
    }

    // 箭头与搜索框之间留弹性空白，搜索框靠右侧控件组。
    row = row.child(drag_area(anchor.clone(), None));

    if let Some(search) = extras.search.take() {
        row = row.child(
            div()
                .flex_none()
                .w(px(240.0))
                .h(px(STRIP_HEIGHT))
                .flex()
                .flex_col()
                .justify_center()
                .child(search),
        );
    }

    // 搜索框 → 账号区固定 15，与账号区内部的「SVIP → 头像」保持一致。
    row = row.child(div().w(px(16.0)).h_full());

    if let Some(login) = extras.on_login_click.take() {
        row = row.child(
            div()
                .id("titlebar-login")
                .h(px(32.0))
                .px(px(10.0))
                .ml(px(12.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .rounded(px(100.0))
                .bg(gpui::Rgba {
                    a: 0.1,
                    ..theme::text()
                })
                .text_size(px(14.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text())
                .cursor_pointer()
                .on_click(move |_, window, cx| login(window, cx))
                .child("登录")
                .child(
                    svg()
                        .path(icons::path("user"))
                        .size(px(20.0))
                        .text_color(theme::text()),
                ),
        );
    }

    if let Some(account) = extras.account {
        row = row.child(account_chip(
            account,
            extras.on_vip_click.take(),
            extras.on_account_click.take(),
        ));
    }

    row = row
        .child(separator())
        // 桌面歌词开关：放在最小化左边，对应汽水音乐原生客户端的同一位置。
        .when(cfg!(target_os = "linux"), |this| {
            this.child(lyrics_button(lyrics_visible, on_toggle_lyrics))
        })
        .child(caption_button(
            "titlebar-min",
            "minus",
            WindowControlArea::Min,
            false,
        ))
        .child(caption_button(
            "titlebar-max",
            maximize_icon,
            WindowControlArea::Max,
            false,
        ))
        .child(caption_button(
            "titlebar-close",
            "x",
            WindowControlArea::Close,
            true,
        ));
    row
}

/// 前进/后退箭头：不可用时只降透明度，不接点击。
fn arrow_button(
    id: &'static str,
    icon: &'static str,
    enabled: bool,
    on_click: Box<dyn Fn(&mut Window, &mut App)>,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(36.0))
        .h(px(STRIP_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |this| {
            this.cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(move |_, window, cx| on_click(window, cx))
        })
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(18.0))
                .text_color(if enabled {
                    gpui::Rgba {
                        a: 0.85,
                        ..theme::text()
                    }
                } else {
                    gpui::Rgba {
                        a: 0.4,
                        ..theme::text()
                    }
                }),
        )
}

/// 账号区与窗口按钮之间的细分隔线。
fn separator() -> impl IntoElement {
    div()
        .flex_none()
        .w(px(1.0))
        .h(px(24.0))
        .self_center()
        .ml(px(14.0))
        .mr(px(8.0))
        .bg(theme::border())
}

/// 账号区：会员徽章（SVIP / VIP / 开通 VIP）+ 头像，整块可点进设置。
fn account_chip(
    account: HeaderAccount,
    on_vip_click: Option<Box<dyn Fn(&mut Window, &mut App)>>,
    on_avatar_click: Option<Box<dyn Fn(&mut Window, &mut App)>>,
) -> impl IntoElement {
    let mut chip = div()
        .id("titlebar-account")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(16.0))
        .h(px(STRIP_HEIGHT));

    if let Some(badge) = account.badge {
        let mut vip = div()
            .id("titlebar-vip")
            .cursor_pointer()
            .flex()
            .items_center();
        if let Some(click) = on_vip_click {
            vip = vip.on_click(move |_, window, cx| click(window, cx));
        }
        // 官方字标：SVIP / VIP 的路径直接从客户端 3.8.0 的 bundle 里取，
        // 颜色用背景色，保持「字是洞」的观感。
        let logo = match badge.as_ref() {
            "SVIP" => Some(
                svg()
                    .path(icons::path("svip"))
                    .w(px(36.25))
                    .h(px(10.0))
                    .text_color(theme::bg()),
            ),
            "VIP" => Some(
                svg()
                    .path(icons::path("vip"))
                    .w(px(26.0))
                    .h(px(10.0))
                    .text_color(theme::bg()),
            ),
            _ => None,
        };
        chip = chip.child(vip.child(match logo {
            Some(logo) => div()
                .h(px(24.0))
                .px(px(8.0))
                .rounded(px(999.0))
                .flex()
                .items_center()
                .bg(theme::surface_hover())
                .text_color(theme::text())
                .child(logo)
                .into_any_element(),
            None => div()
                .h(px(24.0))
                .px(px(8.0))
                .rounded(px(999.0))
                .flex()
                .items_center()
                .bg(theme::surface_hover())
                .text_size(px(12.0))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::text())
                .child(badge)
                .into_any_element(),
        }));
    }

    if let Some(path) = account.avatar {
        let mut avatar = div().id("titlebar-avatar").cursor_pointer();
        if let Some(click) = on_avatar_click {
            avatar = avatar.on_click(move |_, window, cx| click(window, cx));
        }
        chip = chip.child(avatar.child(crate::ui::artwork::cover(Some(path), 28.0, 999.0)));
    }

    chip
}

fn drag_area(anchor: DragAnchor, width: Option<f32>) -> impl IntoElement {
    #[cfg(not(target_os = "linux"))]
    let _ = &anchor;
    let area = div()
        .id(("titlebar-drag", width.map(|w| w as usize).unwrap_or(usize::MAX)))
        .h_full()
        .when(width.is_some(), |this| this.w(px(width.unwrap_or(0.0))).flex_none())
        .when(width.is_none(), |this| this.flex_1())
        .window_control_area(WindowControlArea::Drag);

    // Linux 后端不处理控制区：双击标题栏等同于最大化/还原（Windows 由
    // WM_NCHITTEST 的 HTCAPTION 双击处理），拖拽则等移动超过阈值后再交给合成器。
    #[cfg(target_os = "linux")]
    let area = area
        .on_mouse_down(MouseButton::Left, {
            let anchor = anchor.clone();
            move |event, window, _| {
                if event.click_count >= 2 {
                    anchor.set(None);
                    window.zoom_window();
                } else {
                    anchor.set(Some(event.position));
                }
            }
        })
        .on_mouse_move({
            let anchor = anchor.clone();
            move |event, window, _| {
                let Some(origin) = anchor.get() else { return };
                if event.pressed_button != Some(MouseButton::Left) {
                    anchor.set(None);
                    return;
                }
                if drag_threshold_exceeded(origin, event.position) {
                    // 先清锚点：交给合成器后拿不到 mouse_up，不能靠它复位。
                    anchor.set(None);
                    window.start_window_move();
                }
            }
        })
        .on_mouse_up(MouseButton::Left, {
            let anchor = anchor.clone();
            move |_, _, _| anchor.set(None)
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, _| anchor.set(None));

    area
}

/// 鼠标是否已从按下点移动超过拖拽阈值。
#[cfg(target_os = "linux")]
fn drag_threshold_exceeded(origin: Point<Pixels>, current: Point<Pixels>) -> bool {
    let dx: f32 = (current.x - origin.x).abs().into();
    let dy: f32 = (current.y - origin.y).abs().into();
    dx > DRAG_THRESHOLD || dy > DRAG_THRESHOLD
}

/// 桌面歌词开关：开启时整块填充强调色、图标反白；关闭时只有灰色线条。
#[cfg(target_os = "linux")]
fn lyrics_button(
    active: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("titlebar-lyrics")
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.0))
        .h(px(28.0))
        .mx(px(theme::space::XS))
        .rounded(px(8.0))
        .hover(move |style| {
            if active {
                style
            } else {
                style.bg(theme::surface_hover())
            }
        })
        // 背景只包住图标本身：整格铺满会变成一个黑色方块。
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(22.0))
                .h(px(18.0))
                .rounded(px(8.0))
                .when(active, |this| this.bg(gpui::Rgba {
                    a: 1.0,
                    ..gpui::rgb(0xB5B5C6)
                }))
                .child(
                    svg()
                        .path(icons::path("widget-toggle"))
                        .size(px(16.0))
                        .text_color(if active {
                            theme::bg()
                        } else {
                            theme::text_muted()
                        }),
                ),
        )
        .on_click(move |_, window, cx| on_toggle(window, cx))
}

fn caption_button(
    id: &'static str,
    icon: &'static str,
    area: WindowControlArea,
    danger: bool,
) -> impl IntoElement {
    let base = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(BUTTON_WIDTH))
        .h(px(STRIP_HEIGHT))
        .window_control_area(area)
        .group(id)
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(GLYPH_SIZE))
                .text_color(gpui::Rgba {
                    a: 0.85,
                    ..theme::text()
                })
                .group_hover(id, |style| {
                    if danger {
                        style.text_color(gpui::white())
                    } else {
                        style.text_color(theme::text())
                    }
                }),
        );

    // Windows 由 WM_NCHITTEST 把控制区映射成 HTMINBUTTON 等消息；Linux 后端
    // 不处理控制区，这里直接调用窗口 API，避免两个平台各点一次。
    #[cfg(target_os = "linux")]
    let base = base.on_click(move |_, window, _| match area {
        WindowControlArea::Min => window.minimize_window(),
        WindowControlArea::Max => window.zoom_window(),
        WindowControlArea::Close => window.remove_window(),
        _ => {}
    });

    if danger {
        base.hover(|style| style.bg(CLOSE_HOVER))
    } else {
        base.hover(|style| style.bg(theme::surface_hover()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_anchor() -> DragAnchor {
        Rc::new(Cell::new(None))
    }

    #[test]
    fn titlebar_overlays_panel_backgrounds_without_its_own_fill() {
        let mut titlebar =
            render_with_maximize_icon(
            maximize_icon(false),
            &test_anchor(),
            true,
            |_, _| {},
            HeaderExtras::default(),
        );
        let style = titlebar.style();
        assert_eq!(style.position, Some(gpui::Position::Absolute));
        assert_eq!(style.inset.top, Some(px(0.0).into()));
        assert_eq!(style.inset.left, Some(px(0.0).into()));
        assert_eq!(style.size.height, Some(px(STRIP_HEIGHT).into()));
        assert!(style.background.is_none());
    }

    /// 双击要能收到第二下：按下时不能立刻开始移动，只有超过阈值才移交合成器。
    #[cfg(target_os = "linux")]
    #[test]
    fn drag_starts_only_after_threshold() {
        let origin = Point::new(px(100.0), px(50.0));
        // 手抖 / 双击时的微小位移不算拖拽
        assert!(!drag_threshold_exceeded(
            origin,
            Point::new(px(101.0), px(51.0))
        ));
        assert!(!drag_threshold_exceeded(
            origin,
            Point::new(px(103.9), px(46.1))
        ));
        // 越过阈值才开始移动窗口
        assert!(drag_threshold_exceeded(
            origin,
            Point::new(px(104.1), px(50.0))
        ));
        assert!(drag_threshold_exceeded(
            origin,
            Point::new(px(100.0), px(54.1))
        ));
    }

    #[test]
    fn maximize_button_switches_to_restore_icon() {
        assert_eq!(maximize_icon(false), "square");
        assert_eq!(maximize_icon(true), "restore");
        // 两条分支都必须能构建出同高度的覆盖层。
        for icon in ["square", "restore"] {
            let mut titlebar = render_with_maximize_icon(
                icon,
                &test_anchor(),
                true,
                |_, _| {},
                HeaderExtras::default(),
            );
            assert_eq!(titlebar.style().size.height, Some(px(STRIP_HEIGHT).into()));
        }
    }
}
