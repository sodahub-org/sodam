//! 退出闸门：应用内主动退出时，先把启动期欠下的落盘做完再退出（#198）。
//!
//! GPUI 只给 `on_app_quit` 的等待约 200ms，慢盘 / 杀软扫描 / 写锁被占时可能写不完。
//! 接线在 `crates/sodam/src/app.rs` 的 `Root::request_quit`：应用内退出入口先问这里，
//! 拿到 [`QuitRequest::SaveThenQuit`] 就等在跑的落盘任务，写完了再真正退出。
//!
//! 这里保持纯状态机：不碰 GPUI、不碰文件、时间由调用方传入，方便单测。

use std::time::{Duration, Instant};

/// 等落盘的上限：到点强制退出，别让用户以为程序卡死。
pub const QUIT_PERSIST_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Idle,
    Saving,
    Done,
}

/// 一次退出请求该怎么处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitRequest {
    /// 第一次请求：转成「正在保存」，等落盘任务完成后再真正退出。
    SaveThenQuit,
    /// 已经在保存中，或这次退出的等待已经结束：不再等，立刻退出。
    QuitNow,
}

#[derive(Debug)]
pub struct QuitGate {
    state: State,
    timeout: Duration,
    /// 只在 `Saving` 态有意义：到这个时刻还没等到写完就强制退出。
    deadline: Option<Instant>,
}

impl QuitGate {
    pub fn new(timeout: Duration) -> Self {
        Self {
            state: State::Idle,
            timeout,
            deadline: None,
        }
    }

    /// 收到一次退出请求（托盘「退出」、macOS ⌘Q）。
    pub fn request(&mut self, now: Instant) -> QuitRequest {
        match self.state {
            State::Idle => {
                self.state = State::Saving;
                self.deadline = Some(now + self.timeout);
                QuitRequest::SaveThenQuit
            }
            State::Saving | State::Done => QuitRequest::QuitNow,
        }
    }

    /// 落盘等完了：之后不再等，退出请求一律直接放行。
    pub fn complete(&mut self) {
        self.state = State::Done;
        self.deadline = None;
    }

    /// 到点还没等完就返回 true，同时把闸门关到「不再等」。
    ///
    /// 只在 `Saving` 态有意义：调用方在等待循环里按拍调用。
    pub fn timed_out(&mut self, now: Instant) -> bool {
        match (self.state, self.deadline) {
            (State::Saving, Some(deadline)) if now >= deadline => {
                self.complete();
                true
            }
            _ => false,
        }
    }

    /// 是否正在等落盘。等待期间状态行别再被别的提示顶掉。
    pub fn is_saving(&self) -> bool {
        self.state == State::Saving
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> QuitGate {
        QuitGate::new(QUIT_PERSIST_TIMEOUT)
    }

    #[test]
    fn first_request_waits_then_repeat_request_forces() {
        let mut gate = gate();
        let now = Instant::now();
        assert_eq!(gate.request(now), QuitRequest::SaveThenQuit);
        // 用户等不及又点了一次：不再等，直接退。
        assert_eq!(gate.request(now), QuitRequest::QuitNow);
        assert_eq!(gate.request(now), QuitRequest::QuitNow);
    }

    #[test]
    fn completed_gate_never_waits_again() {
        let mut gate = gate();
        let now = Instant::now();
        assert_eq!(gate.request(now), QuitRequest::SaveThenQuit);
        gate.complete();
        assert_eq!(gate.request(now), QuitRequest::QuitNow);
    }

    #[test]
    fn waiting_is_not_a_timeout_before_deadline() {
        let mut gate = gate();
        let now = Instant::now();
        gate.request(now);
        assert!(gate.is_saving());
        assert!(!gate.timed_out(now));
        assert!(gate.is_saving());
    }

    #[test]
    fn timeout_closes_the_gate() {
        let mut gate = QuitGate::new(Duration::from_millis(50));
        let now = Instant::now();
        gate.request(now);
        assert!(gate.timed_out(now + Duration::from_millis(50)));
        assert!(!gate.is_saving());
        assert_eq!(gate.request(now), QuitRequest::QuitNow);
        // 已经超时退过一次，再问也不算超时。
        assert!(!gate.timed_out(now + Duration::from_secs(10)));
    }
}
