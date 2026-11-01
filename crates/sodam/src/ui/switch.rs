//! Switch：带明确轨道和滑块的二态开关。

use gpui::prelude::*;
use gpui::{div, px, ClickEvent, Context, ElementId, IntoElement, Window};

use crate::ui::theme;

const WIDTH: f32 = 42.0;
const HEIGHT: f32 = 24.0;
const KNOB: f32 = 18.0;
const PADDING: f32 = 3.0;

/// 开关；调用方负责把 `on_click` 绑到返回的元素上。
pub fn render(
    id: impl Into<ElementId>,
    enabled: bool,
    on_click: impl Fn(&mut crate::app::Root, &mut Window, &mut Context<crate::app::Root>) + 'static,
    cx: &mut Context<crate::app::Root>,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .p(px(PADDING))
        .rounded_full()
        .cursor_pointer()
        .border_1()
        .border_color(if enabled {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(if enabled {
            theme::accent()
        } else {
            theme::surface_hover()
        })
        .when(enabled, |this| this.justify_end())
        .when(!enabled, |this| this.justify_start())
        .hover(|style| {
            style.border_color(if enabled {
                theme::accent()
            } else {
                theme::text_faint()
            })
        })
        .child(
            div()
                .flex_none()
                .size(px(KNOB))
                .rounded_full()
                .bg(if enabled {
                    theme::accent_foreground()
                } else {
                    theme::text_muted()
                })
                .shadow_lg(),
        )
        .on_click(cx.listener(move |root, _event: &ClickEvent, window, cx| {
            on_click(root, window, cx);
        }))
}

const _: () = {
    assert!(KNOB < HEIGHT);
    assert!(KNOB < WIDTH);
    assert!(PADDING > 0.0);
    assert!(HEIGHT >= KNOB + PADDING * 2.0);
};
