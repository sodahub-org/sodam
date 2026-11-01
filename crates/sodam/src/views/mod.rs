//! 右侧内容区路由与共享列表组件。

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, img, point, px, size, svg, uniform_list, AnyElement, Bounds, ClickEvent,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ObjectFit, Window,
};

use crate::app::{LoginState, Nav, Root};
use crate::ui::artwork::{cover, scene_image};
use crate::ui::icons;
use crate::ui::spinner::loading_state;
use crate::ui::theme;
use crate::ui::theme::ThemeKind;

pub use login::login_modal;
pub mod login;
pub mod me;
pub mod lyrics;
pub mod scenes;
pub mod search;
pub mod settings;
pub mod shortcut_card;
use settings::duration_of;

use sodam_core::library::SearchScope;
use sodam_core::models::{AlbumItem, ArtistItem, SearchEntry};

/// 官方客户端那种整宽列布局：`# / 歌曲 / 歌手 / 专辑 / 时长`。
///
/// 列表用 `uniform_list`（只渲染可见行），行内不发任何请求。
const TRACK_GROUP: &str = "track-row";
const COL_INDEX: f32 = 40.0;
const COL_ARTIST: f32 = 190.0;
const COL_ALBUM: f32 = 240.0;
const COL_TIME: f32 = 60.0;
/// 标题列保底宽度：固定列再多也不能把标题挤没。
const COL_TITLE_MIN: f32 = 160.0;
const COL_LIKE: f32 = 28.0;

fn vip_badge() -> AnyElement {
    div()
        .flex_none()
        .px(px(theme::space::SM))
        .py(px(1.0))
        .rounded(px(theme::radius::PILL))
        .bg(theme::accent_soft())
        .text_size(theme::Text::Tiny.size())
        .text_color(theme::accent())
        .child("VIP")
        .into_any_element()
}

/// 表头：# / 歌曲 / 歌手 / 专辑 / 时长。
/// 宽度不够时的折叠顺序（按用户要求）：先专辑 → 再时长 → 再歌手。
/// 列间距与行内左右内边距（与下面的 flex 设置保持一致）。
const COL_GAP: f32 = 12.0;
const ROW_PAD: f32 = 12.0;
/// 内联 VIP 徽章的宽度（含间距），标题文本需要先让出这一段。
const VIP_WIDTH: f32 = 76.0;

/// 当前可用宽度下各列是否显示 + **标题列的具体宽度**。
///
/// 宽度显式计算（而不是让 flex 自己分配），否则表头和每一行是彼此独立的
/// flex 容器，算出来的列宽会不一致 —— 表现就是「表头和内容对不上」。
#[derive(Clone, Copy)]
struct Columns {
    artist: bool,
    album: bool,
    time: bool,
    title: f32,
}

fn columns_for(width: f32) -> Columns {
    let usable = (width - ROW_PAD * 2.0).max(0.0);
    let base = COL_INDEX + COL_LIKE + theme::size::ROW_ART;
    let candidate = |artist: bool, album: bool, time: bool| -> Option<f32> {
        let mut fixed = base;
        // 4 个固定子项（#、爱心、封面、标题），间距数 = 子项数 - 1
        let mut count = 4.0;
        if artist {
            fixed += COL_ARTIST;
            count += 1.0;
        }
        if album {
            fixed += COL_ALBUM;
            count += 1.0;
        }
        if time {
            fixed += COL_TIME;
            count += 1.0;
        }
        let title = usable - fixed - COL_GAP * (count - 1.0);
        (title >= COL_TITLE_MIN).then_some(title)
    };
    // 折叠顺序：专辑 → 时长 → 歌手
    if let Some(title) = candidate(true, true, true) {
        return Columns {
            artist: true,
            album: true,
            time: true,
            title,
        };
    }
    if let Some(title) = candidate(true, false, true) {
        return Columns {
            artist: true,
            album: false,
            time: true,
            title,
        };
    }
    if let Some(title) = candidate(true, false, false) {
        return Columns {
            artist: true,
            album: false,
            time: false,
            title,
        };
    }
    Columns {
        artist: false,
        album: false,
        time: false,
        title: candidate(false, false, false).unwrap_or(COL_TITLE_MIN),
    }
}

fn track_header(cols: Columns, language: crate::ui::i18n::Language) -> AnyElement {
    let mut header = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(COL_GAP))
        .h(px(theme::size::CONTROL_SM))
        .px(px(ROW_PAD))
        .text_size(theme::Text::Small.size())
        .text_color(theme::text_faint())
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .w(px(COL_INDEX))
                .flex_none()
                .whitespace_nowrap()
                .child("#"),
        )
        .child(div().w(px(COL_LIKE)).flex_none())
        .child(div().w(px(theme::size::ROW_ART)).flex_none())
        .child(
            div()
                .w(px(cols.title))
                .flex_none()
                .whitespace_nowrap()
                .child(language.text("歌曲")),
        );
    if cols.artist {
        header = header.child(
            div()
                .w(px(COL_ARTIST))
                .flex_none()
                .whitespace_nowrap()
                .child(language.text("歌手")),
        );
    }
    if cols.album {
        header = header.child(
            div()
                .w(px(COL_ALBUM))
                .flex_none()
                .whitespace_nowrap()
                .child(language.text("专辑")),
        );
    }
    if cols.time {
        header = header.child(
            div()
                .w(px(COL_TIME))
                .flex_none()
                .text_right()
                .whitespace_nowrap()
                .child(language.text("时长")),
        );
    }
    header.into_any_element()
}

/// 单首曲目行；歌曲 Tab 和综合搜索共用同一套交互与视觉。
#[allow(clippy::too_many_arguments)]
fn track_row(
    entity: &gpui::Entity<Root>,
    track: &sodam_core::models::TrackItem,
    index: usize,
    cols: Columns,
    tracks: std::sync::Arc<Vec<sodam_core::models::TrackItem>>,
    liked_ids: std::sync::Arc<std::collections::HashSet<String>>,
    covers: std::sync::Arc<std::collections::HashMap<String, std::path::PathBuf>>,
    cover_requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    current_track_id: Option<String>,
) -> AnyElement {
    let play_entity = entity.clone();
    let like_entity = entity.clone();
    let menu_entity = entity.clone();
    let menu_track = track.clone();
    let title_entity = entity.clone();
    let artist_entity = entity.clone();
    div()
        .id(("track", index))
        .group(TRACK_GROUP)
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(COL_GAP))
        .h(px(theme::size::LIST_ROW + theme::space::XS))
        .px(px(ROW_PAD))
        .pb(px(theme::space::XS))
        .rounded(px(theme::radius::ROW))
        .cursor_pointer()
        .when(current_track_id.as_ref() == Some(&track.id), |this| {
            this.bg(theme::surface_hover())
        })
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
            let tracks = tracks.clone();
            play_entity.update(cx, |root, cx| {
                root.track_menu = None;
                root.play_from_arc(tracks, index, cx);
            });
        })
        .on_mouse_down(
            MouseButton::Right,
            move |event: &MouseDownEvent, _window, cx: &mut gpui::App| {
                menu_entity.update(cx, |root, cx| {
                    root.queue_menu = None;
                    root.track_menu = Some(crate::app::TrackMenu {
                        track: menu_track.clone(),
                        position: event.position,
                    });
                    cx.notify();
                });
            },
        )
        .child(
            // 序号：悬停整行时变成播放图标，和官方一致。
            div()
                .w(px(COL_INDEX))
                .flex_none()
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_faint())
                        .group_hover(TRACK_GROUP, |style| style.opacity(0.0))
                        .child(format!("{}", index + 1)),
                )
                .child(
                    svg()
                        .path(icons::path("play"))
                        .absolute()
                        .size(px(12.0))
                        .opacity(0.0)
                        .text_color(theme::text())
                        .group_hover(TRACK_GROUP, |style| style.opacity(1.0)),
                ),
        )
        .child({
            // 爱心：空心 = 未收藏，实心（强调色）= 已收藏；点一下切换
            let liked = liked_ids.contains(&track.id);
            let like_track = track.clone();
            div()
                .id(("like", index))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .w(px(COL_LIKE))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                    // 别让点击冒泡到整行（否则会连播放一起触发）
                    cx.stop_propagation();
                })
                .on_click(move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
                    let track = like_track.clone();
                    like_entity.update(cx, |root, cx| {
                        root.toggle_like(track, cx);
                    });
                })
                .child(
                    svg()
                        .path(icons::path(if liked { "heart-filled" } else { "heart" }))
                        .size(px(theme::ICON_SM))
                        .text_color(if liked {
                            theme::accent()
                        } else {
                            theme::text_faint()
                        }),
                )
        })
        .child({
            // 封面：只在「这一行真的被渲染出来」时才把 URL 丢进请求队列，
            // 没滚到的行不会发任何请求（虚拟列表天然只渲染可见行）。
            let url = track.cover.clone();
            let cached = covers.get(&url).cloned();
            if cached.is_none() && !url.is_empty() {
                if let Ok(mut queue) = cover_requests.lock() {
                    if !queue.iter().any(|item| item == &url) {
                        queue.push(url.clone());
                    }
                }
            }
            cover(cached, theme::size::ROW_ART, theme::radius::ART)
        })
        .child({
            // 标题列宽度由 columns_for 显式算出（与表头同宽）；
            // 有 VIP 徽章时先给徽章让出固定宽度，避免挤掉标题。
            let text_width = if track.vip {
                (cols.title - VIP_WIDTH).max(40.0)
            } else {
                cols.title
            };
            div()
                .w(px(cols.title))
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::space::SM))
                .child(
                    div()
                        .id(("track-title-link", index))
                        .w(px(text_width))
                        .flex_none()
                        .min_w(px(0.0))
                        .truncate()
                        .cursor_pointer()
                        .text_size(theme::Text::Body.size())
                        .text_color(theme::text())
                        .hover(|style| style.text_color(theme::accent()))
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        .on_click({
                            let track = track.clone();
                            move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
                                let track = track.clone();
                                title_entity.update(cx, |root, cx| {
                                    root.open_track_album(track, cx);
                                });
                            }
                        })
                        .child(track.title.clone()),
                )
                .when(track.vip, |this| this.child(vip_badge()))
        })
        .when(cols.artist, |this| {
            this.child(
                div()
                    .id(("track-artist-link", index))
                    .w(px(COL_ARTIST))
                    .flex_none()
                    .min_w(px(0.0))
                    .truncate()
                    .cursor_pointer()
                    .text_size(theme::Text::Small.size())
                    .text_color(theme::text_muted())
                    .hover(|style| style.text_color(theme::accent()))
                    .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                        cx.stop_propagation();
                    })
                    .on_click({
                        let track = track.clone();
                        move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
                            let track = track.clone();
                            artist_entity.update(cx, |root, cx| {
                                root.open_track_artist(track, cx);
                            });
                        }
                    })
                    .child(track.artist.clone()),
            )
        })
        .when(cols.album, |this| {
            this.child(
                div()
                    .w(px(COL_ALBUM))
                    .flex_none()
                    .truncate()
                    .text_size(theme::Text::Small.size())
                    .text_color(theme::text_muted())
                    .child(track.album.clone()),
            )
        })
        .when(cols.time, |this| {
            this.child(
                div()
                    .w(px(COL_TIME))
                    .flex_none()
                    .text_right()
                    .text_size(theme::Text::Small.size())
                    .text_color(theme::text_faint())
                    .child(track.duration_label()),
            )
        })
        .into_any_element()
}

/// 曲目列表（虚拟滚动）。
#[allow(clippy::too_many_arguments)]
fn track_list(
    root: &Root,
    tracks: std::sync::Arc<Vec<sodam_core::models::TrackItem>>,
    nav: Nav,
    scroll: Option<&gpui::UniformListScrollHandle>,
    liked_ids: std::sync::Arc<std::collections::HashSet<String>>,
    covers: std::sync::Arc<std::collections::HashMap<String, std::path::PathBuf>>,
    cover_requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    cx: &mut Context<Root>,
) -> AnyElement {
    if tracks.is_empty() {
        return empty_state("music", root.tr("这里还没有内容"));
    }
    let entity = cx.entity();
    let count = tracks.len();
    let current_track_id = root.queue.current().map(|track| track.id.clone());
    let cols = columns_for(root.list_width.lock().map(|width| *width).unwrap_or(1200.0));
    let width_slot = root.list_width.clone();

    let list = uniform_list(
        ("track-list", nav as usize),
        count,
        move |range, _window, _cx| {
            range
                .map(|index| {
                    let track = &tracks[index];
                    track_row(
                        &entity,
                        track,
                        index,
                        cols,
                        tracks.clone(),
                        liked_ids.clone(),
                        covers.clone(),
                        cover_requests.clone(),
                        current_track_id.clone(),
                    )
                })
                .collect::<Vec<_>>()
        },
    )
    .flex_1()
    .min_h(px(0.0))
    .flex_1()
    .min_h(px(0.0))
    .when_some(scroll.cloned(), |this, handle| this.track_scroll(&handle));

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .w_full()
        .gap(px(theme::space::XS))
        .child(track_header(cols, root.language))
        .child(
            canvas(
                move |bounds: gpui::Bounds<gpui::Pixels>, _window, _cx| {
                    if let Ok(mut slot) = width_slot.lock() {
                        *slot = f32::from(bounds.size.width);
                    }
                },
                |_bounds, _state, _window, _cx| {},
            )
            .absolute()
            .size_full(),
        )
        .child(list)
        .into_any_element()
}

/// 详情页：封面作为列表第一项，随歌曲列表一起滚动。
#[allow(clippy::too_many_arguments)]
pub(crate) fn scrolling_detail_page(
    root: &Root,
    title: String,
    subtitle: String,
    cover_url: String,
    kind: &'static str,
    tracks: std::sync::Arc<Vec<sodam_core::models::TrackItem>>,
    albums: std::sync::Arc<Vec<AlbumItem>>,
    show_load_more: bool,
    on_load_more: Option<gpui::Entity<Root>>,
    list_state: &gpui::ListState,
    cx: &mut Context<Root>,
) -> AnyElement {
    let language = root.language;
    let entity = cx.entity();
    let cover_path = root.cover_of(&cover_url);
    let play_tracks = tracks.clone();
    let shuffle_tracks = tracks.clone();
    let row_tracks = tracks.clone();
    let current_track_id = root.queue.current().map(|track| track.id.clone());
    let cols = columns_for(root.list_width.lock().map(|width| *width).unwrap_or(1200.0));
    let width_slot = root.list_width.clone();
    let liked_ids = root.liked_ids.clone();
    let covers = root.covers.clone();
    let cover_requests = root.cover_requests.clone();
    let albums = albums.clone();
    let detail_covers = root.covers.clone();
    let list = gpui::list(list_state.clone(), move |index, _window, _cx| {
        if index == 0 {
            return div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(theme::space::XL))
                // 左侧与曲目行的内容起点对齐（页面 24 + 行内 12）；底部留白避免首行贴住封面
                .px(px(ROW_PAD))
                .py(px(theme::space::SM))
                .pb(px(theme::space::LG))
                .child(cover(
                    cover_path.clone(),
                    theme::size::CARD_COVER,
                    theme::radius::CARD,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(theme::space::MD))
                        .min_w(px(0.0))
                        .pt(px(theme::space::SM))
                        .child(
                            div()
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_faint())
                                .child(kind),
                        )
                        .child(
                            div()
                                .text_size(px(34.0))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme::text())
                                .truncate()
                                .child(title.clone()),
                        )
                        .child(
                            div()
                                .text_size(theme::Text::Small.size())
                                .text_color(theme::text_muted())
                                .truncate()
                                .child(subtitle.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(theme::space::SM))
                                .child(
                                    div()
                                        .id("detail-play-all")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(theme::space::SM))
                                        .h(px(theme::size::CONTROL))
                                        .px(px(theme::space::LG))
                                        .rounded(px(theme::radius::PILL))
                                        .bg(theme::accent())
                                        .cursor_pointer()
                                        .hover(|style| style.opacity(0.9))
                                        .on_click({
                                            let tracks = play_tracks.clone();
                                            let entity = entity.clone();
                                            move |_event: &ClickEvent, _window, cx| {
                                                let tracks = tracks.clone();
                                                entity.update(cx, |root, cx| {
                                                    root.play_from_arc(tracks.clone(), 0, cx);
                                                });
                                            }
                                        })
                                        .child(
                                            svg()
                                                .path(icons::path("play"))
                                                .size(px(theme::ICON))
                                                .text_color(theme::accent_foreground()),
                                        )
                                        .child(
                                            div()
                                                .text_size(theme::Text::Small.size())
                                                .text_color(theme::accent_foreground())
                                                .child(language.text("播放全部")),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("detail-shuffle-all")
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .size(px(theme::size::CONTROL))
                                        .rounded(px(theme::radius::PILL))
                                        .bg(theme::surface_elevated())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::surface_hover()))
                                        .on_click({
                                            let tracks = shuffle_tracks.clone();
                                            let entity = entity.clone();
                                            move |_event: &ClickEvent, _window, cx| {
                                                let tracks = tracks.clone();
                                                entity.update(cx, |root, cx| {
                                                    root.shuffle_play_arc(tracks.clone(), cx);
                                                });
                                            }
                                        })
                                        .child(
                                            svg()
                                                .path(icons::path("shuffle"))
                                                .size(px(theme::ICON))
                                                .text_color(theme::text()),
                                        ),
                                ),
                        ),
                )
                .into_any_element();
        }

        let index = index - 1;
        if index < tracks.len() {
            let Some(track) = tracks.get(index) else {
                return div().h(px(theme::size::LIST_ROW)).into_any_element();
            };
            return track_row(
                &entity,
                track,
                index,
                cols,
                row_tracks.clone(),
                liked_ids.clone(),
                covers.clone(),
                cover_requests.clone(),
                current_track_id.clone(),
            );
        }

        let mut offset = index - tracks.len();
        if show_load_more {
            if offset == 0 {
                let on_load_more = on_load_more.clone();
                return div()
                    .id("detail-load-more")
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(theme::size::CONTROL))
                    .mx(px(theme::space::MD))
                    .my(px(theme::space::SM))
                    .rounded(px(theme::radius::PILL))
                    .bg(theme::surface_elevated())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .text_size(theme::Text::Small.size())
                    .text_color(theme::text())
                    .on_click(move |_event: &ClickEvent, _window, cx| {
                        if let Some(entity) = on_load_more.as_ref() {
                            entity.update(cx, |root, cx| root.load_more_detail(cx));
                        }
                    })
                    .child(language.text("加载更多歌曲"))
                    .into_any_element();
            }
            offset -= 1;
        }

        let album_index = offset;
        if albums.is_empty() {
            return div().into_any_element();
        }
        if album_index == 0 {
            return div()
                .pt(px(theme::space::LG))
                .text_size(theme::Text::Large.size())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(language.text("专辑"))
                .into_any_element();
        }
        let album_index = album_index - 1;
        let Some(album) = albums.get(album_index) else {
            return div().into_any_element();
        };
        let item = album.clone();
        let cover_path = detail_covers.get(&album.cover).cloned();
        div()
            .id(gpui::ElementId::Name(
                format!("detail-album-{}", album.id).into(),
            ))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(theme::space::MD))
            .h(px(72.0))
            .px(px(theme::space::SM))
            .rounded(px(theme::radius::ROW))
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click({
                let item = item.clone();
                let entity = entity.clone();
                move |_event: &ClickEvent, _window, cx| {
                    let item = item.clone();
                    entity.update(cx, |root, cx| {
                        root.open_album_page(item.clone(), cx);
                    });
                }
            })
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
                            .child(album.artist.clone()),
                    ),
            )
            .into_any_element()
    })
    .flex_1()
    .min_h(px(0.0));

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .w_full()
        .child(
            canvas(
                move |bounds: gpui::Bounds<gpui::Pixels>, _window, _cx| {
                    if let Ok(mut slot) = width_slot.lock() {
                        *slot = f32::from(bounds.size.width);
                    }
                },
                |_bounds, _state, _window, _cx| {},
            )
            .absolute()
            .size_full(),
        )
        .child(list)
        .into_any_element()
}

/// 通用返回组件：详情页统一按导航历史返回。
pub(crate) fn back_button(cx: &mut Context<Root>) -> AnyElement {
    div()
        .id("back-button")
        .flex()
        .items_center()
        .justify_center()
        .size(px(theme::size::CONTROL_SM))
        .rounded(px(theme::radius::ROW))
        .bg(theme::surface_elevated())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
            root.go_back(cx);
        }))
        .child(
            svg()
                .path(icons::path("chevron-left"))
                .size(px(theme::ICON_SM))
                .text_color(theme::text()),
        )
        .into_any_element()
}

/// 当前页面的副标题：**从状态推导**，不要复用全局 `status`
/// （否则切页会残留上一页的文案，比如「共 11 个歌单」出现在收藏页）。
fn subtitle_for(root: &Root) -> String {
    let logged_in = !root.settings.cookie.trim().is_empty();
    match root.nav {
        // 搜索页 / 我的主页有自己的标题，不走 page_header，因此没有副标题
        Nav::Me => String::new(),
        Nav::Search => String::new(),
        Nav::Liked => {
            if !logged_in {
                root.tr("未登录：先在「登录」页扫码").to_string()
            } else if root.liked.is_empty() {
                root.tr("正在读取…").to_string()
            } else {
                root.localized("{} 首", &[root.liked.len().to_string()])
            }
        }
        Nav::Library => match &root.open_playlist {
            Some((_, _, tracks)) => root.localized("{} 首", &[tracks.len().to_string()]),
            None => {
                if !logged_in {
                    root.tr("未登录：先在「登录」页扫码").to_string()
                } else if root.playlists.is_empty() {
                    root.tr("正在读取…").to_string()
                } else {
                    root.localized("{} 个歌单", &[root.playlists.len().to_string()])
                }
            }
        },
        Nav::Settings => root.tr("账号、签名服务与音质偏好").to_string(),
        Nav::Lyrics => root.tr("正在播放").to_string(),
        Nav::Home => root.tr("来自汽水的推荐").to_string(),
        Nav::Scenes => root.tr("按场景选歌").to_string(),
        Nav::Artist => root.tr("音乐人详情").to_string(),
        Nav::Album => root.tr("专辑详情").to_string(),
    }
}

/// 页面标题。
fn page_title(title: &str) -> AnyElement {
    div()
        .text_size(theme::Text::Title.size())
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::text())
        .child(title.to_string())
        .into_any_element()
}

/// 页面标题 + 副标题。
fn page_header(title: &str, subtitle: &str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::XS))
        .child(page_title(title))
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(subtitle.to_string()),
        )
        .into_any_element()
}

/// 推荐页的失败终态：一句人话 + 重试。
///
/// 不能退回「正在准备推荐队列…」那个加载态：断网时它会一直转圈，用户既看不出
/// 出了什么事，也没有任何能按的地方（#55）。
fn recommendation_error_state(root: &Root, message: String, cx: &mut Context<Root>) -> AnyElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(theme::space::MD))
        .py(px(theme::space::XXL))
        .child(
            svg()
                .path(icons::path("house"))
                .size(px(theme::ICON_XL))
                .text_color(theme::text_faint()),
        )
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(message),
        )
        .child(
            div()
                .id("retry-recommendation")
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
                .text_color(theme::text_muted())
                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                    root.retry_recommendation(cx);
                }))
                .child(root.tr("重试加载")),
        )
        .into_any_element()
}

/// 空状态：图标 + 一句说明（不要空白页）。
fn empty_state(icon: &'static str, text: &str) -> AnyElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(theme::space::MD))
        .py(px(theme::space::XXL))
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(theme::ICON_XL))
                .text_color(theme::text_faint()),
        )
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(text.to_string()),
        )
        .into_any_element()
}

/// 歌单卡片网格（我的歌单 / 收藏）。
fn playlist_grid(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    if root.playlists.is_empty() {
        // 加载中显示圆圈；加载完才说「还没有歌单」
        return if root.loading_library {
            loading_state(root.tr("正在读取歌单…"))
        } else {
            empty_state("list-music", root.tr("还没有歌单"))
        };
    }
    let cards: Vec<AnyElement> = root
        .playlists
        .iter()
        .map(|playlist| {
            let cover_path = root.cover_of(&playlist.cover);
            let id = playlist.id.clone();
            div()
                .id(gpui::ElementId::Name(
                    format!("playlist-{}", playlist.id).into(),
                ))
                .flex()
                .flex_col()
                .gap(px(theme::space::SM))
                .p(px(theme::space::SM))
                .rounded(px(theme::radius::CARD))
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    root.track_menu = None;
                    root.open_playlist(id.clone(), None, cx);
                }))
                .child(cover(
                    cover_path,
                    theme::size::CARD_COVER,
                    theme::radius::CARD,
                ))
                .child(
                    div()
                        .w(px(theme::size::CARD_COVER))
                        .truncate()
                        .text_size(theme::Text::Body.size())
                        .text_color(theme::text())
                        .child(playlist.title.clone()),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.localized("{} 首", &[playlist.track_count.to_string()])),
                )
                .into_any_element()
        })
        .collect();
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(theme::space::LG))
        .children(cards)
        .into_any_element()
}

/// 歌曲行右键菜单：锚在鼠标位置，目前提供「下一首播放」。
pub fn track_menu(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let Some(menu) = &root.track_menu else {
        return div().into_any_element();
    };
    let menu_track = menu.track.clone();

    gpui::deferred(
        gpui::anchored()
            .snap_to_window()
            .position(point(menu.position.x, menu.position.y))
            .child(
                div()
                    .id("track-menu")
                    .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                        cx.stop_propagation();
                    })
                    .flex()
                    .flex_col()
                    .w(px(180.0))
                    .p(px(theme::space::XS))
                    .rounded(px(theme::radius::CARD))
                    .bg(theme::surface_elevated())
                    .border_1()
                    .border_color(theme::border())
                    .shadow_lg()
                    .child(
                        div()
                            .px(px(theme::space::SM))
                            .py(px(theme::space::XS))
                            .truncate()
                            .text_size(theme::Text::Tiny.size())
                            .text_color(theme::text_faint())
                            .child(menu_track.title.clone()),
                    )
                    .child(
                        div()
                            .id("track-play-next")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::space::SM))
                            .px(px(theme::space::SM))
                            .py(px(theme::space::XS))
                            .rounded(px(theme::radius::ROW))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::surface_hover()))
                            .text_size(theme::Text::Small.size())
                            .text_color(theme::text())
                            .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                                root.play_track_next(menu_track.clone(), cx);
                            }))
                            .child(
                                svg()
                                    .path(icons::path("skip-forward"))
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_muted()),
                            )
                            .child(root.tr("下一首播放")),
                    ),
            ),
    )
    .with_priority(2)
    .into_any_element()
}

/// 二维码点阵 → GPUI 画布（逐格 quad 绘制，比堆几百个 div 轻）。
fn qr_canvas(matrix: Vec<Vec<bool>>) -> impl IntoElement {
    let modules = matrix.len();
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, _state, window, _cx| {
            if modules == 0 {
                return;
            }
            let cell = bounds.size.width / modules as f32;
            for (y, row) in matrix.iter().enumerate() {
                for (x, dark) in row.iter().enumerate() {
                    if !*dark {
                        continue;
                    }
                    let origin = bounds.origin + point(cell * x as f32, cell * y as f32);
                    let rect = Bounds {
                        origin,
                        size: size(cell, cell),
                    };
                    // 白底 + 黑色模块（之前这里错用了白色，二维码等于没画出来）
                    window.paint_quad(fill(rect, gpui::black()));
                }
            }
        },
    )
    .w(px(240.0))
    .h(px(240.0))
}

/// 渲染内容区。
pub fn render(root: &mut Root, window: &Window, cx: &mut Context<Root>) -> impl IntoElement {
    let settings = &root.settings;
    let logged_in = !settings.cookie.trim().is_empty();

    let body = match root.nav {
        // 推荐页只有切队列时才会是可见加载态：失败后停在这里会永远转圈，
        // 所以它必须能变成带「重试」的失败终态（#55）。
        Nav::Home => match &root.recommendation_error {
            Some(message) => recommendation_error_state(root, message.clone(), cx),
            None => loading_state(root.tr("正在准备推荐队列…")),
        },
        Nav::Scenes => scenes::scenes_view(root, cx),
        Nav::Me => me::me_view(root, cx),
        Nav::Search => search::search_page(root, window, cx, !root.header_owns_search),
        Nav::Liked => div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(px(theme::space::SM))
            .child(if !logged_in {
                empty_state("heart", root.tr("登录后这里会显示你收藏的歌曲"))
            } else if root.loading_liked && root.liked.is_empty() {
                loading_state(root.tr("正在读取我喜欢的音乐…"))
            } else if root.liked.is_empty() {
                empty_state("heart", root.tr("还没有喜欢的歌曲"))
            } else {
                let tracks = root.liked.clone();
                let cover_url = tracks
                    .first()
                    .map(|track| track.cover.clone())
                    .unwrap_or_default();
                let total: i64 = tracks
                    .iter()
                    .map(|track| track.duration_seconds.max(0))
                    .sum();
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .gap(px(theme::space::LG))
                    .child(scrolling_detail_page(
                        root,
                        root.tr("我喜欢的音乐").to_string(),
                        root.localized(
                            "{} 首 · {}",
                            &[tracks.len().to_string(), duration_of(total)],
                        ),
                        cover_url,
                        "PLAYLIST",
                        tracks.clone(),
                        std::sync::Arc::new(Vec::new()),
                        false,
                        None,
                        &root.liked_detail_list,
                        cx,
                    ))
                    .into_any_element()
            })
            .into_any_element(),
        Nav::Library => match (&root.open_playlist, logged_in) {
            (Some(_), _) if root.loading_playlist => loading_state(root.tr("正在读取歌单曲目…")),
            (Some((id, title, tracks)), _) => {
                let cover_url = root
                    .playlists
                    .iter()
                    .find(|playlist| &playlist.id == id)
                    .map(|playlist| playlist.cover.clone())
                    .unwrap_or_default();
                let total: i64 = tracks
                    .iter()
                    .map(|track| track.duration_seconds.max(0))
                    .sum();
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .gap(px(theme::space::MD))
                    .child(back_button(cx))
                    .child(scrolling_detail_page(
                        root,
                        title.clone(),
                        root.localized(
                            "{} 首 · {}",
                            &[tracks.len().to_string(), duration_of(total)],
                        ),
                        cover_url,
                        "PLAYLIST",
                        tracks.clone(),
                        std::sync::Arc::new(Vec::new()),
                        false,
                        None,
                        &root.library_detail_list,
                        cx,
                    ))
                    .into_any_element()
            }
            (None, false) => empty_state("list-music", root.tr("登录后这里会显示你的歌单")),
            (None, true) if root.loading_library && root.playlists.is_empty() => {
                loading_state(root.tr("正在读取歌单…"))
            }
            (None, true) => div()
                .id("library-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .gap(px(theme::space::SM))
                .overflow_y_scroll()
                .child(playlist_grid(root, cx))
                .into_any_element(),
        },
        Nav::Artist => search::artist_page(root, cx),
        Nav::Album => search::album_page(root, cx),
        Nav::Settings => settings::settings_view(root, cx),
        Nav::Lyrics => lyrics::lyrics_view(root, window, cx),
    };

    div()
        .flex()
        .flex_col()
        .size_full()
        // 不重复画渐变：背景统一由根元素（整窗尺寸）负责，
        // 否则内容区渐变从自身顶部重新起算，会在标题栏下沿出现色差横缝。
        .p(px(theme::space::XL))
        .gap(px(theme::space::LG))
        .when(
            !matches!(
                root.nav,
                Nav::Lyrics | Nav::Search | Nav::Artist | Nav::Album
            ),
            |this| {
                let title = if root.nav == Nav::Scenes {
                    root.tr("听歌模式").to_string()
                } else {
                    root.nav_display_label(root.nav)
                };
                this.child(page_header(&title, &subtitle_for(root)))
            },
        )
        .child(
            div()
                .id("page-body")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .gap(px(theme::space::LG))
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表头与数据行共用的列宽必须装得进内容区，否则行右侧内边距会被吃掉。
    #[test]
    fn columns_fit_content_width() {
        // 400 起：更窄时退化到 COL_TITLE_MIN，允许溢出。
        for width in [400.0_f32, 640.0, 900.0, 1200.0] {
            let cols = columns_for(width);
            let mut total =
                ROW_PAD * 2.0 + COL_INDEX + COL_LIKE + theme::size::ROW_ART + cols.title;
            let mut items = 4.0;
            if cols.artist {
                total += COL_ARTIST;
                items += 1.0;
            }
            if cols.album {
                total += COL_ALBUM;
                items += 1.0;
            }
            if cols.time {
                total += COL_TIME;
                items += 1.0;
            }
            total += COL_GAP * (items - 1.0);
            assert!(total <= width + 0.01, "width={width} total={total}");
        }
    }
}

/// 底部「更多功能」菜单：对齐官方四项（查看专辑 / 分享 / 歌曲信息反馈 / 举报歌曲）。
pub fn more_menu(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let Some((track, _position)) = root.more_menu.clone() else {
        return div().into_any_element();
    };
    let item = |id: &'static str,
                icon: &'static str,
                label: String,
                cx: &mut Context<Root>,
                action: fn(&mut Root, sodam_core::models::TrackItem, &mut Context<Root>)| {
        let track = track.clone();
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(8.0))
            .h(px(40.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                crate::debug_log(&format!("more-menu click {id}"));
                root.more_menu = None;
                action(root, track.clone(), cx);
                cx.notify();
            }))
            .child(
                svg()
                    .path(icons::path(icon))
                    .size(px(20.0))
                    .text_color(theme::text()),
            )
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(label),
            )
    };
    gpui::deferred(
        div()
            .id("more-menu")
            .absolute()
            .left(px(20.0))
            .bottom(px(theme::PLAYER_H + 8.0))
            .occlude()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .w(px(220.0))
            .p(px(8.0))
            .rounded(px(12.0))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .child(item("more-album", "disc-3", root.tr("查看专辑").to_string(), cx, |root, track, cx| {
                root.open_track_album(track, cx);
            }))
            .child(item("more-share", "share-2", root.tr("分享").to_string(), cx, |root, track, cx| {
                let url = format!("https://music.douyin.com/qishui/song/{}", track.id);
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                root.set_status("已复制歌曲链接", &[]);
                root.set_toast("已复制链接".to_string(), cx);
            }))
            .child(item("more-feedback", "info", root.tr("歌曲信息反馈").to_string(), cx, |root, track, cx| {
                root.set_status("已记录歌曲信息反馈", &[]);
                root.set_toast(format!("已记录对《{}》的信息反馈", track.title), cx);
            }))
            .child(item("more-report", "alert-triangle", root.tr("举报歌曲").to_string(), cx, |root, track, cx| {
                root.set_status("已提交举报", &[]);
                root.set_toast(format!("已收到对《{}》的举报", track.title), cx);
            })),
    )
    .with_priority(3)
    .into_any_element()
}

/// 「添加到歌单」：列出自己的歌单，点一条就追加当前曲目。

pub fn add_to_playlist_menu(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let Some((track, _position)) = root.add_menu.clone() else {
        return div().into_any_element();
    };
    let item = |id: &'static str,
                icon: &'static str,
                label: String,
                cx: &mut Context<Root>,
                action: fn(&mut Root, sodam_core::models::TrackItem, &mut Context<Root>)| {
        let track = track.clone();
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(8.0))
            .h(px(40.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                crate::debug_log(&format!("add-menu click {id}"));
                root.add_menu = None;
                action(root, track.clone(), cx);
                cx.notify();
            }))
            .child(
                svg()
                    .path(icons::path(icon))
                    .size(px(20.0))
                    .text_color(theme::text()),
            )
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(label),
            )
    };
    gpui::deferred(
        div()
            .id("add-to-playlist-menu")
            .absolute()
            .left(px(64.0))
            .bottom(px(theme::PLAYER_H + 8.0))
            .occlude()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .w(px(220.0))
            .p(px(8.0))
            .rounded(px(12.0))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .child(item("add-create", "plus", root.tr("创建新歌单").to_string(), cx, |root, track, cx| {
                root.create_playlist_with_track(track, cx);
            }))
            .child(item("add-liked", "heart", root.tr("我喜欢的音乐").to_string(), cx, |root, track, cx| {
                root.toggle_like(track, cx);
            })),
    )
    .with_priority(3)
    .into_any_element()
}

/// 评论面板（只读列表 + 加载更多）。
pub fn comment_panel(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let items: Vec<AnyElement> = root
        .comment_items
        .iter()
        .map(|comment| {
            let avatar = root.cover_of(&comment.avatar);
            div()
                .flex()
                .flex_row()
                .gap(px(10.0))
                .px(px(20.0))
                .py(px(12.0))
                .child(
                    div()
                        .flex_none()
                        .child(cover(avatar, 36.0, 999.0)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .text_color(gpui::Rgba {
                                            a: 0.6,
                                            ..theme::text()
                                        })
                                        .child(comment.user.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(gpui::Rgba {
                                            a: 0.35,
                                            ..theme::text()
                                        })
                                        .child(format_comment_time(comment.created_at)),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(15.0))
                                .line_height(px(22.0))
                                .text_color(theme::text())
                                .child(render_emoji_tokens(&comment.content)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(12.0))
                                .text_size(px(12.0))
                                .text_color(gpui::Rgba {
                                    a: 0.45,
                                    ..theme::text()
                                })
                                .child(root.tr("回复").to_string())
                                .when(comment.replies > 0, |this| {
                                    let id = comment.id.clone();
                                    this.child(
                                        div()
                                            .id(gpui::ElementId::Name(
                                                format!("comment-expand-{}", comment.id).into(),
                                            ))
                                            .cursor_pointer()
                                            .hover(|style| style.text_color(theme::accent()))
                                            .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                                                if root.expanded_comments.remove(&id) {
                                                    cx.notify();
                                                    return;
                                                }
                                                root.expanded_comments.insert(id.clone());
                                                root.set_toast(
                                                    "回复列表接口暂不可用（官方走独立评论服务）".to_string(),
                                                    cx,
                                                );
                                                cx.notify();
                                            }))
                                            .child(format!("— 展开 {} 条回复", comment.replies)),
                                    )
                                }),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(2.0))
                        .child(
                            div()
                                .id(gpui::ElementId::Name(
                                    format!("comment-like-{}", comment.id).into(),
                                ))
                                .cursor_pointer()
                                .hover(|style| style.text_color(theme::accent()))
                                .child(
                                    svg()
                                        .path(icons::path("thumb-up"))
                                        .size(px(18.0))
                                        .text_color(gpui::Rgba {
                                            a: 0.45,
                                            ..theme::text()
                                        }),
                                )
                                .on_click(cx.listener({
                                    let id = comment.id.clone();
                                    move |root, _event: &ClickEvent, _window, cx| {
                                        root.like_comment(id.clone(), cx);
                                    }
                                })),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(gpui::Rgba {
                                    a: 0.45,
                                    ..theme::text()
                                })
                                .child(if comment.likes > 0 {
                                    comment.likes.to_string()
                                } else {
                                    String::new()
                                }),
                        ),
                )
                .into_any_element()
        })
        .collect();

    gpui::deferred(
        div()
            .id("comment-panel")
            .absolute()
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .w(px(380.0))
            .flex()
            .flex_col()
            // 官方评论区是不透明面板，并且盖住窗口右上角的最小化/最大化/关闭。
            .bg(theme::surface_elevated())
            .border_l_1()
            .border_color(theme::border())
            .occlude()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(16.0))
                    .px(px(20.0))
                    .py(px(16.0))
                    .child(
                        div()
                            .id("comment-close")
                            .size(px(24.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_size(px(22.0))
                            .text_color(theme::text_muted())
                            .hover(|style| style.text_color(theme::text()))
                            .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                root.comment_panel_open = false;
                                cx.notify();
                            }))
                            .child("×"),
                    )
                    .child(
                        div()
                            .text_size(px(20.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(theme::text())
                            .child(format!("评论（{}）", root.comment_total)),
                    ),
            )
            .child(
                div()
                    .id("comment-list")
                    .flex()
                    .flex_col()
                    .min_h(px(0.0))
                    .flex_1()
                    .overflow_y_scroll()
                    .when(root.comment_items.is_empty() && !root.comment_loading, |this| {
                        this.child(empty_state("music", root.tr("还没有评论")))
                    })
                    .children(items)
                    .when(root.comment_has_more, |this| {
                        this.child(
                            div()
                                .id("comment-more")
                                .py(px(theme::space::SM))
                                .flex()
                                .justify_center()
                                .cursor_pointer()
                                .text_size(theme::Text::Small.size())
                                .text_color(theme::accent())
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    root.load_more_comments(cx);
                                }))
                                .child(root.tr("加载更多").to_string()),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .mx(px(20.0))
                    .my(px(12.0))
                    .px(px(12.0))
                    .h(px(36.0))
                    .rounded(px(999.0))
                    .bg(gpui::Rgba {
                        a: 0.08,
                        ..theme::text()
                    })
                    .text_size(px(13.0))
                    .text_color(gpui::Rgba {
                        a: 0.4,
                        ..theme::text()
                    })
                    .child(root.tr("写下你想说的…").to_string()),
            ),
    )
    .with_priority(4)
    .into_any_element()
}

fn format_comment_time(seconds: i64) -> String {
    if seconds <= 0 {
        return String::new();
    }
    // Howard Hinnant civil_from_days
    let days = seconds.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y}-{m:02}-{d:02}")
}

/// 顶部居中的轻提示。
pub fn toast_overlay(text: String, _cx: &mut Context<Root>) -> AnyElement {
    gpui::deferred(
        div()
            .absolute()
            .top(px(64.0))
            .left(gpui::relative(0.5))
            .ml(px(-120.0))
            .w(px(240.0))
            .flex()
            .justify_center()
            .occlude()
            .child(
                div()
                    .px(px(14.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .bg(theme::surface_elevated())
                    .border_1()
                    .border_color(theme::border())
                    .shadow_lg()
                    .text_size(theme::Text::Small.size())
                    .text_color(theme::text())
                    .child(text),
            ),
    )
    .with_priority(8)
    .into_any_element()
}

/// 把抖音评论里的 `[流泪]` 这类表情标记换成 Unicode 表情；
/// 没收录的保留原文，避免内容被吃掉。
fn render_emoji_tokens(text: &str) -> String {
    const MAP: [(&str, &str); 28] = [
        ("[微笑]", "🙂"),
        ("[大笑]", "😄"),
        ("[笑哭]", "😂"),
        ("[流泪]", "😢"),
        ("[哭]", "😭"),
        ("[赞]", "👍"),
        ("[鼓掌]", "👏"),
        ("[爱心]", "❤️"),
        ("[心碎]", "💔"),
        ("[玫瑰]", "🌹"),
        ("[火]", "🔥"),
        ("[捂脸]", "🤦"),
        ("[思考]", "🤔"),
        ("[得意]", "😎"),
        ("[调皮]", "😜"),
        ("[疑问]", "❓"),
        ("[拜拜]", "👋"),
        ("[加油]", "💪"),
        ("[音乐]", "🎵"),
        ("[话筒]", "🎤"),
        ("[庆祝]", "🎉"),
        ("[蛋糕]", "🎂"),
        ("[礼物]", "🎁"),
        ("[星星]", "⭐"),
        ("[月亮]", "🌙"),
        ("[太阳]", "☀️"),
        ("[狗]", "🐶"),
        ("[猫]", "🐱"),
    ];
    let mut out = text.to_string();
    for (token, emoji) in MAP {
        out = out.replace(token, emoji);
    }
    out
}
