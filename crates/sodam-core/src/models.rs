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

    /// 逐字高亮的**时间比例**：`0.0` = 整行未开始，`1.0` = 整行唱完。
    ///
    /// 返回连续的浮点比例而不是字节边界：渲染层据此按像素裁剪，
    /// 因此亮光能停在**一个字中间**（#80 的“丝滑”就来自这里）。
    ///
    /// 没有逐字时间轴（普通 LRC）时仍然按行内时间线性推进：亮光会平滑扫过整行，
    /// 只是字与字之间没有细粒度的停顿差异。这比"整行突然点亮"更接近主流客户端。
    ///
    /// # 为什么不能按「已唱词时长 / 词时长之和」
    ///
    /// 真实歌词里词与词之间常有空档。例如实测的一行：
    ///
    /// ```text
    /// I     0.00 → 0.56        （下一个词 0.84 才开始，中间空 0.28s）
    /// wanna 0.84 → 1.08
    /// ```
    ///
    /// 若按「词时长之和」归一化，那么所有词唱完后进度就已经是 1.0，**行尾那段
    /// 空档完全不推进**；空档期间进度也会冻结，到下一个词开始才突然跳一截。
    /// 肉眼就是「最后一个字一下亮了，然后立刻换下一句」。
    ///
    /// 这里改成：**进度严格跟随行内时间**，词之间/词之外的空档照常推进，
    /// 只有「当前词内部」按该词的时间线性插值（保证字与字之间平滑）。
    /// 归一化也用**行时长**，所以整行唱完时进度恰好到 1.0。
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

        // 进度就是「此刻在本行内的时间占比」。逐字信息用来保证词内部平滑，
        // 但不需要它来推进——时间本身一直在走，空档因此自动被覆盖。
        (((position_seconds - self.start_seconds) / line_span) as f32).clamp(0.0, 1.0)
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

    /// 时间比例必须连续推进，且按词的真实时长加权（不是按字符数均分）。
    #[test]
    fn highlight_fraction_is_continuous_and_time_weighted() {
        // 三个词时长不同：0.5s / 0.25s / 1.25s，总 2s。
        let raw = "[0,2000]<0,500,0>短<500,250,0>很短<750,1250,0>很长很长";
        let line = &parse_lrc(raw)[0];
        let at = |t: f64| line.highlight_fraction(t);

        assert!((at(-1.0) - 0.0).abs() < 1e-6, "行开始前是 0");
        assert!((at(0.0) - 0.0).abs() < 1e-6);
        // 第 1 个词唱完 = 0.5/2 = 0.25
        assert!((at(0.5) - 0.25).abs() < 1e-3, "got {}", at(0.5));
        // 第 2 个词唱完 = 0.75/2 = 0.375
        assert!((at(0.75) - 0.375).abs() < 1e-3, "got {}", at(0.75));
        // 中途：1.75s = (0.5+0.25+1.0)/2 = 0.875
        assert!((at(1.75) - 0.875).abs() < 1e-3, "got {}", at(1.75));
        assert!((at(2.0) - 1.0).abs() < 1e-6);
        assert!((at(99.0) - 1.0).abs() < 1e-6);
    }

    /// 词与词之间有**空档**时，进度必须照常随时间推进，不能在空档里冻住。
    ///
    /// 这是 #80 的真实 bug：按「已唱词时长 / 词时长之和」算时，
    /// 所有词唱完进度就到 1.0，行尾空档完全不推进 ——
    /// 肉眼就是「最后一个字一下亮了，然后立刻换下一句」。
    #[test]
    fn highlight_fraction_keeps_moving_through_word_gaps() {
        // 实测真实歌词的形状：`I` 唱到 0.56s，下一个词 0.84s 才开始（空 0.28s），
        // 行总长 4.82s，最后一个词唱到 4.82s。
        let raw = "[0,4820]<0,560,0>I <840,240,0>wanna<1080,480,0>talk<4820,0,0>";
        let line = &parse_lrc(raw)[0];
        let at = |t: f64| line.highlight_fraction(t);

        // 空档中点（0.7s）：进度应当在 0.56/4.82 与 0.84/4.82 之间持续前进，
        // 而不是停在「词 1 结束」的那个值上。
        let in_gap = at(0.7);
        let after_word1 = 0.56 / 4.82;
        assert!(
            (in_gap as f64) > after_word1 + 1e-6,
            "空档里进度必须继续前进：{in_gap} vs {after_word1}"
        );

        // 空档前后逐帧采样：不能出现「一段时间完全不动」。
        let mut prev = at(0.5);
        let mut t = 0.5;
        while t <= 1.0 {
            let value = at(t);
            assert!(value >= prev, "t={t} 时进度倒退：{prev} -> {value}");
            prev = value;
            t += 0.02;
        }
    }

    /// 行尾必须恰好走到 1.0（按行时长归一化，不是按词时长之和）。
    #[test]
    fn highlight_fraction_reaches_one_at_line_end() {
        // 行 2s，最后一个词 1.4s 就唱完了，剩余 0.6s 是行尾空档。
        let raw = "[0,2000]<0,1400,0>唱完";
        let line = &parse_lrc(raw)[0];

        assert!(
            (line.highlight_fraction(1.4) - 0.7).abs() < 1e-3,
            "1.4s 应到 70%"
        );
        assert!(
            (line.highlight_fraction(1.7) - 0.85).abs() < 1e-3,
            "1.7s 应到 85%"
        );
        assert!((line.highlight_fraction(2.0) - 1.0).abs() < 1e-6);
    }

    /// 外推：同一拍快照内按本地时钟连续推进，快照变化时重新锚定。
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
