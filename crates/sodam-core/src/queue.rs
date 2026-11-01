//! 播放队列（列表循环 / 单曲循环 / 随机播放）。

use std::collections::HashSet;

use crate::models::TrackItem;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlayMode {
    /// 列表循环：播到末尾回到第一首。
    #[default]
    #[serde(alias = "Sequential")]
    ListLoop,
    /// 单曲循环：停在当前曲目。
    RepeatOne,
    /// 随机播放：先进队后随机取下一首（见 `Queue::history`）。
    Shuffle,
}

impl PlayMode {
    pub fn label(&self) -> &'static str {
        match self {
            PlayMode::ListLoop => "列表循环",
            PlayMode::RepeatOne => "单曲循环",
            PlayMode::Shuffle => "随机播放",
        }
    }

    /// 按官方的轮换顺序切到下一个模式：列表循环 → 单曲循环 → 随机播放。
    pub fn next(self) -> Self {
        match self {
            PlayMode::ListLoop => PlayMode::RepeatOne,
            PlayMode::RepeatOne => PlayMode::Shuffle,
            PlayMode::Shuffle => PlayMode::ListLoop,
        }
    }

    /// 图标名（`ui/icons.rs` 里的 svg 名）。
    pub fn icon(&self) -> &'static str {
        match self {
            PlayMode::ListLoop => "repeat",
            PlayMode::RepeatOne => "repeat-1",
            PlayMode::Shuffle => "shuffle",
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
    /// 随机播放的历史栈：记录走过的下标，`prev`/`rewind` 沿它回退。
    ///
    /// 官方随机播放是「先入队再随机」，而不是把队列洗牌——队列抽屉里
    /// 仍显示真实曲目顺序，只有播放分支是随机的。没有这个栈时
    /// 「上一首」只能退到列表里的前一首，与随机播放的语义不符。
    history: Vec<usize>,
    /// 随机播放的待播顺序（下标）。一轮内不重复，播完再生成新一轮。
    ///
    /// 预先生成而不是每次临时抽取，是为了让队列面板能显示「接下来会播什么」：
    /// 临时抽取时下一首只有播放那一刻才知道，面板只能显示原歌单顺序。
    shuffle_round: Vec<usize>,
    /// 「上一首」退回去之后，下一次「下一首」优先回到被放弃的那首。
    ///
    /// 没有它时「上一首 → 下一首」会在随机模式下跳到别处，原本该播的那首
    /// 也从本轮里消失了。
    redo: Option<usize>,
}

/// 打散时间种子：splitmix64 的收尾混合。
///
/// 直接用纳秒时间戳取模会让低位几乎没有变化——连续调用间隔只有微秒级，
/// `seed % len` 会反复落在同一个值上。混一轮之后低位分布才够散。
fn mix_seed(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

impl Queue {
    pub fn new(tracks: Vec<TrackItem>) -> Self {
        Self {
            revision: 0,
            tracks,
            index: 0,
            mode: PlayMode::default(),
            history: Vec::new(),
            shuffle_round: Vec::new(),
            redo: None,
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

    /// 换一批曲目。播放模式与随机历史一起清空：新队列是一轮新的播放。
    ///
    /// `mode` 不清空——模式是用户的选择，换歌单不该把它重置掉（#12）。
    pub fn replace(&mut self, tracks: Vec<TrackItem>, start: usize) {
        self.bump();
        self.tracks = tracks;
        self.index = start.min(self.tracks.len().saturating_sub(1));
        self.history.clear();
        self.shuffle_round.clear();
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

    /// 下一首。
    ///
    /// * 列表循环：下标 +1，到末尾回到第一首；
    /// * 单曲循环：停在原地（由播放器决定是否重播）；
    /// * 随机播放：把当前下标压入历史，再从「未播放过」的曲目里随机挑一首。
    pub fn advance(&mut self) -> Option<&TrackItem> {
        self.bump();
        if self.tracks.is_empty() {
            return None;
        }
        match self.mode {
            PlayMode::RepeatOne => {}
            PlayMode::ListLoop => {
                self.index = (self.index + 1) % self.tracks.len();
            }
            PlayMode::Shuffle => {
                self.history.push(self.index);
                // 「上一首」之后按「下一首」应回到被放弃的那首，而不是重新随机。
                if let Some(redo) = self.redo.take() {
                    self.index = redo;
                    return self.current();
                }
                if let Some(next) = self.next_shuffle_index() {
                    self.index = next;
                    // 本轮播完就立刻开新一轮：面板的「接下来」不该出现空档，
                    // 否则用户会以为没歌了。清历史保证「上一首」不跨轮回退。
                    if self.shuffle_round.is_empty() {
                        self.history.clear();
                        self.shuffle_round = self.build_shuffle_round();
                    }
                }
            }
        }
        self.current()
    }

    /// 取随机播放的下一个下标，并推进 `shuffle_round`。
    ///
    /// `shuffle_round` 若为空就现生成一轮：除当前曲目外的全部下标随机排列。
    /// 一轮播完后重新生成，因此不会出现「剩下最后一首反复播」。
    fn next_shuffle_index(&mut self) -> Option<usize> {
        let len = self.tracks.len();
        if len <= 1 {
            return None;
        }
        if self.shuffle_round.is_empty() {
            self.shuffle_round = self.build_shuffle_round();
        }
        if self.shuffle_round.is_empty() {
            return None;
        }
        let picked = self.shuffle_round.remove(0);
        Some(picked)
    }

    /// 生成一轮随机顺序（不含当前曲目），并从中移除已取走的元素。
    fn build_shuffle_round(&self) -> Vec<usize> {
        let mut round: Vec<usize> = (0..self.tracks.len())
            .filter(|index| *index != self.index)
            .collect();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15);
        // Fisher-Yates，种子先混一次：纳秒低位连续调用变化太小，直接取模会退化。
        let mut state = mix_seed(seed);
        for i in (1..round.len()).rev() {
            state = mix_seed(state);
            let pick = (state % (i as u64 + 1)) as usize;
            round.swap(i, pick);
        }
        round
    }

    /// 清空随机待播顺序，下次推进时重新生成一轮。
    ///
    /// 切换播放模式后必须调用：否则会沿用上一个模式留下的待播队列，
    /// 面板显示的「接下来」与实际播放不符。
    pub fn reset_shuffle_round(&mut self) {
        self.shuffle_round.clear();
        self.history.clear();
    }

    /// 立刻生成一轮随机待播顺序（随机模式下调用）。
    ///
    /// 切到随机、或整张歌单点「随机播放」入队后调用，让队列面板
    /// 马上显示真实的「接下来」，而不是等下一次切歌才生成。
    pub fn prepare_shuffle_round(&mut self) {
        if self.mode == PlayMode::Shuffle && self.tracks.len() > 1 {
            self.shuffle_round = self.build_shuffle_round();
        }
    }

    /// 随机挑一个起点下标（「随机播放」整张歌单时用）。
    pub fn random_start(len: usize) -> usize {
        if len <= 1 {
            return 0;
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15);
        (mix_seed(seed) % len as u64) as usize
    }

    /// 「接下来」的真实播放顺序（供队列面板显示用）。
    ///
    /// * 列表循环：当前项之后到末尾，再绕回开头；
    /// * 随机播放：`shuffle_round` 里尚未播的部分；
    /// * 单曲循环：空（一直播当前这首）。
    ///
    /// 返回的是下标，调用方用 `tracks()[i]` 取曲目。
    pub fn upcoming(&self) -> Vec<usize> {
        if self.tracks.is_empty() {
            return Vec::new();
        }
        match self.mode {
            PlayMode::RepeatOne => Vec::new(),
            PlayMode::ListLoop => {
                // 不回绕到当前项之前：那部分由抽屉的「已播放」段渲染，
                // 环绕会让同一首歌出现两行。列表循环真正的回绕发生在
                // 播到末尾之后，届时「已播放」会重新变空。
                let len = self.tracks.len();
                (self.index + 1..len).collect()
            }
            PlayMode::Shuffle => self
                .shuffle_round
                .iter()
                .copied()
                .filter(|index| *index < self.tracks.len())
                .collect(),
        }
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
        // 随机轮次和历史栈存的是下标，移除会让后面的项整体左移一位：
        // 不同步的话，轮次会指向错的曲目，末尾还会留下越界下标（被静默跳过，
        // 表现为「少播一首」）。
        self.shift_indices_for_removal(index);
        self.bump();
        Some(removed)
    }

    /// 移除第 `removed` 项后，同步 `shuffle_round` 与 `history` 里的下标。
    fn shift_indices_for_removal(&mut self, removed: usize) {
        let adjust = |value: &mut usize| {
            if *value > removed {
                *value -= 1;
            }
        };
        self.shuffle_round.retain(|value| *value != removed);
        self.shuffle_round.iter_mut().for_each(adjust);
        self.history.retain(|value| *value != removed);
        self.history.iter_mut().for_each(adjust);
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
            let inserted = tracks.len();
            self.tracks.splice(insert_at..insert_at, tracks);
            // 插入点之后的曲目整体右移，轮次与历史里的下标要跟着 +1，
            // 否则它们会指向错位的歌（「下一首播放」在随机模式下会失效）。
            let shift = |value: &mut usize| {
                if *value >= insert_at {
                    *value += inserted;
                }
            };
            self.shuffle_round.iter_mut().for_each(shift);
            self.history.iter_mut().for_each(shift);
            // 「下一首播放」的语义：紧接着播新插入的这批，所以插到轮次最前。
            if self.mode == PlayMode::Shuffle {
                let front: Vec<usize> = (insert_at..insert_at + inserted).collect();
                let mut combined = front;
                combined.append(&mut self.shuffle_round);
                self.shuffle_round = combined;
            }
        }
        self.bump();
        was_empty
    }

    /// 下一首会是谁（不改状态，用于**预取**）。
    ///
    /// * 单曲循环：当前曲目；
    /// * 列表循环：列表里的下一首（到末尾回到第一首）；
    /// * 随机播放：无法预知下一首，退化为「列表里的下一首」，
    ///   只用来预热缓存，不影响实际播放顺序。
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
    /// * 列表循环：从当前曲目之后起环绕取，绕回当前曲目即停，并按 id 去重；
    /// * 单曲循环：只返回当前曲目；
    /// * 随机播放：同样按列表顺序给出候选，仅用于预热缓存——真正的下一首
    ///   由 `advance` 随机决定，预热没命中只是多下一次请求。
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

    /// 上一首。
    ///
    /// * 列表循环：下标 -1，到开头回到队尾；
    /// * 随机播放：沿历史栈回退到真正播过的那首；
    /// * 单曲循环：同样沿历史栈回退（用户点「上一首」是想换歌）。
    pub fn rewind(&mut self) -> Option<&TrackItem> {
        self.bump();
        if self.tracks.is_empty() {
            return None;
        }
        if self.mode == PlayMode::Shuffle {
            if let Some(previous) = self.history.pop() {
                if previous < self.tracks.len() {
                    // 记下被放弃的那首，下一次「下一首」原路返回它。
                    self.redo = Some(self.index);
                    // 它重新回到待播轮次的最前面，面板与「下一首」保持一致。
                    if !self.shuffle_round.contains(&self.index) && self.index != previous {
                        self.shuffle_round.insert(0, self.index);
                    }
                    self.index = previous;
                    return self.current();
                }
            }
        }
        self.index = if self.index == 0 {
            self.tracks.len() - 1
        } else {
            self.index - 1
        };
        self.current()
    }

    /// 跳到指定下标（用户点队列里的某一首）。
    ///
    /// 随机播放时这次跳转也会进历史栈，之后「上一首」能退回原来那首。
    pub fn jump(&mut self, index: usize) -> Option<&TrackItem> {
        self.bump();
        if index < self.tracks.len() {
            if self.mode == PlayMode::Shuffle && index != self.index {
                self.history.push(self.index);
                // 用户点了「接下来」里的某一首：它已经播了，必须从待播轮次里去掉，
                // 否则 `advance` 会把它再播一次。
                self.shuffle_round.retain(|value| *value != index);
            }
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
        assert_eq!(PlayMode::ListLoop.next(), PlayMode::RepeatOne);
        assert_eq!(PlayMode::RepeatOne.next(), PlayMode::Shuffle);
        assert_eq!(PlayMode::Shuffle.next(), PlayMode::ListLoop);
    }

    #[test]
    fn shuffle_never_repeats_before_covering_the_queue() {
        let mut queue = Queue::new(tracks(6));
        queue.mode = PlayMode::Shuffle;
        let mut visited = vec![queue.index()];
        for _ in 0..5 {
            queue.advance();
            visited.push(queue.index());
        }
        // 一轮里每首都该恰好出现一次。
        visited.sort_unstable();
        assert_eq!(visited, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn shuffle_rewind_returns_to_previously_played_track() {
        let mut queue = Queue::new(tracks(6));
        queue.mode = PlayMode::Shuffle;
        let first = queue.index();
        queue.advance();
        let second = queue.index();
        assert_ne!(first, second);
        // 「上一首」沿历史栈退回真正播过的那首。
        assert_eq!(queue.rewind().unwrap().id, first.to_string());
        // 再前进时重新随机，并且不会挑到本轮已经播过的 `first`（历史里还留着它）。
        let third = queue.index();
        queue.advance();
        assert_ne!(queue.index(), first, "本轮不重复已播曲目");
        assert_ne!(queue.index(), third);
    }

    #[test]
    fn shuffle_starts_new_round_after_full_cycle() {
        let mut queue = Queue::new(tracks(3));
        queue.mode = PlayMode::Shuffle;
        // 播完一整轮后应自动开新一轮，不会卡在最后一首、也不会把「接下来」留空。
        for _ in 0..3 {
            queue.advance();
        }
        assert!(queue.current().is_some());
        assert!(
            !queue.upcoming().is_empty(),
            "一轮结束后应立即生成下一轮，面板不能是空的"
        );
        let before = queue.index();
        queue.advance();
        assert_eq!(
            queue.history.len(),
            0,
            "新一轮开始时应清空上一轮的历史，避免跨轮回退"
        );
        assert!(before != queue.index() || queue.len() == 1);
    }

    #[test]
    fn list_loop_wraps_at_the_end() {
        let mut queue = Queue::new(tracks(3));
        queue.mode = PlayMode::ListLoop;
        assert_eq!(queue.advance().unwrap().id, "1");
        assert_eq!(queue.advance().unwrap().id, "2");
        assert_eq!(queue.advance().unwrap().id, "0");
    }

    #[test]
    fn upcoming_matches_actual_play_order_in_shuffle() {
        let mut queue = Queue::new(tracks(6));
        queue.mode = PlayMode::Shuffle;
        // 先推进一次，生成随机轮次。
        queue.advance();
        let shown: Vec<usize> = queue.upcoming();
        let mut actual = Vec::new();
        for _ in 0..shown.len() {
            queue.advance();
            actual.push(queue.index());
        }
        assert_eq!(shown, actual, "面板显示的「接下来」必须等于真实播放顺序");
    }

    #[test]
    fn upcoming_never_shows_empty_mid_playback() {
        let mut queue = Queue::new(tracks(4));
        queue.mode = PlayMode::Shuffle;
        queue.prepare_shuffle_round();
        // 连续推进多轮：任何时候「接下来」都不该是空的（面板会显示成没有歌）。
        for step in 0..20 {
            assert!(
                !queue.upcoming().is_empty(),
                "第 {step} 次推进后面板的「接下来」空了"
            );
            queue.advance();
        }
    }

    #[test]
    fn jumping_to_upcoming_track_removes_it_from_round() {
        let mut queue = Queue::new(tracks(6));
        queue.mode = PlayMode::Shuffle;
        queue.prepare_shuffle_round();
        let target = queue.upcoming()[0];
        queue.jump(target);
        assert!(
            !queue.upcoming().contains(&target),
            "点了「接下来」里的歌之后，它不应再留在待播轮次里（否则会重播）"
        );
    }

    #[test]
    fn rewind_then_advance_returns_to_abandoned_track() {
        let mut queue = Queue::new(tracks(5));
        queue.mode = PlayMode::Shuffle;
        queue.prepare_shuffle_round();
        let first = queue.index();
        queue.advance();
        let second = queue.index();
        assert_ne!(first, second);
        queue.rewind();
        assert_eq!(queue.index(), first, "应退回上一首");
        queue.advance();
        assert_eq!(queue.index(), second, "再按下一首应回到刚放弃的那首");
    }

    #[test]
    fn insert_next_keeps_shuffle_round_aligned() {
        let mut queue = Queue::new(tracks(6));
        queue.mode = PlayMode::Shuffle;
        queue.jump(2);
        queue.prepare_shuffle_round();
        let inserted = vec![TrackItem {
            id: "new".into(),
            title: "new".into(),
            ..Default::default()
        }];
        queue.insert_next(inserted);
        assert_eq!(queue.current().unwrap().id, "2", "当前曲目不变");
        // 新插入的那首应当紧接着播（「下一首播放」语义）。
        let upcoming = queue.upcoming();
        assert_eq!(upcoming[0], 3, "插入项应排在待播轮次最前");
        // 轮次里不能有越界下标。
        assert!(upcoming.iter().all(|index| *index < queue.len()));
        assert_eq!(upcoming.len(), queue.len() - 1);
    }

    #[test]
    fn upcoming_is_empty_for_repeat_one() {
        let mut queue = Queue::new(tracks(3));
        queue.mode = PlayMode::RepeatOne;
        assert!(queue.upcoming().is_empty());
        queue.advance();
        assert_eq!(queue.index(), 0);
    }

    #[test]
    fn upcoming_lists_rest_of_queue_for_list_loop() {
        let mut queue = Queue::new(tracks(3));
        queue.mode = PlayMode::ListLoop;
        queue.jump(1);
        // 不回绕到当前项之前：那部分由抽屉的「已播放」段渲染，重复会出两行。
        assert_eq!(queue.upcoming(), vec![2]);
    }

    #[test]
    fn switching_mode_resets_shuffle_round() {
        let mut queue = Queue::new(tracks(6));
        queue.mode = PlayMode::Shuffle;
        queue.advance();
        assert!(!queue.upcoming().is_empty());
        queue.reset_shuffle_round();
        assert!(queue.upcoming().is_empty(), "重置后要等下一次推进再生成");
    }

    #[test]
    fn random_start_stays_in_range_and_varies() {
        let mut seen = HashSet::new();
        for _ in 0..40 {
            let start = Queue::random_start(20);
            assert!(start < 20);
            seen.insert(start);
        }
        assert!(seen.len() > 3, "起点应当有变化，实测只出现过 {:?}", seen);
        assert_eq!(Queue::random_start(1), 0);
        assert_eq!(Queue::random_start(0), 0);
    }

    #[test]
    fn switching_from_repeat_one_to_shuffle_covers_whole_queue() {
        let mut queue = Queue::new(tracks(10));
        // 模拟用户在单曲循环里听了一段时间（当前项仍是同一首）。
        queue.mode = PlayMode::RepeatOne;
        for _ in 0..5 {
            queue.advance();
        }
        assert_eq!(queue.index(), 0);

        // 点两下切到随机（单曲 → 随机），应当立刻按整个歌单生成轮次。
        queue.mode = queue.mode.next();
        queue.reset_shuffle_round();
        queue.prepare_shuffle_round();

        let mut upcoming = queue.upcoming();
        assert_eq!(
            upcoming.len(),
            9,
            "应覆盖除当前曲目外的全部 9 首，实际 {:?}",
            upcoming
        );
        upcoming.sort_unstable();
        assert_eq!(upcoming, (1..10).collect::<Vec<_>>());
    }

    #[test]
    fn prepare_shuffle_round_fills_upcoming_immediately() {
        let mut queue = Queue::new(tracks(8));
        queue.mode = PlayMode::Shuffle;
        queue.jump(3);
        assert!(queue.upcoming().is_empty(), "尚未生成轮次");
        queue.prepare_shuffle_round();
        let upcoming = queue.upcoming();
        assert_eq!(upcoming.len(), 7, "除当前曲目外的全部曲目");
        assert!(!upcoming.contains(&3), "轮次里不含当前曲目");
    }

    #[test]
    fn shuffle_play_starts_from_random_track() {
        // 「随机播放」整张歌单：起点随机，且大部分情况下下一首不是列表里的下一个。
        let mut starts = HashSet::new();
        let mut sequential_picks = 0;
        let rounds = 40;
        for _ in 0..rounds {
            let mut queue = Queue::new(tracks(12));
            queue.mode = PlayMode::Shuffle;
            let start = Queue::random_start(queue.len());
            queue.jump(start);
            queue.prepare_shuffle_round();
            starts.insert(start);
            if queue.upcoming()[0] == (start + 1) % 12 {
                sequential_picks += 1;
            }
        }
        assert!(starts.len() > 3, "起点应当随机分布");
        // 12 首里恰好轮到顺序下一个的概率约 1/11；40 次里不该频繁出现。
        assert!(
            sequential_picks <= rounds / 4,
            "下一首过多地等于列表顺序的下一个（{sequential_picks}/{rounds}），随机性不足"
        );
    }

    #[test]
    fn remove_keeps_shuffle_round_valid() {
        let mut queue = Queue::new(tracks(8));
        queue.mode = PlayMode::Shuffle;
        queue.prepare_shuffle_round();
        let removed = queue.remove(2).expect("可以移除非当前项");
        assert_eq!(removed.id, "2");

        let upcoming = queue.upcoming();
        // 下标必须都在合法范围内，且不包含被移除后仍指向旧位置的值。
        assert!(
            upcoming.iter().all(|index| *index < queue.len()),
            "轮次里出现越界下标：{:?}（队列长度 {}）",
            upcoming,
            queue.len()
        );
        assert_eq!(upcoming.len(), queue.len() - 1, "剩余曲目应全部在轮次里");
        let mut sorted = upcoming.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), upcoming.len(), "轮次里不该有重复下标");
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
