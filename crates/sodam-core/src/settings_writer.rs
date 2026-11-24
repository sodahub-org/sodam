//! 设置写队列：界面上所有保存排进一条后台线程，退出只等一次 `flush()`。
//!
//! 界面线程只改内存里的 `Settings`，改完把整份快照丢进来；线程攒到 300ms 没有新改动
//! 才落盘，所以连着拖音量、连着点几个开关都只写一次盘。三个例外都走同一条队列，
//! 只是排的不是「快照」：
//!
//! * 启动期欠下的回存（隔离坏文件 / 迁移回存 / 刷备份）；
//! * 登出摘 Cookie（结果要交回界面，所以带一个回执）；
//! * 退出前的刷尾（`flush()`）。
//!
//! 扫码登录成功那一次故意不排队：界面线程当场写盘，否则「登录成功后三秒内退出」
//! 会丢 Cookie（#197）。

use crate::config::{Settings, StartupWriteBack};
use crate::log_outlet::{log_event, LogLevel, LogTopic};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// 一批改动的静默窗口：这么久没有新改动就落盘。
const IDLE_FLUSH_AFTER: Duration = Duration::from_millis(300);

/// 失败来自哪条路：界面排队的那份、启动期欠下的回存，还是队列已经没了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 界面改动排队后的整份落盘。
    Settings,
    /// 启动期欠下的那次回存。
    Startup,
    /// 写线程已经没了，这一笔根本没排进去。
    QueueStopped,
}

impl FailureKind {
    /// 写日志用的上下文说法。
    pub fn context(self) -> &'static str {
        match self {
            Self::Settings => "保存设置失败",
            Self::Startup => "启动配置回存失败",
            Self::QueueStopped => "保存队列已经停了",
        }
    }
}

/// 一次落盘失败：原文留给日志，状态行取走一次就清掉（不每拍重复报）。
#[derive(Debug, Clone)]
pub struct WriteFailure {
    pub kind: FailureKind,
    pub detail: String,
}

/// 登出落盘的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignOut {
    /// 盘上的 Cookie 已经清掉（或本来就是空的）。
    Done,
    /// 盘上已经是另一个身份：这次什么都不改。
    IdentityChanged,
}

/// 真正落盘的那一步。测试里换成记账的假实现，不碰磁盘。
///
/// 只在 crate 内用：外部只认 [`SettingsWriter`]，不该知道落盘是怎么做的。
pub(crate) trait SettingsSink: Send + 'static {
    /// 整份快照落盘，Cookie 沿用盘上那份。
    fn write(&mut self, snapshot: Settings) -> Result<(), String>;
    /// 登出：按盘上最新配置摘掉 Cookie。
    fn sign_out(&mut self, fallback: &Settings, expected_cookie: &str) -> Result<SignOut, String>;
    /// 启动期欠下的回存。
    fn persist_startup(&mut self, work: StartupWriteBack) -> Result<(), String>;
}

/// 真落盘：同一个配置文件、同一把写锁、同目录原子替换。
struct FileSink {
    path: PathBuf,
}

impl SettingsSink for FileSink {
    fn write(&mut self, snapshot: Settings) -> Result<(), String> {
        snapshot
            .save_queued_to(&self.path)
            .map_err(|err| err.to_string())
    }

    fn sign_out(&mut self, fallback: &Settings, expected_cookie: &str) -> Result<SignOut, String> {
        fallback
            .save_signed_out_to(&self.path, expected_cookie)
            .map(|done| {
                if done {
                    SignOut::Done
                } else {
                    SignOut::IdentityChanged
                }
            })
            .map_err(|err| err.to_string())
    }

    fn persist_startup(&mut self, work: StartupWriteBack) -> Result<(), String> {
        work.persist()
    }
}

enum Message {
    /// 整份设置快照：同一批只写最后一份。
    Snapshot(Settings),
    /// 启动期欠下的回存，只跑一次。
    Startup(StartupWriteBack),
    /// 登出：摘 Cookie，结果交回界面。
    SignOut {
        fallback: Settings,
        expected_cookie: String,
        reply: futures_channel::oneshot::Sender<Result<SignOut, String>>,
    },
    /// 退出前刷尾：写到这一拍为止的东西全部落盘后再回执。
    Flush(futures_channel::oneshot::Sender<()>),
    Shutdown,
}

pub struct SettingsWriter {
    tx: mpsc::Sender<Message>,
    thread: Option<std::thread::JoinHandle<()>>,
    failure: Arc<Mutex<Option<WriteFailure>>>,
    /// 强制退出：析构时不再等写线程（慢盘 / 写锁被占时 join 会把进程挂住，#198）。
    abandon_on_drop: AtomicBool,
    /// 只给日志去重：队列停了以后日志只记第一笔（提示每次都照塞）。
    dead_logged: AtomicBool,
}

impl SettingsWriter {
    pub fn new(path: PathBuf) -> Self {
        Self::spawn(Box::new(FileSink { path }), IDLE_FLUSH_AFTER)
    }

    /// 攒多久算「这一批改完了」，真跑用 [`IDLE_FLUSH_AFTER`]，测试里缩短。
    fn spawn(sink: Box<dyn SettingsSink>, window: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let failure = Arc::new(Mutex::new(None));
        let worker_failure = failure.clone();
        let dead_logged = AtomicBool::new(false);
        let spawned = std::thread::Builder::new()
            .name("sodam-settings".into())
            .spawn(move || run(sink, rx, worker_failure, window));
        let thread = match spawned {
            Ok(thread) => Some(thread),
            Err(err) => {
                // 线程起不来 = 队列从头就是死的。原因（内存紧张 / 线程数打满）写进日志；
                // 失败槽只带 kind，界面按 kind 给用户一句人话，不看 detail。
                let detail = err.to_string();
                log_event(
                    LogLevel::Error,
                    LogTopic::App,
                    "thread_spawn_failed",
                    &[("thread", "sodam-settings"), ("detail", detail.as_str())],
                );
                dead_logged.store(true, Ordering::Relaxed);
                if let Ok(mut slot) = failure.lock() {
                    *slot = Some(WriteFailure {
                        kind: FailureKind::QueueStopped,
                        detail,
                    });
                }
                None
            }
        };
        Self {
            tx,
            thread,
            failure,
            abandon_on_drop: AtomicBool::new(false),
            dead_logged,
        }
    }

    /// 排一份整份快照；300ms 内又改了只写最后那份（后到的那份盖掉先到的）。
    pub fn schedule(&self, settings: &Settings) {
        if self.tx.send(Message::Snapshot(settings.clone())).is_err() {
            self.note_dead_queue();
        }
    }

    /// 启动期欠下的回存，排在已经到达的改动之后执行。
    pub fn persist_startup(&self, work: StartupWriteBack) {
        if self.tx.send(Message::Startup(work)).is_err() {
            self.note_dead_queue();
        }
    }

    /// 写线程已经没了：这一笔排不进去，也不能装作没事。
    ///
    /// 记一行日志，再往失败槽里塞一条——下一拍心跳会把提示写进状态行，
    /// 用户重启时不会突然发现刚才的改动白改了。
    ///
    /// 队列死了就再也起不来：日志只记第一笔，免得连点设置刷一串一样的行；
    /// 但提示每次都要塞——心跳取走一条就清空，不塞用户后面就永远看不到「没存上」。
    fn note_dead_queue(&self) {
        let detail = "保存队列已经停了".to_string();
        if !self.dead_logged.swap(true, Ordering::Relaxed) {
            log_event(
                LogLevel::Warn,
                LogTopic::App,
                "settings_queue_stopped",
                &[("detail", detail.as_str())],
            );
        }
        if let Ok(mut slot) = self.failure.lock() {
            *slot = Some(WriteFailure {
                kind: FailureKind::QueueStopped,
                detail,
            });
        }
    }

    /// 登出：排进队列摘 Cookie，写完把结果交回界面。
    ///
    /// 自己开线程也能写，但那样退出时又得多等一个来源；排队则一次 `flush()` 全等完。
    pub fn sign_out(
        &self,
        fallback: Settings,
        expected_cookie: String,
    ) -> impl Future<Output = Result<SignOut, String>> + Send + 'static {
        let (reply, answer) = futures_channel::oneshot::channel();
        let sent = self
            .tx
            .send(Message::SignOut {
                fallback,
                expected_cookie,
                reply,
            })
            .is_ok();
        async move {
            if !sent {
                return Err("保存队列已经停了".to_string());
            }
            answer
                .await
                .unwrap_or_else(|_| Err("保存队列已经停了".to_string()))
        }
    }

    /// 退出前刷干净：返回时队列里排着的东西都已经落盘。
    ///
    /// 写线程已经没了（起线程失败或已退出）就直接返回——没有地方可写，也不该把退出卡住。
    pub fn flush(&self) -> impl Future<Output = ()> + Send + 'static {
        let (reply, answer) = futures_channel::oneshot::channel();
        let _ = self.tx.send(Message::Flush(reply));
        async move {
            let _ = answer.await;
        }
    }

    /// 取走最近一次失败；同一个失败只交出来一次。
    pub fn take_error(&self) -> Option<WriteFailure> {
        self.failure.lock().ok()?.take()
    }

    /// 强制退出（3 秒超时，或用户在等待期间再点一次退出）时调用：
    /// 析构不再等写线程，避免它卡在慢 IO / 配置写锁上把进程拖住（#198）。
    ///
    /// 正常退出不走这里：`flush()` 已经等到刷尾，析构的 join 是兜底。
    pub fn abandon_on_drop(&self) {
        self.abandon_on_drop.store(true, Ordering::Relaxed);
    }
}

impl Drop for SettingsWriter {
    fn drop(&mut self) {
        // 只在 Root / 应用销毁时等最后一次落盘，平时改设置从不等磁盘。
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

/// 写线程：攒改动、按到达顺序跑活、把失败记下来。
fn run(
    mut sink: Box<dyn SettingsSink>,
    rx: mpsc::Receiver<Message>,
    failure: Arc<Mutex<Option<WriteFailure>>>,
    window: Duration,
) {
    let mut pending: Option<Settings> = None;
    // 已经登出过的那个 Cookie。盘上读不出来时 `save_queued_to` 只能按快照自己那份写，
    // 这一笔记号保证登出前拍的旧快照连这条路也带不回旧身份。
    let mut signed_out: Option<String> = None;
    loop {
        // 手上有攒着的改动就只等一个静默窗口；没有就一直睡到下一桩活。
        let message = if pending.is_some() {
            rx.recv_timeout(window)
        } else {
            rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        let mut acknowledge: Option<futures_channel::oneshot::Sender<()>> = None;
        let mut job: Option<Message> = None;
        let mut quitting = false;
        match message {
            Ok(Message::Snapshot(mut snapshot)) => {
                if signed_out.as_deref() == Some(snapshot.cookie.as_str()) {
                    // 登出前排下的那份：把这个 Cookie 摘掉再写。
                    snapshot.cookie.clear();
                } else if signed_out.is_some() && !snapshot.cookie.trim().is_empty() {
                    // 换了身份（重新登录）：记号作废。
                    signed_out = None;
                }
                // 同批只留最后一份：拖音量拖了二十拍就是一次盘。
                pending = Some(snapshot);
                continue;
            }
            Ok(Message::Flush(reply)) => acknowledge = Some(reply),
            Ok(job_message @ (Message::Startup(_) | Message::SignOut { .. })) => {
                job = Some(job_message)
            }
            Ok(Message::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => quitting = true,
            // 静默窗口到点：把攒着的那份写掉。
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // 先写攒着的，再跑插进来的活：一律按到达顺序落盘。
        if let Some(snapshot) = pending.take() {
            record(&failure, FailureKind::Settings, sink.write(snapshot));
        }
        match job {
            Some(Message::Startup(work)) => {
                record(&failure, FailureKind::Startup, sink.persist_startup(work))
            }
            Some(Message::SignOut {
                fallback,
                expected_cookie,
                reply,
            }) => {
                let outcome = sink.sign_out(&fallback, &expected_cookie);
                if matches!(&outcome, Ok(SignOut::Done)) {
                    signed_out = Some(expected_cookie.clone());
                }
                if let Err(detail) = &outcome {
                    // 登出失败由界面自己提示，这里只留一行日志。
                    log_event(
                        LogLevel::Warn,
                        LogTopic::App,
                        "settings_sign_out_failed",
                        &[("detail", detail.as_str())],
                    );
                }
                let _ = reply.send(outcome);
            }
            _ => {}
        }
        if let Some(reply) = acknowledge {
            let _ = reply.send(());
        }
        if quitting {
            break;
        }
    }
}

/// 记一次失败：日志留原文，状态行那份留着给下一拍取走。
fn record(failure: &Mutex<Option<WriteFailure>>, kind: FailureKind, outcome: Result<(), String>) {
    let Err(detail) = outcome else {
        return;
    };
    log_event(
        LogLevel::Warn,
        LogTopic::App,
        "settings_write_failed",
        &[("context", kind.context()), ("detail", detail.as_str())],
    );
    if let Ok(mut slot) = failure.lock() {
        *slot = Some(WriteFailure { kind, detail });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll};

    /// 记账用的假落盘：写了几次、写的哪一份、成功还是失败，全由测试摆布。
    #[derive(Default)]
    struct Recording {
        /// 落盘顺序：`write` / `startup` / `sign_out` 按调用先后排。
        ops: Vec<&'static str>,
        writes: Vec<Settings>,
        sign_outs: Vec<String>,
        startups: usize,
    }

    struct FakeSink {
        log: Arc<Mutex<Recording>>,
        /// 每写一次先从这里收一个信号：拿它把「写完了」和「flush 回来了」错开。
        gate: Option<mpsc::Receiver<()>>,
        fail: bool,
    }

    impl SettingsSink for FakeSink {
        fn write(&mut self, snapshot: Settings) -> Result<(), String> {
            if let Some(gate) = self.gate.as_ref() {
                let _ = gate.recv();
            }
            if self.fail {
                return Err("磁盘只读".to_string());
            }
            let mut log = self.log.lock().unwrap();
            log.ops.push("write");
            log.writes.push(snapshot);
            Ok(())
        }

        fn sign_out(
            &mut self,
            _fallback: &Settings,
            expected_cookie: &str,
        ) -> Result<SignOut, String> {
            if self.fail {
                return Err("磁盘只读".to_string());
            }
            let mut log = self.log.lock().unwrap();
            log.ops.push("sign_out");
            log.sign_outs.push(expected_cookie.to_string());
            Ok(SignOut::Done)
        }

        fn persist_startup(&mut self, _work: StartupWriteBack) -> Result<(), String> {
            if self.fail {
                return Err("磁盘只读".to_string());
            }
            let mut log = self.log.lock().unwrap();
            log.ops.push("startup");
            log.startups += 1;
            Ok(())
        }
    }

    fn writer_with(sink: FakeSink) -> (SettingsWriter, Arc<Mutex<Recording>>) {
        let log = sink.log.clone();
        (SettingsWriter::spawn(Box::new(sink), IDLE_FLUSH_AFTER), log)
    }

    /// 静默窗口调短：防抖那几条用例不用真等 300ms，也不容易在慢机器上抖。
    fn writer_with_window(
        sink: FakeSink,
        window: Duration,
    ) -> (SettingsWriter, Arc<Mutex<Recording>>) {
        let log = sink.log.clone();
        (SettingsWriter::spawn(Box::new(sink), window), log)
    }

    fn recording(fail: bool) -> FakeSink {
        FakeSink {
            log: Arc::new(Mutex::new(Recording::default())),
            gate: None,
            fail,
        }
    }

    /// 没有异步运行时：测试里自己把 future 转起来（只用来等一次性回执）。
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }

    fn settings_with_volume(volume: f32) -> Settings {
        let mut settings = Settings::default();
        settings.set_volume(volume);
        settings
    }

    #[test]
    fn several_changes_land_on_disk_once() {
        let (writer, log) = writer_with(recording(false));
        for volume in [0.9, 0.8, 0.7, 0.6, 0.35] {
            writer.schedule(&settings_with_volume(volume));
        }
        block_on(writer.flush());
        let log = log.lock().unwrap();
        assert_eq!(log.writes.len(), 1, "同一批改动只写一次盘");
        assert!(
            (log.writes[0].volume - 0.35).abs() < f32::EPSILON,
            "写的是最后那份"
        );
    }

    #[test]
    fn changes_after_the_last_write_start_a_new_batch() {
        let (writer, log) = writer_with(recording(false));
        writer.schedule(&settings_with_volume(0.5));
        block_on(writer.flush());
        writer.schedule(&settings_with_volume(0.2));
        block_on(writer.flush());
        let log = log.lock().unwrap();
        assert_eq!(log.writes.len(), 2);
        assert!(
            (log.writes[0].volume - 0.5).abs() < f32::EPSILON,
            "第一批写的是当时那份"
        );
        assert!(
            (log.writes[1].volume - 0.2).abs() < f32::EPSILON,
            "第二批写的是新那份"
        );
    }

    #[test]
    fn flush_returns_only_after_the_write_is_done() {
        let (gate_tx, gate_rx) = mpsc::channel();
        let sink = FakeSink {
            log: Arc::new(Mutex::new(Recording::default())),
            gate: Some(gate_rx),
            fail: false,
        };
        let (writer, log) = writer_with(sink);
        writer.schedule(&settings_with_volume(0.4));
        let mut cx = Context::from_waker(std::task::Waker::noop());
        let mut flush = std::pin::pin!(writer.flush());
        // 写盘被门挡着：刷尾不许回来。
        for _ in 0..20 {
            assert!(
                matches!(flush.as_mut().poll(&mut cx), Poll::Pending),
                "写盘还没落完，flush 就回来了"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(log.lock().unwrap().writes.is_empty());
        gate_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if flush.as_mut().poll(&mut cx).is_ready() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "放行之后 flush 没回来"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(log.lock().unwrap().writes.len(), 1);
    }

    #[test]
    fn a_failed_write_is_reported_once() {
        let (writer, _log) = writer_with(recording(true));
        writer.schedule(&settings_with_volume(0.3));
        block_on(writer.flush());
        let failure = writer.take_error().expect("失败要能拿到");
        assert_eq!(failure.kind, FailureKind::Settings);
        assert!(failure.detail.contains("只读"));
        assert!(writer.take_error().is_none(), "同一个失败只报一次");
    }

    #[test]
    fn startup_write_back_failure_has_its_own_kind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let work = crate::config::load_for_startup(&path)
            .write_back
            .expect("首启要回存");
        let (writer, log) = writer_with(recording(true));
        writer.persist_startup(work);
        block_on(writer.flush());
        let failure = writer.take_error().expect("失败要能拿到");
        assert_eq!(failure.kind, FailureKind::Startup);
        assert_eq!(log.lock().unwrap().startups, 0);
    }

    #[test]
    fn startup_write_back_runs_after_queued_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let work = crate::config::load_for_startup(&path)
            .write_back
            .expect("首启要回存");
        let (writer, log) = writer_with(recording(false));
        writer.schedule(&settings_with_volume(0.6));
        writer.persist_startup(work);
        block_on(writer.flush());
        let log = log.lock().unwrap();
        assert_eq!(log.ops, vec!["write", "startup"], "按到达顺序落盘");
        assert_eq!(log.startups, 1);
    }

    /// 登出排在它之前攒下的那批改动后面：先写改动，再摘 Cookie。
    #[test]
    fn sign_out_runs_after_the_batch_it_follows() {
        let (writer, log) = writer_with(recording(false));
        writer.schedule(&settings_with_volume(0.8));
        let outcome = block_on(writer.sign_out(Settings::default(), "old-cookie".into()));
        assert_eq!(outcome, Ok(SignOut::Done));
        assert_eq!(log.lock().unwrap().ops, vec!["write", "sign_out"]);
    }

    /// 防抖：改完不立刻写，等静默窗口过去才落盘；窗口里又来一份，写的是后到的那份。
    #[test]
    fn changes_wait_for_the_idle_window() {
        let (writer, log) = writer_with_window(recording(false), Duration::from_millis(400));
        writer.schedule(&settings_with_volume(0.7));
        std::thread::sleep(Duration::from_millis(80));
        assert!(
            log.lock().unwrap().writes.is_empty(),
            "刚改完就落盘，没等静默窗口"
        );
        writer.schedule(&settings_with_volume(0.2));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if !log.lock().unwrap().writes.is_empty() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "静默窗口过了还没落盘");
            std::thread::sleep(Duration::from_millis(10));
        }
        let log = log.lock().unwrap();
        assert!(
            (log.writes.last().unwrap().volume - 0.2).abs() < f32::EPSILON,
            "写的是最后那份"
        );
    }

    #[test]
    fn sign_out_goes_through_the_queue_and_reports_back() {
        let (writer, log) = writer_with(recording(false));
        let outcome = block_on(writer.sign_out(Settings::default(), "old-cookie".into()));
        assert_eq!(outcome, Ok(SignOut::Done));
        assert_eq!(
            log.lock().unwrap().sign_outs,
            vec!["old-cookie".to_string()]
        );
    }

    /// 盘上文件被外部删掉时，排队落盘只能按快照自己那份写：登出过的 Cookie 也不许回来。
    #[test]
    fn signed_out_cookie_stays_gone_even_when_the_file_disappears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut signed_in = Settings {
            cookie: "sessionid_ss=old".into(),
            ..Settings::default()
        };
        signed_in.save_to(&path).unwrap();

        let writer = SettingsWriter::new(path.clone());
        assert_eq!(
            block_on(writer.sign_out(signed_in.clone(), "sessionid_ss=old".into())),
            Ok(SignOut::Done)
        );
        // 外部把文件删了：盘上读不出 Cookie，`save_queued_to` 只能按快照写。
        std::fs::remove_file(&path).unwrap();
        signed_in.set_volume(0.4);
        writer.schedule(&signed_in);
        block_on(writer.flush());

        let raw = std::fs::read_to_string(&path).unwrap();
        let reloaded: Settings = serde_json::from_str(&raw).unwrap();
        assert_eq!(reloaded.cookie, "", "登出过的 Cookie 不许被旧快照写回来");
        assert!((reloaded.volume - 0.4).abs() < f32::EPSILON, "别的字段照写");
    }

    /// 换了身份（重新登录）之后记号作废：同一串旧 Cookie 再来不再被摘。
    ///
    /// 真正挡住「旧 Cookie 写回盘」的是盘上那份（`save_queued_to`）；这里只保证记号
    /// 不会一直留着，误伤同一个账号重新登录后拿到的同一串 Cookie。
    #[test]
    fn a_new_identity_drops_the_signed_out_mark() {
        let (writer, log) = writer_with(recording(false));
        assert_eq!(
            block_on(writer.sign_out(Settings::default(), "sessionid_ss=old".into())),
            Ok(SignOut::Done)
        );
        writer.schedule(&Settings {
            cookie: "sessionid_ss=new".into(),
            ..Settings::default()
        });
        writer.schedule(&Settings {
            cookie: "sessionid_ss=old".into(),
            ..Settings::default()
        });
        block_on(writer.flush());
        let log = log.lock().unwrap();
        let last = log.writes.last().expect("该写一份");
        assert_eq!(last.cookie, "sessionid_ss=old", "记号作废后不再摘 Cookie");
    }

    #[test]
    fn sign_out_failure_keeps_the_reason() {
        let (writer, _log) = writer_with(recording(true));
        let outcome = block_on(writer.sign_out(Settings::default(), "old-cookie".into()));
        assert!(outcome.is_err());
        assert!(
            writer.take_error().is_none(),
            "登出失败由界面自己提示，不进状态行队列"
        );
    }
}
