//! 歌词播放页。

use super::scenes::retry_button;
use super::*;

/// 歌词排版沿用 SodaM 原本的样式：字号 `Text::Large`、行高 32、每句间距 32。
/// 不采用官方客户端的 24px/34 字号行高——那套在 SodaM 里显得过大。
const LYRIC_LINE_HEIGHT: f32 = 32.0;
/// 相邻两句之间的间距（单句文本 32 + 间距 32 = 原来每行 64 的效果）。
const LYRIC_ROW_GAP: f32 = 32.0;

/// 歌词区没有内容时（加载中 / 出错 / 无歌词）的占位：铺满右侧栏并垂直居中。
fn centered_state(content: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .child(content)
        .into_any_element()
}

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
    // 歌词区高度：滚到中间要给首尾句留出半个视口的空白，所以先算出来。
    let lyrics_viewport_height =
        (f32::from(window.viewport_size().height) - theme::PLAYER_H).max(360.0);
    let lyric_padding = lyrics_viewport_height / 2.0;
    let list = if root.lyrics.is_empty() {
        None
    } else {
        Some(
            div()
                .id("lyrics-scroll")
                // 整块歌词自己滚动。这里不用虚拟列表：长句折行后每句高度都不一样，
                // 等高列表会把折出来的第二行裁掉（#56）。
                .flex()
                .flex_col()
                .w_full()
                .flex_1()
                .min_h(px(0.0))
                .pt(px(lyric_padding))
                .pb(px(lyric_padding))
                .overflow_y_scroll()
                .track_scroll(&root.lyrics_scroll)
                // 用户滚动时暂停自动跟随 5 秒（官方行为），否则往回翻会被拽回当前句。
                .on_scroll_wheel(cx.listener(|root, _event, _window, cx| {
                    root.lyrics_user_scroll_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                    cx.notify();
                }))
                .children(lines.iter().enumerate().map(|(index, line)| {
                    let is_active = active == Some(index);
                    let color = if is_active {
                        theme::accent()
                    } else {
                        theme::text_muted()
                    };
                    div()
                        .w_full()
                        // 句间距用内边距表达，不能用 `min_h`：`min_h` 撑出的空当
                        // 会被折行文本吃掉，折成两行时间距就变成 0。
                        // 上下各 16，相邻两句之间正好 32；单句总高仍是 64。
                        .py(px(LYRIC_ROW_GAP / 2.0))
                        // 这里不能加 `flex`：flex 容器给子元素的可用宽度是 MaxContent，
                        // gpui 的文本只在拿到确定宽度时才折行，结果就是长句一路撑宽被裁。
                        .text_size(theme::Text::Large.size())
                        .line_height(px(LYRIC_LINE_HEIGHT))
                        .text_color(color)
                        .when(is_active, |this| this.font_weight(gpui::FontWeight::BOLD))
                        .child(line.text.clone())
                        .into_any_element()
                }))
                .into_any_element(),
        )
    };

    let cover_path = root
        .original_covers
        .get(&track.cover)
        .cloned()
        .or_else(|| root.cover_of(&track.cover));
    let original_cover_ready = root.original_covers.contains_key(&track.cover);
    // 切曲后的前 200ms 强制显示加载态（即便图在缓存里）。
    let cover_loading_gate = root
        .lyrics_cover_gate
        .as_ref()
        .map(|(id, at)| id == &track.id && at.elapsed() < std::time::Duration::from_millis(200))
        .unwrap_or(false);
    let cover_path = if cover_loading_gate { None } else { cover_path };
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
            .child(if root.loading_lyrics {
                centered_state(loading_state(root.tr("正在读取歌词…")).into_any_element())
            } else if let Some(error) = &root.lyrics_error {
                centered_state(
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
                        .into_any_element(),
                )
            } else if root.lyrics.is_empty() {
                centered_state(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(theme::space::SM))
                        .child(empty_state("music", root.tr("这首歌暂时没有歌词")))
                        .child(retry_button(root.language, cx))
                        .into_any_element(),
                )
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
                                .child(
                                    div()
                                        .relative()
                                        .w(px(cover_size))
                                        .h(px(cover_size))
                                        .flex_none()
                                        .child(crate::ui::artwork::cover_loading(
                                            cover_path,
                                            cover_size,
                                            theme::radius::CARD,
                                        ))
                                        // 原图加载失败：把重试按钮放进封面内（底部居中），
                                        // 不再占封面下方一行。
                                        .when(
                                            !original_cover_ready && !cover_loading_gate,
                                            |this| {
                                                this.child(
                                                    div()
                                                        .absolute()
                                                        .left_0()
                                                        .right_0()
                                                        .bottom(px(cover_size * 0.10))
                                                        .flex()
                                                        .justify_center()
                                                        .child(retry_button(root.language, cx)),
                                                )
                                            },
                                        ),
                                )
                                .child(track_info),
                        )
                        .child(right_module),
                ),
        )
        .into_any_element()
}
