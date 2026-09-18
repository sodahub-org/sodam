//! Loading 圆圈：内置 SVG 圆弧 + 持续旋转动画。
//!
//! 加载中显示它，而不是把「暂无内容」当成加载态 ——
//! 只有加载完成且确实没有数据时，才展示空状态文案。

use std::time::Duration;

use gpui::prelude::*;
use gpui::{percentage, px, svg, Animation, AnimationExt as _, IntoElement, Rgba, Transformation};

use crate::ui::{icons, theme};

/// 旋转的 loading 圆圈。
pub fn spinner(size: f32, color: Rgba) -> impl IntoElement {
    svg()
        .path(icons::path("spinner"))
        .size(px(size))
        .flex_none()
        .text_color(color)
        .with_animation(
            "sodam-spinner",
            Animation::new(Duration::from_millis(900)).repeat(),
            |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
}

/// 居中的加载态：圆圈 + 一行说明。
pub fn loading_state(text: &str) -> gpui::AnyElement {
    gpui::div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(theme::space::MD))
        .py(px(theme::space::XXL))
        .child(spinner(theme::ICON_XL, theme::text_muted()))
        .child(
            gpui::div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(text.to_string()),
        )
        .into_any_element()
}
