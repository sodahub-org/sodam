//! UI 友好的数据模型（把 libresoda 的类型转成界面直接用得上的形状）。

use libresoda::Song;
use serde::{Deserialize, Serialize};

/// 曲目（列表行 / 播放器 / 队列都用它）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackItem {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artist_id: String,
    pub album_id: String,
    pub cover: String,
    pub duration_seconds: i64,
    pub vip: bool,
}

impl From<&Song> for TrackItem {
    fn from(song: &Song) -> Self {
        Self {
            id: song.id.clone(),
            title: song.name.clone(),
            artist: song.artist.clone(),
            album: song.album.clone(),
            artist_id: song.extra_get("artist_id").unwrap_or_default().to_string(),
            album_id: song.album_id.clone(),
            cover: song.cover.clone(),
            duration_seconds: song.duration,
            vip: song.is_vip,
        }
    }
}

impl From<Song> for TrackItem {
    fn from(song: Song) -> Self {
        Self::from(&song)
    }
}

impl TrackItem {
    /// `mm:ss`（时长未知时返回 `--:--`）。
    pub fn duration_label(&self) -> String {
        if self.duration_seconds <= 0 {
            return "--:--".to_string();
        }
        format!(
            "{:02}:{:02}",
            self.duration_seconds / 60,
            self.duration_seconds % 60
        )
    }
}

/// 歌单（自己创建的 / 收藏的 / 搜索到的）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaylistItem {
    pub id: String,
    pub title: String,
    pub cover: String,
    pub track_count: i64,
    pub creator: String,
}

/// 音乐人（搜索结果 / 音乐人页）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtistItem {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub track_count: i64,
    pub follower_count: i64,
}

/// 专辑（搜索结果 / 专辑页）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlbumItem {
    pub id: String,
    pub title: String,
    pub cover: String,
    pub artist: String,
    pub track_count: i64,
    pub release_date: i64,
    pub company: String,
}

/// 综合搜索里的一类结果。
#[derive(Debug, Clone, PartialEq)]
pub enum SearchEntry {
    Track(TrackItem),
    Artist(ArtistItem),
    Album(AlbumItem),
    Playlist(PlaylistItem),
}

/// 官方综合搜索的分组（你可能想搜 / 歌曲 / 音乐人 / 专辑 / 歌单）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchGroup {
    pub id: String,
    pub title: String,
    pub can_view_all: bool,
    pub entries: Vec<SearchEntry>,
}

/// 一页搜索结果；`groups` 只在综合搜索时有值。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    pub keyword: String,
    pub groups: Vec<SearchGroup>,
    pub tracks: Vec<TrackItem>,
    pub artists: Vec<ArtistItem>,
    pub albums: Vec<AlbumItem>,
    pub playlists: Vec<PlaylistItem>,
}

impl SearchResults {
    /// 用于状态栏 / 空态判断的总条数。
    pub fn total_count(&self) -> usize {
        self.tracks.len() + self.artists.len() + self.albums.len() + self.playlists.len()
    }

    /// 把所有分组里的歌拍平，保证综合页也能一键替换队列。
    pub fn all_tracks(&self) -> Vec<TrackItem> {
        if !self.tracks.is_empty() {
            return self.tracks.clone();
        }
        self.groups
            .iter()
            .flat_map(|group| group.entries.iter())
            .filter_map(|entry| match entry {
                SearchEntry::Track(track) => Some(track.clone()),
                _ => None,
            })
            .collect()
    }
}

/// 音乐人页数据（详情接口 + 热歌/专辑）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtistDetail {
    pub artist: ArtistItem,
    pub description: String,
    pub tracks: Vec<TrackItem>,
    pub albums: Vec<AlbumItem>,
    /// 详情接口的热歌只是第一页；继续加载用这个游标。
    pub tracks_cursor: String,
    pub tracks_has_more: bool,
}

/// 音乐人热歌的下一页。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtistTracksPage {
    pub tracks: Vec<TrackItem>,
    pub next_cursor: String,
    pub has_more: bool,
}

/// 专辑页数据（详情接口 + 全碟曲目）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlbumDetail {
    pub album: AlbumItem,
    pub tracks: Vec<TrackItem>,
}

/// 听歌模式（场景模式）条目。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneItem {
    pub text: String,
    pub entry_type: String,
    pub scene_mode_id: i64,
    pub sub_queue_type: String,
    pub cover: String,
}

/// 逐字歌词里的一个字/词。
#[derive(Debug, Clone, PartialEq)]
pub struct LyricWord {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
}

/// 一行歌词；有逐字数据时 `words` 与 `text` 顺序一致。
#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
    pub words: Vec<LyricWord>,
}

impl LyricLine {
    /// 当前播放位置命中的字/词下标。
    pub fn active_word(&self, position_seconds: f64) -> Option<usize> {
        self.words
            .iter()
            .rposition(|word| position_seconds + 0.05 >= word.start_seconds)
    }
}

/// 解析汽水增强 LRC，保留逐字时间轴；也兼容普通 LRC。
pub fn parse_lrc(raw: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw_line in raw.lines() {
        let mut rest = raw_line.trim();
        let mut start_seconds = None;
        let mut line_duration = None;
        while rest.starts_with('[') {
            let Some((tag, tail)) = rest[1..].split_once(']') else {
                break;
            };
            if let Some((first, second)) = tag.split_once(',') {
                let Ok(first) = first.trim().parse::<f64>() else {
                    break;
                };
                let second = second.trim().parse::<f64>().unwrap_or(0.0);
                start_seconds = Some(first / 1000.0);
                line_duration = Some(second.max(0.0) / 1000.0);
                rest = tail.trim_start();
                break;
            }
            match parse_lrc_time_tag(tag) {
                Some(value) => {
                    start_seconds = Some(value);
                    rest = tail.trim_start();
                }
                None => break,
            }
        }
        let Some(start_seconds) = start_seconds else {
            continue;
        };
        let text = strip_word_tags(rest).trim().to_string();
        if text.is_empty() {
            continue;
        }
        let mut words = parse_word_tags(rest, start_seconds, line_duration);
        if words.is_empty() {
            let end = line_duration
                .map(|duration| start_seconds + duration)
                .unwrap_or(start_seconds);
            words.push(LyricWord {
                start_seconds,
                end_seconds: end,
                text: text.clone(),
            });
        }
        words.sort_by(|a, b| a.start_seconds.total_cmp(&b.start_seconds));
        let end_seconds = words
            .last()
            .map(|word| word.end_seconds)
            .unwrap_or(start_seconds)
            .max(start_seconds);
        lines.push(LyricLine {
            start_seconds,
            end_seconds,
            text,
            words,
        });
    }
    lines.sort_by(|a, b| a.start_seconds.total_cmp(&b.start_seconds));
    lines.dedup_by(|a, b| a.start_seconds == b.start_seconds && a.text == b.text);
    lines
}

fn parse_lrc_time_tag(tag: &str) -> Option<f64> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes = minutes.trim().parse::<f64>().ok()?;
    let seconds = seconds.trim().parse::<f64>().ok()?;
    Some(minutes * 60.0 + seconds)
}

/// 保留文字并删除 `<...>` 标记。
fn strip_word_tags(content: &str) -> String {
    let mut text = String::with_capacity(content.len());
    let mut inside = false;
    for character in content.chars() {
        match character {
            '<' => inside = true,
            '>' => inside = false,
            value if !inside => text.push(value),
            _ => {}
        }
    }
    text
}

/// 解析 `<start,end>字词</start,end>`（汽水内容）；时间单位是毫秒。
fn parse_word_tags(content: &str, line_start: f64, line_duration: Option<f64>) -> Vec<LyricWord> {
    let mut words = Vec::new();
    let mut cursor = 0usize;
    let mut current: Option<(f64, f64)> = None;

    while let Some(offset) = content[cursor..].find('<') {
        let open = cursor + offset;
        push_text_before(&mut words, &content[cursor..open], line_start, current);
        let Some(close_offset) = content[open..].find('>') else {
            break;
        };
        let close = open + close_offset;
        let tag = &content[open + 1..close];
        current = if tag.starts_with('/') {
            None
        } else {
            parse_word_tag(tag, line_start, line_duration)
        };
        cursor = close + 1;
    }
    push_text_before(&mut words, &content[cursor..], line_start, current);
    words
}

fn push_text_before(
    words: &mut Vec<LyricWord>,
    text: &str,
    line_start: f64,
    current: Option<(f64, f64)>,
) {
    if text.trim().is_empty() {
        return;
    }
    let (start, end) = current.unwrap_or((line_start, line_start));
    words.push(LyricWord {
        start_seconds: start,
        end_seconds: end.max(start),
        text: text.trim().to_string(),
    });
}

fn parse_word_tag(tag: &str, line_start: f64, _line_duration: Option<f64>) -> Option<(f64, f64)> {
    let (first, second) = tag.split_once(',')?;
    let start = first.trim().parse::<f64>().ok()? / 1000.0;
    let second = second.trim().parse::<f64>().ok()? / 1000.0;
    let end = if second >= start {
        second
    } else {
        start + second
    };
    Some((start.max(line_start), end.max(start)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lrc_preserves_word_timings() {
        let raw =
            "[14500,5200]<14500,180>你</14680,180>好<14860,340>呀</15200,340>\n[00:01.20]普通行\n";
        let lines = parse_lrc(raw);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start_seconds, 1.2);
        assert_eq!(lines[0].text, "普通行");
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[1].start_seconds, 14.5);
        assert_eq!(lines[1].text, "你好呀");
        assert_eq!(lines[1].words.len(), 3);
        assert_eq!(lines[1].words[0].text, "你");
        assert_eq!(lines[1].words[0].end_seconds, 14.68);
    }

    #[test]
    fn duration_label_formats() {
        let mut track = TrackItem {
            duration_seconds: 236,
            ..Default::default()
        };
        assert_eq!(track.duration_label(), "03:56");
        track.duration_seconds = 0;
        assert_eq!(track.duration_label(), "--:--");
    }
}
