//! 封面状态：一条 URL 一条记录，缩略图和原图各占一格。
//!
//! 取图本身不在这里：这里只回答「这张问过没有 / 排到哪一步了 / 结果是什么」。
//! 读口子（`path` / `color` / `original_path` / `paths_handle` / `originals_handle` / `lookup`）
//! 是纯内存查询，不碰磁盘也不发请求；下载由调用方泵出去，结果再用
//! [`CoverStore::complete`] / [`CoverStore::complete_original`] 交回来。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// 缩略图下载的并发上限。
pub const MAX_CONCURRENCY: usize = 8;
/// 缩略图排队上限：超过就挤掉最旧的，被挤掉的那张退回可重排状态。
pub const MAX_QUEUE: usize = 400;
/// 原图下载的并发上限：单张解码最坏 64 MiB（16 位图翻倍），
/// 用限流防快速切歌时瞬时内存失控。
pub const MAX_ORIGINAL_CONCURRENCY: usize = 4;
/// 原图排队上限：超过就挤掉最旧的，被挤掉的那张退回可重排状态
/// （最新切进来的歌最有用）。
pub const MAX_ORIGINAL_QUEUE: usize = 32;
/// 同一张图失败几次后放弃。只记内存，重启后再试两次。
pub const MAX_FAILURES: u8 = 2;

/// 封面主色。
pub type CoverRgb = (u8, u8, u8);

/// 一轮封面队列统计：新排进几张、上次读取以来挤掉几张。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CoverWant {
    pub queued: usize,
    pub dropped: usize,
}

/// 一次下载的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverOutcome {
    /// 拿到路径（以及解析出的主色）。
    Ready {
        path: PathBuf,
        color: Option<CoverRgb>,
    },
    /// 解析出了路径，但文件在发布前已经被淘汰删掉。
    Vanished,
    /// 没拿到可用路径，按失败记一次。
    Failed,
}

/// 已完成路径和失败终态的只读句柄：渲染闭包克隆它查图，不用整表复制。
#[derive(Debug, Clone, Default)]
pub struct CoverPaths {
    inner: Arc<RwLock<CoverPathsInner>>,
}

#[derive(Debug, Default)]
struct CoverPathsInner {
    paths: HashMap<String, PathBuf>,
    failed: HashSet<String>,
}

/// 一次读取的结果：已有路径，还是需要渲染方继续请求。
#[derive(Debug)]
pub struct CoverLookup {
    pub path: Option<PathBuf>,
    pub should_request: bool,
}

impl CoverPaths {
    /// 这张图有没有已完成路径（只查内存，不克隆）。
    pub fn contains(&self, url: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .paths
            .contains_key(url.trim())
    }

    /// 查一张图的本地路径（只查内存）。
    pub fn get(&self, url: &str) -> Option<PathBuf> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .paths
            .get(url.trim())
            .cloned()
    }

    /// 一次快照同时回答“有没有路径”和“还要不要请求”，避免渲染方分两次读。
    pub fn lookup(&self, url: &str) -> CoverLookup {
        let url = url.trim();
        let inner = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(path) = inner.paths.get(url) {
            return CoverLookup {
                path: Some(path.clone()),
                should_request: false,
            };
        }
        CoverLookup {
            path: None,
            should_request: !url.is_empty() && !inner.failed.contains(url),
        }
    }

    fn insert(&self, url: &str, path: PathBuf) {
        let url = url.trim();
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.paths.insert(url.to_string(), path);
        inner.failed.remove(url);
    }

    fn remove(&self, url: &str) {
        let url = url.trim();
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.paths.remove(url);
        inner.failed.remove(url);
    }

    fn mark_failed(&self, url: &str) {
        let url = url.trim();
        if !url.is_empty() {
            self.inner
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .failed
                .insert(url.to_string());
        }
    }

    fn clear(&self) {
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.paths.clear();
        inner.failed.clear();
    }

    /// 按被删的文件路径反查 URL；只对命中删除列表的条目做磁盘检查。
    ///
    /// 读锁里只挑出命中的几对，`stat` 放到放锁之后，别让写路径等着磁盘。
    pub fn urls_for_removed_paths<F>(
        &self,
        removed: &[PathBuf],
        mut is_still_missing: F,
    ) -> HashSet<String>
    where
        F: FnMut(&Path) -> bool,
    {
        let removed: HashSet<&Path> = removed.iter().map(PathBuf::as_path).collect();
        let candidates: Vec<(String, PathBuf)> = {
            let inner = self
                .inner
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner
                .paths
                .iter()
                .filter(|(_, path)| removed.contains(path.as_path()))
                .map(|(url, path)| (url.clone(), path.clone()))
                .collect()
        };
        candidates
            .into_iter()
            .filter(|(_, path)| is_still_missing(path.as_path()))
            .map(|(url, _)| url)
            .collect()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    /// 没在排队，也没有可用结果；`want` 可以把它排进去。
    #[default]
    Idle,
    Queued,
    Inflight,
    /// 已有可用路径，渲染直接读。
    Ready,
    /// 失败次数到顶，本会话不再自动重试。
    Failed,
}

#[derive(Debug, Default)]
struct Slot {
    state: State,
    failures: u8,
}

/// 一条 URL 的账：缩略图和原图各一格。
#[derive(Debug, Default)]
struct Record {
    thumbnail: Slot,
    original: Slot,
}

impl Record {
    fn is_empty(&self) -> bool {
        self.thumbnail.state == State::Idle
            && self.original.state == State::Idle
            && self.thumbnail.failures == 0
            && self.original.failures == 0
    }
}

fn thumbnail_slot(record: &mut Record) -> &mut Slot {
    &mut record.thumbnail
}

fn original_slot(record: &mut Record) -> &mut Slot {
    &mut record.original
}

type SelectSlot = fn(&mut Record) -> &mut Slot;

/// 两格都回到默认状态时把记录删掉，别让淘汰 / 切歌每轮留下空记录。
fn discard_empty_record(records: &mut HashMap<String, Record>, url: &str) {
    if records.get(url).is_some_and(Record::is_empty) {
        records.remove(url);
    }
}

/// 一条腿的排队与在飞记账：队列、在飞集合、作废标记。
///
/// 缩略图和原图各有一条，差别只在队列 / 并发上限和完成后的落账；
/// 原图额外多一条「清缓存时在飞的那张，落完还缺就补排」。
#[derive(Debug)]
struct Lane {
    queue: VecDeque<String>,
    inflight: HashSet<String>,
    /// 在飞期间被 `forget` 的 URL：这次结果作废，结束后退回 `Idle`。
    abandoned: HashSet<String>,
    /// 作废期间又有人在等这张图，或清缓存时当前曲目的原图还在下载：
    /// 结果落完要立刻补排，否则重试按钮 / 歌词页大图要等下一次请求才生效。
    /// 切歌时 `retain_only` 会把非当前曲目的需求裁掉。
    requeue_after_release: HashSet<String>,
    /// 因队列满挤掉、尚未取走的张数。
    queue_evictions: usize,
    /// 原图挤掉数目前没有消费方；只有缩略图需要交给 `cover_queue`。
    track_queue_evictions: bool,
    max_queue: usize,
    max_inflight: usize,
}

impl Lane {
    fn new(max_queue: usize, max_inflight: usize, track_queue_evictions: bool) -> Self {
        Self {
            queue: VecDeque::new(),
            inflight: HashSet::new(),
            abandoned: HashSet::new(),
            requeue_after_release: HashSet::new(),
            queue_evictions: 0,
            track_queue_evictions,
            max_queue,
            max_inflight,
        }
    }

    fn for_thumbnails() -> Self {
        Self::new(MAX_QUEUE, MAX_CONCURRENCY, true)
    }

    fn for_originals() -> Self {
        Self::new(MAX_ORIGINAL_QUEUE, MAX_ORIGINAL_CONCURRENCY, false)
    }

    /// 单张入队。返回有没有真排进去；启用计数的 Lane 自己累计挤掉数。
    ///
    /// 在飞但已被 `forget` 时，把这次需求记下来，等结果回来立刻重排。
    fn enqueue(
        &mut self,
        url: &str,
        ready: &CoverPaths,
        records: &mut HashMap<String, Record>,
        select: SelectSlot,
    ) -> bool {
        if url.is_empty() || ready.contains(url) {
            return false;
        }
        let state = records.get_mut(url).map(|record| select(record).state);
        if !matches!(state, None | Some(State::Idle)) {
            if state == Some(State::Inflight) && self.abandoned.contains(url) {
                self.requeue_after_release.insert(url.to_string());
            }
            return false;
        }
        let evicted = if self.queue.len() >= self.max_queue {
            self.evict_oldest(records, select)
        } else {
            false
        };
        if self.track_queue_evictions {
            self.queue_evictions += usize::from(evicted);
        }
        let record = records.entry(url.to_string()).or_default();
        select(record).state = State::Queued;
        self.queue.push_back(url.to_string());
        true
    }

    fn take_dropped(&mut self) -> usize {
        std::mem::take(&mut self.queue_evictions)
    }

    /// 泵的上半截：把排队项推进在飞，最多 `max_inflight` 个。
    fn drain(&mut self, records: &mut HashMap<String, Record>, select: SelectSlot) -> Vec<String> {
        let mut started = Vec::new();
        while self.inflight.len() < self.max_inflight {
            let Some(url) = self.queue.pop_front() else {
                break;
            };
            let record = records.entry(url.clone()).or_default();
            let slot = select(record);
            if slot.state != State::Queued {
                continue;
            }
            slot.state = State::Inflight;
            self.inflight.insert(url.clone());
            started.push(url);
        }
        started
    }

    /// 泵的下半截的公共记账：出在飞、处理作废。
    ///
    /// 返回 `true` 表示这次结果作废，调用方不用再发布。
    fn finish(
        &mut self,
        url: &str,
        ready: &CoverPaths,
        records: &mut HashMap<String, Record>,
        select: SelectSlot,
    ) -> bool {
        self.inflight.remove(url);
        if self.abandoned.remove(url) {
            // 下载中被 forget：这次拿回来的东西不要，退回可重排。
            // 不要动已发布路径：这条腿可能是另一格兜的底，文件还在。
            if let Some(record) = records.get_mut(url) {
                let slot = select(record);
                slot.state = State::Idle;
                slot.failures = 0;
            }
            // 作废期间又有人要这张图（点重试、滚动回来）：现在补排一次。
            self.requeue_if_demanded(url, ready, records, select);
            discard_empty_record(records, url);
            return true;
        }
        false
    }

    /// 有人在这张图在飞期间留了需求：结果落完立刻补排。
    fn requeue_if_demanded(
        &mut self,
        url: &str,
        ready: &CoverPaths,
        records: &mut HashMap<String, Record>,
        select: SelectSlot,
    ) {
        if self.requeue_after_release.remove(url) {
            self.enqueue(url, ready, records, select);
        }
    }

    /// 作废一张：清队列位置、记号、失败次数；在飞的那张等结果回来再放行。
    ///
    /// 在飞不立刻放行——否则同一张会同时下两遍。
    fn forget(&mut self, url: &str, records: &mut HashMap<String, Record>, select: SelectSlot) {
        self.queue.retain(|queued| queued.as_str() != url);
        // 这次作废之后，旧的「结果回来再补排」需求也作废；之后再 want 会重新记。
        self.requeue_after_release.remove(url);
        let inflight = records
            .get_mut(url)
            .is_some_and(|record| select(record).state == State::Inflight);
        if inflight {
            self.abandoned.insert(url.to_string());
            if let Some(record) = records.get_mut(url) {
                select(record).failures = 0;
            }
        } else if let Some(record) = records.get_mut(url) {
            *select(record) = Slot::default();
        }
    }

    /// 切歌时只留一张在排队：别的排队项退回可重排，不删失败次数。
    ///
    /// 已经记下的「在飞结果回来要补排」也一起丢掉，否则切走的歌下载完还会再排一次。
    fn retain_only(
        &mut self,
        keep: &str,
        records: &mut HashMap<String, Record>,
        select: SelectSlot,
    ) {
        self.requeue_after_release.retain(|url| url == keep);
        let mut dropped = Vec::new();
        self.queue.retain(|queued| {
            if queued.as_str() == keep {
                true
            } else {
                dropped.push(queued.clone());
                false
            }
        });
        for url in dropped {
            if let Some(record) = records.get_mut(&url) {
                let slot = select(record);
                if slot.state == State::Queued {
                    slot.state = State::Idle;
                }
            }
            discard_empty_record(records, &url);
        }
    }

    /// 挤掉最旧的一条排队项；真挤掉了返回 `true`。
    fn evict_oldest(&mut self, records: &mut HashMap<String, Record>, select: SelectSlot) -> bool {
        while let Some(url) = self.queue.pop_front() {
            let evicted = records.get_mut(&url).is_some_and(|record| {
                let slot = select(record);
                if slot.state == State::Queued {
                    slot.state = State::Idle;
                    true
                } else {
                    false
                }
            });
            if evicted {
                discard_empty_record(records, &url);
                return true;
            }
        }
        false
    }
}

/// 缩略图和原图的唯一账本。
#[derive(Debug)]
pub struct CoverStore {
    records: HashMap<String, Record>,
    thumbnails: Lane,
    originals: Lane,
    /// 渲染闭包够不到 `Root`，用它拿一个只读句柄。
    thumbnail_paths: CoverPaths,
    original_paths: CoverPaths,
    colors: HashMap<String, CoverRgb>,
}

impl Default for CoverStore {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
            thumbnails: Lane::for_thumbnails(),
            originals: Lane::for_originals(),
            thumbnail_paths: CoverPaths::default(),
            original_paths: CoverPaths::default(),
            colors: HashMap::new(),
        }
    }
}

impl CoverStore {
    /// 要这几张缩略图；返回这次真正新排进队列的张数。
    ///
    /// 同一 URL 在排队、在飞、已完成或已放弃时都不会重复入队。
    pub fn want(&mut self, urls: &[String]) -> usize {
        let mut queued = 0;
        for raw in urls {
            let url = raw.trim();
            if self.thumbnails.enqueue(
                url,
                &self.thumbnail_paths,
                &mut self.records,
                thumbnail_slot,
            ) {
                queued += 1;
            }
        }
        queued
    }

    /// 泵的上半截：缩略图最多 [`MAX_CONCURRENCY`] 个在飞。
    pub fn drain(&mut self) -> Vec<String> {
        self.thumbnails.drain(&mut self.records, thumbnail_slot)
    }

    /// 泵的下半截：收一次缩略图结果。
    pub fn complete(&mut self, url: &str, outcome: CoverOutcome) {
        let url = url.trim();
        if url.is_empty() {
            return;
        }
        if self.thumbnails.finish(
            url,
            &self.thumbnail_paths,
            &mut self.records,
            thumbnail_slot,
        ) {
            return;
        }
        match outcome {
            CoverOutcome::Ready { path, color } => {
                let record = self.records.entry(url.to_string()).or_default();
                record.thumbnail.state = State::Ready;
                record.thumbnail.failures = 0;
                self.thumbnail_paths.insert(url, path);
                match color {
                    Some(color) => {
                        self.colors.insert(url.to_string(), color);
                    }
                    None => {
                        self.colors.remove(url);
                    }
                }
            }
            CoverOutcome::Vanished => {
                // 文件在下载落盘和回调之间被删掉，也算一次尝试：渲染每帧 want
                // 会立刻重排，两次后先放弃，避免同一张反复走完整 HTTP 下载。
                let record = self.records.entry(url.to_string()).or_default();
                record.thumbnail.failures = record.thumbnail.failures.saturating_add(1);
                record.thumbnail.state = if record.thumbnail.failures >= MAX_FAILURES {
                    State::Failed
                } else {
                    State::Idle
                };
                // 不抹已发布路径：原图先回来时可能已经兜过底，文件还在。
            }
            CoverOutcome::Failed => {
                let record = self.records.entry(url.to_string()).or_default();
                record.thumbnail.failures = record.thumbnail.failures.saturating_add(1);
                record.thumbnail.state = if record.thumbnail.failures >= MAX_FAILURES {
                    State::Failed
                } else {
                    State::Idle
                };
            }
        }
        if self
            .records
            .get(url)
            .is_some_and(|record| record.thumbnail.state == State::Failed)
        {
            self.thumbnail_paths.mark_failed(url);
        }
    }

    /// 要一张原图；返回这次是否新排进队列。
    pub fn want_original(&mut self, url: &str) -> bool {
        self.originals.enqueue(
            url.trim(),
            &self.original_paths,
            &mut self.records,
            original_slot,
        )
    }

    /// 泵的上半截：原图最多 [`MAX_ORIGINAL_CONCURRENCY`] 个在飞。
    pub fn drain_original(&mut self) -> Vec<String> {
        self.originals.drain(&mut self.records, original_slot)
    }

    /// 泵的下半截：收一次原图结果。
    pub fn complete_original(&mut self, url: &str, outcome: CoverOutcome) {
        let url = url.trim();
        if url.is_empty() {
            return;
        }
        if self
            .originals
            .finish(url, &self.original_paths, &mut self.records, original_slot)
        {
            return;
        }
        match outcome {
            CoverOutcome::Ready { path, .. } => {
                let record = self.records.entry(url.to_string()).or_default();
                record.original.state = State::Ready;
                record.original.failures = 0;
                self.original_paths.insert(url, path);
            }
            CoverOutcome::Vanished => {
                let record = self.records.entry(url.to_string()).or_default();
                // 文件没了也算一次尝试：每次重下都撞上「解析出路径但文件已被删」
                // 时不能无限重排，两次后先放弃，等 `forget` / 清缓存再试。
                record.original.failures = record.original.failures.saturating_add(1);
                record.original.state = if record.original.failures >= MAX_FAILURES {
                    State::Failed
                } else {
                    State::Idle
                };
                // 死路径不能留，否则下一次查询会当成命中。
                self.original_paths.remove(url);
            }
            CoverOutcome::Failed => {
                let record = self.records.entry(url.to_string()).or_default();
                record.original.failures = record.original.failures.saturating_add(1);
                record.original.state = if record.original.failures >= MAX_FAILURES {
                    State::Failed
                } else {
                    State::Idle
                };
            }
        }
        if self
            .records
            .get(url)
            .is_some_and(|record| record.original.state == State::Failed)
        {
            self.original_paths.mark_failed(url);
        }
        // 清缓存时当前曲目的原图还在下载：这次按文件在不在落账，落完还缺就补排。
        self.originals.requeue_if_demanded(
            url,
            &self.original_paths,
            &mut self.records,
            original_slot,
        );
    }

    /// 清缓存后当前曲目的原图还在下载：这次结果照常落账，落账后还缺再补排。
    ///
    /// 不能直接 `forget`：那会把这次已经写完、只是晚一步回来的文件一起丢掉，
    /// 白白多下一份（#250 的「在飞保留、完成时按文件在不在落账」）。
    pub fn retry_original_after_release(&mut self, url: &str) {
        let url = url.trim();
        if self
            .records
            .get(url)
            .is_some_and(|record| record.original.state == State::Inflight)
        {
            self.originals.requeue_after_release.insert(url.to_string());
        }
    }

    /// 作废这些 URL 的两格：清路径、颜色、记号、失败次数；不碰磁盘。
    ///
    /// 在飞的那张不会立刻放行——等这次结果回来再退回可重排状态。
    pub fn forget<'a>(&mut self, urls: impl IntoIterator<Item = &'a str>) {
        for raw in urls {
            let url = raw.trim();
            self.forget_thumbnail_url(url);
            self.forget_original_url(url);
            discard_empty_record(&mut self.records, url);
        }
    }

    /// 只作废缩略图那格；原图路径和排队原样保留。
    ///
    /// 淘汰按文件路径精确反查，只删了缩略图就不该连原图一起清。
    pub fn forget_thumbnail<'a>(&mut self, urls: impl IntoIterator<Item = &'a str>) {
        for raw in urls {
            let url = raw.trim();
            self.forget_thumbnail_url(url);
            discard_empty_record(&mut self.records, url);
        }
    }

    /// 只作废原图那格；缩略图路径和排队原样保留。
    pub fn forget_original<'a>(&mut self, urls: impl IntoIterator<Item = &'a str>) {
        for raw in urls {
            let url = raw.trim();
            self.forget_original_url(url);
            discard_empty_record(&mut self.records, url);
        }
    }

    fn forget_thumbnail_url(&mut self, url: &str) {
        if url.is_empty() {
            return;
        }
        self.thumbnails
            .forget(url, &mut self.records, thumbnail_slot);
        self.thumbnail_paths.remove(url);
        self.colors.remove(url);
    }

    fn forget_original_url(&mut self, url: &str) {
        if url.is_empty() {
            return;
        }
        self.originals.forget(url, &mut self.records, original_slot);
        self.original_paths.remove(url);
    }

    /// 切歌时原图排队只留当前曲目；被剔掉的退回可重排，下次播放会重新请求，
    /// 已经记下的「在飞结果回来要补排」也一起丢掉。
    pub fn retain_original_only(&mut self, url: &str) {
        self.originals
            .retain_only(url.trim(), &mut self.records, original_slot);
    }

    /// 清缓存后作废「已完成」的结果：两格里的 `Ready` / `Failed` 都退回 `Idle`，
    /// 所有格的失败次数清零。
    ///
    /// 排队和在飞状态原样保留：丢掉在飞状态只会让同一张图再排一次，
    /// 个别情况下并发下两遍。
    pub fn clear_completed(&mut self) {
        for record in self.records.values_mut() {
            for slot in [&mut record.thumbnail, &mut record.original] {
                if matches!(slot.state, State::Ready | State::Failed) {
                    slot.state = State::Idle;
                }
                // 失败次数一律清零：清缓存后从两次新机会开始。
                slot.failures = 0;
            }
        }
        self.thumbnail_paths.clear();
        self.original_paths.clear();
        self.colors.clear();
        self.records.retain(|_, record| !record.is_empty());
    }

    /// 原图先下回来时给列表兜个底：只发布缩略图路径，不动在飞 / 排队记账。
    pub fn adopt_thumbnail(&mut self, url: &str, path: PathBuf) {
        let url = url.trim();
        if url.is_empty() || self.thumbnail_paths.contains(url) {
            return;
        }
        self.thumbnail_paths.insert(url, path);
        if let Some(record) = self.records.get_mut(url) {
            if matches!(
                record.thumbnail.state,
                State::Idle | State::Ready | State::Failed
            ) {
                record.thumbnail.state = State::Ready;
                record.thumbnail.failures = 0;
            }
        }
    }

    /// 缩略图本地路径：只查内存。
    pub fn path(&self, url: &str) -> Option<PathBuf> {
        self.thumbnail_paths.get(url)
    }

    /// 原图本地路径：只查内存。
    pub fn original_path(&self, url: &str) -> Option<PathBuf> {
        self.original_paths.get(url)
    }

    /// 氛围色：只查内存。
    pub fn color(&self, url: &str) -> Option<CoverRgb> {
        self.colors.get(url.trim()).copied()
    }

    /// 渲染闭包用的缩略图只读句柄；克隆的是 `Arc`，不复制表。
    pub fn paths_handle(&self) -> CoverPaths {
        self.thumbnail_paths.clone()
    }

    /// 渲染闭包用的原图只读句柄；克隆的是 `Arc`，不复制表。
    pub fn originals_handle(&self) -> CoverPaths {
        self.original_paths.clone()
    }

    /// 取走上次读取以来因队列满挤掉的张数，包括补排路径。
    pub fn take_dropped(&mut self) -> usize {
        self.thumbnails.take_dropped()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CoverOutcome, CoverPaths, CoverStore, MAX_CONCURRENCY, MAX_ORIGINAL_CONCURRENCY,
        MAX_ORIGINAL_QUEUE, MAX_QUEUE,
    };
    use std::path::PathBuf;

    fn urls(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    fn ready(path: &str) -> CoverOutcome {
        CoverOutcome::Ready {
            path: PathBuf::from(path),
            color: Some((1, 2, 3)),
        }
    }

    fn original_ready(path: &str) -> CoverOutcome {
        CoverOutcome::Ready {
            path: PathBuf::from(path),
            color: None,
        }
    }

    /// 渲染每帧都会重新要图；去重必须认 URL，不能认请求次数。
    #[test]
    fn want_same_url_ten_times_fetches_once() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        assert_eq!(store.want(&url), 1);
        for _ in 0..9 {
            assert_eq!(store.want(&url), 0);
        }
        assert_eq!(store.drain(), url, "只该泵出一张");
        assert!(store.drain().is_empty(), "在飞时不能重复泵出");
        store.complete("cover-a", ready("/tmp/a.img"));
        assert_eq!(store.want(&url), 0, "已完成也不能重复入队");
    }

    /// 排队 / 在飞 / 已完成三种状态下都不重复入队。
    #[test]
    fn want_skips_queued_inflight_and_ready() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        assert_eq!(store.want(&url), 1, "排队");
        assert_eq!(store.want(&url), 0);
        assert_eq!(store.drain(), url, "在飞");
        assert_eq!(store.want(&url), 0);
        store.complete("cover-a", ready("/tmp/a.img"));
        assert_eq!(store.want(&url), 0, "已完成");
        assert!(store.path("cover-a").is_some());
    }

    /// 下载失败两次才放弃；放弃后 forget 能重新下。
    #[test]
    fn failed_twice_gives_up_until_forgotten() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        let paths = store.paths_handle();

        assert_eq!(store.want(&url), 1);
        assert_eq!(store.drain(), url);
        store.complete("cover-a", CoverOutcome::Failed);
        assert!(
            paths.lookup("cover-a").should_request,
            "失败一次还要继续请求"
        );
        assert_eq!(store.want(&url), 1, "失败一次还能再试");

        assert_eq!(store.drain(), url);
        store.complete("cover-a", CoverOutcome::Failed);
        assert!(
            !paths.lookup("cover-a").should_request,
            "失败两次后停止自动请求"
        );
        assert_eq!(store.want(&url), 0, "失败两次后本会话不再自动重试");
        assert!(store.drain().is_empty());

        store.forget(url.iter().map(String::as_str));
        assert!(paths.lookup("cover-a").should_request, "作废后恢复请求");
        assert_eq!(store.want(&url), 1, "清掉失败记录后能重新下");
    }

    /// 清缓存后失败的那张也能重新下。
    #[test]
    fn clear_completed_revives_failed_url() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        let paths = store.paths_handle();
        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Failed);
        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Failed);
        assert!(!paths.lookup("cover-a").should_request);
        assert_eq!(store.want(&url), 0);

        store.clear_completed();
        assert!(paths.lookup("cover-a").should_request, "清缓存后恢复请求");
        assert_eq!(store.want(&url), 1);
    }

    #[test]
    fn clear_completed_revives_vanished_url() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        let paths = store.paths_handle();
        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Vanished);
        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Vanished);
        assert!(!paths.lookup("cover-a").should_request);

        store.clear_completed();
        assert!(paths.lookup("cover-a").should_request, "清缓存后恢复请求");
        assert_eq!(store.want(&url), 1);
    }

    #[test]
    fn clear_completed_reclaims_ready_and_failed_records() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);

        store.want(&url);
        store.drain();
        store.complete("cover-a", ready("/tmp/a.img"));
        assert_eq!(store.records.len(), 1);
        store.clear_completed();
        assert!(store.records.is_empty(), "已完成的空记录要回收");

        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Failed);
        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Failed);
        assert_eq!(store.records.len(), 1);
        store.clear_completed();
        assert!(store.records.is_empty(), "失败终态清缓存后也回收");
    }

    #[test]
    fn clear_completed_keeps_queued_and_inflight_records() {
        let mut store = CoverStore::default();
        let many: Vec<String> = (0..MAX_CONCURRENCY + 1)
            .map(|index| format!("cover-{index}"))
            .collect();
        store.want(&many);
        assert_eq!(store.drain().len(), MAX_CONCURRENCY);
        assert_eq!(store.records.len(), many.len());

        store.clear_completed();
        assert_eq!(store.records.len(), many.len(), "排队和在飞记录要保留");
        assert_eq!(store.want(std::slice::from_ref(&many[0])), 0);
        assert_eq!(store.want(std::slice::from_ref(&many[MAX_CONCURRENCY])), 0);
    }

    #[test]
    fn clear_completed_keeps_mixed_slots_until_both_are_empty() {
        let mut store = CoverStore::default();
        let url = "cover-a";
        store.want(&urls(&[url]));
        store.drain();
        store.complete(url, ready("/tmp/thumb.img"));
        assert!(store.want_original(url));
        store.drain_original();

        store.clear_completed();
        assert_eq!(store.records.len(), 1, "原图还在飞，记录不能删");
        assert!(!store.want_original(url), "在飞原图仍不能重复入队");

        store.complete_original(url, CoverOutcome::Failed);
        assert_eq!(store.records.len(), 1, "还有一次失败次数，记录不能删");
        store.clear_completed();
        assert!(store.records.is_empty(), "两格都空了才回收");
    }

    /// 队列满时先撤回最旧项的去重状态，否则被挤掉的图再也排不回来。
    #[test]
    fn full_queue_evicts_oldest_and_allows_requeue() {
        let mut store = CoverStore::default();
        let first: Vec<String> = (0..MAX_QUEUE)
            .map(|index| format!("cover-{index}"))
            .collect();
        assert_eq!(store.want(&first), MAX_QUEUE);
        assert_eq!(store.take_dropped(), 0);

        let extra = urls(&["cover-extra"]);
        assert_eq!(store.want(&extra), 1, "满了也要收新图");
        assert_eq!(store.take_dropped(), 1, "挤掉了最旧的一张");

        let oldest = urls(&["cover-0"]);
        assert_eq!(store.want(&oldest), 1, "被挤掉的那张能重新排队");
        assert_eq!(store.take_dropped(), 1);
        assert_eq!(store.want(&oldest), 0, "排进去之后不重复入队");
    }

    /// 清缓存只作废已完成的结果；在飞原样保留，已完成的放行重排。
    #[test]
    fn clear_completed_keeps_inflight_and_revives_done() {
        let mut store = CoverStore::default();
        let all = urls(&["downloading", "done"]);
        assert_eq!(store.want(&all), 2);
        assert_eq!(store.drain().len(), 2, "两张都进了在飞");
        store.complete("done", ready("/tmp/done.img"));
        assert!(store.path("done").is_some());

        store.clear_completed();
        assert!(store.path("done").is_none(), "已完成的结果要作废");
        assert_eq!(store.want(&urls(&["downloading"])), 0, "在飞不能被再次入队");
        assert_eq!(
            store.want(&urls(&["done"])),
            1,
            "已完成的放行，清缓存后能重新请求"
        );
        assert_eq!(store.drain(), urls(&["done"]), "只泵出重新排队的那张");

        // 磁盘被清空的在飞封面：这次下载结束时退回可重排，否则一直占位图。
        store.complete("downloading", CoverOutcome::Vanished);
        assert_eq!(store.want(&urls(&["downloading"])), 1);
    }

    /// 清缓存不能作废还在排队、没轮到泵的 URL。
    #[test]
    fn clear_completed_keeps_queued_that_never_started() {
        let mut store = CoverStore::default();
        let many: Vec<String> = (0..MAX_CONCURRENCY + 2)
            .map(|index| format!("cover-{index}"))
            .collect();
        assert_eq!(store.want(&many), MAX_CONCURRENCY + 2);

        let started = store.drain();
        assert_eq!(started.len(), MAX_CONCURRENCY);
        let queued_tail = many[MAX_CONCURRENCY].clone();

        store.clear_completed();
        assert_eq!(
            store.want(std::slice::from_ref(&queued_tail)),
            0,
            "排队中的不能被清缓存作废"
        );

        // 让出一个在飞位，排队项要照常泵出。
        store.complete(&started[0], ready("/tmp/a.img"));
        assert_eq!(store.drain(), vec![queued_tail]);
    }

    /// 在飞时 forget 不能立刻放行，否则同一张会同时下两遍。
    #[test]
    fn forget_keeps_inflight_until_completion() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        store.want(&url);
        assert_eq!(store.drain(), url);

        store.forget(url.iter().map(String::as_str));
        store.complete("cover-a", ready("/tmp/a.img"));
        assert!(store.path("cover-a").is_none(), "这次结果要作废");
        assert!(store.drain().is_empty(), "没人再要，就不要自己重排");
        assert_eq!(store.want(&url), 1, "结束后要能重新排队");
    }

    /// 在飞被 forget 之后又有人要（点重试）：结果回来要立刻补排。
    #[test]
    fn demand_during_abandoned_inflight_requeues_after_completion() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        store.want(&url);
        assert_eq!(store.drain(), url);
        store.forget(["cover-a"]);

        assert_eq!(store.want(&url), 0, "在飞期间不能重复下");
        store.complete("cover-a", CoverOutcome::Failed);
        assert_eq!(store.drain(), url, "作废结果回来后要把这次需求补上");
        assert_eq!(store.want(&url), 0, "补排之后不重复入队");
    }

    #[test]
    fn deferred_requeue_eviction_survives_next_want() {
        let mut store = CoverStore::default();
        let first: Vec<String> = (0..MAX_QUEUE)
            .map(|index| format!("cover-{index}"))
            .collect();
        assert_eq!(store.want(&first), MAX_QUEUE);

        let inflight = store.drain();
        assert_eq!(inflight.len(), MAX_CONCURRENCY);
        let fillers: Vec<String> = (MAX_QUEUE..MAX_QUEUE + MAX_CONCURRENCY)
            .map(|index| format!("cover-{index}"))
            .collect();
        assert_eq!(store.want(&fillers), MAX_CONCURRENCY);

        let released = inflight[0].clone();
        store.forget([released.as_str()]);
        assert_eq!(store.want(std::slice::from_ref(&released)), 0);
        store.complete(&released, CoverOutcome::Failed);

        assert_eq!(store.want(std::slice::from_ref(&released)), 0);
        assert_eq!(store.take_dropped(), 1, "补排挤掉的也要记到日志里");
    }

    /// 第二次作废要把第一次作废后留的补排需求一起清掉；之后再要仍能重新记。
    #[test]
    fn repeated_forget_cancels_deferred_original_demand() {
        let mut store = CoverStore::default();
        assert!(store.want_original("original-a"));
        assert_eq!(store.drain_original(), vec!["original-a".to_string()]);

        store.forget(["original-a"]);
        assert!(!store.want_original("original-a"), "在飞期间不重下");
        store.forget(["original-a"]);

        store.complete_original("original-a", original_ready("/tmp/full.img"));
        assert!(
            store.original_path("original-a").is_none(),
            "作废结果不发布"
        );
        assert!(
            store.drain_original().is_empty(),
            "第二次作废把补排需求也清了"
        );
        assert!(store.want_original("original-a"), "之后再要还能排");
    }

    /// 切歌 / 队列满挤掉的排队项不能留下空记录。
    #[test]
    fn dropped_queued_entries_do_not_leave_empty_records() {
        let mut store = CoverStore::default();
        let many: Vec<String> = (0..MAX_ORIGINAL_CONCURRENCY + 1)
            .map(|index| format!("original-{index}"))
            .collect();
        for url in &many {
            assert!(store.want_original(url));
        }
        assert_eq!(store.drain_original().len(), MAX_ORIGINAL_CONCURRENCY);
        assert_eq!(store.records.len(), many.len());

        store.retain_original_only("other-current");
        assert_eq!(
            store.records.len(),
            MAX_ORIGINAL_CONCURRENCY,
            "被切歌放掉的排队项不留空记录"
        );

        let mut store = CoverStore::default();
        let many: Vec<String> = (0..MAX_ORIGINAL_QUEUE + 1)
            .map(|index| format!("original-{index}"))
            .collect();
        for url in &many {
            assert!(store.want_original(url));
        }
        assert_eq!(
            store.records.len(),
            MAX_ORIGINAL_QUEUE,
            "队列满挤掉的最旧项不留空记录"
        );
    }

    /// forget 摘掉排队项和去重标记，重新入队只下一次。
    #[test]
    fn forget_allows_exactly_one_requeue() {
        let mut store = CoverStore::default();
        let both = urls(&["cover-a", "cover-b"]);
        assert_eq!(store.want(&both), 2);
        store.forget(["cover-a"]);
        assert_eq!(store.want(&urls(&["cover-a"])), 1);
        assert_eq!(store.want(&urls(&["cover-a"])), 0);
    }

    /// 文件解析出来后又被删：第一次退回可重排，两次后先放弃，作废再下。
    #[test]
    fn vanished_twice_gives_up_until_forgotten() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        let paths = store.paths_handle();
        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Vanished);
        assert!(paths.lookup("cover-a").should_request, "第一次还要继续请求");
        assert_eq!(store.want(&url), 1, "第一次还能重排");

        store.drain();
        store.complete("cover-a", CoverOutcome::Vanished);
        assert!(!paths.lookup("cover-a").should_request, "两次后停止请求");
        assert_eq!(store.want(&url), 0, "两次后先放弃");
        assert!(store.drain().is_empty());

        store.forget(["cover-a"]);
        assert!(paths.lookup("cover-a").should_request, "作废后恢复请求");
        assert_eq!(store.want(&url), 1, "作废之后能重新下");
    }

    #[test]
    fn paths_normalize_url_whitespace() {
        let paths = CoverPaths::default();
        let url = "https://example.com/cover.jpg";
        let raw_url = " \thttps://example.com/cover.jpg\n";
        let padded_url = " https://example.com/cover.jpg ";
        let path = PathBuf::from("/tmp/cover.jpg");

        paths.insert(raw_url, path.clone());

        assert!(paths.contains(url));
        assert!(paths.contains(padded_url));
        assert_eq!(paths.get(url), Some(path.clone()));
        assert_eq!(paths.get(padded_url), Some(path.clone()));

        paths.remove(url);
        assert!(!paths.contains(padded_url));
        assert_eq!(paths.get(padded_url), None);

        paths.insert(raw_url, path);
        paths.remove(padded_url);
        assert!(!paths.contains(url));
        assert_eq!(paths.get(url), None);
    }

    #[test]
    fn lookup_combines_path_and_request_state() {
        let paths = CoverPaths::default();
        let missing = paths.lookup(" cover-a ");
        assert!(missing.path.is_none());
        assert!(missing.should_request);
        assert!(!paths.lookup("   ").should_request);

        paths.insert("cover-a", PathBuf::from("/tmp/a.img"));
        let ready = paths.lookup("cover-a");
        assert_eq!(ready.path, Some(PathBuf::from("/tmp/a.img")));
        assert!(!ready.should_request);

        paths.mark_failed("cover-a");
        let failed = paths.lookup("cover-a");
        assert_eq!(failed.path, Some(PathBuf::from("/tmp/a.img")));
        assert!(!failed.should_request);

        paths.remove("cover-a");
        let retry = paths.lookup("cover-a");
        assert!(retry.path.is_none());
        assert!(retry.should_request);
    }

    /// 读口子不能偷偷改状态：否则一次渲染就能消费队列或放行重复下载。
    #[test]
    fn read_ports_are_side_effect_free() {
        let mut store = CoverStore::default();
        let all = urls(&["cover-a", "cover-b"]);
        store.want(&all);

        for _ in 0..10 {
            assert!(store.path("cover-a").is_none());
            assert!(store.color("cover-a").is_none());
            assert!(store.paths_handle().get("cover-a").is_none());
            assert!(store.paths_handle().lookup("cover-a").should_request);
        }
        assert_eq!(store.want(&all), 0, "读口子不能放行去重");
        assert_eq!(store.drain(), all, "读口子不能消费排队项");
    }

    /// 验收：原图读口子同样不碰磁盘、不消费排队项。
    #[test]
    fn original_read_ports_are_side_effect_free() {
        let mut store = CoverStore::default();
        assert!(store.want_original("cover-a"));

        for _ in 0..10 {
            assert!(store.original_path("cover-a").is_none());
            assert!(store.originals_handle().get("cover-a").is_none());
        }
        assert!(!store.want_original("cover-a"), "读口子不能放行去重");
        assert_eq!(store.drain_original(), urls(&["cover-a"]));
    }

    /// 下载可能把淘汰列表里的文件重建回来，反查不能据此清掉映射。
    #[test]
    fn removed_paths_skip_rebuilt_files() {
        let mut store = CoverStore::default();
        store.want(&urls(&["cover-a"]));
        store.drain();
        let path = PathBuf::from("/tmp/a.img");
        store.complete(
            "cover-a",
            CoverOutcome::Ready {
                path: path.clone(),
                color: None,
            },
        );

        let handle = store.paths_handle();
        assert!(handle
            .urls_for_removed_paths(std::slice::from_ref(&path), |_| true)
            .contains("cover-a"));
        assert!(handle
            .urls_for_removed_paths(std::slice::from_ref(&path), |_| false)
            .is_empty());
    }

    /// 被删路径成千上万，不能为了反查 URL 对整张内存表逐个 stat。
    #[test]
    fn removed_paths_only_check_matching_entries() {
        let mut store = CoverStore::default();
        store.want(&urls(&["cover-a", "cover-b"]));
        store.drain();
        let removed = PathBuf::from("/tmp/a.img");
        store.complete(
            "cover-a",
            CoverOutcome::Ready {
                path: removed.clone(),
                color: None,
            },
        );
        store.complete(
            "cover-b",
            CoverOutcome::Ready {
                path: PathBuf::from("/tmp/b.img"),
                color: None,
            },
        );

        let mut checked = Vec::new();
        let matches =
            store
                .paths_handle()
                .urls_for_removed_paths(std::slice::from_ref(&removed), |path| {
                    checked.push(path.to_path_buf());
                    true
                });
        assert_eq!(checked, vec![removed]);
        assert_eq!(matches.len(), 1);
        assert!(matches.contains("cover-a"));
    }

    /// 原图先回来时给列表兜底，不打断缩略图自己的记账。
    /// 之后这次缩略图成功返回，会改用刚下载的路径。
    #[test]
    fn adopt_thumbnail_publishes_without_touching_inflight() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        store.want(&url);
        store.drain();

        store.adopt_thumbnail("cover-a", PathBuf::from("/tmp/original.img"));
        assert!(store.path("cover-a").is_some());
        assert_eq!(store.want(&url), 0, "已经有路径了，不再入队");

        store.complete("cover-a", ready("/tmp/a.img"));
        assert_eq!(
            store
                .path("cover-a")
                .map(|path| path.to_string_lossy().into_owned()),
            Some("/tmp/a.img".to_string()),
            "缩略图下载完成后改用新路径"
        );
    }

    /// 验收：原图在飞上限 4 张生效，空出一个才泵下一张。
    #[test]
    fn original_inflight_is_capped_at_four() {
        let mut store = CoverStore::default();
        let many: Vec<String> = (0..MAX_ORIGINAL_CONCURRENCY + 2)
            .map(|index| format!("original-{index}"))
            .collect();
        for url in &many {
            assert!(store.want_original(url));
        }

        let started = store.drain_original();
        assert_eq!(started.len(), MAX_ORIGINAL_CONCURRENCY, "最多 4 张在飞");
        assert!(store.drain_original().is_empty(), "在飞满了不能再泵");

        store.complete_original(&started[0], original_ready("/tmp/a.img"));
        assert_eq!(
            store.drain_original(),
            vec![many[MAX_ORIGINAL_CONCURRENCY].clone()],
            "空出一个在飞位就补下一张"
        );
    }

    /// 验收：切歌只留当前曲目的原图；被放掉的别的原图能重新排进来。
    #[test]
    fn switching_song_releases_other_original_queue_entries() {
        let mut store = CoverStore::default();
        let many: Vec<String> = (0..MAX_ORIGINAL_CONCURRENCY + 3)
            .map(|index| format!("original-{index}"))
            .collect();
        for url in &many {
            assert!(store.want_original(url));
        }
        assert_eq!(
            store.drain_original().len(),
            MAX_ORIGINAL_CONCURRENCY,
            "先把在飞位占满，剩下的才在排队"
        );

        let current = many[MAX_ORIGINAL_CONCURRENCY + 2].clone();
        store.retain_original_only(&current);
        assert!(
            !store.want_original(&current),
            "当前曲目还在排队，不重复入队"
        );

        let released = many[MAX_ORIGINAL_CONCURRENCY].clone();
        assert!(
            store.want_original(&released),
            "被切歌放掉的别的原图要能重新排进来"
        );
        assert!(store.original_path(&current).is_none());
    }

    /// 原图排队上限 32：满了挤掉最旧的，被挤掉的还能再排。
    #[test]
    fn original_queue_cap_evicts_oldest_and_allows_requeue() {
        let mut store = CoverStore::default();
        let many: Vec<String> = (0..MAX_ORIGINAL_QUEUE)
            .map(|index| format!("original-{index}"))
            .collect();
        for url in &many {
            assert!(store.want_original(url));
        }

        let extra = format!("original-{MAX_ORIGINAL_QUEUE}");
        assert!(store.want_original(&extra), "满了也要收新图");

        let oldest = many[0].clone();
        assert!(store.want_original(&oldest), "被挤掉的最旧原图能重新排队");
        assert!(!store.want_original(&oldest), "排进去之后不重复入队");
    }

    /// 验收：原图失败也按两次才放弃，作废之后能重新下。
    #[test]
    fn original_failed_twice_gives_up_until_forgotten() {
        let mut store = CoverStore::default();
        let url = "original-a";
        let paths = store.originals_handle();

        assert!(store.want_original(url));
        assert_eq!(store.drain_original(), vec![url.to_string()]);
        store.complete_original(url, CoverOutcome::Failed);
        assert!(paths.lookup(url).should_request, "失败一次还要继续请求");
        assert!(store.want_original(url), "失败一次还能再试");

        assert_eq!(store.drain_original(), vec![url.to_string()]);
        store.complete_original(url, CoverOutcome::Failed);
        assert!(!paths.lookup(url).should_request, "失败两次后停止请求");
        assert!(!store.want_original(url), "失败两次后本会话不再自动重试");
        assert!(store.drain_original().is_empty());

        store.forget([url]);
        assert!(paths.lookup(url).should_request, "作废后恢复请求");
        assert!(store.want_original(url), "作废之后能重新下");
    }

    /// 清缓存后失败次数从零开始，不会带着上一次的 1 直接到顶。
    #[test]
    fn clear_completed_resets_attempt_count_even_when_idle() {
        let mut store = CoverStore::default();
        let url = "original-a";

        assert!(store.want_original(url));
        assert_eq!(store.drain_original(), vec![url.to_string()]);
        store.complete_original(url, CoverOutcome::Failed);

        store.clear_completed();
        assert!(store.records.is_empty(), "清掉失败次数后不留空记录");
        assert!(store.want_original(url));
        assert_eq!(store.drain_original(), vec![url.to_string()]);
        store.complete_original(url, CoverOutcome::Failed);
        assert!(
            store.want_original(url),
            "清缓存后第二次失败仍算第一次，还能再试"
        );
    }

    /// 验收：第一次失败后还能再排一次，第二次成功就正常落账。
    #[test]
    fn original_first_failure_can_succeed_on_second_attempt() {
        let mut store = CoverStore::default();
        let url = "original-a";

        assert!(store.want_original(url));
        assert_eq!(store.drain_original(), vec![url.to_string()]);
        store.complete_original(url, CoverOutcome::Failed);

        assert!(store.want_original(url), "第一次失败还能再试");
        assert_eq!(store.drain_original(), vec![url.to_string()]);
        store.complete_original(url, original_ready("/tmp/full.img"));

        assert!(store.original_path(url).is_some(), "第二次成功就落账");
        assert!(!store.want_original(url), "有路径了不再重排");
    }

    /// 验收：清缓存两格一起作废，在飞原图原样保留。
    #[test]
    fn clear_completed_clears_both_lanes_and_keeps_inflight_original() {
        let mut store = CoverStore::default();
        store.want(&urls(&["thumb-done"]));
        store.drain();
        store.complete("thumb-done", ready("/tmp/thumb.img"));
        assert!(store.want_original("original-done"));
        store.drain_original();
        store.complete_original("original-done", original_ready("/tmp/original.img"));
        assert!(store.original_path("original-done").is_some());
        assert!(store.want_original("original-inflight"));
        store.drain_original();

        store.clear_completed();
        assert!(store.path("thumb-done").is_none(), "缩略图路径要作废");
        assert!(
            store.original_path("original-done").is_none(),
            "原图路径一起作废"
        );
        assert!(
            !store.want_original("original-inflight"),
            "在飞原图不能被清缓存作废"
        );
        assert!(
            store.want_original("original-done"),
            "已完成的原图能重新请求"
        );
    }

    /// 原图和缩略图的路径表各管各的：只删了一侧不能连另一侧一起清。
    #[test]
    fn thumbnail_and_original_paths_stay_separate() {
        let mut store = CoverStore::default();
        store.want(&urls(&["cover-a"]));
        store.drain();
        store.complete("cover-a", ready("/tmp/thumb.img"));
        store.want_original("cover-a");
        store.drain_original();
        store.complete_original("cover-a", original_ready("/tmp/full.img"));

        assert_eq!(store.path("cover-a"), Some(PathBuf::from("/tmp/thumb.img")));
        assert_eq!(
            store.original_path("cover-a"),
            Some(PathBuf::from("/tmp/full.img"))
        );
        assert!(store
            .paths_handle()
            .urls_for_removed_paths(&[PathBuf::from("/tmp/thumb.img")], |_| true)
            .contains("cover-a"));
        assert!(store
            .originals_handle()
            .urls_for_removed_paths(&[PathBuf::from("/tmp/thumb.img")], |_| true)
            .is_empty());

        // 淘汰只删了缩略图：原图路径要留住。
        store.forget_thumbnail(["cover-a"]);
        assert!(store.path("cover-a").is_none());
        assert_eq!(
            store.original_path("cover-a"),
            Some(PathBuf::from("/tmp/full.img"))
        );

        // 只删了原图：缩略图不受影响（这次缩略图已经清掉了，补回来再看）。
        store.want(&urls(&["cover-a"]));
        store.drain();
        store.complete("cover-a", ready("/tmp/thumb-2.img"));
        store.forget_original(["cover-a"]);
        assert!(store.original_path("cover-a").is_none());
        assert_eq!(
            store.path("cover-a"),
            Some(PathBuf::from("/tmp/thumb-2.img"))
        );
    }

    /// 验收：共用同一个 `forget`：缩略图和原图两格一起作废。
    #[test]
    fn forget_clears_both_cells() {
        let mut store = CoverStore::default();
        store.want(&urls(&["cover-a"]));
        store.drain();
        store.complete("cover-a", ready("/tmp/thumb.img"));
        store.want_original("cover-a");
        store.drain_original();
        store.complete_original("cover-a", original_ready("/tmp/full.img"));

        store.forget(["cover-a"]);
        assert!(store.path("cover-a").is_none());
        assert!(store.original_path("cover-a").is_none());
        assert_eq!(store.want(&urls(&["cover-a"])), 1, "两格都能重新下");
        assert!(store.want_original("cover-a"));
    }

    /// 切歌后旧歌的「落完补排」需求也要丢掉，不能等它下载完再排回来。
    #[test]
    fn switching_song_drops_old_retry_demand() {
        let mut store = CoverStore::default();
        assert!(store.want_original("original-old"));
        assert!(store.want_original("original-current"));
        assert_eq!(store.drain_original().len(), 2, "两张都在飞");

        store.retry_original_after_release("original-old");
        store.retry_original_after_release("original-current");
        store.retain_original_only("original-current");
        store.complete_original("original-old", CoverOutcome::Vanished);

        assert!(store.drain_original().is_empty(), "切歌后旧歌不再补排");
        store.complete_original("original-current", CoverOutcome::Vanished);
        assert_eq!(
            store.drain_original(),
            vec!["original-current".to_string()],
            "当前曲目的补排需求要留着"
        );
    }

    /// 清缓存后当前曲目的原图还在下载：先按文件在不在落账，缺了才补排。
    #[test]
    fn retry_after_release_lands_surviving_file_and_requeues_vanished() {
        let mut store = CoverStore::default();

        assert!(store.want_original("original-a"));
        assert_eq!(store.drain_original(), vec!["original-a".to_string()]);
        store.retry_original_after_release("original-a");
        store.complete_original("original-a", original_ready("/tmp/full.img"));
        assert!(
            store.original_path("original-a").is_some(),
            "文件还在就落账"
        );
        assert!(store.drain_original().is_empty(), "文件在就不用再下");

        assert!(store.want_original("original-b"));
        assert_eq!(store.drain_original(), vec!["original-b".to_string()]);
        store.retry_original_after_release("original-b");
        store.complete_original("original-b", CoverOutcome::Vanished);
        assert!(store.original_path("original-b").is_none());
        assert_eq!(
            store.drain_original(),
            vec!["original-b".to_string()],
            "文件没了才补排"
        );
    }

    /// 文件每次都被删（`Vanished`）也不能无限重排：两次后先放弃，作废再重下。
    #[test]
    fn original_vanished_twice_gives_up_until_forgotten() {
        let mut store = CoverStore::default();
        let url = "original-a";
        let paths = store.originals_handle();

        assert!(store.want_original(url));
        assert_eq!(store.drain_original(), vec![url.to_string()]);
        store.complete_original(url, CoverOutcome::Vanished);
        assert!(store.original_path(url).is_none(), "死路径不能发布");
        assert!(paths.lookup(url).should_request, "第一次还要继续请求");
        assert!(store.want_original(url), "文件没了第一次还能再试");

        assert_eq!(store.drain_original(), vec![url.to_string()]);
        store.complete_original(url, CoverOutcome::Vanished);
        assert!(!paths.lookup(url).should_request, "两次后停止请求");
        assert!(!store.want_original(url), "两次都撞上文件已没就先放弃");
        assert!(store.drain_original().is_empty());

        store.forget([url]);
        assert!(paths.lookup(url).should_request, "作废后恢复请求");
        assert!(store.want_original(url), "作废之后能重新下");
    }

    /// 作废的在飞结果回来后没人再要：记录也要删掉，别留空壳。
    #[test]
    fn abandoned_completion_without_demand_leaves_no_record() {
        let mut store = CoverStore::default();
        assert!(store.want_original("original-a"));
        assert_eq!(store.drain_original(), vec!["original-a".to_string()]);

        store.forget(["original-a"]);
        store.complete_original("original-a", original_ready("/tmp/full.img"));
        assert!(store.original_path("original-a").is_none());
        assert!(store.records.is_empty(), "没人再要就不留空记录");
    }

    /// 原图在飞时作废，结果回来后同样不能重复下；有人要就补排。
    #[test]
    fn original_forget_abandons_inflight_until_completion() {
        let mut store = CoverStore::default();
        assert!(store.want_original("original-a"));
        assert_eq!(store.drain_original(), vec!["original-a".to_string()]);

        store.forget(["original-a"]);
        assert!(!store.want_original("original-a"), "在飞期间不能重复下");

        store.complete_original("original-a", original_ready("/tmp/full.img"));
        assert!(
            store.original_path("original-a").is_none(),
            "这次结果要作废"
        );
        assert!(
            !store.drain_original().is_empty(),
            "作废期间记下的需求会在结果回来后补排"
        );
        assert!(
            !store.want_original("original-a"),
            "已经补排过了，不要再重复入队"
        );
    }
}
