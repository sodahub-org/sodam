//! 搜索、音乐人和专辑详情相关动作。

use gpui::prelude::*;
use gpui::Context;
use sodam_core::library::SearchScope;
use sodam_core::models::{AlbumDetail, AlbumItem, ArtistDetail, ArtistItem, SearchResults};
use sodam_core::session::Session;
use std::collections::HashSet;
use std::sync::Arc;

use super::{Nav, Root};

impl Root {
    /// 触发搜索：网络调用放在 GPUI 的后台执行器上，结果回灌后 `notify` 重绘。
    pub fn run_search(&mut self, cx: &mut Context<Self>) {
        let keyword = self.search_input.trim().to_string();
        if keyword.is_empty() {
            self.set_status("先输入关键词再回车", &[]);
            cx.notify();
            return;
        }
        if self.searching {
            return;
        }
        let scope = self.search_tab;
        let settings = self.settings.clone();
        self.searching = true;
        self.search_keyword = keyword.clone();
        self.status = self.localized("正在搜索「{}」…", std::slice::from_ref(&keyword));
        cx.notify();

        let work_keyword = keyword.clone();
        let work = cx.background_spawn(async move {
            Session::new(settings).search_scope(&work_keyword, scope)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.searching = false;
                match result {
                    Ok(results) => {
                        root.status = root.localized(
                            "「{}」共 {} 条结果",
                            &[
                                results.keyword.to_string(),
                                results.total_count().to_string(),
                            ],
                        );
                        root.results = Arc::new(results.all_tracks());
                        root.search_results = results;
                        let covers: Vec<String> = root
                            .search_results
                            .tracks
                            .iter()
                            .map(|track| track.cover.clone())
                            .chain(
                                root.search_results
                                    .artists
                                    .iter()
                                    .map(|item| item.avatar.clone()),
                            )
                            .chain(
                                root.search_results
                                    .albums
                                    .iter()
                                    .map(|item| item.cover.clone()),
                            )
                            .chain(
                                root.search_results
                                    .playlists
                                    .iter()
                                    .map(|item| item.cover.clone()),
                            )
                            .collect();
                        root.ensure_covers(&covers, cx);
                    }
                    Err(err) => {
                        root.status = root.localized("搜索失败：{err}", &[err.to_string()]);
                        root.results = Arc::new(Vec::new());
                        root.search_results = SearchResults {
                            keyword,
                            ..Default::default()
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 切换搜索页 Tab；同一个关键词会按官方逻辑重新按范围搜索。
    pub fn set_search_tab(&mut self, tab: SearchScope, cx: &mut Context<Self>) {
        if self.search_tab == tab {
            return;
        }
        self.search_tab = tab;
        if self.search_input.trim().is_empty() {
            cx.notify();
        } else {
            self.run_search(cx);
        }
    }

    /// 搜索结果里的音乐人入口。
    pub fn open_artist_page(&mut self, artist: ArtistItem, cx: &mut Context<Self>) {
        self.open_album = None;
        self.open_artist = Some(ArtistDetail {
            artist,
            ..Default::default()
        });
        self.artist_detail_list.reset(1);
        self.loading_artist = true;
        self.set_nav(Nav::Artist, cx);
        cx.notify();

        let artist_id = self
            .open_artist
            .as_ref()
            .map(|detail| detail.artist.id.clone())
            .unwrap_or_default();
        let settings = self.settings.clone();
        let work =
            cx.background_spawn(async move { Session::new(settings).artist_detail(&artist_id) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_artist = false;
                match result {
                    Ok(detail) => {
                        root.artist_detail_list.reset(
                            detail.tracks.len()
                                + 1
                                + if detail.albums.is_empty() {
                                    0
                                } else {
                                    detail.albums.len() + 1
                                },
                        );
                        let covers: Vec<String> = std::iter::once(detail.artist.avatar.clone())
                            .chain(detail.tracks.iter().map(|track| track.cover.clone()))
                            .chain(detail.albums.iter().map(|album| album.cover.clone()))
                            .collect();
                        root.ensure_covers(&covers, cx);
                        root.open_artist = Some(detail);
                        root.set_status("音乐人已加载", &[]);
                    }
                    Err(err) => {
                        root.status = root.localized("读取音乐人失败：{err}", &[err.to_string()])
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 音乐人页加载失败后重试。
    pub fn retry_artist(&mut self, cx: &mut Context<Self>) {
        if let Some(artist) = self
            .open_artist
            .as_ref()
            .map(|detail| detail.artist.clone())
        {
            self.open_artist_page(artist, cx);
        }
    }

    /// 音乐人热歌加载更多。
    pub fn load_more_artist_tracks(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.open_artist.as_ref() else {
            return;
        };
        if self.loading_artist_tracks || !detail.tracks_has_more {
            return;
        }
        let artist_id = detail.artist.id.clone();
        let cursor = detail.tracks_cursor.clone();
        let settings = self.settings.clone();
        self.loading_artist_tracks = true;
        cx.notify();

        let work = cx.background_spawn(async move {
            Session::new(settings).artist_tracks(&artist_id, &cursor, 30)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_artist_tracks = false;
                match result {
                    Ok(page) => {
                        if let Some(detail) = root.open_artist.as_mut() {
                            let known: HashSet<String> =
                                detail.tracks.iter().map(|track| track.id.clone()).collect();
                            for track in page.tracks {
                                if known.contains(&track.id) {
                                    continue;
                                }
                                detail.tracks.push(track);
                            }
                            detail.tracks_cursor = page.next_cursor;
                            detail.tracks_has_more = page.has_more;
                        }
                        let count = root
                            .open_artist
                            .as_ref()
                            .map(|detail| {
                                detail.tracks.len()
                                    + 1
                                    + if detail.albums.is_empty() {
                                        0
                                    } else {
                                        detail.albums.len() + 1
                                    }
                            })
                            .unwrap_or(1);
                        root.artist_detail_list.reset(count);
                        let covers: Vec<String> = root
                            .open_artist
                            .as_ref()
                            .map(|detail| {
                                detail
                                    .tracks
                                    .iter()
                                    .rev()
                                    .take(30)
                                    .map(|track| track.cover.clone())
                                    .collect()
                            })
                            .unwrap_or_default();
                        root.ensure_covers(&covers, cx);
                        root.set_status("音乐人歌曲已加载", &[]);
                    }
                    Err(err) => {
                        root.status =
                            root.localized("加载更多音乐人歌曲失败：{err}", &[err.to_string()])
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 从任意曲目打开专辑页；列表/播放条/播放页共用。
    pub fn open_track_album(
        &mut self,
        track: sodam_core::models::TrackItem,
        cx: &mut Context<Self>,
    ) {
        if track.album_id.is_empty() {
            return;
        }
        self.open_album_page(
            AlbumItem {
                id: track.album_id,
                title: track.album,
                cover: track.cover,
                ..Default::default()
            },
            cx,
        );
    }

    /// 从任意曲目打开音乐人页；列表/播放条/播放页共用。
    pub fn open_track_artist(
        &mut self,
        track: sodam_core::models::TrackItem,
        cx: &mut Context<Self>,
    ) {
        if track.artist_id.is_empty() {
            return;
        }
        self.open_artist_page(
            ArtistItem {
                id: track.artist_id,
                name: track.artist,
                ..Default::default()
            },
            cx,
        );
    }

    /// 搜索结果里的专辑入口。
    pub fn open_album_page(&mut self, album: AlbumItem, cx: &mut Context<Self>) {
        // 保留 open_artist：从歌手页进入专辑后，返回时要能还原歌手详情。
        self.open_album = Some(AlbumDetail {
            album,
            tracks: Vec::new(),
        });
        self.album_detail_list.reset(1);
        self.loading_album = true;
        self.set_nav(Nav::Album, cx);
        cx.notify();

        let album_id = self
            .open_album
            .as_ref()
            .map(|detail| detail.album.id.clone())
            .unwrap_or_default();
        let settings = self.settings.clone();
        let work =
            cx.background_spawn(async move { Session::new(settings).album_detail(&album_id) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_album = false;
                match result {
                    Ok(detail) => {
                        let track_count = detail.tracks.len();
                        let covers: Vec<String> = std::iter::once(detail.album.cover.clone())
                            .chain(detail.tracks.iter().map(|track| track.cover.clone()))
                            .collect();
                        root.ensure_covers(&covers, cx);
                        root.open_album = Some(detail);
                        root.album_detail_list.reset(track_count + 1);
                        root.status = root.localized("专辑共 {} 首", &[track_count.to_string()]);
                    }
                    Err(err) => {
                        root.status = root.localized("读取专辑失败：{err}", &[err.to_string()])
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 专辑页加载失败后重试。
    pub fn retry_album(&mut self, cx: &mut Context<Self>) {
        if let Some(album) = self.open_album.as_ref().map(|detail| detail.album.clone()) {
            self.open_album_page(album, cx);
        }
    }
}
