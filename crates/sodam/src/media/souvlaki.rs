//! macOS 的 Now Playing 中心 / Windows 的 SMTC 传输控件，通过 `souvlaki` 接入。
//!
//! 这两个平台没有 MPRIS，媒体键由系统转给「当前在放音频的那个应用」，
//! 所以我们要做的只是把曲目元数据与播放状态交上去，并接住系统转来的按键。
//! Windows 的控件挂在窗口上，`MediaControls::new` 必须拿到 HWND。

use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{anyhow, Result};
use gpui::App;
use sodam_core::log_outlet::{log_event, LogLevel, LogTopic};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};

use super::{Command, NowPlaying, BUS_NAME, DISPLAY_NAME};

pub struct Controls {
    controls: MediaControls,
}

impl Controls {
    pub fn new(
        hwnd: Option<*mut c_void>,
        commands: Sender<Command>,
        _cx: &mut App,
    ) -> Result<Self> {
        let config = PlatformConfig {
            dbus_name: BUS_NAME,
            display_name: DISPLAY_NAME,
            hwnd,
        };
        let mut controls = MediaControls::new(config).map_err(|error| anyhow!("{error:?}"))?;
        if let Err(error) = controls.attach(move |event| {
            if let Some(command) = command(event) {
                commands.send(command).ok();
            }
        }) {
            let detail = format!("{error:?}");
            log_event(
                LogLevel::Warn,
                LogTopic::Media,
                "hotkey_listen_failed",
                &[("detail", detail.as_str())],
            );
        }
        Ok(Self { controls })
    }

    /// 发布曲目元数据。
    ///
    /// macOS 上这一次调用会**整块替换** `nowPlayingInfo`：调用方必须在之后补一次
    /// `publish_playback`，否则 elapsed time 会一直是空的（进度条不动）。
    pub fn publish_metadata(&mut self, now: &NowPlaying) {
        let cover_url = now.cover_url();
        let metadata = if now.has_track() {
            MediaMetadata {
                title: Some(now.title.as_str()),
                artist: Some(now.artist.as_str()),
                album: Some(now.album.as_str()),
                duration: Some(duration(now.duration_seconds)),
                cover_url: cover_url.as_deref(),
            }
        } else {
            MediaMetadata::default()
        };
        if let Err(error) = self.controls.set_metadata(metadata) {
            let detail = format!("{error:?}");
            log_event(
                LogLevel::Warn,
                LogTopic::Media,
                "metadata_publish_failed",
                &[("detail", detail.as_str())],
            );
        }
    }

    /// 发布播放状态与进度。
    pub fn publish_playback(&mut self, now: &NowPlaying) {
        let progress = Some(MediaPosition(duration(now.position_seconds)));
        let playback = match (now.has_track(), now.playing) {
            (false, _) => MediaPlayback::Stopped,
            (true, true) => MediaPlayback::Playing { progress },
            (true, false) => MediaPlayback::Paused { progress },
        };
        if let Err(error) = self.controls.set_playback(playback) {
            let detail = format!("{error:?}");
            log_event(
                LogLevel::Warn,
                LogTopic::Media,
                "playback_publish_failed",
                &[("detail", detail.as_str())],
            );
        }
    }
}

fn duration(seconds: f64) -> Duration {
    Duration::from_secs_f64(seconds.max(0.0))
}

fn command(event: MediaControlEvent) -> Option<Command> {
    Some(match event {
        MediaControlEvent::Play => Command::Play,
        MediaControlEvent::Pause | MediaControlEvent::Stop => Command::Pause,
        MediaControlEvent::Toggle => Command::Toggle,
        MediaControlEvent::Next => Command::Next,
        MediaControlEvent::Previous => Command::Previous,
        // 进度拖动、音量、打开链接这些系统控件也会发过来，暂时不接。
        MediaControlEvent::SetPosition(_)
        | MediaControlEvent::Seek(_)
        | MediaControlEvent::SeekBy(_, _)
        | MediaControlEvent::SetVolume(_)
        | MediaControlEvent::OpenUri(_)
        | MediaControlEvent::Raise
        | MediaControlEvent::Quit => return None,
    })
}
