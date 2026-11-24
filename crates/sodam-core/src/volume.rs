//! 音量滑条的拖动记账：拖的时候每秒改几十次，不该每次都整份落盘。
//!
//! 纯状态机，不碰 GPUI、不碰时钟：时刻由调用方喂进来，测试里给定时刻就能断言。
//!
//! 收尾有两个来源：松手事件（`on_mouse_up` / `on_mouse_up_out`），以及心跳里的
//! 「停手够久」。只要松手事件可能丢，两者就得都在——拖到窗口外松手、拖动中切走窗口，
//! 这两种情况下 `up` 都收不到。

use std::time::{Duration, Instant};

/// 停手这么久就算这次拖动结束了。
pub const IDLE: Duration = Duration::from_millis(600);

#[derive(Debug)]
pub struct VolumeScrub {
    /// 拖动着就是 `Some(最后一次改音量的时刻)`。
    pending: Option<Instant>,
    idle: Duration,
}

impl Default for VolumeScrub {
    fn default() -> Self {
        Self::new(IDLE)
    }
}

impl VolumeScrub {
    pub fn new(idle: Duration) -> Self {
        Self {
            pending: None,
            idle,
        }
    }

    /// 按下滑条：从现在起算拖动中。
    pub fn begin(&mut self, now: Instant) {
        self.pending = Some(now);
    }

    /// 拖动中改了一次音量。返回 `true` 表示「正在拖，这次先别落盘」。
    pub fn touch(&mut self, now: Instant) -> bool {
        match self.pending {
            Some(_) => {
                self.pending = Some(now);
                true
            }
            None => false,
        }
    }

    /// 现在还在拖吗（外部热重载要靠它决定别回弹音量）。
    pub fn scrubbing(&self) -> bool {
        self.pending.is_some()
    }

    /// 停手够久了吗：心跳每拍问一次，够了就收尾。
    pub fn settled(&self, now: Instant) -> bool {
        self.pending
            .is_some_and(|at| now.saturating_duration_since(at) >= self.idle)
    }

    /// 收尾。返回 `true` 表示「这次拖动有值没落盘，补一次」。
    ///
    /// 重复调用只会第一次返回 `true`。
    pub fn finish(&mut self) -> bool {
        self.pending.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_while_dragging_does_not_ask_for_a_write() {
        let mut scrub = VolumeScrub::new(IDLE);
        let start = Instant::now();
        scrub.begin(start);
        assert!(
            scrub.touch(start + Duration::from_millis(10)),
            "拖动中不落盘"
        );
        assert!(scrub.scrubbing());
    }

    #[test]
    fn not_dragging_writes_right_away() {
        let mut scrub = VolumeScrub::new(IDLE);
        assert!(!scrub.touch(Instant::now()), "没在拖，照常落盘");
        assert!(!scrub.scrubbing());
    }

    #[test]
    fn idle_after_release_ends_the_drag_and_asks_for_one_write() {
        let mut scrub = VolumeScrub::new(IDLE);
        let start = Instant::now();
        scrub.begin(start);
        assert!(
            !scrub.settled(start + Duration::from_millis(100)),
            "才 100ms"
        );
        assert!(scrub.settled(start + IDLE), "停够久了该收尾");
        assert!(scrub.finish(), "这次拖动要补一次落盘");
        assert!(!scrub.finish(), "同一个拖动只补一次");
        assert!(!scrub.scrubbing(), "收尾后不再算拖动中");
    }

    #[test]
    fn touching_postpones_the_idle_end() {
        let mut scrub = VolumeScrub::new(IDLE);
        let start = Instant::now();
        scrub.begin(start);
        let later = start + Duration::from_millis(500);
        assert!(scrub.touch(later), "仍在拖");
        assert!(
            !scrub.settled(later + Duration::from_millis(300)),
            "最后一次动是 300ms 前，还不算停手"
        );
        assert!(scrub.settled(later + IDLE));
    }

    #[test]
    fn release_before_the_idle_window_still_ends_the_drag() {
        let mut scrub = VolumeScrub::new(IDLE);
        scrub.begin(Instant::now());
        assert!(scrub.finish(), "松手了就立刻收尾，不等停手窗口");
        assert!(!scrub.scrubbing());
    }
}
