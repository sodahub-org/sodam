//! 封面组件：统一尺寸、统一圆角，缺图时给同色系占位块。

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{div, img, px, svg, AnyElement, ImageSource, Img, IntoElement, ObjectFit, Resource};

use crate::ui::cover_cache::{sample_edge, CoverImageCache};
use crate::ui::{icons, theme};

/// 封面图源：走自定义缓存（按显示尺寸解码，而非原分辨率）。
fn cover_img(path: PathBuf, size: f32, radius: f32) -> Img {
    let resource = Resource::from(path);
    let source = ImageSource::Custom(Arc::new(move |window, cx| {
        let edge = sample_edge(size, window.scale_factor());
        let cache = CoverImageCache::entity(cx);
        cache.update(cx, |cache, cx| cache.load_at(&resource, edge, window, cx))
    }));
    img(source)
        .w(px(size))
        .h(px(size))
        .flex_none()
        .rounded(px(radius))
        .object_fit(ObjectFit::Cover)
}

/// 缺图占位：抬升面 + 音符图标（列表行也要有视觉重量）。
fn cover_placeholder(size: f32, radius: f32) -> AnyElement {
    div()
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
        .into_any_element()
}

/// 大尺寸封面加载态：背景占位 + 旋转圆圈。
fn loading_placeholder(size: f32, radius: f32) -> AnyElement {
    div()
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
        .into_any_element()
}

/// 渲染封面。
///
/// `path` 是已经下载到本地的封面文件（见 `sodam_core::Session::cover_path`）。
/// 调用方传入的路径都来自 `Root::covers` / `Root::original_covers` 内存表：
/// 表项在下载完成回执时验证过存在，淘汰删文件时由 `drop_removed_covers`
/// 同步清除。这里不做每帧 `path.exists()`（#135：渲染路径上的每次 stat 都
/// 是白做的系统调用），直接按表项渲染；但后台封面任务回执与淘汰删文件之间
/// 有竞态窗口——路径入表后文件可能已被清掉（PR #348 审查意见），所以挂上
/// `with_fallback` 占位：读图失败的帧显示占位块，不出现空白，也不需要
/// 每帧 stat。
pub fn cover(path: Option<PathBuf>, size: f32, radius: f32) -> AnyElement {
    match path {
        Some(path) => cover_img(path, size, radius)
            .with_fallback(move || cover_placeholder(size, radius))
            .into_any_element(),
        None => cover_placeholder(size, radius),
    }
}

/// 大尺寸封面：缺图、以及**解码中**都显示旋转圆圈。
///
/// 存在性判断同样交给调用方的内存表（见 [`cover`] 注释），不在渲染路径 stat。
/// 读图失败（文件已被缓存清理删掉）落回静态占位块：转圈是「解码中」的
/// 过渡态，文件没了就该停。
pub fn cover_loading(path: Option<PathBuf>, size: f32, radius: f32) -> AnyElement {
    match path {
        Some(path) => cover_img(path, size, radius)
            .with_loading(move || loading_placeholder(size, radius))
            .with_fallback(move || cover_placeholder(size, radius))
            .into_any_element(),
        None => loading_placeholder(size, radius),
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
