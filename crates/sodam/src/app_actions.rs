//! 用户动作层：搜索、推荐、媒体库、播放和封面加载。
//!
//! [`Root`] 的字段定义在 [`super`]，这里按领域拆分实现，避免单文件继续膨胀。

use crate::loop_probe::LoopProbe;
use crate::ui::i18n::Language;
use crate::ui::middle_drag::DragTarget;
use crate::ui::theme::{self, ThemeKind};
use futures_lite::StreamExt;
use gpui::prelude::*;
use gpui::{Context, Window};
use sodam_core::audio::PlaybackSnapshot;
use sodam_core::cache_check::{Blockers, MaintenanceAsk, MaintenanceDecision, MaintenanceTrigger};
use sodam_core::config::EnvCookieFill;
use sodam_core::cover::CoverOutcome;
use sodam_core::failure::{
    PlayBudget, PlaybackFailure, RecoveryAction, Stage, UnplayableReason, UnplayableTracks,
};
use sodam_core::library::RecommendationSource;
use sodam_core::log_outlet::{self, log_event, LogLevel, LogTopic};
use sodam_core::login::LoginStatus;
use sodam_core::models::{LyricLine, PlaylistItem, SceneItem, TrackItem};
use sodam_core::play_failure::{
    counts_consecutive_failure, decide_play_failure, Advance, FailureRecord, Halt, NoticeChannel,
    NoticeSlot, PlayFailureDecision, PlayFailureInput, PlayIntent,
};
use sodam_core::playback_trace::{drive_play_attempt, PlaybackTrace};
use sodam_core::prefetch::{is_global_failure, SuspensionClass};
use sodam_core::session::Session;
use sodam_core::settings_writer::{FailureKind, SignOut, WriteFailure};
use sodam_core::upgrade::{
    identity_change, stop_reason, IdentityChange, NotImprovedReason, Outcome, StopConditions,
    StopReason, Tally,
};
use sodam_core::Settings;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::{
    apply_verify_window_result, status_label, verify_window_step, BatchUpgradeProgress, LoginState,
    Nav, QueueOrigin, Root, SceneModesState, VerifyWindowState, VerifyWindowStep,
    QUEUE_STOPPED_TEXT, REMOTE_TEXT, RETRY_TEXT, ROW_BATCH, SAVE_TEXT, STARTUP_SAVE_TEXT,
};
use sodam_core::queue::{PlayMode, Queue};

/// 单首 / 批量两条路各自的日志级别。
///
/// 用带名字的字段而不是两个挨着的 `LogLevel`：同一个类型相邻，调用点写反了照样编译过，
/// 只会变成「成功记 WARN、失败记 INFO」这种测不出来的错。
#[derive(Clone, Copy)]
struct UpgradeLogLevels {
    success: LogLevel,
    failure: LogLevel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BatchUpgradeNotice {
    Status(String),
    Success(String),
    Warning(String),
}

// 失败优先于成功：有失败就让用户看见；全零只走普通状态兜底。
fn batch_upgrade_notice(tally: Tally, message: String) -> BatchUpgradeNotice {
    if tally.failed > 0 {
        BatchUpgradeNotice::Warning(message)
    } else if tally.upgraded > 0 {
        BatchUpgradeNotice::Success(message)
    } else {
        BatchUpgradeNotice::Status(message)
    }
}

fn cache_upgrade_log_parts<'a>(
    track_id: &'a str,
    quality: &'a str,
    reason: Option<NotImprovedReason>,
) -> (&'static str, Vec<(&'a str, &'a str)>) {
    let event = if reason.is_some() {
        "cache_upgrade_not_improved"
    } else {
        "cache_upgraded"
    };
    let mut fields = vec![("track_id", track_id), ("quality", quality)];
    if let Some(reason) = reason {
        fields.push(("reason", reason.as_str()));
    }
    (event, fields)
}

/// 歌词页显示哪首歌的封面，补下载就按哪首取 URL。
///
/// 歌词页用的是 `pending_track.or(queue.current())`（views/lyrics.rs），正在装载、
/// 还没进队列的那首歌也显示它。缓存清理后补原图必须同口径，否则那首歌的大图
/// 补不回来。两处补偿共用这个函数，避免一边改了口径另一边漏掉。
fn current_cover_url(pending: Option<&TrackItem>, queued: Option<&TrackItem>) -> Option<String> {
    // `CoverStore` 里存的和查的都是 trim 过的 key；这里不 trim，淘汰反查
    // 时当前曲目就会跟删除列表对不上，原图不会补回来。
    pending
        .or(queued)
        .map(|track| track.cover.trim().to_string())
}

/// 文件在就发布；文件已删就不发布并放行重试；没下到则保留失败记号。
///
/// 这条复核不能省：死路径会被 `CoverStore` 当成已有封面，歌词页大图再也回不来。
fn settle_original_cover(
    path: Option<PathBuf>,
    file_exists: impl FnOnce(&std::path::Path) -> bool,
) -> CoverOutcome {
    match path {
        Some(path) if file_exists(&path) => CoverOutcome::Ready { path, color: None },
        Some(_) => CoverOutcome::Vanished,
        None => CoverOutcome::Failed,
    }
}

/// 原图这次没成功（文件没了，或失败一次）后，要不要顺手给当前曲目再排一次。
///
/// 清缓存还在跑时交给 `clear_cache` 的补偿，别和它抢；淘汰删文件时
/// `drop_removed_covers` 反查不到还没发布过的 URL，只能在这里补。
/// 失败重试也走这里：缩略图靠渲染每帧 `want`，原图没有这个口子，
/// 第一次失败后不主动再排，就只剩重试按钮。
fn retry_current_original_cover(
    cache_clearing: bool,
    displayed_cover: Option<&str>,
    url: &str,
) -> bool {
    !cache_clearing && displayed_cover == Some(url)
}

/// 进听歌模式页时要不要自动拉一次。
///
/// 探索卡请求在飞时不叠加。模式失败终态还在时整页不自动重发：断网时每次
/// 进页面都再打一轮就成了请求循环；卡的恢复交给页底那颗「重试加载」。
fn should_auto_load_scene_page(
    cards_empty: bool,
    cards_loading: bool,
    modes: SceneModesState,
) -> bool {
    cards_empty && !cards_loading && !matches!(modes, SceneModesState::Failed)
}

impl Root {
    /// 设置页贡献者墙：用 contrib.rocks 的组合图，后台拉取并缓存在内存。
    ///
    /// 不用 GitHub API，因此没有 token / 请求配额问题；贡献者增减由服务自动更新。
    ///
    /// `force` 只在用户点击「重试」时为 true；渲染路径一律传 false。
    /// 失败态由 `sodam_core::contributors::should_fetch_contributors` 判定是否停手，
    /// 避免「失败 → notify → 重绘 → 再拉取」的死循环（#135 C）。
    pub fn ensure_contributors(&mut self, cx: &mut Context<Self>, force: bool) {
        if !sodam_core::contributors::should_fetch_contributors(
            self.contributors_image.is_some(),
            self.contributors_fetching,
            self.contributors_failed,
            force,
        ) {
            return;
        }
        self.contributors_failed = false;
        self.contributors_fetching = true;
        let work =
            cx.background_spawn(async { sodam_core::contributors::fetch_contributors_svg() });
        cx.spawn(async move |this, cx| {
            let image = work
                .await
                .map(|bytes| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Svg, bytes)));
            let _ = this.update(cx, |root, cx| {
                root.contributors_fetching = false;
                root.contributors_failed = image.is_none();
                root.contributors_image = image;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn search_input_focused(&self, window: &Window) -> bool {
        self.search_focus.is_focused(window)
    }

    /// 预取队列接下来几首到本地缓存（只落盘，不动界面状态）。
    ///
    /// 保持「拉完再播」的简单架构不变，但把等待时间提前吃掉：
    /// 当前曲目一开播，就在后台把接下来几首下好；切歌时命中缓存直接播。
    ///
    /// * 预取数量：`SODAM_PREFETCH_COUNT`（默认 3）；
    /// * 并发上限：`SODAM_PREFETCH_CONCURRENCY`（默认 2，避免签名服务/网络压力）；
    /// * 已缓存 / 正在预取 / 正在装载的曲目跳过，继续看更后面的。
    ///
    /// 触发点：装载成功、切音质、「下一首播放」，以及心跳每 ~5s 的巡检
    /// （后者自动覆盖 append/remove 等队列变更，不必逐个调用点补）。
    pub(crate) fn spawn_prefetch(&mut self, cx: &mut Context<Self>) {
        // 全局故障期间整条管线挂起：巡检每 5 秒仍会路过这里，但一次请求都不发。
        // 冷却到期由心跳在未落闸的巡检里轮询，这里只问闸开没开。
        if !self.prefetch_gate.allows() {
            return;
        }
        let log = log_outlet::enabled(LogLevel::Debug, LogTopic::Prefetch);
        // 这一轮预取使用同一个时刻判断冷却，避免逐首重采样把同一批候选判成不同状态。
        let cooldown_now = std::time::Instant::now();
        // 这次请求属于哪一份登录凭据：回来时若已经换过（重新登录 / 退出），
        // 失败结论属于旧身份，不能用来挂起刚恢复的管线。
        let auth_epoch = self.auth_epoch;
        let ahead = prefetch_ahead_count();
        let max_inflight = prefetch_concurrency().max(1);
        let account_is_vip = self.account.as_ref().map(|a| a.vip);
        let candidates: Vec<TrackItem> = self
            .queue
            .peek_ahead(ahead)
            .iter()
            .map(|track| (*track).clone())
            .collect();
        if candidates.is_empty() {
            if log {
                log_event(
                    LogLevel::Debug,
                    LogTopic::Prefetch,
                    "queue_ahead_empty",
                    &[],
                );
            }
            return;
        }
        for next in candidates {
            if self.prefetch_inflight.len() >= max_inflight {
                break;
            }
            // 已确认当前账号放不出来的曲目不再预取：那只是在为已知结论重复打网络。
            // 集合作废（账号 / 会员状态变化）后才重新参与。
            if self.unplayable.is_unavailable(&next.id) {
                if log {
                    log_event(
                        LogLevel::Debug,
                        LogTopic::Prefetch,
                        "skip_unplayable",
                        &[("track_id", next.id.as_str())],
                    );
                }
                continue;
            }
            // 已经在缓存里就不用再排（继续看更后面的：多首预取的意义）
            if let Some(session) = &self.session {
                if session.is_cached(&next.id) {
                    if log {
                        log_event(
                            LogLevel::Debug,
                            LogTopic::Prefetch,
                            "skip_cached",
                            &[("track_id", next.id.as_str())],
                        );
                    }
                    continue;
                }
            }
            // 正在预取 / 正在装载（播放下载中）的曲目不重复排
            if self.prefetch_inflight.contains(&next.id)
                || self
                    .pending_track
                    .as_ref()
                    .map(|track| track.id == next.id)
                    .unwrap_or(false)
            {
                continue;
            }
            // 失败过的曲目只做 60 秒冷却：这张表刻意不区分可重试性，也不留永久条目。
            // 按曲目结论已进不可播集合，全局故障已挂管线；能走到这里的只剩抖动、限流
            // 与无法归类的失败——等一个冷却周期，比给单首留一张永不过期的黑名单更符合
            // #112 的意图。
            if self
                .prefetch_cooldown_since
                .get(&next.id)
                .is_some_and(|failed_at| prefetch_cooldown_active(*failed_at, cooldown_now))
            {
                continue;
            }
            if log {
                log_event(
                    LogLevel::Debug,
                    LogTopic::Prefetch,
                    "start",
                    &[
                        ("track_id", next.id.as_str()),
                        ("title", next.title.as_str()),
                    ],
                );
            }
            self.prefetch_inflight.insert(next.id.clone());
            let settings = self.settings.clone();
            let track = next.clone();
            let work_track = track.clone();
            let result = cx.background_spawn(async move {
                Session::new(settings).download_to_cache(&work_track, account_is_vip)
            });
            cx.spawn(async move |this, cx| {
                let outcome = result.await;
                if log {
                    if let Ok(cached) = &outcome {
                        log_event(
                            LogLevel::Debug,
                            LogTopic::Prefetch,
                            "done",
                            &[
                                ("track_id", track.id.as_str()),
                                ("quality", cached.quality.as_str()),
                            ],
                        );
                    }
                }
                if let Err(failure) = &outcome {
                    // 预取失败不该只在开发期可见：它决定切歌时是秒开还是干等。
                    let detail = failure.to_string();
                    log_event(
                        LogLevel::Warn,
                        LogTopic::Prefetch,
                        "failed",
                        &[("track_id", track.id.as_str()), ("detail", detail.as_str())],
                    );
                }
                let _ = this.update(cx, |root, cx| {
                    // 只删自己的：避免早先的完成回执误清后来排的曲目（单槽时代踩过）
                    root.prefetch_inflight.remove(&track.id);
                    match &outcome {
                        Ok(_) => {
                            root.prefetch_cooldown_since.remove(&track.id);
                        }
                        Err(failure) => {
                            // 这次请求跨过一次登录变更：结论属于旧身份，既不挂起
                            // 已经恢复的管线，也不写回已作废的不可播集合。
                            if root.auth_epoch != auth_epoch {
                                if log {
                                    log_event(
                                        LogLevel::Debug,
                                        LogTopic::Prefetch,
                                        "stale_after_auth_change",
                                        &[("track_id", track.id.as_str())],
                                    );
                                }
                                return;
                            }
                            let now = std::time::Instant::now();
                            match prefetch_failure_route(failure) {
                                PrefetchFailureRoute::PerTrack => {
                                    // 按曲目类失败不挂管线：结论只关于这一首，进不可播集合，
                                    // 后面的歌照常排；集合本身也会让这首不再被预取。
                                    // 标记要立刻出现在各列表里：心跳只在播放中刷新，
                                    // 预取失败后停着不动时也得让用户看到结论。
                                    root.unplayable.record_failure(&track.id, failure);
                                    cx.notify();
                                }
                                PrefetchFailureRoute::Suspend => {
                                    // 全局故障挂起整条管线（只在真的挂上时写一行日志）。
                                    root.suspend_prefetch(failure);
                                }
                                PrefetchFailureRoute::Cooldown => {
                                    // 抖动 / 限流 / 无法归类的失败只影响这一首：60 秒冷却，
                                    // 不做永久记录，也不随 `retryability` 改变含义。
                                    note_prefetch_cooldown(
                                        &mut root.prefetch_cooldown_since,
                                        &track.id,
                                        now,
                                    );
                                }
                            }
                        }
                    }
                });
            })
            .detach();
        }
    }

    /// 播放装载遇到全局故障时同样挂起预取管线。
    ///
    /// 装载这条路先看见了故障，预取没理由继续巡检。按曲目类与抖动类不挂：
    /// 前者只进不可播集合，后者有逐曲冷却。
    fn suspend_prefetch(&mut self, failure: &PlaybackFailure) {
        let Some(class) = self
            .prefetch_gate
            .suspend_for(failure, std::time::Instant::now())
        else {
            return;
        };
        log_prefetch_suspension(class);
    }

    /// 启动期配置回存失败正常情况下由托盘拍报告；退出等待期间不消费，留给退出状态。
    pub(crate) fn report_startup_persist_result(&mut self, cx: &mut Context<Self>) {
        if self.quit_gate.is_saving() {
            return;
        }
        let Some(failure) = self.settings_writer.take_startup_error() else {
            return;
        };
        self.report_settings_failure(failure, cx);
    }

    fn report_settings_failure(&mut self, failure: WriteFailure, cx: &mut Context<Self>) {
        let text = match failure.kind {
            FailureKind::Startup => STARTUP_SAVE_TEXT,
            FailureKind::QueueStopped => QUEUE_STOPPED_TEXT,
            FailureKind::Settings => SAVE_TEXT,
        };
        let message = self.tr(text).to_string();
        self.notify_error_once("settings-save-failed", message);
        cx.notify();
    }

    fn wake_heartbeat(&mut self) {
        let _ = self.heartbeat_wake_tx.try_send(());
    }

    /// UI 心跳：热档 200ms，空闲 1s；空闲拍只跑尾巴活。
    pub(crate) fn start_heartbeat(
        cx: &mut Context<Self>,
        mut wake_rx: futures_channel::mpsc::Receiver<()>,
    ) {
        cx.spawn(async move |this, cx| {
            const HOT: std::time::Duration = std::time::Duration::from_millis(200);
            const IDLE: std::time::Duration = std::time::Duration::from_secs(1);
            const PREFETCH_PATROL: std::time::Duration = std::time::Duration::from_secs(5);
            const CACHE_MAINTENANCE: std::time::Duration = std::time::Duration::from_secs(60);
            enum Waited {
                Timer,
                Signal,
                Closed,
            }
            let mut probe = LoopProbe::new("heartbeat");
            let mut interval = HOT;
            loop {
                let sleep = cx.background_executor().timer(interval);
                let waited = futures_lite::future::or(
                    async {
                        sleep.await;
                        Waited::Timer
                    },
                    async {
                        if wake_rx.next().await.is_some() {
                            Waited::Signal
                        } else {
                            Waited::Closed
                        }
                    },
                )
                .await;
                match waited {
                    Waited::Timer => probe.tick(),
                    Waited::Signal => probe.check(),
                    Waited::Closed => break,
                }
                let mut next_interval = IDLE;
                let updated = this.update(cx, |root, cx| {
                    probe.app_entry();
                    let snap = root.engine.snapshot();
                    probe.engine_snapshot();
                    root.play_reporter.observe(&snap);

                    let new_finished = finished_event_is_current(
                        snap.finished_seq,
                        snap.seek_seq,
                        &mut root.last_finished_seq,
                        &mut root.last_seek_seq,
                    );
                    let new_error = snap.error_seq != root.last_error_seq;
                    let loading =
                        root.pending_track.is_some() || root.pending_engine_load.is_some();
                    let engine_error_unshown = !snap.error.is_empty()
                        && !snap.track_id.is_empty()
                        && root.playback_error.as_deref() != Some(snap.error.as_str());
                    let hot = snap.playing
                        || root.playing
                        || loading
                        || (snap.finished && new_finished)
                        || new_error;
                    if hot {
                        if root.pending_engine_load == Some(snap.load_seq) {
                            root.pending_engine_load = None;
                            root.pending_track = None;
                            root.playing = snap.playing;
                            cx.notify();
                        }
                        if root.pending_track.is_none() && engine_error_unshown {
                            root.playing = false;
                            root.playback_error = Some(snap.error.clone());
                            let message = root.language.engine_error(&snap.error);
                            root.notify_error_once("playback-engine-failed", message);
                            cx.notify();
                        }
                        if snap.playing || root.pending_track.is_some() {
                            cx.notify();
                        }
                        let progress = AttemptProgress::of(&snap, root.progress_token);
                        if progress.recovered() {
                            root.consecutive_failures = 0;
                            root.progress_token = None;
                        }
                        // 封顶时继续翻歌等于替用户决定再试几首。
                        let should_advance = new_finished && !root.playback_halted;
                        if snap.finished && should_advance && root.pending_track.is_none() {
                            if root.queue.mode == PlayMode::RepeatOne {
                                // 单曲循环只重播，不能换成跳不可播的下一首。
                                root.advance_next(false, cx);
                            } else {
                                // 自动续播也先查不可播集合，避免重复打网络。
                                root.auto_advance_track(cx);
                            }
                        }
                    }

                    // 装载期产生的错误等 loading 退出后再消费。
                    let defer_error =
                        new_error && root.pending_track.is_some() && engine_error_unshown;
                    if new_error && !defer_error {
                        root.last_error_seq = snap.error_seq;
                    }
                    if root.round_exhausted.is_some() && root.has_playable_ahead() {
                        root.clear_round_exhaustion();
                    }
                    // 先判退出刷尾，避免把保存失败的提示在退出几秒里取走扔掉。
                    if !root.quit_gate.is_saving() {
                        if let Some(failure) = root.settings_writer.take_error() {
                            root.report_settings_failure(failure, cx);
                        }
                    }
                    root.settle_volume_scrub();
                    // 预取巡检按真实时间算间隔，不数心跳拍子，队列增删不会把巡检往后拖；
                    // 落闸期间跳过，撤闸后第一拍立刻补跑。
                    let now = std::time::Instant::now();
                    if now.saturating_duration_since(root.last_prefetch_patrol) >= PREFETCH_PATROL
                        && !root.playback_halted
                    {
                        root.last_prefetch_patrol = now;
                        log_prefetch_resume(root.prefetch_gate.expire_cooldown(now));
                        root.prefetch_cooldown_since
                            .retain(|_, failed_at| prefetch_cooldown_active(*failed_at, now));
                        root.spawn_prefetch(cx);
                    }
                    // 下载记账把占用推过九成时不等 60 秒那一拍：每拍只比一次数字，
                    // 真要跑的时候决策函数还会再确认一遍。
                    let cache_limits = root.cache_maintenance_limits();
                    if sodam_core::cache_check::ledger_pressure(
                        root.cache_mark.as_ref(),
                        cache_limits,
                        sodam_core::cache::pending_audio_bytes(),
                    ) {
                        root.maintain_cache_from_ledger(cx);
                    } else if now.saturating_duration_since(root.last_cache_check)
                        >= CACHE_MAINTENANCE
                    {
                        root.last_cache_check = now;
                        // 被挡下的强制请求由决策函数记着，这一拍按到期规则问一句就行。
                        root.check_cache_maintenance(cx);
                    }
                    root.load_more_recommendation(cx);
                    next_interval = if hot || root.volume_scrub.scrubbing() {
                        HOT
                    } else {
                        IDLE
                    };
                });
                if updated.is_err() {
                    break;
                }
                interval = next_interval;
            }
        })
        .detach();
    }

    /// 歌词逐字高亮节拍：走字时 16ms，空闲时按需睡，没有窗口时睡 1s。
    pub(crate) fn start_lyrics_ticker(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            const HOT: std::time::Duration = std::time::Duration::from_millis(16);
            const IDLE: std::time::Duration = std::time::Duration::from_millis(120);
            const NO_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
            let mut probe = LoopProbe::new("lyrics");
            loop {
                probe.tick();
                let has_window = cx.update(|cx| !cx.windows().is_empty());
                probe.app_entry();
                let (interval, notify) = if has_window {
                    match this.update(cx, |root, _cx| {
                        probe.app_entry();
                        if root.nav != Nav::Lyrics {
                            return (IDLE, false);
                        }
                        if root.lyrics_demo_position.is_some() {
                            return (HOT, true);
                        }
                        if root.lyrics_scroll_animating() {
                            return (IDLE, false);
                        }
                        probe.engine_snapshot();
                        let snapshot = root.engine.snapshot();
                        if !snapshot.playing {
                            return (IDLE, false);
                        }
                        let revealing = root
                            .lyrics_active
                            .and_then(|index| root.lyrics.get(index))
                            .is_some_and(|line| {
                                line_undergoing_reveal(line, root.lyrics_position_seconds)
                            });
                        if revealing {
                            return (HOT, true);
                        }
                        let next_start = root
                            .lyrics
                            .iter()
                            .map(|line| line.start_seconds)
                            .filter(|start| *start > snapshot.position_seconds)
                            .fold(f64::INFINITY, f64::min);
                        let next_start = next_start.is_finite().then_some(next_start);
                        let near = lyrics_near_next_line(snapshot.position_seconds, next_start);
                        (
                            lyrics_idle_wait(snapshot.position_seconds, next_start),
                            near,
                        )
                    }) {
                        Ok(value) => value,
                        Err(_) => break,
                    }
                } else {
                    (NO_WINDOW, false)
                };
                if notify
                    && this
                        .update(cx, |_root, cx| {
                            probe.app_entry();
                            cx.notify();
                        })
                        .is_err()
                {
                    break;
                }
                cx.background_executor().timer(interval).await;
            }
        })
        .detach();
    }

    /// 打开抽屉时把滚动位置锚到「正在播放」这一行。
    pub fn anchor_queue_scroll(&self) {
        let row = crate::ui::player_bar::now_playing_slot(self);
        self.queue_scroll
            .scroll_to_item(row, gpui::ScrollStrategy::Center);
    }

    /// 从队列移除某一首（右键菜单）。正在播放的那首不允许移除：
    /// 它还挂在引擎上，移除后「队列当前项」与「实际在播」会分叉。
    pub fn remove_from_queue(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == self.queue.index() {
            self.queue_menu = None;
            let message = self
                .tr("正在播放的曲目不能从队列移除，可直接点「下一首」")
                .to_string();
            self.notify_warning_once("remove-current-track", message);
            cx.notify();
            return;
        }
        if self.queue.remove(index).is_some() {
            self.queue_menu = None;
            self.sync_queue_cache();
            cx.notify();
        }
    }

    /// 队列变化后刷新快照（抽屉渲染只读它，不再每帧 to_vec）。
    pub(crate) fn sync_queue_cache(&mut self) {
        let revision = self.queue.revision();
        if self.queue_cache.0 != revision {
            self.queue_cache = (revision, Arc::new(self.queue.tracks().to_vec()));
        }
    }

    /// 设置音质**意图**（`auto` = 不设上限）。换档后按新意图预取下一首，
    /// 当前这首不重载：中途断开已取到的流不划算，也与主流客户端一致。
    pub fn set_quality(&mut self, quality: &str, cx: &mut Context<Self>) {
        let intent = sodam_core::quality::QualityIntent::parse(quality);
        self.settings.set_quality_intent_by_user(&intent);
        // 会话只认会话输入：换档后立刻重建会话，缓存文件按档位命名，
        // 界面新档、会话旧档错开就会互相找不到。
        if let Some(session) = self.session.as_mut() {
            session.adopt(&self.settings);
        }
        // 落盘排进后台队列：连点几档也只写一次盘（#259）。
        // 与以前「写盘成功才算数」不同：这里先认新档位，真写失败时状态行会补一句没保存上，
        // 本次运行照常用新档。
        self.save_settings();
        // 普通操作反馈走状态行：确认「选了哪一档、什么时候生效」就够了，
        // 不值得再弹一条抢注意力的全局提示（见 #83）。
        let label = crate::ui::player_bar::quality_label(intent.as_setting(), self.language);
        let message = self.localized("音质已设为 {}，从下一首生效", &[label]);
        self.set_status_text(message);
        // 切档后按新意图重新预取下一首（正在播的这首不受影响）
        self.spawn_prefetch(cx);
        self.quality_menu = None;
        // 与 `set_detailed_logs` 一致：动作自己发一次重绘，不靠入口顺手带。
        cx.notify();
    }

    /// 设置页「详细日志」开关：写进配置并即时生效，重启后保持。
    pub fn set_detailed_logs(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.detailed_logs = enabled;
        log_outlet::set_global_detailed(enabled);
        // 详细日志不算会话输入，会话不用同步；落盘排进后台队列。
        self.save_settings();
        let message = if enabled {
            self.tr("已开启详细日志：六个话题都会记到 DEBUG，重启后保持")
        } else {
            self.tr("已关闭详细日志：回到 INFO，环境变量仍然有效")
        };
        // 同上：开关就在设置页上，一句状态行确认即可，不弹全局提示。
        self.set_status_text(message);
        cx.notify();
    }

    /// 按比例跳转播放位置（0.0~1.0）。
    pub fn seek_fraction(&mut self, fraction: f32) {
        let snapshot = self.engine.snapshot();
        if snapshot.duration_seconds <= 0.0 {
            return;
        }
        let target = snapshot.duration_seconds * fraction.clamp(0.0, 1.0) as f64;
        self.engine.seek(target);
        // 拖动进度后立刻把歌词对齐到新位置，不等下一帧的进度回调。
        self.align_lyrics_to_position(target);
    }

    /// 把歌词对齐到指定播放位置。
    ///
    /// 切歌与拖动进度都走这里：立刻算出当前句并滚动过去，
    /// 布局未就绪时由 `pending_lyrics_scroll` 在下一帧补做。
    pub(crate) fn align_lyrics_to_position(&mut self, position_seconds: f64) {
        if self.lyrics.is_empty() {
            return;
        }
        let active = self
            .lyrics
            .iter()
            .rposition(|line| position_seconds >= line.start_seconds)
            .or(Some(0));
        self.lyrics_active = active;
        // 用户在手动浏览时不抢滚动位置：只有自动跟随才动。
        let user_scrolling = self
            .lyrics_user_scroll_until
            .is_some_and(|until| std::time::Instant::now() < until);
        if !user_scrolling {
            if let Some(index) = active {
                self.scroll_lyrics_to(index);
            }
        }
    }

    /// 设置音量（0.0~1.0）。
    pub fn set_volume(&mut self, volume: f32) {
        self.settings.set_volume(volume);
        self.engine.set_volume(self.settings.volume);
        // 拖滑条时每动一下就排一份整份快照太浪费：拖动中只记时刻，
        // 松手（或停手一会儿）再补一次整份落盘。
        if !self.volume_scrub.touch(std::time::Instant::now()) {
            self.save_settings();
        }
    }

    /// 音量拖动开始（按下滑条）：之后的外部重载不弹回音量，落盘等停手再说。
    pub(crate) fn begin_volume_scrub(&mut self) {
        let started = !self.volume_scrub.scrubbing();
        self.volume_scrub.begin(std::time::Instant::now());
        if started {
            self.wake_heartbeat();
        }
    }

    /// 音量拖动结束（松手 / 拖出滑条）：补排一次落盘。
    ///
    /// 松手事件不一定会来（拖到窗口外松手、拖动中切走窗口），所以心跳还会用
    /// [`Self::settle_volume_scrub`] 兜一层。
    pub(crate) fn end_volume_scrub(&mut self) {
        if self.volume_scrub.finish() {
            self.save_settings();
        }
    }

    /// 停手够久就当这次拖动结束了：补排落盘、清掉记号。
    ///
    /// 只靠 `on_mouse_up` 收尾的话，收不到松手时会丢两样：音量没落盘（重启回到旧值），
    /// 拖动记号一直挂着（之后外部热重载送来的音量全被忽略）。
    pub(crate) fn settle_volume_scrub(&mut self) {
        if self.volume_scrub.settled(std::time::Instant::now()) {
            self.end_volume_scrub();
        }
    }

    /// 静音 / 恢复。以设置中的即时值判断，快速连点不依赖音频线程的回执时机。
    pub fn toggle_mute(&mut self) {
        self.settings.toggle_mute();
        self.engine.set_volume(self.settings.volume);
        self.save_settings();
    }

    pub(crate) fn set_cache_limit_mb(&mut self, mb: u64, cx: &mut Context<Self>) {
        self.settings.audio_cache_limit_mb = mb.clamp(1024, 1024 * 1024);
        self.save_settings();
        self.maintain_cache_now(cx);
        cx.notify();
    }

    /// 心跳与启动用：按到期规则问一句；欠着补跑的话，这一拍就补。
    pub(crate) fn check_cache_maintenance(&mut self, cx: &mut Context<Self>) {
        self.run_cache_maintenance(MaintenanceTrigger::Due, cx);
    }

    /// 用户改了缓存上限这种必须立刻收口的场合：跳过到期判定。
    pub(crate) fn maintain_cache_now(&mut self, cx: &mut Context<Self>) {
        self.run_cache_maintenance(MaintenanceTrigger::Forced, cx);
    }

    /// 下载记账说占用越过了九成：不等例行检查，马上看一次盘。
    pub(crate) fn maintain_cache_from_ledger(&mut self, cx: &mut Context<Self>) {
        self.run_cache_maintenance(MaintenanceTrigger::Ledger, cx);
    }

    fn cache_maintenance_limits(&self) -> sodam_core::cache::MaintenanceLimits {
        sodam_core::cache::MaintenanceLimits::from_mb(
            self.settings.audio_cache_limit_mb,
            sodam_core::cache::COVER_LIMIT_MB,
        )
    }

    fn run_cache_maintenance(&mut self, trigger: MaintenanceTrigger, cx: &mut Context<Self>) {
        let limits = self.cache_maintenance_limits();
        // 挡路、到期、欠账、越线都由 `cache_check` 给结论，这里只照着做。
        // 心跳那行的 `ledger_pressure` 预检只决定这一拍走 Ledger 还是 Due，
        // 用的是同一个判据函数；真要跑时 `ask` 会按同样的输入再确认一次。
        let decision = self.cache_check.ask(MaintenanceAsk {
            trigger,
            // 批量升级期间不收缩缓存：被淘汰的正好是还没轮到的那几首，剩下的会一个接一个
            // 以「文件已被删」失败。巡检每 60 秒一拍，等批量收尾再收。
            blockers: Blockers {
                running: self.cache_maintenance_running,
                clearing: self.cache_clearing,
                upgrading: self.batch_upgrading,
            },
            mark: self.cache_mark.as_ref(),
            limits,
            // 账本增量由这里读、传进去：判断那边不碰全局原子，测试才好造数。
            pending_audio: sodam_core::cache::pending_audio_bytes(),
            now: std::time::SystemTime::now(),
        });
        if decision != MaintenanceDecision::Run {
            return;
        }
        self.cache_maintenance_running = true;
        self.cache_summary_request_seq = self.cache_summary_request_seq.wrapping_add(1);
        let stats_seq = self.cache_summary_request_seq;
        let limit = self.settings.audio_cache_limit_mb;
        // 在渲染线程采样「最近显示时间」：UI 命中内存缓存时不碰磁盘，mtime 只代表
        // 最后下载/落盘，只有这份快照能说明用户最近还在看。后台只拿值，不持实体、不碰 UI。
        // 只在真要跑一轮时采：按到期规则跳过的那些拍子不该在 UI 线程上分配这张表。
        let last_seen = crate::ui::cover_cache::shown_snapshot(cx);
        if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
            let sampled = last_seen.len().to_string();
            log_event(
                LogLevel::Debug,
                LogTopic::App,
                "cover_last_seen_sample",
                &[("paths", sampled.as_str())],
            );
        }
        let work = cx.background_spawn(async move {
            // 一轮维护：音频树、封面树各只扫一次，配额淘汰、暂存清扫、统计都吃
            // 同一份盘点结果；旧格式残留的进程内首轮清扫也在里面。
            let report = sodam_core::cache::maintain(limits, &last_seen);
            let mark = report.mark(std::time::SystemTime::now());
            // 落盘失败不影响这轮结论：最坏下次启动多扫一轮。
            let _ = sodam_core::cache::save_mark(&mark);
            (report, mark)
        });
        cx.spawn(async move |this, cx| {
            let (report, mark) = work.await;
            let _ = this.update(cx, |root, cx| {
                root.cache_maintenance_running = false;
                root.cache_mark = Some(mark);
                // 旧扫描晚到不能把新数字盖回旧值（#248）。统计落了再顺手补设置页的预估：
                // 未登录启动直接停在设置页，不走 `on_nav_changed`，这条是它唯一的算预估机会。
                let mut changed = false;
                if root.cache_summary_request_seq == stats_seq {
                    let summary = report.summary();
                    changed |= root.cache_summary != summary;
                    root.cache_summary = summary;
                    if root.nav == Nav::Settings {
                        root.refresh_upgrade_estimate(cx);
                    }
                } else {
                    // 这次统计被更新的扫描抢先作废，但这轮维护已经删过文件；
                    // 那个更新的扫描可能是在删之前读的盘，重扫一次，别等下一拍。
                    root.refresh_cache_stats(cx);
                }
                // 只同步这次真被淘汰的封面路径，不在主线程上全表扫盘。
                changed |= !report.removed_covers.is_empty();
                root.drop_removed_covers(&report.removed_covers, cx);
                if root.settings.audio_cache_limit_mb != limit {
                    root.maintain_cache_now(cx);
                }
                // 数字没变、也没删任何文件时不必重绘：一轮空 notify 会把整窗口的
                // 布局和封面命中都白跑一遍。
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 淘汰删了磁盘上的封面后，同步清掉内存里指向它们的路径映射。
    ///
    /// 只处理 `removed` 里的文件路径，按 value 精确反查 URL——缩略图和原图各查各的，
    /// 只删了缩略图就不动原图映射。清掉缩略图那笔账后，渲染发现缺图会重新排队；
    /// 当前曲目的原图被删时直接补一次下载，歌词页没有原图的自动入队口子，
    /// 不等它自己回来。
    pub(crate) fn drop_removed_covers(&mut self, removed: &[PathBuf], cx: &mut Context<Self>) {
        if removed.is_empty() {
            return;
        }
        // 淘汰到回调之间下载可能把文件重建回来。还活着的路径不当成被删，
        // 否则刚下好的封面会被误清。（exists 只对命中内存表的那几条调用。）
        let thumb_urls = self
            .cover_store
            .paths_handle()
            .urls_for_removed_paths(removed, |path| !path.exists());
        let original_urls = self
            .cover_store
            .originals_handle()
            .urls_for_removed_paths(removed, |path| !path.exists());
        if thumb_urls.is_empty() && original_urls.is_empty() {
            return;
        }
        // 各清各的账：缩略图靠渲染发现缺图重新 want，原图没有这个口子，
        // 当前曲目被清到时显式补一次。
        self.cover_store
            .forget_thumbnail(thumb_urls.iter().map(String::as_str));
        if original_urls.is_empty() {
            return;
        }
        self.cover_store
            .forget_original(original_urls.iter().map(String::as_str));
        // 歌词页显示的是 pending_track.or(queue.current())，补下载也得按同一个口径，
        // 不然正在装载、还没进队列的那首歌原图被淘汰后不会补回来。
        let current_cover = current_cover_url(self.pending_track.as_ref(), self.queue.current());
        if let Some(url) = current_cover {
            if original_urls.contains(&url) {
                self.ensure_original_cover(&url, cx);
            }
        }
    }

    /// 清缓存后补下当前曲目的原图。
    ///
    /// 已完成结果的作废由调用方先做 `clear_completed()`；这里只负责把当前曲目
    /// 重新排进 pump。缩略图靠渲染每帧重新 want 自愈，原图没有这个口子，清完缓存
    /// 必须显式补一次，否则歌词页大图停在占位图。口径跟歌词页一致：
    /// `pending_track.or(queue.current())`。
    ///
    /// 此刻还在下载的那张不打断：完成时按文件在不在照常落账（#250），
    /// 落完要是文件也没了再补排，不会同一张并发下两遍。
    pub(crate) fn restore_current_original_cover(&mut self, cx: &mut Context<Self>) {
        let Some(url) = current_cover_url(self.pending_track.as_ref(), self.queue.current()) else {
            return;
        };
        self.cover_store.retry_original_after_release(&url);
        self.ensure_original_cover(&url, cx);
    }

    pub(crate) fn clear_cache(&mut self, cx: &mut Context<Self>) {
        if self.cache_clearing {
            return;
        }
        // 批量升级期间清缓存，剩下的每一首都会以「文件已被删」失败，结尾还报成功。
        // 直接拦住：先取消批量，或者等它跑完再清。已经点过取消的另说一句——
        // 那时 `batch_upgrading` 要等这一首跑完才落假，再让人「先取消」等于让他干等。
        if self.batch_upgrading {
            let message = if self.batch_upgrade_cancel {
                self.tr("正在停止批量升级，等这一首跑完再清缓存")
            } else {
                self.tr("批量升级进行中，请先取消再清空缓存")
            };
            self.notify_warning_once("cache-clear-blocked", message);
            cx.notify();
            return;
        }
        self.cache_clearing = true;
        let work = cx.background_spawn(async { sodam_core::audio::clear_cache() });
        cx.spawn(async move |this, cx| {
            let report = work.await;
            let _ = this.update(cx, |root, cx| {
                root.cache_clearing = false;
                // 文件已清理，已完成的结果作废；排队 / 在飞保留，免得同一张下两遍。
                root.cover_store.clear_completed();
                // 文件没了，升级去重表也跟着作废，同一首能重新跑一次。
                root.cache_upgrade_attempted.clear();
                // 已完成结果的作废由上面的 `clear_completed()` 完成；
                // 原图没有渲染期自动入队口子，这里只补排当前曲目。
                root.restore_current_original_cover(cx);
                // 读不全时没看到的文件清不掉，别报成「清干净了」；按 UI-SPEC 走警告。
                if report.incomplete {
                    let message = root.localized(
                        "已清理缓存：{} 个文件；有缓存文件暂时读不到，可能没清干净",
                        &[report.removed.to_string()],
                    );
                    root.notify_warning_once("cache-clear", message);
                } else {
                    let message =
                        root.localized("已清理缓存：{} 个文件", &[report.removed.to_string()]);
                    root.notify_success_once("cache-clear", message);
                }
                root.refresh_cache_stats(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 用系统文件管理器打开诊断日志目录。
    pub fn open_log_directory(&mut self, cx: &mut Context<Self>) {
        apply_external_open(cx, log_directory_open_target());
    }

    /// 只删除诊断日志文件；音频与封面缓存不在同一目录，保持原样。
    pub(crate) fn clear_diagnostic_logs(&mut self, cx: &mut Context<Self>) {
        if self.log_clearing {
            return;
        }
        self.log_clearing = true;
        let work = cx.background_spawn(async {
            sodam_core::log_outlet::global().map(|outlet| outlet.clear_logs())
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.log_clearing = false;
                match result {
                    Some(Err(err)) => {
                        let message = root.failure_text(
                            "清除诊断日志失败",
                            &err,
                            "清除诊断日志失败，文件可能被其他程序占用",
                        );
                        root.notify_error_once("log-clear-failed", message);
                    }
                    _ => {
                        let message = root.tr("已清除诊断日志").to_string();
                        root.notify_success_once("log-clear", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 跳到队列中的某一首并播放。
    pub fn jump_in_queue(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(track) = self.queue.tracks().get(index).cloned() else {
            return;
        };
        // 用户点了「接下来」里已知不可播的那一首：不跳、不重打网络，原地给原因。
        if self.block_explicit_play(&track, cx) {
            return;
        }
        if log_outlet::enabled(LogLevel::Debug, LogTopic::Player) {
            let index = index.to_string();
            log_event(
                LogLevel::Debug,
                LogTopic::Player,
                "queue_jump",
                &[("index", index.as_str())],
            );
        }
        if self.queue.jump(index).is_some() {
            self.sync_queue_cache();
            self.start_track(true, cx);
        }
    }

    /// 创建二维码并在后台轮询扫码状态；成功后写入 Cookie。
    pub fn start_login(&mut self, cx: &mut Context<Self>) {
        let language = self.language;
        let settings = self.settings.clone();
        self.login = LoginState::Loading;
        cx.notify();

        let create_settings = settings.clone();
        let create =
            cx.background_spawn(async move { Session::new(create_settings).create_qr_login() });
        cx.spawn(async move |this, cx| {
            let created = match create.await {
                Ok(created) => created,
                Err(err) => {
                    let _ = this.update(cx, |root, cx| {
                        root.login = LoginState::Failed(root.failure_text(
                            "创建二维码失败",
                            &err,
                            REMOTE_TEXT,
                        ));
                        let status = language
                            .text("创建二维码失败，可点「重新获取二维码」重试")
                            .to_string();
                        root.set_status_text(status);
                        cx.notify();
                    });
                    return;
                }
            };
            let matrix = match sodam_core::login::qr_matrix(&created.scan_url) {
                Ok(matrix) => matrix,
                Err(err) => {
                    let _ = this.update(cx, |root, cx| {
                        root.login = LoginState::Failed(root.failure_text(
                            "二维码编码失败",
                            &err,
                            RETRY_TEXT,
                        ));
                        cx.notify();
                    });
                    return;
                }
            };
            let token = created.token.clone();
            let expires_at = created.expire_time;
            let _ = this.update(cx, |root, cx| {
                root.login = LoginState::Waiting {
                    matrix,
                    token: token.clone(),
                    status: language.text("等待扫码…").to_string(),
                    expires_at,
                    need_second_verify: false,
                    verify_window: VerifyWindowState::NotRequested,
                };
                root.set_status_text(language.text("请用汽水音乐 App 扫码并确认").to_string());
                cx.notify();
            });
            let expires_at_text = expires_at.to_string();
            log_event(
                LogLevel::Info,
                LogTopic::App,
                "login_qr_created",
                &[("expires_at", expires_at_text.as_str())],
            );

            // 轮询：libresoda 内部有 2.5s 最小间隔 + 5s 限流冷却，这里 3s 一次足够温和
            // 二次验证窗口只自动拉起一次（失败也不重试，用户可关掉重开登录流程）；
            // 完成后轮询自动领取结果
            let mut verify_window = VerifyWindowState::NotRequested;
            let opened_text = language
                .text("验证窗口已打开；完成后这里会自动更新")
                .to_string();
            let mut last_login_phase = String::new();
            for _ in 0..80 {
                // 6 秒一次：3 秒会触发服务端限流（error_code=7），确认握手会被丢掉
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(6))
                    .await;
                let poll_settings = settings.clone();
                let poll_token = token.clone();
                let poll = cx.background_spawn(async move {
                    Session::new(poll_settings).check_qr_login(&poll_token)
                });
                let result = match poll.await {
                    Ok(result) => result,
                    Err(err) => {
                        // 单次失败（限流 / 网络抖动）不该结束整个登录流程，继续等下一轮
                        let _ = this.update(cx, |root, cx| {
                            // 先记原文、先取文案：下面要对 root.login 取可变借用。
                            root.log_failure("登录轮询异常", &err);
                            let status_text = root.tr(RETRY_TEXT).to_string();
                            if let LoginState::Waiting {
                                status,
                                verify_window,
                                ..
                            } = &mut root.login
                            {
                                // 验证窗口打开失败的原因留着：一次网络抖动不该把它换掉。
                                if *verify_window != VerifyWindowState::Failed {
                                    *status = status_text;
                                }
                            }
                            cx.notify();
                        });
                        continue;
                    }
                };
                let status = if result.message.trim().is_empty() {
                    status_label(result.status, language).to_string()
                } else {
                    result.message.clone()
                };
                let cookie = result.cookie.trim().to_string();
                let finished = result.is_finished();
                let status_key = match result.status {
                    LoginStatus::Waiting => "waiting",
                    LoginStatus::Scanned => "scanned",
                    LoginStatus::Success => "success",
                    LoginStatus::Expired => "expired",
                    LoginStatus::Failed => "failed",
                };
                let phase = if result.need_second_verify {
                    "second_verify"
                } else if result.rate_limited {
                    "rate_limited"
                } else if verify_window == VerifyWindowState::Opened && !finished {
                    "second_verify_submitted"
                } else {
                    status_key
                };
                if last_login_phase != phase {
                    log_event(
                        LogLevel::Info,
                        LogTopic::App,
                        "login_poll",
                        &[
                            ("phase", phase),
                            ("status", status_key),
                            (
                                "rate_limited",
                                if result.rate_limited { "true" } else { "false" },
                            ),
                        ],
                    );
                    last_login_phase = phase.to_string();
                }
                if finished {
                    log_event(
                        LogLevel::Info,
                        LogTopic::App,
                        "login_success",
                        &[(
                            "second_verify",
                            if verify_window == VerifyWindowState::Opened {
                                "true"
                            } else {
                                "false"
                            },
                        )],
                    );
                    let _ = this.update(cx, |root, cx| {
                        // 先同步写盘，写成功了才算登录：写盘失败时 Cookie 不进内存，
                        // 界面不会一边显示已登录、一边重启又掉线。这枚 Cookie 必须
                        // 当场落盘——用户可能登录成功三秒内就退出应用。
                        match root.settings.apply_login_cookie(&cookie) {
                            Ok(()) => {
                                // 会话只持会话输入那几个字段，换 Cookie 就是重建会话；其它设置
                                // （登录期间改过的主题、音量等）本来就没进过会话，不会被写回旧值。
                                match root.session.as_mut() {
                                    Some(session) => session.adopt(&root.settings),
                                    None => {
                                        root.session = Some(Session::new(&root.settings));
                                    }
                                }
                                // 扫码登录成功：解除登出记号，环境变量补全恢复（#192）。
                                root.env_cookie_opted_out = false;
                                // 换了一份登录凭据：旧账号的不可播结论与逐曲冷却
                                // 一并作废。账号 id / 会员状态的逐项比较在
                                // `refresh_account` 里，但账号信息读取失败时拿不到
                                // 新身份，这里先保守作废。
                                root.invalidate_identity_conclusions();
                                // 听歌模式同样按账号给：登录前留下的模式、探索卡和
                                // 失败终态不能带进新会话，在飞的请求也按序号丢掉。
                                root.reset_scene_state();
                                // 正停在听歌模式页就用新身份读一遍，否则页面要等
                                // 用户离开再回来才有内容。
                                if root.nav == Nav::Scenes {
                                    root.load_scenes(cx);
                                }
                                // 新 Cookie 已生效，旧账号 id 不能再和它配对；范围缺失时按放行处理。
                                root.account = None;
                                if root.nav == Nav::Settings {
                                    root.refresh_upgrade_estimate(cx);
                                }
                                // 登录类挂起的恢复事件 = 重新登录成功（票 #112）。
                                log_prefetch_resume(root.prefetch_gate.note_login_success());
                                // 队列里可能还攒着登录前端改动拍的快照：拿现在这份盖掉它，
                                // 免得 300ms 后落盘时把别的字段写回登录前的值（#259）。
                                root.save_settings();
                                root.set_status_text(
                                    language.text("登录成功，正在读取账号信息…").to_string(),
                                );
                            }
                            Err(err) => {
                                let message =
                                    root.failure_text("登录成功但保存失败", &err, SAVE_TEXT);
                                root.notify_error_once("login-save-failed", message);
                            }
                        }
                        root.login = LoginState::Idle;
                        root.login_modal_open = false;
                        cx.notify();
                    });
                    let _ = this.update(cx, |root, cx| {
                        root.refresh_account(cx);
                        root.load_liked_ids(cx);
                    });
                    return;
                }
                if result.rate_limited {
                    // 被限流就再多等一会儿，避免继续加压
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(9))
                        .await;
                }
                let expired = result.is_terminal_failure();
                // 二次验证窗口只拉起一次。打开失败后保留错误文案，等用户重开登录流程
                // （`start_login` 会重新建一份 NotRequested 的状态）。
                let step = verify_window_step(verify_window, result.need_second_verify);
                if step == VerifyWindowStep::Open {
                    // 自动拉起验证窗口（libresoda 会经签名页桥接官方验证组件）；
                    // 用户在窗口里完成验证后，下一次轮询会拿到登录成功
                    let open_settings = settings.clone();
                    let open_token = token.clone();
                    let open = cx.background_spawn(async move {
                        Session::new(open_settings).open_second_verify(&open_token)
                    });
                    let (window_state, message) = match open.await {
                        Ok(_) => {
                            log_event(
                                LogLevel::Info,
                                LogTopic::App,
                                "login_second_verify_opened",
                                &[],
                            );
                            (
                                VerifyWindowState::Opened,
                                language
                                    .text("已打开二次验证窗口，请在其中完成验证")
                                    .to_string(),
                            )
                        }
                        Err(err) => {
                            // 这里没有 `root` 可借；脱敏由日志出口在写盘前统一做，
                            // 不依赖调用方是否拿得到 `root`。
                            let detail = err.to_string();
                            log_event(
                                LogLevel::Warn,
                                LogTopic::App,
                                "second_verify_open_failed",
                                &[("detail", detail.as_str())],
                            );
                            (
                                VerifyWindowState::Failed,
                                language
                                    .text("打开验证窗口失败，请改用官方客户端导出 Cookie。")
                                    .to_string(),
                            )
                        }
                    };
                    // 成功才置 Opened；失败置 Failed：下一轮不会再开窗，
                    // 也不会把错误文案换成「已打开」。
                    verify_window = window_state;
                    let _ = this.update(cx, |root, cx| {
                        root.set_status_text(message.clone());
                        if let LoginState::Waiting {
                            status: slot,
                            need_second_verify: flag,
                            verify_window: state,
                            ..
                        } = &mut root.login
                        {
                            *flag = true;
                            *state = window_state;
                            *slot = message;
                        }
                        cx.notify();
                    });
                }
                let _ = this.update(cx, |root, cx| {
                    if let LoginState::Waiting {
                        status: slot,
                        need_second_verify: flag,
                        verify_window: state,
                        ..
                    } = &mut root.login
                    {
                        apply_verify_window_result(
                            *state,
                            flag,
                            slot,
                            status,
                            result.need_second_verify,
                            &opened_text,
                        );
                    }
                    cx.notify();
                });
                if expired {
                    // 过期就自动换一张新码（会话只有 3 分钟，用户常常来不及扫/确认）
                    log_event(LogLevel::Warn, LogTopic::App, "login_qr_expired", &[]);
                    let _ = this.update(cx, |root, cx| {
                        root.set_status_text(
                            language.text("二维码已过期，正在自动换新码…").to_string(),
                        );
                        root.start_login(cx);
                    });
                    return;
                }
            }
        })
        .detach();
    }

    /// 读取账号信息。音质配置不由账号状态推导，这里只刷新昵称、头像与 VIP。
    pub fn refresh_account(&mut self, cx: &mut Context<Self>) {
        if self.settings.cookie.trim().is_empty() {
            return;
        }
        // 发起时的登录身份：回来时若已经换过，这份账号信息属于旧身份，丢弃。
        let auth_epoch = self.auth_epoch;
        let settings = self.settings.clone();
        let work = cx.background_spawn(async move { Session::new(settings).fetch_account() });
        cx.spawn(async move |this, cx| {
            let account = work.await;
            let _ = this.update(cx, |root, cx| {
                if root.auth_epoch != auth_epoch {
                    return;
                }
                match account {
                    Ok(info) => {
                        let vip = info.vip;
                        if !info.avatar_url.trim().is_empty() {
                            root.ensure_covers(std::slice::from_ref(&info.avatar_url.clone()), cx);
                        }
                        // 不可播结论的作用域是「账号 × 会话」：账号 id 或会员状态
                        // 任一变化就整份作废，否则换号 / 刚续费的用户仍会被旧结论挡住。
                        let previous = root
                            .account
                            .as_ref()
                            .map(|account| (account.user_id.as_str(), account.vip));
                        let invalidated = root.unplayable.invalidate_if_account_changed(
                            previous,
                            (info.user_id.as_str(), info.vip),
                        );
                        if invalidated {
                            // 身份变了：旧账号的不可播结论与逐曲冷却一并作废；
                            // 在途预取带着旧身份起跑，代数推进后它们不能再写回
                            // 新集合或挂起管线。
                            root.invalidate_identity_conclusions();
                        }
                        root.account = Some(info.clone());
                        root.play_reporter.set_user_id(info.user_id.clone());
                        root.login = LoginState::LoggedIn(info.clone());
                        if root.nav == Nav::Settings {
                            root.refresh_upgrade_estimate(cx);
                        }
                        // 账号请求成功说明网络就绪；若收藏 ids 此前拉取失败，自动重试。
                        if root.liked_ids_failed {
                            root.load_liked_ids(cx);
                        }
                        let message = root.localized(
                            "已登录：{}（{}）",
                            &[
                                if info.nickname.is_empty() {
                                    root.tr("账号")
                                } else {
                                    info.nickname.as_str()
                                }
                                .to_string(),
                                if vip {
                                    root.tr("VIP")
                                } else {
                                    root.tr("非 VIP")
                                }
                                .to_string(),
                            ],
                        );
                        root.notify_success_once("login-ready", message);
                        // 音质配置不再随账号改写；其余设置由 `set_*` 各自即时落盘，
                        // 这里整体回填会把请求期间用户改动的字段覆盖回旧值。
                    }
                    Err(err) => {
                        let message = root.failure_text("读取账号信息失败", &err, REMOTE_TEXT);
                        root.notify_error_once("account-load-failed", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 只应用主题、不保存：设置页与热重载共用这一套应用逻辑（#185）。
    ///
    /// 空/未识别的值回退到跟随系统；调用方自己决定要不要回写 `settings.theme`。
    pub(crate) fn apply_theme_state(&mut self, key: &str, cx: &mut Context<Self>) {
        match ThemeKind::from_key(key) {
            Some(theme) => {
                self.theme = theme;
                self.theme_follows_system = false;
            }
            None => {
                self.theme_follows_system = true;
                self.theme = theme::theme_from_appearance(cx.window_appearance());
            }
        }
        theme::set_theme(self.theme);
    }

    /// 设置界面主题并写入配置；一旦手动选择，不再跟随系统。
    pub fn set_ui_theme(&mut self, theme: ThemeKind, cx: &mut Context<Self>) {
        self.apply_theme_state(theme.key(), cx);
        self.settings.theme = theme.key().to_string();
        self.save_settings();
        // 文案要过词典：状态行现在真的会显示，英文界面不能露出中文。
        let label = if theme == ThemeKind::Light {
            self.tr("浅色")
        } else {
            self.tr("深色")
        };
        let status = self.localized("主题已切换为{}", &[label.to_string()]);
        self.set_status_text(status);
        cx.notify();
    }

    /// 主题恢复为跟随系统：清空配置里的 theme（空串 = auto），渲染时按窗口外观取值。
    pub fn set_ui_theme_system(&mut self, cx: &mut Context<Self>) {
        self.apply_theme_state("", cx);
        self.settings.theme = String::new();
        self.save_settings();
        self.set_status_text(self.tr("主题已恢复为跟随系统"));
        cx.notify();
    }

    /// 用系统默认应用打开配置文件。
    pub fn open_config_file(&mut self, cx: &mut Context<Self>) {
        apply_external_open(cx, config_open_target());
    }

    /// 用系统默认浏览器打开项目 GitHub 仓库。
    pub fn open_github_repository(&mut self, cx: &mut Context<Self>) {
        apply_external_open(cx, repository_open_target());
    }

    /// 打开歌词播放页，并按需加载当前（或正在装载）歌曲的歌词。
    pub fn open_lyrics(&mut self, cx: &mut Context<Self>) {
        let track = self
            .pending_track
            .clone()
            .or_else(|| self.queue.current().cloned());
        let Some(track) = track else {
            self.set_status_text(self.tr("还没有正在播放的歌曲"));
            cx.notify();
            return;
        };
        if self.nav != Nav::Lyrics {
            self.lyrics_return_nav = Some(self.nav);
        }
        self.set_nav(Nav::Lyrics, cx);
        self.load_lyrics(track, false, cx);
    }

    /// 原图没有渲染期自动入队口子，调用方在切歌、进播放页、清缓存、点重试时显式要。
    ///
    /// 4 张在飞 / 32 张排队 / 切歌只留当前曲目，三条规则都由 `CoverStore` 管。
    pub(crate) fn ensure_original_cover(&mut self, url: &str, cx: &mut Context<Self>) {
        if self.cover_store.want_original(url) {
            self.pump_original_covers(cx);
        }
    }

    /// 驱动原图下载：保持最多 4 个在飞，完成一个补一个。
    pub(crate) fn pump_original_covers(&mut self, cx: &mut Context<Self>) {
        for url in self.cover_store.drain_original() {
            let settings = self.settings.clone();
            let work_url = url.clone();
            let fetch_url = url;
            let work = cx.background_spawn(async move {
                Session::new(settings).original_cover_path(&work_url)
            });
            cx.spawn(async move |this, cx| {
                let path = work.await;
                let _ = this.update(cx, |root, cx| {
                    // 和缩略图一样在回调里复核文件在不在：清缓存的清表也是
                    // 同一线程的 update，后清表会把这次抢先写进去的死路径一起扫掉。
                    let outcome = settle_original_cover(path, |file| file.exists());
                    let retryable =
                        matches!(outcome, CoverOutcome::Vanished | CoverOutcome::Failed);
                    root.cover_store.complete_original(&fetch_url, outcome);
                    // 原图下载成功但缩略图失败时，列表 / 队列也能立刻拿到封面。
                    if let Some(path) = root.cover_store.original_path(&fetch_url) {
                        root.cover_store.adopt_thumbnail(&fetch_url, path);
                    }
                    if retryable {
                        // 文件被清缓存 / 淘汰删掉，或下载失败：`complete_original`
                        // 已经记了一次尝试；两次到顶就不再重排。
                        // 清缓存进行中交给它的补偿；当前曲目在这里补一次，
                        // 歌词页不用等下一次切歌或手动点重试。
                        let displayed_cover =
                            current_cover_url(root.pending_track.as_ref(), root.queue.current());
                        if retry_current_original_cover(
                            root.cache_clearing,
                            displayed_cover.as_deref(),
                            &fetch_url,
                        ) {
                            root.ensure_original_cover(&fetch_url, cx);
                        }
                    }
                    // 一个下载完成就补下一个，始终有界并发。
                    root.pump_original_covers(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(crate) fn load_lyrics(&mut self, track: TrackItem, force: bool, cx: &mut Context<Self>) {
        if !force && self.lyrics_track_id == track.id && !self.lyrics.is_empty() {
            return;
        }
        // 样张模式：歌词由样张提供，不让真实歌词覆盖（仅用于逐字高亮的视觉验证）。
        if self.lyrics_demo_position.is_some() {
            return;
        }
        self.lyrics_request_seq = self.lyrics_request_seq.wrapping_add(1);
        let request_seq = self.lyrics_request_seq;
        self.loading_lyrics = true;
        self.lyrics_error = None;
        self.lyrics_title = track.title.clone();
        self.lyrics_artist = track.artist.clone();
        self.lyrics_cover = track.cover.clone();
        self.lyrics_track_id = track.id.clone();
        // 切歌了：排队里的原图多半是已经划过去的歌，只留当前曲目的封面；
        // 被剔掉的退回可重排，那些歌下次播放时会自然重新请求。
        self.cover_store.retain_original_only(&track.cover);
        self.ensure_covers(std::slice::from_ref(&track.cover), cx);
        self.ensure_original_cover(&track.cover, cx);
        self.set_status("正在读取「{}」歌词…", std::slice::from_ref(&track.title));
        cx.notify();

        let settings = self.settings.clone();
        let work_track = track.clone();
        let work = cx.background_spawn(async move { Session::new(settings).lyrics(&work_track) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if root.lyrics_request_seq != request_seq {
                    return;
                }
                root.loading_lyrics = false;
                match result {
                    Ok(raw) => {
                        root.lyrics = Arc::new(raw);
                        root.lyrics_active = None;
                        // 新歌词到位后立刻对齐到当前播放位置，不等下一帧进度回调；
                        // 布局还没就绪时由 pending 标记在下一帧补做。
                        let position = root.engine.snapshot().position_seconds;
                        root.align_lyrics_to_position(position);
                        if root.lyrics.is_empty() {
                            let message = root.localized(
                                "「{}」没有可用歌词",
                                std::slice::from_ref(&track.title),
                            );
                            root.set_status_text(message);
                        } else {
                            let message = root
                                .localized("「{}」歌词已加载", std::slice::from_ref(&track.title));
                            root.set_status_text(message);
                        }
                    }
                    Err(err) => {
                        root.lyrics = Arc::new(Vec::new());
                        root.lyrics_active = None;
                        root.log_failure("读取歌词失败", &err);
                        root.lyrics_error = Some(root.tr(REMOTE_TEXT).to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 把推荐流 / 听歌模式的曲目装进队列；`autoplay` 为假时停在待播状态。
    pub(crate) fn activate_recommended_queue(
        &mut self,
        source: RecommendationSource,
        tracks: Vec<TrackItem>,
        label: &str,
        autoplay: bool,
        cx: &mut Context<Self>,
    ) {
        // 队列已经就位，切换态必须在这里终结。
        //
        // `switching_queue` 会让歌词页整页停在「正在切换」的加载态
        // （`lyrics_view` 开头的守卫）。原先只有推荐流在**加载回调**里清它，
        // 而听歌模式走的是 `start_scene_queue` → 本函数，回调里忘了清，
        // 于是点进「熟悉模式 / 新鲜模式」后歌词页永久转圈，但音频其实已经在放。
        // 放在这里对所有调用方（推荐 / 听歌模式 / 探索电台上游）一并生效。
        self.switching_queue = None;
        self.queue.replace(tracks, 0);
        self.played_history.clear();
        self.playback_error = None;
        self.new_playback_round();
        self.queue_origin = match source.scene.as_ref() {
            Some(scene) => QueueOrigin::FeedMode(scene.sub_queue_type.clone()),
            None => QueueOrigin::Feed,
        };
        // 推荐流固定列表循环：模式按钮在那边不显示，不能沿用歌单里的单曲循环。
        self.queue.mode = sodam_core::queue::PlayMode::ListLoop;
        self.queue.reset_shuffle_round();
        self.recommendation = Some(source);
        self.sync_queue_cache();
        let covers: Vec<String> = self
            .queue
            .tracks()
            .iter()
            .map(|track| track.cover.clone())
            .collect();
        self.ensure_covers(&covers, cx);
        self.open_lyrics(cx);
        // 确认要写在切页之后：状态行在换页时清空，写在 `open_lyrics`
        // （内部 `set_nav(Nav::Lyrics)`）之前等于刚写上就被收走。
        self.set_status("已切换到{}队列", &[label.to_string()]);
        if autoplay {
            self.start_track(true, cx);
        } else {
            self.pause_pending(cx);
        }
    }

    pub(crate) fn sync_scenes_list(&mut self) {
        let width = self.scenes_width.lock().map(|width| *width).unwrap_or(0.0);
        // 官方 SceneMode.vue 的自适应公式：floor((width - 48) / 187)。
        let columns = if width <= 48.0 {
            4
        } else {
            ((width - 48.0) / 187.0).floor().clamp(1.0, 8.0) as usize
        };
        let old_columns = (self.scenes_common_columns, self.scenes_card_columns);
        self.scenes_common_columns = columns;
        self.scenes_card_columns = columns;
        let common_rows = self.scenes.len().div_ceil(columns).max(1);
        let card_rows = self.scene_cards.len().div_ceil(columns).max(1);
        let count = common_rows + card_rows + 3;
        let columns_changed = old_columns != (columns, columns);
        if columns_changed {
            self.finish_middle_drag_for(DragTarget::Scenes);
            self.scenes_list.reset(count);
        } else if count > self.scenes_list_items {
            let old_count = self.scenes_list_items;
            self.scenes_list
                .splice(old_count..old_count, count - old_count);
        } else if count < self.scenes_list_items {
            self.finish_middle_drag_for(DragTarget::Scenes);
            self.scenes_list.reset(count);
        }
        self.scenes_list_items = count;
    }

    pub fn load_scenes(&mut self, cx: &mut Context<Self>) {
        // 只拦自己那条探索卡请求：模式在重试时点「加载更多」，照常补卡。
        if self.loading_scene_cards {
            self.set_status_text(self.tr("正在加载…"));
            cx.notify();
            return;
        }
        let existing_ids: Vec<u64> = self
            .scene_cards
            .iter()
            .filter_map(|card| card.inner_block_id.parse().ok())
            .collect();
        // 模式和探索卡是两次请求：模式读不读只看它自己的状态，
        // 不能拿「探索卡有没有数据」当判据（#213）。
        let needs_modes = self.scene_modes_state.needs_request();
        self.loading_scene_cards = true;
        if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
            let existing = existing_ids.len().to_string();
            let modes = if needs_modes { "true" } else { "false" };
            log_event(
                LogLevel::Debug,
                LogTopic::App,
                "scenes_load_start",
                &[("existing", existing.as_str()), ("modes", modes)],
            );
        }
        let message = if needs_modes {
            self.tr("正在读取听歌模式…").to_string()
        } else {
            self.tr("正在加载更多探索歌单…").to_string()
        };
        self.set_status_text(message);
        cx.notify();
        // 两条请求各跑各的：模式慢或失败都不用等探索卡（#213）。
        if needs_modes {
            self.start_scene_modes_request(false, cx);
        }
        self.start_scene_cards_request(existing_ids, cx);
    }

    /// 发一次模式请求：结果一回来就落状态，不等探索卡。
    ///
    /// `announce` 控制成功时要不要写状态行：首屏加载由探索卡那条收尾，
    /// 重试路径自己说一句「常用模式已更新」。
    fn start_scene_modes_request(&mut self, announce: bool, cx: &mut Context<Self>) {
        self.scene_modes_state = SceneModesState::Loading;
        self.scene_modes_seq = self.scene_modes_seq.wrapping_add(1);
        let request_seq = self.scene_modes_seq;
        let settings = self.settings.clone();
        let work = cx.background_spawn(async move {
            sodam_core::library::scene_modes(&Session::new(settings))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                // 登出 / 换号后旧请求才回来：结果属于上一个身份，丢掉。
                if root.scene_modes_seq != request_seq {
                    return;
                }
                root.apply_scene_modes(result, announce, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 模式结果落回：成功就换列表刷封面，失败记失败终态。
    fn apply_scene_modes(
        &mut self,
        result: anyhow::Result<Vec<SceneItem>>,
        announce: bool,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(modes) => {
                self.scene_modes_state = SceneModesState::Ready;
                if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
                    let count = modes.len().to_string();
                    log_event(
                        LogLevel::Debug,
                        LogTopic::App,
                        "scenes_modes_applied",
                        &[("modes", count.as_str())],
                    );
                }
                if modes.is_empty() {
                    if announce {
                        self.set_status_text(self.tr("暂无常用模式"));
                    }
                    return;
                }
                let covers: Vec<String> = modes
                    .iter()
                    .map(|scene| scene.cover.clone())
                    .filter(|cover| !cover.is_empty())
                    .collect();
                self.scenes = Arc::new(modes);
                self.sync_scenes_list();
                self.ensure_covers(&covers, cx);
                if announce {
                    self.set_status_text(self.tr("常用模式已更新"));
                }
            }
            Err(err) => {
                self.scene_modes_state = SceneModesState::Failed;
                // 失败默认可见：只开到 DEBUG 才记等于报障时又是空白。
                let error = err.to_string();
                log_event(
                    LogLevel::Warn,
                    LogTopic::App,
                    "scenes_modes_failed",
                    &[("error", error.as_str())],
                );
                let message = self.failure_text("读取听歌模式失败", &err, REMOTE_TEXT);
                self.notify_warning_once("scenes-load-failed", message);
            }
        }
    }

    /// 发一次探索卡请求：结果只动探索卡，不碰模式状态。
    fn start_scene_cards_request(&mut self, existing_ids: Vec<u64>, cx: &mut Context<Self>) {
        self.scene_cards_seq = self.scene_cards_seq.wrapping_add(1);
        let request_seq = self.scene_cards_seq;
        let settings = self.settings.clone();
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            let result = if existing_ids.is_empty() {
                sodam_core::library::scene_discover_cards(&session).and_then(|mut cards| {
                    if cards.is_empty() {
                        cards = sodam_core::library::scene_discover_cards_more(
                            &session,
                            &existing_ids,
                        )?;
                    }
                    Ok(cards)
                })
            } else {
                sodam_core::library::scene_discover_cards_more(&session, &[])
            };
            let mut cards = result?;
            cards.retain(|card| {
                card.inner_block_id
                    .parse::<u64>()
                    .map(|id| !existing_ids.contains(&id))
                    .unwrap_or(true)
            });
            Ok::<_, anyhow::Error>(cards)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if root.scene_cards_seq != request_seq {
                    return;
                }
                root.loading_scene_cards = false;
                match result {
                    Ok(cards) => {
                        root.scene_cards_failed = false;
                        if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
                            let count = cards.len().to_string();
                            log_event(
                                LogLevel::Debug,
                                LogTopic::App,
                                "scenes_cards_applied",
                                &[("cards", count.as_str())],
                            );
                        }
                        let covers: Vec<String> = cards
                            .iter()
                            .map(|card| card.playlist.cover.clone())
                            .collect();
                        let added = cards.len();
                        let mut all_cards = root.scene_cards.as_ref().clone();
                        all_cards.extend(cards);
                        root.scene_cards = Arc::new(all_cards);
                        root.sync_scenes_list();
                        root.ensure_covers(&covers, cx);
                        if added == 0 {
                            root.set_status_text(root.tr("没有更多探索歌单"));
                        } else {
                            let message =
                                root.localized("探索歌单已追加 {} 张", &[added.to_string()]);
                            root.set_status_text(message);
                        }
                    }
                    Err(err) => {
                        root.scene_cards_failed = true;
                        let error = err.to_string();
                        log_event(
                            LogLevel::Warn,
                            LogTopic::App,
                            "scenes_cards_failed",
                            &[("error", error.as_str())],
                        );
                        let message = root.failure_text("读取探索歌单失败", &err, REMOTE_TEXT);
                        root.notify_warning_once("scenes-load-failed", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 只重发「常用模式」：不追加探索卡，也不写「没有更多探索歌单」。
    ///
    /// 页面上的重试按钮走这条：模式单独失败时，探索卡那边可能已经有数据了，
    /// 再跑一遍 `load_scenes` 只会顺手读卡，和用户按的那颗按钮对不上（#213）。
    pub fn retry_scene_modes(&mut self, cx: &mut Context<Self>) {
        if matches!(self.scene_modes_state, SceneModesState::Loading) {
            // 模式请求还在飞：给一句反馈，别让按钮看起来没接上。
            self.set_status_text(self.tr("正在加载…"));
            cx.notify();
            return;
        }
        self.set_status_text(self.tr("正在读取听歌模式…"));
        cx.notify();
        self.start_scene_modes_request(true, cx);
    }

    pub fn start_scene_queue(&mut self, scene: SceneItem, cx: &mut Context<Self>) {
        let origin = QueueOrigin::FeedMode(scene.sub_queue_type.clone());
        if self.queue_origin == origin {
            self.open_playback(cx);
            return;
        }
        let scene_name = self.scene_display_text(&scene.sub_queue_type, &scene.text);
        self.begin_queue_switch(origin, &scene_name, cx);
        self.loading_recommendation = true;
        self.recommendation_request_seq = self.recommendation_request_seq.wrapping_add(1);
        let request_seq = self.recommendation_request_seq;
        self.set_status_text(self.localized("正在加载「{}」队列…", &[scene_name]));
        cx.notify();

        let settings = self.settings.clone();
        let played_media = self.play_reporter.played_media();
        let work = cx.background_spawn(async move {
            sodam_core::library::recommended_scene_start(
                &Session::new(settings),
                scene,
                &played_media,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if root.recommendation_request_seq != request_seq {
                    return;
                }
                root.loading_recommendation = false;
                match result {
                    Ok(start) => {
                        root.activate_recommended_queue(
                            start.source,
                            start.tracks,
                            root.tr("听歌模式"),
                            true,
                            cx,
                        );
                    }
                    Err(err) => {
                        // 失败也要结束切换态：否则歌词页会停在「正在切换」的
                        // 加载态上，用户既看不到错误也回不去。
                        //
                        // 并且和推荐流保持一致——回到有重试入口的页面（听歌模式列表），
                        // 而不是留在已经停播的歌词页上。
                        root.switching_queue = None;
                        root.set_nav(Nav::Scenes, cx);
                        let message = root.failure_text("加载听歌模式队列失败", &err, REMOTE_TEXT);
                        root.notify_error_once("scene-queue-load-failed", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 装载第一首但不播放，停在第一秒等待用户点播放。
    ///
    /// 第一首正好放不出来时不停在这首上：照播放路径的做法换到下一首能播的
    /// （票 #109），换过去之后仍然只装载，不替用户按下播放。
    pub(crate) fn pause_pending(&mut self, cx: &mut Context<Self>) {
        self.playing = true;
        self.play_seq = self.play_seq.wrapping_add(1);
        let request_seq = self.play_seq;
        let Some(track) = self.queue.current().cloned() else {
            return;
        };
        self.pending_track = Some(track.clone());
        self.pending_engine_load = None;
        self.progress_preview = Some(0.0);
        self.wake_heartbeat();
        self.set_status("已准备播放：{}", std::slice::from_ref(&track.title));
        self.ensure_covers(std::slice::from_ref(&track.cover), cx);
        cx.notify();
        let settings = self.settings.clone();
        let account_is_vip = self.account.as_ref().map(|a| a.vip);
        // 这次装载属于哪一份登录身份：回来时若已经换过，结论不成立。
        let auth_epoch = self.auth_epoch;
        let work_track = track.clone();
        let work = cx.background_spawn(async move {
            Session::new(settings).download_to_cache(&work_track, account_is_vip)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if root.play_seq != request_seq {
                    return;
                }
                root.pending_track = None;
                root.playing = false;
                root.progress_preview = None;
                // 这次尝试横跨了一次登录 / 换账号：旧账号的结论不能拿来记不可播、
                // 挂预取、落闸或替用户换歌，提示也一起丢掉，新一轮会重写（#108 的做法）。
                // 装载没成功，引擎里还是原来那首，`playing` 交还给引擎快照。
                if root.auth_epoch != auth_epoch {
                    root.playing = root.engine.snapshot().playing;
                    return;
                }
                if result.is_ok() {
                    // 第一首准备好也算一次成功的装载：作废旧一轮「扫一圈」的游标。
                    // 只在成功时做，与 `start_track` 成功分支的位置一致。
                    root.new_playback_round();
                }
                if let Err(err) = result {
                    let context = load_failure_context(err.stage, root.language);
                    root.log_failure(&context, &err);
                    // 全局故障同样挂起预取：这条路先看见了故障，预取没理由继续重打。
                    root.suspend_prefetch(&err);
                    // 换不换歌只看失败的结论是关于这一首还是关于整个账号 / 环境，
                    // 与 `start_track` 同一个判据（票 #109）。换过去之后仍然只装载，
                    // 不替用户按下播放。
                    if err.is_per_track() {
                        root.unplayable.record_failure(&track.id, &err);
                        // 有没有真的换过去，以队列推进的返回值为准：它内部本来就是
                        // 同一个查找，先问一遍再问一遍只会得到两个答案。
                        let advanced = {
                            let unplayable = &root.unplayable;
                            root.queue
                                .advance_playable(|id| unplayable.is_unavailable(id))
                                .is_some()
                        };
                        if advanced {
                            let message = root.tr(play_failure_text(&err, true)).to_string();
                            // 换到了：这条轻提示属于一次性反馈，与这次结论无关的
                            // 旧错误提示同时撤下。
                            root.playback_error = None;
                            root.clear_round_exhaustion();
                            root.notify_warning_once("track-unavailable", message);
                            root.sync_queue_cache();
                            root.pause_pending(cx);
                        } else {
                            // 扫一圈都确认放不出来：和播放路径同一句落闸结论。
                            root.halt_exhausted_round();
                        }
                    } else {
                        // 全局故障与网络抖动：不换歌，把该做的动作留在错误提示里。
                        // 这条路没播过声音，不能说「已跳过」，也不能说「已停止播放」。
                        root.playback_error = Some(root.tr(pending_failure_text(&err)).to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 打开播放页；永远不改变当前队列来源。
    pub fn open_playback(&mut self, cx: &mut Context<Self>) {
        self.set_nav(Nav::Lyrics, cx);
        if self.lyrics_return_nav.is_none() {
            self.lyrics_return_nav = Some(Nav::Home);
        }
        if let Some(track) = self
            .pending_track
            .clone()
            .or_else(|| self.queue.current().cloned())
        {
            self.ensure_playback_assets(track, cx);
        }
        cx.notify();
    }

    /// 点击「推荐」：如果当前已是推荐队列，只打开播放页；否则替换队列。
    pub fn activate_recommendation(&mut self, cx: &mut Context<Self>) {
        // 有失败终态时，点「推荐」的语义就是重试：队列里可能是上一次的旧内容，
        // 跳到播放页会让用户看不到失败提示，也和页面上的重试按钮自相矛盾（#55）。
        if self.recommendation_error.is_some() {
            self.retry_recommendation(cx);
            return;
        }
        if matches!(self.queue_origin, QueueOrigin::Feed) {
            self.set_nav(Nav::Lyrics, cx);
            self.lyrics_return_nav = Some(Nav::Home);
            cx.notify();
            return;
        }
        // 在途请求不能吞掉点击：用户至少要知道「正在加载」。（#55）
        if self.loading_recommendation {
            self.set_status_text(self.tr("推荐队列还在加载，稍等一下…"));
            cx.notify();
            return;
        }
        self.start_recommendation(true, cx);
    }

    /// 推荐页失败终态的「重试」入口。
    ///
    /// 一次失败不能把入口永久锁死：只要不在装载中，重试就重新发起请求。
    pub fn retry_recommendation(&mut self, cx: &mut Context<Self>) {
        if self.loading_recommendation {
            return;
        }
        self.start_recommendation(true, cx);
    }

    pub(crate) fn begin_queue_switch(
        &mut self,
        origin: QueueOrigin,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        // 推荐流没有播放模式（按钮也不显示），统一按列表循环推进，
        // 避免上一张歌单留下的单曲循环让流停在同一首。
        if origin.is_feed_like() {
            self.queue.mode = sodam_core::queue::PlayMode::ListLoop;
            self.queue.reset_shuffle_round();
        }
        self.switching_queue = Some(origin.clone());
        self.switching_label = label.to_string();
        self.set_nav(Nav::Lyrics, cx);
        if self.lyrics_return_nav.is_none() {
            self.lyrics_return_nav = Some(Nav::Home);
        }
        self.engine.stop();
        self.playing = false;
        self.pending_track = None;
        self.progress_preview = None;
        self.lyrics = Arc::new(Vec::new());
        self.lyrics_active = None;
        self.lyrics_error = None;
        self.loading_lyrics = false;
        self.playback_error = None;
        self.new_playback_round();
    }

    /// 歌词滚动过渡的时长范围与换算。
    ///
    /// 固定时长在两种场景下都不对：换一句只挪 64px，280ms 会显得慢吞吞；
    /// 拖动进度条可能跨越几十句、好几千像素，同样 280ms 就变成"唰"一下闪过。
    /// 所以按距离伸缩，并夹在上下限之间。
    const LYRICS_SCROLL_MIN_MS: f32 = 180.0;
    const LYRICS_SCROLL_MAX_MS: f32 = 420.0;
    /// 每移动这么多像素就多给 1ms，其余由上下限约束。
    const LYRICS_SCROLL_MS_PER_PIXEL: f32 = 0.55;

    /// 按滚动距离算出这次过渡该用多久。
    fn lyrics_scroll_duration(distance: f32) -> std::time::Duration {
        let ms = (distance.abs() * Self::LYRICS_SCROLL_MS_PER_PIXEL)
            .clamp(Self::LYRICS_SCROLL_MIN_MS, Self::LYRICS_SCROLL_MAX_MS);
        std::time::Duration::from_secs_f32(ms / 1000.0)
    }

    /// 计算「让第 `index` 句停在歌词视口中心上方一行」所需的滚动偏移。
    ///
    /// 第一句特殊：不居中，改为对齐封面顶部（列表顶端），避免开头就把列表
    /// 滚进上方留白。
    ///
    /// 之前是「顶部 + 80px」，高亮句会整体偏上；列表首尾各留了半个视口的
    /// padding，所以第一句/最后一句也能滚到正中间。16px 是实测校准：
    /// 文字行的视觉重心略低于几何中心（汉字下沉、行高上下留白不对称）。
    fn lyrics_scroll_target(&self, index: usize) -> Option<f32> {
        // 对齐目标是**封面中心**：歌词视口与封面列在同一水平带内，
        // 但歌词视口本身不居中（受窗口布局影响），直接用视口中心会偏。
        // 封面在窗口里的垂直中心 = 播放区（窗口高 - 底部播放条）的中心。
        let viewport = self.lyrics_scroll.bounds();
        // 直接用滚动视口自身的中心（窗口坐标）：布局就是「左封面 + 右歌词」
        // 在同一水平带内居中排列，视口中心即封面中心；之前的窗口高换算把
        // 外层 padding 也算进去，反而造成固定偏移。
        // 再向上偏一行（64px = 单句 32 + 间距 32）：官方播放页高亮句停在中线上方
        // 一行的位置，下方能看到下一句的走势；16px 是文字视觉重心的校准。
        const LYRIC_CENTER_OFFSET: f32 =
            crate::views::lyrics::LYRIC_ROW_GAP + crate::views::lyrics::LYRIC_LINE_HEIGHT + 16.0;
        let viewport_center = viewport.center().y - gpui::px(LYRIC_CENTER_OFFSET);
        let line_center = self.lyrics_scroll.bounds_for_item(index)?.center().y;
        // offset 为负表示内容上移：把该句中心推到视口中心上方一行。
        let distance = line_center - viewport_center;
        let target = -f32::from(distance);
        // 开场规则（issue #180 讨论）：歌词列表**先不动**——高亮句随播放
        // 一句句往下走；直到高亮句在**屏幕上**走到中线附近，才开始常规的
        // 中线跟随滚动。判定用的是「该句在**顶端偏移**下的屏幕位置」
        // （内容坐标 + intro_offset），不是当前滚动偏移下的位置：居中之后
        // 高亮句恰好压在跟随线上，若按当前偏移判定会翻转回顶端目标，
        // 两个目标间来回振荡（实测踩过）。顶端偏移是常量，判定结果与
        // 当前滚动无关，每句的结论恒定。
        // 顶端偏移：把第一句顶部推到视口顶部（滚掉列表的半屏 padding）。
        let first_top = self.lyrics_scroll.bounds_for_item(0)?.top();
        let intro_offset = f32::from(viewport.top() - first_top);
        // 「还在开场」的判定必须只依赖**内容几何**（每句的固定位置），不能依赖
        // 当前偏移：居中之后高亮句的屏幕位置恰好压在跟随线上，若用屏幕位置
        // 判定会在「顶端 ↔ 居中」两个目标间来回翻转（实测上下振荡）。
        // 用「该句在顶端偏移下的屏幕位置是否已越过跟随线」则与偏移无关，
        // 每句的判定结果恒定，越过线后自然切到居中跟随。
        let intro_screen_center = line_center + gpui::px(intro_offset);
        // 目标必须夹进**实际可滚动的范围**：短歌词（尤其只有几句）的尾部
        // 达不到理想目标时，偏移会被 gpui 钳在最大滚动量上，补间永远差一截，
        // 每帧都会重新起补间（表现为结尾句反复小幅抖动）。钳位后补间能落到
        // 终点，动画自然收敛。顶端目标同样钳位，保证开场判定与实际一致。
        // 可滚动范围直接用 gpui 量好的 `max_offset()`：它包含列表上下 padding。
        // 自己拿首尾句拼范围会漏掉 padding，把所有目标都钳成 0，歌词完全不滚
        // （实测踩过：0be11d2 的回归）。
        let max_scroll = f32::from(self.lyrics_scroll.max_offset().y).max(0.0);
        let clamp_to_range = |value: f32| value.clamp(-max_scroll, 0.0);
        // 第一句始终用顶端目标：折行成长句时首句中心在顶端偏移下会低于跟随线，
        // 走几何判定会把它滚出视口顶端，截掉句首。
        if index == 0 {
            return Some(clamp_to_range(intro_offset));
        }
        if intro_screen_center <= viewport_center {
            return Some(clamp_to_range(intro_offset));
        }
        Some(clamp_to_range(target))
    }

    /// 把歌词滚到指定句，**走过渡动画**。
    ///
    /// 定位：当前句的**垂直中心**对齐歌词视口中心（issue #180）。列表顶部
    /// 留了半屏空白，所以首句也能停在视口中间；不再使用顶部固定 80px 的偏上位置。
    ///
    /// 布局还没就绪时（切歌、拖动进度后的第一帧）记下目标句，由下一帧的
    /// `flush_pending_lyrics_scroll` 补上，避免歌词停在上一首的位置。
    ///
    /// 换句时整块歌词瞬移会显得很硬（即便只挪一行的高度），这里改成记下
    /// 起终点，由 [`Self::advance_lyrics_scroll_tween`] 在每帧里插值。
    pub(crate) fn scroll_lyrics_to(&mut self, index: usize) {
        // 动画进行中且**目标句没变**：直接返回，绝不能在动画途中重算目标。
        //
        // 这是"不丝滑"的根因。`bounds_for_item` 返回的是含滚动偏移的窗口坐标
        // （`layout_bounds` 内部会加 `element_offset()`），滚动过程中它每帧都在变。
        // 若每帧重算 target，就会一路追着"此刻追到哪"去改目标，
        // 形成一个正反馈：动画自己把自己的终点往后推，于是永远收不了尾，
        // 看起来像卡顿、或者最后一段突然刹住。
        //
        // 正确做法：目标句确定后就把目标**冻结**下来，等动画跑完再接受新目标。
        if let Some((_, _, _, _)) = self.lyrics_scroll_tween {
            if self.lyrics_scroll_tween_index == Some(index) {
                return;
            }
        }
        let Some(target) = self.lyrics_scroll_target(index) else {
            self.pending_lyrics_scroll = Some(index);
            return;
        };
        self.pending_lyrics_scroll = None;
        let current = f32::from(self.lyrics_scroll.offset().y);
        // 已经在目标位置（比如暂停时重复调用）：清掉动画即可。
        if (target - current).abs() < 0.5 {
            self.lyrics_scroll_tween = None;
            self.lyrics_scroll_tween_index = None;
            return;
        }
        // 起点取**当前实际偏移**而不是上一次的终点：中途换目标时能接得上，不会回弹。
        let duration = Self::lyrics_scroll_duration(target - current);
        self.lyrics_scroll_tween = Some((current, target, std::time::Instant::now(), duration));
        self.lyrics_scroll_tween_index = Some(index);
    }

    /// 推进歌词滚动的过渡动画；返回是否还需要下一帧。
    ///
    /// 缓动见 [`lyrics_scroll_ease`]（CSS `ease-out`）。
    pub(crate) fn advance_lyrics_scroll_tween(&mut self) -> bool {
        let Some((from, to, started, duration)) = self.lyrics_scroll_tween else {
            return false;
        };
        let elapsed = started.elapsed();
        let progress = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0);
        let eased = lyrics_scroll_ease(progress);
        let mut offset = self.lyrics_scroll.offset();
        offset.y = gpui::px(from + (to - from) * eased);
        self.lyrics_scroll.set_offset(offset);
        if progress >= 1.0 {
            self.lyrics_scroll_tween = None;
            self.lyrics_scroll_tween_index = None;
            false
        } else {
            true
        }
    }

    /// 中断进行中的歌词滚动动画（用户手动滚轮时调用）。
    pub(crate) fn cancel_lyrics_scroll_tween(&mut self) {
        self.lyrics_scroll_tween = None;
        self.lyrics_scroll_tween_index = None;
    }

    /// 歌词滚动是否还在过渡中。
    pub(crate) fn lyrics_scroll_animating(&self) -> bool {
        self.lyrics_scroll_tween.is_some()
    }

    /// 补做上一帧没能完成的歌词对齐。
    ///
    /// 切歌或拖动进度后，歌词可能还没完成布局，`bounds_for_item` 返回 `None`；
    /// 这里在渲染路径上重试一次，保证新歌词一出现就停在当前句。
    /// 不在歌词页时直接丢弃，避免每帧空转，也避免回到歌词页时跳到过期位置。
    pub(crate) fn flush_pending_lyrics_scroll(&mut self) {
        let Some(index) = self.pending_lyrics_scroll else {
            return;
        };
        if self.nav != crate::app::Nav::Lyrics || self.lyrics.is_empty() {
            self.pending_lyrics_scroll = None;
            return;
        }
        self.scroll_lyrics_to(index);
    }

    pub(crate) fn ensure_playback_assets(&mut self, track: TrackItem, cx: &mut Context<Self>) {
        if self.lyrics_track_id != track.id || self.lyrics.is_empty() || self.lyrics_error.is_some()
        {
            self.load_lyrics(track.clone(), false, cx);
        }
        self.ensure_covers(std::slice::from_ref(&track.cover), cx);
        self.ensure_original_cover(&track.cover, cx);
    }

    pub fn retry_playback_assets(&mut self, track: TrackItem, cx: &mut Context<Self>) {
        // 两格一起作废：在飞的那张等结果回来再放行，不会同一张并发下两遍。
        self.cover_store.forget([track.cover.as_str()]);
        if let Some(session) = self.session.as_mut() {
            session.invalidate_original_cover(&track.cover);
            session.invalidate_cover_path(&track.cover);
        }
        self.load_lyrics(track.clone(), true, cx);
        self.ensure_covers(std::slice::from_ref(&track.cover), cx);
        self.ensure_original_cover(&track.cover, cx);
        cx.notify();
    }

    /// 点击「推荐」：清空旧队列并切换到官方推荐队列。
    pub fn start_recommendation(&mut self, autoplay: bool, cx: &mut Context<Self>) {
        if self.loading_recommendation || self.settings.cookie.trim().is_empty() {
            return;
        }
        // 开发/验证便利：`SODAM_PENDING_START=1` 时，推荐队列一律只装载、不自动开播，
        // 走的正是「进队列自动装载第一首」那条路（票 #109）。
        //
        // 平时这条路只有启动时那一次调用会走到，第一首由服务端给，不可播的样本未必排在
        // 第一位，验收就只能碰运气；挂上这个开关后，点左栏「推荐」或失败后的「重试」都会
        // 停在待播状态，可以反复验证。和 `SODAM_QUERY` / `SODAM_NAV` 同级：不设就等于不存在。
        // 听歌模式不在这里改：它的「点进去就播」是既有语义。
        let autoplay = autoplay && std::env::var("SODAM_PENDING_START").is_err();
        self.loading_recommendation = true;
        // 重试时先把上一次的失败终态清掉：页面要回到加载态，而不是一边加载
        // 一边还挂着上一轮的失败提示。
        self.recommendation_error = None;
        self.recommendation_request_seq = self.recommendation_request_seq.wrapping_add(1);
        self.recommendation_load_seq = self.recommendation_load_seq.wrapping_add(1);
        let load_seq = self.recommendation_load_seq;
        // 切队列前先看看队列里是不是上一次的推荐内容，失败时保住它，不用开空白页。
        let cached = self.queue_origin == QueueOrigin::Feed && !self.queue.is_empty();
        self.begin_queue_switch(QueueOrigin::Feed, self.tr("推荐队列"), cx);
        self.set_status_text(self.tr("正在加载推荐队列…"));
        cx.notify();

        let settings = self.settings.clone();
        let played_media = self.play_reporter.played_media();
        let work = cx.background_spawn(async move {
            sodam_core::library::recommended_start(&Session::new(settings), &played_media)
        });
        cx.spawn(async move |this, cx| {
            // 请求本身没设截止时间，断网时 HTTP 可能等很久。这里加一层兜底：
            // 到点就算失败，把 loading 终结为可操作的失败态，而不是让一个全局布尔
            // 把导航入口锁死（#55）。超时不算过期，所以靠 load_seq 而不是请求序号判定。
            //
            // `work` 是 gpui 的 `Task`，drop 它会取消后台任务；所以超时后必须把它
            // `detach()` 掉，让请求继续跑完（结果没人收，也不会写回界面）。
            let mut work = work;
            let result = first_of(
                &mut work,
                cx.background_executor().timer(RECOMMENDATION_DEADLINE),
            )
            .await;
            if result.is_none() {
                work.detach();
            }
            let _ = this.update(cx, |root, cx| {
                // 已有更新的装载请求：本次结果作废，但**不能碰 loading 与切换态**。
                if root.recommendation_load_seq != load_seq {
                    return;
                }
                root.loading_recommendation = false;
                root.switching_queue = None;
                match result {
                    Some(Ok(start)) => {
                        root.recommendation_error = None;
                        root.activate_recommended_queue(
                            start.source,
                            start.tracks,
                            root.tr("推荐"),
                            autoplay,
                            cx,
                        );
                    }
                    Some(Err(err)) => {
                        let message = root.failure_text("加载推荐队列失败", &err, REMOTE_TEXT);
                        root.recommendation_failed(message, cached, cx);
                    }
                    None => {
                        let message = root.tr(REMOTE_TEXT).to_string();
                        // 超时是「界面不再等它」，不是请求已结束：底层请求会自己跑完，
                        // 结果被 load_seq 丢弃。日志说清楚免得后续排查误读。
                        root.log_failure(
                            "加载推荐队列超时（界面已停止等待）",
                            format_args!("deadline={}s", RECOMMENDATION_DEADLINE.as_secs()),
                        );
                        root.recommendation_failed(message, cached, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 推荐队列装载失败后的统一落地：进入**可重试的终端态**，而不是停在转圈页。
    ///
    /// 网络错误 / 超时 / 无数据三种情况都走这里。调用点已经用 `load_seq` 过滤过
    /// 过期结果，所以这里不再判断请求序号。
    fn recommendation_failed(
        &mut self,
        message: String,
        had_cached_recommendation: bool,
        cx: &mut Context<Self>,
    ) {
        self.recommendation_error = Some(message.clone());
        // 上次成功加载的推荐内容还在队列里就留着，别把可用内容清空。
        //
        // 注意：`begin_queue_switch` 已经把引擎停了，所以这里只是**保住列表**，
        // 不会自动续播；用户点重试或播放才重新出声。
        if !had_cached_recommendation {
            self.recommendation = None;
            self.queue = Queue::new(Vec::new());
            // 队列都没了，上一轮的扫描游标与「整圈无候选」结论不再成立。
            self.new_playback_round();
            self.queue_origin = QueueOrigin::None;
            self.played_history.clear();
            self.pending_track = None;
            self.sync_queue_cache();
        }
        self.set_nav(Nav::Home, cx);
        self.notify_error_once("recommendation-load-failed", message);
    }

    /// 听歌模式探索卡：歌单走歌单曲队，电台走 FeedRadioTracks。
    pub fn start_scene_card(
        &mut self,
        card: sodam_core::library::SceneCard,
        cx: &mut Context<Self>,
    ) {
        match card.kind {
            sodam_core::library::SceneCardKind::Playlist => {
                self.start_scene_playlist(card.playlist, cx);
            }
            sodam_core::library::SceneCardKind::Radio => {
                let Some(radio) = card.radio.clone() else {
                    return;
                };
                self.begin_queue_switch(
                    QueueOrigin::Radio(radio.radio_id.clone()),
                    &card.playlist.title,
                    cx,
                );
                self.loading_playlist = true;
                self.set_status(
                    "正在加载电台「{}」…",
                    std::slice::from_ref(&card.playlist.title),
                );
                cx.notify();

                let settings = self.settings.clone();
                let played_media = self.play_reporter.played_media();
                let work = cx.background_spawn(async move {
                    let session = Session::new(settings);
                    sodam_core::library::radio_tracks(&session, &radio, &played_media)
                });
                cx.spawn(async move |this, cx| {
                    let result = work.await;
                    let _ = this.update(cx, |root, cx| {
                        root.loading_playlist = false;
                        root.switching_queue = None;
                        match result {
                            Ok(tracks) if !tracks.is_empty() => {
                                root.queue.replace(tracks, 0);
                                root.played_history.clear();
                                root.playback_error = None;
                                root.new_playback_round();
                                root.queue_origin = QueueOrigin::Radio(card.playlist.id.clone());
                                root.recommendation = None;
                                root.sync_queue_cache();
                                let covers: Vec<String> = root
                                    .queue
                                    .tracks()
                                    .iter()
                                    .map(|track| track.cover.clone())
                                    .collect();
                                root.ensure_covers(&covers, cx);
                                root.open_lyrics(cx);
                                root.start_track(true, cx);
                            }
                            Ok(_) => {
                                let message = root.localized(
                                    "电台「{}」没有可播放曲目",
                                    std::slice::from_ref(&card.playlist.title),
                                );
                                root.notify_warning_once("radio-empty", message);
                            }
                            Err(err) => {
                                let message = root.failure_text("加载电台失败", &err, REMOTE_TEXT);
                                root.notify_error_once("radio-load-failed", message);
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// 听歌模式探索卡：直接播放整张歌单，不进入详情页。
    pub fn start_scene_playlist(&mut self, playlist: PlaylistItem, cx: &mut Context<Self>) {
        if self.loading_playlist {
            return;
        }
        self.begin_queue_switch(
            QueueOrigin::Playlist(playlist.id.clone()),
            &playlist.title,
            cx,
        );
        self.loading_playlist = true;
        self.set_status("正在加载歌单「{}」…", std::slice::from_ref(&playlist.title));
        cx.notify();

        let settings = self.settings.clone();
        let playlist_id = playlist.id.clone();
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            sodam_core::library::playlist_tracks(&session, &playlist_id)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_playlist = false;
                match result {
                    Ok(tracks) if !tracks.is_empty() => {
                        root.queue.replace(tracks, 0);
                        root.played_history.clear();
                        root.playback_error = None;
                        root.new_playback_round();
                        root.queue_origin = QueueOrigin::Playlist(playlist.id.clone());
                        root.recommendation = None;
                        root.switching_queue = None;
                        root.sync_queue_cache();
                        let covers: Vec<String> = root
                            .queue
                            .tracks()
                            .iter()
                            .map(|track| track.cover.clone())
                            .collect();
                        root.ensure_covers(&covers, cx);
                        root.open_lyrics(cx);
                        root.start_track(true, cx);
                    }
                    Ok(_) => root.set_status(
                        "歌单「{}」没有可播放曲目",
                        std::slice::from_ref(&playlist.title),
                    ),
                    Err(err) => {
                        let message = root.failure_text("加载歌单失败", &err, REMOTE_TEXT);
                        root.notify_error_once("playlist-load-failed", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 官方推荐队列的动态加载：当前歌后方少于 6 首时由心跳触发。
    pub(crate) fn load_more_recommendation(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.recommendation.clone() else {
            return;
        };
        if !source.has_more || self.loading_recommendation || self.settings.cookie.trim().is_empty()
        {
            return;
        }
        // 推荐流要保证「接下来还有足够的歌可播」：
        //
        // * 列表循环：按顺序剩余量算，和原来一致；
        // * 随机播放：下一首在整个队列里随机挑，所以按「总长度」算；
        // * 单曲循环：一直播当前这首，不需要追加，直接放行。
        let remaining = match self.queue.mode {
            sodam_core::queue::PlayMode::ListLoop => {
                self.queue.len().saturating_sub(self.queue.index() + 1)
            }
            sodam_core::queue::PlayMode::Shuffle => self.queue.len(),
            sodam_core::queue::PlayMode::RepeatOne => {
                self.loading_recommendation = false;
                return;
            }
        };
        if remaining >= 6 {
            return;
        }

        self.loading_recommendation = true;
        self.recommendation_request_seq = self.recommendation_request_seq.wrapping_add(1);
        let request_seq = self.recommendation_request_seq;
        self.set_status_text(self.tr("正在加载更多推荐…"));
        cx.notify();

        let settings = self.settings.clone();
        let played_media = self.play_reporter.played_media();
        let work = cx.background_spawn(async move {
            sodam_core::library::recommended_append(&Session::new(settings), &source, &played_media)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if root.recommendation_request_seq != request_seq {
                    return;
                }
                root.loading_recommendation = false;
                match result {
                    Ok(batch) => {
                        let added = root.queue.append(batch.tracks);
                        root.recommendation = Some(batch.source);
                        root.sync_queue_cache();
                        let covers: Vec<String> = root
                            .queue
                            .tracks()
                            .iter()
                            .skip(root.queue.len().saturating_sub(added))
                            .map(|track| track.cover.clone())
                            .collect();
                        root.ensure_covers(&covers, cx);
                        root.set_status("推荐队列已追加 {} 首", &[added.to_string()]);
                    }
                    Err(err) => {
                        let message = root.failure_text("加载更多推荐失败", &err, REMOTE_TEXT);
                        root.notify_warning_once("recommendation-more-failed", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 账号身份变化：不可播结论与逐曲冷却都属于旧身份，必须一起作废。
    ///
    /// 三个入口（重新登录成功、账号信息刷新、退出登录）都走这里，避免只清其中一张表；
    /// 同时推进 `auth_epoch`，让在途预取回来时知道自己属于旧身份。
    fn invalidate_identity_conclusions(&mut self) {
        self.auth_epoch = self.auth_epoch.wrapping_add(1);
        self.unplayable.clear();
        self.prefetch_cooldown_since.clear();
        self.clear_round_exhaustion();
        // 升级去重表跟着身份走：换号后同一首同一档位在新账号下要能再试一次。
        // 正在跑的批量下一首之前会看到代数变了，自己停。
        self.cache_upgrade_attempted.observe_epoch(self.auth_epoch);
    }

    /// 退出登录：落盘成功后清除 Cookie 并回到未登录状态；落盘失败保持登录。
    ///
    /// 播放和队列一并停止，避免旧账号的歌单/收藏继续留在界面上；
    /// 封面本地缓存只按 URL 索引，可以复用，不涉及凭据。
    ///
    /// 落盘放后台线程：磁盘慢或写锁被占时界面不跟着卡（#197）。
    pub fn logout(&mut self, cx: &mut Context<Self>) {
        // 落盘期间再点只算一次：不起第二个保存，也不把状态清理跑两遍。
        // 正在排队退出时也不接新的登出：退出流程只等它开始那一刻已有的保存，
        // 这时候再起一个新保存没人等，慢盘上会带着旧 Cookie 退出（#197）。
        if self.logout_in_flight || self.quit_gate.is_saving() {
            return;
        }
        // 点登出时拍一份快照：盘上读不出来时拿它兜底（真正写盘时会摘掉 Cookie）。
        let logout_snapshot = self.settings.clone();
        // 结果回来时对一下 Cookie：期间登录成功、换了账号或被热重载清掉，
        // 这次结果都属于旧身份，整条丢掉，不能拿它清身份。
        //
        // 这里不看 `auth_epoch`：同账号的会员状态刷新也会推进它，
        // 那种情况身份没变，登出该照常完成（#197）。
        let logout_cookie = self.settings.cookie.clone();

        // 先落盘、成功后再清身份：落盘失败时保持「还在登录」，免得内存已登出、
        // 磁盘还留着旧 Cookie，热重载下一轮又把身份放回来（#192）。
        //
        // 写盘在锁里读盘上最新配置、只摘 Cookie 再写回：盘上的 Cookie 已经不是
        // 点击时那个就整次跳过，不会拿旧快照盖掉换进来的新身份，也不吃掉
        // 保存期间改过的主题、音量等字段（#197）。
        // 摘 Cookie 这一步也排进写队列：退出应用只等一个 flush，它自然包含在内（#259）。
        let expected_cookie = logout_cookie.clone();
        let save = self
            .settings_writer
            .sign_out(logout_snapshot, expected_cookie);
        self.logout_in_flight = true;
        // 慢盘时保存要等一会儿，先给一句反馈，别让点击看起来没反应。
        self.set_status_text(self.tr("正在退出登录…"));
        cx.notify();
        // 任务不留在 `Root` 上：落盘本身排在队列里，退出时 `flush()` 会等到它写完。
        cx.spawn(async move |this, cx| {
            let outcome = save.await;
            let _ = this.update(cx, |root, cx| {
                root.logout_in_flight = false;
                if root.settings.cookie != logout_cookie {
                    // 保存期间换过身份：这份结果已经过期，丢掉，不能拿来清身份。
                    // 状态行还挂着「正在退出登录…」：现在确实没登录就改回一句实话，
                    // 换了别的账号则让登录流程自己的状态行留着。
                    if root.settings.cookie.trim().is_empty() {
                        root.set_status_text(root.tr("已退出登录"));
                    }
                    cx.notify();
                    return;
                }
                let done = match outcome {
                    Ok(SignOut::Done) => {
                        // 本次运行里明确登出过：热重载不能再让 `SODA_COOKIE` 把身份补回来（#192）。
                        // 记号不落盘，重启后仍按环境变量启动。
                        root.env_cookie_opted_out = true;
                        // 只在内存里摘掉 Cookie：保存期间改过的其它设置（音量、日志等）
                        // 不跟着回滚到点击那一刻的快照。
                        root.settings.cookie.clear();
                        // 盘上已经写成登出状态，这里只重建会话，不再写一次盘。
                        if let Some(session) = root.session.as_mut() {
                            session.adopt(root.settings.clone());
                        }
                        // 队列里要是还攒着登出前拍的快照，也不用在这里补一次确认写：
                        // 排队落盘的 Cookie 一律沿用盘上那份（`Settings::save_queued_to`），
                        // 盘上已经空了，怎么写都带不回旧身份（#259）。
                        true
                    }
                    Ok(SignOut::IdentityChanged) => {
                        // 盘上身份已经变了，这次写入被跳过：什么都不改，
                        // 等热重载或登录流程把它们自己的状态行发出来。
                        false
                    }
                    Err(err) => {
                        // 错误提示会把状态行里的「正在退出登录…」换成同一句失败文案。
                        root.notify_error_once(
                            "logout-save-failed",
                            root.failure_text(
                                "保存退出登录状态",
                                &err,
                                "退出登录失败，账号仍保持登录；检查配置文件能否写入后重试。",
                            ),
                        );
                        false
                    }
                };
                if done {
                    root.clear_identity_state(cx);
                    // 备份不用在这里再写一次：保存本身在写锁内已经刷新过（#185）。
                    if sodam_core::config::env_cookie_present() {
                        root.set_status_text(root.tr(
                            "已退出登录；要彻底退出请取消环境变量 SODA_COOKIE，否则重启会重新登录",
                        ));
                    } else {
                        root.set_status_text(root.tr("已退出登录"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 场景页内容按账号给：登出、换号都清掉，在飞的回调按序号丢弃。
    fn reset_scene_state(&mut self) {
        self.scene_modes_seq = self.scene_modes_seq.wrapping_add(1);
        self.scene_cards_seq = self.scene_cards_seq.wrapping_add(1);
        self.scenes = Arc::new(Vec::new());
        self.scene_cards = Arc::new(Vec::new());
        self.scene_modes_state = SceneModesState::Idle;
        self.loading_scene_cards = false;
        self.scene_cards_failed = false;
        self.sync_scenes_list();
    }

    /// 旧身份的界面状态清理：播放停住、队列与账号/收藏清空、回到设置页。
    ///
    /// 退出登录与「外部把 Cookie 清空了」（热重载）共用这一份，不做保存。
    fn clear_identity_state(&mut self, cx: &mut Context<Self>) {
        self.engine.stop();
        self.playing = false;
        self.pending_track = None;
        self.playback_error = None;
        // 推进播放请求序号：正在下载的旧身份装载回来会被序号检查拒绝，
        // 否则清空队列后旧下载成功仍会把旧曲目塞回引擎（#185）。
        self.play_seq = self.play_seq.wrapping_add(1);
        // 在途的队列请求（推荐/听歌模式/加载更多）一并作废：旧回调过了序号
        // 检查仍会重建队列并开始装载，退出/换号后还能把旧队列播起来。
        self.recommendation_request_seq = self.recommendation_request_seq.wrapping_add(1);
        self.recommendation_load_seq = self.recommendation_load_seq.wrapping_add(1);
        self.loading_recommendation = false;
        self.recommendation_error = None;
        self.switching_queue = None;
        // 不可播结论与逐曲冷却按账号 × 会话隔离：退出后旧账号的结论一条都不留。
        // 在途预取回来后按代数丢弃，不能再写回这些已经清空的表。
        self.invalidate_identity_conclusions();
        self.queue = Queue::new(Vec::new());
        self.played_history.clear();
        self.sync_queue_cache();

        self.account = None;
        if self.nav == Nav::Settings {
            self.refresh_upgrade_estimate(cx);
        }
        self.login = LoginState::Idle;
        self.login_modal_open = false;
        self.results = Arc::new(Vec::new());
        self.searching = false;
        self.playlists.clear();
        self.liked = Arc::new(Vec::new());
        self.liked_ids = Arc::new(HashSet::new());
        self.liked_ids_failed = false;
        self.liked_loaded = false;
        self.open_playlist = None;
        self.track_menu = None;
        self.queue_menu = None;
        // 听歌模式是按账号给的内容：换号不清，新账号会先看到上一个账号的常用模式。
        self.reset_scene_state();
        self.set_nav(Nav::Settings, cx);
    }

    /// 详情列表内的加载更多分发。
    pub fn load_more_detail(&mut self, cx: &mut Context<Self>) {
        if self.nav == Nav::Artist {
            self.load_more_artist_tracks(cx);
        }
    }

    /// 队列抽屉内部滚动由 `UniformListScrollHandle` 的 `base_handle` 驱动，
    /// 抽出来当成一个普通 `ScrollHandle` 用，避免到处穿透 `UniformListScrollHandle` 内部。
    fn queue_base_handle(&self) -> gpui::ScrollHandle {
        self.queue_scroll.0.borrow().base_handle.clone()
    }

    /// 搜索页「歌曲」列表也是虚拟列表，同样借 `base_handle` 做像素滚动。
    fn search_tracks_base_handle(&self) -> gpui::ScrollHandle {
        self.search_tracks_scroll.0.borrow().base_handle.clone()
    }

    /// 把 GPUI 的滚动偏移换成「离顶部距离」。
    /// 偏移是负的：0 在最顶上，越往下滚越负。
    fn top_from_offset(handle: &gpui::ScrollHandle) -> f32 {
        -f32::from(handle.offset().y)
    }

    /// 把「离顶部距离」写回 GPUI 的滚动偏移；横轴保持不变。
    fn offset_with_top(
        mut offset: gpui::Point<gpui::Pixels>,
        top: f32,
    ) -> gpui::Point<gpui::Pixels> {
        offset.y = gpui::px(-top);
        offset
    }

    /// 普通滚动句柄目标（设置页、队列、搜索页、歌词页）。
    /// 六个变高虚拟列表不走这里，它们只吃增量。
    fn drag_scroll_handle(&self, target: DragTarget) -> Option<gpui::ScrollHandle> {
        Some(match target {
            DragTarget::Settings => self.settings_scroll.clone(),
            DragTarget::Queue => self.queue_base_handle(),
            DragTarget::SearchGroups => self.search_groups_scroll.clone(),
            DragTarget::SearchTracks => self.search_tracks_base_handle(),
            DragTarget::SearchArtists => self.search_artists_scroll.clone(),
            DragTarget::SearchAlbums => self.search_albums_scroll.clone(),
            DragTarget::SearchPlaylists => self.search_playlists_scroll.clone(),
            DragTarget::PlaylistGrid => self.playlist_grid_scroll.clone(),
            DragTarget::Lyrics => self.lyrics_scroll.clone(),
            _ => return None,
        })
    }

    /// 六个变高虚拟列表各自的滚动状态。
    fn drag_list_state(&self, target: DragTarget) -> Option<&gpui::ListState> {
        Some(match target {
            DragTarget::Scenes => &self.scenes_list,
            DragTarget::LikedDetail => &self.liked_detail_list,
            DragTarget::LibraryDetail => &self.library_detail_list,
            DragTarget::SearchPlaylistDetail => &self.search_playlist_detail_list,
            DragTarget::ArtistDetail => &self.artist_detail_list,
            DragTarget::AlbumDetail => &self.album_detail_list,
            _ => return None,
        })
    }

    /// 这个目标眼下离顶部多远。
    fn drag_top(&self, target: DragTarget) -> f32 {
        if let Some(handle) = self.drag_scroll_handle(target) {
            return Self::top_from_offset(&handle);
        }
        if let Some(list) = self.drag_list_state(target) {
            return -f32::from(list.scroll_px_offset_for_scrollbar().y);
        }
        0.0
    }

    /// 这个目标最多能滚多远。
    /// 变高列表用的是「已测量高度」，会随滚动变大；差的那段靠下一帧补（#168）。
    fn drag_max_top(&self, target: DragTarget) -> f32 {
        if let Some(handle) = self.drag_scroll_handle(target) {
            return f32::from(handle.max_offset().y);
        }
        if let Some(list) = self.drag_list_state(target) {
            return f32::from(list.max_offset_for_scrollbar().y);
        }
        0.0
    }

    /// 把「离顶部距离」写回目标：普通句柄写绝对偏移，列表按增量滚。
    fn apply_drag_top(&self, target: DragTarget, top: f32) {
        if let Some(handle) = self.drag_scroll_handle(target) {
            handle.set_offset(Self::offset_with_top(handle.offset(), top));
            return;
        }
        if let Some(list) = self.drag_list_state(target) {
            let delta = top - self.drag_top(target);
            list.scroll_by(gpui::px(delta));
        }
    }

    /// 结束当前拖动（不管是哪个目标），帧循环的计时也一并清掉。
    pub(crate) fn finish_middle_drag(&mut self) {
        self.middle_drag.finish();
        self.middle_drag_last_frame = None;
    }

    /// 只结束「正好是这个目标」的拖动。
    /// 异步回调迟到时，用户可能已经在别的列表上开拖了，不能一把全停。
    pub(crate) fn finish_middle_drag_for(&mut self, target: DragTarget) {
        if self.middle_drag.target() == Some(target) {
            self.finish_middle_drag();
        }
    }

    /// 上报「中键按下」：记下起点和这个区域当时的滚动位置。
    fn begin_middle_drag_on(&mut self, target: DragTarget, y: f32) {
        // 上一次手势的帧计时可能还留着；新按下就清掉，首帧从 dt = 0 起步。
        self.middle_drag_last_frame = None;
        let top = self.drag_top(target);
        let nav = self.nav;
        self.middle_drag.begin(y, top, nav, target);
    }

    /// 设置页上报「中键按下」。
    pub(crate) fn begin_middle_drag(&mut self, y: f32) {
        self.begin_middle_drag_on(DragTarget::Settings, y);
    }

    /// 队列抽屉上报「中键按下」。
    pub(crate) fn begin_middle_drag_queue(&mut self, y: f32) {
        self.begin_middle_drag_on(DragTarget::Queue, y);
    }

    /// 搜索页各个列表和歌单网格上报「中键按下」，按 `target` 分清是谁。
    pub(crate) fn begin_middle_drag_for(&mut self, target: DragTarget, y: f32) {
        self.begin_middle_drag_on(target, y);
    }

    /// 歌词页上报「中键按下」：只记下起点和歌词当时的滚动位置。
    ///
    /// 这里**不动**自动跟唱：按下没拖（或者抖动不到 2px）就什么都不该发生，
    /// 让位留到真正滚起来那一刻（见 [`Self::middle_drag_move`]）。
    /// 按下到首次移动之间补间还会推进列表；起步前的每次移动都会按此刻偏移重定锚点（#169）。
    pub(crate) fn begin_middle_drag_lyrics(&mut self, y: f32) {
        self.begin_middle_drag_on(DragTarget::Lyrics, y);
    }

    /// 用户自己动手了（滚轮或中键拖动）：先停 5 秒自动跟唱，再停掉滚动动画。
    ///
    /// 两处入口共用同一份逻辑，行为保持一致：跟唱停 5 秒是官方的手感，
    /// 而动画不停会和手势抢同一个偏移，看起来就是拖一下被拽回去一下。
    pub(crate) fn pause_lyrics_follow(&mut self) {
        self.lyrics_user_scroll_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
        self.cancel_lyrics_scroll_tween();
    }

    /// 中键按着移动时更新指针位置（#182）。
    ///
    /// 距离驱动语义下指针只决定速度，不直接决定位置；位置的推进由帧循环
    /// （`Root::render` 每帧的 [`Self::advance_middle_drag_scroll`]）负责，
    /// 指针停住不动也继续滚。刚越过死区那一刻歌词页先让位一次，
    /// 之后的持续让位由帧循环每帧续。
    pub(crate) fn middle_drag_move(
        &mut self,
        y: f32,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.middle_drag.target() else {
            return;
        };
        let already_scrolling = self.middle_drag.is_scrolling();
        let is_lyrics = target == DragTarget::Lyrics;
        let nav = self.nav;
        let window_active = window.is_window_active();
        // 歌词页按下后，自动跟唱补间仍可能推进列表；起步前每次移动都按此刻的真实偏移
        // 重定积分起点，真正越过死区时从最新位置开始，首帧不跳（#169）。
        // 起步后积分起点不能再改，重定只会空转，直接跳过（#189）。
        if is_lyrics && !already_scrolling {
            let top = self.drag_top(target);
            self.middle_drag
                .rebase_unstarted_top(top, nav, window_active);
        }
        let just_started = self.middle_drag.update_pointer(y, nav, window_active);
        if is_lyrics && just_started {
            self.pause_lyrics_follow();
        }
        // 起步后帧循环每帧都会经由 `request_animation_frame` 标脏，指针位置下一帧
        // 就会被读到；鼠标移动再标脏是重复动作（#189）。没起步（含死区内的每次移动）
        // 和起步那次仍然标脏；`last_frame` 为空说明起步后一帧还没跑过（起步事件可能
        // 落在绘制中途被吞），这里补一次。已知边界：帧循环完全停摆时（窗口最小化等）
        // 这里不会自愈，靠系统重绘恢复。
        if !already_scrolling || self.middle_drag_last_frame.is_none() {
            cx.notify();
        }
    }

    /// 距离驱动的自动滚动 dt 上限（毫秒）：渲染卡了一拍也只按这个时长推进，
    /// 不会把卡住的时间一口气滚完。
    const MAX_DRAG_FRAME_DT_MS: f32 = 50.0;

    /// 帧循环推进一次中键自动滚动（#182）。
    ///
    /// 由 `Root::render` 在 `is_scrolling()` 时每帧调用：按当前指针离参照点的
    /// 距离算速度，乘上距上一帧的 dt，积分出这一帧的目标位置并写回。
    /// 指针停住不动时没有鼠标事件，滚动全靠这里。松手、切页、失焦后
    /// `middle_drag` 已被清掉，直接收摊，下一帧循环自然停。
    pub(crate) fn advance_middle_drag_scroll(&mut self, window: &mut gpui::Window) {
        let Some(target) = self.middle_drag.target() else {
            self.middle_drag_last_frame = None;
            return;
        };
        let nav = self.nav;
        let window_active = window.is_window_active();
        let max_top = self.drag_max_top(target);
        let now = std::time::Instant::now();
        let dt_ms = self
            .middle_drag_last_frame
            .map_or(0.0, |last| (now - last).as_secs_f32() * 1000.0)
            .min(Self::MAX_DRAG_FRAME_DT_MS);
        self.middle_drag_last_frame = Some(now);
        let Some(top) = self.middle_drag.advance(dt_ms, max_top, nav, window_active) else {
            self.middle_drag_last_frame = None;
            return;
        };
        // 歌词页：拖动期间每帧续一次让位。指针停住不动时没有鼠标事件来续，
        // 不在这里续的话跟唱 5 秒一到就会把视图拽回当前句。
        if target == DragTarget::Lyrics {
            self.pause_lyrics_follow();
        }
        self.apply_drag_top(target, top);
    }

    /// 统一页面切换：保留返回栈，详情页使用通用返回组件。
    pub(crate) fn set_nav(&mut self, nav: Nav, cx: &mut Context<Self>) {
        if self.nav == nav {
            return;
        }
        self.nav_history.push(self.nav);
        if self.nav_history.len() > 50 {
            self.nav_history.remove(0);
        }
        self.nav = nav;
        self.on_nav_changed(cx);
        cx.notify();
    }

    /// 后台刷新缓存统计（同步扫盘不能进渲染路径：设置页每次重绘都扫
    /// 目录会把 UI 卡出顿挫，预取/心跳的高频 notify 会放大这个问题）。
    pub(crate) fn refresh_cache_stats(&mut self, cx: &mut Context<Self>) {
        self.cache_summary_request_seq = self.cache_summary_request_seq.wrapping_add(1);
        let seq = self.cache_summary_request_seq;
        let work = cx.background_spawn(async { sodam_core::audio::cache_stats() });
        cx.spawn(async move |this, cx| {
            let stats = work.await;
            let _ = this.update(cx, |root, cx| {
                // 旧扫描晚到不能把新数字盖回旧值（#248）。只有最新那次落回并接着刷预估。
                if root.cache_summary_request_seq == seq {
                    root.cache_summary = stats;
                    root.refresh_upgrade_estimate(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn upgrade_account(&self) -> (Option<bool>, Option<String>) {
        (
            self.account.as_ref().map(|account| account.vip),
            self.account.as_ref().map(|account| account.user_id.clone()),
        )
    }

    fn upgrade_scope(&self) -> Option<String> {
        let (is_vip, account_user_id) = self.upgrade_account();
        sodam_core::cache::unknown_probe_scope(account_user_id.as_deref(), is_vip)
    }

    /// 刷新缓存批量升级预估信息（后台扫盘，不阻塞 UI）。
    pub(crate) fn refresh_upgrade_estimate(&mut self, cx: &mut Context<Self>) {
        self.batch_upgrade_estimate_request_seq =
            self.batch_upgrade_estimate_request_seq.wrapping_add(1);
        let seq = self.batch_upgrade_estimate_request_seq;
        let target_intent = self.batch_upgrade_target.clone();
        let (is_vip, _) = self.upgrade_account();
        let scope = self.upgrade_scope();
        let target_rank = match sodam_core::quality::target_rank(&target_intent, is_vip) {
            Some(rank) => rank,
            None => {
                self.batch_upgrade_estimate = None;
                cx.notify();
                return;
            }
        };
        let work = cx.background_spawn(async move {
            let dir = sodam_core::audio::cache_dir();
            let records = sodam_core::cache::list_cached_audio(&dir);
            sodam_core::cache::estimate_upgrade(&records, target_rank, scope.as_deref())
        });
        cx.spawn(async move |this, cx| {
            let estimate = work.await;
            let _ = this.update(cx, |root, cx| {
                if estimate_result_is_current(
                    &root.batch_upgrade_target,
                    root.batch_upgrade_estimate_request_seq,
                    &target_intent,
                    seq,
                ) {
                    root.batch_upgrade_estimate = Some(estimate);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 切换批量升级的目标档位，并重新计算预估。
    pub(crate) fn set_batch_upgrade_target(
        &mut self,
        intent: sodam_core::quality::QualityIntent,
        cx: &mut Context<Self>,
    ) {
        self.batch_upgrade_target = intent;
        self.refresh_upgrade_estimate(cx);
        cx.notify();
    }

    /// 跑一首：建会话 → 调升级 → 直接取升级路径给的「换没换文件」判定结果。
    ///
    /// 单首升级和批量升级共用这一段，改一处两边同时生效。同步函数：调用方自己
    /// 决定放进哪个后台任务；这里只碰磁盘和网络，不碰 `Root`。
    ///
    /// `levels` 决定逐首日志：单首默认日志要看得见，升级成功、「没换成更好整曲」和
    /// 「升级结果未知」记 INFO、失败记 WARN；批量几百首逐首记会刷屏，只留开跑和收尾两行汇总，
    /// 所以这几种逐首日志都降到 DEBUG。
    fn upgrade_one_cached_track(
        settings: Settings,
        track: TrackItem,
        target_intent: sodam_core::quality::QualityIntent,
        account_is_vip: Option<bool>,
        account_user_id: Option<String>,
        levels: UpgradeLogLevels,
    ) -> Outcome {
        let session = Session::new(settings);
        match session.upgrade_cached_track(
            &track,
            Some(&target_intent),
            account_is_vip,
            account_user_id.as_deref(),
        ) {
            Ok(run) => {
                // 换没换文件由升级那条路自己判定；日志和对外结论都只认 not_improved_reason。
                let outcome = Outcome::from_not_improved_reason(run.not_improved_reason);
                let (event, fields) = cache_upgrade_log_parts(
                    track.id.as_str(),
                    run.track.quality.as_str(),
                    run.not_improved_reason,
                );
                log_outlet::log_event(levels.success, LogTopic::Player, event, &fields);
                outcome
            }
            Err(failure) => {
                // 级别由调用方定：单首失败要用 WARN 进默认日志；批量几百首逐首记会刷屏，
                // 用 DEBUG 只在详细模式留痕，默认日志只看收尾汇总。
                log_outlet::log_event(
                    levels.failure,
                    LogTopic::Player,
                    "cache_upgrade_failed",
                    &[
                        ("track_id", track.id.as_str()),
                        ("detail", failure.detail.as_str()),
                    ],
                );
                Outcome::Failed
            }
        }
    }

    /// 触发单首曲目的后台缓存升级（命中缓存但档位低于当前所选时）。
    pub(crate) fn spawn_background_cache_upgrade(
        &mut self,
        track: TrackItem,
        intent: sodam_core::quality::QualityIntent,
        account_is_vip: Option<bool>,
        account_user_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let max_inflight = prefetch_concurrency().max(1);
        let key = format!("{}:{}", track.id, intent.as_setting());

        // N2 并发上限保护（复用 prefetch_concurrency，避免连续切歌并发冲垮签名服务）。
        // 数的是**在跑的任务**，不是「哪几首」：同一首在换号或清缓存之后会重新发起，
        // 按 key 记的话两次发起会共用一个槽位，上限就守不住了。
        if self.cache_upgrade_running >= max_inflight {
            return;
        }
        // 本次会话已试过升级该曲目到此档位，避免反复重试
        if !self.cache_upgrade_attempted.begin(key) {
            return;
        }

        self.cache_upgrade_running += 1;

        let settings = self.settings.clone();
        let work = cx.background_spawn(async move {
            Self::upgrade_one_cached_track(
                settings,
                track,
                intent,
                account_is_vip,
                account_user_id,
                UpgradeLogLevels {
                    success: LogLevel::Info,
                    failure: LogLevel::Warn,
                },
            )
        });
        cx.spawn(async move |this, cx| {
            // 单首这条路不按结果改去重记号：记号留着，档位结论由 sidecar 与下次命中判定。
            let outcome = work.await;
            let _ = this.update(cx, |root, cx| {
                root.cache_upgrade_running = root.cache_upgrade_running.saturating_sub(1);
                // 单首升级跑完了，盘上文件或记录可能变了，设置页正开着就不能还拿打开那一刻的数字说话（#248）。
                // `Upgraded` 一定换了文件；`NoHigherTier` 与 `UnknownRank` 可能只写了 sidecar 记录。
                // 统计也一起刷：字节数可能变，同一屏不能一新一旧。
                // `Failed` 不动正式缓存；批量跑着时卡片显示的是进度，收尾那条路自己会重算，这里不白扫一遍。
                if outcome != Outcome::Failed && root.nav == Nav::Settings && !root.batch_upgrading
                {
                    root.refresh_cache_stats(cx);
                }
            });
        })
        .detach();
    }

    /// 执行已有缓存的批量升级任务。
    ///
    /// 循环只跟单首共用「跑一首」；循环本身、状态和文案留在应用层。
    pub(crate) fn start_batch_upgrade(&mut self, cx: &mut Context<Self>) {
        if self.batch_upgrading {
            return;
        }
        // 正在清缓存或收缩缓存时不开跑：扫完盘到真跑之间那几首会被删掉，剩下的一首首都以
        // 「文件已被删」失败。和 `clear_cache` / 缓存维护那两道拦的是同一件事。
        if self.cache_clearing || self.cache_maintenance_running {
            let message = self.tr("正在整理缓存，请稍后再试");
            self.notify_warning_once("batch-upgrade-cache-busy", message);
            cx.notify();
            return;
        }
        // 正在退出登录：每首之前都会按 `LoggingOut` 停住，扫了也是白扫一遍全盘。
        if self.logout_in_flight {
            let message = self.tr("正在退出登录，请稍后再试");
            self.notify_warning_once("batch-upgrade-blocked", message);
            cx.notify();
            return;
        }

        let target_intent = self.batch_upgrade_target.clone();
        let (is_vip, start_user_id) = self.upgrade_account();

        // P2 权益预检：需要会员的档位只在账号确认有权益时才放行（认不出权益也算不行）。
        if sodam_core::quality::batch_upgrade_needs_confirmed_entitlement(&target_intent, is_vip) {
            let label =
                crate::ui::player_bar::quality_label(target_intent.as_setting(), self.language);
            let warn = self.localized("「{}」音质需 VIP 会员权限", std::slice::from_ref(&label));
            self.notify_warning_once("batch-upgrade-vip", warn);
            cx.notify();
            return;
        }

        self.batch_upgrading = true;
        self.batch_upgrade_cancel = false;
        self.batch_upgrade_progress = BatchUpgradeProgress::default();
        cx.notify();

        let target_rank = match sodam_core::quality::target_rank(&target_intent, is_vip) {
            Some(rank) => rank,
            None => {
                self.batch_upgrading = false;
                cx.notify();
                return;
            }
        };

        // 点下去那一刻的身份代数：扫盘也要几百毫秒，期间登出/换号的话这一批一个都不该发。
        let start_epoch = self.auth_epoch;
        // 收尾文案要分清「真的换了号」和「同一账号的会员状态刷新」：两者都推进代数，
        // 但后者报「账号已变更」是撒谎。账号信息没加载出来时也不说换了号。
        let scan_scope = self.upgrade_scope();

        // 后台扫盘读取待升级记录（排除试听文件并遵循意图上限），不阻塞主渲染线程
        let scan_work = cx.background_spawn(async move {
            let dir = sodam_core::audio::cache_dir();
            let records = sodam_core::cache::list_cached_audio(&dir);
            records
                .into_iter()
                .filter(|r| r.is_upgradeable_to(target_rank, scan_scope.as_deref()))
                .collect::<Vec<_>>()
        });

        cx.spawn(async move |this, cx| {
            let upgradeables = scan_work.await;
            let total = upgradeables.len();

            let update_res = this.update(cx, |root, cx| {
                root.batch_upgrade_progress = BatchUpgradeProgress { done: 0, total };
                cx.notify();
            });
            if update_res.is_err() {
                return;
            }

            let total_text = total.to_string();
            log_event(
                LogLevel::Info,
                LogTopic::Player,
                "cache_upgrade_batch_started",
                &[
                    ("total", total_text.as_str()),
                    ("target", target_intent.as_setting()),
                ],
            );
            let mut tally = Tally::default();
            let mut stopped: Option<StopReason> = None;
            for record in upgradeables {
                // 每首开始前重读身份、取消标志、设置和会员状态。开场那份快照不再用——
                // 旧 Cookie 就是从这里漏出去的。
                let step = this.update(cx, |root, _cx| {
                    (
                        root.batch_upgrade_stop(start_epoch),
                        root.settings.clone(),
                        root.upgrade_account(),
                    )
                });
                let Ok((stop, mut settings, (is_vip, account_user_id))) = step else {
                    return;
                };
                if let Some(reason) = stop {
                    stopped = Some(reason);
                    break;
                }

                let track = TrackItem {
                    id: record.track_id.clone(),
                    ..Default::default()
                };
                // 这里只是为了一次性会话还原缓存文件自己的档位意图，不落盘，所以来源记为
                // Derived（程序推导），不能冒充成用户的手动选择。
                settings.set_quality_intent_with_source(
                    &sodam_core::quality::QualityIntent::parse(&record.quality_tag),
                    sodam_core::quality::QualitySource::Derived,
                );
                let target_clone = target_intent.clone();
                let outcome = cx
                    .background_spawn(async move {
                        Self::upgrade_one_cached_track(
                            settings,
                            track,
                            target_clone,
                            is_vip,
                            account_user_id,
                            UpgradeLogLevels {
                                success: LogLevel::Debug,
                                failure: LogLevel::Debug,
                            },
                        )
                    })
                    .await;
                tally.record(outcome);

                let done = tally.total();
                let update_res = this.update(cx, |root, cx| {
                    root.batch_upgrade_progress = BatchUpgradeProgress { done, total };
                    cx.notify();
                });
                if update_res.is_err() {
                    return;
                }
            }

            // 取消和换号都可能落在最后一首下载期间：那时循环已经没有「下一首」可停，这两件事
            // 照样要说出来，不能被一句「完成」盖掉。判据还是那一个 `stop_reason`，
            // 免得同一个窗口里两套口径给出两种答案。
            if stopped.is_none() {
                stopped = this
                    .update(cx, |root, _cx| root.batch_upgrade_stop(start_epoch))
                    .unwrap_or(None);
            }

            let ran_text = tally.total().to_string();
            let upgraded_text = tally.upgraded.to_string();
            let failed_text = tally.failed.to_string();
            let no_higher_text = tally.no_higher.to_string();
            let unknown_result_text = tally.unknown_result.to_string();
            let stop_text = stopped.map_or("done", |reason| reason.as_str());
            log_event(
                LogLevel::Info,
                LogTopic::Player,
                "cache_upgrade_batch_finished",
                &[
                    ("total", total_text.as_str()),
                    ("ran", ran_text.as_str()),
                    ("upgraded", upgraded_text.as_str()),
                    ("failed", failed_text.as_str()),
                    ("no_higher", no_higher_text.as_str()),
                    ("unknown_result", unknown_result_text.as_str()),
                    ("stop", stop_text),
                ],
            );

            let _ = this.update(cx, |root, cx| {
                root.batch_upgrading = false;
                root.batch_upgrade_cancel = false;
                root.batch_upgrade_progress = BatchUpgradeProgress {
                    done: tally.total(),
                    total,
                };
                root.refresh_cache_stats(cx);
                // 一首都没扫到也照样说一句：用户按过取消或换了号，状态行不能停在「正在停止…」。
                if total > 0 || stopped.is_some() {
                    let counts = [tally.total().to_string(), total.to_string()];
                    match stopped {
                        Some(StopReason::IdentityChanged) => {
                            // 会员状态刷新也会推进代数：只有真换了号才说「账号已变更」。
                            let current_user_id = root
                                .account
                                .as_ref()
                                .map(|account| account.user_id.as_str());
                            let message =
                                match identity_change(start_user_id.as_deref(), current_user_id) {
                                    IdentityChange::AccountChanged => {
                                        root.localized("账号已变更，批量已停止 ({} / {})", &counts)
                                    }
                                    IdentityChange::LoginStateChanged => root
                                        .localized("登录状态已变化，批量已停止 ({} / {})", &counts),
                                };
                            root.notify_warning_once("batch-upgrade-stopped", message);
                        }
                        Some(StopReason::LoggingOut) => {
                            let message =
                                root.localized("正在退出登录，批量已停止 ({} / {})", &counts);
                            root.notify_warning_once("batch-upgrade-stopped", message);
                        }
                        Some(StopReason::Cancelled) => {
                            root.set_status("已取消 ({} / {})", &counts);
                        }
                        None => {
                            let message = root.localized(
                                "缓存升级完成：成功升级 {} 首，{} 首失败，{} 首无更好整曲，{} 首升级结果未知",
                                &[
                                    tally.upgraded.to_string(),
                                    tally.failed.to_string(),
                                    tally.no_higher.to_string(),
                                    tally.unknown_result.to_string(),
                                ],
                            );
                            match batch_upgrade_notice(tally, message) {
                                BatchUpgradeNotice::Warning(text) => {
                                    root.notify_warning_once("batch-upgrade-complete", text);
                                }
                                BatchUpgradeNotice::Success(text) => {
                                    root.notify_success_once("batch-upgrade-complete", text);
                                }
                                BatchUpgradeNotice::Status(text) => root.set_status_text(text),
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 这一轮批量现在该不该停。判据全在 [`stop_reason`] 里，这里只负责把状态喂进去。
    fn batch_upgrade_stop(&self, start_epoch: u64) -> Option<StopReason> {
        stop_reason(StopConditions {
            start_epoch,
            current_epoch: self.auth_epoch,
            cancelled: self.batch_upgrade_cancel,
            logging_out: self.logout_in_flight,
        })
    }

    /// 取消批量升级：只停在下一首之前，正在下的那一首不打断（底层没有取消句柄）。
    pub(crate) fn cancel_batch_upgrade(&mut self, cx: &mut Context<Self>) {
        if !self.batch_upgrading || self.batch_upgrade_cancel {
            return;
        }
        self.batch_upgrade_cancel = true;
        // 正在下的那一首跑完才停：先说清在等什么，别让按钮看起来没反应。
        self.set_status_text(self.tr("正在停止批量升级…"));
        cx.notify();
    }

    /// 通用返回：同页详情优先关闭，再按真实访问历史回退。
    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        // 关掉同页详情时页签没变，渲染里的 sync_context 认不出来；中键还按着
        // 就会继续滚已经不显示的列表。这里先手动停手，切页时这句是幂等的。
        self.finish_middle_drag();
        if self.nav == Nav::Search && self.search_playlist.is_some() {
            self.search_playlist = None;
            self.loading_search_playlist = false;
            cx.notify();
            return;
        }
        if self.nav == Nav::Library && self.open_playlist.is_some() {
            self.open_playlist = None;
            self.loading_playlist = false;
            cx.notify();
            return;
        }
        let fallback = if self.settings.cookie.trim().is_empty() {
            Nav::Settings
        } else {
            Nav::Home
        };
        let nav = self.nav_history.pop().unwrap_or(fallback);
        self.nav = nav;
        self.on_nav_changed(cx);
        cx.notify();
    }

    /// 切换导航时的懒加载：进入「我的歌单 / 我喜欢的音乐」才拉数据。
    ///
    /// 未登录时其他页面一律进不去（登录入口在设置 → 账户）。
    pub fn on_nav_changed(&mut self, cx: &mut Context<Self>) {
        // 进设置页要重扫缓存统计和升级预估。`set_nav` 和 `go_back` 都走这里；
        // 只挂在 `set_nav` 上会让「从详情页返回」那条路继续显示旧数字（#248）。
        if matches!(self.nav, Nav::Settings) {
            self.refresh_cache_stats(cx);
        }
        // 状态行是全局反馈，不属于某一页：切页就收走，否则「正在搜索…」会跟着
        // 用户跑到设置页上。本页自己的加载提示在这之后写入，不受影响。
        self.status_line.clear();
        // 离开设置页时录制态不会自己结束：卡片不在树上时收不到键，但回到设置页那一行
        // 还显示着「按下新的组合键…」，用户随手按一个键就被写进配置。
        self.shortcut_capture = None;
        self.shortcut_note = None;
        if !self.settings.cookie.trim().is_empty() {
            match self.nav {
                // 已经有失败终态时不自动重发：否则「失败 → set_nav(Home) → 再发」会变成
                // 断网下的无限重试循环。此时页面显示重试入口，重试由用户显式触发（#55）。
                Nav::Home if self.recommendation_error.is_none() => {
                    self.start_recommendation(true, cx)
                }
                // 听歌模式只负责首屏懒加载；“加载更多”必须由用户显式点击。
                Nav::Scenes
                    if should_auto_load_scene_page(
                        self.scene_cards.is_empty(),
                        self.loading_scene_cards,
                        self.scene_modes_state,
                    ) =>
                {
                    self.load_scenes(cx)
                }
                Nav::Library => self.load_playlists(cx),
                Nav::Liked => {
                    // 收藏页使用独立列表状态；已加载数据也要重建到页首。
                    self.finish_middle_drag_for(DragTarget::LikedDetail);
                    self.liked_detail_list.reset(self.liked.len() + 1);
                    self.load_liked(cx);
                }
                _ => {}
            }
        }
    }

    /// 封面本地路径（渲染路径唯一入口：只查状态，不发请求）。
    pub fn cover_of(&self, url: &str) -> Option<PathBuf> {
        self.cover_store.path(url)
    }

    /// 把封面交给 CoverStore；缺图时渲染先显示占位块。
    ///
    /// 去重、队列上限、失败次数都由 store 管，这里不再维护第二份账。
    /// 返回这轮新入队数；挤掉的数留给封面消费循环取走，避免直接调用提前消费。
    pub fn ensure_covers(&mut self, urls: &[String], cx: &mut Context<Self>) -> usize {
        let queued = self.cover_store.want(urls);
        if queued > 0 {
            self.pump_covers(cx);
        }
        queued
    }

    /// 驱动请求：保持最多 `MAX_CONCURRENCY` 个下载在飞，完成一个补一个。
    pub(crate) fn pump_covers(&mut self, cx: &mut Context<Self>) {
        for url in self.cover_store.drain() {
            let settings = self.settings.clone();
            let fetch_url = url.clone();
            let task = cx.background_spawn(async move {
                let session = Session::new(settings);
                let path = session.cover_path(&fetch_url);
                let color = path.as_deref().and_then(sodam_core::color::dominant_color);
                (path, color)
            });
            cx.spawn(async move |this, cx| {
                let (path, color) = task.await;
                let _ = this.update(cx, |root, cx| {
                    if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
                        let file = path
                            .as_ref()
                            .and_then(|path| path.file_name())
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let color = color
                            .map(|rgb| format!("{rgb:?}"))
                            .unwrap_or_else(|| "none".to_string());
                        log_event(
                            LogLevel::Debug,
                            LogTopic::App,
                            "cover_resolved",
                            &[
                                ("url", url.as_str()),
                                ("file", file.as_str()),
                                ("color", color.as_str()),
                            ],
                        );
                    }
                    // 下载期间这张封面可能刚好被后台淘汰删掉：解析出了路径但文件
                    // 已经没了，按 Vanished 处理，让它退回可重排，而不是把空路径
                    // 发布出去。拿不到路径则记一次失败，两次之后才放弃。
                    let outcome = match path {
                        Some(path) if path.exists() => CoverOutcome::Ready { path, color },
                        Some(_) => CoverOutcome::Vanished,
                        None => CoverOutcome::Failed,
                    };
                    root.cover_store.complete(&url, outcome);
                    // 失败 / 文件已没也要重绘一次：渲染会重新请求。
                    // 两种都按 `CoverStore` 的两次上限，文件反复被删不会无限重下。
                    cx.notify();
                    // 一个下载完成就补下一个，始终有界并发
                    root.pump_covers(cx);
                });
            })
            .detach();
        }
    }

    /// 拉「我的歌单」（自己创建的 + 收藏的）。
    pub fn load_playlists(&mut self, cx: &mut Context<Self>) {
        if self.loading_library || !self.playlists.is_empty() {
            return;
        }
        if self.session.is_none() {
            self.set_status_text(self.tr("需要先登录才能读取歌单"));
            cx.notify();
            return;
        }
        let settings = self.settings.clone();
        self.loading_library = true;
        self.set_status_text(self.tr("正在读取歌单…"));
        cx.notify();
        // 请求归属发起时的身份：外部换号后旧结果整份丢弃，不让旧账号的
        // 歌单写回缓存，也把新身份的重拉接上（#185）。
        let auth_epoch = self.auth_epoch;
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            sodam_core::library::user_playlists(&session)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_library = false;
                if root.auth_epoch != auth_epoch {
                    if !root.settings.cookie.trim().is_empty() {
                        root.load_playlists(cx);
                    }
                    return;
                }
                match result {
                    Ok(playlists) => {
                        root.set_status("共 {} 个歌单", &[playlists.len().to_string()]);
                        let covers: Vec<String> = playlists
                            .iter()
                            .take(ROW_BATCH)
                            .map(|item| item.cover.clone())
                            .collect();
                        root.playlists = playlists;
                        root.ensure_covers(&covers, cx);
                    }
                    Err(err) => {
                        let message = root.failure_text("读取歌单失败", &err, REMOTE_TEXT);
                        root.notify_error_once("library-load-failed", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 只拉「我喜欢的音乐」的 id 集合（列表爱心状态用，比整表轻）。
    pub fn load_liked_ids(&mut self, cx: &mut Context<Self>) {
        if self.liked_loading || self.settings.cookie.trim().is_empty() {
            return;
        }
        self.liked_loading = true;
        let settings = self.settings.clone();
        // 请求归属发起时的身份：外部换号后旧结果整份丢弃，不让旧账号的
        // ids 写进新会话（#185）。
        let auth_epoch = self.auth_epoch;
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            // 启动时网络/接口可能未就绪：最多尝试 3 次（2s / 4s 退避），
            // 否则一次瞬时失败会让整个会话的爱心状态全部落在「未收藏」。
            let mut attempt = 0;
            loop {
                match sodam_core::library::liked_track_ids(&session) {
                    Ok(ids) => return Ok(ids),
                    Err(_err) if attempt < 2 => {
                        attempt += 1;
                        std::thread::sleep(std::time::Duration::from_secs(2 * attempt));
                    }
                    Err(err) => return Err(err),
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.liked_loading = false;
                if root.auth_epoch != auth_epoch {
                    // 旧身份的结果不要：新身份还没拉过 ids，接着拉一次。
                    if !root.settings.cookie.trim().is_empty() {
                        root.load_liked_ids(cx);
                    }
                    return;
                }
                match result {
                    Ok(ids) => {
                        root.liked_ids = Arc::new(ids);
                        root.liked_ids_failed = false;
                        cx.notify();
                    }
                    Err(_) => {
                        // 未登录/网络异常：保持空集合，不打扰用户；
                        // 但置失败标记，等账号刷新成功（网络就绪信号）后自动重试。
                        root.liked_ids_failed = true;
                    }
                }
            });
        })
        .detach();
    }

    /// 点爱心：收藏 / 取消收藏（乐观更新，失败回滚）。
    pub fn toggle_like(&mut self, track: TrackItem, cx: &mut Context<Self>) {
        let id = track.id.clone();
        let liked = !self.liked_ids.contains(&id);
        {
            let mut ids = self.liked_ids.as_ref().clone();
            if liked {
                ids.insert(id.clone());
            } else {
                ids.remove(&id);
            }
            self.liked_ids = Arc::new(ids);
        }
        // 只有完整加载后才同步本地列表；未加载时后台拉全量，避免覆盖历史收藏。
        if self.liked_loaded {
            let mut list = self.liked.as_ref().clone();
            if liked {
                if !list.iter().any(|item| item.id == id) {
                    list.insert(0, track.clone());
                }
            } else {
                list.retain(|item| item.id != id);
            }
            self.liked = Arc::new(list);
        } else {
            self.load_liked(cx);
        }
        let message = if liked {
            self.localized("已收藏：{}", std::slice::from_ref(&track.title))
        } else {
            self.localized("已取消收藏：{}", std::slice::from_ref(&track.title))
        };
        self.set_status_text(message);
        cx.notify();

        let settings = self.settings.clone();
        let work_id = id.clone();
        let rollback_track = track.clone();
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            sodam_core::library::set_track_liked(&session, &work_id, liked)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if let Err(err) = result {
                    // 回滚 ids
                    let mut ids = root.liked_ids.as_ref().clone();
                    if liked {
                        ids.remove(&id);
                    } else {
                        ids.insert(id.clone());
                    }
                    root.liked_ids = Arc::new(ids);
                    // 同步回滚本地完整列表，避免 ids 与列表互相矛盾。
                    if root.liked_loaded {
                        let mut list = root.liked.as_ref().clone();
                        if liked {
                            list.retain(|item| item.id != id);
                        } else if !list.iter().any(|item| item.id == id) {
                            list.insert(0, rollback_track);
                        }
                        root.liked = Arc::new(list);
                    }
                    let message = root.failure_text("收藏失败", &err, REMOTE_TEXT);
                    root.notify_error_once("like-toggle-failed", message);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 拉「我喜欢的音乐」。
    pub fn load_liked(&mut self, cx: &mut Context<Self>) {
        self.finish_middle_drag_for(DragTarget::LikedDetail);
        self.liked_detail_list.reset(self.liked.len() + 1);
        // `liked` 可能被点爱心提前预填；是否完整加载必须单独标记。
        if self.loading_liked || self.liked_loaded {
            return;
        }
        if self.session.is_none() {
            self.set_status_text(self.tr("需要先登录才能读取收藏"));
            cx.notify();
            return;
        }
        let settings = self.settings.clone();
        self.loading_liked = true;
        self.set_status_text(self.tr("正在读取我喜欢的音乐…"));
        cx.notify();
        // 请求归属发起时的身份：外部换号后旧结果整份丢弃（#185）。
        let auth_epoch = self.auth_epoch;
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            sodam_core::library::liked_songs(&session)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_liked = false;
                if root.auth_epoch != auth_epoch {
                    // 旧身份的结果不要；新身份还没拉过完整收藏，接着拉一次。
                    if !root.settings.cookie.trim().is_empty() {
                        root.load_liked(cx);
                    }
                    return;
                }
                match result {
                    Ok(tracks) => {
                        root.liked_loaded = true;
                        // 全量列表是收藏的权威快照：同步修复全局 ids，
                        // 避免启动时 ids 拉取失败后爱心状态一直错。
                        root.liked_ids =
                            Arc::new(tracks.iter().map(|track| track.id.clone()).collect());
                        root.liked_ids_failed = false;
                        root.finish_middle_drag_for(DragTarget::LikedDetail);
                        root.liked_detail_list.reset(tracks.len() + 1);
                        root.set_status("我喜欢的音乐：{} 首", &[tracks.len().to_string()]);
                        // 只取头部封面（列表封面由「可见行」按需排队）
                        let head: Vec<String> = tracks
                            .first()
                            .map(|track| vec![track.cover.clone()])
                            .unwrap_or_default();
                        root.liked = Arc::new(tracks);
                        root.ensure_covers(&head, cx);
                    }
                    Err(err) => {
                        let message = root.failure_text("读取收藏失败", &err, REMOTE_TEXT);
                        root.notify_error_once("liked-load-failed", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 点列表行播放：用户明确点了**这一首**。
    ///
    /// 命中不可播集合时在改队列之前返回：播放状态与当前曲目都不动，
    /// 不替他跳、也不重打网络。其余情况与 [`Self::play_from`] 相同（复制一份入队）。
    pub fn play_selected_arc(
        &mut self,
        tracks: std::sync::Arc<Vec<TrackItem>>,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(track) = tracks.get(index) else {
            return;
        };
        if self.block_explicit_play(track, cx) {
            return;
        }
        self.play_from(tracks.as_ref().clone(), index, cx);
    }

    /// 随机播放整张歌单：按原顺序入队，并把播放模式切到随机。
    ///
    /// 不洗牌队列本身：官方是「先进队再随机取下一首」，队列抽屉里仍显示
    /// 真实曲目顺序。洗牌会让「上一首 / 下一首」和抽屉里的顺序对不上。
    /// 随机播放整张歌单：按原顺序入队，随机挑一首作为起点，并把模式切到随机。
    ///
    /// 不洗牌队列本身：官方是「先进队再随机取下一首」，队列抽屉里仍能
    /// 看到真实曲目。顺序上必须先切模式再入队——`play_from` 里的
    /// `queue.replace` 会清空随机轮次，反过来的话刚生成的轮次立刻失效。
    pub fn shuffle_play_arc(
        &mut self,
        tracks: std::sync::Arc<Vec<TrackItem>>,
        cx: &mut Context<Self>,
    ) {
        if tracks.is_empty() {
            return;
        }
        self.queue.mode = sodam_core::queue::PlayMode::Shuffle;
        self.settings.play_mode = self.queue.mode;
        self.save_settings();
        let start = Queue::random_start(tracks.len());
        self.play_from(tracks.as_ref().clone(), start, cx);
        // 入队后立刻生成一轮，让抽屉马上显示真实的「接下来」。
        self.queue.prepare_shuffle_round();
        self.sync_queue_cache();
        cx.notify();
    }

    /// 把整张列表入队并从 `index` 开始播放（「播放全部」与开发入口）。
    ///
    /// 这不是点某一首：起点命中不可播集合时仍走真实装载，由失败分支按 #109 的
    /// 「自动装载第一首也换歌」处理。用户点单首请用 [`Self::play_selected_arc`]。
    pub fn play_from(&mut self, tracks: Vec<TrackItem>, index: usize, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            return;
        }
        let title = tracks
            .get(index)
            .map(|track| track.title.clone())
            .unwrap_or_default();
        let covers: Vec<String> = tracks.iter().map(|track| track.cover.clone()).collect();
        self.queue.replace(tracks, index);
        // 换了一轮队列：上一轮的扫描游标与「整圈无候选」结论不再成立。
        self.new_playback_round();
        // 列表整体换队时清掉旧播放历史，避免抽屉把上一批队列误标成“已播放”。
        self.played_history.clear();
        // 换队时保留用户选好的播放模式：之前这里强制重置成顺序播放，
        // 导致每次点歌都丢掉「单曲循环 / 随机播放」（#12）。
        self.queue_origin = match self.nav {
            Nav::Liked => QueueOrigin::Liked,
            Nav::Library => self
                .open_playlist
                .as_ref()
                .map(|(id, _, _)| QueueOrigin::Playlist(id.clone()))
                .unwrap_or(QueueOrigin::Playlist(String::new())),
            Nav::Artist => self
                .open_artist
                .as_ref()
                .map(|detail| QueueOrigin::Artist(detail.artist.id.clone()))
                .unwrap_or(QueueOrigin::Search),
            Nav::Album => self
                .open_album
                .as_ref()
                .map(|detail| QueueOrigin::Album(detail.album.id.clone()))
                .unwrap_or(QueueOrigin::Search),
            Nav::Search => QueueOrigin::Search,
            Nav::Home => QueueOrigin::Feed,
            _ => self.queue_origin.clone(),
        };
        self.set_status("已入队：{}", std::slice::from_ref(&title));
        // `queue.replace` 会清空随机轮次。随机模式下直接点歌单里某首歌时，
        // 这里要立刻补生成，否则抽屉的「接下来」是空的，要等下一次切歌才有。
        // 其他模式是空操作（`prepare_shuffle_round` 自带判断）。
        self.queue.prepare_shuffle_round();
        self.sync_queue_cache();
        self.ensure_covers(&covers, cx);
        self.start_track(true, cx);
    }

    /// 打开搜索结果里的歌单：带着已知标题，切换到歌单详情页。
    pub fn open_search_playlist(&mut self, playlist: PlaylistItem, cx: &mut Context<Self>) {
        if self.loading_search_playlist {
            return;
        }
        // 从搜索结果点进歌单详情，可拖的列表换了，旧起点作废。
        self.finish_middle_drag();
        let playlist_id = playlist.id.clone();
        self.search_playlist = Some((
            playlist_id.clone(),
            playlist.title.clone(),
            Arc::new(Vec::new()),
        ));
        self.loading_search_playlist = true;
        self.set_status("正在读取歌单「{}」…", std::slice::from_ref(&playlist.title));
        cx.notify();

        let settings = self.settings.clone();
        let work_playlist_id = playlist_id.clone();
        let work = cx.background_spawn(async move {
            sodam_core::library::playlist_tracks(&Session::new(settings), &work_playlist_id)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_search_playlist = false;
                match result {
                    Ok(tracks) => {
                        let count = tracks.len();
                        root.finish_middle_drag_for(DragTarget::SearchPlaylistDetail);
                        root.search_playlist_detail_list.reset(count + 1);
                        let head: Vec<String> = tracks
                            .first()
                            .map(|track| vec![track.cover.clone()])
                            .unwrap_or_default();
                        root.search_playlist =
                            Some((playlist_id, playlist.title, Arc::new(tracks)));
                        root.ensure_covers(&head, cx);
                        root.set_status("搜索歌单共 {} 首", &[count.to_string()]);
                    }
                    Err(err) => {
                        root.search_playlist = None;
                        let message = root.failure_text("读取搜索歌单失败", &err, REMOTE_TEXT);
                        root.notify_error_once("search-playlist-load-failed", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 单曲右键「下一首播放」：插到当前曲目后面，不打断当前歌。
    pub fn play_track_next(&mut self, track: TrackItem, cx: &mut Context<Self>) {
        let title = track.title.clone();
        let cover = track.cover.clone();
        let was_empty = self.queue.insert_next(vec![track]);
        self.track_menu = None;
        self.set_status("下一首将播放：{}", std::slice::from_ref(&title));
        self.sync_queue_cache();
        self.ensure_covers(std::slice::from_ref(&cover), cx);
        if was_empty {
            self.start_track(true, cx);
        } else {
            self.spawn_prefetch(cx);
        }
        cx.notify();
    }

    /// 打开歌单：拉取曲目并展示（标题来自列表，曲目来自接口）。
    pub fn open_playlist(
        &mut self,
        playlist_id: String,
        known_title: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.loading_playlist {
            return;
        }
        // 从歌单网格点进详情，可拖的列表换了，旧起点作废。
        self.finish_middle_drag();
        let playlist_title = known_title.unwrap_or_else(|| {
            self.playlists
                .iter()
                .find(|playlist| playlist.id == playlist_id)
                .map(|playlist| playlist.title.clone())
                .unwrap_or_else(|| self.tr("歌单").to_string())
        });

        // 立刻切到详情页（曲目先空着），页面内显示 loading ——
        // 不然大歌单要等好几秒才「进得去」。
        self.open_playlist = Some((
            playlist_id.clone(),
            playlist_title.clone(),
            Arc::new(Vec::new()),
        ));
        self.finish_middle_drag_for(DragTarget::LibraryDetail);
        self.library_detail_list.reset(1);
        self.loading_playlist = true;
        self.set_status_text(self.tr("正在读取歌单曲目…"));
        cx.notify();

        let settings = self.settings.clone();
        let work_id = playlist_id.clone();
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            sodam_core::library::playlist_tracks(&session, &work_id)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_playlist = false;
                match result {
                    Ok(tracks) => {
                        // 系统歌单「Ta喜欢的音乐」在部分账号上普通详情接口会返回空；
                        // 已有 liked 全量数据时直接兜底，避免详情页空白。
                        let tracks = if tracks.is_empty()
                            && playlist_title.contains("喜欢")
                            && !root.liked.is_empty()
                        {
                            root.liked.as_ref().clone()
                        } else {
                            tracks
                        };
                        root.set_status("歌单共 {} 首", &[tracks.len().to_string()]);
                        let actual = tracks.len() as i64;
                        // 接口给的 track_count 可能是过期值（实测 361 vs 实际 414），
                        // 以实际拉到的曲目数为准回写网格。
                        for playlist in root.playlists.iter_mut() {
                            if playlist.id == playlist_id {
                                playlist.track_count = actual;
                            }
                        }
                        let final_count = tracks.len();
                        root.open_playlist = Some((playlist_id, playlist_title, Arc::new(tracks)));
                        root.finish_middle_drag_for(DragTarget::LibraryDetail);
                        root.library_detail_list.reset(final_count + 1);
                    }
                    Err(err) => {
                        let message = root.failure_text("读取歌单失败", &err, REMOTE_TEXT);
                        root.notify_error_once("playlist-detail-load-failed", message);
                        root.open_playlist = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 连败封顶落闸：把声音真的停下。
    ///
    /// 只写 `playing = false` 拦不住任何事——引擎照旧出声，播完照旧翻下一首，
    /// 于是「连续 3 首没播出来，已暂停」成了谎话（真机踩到）。
    pub(crate) fn halt_playback(&mut self) {
        let engine = &self.engine;
        halt_playback_with(&mut self.playback_halted, &mut self.playing, || {
            engine.pause()
        });
    }

    /// 扫一圈都没有可播候选：落闸，并把本轮结论留在常驻错误位。
    ///
    /// 条件解除（账号 / 会员状态变化让集合作废、或队列换新）后由心跳自动撤下。
    pub(crate) fn halt_exhausted_round(&mut self) {
        let message = self.tr("这一轮没有能播的歌了").to_string();
        self.round_exhausted = Some(message.clone());
        self.playback_error = Some(message.clone());
        self.notify_error_once("round-exhausted", message);
        self.halt_playback();
    }

    /// 撤下「这一轮没有能播的歌了」。
    ///
    /// 只在常驻位还挂着这句时才清空：期间它可能已被别的错误占用，不能顺手抹掉。
    pub(crate) fn clear_round_exhaustion(&mut self) {
        if let Some(message) = self.round_exhausted.take() {
            if self.playback_error.as_deref() == Some(message.as_str()) {
                self.playback_error = None;
            }
        }
    }

    /// 换队列或成功装载：上一轮的扫描游标与「整圈无候选」判定都作废。
    fn new_playback_round(&mut self) {
        self.unplayable.reset_scan();
        self.clear_round_exhaustion();
    }

    /// 当前位置之后是否还有可播候选（不含当前曲目）。
    fn has_playable_ahead(&self) -> bool {
        let unplayable = &self.unplayable;
        self.queue
            .next_playable(|id| unplayable.is_unavailable(id))
            .is_some()
    }

    /// 自动续播（曲目播完）：按不可播集合选下一个候选，不再为已知不可播的曲目打网络。
    ///
    /// 与 [`Self::next_track`] 分开，是因为手动「下一首」是用户显式动作，
    /// 单曲循环下的「播完重播」也仍走 [`Self::next_track`] 的既有语义。
    pub(crate) fn auto_advance_track(&mut self, cx: &mut Context<Self>) {
        self.progress_preview = None;
        let has_candidate = {
            let unplayable = &self.unplayable;
            self.queue
                .advance_playable(|id| unplayable.is_unavailable(id))
                .is_some()
        };
        if !has_candidate {
            // 扫一圈只剩当前这首：它没被确认不可播就接着放——列表循环绕回自己，
            // 随机播放视为新一轮只剩这一首。只有当前这首也不可用时，
            // 「这一轮没有能播的歌了」才是真话。
            let current_playable = self
                .queue
                .current()
                .is_some_and(|track| !self.unplayable.is_unavailable(&track.id));
            if current_playable {
                self.sync_queue_cache();
                self.start_track(false, cx);
                return;
            }
            self.halt_exhausted_round();
            cx.notify();
            return;
        }
        self.sync_queue_cache();
        self.start_track(false, cx);
    }

    /// 用户明确要求继续播放：熔断回到半开，连败重新数。
    ///
    /// 只在「本来落着闸」时复位，自动跳歌那条路（还没到封顶阈值）不会路过这里，
    /// 否则每跳一首都把连败清零，封顶永远够不着。
    pub(crate) fn release_playback_halt(&mut self) {
        if self.playback_halted {
            self.playback_halted = false;
            self.consecutive_failures = 0;
        }
    }

    /// 播放 / 暂停：引擎没装载当前曲目时先装载，否则只切换播放状态。
    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.pending_track.is_some() {
            return;
        }
        let snapshot = self.engine.snapshot();
        let needs_load = snapshot.track_id.is_empty()
            || !snapshot.error.is_empty()
            || snapshot.finished
            || self
                .queue
                .current()
                .is_some_and(|track| track.id != snapshot.track_id);
        if needs_load {
            if self.queue.is_empty() {
                self.release_playback_halt();
                self.consecutive_failures = 0;
                self.set_status_text(self.tr("队列是空的：先去搜索或打开一张歌单"));
                cx.notify();
                return;
            }
            // 队列里有歌但引擎还没装载 → 装载当前曲目
            let Some(track) = self.queue.current().cloned() else {
                return;
            };
            // 先拦再复位闸门：被拦下的点播没有尝试装载，不构成「用户要求重试」。
            if self.block_explicit_play(&track, cx) {
                return;
            }
            self.release_playback_halt();
            self.consecutive_failures = 0;
            self.start_track(true, cx);
            return;
        }
        self.release_playback_halt();
        // 恢复前先把暂停区间记到观察边界，避免播放上报把暂停时长算进收听。
        if !snapshot.playing {
            self.play_reporter.observe(&snapshot);
        }
        self.engine.toggle();
        self.playing = !snapshot.playing;
        self.wake_heartbeat();
        if self.playing {
            // 已装载曲目的「按播放」同样是用户主动播放：磁盘类预取挂起由它解除，
            // 与 `start_track(user_initiated = true)` 保持同一语义（票 #117）。
            log_prefetch_resume(self.prefetch_gate.note_user_playback());
        }
        let message = if self.playing {
            self.tr("播放中").to_string()
        } else {
            self.tr("已暂停").to_string()
        };
        self.set_status_text(message);
        cx.notify();
    }

    /// 用户显式点播命中不可播集合：不发起装载、不换歌，直接给按曲目类的原因。
    ///
    /// 返回 `true` 表示已拦截；调用方必须在**改队列之前**调用，播放状态才不会
    /// 指向新曲目、声音还停在旧曲目。两条边界：
    /// * 已落盘的**整曲**缓存先于结论（试听片段不算），装载它是本地读文件；
    /// * 账号 id 或会员状态变化即整份作废，作废后 `reason` 返回 `None`，
    ///   点播自然回到真实装载请求。
    fn block_explicit_play(&mut self, track: &TrackItem, cx: &mut Context<Self>) -> bool {
        let Some(reason) = self.unplayable.reason(&track.id) else {
            return false;
        };
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.has_full_track_cached(&track.id))
        {
            return false;
        }
        if log_outlet::enabled(LogLevel::Info, LogTopic::Player) {
            log_event(
                LogLevel::Info,
                LogTopic::Player,
                "explicit_unplayable",
                &[("track_id", track.id.as_str()), ("reason", reason.as_str())],
            );
        }
        let message = self.tr(per_track_reason_text(reason)).to_string();
        // 一次性反馈：状态行 + 轻提示，不占用常驻错误位。
        self.notify_warning_once("track-unavailable-explicit", message);
        cx.notify();
        true
    }

    /// 装载并播放「当前队列曲目」：后台下载+解密 → 回 UI 线程交给引擎。
    ///
    /// `user_initiated` 只用于预取管线的恢复信号：磁盘类挂起等的是「下一次用户
    /// 主动播放」，自动续播 / 自动跳过不算——它们可能正被同一个故障卡着。
    pub fn start_track(&mut self, user_initiated: bool, cx: &mut Context<Self>) {
        let Some(track) = self.queue.current().cloned() else {
            self.set_status_text(self.tr("队列是空的：先去搜索或打开一张歌单"));
            cx.notify();
            return;
        };
        // 走到这里就是一次被明确要求（或被允许）的播放尝试：闸门若开着，说明是用户
        // 按了播放/切歌，熔断回半开。自动续播路径在巡检处已被闸门拦住。
        self.release_playback_halt();
        // 恢复信号按故障类分：磁盘类以下一次用户主动播放为事件（票 #112）。
        // 自动续播 / 自动跳过不解除：它们可能正被同一个故障卡着。
        if user_initiated {
            log_prefetch_resume(self.prefetch_gate.note_user_playback());
        }
        self.playing = true;
        self.play_seq = self.play_seq.wrapping_add(1);
        let request_seq = self.play_seq;
        self.pending_track = Some(track.clone());
        self.pending_engine_load = None;
        self.progress_preview = None;
        self.wake_heartbeat();
        self.set_status("正在准备播放：{}…", std::slice::from_ref(&track.title));
        // 每次切歌都只留新曲目的原图需求；不在歌词页时也要清掉上一轮的排队和补排。
        self.cover_store.retain_original_only(&track.cover);
        self.ensure_covers(std::slice::from_ref(&track.cover), cx);
        if self.nav == Nav::Lyrics {
            self.load_lyrics(track.clone(), false, cx);
        }
        cx.notify();

        let settings = self.settings.clone();
        let account_is_vip = self.account.as_ref().map(|a| a.vip);
        // 这次尝试属于哪一份登录身份：回来时若已经换过，结论不成立。
        let auth_epoch = self.auth_epoch;
        let work_track = track.clone();
        // 单层预算：还要不要再来一次由**这次失败的分类**决定。
        // 之前是无条件 3 次线性退避，于是「这首已下架」「签名服务要求鉴权」这类
        // 永远不会因为重试变好的失败也要打满三次——既拖慢恢复，又白踩共享服务。
        // 单次尝试内部的「卡住」由 libresoda 的空闲读超时（20s）负责；这里刻意不设
        // 总墙钟上限，否则等于把刚拆掉的「整首下载总时限」从另一侧装回来。
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            let was_cached = session.is_cached(&work_track.id);
            // 轨迹写在后台线程：开始 / 每次失败判定 / 最终成功或放弃各一行。
            // 永久失败只留一次尝试的记录，不会因为预算再打一次远程。
            let trace = PlaybackTrace::global(&work_track.id);
            let result = drive_play_attempt(
                PlayBudget::default(),
                trace.as_ref(),
                std::time::Instant::now(),
                |_| session.download_to_cache(&work_track, account_is_vip),
                std::thread::sleep,
            );
            (result, was_cached)
        });
        cx.spawn(async move |this, cx| {
            let (result, was_cached) = work.await;
            let _ = this.update(cx, |root, cx| {
                // 这次加载已经被更新的播放请求取代 → 整个结果作废
                if root.play_seq != request_seq {
                    if log_outlet::enabled(LogLevel::Debug, LogTopic::Player) {
                        let seq = request_seq.to_string();
                        let current = root.play_seq.to_string();
                        log_event(
                            LogLevel::Debug,
                            LogTopic::Player,
                            "load_stale",
                            &[
                                ("seq", seq.as_str()),
                                ("current", current.as_str()),
                                ("track_id", track.id.as_str()),
                            ],
                        );
                    }
                    return;
                }
                match result {
                    Ok(cached) => {
                        let actual_rank = cached.quality_rank;
                        let actual_quality = cached.quality.clone();
                        let intent = root.settings.quality_intent();
                        let (is_vip, account_user_id) = root.upgrade_account();
                        let scope = root.upgrade_scope();
                        let needs_upgrade = was_cached
                            && cached.needs_auto_upgrade(&intent, is_vip, scope.as_deref());
                        let sequence = root.engine.load(track.clone(), cached);
                        root.pending_engine_load = Some(sequence);
                        root.progress_token = Some(sequence);
                        root.playback_error = None;
                        // 成功装载：上一轮「扫一圈」的游标作废。
                        root.new_playback_round();
                        // 记录真实播放历史（去重最近一条，避免重复刷屏）
                        if root.played_history.last().map(String::as_str) != Some(track.id.as_str())
                        {
                            root.played_history.push(track.id.clone());
                            let overflow = root.played_history.len().saturating_sub(100);
                            if overflow > 0 {
                                root.played_history.drain(0..overflow);
                            }
                        }
                        // 命中缓存但档位低于目标，且没有探测过该目标档位时，后台升级。
                        if needs_upgrade {
                            root.spawn_background_cache_upgrade(
                                track.clone(),
                                intent.clone(),
                                is_vip,
                                account_user_id,
                                cx,
                            );
                        }
                        // 与所选不符：低于和高于都用同一句格式说出两侧，不猜原因——
                        // 低于是「没有权益」与「这首没有该档位」返回值相同；高于是候选集里
                        // 没有等于或低于上限的流，服务端退回全量取最优（#82），但据此说
                        // 「这首只有更高档」是过头的：实测回退的全是试听片段。
                        // 通道走状态行，#94 之后它有渲染出口，这类事实不该打断用户。
                        let mismatch =
                            sodam_core::quality::visible_tier_mismatch(&intent, actual_rank);
                        let selected_name = crate::ui::player_bar::quality_label(
                            intent.as_setting(),
                            root.language,
                        );
                        let actual_name =
                            crate::ui::player_bar::localized_quality(actual_quality, root.language);
                        let both = [selected_name, actual_name];
                        // 三条文案都写状态行。不符的两句模板放在核心层（`TierMismatch::notice_key`），
                        // 与档位菜单共用同一条叫法；「播放中」这条字面量留在 `.set_status(` 后面，
                        // 好让 i18n 守卫 `every_static_ui_key_has_an_english_entry` 扫到。
                        match mismatch {
                            Some(direction) => {
                                root.set_status(direction.notice_key(), &both);
                            }
                            None => {
                                root.set_status("播放中：{}", std::slice::from_ref(&track.title));
                            }
                        }
                        // 开播后立刻预取下一首，切歌时基本是秒开
                        root.spawn_prefetch(cx);
                    }
                    Err(failure) => {
                        // 技术原文只进日志漏斗（阶段 + 已切断查询串的原始错误）；
                        // 界面上只留下面那一句人话。
                        let context = load_failure_context(failure.stage, root.language);
                        root.log_failure(&context, &failure);
                        let engine_playing = root.engine.snapshot().playing;
                        let identity_current = clear_attempt_and_check_identity(
                            &mut root.pending_track,
                            &mut root.progress_token,
                            &mut root.playing,
                            engine_playing,
                            root.auth_epoch == auth_epoch,
                        );
                        // 尝试跨过一次登录 / 账号身份变更：结论属于旧身份，别用它
                        // 落闸、挂预取、写不可播集合，也别替用户跳歌。
                        root.handle_play_failure(
                            identity_current,
                            PlayIntent::Now,
                            &failure,
                            &track,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn handle_play_failure(
        &mut self,
        identity_current: bool,
        intent: PlayIntent,
        failure: &PlaybackFailure,
        track: &TrackItem,
        cx: &mut Context<Self>,
    ) {
        if !identity_current {
            return;
        }
        let recorded_at = std::time::Instant::now();
        let previous_failures = self.consecutive_failures;
        let mut decision = decide_play_failure(PlayFailureInput {
            intent,
            failure,
            has_candidate: has_playable_after_failure(
                &self.queue,
                &self.unplayable,
                &track.id,
                failure,
                recorded_at,
            ),
            consecutive_failures: previous_failures,
            mode: self.queue.mode,
        });
        self.suspend_prefetch(failure);

        match decision.record {
            FailureRecord::None => {}
            FailureRecord::Unplayable => {
                self.unplayable
                    .record_failure_at(&track.id, failure, recorded_at);
            }
        }
        if counts_consecutive_failure(intent, failure) {
            self.consecutive_failures += 1;
        }

        let mut advanced = false;
        if decision.advance == Advance::Stay && decision.halt == Halt::ExhaustedRound {
            // 整圈只清理失效 redo，不移动队列。
            let unplayable = &self.unplayable;
            self.queue
                .discard_unavailable_redo(|id| unplayable.is_unavailable(id));
        }
        if decision.advance == Advance::Next {
            advanced = advance_failure_queue(&mut self.queue, &self.unplayable);
            if !advanced {
                decision = decide_play_failure(PlayFailureInput {
                    intent,
                    failure,
                    has_candidate: false,
                    consecutive_failures: previous_failures,
                    mode: self.queue.mode,
                });
            }
        }
        let notice_key = match decision.halt {
            Halt::ExhaustedRound => "round-exhausted",
            _ if decision.record == FailureRecord::Unplayable => "track-unavailable",
            _ => "track-stream-failed",
        };
        let skipped = skipped_after_advance(decision, advanced);

        let message = if decision.halt == Halt::ExhaustedRound {
            self.tr("这一轮没有能播的歌了").to_string()
        } else {
            self.tr(play_failure_text(failure, skipped)).to_string()
        };
        match decision.notice.slot {
            NoticeSlot::None => {
                if decision.halt == Halt::None {
                    self.playback_error = None;
                    if decision.record == FailureRecord::Unplayable {
                        self.clear_round_exhaustion();
                    }
                }
            }
            NoticeSlot::PlaybackError => self.playback_error = Some(message.clone()),
            NoticeSlot::RoundExhausted => {
                self.round_exhausted = Some(message.clone());
                self.playback_error = Some(message.clone());
            }
        }
        match decision.notice.channel {
            NoticeChannel::None => {}
            NoticeChannel::Warning => {
                self.notify_warning_once(notice_key, message.clone());
            }
            NoticeChannel::Error => {
                self.notify_error_once(notice_key, message.clone());
            }
            NoticeChannel::WarningThenError => {
                self.notify_warning_once(notice_key, message.clone());
            }
        }

        match decision.halt {
            Halt::None => {}
            Halt::ExhaustedRound | Halt::Playback => self.halt_playback(),
            Halt::ConsecutiveFailures => {
                self.halt_playback();
                let halted = self.tr("连续 3 首没播出来，已暂停").to_string();
                self.playback_error = Some(halted.clone());
                self.notify_error_once("consecutive-stream-failures", halted);
            }
        }

        match decision.advance {
            Advance::Stay => {}
            Advance::RetrySame => self.advance_next(false, cx),
            Advance::Next => {
                self.progress_preview = None;
                self.sync_queue_cache();
                self.start_track(false, cx);
            }
        }
    }

    /// 切到下一个播放模式：列表循环 → 单曲循环 → 随机播放（对齐官方轮换顺序）。
    pub fn cycle_play_mode(&mut self, cx: &mut Context<Self>) {
        self.queue.mode = self.queue.mode.next();
        // 换模式后重算随机顺序，避免沿用上一个模式留下的待播队列。
        self.queue.reset_shuffle_round();
        // 切到随机时立刻按当前歌单生成一整轮，否则面板要等下一次切歌才有内容，
        // 看起来像是「只有当前这首在随机」。
        self.queue.prepare_shuffle_round();
        self.settings.play_mode = self.queue.mode;
        let label = self.tr(self.queue.mode.label());
        self.set_status("播放模式：{}", &[label.to_string()]);
        self.save_settings();
        cx.notify();
    }

    /// 用户点了下一首（按钮 / 快捷键 / 媒体键）：算主动播放，自动路径见 [`Self::advance_next`]。
    pub fn next_track(&mut self, cx: &mut Context<Self>) {
        self.advance_next(true, cx);
    }

    /// 推进到下一首。`user_initiated` 区分用户按钮与自动路径（播完重播、
    /// 单曲循环原地重试）：只有前者算预取管线磁盘类挂起的恢复事件。
    fn advance_next(&mut self, user_initiated: bool, cx: &mut Context<Self>) {
        // 切歌时丢掉进度预览，避免「拖到一半换歌」把 seek 用到新歌上。
        // 队列当前项不会被移除（remove 拒绝当前项），所以直接推进即可。
        self.progress_preview = None;
        self.queue.advance();
        self.sync_queue_cache();
        self.start_track(user_initiated, cx);
    }

    /// 用户点了上一首：同样算主动播放。
    pub fn prev_track(&mut self, cx: &mut Context<Self>) {
        self.progress_preview = None;
        self.queue.rewind();
        self.sync_queue_cache();
        self.start_track(true, cx);
    }

    /// 发起一次配置热重载轮询（main.rs 心跳每 ~500ms 调用一次）。
    ///
    /// 返回 `None` 表示上一次轮询还在后台跑，这次跳过。
    pub(crate) fn begin_config_poll(&mut self) -> Option<super::ConfigPollRequest> {
        if self.config_poll_busy {
            return None;
        }
        self.config_poll_busy = true;
        Some(super::ConfigPollRequest {
            tracker: self.config_reload.clone(),
            path: Settings::config_path(),
            current: self.settings.clone(),
            cookie_fill: if self.env_cookie_opted_out {
                EnvCookieFill::Skip
            } else {
                EnvCookieFill::Allow
            },
        })
    }

    /// 收轮询结果并应用（UI 线程）。
    ///
    /// `snapshot` 是发起轮询时的设置快照：若本地在这段往返时间里又改过，
    /// 就不拿旧快照之后的外部结果整份覆盖新值（#185 审计）——丢弃本次应用且
    /// 不确认指纹，下一轮轮询会拿新快照重新比较，外部改动不会被吞掉。
    pub(crate) fn finish_config_poll(
        &mut self,
        event: sodam_core::config_watch::ReloadEvent,
        snapshot: Settings,
        cx: &mut Context<Self>,
    ) {
        self.config_poll_busy = false;
        match event {
            sodam_core::config_watch::ReloadEvent::Quiet
            | sodam_core::config_watch::ReloadEvent::Unchanged => {}
            sodam_core::config_watch::ReloadEvent::Apply {
                settings,
                fingerprint,
            } => {
                // 本地没动过才应用；应用了才确认这份指纹——被丢弃时留给下一轮
                // 重新处理（下一轮拿新快照比较，文件没变就 Unchanged 收尾）。
                if self.settings == snapshot {
                    // 登出记号只在「外部文件写进非空 Cookie」时解除。记号生效时
                    // 轮询传 `EnvCookieFill::Skip`，补出来的 Cookie 一定是空的；
                    // 能走到这里，且本地这份快照还是登出后的空 Cookie，说明非空
                    // Cookie 来自文件（手动粘贴/换号），不是环境变量（#192）。
                    if !settings.cookie.trim().is_empty() {
                        self.env_cookie_opted_out = false;
                    }
                    self.apply_external_settings(*settings, cx);
                    let mut tracker = self
                        .config_reload
                        .lock()
                        .unwrap_or_else(|err| err.into_inner());
                    tracker.acknowledge(Some(fingerprint));
                }
            }
            sodam_core::config_watch::ReloadEvent::Corrupt(error) => {
                self.log_failure("配置文件热重载解析失败", &error);
                let message = self.localized(
                    "配置文件解析失败（{}）：已保留当前设置，未改动文件",
                    &[error],
                );
                self.notify_warning_once("config-reload-corrupt", message);
                cx.notify();
            }
        }
    }

    /// 外部改动的配置热生效：只应用、不保存（#185）。
    ///
    /// 字段分三类：
    /// * 已有即时生效路径的（主题/语言/音量/播放模式/缓存上限/详细日志/快捷键）——
    ///   沿用同样的应用逻辑，只是不回写文件；
    /// * 会话类（签名地址/Token/Cookie/设备身份/音质意图）——重建会话，当前曲目
    ///   不断，下一次取流/搜索/上报用新值；Cookie 变更视为换身份；
    /// * 其余（搜索历史、titlebar 等）——随 `settings` 整体替换自然生效。
    fn apply_external_settings(&mut self, mut incoming: Settings, cx: &mut Context<Self>) {
        let old = self.settings.clone();

        // 防回跳：用户正拖着音量滑条时，外部重载不动音量。
        if self.volume_scrub.scrubbing() {
            incoming.volume = old.volume;
            incoming.volume_before_mute = old.volume_before_mute;
        }
        // 设备身份不轮换：外部文件没带（或带坏了）身份时，沿用当前已生成的这份。
        if !incoming.login_device().is_valid() && old.login_device().is_valid() {
            incoming.login_device_id = old.login_device_id.clone();
            incoming.login_install_id = old.login_install_id.clone();
        }
        let shortcuts_changed = old.shortcut_toggle != incoming.shortcut_toggle
            || old.shortcut_previous != incoming.shortcut_previous
            || old.shortcut_next != incoming.shortcut_next;
        // 另算一份「音质变没变」不算第二张分类表：它只决定要不要按新意图重新预取，
        // 要不要重建会话由下面的 `session_inputs_differ` 统一判。
        let quality_changed =
            old.quality != incoming.quality || old.quality_source != incoming.quality_source;
        let session_changed = incoming.session_inputs_differ(&old);
        let cookie_changed = incoming.cookie != old.cookie;
        self.settings = incoming.clone();

        // 主题：与 `set_ui_theme*` 同一套应用逻辑（认不得的值回到跟随系统），不落盘。
        if old.theme != incoming.theme {
            self.apply_theme_state(&incoming.theme, cx);
        }
        // 语言：与 `set_language` 同一套应用逻辑，但不落盘。
        if old.language != incoming.language {
            self.apply_language_state(&incoming.language, cx);
        }
        // 详细日志即时生效。
        if old.detailed_logs != incoming.detailed_logs {
            log_outlet::set_global_detailed(incoming.detailed_logs);
        }
        // 音量与静音前音量：直接喂引擎（set_volume 内部已钳制）。
        if old.volume != incoming.volume || old.volume_before_mute != incoming.volume_before_mute {
            self.engine.set_volume(incoming.volume);
        }
        // 播放模式：队列按钮在播放栏，settings 换掉后同步给队列。
        // 切到随机要重建随机轮次：面板的「接下来」直接读 shuffle_round，
        // 不重建会显示空列表或旧模式残留的顺序。
        if old.play_mode != incoming.play_mode {
            self.queue.mode = incoming.play_mode;
            self.queue.reset_shuffle_round();
            self.queue.prepare_shuffle_round();
        }
        // 缓存上限：触一次维护，让超限的旧缓存马上被清。
        if old.audio_cache_limit_mb != incoming.audio_cache_limit_mb {
            self.maintain_cache_now(cx);
        }
        // 快捷键：置脏位，main.rs 心跳里统一重绑（清空 + 重建 keymap）。
        if shortcuts_changed {
            self.shortcuts_dirty.store(true, Ordering::Release);
        }
        // 会话类改动：重建会话（不落盘）。当前曲目不断；下一次取流/搜索/上报用新值。
        if session_changed {
            match self.session.as_mut() {
                Some(session) => session.adopt(&incoming),
                None => self.session = Some(Session::new(&incoming)),
            }
        }
        self.play_reporter.apply_settings(incoming.clone());
        // Cookie 变了就是换了身份：旧身份的结论全部作废，账号与「我喜欢的」按新身份刷新。
        if cookie_changed {
            if incoming.cookie.trim().is_empty() {
                self.clear_identity_state(cx);
            } else {
                self.invalidate_identity_conclusions();
                // A 的账号信息（昵称/VIP）不再代表当前身份：先清空，刷新成功写入
                // B 的信息；刷新失败时 VIP 状态按未知处理，不按 A 的算（#185 审计）。
                self.account = None;
                if self.nav == Nav::Settings {
                    self.refresh_upgrade_estimate(cx);
                }
                // 旧账号的歌单/收藏缓存是身份私有数据：换号不清，新账号会直接
                // 看到旧账号的列表（加载守卫会拦住重拉）。保留当前播放。
                self.playlists.clear();
                self.liked = Arc::new(Vec::new());
                self.liked_ids = Arc::new(HashSet::new());
                self.liked_ids_failed = false;
                self.liked_loaded = false;
                self.open_playlist = None;
                // 常用模式/探索歌单同样属于上一个账号：清掉；正停在这一页就用新身份重读，
                // 否则页面会空到用户离开再回来。
                self.reset_scene_state();
                if self.nav == Nav::Scenes {
                    self.load_scenes(cx);
                }
                self.refresh_account(cx);
                self.load_liked_ids(cx);
            }
        }
        // 音质意图变了：按新意图重新预取下一首（正在播的这首不受影响）。
        if quality_changed {
            self.spawn_prefetch(cx);
        }

        // 接受了外部改动，备份也跟上：热重载不写盘，备份不同步的话，
        // 之后主文件损坏走恢复会退回加载时的旧值（#185 审计）。IO 放后台。
        let backup_path = Settings::config_path();
        let backup_settings = self.settings.clone();
        cx.background_spawn(async move {
            let _ = sodam_core::config::write_settings_backup(&backup_path, &backup_settings);
        })
        .detach();

        // 普通反馈只走底部状态行（#203）：状态行本身是新文案顶掉旧文案，
        // 不会再叠出第二条；全局轻提示留给需要叫住用户的事件。
        self.set_status_text(self.tr("配置文件已更新，改动已生效"));
        cx.notify();
    }
}

/// 外部打开的两种语义，不可互换。
#[derive(Debug)]
pub(crate) enum ExternalOpen {
    /// 交给系统默认浏览器/协议处理程序的 URL。
    Url(&'static str),
    /// 交给系统默认应用的本地路径（文件或目录）；Linux 下裸路径进 `open_url` 会因 URI 解析失败而静默无操作。
    Path(PathBuf),
}

fn config_open_target() -> ExternalOpen {
    ExternalOpen::Path(Settings::config_path())
}

fn log_directory_open_target() -> ExternalOpen {
    ExternalOpen::Path(sodam_core::log_outlet::log_dir())
}

fn repository_open_target() -> ExternalOpen {
    ExternalOpen::Url(env!("CARGO_PKG_REPOSITORY"))
}

/// gpui 的入口返回 `()` 且失败只写日志，所以这里既没有错误分支，也不写状态文案。
fn apply_external_open(cx: &mut Context<Root>, target: ExternalOpen) {
    match target {
        ExternalOpen::Url(url) => cx.open_url(url),
        ExternalOpen::Path(path) => cx.open_with_system(&path),
    }
}

/// 预取逐曲冷却：非按曲目、非全局的失败在 60 秒内不再重排。
///
/// 按曲目结论由不可播集合永久拦截，全局故障由管线闸门挂起——这张冷却表两者都不收，
/// 所以它没有「永久失败」这一档，也不该用 `Retryability::Never` 当跳过判据。
///
/// 与 `PrefetchGate::COOLDOWN` 数值相同但作用域不同（逐曲 vs 整条管线），刻意各留
/// 一个常量：以后调一处时不该被动改另一处。
const PREFETCH_RETRY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// 冷却是否仍在生效。只按时间判定：这里没有「永久」分支（见上面的常量注释）。
fn prefetch_cooldown_active(failed_at: std::time::Instant, now: std::time::Instant) -> bool {
    now.saturating_duration_since(failed_at) < PREFETCH_RETRY_COOLDOWN
}

/// 记下一次逐曲冷却；超过上限时淘汰最旧的一条。
///
/// 冷却表只影响下一轮重排，淘汰最旧条目最多让某一首早一点重试，不会产生永久黑名单。
fn note_prefetch_cooldown(
    cooldown: &mut HashMap<String, std::time::Instant>,
    track_id: &str,
    now: std::time::Instant,
) {
    cooldown.insert(track_id.to_string(), now);
    if cooldown.len() <= 256 {
        return;
    }
    let oldest = cooldown
        .iter()
        .min_by_key(|(_, failed_at)| **failed_at)
        .map(|(track_id, _)| track_id.clone());
    if let Some(oldest) = oldest {
        cooldown.remove(&oldest);
    }
}

/// 预取路径对一次失败的三种处置，与 `is_per_track` / `is_global_failure` 同一判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefetchFailureRoute {
    /// 按曲目不可播：进不可播集合，后面的歌照常排。
    PerTrack,
    /// 全局故障：挂起整条管线。
    Suspend,
    /// 其余失败：只给这一首 60 秒冷却。
    Cooldown,
}

fn prefetch_failure_route(failure: &PlaybackFailure) -> PrefetchFailureRoute {
    if failure.is_per_track() {
        PrefetchFailureRoute::PerTrack
    } else if is_global_failure(failure) {
        PrefetchFailureRoute::Suspend
    } else {
        PrefetchFailureRoute::Cooldown
    }
}

/// 预取管线挂起/恢复只在日志里留痕（对用户静默）。
///
/// 同一段故障最多每分钟一对（冷却到期后重试仍失败会再挂起一次），
/// 巡检每 5 秒路过一次不会重复刷屏。
fn log_prefetch_suspension(class: SuspensionClass) {
    log_event(
        LogLevel::Info,
        LogTopic::Prefetch,
        "suspended",
        &[("class", class.as_str())],
    );
}

/// `None` 表示这次事件没有解除任何挂起（不写日志）。
fn log_prefetch_resume(reason: Option<SuspensionClass>) {
    let Some(reason) = reason else {
        return;
    };
    log_event(
        LogLevel::Info,
        LogTopic::Prefetch,
        "resumed",
        &[("reason", reason.as_str())],
    );
}

/// 阶段名的中文原文（稳定 i18n key）。
///
/// 「地址解析（含签名）」不是笔误：上游把这两步的失败压成了同一个无相位错误，
/// 现在分不开就明说分不开，别假装报出了四个独立阶段。
fn stage_label(stage: Stage) -> &'static str {
    match stage {
        Stage::ResolveStream => "地址解析（含签名）",
        Stage::Transfer => "音频体传输",
        Stage::Unpack => "解密落盘",
        Stage::Input => "参数",
        Stage::Network => "网络传输",
    }
}

fn has_playable_after_failure(
    queue: &Queue,
    unplayable: &UnplayableTracks,
    track_id: &str,
    failure: &PlaybackFailure,
    recorded_at: std::time::Instant,
) -> bool {
    queue
        .next_playable(|id| {
            unplayable.is_unavailable_after_failure(track_id, id, failure, recorded_at)
        })
        .is_some()
}

fn advance_failure_queue(queue: &mut Queue, unplayable: &UnplayableTracks) -> bool {
    queue
        .advance_playable(|id| unplayable.is_unavailable(id))
        .is_some()
}

fn skipped_after_advance(decision: PlayFailureDecision, advanced: bool) -> bool {
    decision.advance == Advance::Next && advanced
}

fn clear_attempt_and_check_identity(
    pending_track: &mut Option<TrackItem>,
    progress_token: &mut Option<u64>,
    playing: &mut bool,
    engine_playing: bool,
    identity_current: bool,
) -> bool {
    *pending_track = None;
    *progress_token = None;
    if identity_current {
        return true;
    }
    *playing = engine_playing;
    false
}

/// 状态位与暂停回调同一入口，测试可确认没有只改标志位。
fn halt_playback_with(playback_halted: &mut bool, playing: &mut bool, pause: impl FnOnce()) {
    *playback_halted = true;
    *playing = false;
    pause();
}

/// 日志里的装载失败上下文。阶段名也走词典：界面是什么语言，日志就是什么语言，
/// 不必让用户（或替用户看日志的人）在英文界面里读中文阶段名。
fn load_failure_context(stage: Stage, language: Language) -> String {
    format!("播放装载失败（{}）", language.text(stage_label(stage)))
}

/// 一次播放尝试失败后给人看的那一句（中文原文即稳定 i18n key）。
///
/// TLS 握手失败、超时、DNS 对用户是同一件事，合并成一条——多个可能原因不该甩给用户排查
/// （Microsoft / NN/g 的成文准则）。上游技术原文一律只进 `log_failure`，不上界面。
/// 三条共用说法 `REMOTE_TEXT` / `SAVE_TEXT` / `RETRY_TEXT` 定义在 [`super`]。
///
/// `skipped` 表示这次失败真的已经换到了下一首：只有此时才能说「已跳过」。
/// 自动装载第一首那条路也照这条规矩来（`pause_pending`）。
fn play_failure_text(failure: &PlaybackFailure, skipped: bool) -> &'static str {
    match failure.action {
        // 登录类（登录过期、缺 Cookie / 缺登录用户 id）是全局故障，换一首也一样。
        RecoveryAction::Relogin | RecoveryAction::CheckEntitlement => {
            "账号登录已过期，已停止播放。"
        }
        RecoveryAction::CheckSigner => "这首歌暂时放不出来。签名服务不可用，可在设置里更换。",
        // 按曲目不可播的两种成因分成两句话；都不报歌名，识别交给列表标记。
        RecoveryAction::TrackUnavailable => {
            if skipped {
                "这首在你的账号下没有整曲，已跳过。"
            } else {
                per_track_reason_text(UnplayableReason::NoFullTrack)
            }
        }
        RecoveryAction::PickAnother => {
            if skipped {
                "这首当前取不到，已跳过。"
            } else {
                per_track_reason_text(UnplayableReason::Unavailable)
            }
        }
        RecoveryAction::Undecodable => {
            if skipped {
                "这首的音频解不开，已跳过。"
            } else {
                per_track_reason_text(UnplayableReason::DecodeFailed)
            }
        }
        RecoveryAction::FreeSpace => "本地缓存写入失败，检查磁盘空间。",
        RecoveryAction::Nothing => match failure.stage {
            Stage::Unpack => "本地缓存写入失败，检查磁盘空间。",
            // 参数本身不合法是我们的问题，不该让用户去查网络。
            Stage::Input => "这首歌暂时放不出来。",
            _ => "暂时连不上，检查网络后重试。",
        },
    }
}

/// 按曲目不可播的成因文案（不含「已跳过」）：显式点播没有跳过这个动作，
/// 用户点的是这首歌，程序替他跳到别处就违背了意图。
fn per_track_reason_text(reason: UnplayableReason) -> &'static str {
    match reason {
        UnplayableReason::NoFullTrack => "这首在你的账号下没有整曲。",
        UnplayableReason::Unavailable => "这首当前取不到。",
        UnplayableReason::DecodeFailed => "这首的音频解不开。",
    }
}

/// 自动装载第一首、但不换歌时（全局故障与网络抖动）给用户看的那一句。
///
/// 这条路没有播过声音，登录类不能说「已停止播放」；关于某一首的失败走
/// [`play_failure_text`] 的 `skipped=true`（真的换到下一首时），不经过这里。
fn pending_failure_text(failure: &PlaybackFailure) -> &'static str {
    match failure.action {
        RecoveryAction::Relogin | RecoveryAction::CheckEntitlement => {
            "账号登录已过期，请重新登录。"
        }
        _ => play_failure_text(failure, false),
    }
}

/// 熔断复位判据：声音播到这么多秒才算「真的缓过来」。
const PLAYBACK_RECOVER_SECONDS: f64 = 3.0;

/// 推荐队列请求的客户端截止时间。
///
/// 底层 HTTP 在断网 / 黑洞路由下可能很久才放弃，而推荐是唯一一条「先换队列再拉数据」
/// 的入口：请求不落地，全局 loading 就一直为真，左栏「推荐」会被静默 return（#55）。
/// 10s 取 NN/g 响应时间三限值的上界：超过它必须有可见的失败终态与可打断的入口。
/// 超时只终结界面状态，底层请求仍会在后台自己跑完，结果被 `load_seq` 丢弃。
const RECOMMENDATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// `future::select` 的本地实现：两个 future，谁先完成就用谁的结果。
///
/// 返回 `Some(value)` 表示 `work` 先完成，`None` 表示截止时间先到。
///
/// 拿 `&mut F` 而不是 `F`：`gpui::Task` 被 drop 时会 `set_canceled()` 真正取消任务
/// （`async_task::Task` 的 Drop 行为）。超时分支的调用方需要任务活下来去把那一次
/// 请求跑完，所以所有权必须留在外面，由调用方决定要不要 `detach()`。
/// `futures_lite::future::or` 会吃掉 future，这里不能用它替代。
async fn first_of<F>(
    work: &mut F,
    deadline: impl std::future::Future<Output = ()>,
) -> Option<F::Output>
where
    F: std::future::Future + Unpin,
{
    use std::pin::pin;
    use std::task::Poll;

    let mut deadline = pin!(deadline);
    std::future::poll_fn(move |cx| {
        if let Poll::Ready(value) = std::pin::Pin::new(&mut *work).poll(cx) {
            return Poll::Ready(Some(value));
        }
        match deadline.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

/// 一次「播出去了没有」的判定材料。
///
/// `PlaybackSnapshot` 有 `pub(crate)` 字段，本 crate 连 `..Default::default()` 都构造不出，
/// 所以判定放在这个自有小结构上，测试可以离线直接喂数据。
struct AttemptProgress {
    /// 最近一次**成功装载**的引擎序号；一旦有装载失败就作废。
    token: Option<u64>,
    load_seq: u64,
    playing: bool,
    position_seconds: f64,
    errored: bool,
}

impl AttemptProgress {
    fn of(snap: &PlaybackSnapshot, token: Option<u64>) -> Self {
        Self {
            token,
            load_seq: snap.load_seq,
            playing: snap.playing,
            position_seconds: snap.position_seconds,
            errored: !snap.error.is_empty(),
        }
    }

    /// 连败该不该清零：**最近一次成功装载**的那首真的播出去若干秒才算。
    ///
    /// 判据刻意不看「此刻有没有声音」：上一首还在放而后面几首装载失败时，ambient
    /// 的播放状态会把计数一直清零，封顶就永远触发不了（真机踩过）。归属到最近一次
    /// 尝试才是业界一致做法——gRPC 的 attempt、Media3 的 `loadTaskId`/`EventTime`、
    /// mpv 挂在 playlist entry 上的 `init_failed` 都是这个形状。
    fn recovered(self) -> bool {
        self.token == Some(self.load_seq)
            && self.playing
            && !self.errored
            && self.position_seconds >= PLAYBACK_RECOVER_SECONDS
    }
}

/// 预取数量：环境变量 `SODAM_PREFETCH_COUNT`（默认 3，解析失败/为 0 用默认）。
fn prefetch_ahead_count() -> usize {
    std::env::var("SODAM_PREFETCH_COUNT")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or(3)
}

/// seek 之前产生的完成事件已经失效；UI 只消费序号并跳过该事件。
fn finished_event_is_current(
    finished_seq: u64,
    seek_seq: u64,
    last_finished_seq: &mut u64,
    last_seek_seq: &mut u64,
) -> bool {
    let stale_finished = seek_seq != *last_seek_seq;
    *last_seek_seq = seek_seq;
    let current = finished_seq != *last_finished_seq && !stale_finished;
    // 只消费一次：成功处理过完成事件后回写，否则已上报的 finished 会在
    // 下一页心跳里继续返回 true，自动切换到下一首。
    if current {
        *last_finished_seq = finished_seq;
    }
    current
}

/// 这次扫盘的结果还能不能落回状态：档位没变，而且它是最新发起的那次。
///
/// 进设置页、切档位、升级成功都会各发一次后台扫盘。先发的慢扫描晚到，
/// 不能把升级后的新数字盖回旧值，否则又变回「停在打开那一刻」（#248）。
fn estimate_result_is_current(
    current_intent: &sodam_core::quality::QualityIntent,
    current_seq: u64,
    result_intent: &sodam_core::quality::QualityIntent,
    result_seq: u64,
) -> bool {
    current_intent == result_intent && current_seq == result_seq
}

/// 预取并发上限：环境变量 `SODAM_PREFETCH_CONCURRENCY`（默认 2）。
fn prefetch_concurrency() -> usize {
    std::env::var("SODAM_PREFETCH_CONCURRENCY")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or(2)
}

#[cfg(test)]
mod batch_upgrade_notice_tests {
    use super::*;

    #[test]
    fn no_higher_only_uses_normal_status() {
        let tally = Tally {
            upgraded: 0,
            failed: 0,
            no_higher: 2,
            unknown_result: 0,
        };
        assert_eq!(
            batch_upgrade_notice(tally, "summary".into()),
            BatchUpgradeNotice::Status("summary".into())
        );
    }

    #[test]
    fn unknown_result_only_uses_normal_status() {
        let tally = Tally {
            upgraded: 0,
            failed: 0,
            no_higher: 0,
            unknown_result: 2,
        };
        assert_eq!(
            batch_upgrade_notice(tally, "summary".into()),
            BatchUpgradeNotice::Status("summary".into())
        );
    }

    #[test]
    fn any_failure_uses_warning() {
        let tally = Tally {
            upgraded: 1,
            failed: 1,
            no_higher: 0,
            unknown_result: 0,
        };
        assert_eq!(
            batch_upgrade_notice(tally, "summary".into()),
            BatchUpgradeNotice::Warning("summary".into())
        );
    }

    #[test]
    fn upgrades_without_failures_use_success() {
        let tally = Tally {
            upgraded: 2,
            failed: 0,
            no_higher: 1,
            unknown_result: 0,
        };
        assert_eq!(
            batch_upgrade_notice(tally, "summary".into()),
            BatchUpgradeNotice::Success("summary".into())
        );
    }
}

#[cfg(test)]
mod upgrade_estimate_tests {
    use super::estimate_result_is_current;
    use sodam_core::quality::QualityIntent;

    /// 只有最新一次扫盘能落回状态：慢扫描晚到、切档位后的旧结果、A→B→A 都要丢掉。
    #[test]
    fn only_newest_estimate_scan_lands() {
        let lossless = QualityIntent::Tier("lossless".into());
        let highest = QualityIntent::Tier("highest".into());
        assert!(estimate_result_is_current(&lossless, 2, &lossless, 2));
        assert!(!estimate_result_is_current(&lossless, 2, &lossless, 1));
        assert!(!estimate_result_is_current(&highest, 2, &lossless, 2));
        assert!(!estimate_result_is_current(&lossless, 3, &lossless, 1));
    }
}

#[cfg(test)]
mod play_failure_flow_tests {
    use super::*;
    use sodam_core::failure::Retryability;

    fn per_track_failure() -> PlaybackFailure {
        PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::TrackUnavailable,
            "returned preview stream",
        )
    }

    fn full_unplayable_set() -> UnplayableTracks {
        let mut set = UnplayableTracks::default();
        let failure = per_track_failure();
        for index in 0..UnplayableTracks::CAPACITY {
            set.record_failure(&format!("old-{index}"), &failure);
        }
        set.reset_scan();
        set
    }

    fn queue_with_old_tracks() -> Queue {
        let mut tracks = vec![TrackItem {
            id: "current".into(),
            ..Default::default()
        }];
        tracks.extend((0..UnplayableTracks::CAPACITY).map(|index| TrackItem {
            id: format!("old-{index}"),
            ..Default::default()
        }));
        Queue::new(tracks)
    }

    #[test]
    fn candidate_projection_matches_advance_when_eviction_frees_a_track() {
        let set = full_unplayable_set();
        let mut queue = queue_with_old_tracks();
        let failure = per_track_failure();
        let recorded_at = std::time::Instant::now();
        assert!(has_playable_after_failure(
            &queue,
            &set,
            "current",
            &failure,
            recorded_at,
        ));

        let mut actual = set.clone();
        actual.record_failure_at("current", &failure, recorded_at);
        assert!(advance_failure_queue(&mut queue, &actual));
    }

    #[test]
    fn candidate_projection_matches_advance_when_incoming_is_evicted() {
        let set = full_unplayable_set();
        let mut queue = queue_with_old_tracks();
        let failure = PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Transient,
            RecoveryAction::TrackUnavailable,
            "preview stream",
        );
        let recorded_at = std::time::Instant::now();
        assert!(!has_playable_after_failure(
            &queue,
            &set,
            "current",
            &failure,
            recorded_at,
        ));

        let mut actual = set.clone();
        actual.record_failure_at("current", &failure, recorded_at);
        assert!(!advance_failure_queue(&mut queue, &actual));
    }

    #[test]
    fn skipped_copy_waits_for_real_queue_movement() {
        let failure = per_track_failure();
        let decision = decide_play_failure(PlayFailureInput {
            intent: PlayIntent::Now,
            failure: &failure,
            has_candidate: true,
            consecutive_failures: 0,
            mode: PlayMode::ListLoop,
        });
        let mut queue = Queue::new(vec![
            TrackItem {
                id: "current".into(),
                ..Default::default()
            },
            TrackItem {
                id: "next".into(),
                ..Default::default()
            },
        ]);
        let before = queue.current().map(|track| track.id.clone());
        let advanced = advance_failure_queue(&mut queue, &UnplayableTracks::default());
        let skipped = skipped_after_advance(decision, advanced);
        assert!(skipped);
        assert_ne!(queue.current().map(|track| track.id.clone()), before);
        assert!(play_failure_text(&failure, skipped).contains("已跳过"));

        let mut one_track = Queue::new(vec![TrackItem {
            id: "only".into(),
            ..Default::default()
        }]);
        let advanced = advance_failure_queue(&mut one_track, &UnplayableTracks::default());
        assert!(!advanced);
        assert!(!play_failure_text(&failure, false).contains("已跳过"));
    }

    #[test]
    fn attempt_cleanup_distinguishes_current_and_stale_identity() {
        let mut pending = Some(TrackItem {
            id: "track".into(),
            ..Default::default()
        });
        let mut progress_token = Some(7);
        let mut playing = true;
        assert!(clear_attempt_and_check_identity(
            &mut pending,
            &mut progress_token,
            &mut playing,
            false,
            true,
        ));
        assert!(pending.is_none());
        assert!(progress_token.is_none());
        assert!(playing);

        pending = Some(TrackItem {
            id: "old-account".into(),
            ..Default::default()
        });
        progress_token = Some(8);
        playing = true;
        assert!(!clear_attempt_and_check_identity(
            &mut pending,
            &mut progress_token,
            &mut playing,
            false,
            false,
        ));
        assert!(pending.is_none());
        assert!(progress_token.is_none());
        assert!(!playing);
    }

    #[test]
    fn halt_updates_flags_and_calls_the_engine_pause() {
        let mut playback_halted = false;
        let mut playing = true;
        let mut engine_paused = false;
        halt_playback_with(&mut playback_halted, &mut playing, || {
            engine_paused = true;
        });
        assert!(playback_halted);
        assert!(!playing);
        assert!(engine_paused);
    }
}

#[cfg(test)]
mod failure_text_tests {
    use super::*;
    use sodam_core::failure::Retryability;

    /// 界面只说人话：不复述上游技术原文，也不带接口地址。
    #[test]
    fn play_failure_text_is_human_only() {
        let raw = "soda track_v2 failed: https://x.test/a?sign=SECRET: tls connection init failed";
        let signer = "这首歌暂时放不出来。签名服务不可用，可在设置里更换。";
        let login = "账号登录已过期，已停止播放。";
        let no_full_track = "这首在你的账号下没有整曲，已跳过。";
        let unavailable = "这首当前取不到，已跳过。";
        let cache = "本地缓存写入失败，检查磁盘空间。";
        let undecodable_skipped = "这首的音频解不开，已跳过。";
        let offline = "暂时连不上，检查网络后重试。";
        let cant_play = "这首歌暂时放不出来。";
        let at = |stage, action| {
            play_failure_text(
                &PlaybackFailure::new(stage, Retryability::Never, action, raw),
                true,
            )
        };

        assert_eq!(at(Stage::Network, RecoveryAction::Nothing), offline);
        assert_eq!(at(Stage::Transfer, RecoveryAction::Nothing), offline);
        assert_eq!(at(Stage::Unpack, RecoveryAction::Nothing), cache);
        assert_eq!(at(Stage::Input, RecoveryAction::Nothing), cant_play);
        assert_eq!(at(Stage::ResolveStream, RecoveryAction::Relogin), login);
        assert_eq!(
            at(Stage::ResolveStream, RecoveryAction::CheckEntitlement),
            login
        );
        assert_eq!(
            at(Stage::ResolveStream, RecoveryAction::CheckSigner),
            signer
        );
        assert_eq!(
            at(Stage::ResolveStream, RecoveryAction::TrackUnavailable),
            no_full_track
        );
        assert_eq!(
            at(Stage::ResolveStream, RecoveryAction::PickAnother),
            unavailable
        );
        assert_eq!(at(Stage::ResolveStream, RecoveryAction::FreeSpace), cache);
        // 解密失败不再借磁盘话术：说「解不开」。
        assert_eq!(
            at(Stage::Unpack, RecoveryAction::Undecodable),
            undecodable_skipped
        );
        assert_eq!(
            play_failure_text(
                &PlaybackFailure::new(
                    Stage::Unpack,
                    Retryability::Never,
                    RecoveryAction::Undecodable,
                    raw
                ),
                false
            ),
            "这首的音频解不开。"
        );

        for stage in [Stage::Network, Stage::Transfer, Stage::ResolveStream] {
            let text = at(stage, RecoveryAction::Nothing);
            assert!(!text.contains("tls"), "{text}");
            assert!(!text.contains("://"), "{text}");
            assert!(!text.contains("SECRET"), "{text}");
        }
    }

    /// 三句按类文案互不相同、都有英文，且按曲目文案不报歌名。
    #[test]
    fn failure_copy_is_split_by_class_and_translated() {
        let at = |action| {
            play_failure_text(
                &PlaybackFailure::new(
                    Stage::ResolveStream,
                    Retryability::Never,
                    action,
                    "returned preview stream",
                ),
                true,
            )
        };
        let login = at(RecoveryAction::Relogin);
        let no_full_track = at(RecoveryAction::TrackUnavailable);
        let unavailable = at(RecoveryAction::PickAnother);
        let decode_failed = at(RecoveryAction::Undecodable);

        assert_ne!(login, no_full_track);
        assert_ne!(no_full_track, unavailable);
        assert_ne!(login, unavailable);
        assert_ne!(decode_failed, no_full_track);
        assert_ne!(decode_failed, unavailable);
        // 解密失败不再借「检查磁盘空间」的话术。
        assert_ne!(decode_failed, "本地缓存写入失败，检查磁盘空间。");
        assert!(no_full_track.contains("已跳过"), "{no_full_track}");
        assert!(unavailable.contains("已跳过"), "{unavailable}");
        assert!(decode_failed.contains("已跳过"), "{decode_failed}");

        for key in [login, no_full_track, unavailable, decode_failed] {
            let english = Language::English.text(key);
            assert_ne!(english, key, "英文词典缺少：{key}");
            // 被跳过的曲目不报歌名：文案是静态的，不含任何曲目标题占位符。
            assert!(!key.contains("{"), "{key}");
        }
    }

    /// 没有真的换到下一首时用的那套措辞（`skipped=false`）不能说「已跳过」；
    /// 自动装载第一首那条路的登录类提示也不能说「已停止播放」。
    #[test]
    fn non_skipping_copy_does_not_claim_skipped() {
        let at = |action| {
            play_failure_text(
                &PlaybackFailure::new(Stage::ResolveStream, Retryability::Never, action, "detail"),
                false,
            )
        };
        for key in [
            at(RecoveryAction::TrackUnavailable),
            at(RecoveryAction::PickAnother),
            at(RecoveryAction::Undecodable),
        ] {
            assert!(!key.contains("已跳过"), "{key}");
            let english = Language::English.text(key);
            assert_ne!(english, key, "英文词典缺少：{key}");
        }

        // 自动装载第一首时还没开始播放，登录类不能说「已停止播放」。
        let login = pending_failure_text(&PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::Relogin,
            "detail",
        ));
        assert!(login.contains("请重新登录"), "{login}");
        assert!(!login.contains("已停止播放"), "{login}");
        let english = Language::English.text(login);
        assert_ne!(english, login, "英文词典缺少：{login}");
    }

    /// 「已停止播放」只能给真的会落闸的点播入口；预装仍说重新登录。
    #[test]
    fn stopped_playback_copy_only_when_point_playback_halts() {
        let login = PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::Relogin,
            "detail",
        );
        for (intent, expected_halt) in [
            (PlayIntent::Now, Halt::Playback),
            (PlayIntent::PrepareFirst, Halt::None),
        ] {
            let decision = decide_play_failure(PlayFailureInput {
                intent,
                failure: &login,
                has_candidate: false,
                consecutive_failures: 0,
                mode: PlayMode::ListLoop,
            });
            assert_eq!(decision.halt, expected_halt, "{intent:?}");
            let text = if intent == PlayIntent::Now {
                play_failure_text(&login, false)
            } else {
                pending_failure_text(&login)
            };
            assert_eq!(
                text.contains("已停止播放"),
                intent == PlayIntent::Now,
                "{text}"
            );
        }
    }

    /// 共用说法里不该再出现 `{err}` 占位符——那等于把原文再塞回界面。
    #[test]
    fn shared_failure_texts_carry_no_placeholders() {
        for text in [REMOTE_TEXT, SAVE_TEXT, RETRY_TEXT] {
            assert!(!text.contains("{err}"), "{text}");
            assert!(!text.contains("：:"), "{text}");
        }
    }
}

#[cfg(test)]
mod prefetch_policy_tests {
    use super::*;
    use sodam_core::failure::Retryability;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    /// 逐曲冷却只按时间判定，60 秒到点即放行，没有「永久」分支。
    #[test]
    fn cooldown_expires_and_has_no_permanent_branch() {
        let failed_at = Instant::now();
        assert!(prefetch_cooldown_active(failed_at, failed_at));
        assert!(prefetch_cooldown_active(
            failed_at,
            failed_at + PREFETCH_RETRY_COOLDOWN - Duration::from_millis(1)
        ));
        assert!(!prefetch_cooldown_active(
            failed_at,
            failed_at + PREFETCH_RETRY_COOLDOWN
        ));
        assert!(!prefetch_cooldown_active(
            failed_at,
            failed_at + Duration::from_secs(3600)
        ));
    }

    /// 冷却表到上限时淘汰最旧的一条：只影响重试顺序，不形成永久条目。
    #[test]
    fn cooldown_table_caps_at_256_and_evicts_the_oldest() {
        let base = Instant::now();
        let mut cooldown = HashMap::new();
        for index in 0..=256 {
            note_prefetch_cooldown(
                &mut cooldown,
                &format!("track-{index}"),
                base + Duration::from_millis(index as u64),
            );
        }
        assert_eq!(cooldown.len(), 256);
        assert!(!cooldown.contains_key("track-0"), "最旧的条目应先出局");
        assert!(cooldown.contains_key("track-1"));
        assert!(cooldown.contains_key("track-256"));
    }

    /// 无法归类、可重试性为 `Never` 的失败不能变成永久黑名单：它只进冷却。
    ///
    /// 对应 #117 的三种来源：非鉴权业务码、未识别的 HTTP 状态码。
    /// （解密失败后来归成了「解不开」，按曲目走，见下。）按曲目类与全局类
    /// 仍各走自己的出口，三者的判据是同一份失败分类。
    #[test]
    fn unclassified_never_failures_route_to_cooldown_not_a_permanent_ban() {
        let cooldown = [
            PlaybackFailure::new(
                Stage::ResolveStream,
                Retryability::Never,
                RecoveryAction::Nothing,
                "api business code",
            ),
            PlaybackFailure::new(
                Stage::ResolveStream,
                Retryability::Never,
                RecoveryAction::Nothing,
                "http request failed: status 400",
            ),
        ];
        for failure in cooldown {
            assert_eq!(
                prefetch_failure_route(&failure),
                PrefetchFailureRoute::Cooldown,
                "{failure:?}"
            );
        }

        let per_track = PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::TrackUnavailable,
            "returned preview stream",
        );
        assert_eq!(
            prefetch_failure_route(&per_track),
            PrefetchFailureRoute::PerTrack
        );

        // 解密失败归成的「解不开」走按曲目分支：进不可播集合，不进 60 秒逐曲冷却。
        // （真实 SodaError → Undecodable 的分类断言在 sodam-core 的 failure / prefetch 测试里。）
        let decode = PlaybackFailure::new(
            Stage::Unpack,
            Retryability::Never,
            RecoveryAction::Undecodable,
            "decrypted size mismatch",
        );
        assert_eq!(
            prefetch_failure_route(&decode),
            PrefetchFailureRoute::PerTrack,
            "{decode:?}"
        );

        let signer = PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::CheckSigner,
            "signer error",
        );
        assert_eq!(
            prefetch_failure_route(&signer),
            PrefetchFailureRoute::Suspend
        );
    }
}

#[cfg(test)]
mod recommendation_gate_tests {
    use super::*;

    /// 一个极简的单线程阻塞执行器：只要能把 future 轮流 poll 到完成即可。
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::task::{Context, Poll};

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// 截止时间必须真的能打断「一直在等」的请求，而不是只写一个常量。（#55）
    #[test]
    fn deadline_fires_without_a_result() {
        use std::time::{Duration, Instant};

        let started = Instant::now();
        let mut work = std::future::pending::<u8>();
        let outcome = block_on(first_of(&mut work, async {
            std::thread::sleep(Duration::from_millis(10));
        }));
        assert_eq!(outcome, None, "超时必须以「没有结果」的形态返回");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "不该等到请求自己结束"
        );
    }

    /// 结果先到时不能被截止时间抢走。
    #[test]
    fn early_result_wins_over_the_deadline() {
        let mut work = std::future::ready(7u8);
        let outcome = block_on(first_of(&mut work, std::future::pending::<()>()));
        assert_eq!(outcome, Some(7));
    }

    /// 超时分支必须 `detach()` 后台任务，而不是让它在 select 结构里被丢掉。
    ///
    /// 第一版实现真犯过这个错：`gpui::Task` drop 时会走到 `async_task::Task::drop`
    /// 的 `set_canceled()`，于是「界面不再等」连带把请求也取消了。gpui 没有暴露
    /// 可离线构造 `Task` 的测试设施（需要 `test-support` feature），所以这里用源码
    /// 约束代替运行时断言：超时分支的代码里必须出现 `detach()`。
    #[test]
    fn timeout_branch_detaches_instead_of_dropping_the_task() {
        let source = include_str!("app_actions.rs");
        let branch = source
            .split("if result.is_none()")
            .nth(1)
            .expect("超时分支应当存在");
        let branch = &branch[..branch.find("let _ = this.update").unwrap_or(branch.len())];
        assert!(
            branch.contains("detach()"),
            "超时后必须 detach 后台任务，否则请求会被真正取消：{branch}"
        );
    }

    /// 截止时间要落在 NN/g 的那个界里：超过 10s 必须有可打断的失败终态。
    #[test]
    fn deadline_is_in_the_interactive_budget() {
        assert!(RECOMMENDATION_DEADLINE >= std::time::Duration::from_secs(1));
        assert!(RECOMMENDATION_DEADLINE <= std::time::Duration::from_secs(10));
    }

    /// 有失败终态时，左栏「推荐」的语义必须是重试，而不是跳播放页。
    ///
    /// 否则：失败后队列里还留着上一次的推荐内容、`queue_origin` 仍是 `Feed`，
    /// `activate_recommendation` 会直接跳去播放页——用户看不到失败提示，
    /// 只能靠页面上的重试按钮，两个入口行为自相矛盾（#55）。
    #[test]
    fn clicking_home_with_a_failed_terminal_state_retries() {
        let source = include_str!("app_actions.rs");
        let body = source
            .split("pub fn activate_recommendation")
            .nth(1)
            .expect("activate_recommendation 应当存在");
        let body = &body[..body.find("pub fn begin_queue_switch").unwrap_or(body.len())];

        let retry_at = body
            .find("recommendation_error.is_some()")
            .expect("必须先判断失败终态：否则断网失败后点「推荐」会跳到播放页，永远不会重试");
        let feed_at = body
            .find("QueueOrigin::Feed)")
            .expect("应当保留「已是推荐队列则跳播放页」的分支");
        assert!(
            retry_at < feed_at,
            "重试判断必须排在「已是推荐队列」之前，否则不会被命中"
        );
    }
}

#[cfg(test)]
mod external_open_tests {
    use super::*;

    /// 守住院落里最容易犯的回归：把本地路径当 URL 交给 `open_url`。
    #[test]
    fn config_and_log_entries_use_path_semantics_and_repo_entry_uses_url() {
        assert!(matches!(config_open_target(), ExternalOpen::Path(_)));
        assert!(matches!(log_directory_open_target(), ExternalOpen::Path(_)));
        assert!(matches!(repository_open_target(), ExternalOpen::Url(_)));
    }

    #[test]
    fn seek_invalidates_finished_event_from_before_the_seek() {
        let mut last_finished = 0;
        let mut last_seek = 0;
        assert!(finished_event_is_current(
            1,
            0,
            &mut last_finished,
            &mut last_seek
        ));
        assert_eq!(last_finished, 1);
        // 同一个完成事件不能被连续两拍都消费。
        assert!(!finished_event_is_current(
            1,
            0,
            &mut last_finished,
            &mut last_seek
        ));
        // seek 会让 seek 前积压的完成事件失效。
        assert!(!finished_event_is_current(
            2,
            1,
            &mut last_finished,
            &mut last_seek
        ));
        // 首次消费后回写，让下一首的新完成事件仍能推进。
        assert!(finished_event_is_current(
            3,
            1,
            &mut last_finished,
            &mut last_seek
        ));
        assert_eq!(last_finished, 3);
    }

    #[test]
    fn first_real_completion_after_startup_still_advances() {
        let mut last_finished = 7;
        let mut last_seek = 3;
        assert!(finished_event_is_current(
            8,
            3,
            &mut last_finished,
            &mut last_seek
        ));
    }
}

#[cfg(test)]
mod playback_budget_tests {
    use super::*;
    use crate::ui::i18n::Language;

    fn progress(token: Option<u64>, seq: u64, playing: bool, at: f64) -> AttemptProgress {
        AttemptProgress {
            token,
            load_seq: seq,
            playing,
            position_seconds: at,
            errored: false,
        }
    }

    /// 复位必须归属到**最近一次成功装载**：上一首还在放不算缓过来。
    #[test]
    fn streak_resets_only_for_the_latest_attempt() {
        let fresh = progress(Some(7), 7, true, PLAYBACK_RECOVER_SECONDS).recovered();
        assert!(fresh, "最新一次尝试播满了就该复位");
        // 装载失败把 token 作废之后，旧曲继续播放不该抵账
        let stale = progress(None, 5, true, 90.0).recovered();
        assert!(!stale, "上一首还在放不该抵账");
        // 引擎已经翻到别的装载序号
        let newer = progress(Some(6), 7, true, 90.0).recovered();
        assert!(!newer, "进展属于别的尝试");
    }

    /// 归属对了，还要真的播出去若干秒、且没报错。
    #[test]
    fn streak_needs_audible_progress() {
        let early = progress(Some(3), 3, true, PLAYBACK_RECOVER_SECONDS - 0.1).recovered();
        assert!(!early, "刚交给引擎不该清零");
        let paused = progress(Some(3), 3, false, 90.0).recovered();
        assert!(!paused, "暂停中不该复位");
        let mut broken = progress(Some(3), 3, true, 90.0);
        broken.errored = true;
        assert!(!broken.recovered(), "带错误不该复位");
    }

    /// 封顶的数字改动必须连带那句用户看到的文案。
    #[test]
    fn halt_threshold_matches_the_copy() {
        let shown = Language::Chinese.text("连续 3 首没播出来，已暂停");
        let hits = shown.contains(&sodam_core::play_failure::PLAY_FAILURE_HALT_AFTER.to_string());
        assert!(hits, "{shown}");
    }
}

/// 这一行此刻是否"正在走字"（亮光还在推进）。
///
/// 只有同时满足两个条件才算：有逐字时间轴（否则整行瞬间全亮、没有动画），
/// 且还没走到行尾。这是 60fps 节拍的唯一正当理由 ——
/// 实测无逐字轴的歌若仍按 60fps 重绘，会白占近半个核。
fn line_undergoing_reveal(line: &LyricLine, position: f64) -> bool {
    // 下界同样重要：第一句开始前 `lyrics_active` 会回退到 0，
    // 只判上界会让**前奏阶段**也按 60fps 重绘，而画面完全不变。
    line.has_word_timeline && position >= line.start_seconds && position < line.end_seconds
}

/// 没有走字动画时，下一拍该睡多久。
///
/// 目标是「下一句开始」那一刻 —— 那时才需要重新点亮并开始走字。
///
/// 为什么要提前 `SAFETY`：`position` 来自音频快照，最多滞后约 100ms。
/// 若直接睡到 `next_start`，可能睡过头，导致新一句的高亮迟到（肉眼是"亮晚了"）。
/// 提前一点醒来最多多跑几拍（下界 8ms），代价远小于迟到。
/// 空闲时"提前量"：在下一句开始前这么久就醒，留出渲染与状态更新的余量。
const LYRIC_IDLE_SAFETY: f64 = 0.1;

fn lyrics_idle_wait(position: f64, next_start: Option<f64>) -> std::time::Duration {
    const MIN: f64 = 0.008;
    const MAX: f64 = 0.12;
    match next_start {
        Some(next) if next > position => std::time::Duration::from_secs_f64(
            (next - position - LYRIC_IDLE_SAFETY).clamp(MIN, MAX),
        ),
        _ => std::time::Duration::from_millis(120),
    }
}

/// 是否已进入「下一句马上开始」的窗口。
///
/// 一旦进入就必须 notify 一次：位置与当前行只在渲染路径里更新，
/// 不 notify 的话下一次醒来仍拿旧位置判定"还没到"，会一直空转到主心跳。
fn lyrics_near_next_line(position: f64, next_start: Option<f64>) -> bool {
    next_start.is_some_and(|next| next - position <= LYRIC_IDLE_SAFETY)
}

/// 歌词滚动的缓出曲线：`1 - (1-t)^3`。
///
/// 起步快、收尾慢，符合"滑到位置"的观感，与官方 `transition: ... ease-out` 一致。
/// 单独抽出来是为了能脱离窗口环境测试（其余滚动逻辑都要 `ScrollHandle`）。
fn lyrics_scroll_ease(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    // 三次贝塞尔近似 `cubic-bezier(0, 0, 0.58, 1)`（即 CSS 的 ease-out）。
    //
    // 之前用的 `1-(1-t)^3` 首帧就吃掉 17.6% 的位移，峰均比 2.82 ——
    // 起步那一下太猛，肉眼像"弹"过去而不是"滑"过去。
    // 这条曲线首帧 10.2%、峰均比 1.63，加速度更平缓，也正好是 CSS ease-out。
    cubic_bezier_ease_out(progress)
}

/// `cubic-bezier(0, 0, 0.58, 1)` 在给定 x 上的 y 值。
///
/// 贝塞尔参数方程里 x 和 y 都由同一个 t 表达，给定 x 求 y 需要先解出 t。
/// 用二分即可：曲线在两个方向上都是单调的，20 次迭代精度远高于一像素。
fn cubic_bezier_ease_out(x: f32) -> f32 {
    const P1X: f32 = 0.0;
    const P1Y: f32 = 0.0;
    const P2X: f32 = 0.58;
    const P2Y: f32 = 1.0;

    let bezier = |t: f32, p1: f32, p2: f32| {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    };

    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..20 {
        let mid = (lo + hi) * 0.5;
        if bezier(mid, P1X, P2X) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bezier((lo + hi) * 0.5, P1Y, P2Y)
}

#[cfg(test)]
mod tests {
    use super::{
        cache_upgrade_log_parts, current_cover_url, line_undergoing_reveal, lyrics_idle_wait,
        lyrics_near_next_line, lyrics_scroll_ease, retry_current_original_cover,
        settle_original_cover,
    };
    use sodam_core::cover::CoverOutcome;
    use sodam_core::models::{LyricLine, LyricWord, TrackItem};
    use sodam_core::upgrade::NotImprovedReason;
    use std::path::PathBuf;

    #[test]
    fn cache_upgrade_log_distinguishes_not_improved_reasons() {
        for (reason, expected) in [
            (NotImprovedReason::PreviewOnly, "preview_only"),
            (NotImprovedReason::PreviewBackoff, "preview_backoff"),
            (NotImprovedReason::NotBetter, "not_better"),
            (NotImprovedReason::NoHigherTier, "no_higher_tier"),
            (NotImprovedReason::AlreadyChecked, "already_checked"),
            (NotImprovedReason::UnknownRank, "unknown_rank"),
        ] {
            let (event, fields) = cache_upgrade_log_parts("track-1", "极高 320k", Some(reason));
            assert_eq!(event, "cache_upgrade_not_improved");
            assert_eq!(
                fields,
                vec![
                    ("track_id", "track-1"),
                    ("quality", "极高 320k"),
                    ("reason", expected),
                ]
            );
        }

        let (event, fields) = cache_upgrade_log_parts("track-1", "无损 871k", None);
        assert_eq!(event, "cache_upgraded");
        assert_eq!(
            fields,
            vec![("track_id", "track-1"), ("quality", "无损 871k")]
        );
    }

    /// 清缓存或淘汰撞上在飞的原图：回调解析出路径但文件已经没了，
    /// 不能交给 `CoverStore` 发布死路径，否则这首歌的歌词页大图
    /// 整个会话都回不来。
    #[test]
    fn vanished_original_cover_is_not_published() {
        let path = PathBuf::from("covers/original/gone.img");
        assert_eq!(
            settle_original_cover(Some(path.clone()), |_| false),
            CoverOutcome::Vanished,
            "文件没了就不能发布死路径"
        );
        assert_eq!(
            settle_original_cover(Some(path.clone()), |_| true),
            CoverOutcome::Ready { path, color: None },
            "文件还在就直接用"
        );
        assert_eq!(
            settle_original_cover(None, |_| true),
            CoverOutcome::Failed,
            "下载失败按失败记一次"
        );
    }

    /// 原图没成功要不要补：清缓存进行中交给它的补偿，当前曲目才自己补。
    #[test]
    fn original_cover_retries_only_current_track_outside_a_cache_clear() {
        assert!(
            retry_current_original_cover(false, Some("url/current"), "url/current"),
            "当前曲目的原图没成功：要补"
        );
        assert!(
            !retry_current_original_cover(false, Some("url/other"), "url/current"),
            "不是当前曲目：不补"
        );
        assert!(
            !retry_current_original_cover(false, None, "url/current"),
            "没有当前曲目：不补"
        );
        assert!(
            !retry_current_original_cover(true, Some("url/current"), "url/current"),
            "清缓存进行中：交给 clear_cache 的补偿"
        );
    }

    /// 应用层链路：原图第一次失败后不用再点按钮，当前曲目自动排第二次并落账。
    #[test]
    fn current_original_retry_chain_needs_no_user_action() {
        use sodam_core::cover::{CoverOutcome, CoverStore};

        let mut store = CoverStore::default();
        let url = "url/current";
        assert!(store.want_original(url));
        assert_eq!(store.drain_original(), vec![url.to_string()]);

        store.complete_original(url, CoverOutcome::Failed);
        assert!(
            retry_current_original_cover(false, Some(url), url),
            "第一次失败后当前曲目要自动补排"
        );
        assert!(store.want_original(url), "补排真的进队列");
        assert_eq!(store.drain_original(), vec![url.to_string()]);

        store.complete_original(
            url,
            CoverOutcome::Ready {
                path: PathBuf::from("/tmp/current.img"),
                color: None,
            },
        );
        assert!(store.original_path(url).is_some(), "第二次成功就落账");
    }

    /// 清缓存/淘汰后补下载要跟歌词页显示同一首歌：pending 优先于队列当前。
    /// 取错口径会让正在装载、还没进队列那首歌的大图补不回来。
    #[test]
    fn cover_restore_url_follows_lyrics_page_track_precedence() {
        let make = |id: &str, cover: &str| TrackItem {
            id: id.to_string(),
            cover: cover.to_string(),
            ..Default::default()
        };
        let pending = make("pending", "url/pending");
        let queued = make("queued", "url/queued");
        let padded = make("padded", "  url/queued  ");

        // pending 在场时歌词页显示它，补下载也得用它。
        assert_eq!(
            current_cover_url(Some(&pending), Some(&queued)).as_deref(),
            Some("url/pending")
        );
        // 没有 pending 才退回队列当前曲目。
        assert_eq!(
            current_cover_url(None, Some(&queued)).as_deref(),
            Some("url/queued")
        );
        // 跟 CoverStore 的 key 一样 trim 过，淘汰反查才对得上。
        assert_eq!(
            current_cover_url(None, Some(&padded)).as_deref(),
            Some("url/queued")
        );
        // 两个都没有就没有可补的封面。
        assert_eq!(current_cover_url(None, None), None);
    }

    fn line(start: f64, end: f64, has_word_timeline: bool) -> LyricLine {
        LyricLine {
            start_seconds: start,
            end_seconds: end,
            text: "测试".to_string(),
            words: vec![LyricWord {
                start_seconds: start,
                end_seconds: end,
                text: "测试".to_string(),
            }],
            has_word_timeline,
        }
    }

    /// 只有"有逐字轴且还没走到行尾"才算需要 60fps。
    ///
    /// 无逐字轴的歌整行瞬间全亮，没有任何动画；逐字行走满之后也只是停在原地
    /// 等下一句。这两种情况按 60fps 重绘纯属空转（实测能白占近半个核）。
    #[test]
    fn only_word_timeline_lines_need_high_fps() {
        let timed = line(10.0, 12.0, true);
        // 前奏阶段：第一句还没开始。`lyrics_active` 会回退到 0，
        // 若只判上界就会让前奏也按 60fps 空转（画面完全不变）。
        assert!(!line_undergoing_reveal(&timed, 5.0), "第一句开始前不该高频");
        assert!(line_undergoing_reveal(&timed, 11.0), "走字进行中应高频");
        assert!(!line_undergoing_reveal(&timed, 12.0), "走满后不必高频");
        assert!(!line_undergoing_reveal(&timed, 13.0));

        let plain = line(10.0, 10.0, false);
        assert!(!line_undergoing_reveal(&plain, 10.0), "无逐字轴没有动画");
        assert!(!line_undergoing_reveal(&plain, 11.0));
    }

    /// 进入下一句窗口必须 notify，否则位置不更新会一直空转。
    #[test]
    fn notify_once_the_next_line_window_opens() {
        // 距下一句 0.3s：还没进窗口，安静等待。
        assert!(!lyrics_near_next_line(10.0, Some(10.3)));
        // 距下一句 0.05s：已进窗口，必须 notify 让渲染刷新位置/当前行。
        assert!(lyrics_near_next_line(10.0, Some(10.05)));
        // 刚好在安全提前量上也算（边界闭合）。
        assert!(lyrics_near_next_line(10.0, Some(10.1)));
        // 没有下一句：不需要为了切句而 notify。
        assert!(!lyrics_near_next_line(10.0, None));
    }

    /// 空闲等待朝「下一句开始」对齐，并夹在 8~120ms 之间。
    #[test]
    fn idle_wait_sleeps_until_next_line() {
        // 距离下一句还很久：按上限 120ms 醒一次（保证状态变化能被及时看到）。
        assert_eq!(lyrics_idle_wait(10.0, Some(99.0)).as_millis(), 120);
        // 距下一句 0.3s：扣掉安全提前量后仍超过上限。
        assert_eq!(lyrics_idle_wait(10.0, Some(10.3)).as_millis(), 120);
        // 距下一句 0.15s：扣掉 0.1s 安全量后剩 50ms。
        assert_eq!(lyrics_idle_wait(10.0, Some(10.15)).as_millis(), 50);
        // 下一句马上到：不能退化成 0（否则会在毫秒级空转）。
        assert!(lyrics_idle_wait(10.0, Some(10.0001)).as_millis() >= 8);
        // 没有下一句（最后一行唱完）：走满档等待。
        assert_eq!(lyrics_idle_wait(10.0, None).as_millis(), 120);
        // 边界：下一句等于当前（脏数据）时不能睡 0。
        assert_eq!(lyrics_idle_wait(10.0, Some(10.0)).as_millis(), 120);
    }

    #[test]
    fn ease_starts_at_zero_and_ends_at_one() {
        assert!((lyrics_scroll_ease(0.0) - 0.0).abs() < 1e-6);
        assert!((lyrics_scroll_ease(1.0) - 1.0).abs() < 1e-6);
    }

    /// 缓出：前半程走完一半以上，收尾变慢。
    #[test]
    fn ease_front_loads_the_motion() {
        assert!(
            lyrics_scroll_ease(0.5) > 0.5,
            "0.5 处应已过半，实际 {}",
            lyrics_scroll_ease(0.5)
        );
    }

    /// 起步不能太猛：首帧位移占比过高会让动画看起来像"弹"过去。
    ///
    /// 旧的 `1-(1-t)^3` 在 60fps / 17 帧下首帧就吃掉 17.6%（峰均比 2.82），
    /// 这是"不够丝滑"的直接观感来源之一。
    #[test]
    fn ease_does_not_jump_on_the_first_frame() {
        const FRAMES: usize = 17;
        let first = lyrics_scroll_ease(1.0 / FRAMES as f32);
        assert!(first < 0.13, "首帧位移占比 {} 过大，会显得像弹出", first);
    }

    /// 全程单调递增，不能回弹（回弹会看到歌词"抽一下"）。
    #[test]
    fn ease_is_monotonic() {
        let mut prev = -1.0;
        for i in 0..=100 {
            let value = lyrics_scroll_ease(i as f32 / 100.0);
            assert!(value >= prev, "t={} 处倒退：{} -> {}", i, prev, value);
            prev = value;
        }
    }

    /// 越界输入要夹住，避免进度算飞导致歌词滚出可视区。
    #[test]
    fn ease_clamps_out_of_range_input() {
        assert!((lyrics_scroll_ease(-0.5) - 0.0).abs() < 1e-6);
        assert!((lyrics_scroll_ease(2.0) - 1.0).abs() < 1e-6);
    }
}

#[cfg(test)]
mod scene_modes_gate_tests {
    use super::*;

    /// 失败终态挡住的是自动重发，不是用户点的重试（#213）。
    #[test]
    fn failed_modes_state_blocks_only_automatic_reload() {
        assert!(should_auto_load_scene_page(
            true,
            false,
            SceneModesState::Idle
        ));
        assert!(
            !should_auto_load_scene_page(true, false, SceneModesState::Failed),
            "失败终态还在时进页面不能再自动打请求"
        );
        assert!(
            !should_auto_load_scene_page(false, false, SceneModesState::Idle),
            "探索卡已经有数据就不用再走首屏加载"
        );
        assert!(
            !should_auto_load_scene_page(true, true, SceneModesState::Idle),
            "探索卡请求在飞时不要叠加"
        );
    }

    /// 模式只在没读过时自动读；读完（含读失败）都不再自己重发。
    #[test]
    fn modes_are_requested_only_before_the_first_answer() {
        assert!(SceneModesState::Idle.needs_request());
        for state in [
            SceneModesState::Loading,
            SceneModesState::Ready,
            SceneModesState::Failed,
        ] {
            assert!(!state.needs_request(), "{state:?} 不该再自动读模式");
        }
    }

    /// 重试按钮只重发模式：不追加探索卡，也不复用「没有更多探索歌单」的文案。
    ///
    /// gpui 的 `Context` 没法离线构造，这里按仓库既有做法（#55）用源码约束代替运行时断言。
    #[test]
    fn retrying_modes_does_not_append_scene_cards() {
        let source = include_str!("app_actions.rs");
        let body = source
            .split("pub fn retry_scene_modes")
            .nth(1)
            .expect("retry_scene_modes 应当存在");
        let body = &body[..body.find("pub fn start_scene_queue").unwrap_or(body.len())];

        assert!(
            body.contains("start_scene_modes_request(true"),
            "重试只发模式请求"
        );
        assert!(
            !body.contains("start_scene_cards_request"),
            "重试不该顺手读探索卡"
        );
        assert!(
            !body.contains("没有更多探索歌单"),
            "重试不该复用「加载更多」的文案"
        );
    }

    /// 模式结果不等探索卡：两条请求各自落回，互不阻塞（#213）。
    #[test]
    fn mode_result_does_not_wait_for_the_cards_request() {
        let source = include_str!("app_actions.rs");
        let modes = source
            .split("fn start_scene_modes_request")
            .nth(1)
            .expect("start_scene_modes_request 应当存在");
        let modes = &modes[..modes.find("fn apply_scene_modes").unwrap_or(modes.len())];
        assert!(
            !modes.contains("scene_discover_cards"),
            "模式请求不该把探索卡串在同一条后台任务里"
        );

        let cards = source
            .split("fn start_scene_cards_request")
            .nth(1)
            .expect("start_scene_cards_request 应当存在");
        let cards = &cards[..cards
            .find("pub fn retry_scene_modes")
            .unwrap_or(cards.len())];
        assert!(
            !cards.contains("scene_modes("),
            "探索卡请求不该写常用模式的状态"
        );
        assert!(!cards.contains("scene_modes_state"), "探索卡请求只动探索卡");
    }
}
