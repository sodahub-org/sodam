//! Windows 自绘标题栏：拖拽区 + 最小化/最大化/关闭按钮。
//!
//! gpui Windows 后端在 `TitlebarOptions::appears_transparent` 时会移除系统
//! 标题栏（WM_NCCALCSIZE），由应用通过 `window_control_area` 提供控制区：
//! WM_NCHITTEST 把这些区域映射为 HTCAPTION / HTMINBUTTON / HTMAXBUTTON /
//! HTCLOSE，拖拽、双击最大化、Win11 贴靠布局全部由系统处理。
//!
//! 命中匹配取「第一个包含光标的 hitbox」，因此按钮必须与拖拽区是
//! 兄弟节点，不能嵌套 —— 否则拖拽区先匹配，按钮会失效。
//! macOS 走系统红绿灯，不使用本组件。

use gpui::prelude::*;
use gpui::{div, hsla, px, svg, Div, Hsla, WindowControlArea};

use crate::ui::{icons, theme};

/// 标题栏条高度（逻辑像素，与 Windows 惯例一致）。
const STRIP_HEIGHT: f32 = 32.0;
/// 单个标题栏按钮宽度。
const BUTTON_WIDTH: f32 = 44.0;
/// 控制图标边长。
const GLYPH_SIZE: f32 = 12.0;
/// 关闭按钮悬停色（Windows 惯例红）。
const CLOSE_HOVER: Hsla = hsla(4.0 / 360.0, 0.83, 0.49, 1.0);

/// 渲染标题栏条：左侧全部为拖拽区，右侧是三个窗口控制按钮。
pub fn render() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .h(px(STRIP_HEIGHT))
        .child(drag_area())
        .child(caption_button(
            "titlebar-min",
            "minus",
            WindowControlArea::Min,
            false,
        ))
        .child(caption_button(
            "titlebar-max",
            "square",
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
    div()
        .id("titlebar-drag")
        .flex_1()
        .h_full()
        .window_control_area(WindowControlArea::Drag)
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

    if danger {
        base.hover(|style| style.bg(CLOSE_HOVER))
    } else {
        base.hover(|style| style.bg(theme::surface_hover()))
    }
}
