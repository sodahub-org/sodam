//! Linux 的 MPRIS（`org.mpris.MediaPlayer2`），通过 `mpris-server` 接入。
//!
//! 注册之后 `playerctl`、GNOME/KDE 的媒体挂件、以及合成器里绑的 `XF86Audio*`
//! 都能控制播放（Hyprland 默认配置就是把媒体键交给 playerctl）。
//!
//! `mpris-server` 的 `Player` 是 `!Send` 的，所以它留在 gpui 的任务里（主线程），
//! 应用侧的每次发布只是往 channel 里排一条更新，按顺序应用。

use std::ffi::c_void;
use std::time::Duration;

use anyhow::Result;
use futures_lite::future;
use gpui::{App, Task};
use mpris_server::{Metadata, PlaybackStatus, Player, Time, TrackId};
use sodam_core::log_outlet::{log_event, LogLevel, LogTopic};
use tokio::sync::mpsc;

use super::{Command, CommandSender, NowPlaying, BUS_NAME, DESKTOP_ENTRY, DISPLAY_NAME};

/// 注册失败后的首次重试间隔；之后指数退避到这个上限。
const REGISTER_RETRY: Duration = Duration::from_secs(5);
const REGISTER_RETRY_MAX: Duration = Duration::from_secs(60);

/// 发布到 MPRIS 上的一次变更，按发送顺序应用。
enum Update {
    Metadata(Metadata),
    Playback(PlaybackStatus, Time),
}

pub struct Controls {
    updates: mpsc::UnboundedSender<Update>,
    /// 持有服务任务：丢掉它等于关掉 MPRIS。
    _server: Task<()>,
}

impl Controls {
    /// 起后台任务去抢总线名。抢不到只记日志，之后每次发布都变成空操作。
    pub fn new(_hwnd: Option<*mut c_void>, commands: CommandSender, cx: &mut App) -> Result<Self> {
        let (updates, mut receiver) = mpsc::unbounded_channel();
        let _server = cx.spawn(async move |cx| {
            // 抢总线名可能失败：上一个实例还没退出、或者同时开了两个 sodam。
            // 之前失败就直接放弃，于是"第二个启动的实例永远没有 MPRIS"，
            // 媒体键和桌面挂件全用它——这里改成失败后退避重试。
            let mut backoff = REGISTER_RETRY;
            loop {
                let player = match build(commands.clone()).await {
                    Ok(player) => player,
                    Err(error) => {
                        let detail = error.to_string();
                        let retry_s = backoff.as_secs().to_string();
                        log_event(
                            LogLevel::Warn,
                            LogTopic::Media,
                            "register_failed",
                            &[("detail", detail.as_str()), ("retry_s", retry_s.as_str())],
                        );
                        cx.background_executor().timer(backoff).await;
                        backoff = (backoff * 2).min(REGISTER_RETRY_MAX);
                        continue;
                    }
                };
                backoff = REGISTER_RETRY;

                let apply = async {
                    while let Some(update) = receiver.recv().await {
                        if let Err(error) = apply(&player, update).await {
                            let detail = error.to_string();
                            log_event(
                                LogLevel::Warn,
                                LogTopic::Media,
                                "publish_failed",
                                &[("detail", detail.as_str())],
                            );
                        }
                    }
                };
                // 两个 future 都要一直跑：`run()` 服务请求，`apply` 应用应用侧的状态。
                future::zip(player.run(), apply).await;

                // 更新通道已关闭 = 应用侧丢掉了 Controls，没必要再重连。
                if receiver.is_closed() {
                    return;
                }
                // 否则是 D-Bus 连接断了（会话重启等），退避后重新注册。
                let retry_s = backoff.as_secs().to_string();
                log_event(
                    LogLevel::Warn,
                    LogTopic::Media,
                    "connection_lost",
                    &[("retry_s", retry_s.as_str())],
                );
                cx.background_executor().timer(backoff).await;
            }
        });
        Ok(Self { updates, _server })
    }

    /// 发布曲目元数据（只在曲目或封面变化时调用）。
    pub fn publish_metadata(&mut self, now: &NowPlaying) {
        let metadata = if now.has_track() {
            let mut metadata = Metadata::builder()
                .trackid(track_id(&now.track_id))
                .title(now.title.clone())
                .artist(vec![now.artist.clone()])
                .album(now.album.clone())
                .length(time(now.duration_seconds))
                .build();
            metadata.set_art_url(now.cover_url().as_deref());
            metadata
        } else {
            Metadata::builder().trackid(TrackId::NO_TRACK).build()
        };
        self.send(Update::Metadata(metadata));
    }

    /// 发布播放状态与进度（播放/暂停切换、切曲、拖动时调用）。
    pub fn publish_playback(&mut self, now: &NowPlaying) {
        let status = match (now.has_track(), now.playing) {
            (false, _) => PlaybackStatus::Stopped,
            (true, true) => PlaybackStatus::Playing,
            (true, false) => PlaybackStatus::Paused,
        };
        self.send(Update::Playback(status, time(now.position_seconds)));
    }

    fn send(&self, update: Update) {
        self.updates.send(update).ok();
    }
}

/// 注册 `org.mpris.MediaPlayer2.sodam`，把播放器接受的方法转成 `Command`。
async fn build(commands: CommandSender) -> mpris_server::zbus::Result<Player> {
    let player = Player::builder(BUS_NAME)
        .identity(DISPLAY_NAME)
        .desktop_entry(DESKTOP_ENTRY)
        .can_play(true)
        .can_pause(true)
        .can_go_next(true)
        .can_go_previous(true)
        .can_control(true)
        .build()
        .await?;

    let send = move |command| {
        commands.send(command).ok();
    };
    player.connect_play({
        let send = send.clone();
        move |_| send(Command::Play)
    });
    player.connect_pause({
        let send = send.clone();
        move |_| send(Command::Pause)
    });
    player.connect_stop({
        let send = send.clone();
        move |_| send(Command::Pause)
    });
    player.connect_play_pause({
        let send = send.clone();
        move |_| send(Command::Toggle)
    });
    player.connect_next({
        let send = send.clone();
        move |_| send(Command::Next)
    });
    player.connect_previous(move |_| send(Command::Previous));
    Ok(player)
}

async fn apply(player: &Player, update: Update) -> mpris_server::zbus::Result<()> {
    match update {
        Update::Metadata(metadata) => player.set_metadata(metadata).await,
        Update::Playback(status, at) => {
            player.set_position(at);
            player.set_playback_status(status).await
        }
    }
}

/// MPRIS 需要每首曲目一个对象路径，用来把「跳转到哪个曲目」的请求对上号。
/// 服务端的 id 里有路径不允许的字符，所以路径放它的哈希。
fn track_id(id: &str) -> TrackId {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    TrackId::try_from(format!("/app/sodam/track/t{:016x}", hasher.finish()))
        .unwrap_or(TrackId::NO_TRACK)
}

fn time(seconds: f64) -> Time {
    Time::from_micros((seconds.max(0.0) * 1_000_000.0) as i64)
}
