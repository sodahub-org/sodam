//! 帧日志开关下的循环迭代、进 app 与引擎快照计数。
//! `SODAM_FRAME_LOG=1` 时每个循环每 60 秒输出一行 `loop_counts`。
//! `timer=shared:tray` 表示逻辑循环借用托盘拍，不额外产生定时器唤醒。
//! `iterations` 记录逻辑循环次数；真实唤醒总数只累加 `timer_wakes`。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sodam_core::log_outlet::{enabled, log_event, LogLevel, LogTopic};

const REPORT_AFTER: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
enum TimerSource {
    Independent,
    SharedTray,
}

impl TimerSource {
    fn label(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::SharedTray => "shared:tray",
        }
    }
}

pub(crate) struct LoopProbe {
    name: &'static str,
    timer: TimerSource,
    started: Instant,
    logging: bool,
    timer_wakes: u64,
    iterations: u64,
    app_entries: Arc<AtomicU64>,
    signal_app_entries: u64,
    engine_snapshots: u64,
}

impl LoopProbe {
    pub(crate) fn new(name: &'static str) -> Self {
        Self::with_source(name, TimerSource::Independent)
    }

    pub(crate) fn new_shared_tray(name: &'static str) -> Self {
        Self::with_source(name, TimerSource::SharedTray)
    }

    fn with_source(name: &'static str, timer: TimerSource) -> Self {
        Self {
            name,
            timer,
            started: Instant::now(),
            logging: false,
            timer_wakes: 0,
            iterations: 0,
            app_entries: Arc::new(AtomicU64::new(0)),
            signal_app_entries: 0,
            engine_snapshots: 0,
        }
    }

    pub(crate) fn tick(&mut self) {
        self.timer_wakes += 1;
        self.iterations += 1;
        self.report();
    }

    pub(crate) fn check(&mut self) {
        self.iterations += 1;
        self.report();
    }

    pub(crate) fn app_entry(&mut self) {
        self.app_entries.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn signal_app_entry(&mut self) {
        self.app_entry();
        self.signal_app_entries += 1;
    }

    pub(crate) fn app_entry_counter(&self) -> Arc<AtomicU64> {
        self.app_entries.clone()
    }

    pub(crate) fn engine_snapshot(&mut self) {
        self.engine_snapshots += 1;
    }

    fn report(&mut self) {
        if !enabled(LogLevel::Debug, LogTopic::App) {
            self.logging = false;
            self.app_entries.store(0, Ordering::Relaxed);
            self.clear_counts();
            return;
        }
        if !self.logging {
            self.logging = true;
            self.started = Instant::now();
            self.app_entries.store(0, Ordering::Relaxed);
            self.clear_counts();
            return;
        }
        let elapsed = self.started.elapsed();
        if elapsed < REPORT_AFTER {
            return;
        }

        let seconds = elapsed.as_secs_f64();
        let timer_wakes = self.timer_wakes.to_string();
        let timer_wakes_per_s = rate(self.timer_wakes, seconds);
        let iterations = self.iterations.to_string();
        let iterations_per_s = rate(self.iterations, seconds);
        let app_entries_count = self.app_entries.swap(0, Ordering::Relaxed);
        let app_entries = app_entries_count.to_string();
        let app_entries_per_s = rate(app_entries_count, seconds);
        let signal_app_entries = self.signal_app_entries.to_string();
        let signal_app_entries_per_s = rate(self.signal_app_entries, seconds);
        let engine_snapshots = self.engine_snapshots.to_string();
        let engine_snapshots_per_s = rate(self.engine_snapshots, seconds);
        log_event(
            LogLevel::Debug,
            LogTopic::App,
            "loop_counts",
            &[
                ("loop", self.name),
                ("timer", self.timer.label()),
                ("timer_wakes", timer_wakes.as_str()),
                ("timer_wakes_per_s", timer_wakes_per_s.as_str()),
                ("iterations", iterations.as_str()),
                ("iterations_per_s", iterations_per_s.as_str()),
                ("app_entries", app_entries.as_str()),
                ("app_entries_per_s", app_entries_per_s.as_str()),
                ("signal_app_entries", signal_app_entries.as_str()),
                (
                    "signal_app_entries_per_s",
                    signal_app_entries_per_s.as_str(),
                ),
                ("engine_snapshots", engine_snapshots.as_str()),
                ("engine_snapshots_per_s", engine_snapshots_per_s.as_str()),
            ],
        );
        self.started = Instant::now();
        self.clear_counts();
    }

    fn clear_counts(&mut self) {
        self.timer_wakes = 0;
        self.iterations = 0;
        self.signal_app_entries = 0;
        self.engine_snapshots = 0;
    }
}

fn rate(count: u64, seconds: f64) -> String {
    format!("{:.2}", count as f64 / seconds)
}
