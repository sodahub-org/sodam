//! 用户动作层：搜索、推荐、媒体库、播放和封面加载。
//!
//! [`Root`] 的字段定义在 [`super`]，这里按领域拆分实现，避免单文件继续膨胀。

use crate::ui::theme::{self, ThemeKind};
use gpui::prelude::*;
use gpui::{Context, Window};
use sodam_core::library::RecommendationSource;
use sodam_core::models::{PlaylistItem, SceneItem, TrackItem};
use sodam_core::session::Session;
use sodam_core::Settings;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use super::{status_label, LoginState, Nav, QueueOrigin, Root, ROW_BATCH};
use sodam_core::queue::Queue;

impl Root {
    pub(crate) fn search_input_focused(&self, window: &Window) -> bool {
        self.search_focus.is_focused(window)
    }

    pub(crate) fn search_utf16_to_byte_offset(&self, offset: usize) -> usize {
        let mut units = 0;
        for (byte_offset, character) in self.search_input.char_indices() {
            if units >= offset {
                return byte_offset;
            }
            units += character.len_utf16();
        }
        self.search_input.len()
    }

    pub(crate) fn search_byte_to_utf16_offset(&self, offset: usize) -> usize {
        self.search_input
            .char_indices()
            .take_while(|(byte_offset, _)| *byte_offset < offset)
            .map(|(_, character)| character.len_utf16())
            .sum()
    }

    pub(crate) fn search_byte_range_from_utf16(
        &self,
        range: std::ops::Range<usize>,
    ) -> std::ops::Range<usize> {
        let start = self.search_utf16_to_byte_offset(range.start);
        let end = self.search_utf16_to_byte_offset(range.end);
        start.min(end)..start.max(end)
    }

    pub(crate) fn search_utf16_range_from_byte(
        &self,
        range: std::ops::Range<usize>,
    ) -> std::ops::Range<usize> {
        self.search_byte_to_utf16_offset(range.start)..self.search_byte_to_utf16_offset(range.end)
    }

    pub(crate) fn search_replacement_range(
        &self,
        range_utf16: Option<std::ops::Range<usize>>,
    ) -> std::ops::Range<usize> {
        range_utf16
            .map(|range| self.search_byte_range_from_utf16(range))
            .or_else(|| self.search_marked_range.clone())
            .unwrap_or(self.search_input.len()..self.search_input.len())
    }

    /// 预取队列下一首到本地缓存（只落盘，不动界面状态）。
    ///
    /// 保持「拉完再播」的简单架构不变，但把等待时间提前吃掉：
    /// 当前曲目一开播，就在后台把下一首下好；切歌时命中缓存直接播。
    pub(crate) fn spawn_prefetch(&mut self, cx: &mut Context<Self>) {
        let log = std::env::var("SODAM_PREFETCH_LOG").is_ok();
        let Some(next) = self.queue.peek_next().cloned() else {
            if log {
                eprintln!("[prefetch] 队列为空，跳过");
            }
            return;
        };
        // 已经在缓存里、或这一首正在预取/正在播放，就不用再排
        if let Some(session) = &self.session {
            if session.is_cached(&next.id) {
                if log {
                    eprintln!("[prefetch] 跳过（已缓存）：{}", next.title);
                }
                return;
            }
        }
        if self.prefetch_inflight.as_deref() == Some(next.id.as_str())
            || self
                .pending_track
                .as_ref()
                .map(|track| track.id == next.id)
                .unwrap_or(false)
        {
            if log {
                eprintln!("[prefetch] 跳过（已在进行中）：{}", next.title);
            }
            return;
        }
        if log {
            eprintln!("[prefetch] 开始预取：{}", next.title);
        }
        self.prefetch_inflight = Some(next.id.clone());
        let settings = self.settings.clone();
        let track = next.clone();
        let work_track = track.clone();
        let result = cx
            .background_spawn(async move { Session::new(settings).download_to_cache(&work_track) });
        cx.spawn(async move |this, cx| {
            let outcome = result.await;
            if log {
                match &outcome {
                    Ok(cached) => {
                        eprintln!("[prefetch] {} → {}", track.title, cached.quality)
                    }
                    Err(err) => eprintln!("[prefetch] {} 失败: {err}", track.title),
                }
            }
            let _ = this.update(cx, |root, _cx| {
                root.prefetch_inflight = None;
            });
        })
        .detach();
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
                // 加载完成：引擎已切到 pending 曲目 → 清 loading 态
                if let Some(pending) = &root.pending_track {
                    if snap.track_id == pending.id {
                        root.pending_track = None;
                    }
                }
                if snap.playing || root.pending_track.is_some() {
                    cx.notify();
                }
                // 播完自动下一首：按序号判断（循环同一首也不会漏切）
                if snap.finished && snap.finished_seq != root.last_finished_seq {
                    root.last_finished_seq = snap.finished_seq;
                    root.next_track(cx);
                }
                root.load_more_recommendation(cx);
            });
        })
        .detach();
    }

    /// 打开抽屉时把滚动位置锚到「正在播放」这一行。
    pub fn anchor_queue_scroll(&self) {
        let row = crate::ui::player_bar::now_playing_slot(self);
        self.queue_scroll
            .scroll_to_item(row, gpui::ScrollStrategy::Center);
    }

    /// 从队列移除某一首（右键菜单）。移除的是正在播放的那首时，
    /// 只把它从队列拿掉，音频继续播当前这首。
    pub fn remove_from_queue(&mut self, index: usize, cx: &mut Context<Self>) {
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

    /// 设置音质档位（`auto` = 按账号权益自动）。换档后重新拉当前曲目的流。
    pub fn set_quality(&mut self, quality: &str, cx: &mut Context<Self>) {
        let value = if quality == "auto" { "" } else { quality };
        self.settings.quality = value.to_string();
        let _ = self.settings.save();
        if let Some(session) = self.session.as_mut() {
            let _ = session.apply(self.settings.clone());
        }
        // 切档后按新档位重新预取下一首（正在播的这首不受影响）
        self.spawn_prefetch(cx);
    }

    /// 按比例跳转播放位置（0.0~1.0）。
    pub fn seek_fraction(&mut self, fraction: f32) {
        let snapshot = self.engine.snapshot();
        if snapshot.duration_seconds <= 0.0 {
            return;
        }
        let target = snapshot.duration_seconds * fraction.clamp(0.0, 1.0) as f64;
        self.engine.seek(target);
    }

    /// 设置音量（0.0~1.0）。
    pub fn set_volume(&mut self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        self.engine.set_volume(volume);
        if volume > 0.0 {
            self.volume_before_mute = volume;
        }
    }

    /// 静音 / 恢复。
    pub fn toggle_mute(&mut self) {
        let current = self.engine.snapshot().volume;
        if current > 0.001 {
            self.volume_before_mute = current;
            self.engine.set_volume(0.0);
        } else {
            self.engine.set_volume(if self.volume_before_mute <= 0.001 {
                1.0
            } else {
                self.volume_before_mute
            });
        }
    }

    /// 跳到队列中的某一首并播放。
    pub fn jump_in_queue(&mut self, index: usize, cx: &mut Context<Self>) {
        if std::env::var("SODAM_PLAYER_LOG").is_ok() {
            eprintln!("[player] 队列跳转 → #{index}");
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
                        root.login = LoginState::Failed(root.localized("创建二维码失败：{err}", &[err.to_string()]));
                        root.status = language.text("创建二维码失败，可点「重新获取二维码」重试").to_string();
                        cx.notify();
                    });
                    return;
                }
            };
            let matrix = match sodam_core::login::qr_matrix(&created.scan_url) {
                Ok(matrix) => matrix,
                Err(err) => {
                    let _ = this.update(cx, |root, cx| {
                        root.login = LoginState::Failed(root.localized("二维码编码失败：{err}", &[err.to_string()]));
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
                root.status = language.text("请用汽水音乐 App 扫码并确认").to_string();
                cx.notify();
            });

            // 轮询：libresoda 内部有 2.5s 最小间隔 + 5s 限流冷却，这里 3s 一次足够温和
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
                            if let LoginState::Waiting { status, .. } = &mut root.login {
                                *status = language.textf("轮询异常（会自动重试）：{err}", &[err.to_string()]);
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
                            Some(session) => match session.apply_login_cookie(&cookie) {
                                Ok(()) => {
                                    root.settings = session.settings().clone();
                                    root.status = language.text("登录成功，正在读取账号信息…").to_string();
                                }
                                Err(err) => {
                                    root.set_status("登录成功但保存失败：{err}", &[err.to_string()]);
                                }
                            },
                            None => root.status = language.text("登录成功（会话未初始化）").to_string(),
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
                if result.need_second_verify {
                    let _ = this.update(cx, |root, cx| {
                        root.status =
                            language.text("服务端要求二次验证（短信）：当前 libresoda 尚未闭环，请改用官方客户端导出 Cookie").to_string();
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
                        root.status = language.text("二维码已过期，正在自动换新码…").to_string();
                        root.start_login(cx);
                    });
                    return;
                }
            }
        })
        .detach();
    }

    /// 读取账号信息，并在用户没有手动指定音质时按 VIP 情况自动选最高档。
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
                        root.login = LoginState::LoggedIn(info.clone());
                        root.set_status(
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
                        if let Some(session) = root.session.as_mut() {
                            match session.refresh_quality(vip) {
                                Ok(Some(quality)) => {
                                    root.settings = session.settings().clone();
                                    root.status = root.localized(
                                        "{}；音质已按权益自动设为 {}",
                                        &[root.status.clone(), quality.to_string()],
                                    );
                                }
                                Ok(None) => {}
                                Err(err) => {
                                    root.status = root
                                        .localized("音质偏好保存失败：{err}", &[err.to_string()])
                                }
                            }
                        }
                    }
                    Err(err) => {
                        root.status = root.localized("读取账号信息失败：{err}", &[err.to_string()])
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
        self.status = match save_result {
            Ok(()) => format!(
                "主题已切换为{}",
                if theme == ThemeKind::Light {
                    "浅色"
                } else {
                    "深色"
                }
            ),
            Err(err) => self.localized("主题已切换，但保存失败：{err}", &[err.to_string()]),
        };
        cx.notify();
    }

    /// 用系统默认应用打开配置文件（Linux 上是 xdg-open）。
    pub fn open_config_file(&mut self, cx: &mut Context<Self>) {
        let path = Settings::config_path();
        let result = std::process::Command::new("xdg-open").arg(path).spawn();
        self.status = match result {
            Ok(_) => self.tr("已用系统默认应用打开配置文件").to_string(),
            Err(err) => self.localized("打开配置文件失败：{err}", &[err.to_string()]),
        };
        cx.notify();
    }

    /// 用系统默认浏览器打开项目 GitHub 仓库。
    pub fn open_github_repository(&mut self, cx: &mut Context<Self>) {
        let url = env!("CARGO_PKG_REPOSITORY");
        let result = std::process::Command::new("xdg-open").arg(url).spawn();
        self.status = match result {
            Ok(_) => self.tr("已在浏览器打开 GitHub 仓库").to_string(),
            Err(err) => self.localized("打开 GitHub 仓库失败：{err}", &[err.to_string()]),
        };
        cx.notify();
    }

    /// 打开歌词播放页，并按需加载当前（或正在装载）歌曲的歌词。
    pub fn open_lyrics(&mut self, cx: &mut Context<Self>) {
        let track = self
            .pending_track
            .clone()
            .or_else(|| self.queue.current().cloned());
        let Some(track) = track else {
            self.status = self.tr("还没有正在播放的歌曲").to_string();
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
                        root.status = if root.lyrics.is_empty() {
                            root.localized("「{}」没有可用歌词", std::slice::from_ref(&track.title))
                        } else {
                            root.localized("「{}」歌词已加载", std::slice::from_ref(&track.title))
                        };
                    }
                    Err(err) => {
                        root.lyrics = Arc::new(Vec::new());
                        root.lyrics_active = None;
                        root.lyrics_error = Some(err.to_string());
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
        if std::env::var("SODAM_SCENE_LOG").is_ok() {
            eprintln!(
                "[scenes] load_more start existing={} loading={}",
                existing_ids.len(),
                self.loading_scene_cards
            );
        }
        self.status = if existing_ids.is_empty() {
            self.tr("正在读取听歌模式…").to_string()
        } else {
            self.tr("正在加载更多探索歌单…").to_string()
        };
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
                        if std::env::var("SODAM_SCENE_LOG").is_ok() {
                            eprintln!("[scenes] load_more applied cards={} modes={} mode_error={:?} card_error={:?}", cards.len(), modes.len(), mode_error, card_error);
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
                        root.status = if let Some(err) = mode_error.or(card_error) {
                            root.localized("读取听歌模式失败：{err}", &[err])
                        } else if added == 0 {
                            root.tr("没有更多探索歌单").to_string()
                        } else {
                            root.localized("探索歌单已追加 {} 张",&[added.to_string()])
                        };
                    }
                    Err(err) => root.status = root.localized("读取听歌模式失败：{err}", &[err.to_string()]),
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
        self.status = self.localized("正在加载「{}」队列…", &[scene_name]);
        cx.notify();

        let settings = self.settings.clone();
        let work = cx.background_spawn(async move {
            sodam_core::library::recommended_scene_start(&Session::new(settings), scene)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if root.recommendation_request_seq != request_seq {
                    return;
                }
                root.loading_recommendation = false;
                root.switching_queue = None;
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
                        root.set_status("加载听歌模式队列失败：{err}", &[err.to_string()]);
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
                if root.play_seq == request_seq {
                    root.pending_track = None;
                    root.progress_preview = None;
                }
                if let Err(err) = result {
                    root.playback_error = Some(err.to_string());
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
        if matches!(self.queue_origin, QueueOrigin::Feed) {
            self.set_nav(Nav::Lyrics, cx);
            self.lyrics_return_nav = Some(Nav::Home);
            cx.notify();
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
        self.begin_queue_switch(QueueOrigin::Feed, self.tr("推荐队列"), cx);
        self.recommendation_request_seq = self.recommendation_request_seq.wrapping_add(1);
        let request_seq = self.recommendation_request_seq;
        self.status = self.tr("正在加载推荐队列…").to_string();
        cx.notify();

        let settings = self.settings.clone();
        let work = cx.background_spawn(async move {
            sodam_core::library::recommended_start(&Session::new(settings))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if root.recommendation_request_seq != request_seq {
                    return;
                }
                root.loading_recommendation = false;
                root.switching_queue = None;
                match result {
                    Ok(start) => {
                        root.activate_recommended_queue(
                            start.source,
                            start.tracks,
                            root.tr("推荐"),
                            autoplay,
                            cx,
                        );
                    }
                    Err(err) => {
                        root.set_status("加载推荐队列失败：{err}", &[err.to_string()]);
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
                let work = cx.background_spawn(async move {
                    let session = Session::new(settings);
                    sodam_core::library::radio_tracks(&session, &radio)
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
                                root.status = root.localized(
                                    "电台「{}」没有可播放曲目",
                                    std::slice::from_ref(&card.playlist.title),
                                );
                            }
                            Err(err) => {
                                root.status =
                                    root.localized("加载电台失败：{err}", &[err.to_string()])
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
                        root.status = root.localized("加载歌单失败：{err}", &[err.to_string()])
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
        let current = self.queue.index();
        let remaining = self.queue.len().saturating_sub(current + 1);
        if remaining >= 6 {
            return;
        }

        self.loading_recommendation = true;
        self.recommendation_request_seq = self.recommendation_request_seq.wrapping_add(1);
        let request_seq = self.recommendation_request_seq;
        self.status = self.tr("正在加载更多推荐…").to_string();
        cx.notify();

        let settings = self.settings.clone();
        let work = cx.background_spawn(async move {
            sodam_core::library::recommended_append(&Session::new(settings), &source)
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
                        root.set_status("加载更多推荐失败：{err}", &[err.to_string()]);
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
        self.status = match save_result {
            Ok(()) => self.tr("已退出登录").to_string(),
            Err(err) => self.localized("已退出，但保存配置失败：{err}", &[err.to_string()]),
        };
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
        self.on_nav_changed(cx);
        cx.notify();
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
        if !self.settings.cookie.trim().is_empty() {
            match self.nav {
                Nav::Home => self.start_recommendation(true, cx),
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
                    if std::env::var("SODAM_THEME_LOG").is_ok() {
                        eprintln!(
                            "[theme] cover url={:?} path={:?} color={:?}",
                            url,
                            path.as_ref().map(|path| path
                                .file_name()
                                .map(|name| name.to_string_lossy())
                                .unwrap_or_default()),
                            color
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
            self.status = self.tr("需要先登录才能读取歌单").to_string();
            cx.notify();
            return;
        }
        let settings = self.settings.clone();
        self.loading_library = true;
        self.status = self.tr("正在读取歌单…").to_string();
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
                        root.status = root.localized("读取歌单失败：{err}", &[err.to_string()])
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
            sodam_core::library::liked_track_ids(&session)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                root.liked_loading = false;
                match result {
                    Ok(ids) => {
                        root.liked_ids = Arc::new(ids);
                        cx.notify();
                    }
                    Err(_) => { /* 未登录/网络异常：保持空集合，不打扰用户 */ }
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
        self.status = if liked {
            self.localized("已收藏：{}", std::slice::from_ref(&track.title))
        } else {
            self.localized("已取消收藏：{}", std::slice::from_ref(&track.title))
        };
        cx.notify();

        let settings = self.settings.clone();
        let work_id = id.clone();
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            sodam_core::library::set_track_liked(&session, &work_id, liked)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                if let Err(err) = result {
                    // 回滚
                    let mut ids = root.liked_ids.as_ref().clone();
                    if liked {
                        ids.remove(&id);
                    } else {
                        ids.insert(id.clone());
                    }
                    root.liked_ids = Arc::new(ids);
                    root.localized("收藏失败：{err}", &[err.to_string()]);
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
            self.status = self.tr("需要先登录才能读取收藏").to_string();
            cx.notify();
            return;
        }
        let settings = self.settings.clone();
        self.loading_liked = true;
        self.status = self.tr("正在读取我喜欢的音乐…").to_string();
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
                        root.status = root.localized("读取收藏失败：{err}", &[err.to_string()])
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

    /// 随机播放整张歌单：清空当前队列，按洗牌后的顺序从第一首开始。
    pub fn shuffle_play_arc(
        &mut self,
        tracks: std::sync::Arc<Vec<TrackItem>>,
        cx: &mut Context<Self>,
    ) {
        let shuffled = Queue::shuffled(tracks.as_ref().clone());
        self.play_from(shuffled, 0, cx);
    }

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
        self.queue.mode = sodam_core::queue::PlayMode::Sequential;
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
                        root.set_status("读取搜索歌单失败：{err}", &[err.to_string()]);
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
        self.status = self.tr("正在读取歌单曲目…").to_string();
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
                        root.set_status("读取歌单失败：{err}", &[err.to_string()]);
                        root.open_playlist = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.engine.snapshot();
        if snapshot.track_id.is_empty() {
            if self.queue.is_empty() {
                self.status = self.tr("队列是空的：先去搜索或打开一张歌单").to_string();
                cx.notify();
                return;
            }
            // 队列里有歌但引擎还没装载 → 装载当前曲目
            self.start_track(cx);
            return;
        }
        self.engine.toggle();
        self.playing = !snapshot.playing;
        self.status = if self.playing {
            self.tr("播放中").to_string()
        } else {
            self.tr("已暂停").to_string()
        };
        cx.notify();
    }

    /// 装载并播放「当前队列曲目」：后台下载+解密 → 回 UI 线程交给引擎。
    pub fn start_track(&mut self, cx: &mut Context<Self>) {
        let Some(track) = self.queue.current().cloned() else {
            self.status = self.tr("队列是空的：先去搜索或打开一张歌单").to_string();
            cx.notify();
            return;
        };
        if std::env::var("SODAM_PLAYER_LOG").is_ok() {
            eprintln!("[player] 开始装载：{}（id={}）", track.title, track.id);
        }
        self.playing = true;
        self.play_seq = self.play_seq.wrapping_add(1);
        let request_seq = self.play_seq;
        self.pending_track = Some(track.clone());
        self.progress_preview = None;
        self.set_status("正在准备播放：{}…", std::slice::from_ref(&track.title));
        self.ensure_covers(std::slice::from_ref(&track.cover), cx);
        if self.nav == Nav::Lyrics {
            self.load_lyrics(track.clone(), false, cx);
        }
        cx.notify();

        let settings = self.settings.clone();
        let work_track = track.clone();
        // 拉流失败重试 2 次（共 3 次），退避逐渐拉长
        let work = cx.background_spawn(async move {
            let session = Session::new(settings);
            let mut last_err = None;
            for attempt in 1..=3 {
                match session.download_to_cache(&work_track) {
                    Ok(cached) => return Ok(cached),
                    Err(err) => {
                        if std::env::var("SODAM_PLAYER_LOG").is_ok() {
                            eprintln!("[player] 第 {attempt} 次拉流失败：{err}");
                        }
                        last_err = Some(err);
                        if attempt < 3 {
                            std::thread::sleep(std::time::Duration::from_millis(
                                800 * attempt as u64,
                            ));
                        }
                    }
                }
            }
            Err(last_err.unwrap_or_else(|| anyhow::anyhow!("拉流失败")))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |root, cx| {
                // 这次加载已经被更新的播放请求取代 → 整个结果作废
                if root.play_seq != request_seq {
                    if std::env::var("SODAM_PLAYER_LOG").is_ok() {
                        eprintln!(
                            "[player] 丢弃过期加载结果（seq={request_seq} 当前={}）：{}",
                            root.play_seq, track.title
                        );
                    }
                    return;
                }
                match result {
                    Ok(cached) => {
                        root.engine.load(track.clone(), cached.path, cached.quality);
                        if std::env::var("SODAM_PLAYER_LOG").is_ok() {
                            eprintln!("[player] 装载完成：{}", track.title);
                        }
                        root.consecutive_failures = 0;
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
                        root.set_status("播放中：{}", std::slice::from_ref(&track.title));
                        // 开播后立刻预取下一首，切歌时基本是秒开
                        root.spawn_prefetch(cx);
                    }
                    Err(err) => {
                        if std::env::var("SODAM_PLAYER_LOG").is_ok() {
                            eprintln!("[player] 装载失败：{} → {err}", track.title);
                        }
                        root.pending_track = None;
                        root.consecutive_failures += 1;
                        let message = root.localized(
                            "「{}」拉流失败：{err}",
                            &[track.title.to_string(), err.to_string()],
                        );
                        root.status = message.clone();
                        root.playback_error = Some(message);
                        if root.consecutive_failures >= 3 {
                            root.playing = false;
                            root.status = root
                                .tr("连续 3 首拉流失败，已暂停（检查网络或签名服务）")
                                .to_string();
                        } else if !root.queue.is_empty() {
                            root.next_track(cx);
                        } else {
                            root.playing = false;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn next_track(&mut self, cx: &mut Context<Self>) {
        // 切歌时丢掉进度预览，避免「拖到一半换歌」把 seek 用到新歌上
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
        match self
            .session
            .as_mut()
            .map(|session| session.apply(settings.clone()))
        {
            Some(Ok(())) => {
                self.settings = settings;
                self.status = self.tr("设置已保存").to_string();
            }
            Some(Err(err)) => self.status = self.localized("保存失败：{err}", &[err.to_string()]),
            None => {
                self.settings = settings;
                self.session = Some(Session::new(self.settings.clone()));
            }
        }
    }
}
