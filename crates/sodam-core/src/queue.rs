//! 播放队列（顺序 / 随机 / 单曲循环）。

use std::collections::HashSet;

use crate::models::TrackItem;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlayMode {
    #[default]
    Sequential,
    Shuffle,
    RepeatOne,
}

impl PlayMode {
    pub fn label(&self) -> &'static str {
        match self {
            PlayMode::Sequential => "顺序播放",
            PlayMode::Shuffle => "随机播放",
            PlayMode::RepeatOne => "单曲循环",
        }
    }

    pub fn next(self) -> Self {
        match self {
            PlayMode::Sequential => PlayMode::Shuffle,
            PlayMode::Shuffle => PlayMode::RepeatOne,
            PlayMode::RepeatOne => PlayMode::Sequential,
        }
    }
}

/// 播放队列：维护曲目列表与当前下标。
#[derive(Debug, Clone, Default)]
pub struct Queue {
    /// 队列版本号：每次变更 +1，UI 用它判断需不需要重建缓存快照。
    revision: u64,
    tracks: Vec<TrackItem>,
    index: usize,
    pub mode: PlayMode,
}

impl Queue {
    pub fn new(tracks: Vec<TrackItem>) -> Self {
        Self {
            revision: 0,
            tracks,
            index: 0,
            mode: PlayMode::default(),
        }
    }

    pub fn tracks(&self) -> &[TrackItem] {
        &self.tracks
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn current(&self) -> Option<&TrackItem> {
        self.tracks.get(self.index)
    }

    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// 队列版本号（UI 缓存用）。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn replace(&mut self, tracks: Vec<TrackItem>, start: usize) {
        self.bump();
        self.tracks = tracks;
        self.index = start.min(self.tracks.len().saturating_sub(1));
    }

    /// 返回随机排序后的新队列（不改变原 Vec）。
    ///
    /// 播放页的「随机播放」不是切换播放模式，而是把歌单重新洗牌后入队；
    /// 这样队列抽屉里能直接看到真实播放顺序。
    pub fn shuffled(tracks: Vec<TrackItem>) -> Vec<TrackItem> {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15)
            ^ (tracks.as_ptr() as u64);
        Self::shuffle_with_seed(tracks, seed)
    }

    /// 用种子做 Fisher-Yates 洗牌（测试可复现）。
    pub fn shuffle_with_seed(mut tracks: Vec<TrackItem>, mut seed: u64) -> Vec<TrackItem> {
        if tracks.len() < 2 {
            return tracks;
        }
        seed = seed.max(1);
        for index in (1..tracks.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let swap = (seed % (index as u64 + 1)) as usize;
            tracks.swap(index, swap);
        }
        tracks
    }

    /// 追加曲目；按曲目 id 去重，返回实际新增数量。
    pub fn append(&mut self, tracks: Vec<TrackItem>) -> usize {
        let known: HashSet<String> = self.tracks.iter().map(|track| track.id.clone()).collect();
        let before = self.tracks.len();
        for track in tracks {
            if !known.contains(&track.id) {
                self.tracks.push(track);
            }
        }
        let added = self.tracks.len() - before;
        if added > 0 {
            self.bump();
        }
        added
    }

    /// 下一首：单曲循环时停在原地（由播放器决定是否重播）。
    pub fn advance(&mut self) -> Option<&TrackItem> {
        self.bump();
        if self.tracks.is_empty() {
            return None;
        }
        if self.mode != PlayMode::RepeatOne {
            self.index = (self.index + 1) % self.tracks.len();
        }
        self.current()
    }

    /// 从队列里移除某一项（右键菜单用）。
    ///
    /// **不允许移除当前项**：正在播的这首还挂在引擎上，移除它会让
    /// 「队列当前项」与「引擎正在播的」分叉，自动切歌的推进规则全得跟着
    /// 特判（实测踩过：后继曲目会被跳过）。想换歌直接点下一首。
    ///
    /// 移除当前项之前的项则下标左移，保证仍指向同一首歌。
    pub fn remove(&mut self, index: usize) -> Option<TrackItem> {
        if index >= self.tracks.len() || index == self.index {
            return None;
        }
        let removed = self.tracks.remove(index);
        if index < self.index {
            self.index -= 1;
        }
        self.bump();
        Some(removed)
    }

    /// 把一批曲目插到当前曲目的后面（歌单右键「下一首播放」）。
    ///
    /// 队列为空时等价于从这批曲目的第一首开始播放；否则保持当前曲目不变。
    pub fn insert_next(&mut self, tracks: Vec<TrackItem>) -> bool {
        if tracks.is_empty() {
            return false;
        }

        let was_empty = self.tracks.is_empty();
        if was_empty {
            self.tracks = tracks;
            self.index = 0;
        } else {
            let insert_at = self.index + 1;
            self.tracks.splice(insert_at..insert_at, tracks);
        }
        self.bump();
        was_empty
    }

    /// 下一首会是谁（不改状态，用于**预取**）。
    ///
    /// 与 [`Queue::advance`] 的规则保持一致；单曲循环时返回当前曲目。
    pub fn peek_next(&self) -> Option<&TrackItem> {
        if self.tracks.is_empty() {
            return None;
        }
        match self.mode {
            PlayMode::RepeatOne => self.current(),
            _ => self.tracks.get((self.index + 1) % self.tracks.len()),
        }
    }

    /// 接下来会按序播放的至多 `count` 首（不改状态，用于**多首预取**）。
    ///
    /// * Sequential/Shuffle：从当前曲目之后起环绕取，绕回当前曲目即停，
    ///   并按曲目 id 去重（队列里可能有重复 id）；
    /// * RepeatOne：只返回当前曲目（它必然已缓存，调用方的缓存检查会跳过）。
    ///
    /// 与 [`Queue::advance`] 的推进规则保持一致。
    pub fn peek_ahead(&self, count: usize) -> Vec<&TrackItem> {
        let mut out: Vec<&TrackItem> = Vec::new();
        if self.tracks.is_empty() || count == 0 {
            return out;
        }
        match self.mode {
            PlayMode::RepeatOne => {
                if let Some(current) = self.current() {
                    out.push(current);
                }
            }
            _ => {
                let len = self.tracks.len();
                let mut cursor = self.index;
                let mut seen: HashSet<&str> = HashSet::new();
                while out.len() < count {
                    cursor = (cursor + 1) % len;
                    // 绕回当前曲目：到此为止（不预取正在播的这首）
                    if cursor == self.index {
                        break;
                    }
                    let track = &self.tracks[cursor];
                    if seen.insert(track.id.as_str()) {
                        out.push(track);
                    }
                }
            }
        }
        out
    }

    /// 上一首（循环到队尾）。
    pub fn rewind(&mut self) -> Option<&TrackItem> {
        self.bump();
        if self.tracks.is_empty() {
            return None;
        }
        self.index = if self.index == 0 {
            self.tracks.len() - 1
        } else {
            self.index - 1
        };
        self.current()
    }

    pub fn jump(&mut self, index: usize) -> Option<&TrackItem> {
        self.bump();
        if index < self.tracks.len() {
            self.index = index;
        }
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks(n: usize) -> Vec<TrackItem> {
        (0..n)
            .map(|i| TrackItem {
                id: i.to_string(),
                title: format!("t{i}"),
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn queue_navigates_and_wraps() {
        let mut queue = Queue::new(tracks(3));
        assert_eq!(queue.current().unwrap().title, "t0");
        assert_eq!(queue.advance().unwrap().title, "t1");
        assert_eq!(queue.rewind().unwrap().title, "t0");
        assert_eq!(queue.rewind().unwrap().title, "t2");
        assert_eq!(queue.jump(1).unwrap().title, "t1");
    }

    #[test]
    fn insert_next_keeps_current_and_inserts_after_it() {
        let mut queue = Queue::new(tracks(3));
        let inserted = tracks(2);
        assert!(!queue.insert_next(inserted));
        assert_eq!(queue.current().unwrap().title, "t0");
        assert_eq!(
            queue
                .tracks()
                .iter()
                .map(|track| track.title.as_str())
                .collect::<Vec<_>>(),
            ["t0", "t0", "t1", "t1", "t2"]
        );
        assert_eq!(queue.advance().unwrap().title, "t0");
        assert_eq!(queue.advance().unwrap().title, "t1");
    }

    #[test]
    fn insert_next_in_empty_queue_starts_new_queue() {
        let mut queue = Queue::default();
        assert!(queue.insert_next(tracks(2)));
        assert_eq!(queue.current().unwrap().title, "t0");
    }

    #[test]
    fn append_deduplicates_by_track_id() {
        let mut queue = Queue::new(tracks(2));
        let appended = vec![
            tracks(1)[0].clone(),
            TrackItem {
                id: "new".into(),
                title: "new".into(),
                ..Default::default()
            },
        ];
        assert_eq!(queue.append(appended), 1);
        assert_eq!(queue.len(), 3);
    }

    #[test]
    fn repeat_one_stays_on_track() {
        let mut queue = Queue::new(tracks(2));
        queue.mode = PlayMode::RepeatOne;
        assert_eq!(queue.advance().unwrap().title, "t0");
        assert_eq!(PlayMode::Sequential.next(), PlayMode::Shuffle);
        assert_eq!(PlayMode::RepeatOne.next(), PlayMode::Sequential);
    }

    #[test]
    fn shuffle_with_seed_is_reproducible() {
        let first = Queue::shuffle_with_seed(tracks(8), 42);
        let second = Queue::shuffle_with_seed(tracks(8), 42);
        assert_eq!(first, second);
        let mut ids: Vec<_> = first.iter().map(|track| track.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["0", "1", "2", "3", "4", "5", "6", "7"]);
    }

    #[test]
    fn empty_queue_is_safe() {
        let mut queue = Queue::default();
        assert!(queue.is_empty());
        assert!(queue.advance().is_none());
        assert!(queue.rewind().is_none());
        assert!(queue.current().is_none());
    }

    #[test]
    fn peek_ahead_returns_play_order_with_wrap() {
        let mut queue = Queue::new(tracks(4));
        queue.jump(1);
        let ahead: Vec<_> = queue
            .peek_ahead(2)
            .iter()
            .map(|t| t.title.clone())
            .collect();
        assert_eq!(ahead, ["t2", "t3"]);
        // 环绕：队尾接队首，但不包含正在播的 t1
        let ahead: Vec<_> = queue
            .peek_ahead(3)
            .iter()
            .map(|t| t.title.clone())
            .collect();
        assert_eq!(ahead, ["t2", "t3", "t0"]);
        // 要的比剩余多：停在绕回当前曲目前
        let ahead: Vec<_> = queue
            .peek_ahead(10)
            .iter()
            .map(|t| t.title.clone())
            .collect();
        assert_eq!(ahead, ["t2", "t3", "t0"]);
    }

    #[test]
    fn remove_rejects_current_but_shifts_earlier_indices() {
        let mut queue = Queue::new(tracks(3));
        queue.jump(1); // 当前 t1
                       // 当前项不允许移除：它还挂在引擎上，移除会造成状态分叉
        assert!(queue.remove(1).is_none());
        assert_eq!(queue.len(), 3);
        // 移除当前项之前的项：下标左移，仍指向 t1
        assert_eq!(queue.remove(0).unwrap().title, "t0");
        assert_eq!(queue.current().unwrap().title, "t1");
        assert_eq!(queue.index(), 0);
        // 移除之后的项不影响当前项
        assert_eq!(queue.remove(1).unwrap().title, "t2");
        assert_eq!(queue.current().unwrap().title, "t1");
        // 越界拒绝
        assert!(queue.remove(9).is_none());
    }

    #[test]
    fn peek_ahead_handles_edge_cases() {
        // 空队列 / 单曲队列
        assert!(Queue::default().peek_ahead(3).is_empty());
        let single = Queue::new(tracks(1));
        assert!(single.peek_ahead(3).is_empty());
        // 单曲循环：只返回当前曲目
        let mut queue = Queue::new(tracks(3));
        queue.mode = PlayMode::RepeatOne;
        let ahead: Vec<_> = queue
            .peek_ahead(3)
            .iter()
            .map(|t| t.title.clone())
            .collect();
        assert_eq!(ahead, ["t0"]);
        // 队列里有重复 id：不重复返回
        let mut dup = Queue::new(tracks(2));
        dup.append(vec![tracks(3)[2].clone()]);
        // [t0, t1, t2]，无重复；构造重复：insert_next 插入已存在的 t1
        dup.insert_next(vec![tracks(2)[1].clone()]);
        // [t0, t1, t1, t2]
        let ahead: Vec<_> = dup.peek_ahead(5).iter().map(|t| t.title.clone()).collect();
        assert_eq!(ahead, ["t1", "t2"]);
    }
}
