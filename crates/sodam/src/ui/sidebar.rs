//! 左侧导航栏：品牌区 + 「我的音乐」分组 + 导航行。
//!
//! 行高、间距、图标尺寸全部来自 `theme`，选中态用 `SURFACE_SELECTED`，
//! 图标在选中时变成强调色 —— 和官方客户端一样，强调色只出现在「当前项」上。

use gpui::prelude::*;
use gpui::{div, img, px, svg, AnyElement, ClickEvent, Context, IntoElement};

use crate::app::{Nav, Root};
use crate::ui::{icons, theme};

/// 导航项的图标名（Lucide 线性图标）。
fn icon_name(nav: Nav) -> &'static str {
    match nav {
        Nav::Home => "house",
        Nav::Scenes => "radio",
        Nav::Search => "search",
        Nav::Liked => "heart",
        Nav::Library => "list-music",
        Nav::Artist | Nav::Album => "search",
        Nav::Settings => "settings",
        Nav::Lyrics => "music",
    }
}

fn nav_row(nav: Nav, label: String, selected: bool, cx: &mut Context<Root>) -> AnyElement {
    let foreground = if selected {
        theme::text()
    } else {
        theme::text_muted()
    };
    let icon_color = if selected {
        theme::accent()
    } else {
        theme::text_muted()
    };
    div()
        .id(("nav", nav as usize))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::space::MD))
        .h(theme::nav_row())
        .px(px(theme::space::MD))
        .rounded(px(theme::radius::ROW))
        .cursor_pointer()
        .when(selected, |this| this.bg(theme::surface_selected()))
        .when(!selected, |this| {
            this.hover(|style| style.bg(theme::surface_hover()))
        })
        .text_color(foreground)
        .text_size(theme::Text::Body.size())
        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
            // 未登录时只放行「设置」，其他入口都拦回去（登录在设置 → 账户）
            if root.settings.cookie.trim().is_empty() && nav != Nav::Settings {
                root.set_nav(Nav::Settings, cx);
                root.set_status("请先在「设置 → 账户」扫码登录", &[]);
                return;
            }
            if nav == Nav::Home {
                root.activate_recommendation(cx);
            } else {
                root.set_nav(nav, cx);
            }
        }))
        .child(
            svg()
                .path(icons::path(icon_name(nav)))
                .size(px(theme::ICON))
                .flex_none()
                .text_color(icon_color),
        )
        .child(div().min_w_0().truncate().child(label))
        .into_any_element()
}

fn section_label(text: &str) -> impl IntoElement {
    div()
        .px(px(theme::space::MD))
        .pt(px(theme::space::LG))
        .pb(px(theme::space::SM))
        .text_size(theme::Text::Tiny.size())
        .text_color(theme::text_faint())
        .child(text.to_string())
}

/// 渲染导航栏：点击切换 [`Root::nav`]。
pub fn render(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    let current = root.nav;
    let playing = current == Nav::Lyrics;
    let origin = root.queue_origin.clone();
    // 切换队列时立刻用目标来源决定高亮，避免推荐 -> 听歌模式闪一次推荐。
    let active_origin = root
        .switching_queue
        .clone()
        .unwrap_or_else(|| origin.clone());
    let home_active = if playing {
        matches!(active_origin, crate::app::QueueOrigin::Feed)
    } else {
        current == Nav::Home
    };
    let scene_active = if playing {
        matches!(active_origin, crate::app::QueueOrigin::FeedMode(_))
    } else {
        current == Nav::Scenes
    };
    let liked_active = if playing {
        matches!(active_origin, crate::app::QueueOrigin::Liked)
    } else {
        current == Nav::Liked
    };
    let labels: std::collections::HashMap<Nav, String> = [
        Nav::Home,
        Nav::Scenes,
        Nav::Search,
        Nav::Liked,
        Nav::Library,
        Nav::Settings,
    ]
    .into_iter()
    .map(|nav| (nav, root.nav_display_label(nav)))
    .collect();
    let main_items: Vec<AnyElement> = [Nav::Home, Nav::Scenes, Nav::Search]
        .into_iter()
        .map(|nav| {
            let active = match nav {
                Nav::Home => home_active,
                Nav::Scenes => scene_active,
                _ => current == nav,
            };
            nav_row(nav, labels[&nav].clone(), active, cx)
        })
        .collect();
    let library_items: Vec<AnyElement> = [Nav::Liked, Nav::Library]
        .into_iter()
        .map(|nav| {
            let active = match nav {
                Nav::Liked => liked_active,
                Nav::Library => {
                    if playing {
                        matches!(origin, crate::app::QueueOrigin::Playlist(_))
                    } else {
                        current == Nav::Library
                    }
                }
                _ => current == nav,
            };
            nav_row(nav, labels[&nav].clone(), active, cx)
        })
        .collect();
    let footer_items: Vec<AnyElement> = [Nav::Settings]
        .into_iter()
        .map(|nav| nav_row(nav, labels[&nav].clone(), nav == current, cx))
        .collect();

    div()
        .flex()
        .flex_col()
        .w(px(theme::SIDEBAR_W))
        .h_full()
        .flex_none()
        .bg(theme::bg())
        .border_r_1()
        .border_color(theme::border())
        .p(px(theme::space::MD))
        .gap(px(theme::space::XS))
        .child(
            // 品牌区：强调色圆角块 + 名称，和官方客户端左上角一致
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::space::MD))
                .px(px(theme::space::MD))
                .py(px(theme::space::MD))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(28.0))
                        .rounded(px(theme::radius::ROW))
                        .overflow_hidden()
                        .child(img("icons/sodam-logo.svg").size(px(28.0))),
                )
                .child(
                    div()
                        .text_size(theme::Text::Large.size())
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::text())
                        .child("SodaM"),
                ),
        )
        .children(main_items)
        .child(section_label(root.tr("我的音乐")))
        .children(library_items)
        .child(div().flex_1())
        .children(footer_items)
}
