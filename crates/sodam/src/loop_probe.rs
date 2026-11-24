use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sodam_core::log_outlet::{enabled, log_event, LogLevel, LogTopic};

const REPORT_AFTER: Duration = Duration::from_secs(60);

pub(crate) struct LoopProbe {
    name: &'static str,
    started: Instant,
    logging: bool,
    wakes: u64,
    checks: u64,
    app_entries: Arc<AtomicU64>,
    idle_app_entries: u64,
    engine_snapshots: u64,
}

impl LoopProbe {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            started: Instant::now(),
            logging: false,
            wakes: 0,
            checks: 0,
            app_entries: Arc::new(AtomicU64::new(0)),
            idle_app_entries: 0,
            engine_snapshots: 0,
        }
    }

    pub(crate) fn tick(&mut self) {
        self.wakes += 1;
        self.report();
    }

    pub(crate) fn check(&mut self) {
        self.checks += 1;
        self.report();
    }

    pub(crate) fn app_entry(&mut self) {
        self.app_entries.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn idle_app_entry(&mut self) {
        self.app_entry();
        self.idle_app_entries += 1;
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
        let wakes = self.wakes.to_string();
        let wakes_per_s = rate(self.wakes, seconds);
        let checks = self.checks.to_string();
        let checks_per_s = rate(self.checks, seconds);
        let app_entries_count = self.app_entries.swap(0, Ordering::Relaxed);
        let app_entries = app_entries_count.to_string();
        let app_entries_per_s = rate(app_entries_count, seconds);
        let idle_app_entries = self.idle_app_entries.to_string();
        let idle_app_entries_per_s = rate(self.idle_app_entries, seconds);
        let engine_snapshots = self.engine_snapshots.to_string();
        let engine_snapshots_per_s = rate(self.engine_snapshots, seconds);
        log_event(
            LogLevel::Debug,
            LogTopic::App,
            "loop_counts",
            &[
                ("loop", self.name),
                ("wakes", wakes.as_str()),
                ("wakes_per_s", wakes_per_s.as_str()),
                ("checks", checks.as_str()),
                ("checks_per_s", checks_per_s.as_str()),
                ("app_entries", app_entries.as_str()),
                ("app_entries_per_s", app_entries_per_s.as_str()),
                ("idle_app_entries", idle_app_entries.as_str()),
                ("idle_app_entries_per_s", idle_app_entries_per_s.as_str()),
                ("engine_snapshots", engine_snapshots.as_str()),
                ("engine_snapshots_per_s", engine_snapshots_per_s.as_str()),
            ],
        );
        self.started = Instant::now();
        self.clear_counts();
    }

    fn clear_counts(&mut self) {
        self.wakes = 0;
        self.checks = 0;
        self.idle_app_entries = 0;
        self.engine_snapshots = 0;
    }
}

fn rate(count: u64, seconds: f64) -> String {
    format!("{:.2}", count as f64 / seconds)
}
