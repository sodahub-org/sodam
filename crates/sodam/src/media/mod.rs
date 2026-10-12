//! 系统媒体控制：媒体键与桌面媒体挂件（MPRIS / Now Playing / SMTC）。
//!
//! 做法照 sonora：Linux 走 `mpris-server`（注册 `org.mpris.MediaPlayer2`，
//! `playerctl`、桌面挂件、Hyprland 默认绑定的 `XF86Audio*` 都能控），
//! macOS 与 Windows 走 `souvlaki`（Now Playing 中心 / SMTC 传输控件）。
//!
//! 平台回调在别的线程上跑，所以只往 channel 里塞命令；播放状态反向发布，
//! 且只在内容变化时发布（不是定时刷）。

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use gpui::{App, Global};

#[cfg(target_os = "linux")]
mod mpris;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod souvlaki;

#[cfg(target_os = "linux")]
use mpris::Controls;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use souvlaki::Controls;

/// MPRIS / SMTC 上的进程名与展示名。
const BUS_NAME: &str = "com.sodahub.SodaM";
const DISPLAY_NAME: &str = "SodaM";
/// MPRIS 的 DesktopEntry：指向安装的 `sodam.desktop`。
#[cfg(target_os = "linux")]
const DESKTOP_ENTRY: &str = "sodam";

/// 桌面媒体控件发过来的请求（媒体键、挂件按钮都会变成这几个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
}

/// 要发布给系统的播放状态。
#[derive(Debug, Clone, Default)]
pub struct NowPlaying {
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 本地缓存里的封面文件。
    ///
    /// 发本地文件而不是远程 URL 是 sonora 踩过的坑：系统挂件会自己去下远程封面，
    /// 在 macOS 上那一下失败会把进程带崩。
    pub cover_path: Option<PathBuf>,
    pub duration_seconds: f64,
    pub position_seconds: f64,
    pub playing: bool,
}

impl NowPlaying {
    /// 是否真的有一首在放（没有就让平台层报 Stopped）。
    fn has_track(&self) -> bool {
        !self.track_id.trim().is_empty() || !self.title.trim().is_empty()
    }

    fn cover_url(&self) -> Option<String> {
        self.cover_path
            .as_ref()
            .map(|path| format!("file://{}", path.display()))
    }

    /// 变化检测用的键：同一首、同一状态、同一秒就不重复发布。
    fn key(&self) -> (String, bool, i64, Option<String>) {
        (
            self.track_id.clone(),
            self.playing,
            self.position_seconds as i64,
            self.cover_url(),
        )
    }
}

/// 系统媒体控制句柄。
struct Media {
    controls: Controls,
    commands: Receiver<Command>,
    published: Option<(String, bool, i64, Option<String>)>,
}

struct Attached {
    media: RefCell<Media>,
}

impl Global for Attached {}

/// 注册系统媒体控制。窗口建好之后调用（Windows 的 SMTC 需要 HWND）。
///
/// 注册失败只记日志：拿不到媒体面板不该影响播放。
pub fn attach(hwnd: Option<*mut c_void>, cx: &mut App) {
    if cx.has_global::<Attached>() {
        return;
    }
    let (sender, commands) = std::sync::mpsc::channel::<Command>();
    let controls = match Controls::new(hwnd, sender, cx) {
        Ok(controls) => controls,
        Err(error) => {
            eprintln!("[media] 无法接入系统媒体控制：{error:#}");
            return;
        }
    };
    cx.set_global(Attached {
        media: RefCell::new(Media {
            controls,
            commands,
            published: None,
        }),
    });
}

/// 取走桌面媒体控件发来的命令。
pub fn drain_commands(cx: &App) -> Vec<Command> {
    cx.try_global::<Attached>()
        .map(|attached| attached.media.borrow().commands.try_iter().collect())
        .unwrap_or_default()
}

/// 把当前播放状态发布给系统；内容没变就不发。
pub fn publish(cx: &App, now: &NowPlaying) {
    let Some(attached) = cx.try_global::<Attached>() else {
        return;
    };
    let mut media = attached.media.borrow_mut();
    let key = now.key();
    if media.published.as_ref() == Some(&key) {
        return;
    }
    media.published = Some(key);
    media.controls.publish(now);
}
