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
//! macOS 走系统红绿灯，不使用本组件。

use gpui::prelude::*;
use gpui::{div, hsla, px, svg, App, Div, Hsla, MouseButton, Window, WindowControlArea};

use crate::ui::{icons, theme};

/// 标题栏条高度（逻辑像素，与 Windows 惯例一致）。
pub const STRIP_HEIGHT: f32 = 32.0;
/// 单个标题栏按钮宽度。
const BUTTON_WIDTH: f32 = 44.0;
/// 控制图标边长。
const GLYPH_SIZE: f32 = 12.0;
/// 关闭按钮悬停色（Windows 惯例红）。
const CLOSE_HOVER: Hsla = hsla(4.0 / 360.0, 0.83, 0.49, 1.0);

/// 覆盖在窗口顶部，不占布局高度：侧边栏/内容列各自为控件留出空间，
/// 背景仍延伸到标题栏下方，避免独立标题栏横条与面板之间出现色差。
/// 左侧全部为拖拽区，右侧是三个窗口控制按钮。
pub fn render(
    window: &Window,
    lyrics_visible: bool,
    on_toggle_lyrics: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    // 最大化后按钮换成「还原」图标，和系统窗口的语义一致。
    let maximize_icon = if window.is_maximized() {
        "restore"
    } else {
        "square"
    };
    div()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .h(px(STRIP_HEIGHT))
        .flex_none()
        .child(drag_area())
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
        ))
}

fn drag_area() -> impl IntoElement {
    let area = div()
        .id("titlebar-drag")
        .flex_1()
        .h_full()
        .window_control_area(WindowControlArea::Drag);

    // Linux 后端不处理控制区：拖拽要自己交给合成器，双击标题栏则等同于
    // 最大化/还原（Windows 由 WM_NCHITTEST 的 HTCAPTION 双击处理）。
    #[cfg(target_os = "linux")]
    let area = area.on_mouse_down(MouseButton::Left, |event, window, _| {
        if event.click_count >= 2 {
            window.zoom_window();
        } else {
            window.start_window_move();
        }
    });

    area
}

/// 桌面歌词开关：开启时整块填充强调色、图标反白；关闭时只有灰色线条。
fn lyrics_button(
    active: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("titlebar-lyrics")
        .flex()
        .items_center()
        .justify_center()
        .w(px(BUTTON_WIDTH))
        .h(px(STRIP_HEIGHT))
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
                .rounded(px(4.0))
                .when(active, |this| this.bg(theme::surface_hover()))
                .child(
                    svg()
                        .path(icons::path("captions"))
                        .size(px(GLYPH_SIZE))
                        .text_color(if active {
                            theme::text()
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
                .text_color(theme::text_muted())
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

    #[test]
    fn titlebar_overlays_panel_backgrounds_without_its_own_fill() {
        let mut titlebar = render();
        let style = titlebar.style();
        assert_eq!(style.position, Some(gpui::Position::Absolute));
        assert_eq!(style.inset.top, Some(px(0.0).into()));
        assert_eq!(style.inset.left, Some(px(0.0).into()));
        assert_eq!(style.size.height, Some(px(STRIP_HEIGHT).into()));
        assert!(style.background.is_none());
    }
}
