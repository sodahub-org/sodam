//! 官方风格搜索结果页，以及音乐人 / 专辑详情页。

use super::*;
use gpui::{MouseMoveEvent, MouseUpEvent};
/// 搜索框：单击定位、拖拽扩选、双击选词、三击全选；按键走 keymap + action（见 `app_text`）。
fn search_box(root: &Root, window: &Window, cx: &mut Context<Root>) -> impl IntoElement {
    let focused = root.search_focus.is_focused(window);
    let input_focus = root.search_focus.clone();
    let input_entity = cx.entity();
    let empty = root.search_field.is_empty();
    // 空框用正常布局画占位文案（天然居中）；有内容时真实文本交给下面的 canvas 直接
    // `line.paint`，因为只有这样才能跟着 `scroll_x` 走，而且它和光标、选区用的是
    // 同一条 ShapedLine —— 三者不会互相错位。
    let flow_text = if empty {
        root.tr("搜索歌曲、歌手、专辑或歌单").to_string()
    } else {
        String::new()
    };
    let color = if empty {
        theme::text_muted()
    } else {
        theme::text()
    };
    let line = root.search_shaped_line_with(window, theme::text());
    let scroll = root.search_scroll_x();
    // 统一成「相对文本区左边缘」的坐标，canvas 里再加回 bounds.origin.x。
    let selection = root
        .search_selection_edges(window)
        .map(|(start, end)| (start - scroll, end - scroll));
    let caret_x = root.search_caret_x(window);
    // 选区色取主题强调色加透明度；失焦时不消失，只是淡下来（与浏览器/Chromium 一致：
    // 非活跃选区是「同色 + fading」，不是没有）。
    let selection_color = gpui::Rgba {
        a: if focused { 0.30 } else { 0.15 },
        ..theme::accent()
    };

    div()
        .id("search-input")
        .relative()
        .track_focus(&root.search_focus)
        // 播放快捷键绑在 `!input` 上：这里打字时不能让 Space 之类触发播放动作。
        .key_context(crate::shortcuts::INPUT_CONTEXT)
        .flex()
        .flex_row()
        .items_center()
        .h(px(38.0))
        .px_3()
        .rounded_md()
        .bg(theme::surface())
        .border_1()
        .border_color(if focused {
            theme::accent()
        } else {
            theme::border()
        })
        .text_color(color)
        .cursor_text()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, event: &MouseDownEvent, window, cx| {
                window.focus(&root.search_focus, cx);
                // 单击定位插入点、双击选词、三击全选（Chromium 的单行框三击就是全选）。
                let changed =
                    root.search_pointer_down(event.position.x, event.click_count, window);
                if changed {
                    cx.notify();
                }
            }),
        )
        .on_mouse_move(
            cx.listener(|root, event: &MouseMoveEvent, window, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                let mut changed = root.search_pointer_drag(event.position.x, window);
                // 拖到边缘内侧 16px 就横滚，滚完必须再算一次端点：指针没动，但字动了。
                if let Some(bounds) = root.search_bounds.get() {
                    let width = bounds.size.width;
                    let left_edge = bounds.origin.x + px(16.0);
                    let right_edge = bounds.origin.x + width - px(16.0);
                    let scrolled = if event.position.x < left_edge {
                        root.search_scroll_by(-px(12.0), window, width)
                    } else if event.position.x > right_edge {
                        root.search_scroll_by(px(12.0), window, width)
                    } else {
                        false
                    };
                    if scrolled {
                        changed |= root.search_pointer_drag(event.position.x, window);
                    }
                }
                if changed {
                    cx.notify();
                }
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|root, _event: &MouseUpEvent, _window, _cx| {
                root.search_pointer_up();
            }),
        )
        // 编辑键不在 on_key_down 里逐个匹配字符串：gpui 的 keymap 匹配修饰键是精确的，
        // 绑定表集中在 app_text::bindings()，一处读得全（可输入字符仍走 EntityInputHandler）。
        .on_action(cx.listener(Root::text_backspace))
        .on_action(cx.listener(Root::text_delete))
        .on_action(cx.listener(Root::text_delete_word_backward))
        .on_action(cx.listener(Root::text_left))
        .on_action(cx.listener(Root::text_right))
        .on_action(cx.listener(Root::text_word_left))
        .on_action(cx.listener(Root::text_word_right))
        .on_action(cx.listener(Root::text_select_left))
        .on_action(cx.listener(Root::text_select_right))
        .on_action(cx.listener(Root::text_select_word_left))
        .on_action(cx.listener(Root::text_select_word_right))
        .on_action(cx.listener(Root::text_home))
        .on_action(cx.listener(Root::text_end))
        .on_action(cx.listener(Root::text_select_home))
        .on_action(cx.listener(Root::text_select_end))
        .on_action(cx.listener(Root::text_select_all))
        .on_action(cx.listener(Root::text_copy))
        .on_action(cx.listener(Root::text_cut))
        .on_action(cx.listener(Root::text_paste))
        .on_action(cx.listener(Root::text_undo))
        .on_action(cx.listener(Root::text_redo))
        .on_action(cx.listener(Root::text_submit))
        .on_action(cx.listener(Root::text_cancel))
        .child(
            // 26 = 占位文案的行盒高度，也是原来那条 4 + 18 + 4 的光标几何所暗示的高度；
            // 显式定高之后，真实文本改由 canvas 绘制，盒子才不会塌成 0。
            div()
                .id("search-text")
                .relative()
                .flex()
                .items_center()
                .flex_1()
                .min_w(px(0.0))
                .h(px(26.0))
                .overflow_hidden()
                .child(flow_text)
                .child(
                    canvas(
                        {
                            let bounds_cell = root.search_bounds.clone();
                            move |bounds, _window, _cx| {
                                // 记下文本区位置：鼠标点击与拖拽都要用它把 x 换算成字符下标。
                                bounds_cell.set(Some(bounds));
                                bounds
                            }
                        },
                        move |bounds, _state, window, cx| {
                            let line_height = window.line_height();
                            // 在盒子里竖向居中，跟 flow 布局的占位文案同一套算法。
                            let top = bounds.origin.y
                                + px((f32::from(bounds.size.height) - f32::from(line_height)) / 2.0);
                            // 先画选区，再画字，最后画光标：顺序即层级。
                            if let Some((start, end)) = selection {
                                let quad = gpui::fill(
                                    gpui::Bounds::from_corners(
                                        gpui::point(bounds.origin.x + start, bounds.origin.y),
                                        gpui::point(bounds.origin.x + end, bounds.origin.y + bounds.size.height),
                                    ),
                                    selection_color,
                                );
                                window.paint_quad(quad);
                            }
                            if let Some(line) = &line {
                                let _ = line.paint(
                                    gpui::point(bounds.origin.x - scroll, top),
                                    line_height,
                                    gpui::TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            }
                            if focused {
                                let caret = gpui::fill(
                                    gpui::Bounds::new(
                                        gpui::point(bounds.origin.x + caret_x, top),
                                        gpui::size(crate::app::CARET_WIDTH, line_height),
                                    ),
                                    theme::accent(),
                                );
                                window.paint_quad(caret);
                            }
                            window.handle_input(
                                &input_focus,
                                gpui::ElementInputHandler::new(bounds, input_entity.clone()),
                                cx,
                            );
                        },
                    )
                    .absolute()
                    .size_full(),
                ),
        )
}

/// 数字缩写：官方搜索里的“2442.2万人关注”。
fn compact_number(value: i64, language: crate::ui::i18n::Language) -> String {
    let value = value.max(0) as f64;
    let render = |scaled: f64, suffix: &str| {
        let mut text = if scaled >= 10.0 {
            format!("{scaled:.0}")
        } else {
            format!("{scaled:.1}")
        };
        if text.ends_with(".0") {
            text.truncate(text.len() - 2);
        }
        format!("{text}{suffix}")
    };
    if language.resolved().is_zh() {
        if value >= 100_000_000.0 {
            render(value / 100_000_000.0, language.text("亿"))
        } else if value >= 10_000.0 {
            render(value / 10_000.0, language.text("万"))
        } else {
            value.to_string()
        }
    } else if value >= 1_000_000_000.0 {
        render(value / 1_000_000_000.0, "B")
    } else if value >= 1_000_000.0 {
        render(value / 1_000_000.0, "M")
    } else if value >= 1_000.0 {
        render(value / 1_000.0, "K")
    } else {
        value.to_string()
    }
}

/// 搜索框聚焦且输入为空时的本机搜索历史。
fn search_history_panel(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let chips = root
        .settings
        .search_history
        .iter()
        .enumerate()
        .map(|(index, query)| {
            let select_query = query.clone();
            let remove_query = query.clone();
            div()
                .id(("search-history-chip", index))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::space::XS))
                .px(px(theme::space::MD))
                .py(px(theme::space::XS))
                .rounded(px(theme::radius::PILL))
                .bg(theme::surface_hover())
                .max_w(px(240.0))
                .child(
                    div()
                        .id(("search-history-select", index))
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text())
                        .cursor_pointer()
                        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                            root.run_history_search(select_query.clone(), cx);
                        }))
                        .child(remove_query.clone()),
                )
                .child(
                    div()
                        .id(("search-history-remove", index))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(18.0))
                        .rounded_full()
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_selected()))
                        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                            root.remove_search_history(index, cx);
                        }))
                        .child(
                            svg()
                                .path(icons::path("x"))
                                .size(px(12.0))
                                .text_color(theme::text_faint()),
                        ),
                )
        })
        .collect::<Vec<_>>();

    div()
        .id("search-history-panel")
        .flex()
        .flex_col()
        .gap(px(theme::space::SM))
        .p(px(theme::space::MD))
        .rounded(px(theme::radius::CARD))
        .bg(theme::surface_elevated())
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(root.tr("搜索历史")),
                )
                .child(
                    div()
                        .id("search-history-clear")
                        .cursor_pointer()
                        .text_size(theme::Text::Tiny.size())
                        .text_color(theme::text_muted())
                        .hover(|style| style.text_color(theme::text()))
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.clear_search_history(cx);
                        }))
                        .child(root.tr("清空")),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(theme::space::SM))
                .children(chips),
        )
        .into_any_element()
}

/// 秒级时间戳 → `YYYY-MM-DD`；官方专辑页会展示发行时间。
fn release_date_label(timestamp_seconds: i64) -> String {
    if timestamp_seconds <= 0 {
        return String::new();
    }
    // Howard Hinnant 的 civil_from_days，可避免仅为了格式化日期引入 chrono。
    let days = timestamp_seconds.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// 搜索页 Tab，对齐官方综合 / 歌曲 / 音乐人 / 专辑 / 歌单。
fn search_tabs(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let tabs = [
        (SearchScope::All, root.tr("综合")),
        (SearchScope::Tracks, root.tr("歌曲")),
        (SearchScope::Artists, root.tr("音乐人")),
        (SearchScope::Albums, root.tr("专辑")),
        (SearchScope::Playlists, root.tr("歌单")),
    ];
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::space::SM))
        .children(tabs.map(|(scope, label)| {
            let selected = root.search_tab == scope;
            div()
                .id(gpui::ElementId::Name(
                    format!("search-tab-{scope:?}").into(),
                ))
                .flex()
                .items_center()
                .h(px(theme::size::CONTROL_SM))
                .px(px(theme::space::MD))
                .rounded(px(theme::radius::PILL))
                .text_size(theme::Text::Small.size())
                .text_color(if selected {
                    theme::accent_foreground()
                } else {
                    theme::text_muted()
                })
                .bg(if selected {
                    theme::accent()
                } else {
                    theme::surface_elevated()
                })
                .cursor_pointer()
                .hover(|style| {
                    style.bg(if selected {
                        theme::accent()
                    } else {
                        theme::surface_hover()
                    })
                })
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    root.set_search_tab(scope, cx);
                }))
                .child(label)
        }))
        .into_any_element()
}

/// 搜索结果中的音乐人行。
fn artist_row(root: &Root, artist: &ArtistItem, cx: &mut Context<Root>) -> AnyElement {
    let avatar_path = root.cover_of(&artist.avatar);
    let item = artist.clone();
    div()
        .id(gpui::ElementId::Name(
            format!("artist-{}", artist.id).into(),
        ))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::space::MD))
        .h(px(72.0))
        .px(px(theme::space::MD))
        .rounded(px(theme::radius::ROW))
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
            root.open_artist_page(item.clone(), cx);
        }))
        .child(cover(avatar_path, 52.0, 999.0))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .child(
                    div()
                        .truncate()
                        .text_size(theme::Text::Body.size())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .child(artist.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.localized(
                            "{}人关注 · {}首歌",
                            &[
                                compact_number(artist.follower_count, root.language),
                                compact_number(artist.track_count, root.language),
                            ],
                        )),
                ),
        )
        .into_any_element()
}

/// 搜索结果中的专辑行。
fn album_row(root: &Root, album: &AlbumItem, cx: &mut Context<Root>) -> AnyElement {
    let cover_path = root.cover_of(&album.cover);
    let item = album.clone();
    let mut subtitle = if root.language.resolved().is_zh() {
        root.localized("{} 首", &[album.track_count.to_string()])
    } else {
        format!("{} tracks", album.track_count)
    };
    if !album.artist.is_empty() {
        subtitle.push_str(&format!(" · {}", album.artist));
    }
    let release = release_date_label(album.release_date);
    if !release.is_empty() {
        subtitle.push_str(&format!(" · {release}"));
    }
    div()
        .id(gpui::ElementId::Name(format!("album-{}", album.id).into()))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::space::MD))
        .h(px(72.0))
        .px(px(theme::space::MD))
        .rounded(px(theme::radius::ROW))
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
            root.open_album_page(item.clone(), cx);
        }))
        .child(cover(cover_path, 52.0, theme::radius::ART))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .child(
                    div()
                        .truncate()
                        .text_size(theme::Text::Body.size())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .child(album.title.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(subtitle),
                ),
        )
        .into_any_element()
}

/// 搜索结果中的歌单行。
fn search_playlist_row(
    root: &Root,
    playlist: &sodam_core::models::PlaylistItem,
    cx: &mut Context<Root>,
) -> AnyElement {
    let cover_path = root.cover_of(&playlist.cover);
    let item = playlist.clone();
    let mut subtitle = if root.language.resolved().is_zh() {
        root.localized("{} 首", &[playlist.track_count.to_string()])
    } else {
        format!("{} tracks", playlist.track_count)
    };
    if !playlist.creator.is_empty() {
        subtitle.push_str(&format!(" · {}", playlist.creator));
    }
    div()
        .id(gpui::ElementId::Name(
            format!("search-playlist-{}", playlist.id).into(),
        ))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::space::MD))
        .h(px(72.0))
        .px(px(theme::space::MD))
        .rounded(px(theme::radius::ROW))
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
            root.open_search_playlist(item.clone(), cx);
        }))
        .child(cover(cover_path, 52.0, theme::radius::ART))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .child(
                    div()
                        .truncate()
                        .text_size(theme::Text::Body.size())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .child(playlist.title.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(subtitle),
                ),
        )
        .into_any_element()
}

/// 综合页歌曲行：转发到歌曲列表的共用行实现。
#[allow(clippy::too_many_arguments)]
fn search_track_row(
    root: &Root,
    track: &sodam_core::models::TrackItem,
    tracks: std::sync::Arc<Vec<sodam_core::models::TrackItem>>,
    index: usize,
    cx: &mut Context<Root>,
) -> AnyElement {
    let entity = cx.entity();
    let measured_width = root.list_width.lock().map(|width| *width).unwrap_or(1200.0);
    let cols = columns_for((measured_width - theme::space::SM * 2.0).max(320.0));
    track_row(
        &entity,
        track,
        index,
        cols,
        tracks,
        root.liked_ids.clone(),
        root.covers.clone(),
        root.cover_requests.clone(),
        root.queue.current().map(|track| track.id.clone()),
    )
}

/// 综合搜索的分组卡片。
fn search_groups_view(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    if root.search_keyword.trim().is_empty() {
        return empty_state("search", root.tr("输入关键词进行搜索")).into_any_element();
    }

    let groups = root.search_results.groups.clone();
    let width_slot = root.list_width.clone();
    div()
        .id("search-groups")
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::space::MD))
        .overflow_y_scroll()
        .child(
            canvas(
                move |bounds, window, _cx| {
                    let width = f32::from(bounds.size.width);
                    let changed = width_slot
                        .lock()
                        .map(|mut slot| {
                            let changed = *slot != width;
                            *slot = width;
                            changed
                        })
                        .unwrap_or(false);
                    // 下一帧用真实卡片宽度重算歌曲列，避免首帧时长被裁掉。
                    if changed {
                        window.refresh();
                    }
                },
                |_bounds, _state, _window, _cx| {},
            )
            .absolute()
            .size_full(),
        )
        .when(groups.is_empty(), |this| {
            this.child(empty_state("search", root.tr("没有找到相关内容")))
        })
        .children(groups.into_iter().map(|group| {
            let tracks: std::sync::Arc<Vec<sodam_core::models::TrackItem>> = std::sync::Arc::new(
                group
                    .entries
                    .iter()
                    .filter_map(|entry| match entry {
                        SearchEntry::Track(track) => Some(track.clone()),
                        _ => None,
                    })
                    .collect(),
            );
            let mut track_index = 0usize;
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .p(px(theme::space::SM))
                .rounded(px(theme::radius::CARD))
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .when(!group.title.is_empty(), |this| {
                    this.child(
                        div()
                            .px(px(theme::space::MD))
                            .pt(px(theme::space::XS))
                            .text_size(theme::Text::Small.size())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::text_muted())
                            .child(group.title.clone()),
                    )
                })
                .children(group.entries.iter().map(|entry| match entry {
                    SearchEntry::Track(track) => {
                        let index = track_index;
                        track_index += 1;
                        search_track_row(root, track, tracks.clone(), index, cx)
                    }
                    SearchEntry::Artist(artist) => artist_row(root, artist, cx),
                    SearchEntry::Album(album) => album_row(root, album, cx),
                    SearchEntry::Playlist(playlist) => search_playlist_row(root, playlist, cx),
                }))
        }))
        .into_any_element()
}

/// 搜索结果主视图：按当前 Tab 渲染。
fn search_results_view(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    match root.search_tab {
        SearchScope::All => search_groups_view(root, cx),
        SearchScope::Tracks => {
            if root.search_keyword.trim().is_empty() {
                empty_state("search", root.tr("输入关键词进行搜索"))
            } else if root.results.is_empty() {
                empty_state("search", root.tr("没有找到相关歌曲"))
            } else {
                track_list(
                    root,
                    root.results.clone(),
                    Nav::Search,
                    None,
                    root.liked_ids.clone(),
                    root.covers.clone(),
                    root.cover_requests.clone(),
                    cx,
                )
            }
        }
        SearchScope::Artists => div()
            .id("search-artists")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(px(theme::space::XS))
            .overflow_y_scroll()
            .when(root.search_keyword.trim().is_empty(), |this| {
                this.child(empty_state("search", root.tr("输入关键词进行搜索")))
            })
            .when(
                !root.search_keyword.trim().is_empty() && root.search_results.artists.is_empty(),
                |this| this.child(empty_state("search", root.tr("没有找到相关音乐人"))),
            )
            .children(
                root.search_results
                    .artists
                    .clone()
                    .iter()
                    .map(|artist| artist_row(root, artist, cx))
                    .collect::<Vec<_>>(),
            )
            .into_any_element(),
        SearchScope::Albums => div()
            .id("search-albums")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(px(theme::space::XS))
            .overflow_y_scroll()
            .when(root.search_keyword.trim().is_empty(), |this| {
                this.child(empty_state("search", root.tr("输入关键词进行搜索")))
            })
            .when(
                !root.search_keyword.trim().is_empty() && root.search_results.albums.is_empty(),
                |this| this.child(empty_state("search", root.tr("没有找到相关专辑"))),
            )
            .children(
                root.search_results
                    .albums
                    .clone()
                    .iter()
                    .map(|album| album_row(root, album, cx))
                    .collect::<Vec<_>>(),
            )
            .into_any_element(),
        SearchScope::Playlists => div()
            .id("search-playlists")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(px(theme::space::XS))
            .overflow_y_scroll()
            .when(root.search_keyword.trim().is_empty(), |this| {
                this.child(empty_state("search", root.tr("输入关键词进行搜索")))
            })
            .when(
                !root.search_keyword.trim().is_empty() && root.search_results.playlists.is_empty(),
                |this| this.child(empty_state("search", root.tr("没有找到相关歌单"))),
            )
            .children(
                root.search_results
                    .playlists
                    .clone()
                    .iter()
                    .map(|playlist| search_playlist_row(root, playlist, cx))
                    .collect::<Vec<_>>(),
            )
            .into_any_element(),
    }
}

/// 官方搜索结果页：搜索框 + Tab + 分组结果。
/// 搜索结果里的歌单详情；仍保持在搜索上下文中。
fn search_playlist_detail(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let Some((_, title, tracks)) = root.search_playlist.clone() else {
        return empty_state("search", root.tr("歌单已关闭")).into_any_element();
    };
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::space::MD))
        .child(back_button(cx))
        .child(scrolling_detail_page(
            root,
            title,
            if root.language.resolved().is_zh() {
                root.localized("{} 首", &[tracks.len().to_string()])
            } else {
                format!("{} tracks", tracks.len())
            },
            String::new(),
            "PLAYLIST",
            tracks,
            std::sync::Arc::new(Vec::new()),
            false,
            None,
            &root.search_playlist_detail_list,
            cx,
        ))
        .into_any_element()
}

pub(crate) fn search_page(root: &Root, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    if root.search_playlist.is_some() {
        return search_playlist_detail(root, cx);
    }
    let query = root.search_keyword.trim().to_string();
    let show_history = root.settings.search_history_is_enabled()
        && root.search_field.text().trim().is_empty()
        && root.search_focus.is_focused(window)
        && !root.searching
        && !root.settings.search_history.is_empty();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::space::MD))
        .child(search_box(root, window, cx))
        .when(show_history, |this| {
            this.child(search_history_panel(root, cx))
        })
        .child(if query.is_empty() {
            page_title(root.tr("搜索"))
        } else {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::space::SM))
                .text_size(px(22.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text())
                .child(root.tr("搜索"))
                .child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        .text_color(theme::accent())
                        .child(query),
                )
                .into_any_element()
        })
        .child(search_tabs(root, cx))
        .child(if root.searching {
            loading_state(root.tr("正在搜索…"))
        } else {
            search_results_view(root, cx)
        })
        .into_any_element()
}

/// 歌手页：大头像、热歌和专辑。
pub(crate) fn artist_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let Some(detail) = root.open_artist.clone() else {
        return empty_state("search", root.tr("还没有选择音乐人")).into_any_element();
    };
    let tracks = std::sync::Arc::new(detail.tracks.clone());
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::space::MD))
        .child(back_button(cx))
        .child(if detail.tracks.is_empty() {
            if root.loading_artist {
                loading_state(root.tr("正在读取音乐人热歌…")).into_any_element()
            } else {
                retry_state(
                    root.tr("暂时没有读到音乐人热歌"),
                    "artist-retry",
                    root.language,
                    cx,
                )
            }
        } else {
            scrolling_detail_page(
                root,
                detail.artist.name.clone(),
                root.localized(
                    "{}人关注 · {}首歌",
                    &[
                        compact_number(detail.artist.follower_count, root.language),
                        compact_number(
                            detail.artist.track_count.max(tracks.len() as i64),
                            root.language,
                        ),
                    ],
                ),
                detail.artist.avatar.clone(),
                "ARTIST",
                tracks,
                std::sync::Arc::new(detail.albums.clone()),
                detail.tracks_has_more,
                Some(cx.entity()),
                &root.artist_detail_list,
                cx,
            )
        })
        .into_any_element()
}

/// 专辑页：大封面、元数据和全碟歌曲。
pub(crate) fn album_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let Some(detail) = root.open_album.clone() else {
        return empty_state("search", root.tr("还没有选择专辑")).into_any_element();
    };
    let tracks = std::sync::Arc::new(detail.tracks.clone());
    let mut subtitle = root.localized(
        "{} 首",
        &[detail
            .album
            .track_count
            .max(tracks.len() as i64)
            .to_string()],
    );
    if !detail.album.artist.is_empty() {
        subtitle.push_str(&format!(" · {}", detail.album.artist));
    }
    let release = release_date_label(detail.album.release_date);
    if !release.is_empty() {
        subtitle.push_str(&format!(" · {release}"));
    }
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::space::MD))
        .child(back_button(cx))
        .child(if detail.tracks.is_empty() {
            if root.loading_album {
                loading_state(root.tr("正在读取专辑歌曲…")).into_any_element()
            } else {
                retry_state(
                    root.tr("暂时没有读到专辑歌曲"),
                    "album-retry",
                    root.language,
                    cx,
                )
            }
        } else {
            scrolling_detail_page(
                root,
                detail.album.title.clone(),
                subtitle,
                detail.album.cover.clone(),
                "ALBUM",
                tracks,
                std::sync::Arc::new(Vec::new()),
                false,
                None,
                &root.album_detail_list,
                cx,
            )
        })
        .into_any_element()
}

/// 歌手/专辑页的加载失败重试态。
fn retry_state(
    text: &str,
    scope: &'static str,
    language: crate::ui::i18n::Language,
    cx: &mut Context<Root>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(theme::space::MD))
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(text.to_string()),
        )
        .child(
            div()
                .id(gpui::ElementId::Name(scope.into()))
                .flex()
                .items_center()
                .justify_center()
                .h(px(theme::size::CONTROL_SM))
                .px(px(theme::space::MD))
                .rounded(px(theme::radius::PILL))
                .bg(theme::surface_elevated())
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .text_size(theme::Text::Tiny.size())
                .text_color(theme::text())
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    if scope == "artist-retry" {
                        root.retry_artist(cx);
                    } else {
                        root.retry_album(cx);
                    }
                }))
                .child(language.text("重试")),
        )
        .into_any_element()
}
