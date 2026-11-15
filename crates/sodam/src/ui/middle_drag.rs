//! 按住鼠标中键拖动页面时，只负责算「该滚到哪」。
//!
//! 这里不碰窗口，也不持有滚动句柄，全是普通数字，所以能直接单测。
//! 界面层把事件喂进来，再把算出的偏移写回滚动句柄。

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

impl DragTarget {
    /// 这次拖动的目标是不是变高虚拟列表（`ListState`）。
    ///
    /// 列表只认增量、而且「最多能滚多远」会随测量变大，所以写回和到边
    /// 处理都跟普通滚动句柄分开走。
    pub fn is_list(self) -> bool {
        matches!(
            self,
            DragTarget::Scenes
                | DragTarget::LikedDetail
                | DragTarget::LibraryDetail
                | DragTarget::SearchPlaylistDetail
                | DragTarget::ArtistDetail
                | DragTarget::AlbumDetail
        )
    }
}

/// 起步死区：鼠标没挪够 2px 就不滚，免得中键轻点让页面抖一下。
const DEAD_ZONE: f32 = 2.0;

/// 按下时记下的起点。`top` 是内容离顶部的距离，0 表示已经在最顶上。
#[derive(Clone, Copy)]
struct Anchor {
    y: f32,
    top: f32,
    page: Nav,
    /// 这次拖动绑在哪个滚动区（设置页 / 队列抽屉 / 歌词页），写回时按它分流。
    target: DragTarget,
    /// 已经挪够 2px、开始滚了。起步之后不再判死区，否则拖回起点会卡住。
    started: bool,
    /// 上一次算出的「想去哪」，没有按当前测量上限夹过。
    /// 变高虚拟列表的已测量高度会越滚越大，夹掉的这段距离靠它补回来。
    wanted: f32,
}

/// 一次中键拖动。`None` 表示现在没在拖。
#[derive(Clone, Copy, Default)]
pub struct MiddleDrag(Option<Anchor>);

impl MiddleDrag {
    /// 按下中键。已经按着就忽略：重复按下不会把起点挪走，也不会叠加位移。
    pub fn begin(&mut self, y: f32, top: f32, page: Nav, target: DragTarget) {
        if self.0.is_none() {
            self.0 = Some(Anchor {
                y,
                top,
                page,
                target,
                started: false,
                wanted: top,
            });
        }
    }

    /// 起步前把锚点里「离顶部距离」换成此刻的真实值。
    ///
    /// 按下之后、真正挪过死区之前，列表可能自己动了（歌词页的跟唱补间就是这样）。
    /// 起步前的移动都会重定一次，拖动就从眼前的画面开始，不会把这段时间的位移一并抹掉。
    /// 已经起步就不动锚点：中途再改会让拖动一路漂移；页面换了或窗口失焦也直接清掉，
    /// 不往失效的锚点里写值。
    pub fn rebase_unstarted_top(&mut self, top: f32, page: Nav, window_active: bool) {
        self.sync_context(page, window_active);
        if let Some(mut anchor) = self.0 {
            if !anchor.started {
                anchor.top = top;
                anchor.wanted = top;
                self.0 = Some(anchor);
            }
        }
    }

    /// 当前拖动绑在哪个滚动区；没在拖就是 `None`。
    /// `move_to` 只负责算偏移，真正写回交给上层按这个目标分流。
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

    /// 鼠标移到 `y`，算出新的「离顶部距离」。
    /// 返回 `None` 就是这次不用滚：没在拖、还没过死区，或者上下文已经失效。
    pub fn move_to(&mut self, y: f32, max_top: f32, page: Nav, window_active: bool) -> Option<f32> {
        self.sync_context(page, window_active);
        let mut anchor = self.0?;
        let moved = y - anchor.y;
        // 死区只在起步前判一次。起步之后哪怕拖回按下点，也要继续 1:1 跟手。
        if !anchor.started {
            if moved.abs() < DEAD_ZONE {
                return None;
            }
            anchor.started = true;
            self.0 = Some(anchor);
        }
        // 往下拖，内容跟着往下走，离顶部就近了；往上拖反过来。
        let wanted = (anchor.top - moved).max(0.0);
        anchor.wanted = wanted;
        self.0 = Some(anchor);
        Some(wanted.min(max_top))
    }

    /// 上一次想去的目标位置，没有按当前测量上限夹过。
    /// 变高虚拟列表补欠账时用它算还差多少；没在拖就是 `None`。
    pub fn wanted_top(&self) -> Option<f32> {
        self.0.map(|anchor| anchor.wanted)
    }

    /// 变高虚拟列表用：把「想去哪」换成「这一帧该滚多少」。
    /// 返回 `None` 的条件和 [`Self::move_to`] 一样。
    pub fn move_by(
        &mut self,
        y: f32,
        current_top: f32,
        max_top: f32,
        page: Nav,
        window_active: bool,
    ) -> Option<f32> {
        let target_top = self.move_to(y, max_top, page, window_active)?;
        Some(target_top - current_top)
    }
}

#[cfg(test)]
mod tests {
    use super::{DragTarget, MiddleDrag};
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

    /// 不到 2px 一动不动，正好 2px 就开始跟手。
    #[test]
    fn small_moves_do_not_scroll() {
        let mut drag = dragging(100.0, 50.0);
        assert_eq!(drag.move_to(101.0, 500.0, SETTINGS, true), None);
        assert_eq!(drag.move_to(99.5, 500.0, SETTINGS, true), None);
        assert_eq!(drag.move_to(102.0, 500.0, SETTINGS, true), Some(48.0));
        assert_eq!(drag.move_to(98.0, 500.0, SETTINGS, true), Some(52.0));
    }

    /// 起步之后往回拖也跟手，不会再被 2px 死区拦住。
    #[test]
    fn moving_back_after_start_still_follows_the_pointer() {
        let mut drag = dragging(100.0, 50.0);
        assert_eq!(drag.move_to(130.0, 500.0, SETTINGS, true), Some(20.0));
        // 拖回按下点：必须给出 50，不能因为「总位移不到 2px」又停下。
        assert_eq!(drag.move_to(100.0, 500.0, SETTINGS, true), Some(50.0));
        assert_eq!(drag.move_to(101.0, 500.0, SETTINGS, true), Some(49.0));
        assert_eq!(drag.move_to(99.0, 500.0, SETTINGS, true), Some(51.0));
        // 越过起点继续往上拖也一样跟手。
        assert_eq!(drag.move_to(80.0, 500.0, SETTINGS, true), Some(70.0));
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
        drag.begin(180.0, 10.0, SETTINGS, DragTarget::Settings);
        assert_eq!(drag.move_to(130.0, 500.0, SETTINGS, true), Some(20.0));
    }

    /// 按下后列表自己动了（歌词页跟唱补间）：起步前重定锚点，
    /// 首帧从眼前的位置算，不会把这段位移当成拖动。
    #[test]
    fn rebase_before_start_uses_latest_top() {
        let mut drag = lyrics_dragging(100.0, 50.0);
        drag.rebase_unstarted_top(118.0, LYRICS, true);
        assert_eq!(drag.move_to(108.0, 500.0, LYRICS, true), Some(110.0));
    }

    /// 死区里的小移动不滚；锚点在每次移动前被刷新到最新位置，
    /// 真正越过死区时用最后那个值。
    #[test]
    fn rebase_after_dead_zone_move_uses_latest_top() {
        let mut drag = lyrics_dragging(100.0, 50.0);
        assert_eq!(drag.move_to(101.0, 500.0, LYRICS, true), None);
        drag.rebase_unstarted_top(62.0, LYRICS, true);
        assert_eq!(drag.move_to(103.0, 500.0, LYRICS, true), Some(59.0));
    }

    /// 已经起步后重定无效：拖动中途再改锚点，位移仍按起步那一刻算。
    #[test]
    fn rebase_after_start_is_ignored() {
        let mut drag = lyrics_dragging(100.0, 50.0);
        assert_eq!(drag.move_to(108.0, 500.0, LYRICS, true), Some(42.0));
        drag.rebase_unstarted_top(200.0, LYRICS, true);
        assert_eq!(drag.move_to(116.0, 500.0, LYRICS, true), Some(34.0));
    }

    /// 页面换了或窗口失焦：重定会先把失效的拖动清掉，不往旧锚点里写值。
    #[test]
    fn rebase_after_context_change_is_ignored() {
        let mut drag = lyrics_dragging(100.0, 50.0);
        drag.rebase_unstarted_top(118.0, HOME, true);
        assert_eq!(drag.move_to(108.0, 500.0, HOME, true), None);

        let mut drag = lyrics_dragging(100.0, 50.0);
        drag.rebase_unstarted_top(118.0, LYRICS, false);
        assert_eq!(drag.move_to(108.0, 500.0, LYRICS, true), None);
    }

    /// 歌词页按下时上报的是歌词区，上层靠它把偏移写回 `lyrics_scroll`；
    /// 松手之后目标也要跟着清掉。
    #[test]
    fn lyrics_drag_reports_its_target() {
        let mut drag = lyrics_dragging(100.0, 40.0);
        assert_eq!(drag.target(), Some(DragTarget::Lyrics));
        assert_eq!(drag.move_to(130.0, 500.0, LYRICS, true), Some(10.0));
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
            assert_eq!(drag.move_to(130.0, 500.0, page, true), Some(20.0));
        }
    }

    /// 变高列表按增量写回：目标减当前位置，就是这一帧该滚的距离。
    #[test]
    fn move_by_returns_the_step_from_the_current_offset() {
        let mut drag = dragging(100.0, 50.0);
        assert_eq!(
            drag.move_by(130.0, 50.0, 500.0, SETTINGS, true),
            Some(-30.0)
        );
        // 列表已经走到 20；再往下拖 10px，只该滚 10。
        assert_eq!(
            drag.move_by(140.0, 20.0, 500.0, SETTINGS, true),
            Some(-10.0)
        );
    }

    /// 被测量上限夹掉时，`wanted_top` 仍记着真正想去的位置；
    /// 后面内容量出来了，下一帧还能把欠的这段补上。
    #[test]
    fn clamped_drag_remembers_where_it_wanted_to_go() {
        let mut drag = dragging(100.0, 10.0);
        // 往上拖 200px：想去 210，但眼下只量到 100。
        assert_eq!(drag.move_to(-100.0, 100.0, SETTINGS, true), Some(100.0));
        assert_eq!(drag.wanted_top(), Some(210.0));
        // 下一帧量出更多内容，上限变 300：这一帧该滚 110，正好补完。
        assert_eq!(
            drag.move_by(-100.0, 100.0, 300.0, SETTINGS, true),
            Some(110.0)
        );
        assert_eq!(drag.wanted_top(), Some(210.0));
    }

    /// 一次大幅拖动被截短后，上限变大时几帧之内能补到想去的位置。
    #[test]
    fn clamped_drag_catches_up_across_frames() {
        let mut drag = dragging(100.0, 10.0);
        // 第一帧：想去 110，但只量到 100，先滚 90，差 10 记着。
        assert_eq!(drag.move_by(0.0, 10.0, 100.0, SETTINGS, true), Some(90.0));
        // 第二帧：内容量多了，上限 120；把差的 10 补上。
        assert_eq!(drag.move_by(0.0, 100.0, 120.0, SETTINGS, true), Some(10.0));
        // 第三帧：已经到想去的位置，不再动。
        assert_eq!(drag.move_by(0.0, 110.0, 120.0, SETTINGS, true), Some(0.0));
        assert_eq!(drag.wanted_top(), Some(110.0));
    }

    /// 六个变高列表能被认出来；普通滚动句柄不算。
    #[test]
    fn list_targets_are_marked() {
        let lists = [
            DragTarget::Scenes,
            DragTarget::LikedDetail,
            DragTarget::LibraryDetail,
            DragTarget::SearchPlaylistDetail,
            DragTarget::ArtistDetail,
            DragTarget::AlbumDetail,
        ];
        for target in lists {
            assert!(target.is_list(), "{target:?}");
        }
        for target in [
            DragTarget::Settings,
            DragTarget::Queue,
            DragTarget::Lyrics,
            DragTarget::SearchTracks,
            DragTarget::PlaylistGrid,
        ] {
            assert!(!target.is_list(), "{target:?}");
        }
    }
}
