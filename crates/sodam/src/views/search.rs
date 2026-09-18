//! 官方风格搜索结果页，以及音乐人 / 专辑详情页。

use super::*;
/// 搜索框：点击获取焦点，回车触发搜索，Esc 清空。
fn search_box(root: &Root, window: &Window, cx: &mut Context<Root>) -> impl IntoElement {
    let focused = root.search_focus.is_focused(window);
    let input_focus = root.search_focus.clone();
    let input_entity = cx.entity();
    let text = if root.search_input.is_empty() {
        root.tr("搜索歌曲 / 歌手（点这里后输入，回车搜索）")
            .to_string()
    } else {
        format!("{}{}", root.search_input, if focused { "▌" } else { "" })
    };
    let color = if root.search_input.is_empty() {
        theme::text_muted()
    } else {
        theme::text()
    };

    div()
        .id("search-input")
        .relative()
        .track_focus(&root.search_focus)
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
            cx.listener(|root, _event, window, cx| {
                window.focus(&root.search_focus, cx);
            }),
        )
        .on_key_down(cx.listener(|root, event: &KeyDownEvent, _window, cx| {
            let key = event.keystroke.key.as_str();
            match key {
                "enter" => root.run_search(cx),
                "backspace" => {
                    let _ = root.search_input.pop();
                    cx.notify();
                }
                "escape" => {
                    root.search_input.clear();
                    cx.notify();
                }
                // 可打印字符（含 Fcitx 上屏）交给 EntityInputHandler；
                // 这里手动追加会和 IME 通道重复插入。
                _ => {}
            }
        }))
        .child(
            canvas(
                |_bounds, _window, _cx| {},
                move |bounds, _state, window, cx| {
                    window.handle_input(
                        &input_focus,
                        gpui::ElementInputHandler::new(bounds, input_entity.clone()),
                        cx,
                    );
                },
            )
            .absolute()
            .size_full(),
        )
        .child(text)
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
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::space::MD))
        .child(search_box(root, window, cx))
        .child(if query.is_empty() {
            page_header(root.tr("搜索"), root.tr("回车搜索歌曲、音乐人、专辑或歌单"))
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
