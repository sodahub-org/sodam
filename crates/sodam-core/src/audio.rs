//! 播放引擎：用 rodio 播放 libresoda 解密后的本地文件。
//!
//! 设计：`OutputStream` 在 rodio 0.20 里是 `!Send`，所以**音频对象全部留在专属线程**，
//! UI 侧只通过命令通道操作、通过 `Arc<Mutex<PlaybackSnapshot>>` 读状态。
//!
//! 没有可用音频设备时**不 panic**：把错误写进快照，界面照常能跑（用户可能没接耳机）。

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::models::TrackItem;

/// 播放器状态快照（UI 每帧读这个，加锁时间极短）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaybackSnapshot {
    pub track_id: String,
    pub title: String,
    pub playing: bool,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub volume: f32,
    /// 当前曲目**实际拉到的流**音质（例：`无损 871k`）。
    pub quality: String,
    /// 最近一次错误（设备不可用、解码失败…），UI 直接展示
    pub error: String,
    /// 当前曲目是否已播完（UI 据此自动切下一首）
    pub finished: bool,
    /// 播完序号：每播完一次 +1。UI 用它做「处理过了吗」的判断 ——
    /// 之前用 track_id 比较，遇到循环同一首或 id 重复就会漏切。
    pub finished_seq: u64,
}

impl PlaybackSnapshot {
    /// 播放进度比例（0.0~1.0），时长未知时返回 0。
    pub fn progress_fraction(&self) -> f32 {
        if self.duration_seconds <= 0.0 {
            return 0.0;
        }
        (self.position_seconds / self.duration_seconds).clamp(0.0, 1.0) as f32
    }

    /// 当前播放位置 `mm:ss`。
    pub fn position_label(&self) -> String {
        format_seconds(self.position_seconds)
    }

    /// 总时长 `mm:ss`。
    pub fn duration_label(&self) -> String {
        format_seconds(self.duration_seconds)
    }

    /// `mm:ss / mm:ss` 形式的进度文案。
    pub fn progress_label(&self) -> String {
        format!("{} / {}", self.position_label(), self.duration_label())
    }
}

enum Command {
    Load {
        track: Box<TrackItem>,
        path: PathBuf,
        /// 这次实际拉到的流音质（显示用）
        quality: String,
    },
    Toggle,
    Play,
    Pause,
    Stop,
    SetVolume(f32),
    /// 跳转到指定秒数
    Seek(f64),
}

/// 播放引擎句柄（可以在任意线程调用，内部只有发命令与读快照）。
pub struct PlaybackEngine {
    tx: Sender<Command>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
}

impl Default for PlaybackEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackEngine {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot {
            volume: 1.0,
            ..Default::default()
        }));
        let state = snapshot.clone();
        let spawned = std::thread::Builder::new()
            .name("sodam-audio".to_string())
            .spawn(move || audio_thread(rx, state));
        if let Err(err) = spawned {
            if let Ok(mut snap) = snapshot.lock() {
                snap.error = format!("音频线程启动失败：{err}");
            }
        }
        Self { tx, snapshot }
    }

    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.snapshot
            .lock()
            .map(|snap| snap.clone())
            .unwrap_or_default()
    }

    /// 播放一个**已经下载并解密**到本地的文件。
    /// 装载并播放：`quality` 是这次实际拉到的流音质（显示用）。
    pub fn load(&self, track: TrackItem, path: PathBuf, quality: impl Into<String>) {
        let _ = self.tx.send(Command::Load {
            track: Box::new(track),
            path,
            quality: quality.into(),
        });
    }

    pub fn toggle(&self) {
        let _ = self.tx.send(Command::Toggle);
    }

    pub fn play(&self) {
        let _ = self.tx.send(Command::Play);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Command::Pause);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }

    pub fn set_volume(&self, volume: f32) {
        let _ = self.tx.send(Command::SetVolume(volume.clamp(0.0, 1.0)));
    }

    /// 跳转到指定秒数（越界会被引擎裁剪到 [0, 时长]）。
    pub fn seek(&self, seconds: f64) {
        let _ = self.tx.send(Command::Seek(seconds.max(0.0)));
    }
}

fn set_error(state: &Arc<Mutex<PlaybackSnapshot>>, message: String) {
    if let Ok(mut snap) = state.lock() {
        snap.error = message;
        snap.playing = false;
    }
}

fn sync_progress(state: &Arc<Mutex<PlaybackSnapshot>>, player: &rodio::Player) {
    if let Ok(mut snap) = state.lock() {
        snap.position_seconds = player.get_pos().as_secs_f64();
        snap.playing = !player.is_paused() && !player.empty();
    }
}

fn audio_thread(rx: Receiver<Command>, state: Arc<Mutex<PlaybackSnapshot>>) {
    // 打不开输出设备就把错误记在快照里，后续命令照常消费（界面不崩）
    let output = match rodio::DeviceSinkBuilder::open_default_sink() {
        Ok(device) => Some(device),
        Err(err) => {
            set_error(&state, format!("音频设备不可用：{err}"));
            None
        }
    };

    let mut player: Option<rodio::Player> = None;
    // 当前曲目是否已经上报过「播完」；换曲时重置
    let mut finished_reported = false;
    loop {
        // 为什么要超时 recv：进度必须**持续**更新（UI 的进度条依赖它）。
        // 之前只在 play/pause 时同步一次，所以进度条永远停在 0。
        let command = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(command) => Some(command),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let Some(command) = command else {
            if let Some(current) = &player {
                sync_progress(&state, current);
                // 播完：置 finished，UI 会据此自动切下一首
                // 注意：sync_progress 会把 playing 置 false，所以这里不能用 playing 判断，
                // 必须用本地的 finished_reported 标志（否则自动下一首永远不触发）。
                if current.empty() && !finished_reported {
                    finished_reported = true;
                    if let Ok(mut snap) = state.lock() {
                        snap.playing = false;
                        snap.finished = true;
                        snap.finished_seq = snap.finished_seq.wrapping_add(1);
                    }
                }
            }
            continue;
        };
        match command {
            Command::Load {
                track,
                path,
                quality,
            } => {
                if let Some(previous) = player.take() {
                    previous.stop();
                }
                let Some(device) = &output else {
                    set_error(&state, "音频设备不可用，无法播放".to_string());
                    continue;
                };
                let file = match std::fs::File::open(&path) {
                    Ok(file) => file,
                    Err(err) => {
                        set_error(&state, format!("打开音频文件失败：{err}"));
                        continue;
                    }
                };
                let decoder = match rodio::Decoder::new(std::io::BufReader::new(file)) {
                    Ok(decoder) => decoder,
                    Err(err) => {
                        set_error(&state, format!("解码失败（文件可能未解密）：{err}"));
                        continue;
                    }
                };
                let new_player = rodio::Player::connect_new(device.mixer());
                let volume = state.lock().map(|snap| snap.volume).unwrap_or(1.0);
                new_player.set_volume(volume);
                new_player.append(decoder);
                new_player.play();
                if let Ok(mut snap) = state.lock() {
                    snap.track_id = track.id.clone();
                    snap.title = track.title.clone();
                    snap.playing = true;
                    snap.position_seconds = 0.0;
                    snap.duration_seconds = track.duration_seconds.max(0) as f64;
                    snap.quality = quality.clone();
                    snap.error.clear();
                    snap.finished = false;
                    finished_reported = false;
                }
                player = Some(new_player);
            }
            Command::Toggle => {
                if let Some(current) = &player {
                    if current.is_paused() {
                        current.play();
                    } else {
                        current.pause();
                    }
                    sync_progress(&state, current);
                } else {
                    set_error(&state, "还没有可播放的曲目".to_string());
                }
            }
            Command::Play => {
                if let Some(current) = &player {
                    current.play();
                    sync_progress(&state, current);
                }
            }
            Command::Pause => {
                if let Some(current) = &player {
                    current.pause();
                    sync_progress(&state, current);
                }
            }
            Command::Stop => {
                if let Some(current) = player.take() {
                    current.stop();
                }
                if let Ok(mut snap) = state.lock() {
                    snap.playing = false;
                    snap.position_seconds = 0.0;
                }
            }
            Command::Seek(seconds) => {
                if let Some(current) = &player {
                    if current
                        .try_seek(std::time::Duration::from_secs_f64(seconds))
                        .is_ok()
                    {
                        // 跳转后进度要立刻反映出来（避免界面回跳）
                        if let Ok(mut snap) = state.lock() {
                            snap.position_seconds = seconds;
                        }
                    }
                }
            }
            Command::SetVolume(volume) => {
                if let Some(current) = &player {
                    current.set_volume(volume);
                }
                if let Ok(mut snap) = state.lock() {
                    snap.volume = volume;
                }
            }
        }
        if let Some(current) = &player {
            sync_progress(&state, current);
        }
    }
}

/// 秒 → `mm:ss`。
fn format_seconds(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// 音频缓存目录。
pub fn cache_dir() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sodam")
        .join("audio")
}

/// 封面缓存目录。
pub fn cover_cache_dir() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sodam")
        .join("covers")
}

/// 目录占用（字节数，文件数）。
fn dir_usage(dir: &std::path::Path) -> (u64, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut bytes = 0u64;
    let mut files = 0usize;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// 缓存统计：(音频字节, 音频文件数, 封面字节, 封面文件数)。
pub fn cache_stats() -> (u64, usize, u64, usize) {
    let (audio_bytes, audio_files) = dir_usage(&cache_dir());
    let (cover_bytes, cover_files) = dir_usage(&cover_cache_dir());
    (audio_bytes, audio_files, cover_bytes, cover_files)
}

/// 清空缓存（音频 + 封面），返回删除的文件数。
pub fn clear_cache() -> usize {
    let mut removed = 0usize;
    for dir in [cache_dir(), cover_cache_dir()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_label_formats() {
        let snap = PlaybackSnapshot {
            position_seconds: 65.4,
            duration_seconds: 236.6,
            ..Default::default()
        };
        assert_eq!(snap.progress_label(), "01:05 / 03:56");
    }

    #[test]
    fn engine_starts_without_audio_device_and_reports_state() {
        // 没有音频设备时也应该能创建引擎（错误写进快照，不 panic）
        let engine = PlaybackEngine::new();
        let snap = engine.snapshot();
        assert_eq!(snap.volume, 1.0);
        assert!(!snap.playing);
    }
}
