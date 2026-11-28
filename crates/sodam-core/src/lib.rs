//! sodam 的领域层：配置、会话、队列与数据模型。
//!
//! 这里**不依赖 GPUI**，方便单独测试与复用（未来做 CLI/TUI 或守护进程都能直接用）。

pub mod audio;
pub mod cache;
pub mod cache_check;
pub mod color;
pub mod config;
pub mod config_watch;
pub mod contributors;
pub mod cover;
pub mod failure;
pub mod library;
pub mod log_outlet;
pub mod login;
pub mod mem;
pub mod models;
pub mod play_failure;
pub mod play_report;
pub mod playback_trace;
pub mod prefetch;
pub mod quality;
pub mod queue;
pub mod quit_gate;
pub mod session;
pub mod settings_writer;
pub mod upgrade;
pub mod volume_scrub;

pub use audio::{PlaybackEngine, PlaybackSnapshot};
pub use config::Settings;
pub use log_outlet::{flush_global, LogLevel, LogOutlet, LogRecord, LogTopic};
pub use models::{PlaylistItem, SceneItem, TrackItem};
pub use play_report::PlayReporter;
pub use queue::{PlayMode, Queue};
pub use session::Session;

/// 测试用：进程环境变量是全局的，同 crate 里改环境变量的用例共用这把锁，
/// 避免一个用例设/清变量时被并行跑的用例看到（#192）。
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

/// 测试用：把环境变量设到指定值，离开作用域时还原到设值前的状态——
/// 本机原本就设过这个变量的也不丢。与 [`env_test_lock`] 搭配：
/// 改环境变量的用例先拿锁再设值（#192）。
#[cfg(test)]
pub(crate) struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl EnvGuard {
    pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
