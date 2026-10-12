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
            eprintln!("[media] 无法监听媒体键：{error:?}");
        }
        Ok(Self { controls })
    }

    pub fn publish(&mut self, now: &NowPlaying) {
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
            eprintln!("[media] 发布曲目失败：{error:?}");
        }

        let progress = Some(MediaPosition(duration(now.position_seconds)));
        let playback = match (now.has_track(), now.playing) {
            (false, _) => MediaPlayback::Stopped,
            (true, true) => MediaPlayback::Playing { progress },
            (true, false) => MediaPlayback::Paused { progress },
        };
        if let Err(error) = self.controls.set_playback(playback) {
            eprintln!("[media] 发布播放状态失败：{error:?}");
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
