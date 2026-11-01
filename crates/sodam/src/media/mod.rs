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
use sodam_core::log_outlet::{log_event, LogLevel, LogTopic};

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
    /// 桌面歌词扩展点收藏按钮时发来。
    ToggleLike,
    /// 桌面歌词条显隐（标题栏开关或扩展发来）。
    SetBarVisible(bool),
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
    /// 当前歌词行与下一行，供桌面歌词扩展读取。
    pub lyric_current: String,
    pub lyric_next: String,
    /// 当前曲目是否有歌词。
    pub has_lyrics: bool,
    /// 当前曲目是否已收藏。
    pub liked: bool,
    /// 桌面歌词条是否显示。
    pub lyrics_visible: bool,
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

    /// 元数据的变化键：同一首、同一封面就不重发。
    ///
    /// 这条很关键：souvlaki 在 macOS 上每次 `set_metadata` 都会整块替换
    /// `nowPlayingInfo`（连 elapsed time 和封面一起清掉，封面再异步补），
    /// 系统那边每次都会当成一个新内容项。之前把进度秒数也算进 key，
    /// 等于每秒重建一次内容项 —— 封面会闪、进度条会跳回 0。
    fn metadata_key(&self) -> (String, Option<String>) {
        (self.track_id.clone(), self.cover_url())
    }

    /// 播放状态的变化键：同一首、同一播放状态下不重发。
    /// 进度由系统按播放速率自行插值，只有明显跳转（拖动/切曲）才值得重发。
    fn playback_key(&self) -> (String, bool) {
        (self.track_id.clone(), self.playing)
    }
}

/// 进度与上次发布值差多少才算「跳转」（秒）。系统会自己插值，不需要逐秒同步。
const POSITION_JUMP_SECONDS: f64 = 2.5;

/// 系统媒体控制句柄。
struct Media {
    controls: Controls,
    commands: Receiver<Command>,
    published_metadata: Option<(String, Option<String>)>,
    published_playback: Option<(String, bool)>,
    /// 上次发布出去的进度，用来识别拖动/切曲这种跳转。
    last_position: f64,
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
            let detail = format!("{error:#}");
            log_event(
                LogLevel::Warn,
                LogTopic::Media,
                "attach_failed",
                &[("detail", detail.as_str())],
            );
            return;
        }
    };
    cx.set_global(Attached {
        media: RefCell::new(Media {
            controls,
            commands,
            published_metadata: None,
            published_playback: None,
            last_position: 0.0,
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
///
/// 元数据与播放状态分开判断，理由见 `NowPlaying::metadata_key`：重发元数据在 macOS 上
/// 等于重建内容项，会把封面和进度一起清掉。元数据重建之后立刻补一次播放状态，
/// 因为那一下已经把 elapsed time 抹掉了。
pub fn publish(cx: &App, now: &NowPlaying) {
    let Some(attached) = cx.try_global::<Attached>() else {
        return;
    };
    let mut media = attached.media.borrow_mut();

    let metadata_key = now.metadata_key();
    if media.published_metadata.as_ref() != Some(&metadata_key) {
        media.published_metadata = Some(metadata_key);
        media.published_playback = None;
        media.controls.publish_metadata(now);
    }

    let playback_key = now.playback_key();
    let jumped = (now.position_seconds - media.last_position).abs() > POSITION_JUMP_SECONDS;
    if media.published_playback.as_ref() != Some(&playback_key) || jumped {
        media.published_playback = Some(playback_key);
        media.controls.publish_playback(now);
    }
    media.last_position = now.position_seconds;

    // 歌词走独立的 D-Bus 接口（MPRIS 没有歌词字段），只在 Linux 上提供。
    #[cfg(target_os = "linux")]
    media.controls.publish_lyrics(mpris::LyricsState {
        current: now.lyric_current.clone(),
        next: now.lyric_next.clone(),
        has_lyrics: now.has_lyrics,
        liked: now.liked,
        visible: now.lyrics_visible,
    });
}
