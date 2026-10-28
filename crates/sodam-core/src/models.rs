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
    /// 这行是否来自增强 LRC 的逐字标签（`<start,end>字`）。
    ///
    /// 不能用 `words.len() > 1` 推断：普通 LRC 会被补一个覆盖整行的伪词，
    /// 而增强 LRC 的单字句也只有一个词。两者行为完全不同，必须显式区分。
    pub has_word_timeline: bool,
}

impl LyricLine {
    /// 当前播放位置命中的字/词下标。
    pub fn active_word(&self, position_seconds: f64) -> Option<usize> {
        self.words
            .iter()
            .rposition(|word| position_seconds + 0.05 >= word.start_seconds)
    }

    /// 本行**保持高亮**到什么时候：下一句开始的那一刻。
    ///
    /// 不给"走满后先变暗"留窗口：上一句一直亮到下一句接管为止。
    /// 这样切句时是"旧的整句亮着 -> 新的整句接上"，中间不会出现
    /// 整屏都暗一下的空档。
    ///
    /// 没有下一句时（最后一句）就一直亮着。
    pub fn active_end_seconds(&self, next_start: Option<f64>) -> f64 {
        match next_start {
            Some(next) if next > self.start_seconds => next,
            _ => next_start.unwrap_or(f64::INFINITY),
        }
    }

    /// 本行此刻是否处于"正在唱"状态（应以强调色渲染）。
    ///
    /// 区间是 `[start_seconds, 下一句开始)`：走完之后亮光停在整行全亮，
    /// 直到下一句接管。普通 LRC 没有行时长时同样是这个区间，
    /// 只是它的进度会直接视为已满。
    pub fn is_active_at(&self, position_seconds: f64, next_start: Option<f64>) -> bool {
        position_seconds >= self.start_seconds
            && position_seconds < self.active_end_seconds(next_start)
    }

    /// 逐字高亮的**进度比例**：`0.0` = 整行未开始，`1.0` = 整行唱完。
    ///
    /// 返回连续的浮点比例而不是字节边界：渲染层据此按像素裁剪，
    /// 因此亮光能停在**一个字中间**（#80 的“丝滑”就来自这里）。
    ///
    /// 有逐字时间轴时按**每个字真实的演唱时长**推进（见 [`Self::timed_char_fraction`]），
    /// 没有时退化成按行内时间线性摊平。
    pub fn highlight_fraction(&self, position_seconds: f64) -> f32 {
        if position_seconds < self.start_seconds {
            return 0.0;
        }
        if position_seconds >= self.end_seconds {
            return 1.0;
        }

        let line_span = self.end_seconds - self.start_seconds;
        if line_span <= 0.0 {
            return 1.0;
        }

        // 有逐字时间轴就按**每个字真实的演唱时长**推进。
        //
        // 为什么不能按行内时间线性摊平：歌词里每个字占的时长差异很大——
        // 拖长音的字可能占一个词 1.5s 中的全部，而快词 3 个字只占 0.5s。
        // 线性摊平会让最后一个字"唰"一下掠过（这正是实测到的
        // "最后一个字走字速度特别快"），而长音反而走得慢。
        //
        // 这里把每个词的时长**按字符数均分**到它的字上，逐个累加。
        // 词与词之间的空档不计入（真实演唱中那段本来就没在唱）。
        if self.has_word_timeline {
            if let Some(fraction) = self.timed_char_fraction(position_seconds) {
                return fraction;
            }
        }

        // 没有可用逐字轴（普通 LRC / 脏数据）：退化成按行内时间线性推进。
        (((position_seconds - self.start_seconds) / line_span) as f32).clamp(0.0, 1.0)
    }

    /// 按逐字时间轴算出「已唱完的字数占比」，范围 `0.0..=1.0`。
    ///
    /// 每个词的总时长按它的**字符数均分**——汽水的逐字标签只给到词一级
    /// （`<offset,span,flag>` 后跟整段文本），单字内部没有独立时间，
    /// 均分是最接近实际的近似。
    ///
    /// 返回 `None` 表示这条时间轴不可用（没有任何有时长的词），由调用方退化。
    fn timed_char_fraction(&self, position_seconds: f64) -> Option<f32> {
        // 只统计"真正在唱"的词：时长为零的词（纯空格/标点占位）不占进度。
        let mut timed: Vec<(&LyricWord, f64, usize)> = Vec::new();
        for word in &self.words {
            let span = word.end_seconds - word.start_seconds;
            let count = word.text.chars().count();
            if count == 0 {
                continue;
            }
            // 词时长可以为 0（同一时刻的连读），仍要占宽度，但不占时间。
            timed.push((word, span.max(0.0), count));
        }
        let total_chars: usize = timed.iter().map(|(_, _, count)| count).sum();
        if total_chars == 0 {
            return None;
        }

        let mut done_chars = 0.0f64;
        for (word, span, count) in &timed {
            if position_seconds >= word.end_seconds {
                // 整个词已唱完。
                done_chars += *count as f64;
                continue;
            }
            if position_seconds <= word.start_seconds {
                // 还没开始这个词。
                break;
            }
            // 正在这个词内部：按时间比例推进它的字符。
            let ratio = if *span > 0.0 {
                (position_seconds - word.start_seconds) / span
            } else {
                1.0
            };
            done_chars += *count as f64 * ratio.clamp(0.0, 1.0);
            break;
        }

        Some((done_chars / total_chars as f64) as f32)
    }
}

/// 在两次进度快照之间做线性外推，让逐字高亮连续推进（#80）。
///
/// 背景：音频线程每 100ms 才把播放位置写进快照一次（那是为了省电，不该为歌词改），
/// 而歌词页以 60fps 重绘。直接读快照会让亮光每 100ms 跳一格，肉眼就是“一顿一顿”。
///
/// `anchor` 是「上次读到快照值的本地时刻 + 那个值」。快照值没变说明还在同一拍里，
/// 就按本地时钟往前外推；变了就重新锚定。
///
/// 两个上限都是为了“宁可慢半拍，也不要乱跳”：
/// * `max_lookahead`：快照长时间不动（卡顿 / 缓冲）时不要凭空往前跑；
/// * `duration`：不要超过曲目长度。
///
/// 只在播放中调用；暂停时位置本就不该变。
pub fn extrapolate_position(
    anchor: (std::time::Instant, f64),
    snapshot_position: f64,
    now: std::time::Instant,
    duration_seconds: f64,
    max_lookahead: f64,
) -> (f64, (std::time::Instant, f64)) {
    // 快照值变了 → 用新的值重新锚定到“现在”。
    let anchored = if (anchor.1 - snapshot_position).abs() > 1e-6 {
        (now, snapshot_position)
    } else {
        anchor
    };
    let elapsed = now.duration_since(anchored.0).as_secs_f64().max(0.0);
    let position = (anchored.1 + elapsed.min(max_lookahead)).min(duration_seconds.max(0.0));
    (position, anchored)
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
        let has_word_timeline = !words.is_empty();
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
        // 行结束时间：优先用行级时长（`[start,duration]` 里的 duration），
        // 没给时长时取逐字词里最晚的结束时间。
        // 不能直接取 `words.last().end_seconds`：按 start 排序不等于按 end 排序，
        // 末尾那个词的结束时间不一定最大。
        let end_seconds = match line_duration {
            Some(duration) if duration > 0.0 => start_seconds + duration,
            _ => words
                .iter()
                .map(|word| word.end_seconds)
                .fold(start_seconds, f64::max),
        };
        lines.push(LyricLine {
            start_seconds,
            end_seconds,
            text,
            words,
            has_word_timeline,
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
    // 只有真的遇到「可解析的逐字时间标签」才说明这行带逐字时间轴。
    // 不能用 `words` 非空判断：没有标签时剩下的整行文本也会被当成一个词推入。
    let mut saw_timeline_tag = false;

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
            let parsed = parse_word_tag(tag, line_start, line_duration);
            saw_timeline_tag |= parsed.is_some();
            parsed
        };
        cursor = close + 1;
    }
    push_text_before(&mut words, &content[cursor..], line_start, current);
    if !saw_timeline_tag {
        // 没有逐字标签：把推入的词清掉，让调用方当成普通 LRC。
        words.clear();
    }
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

/// 解析一个逐字标签，换成行内绝对时间（秒）。
///
/// 汽水真实格式是**三个字段**：`<offset, span, flag>`，单位毫秒，且
/// **`offset` 是相对行首的偏移**，不是绝对时间。例如：
///
/// ```text
/// [17240,5060]<0,480,0>Don't <480,240,0>want <720,240,0>it
/// ```
///
/// 旧实现只切两段、还当绝对时间，结果真实数据一个词都解析不出来（`words` 只剩
/// 解析器补的整行伪词），逐字高亮只能空跑。这里按行首 + 偏移换算。
fn parse_word_tag(tag: &str, line_start: f64, _line_duration: Option<f64>) -> Option<(f64, f64)> {
    let mut parts = tag.split(',');
    let offset: f64 = parts.next()?.trim().parse().ok()?;
    let span: f64 = parts.next()?.trim().parse().ok()?;
    // 第三个字段（flag）不参与时间计算，但要允许它存在。
    let start = line_start + offset / 1000.0;
    let end = start + span.max(0.0) / 1000.0;
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lrc_preserves_word_timings() {
        // 汽水真实格式：`<相对偏移,时长,flag>`，偏移**相对行首**。
        let raw = "[14500,5200]<0,180,0>你<180,180,0>好<360,340,0>呀\n[00:01.20]普通行\n";
        let lines = parse_lrc(raw);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start_seconds, 1.2);
        assert_eq!(lines[0].text, "普通行");
        assert_eq!(lines[0].words.len(), 1);
        // 普通 LRC 没有逐字时间轴。
        assert!(!lines[0].has_word_timeline);
        assert_eq!(lines[1].start_seconds, 14.5);
        assert_eq!(lines[1].text, "你好呀");
        assert!(lines[1].has_word_timeline);
        assert_eq!(lines[1].words.len(), 3);
        assert_eq!(lines[1].words[0].text, "你");
        // 首词从行首开始，持续 180ms。
        assert!((lines[1].words[0].start_seconds - 14.5).abs() < 1e-9);
        assert!((lines[1].words[0].end_seconds - 14.68).abs() < 1e-9);
    }

    /// 逐字高亮：行开始前是 0，行结束后是 1。
    #[test]
    fn highlight_is_empty_before_the_line_and_full_after_it() {
        let raw = "[10000,2000]<0,500,0>你<500,500,0>好<1000,1000,0>呀";
        let line = &parse_lrc(raw)[0];
        assert_eq!(line.text, "你好呀");
        assert_eq!(line.highlight_fraction(9.9), 0.0, "行开始前不该亮");
        assert_eq!(line.highlight_fraction(10.0), 0.0, "刚开嗓时还一个字没吐");
        assert_eq!(line.highlight_fraction(12.0), 1.0, "行结束后整行亮");
        assert_eq!(line.highlight_fraction(99.0), 1.0);
    }

    /// 亮光必须随时间往前推，并且是连续值（不是只落在整字边界上）。
    #[test]
    fn highlight_advances_with_time() {
        let raw = "[0,2000]<0,1000,0>你<1000,1000,0>好";
        let line = &parse_lrc(raw)[0];
        let at = |seconds: f64| line.highlight_fraction(seconds);

        assert_eq!(at(0.0), 0.0);
        // 一半进度：这正是“字内过渡”的来源——旧实现只能整字跳变，这里必须是 0.5。
        assert!((at(1.0) - 0.5).abs() < 1e-3, "got {}", at(1.0));
        assert!((at(1.5) - 0.75).abs() < 1e-3, "got {}", at(1.5));
        assert_eq!(at(2.0), 1.0);
    }

    /// 普通 LRC（无逐字数据）没有时长信息，到点即整行点亮。
    #[test]
    fn plain_lrc_has_no_word_progress() {
        let line = &parse_lrc("[00:01.00]普通行")[0];
        assert_eq!(line.text, "普通行");
        assert!(!line.has_word_timeline);
        assert_eq!(line.highlight_fraction(0.5), 0.0, "行开始前");
        assert_eq!(line.highlight_fraction(1.0), 1.0, "行到时即满");
        assert_eq!(line.highlight_fraction(99.0), 1.0);
    }

    /// 词与原文对不上、或词里带空格（`trim` 后与原文不一致）时不能崩、不能越界。
    ///
    /// 旧实现靠“累加 word.text.len()”定位字节偏移，这两种情况都会错位；
    /// 现在按时间比例计算，天然不受文本与词表长度差异影响。
    #[test]
    fn highlight_tolerates_mismatched_and_spaced_words() {
        // 脏数据：词与原文完全对不上。
        let broken = LyricLine {
            start_seconds: 0.0,
            end_seconds: 2.0,
            text: "实际歌词".to_string(),
            words: vec![LyricWord {
                start_seconds: 0.0,
                end_seconds: 1.0,
                text: "对不上的词".to_string(),
            }],
            has_word_timeline: true,
        };
        let value = broken.highlight_fraction(1.5);
        assert!((0.0..=1.0).contains(&value), "比例必须落在 0..=1：{value}");

        // 词之间的空格被 trim 掉，而 text 保留空格。
        let spaced = &parse_lrc("[0,3000]<0,1500,0>first <1500,1500,0>second")[0];
        assert!(
            spaced.text.contains(' '),
            "原文应保留空格：{:?}",
            spaced.text
        );
        assert_eq!(spaced.highlight_fraction(3.0), 1.0, "全部唱完是整行");
    }

    /// 词时长不同时，进度按**词内字符均分的真实时长**累加。
    #[test]
    fn highlight_fraction_is_time_weighted_per_char() {
        // 三个词：1 字/0.5s、2 字/0.25s、4 字/1.25s，共 7 字、2s。
        let raw = "[0,2000]<0,500,0>短<500,250,0>很短<750,1250,0>很长很长";
        let line = &parse_lrc(raw)[0];
        let at = |t: f64| line.highlight_fraction(t);

        assert!((at(-1.0) - 0.0).abs() < 1e-6, "行开始前是 0");
        assert!((at(0.0) - 0.0).abs() < 1e-6);
        // 第 1 个词（1 字）唱完 -> 1/7
        assert!((at(0.5) - 1.0 / 7.0).abs() < 1e-3, "got {}", at(0.5));
        // 第 2 个词（2 字）唱完 -> 3/7
        assert!((at(0.75) - 3.0 / 7.0).abs() < 1e-3, "got {}", at(0.75));
        // 第 3 个词走一半（1.375s）-> 3 + 2 = 5/7
        assert!((at(1.375) - 5.0 / 7.0).abs() < 1e-3, "got {}", at(1.375));
        assert!((at(2.0) - 1.0).abs() < 1e-6);
        assert!((at(99.0) - 1.0).abs() < 1e-6);
    }

    /// 词与词之间的空档（真实换气）会让进度短暂停住，但**不能倒退**，
    /// 且空档结束后必须继续推进。
    ///
    /// 这是按字推进的必然结果：空档里没有字在唱，进度自然不前进。
    /// 实测词间空档约 0.28s，观感上像一次换气，比"匀速硬走"更自然。
    #[test]
    fn progress_pauses_in_gaps_but_never_rewinds() {
        // 实测形状：`I` 0~0.56s，下一个词 0.84s 才开始（空 0.28s）。
        let raw = "[0,4820]<0,560,0>I<840,240,0>wanna<1080,480,0>talk<4340,480,0>end";
        let line = &parse_lrc(raw)[0];
        let at = |t: f64| line.highlight_fraction(t);

        // 空档期间（0.56~0.84）进度停在词 1 结束处。
        assert!((at(0.7) - at(0.56)).abs() < 1e-6, "空档内应停住");
        // 空档结束后继续推进。
        assert!(at(1.0) > at(0.84), "空档后必须继续走");

        // 全程单调不倒退。
        let mut prev = at(0.0);
        let mut t = 0.0;
        while t <= 4.82 {
            let value = at(t);
            assert!(value >= prev, "t={t} 时进度倒退：{prev} -> {value}");
            prev = value;
            t += 0.02;
        }
    }

    /// 行尾恰好走到 1.0（最后一个词结束 == 行结束，实测数据就是这个形状）。
    #[test]
    fn highlight_fraction_reaches_one_at_line_end() {
        let raw = "[0,2000]<0,1200,0>前半<1200,800,0>末字";
        let line = &parse_lrc(raw)[0];

        assert!((line.highlight_fraction(2.0) - 1.0).abs() < 1e-6);
        // 最后一词走一半时是 3/4（前 2 字 + 末词 1/2）。
        assert!(
            (line.highlight_fraction(1.6) - 0.75).abs() < 1e-3,
            "got {}",
            line.highlight_fraction(1.6)
        );
    }

    /// 走字必须在行结束处走到 1.0，并一直亮到下一句接管。
    ///
    /// "走不完"的根因是走满的瞬间同时也是取消高亮的瞬间，
    /// `progress == 1.0` 那一帧没机会被画出来。现在不再提前取消：
    /// 整行亮着，直到下一句开始。
    #[test]
    fn highlight_stays_lit_until_the_next_line_takes_over() {
        // 本行 10.0~12.0，下一句 13.0 开始（中间空 1s，与实测数据形状一致）。
        let line = &parse_lrc("[10000,2000]<0,2000,0>整句")[0];

        assert!(
            (line.highlight_fraction(12.0) - 1.0).abs() < 1e-6,
            "12.0s 应走满"
        );
        // 走满之后到下一句之前，保持高亮，不再"先变暗"。
        assert!(line.is_active_at(12.0, Some(13.0)));
        assert!(
            line.is_active_at(12.5, Some(13.0)),
            "下一句开始前必须一直亮着"
        );
        assert!(line.is_active_at(12.99, Some(13.0)));
        // 下一句开始当即让位。
        assert!(!line.is_active_at(13.0, Some(13.0)));
    }

    /// 每个字按**自己真实的演唱时长**推进，不能按行内时间线性摊平。
    ///
    /// 线性摊平会让拖长音的字走得飞快（"最后一个字唰一下就过去了"）。
    #[test]
    fn chars_advance_by_their_own_duration() {
        // 2.0s 一行，3 个字：
        //   "AB" 占 0~0.5s（快），"C" 占 0.5~2.0s（拖长音 1.5s）。
        let line = &parse_lrc("[0,2000]<0,500,0>AB<500,1500,0>C")[0];

        // t=1.25 是 C 的中间：已唱完 2 个字 + C 的一半 = 2.5/3 = 0.833。
        // 若按行内时间线性摊平，这里只有 1.25/2 = 0.625，最后一个字会明显走得快。
        let at_mid_c = line.highlight_fraction(1.25);
        assert!(
            (at_mid_c - 0.833).abs() < 5e-3,
            "拖长音的字应按自身时长推进，期望 0.833，实际 {at_mid_c}"
        );

        // C 走一半（0.5 + 0.75 = 1.25s）刚好一半宽 —— 末字速度与时长成正比。
        assert!((line.highlight_fraction(0.5) - 2.0 / 3.0).abs() < 5e-3);
        assert!((line.highlight_fraction(2.0) - 1.0).abs() < 1e-6);
    }

    /// 没有行时长（普通 LRC）时退化成"亮到下一句开始"。
    #[test]
    fn plain_lrc_stays_lit_until_next_line() {
        let line = &parse_lrc("[00:10.00]普通行")[0];
        assert_eq!(line.end_seconds, line.start_seconds, "普通 LRC 没有时长");
        assert!(line.is_active_at(10.5, Some(14.0)));
        assert!(!line.is_active_at(14.0, Some(14.0)), "下一句开始即让位");
        // 没有下一句就一直亮着。
        assert!(line.is_active_at(99.0, None));
    }

    /// 外推：同一拍快照内按本地时钟连续推进，快照变化时重新锚定。    /// 外推：同一拍快照内按本地时钟连续推进，快照变化时重新锚定。    /// 外推：同一拍快照内按本地时钟连续推进，快照变化时重新锚定。    /// 外推：同一拍快照内按本地时钟连续推进，快照变化时重新锚定。
    #[test]
    fn extrapolation_advances_within_a_snapshot_tick() {
        use std::time::{Duration, Instant};

        let t0 = Instant::now();
        let anchor = (t0, 10.0);
        let lookahead = 0.25;

        // 快照还是 10.0，过了 50ms → 外推到 10.05
        let (pos, _) = extrapolate_position(
            anchor,
            10.0,
            t0 + Duration::from_millis(50),
            200.0,
            lookahead,
        );
        assert!((pos - 10.05).abs() < 1e-6, "got {pos}");

        // 快照跳到 10.1（音频线程写了新值）→ 立即以新值为准，不叠加旧外推
        let (pos, new_anchor) = extrapolate_position(
            anchor,
            10.1,
            t0 + Duration::from_millis(100),
            200.0,
            lookahead,
        );
        assert!((pos - 10.1).abs() < 1e-6, "got {pos}");
        assert!((new_anchor.1 - 10.1).abs() < 1e-6);
    }

    /// 快照长期不动（卡顿 / 缓冲）时，外推必须有上限，不能跑到声音前面。
    #[test]
    fn extrapolation_is_capped_when_the_snapshot_stalls() {
        use std::time::{Duration, Instant};

        let t0 = Instant::now();
        let (pos, _) =
            extrapolate_position((t0, 5.0), 5.0, t0 + Duration::from_secs(5), 200.0, 0.25);
        assert!((pos - 5.25).abs() < 1e-6, "最多外推 250ms，got {pos}");
    }

    /// 外推不能超过曲目时长。
    #[test]
    fn extrapolation_never_exceeds_duration() {
        use std::time::{Duration, Instant};

        let t0 = Instant::now();
        let (pos, _) =
            extrapolate_position((t0, 199.9), 199.9, t0 + Duration::from_secs(1), 200.0, 0.25);
        assert!(pos <= 200.0, "got {pos}");
    }

    /// 单调性：外推不能让位置倒退（倒退比慢半拍更刺眼）。
    #[test]
    fn extrapolation_never_goes_backwards() {
        use std::time::{Duration, Instant};

        let t0 = Instant::now();
        let mut anchor = (t0, 0.0);
        let mut last = 0.0f64;
        // 模拟快照每 100ms 更新、UI 每 16ms 读一次
        for frame in 0..30u64 {
            let now = t0 + Duration::from_millis(frame * 16);
            let snapshot = (frame * 16 / 100) as f64 * 0.1;
            let (pos, next) = extrapolate_position(anchor, snapshot, now, 200.0, 0.25);
            assert!(pos >= last - 1e-9, "第 {frame} 帧倒退了：{last} -> {pos}");
            last = pos;
            anchor = next;
        }
        assert!(last > 0.3, "应当持续推进到 0.3 以上，got {last}");
    }

    /// 比例必须单调递增：不能出现“倒回去”。
    #[test]
    fn highlight_fraction_never_goes_backwards() {
        let raw = "[0,3000]<0,1000,0>甲<1000,1000,0>乙<2000,1000,0>丙";
        let line = &parse_lrc(raw)[0];
        let mut last = -1.0f32;
        let mut t = 0.0;
        while t <= 3.0 {
            let value = line.highlight_fraction(t);
            assert!(value >= last, "t={t} 时倒退了：{last} -> {value}");
            assert!((0.0..=1.0).contains(&value));
            last = value;
            t += 0.05;
        }
        assert!((last - 1.0).abs() < 1e-6);
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
