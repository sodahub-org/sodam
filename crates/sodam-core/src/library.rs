//! 音乐库数据：听歌模式、我喜欢的音乐、我的歌单、我收藏的。
//!
//! 全部是**同步**调用（libresoda 基于 ureq）；UI 侧要放到后台线程执行，
//! 别阻塞 GPUI 的渲染循环。

use std::collections::HashSet;

use crate::models::{
    AlbumDetail, AlbumItem, ArtistDetail, ArtistItem, ArtistTracksPage, PlaylistItem, SceneItem,
    SearchEntry, SearchGroup, SearchResults, TrackItem,
};
use crate::session::Session;
use libresoda::soda::track::build_song_from_track;
use libresoda::soda::types::build_image_url;
use libresoda::soda::types::{Album, Artist, Track, UserPlaylistItem};

fn wrap_err(context: &str, err: libresoda::SodaError) -> anyhow::Error {
    anyhow::anyhow!("{context}: {err}")
}

/// 听歌模式（场景模式）列表。
pub fn feed_scenes(session: &Session) -> anyhow::Result<Vec<SceneItem>> {
    let mode = session
        .soda()
        .feed_mode()
        .map_err(|err| wrap_err("读取听歌模式失败", err))?;
    Ok(mode
        .scenes()
        .into_iter()
        .map(|scene| SceneItem {
            text: scene.text,
            entry_type: scene.entry_type,
            scene_mode_id: scene.scene_mode_id,
            sub_queue_type: scene.sub_queue_type,
            cover: scene.cover_url,
        })
        .collect())
}

/// 推荐队列分页状态，语义对齐官方 `queueLoader.pagination`。
#[derive(Debug, Clone, PartialEq)]
pub struct RecommendationSource {
    /// `None` 是首页推荐；`Some` 是听歌模式。
    pub scene: Option<SceneItem>,
    /// 官方在每次发请求前 +1；首次请求为 1。
    pub fetch_counter: u32,
    pub has_more: bool,
    /// 官方会持久化“首次使用时间”，这里随推荐状态保存。
    pub did_first_use_time: u64,
}

/// 首次推荐队列。
#[derive(Debug, Clone, PartialEq)]
pub struct RecommendedStart {
    pub source: RecommendationSource,
    pub tracks: Vec<TrackItem>,
}

/// 追加的一批推荐曲目。
#[derive(Debug, Clone, PartialEq)]
pub struct RecommendedBatch {
    pub source: RecommendationSource,
    pub tracks: Vec<TrackItem>,
}

impl RecommendationSource {
    pub fn new() -> Self {
        let did_first_use_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or_default();
        Self {
            scene: None,
            fetch_counter: 0,
            has_more: true,
            did_first_use_time,
        }
    }
}

impl Default for RecommendationSource {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析官方 `FeedResponse.items[]` 中的音频曲目。
///
/// 官方 track 字段比通用 `Song` 模型复杂得多（album/artists 是结构体），
/// 这里只提取播放队列需要的字段，单条坏数据不会拖垮整页推荐。
fn recommended_tracks(value: &serde_json::Value) -> anyhow::Result<Vec<TrackItem>> {
    let items = value
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("推荐响应缺少 items"))?;
    let mut tracks = Vec::new();
    for item in items {
        if item.get("type").and_then(serde_json::Value::as_str) != Some("track") {
            continue;
        }
        let Some(track) = item.pointer("/entity/track_wrapper/track") else {
            continue;
        };
        let id = track
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let title = track
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("未知曲目")
            .to_string();
        let artist = track
            .get("artists")
            .and_then(serde_json::Value::as_array)
            .map(|artists| {
                artists
                    .iter()
                    .filter_map(|artist| artist.get("name").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default();
        let artist_id = track
            .pointer("/artists/0/id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let album = track
            .pointer("/album/name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let album_id = track
            .pointer("/album/id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let duration_seconds = (track
            .get("duration")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or_default()
            / 1000.0)
            .round() as i64;
        let vip = track
            .pointer("/label_info/only_vip_playable")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let cover = track
            .pointer("/album/url_cover")
            .and_then(|image| {
                let urls = image.get("urls").and_then(serde_json::Value::as_array)?;
                let _prefix = urls.first()?.as_str()?;
                let uri = image.get("uri").and_then(serde_json::Value::as_str)?;
                let template = image
                    .get("template_prefix")
                    .and_then(serde_json::Value::as_str)?;
                Some(format!(
                    "https://p3-luna.douyinpic.com/img/{uri}~{template}-resize:512:512.png"
                ))
            })
            .unwrap_or_default();
        tracks.push(TrackItem {
            id,
            title,
            artist,
            album,
            artist_id,
            album_id,
            cover,
            duration_seconds,
            vip,
        });
    }
    Ok(tracks)
}

/// 请求一页官方推荐队列。
fn recommended_page(
    session: &Session,
    source: &RecommendationSource,
) -> anyhow::Result<(RecommendationSource, Vec<TrackItem>)> {
    let fetch_counter = source.fetch_counter.saturating_add(1);
    let mut body = serde_json::json!({
        "played_media": [],
        "did_first_use_time": source.did_first_use_time,
        "is_first_request": source.fetch_counter == 0,
        "is_did_first_request": source.fetch_counter == 0,
        "feed_counts": { "mix_session_count": fetch_counter },
    });
    if let Some(scene) = &source.scene {
        if scene.scene_mode_id >= 0 {
            body["feed_preference"] = serde_json::json!({ "scene_mode_id": scene.scene_mode_id });
        } else {
            body["feed_preference"] =
                serde_json::json!({ "preference_mode": scene.sub_queue_type });
        }
    }
    let value = session
        .soda()
        .fetch_feed_song_tab(&body)
        .map_err(|err| wrap_err("读取推荐队列失败", err))?;
    let tracks = recommended_tracks(&value)?;
    let source = RecommendationSource {
        scene: source.scene.clone(),
        fetch_counter,
        has_more: value
            .get("has_more")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        did_first_use_time: source.did_first_use_time,
    };
    Ok((source, tracks))
}

/// 首次加载官方推荐队列。
pub fn recommended_start(session: &Session) -> anyhow::Result<RecommendedStart> {
    let (source, tracks) = recommended_page(session, &RecommendationSource::new())?;
    anyhow::ensure!(!tracks.is_empty(), "推荐队列为空");
    Ok(RecommendedStart { source, tracks })
}

/// 听歌模式专用推荐队列，等价官方 `FeedModeQueueItem`。
pub fn recommended_scene_start(
    session: &Session,
    scene: SceneItem,
) -> anyhow::Result<RecommendedStart> {
    let mut source = RecommendationSource::new();
    source.scene = Some(scene);
    let (source, tracks) = recommended_page(session, &source)?;
    anyhow::ensure!(!tracks.is_empty(), "该听歌模式队列为空");
    Ok(RecommendedStart { source, tracks })
}

/// 官方听歌模式页常用模式：本地“熟悉/新鲜” + feed_mode 第一组前 6 项。
pub fn scene_modes(session: &Session) -> anyhow::Result<Vec<SceneItem>> {
    let mode = session
        .soda()
        .feed_mode()
        .map_err(|err| wrap_err("读取听歌模式失败", err))?;
    let mut local = vec![
        SceneItem {
            text: "熟悉模式".to_string(),
            entry_type: "scene_mode".to_string(),
            scene_mode_id: -1,
            sub_queue_type: "familiar".to_string(),
            cover: String::new(),
        },
        SceneItem {
            text: "新鲜模式".to_string(),
            entry_type: "scene_mode".to_string(),
            scene_mode_id: -1,
            sub_queue_type: "fresh".to_string(),
            cover: String::new(),
        },
    ];
    let server = mode
        .feed_mode_block
        .iter()
        .flat_map(|block| block.feed_mode.iter())
        .collect::<Vec<_>>();
    for item in server {
        local.push(SceneItem {
            text: item.text.clone(),
            entry_type: item.entry_type.clone(),
            scene_mode_id: item.entity.feed_scene_mode.scene_mode_id,
            sub_queue_type: item.entity.feed_scene_mode.sub_queue_type.clone(),
            cover: {
                let image = &item.url_info;
                let prefix = image.urls.first().cloned().unwrap_or_default();
                let uri = image.uri.trim();
                let template = image.template_prefix.trim();
                if !uri.is_empty() && !template.is_empty() {
                    format!("https://p3-luna.douyinpic.com/img/{uri}~{template}-resize:128:128.png")
                } else {
                    prefix + uri
                }
            },
        });
    }
    Ok(local)
}

/// 听歌模式探索卡：歌单首屏和翻页批次的统一形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneCardKind {
    Playlist,
    Radio,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RadioSource {
    pub radio_id: String,
    pub flow_type: i64,
    pub trigger_info: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneCard {
    pub kind: SceneCardKind,
    pub inner_block_id: String,
    pub playlist: PlaylistItem,
    pub radio: Option<RadioSource>,
}

fn image_url(image: &serde_json::Value) -> String {
    let urls = image
        .get("urls")
        .and_then(serde_json::Value::as_array)
        .and_then(|urls| urls.first())
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let uri = image
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let template = image
        .get("template_prefix")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if uri.trim().is_empty() || template.trim().is_empty() {
        format!("{urls}{uri}")
    } else {
        format!("https://p3-luna.douyinpic.com/img/{uri}~{template}-resize:512:512.png")
    }
}

fn radio_source(resource: &serde_json::Value) -> Option<RadioSource> {
    let style = resource.pointer("/style")?;
    let radio_id = resource
        .get("resource_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            style
                .get("link")
                .and_then(|link| link.as_str())
                .and_then(|link| {
                    link.split("queue_id=")
                        .nth(1)
                        .and_then(|rest| rest.split('&').next())
                })
        })?
        .to_string();
    let link = style
        .get("link")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let query = link
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let mut flow_type = 2;
    let mut trigger_info = String::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "flow_type" => flow_type = value.parse().unwrap_or(2),
            "trigger_info" => trigger_info = value.to_string(),
            _ => {}
        }
    }
    Some(RadioSource {
        radio_id,
        flow_type,
        trigger_info,
    })
}

fn scene_card(block: &serde_json::Value) -> Option<SceneCard> {
    let inner_block_id = block
        .get("inner_block_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if inner_block_id.trim().is_empty() {
        return None;
    }

    let playlist_value = block
        .pointer("/entity/playlist")
        .or_else(|| block.pointer("/resources/0/entity/playlist"));
    if let Some(playlist) = playlist_value {
        let id = playlist
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if id.trim().is_empty() {
            return None;
        }
        let title = playlist
            .get("public_title")
            .or_else(|| playlist.get("title"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("歌单");
        return Some(SceneCard {
            kind: SceneCardKind::Playlist,
            playlist: PlaylistItem {
                id: id.to_string(),
                title: title.to_string(),
                cover: playlist
                    .pointer("/url_cover")
                    .map(image_url)
                    .unwrap_or_default(),
                track_count: playlist
                    .get("count_tracks")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default(),
                creator: String::new(),
            },
            radio: None,
            inner_block_id,
        });
    }

    let resource = block.pointer("/resources/0")?;
    if resource.get("type").and_then(serde_json::Value::as_str) != Some("radio") {
        return None;
    }
    let radio = radio_source(resource)?;
    let style = resource.pointer("/style")?;
    let title = style
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("电台");
    let cover = style
        .pointer("/cover_url_list/0")
        .map(image_url)
        .unwrap_or_default();
    Some(SceneCard {
        kind: SceneCardKind::Radio,
        playlist: PlaylistItem {
            id: radio.radio_id.clone(),
            title: title.to_string(),
            cover,
            track_count: 0,
            creator: String::new(),
        },
        radio: Some(radio),
        inner_block_id,
    })
}

/// 首屏探索卡：官方 `/luna/pc/discover` 中 `discover_feed_playlist` block。
pub fn scene_discover_cards(session: &Session) -> anyhow::Result<Vec<SceneCard>> {
    let value = session
        .soda()
        .fetch_discover()
        .map_err(|err| wrap_err("读取探索模式失败", err))?;
    let mut cards = Vec::new();
    let mut seen = HashSet::new();
    for block in value
        .get("blocks")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if block.get("type").and_then(serde_json::Value::as_str) != Some("discover_feed_playlist") {
            continue;
        }
        for item in block
            .get("inner_block")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(card) = scene_card(item) {
                if seen.insert(card.inner_block_id.clone()) {
                    cards.push(card);
                }
            }
        }
    }
    Ok(cards)
}

/// 追加探索卡：官方 `/luna/pc/discover/mix` 的 `discover_feed_playlist`。
pub fn scene_discover_cards_more(
    session: &Session,
    exposure_ids: &[u64],
) -> anyhow::Result<Vec<SceneCard>> {
    let body = serde_json::json!({
        "block_type": "discover_playlist_mix",
        "sub_channel_id": -2,
        "exposure_radio_list": exposure_ids,
    });
    let value = session
        .soda()
        .fetch_discover_mix_body(&body)
        .map_err(|err| wrap_err("读取更多探索模式失败", err))?;
    let mut cards = Vec::new();
    let mut seen = HashSet::new();
    for item in value
        .get("inner_block")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(card) = scene_card(item) {
            if seen.insert(card.inner_block_id.clone()) {
                cards.push(card);
            }
        }
    }
    Ok(cards)
}

/// 电台探索卡首队：官方 `FeedRadioTracks`。
pub fn radio_tracks(session: &Session, radio: &RadioSource) -> anyhow::Result<Vec<TrackItem>> {
    let body = serde_json::json!({
        "radio_id": radio.radio_id,
        "cursor": "",
        "flow_type": radio.flow_type,
        "trigger_info": radio.trigger_info,
    });
    let value = session
        .soda()
        .fetch_feed_radio_tracks_body(&body)
        .map_err(|err| wrap_err("读取电台队列失败", err))?;
    let tracks = recommended_tracks(&value)?;
    anyhow::ensure!(!tracks.is_empty(), "电台队列为空");
    Ok(tracks)
}

/// 官方 `autoLoadMore`：当前歌后方少于 6 首时追加。
pub fn recommended_append(
    session: &Session,
    source: &RecommendationSource,
) -> anyhow::Result<RecommendedBatch> {
    let (source, tracks) = recommended_page(session, source)?;
    anyhow::ensure!(!tracks.is_empty(), "推荐队列没有追加到新曲目");
    Ok(RecommendedBatch { source, tracks })
}

/// 搜索范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    All,
    Tracks,
    Artists,
    Albums,
    Playlists,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawSearchEntity {
    #[serde(default)]
    track: Option<Track>,
    #[serde(default)]
    artist: Option<Artist>,
    #[serde(default)]
    album: Option<Album>,
    #[serde(default)]
    playlist: Option<UserPlaylistItem>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawSearchItem {
    #[serde(default)]
    entity: RawSearchEntity,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawSearchGroup {
    #[serde(default)]
    id: String,
    #[serde(default)]
    display_title: String,
    #[serde(default)]
    display_view_all: bool,
    #[serde(default)]
    data: Vec<RawSearchItem>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawSearchResponse {
    #[serde(default)]
    result_groups: Vec<RawSearchGroup>,
}

fn artist_item(artist: &Artist) -> ArtistItem {
    ArtistItem {
        id: artist.id.clone(),
        name: artist.name.clone(),
        avatar: build_image_url(&artist.url_avatar, "~c5_300x300.jpg"),
        track_count: artist.count_tracks,
        follower_count: artist.stats.count_collected,
    }
}

fn album_item(album: &Album) -> AlbumItem {
    AlbumItem {
        id: album.id.clone(),
        title: album.name.clone(),
        cover: build_image_url(&album.url_cover, "~c5_300x300.jpg"),
        artist: libresoda::soda::types::join_track_artists(&album.artists),
        track_count: album.count_tracks,
        release_date: album.release_date,
        company: album.company.clone(),
    }
}

fn playlist_item(playlist: &UserPlaylistItem) -> PlaylistItem {
    PlaylistItem {
        id: playlist.id.clone(),
        title: if playlist.public_title.is_empty() {
            playlist.title.clone()
        } else {
            playlist.public_title.clone()
        },
        cover: build_image_url(&playlist.url_cover, "~c5_300x300.jpg"),
        track_count: playlist.count_tracks,
        creator: if playlist.owner.public_name.is_empty() {
            playlist.owner.nickname.clone()
        } else {
            playlist.owner.public_name.clone()
        },
    }
}

fn raw_search_results(keyword: &str, body: &[u8]) -> anyhow::Result<SearchResults> {
    let response: RawSearchResponse = serde_json::from_slice(body)
        .map_err(|err| anyhow::anyhow!("解析综合搜索结果失败: {err}"))?;
    let mut results = SearchResults {
        keyword: keyword.to_string(),
        ..Default::default()
    };

    for group in response.result_groups {
        let mut entries = Vec::new();
        for item in group.data {
            let entity = item.entity;
            if let Some(track) = entity.track.filter(|track| !track.id.is_empty()) {
                let track = TrackItem::from(build_song_from_track(&track));
                if !results.tracks.iter().any(|old| old.id == track.id) {
                    results.tracks.push(track.clone());
                }
                entries.push(SearchEntry::Track(track));
            }
            if let Some(artist) = entity.artist.filter(|artist| !artist.id.is_empty()) {
                let artist = artist_item(&artist);
                if !results.artists.iter().any(|old| old.id == artist.id) {
                    results.artists.push(artist.clone());
                }
                entries.push(SearchEntry::Artist(artist));
            }
            if let Some(album) = entity.album.filter(|album| !album.id.is_empty()) {
                let album = album_item(&album);
                if !results.albums.iter().any(|old| old.id == album.id) {
                    results.albums.push(album.clone());
                }
                entries.push(SearchEntry::Album(album));
            }
            if let Some(playlist) = entity.playlist.filter(|playlist| !playlist.id.is_empty()) {
                let playlist = playlist_item(&playlist);
                if !results.playlists.iter().any(|old| old.id == playlist.id) {
                    results.playlists.push(playlist.clone());
                }
                entries.push(SearchEntry::Playlist(playlist));
            }
        }
        if entries.is_empty() {
            continue;
        }
        let title = if group.display_title.is_empty() {
            match group.id.as_str() {
                "top_results" => "你可能想搜".to_string(),
                "tracks" => "歌曲".to_string(),
                "artists" => "音乐人".to_string(),
                "albums" => "专辑".to_string(),
                "playlists" => "歌单".to_string(),
                _ => String::new(),
            }
        } else {
            group.display_title
        };
        results.groups.push(SearchGroup {
            id: group.id,
            title,
            can_view_all: group.display_view_all,
            entries,
        });
    }

    Ok(results)
}

fn scoped_results(
    keyword: String,
    tracks: Vec<TrackItem>,
    artists: Vec<ArtistItem>,
    albums: Vec<AlbumItem>,
    playlists: Vec<PlaylistItem>,
) -> SearchResults {
    let push_group =
        |results: &mut SearchResults, id: &str, title: &str, values: Vec<SearchEntry>| {
            if !values.is_empty() {
                results.groups.push(SearchGroup {
                    id: id.to_string(),
                    title: title.to_string(),
                    can_view_all: false,
                    entries: values,
                });
            }
        };
    let mut results = SearchResults {
        keyword,
        tracks,
        artists,
        albums,
        playlists,
        ..Default::default()
    };
    let tracks = results.tracks.clone();
    let artists = results.artists.clone();
    let albums = results.albums.clone();
    let playlists = results.playlists.clone();
    push_group(
        &mut results,
        "tracks",
        "歌曲",
        tracks.into_iter().map(SearchEntry::Track).collect(),
    );
    push_group(
        &mut results,
        "artists",
        "音乐人",
        artists.into_iter().map(SearchEntry::Artist).collect(),
    );
    push_group(
        &mut results,
        "albums",
        "专辑",
        albums.into_iter().map(SearchEntry::Album).collect(),
    );
    push_group(
        &mut results,
        "playlists",
        "歌单",
        playlists.into_iter().map(SearchEntry::Playlist).collect(),
    );
    results
}

/// 官方搜索页：综合 / 歌曲 / 音乐人 / 专辑 / 歌单。
pub fn search(
    session: &Session,
    keyword: &str,
    scope: SearchScope,
) -> anyhow::Result<SearchResults> {
    let soda = session.soda();
    match scope {
        SearchScope::All => {
            let body = soda
                .fetch_search_all_body(keyword, 1, 30)
                .map_err(|err| wrap_err("综合搜索失败", err))?;
            raw_search_results(keyword, &body)
        }
        SearchScope::Tracks => {
            let tracks = session.search(keyword)?;
            Ok(scoped_results(
                keyword.to_string(),
                tracks,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ))
        }
        SearchScope::Artists => {
            let artists = soda
                .search_artist(keyword)
                .map_err(|err| wrap_err("搜索音乐人失败", err))?
                .iter()
                .map(artist_item)
                .collect();
            Ok(scoped_results(
                keyword.to_string(),
                Vec::new(),
                artists,
                Vec::new(),
                Vec::new(),
            ))
        }
        SearchScope::Albums => {
            let albums = soda
                .search_album(keyword)
                .map_err(|err| wrap_err("搜索专辑失败", err))?
                .iter()
                .map(|album| AlbumItem {
                    id: album.id.clone(),
                    title: album.name.clone(),
                    cover: album.cover.clone(),
                    artist: album.creator.clone(),
                    track_count: album.track_count,
                    release_date: album
                        .extra
                        .get("release_date")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or_default(),
                    company: album.description.clone(),
                })
                .collect();
            Ok(scoped_results(
                keyword.to_string(),
                Vec::new(),
                Vec::new(),
                albums,
                Vec::new(),
            ))
        }
        SearchScope::Playlists => {
            let playlists = soda
                .search_playlist(keyword)
                .map_err(|err| wrap_err("搜索歌单失败", err))?
                .iter()
                .map(|playlist| PlaylistItem {
                    id: playlist.id.clone(),
                    title: playlist.name.clone(),
                    cover: playlist.cover.clone(),
                    track_count: playlist.track_count,
                    creator: playlist.creator.clone(),
                })
                .collect();
            Ok(scoped_results(
                keyword.to_string(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                playlists,
            ))
        }
    }
}

/// 音乐人页：详情接口已返回热歌和专辑，无需分别翻页。
pub fn artist_detail(session: &Session, artist_id: &str) -> anyhow::Result<ArtistDetail> {
    let value = session
        .soda()
        .fetch_artist_detail(artist_id)
        .map_err(|err| wrap_err("读取音乐人失败", err))?;
    anyhow::ensure!(
        value
            .get("status_code")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
            == 0,
        "读取音乐人失败：{}",
        value
            .pointer("/status_info/status_msg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("接口返回错误")
    );

    let info = value.get("artist_info").cloned().unwrap_or_default();
    let artist = Artist {
        id: info
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(artist_id)
            .to_string(),
        name: info
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("音乐人")
            .to_string(),
        count_tracks: info
            .get("count_tracks")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default(),
        stats: libresoda::soda::types::ArtistStats {
            count_collected: value
                .pointer("/artist_info/stats/count_collected")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default(),
        },
        url_avatar: info
            .get("url_avatar")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|err| anyhow::anyhow!("解析音乐人头像失败: {err}"))?
            .unwrap_or_default(),
    };
    let mut tracks = Vec::new();
    for track in value
        .get("hot_tracks")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let track: Track = serde_json::from_value(track)
            .map_err(|err| anyhow::anyhow!("解析音乐人热歌失败: {err}"))?;
        tracks.push(TrackItem::from(build_song_from_track(&track)));
    }
    let mut albums = Vec::new();
    for album in value
        .get("hot_albums")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let album: Album = serde_json::from_value(album)
            .map_err(|err| anyhow::anyhow!("解析音乐人专辑失败: {err}"))?;
        albums.push(album_item(&album));
    }

    let tracks_has_more = value
        .get("has_more_tracks")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(!tracks.is_empty());
    Ok(ArtistDetail {
        artist: artist_item(&artist),
        description: info
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        tracks,
        albums,
        tracks_cursor: String::new(),
        tracks_has_more,
    })
}

/// 音乐人热歌翻页；第一页由 `artist_detail` 的详情接口返回。
pub fn artist_tracks_page(
    session: &Session,
    artist_id: &str,
    cursor: &str,
    count: i64,
) -> anyhow::Result<ArtistTracksPage> {
    let value = session
        .soda()
        .list_artist_tracks(artist_id, cursor, count)
        .map_err(|err| wrap_err("读取音乐人歌曲失败", err))?;
    anyhow::ensure!(
        value
            .get("status_code")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
            == 0,
        "读取音乐人歌曲失败：{}",
        value
            .pointer("/status_info/status_msg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("接口返回错误")
    );
    let mut tracks = Vec::new();
    for track in value
        .get("tracks")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let track: Track = serde_json::from_value(track)
            .map_err(|err| anyhow::anyhow!("解析音乐人歌曲失败: {err}"))?;
        tracks.push(TrackItem::from(build_song_from_track(&track)));
    }
    Ok(ArtistTracksPage {
        tracks,
        next_cursor: value
            .get("next_cursor")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        has_more: value
            .get("has_more")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// 专辑页：PC 详情接口一次返回专辑信息和全碟曲目。
pub fn album_detail(session: &Session, album_id: &str) -> anyhow::Result<AlbumDetail> {
    let value = session
        .soda()
        .fetch_pc_album_detail(album_id)
        .map_err(|err| wrap_err("读取专辑失败", err))?;
    anyhow::ensure!(
        value
            .get("status_code")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
            == 0,
        "读取专辑失败：{}",
        value
            .pointer("/status_info/status_msg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("接口返回错误")
    );
    let album: Album = serde_json::from_value(value.get("album_info").cloned().unwrap_or_default())
        .map_err(|err| anyhow::anyhow!("解析专辑信息失败: {err}"))?;
    let mut tracks = Vec::new();
    for track in value
        .get("tracks")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let track: Track = serde_json::from_value(track)
            .map_err(|err| anyhow::anyhow!("解析专辑歌曲失败: {err}"))?;
        tracks.push(TrackItem::from(build_song_from_track(&track)));
    }
    Ok(AlbumDetail {
        album: album_item(&album),
        tracks,
    })
}

/// 我的歌单（含「我喜欢的音乐」）。
pub fn user_playlists(session: &Session) -> anyhow::Result<Vec<PlaylistItem>> {
    let playlists = session
        .soda()
        .get_user_playlists(1, 50)
        .map_err(|err| wrap_err("读取我的歌单失败", err))?;
    Ok(playlists
        .into_iter()
        .map(|playlist| PlaylistItem {
            id: playlist.id,
            title: playlist.name,
            cover: playlist.cover,
            track_count: playlist.track_count,
            creator: playlist.creator,
        })
        .collect())
}

/// 「我喜欢的音乐」里的曲目。
///
/// 官方客户端把它当作一张 `type == 1` 的系统歌单；这里按名字匹配兜底
/// （不同版本命名可能是「我喜欢的音乐」/「喜欢的音乐」）。
pub fn liked_songs(session: &Session) -> anyhow::Result<Vec<TrackItem>> {
    let playlists = session
        .soda()
        .get_user_playlists(1, 50)
        .map_err(|err| wrap_err("读取我的歌单失败", err))?;
    let liked = playlists
        .iter()
        .find(|playlist| {
            let name = playlist.name.as_str();
            name.contains("喜欢") || name.contains("收藏")
        })
        .ok_or_else(|| anyhow::anyhow!("没找到「我喜欢的音乐」歌单（账号可能未登录）"))?;
    let songs = session
        .soda()
        .get_playlist_songs(&liked.id)
        .map_err(|err| wrap_err("读取我喜欢的音乐失败", err))?;
    Ok(songs.iter().map(TrackItem::from).collect())
}

/// 我收藏的（歌单 / 专辑）。参数与官方客户端一致。
pub fn collected_playlists_and_albums(session: &Session) -> anyhow::Result<Vec<PlaylistItem>> {
    let items = session
        .soda()
        .collected_items("", 500, &["album", "playlist"])
        .map_err(|err| wrap_err("读取我收藏的内容失败", err))?;
    Ok(items
        .into_iter()
        .map(|item| PlaylistItem {
            id: item.id(),
            title: item.title(),
            cover: item.cover_url(),
            track_count: 0,
            creator: String::new(),
        })
        .collect())
}

/// 某个歌单的全部歌曲（点歌单卡片后展示）。
pub fn playlist_tracks(session: &Session, playlist_id: &str) -> anyhow::Result<Vec<TrackItem>> {
    let songs = session
        .soda()
        .get_playlist_songs(playlist_id)
        .map_err(|err| wrap_err("读取歌单歌曲失败", err))?;
    Ok(songs.iter().map(TrackItem::from).collect())
}

/// 「我喜欢的音乐」里所有曲目的 id（用于列表里的爱心状态）。
pub fn liked_track_ids(session: &Session) -> anyhow::Result<std::collections::HashSet<String>> {
    Ok(liked_songs(session)?
        .into_iter()
        .map(|track| track.id)
        .collect())
}

/// 收藏 / 取消收藏一首歌到「我喜欢的音乐」。
pub fn set_track_liked(session: &Session, track_id: &str, liked: bool) -> anyhow::Result<()> {
    let media = [libresoda::soda::media_ref::MediaRef::track(track_id)];
    let result = if liked {
        session.soda().collect_media(&media)
    } else {
        session.soda().uncollect_media(&media)
    };
    result.map(|_| ()).map_err(|err| {
        wrap_err(
            if liked {
                "收藏失败"
            } else {
                "取消收藏失败"
            },
            err,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    fn fake_session() -> Session {
        // 只验证"没配 cookie 时不 panic、返回错误"这类离线行为。
        Session::new(Settings::default())
    }

    #[test]
    fn liked_songs_requires_login() {
        let session = fake_session();
        assert!(liked_songs(&session).is_err());
    }
}
