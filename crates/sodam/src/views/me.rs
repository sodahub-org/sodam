//! 我的主页：账号资料 + 创建的歌单。布局对齐官方 `/me`。

use super::*;

/// 我的主页。
pub(crate) fn me_view(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let Some(account) = root.account.as_ref() else {
        return empty_state("user", root.tr("登录后这里会显示你的主页"));
    };

    let avatar = root.cover_of(&account.avatar_url);
    let name = if account.nickname.is_empty() {
        root.tr("已登录").to_string()
    } else {
        account.nickname.clone()
    };
    let badge = if account.vip_stage == "svip" {
        Some(svg().path(icons::path("svip")).w(px(36.25)).h(px(10.0)).text_color(theme::text()))
    } else if account.vip {
        Some(svg().path(icons::path("vip")).w(px(26.0)).h(px(10.0)).text_color(theme::text()))
    } else {
        None
    };

    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .p(px(20.0))
        .child(cover(avatar, 72.0, 999.0))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .ml(px(24.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .child(
                            div()
                                .text_size(px(20.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text())
                                .child(name),
                        )
                        .when_some(badge, |this, logo| {
                            this.child(
                                div()
                                    .h(px(16.0))
                                    .px(px(4.0))
                                    .rounded(px(999.0))
                                    .flex()
                                    .items_center()
                                    .bg(theme::surface_hover())
                                    .text_color(theme::text())
                                    .child(logo),
                            )
                        }),
                )
                .child(
                    div()
                        .mt(px(4.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(20.0))
                        .child(stat(root, account.following, root.tr("关注")))
                        .child(stat(root, account.follower, root.tr("粉丝")))
                        .child(stat(root, account.likes, root.tr("获赞"))),
                ),
        )
        .child(
            div()
                .id("me-vip-entry")
                .h(px(36.0))
                .px(px(16.0))
                .flex()
                .items_center()
                .rounded(px(8.0))
                .bg(theme::surface_hover())
                .text_size(px(14.0))
                .text_color(theme::text())
                .cursor_pointer()
                .on_click(cx.listener(|_root, _event: &ClickEvent, _window, cx| {
                    cx.open_url("https://qishui.douyin.com/");
                }))
                .child(root.tr("会员中心").to_string()),
        );

    let mut page = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .child(header);

    // 创建的歌单
    page = page.child(section_title(root.tr("创建的歌单")));
    page = page.child(if root.loading_library && root.playlists.is_empty() {
        loading_state(root.tr("正在读取歌单…"))
    } else if root.playlists.is_empty() {
        empty_state("list-music", root.tr("还没有歌单"))
    } else {
        let cards: Vec<AnyElement> = root
            .playlists
            .iter()
            .map(|playlist| {
                let cover_path = root.cover_of(&playlist.cover);
                let id = playlist.id.clone();
                div()
                    .id(gpui::ElementId::Name(
                        format!("me-playlist-{}", playlist.id).into(),
                    ))
                    .flex()
                    .flex_col()
                    .gap(px(theme::space::SM))
                    .w(px(160.0))
                    .cursor_pointer()
                    .on_click(cx.listener(
                        move |root, _event: &ClickEvent, _window, cx| {
                            root.track_menu = None;
                            root.open_playlist(id.clone(), None, cx);
                        },
                    ))
                    .child(cover(cover_path, 160.0, 8.0))
                    .child(
                        div()
                            .w(px(160.0))
                            .truncate()
                            .text_size(theme::Text::Body.size())
                            .text_color(theme::text())
                            .child(playlist.title.clone()),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_wrap()
            .gap(px(16.0))
            .px(px(20.0))
            .pb(px(20.0))
            .children(cards)
            .into_any_element()
    });

    page.into_any_element()
}

fn section_title(text: &str) -> AnyElement {
    div()
        .px(px(20.0))
        .pt(px(16.0))
        .pb(px(12.0))
        .text_size(px(13.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(gpui::Rgba {
            a: 0.5,
            ..theme::text()
        })
        .child(text.to_string())
        .into_any_element()
}

fn stat(root: &Root, value: u64, label: &str) -> AnyElement {
    let _ = root;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(14.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(gpui::Rgba {
                    a: 0.85,
                    ..theme::text()
                })
                .child(value.to_string()),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(gpui::Rgba {
                    a: 0.75,
                    ..theme::text()
                })
                .child(label.to_string()),
        )
        .into_any_element()
}
