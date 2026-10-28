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
/// `reveal` 说明亮光走到哪：整行未亮 / 已亮一部分（按**字符位置**给出）/ 整行全亮。
/// 用字符位置而不是比例，是为了让渲染层能按真实字形宽度换算像素。
/// 把「亮到第几个字符」换算成**未折行坐标下的像素 x**。
///
/// 逐字形累加真实前进宽度，而不是用「字符比例 × 总宽」：
/// 字符宽度差异很大（`I` 窄、`friends` 宽），按比例算会让亮光位置与字形
/// 错位，末尾那段被一次性点亮，看起来就是"最后一个字一闪而过"。
///
/// `done` 可带小数（`7.42` = 前 7 字已满 + 第 8 字亮 42%），小数部分按
/// 该字形自身的宽度插值，所以亮光能停在**一个字中间**。
fn reveal_x_from_chars(line: &gpui::WrappedLine, done: f32, fallback_width: f32) -> f32 {
    let xs: Vec<f32> = line
        .runs()
        .iter()
        .flat_map(|run| run.glyphs.iter())
        .map(|glyph| f32::from(glyph.position.x))
        .collect();
    reveal_x_from_glyph_xs(&xs, done, fallback_width)
}

/// [`reveal_x_from_chars`] 的纯计算部分：只依赖各字形的左边界 x。
///
/// 抽出来是为了能脱离窗口环境做单元测试 —— 这里是"走得丝不丝滑"的核心，
/// 之前反复出错（比例当像素、末字不亮）。
fn reveal_x_from_glyph_xs(xs: &[f32], done: f32, fallback_width: f32) -> f32 {
    if xs.is_empty() || done <= 0.0 {
        return 0.0;
    }

    // `done` 已经是「`text` 里第几个字符」，与字形序列一一对应，直接索引即可。
    // （`total` 保留在签名里是为了让调用方显式表明这是字符位置，不再做缩放。）
    let whole = done.floor().max(0.0) as usize;
    let fraction = (done - whole as f32).clamp(0.0, 1.0);

    // 全部字符都亮完：返回一个足够大的 x，让所有视觉行都被整段裁剪。
    //
    // 不能返回最后一个字形的左边界 —— 那样**最后一个字永远不亮**，
    // 正是"最后一个字走不完"的直接来源。
    if whole >= xs.len() {
        return f32::MAX;
    }
    let start_x = xs[whole];
    if fraction <= 0.0 {
        return start_x;
    }
    // 该字形的自身宽度 = 下一个字形的左边界 - 本字形左边界。
    // 末字形没有"下一个"，用调用方补上的宽度（`fallback_width`）兜底：
    // 否则末字宽被算成 0，它的字内推进恒为 0 —— 表现就是"最后一个字不亮，
    // 直接跳到下一句"。这是实测复现过的 bug。
    let next_x = xs
        .get(whole + 1)
        .copied()
        .unwrap_or(start_x + fallback_width);
    let glyph_width = (next_x - start_x).max(0.0);
    start_x + glyph_width * fraction
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LyricReveal {
    /// 整行未点亮。
    None,
    /// 整行已点亮。
    All,
    /// 亮到 `done` 个字符（可含小数）。字符下标与 `text` 一一对应，
    /// 渲染层直接索引字形即可，不需要总数。
    Partial { done: f32 },
}

fn highlighted_line(
    text: &str,
    reveal: LyricReveal,
    base: gpui::Rgba,
    lit: gpui::Rgba,
) -> AnyElement {
    // 两端直接走普通文本：省掉 shaping 与两次绘制。
    if matches!(reveal, LyricReveal::None) || matches!(reveal, LyricReveal::All) {
        let color = if matches!(reveal, LyricReveal::All) {
            lit
        } else {
            base
        };
        return div()
            .text_color(color)
            .child(text.to_string())
            .into_any_element();
    }

    let base: gpui::Hsla = base.into();
    let lit: gpui::Hsla = lit.into();
    let shared: gpui::SharedString = text.to_string().into();

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
                // 字重必须与普通文本**完全一致**：当前行改用粗体会让字宽变化，
                // 高亮行与非高亮行的字距就对不上了（官方也是整段统一字重，
                // 只用颜色区分高亮）。这里直接用继承来的字体，不覆盖 weight。
                font: style.font(),
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
            (lines, line_height)
        },
        move |bounds, (lines, line_height), window, cx| {
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
                // 末字形没有"下一个字形"，它的宽度得单独估。
                //
                // 这个宽度有两个用途，缺了任何一个都会让**最后一个字永远不亮**：
                //   1. 末字的字内插值（否则推进恒为 0，亮光卡在末字左边界）；
                //   2. 末行总宽（`content_right` 若只取末字左边界，裁剪区域就
                //      不包含末字本身，末字被裁掉）。
                //
                // 不能用 `text_system().advance(font_id, ..)` 查：`font_id` 是主字体，
                // 而中日韩字符往往由 **fallback 字体**渲染，主字体查不到对应字形，
                // `advance` 直接失败返回 0 —— 末字就又回到"永远不亮"。
                // 这里改用与末字形**同一 run** 里前一个字形的间距：同一 run 意味着
                // 同一个实际字体，间距才是对的。只有一个字形时退化成整行宽度。
                let last_advance = {
                    let glyphs: Vec<&gpui::ShapedGlyph> =
                        runs.iter().flat_map(|run| run.glyphs.iter()).collect();
                    match glyphs.len() {
                        0 => 0.0,
                        1 => f32::from(base_line.width()),
                        _ => {
                            let last = glyphs[glyphs.len() - 1];
                            let prev = glyphs[glyphs.len() - 2];
                            let gap = f32::from(last.position.x) - f32::from(prev.position.x);
                            // 同一字体里中文等宽、西文差异不大；用一个正的间距兜底。
                            gap.max(0.0)
                        }
                    }
                };

                // 末行右端 = 末字形左边界 + 末字形宽度。
                //
                // 之前只取 `last_glyph_right`（左边界），末行宽度少了一个字，
                // 裁剪区域永远覆盖不到最后一个字 —— 实测"最后一个字不亮、
                // 直接跳到下一句"就是这个原因。
                let content_right = (last_glyph_right + last_advance).max(cursor_x);
                line_spans.push((cursor_x, content_right.max(cursor_x)));

                // 把"亮到第几个字符"换算成**像素位置**。
                //
                // 不能拿字符比例乘内容总宽：字符宽度差异很大（`I` 窄、`friends` 宽），
                // 比例和像素位置不成正比，末尾那段会被一次性点亮——表现为
                // "最后一个字一闪而过"。这里按字形逐个累加真实宽度。
                let revealed_x = match reveal {
                    LyricReveal::Partial { done } => {
                        reveal_x_from_chars(base_line, done, last_advance)
                    }
                    _ => 0.0,
                };
                let mut remaining = revealed_x;
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
                    // 用户已经自己滚了，过渡动画必须让位，否则会和手动滚动打架。
                    root.cancel_lyrics_scroll_tween();
                    cx.notify();
                }))
                .children(lines.iter().enumerate().map(|(index, line)| {
                    let next_start = lines.get(index + 1).map(|next| next.start_seconds);
                    // 本行此刻是否"正在唱"：区间是 `[start, 下一句开始)`。
                    //
                    // 上一句一直保持高亮到下一句接管为止 —— 走完后亮光停在整行全亮，
                    // 不会提前变暗。切句动画在**下一句开始之后**才起跑（滚动跟随
                    // 不再提前 250ms），所以顺序是：先切换高亮（上一句归还强调色、
                    // 下一句亮起），再播滚动动画把下一句带进来。
                    let position = root.lyrics_position_seconds;
                    let reveal = if line.is_active_at(position, next_start) {
                        match line.chars_revealed(position) {
                            Some((done, _total)) => LyricReveal::Partial { done },
                            // 普通 LRC（没有逐字轴）：整行直接算已亮。
                            None => LyricReveal::All,
                        }
                    } else {
                        LyricReveal::None
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
                        .child(highlighted_line(
                            &line.text,
                            reveal,
                            theme::lyric_dim(),
                            theme::accent(),
                        ))
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

#[cfg(test)]
mod tests {
    use super::reveal_x_from_glyph_xs;

    /// 各字形的左边界（未折行坐标）。注意宽度不等：
    /// `I` 很窄（10px），`wide` 很宽（60px），用来暴露"按比例算"的错误。
    const XS: [f32; 5] = [0.0, 10.0, 40.0, 100.0, 160.0];
    /// 末字形的宽度（末字后面没有"下一个字形"，必须由调用方补）。
    const LAST_W: f32 = 40.0;

    /// 走满时必须覆盖整行 —— 最后一个字要亮起来。
    ///
    /// 旧实现走到 `whole == glyphs.len()` 时返回最后一个字的**左边界**，
    /// 于是最后一个字永远不亮，就是"最后一个字走不完"。
    #[test]
    fn full_reveal_covers_every_glyph() {
        assert_eq!(
            reveal_x_from_glyph_xs(&XS, 5.0, LAST_W),
            f32::MAX,
            "走满必须覆盖整行，不能停在最后一个字的左边界"
        );
    }

    /// 字内插值按**该字形自身宽度**，不是按平均宽度。
    ///
    /// 第 1 个字宽 10px，第 2 个字宽 30px。走到 1.5 个字时，
    /// 应当是 10 + 30*0.5 = 25，而不是 (1.5/5)*总宽。
    #[test]
    fn partial_reveal_uses_each_glyph_width() {
        assert!((reveal_x_from_glyph_xs(&XS, 1.5, LAST_W) - 25.0).abs() < 1e-3);
        // 第 3 个字宽 60px，走到 2.5 个字 = 40 + 30 = 70。
        assert!((reveal_x_from_glyph_xs(&XS, 2.5, LAST_W) - 70.0).abs() < 1e-3);
    }

    /// 整字位置必须精确落在字形左边界上（否则字与字之间会有跳变）。
    #[test]
    fn integer_positions_snap_to_glyph_edges() {
        assert!((reveal_x_from_glyph_xs(&XS, 2.0, LAST_W) - 40.0).abs() < 1e-3);
        assert!((reveal_x_from_glyph_xs(&XS, 3.0, LAST_W) - 100.0).abs() < 1e-3);
    }

    /// **最后一个字必须能推进**：末字形没有"下一个字形"，宽度要靠
    /// 调用方补；补成 0 的话末字的字内推进恒为 0，亮光会卡在它的左边界，
    /// 然后直接跳到下一句（实测复现过的 bug）。
    #[test]
    fn last_glyph_advances_with_its_real_width() {
        let last_start = XS[XS.len() - 1]; // 160
                                           // 走到 4.5 个字：末字形亮一半 = 160 + 40*0.5 = 180。
        let x = reveal_x_from_glyph_xs(&XS, 4.5, LAST_W);
        assert!(
            (x - (last_start + LAST_W * 0.5)).abs() < 1e-3,
            "末字应能按自身宽度推进，得到 {x}"
        );
        // 宽度为 0 时末字确实推不动——这就是要防的情况。
        let stuck = reveal_x_from_glyph_xs(&XS, 4.5, 0.0);
        assert!((stuck - last_start).abs() < 1e-3);
    }

    /// 推进必须单调：不能出现亮光回退（回退会看到"抽一下"）。
    #[test]
    fn reveal_is_monotonic() {
        let mut prev = -1.0f32;
        let mut t = 0.0f32;
        while t <= 5.0 {
            let x = reveal_x_from_glyph_xs(&XS, t, LAST_W);
            assert!(x >= prev, "t={t} 处回退：{prev} -> {x}");
            prev = x;
            t += 0.05;
        }
    }

    /// 边界输入不能崩。
    #[test]
    fn reveal_handles_edge_inputs() {
        assert_eq!(reveal_x_from_glyph_xs(&[], 1.0, LAST_W), 0.0);
        assert_eq!(reveal_x_from_glyph_xs(&XS, 0.0, LAST_W), 0.0);
        // 超过字形数（走满或脏数据）：按走满处理，否则最后一个字不亮。
        assert_eq!(reveal_x_from_glyph_xs(&XS, 5.0, LAST_W), f32::MAX);
        assert_eq!(reveal_x_from_glyph_xs(&XS, 9.0, LAST_W), f32::MAX);
    }
}
