//! 封面组件：统一尺寸、统一圆角，缺图时给同色系占位块。

use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{div, img, px, svg, AnyElement, IntoElement, ObjectFit};

use crate::ui::{icons, theme};

/// 渲染封面。
///
/// * `path` 是已经下载到本地的封面文件（见 `sodam_core::Session::cover_path`）；
/// * 没有封面时画一个抬升面 + 音符图标，保证列表行的视觉重量一致，
///   不会出现「有的行有图、有的行塌成一条线」。
pub fn cover(path: Option<PathBuf>, size: f32, radius: f32) -> AnyElement {
    match path.filter(|path| path.exists()) {
        Some(path) => img(path)
            .w(px(size))
            .h(px(size))
            .flex_none()
            .rounded(px(radius))
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        None => div()
            .w(px(size))
            .h(px(size))
            .flex_none()
            .rounded(px(radius))
            .bg(theme::surface_elevated())
            .flex()
            .items_center()
            .justify_center()
            .child(
                svg()
                    .path(icons::path("music"))
                    .size(px((size * 0.42).max(theme::ICON_SM)))
                    .text_color(theme::text_faint()),
            )
            .into_any_element(),
    }
}

/// 大尺寸封面加载态：背景占位 + 旋转圆圈。
pub fn cover_loading(path: Option<PathBuf>, size: f32, radius: f32) -> AnyElement {
    match path.filter(|path| path.exists()) {
        Some(path) => cover(Some(path), size, radius),
        None => div()
            .relative()
            .w(px(size))
            .h(px(size))
            .flex_none()
            .rounded(px(radius))
            .bg(theme::surface_elevated())
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .absolute()
                    .size(px(size * 0.72))
                    .rounded(px(radius))
                    .bg(theme::surface_hover()),
            )
            .child(crate::ui::spinner::spinner(
                (size * 0.28).max(theme::ICON_LG),
                theme::text(),
            ))
            .into_any_element(),
    }
}

/// 渲染内置听歌模式图标。
pub fn scene_image(sub_queue_type: &str, size: f32) -> AnyElement {
    // 服务端原图是黑色/透明；Dark 使用反色后的白色版本。
    let light = crate::ui::theme::current_theme() == crate::ui::theme::ThemeKind::Light;
    // 熟悉/新鲜的本地基础图是白色版；浅色使用生成的黑色版。
    let local_suffix = if light { "-dark" } else { "" };
    if matches!(sub_queue_type, "familiar" | "fresh") {
        return img(format!("icons/{sub_queue_type}-mode{local_suffix}.png"))
            .w(px(size))
            .h(px(size))
            .flex_none()
            .object_fit(ObjectFit::Cover)
            .into_any_element();
    }
    match crate::ui::icons::scene_image_path(sub_queue_type, light) {
        Some(path) => img(path)
            .w(px(size))
            .h(px(size))
            .flex_none()
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        None => div()
            .w(px(size))
            .h(px(size))
            .flex_none()
            .rounded(px(theme::radius::ART))
            .bg(theme::surface_elevated())
            .into_any_element(),
    }
}

/// 图标按钮：固定点击区（避免图标大小各异导致「点不准」），悬停给一层浅底。
pub fn icon_button(
    id: impl Into<gpui::ElementId>,
    icon: &'static str,
    size: f32,
    color: gpui::Rgba,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(theme::size::CONTROL_SM))
        .rounded(px(theme::radius::ROW))
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(size))
                .text_color(color),
        )
}
