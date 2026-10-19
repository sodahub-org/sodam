//! 播放引擎：用 rodio 播放 libresoda 解密后的本地文件。
//!
//! 设计：`OutputStream` 在 rodio 0.20 里是 `!Send`，所以**音频对象全部留在专属线程**，
//! UI 侧只通过命令通道操作、通过 `Arc<Mutex<PlaybackSnapshot>>` 读状态。
//!
//! 没有可用音频设备时**不 panic**：把错误写进快照，界面照常能跑（用户可能没接耳机）。
//!
//! **跟随系统默认输出设备**：Windows(WASAPI)、macOS(CoreAudio) 下流在创建时就跟设备
//! 绑死，切换默认设备（音响 → USB 耳机）对已开的流无效，必须重建；否则要重启应用才生效。
//! 播放线程每 2 秒探测一次默认设备 ID，变化时换新输出并从原位置续播。
//! Linux 的 cpal ALSA 后端恒返回 `default` 单一设备，且 PipeWire/WirePlumber 会把现有流
//! 自动迁到新的默认节点，因此探测天然不触发 —— 行为与修复前一致。

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::models::TrackItem;
use crate::session::CachedTrack;
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// 最近处理完的装载请求（成功/失败都会更新），UI 不再只用 track_id 判断完成。
    pub load_seq: u64,
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

type OutputErrors = Arc<Mutex<Option<String>>>;
type OutputResult = Result<rodio::MixerDeviceSink, rodio::DeviceSinkError>;

enum Command {
    Load {
        track: Box<TrackItem>,
        path: PathBuf,
        /// 这次实际拉到的流音质（显示用）
        quality: String,
        lease: crate::cache::CacheLease,
        sequence: u64,
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
    next_load: AtomicU64,
}

impl Default for PlaybackEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackEngine {
    pub fn new() -> Self {
        Self::with_volume(1.0)
    }

    pub fn with_volume(saved_volume: f32) -> Self {
        Self::with_output(saved_volume, open_output, probe_default_output_id)
    }

    // 单测注入故障输出与设备探测，不触碰硬件/COM；正式运行始终使用上面的真实实现。
    fn with_output(
        saved_volume: f32,
        open_output: impl FnMut(OutputErrors) -> OutputResult + Send + 'static,
        probe: impl FnMut() -> Option<String> + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        // 显式环境覆盖仍优先（静音开发测试），无覆盖时恢复用户保存的值。
        let initial_volume =
            initial_volume(saved_volume, std::env::var("SODAM_VOLUME").ok().as_deref());
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot {
            volume: initial_volume,
            ..Default::default()
        }));
        let state = snapshot.clone();
        let spawned = std::thread::Builder::new()
            .name("sodam-audio".to_string())
            .spawn(move || audio_thread(rx, state, open_output, probe));
        if let Err(err) = spawned {
            if let Ok(mut snap) = snapshot.lock() {
                snap.error = format!("音频线程启动失败：{err}");
            }
        }
        Self {
            tx,
            snapshot,
            next_load: AtomicU64::new(1),
        }
    }

    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.snapshot
            .lock()
            .map(|snap| snap.clone())
            .unwrap_or_default()
    }

    /// 播放一个**已经下载并解密**到本地的文件。
    /// 装载并播放：`quality` 是这次实际拉到的流音质（显示用）。
    pub fn load(&self, track: TrackItem, cached: CachedTrack) -> u64 {
        let sequence = self.next_load.fetch_add(1, Ordering::Relaxed);
        self.send(Command::Load {
            track: Box::new(track),
            path: cached.path,
            quality: cached.quality,
            lease: cached.lease,
            sequence,
        });
        sequence
    }

    fn send(&self, command: Command) {
        if let Err(err) = self.tx.send(command) {
            let message = "音频线程已退出，请重启应用".to_string();
            if let Command::Load {
                track, sequence, ..
            } = err.0
            {
                load_failed(&self.snapshot, &track, sequence, message);
            } else {
                set_error(&self.snapshot, message);
            }
        }
    }

    pub fn toggle(&self) {
        self.send(Command::Toggle);
    }

    pub fn play(&self) {
        self.send(Command::Play);
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn set_volume(&self, volume: f32) {
        if volume.is_finite() {
            self.send(Command::SetVolume(volume.clamp(0.0, 1.0)));
        }
    }

    /// 跳转到指定秒数（越界会被引擎裁剪到 [0, 时长]）。
    pub fn seek(&self, seconds: f64) {
        self.send(Command::Seek(seconds.max(0.0)));
    }
}

fn set_error(state: &Arc<Mutex<PlaybackSnapshot>>, message: String) {
    if let Ok(mut snap) = state.lock() {
        snap.error = message;
        snap.playing = false;
        snap.finished = false;
    }
}

fn initial_volume(saved: f32, override_value: Option<&str>) -> f32 {
    override_value
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(if saved.is_finite() { saved } else { 1.0 })
        .clamp(0.0, 1.0)
}

fn load_failed(
    state: &Arc<Mutex<PlaybackSnapshot>>,
    track: &TrackItem,
    sequence: u64,
    message: String,
) {
    if let Ok(mut snap) = state.lock() {
        snap.track_id = track.id.clone();
        snap.title = track.title.clone();
        snap.load_seq = sequence;
        snap.position_seconds = 0.0;
        snap.duration_seconds = track.duration_seconds.max(0) as f64;
        snap.quality.clear();
        snap.error = message;
        snap.playing = false;
        snap.finished = false;
    }
}

fn sync_progress(state: &Arc<Mutex<PlaybackSnapshot>>, player: &rodio::Player) {
    // 不拿快照锁调用 rodio，避免设备/解码器锁等待传染到 UI 的 snapshot()。
    let position = player.get_pos().as_secs_f64();
    let playing = !player.is_paused() && !player.empty();
    if let Ok(mut snap) = state.lock() {
        snap.position_seconds = position;
        snap.playing = playing;
    }
}

fn open_output(errors: OutputErrors) -> OutputResult {
    rodio::DeviceSinkBuilder::from_default_device()?
        .with_error_callback(move |err| {
            if let Ok(mut errors) = errors.lock() {
                *errors = Some(format!("音频设备连接中断：{err}"));
            }
        })
        .open_sink_or_fallback()
}

struct OutputWatchdog {
    position: f64,
    advanced_at: std::time::Instant,
}

impl OutputWatchdog {
    fn new(now: std::time::Instant) -> Self {
        Self {
            position: 0.0,
            advanced_at: now,
        }
    }

    fn stalled(&mut self, position: f64, running: bool, now: std::time::Instant) -> bool {
        if !running || (position - self.position).abs() > 0.001 {
            self.position = position;
            self.advanced_at = now;
        }
        running && now.duration_since(self.advanced_at) >= std::time::Duration::from_secs(15)
    }
}

/// 探测系统默认输出设备的稳定 ID（`None` = 现在拿不到默认设备）。
fn probe_default_output_id() -> Option<String> {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};
    let host = rodio::cpal::default_host();
    let device = host.default_output_device()?;
    device.id().ok().map(|id| id.to_string())
}

/// 默认输出设备轮询节流：播放线程 100ms 一个 tick，20 个 tick ≈ 2 秒。
const DEVICE_POLL_TICKS: u8 = 20;

/// 默认输出设备变化检测。
///
/// 初次探测为空（启动时没有设备）不算切换，避免启动瞬间误重建。
/// 探测失败（一瞬间拿不到默认设备）也不算切换，避免把正在播的歌打断。
struct DeviceWatcher {
    last: Option<String>,
    ticks: u8,
}

impl DeviceWatcher {
    fn new(initial: Option<String>) -> Self {
        Self {
            last: initial,
            ticks: 0,
        }
    }

    /// 每个 tick 调用一次；返回 `true` 表示默认设备确实换了。
    fn poll(&mut self, probe: &mut impl FnMut() -> Option<String>) -> bool {
        self.ticks = self.ticks.saturating_add(1);
        if self.ticks < DEVICE_POLL_TICKS {
            return false;
        }
        self.ticks = 0;
        let Some(current) = probe() else {
            return false;
        };
        let changed = match &self.last {
            Some(previous) => *previous != current,
            None => false,
        };
        self.last = Some(current);
        changed
    }
}

fn audio_thread(
    rx: Receiver<Command>,
    state: Arc<Mutex<PlaybackSnapshot>>,
    mut open_output: impl FnMut(OutputErrors) -> OutputResult,
    mut probe: impl FnMut() -> Option<String>,
) {
    let stream_error = Arc::new(Mutex::new(None));
    // 失效的输出会在下一次装载时重新打开，不把启动时/休眠后的失败永久锁死。
    let mut output = match open_output(stream_error.clone()) {
        Ok(device) => Some(device),
        Err(err) => {
            set_error(&state, format!("音频设备不可用：{err}"));
            None
        }
    };

    let mut player: Option<rodio::Player> = None;
    // 重建输出时要从原位置续播，必须记住当前曲目的文件路径。
    let mut current_path: Option<PathBuf> = None;
    let mut device_watcher = DeviceWatcher::new(probe());
    let mut _current_lease = None;
    let mut watchdog = OutputWatchdog::new(std::time::Instant::now());
    // 当前曲目是否已经上报过「播完」；换曲时重置
    let mut finished_reported = false;
    loop {
        let stream_failure = stream_error.lock().ok().and_then(|mut error| error.take());
        if let Some(error) = stream_failure {
            if let Some(previous) = player.take() {
                previous.stop();
            }
            _current_lease = None;
            current_path = None;
            output = None;
            set_error(&state, error);
            finished_reported = true;
        }
        // 为什么要超时 recv：进度必须**持续**更新（UI 的进度条依赖它）。
        // 之前只在 play/pause 时同步一次，所以进度条永远停在 0。
        let command = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(command) => Some(command),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let Some(command) = command else {
            if let Some(current) = &player {
                if watchdog.stalled(
                    current.get_pos().as_secs_f64(),
                    !current.is_paused() && !current.empty(),
                    std::time::Instant::now(),
                ) {
                    if let Some(previous) = player.take() {
                        previous.stop();
                    }
                    _current_lease = None;
                    current_path = None;
                    output = None;
                    set_error(&state, "音频输出长时间无响应，请重试播放".to_string());
                    finished_reported = true;
                    continue;
                }
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
            // 设备切换放在「播完」判定**之后**：如果恰好在这个 tick 播完，
            // 必须先上报 finished（UI 才会切下一首），否则重建输出会把空播放器
            // 丢掉，finished 永远不触发，播放会卡在上一首的结尾。
            // 系统默认输出设备被切换（Windows 音响 → USB 耳机、macOS 换 AirPods…）：
            // 已打开的流仍绑在旧设备上，不重建就得重启应用才生效。
            if output.is_some() && device_watcher.poll(&mut probe) {
                follow_default_output(
                    &mut output,
                    &mut player,
                    &mut current_path,
                    &mut watchdog,
                    &state,
                    &stream_error,
                    &mut open_output,
                );
            }
            continue;
        };
        match command {
            Command::Load {
                track,
                path,
                quality,
                lease,
                sequence,
            } => {
                if let Some(previous) = player.take() {
                    previous.stop();
                }
                _current_lease = None;
                if output.is_none() {
                    match open_output(stream_error.clone()) {
                        Ok(device) => output = Some(device),
                        Err(err) => {
                            load_failed(&state, &track, sequence, format!("音频设备不可用：{err}"));
                            continue;
                        }
                    }
                }
                let Some(device) = &output else {
                    continue;
                };
                let file = match std::fs::File::open(&path) {
                    Ok(file) => file,
                    Err(err) => {
                        load_failed(&state, &track, sequence, format!("打开音频文件失败：{err}"));
                        continue;
                    }
                };
                let decoder = match rodio::Decoder::new(std::io::BufReader::new(file)) {
                    Ok(decoder) => decoder,
                    Err(err) => {
                        // 无效缓存不能永久冒充命中；下一次播放重新下载，而不是反复解码坏文件。
                        crate::cache::invalidate_audio(&path);
                        load_failed(
                            &state,
                            &track,
                            sequence,
                            format!("解码失败（文件可能未解密）：{err}"),
                        );
                        continue;
                    }
                };
                let new_player = rodio::Player::connect_new(device.mixer());
                let volume = state.lock().map(|snap| snap.volume).unwrap_or(1.0);
                new_player.set_volume(volume);
                new_player.append(decoder);
                new_player.play();
                if let Ok(mut snap) = state.lock() {
                    snap.load_seq = sequence;
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
                current_path = Some(path);
                watchdog = OutputWatchdog::new(std::time::Instant::now());
                _current_lease = Some(lease);
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
                _current_lease = None;
                current_path = None;
                if let Ok(mut snap) = state.lock() {
                    snap.playing = false;
                    snap.finished = false;
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

/// 默认设备变化后把输出重建到新设备，并从原位置继续播放。
///
/// 打开新设备失败时**保留旧流**（例如目标设备刚好在枚举瞬间消失），
/// 下一次轮询再试，避免一次瞬时失败把正在播的歌停掉。
fn follow_default_output(
    output: &mut Option<rodio::MixerDeviceSink>,
    player: &mut Option<rodio::Player>,
    current_path: &mut Option<PathBuf>,
    watchdog: &mut OutputWatchdog,
    state: &Arc<Mutex<PlaybackSnapshot>>,
    stream_error: &Arc<Mutex<Option<String>>>,
    open_output: &mut impl FnMut(OutputErrors) -> OutputResult,
) {
    // 先把续播信息取出来，再去动旧流。
    let resume = player
        .as_ref()
        .filter(|current| !current.empty())
        .map(|current| (current.get_pos(), current.is_paused()));
    let volume = state.lock().map(|snap| snap.volume).unwrap_or(1.0);

    let Ok(new_output) = open_output(stream_error.clone()) else {
        return;
    };
    if let Some(previous) = player.take() {
        previous.stop();
    }
    *output = Some(new_output);

    let Some((position, paused)) = resume else {
        // 没在播（已停止 / 已播完）：换好输出即可，下一次装载直接用新设备。
        *current_path = None;
        return;
    };
    let Some(path) = current_path.clone() else {
        return;
    };

    let decoder = std::fs::File::open(&path)
        .map_err(|err| err.to_string())
        .and_then(|file| {
            rodio::Decoder::new(std::io::BufReader::new(file)).map_err(|err| err.to_string())
        });
    let decoder = match decoder {
        Ok(decoder) => decoder,
        Err(err) => {
            set_error(state, format!("切换音频设备后重新打开失败：{err}"));
            return;
        }
    };
    let Some(device) = output.as_ref() else {
        return;
    };
    let new_player = rodio::Player::connect_new(device.mixer());
    new_player.set_volume(volume);
    new_player.append(decoder);
    if new_player.try_seek(position).is_err() {
        // 定位失败就从头播，总比丢歌好。
        let _ = new_player.try_seek(std::time::Duration::ZERO);
    }
    if paused {
        new_player.pause();
    } else {
        new_player.play();
    }
    *player = Some(new_player);
    *watchdog = OutputWatchdog::new(std::time::Instant::now());
    if let Ok(mut snap) = state.lock() {
        snap.position_seconds = position.as_secs_f64();
        snap.playing = !paused;
        snap.error.clear();
    }
}

/// 秒 → `mm:ss`。
fn format_seconds(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// 音频缓存目录。
pub fn cache_dir() -> std::path::PathBuf {
    crate::cache::root_dir().join("audio")
}

/// 封面缓存目录。
pub fn cover_cache_dir() -> std::path::PathBuf {
    crate::cache::root_dir().join("covers")
}

/// 目录占用：**递归**统计（字节数，文件数）。
///
/// 封面缩略图在 `covers/`，原图在 `covers/original/` 子目录 ——
/// 只看顶层会把整个子目录漏掉（实测踩过：字节/张数都偏小）。
fn dir_usage(dir: &std::path::Path) -> (u64, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut bytes = 0u64;
    let mut files = 0usize;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() {
            bytes += meta.len();
            files += 1;
        } else if meta.is_dir() {
            let (sub_bytes, sub_files) = dir_usage(&entry.path());
            bytes += sub_bytes;
            files += sub_files;
        }
    }
    (bytes, files)
}

/// 顶层目录里指定扩展名的文件数（用于「歌曲数」：只数 `.m4a`）。
fn count_extension(dir: &std::path::Path, extension: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().map(|ext| ext == extension) == Some(true))
        .count()
}

/// 缓存统计：(音频字节, 歌曲数, 封面字节, 封面张数)。
///
/// * 字节数含**全部**文件（`.m4a` / `.quality` 边车 / 下载中的 `.part` /
///   封面原图子目录）；
/// * 「歌曲数」只数 `.m4a`：边车和临时文件不是歌，全算会把数量翻倍（踩过）；
/// * 「封面张数」数全部图片缓存文件（含原图）。
///
/// 注意：这是同步扫盘，别在渲染路径里每帧调 —— 调用方应后台执行并缓存结果。
pub fn cache_stats() -> (u64, usize, u64, usize) {
    let (audio_bytes, _) = dir_usage(&cache_dir());
    let songs = count_extension(&cache_dir(), "m4a");
    let (cover_bytes, cover_files) = dir_usage(&cover_cache_dir());
    (audio_bytes, songs, cover_bytes, cover_files)
}

/// 清空缓存（音频 + 封面），返回删除的文件数。
pub fn clear_cache() -> usize {
    crate::cache::clear_in(&cache_dir()) + crate::cache::clear_in(&cover_cache_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 跑满一个轮询周期，返回每个 tick 的 `poll` 结果。
    fn advance(
        watcher: &mut DeviceWatcher,
        probe: &mut impl FnMut() -> Option<String>,
    ) -> Vec<bool> {
        (0..DEVICE_POLL_TICKS)
            .map(|_| watcher.poll(probe))
            .collect()
    }

    #[test]
    fn device_watcher_reports_each_switch_exactly_once() {
        let device = std::rc::Rc::new(std::cell::RefCell::new(Some("wasapi:speakers".to_string())));
        let mut watcher = DeviceWatcher::new(device.borrow().clone());
        let mut probe = || device.borrow().clone();

        // 没换设备：一个周期内一次都不报。
        assert!(advance(&mut watcher, &mut probe)
            .iter()
            .all(|changed| !changed));

        // 切到 USB 耳机：只在真正探测的那一个 tick 报一次。
        *device.borrow_mut() = Some("wasapi:usb-headset".to_string());
        let ticks = advance(&mut watcher, &mut probe);
        assert_eq!(ticks.iter().filter(|changed| **changed).count(), 1);
        assert!(ticks.last().copied().unwrap_or(false));

        // 之后不再重复报（否则会持续重建输出）。
        assert!(advance(&mut watcher, &mut probe)
            .iter()
            .all(|changed| !changed));
    }

    #[test]
    fn device_watcher_ignores_unknown_start_and_transient_failures() {
        let device: std::rc::Rc<std::cell::RefCell<Option<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        // 启动时没有默认设备：首次探测到设备只做采纳，不算切换。
        let mut watcher = DeviceWatcher::new(None);
        let mut probe = || device.borrow().clone();
        *device.borrow_mut() = Some("coreaudio:builtin".to_string());
        assert!(advance(&mut watcher, &mut probe)
            .iter()
            .all(|changed| !changed));

        // 探测失败（一瞬间拿不到默认设备）不能把正在播的歌打断。
        *device.borrow_mut() = None;
        assert!(advance(&mut watcher, &mut probe)
            .iter()
            .all(|changed| !changed));

        // 设备回来且没变：不报切换。
        *device.borrow_mut() = Some("coreaudio:builtin".to_string());
        assert!(advance(&mut watcher, &mut probe)
            .iter()
            .all(|changed| !changed));

        // 真正换成 AirPods 才报。
        *device.borrow_mut() = Some("coreaudio:airpods".to_string());
        assert_eq!(
            advance(&mut watcher, &mut probe)
                .iter()
                .filter(|changed| **changed)
                .count(),
            1
        );
    }

    #[test]
    fn watchdog_reports_stalls_but_not_pauses_or_normal_progress() {
        let now = std::time::Instant::now();
        let mut watchdog = OutputWatchdog::new(now);
        assert!(!watchdog.stalled(0.0, true, now + std::time::Duration::from_secs(14)));
        assert!(watchdog.stalled(0.0, true, now + std::time::Duration::from_secs(15)));
        assert!(!watchdog.stalled(0.0, false, now + std::time::Duration::from_secs(100)));
        assert!(!watchdog.stalled(0.0, true, now + std::time::Duration::from_secs(101)));
        assert!(!watchdog.stalled(2.0, true, now + std::time::Duration::from_secs(115)));
        assert!(!watchdog.stalled(3.0, true, now + std::time::Duration::from_secs(129)));
    }

    #[test]
    fn saved_volume_and_explicit_override_are_sanitized() {
        assert_eq!(initial_volume(0.37, None), 0.37);
        assert_eq!(initial_volume(0.0, None), 0.0);
        assert_eq!(initial_volume(0.37, Some("0")), 0.0);
        assert_eq!(initial_volume(0.37, Some("NaN")), 0.37);
        assert_eq!(initial_volume(0.37, Some("bad")), 0.37);
        assert_eq!(initial_volume(3.0, None), 1.0);
    }

    #[test]
    fn audio_thread_acknowledges_failed_loads_and_accepts_retries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.m4a");
        std::fs::write(&path, b"not an audio file").unwrap();
        let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_opens = opens.clone();
        let engine = PlaybackEngine::with_output(
            0.0,
            move |_| {
                worker_opens.fetch_add(1, Ordering::SeqCst);
                Err(rodio::DeviceSinkError::NoDevice)
            },
            || None,
        );
        for _ in 0..2 {
            let cached = CachedTrack {
                path: path.clone(),
                quality: "test".into(),
                lease: crate::cache::CacheLease::acquire(&path),
            };
            let seq = engine.load(
                TrackItem {
                    id: "invalid".into(),
                    ..Default::default()
                },
                cached,
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while engine.snapshot().load_seq != seq && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let snap = engine.snapshot();
            assert_eq!(snap.load_seq, seq);
            assert!(!snap.error.is_empty());
            assert!(!snap.playing && !snap.finished);
        }
        // 启动一次、两次装载各重试一次，失败没有把设备恢复永久锁死。
        assert_eq!(opens.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn disconnected_worker_acknowledges_loads_and_releases_their_leases() {
        let (tx, rx) = channel();
        drop(rx);
        let engine = PlaybackEngine {
            tx,
            snapshot: Arc::new(Mutex::new(PlaybackSnapshot::default())),
            next_load: AtomicU64::new(1),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached.m4a");
        std::fs::write(&path, b"cached").unwrap();
        let cached = CachedTrack {
            lease: crate::cache::CacheLease::acquire(&path),
            path,
            quality: "test".into(),
        };
        let seq = engine.load(
            TrackItem {
                id: "failed".into(),
                ..Default::default()
            },
            cached,
        );
        let snap = engine.snapshot();
        assert_eq!(snap.load_seq, seq);
        assert_eq!(snap.track_id, "failed");
        assert!(!snap.playing && !snap.finished && !snap.error.is_empty());
        assert_eq!(crate::cache::clear_in(dir.path()), 1);
    }

    #[test]
    fn load_failure_acknowledges_the_request_and_clears_finished() {
        let state = Arc::new(Mutex::new(PlaybackSnapshot {
            finished: true,
            playing: true,
            ..Default::default()
        }));
        let track = TrackItem {
            id: "failed-track".into(),
            ..Default::default()
        };
        load_failed(&state, &track, 42, "device unavailable".into());
        let snap = state.lock().unwrap();
        assert_eq!(snap.load_seq, 42);
        assert_eq!(snap.track_id, track.id);
        assert!(!snap.playing && !snap.finished);
        assert!(!snap.error.is_empty());
    }

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
        // 确定性模拟没有音频设备：不依赖 runner 恰好未安装声卡。
        let engine =
            PlaybackEngine::with_output(1.0, |_| Err(rodio::DeviceSinkError::NoDevice), || None);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while engine.snapshot().error.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let snap = engine.snapshot();
        assert_eq!(snap.volume, 1.0);
        assert!(!snap.playing);
        assert!(!snap.error.is_empty());
    }

    #[test]
    fn dir_usage_and_count_extension_classify_files_correctly() {
        let dir = std::env::temp_dir().join(format!("sodam-cache-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("original")).expect("mkdir");
        std::fs::write(dir.join("a.m4a"), [0u8; 100]).expect("m4a");
        std::fs::write(dir.join("a.quality"), b"lossless\t100").expect("sidecar");
        std::fs::write(dir.join("b.part"), [0u8; 50]).expect("part");
        std::fs::write(dir.join("original/full.img"), [0u8; 25]).expect("cover");

        // 字节：递归含子目录，全部文件都算（100 + 12 + 50 + 25）
        let (bytes, files) = dir_usage(&dir);
        assert_eq!(bytes, 187, "应含子目录与边车/part");
        assert_eq!(files, 4);
        // 歌曲数：只数 .m4a（边车/part 不算歌）
        assert_eq!(count_extension(&dir, "m4a"), 1);
        assert_eq!(count_extension(&dir, "img"), 0, "只看顶层");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
