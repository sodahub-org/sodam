//! 封面缩略图状态：一条 URL 一条记录。
//!
//! 取图本身不在这里：这里只回答「这张问过没有 / 排到哪一步了 / 结果是什么」。
//! 读口子（`path` / `color` / `paths_handle`）是纯内存查询，不碰磁盘也不发请求；
//! 下载由调用方泵出去，结果再用 [`CoverStore::complete`] 交回来。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// 缩略图下载的并发上限。
pub const MAX_CONCURRENCY: usize = 8;
/// 排队上限：超过就挤掉最旧的，被挤掉的那张退回可重排状态。
pub const MAX_QUEUE: usize = 400;
/// 同一张图失败几次后放弃。只记内存，重启后再试两次。
pub const MAX_FAILURES: u8 = 2;

/// 封面主色。
pub type CoverRgb = (u8, u8, u8);

/// 一轮 `want` 的结果：新排进几张、因队列满挤掉几张。
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

/// 已完成路径的只读句柄：渲染闭包克隆它查图，不用整表复制。
#[derive(Debug, Clone, Default)]
pub struct CoverPaths {
    inner: Arc<RwLock<HashMap<String, PathBuf>>>,
}

impl CoverPaths {
    /// 这张图有没有已完成路径（只查内存，不克隆）。
    pub fn contains(&self, url: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(url.trim())
    }

    /// 查一张图的本地路径（只查内存）。
    pub fn get(&self, url: &str) -> Option<PathBuf> {
        let paths = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        paths.get(url.trim()).cloned()
    }

    fn insert(&self, url: &str, path: PathBuf) {
        let mut paths = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        paths.insert(url.to_string(), path);
    }

    fn remove(&self, url: &str) {
        let mut paths = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        paths.remove(url);
    }

    fn clear(&self) {
        self.inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
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
            let paths = self
                .inner
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            paths
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
struct Record {
    state: State,
    failures: u8,
}

/// 缩略图封面的唯一账本。
#[derive(Debug, Default)]
pub struct CoverStore {
    records: HashMap<String, Record>,
    queue: VecDeque<String>,
    inflight: HashSet<String>,
    /// 渲染闭包够不到 `Root`，用它拿一个只读句柄。
    ready: CoverPaths,
    colors: HashMap<String, CoverRgb>,
    /// 在飞期间被 `forget` 的 URL：这次结果作废，结束后退回 `Idle`。
    abandoned: HashSet<String>,
    /// 作废期间又有人在等这张图：结果回来放行后要立刻重排，
    /// 否则重试按钮要等下一次渲染才生效。
    requeue_after_release: HashSet<String>,
    /// 队列满时挤掉的张数（日志用；每轮 `want` 重新计数）。
    evicted: usize,
}

impl CoverStore {
    /// 要这几张；返回这次真正新排进队列的 URL。
    ///
    /// 同一 URL 在排队、在飞、已完成或已放弃时都不会重复入队。
    pub fn want(&mut self, urls: &[String]) -> Vec<String> {
        // 挤掉的张数按这一轮重置；入队数直接用返回值，不再另记一份。
        self.evicted = 0;
        let mut queued = Vec::new();
        for raw in urls {
            if self.enqueue(raw.trim()) {
                queued.push(raw.trim().to_string());
            }
        }
        queued
    }

    /// 单张入队；入队数由 `want` 的返回值算，这里只可能在满队列时记一次 dropped。
    /// 作废放行补排也走这里，那次挤出照样记；新一轮 `want` 开头会清零。
    fn enqueue(&mut self, url: &str) -> bool {
        if url.is_empty() || self.ready.contains(url) {
            return false;
        }
        let state = self.records.get(url).map(|record| record.state);
        if !matches!(state, None | Some(State::Idle)) {
            // 在飞但已被 forget：把这次需求记下来，等结果回来立刻重排。
            if state == Some(State::Inflight) && self.abandoned.contains(url) {
                self.requeue_after_release.insert(url.to_string());
            }
            return false;
        }
        if self.queue.len() >= MAX_QUEUE {
            self.evict_oldest();
        }
        let record = self.records.entry(url.to_string()).or_default();
        record.state = State::Queued;
        self.queue.push_back(url.to_string());
        true
    }

    /// 泵的上半截：把排队项推进在飞，最多 [`MAX_CONCURRENCY`] 个。
    pub fn drain(&mut self) -> Vec<String> {
        let mut started = Vec::new();
        while self.inflight.len() < MAX_CONCURRENCY {
            let Some(url) = self.queue.pop_front() else {
                break;
            };
            let record = self.records.entry(url.clone()).or_default();
            if record.state != State::Queued {
                continue;
            }
            record.state = State::Inflight;
            self.inflight.insert(url.clone());
            started.push(url);
        }
        started
    }

    /// 泵的下半截：收一次下载结果。
    pub fn complete(&mut self, url: &str, outcome: CoverOutcome) {
        let url = url.trim();
        if url.is_empty() {
            return;
        }
        self.inflight.remove(url);
        if self.abandoned.remove(url) {
            // 下载中被 forget：这次拿回来的东西不要，退回可重排。
            // 不要动已发布路径：原图可能已经先兜过底，文件还在。
            if let Some(record) = self.records.get_mut(url) {
                record.state = State::Idle;
                record.failures = 0;
            }
            // 作废期间又有人要这张图（点重试、滚动回来）：现在补排一次。
            if self.requeue_after_release.remove(url) {
                self.enqueue(url);
            }
            return;
        }
        match outcome {
            CoverOutcome::Ready { path, color } => {
                let record = self.records.entry(url.to_string()).or_default();
                record.state = State::Ready;
                record.failures = 0;
                self.ready.insert(url, path);
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
                if let Some(record) = self.records.get_mut(url) {
                    record.state = State::Idle;
                }
                // 不抹已发布路径：原图先回来时可能已经兜过底，文件还在。
            }
            CoverOutcome::Failed => {
                let record = self.records.entry(url.to_string()).or_default();
                record.failures = record.failures.saturating_add(1);
                record.state = if record.failures >= MAX_FAILURES {
                    State::Failed
                } else {
                    State::Idle
                };
            }
        }
    }

    /// 作废这些 URL：清路径、颜色、记号、失败次数；不碰磁盘。
    ///
    /// 在飞的那张不会立刻放行——否则同一张会同时下两遍；等这次结果回来再退回
    /// 可重排状态。
    pub fn forget<'a>(&mut self, urls: impl IntoIterator<Item = &'a str>) {
        for raw in urls {
            let url = raw.trim();
            if url.is_empty() {
                continue;
            }
            self.queue.retain(|queued| queued.as_str() != url);
            let inflight = self
                .records
                .get(url)
                .is_some_and(|record| record.state == State::Inflight);
            if inflight {
                self.abandoned.insert(url.to_string());
                if let Some(record) = self.records.get_mut(url) {
                    record.failures = 0;
                }
            } else {
                self.records.remove(url);
            }
            self.ready.remove(url);
            self.colors.remove(url);
        }
    }

    /// 清缓存后作废「已完成」的结果：`Ready` / `Failed` 退回 `Idle`。
    ///
    /// 排队和在飞原样保留：丢掉在飞状态只会让同一张图再排一次，
    /// 个别情况下并发下两遍。
    pub fn clear_completed(&mut self) {
        for record in self.records.values_mut() {
            if matches!(record.state, State::Ready | State::Failed) {
                record.state = State::Idle;
                record.failures = 0;
            }
        }
        self.ready.clear();
        self.colors.clear();
    }

    /// 原图先下回来时给列表兜个底：只发布路径，不动在飞 / 排队记账。
    pub fn adopt_thumbnail(&mut self, url: &str, path: PathBuf) {
        let url = url.trim();
        if url.is_empty() || self.ready.contains(url) {
            return;
        }
        self.ready.insert(url, path);
        if let Some(record) = self.records.get_mut(url) {
            if matches!(record.state, State::Idle | State::Ready | State::Failed) {
                record.state = State::Ready;
                record.failures = 0;
            }
        }
    }

    /// 本地路径：只查内存。
    pub fn path(&self, url: &str) -> Option<PathBuf> {
        self.ready.get(url)
    }

    /// 氛围色：只查内存。
    pub fn color(&self, url: &str) -> Option<CoverRgb> {
        self.colors.get(url.trim()).copied()
    }

    /// 渲染闭包用的只读句柄；克隆的是 `Arc`，不复制表。
    pub fn paths_handle(&self) -> CoverPaths {
        self.ready.clone()
    }

    /// 取走上一轮 `want` 因队列满挤掉的张数。
    pub fn take_dropped(&mut self) -> usize {
        std::mem::take(&mut self.evicted)
    }

    fn evict_oldest(&mut self) {
        while let Some(url) = self.queue.pop_front() {
            if let Some(record) = self.records.get_mut(&url) {
                if record.state == State::Queued {
                    record.state = State::Idle;
                    self.evicted += 1;
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CoverOutcome, CoverStore, MAX_CONCURRENCY, MAX_QUEUE};
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

    /// 验收：同一 URL 连要十次，最多只发一次取图请求。
    #[test]
    fn want_same_url_ten_times_fetches_once() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        assert_eq!(store.want(&url).len(), 1);
        for _ in 0..9 {
            assert!(store.want(&url).is_empty());
        }
        assert_eq!(store.drain(), url, "只该泵出一张");
        assert!(store.drain().is_empty(), "在飞时不能重复泵出");
        store.complete("cover-a", ready("/tmp/a.img"));
        assert!(store.want(&url).is_empty(), "已完成也不能重复入队");
    }

    /// 排队 / 在飞 / 已完成三种状态下都不重复入队。
    #[test]
    fn want_skips_queued_inflight_and_ready() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        assert_eq!(store.want(&url).len(), 1, "排队");
        assert!(store.want(&url).is_empty());
        assert_eq!(store.drain(), url, "在飞");
        assert!(store.want(&url).is_empty());
        store.complete("cover-a", ready("/tmp/a.img"));
        assert!(store.want(&url).is_empty(), "已完成");
        assert!(store.path("cover-a").is_some());
    }

    /// 下载失败两次才放弃；放弃后 forget 能重新下。
    #[test]
    fn failed_twice_gives_up_until_forgotten() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);

        assert_eq!(store.want(&url).len(), 1);
        assert_eq!(store.drain(), url);
        store.complete("cover-a", CoverOutcome::Failed);
        assert_eq!(store.want(&url).len(), 1, "失败一次还能再试");

        assert_eq!(store.drain(), url);
        store.complete("cover-a", CoverOutcome::Failed);
        assert!(store.want(&url).is_empty(), "失败两次后本会话不再自动重试");
        assert!(store.drain().is_empty());

        store.forget(url.iter().map(String::as_str));
        assert_eq!(store.want(&url).len(), 1, "清掉失败记录后能重新下");
    }

    /// 清缓存后失败户也能重新下。
    #[test]
    fn clear_completed_revives_failed_url() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Failed);
        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Failed);
        assert!(store.want(&url).is_empty());

        store.clear_completed();
        assert_eq!(store.want(&url).len(), 1);
    }

    /// 验收：队列塞满后挤掉的那张还能重新排进来。
    #[test]
    fn full_queue_evicts_oldest_and_allows_requeue() {
        let mut store = CoverStore::default();
        let first: Vec<String> = (0..MAX_QUEUE)
            .map(|index| format!("cover-{index}"))
            .collect();
        assert_eq!(store.want(&first).len(), MAX_QUEUE);
        assert_eq!(store.take_dropped(), 0);

        let extra = urls(&["cover-extra"]);
        assert_eq!(store.want(&extra).len(), 1, "满了也要收新图");
        assert_eq!(store.take_dropped(), 1, "挤掉了最旧的一张");

        let oldest = urls(&["cover-0"]);
        assert_eq!(store.want(&oldest).len(), 1, "被挤掉的那张能重新排队");
        assert_eq!(store.take_dropped(), 1);
        assert!(store.want(&oldest).is_empty(), "排进去之后不重复入队");
    }

    /// 清缓存只作废已完成的结果；在飞原样保留，已完成的放行重排。
    #[test]
    fn clear_completed_keeps_inflight_and_revives_done() {
        let mut store = CoverStore::default();
        let all = urls(&["downloading", "queued", "done"]);
        assert_eq!(store.want(&all).len(), 3);
        assert_eq!(store.drain().len(), 3, "三张都进了在飞");
        store.complete("done", ready("/tmp/done.img"));
        assert!(store.path("done").is_some());

        store.clear_completed();
        assert!(store.path("done").is_none(), "已完成的结果要作废");
        assert!(
            store.want(&urls(&["downloading"])).is_empty(),
            "在飞不能被再次入队"
        );
        assert!(
            store.want(&urls(&["queued"])).is_empty(),
            "在飞不能被再次入队"
        );
        assert_eq!(
            store.want(&urls(&["done"])).len(),
            1,
            "已完成的放行，清缓存后能重新请求"
        );
        assert_eq!(store.drain(), urls(&["done"]), "只泵出重新排队的那张");

        // 磁盘被清空的在飞封面：这次下载结束时退回可重排，否则一直占位图。
        store.complete("downloading", CoverOutcome::Vanished);
        assert_eq!(store.want(&urls(&["downloading"])).len(), 1);
    }

    /// 清缓存不能作废还在排队、没轮到泵的 URL。
    #[test]
    fn clear_completed_keeps_queued_that_never_started() {
        let mut store = CoverStore::default();
        let many: Vec<String> = (0..MAX_CONCURRENCY + 2)
            .map(|index| format!("cover-{index}"))
            .collect();
        assert_eq!(store.want(&many).len(), MAX_CONCURRENCY + 2);

        let started = store.drain();
        assert_eq!(started.len(), MAX_CONCURRENCY);
        let queued_tail = many[MAX_CONCURRENCY].clone();

        store.clear_completed();
        assert!(
            store.want(std::slice::from_ref(&queued_tail)).is_empty(),
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
        assert_eq!(store.want(&url).len(), 1, "结束后要能重新排队");
    }

    /// 作废放行时的补排不能污染下一轮 want 的日志计数。
    #[test]
    fn requeue_after_release_keeps_want_stats_clean() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        store.want(&url);
        store.drain();
        store.forget(["cover-a"]);
        assert!(store.want(&url).is_empty());
        store.complete("cover-a", CoverOutcome::Failed);

        assert_eq!(store.drain(), url, "补排已经回到队列");
        assert_eq!(store.take_dropped(), 0, "补排不能算进上一轮 want");
    }

    /// 在飞被 forget 之后又有人要（点重试）：结果回来要立刻补排。
    #[test]
    fn demand_during_abandoned_inflight_requeues_after_completion() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        store.want(&url);
        assert_eq!(store.drain(), url);
        store.forget(["cover-a"]);

        assert!(store.want(&url).is_empty(), "在飞期间不能重复下");
        store.complete("cover-a", CoverOutcome::Failed);
        assert_eq!(store.drain(), url, "作废结果回来后要把这次需求补上");
        assert!(store.want(&url).is_empty(), "补排之后不重复入队");
    }

    /// forget 摘掉排队项和去重标记，重新入队只下一次。
    #[test]
    fn forget_allows_exactly_one_requeue() {
        let mut store = CoverStore::default();
        let both = urls(&["cover-a", "cover-b"]);
        assert_eq!(store.want(&both).len(), 2);
        store.forget(["cover-a"]);
        assert_eq!(store.want(&urls(&["cover-a"])).len(), 1);
        assert!(store.want(&urls(&["cover-a"])).is_empty());
    }

    /// 文件解析出来后又被删：退回可重排。
    #[test]
    fn vanished_allows_requeue() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        store.want(&url);
        store.drain();
        store.complete("cover-a", CoverOutcome::Vanished);
        assert_eq!(store.want(&url).len(), 1);
    }

    /// 验收：查路径 / 查颜色 / 取快照只读，不改状态、不消费排队项。
    ///
    /// store 本身没有取图入口，取图只在 `drain` 之后由调用方做；这里钉住
    /// 读口子不能把排队项吃掉、不能放行去重、不能凭空变出路径。
    #[test]
    fn read_ports_are_side_effect_free() {
        let mut store = CoverStore::default();
        let all = urls(&["cover-a", "cover-b"]);
        store.want(&all);

        for _ in 0..10 {
            assert!(store.path("cover-a").is_none());
            assert!(store.color("cover-a").is_none());
            assert!(store.paths_handle().get("cover-a").is_none());
        }
        assert!(store.want(&all).is_empty(), "读口子不能放行去重");
        assert_eq!(store.drain(), all, "读口子不能消费排队项");
    }

    /// 淘汰反查只认「真没了」的路径：重建回来的不算缺。
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

    /// 原图先回来时给列表兜底，不打断缩略图自己的记账。
    ///
    /// 之后再收到这次缩略图的结果（成功 / 失败 / 文件已没）都不能抹掉这条路。
    #[test]
    fn adopt_thumbnail_publishes_without_touching_inflight() {
        let mut store = CoverStore::default();
        let url = urls(&["cover-a"]);
        store.want(&url);
        store.drain();

        store.adopt_thumbnail("cover-a", PathBuf::from("/tmp/original.img"));
        assert!(store.path("cover-a").is_some());
        assert!(store.want(&url).is_empty(), "已经有路径了，不再入队");

        store.complete("cover-a", ready("/tmp/a.img"));
        assert_eq!(
            store
                .path("cover-a")
                .map(|path| path.to_string_lossy().into_owned()),
            Some("/tmp/a.img".to_string())
        );
    }
}
