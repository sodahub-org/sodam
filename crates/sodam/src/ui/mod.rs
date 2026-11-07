//! 通用 UI 组件与主题。

use gpui::prelude::*;
use gpui::{div, px, svg, AnyElement};

pub mod artwork;
pub mod cover_cache;
pub mod i18n;
pub mod icons;
pub mod message;
pub mod player_bar;
pub mod sidebar;
pub mod spinner;
pub mod status_line;
pub mod switch;
pub mod text_field;
pub mod theme;
pub mod titlebar;

/// 不可播标记：禁播图标 + 既有二级文本色。
///
/// 不新增第三级灰阶——浅色主题下 `text_muted` 与 `text_faint` 已经撞值，
/// 再分一层分不出来。标记只说「是哪一首」，且**不改变可点击性**：
/// 条目照常点得动，点了由播放路径给出原因。
///
/// 列表行与「接下来」面板共用这一个实现，避免两处各自调色 / 调尺寸。
pub fn unplayable_mark() -> AnyElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path(icons::path("ban"))
                .size(px(theme::ICON_SM))
                .text_color(theme::text_muted()),
        )
        .into_any_element()
}
