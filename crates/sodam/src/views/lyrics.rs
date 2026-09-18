//! 歌词播放页。

use super::scenes::retry_button;
use super::*;
/// 歌词播放页：左封面，右滚动歌词，当前行跟随播放进度。
pub(crate) fn lyrics_view(root: &Root, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    if root.switching_queue.is_some() {
        return div()
            .flex()
            .flex_col()
            .size_full()
            .child(loading_state(&root.switching_label))
            .into_any_element();
    }

    let snapshot = root.engine.snapshot();
    let Some(track) = root
        .pending_track
        .clone()
        .or_else(|| root.queue.current().cloned())
    else {
        return div()
            .flex()
            .flex_col()
            .size_full()
            .child(loading_state(root.tr("正在准备播放…")))
            .into_any_element();
    };

    let liked = root.liked_ids.contains(&track.id);
    let active = root.lyrics_active;
    let lines = root.lyrics.clone();
    let list = if root.lyrics.is_empty() {
        None
    } else {
        Some(
            uniform_list(
                "lyrics-list",
                root.lyrics.len(),
                move |range, _window, _cx| {
                    range
                        .map(|index| {
                            let Some(line) = lines.get(index) else {
                                return div().h(px(64.0)).into_any_element();
                            };
                            let is_active = active == Some(index);
                            let color = if is_active {
                                theme::accent()
                            } else {
                                theme::text_muted()
                            };
                            div()
                                .w_full()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .h(px(64.0))
                                .text_size(theme::Text::Large.size())
                                .line_height(px(32.0))
                                .text_color(color)
                                .when(is_active, |this| this.font_weight(gpui::FontWeight::BOLD))
                                .child(line.text.clone())
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                },
            )
            .flex_1()
            .min_h(px(0.0))
            .track_scroll(&root.lyrics_scroll)
            .into_any_element(),
        )
    };

    let cover_path = root
        .original_covers
        .get(&track.cover)
        .cloned()
        .or_else(|| root.cover_of(&track.cover));
    let original_cover_ready = root.original_covers.contains_key(&track.cover);
    let like_button = div()
        .id("playback-like")
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(theme::size::CONTROL_SM))
        .rounded(px(theme::radius::PILL))
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener({
            let track = track.clone();
            move |root, _event: &ClickEvent, _window, cx| {
                root.toggle_like(track.clone(), cx);
            }
        }))
        .child(
            svg()
                .path(icons::path(if liked { "heart-filled" } else { "heart" }))
                .size(px(theme::ICON_SM))
                .text_color(if liked {
                    theme::accent()
                } else {
                    theme::text_muted()
                }),
        );

    // 汽水播放页的规则：封面取 min(40vh, 30vw)；
    // 歌词列是 30vw，并限制在 300~500px。这里 vh/vw 以可用播放区为准。
    let viewport_width = f32::from(window.viewport_size().width);
    let viewport_height = (f32::from(window.viewport_size().height) - theme::PLAYER_H).max(360.0);
    let mut cover_size = (viewport_height * 0.40).min(viewport_width * 0.30);
    let mut lyrics_width = (viewport_width * 0.30).clamp(300.0, 500.0);

    // 窗口被平铺 WM 压得比官方最小尺寸还窄时，按同一比例整体收缩，
    // 避免固定 300px 歌词下限把内容挤出可视区。
    let centered_width = (viewport_width - theme::SIDEBAR_W - 100.0).max(220.0);
    let natural_width = cover_size + 50.0 + lyrics_width;
    if natural_width > centered_width {
        let scale = centered_width / natural_width;
        cover_size *= scale;
        lyrics_width *= scale;
    }

    let track_info = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(theme::space::XS))
        .w(px(cover_size))
        .max_w_full()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(px(theme::space::SM))
                .max_w_full()
                .min_w(px(0.0))
                .child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(theme::Text::Large.size())
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::text())
                        .child(track.title.clone()),
                )
                .when(track.vip, |this| this.child(vip_badge()))
                .child(like_button),
        )
        .child(
            div()
                .id("playback-artist-link")
                .max_w_full()
                .truncate()
                .cursor_pointer()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .hover(|style| style.text_color(theme::accent()))
                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                    cx.stop_propagation();
                })
                .on_click(cx.listener({
                    let track = track.clone();
                    move |root, _event: &ClickEvent, _window, cx| {
                        root.open_track_artist(track.clone(), cx);
                    }
                }))
                .child(track.artist.clone()),
        );

    let right_module = if root.queue_open {
        div()
            .w(px(lyrics_width))
            .max_w(px(500.0))
            .min_w(px(300.0))
            .h_full()
            .flex_none()
            .min_h(px(0.0))
            .child(crate::ui::player_bar::queue_panel(root, cx))
            .into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
            .w(px(lyrics_width))
            .max_w(px(500.0))
            .min_w(px(300.0))
            .h_full()
            .flex_none()
            .min_h(px(0.0))
            .gap(px(theme::space::MD))
            .child(
                div()
                    .text_size(theme::Text::Title.size())
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(theme::text())
                    .truncate()
                    .child(track.title.clone()),
            )
            .child(
                div()
                    .text_size(theme::Text::Small.size())
                    .text_color(theme::text_muted())
                    .child(format!(
                        "{} · {} · {}",
                        track.artist,
                        track.duration_label(),
                        snapshot.progress_label()
                    )),
            )
            .child(if root.loading_lyrics {
                loading_state(root.tr("正在读取歌词…")).into_any_element()
            } else if let Some(error) = &root.lyrics_error {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(theme::space::SM))
                    .child(
                        div()
                            .text_size(theme::Text::Small.size())
                            .text_color(theme::text_muted())
                            .child(error.clone()),
                    )
                    .child(retry_button(root.language, cx))
                    .into_any_element()
            } else if root.lyrics.is_empty() {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(theme::space::SM))
                    .child(empty_state("music", root.tr("这首歌暂时没有歌词")))
                    .child(retry_button(root.language, cx))
                    .into_any_element()
            } else {
                list.expect("loaded lyrics have a list")
            })
            .into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .px(px(50.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .h_full()
                        .items_start()
                        .justify_center()
                        .max_w_full()
                        .flex_none()
                        .min_h(px(0.0))
                        .gap(px(50.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .h_full()
                                .gap(px(theme::space::LG))
                                .w(px(cover_size))
                                .flex_none()
                                .child(crate::ui::artwork::cover_loading(
                                    cover_path,
                                    cover_size,
                                    theme::radius::CARD,
                                ))
                                .when(!original_cover_ready, |this| {
                                    this.child(retry_button(root.language, cx))
                                })
                                .child(track_info),
                        )
                        .child(right_module),
                ),
        )
        .into_any_element()
}
