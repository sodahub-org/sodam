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
use gpui::{div, hsla, px, svg, Div, Hsla, Pixels, Point, Window, WindowControlArea};
use std::{cell::Cell, rc::Rc};

use crate::ui::{icons, theme};

/// 标题栏条高度（逻辑像素，与 Windows 惯例一致）。
pub const STRIP_HEIGHT: f32 = 32.0;
/// 单个标题栏按钮宽度。
const BUTTON_WIDTH: f32 = 44.0;
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

/// 覆盖在窗口顶部，不占布局高度：侧边栏/内容列各自为控件留出空间，
/// 背景仍延伸到标题栏下方，避免独立标题栏横条与面板之间出现色差。
/// 左侧全部为拖拽区，右侧是三个窗口控制按钮。
pub fn render(window: &Window, anchor: &DragAnchor) -> Div {
    render_with_maximize_icon(maximize_icon(window.is_maximized()), anchor)
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
fn render_with_maximize_icon(maximize_icon: &'static str, anchor: &DragAnchor) -> Div {
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
        .child(drag_area(anchor.clone()))
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

fn drag_area(anchor: DragAnchor) -> impl IntoElement {
    #[cfg(not(target_os = "linux"))]
    let _ = &anchor;
    let area = div()
        .id("titlebar-drag")
        .flex_1()
        .h_full()
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

    fn test_anchor() -> DragAnchor {
        Rc::new(Cell::new(None))
    }

    #[test]
    fn titlebar_overlays_panel_backgrounds_without_its_own_fill() {
        let mut titlebar = render_with_maximize_icon(maximize_icon(false), &test_anchor());
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
            let mut titlebar = render_with_maximize_icon(icon, &test_anchor());
            assert_eq!(titlebar.style().size.height, Some(px(STRIP_HEIGHT).into()));
        }
    }
}
