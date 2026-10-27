//! 歌词播放页。

use super::scenes::retry_button;
use super::*;

/// 歌词排版沿用 SodaM 原本的样式：字号 `Text::Large`、行高 32、每句间距 32。
/// 不采用官方客户端的 24px/34 字号行高——那套在 SodaM 里显得过大。
const LYRIC_LINE_HEIGHT: f32 = 32.0;
/// 相邻两句之间的间距（单句文本 32 + 间距 32 = 原来每行 64 的效果）。
const LYRIC_ROW_GAP: f32 = 32.0;

/// 一行歌词的逐字高亮文字。
///
/// 官方客户端用 CSS 的 `background-clip: text` + 逐帧调 `background-position-x`，
/// 所以亮光能停在**一个字中间**（亚像素级平滑）。gpui 没有 background-clip，
/// 但等价的东西做得到：把同一段文字画两遍，第二遍用像素矩形裁出"已唱到"的宽度。
///
/// 为什么不能继续用 `TextRun` 分段：`TextRun` 按字节区间切，必然只能整个字跳变，
/// 字内没有过渡 —— 这就是"能走字但不丝滑"的原因。
///
/// 两层文字共用**同一次 shaping**（同一份 `WrappedLine`），所以折行、字距、基线
/// 完全一致，不会出现两层对不齐。
///
/// `progress` 是这一行的时间进度：`0.0` = 整行未点亮，`1.0` = 整行已点亮。
fn highlighted_line(
    text: &str,
    progress: f32,
    base: gpui::Rgba,
    lit: gpui::Rgba,
    bold: bool,
) -> AnyElement {
    let progress = progress.clamp(0.0, 1.0);

    // 两端直接走普通文本：省掉 shaping 与两次绘制。
    if progress >= 1.0 || progress <= 0.0 {
        let color = if progress >= 1.0 { lit } else { base };
        return div()
            .text_color(color)
            .when(bold, |this| this.font_weight(gpui::FontWeight::BOLD))
            .child(text.to_string())
            .into_any_element();
    }

    let base: gpui::Hsla = base.into();
    let lit: gpui::Hsla = lit.into();
    let shared: gpui::SharedString = text.to_string().into();
    let weight = if bold {
        gpui::FontWeight::BOLD
    } else {
        gpui::FontWeight::NORMAL
    };

    let overlay = gpui::canvas(
        move |bounds, window, _cx| {
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let line_height = window.pixel_snap(
                style
                    .line_height
                    .to_pixels(font_size.into(), window.rem_size()),
            );
            let run = |color| gpui::TextRun {
                len: shared.len(),
                font: gpui::Font {
                    weight,
                    ..style.font()
                },
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            // 按可用宽度折行：与普通文本一致，长句不会一路撑宽被裁（#56）。
            let wrap = Some(bounds.size.width);
            // 只 shape 一次：第二遍绘制用 `paint_tinted` 覆盖颜色，
            // 不需要为了换色再 shape 一遍（省掉一半排版开销）。
            let lines = window
                .text_system()
                .shape_text(shared.clone(), font_size, &[run(base)], wrap, None)
                .ok()
                .map(|lines| lines.into_iter().collect::<Vec<_>>())
                .unwrap_or_default();
            (lines, line_height, font_size)
        },
        move |bounds, (lines, line_height, _font_size), window, cx| {
            // 已唱到的宽度按**像素**算，不对齐字符边界 —— 这是字内过渡的全部来源。
            let origin_x = bounds.origin.x;

            for base_line in lines.iter() {
                let block_origin = gpui::point(origin_x, bounds.origin.y);

                // 第 1 遍：整块底色（未唱部分）。一次画完，不用管折行。
                let _ = base_line.paint(
                    block_origin,
                    line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );

                // 第 2 遍：亮色重画，按**视觉行**逐段裁剪。
                //
                // 一个 `WrappedLine` 是**整段文本**，折行后含多个视觉行；`size()` 是
                // 整块尺寸，不是单行尺寸（#80 实测：直接用它会让第二行整行亮或永远不亮）。
                //
                // 每个视觉行在**未折行**坐标里的 x 区间：
                //   起点 = 上一个折行点 glyph 的 x，终点 = 本折行点（末行取整段宽度）。
                // 相减就是该行的真实宽度。runs() 给的是未折行坐标，正好可以直接用。
                let boundaries = base_line.wrap_boundaries();
                let runs = base_line.runs();
                let glyph_x = |run_ix: usize, glyph_ix: usize| -> f32 {
                    runs.get(run_ix)
                        .and_then(|run| run.glyphs.get(glyph_ix))
                        .map(|glyph| f32::from(glyph.position.x))
                        .unwrap_or(0.0)
                };
                // 每行的未折行 x 起点/终点。
                let mut line_spans: Vec<(f32, f32)> = Vec::with_capacity(boundaries.len() + 1);
                let mut cursor_x = 0.0f32;
                for boundary in boundaries {
                    let x = glyph_x(boundary.run_ix, boundary.glyph_ix);
                    line_spans.push((cursor_x, x));
                    cursor_x = x;
                }
                // 最后一行的右端：用**最后一个 glyph 的 x + 它的步进**，
                // 不能用 `width()`——那是折行后的容器宽度（等于列宽），
                // 末行通常远短于它，用错会把末行宽度算成“列宽 - 已用”。
                let last_glyph_right = runs
                    .iter()
                    .flat_map(|run| run.glyphs.iter())
                    .last()
                    .map(|glyph| f32::from(glyph.position.x))
                    .unwrap_or(0.0);
                // 末行宽度 = 整段宽度 - 末行起点，但这个“整段宽度”要用真实内容宽度。
                let content_right = last_glyph_right.max(cursor_x);
                line_spans.push((cursor_x, content_right.max(cursor_x)));

                // 本段（这一句）的内容总宽：折行后所有视觉行之和。
                //
                // **必须用内容宽度，不能用 `bounds.size.width`（列宽）**：
                // 折行后内容总宽可能远大于列宽（实测 455px vs 300px），用列宽乘进度
                // 会让进度永远到不了后面的视觉行——progress=1 也只点亮第一行（#80）。
                let content_width: f32 = line_spans.iter().map(|(a, b)| (b - a).max(0.0)).sum();
                let mut remaining = content_width * progress;
                for (visual, (start_x, end_x)) in line_spans.iter().enumerate() {
                    if remaining <= 0.0 {
                        break;
                    }
                    let line_width = (end_x - start_x).max(0.0);
                    let visible = remaining.min(line_width);
                    if visible > 0.0 {
                        let top = bounds.origin.y + line_height * visual as f32;
                        let mask = gpui::ContentMask {
                            bounds: gpui::Bounds {
                                origin: gpui::point(origin_x, top),
                                size: gpui::size(gpui::px(visible), line_height),
                            },
                        };
                        // 第二遍用 `paint_tinted` 把字形染成亮色，
                        // 复用同一份 shaping 结果。
                        window.with_content_mask(Some(mask), |window| {
                            let _ = base_line.paint_tinted(
                                block_origin,
                                line_height,
                                gpui::TextAlign::Left,
                                None,
                                lit,
                                window,
                                cx,
                            );
                        });
                    }
                    remaining -= line_width;
                }
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element();

    // 高度必须由**真实文本**撑出来：canvas 没有固有高度，单独用它会让外层布局
    // 把整行压成 0（表现就是下一句贴在这一句上）。所以：
    //   1. 先用一段同字号同字重的**透明文本**参与布局，决定高度与折行；
    //   2. canvas 绝对定位覆盖在它上面，负责真正的两遍绘制。
    // 两者用同一份文字和同样的宽度约束，折行位置一致。
    div()
        .relative()
        .w_full()
        .child(
            div()
                .w_full()
                .text_color(gpui::transparent_black())
                .child(text.to_string()),
        )
        .child(overlay)
        .into_any_element()
}

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
                    // 已唱过的行保持强调色，当前行按进度逐字点亮（#80）。
                    //
                    // 主流客户端（官方 / Spotify / Apple Music）都是这个模型：
                    // 当前行整行已是强调色，亮光在行内从左推到右，“推到哪”靠时间轴。
                    // 所以底色用强调色、未亮部分压暗，而不是分两级不同颜色。
                    let passed = active.is_some_and(|current| index < current);
                    let (base, lit, bold) = if is_active {
                        (theme::accent_dimmed(), theme::accent(), true)
                    } else if passed {
                        (theme::text_muted(), theme::text_muted(), false)
                    } else {
                        (theme::text_faint(), theme::text_faint(), false)
                    };
                    // 只有当前行才做逐字推进：其他行要么整行已点亮要么完全未点亮。
                    // 用连续比例而不是字节边界，字内才能有过渡（#80）。
                    let progress = if is_active {
                        line.highlight_fraction(root.lyrics_position_seconds)
                    } else if passed {
                        1.0
                    } else {
                        0.0
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
                        .child(highlighted_line(&line.text, progress, base, lit, bold))
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
