//! sodam 的领域层：配置、会话、队列与数据模型。
//!
//! 这里**不依赖 GPUI**，方便单独测试与复用（未来做 CLI/TUI 或守护进程都能直接用）。

pub mod audio;
pub mod cache;
pub mod color;
pub mod config;
pub mod contributors;
pub mod failure;
pub mod library;
pub mod log_outlet;
pub mod login;
pub mod mem;
pub mod models;
pub mod play_report;
pub mod playback_trace;
pub mod preferences;
pub mod quality;
pub mod queue;
pub mod session;

pub use audio::{PlaybackEngine, PlaybackSnapshot};
pub use config::Settings;
pub use log_outlet::{flush_global, LogLevel, LogOutlet, LogRecord, LogTopic};
pub use models::{PlaylistItem, SceneItem, TrackItem};
pub use play_report::PlayReporter;
pub use queue::{PlayMode, Queue};
pub use session::Session;
