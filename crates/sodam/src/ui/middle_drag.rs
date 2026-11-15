//! 按住鼠标中键拖动页面时，只负责算「这一帧该滚到哪」。
//!
//! 这里不碰窗口，也不持有滚动句柄，全是普通数字，所以能直接单测。
//! 界面层把事件喂进来，再把算出的偏移写回滚动句柄。
//!
//! 语义是「距离驱动的持续自动滚动」（#182，浏览器 / VS Code 手感）：
//! 按住后指针离按下点越远滚得越快（见 [`scroll_speed`]），指针停住也继续滚，
//! 松手立即停，没有惯性。速度只跟距离有关，跟拖动速度、松手动量都无关；
//! 内容位置由界面层的帧循环每帧按 `速度 × dt` 积分，指针不再直接决定位置。

use crate::app::Nav;

/// 这次中键拖动绑在哪个滚动区。只用来决定偏移最终写回哪个句柄，
/// 不做任何「写不了就退而求其次」的回退分支。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragTarget {
    /// 设置页（用 `settings_scroll`）。
    Settings,
    /// 队列抽屉（用 `queue_scroll` 内部的 `base_handle`）。
    Queue,
    /// 搜索页「综合」分组列表（用 `search_groups_scroll`）。
    SearchGroups,
    /// 搜索页「歌曲」列表（用 `search_tracks_scroll` 内部的 `base_handle`）。
    SearchTracks,
    /// 搜索页「音乐人」列表（用 `search_artists_scroll`）。
    SearchArtists,
    /// 搜索页「专辑」列表（用 `search_albums_scroll`）。
    SearchAlbums,
    /// 搜索页「歌单」列表（用 `search_playlists_scroll`）。
    SearchPlaylists,
    /// 我的歌单卡片网格（用 `playlist_grid_scroll`）。
    PlaylistGrid,
    /// 歌词页（用 `lyrics_scroll`）。
    Lyrics,
    /// 听歌模式页（用 `scenes_list`，变高虚拟列表）。
    Scenes,
    /// 我喜欢的音乐详情（用 `liked_detail_list`，变高虚拟列表）。
    LikedDetail,
    /// 我的歌单详情（用 `library_detail_list`，变高虚拟列表）。
    LibraryDetail,
    /// 搜索歌单详情（用 `search_playlist_detail_list`，变高虚拟列表）。
    SearchPlaylistDetail,
    /// 音乐人详情（用 `artist_detail_list`，变高虚拟列表）。
    ArtistDetail,
    /// 专辑详情（用 `album_detail_list`，变高虚拟列表）。
    AlbumDetail,
}

/// 起步死区：鼠标没挪够 2px 就不滚，免得中键轻点让页面抖一下。
const DEAD_ZONE: f32 = 2.0;

/// 速度曲线系数：按 Chromium 中键自动滚动的形状取值。
/// 速度 = [`SPEED_COEFF`] × 距离^[`SPEED_EXPONENT`]（px/ms）——近处极慢，
/// 拖远了按幂次急剧变快。要调手感先改这组常量（#182）。
const SPEED_COEFF: f32 = 0.000_008;

/// 速度曲线的指数，Chromium 用 2.2。
const SPEED_EXPONENT: f32 = 2.2;

/// 速度封顶（px/ms，约每秒 8000px）：拖得再远也不超过它。
/// 不封顶的话指针一靠近窗口边缘就会快到失控；要调手感先改这里。
const MAX_SPEED: f32 = 8.0;

/// 指针离锚点的距离到滚动速度的换算曲线，单位 px/ms。
///
/// 按 Chromium 的形状：`速度 = SPEED_COEFF × 距离^2.2`，在 [`MAX_SPEED`] 处封顶。
/// 整条曲线连续、在封顶前严格单调增，0 处为 0——死区附近速度小到接近不动
/// （2px 处每秒不到 0.1px），拖远了急剧变快，到顶后不再变快。
fn scroll_speed(distance: f32) -> f32 {
    (SPEED_COEFF * distance.max(0.0).powf(SPEED_EXPONENT)).min(MAX_SPEED)
}

/// 按下时记下的起点。`y` 是速度的参照点：指针离它越远滚得越快，
/// 参照点本身不动；`wanted` 从按下时的内容位置起步，起步前可能被重定。
#[derive(Clone, Copy)]
struct Anchor {
    y: f32,
    page: Nav,
    /// 这次拖动绑在哪个滚动区（设置页 / 队列抽屉 / 歌词页），写回时按它分流。
    target: DragTarget,
    /// 指针此刻的位置。速度只看它离 `y` 多远；指针停住也继续滚。
    pointer: f32,
    /// 已经挪够 2px、自动滚动起步了。起步之后不再判死区，往回挪只变慢不清零。
    started: bool,
    /// 目前想滚到的位置：帧循环每帧按 `速度 × dt` 积分，并按上限夹住。
    /// 变高列表量到哪滚到哪，不攒欠账、不越界。
    wanted: f32,
}

/// 一次中键拖动。`None` 表示现在没在拖。
#[derive(Clone, Copy, Default)]
pub struct MiddleDrag(Option<Anchor>);

impl MiddleDrag {
    /// 按下中键。已经按着就忽略：重复按下不会挪参照点，也不会叠加速度。
    pub fn begin(&mut self, y: f32, top: f32, page: Nav, target: DragTarget) {
        if self.0.is_none() {
            self.0 = Some(Anchor {
                y,
                page,
                target,
                pointer: y,
                started: false,
                wanted: top,
            });
        }
    }

    /// 起步前把锚点里「想滚到的位置」换成此刻的真实值。
    ///
    /// 按下之后、真正挪过死区之前，列表可能自己动了（歌词页的跟唱补间就是这样）。
    /// 起步前的移动都会重定一次，自动滚动就从眼前的画面开始，不会把这段时间
    /// 的位移一并抹掉。已经起步就不动锚点：积分不能被中途改起点；
    /// 页面换了或窗口失焦也直接清掉，不往失效的锚点里写值。
    pub fn rebase_unstarted_top(&mut self, top: f32, page: Nav, window_active: bool) {
        self.sync_context(page, window_active);
        if let Some(mut anchor) = self.0 {
            if !anchor.started {
                anchor.wanted = top;
                self.0 = Some(anchor);
            }
        }
    }

    /// 当前拖动绑在哪个滚动区；没在拖就是 `None`。
    /// 帧循环只负责积分，真正写回交给上层按这个目标分流。
    pub fn target(&self) -> Option<DragTarget> {
        self.0.map(|anchor| anchor.target)
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

    /// 自动滚动是否已经起步。帧循环只在起步后跑：中键刚按下、还没挪出
    /// 死区时不烧帧。
    pub fn is_scrolling(&self) -> bool {
        self.0.is_some_and(|anchor| anchor.started)
    }

    /// 鼠标移动时更新指针位置，返回是否**刚刚**起步（这次移动越过了死区）。
    ///
    /// 距离驱动语义下指针只决定速度，不直接决定位置；位置由 [`Self::advance`]
    /// 在帧循环里每帧积分。页面换了或窗口失焦当场结束，之后鼠标再动也不滚。
    pub fn update_pointer(&mut self, y: f32, page: Nav, window_active: bool) -> bool {
        self.sync_context(page, window_active);
        let mut just_started = false;
        if let Some(mut anchor) = self.0 {
            anchor.pointer = y;
            // 死区只在起步前判一次。起步之后往回挪到参照点附近只是变慢，
            // 不会再被死区拦住。
            if !anchor.started && (y - anchor.y).abs() >= DEAD_ZONE {
                anchor.started = true;
                just_started = true;
            }
            self.0 = Some(anchor);
        }
        just_started
    }

    /// 帧循环推进一次：按当前距离算速度，乘上帧长，积分出这一帧的目标位置。
    ///
    /// `dt_ms` 是距上一帧的毫秒数（gpui 的鼠标事件没有时间戳，由调用方取时钟）。
    /// 返回积分后的目标位置，调用方负责写回滚动句柄；返回 `None` 表示没在拖、
    /// 还没起步或上下文已失效，这一帧不用滚。
    ///
    /// 目标位置始终按 `max_top` 夹在 0 到上限之间：变高列表的测量上限会越滚越大，
    /// 夹住的那段不记账，上限长出来之后下一帧自然接着滚——量到哪滚到哪，
    /// 不会攒一笔欠账突然窜出去。
    pub fn advance(
        &mut self,
        dt_ms: f32,
        max_top: f32,
        page: Nav,
        window_active: bool,
    ) -> Option<f32> {
        self.sync_context(page, window_active);
        let mut anchor = self.0?;
        if !anchor.started {
            return None;
        }
        let moved = anchor.pointer - anchor.y;
        // 往下拖（指针在参照点下方），内容跟着往下走，离顶部就近了；往上拖反过来。
        let step = scroll_speed(moved.abs()) * dt_ms;
        let wanted = if moved >= 0.0 {
            anchor.wanted - step
        } else {
            anchor.wanted + step
        };
        let wanted = wanted.clamp(0.0, max_top);
        anchor.wanted = wanted;
        self.0 = Some(anchor);
        Some(wanted)
    }
}

#[cfg(test)]
mod tests {
    use super::{scroll_speed, DragTarget, MiddleDrag, MAX_SPEED};
    use crate::app::Nav;

    const SETTINGS: Nav = Nav::Settings;
    const HOME: Nav = Nav::Home;
    const LYRICS: Nav = Nav::Lyrics;

    fn dragging(at: f32, top: f32) -> MiddleDrag {
        let mut drag = MiddleDrag::default();
        drag.begin(at, top, SETTINGS, DragTarget::Settings);
        drag
    }

    fn lyrics_dragging(at: f32, top: f32) -> MiddleDrag {
        let mut drag = MiddleDrag::default();
        drag.begin(at, top, LYRICS, DragTarget::Lyrics);
        drag
    }

    /// 曲线在原点为 0，死区附近慢到几乎不动，不会一抖就飞。
    #[test]
    fn speed_starts_at_zero_and_is_tiny_near_the_dead_zone() {
        assert_eq!(scroll_speed(0.0), 0.0);
        assert!(
            scroll_speed(2.0) < 0.001,
            "speed(2) = {}",
            scroll_speed(2.0)
        );
        assert!(
            scroll_speed(10.0) < 1.0,
            "speed(10) = {}",
            scroll_speed(10.0)
        );
    }

    /// 距离越远滚得越快：封顶之前严格单调增。
    #[test]
    fn speed_is_strictly_monotonic_until_the_cap() {
        let mut prev = 0.0;
        for step in 1..=250 {
            let d = step as f32 * 2.0;
            let speed = scroll_speed(d);
            assert!(speed > prev, "d = {d}, speed = {speed}, prev = {prev}");
            prev = speed;
        }
    }

    /// 拖得再远也不超过封顶速度。
    #[test]
    fn speed_caps_at_the_maximum() {
        for d in [600.0_f32, 1000.0, 5000.0, 100000.0] {
            assert_eq!(scroll_speed(d), MAX_SPEED, "d = {d}");
        }
    }

    /// 曲线连续：相邻采样点之间速度不会跳变。
    #[test]
    fn speed_is_continuous() {
        let mut prev = scroll_speed(0.0);
        for step in 1..=2000 {
            let d = step as f32 * 0.5;
            let speed = scroll_speed(d);
            assert!(
                speed - prev < 0.1,
                "d = {d}, speed = {speed}, prev = {prev}"
            );
            prev = speed;
        }
    }

    /// 不到 2px 不起步；挪够 2px 就起步。
    #[test]
    fn dead_zone_gates_the_start() {
        let mut drag = dragging(100.0, 50.0);
        assert!(!drag.update_pointer(101.0, SETTINGS, true));
        assert!(!drag.is_scrolling());
        assert!(drag.update_pointer(102.0, SETTINGS, true));
        assert!(drag.is_scrolling());
    }

    /// 没起步（刚按下、还没挪出死区）帧循环推进不了任何东西。
    #[test]
    fn advance_before_start_does_nothing() {
        let mut drag = dragging(100.0, 50.0);
        drag.update_pointer(101.5, SETTINGS, true);
        assert_eq!(drag.advance(16.0, 500.0, SETTINGS, true), None);
    }

    /// 指针停在锚点下方：内容持续往顶走（往下拖，内容往下走），
    /// 每帧推进 速度 × 帧长；指针停住不动也继续滚。
    #[test]
    fn holding_the_pointer_below_the_anchor_scrolls_up_continuously() {
        let mut drag = dragging(100.0, 600.0);
        drag.update_pointer(200.0, SETTINGS, true);
        let first = drag.advance(16.0, 5000.0, SETTINGS, true).unwrap();
        let expected = 600.0 - scroll_speed(100.0) * 16.0;
        assert!((first - expected).abs() < 1e-3, "first = {first}");
        // 指针没动，第二帧接着往同一个方向走。
        let second = drag.advance(16.0, 5000.0, SETTINGS, true).unwrap();
        assert!(second < first, "second = {second}, first = {first}");
    }

    /// 指针停在锚点上方：方向相反，内容往底走。上下对称。
    #[test]
    fn holding_the_pointer_above_the_anchor_scrolls_down() {
        let mut drag = dragging(100.0, 600.0);
        drag.update_pointer(0.0, SETTINGS, true);
        let top = drag.advance(16.0, 5000.0, SETTINGS, true).unwrap();
        let expected = 600.0 + scroll_speed(100.0) * 16.0;
        assert!((top - expected).abs() < 1e-3, "top = {top}");
    }

    /// 离锚点越远每帧推进越多：同样的帧长，100px 处比 50px 处快。
    #[test]
    fn farther_pointer_moves_faster_per_frame() {
        let motion = |offset: f32| {
            let mut drag = dragging(100.0, 600.0);
            drag.update_pointer(offset, SETTINGS, true);
            600.0 - drag.advance(16.0, 5000.0, SETTINGS, true).unwrap()
        };
        assert!(motion(200.0) > motion(150.0));
    }

    /// 帧长翻倍推进也翻倍：位置是 速度 × 时间的积分。
    #[test]
    fn motion_scales_with_frame_time() {
        let motion = |dt: f32| {
            let mut drag = dragging(100.0, 600.0);
            drag.update_pointer(200.0, SETTINGS, true);
            600.0 - drag.advance(dt, 5000.0, SETTINGS, true).unwrap()
        };
        let one = motion(16.0);
        let two = motion(32.0);
        assert!((two - 2.0 * one).abs() < 1e-3, "one = {one}, two = {two}");
    }

    /// 到顶夹住，不滚出负的偏移；夹住后不攒欠账，继续推还是停在 0。
    #[test]
    fn clamped_at_the_top_and_no_debt() {
        let mut drag = dragging(100.0, 10.0);
        drag.update_pointer(200.0, SETTINGS, true);
        let first = drag.advance(16.0, 500.0, SETTINGS, true).unwrap();
        assert!(
            (first - (10.0 - scroll_speed(100.0) * 16.0)).abs() < 1e-3,
            "first = {first}"
        );
        // 长帧直接夹到 0；再推一帧还是 0，欠账没有越攒越多。
        assert_eq!(drag.advance(1000.0, 500.0, SETTINGS, true), Some(0.0));
        assert_eq!(drag.advance(16.0, 500.0, SETTINGS, true), Some(0.0));
    }

    /// 到底就停住，剩下的距离不会冒到外层去。
    #[test]
    fn stops_at_the_bottom() {
        let mut drag = dragging(100.0, 10.0);
        drag.update_pointer(0.0, SETTINGS, true);
        assert_eq!(drag.advance(1000.0, 100.0, SETTINGS, true), Some(100.0));
    }

    /// 到底（顶）之后指针往回挪，下一帧马上往回走，没有迟滞。
    #[test]
    fn reversing_direction_recovers_immediately() {
        let mut drag = dragging(100.0, 10.0);
        drag.update_pointer(200.0, SETTINGS, true);
        assert_eq!(drag.advance(1000.0, 500.0, SETTINGS, true), Some(0.0));
        drag.update_pointer(0.0, SETTINGS, true);
        let top = drag.advance(16.0, 500.0, SETTINGS, true).unwrap();
        assert!(top > 0.0, "top = {top}");
    }

    /// 变高列表：上限只按已测量高度算。夹在测量上限时不丢位置，
    /// 上限变大后下一帧接着往深处滚（#168 补欠账的等价行为）。
    #[test]
    fn growing_max_top_keeps_the_scroll_going() {
        let mut drag = dragging(100.0, 10.0);
        drag.update_pointer(0.0, SETTINGS, true);
        // 第一帧只量到 100，先滚到 100。
        assert_eq!(drag.advance(1000.0, 100.0, SETTINGS, true), Some(100.0));
        // 第二帧内容量到 300：接着滚。
        assert_eq!(drag.advance(1000.0, 300.0, SETTINGS, true), Some(300.0));
        // 上限到 500，最后这段补完。
        assert_eq!(drag.advance(1000.0, 500.0, SETTINGS, true), Some(500.0));
    }

    /// 松开之后，再怎么推进都不滚。
    #[test]
    fn release_stops_the_drag() {
        let mut drag = dragging(100.0, 50.0);
        drag.update_pointer(200.0, SETTINGS, true);
        drag.finish();
        assert_eq!(drag.advance(16.0, 500.0, SETTINGS, true), None);
    }

    /// 窗口失焦当场停，之后就算焦点回来也不接着滚。
    #[test]
    fn losing_focus_stops_the_drag() {
        let mut drag = dragging(100.0, 50.0);
        drag.update_pointer(200.0, SETTINGS, true);
        assert_eq!(drag.advance(16.0, 500.0, SETTINGS, false), None);
        assert_eq!(drag.advance(16.0, 500.0, SETTINGS, true), None);
    }

    /// 切页当场停，回到原来的页面也不接着滚。
    #[test]
    fn changing_page_stops_the_drag() {
        let mut drag = dragging(100.0, 50.0);
        drag.update_pointer(200.0, SETTINGS, true);
        assert_eq!(drag.advance(16.0, 500.0, HOME, true), None);
        assert_eq!(drag.advance(16.0, 500.0, SETTINGS, true), None);
    }

    /// 只切页或只失焦、鼠标一下没动，状态也会清掉。
    #[test]
    fn context_change_clears_without_mouse_move() {
        let mut drag = dragging(100.0, 50.0);
        drag.update_pointer(200.0, SETTINGS, true);
        drag.sync_context(HOME, true);
        assert_eq!(drag.advance(16.0, 500.0, SETTINGS, true), None);

        let mut drag = dragging(100.0, 50.0);
        drag.update_pointer(200.0, SETTINGS, true);
        drag.sync_context(SETTINGS, false);
        assert_eq!(drag.advance(16.0, 500.0, SETTINGS, true), None);

        let mut drag = dragging(100.0, 50.0);
        drag.update_pointer(200.0, SETTINGS, true);
        drag.sync_context(SETTINGS, true);
        assert!(drag.advance(16.0, 500.0, SETTINGS, true).is_some());
    }

    /// 拖着的时候又按一次中键，参照点不挪，速度也不叠。
    #[test]
    fn pressing_again_does_not_restart_or_stack() {
        let mut drag = dragging(100.0, 50.0);
        drag.update_pointer(200.0, SETTINGS, true);
        drag.begin(180.0, 10.0, SETTINGS, DragTarget::Settings);
        // 参照点还是 100，速度仍按 |200 - 100| 算。
        let top = drag.advance(16.0, 5000.0, SETTINGS, true).unwrap();
        let expected = 50.0 - scroll_speed(100.0) * 16.0;
        assert!((top - expected).abs() < 1e-3, "top = {top}");
    }

    /// 按下后列表自己动了（歌词页跟唱补间）：起步前重定锚点，
    /// 积分从眼前的位置开始，不会把这段位移当成拖动。
    #[test]
    fn rebase_before_start_uses_latest_top() {
        let mut drag = lyrics_dragging(100.0, 50.0);
        drag.rebase_unstarted_top(118.0, LYRICS, true);
        drag.update_pointer(200.0, LYRICS, true);
        let top = drag.advance(16.0, 5000.0, LYRICS, true).unwrap();
        let expected = 118.0 - scroll_speed(100.0) * 16.0;
        assert!((top - expected).abs() < 1e-3, "top = {top}");
    }

    /// 已经起步后重定无效：积分不被中途改起点。
    #[test]
    fn rebase_after_start_is_ignored() {
        let mut drag = lyrics_dragging(100.0, 50.0);
        drag.update_pointer(200.0, LYRICS, true);
        let first = drag.advance(16.0, 5000.0, LYRICS, true).unwrap();
        drag.rebase_unstarted_top(200.0, LYRICS, true);
        let second = drag.advance(16.0, 5000.0, LYRICS, true).unwrap();
        let expected = first - scroll_speed(100.0) * 16.0;
        assert!((second - expected).abs() < 1e-3, "second = {second}");
    }

    /// 页面换了或窗口失焦：重定会先把失效的拖动清掉，不往旧锚点里写值。
    #[test]
    fn rebase_after_context_change_is_ignored() {
        let mut drag = lyrics_dragging(100.0, 50.0);
        drag.rebase_unstarted_top(118.0, HOME, true);
        drag.update_pointer(200.0, HOME, true);
        assert_eq!(drag.advance(16.0, 5000.0, HOME, true), None);

        let mut drag = lyrics_dragging(100.0, 50.0);
        drag.rebase_unstarted_top(118.0, LYRICS, false);
        drag.update_pointer(200.0, LYRICS, false);
        assert_eq!(drag.advance(16.0, 5000.0, LYRICS, true), None);
    }

    /// 歌词页按下时上报的是歌词区，上层靠它把偏移写回 `lyrics_scroll`；
    /// 松手之后目标也要跟着清掉。
    #[test]
    fn lyrics_drag_reports_its_target() {
        let mut drag = lyrics_dragging(100.0, 40.0);
        assert_eq!(drag.target(), Some(DragTarget::Lyrics));
        drag.update_pointer(130.0, LYRICS, true);
        assert!(drag.advance(16.0, 500.0, LYRICS, true).is_some());
        drag.finish();
        assert_eq!(drag.target(), None);
    }

    /// 搜索页各处列表和歌单网格各报各的目标；按下之后就锁住，
    /// 别处再按一次中键也换不掉。
    #[test]
    fn area_targets_are_reported_and_locked() {
        let cases = [
            (Nav::Search, DragTarget::SearchGroups),
            (Nav::Search, DragTarget::SearchTracks),
            (Nav::Search, DragTarget::SearchArtists),
            (Nav::Search, DragTarget::SearchAlbums),
            (Nav::Search, DragTarget::SearchPlaylists),
            (Nav::Library, DragTarget::PlaylistGrid),
        ];
        for (page, target) in cases {
            let mut drag = MiddleDrag::default();
            drag.begin(100.0, 50.0, page, target);
            assert_eq!(drag.target(), Some(target));
            // 按住不放时在别处又点一次中键：目标还是最先按下那一个。
            drag.begin(180.0, 10.0, page, DragTarget::PlaylistGrid);
            assert_eq!(drag.target(), Some(target));
            drag.update_pointer(200.0, page, true);
            assert!(drag.advance(16.0, 500.0, page, true).is_some());
        }
    }
}
