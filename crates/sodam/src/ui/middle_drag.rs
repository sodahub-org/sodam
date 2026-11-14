//! 按住鼠标中键拖动页面时，只负责算「该滚到哪」。
//!
//! 这里不碰窗口，也不持有滚动句柄，全是普通数字，所以能直接单测。
//! 界面层把事件喂进来，再把算出的偏移写回滚动句柄。

use crate::app::Nav;

/// 起步死区：鼠标没挪够 2px 就不滚，免得中键轻点让页面抖一下。
const DEAD_ZONE: f32 = 2.0;

/// 按下时记下的起点。`top` 是内容离顶部的距离，0 表示已经在最顶上。
#[derive(Clone, Copy)]
struct Anchor {
    y: f32,
    top: f32,
    page: Nav,
}

/// 一次中键拖动。`None` 表示现在没在拖。
#[derive(Clone, Copy, Default)]
pub struct MiddleDrag(Option<Anchor>);

impl MiddleDrag {
    /// 按下中键。已经按着就忽略：重复按下不会把起点挪走，也不会叠加位移。
    pub fn begin(&mut self, y: f32, top: f32, page: Nav) {
        if self.0.is_none() {
            self.0 = Some(Anchor { y, top, page });
        }
    }

    /// 松开中键、切页、窗口失焦，都走这里收尾。
    pub fn finish(&mut self) {
        self.0 = None;
    }

    /// 页面换了或窗口失焦，当场结束；之后鼠标再动也不滚。
    pub fn sync_context(&mut self, page: Nav, window_active: bool) {
        if self
            .0
            .is_some_and(|anchor| anchor.page != page || !window_active)
        {
            self.0 = None;
        }
    }

    /// 鼠标移到 `y`，算出新的「离顶部距离」。
    /// 返回 `None` 就是这次不用滚：没在拖、还没过死区，或者上下文已经失效。
    pub fn move_to(&mut self, y: f32, max_top: f32, page: Nav, window_active: bool) -> Option<f32> {
        self.sync_context(page, window_active);
        let anchor = self.0?;
        let moved = y - anchor.y;
        if moved.abs() < DEAD_ZONE {
            return None;
        }
        // 往下拖，内容跟着往下走，离顶部就近了；往上拖反过来。
        Some((anchor.top - moved).clamp(0.0, max_top))
    }
}

#[cfg(test)]
mod tests {
    use super::MiddleDrag;
    use crate::app::Nav;

    const SETTINGS: Nav = Nav::Settings;
    const HOME: Nav = Nav::Home;

    fn dragging(at: f32, top: f32) -> MiddleDrag {
        let mut drag = MiddleDrag::default();
        drag.begin(at, top, SETTINGS);
        drag
    }

    /// 不到 2px 一动不动，正好 2px 就开始跟手。
    #[test]
    fn small_moves_do_not_scroll() {
        let mut drag = dragging(100.0, 50.0);
        assert_eq!(drag.move_to(101.0, 500.0, SETTINGS, true), None);
        assert_eq!(drag.move_to(99.5, 500.0, SETTINGS, true), None);
        assert_eq!(drag.move_to(102.0, 500.0, SETTINGS, true), Some(48.0));
        assert_eq!(drag.move_to(98.0, 500.0, SETTINGS, true), Some(52.0));
    }

    /// 鼠标移多少内容就移多少：往下拖内容往下走，往上拖内容往上走。
    #[test]
    fn pointer_moves_one_to_one() {
        let mut drag = dragging(100.0, 80.0);
        assert_eq!(drag.move_to(130.0, 500.0, SETTINGS, true), Some(50.0));
        assert_eq!(drag.move_to(70.0, 500.0, SETTINGS, true), Some(110.0));
    }

    /// 到顶就停住，不会滚出负的偏移。
    #[test]
    fn stops_at_the_top() {
        let mut drag = dragging(100.0, 10.0);
        assert_eq!(drag.move_to(200.0, 500.0, SETTINGS, true), Some(0.0));
    }

    /// 到底就停住，剩下的距离不会冒到外层去。
    #[test]
    fn stops_at_the_bottom() {
        let mut drag = dragging(100.0, 10.0);
        assert_eq!(drag.move_to(-400.0, 100.0, SETTINGS, true), Some(100.0));
    }

    /// 松开之后，再怎么移动都不滚。
    #[test]
    fn release_stops_the_drag() {
        let mut drag = dragging(100.0, 50.0);
        drag.finish();
        assert_eq!(drag.move_to(300.0, 500.0, SETTINGS, true), None);
    }

    /// 窗口失焦当场停，之后就算焦点回来也不接着滚。
    #[test]
    fn losing_focus_stops_the_drag() {
        let mut drag = dragging(100.0, 50.0);
        assert_eq!(drag.move_to(200.0, 500.0, SETTINGS, false), None);
        assert_eq!(drag.move_to(300.0, 500.0, SETTINGS, true), None);
    }

    /// 切页当场停，回到原来的页面也不接着滚。
    #[test]
    fn changing_page_stops_the_drag() {
        let mut drag = dragging(100.0, 50.0);
        assert_eq!(drag.move_to(200.0, 500.0, HOME, true), None);
        assert_eq!(drag.move_to(300.0, 500.0, SETTINGS, true), None);
    }

    /// 只切页或只失焦、鼠标一下没动，状态也会清掉。
    #[test]
    fn context_change_clears_without_mouse_move() {
        let mut drag = dragging(100.0, 50.0);
        drag.sync_context(HOME, true);
        assert_eq!(drag.move_to(300.0, 500.0, SETTINGS, true), None);

        let mut drag = dragging(100.0, 50.0);
        drag.sync_context(SETTINGS, false);
        assert_eq!(drag.move_to(300.0, 500.0, SETTINGS, true), None);

        let mut drag = dragging(100.0, 50.0);
        drag.sync_context(SETTINGS, true);
        assert_eq!(drag.move_to(130.0, 500.0, SETTINGS, true), Some(20.0));
    }

    /// 拖着的时候又按一次中键，起点不挪，位移也不叠。
    #[test]
    fn pressing_again_does_not_restart_or_stack() {
        let mut drag = dragging(100.0, 50.0);
        drag.begin(180.0, 10.0, SETTINGS);
        assert_eq!(drag.move_to(130.0, 500.0, SETTINGS, true), Some(20.0));
    }
}
