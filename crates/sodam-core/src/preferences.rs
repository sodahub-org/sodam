//! 播放偏好单写者：滑块拖动防抖，退出时刷尾；只合并偏好字段，不覆盖账号等新设置。

use crate::config::{Settings, SETTINGS_WRITE_LOCK};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy)]
pub struct PlaybackPreferences {
    volume: f32,
    volume_before_mute: f32,
    audio_cache_limit_mb: u64,
}

impl From<&Settings> for PlaybackPreferences {
    fn from(settings: &Settings) -> Self {
        Self {
            volume: settings.volume,
            volume_before_mute: settings.volume_before_mute,
            audio_cache_limit_mb: settings.audio_cache_limit_mb,
        }
    }
}

impl PlaybackPreferences {
    fn save_to(self, path: &Path) -> anyhow::Result<()> {
        let _guard = SETTINGS_WRITE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let mut settings = match std::fs::read(path) {
            // 字段级容错：单个字段类型坏不连累整份合并写（#185）。
            Ok(bytes) => crate::config::parse_settings_bytes(&bytes)
                .map_err(|error| anyhow::anyhow!("{error}"))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(err) => return Err(err.into()),
        };
        settings.volume = self.volume;
        settings.volume_before_mute = self.volume_before_mute;
        settings.audio_cache_limit_mb = self.audio_cache_limit_mb;
        settings.normalize_playback();
        settings.save_to_locked(path)
    }
}

enum Message {
    Update(PlaybackPreferences),
    Flush(futures_channel::oneshot::Sender<()>),
    Shutdown,
}

pub struct PreferencesWriter {
    tx: mpsc::Sender<Message>,
    thread: Option<std::thread::JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
    /// 强制退出：析构时不再等写线程（慢盘 / 写锁被占时 join 会把进程挂住，#198）。
    abandon_on_drop: AtomicBool,
}

impl PreferencesWriter {
    pub fn new(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let error = Arc::new(Mutex::new(None));
        let worker_error = error.clone();
        let spawned = std::thread::Builder::new()
            .name("sodam-preferences".into())
            .spawn(move || {
                let mut pending = None;
                loop {
                    let message = if pending.is_some() {
                        rx.recv_timeout(Duration::from_millis(300))
                    } else {
                        rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    };
                    let (quitting, acknowledge) = match message {
                        Ok(Message::Update(value)) => {
                            pending = Some(value);
                            continue;
                        }
                        Ok(Message::Flush(acknowledge)) => (false, Some(acknowledge)),
                        Ok(Message::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            (true, None)
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => (false, None),
                    };
                    if let Some(value) = pending.take() {
                        if let Err(err) = value.save_to(&path) {
                            if let Ok(mut error) = worker_error.lock() {
                                *error = Some(err.to_string());
                            }
                        }
                    }
                    if let Some(acknowledge) = acknowledge {
                        let _ = acknowledge.send(());
                    }
                    if quitting {
                        break;
                    }
                }
            });
        let thread = match spawned {
            Ok(thread) => Some(thread),
            Err(err) => {
                if let Ok(mut error) = error.lock() {
                    *error = Some(err.to_string());
                }
                None
            }
        };
        Self {
            tx,
            thread,
            error,
            abandon_on_drop: AtomicBool::new(false),
        }
    }

    /// 强制退出（3 秒超时，或用户在等待期间再点一次退出）时调用：
    /// 析构不再等写线程，避免它卡在慢 IO / 配置写锁上把进程拖住（#198）。
    ///
    /// 正常退出不走这里：`flush()` 已经等到刷尾，析构的 join 是兜底。
    pub fn abandon_on_drop(&self) {
        self.abandon_on_drop.store(true, Ordering::Relaxed);
    }

    pub fn schedule(&self, settings: &Settings) {
        let _ = self.tx.send(Message::Update(settings.into()));
    }

    /// 应用退出前立即刷尾；等待回执不占 UI/GPUI 后台线程池。
    pub fn flush(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let (tx, rx) = futures_channel::oneshot::channel();
        let _ = self.tx.send(Message::Flush(tx));
        async move {
            let _ = rx.await;
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok()?.take()
    }
}

impl Drop for PreferencesWriter {
    fn drop(&mut self) {
        // 仅在 Root/应用销毁时等待最后一次本地写盘，正常调音量从不等磁盘。
        let _ = self.tx.send(Message::Shutdown);
        if self.abandon_on_drop.load(Ordering::Relaxed) {
            // 强制退出：写线程可能卡在慢 IO 或配置写锁上，丢下它，进程退出会带走。
            let _ = self.thread.take();
            return;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_flushes_last_value_without_overwriting_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut settings = Settings::default();
        settings.save_to(&path).unwrap();
        let writer = PreferencesWriter::new(path.clone());
        for volume in [0.9, 0.3, 0.37] {
            settings.set_volume(volume);
            writer.schedule(&settings);
        }
        // 模拟调音量后另一个设置操作保存了语言/音质。
        let mut other = Settings::load_from(&path);
        other.language = "en".into();
        other.quality = "medium".into();
        other.save_to(&path).unwrap();
        drop(writer);
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.volume, 0.37);
        assert_eq!(loaded.volume_before_mute, 0.37);
        assert_eq!(loaded.language, "en");
        assert_eq!(loaded.quality, "medium");
    }

    #[test]
    fn explicit_flush_writes_before_writer_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let writer = PreferencesWriter::new(path.clone());
        let mut settings = Settings::default();
        settings.set_volume(0.42);
        writer.schedule(&settings);
        let (tx, mut rx) = futures_channel::oneshot::channel();
        assert!(writer.tx.send(Message::Flush(tx)).is_ok());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if rx.try_recv().unwrap().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "flush timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(Settings::load_from(&path).volume, 0.42);
    }

    #[test]
    fn muted_restart_restores_previous_level_and_cache_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let writer = PreferencesWriter::new(path.clone());
        let mut settings = Settings::default();
        settings.set_volume(0.24);
        settings.toggle_mute();
        settings.audio_cache_limit_mb = 20 * 1024;
        writer.schedule(&settings);
        drop(writer);
        let mut loaded = Settings::load_from(&path);
        assert_eq!(loaded.volume, 0.0);
        assert_eq!(loaded.audio_cache_limit_mb, 20 * 1024);
        loaded.toggle_mute();
        assert_eq!(loaded.volume, 0.24);
    }
}
