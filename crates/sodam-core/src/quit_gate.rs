//! 退出闸门：应用内主动退出时，先把启动期欠下的落盘做完再退出（#198）。
//!
//! GPUI 只给 `on_app_quit` 的等待约 200ms，慢盘 / 杀软扫描 / 写锁被占时可能写不完。
//! 接线在 `crates/sodam/src/app.rs` 的 `Root::request_quit`：应用内退出入口先问这里，
//! 拿到 [`QuitRequest::SaveThenQuit`] 就等在跑的落盘任务，写完了再真正退出。
//!
//! 这里保持纯状态机：不碰 GPUI、不碰文件，方便单测。

use std::time::Duration;

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

#[derive(Debug, Default)]
pub struct QuitGate {
    state: State,
}

impl QuitGate {
    /// 收到一次退出请求（托盘「退出」、macOS ⌘Q）。
    pub fn request(&mut self) -> QuitRequest {
        match self.state {
            State::Idle => {
                self.state = State::Saving;
                QuitRequest::SaveThenQuit
            }
            State::Saving | State::Done => QuitRequest::QuitNow,
        }
    }

    /// 落盘等待结束（写完，或到了上限）：之后再来的退出请求一律直接退出。
    pub fn finish(&mut self) {
        self.state = State::Done;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_request_waits_then_repeat_request_forces() {
        let mut gate = QuitGate::default();
        assert_eq!(gate.request(), QuitRequest::SaveThenQuit);
        // 用户等不及又点了一次：不再等，直接退。
        assert_eq!(gate.request(), QuitRequest::QuitNow);
        assert_eq!(gate.request(), QuitRequest::QuitNow);
    }

    #[test]
    fn finished_gate_never_waits_again() {
        let mut gate = QuitGate::default();
        assert_eq!(gate.request(), QuitRequest::SaveThenQuit);
        gate.finish();
        assert_eq!(gate.request(), QuitRequest::QuitNow);
    }
}
