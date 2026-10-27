//! 用户动作层：搜索、推荐、媒体库、播放和封面加载。
//!
//! [`Root`] 的字段定义在 [`super`]，这里按领域拆分实现，避免单文件继续膨胀。

use crate::ui::i18n::Language;
use crate::ui::theme::{self, ThemeKind};
use gpui::prelude::*;
use gpui::{Context, Window};
use sodam_core::audio::PlaybackSnapshot;
use sodam_core::failure::{PlayBudget, PlaybackFailure, RecoveryAction, Retryability, Stage};
use sodam_core::library::RecommendationSource;
use sodam_core::log_outlet::{self, log_event, LogLevel, LogTopic};
use sodam_core::models::{LyricLine, PlaylistItem, SceneItem, TrackItem};
use sodam_core::playback_trace::{drive_play_attempt, PlaybackTrace};
use sodam_core::session::Session;
use sodam_core::Settings;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use super::{
    status_label, LoginState, Nav, QueueOrigin, Root, REMOTE_TEXT, RETRY_TEXT, ROW_BATCH, SAVE_TEXT,
};
use sodam_core::queue::Queue;

impl Root {
    /// 设置页贡献者墙：用 contrib.rocks 的组合图，后台拉取并缓存在内存。
    ///
    /// 不用 GitHub API，因此没有 token / 请求配额问题；贡献者增减由服务自动更新。
    pub fn ensure_contributors(&mut self, cx: &mut Context<Self>) {
        if self.contributors_image.is_some() || self.contributors_fetching {
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
        let log = log_outlet::enabled(LogLevel::Debug, LogTopic::Prefetch);
        let ahead = prefetch_ahead_count();
        let max_inflight = prefetch_concurrency().max(1);
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
            // 失败过的曲目按分类处置：
            // * 永久失败（下架 / 无权益 / 签名服务鉴权）——**不再自动排队**。巡检每 5 秒
            //   重排一次就是在持续打扰那台共享签名服务，而这正是预取路径最容易犯的错；
            // * 瞬时 / 限流——进冷却，期满再试。
            if self
                .prefetch_failed
                .get(&next.id)
                .is_some_and(|(failed_at, retryability)| match retryability {
                    Retryability::Never => true,
                    _ => failed_at.elapsed() < PREFETCH_RETRY_COOLDOWN,
                })
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
                Session::new(settings).download_to_cache(&work_track)
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
                let _ = this.update(cx, |root, _cx| {
                    // 只删自己的：避免早先的完成回执误清后来排的曲目（单槽时代踩过）
                    root.prefetch_inflight.remove(&track.id);
                    match &outcome {
                        Ok(_) => {
                            root.prefetch_failed.remove(&track.id);
                        }
                        Err(failure) => {
                            // 只做内存封顶：清掉冷却类，永久类留着当「别再自动排队」。
                            if root.prefetch_failed.len() > 256 {
                                root.prefetch_failed.retain(|_, (_, retryability)| {
                                    *retryability == Retryability::Never
                                });
                            }
                            root.prefetch_failed.insert(
                                track.id.clone(),
                                (std::time::Instant::now(), failure.retryability),
                            );
                        }
                    }
                });
            })
            .detach();
        }
    }

    /// UI 心跳：播放中每 200ms 刷新一次（进度条需要持续重绘），
    /// 曲目播完则自动切下一首。
    pub(crate) fn start_heartbeat(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            let _ = this.update(cx, |root, cx| {
                let snap = root.engine.snapshot();
                // 播放进度 / 时长上报：喂一拍快照（内部异步发请求，不阻塞）
                root.play_reporter.observe(&snap);
                // 加载完成：引擎已切到 pending 曲目 → 清 loading 态
                if root.pending_engine_load == Some(snap.load_seq) {
                    root.pending_engine_load = None;
                    root.pending_track = None;
                    root.playing = snap.playing;
                    cx.notify();
                }
                if root.pending_track.is_none()
                    && !snap.error.is_empty()
                    && !snap.track_id.is_empty()
                    && root.playback_error.as_deref() != Some(snap.error.as_str())
                {
                    root.playing = false;
                    root.playback_error = Some(snap.error.clone());
                    let message = root.language.engine_error(&snap.error);
                    root.notify_error_once("playback-engine-failed", message);
                    cx.notify();
                }
                if let Some(error) = root.preferences_writer.take_error() {
                    let message = root.failure_text("保存播放设置失败", &error, SAVE_TEXT);
                    root.notify_error_once("preferences-save-failed", message);
                    cx.notify();
                }
                if snap.playing || root.pending_track.is_some() {
                    cx.notify();
                }
                // 熔断复位：装载成功不算「缓过来」，声音真的播出去若干秒才算；
                // 而且必须归属到最近一次成功装载——上一首还在放时清零连败，
                // 封顶就永远触发不了（见 AttemptProgress::recovered）。
                let progress = AttemptProgress::of(&snap, root.progress_token);
                if progress.recovered() {
                    root.consecutive_failures = 0;
                    root.progress_token = None;
                }
                // 播完自动下一首：按序号判断（循环同一首也不会漏切）。
                // 注意：正在装载下一首（pending）时「播完」属于旧曲目，
                // 只消费事件不推进——否则会把 pending 的那首跳过去（收尾瞬间
                // 手动切歌/插入队列时踩过）。
                // seek 之前积压的完成事件不能把当前曲目推进到下一首。
                let current = finished_event_is_current(
                    snap.finished_seq,
                    snap.seek_seq,
                    &mut root.last_finished_seq,
                    &mut root.last_seek_seq,
                );
                // 封顶闸门打开时不推进：连续几首都没播出来还往后翻，就是在替用户决定
                // 「再试几首」，而网络恢复该由用户按下播放来确认。
                let should_advance = current && !root.playback_halted;
                if snap.finished && should_advance && root.pending_track.is_none() {
                    root.next_track(cx);
                }
                // 预取巡检：每 25 拍（约 5s）补一次，覆盖 append/remove 等
                // 队列变更路径（spawn_prefetch 自身会去重，空转开销极小）
                root.prefetch_patrol = root.prefetch_patrol.wrapping_add(1);
                if root.prefetch_patrol % 25 == 0 && !root.playback_halted {
                    root.prefetch_failed.retain(|_, (failed_at, retryability)| {
                        *retryability == Retryability::Never
                            || failed_at.elapsed() < PREFETCH_RETRY_COOLDOWN
                    });
                    root.spawn_prefetch(cx);
                }
                if root.prefetch_patrol % 300 == 0 {
                    root.maintain_cache(cx);
                }
                root.load_more_recommendation(cx);
            });
        })
        .detach();
    }

    /// 歌词页的逐字高亮节拍（#80）。
    ///
    /// 主心跳是 200ms（5fps），对进度条够用，但逐字高亮按像素推进时需要更高帧率，
    /// 否则亮光会一跳一跳地走。这里单独跑一个节拍，**只在"亮光确实在推进"时**
    /// 才用 60fps 并触发重绘；其余时间睡到下一个会变化的时刻，一次 `notify` 都不发。
    ///
    /// 判据见 [`line_undergoing_reveal`] 与 [`lyrics_idle_wait`]。
    ///
    /// 为什么不用 `with_animation`：那个是给 SVG / 变换用的，无法带动文字重排；
    /// 而 `cx.notify()` 只是标脏，真正重绘仍由 gpui 的帧调度合并，不会堆帧。
    pub(crate) fn start_lyrics_ticker(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            // 先决定这一拍要不要重绘、以及下一拍睡多久。
            //
            // 关键判据是「**这一行此刻是否真的在走字**」，而不是「有没有当前行」：
            // 没有逐字时间轴的歌（普通 LRC）整行直接全亮，根本不会有动画；
            // 逐字行也有大半时间停在"已走满、等下一句"。这些时候按 60fps 重绘
            // 纯粹是空转 —— 实测一首无逐字轴的歌能占到单核 46%，白白浪费。
            //
            // 所以只在真正需要时用 16ms，其余时间直接**睡到下一个会变化的时刻**。
            let (interval, notify) = this
                .update(cx, |root, _cx| {
                    const HOT: std::time::Duration = std::time::Duration::from_millis(16);
                    const IDLE: std::time::Duration = std::time::Duration::from_millis(120);
                    if root.nav != Nav::Lyrics {
                        return (IDLE, false);
                    }
                    // 样张模式（开发验证）也要能刷新。
                    if root.lyrics_demo_position.is_some() {
                        return (HOT, true);
                    }
                    // 滚动过渡进行中：让 timer 退到空闲档，动画交给
                    // `request_animation_frame` 驱动（见 `Root::render`）。
                    // 两个驱动源同时推进会造成重复推进、间隔不匀，反而更不平滑。
                    if root.lyrics_scroll_animating() {
                        return (IDLE, false);
                    }
                    let snapshot = root.engine.snapshot();
                    if !snapshot.playing {
                        return (IDLE, false);
                    }

                    // 当前行是否还在"走字"：有逐字轴，且还没走到行尾。
                    let revealing = root
                        .lyrics_active
                        .and_then(|index| root.lyrics.get(index))
                        .is_some_and(|line| {
                            line_undergoing_reveal(line, root.lyrics_position_seconds)
                        });
                    if revealing {
                        return (HOT, true);
                    }

                    // 空闲：睡到下一句开始之前，那时才需要重新点亮/开始走字。
                    //
                    // 用快照位置而不是 `lyrics_position_seconds`：后者只在渲染时更新，
                    // 空闲期间不渲染就会滞后，按它算等待会睡过头。
                    let next_start = root
                        .lyrics
                        .iter()
                        .map(|line| line.start_seconds)
                        .filter(|start| *start > snapshot.position_seconds)
                        .fold(f64::INFINITY, f64::min);
                    let next_start = next_start.is_finite().then_some(next_start);
                    // 进入"下一句马上开始"的窗口就必须 notify：
                    // `lyrics_position_seconds` 与 `lyrics_active` 只在 `Root::render`
                    // 里更新。若醒来自顾自继续睡，就会一直拿旧位置判定"还没到"，
                    // 直到 200ms 主心跳才切句 —— 正好抵消 `lyrics_idle_wait`
                    // 想到达的"刚好赶上"。
                    let near = lyrics_near_next_line(snapshot.position_seconds, next_start);
                    (
                        lyrics_idle_wait(snapshot.position_seconds, next_start),
                        near,
                    )
                })
                .unwrap_or((std::time::Duration::from_millis(120), false));
            let _ = this.update(cx, |_root, cx| {
                // 滚动过渡不在这里推进：它跑在 `Root::render` 的帧回调里，
                // 那里与平台刷新同步。这里只负责常规的逐字高亮重绘。
                if notify {
                    cx.notify();
                }
            });
            cx.background_executor().timer(interval).await;
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
        self.settings.set_quality_intent(&intent);
        if let Err(err) = self.settings.save() {
            let message = self.failure_text("保存音质设置失败", &err, SAVE_TEXT);
            self.notify_error_once("quality-save-failed", message);
        }
        if let Some(session) = self.session.as_mut() {
            let _ = session.apply(self.settings.clone());
        }
        // 切档后按新意图重新预取下一首（正在播的这首不受影响）
        self.spawn_prefetch(cx);
        let label = crate::ui::player_bar::quality_label(intent.as_setting(), self.language);
        self.set_status_text(self.localized("音质已设为 {}，从下一首生效", &[label]));
        self.quality_menu = None;
    }

    /// 设置页「详细日志」开关：写进配置并即时生效，重启后保持。
    pub fn set_detailed_logs(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.detailed_logs = enabled;
        log_outlet::set_global_detailed(enabled);
        // 会话持有一份设置副本，登录时会被整体落盘；不同步就会把这里的选择写回旧值。
        self.persist_playback_preferences();
        match self.settings.save() {
            Ok(()) => {
                let message = if enabled {
                    self.tr("已开启详细日志：六个话题都会记到 DEBUG，重启后保持")
                } else {
                    self.tr("已关闭详细日志：回到 INFO，环境变量仍然有效")
                };
                self.set_status_text(message);
            }
            Err(err) => {
                let message = self.failure_text("详细日志已生效，但保存失败", &err, SAVE_TEXT);
                self.notify_error_once("detailed-logs-save-failed", message);
            }
        }
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
        self.persist_playback_preferences();
    }

    /// 静音 / 恢复。以设置中的即时值判断，快速连点不依赖音频线程的回执时机。
    pub fn toggle_mute(&mut self) {
        self.settings.toggle_mute();
        self.engine.set_volume(self.settings.volume);
        self.persist_playback_preferences();
    }

    pub(crate) fn set_cache_limit_mb(&mut self, mb: u64, cx: &mut Context<Self>) {
        self.settings.audio_cache_limit_mb = mb.clamp(1024, 1024 * 1024);
        self.persist_playback_preferences();
        self.maintain_cache(cx);
        cx.notify();
    }

    fn persist_playback_preferences(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.update_playback_preferences(&self.settings);
        }
        self.preferences_writer.schedule(&self.settings);
    }

    pub(crate) fn maintain_cache(&mut self, cx: &mut Context<Self>) {
        if self.cache_maintenance_running || self.cache_clearing {
            return;
        }
        self.cache_maintenance_running = true;
        let limit = self.settings.audio_cache_limit_mb;
        let work = cx.background_spawn(async move {
            sodam_core::cache::trim_audio(limit);
            sodam_core::audio::cache_stats()
        });
        cx.spawn(async move |this, cx| {
            let stats = work.await;
            let _ = this.update(cx, |root, cx| {
                root.cache_maintenance_running = false;
                root.cache_summary = stats;
                if root.settings.audio_cache_limit_mb != limit {
                    root.maintain_cache(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn clear_cache(&mut self, cx: &mut Context<Self>) {
        if self.cache_clearing {
            return;
        }
        self.cache_clearing = true;
        let work = cx.background_spawn(async { sodam_core::audio::clear_cache() });
        cx.spawn(async move |this, cx| {
            let removed = work.await;
            let _ = this.update(cx, |root, cx| {
                root.cache_clearing = false;
                // 文件已清理，路径映射不能继续把不存在的图片当成命中。
                root.covers = Arc::new(Default::default());
                root.original_covers.clear();
                if let Ok(mut tried) = root.cover_attempted.lock() {
                    tried.clear();
                }
                if let Ok(mut tried) = root.original_cover_attempted.lock() {
                    tried.clear();
                }
                root.cover_pool.seen.clear();
                let message = root.localized("已清理缓存：{} 个文件", &[removed.to_string()]);
                root.notify_success_once("cache-clear", message);
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
            self.start_track(cx);
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
            let _ = this.update(cx, |root, cx| {
                root.login = LoginState::Waiting {
                    matrix,
                    token: token.clone(),
                    status: language.text("等待扫码…").to_string(),
                };
                root.set_status_text(language.text("请用汽水音乐 App 扫码并确认").to_string());
                cx.notify();
            });

            // 轮询：libresoda 内部有 2.5s 最小间隔 + 5s 限流冷却，这里 3s 一次足够温和
            // 二次验证窗口只自动拉起一次（误关可重试登录流程）；完成后轮询自动领取结果
            let mut verify_window_opened = false;
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
                            if let LoginState::Waiting { status, .. } = &mut root.login {
                                *status = status_text;
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
                if finished {
                    let _ = this.update(cx, |root, cx| {
                        match root.session.as_mut() {
                            Some(session) => {
                                // 会话里的设置是启动时的副本：先把用户此刻的偏好同步进去，
                                // 否则 `apply_login_cookie` 整体落盘会把登录期间改过的字段写回旧值。
                                session.update_playback_preferences(&root.settings);
                                match session.apply_login_cookie(&cookie) {
                                    Ok(()) => {
                                        root.settings = session.settings().clone();
                                        root.set_status_text(
                                            language
                                                .text("登录成功，正在读取账号信息…")
                                                .to_string(),
                                        );
                                    }
                                    Err(err) => {
                                        let message = root.failure_text(
                                            "登录成功但保存失败",
                                            &err,
                                            SAVE_TEXT,
                                        );
                                        root.notify_error_once("login-save-failed", message);
                                    }
                                }
                            }
                            None => {
                                let message = language.text("登录成功（会话未初始化）").to_string();
                                root.notify_warning_once("login-session-missing", message);
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
                if result.need_second_verify && !verify_window_opened {
                    verify_window_opened = true;
                    // 自动拉起验证窗口（libresoda 会经签名页桥接官方验证组件）；
                    // 用户在窗口里完成验证后，下一次轮询会拿到登录成功
                    let open_settings = settings.clone();
                    let open_token = token.clone();
                    let open = cx.background_spawn(async move {
                        Session::new(open_settings).open_second_verify(&open_token)
                    });
                    let message = match open.await {
                        Ok(_) => language
                            .text("已打开二次验证窗口，请在其中完成验证")
                            .to_string(),
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
                            language
                                .text("打开验证窗口失败，请改用官方客户端导出 Cookie。")
                                .to_string()
                        }
                    };
                    let _ = this.update(cx, |root, cx| {
                        root.set_status_text(message);
                        cx.notify();
                    });
                }
                let _ = this.update(cx, |root, cx| {
                    if let LoginState::Waiting { status: slot, .. } = &mut root.login {
                        *slot = status;
                    }
                    cx.notify();
                });
                if expired {
                    // 过期就自动换一张新码（会话只有 3 分钟，用户常常来不及扫/确认）
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
        let settings = self.settings.clone();
        let work = cx.background_spawn(async move { Session::new(settings).fetch_account() });
        cx.spawn(async move |this, cx| {
            let account = work.await;
            let _ = this.update(cx, |root, cx| {
                match account {
                    Ok(info) => {
                        let vip = info.vip;
                        if !info.avatar_url.trim().is_empty() {
                            root.ensure_covers(std::slice::from_ref(&info.avatar_url.clone()), cx);
                        }
                        root.account = Some(info.clone());
                        root.play_reporter.set_user_id(info.user_id.clone());
                        root.login = LoginState::LoggedIn(info.clone());
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

    /// 设置界面主题并写入配置；一旦手动选择，不再跟随系统。
    pub fn set_ui_theme(&mut self, theme: ThemeKind, cx: &mut Context<Self>) {
        self.theme = theme;
        self.theme_follows_system = false;
        self.settings.theme = theme.key().to_string();
        let save_result = self.settings.save();
        theme::set_theme(theme);
        match save_result {
            Ok(()) => {
                let status = format!(
                    "主题已切换为{}",
                    if theme == ThemeKind::Light {
                        "浅色"
                    } else {
                        "深色"
                    }
                );
                self.set_status_text(status);
            }
            Err(err) => {
                self.notify_error_once(
                    "theme-save-failed",
                    self.failure_text("主题已切换，但保存失败", &err, SAVE_TEXT),
                );
            }
        }
        cx.notify();
    }

    /// 主题恢复为跟随系统：清空配置里的 theme（空串 = auto），渲染时按窗口外观取值。
    pub fn set_ui_theme_system(&mut self, cx: &mut Context<Self>) {
        self.theme_follows_system = true;
        self.theme = theme::theme_from_appearance(cx.window_appearance());
        self.settings.theme = String::new();
        let save_result = self.settings.save();
        theme::set_theme(self.theme);
        match save_result {
            Ok(()) => self.set_status_text(self.tr("主题已恢复为跟随系统")),
            Err(err) => {
                self.notify_error_once(
                    "theme-save-failed",
                    self.failure_text("已切换为跟随系统，但保存失败", &err, SAVE_TEXT),
                );
            }
        }
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

    pub(crate) fn ensure_original_cover(&mut self, url: &str, cx: &mut Context<Self>) {
        let url = url.trim();
        if url.is_empty() || self.original_covers.contains_key(url) {
            return;
        }
        let first_try = match self.original_cover_attempted.lock() {
            Ok(mut attempted) => attempted.insert(url.to_string()),
            Err(_) => false,
        };
        if !first_try {
            return;
        }
        let settings = self.settings.clone();
        let fetch_url = url.to_string();
        let work_url = fetch_url.clone();
        let work = cx
            .background_spawn(async move { Session::new(settings).original_cover_path(&work_url) });
        cx.spawn(async move |this, cx| {
            let path = work.await;
            let _ = this.update(cx, |root, cx| {
                if let Some(path) = path {
                    root.original_covers.insert(fetch_url.clone(), path.clone());
                    // 原图下载成功但缩略图失败时，列表 / 队列也能立刻拿到封面。
                    if !root.covers.contains_key(&fetch_url) {
                        Arc::make_mut(&mut root.covers).insert(fetch_url.clone(), path);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
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

    pub(crate) fn activate_recommended_queue(
        &mut self,
        source: RecommendationSource,
        tracks: Vec<TrackItem>,
        label: &str,
        autoplay: bool,
        cx: &mut Context<Self>,
    ) {
        self.queue.replace(tracks, 0);
        self.played_history.clear();
        self.playback_error = None;
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
        self.set_status("已切换到{}队列", &[label.to_string()]);
        self.ensure_covers(&covers, cx);
        self.open_lyrics(cx);
        if autoplay {
            self.start_track(cx);
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
            self.scenes_list.reset(count);
        } else if count > self.scenes_list_items {
            let old_count = self.scenes_list_items;
            self.scenes_list
                .splice(old_count..old_count, count - old_count);
        } else if count < self.scenes_list_items {
            self.scenes_list.reset(count);
        }
        self.scenes_list_items = count;
    }

    pub fn load_scenes(&mut self, cx: &mut Context<Self>) {
        if self.loading_scenes {
            return;
        }
        let existing_ids: Vec<u64> = self
            .scene_cards
            .iter()
            .filter_map(|card| card.inner_block_id.parse().ok())
            .collect();
        self.loading_scenes = true;
        self.loading_scene_cards = !existing_ids.is_empty();
        if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
            let existing = existing_ids.len().to_string();
            let loading = self.loading_scene_cards.to_string();
            log_event(
                LogLevel::Debug,
                LogTopic::App,
                "scenes_load_start",
                &[
                    ("existing", existing.as_str()),
                    ("loading", loading.as_str()),
                ],
            );
        }
        let message = if existing_ids.is_empty() {
            self.tr("正在读取听歌模式…").to_string()
        } else {
            self.tr("正在加载更多探索歌单…").to_string()
        };
        self.set_status_text(message);
        cx.notify();
        let settings = self.settings.clone();
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            let mut mode_error = None;
            let mut card_error = None;
            let mut modes = Vec::new();
            let mut cards = Vec::new();

            if existing_ids.is_empty() {
                match sodam_core::library::scene_modes(&session) {
                    Ok(value) => modes = value,
                    Err(err) => mode_error = Some(err.to_string()),
                }
            }

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
            match result {
                Ok(value) => cards = value,
                Err(err) => card_error = Some(err.to_string()),
            }
            cards.retain(|card| {
                card.inner_block_id
                    .parse::<u64>()
                    .map(|id| !existing_ids.contains(&id))
                    .unwrap_or(true)
            });
            Ok::<_, anyhow::Error>((modes, cards, mode_error, card_error))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_scenes = false;
                root.loading_scene_cards = false;
                match result {
                    Ok((modes, cards, mode_error, card_error)) => {
                        if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
                            let cards_count = cards.len().to_string();
                            let modes_count = modes.len().to_string();
                            log_event(
                                LogLevel::Debug,
                                LogTopic::App,
                                "scenes_load_applied",
                                &[
                                    ("cards", cards_count.as_str()),
                                    ("modes", modes_count.as_str()),
                                ],
                            );
                        }
                        if mode_error.is_some() || card_error.is_some() {
                            // 部分失败默认可见：只开到 DEBUG 才记等于报障时又是空白。
                            let mode_error_text = mode_error.as_deref().unwrap_or_default();
                            let card_error_text = card_error.as_deref().unwrap_or_default();
                            log_event(
                                LogLevel::Warn,
                                LogTopic::App,
                                "scenes_load_partial",
                                &[
                                    ("mode_error", mode_error_text),
                                    ("card_error", card_error_text),
                                ],
                            );
                        }
                        let mut covers: Vec<String> = cards
                            .iter()
                            .map(|card| card.playlist.cover.clone())
                            .collect();
                        covers.extend(
                            modes
                                .iter()
                                .map(|scene| scene.cover.clone())
                                .filter(|cover| !cover.is_empty()),
                        );
                        let added = cards.len();
                        if !modes.is_empty() {
                            root.scenes = Arc::new(modes);
                        }
                        let mut all_cards = root.scene_cards.as_ref().clone();
                        all_cards.extend(cards);
                        root.scene_cards = Arc::new(all_cards);
                        root.sync_scenes_list();
                        root.ensure_covers(&covers, cx);
                        if let Some(err) = mode_error.or(card_error) {
                            let message = root.failure_text("读取听歌模式失败", &err, REMOTE_TEXT);
                            root.notify_warning_once("scenes-load-failed", message);
                        } else if added == 0 {
                            root.set_status_text(root.tr("没有更多探索歌单"));
                        } else {
                            let message =
                                root.localized("探索歌单已追加 {} 张", &[added.to_string()]);
                            root.set_status_text(message);
                        }
                    }
                    Err(err) => {
                        let message = root.failure_text("读取听歌模式失败", &err, REMOTE_TEXT);
                        root.notify_error_once("scenes-load-failed", message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
        self.set_status("已准备播放：{}", std::slice::from_ref(&track.title));
        self.ensure_covers(std::slice::from_ref(&track.cover), cx);
        cx.notify();
        let settings = self.settings.clone();
        let work_track = track.clone();
        let work = cx
            .background_spawn(async move { Session::new(settings).download_to_cache(&work_track) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if root.play_seq != request_seq {
                    return;
                }
                root.pending_track = None;
                root.playing = false;
                root.progress_preview = None;
                if let Err(err) = result {
                    let context = load_failure_context(err.stage, root.language);
                    root.log_failure(&context, &err);
                    root.playback_error = Some(root.tr(play_failure_text(&err)).to_string());
                    root.playing = false;
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

    /// 计算「让第 `index` 句停在视口顶部 + 80px」所需的滚动偏移。
    fn lyrics_scroll_target(&self, index: usize) -> Option<f32> {
        const LYRIC_TOP_INSET: f32 = 80.0;
        let bounds = self.lyrics_scroll.bounds_for_item(index)?;
        let viewport_top = self.lyrics_scroll.bounds().top();
        // offset 为负表示内容上移，所以取「该句顶部 - 视口顶 - 80」的相反数。
        let distance = bounds.top() - viewport_top - gpui::px(LYRIC_TOP_INSET);
        Some(-f32::from(distance))
    }

    /// 把歌词滚到指定句，**走过渡动画**。
    ///
    /// 定位沿用之前的行为：当前句顶部落在视口顶部往下 80px 处（`offsetTop - 80`），
    /// 不做垂直居中。列表顶部留了半屏空白，所以首句也能停在这个位置。
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
        self.original_cover_attempted
            .lock()
            .ok()
            .map(|mut attempted| attempted.remove(&track.cover));
        self.cover_attempted
            .lock()
            .ok()
            .map(|mut attempted| attempted.remove(&track.cover));
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
                                root.start_track(cx);
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
                        root.start_track(cx);
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

    /// 退出登录：清除 Cookie 并立即回到未登录状态。
    ///
    /// 播放和队列一并停止，避免旧账号的歌单/收藏继续留在界面上；
    /// 封面本地缓存只按 URL 索引，可以复用，不涉及凭据。
    pub fn logout(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.settings.clone();
        settings.cookie.clear();

        self.engine.stop();
        self.playing = false;
        self.pending_track = None;
        self.playback_error = None;
        self.queue = Queue::new(Vec::new());
        self.played_history.clear();
        self.sync_queue_cache();

        self.account = None;
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
        self.set_nav(Nav::Settings, cx);

        let save_result = if let Some(session) = self.session.as_mut() {
            session.apply(settings.clone())
        } else {
            settings.save()
        };
        self.settings = settings;
        match save_result {
            Ok(()) => self.set_status_text(self.tr("已退出登录")),
            Err(err) => {
                self.notify_error_once(
                    "logout-save-failed",
                    self.failure_text("已退出，但保存配置失败", &err, SAVE_TEXT),
                );
            }
        }
        cx.notify();
    }

    /// 详情列表内的加载更多分发。
    pub fn load_more_detail(&mut self, cx: &mut Context<Self>) {
        if self.nav == Nav::Artist {
            self.load_more_artist_tracks(cx);
        }
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
        if matches!(nav, Nav::Settings) {
            self.refresh_cache_stats(cx);
        }
        self.on_nav_changed(cx);
        cx.notify();
    }

    /// 后台刷新缓存统计（同步扫盘不能进渲染路径：设置页每次重绘都扫
    /// 目录会把 UI 卡出顿挫，预取/心跳的高频 notify 会放大这个问题）。
    pub(crate) fn refresh_cache_stats(&mut self, cx: &mut Context<Self>) {
        let work = cx.background_spawn(async { sodam_core::audio::cache_stats() });
        cx.spawn(async move |this, cx| {
            let stats = work.await;
            let _ = this.update(cx, |root, cx| {
                root.cache_summary = stats;
                cx.notify();
            });
        })
        .detach();
    }

    /// 通用返回：同页详情优先关闭，再按真实访问历史回退。
    pub fn go_back(&mut self, cx: &mut Context<Self>) {
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
                Nav::Scenes if self.scene_cards.is_empty() && !self.loading_scenes => {
                    self.load_scenes(cx)
                }
                Nav::Library => self.load_playlists(cx),
                Nav::Liked => {
                    // 收藏页使用独立列表状态；已加载数据也要重建到页首。
                    self.liked_detail_list.reset(self.liked.len() + 1);
                    self.load_liked(cx);
                }
                _ => {}
            }
        }
    }

    /// 封面本地路径（渲染路径唯一入口：只查缓存，不发请求）。
    pub fn cover_of(&self, url: &str) -> Option<PathBuf> {
        self.covers.get(url).cloned()
    }

    /// 把封面放进请求池（渲染只读缓存，缺图先用占位块）。
    ///
    /// 用 `cover_attempted` 去重：下载失败的 URL 也记下来，
    /// 否则「缺图 → 每帧重新排队」会变成 CPU/网络空转（实测踩过）。
    pub fn ensure_covers(&mut self, urls: &[String], cx: &mut Context<Self>) {
        let mut added = false;
        for url in urls {
            let url = url.trim();
            if url.is_empty() {
                continue;
            }
            let first_try = match self.cover_attempted.lock() {
                Ok(mut attempted) => attempted.insert(url.to_string()),
                Err(_) => false,
            };
            if first_try {
                added |= self.cover_pool.push(url);
            }
        }
        if added {
            self.pump_covers(cx);
        }
    }

    /// 驱动请求池：保持最多 `MAX_CONCURRENCY` 个下载在飞，完成一个补一个。
    pub(crate) fn pump_covers(&mut self, cx: &mut Context<Self>) {
        while let Some(url) = self.cover_pool.next() {
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
                    root.cover_pool.done();
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
                    let has_result = path.is_some() || color.is_some();
                    if let Some(path) = path {
                        Arc::make_mut(&mut root.covers).insert(url.clone(), path);
                    }
                    if let Some(color) = color {
                        root.ambient_colors.insert(url.clone(), color);
                    }
                    if has_result {
                        cx.notify();
                    }
                    // 一个下载完成就补下一个，池子始终保持有界并发
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
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            sodam_core::library::user_playlists(&session)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_library = false;
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
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            sodam_core::library::liked_songs(&session)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.loading_liked = false;
                match result {
                    Ok(tracks) => {
                        root.liked_loaded = true;
                        // 全量列表是收藏的权威快照：同步修复全局 ids，
                        // 避免启动时 ids 拉取失败后爱心状态一直错。
                        root.liked_ids =
                            Arc::new(tracks.iter().map(|track| track.id.clone()).collect());
                        root.liked_ids_failed = false;
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

    /// 点列表行播放（列表以 `Arc` 共享，避免每次点击都深拷贝整张表）。
    pub fn play_from_arc(
        &mut self,
        tracks: std::sync::Arc<Vec<TrackItem>>,
        index: usize,
        cx: &mut Context<Self>,
    ) {
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
        self.persist_playback_preferences();
        let start = Queue::random_start(tracks.len());
        self.play_from(tracks.as_ref().clone(), start, cx);
        // 入队后立刻生成一轮，让抽屉马上显示真实的「接下来」。
        self.queue.prepare_shuffle_round();
        self.sync_queue_cache();
        cx.notify();
    }

    /// 点歌单/收藏列表里的某一行播放（把该列表整体入队）。
    /// 点歌单/收藏列表里的某一行播放（把该列表整体入队）。
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
        self.start_track(cx);
    }

    /// 打开搜索结果里的歌单：带着已知标题，切换到歌单详情页。
    pub fn open_search_playlist(&mut self, playlist: PlaylistItem, cx: &mut Context<Self>) {
        if self.loading_search_playlist {
            return;
        }
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
            self.start_track(cx);
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
        self.playback_halted = true;
        self.playing = false;
        self.engine.pause();
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

    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.pending_track.is_some() {
            return;
        }
        self.release_playback_halt();
        let snapshot = self.engine.snapshot();
        if snapshot.track_id.is_empty()
            || !snapshot.error.is_empty()
            || snapshot.finished
            || self
                .queue
                .current()
                .is_some_and(|track| track.id != snapshot.track_id)
        {
            self.consecutive_failures = 0;
            if self.queue.is_empty() {
                self.set_status_text(self.tr("队列是空的：先去搜索或打开一张歌单"));
                cx.notify();
                return;
            }
            // 队列里有歌但引擎还没装载 → 装载当前曲目
            self.start_track(cx);
            return;
        }
        self.engine.toggle();
        self.playing = !snapshot.playing;
        let message = if self.playing {
            self.tr("播放中").to_string()
        } else {
            self.tr("已暂停").to_string()
        };
        self.set_status_text(message);
        cx.notify();
    }

    /// 装载并播放「当前队列曲目」：后台下载+解密 → 回 UI 线程交给引擎。
    pub fn start_track(&mut self, cx: &mut Context<Self>) {
        let Some(track) = self.queue.current().cloned() else {
            self.set_status_text(self.tr("队列是空的：先去搜索或打开一张歌单"));
            cx.notify();
            return;
        };
        // 走到这里就是一次被明确要求（或被允许）的播放尝试：闸门若开着，说明是用户
        // 按了播放/切歌，熔断回半开。自动续播路径在巡检处已被闸门拦住。
        self.release_playback_halt();
        self.playing = true;
        self.play_seq = self.play_seq.wrapping_add(1);
        let request_seq = self.play_seq;
        self.pending_track = Some(track.clone());
        self.pending_engine_load = None;
        self.progress_preview = None;
        self.set_status("正在准备播放：{}…", std::slice::from_ref(&track.title));
        self.ensure_covers(std::slice::from_ref(&track.cover), cx);
        if self.nav == Nav::Lyrics {
            self.load_lyrics(track.clone(), false, cx);
        }
        cx.notify();

        let settings = self.settings.clone();
        let work_track = track.clone();
        // 单层预算：还要不要再来一次由**这次失败的分类**决定。
        // 之前是无条件 3 次线性退避，于是「这首已下架」「签名服务要求鉴权」这类
        // 永远不会因为重试变好的失败也要打满三次——既拖慢恢复，又白踩共享服务。
        // 单次尝试内部的「卡住」由 libresoda 的空闲读超时（20s）负责；这里刻意不设
        // 总墙钟上限，否则等于把刚拆掉的「整首下载总时限」从另一侧装回来。
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            // 轨迹写在后台线程：开始 / 每次失败判定 / 最终成功或放弃各一行。
            // 永久失败只留一次尝试的记录，不会因为预算再打一次远程。
            let trace = PlaybackTrace::global(&work_track.id);
            drive_play_attempt(
                PlayBudget::default(),
                trace.as_ref(),
                std::time::Instant::now(),
                |_| session.download_to_cache(&work_track),
                std::thread::sleep,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
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
                        let sequence = root.engine.load(track.clone(), cached);
                        root.pending_engine_load = Some(sequence);
                        root.progress_token = Some(sequence);
                        root.playback_error = None;
                        // 记录真实播放历史（去重最近一条，避免重复刷屏）
                        if root.played_history.last().map(String::as_str) != Some(track.id.as_str())
                        {
                            root.played_history.push(track.id.clone());
                            let overflow = root.played_history.len().saturating_sub(100);
                            if overflow > 0 {
                                root.played_history.drain(0..overflow);
                            }
                        }
                        // 降档：实际低于所选时如实说出两边，不猜原因——取流那一段在
                        // 「账号没有权益」与「这首没有该档位」下返回值相同，没有字段能分开。
                        // 实际档位标签由核心层按中文档位名生成，英文界面要过一遍翻译。
                        // 走全局提示而不是 `status`：后者没有渲染（见 #40）。
                        let intent = root.settings.quality_intent();
                        if sodam_core::quality::is_downgraded(&intent, actual_rank) {
                            let message = root.localized(
                                "已降档：所选 {}，实际 {}",
                                &[
                                    crate::ui::player_bar::quality_label(
                                        intent.as_setting(),
                                        root.language,
                                    ),
                                    crate::ui::player_bar::localized_quality(
                                        actual_quality,
                                        root.language,
                                    ),
                                ],
                            );
                            root.notify_warning_once("quality-downgraded", message);
                        } else {
                            root.set_status("播放中：{}", std::slice::from_ref(&track.title));
                        }
                        // 开播后立刻预取下一首，切歌时基本是秒开
                        root.spawn_prefetch(cx);
                    }
                    Err(failure) => {
                        // 技术原文只进日志漏斗（阶段 + 已切断查询串的原始错误）；
                        // 界面上只留下面那一句人话。
                        let context = load_failure_context(failure.stage, root.language);
                        root.log_failure(&context, &failure);
                        root.pending_track = None;
                        // 这次尝试失败了：它之前的「播出去」不再为它抵账。
                        root.progress_token = None;
                        let message = root.tr(play_failure_text(&failure)).to_string();
                        root.playback_error = Some(message.clone());
                        root.notify_warning_once("track-stream-failed", message);
                        if should_skip_to_next(&failure) {
                            root.consecutive_failures += 1;
                            if root.consecutive_failures >= PLAY_FAILURE_HALT_AFTER {
                                root.halt_playback();
                                root.notify_error_once(
                                    "consecutive-stream-failures",
                                    root.tr("连续 3 首没播出来，已暂停").to_string(),
                                );
                            } else if !root.queue.is_empty() {
                                root.next_track(cx);
                            } else {
                                root.halt_playback();
                            }
                        } else {
                            // 全局性故障（登录 / 签名服务 / 权益 / 磁盘）：换一首也一样失败。
                            // 停下并把修复动作留在状态里，别用连续跳过把全局故障
                            // 伪装成「这几首运气不好」。
                            root.halt_playback();
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
        self.persist_playback_preferences();
        cx.notify();
    }

    pub fn next_track(&mut self, cx: &mut Context<Self>) {
        // 切歌时丢掉进度预览，避免「拖到一半换歌」把 seek 用到新歌上。
        // 队列当前项不会被移除（remove 拒绝当前项），所以直接推进即可。
        self.progress_preview = None;
        self.queue.advance();
        self.sync_queue_cache();
        self.start_track(cx);
    }

    pub fn prev_track(&mut self, cx: &mut Context<Self>) {
        self.progress_preview = None;
        self.queue.rewind();
        self.sync_queue_cache();
        self.start_track(cx);
    }

    /// 设置页保存后调用：落盘并重建会话。
    #[allow(dead_code)] // 设置表单（roadmap 第 3 项）接入后由保存按钮调用
    pub fn apply_settings(&mut self, settings: Settings) {
        let reporter_settings = settings.clone();
        match self
            .session
            .as_mut()
            .map(|session| session.apply(settings.clone()))
        {
            Some(Ok(())) => {
                self.settings = settings;
                self.set_status_text(self.tr("设置已保存"));
            }
            Some(Err(err)) => {
                let message = self.failure_text("保存失败", &err, SAVE_TEXT);
                self.notify_error_once("settings-save-failed", message);
            }
            None => {
                self.settings = settings;
                self.session = Some(Session::new(self.settings.clone()));
            }
        }
        self.play_reporter.apply_settings(reporter_settings);
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

/// 预取失败冷却：失败的曲目 60s 内不再重试（巡检会把永久失败的歌无限重试，
/// 持续打签名服务和网络）。
const PREFETCH_RETRY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);
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
fn play_failure_text(failure: &PlaybackFailure) -> &'static str {
    match failure.action {
        RecoveryAction::Relogin => "登录已过期，请重新登录。",
        RecoveryAction::CheckSigner => "这首歌暂时放不出来。签名服务不可用，可在设置里更换。",
        RecoveryAction::CheckEntitlement | RecoveryAction::PickAnother => {
            "这首歌在你的账号下无法播放。"
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

/// 这类失败要不要换一首再试。
///
/// * 下架 / 取不到（`PickAnother`）与纯网络抖动（`Nothing`）→ 换，队列里还有别的歌；
/// * 登录、签名服务、权益、磁盘 → 换一首也一样失败，停下来把动作告诉用户，
///   别用「跳过」把同一个全局故障伪装成单首曲目的问题。
fn should_skip_to_next(failure: &PlaybackFailure) -> bool {
    matches!(
        failure.action,
        RecoveryAction::PickAnother | RecoveryAction::Nothing
    )
}

/// 连败封顶阈值：连续这么多首装载失败就停下，别再替用户往后翻。
const PLAY_FAILURE_HALT_AFTER: u32 = 3;
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
/// 不引新依赖：`futures-lite` 现在只挂在 Linux target 下，而超时兜底是三个平台
/// 都要的行为。返回 `Some(value)` 表示 `work` 先完成，`None` 表示截止时间先到。
///
/// 拿 `&mut F` 而不是 `F`：`gpui::Task` 被 drop 时会 `set_canceled()` 真正取消任务
/// （`async_task::Task` 的 Drop 行为）。超时分支的调用方需要任务活下来去把那一次
/// 请求跑完，所以所有权必须留在外面，由调用方决定要不要 `detach()`。
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

/// 预取并发上限：环境变量 `SODAM_PREFETCH_CONCURRENCY`（默认 2）。
fn prefetch_concurrency() -> usize {
    std::env::var("SODAM_PREFETCH_CONCURRENCY")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or(2)
}

#[cfg(test)]
mod failure_text_tests {
    use super::*;

    /// 界面只说人话：不复述上游技术原文，也不带接口地址。
    #[test]
    fn play_failure_text_is_human_only() {
        let raw = "soda track_v2 failed: https://x.test/a?sign=SECRET: tls connection init failed";
        let signer = "这首歌暂时放不出来。签名服务不可用，可在设置里更换。";
        let no_rights = "这首歌在你的账号下无法播放。";
        let cache = "本地缓存写入失败，检查磁盘空间。";
        let offline = "暂时连不上，检查网络后重试。";
        let relogin = "登录已过期，请重新登录。";
        let cant_play = "这首歌暂时放不出来。";
        let at = |stage, action| {
            play_failure_text(&PlaybackFailure::new(
                stage,
                Retryability::Never,
                action,
                raw,
            ))
        };

        assert_eq!(at(Stage::Network, RecoveryAction::Nothing), offline);
        assert_eq!(at(Stage::Transfer, RecoveryAction::Nothing), offline);
        assert_eq!(at(Stage::Unpack, RecoveryAction::Nothing), cache);
        assert_eq!(at(Stage::Input, RecoveryAction::Nothing), cant_play);
        assert_eq!(at(Stage::ResolveStream, RecoveryAction::Relogin), relogin);
        assert_eq!(
            at(Stage::ResolveStream, RecoveryAction::CheckSigner),
            signer
        );
        assert_eq!(
            at(Stage::ResolveStream, RecoveryAction::CheckEntitlement),
            no_rights
        );
        assert_eq!(
            at(Stage::ResolveStream, RecoveryAction::PickAnother),
            no_rights
        );
        assert_eq!(at(Stage::ResolveStream, RecoveryAction::FreeSpace), cache);

        for stage in [Stage::Network, Stage::Transfer, Stage::ResolveStream] {
            let text = at(stage, RecoveryAction::Nothing);
            assert!(!text.contains("tls"), "{text}");
            assert!(!text.contains("://"), "{text}");
            assert!(!text.contains("SECRET"), "{text}");
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
        let hits = shown.contains(&PLAY_FAILURE_HALT_AFTER.to_string());
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
        line_undergoing_reveal, lyrics_idle_wait, lyrics_near_next_line, lyrics_scroll_ease,
    };
    use sodam_core::models::{LyricLine, LyricWord};

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
