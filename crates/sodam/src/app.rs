//! 应用根视图：状态定义、初始化、输入处理与顶层渲染。

#[path = "app_actions.rs"]
mod actions;
#[path = "app_search.rs"]
mod search_actions;

use gpui::prelude::*;
use gpui::{
    div, px, Context, Decorations, EntityInputHandler, FocusHandle, Image, Render, UTF16Selection,
    Window,
};
use sodam_core::{
    library::{RecommendationSource, SceneCard, SearchScope},
    log_outlet::{self, log_event, LogLevel, LogTopic},
    login::{LoginStatus, QrMatrix},
    models::{
        AlbumDetail, ArtistDetail, LyricLine, PlaylistItem, SceneItem, SearchResults, TrackItem,
    },
    queue::Queue,
    session::{AccountInfo, Session},
    PlayReporter, PlaybackEngine, Settings,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 远程读取类失败的统一说法：读不到歌单、收藏、搜索、推荐都是这一句。
pub(crate) const REMOTE_TEXT: &str = "出现了一点问题，检查网络后重试。";
/// 本地保存/写入类失败的统一说法：这类失败和网络无关。
pub(crate) const SAVE_TEXT: &str = "没能保存，检查磁盘空间后重试。";
/// 既不是网络也不是写入的失败（例如本地编码出错）：不指责任何一方，只说重试。
pub(crate) const RETRY_TEXT: &str = "出现了一点问题，请重试。";

/// 封面请求池：有界并发 + 去重 + 队列上限。
///
/// 直接对整页每张封面发请求（尤其是 300 首的歌单）会把网络和 CDN 打爆，
/// 也会让后台线程池占满。池子只保持 `MAX_CONCURRENCY` 个在飞请求，
/// 其余排队；渲染永远只读缓存。
pub(crate) struct CoverPool {
    queue: VecDeque<String>,
    inflight: usize,
    seen: HashSet<String>,
}

impl CoverPool {
    const MAX_CONCURRENCY: usize = 8;
    const MAX_QUEUE: usize = 400;

    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            inflight: 0,
            seen: HashSet::new(),
        }
    }

    /// 入队（去重 + 队列上限），返回是否新增。
    fn push(&mut self, url: &str) -> bool {
        let url = url.trim();
        if url.is_empty() || self.seen.contains(url) || self.queue.len() >= Self::MAX_QUEUE {
            return false;
        }
        self.seen.insert(url.to_string());
        self.queue.push_back(url.to_string());
        true
    }

    fn next(&mut self) -> Option<String> {
        if self.inflight >= Self::MAX_CONCURRENCY {
            return None;
        }
        let url = self.queue.pop_front()?;
        self.inflight += 1;
        Some(url)
    }

    fn done(&mut self) {
        self.inflight = self.inflight.saturating_sub(1);
    }
}

/// 列表一次渲染的行数上限：GPUI 没有虚拟滚动，一次性布局几百行 + 解码封面
/// 会把主线程顶住；分批渲染 + 「显示更多」是这里最稳的做法。
pub const ROW_BATCH: usize = 40;

/// 逐字高亮最多往前外推多久（秒）。
///
/// 音频线程 100ms 一拍，正常外推不超过 100ms；250ms 是给「快照刚好停住」留的余量——
/// 再多就说明真的卡了，宁可停住也别让亮光跑到声音前面（#80）。
const LYRIC_LOOKAHEAD_SECONDS: f64 = 0.25;

/// 把扫码状态翻成中文短标签（用于「等待扫码 / 已扫码」这类提示）。
pub(crate) fn status_label(status: LoginStatus, language: Language) -> &'static str {
    match status {
        LoginStatus::Waiting => language.text("等待扫码"),
        LoginStatus::Scanned => language.text("已扫码"),
        LoginStatus::Success => language.text("登录成功"),
        LoginStatus::Expired => language.text("已过期"),
        LoginStatus::Failed => language.text("失败"),
    }
}

use crate::ui;
use crate::ui::i18n::{scene_mode_text, Language};
use crate::ui::text_field::TextField;
use crate::ui::theme::{self, ThemeKind};
use crate::views;

/// 左侧导航项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Nav {
    Home,
    Scenes,
    Search,
    Liked,
    Library,
    Artist,
    Album,
    Settings,
    Lyrics,
}

impl Root {
    /// 侧栏/页面标题用的导航名；听歌模式跟随当前队列来源动态变化。
    pub fn nav_display_label(&self, nav: Nav) -> String {
        let label = match nav {
            Nav::Home => self.tr("推荐"),
            Nav::Scenes => self.tr("听歌模式"),
            Nav::Search => self.tr("搜索"),
            Nav::Liked => self.tr("我喜欢的音乐"),
            Nav::Library => self.tr("我的歌单"),
            Nav::Artist => self.tr("音乐人"),
            Nav::Album => self.tr("专辑"),
            Nav::Settings => self.tr("设置"),
            Nav::Lyrics => self.tr("正在播放"),
        };
        if nav != Nav::Scenes {
            return label.to_string();
        }
        let active_origin = self
            .switching_queue
            .clone()
            .unwrap_or_else(|| self.queue_origin.clone());
        let QueueOrigin::FeedMode(kind) = active_origin else {
            return label.to_string();
        };
        let scene_text = self
            .recommendation
            .as_ref()
            .and_then(|source| source.scene.as_ref())
            .filter(|scene| scene.sub_queue_type == kind)
            .map(|scene| self.scene_display_text(&scene.sub_queue_type, &scene.text))
            .or_else(|| {
                self.scenes
                    .iter()
                    .find(|scene| scene.sub_queue_type == kind)
                    .map(|scene| self.scene_display_text(&scene.sub_queue_type, &scene.text))
            });
        scene_text.unwrap_or_else(|| label.to_string())
    }
}

/// 登录状态机。
pub enum LoginState {
    /// 还没开始
    Idle,
    /// 正在创建二维码
    Loading,
    /// 二维码已就绪，等待扫码（`status` 是轮询到的最新状态文案）
    Waiting {
        matrix: QrMatrix,
        token: String,
        status: String,
    },
    /// 已登录
    LoggedIn(#[allow(dead_code)] AccountInfo),
    /// 出错（可点重试）
    Failed(String),
}

/// 当前播放队列来源，语义对齐官方 queue metaInfo。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueOrigin {
    None,
    Feed,
    FeedMode(String),
    Radio(String),
    Liked,
    Playlist(String),
    Artist(String),
    Album(String),
    Search,
}

impl QueueOrigin {
    /// 是否为推荐 / 听歌模式这类无限流。
    ///
    /// 这类队列的歌由服务端持续追加，播放模式没有意义：单曲循环会让流停住，
    /// 随机也打不乱服务端的推荐节奏。播放器栏因此隐藏模式按钮。
    pub fn is_feed_like(&self) -> bool {
        matches!(
            self,
            QueueOrigin::Feed | QueueOrigin::FeedMode(_) | QueueOrigin::Radio(_)
        )
    }
}

/// 歌曲右键菜单状态：目标曲目与弹出位置。
pub struct TrackMenu {
    pub track: TrackItem,
    pub position: gpui::Point<gpui::Pixels>,
}

/// 根视图状态。
pub struct Root {
    pub nav: Nav,
    /// 自绘标题栏拖拽区的按下锚点：按下只记录，移动超过阈值才交给合成器，
    /// 否则合成器会抢走指针，双击最大化收不到第二次按下（见 ui::titlebar）。
    pub(crate) titlebar_anchor: ui::titlebar::DragAnchor,
    /// 页面返回栈；详情页统一使用返回组件。
    pub(crate) nav_history: Vec<Nav>,
    pub lyrics_return_nav: Option<Nav>,
    pub theme: ThemeKind,
    pub theme_follows_system: bool,
    pub language: Language,
    pub language_follows_system: bool,
    pub settings: Settings,
    pub session: Option<Session>,
    pub queue: Queue,
    pub queue_origin: QueueOrigin,
    pub playing: bool,
    pub status: String,
    /// 全局轻提示：每次 status 更新都递增序号，并记录起始时间。
    pub(crate) status_seq: u64,
    pub(crate) status_since: Option<std::time::Instant>,
    /// 全局 message 队列与计时去重集合。
    pub(crate) messages: Vec<ui::message::Message>,
    pub(crate) next_message_id: u64,
    pub(crate) message_timers: HashSet<u64>,
    /// 官方推荐队列分页状态。
    pub recommendation: Option<RecommendationSource>,
    pub loading_recommendation: bool,
    /// 推荐队列**首次装载**失败的原因（用户可见的那一句，`None` = 没有失败）。
    ///
    /// 只描述队列本身，不是播放错误：播放错误有自己的 `playback_error`。有值时推荐页
    /// 显示重试入口，而不是退回「正在准备推荐队列…」那个永远转圈的加载态。
    pub recommendation_error: Option<String>,
    /// 点推荐后的整页切换 loading；后台 load-more 不置位。
    ///
    /// **只在用户主动换队列时置位，过期请求不得改写它**：它决定播放页画的是队列切换页
    /// 还是真实内容，属于视图归属的导航状态，不该被一次超时或旧请求回写（#55）。
    pub switching_queue: Option<QueueOrigin>,
    pub switching_label: String,
    /// 推荐队列请求的序号（装载与分页共用）。
    ///
    /// **只在真正发起新请求时递增**：拿它判断「结果是否过期」，所以超时兜底不能动它，
    /// 否则在途请求回来时会被当成过期结果静默丢弃。
    pub(crate) recommendation_request_seq: u64,
    /// 推荐队列「装载」请求序号；与分页序号分开，两者可以同时在途。
    pub(crate) recommendation_load_seq: u64,
    /// 搜索框内容与焦点。
    pub lyrics: Arc<Vec<LyricLine>>,
    pub lyrics_track_id: String,
    pub lyrics_title: String,
    pub lyrics_artist: String,
    pub lyrics_cover: String,
    pub loading_lyrics: bool,
    pub lyrics_error: Option<String>,
    pub lyrics_active: Option<usize>,
    /// 最近一帧的播放位置（秒）；逐字高亮按它决定亮到第几个字。
    ///
    /// 由 `Root::render` 每帧写入，`views::lyrics` 只读。存在这里而不是在渲染函数里
    /// 取快照，是因为渲染路径不应该额外拿锁。
    pub(crate) lyrics_position_seconds: f64,
    /// 上一次从引擎读到播放位置时的「本地时刻」与「读到的那一帧的值」。
    ///
    /// 音频线程每 100ms 才写一次 `position_seconds`，而 UI 以 60fps 重绘。
    /// 直接读快照会让亮光每 100ms 跳一格（肉眼就是“不流畅”）。这里按本地
    /// 单调时钟把两次快照之间的位置线性外推，让高亮连续地走。
    /// `(读到时的 Instant, 读到时的秒数)`。
    pub(crate) lyrics_position_anchor: Option<(std::time::Instant, f64)>,
    /// 逐字高亮样张模式：设置后 `render` 每帧把播放位置固定在这个秒数（#80 的视觉验证）。
    /// 生产环境恒为 `None`。
    pub(crate) lyrics_demo_position: Option<f64>,
    /// 歌词区自己管滚动（外层是普通滚动容器，不是虚拟列表），
    /// 这样长句折行后行高不一致也能正确定位。
    pub lyrics_scroll: gpui::ScrollHandle,
    /// 用户手动滚过歌词后，暂停自动跟随到这个时刻。
    ///
    /// 官方客户端是「滚轮后 5 秒内不跟着进度跳」，这里沿用同一策略，
    /// 否则用户往回翻歌词会被立刻拽回当前句。
    pub lyrics_user_scroll_until: Option<std::time::Instant>,
    /// 等待下一帧补做的歌词对齐（布局未就绪时记下目标句）。
    pub(crate) pending_lyrics_scroll: Option<usize>,
    /// 歌词滚动的过渡动画：`(起点偏移, 终点偏移, 开始时刻)`。
    ///
    /// 直接 `set_offset` 会让整块歌词在换句瞬间瞬移，即便只是挪一行的高度，
    /// 肉眼也会觉得"跳"。这里改成在 ticker 的每一帧里按缓出曲线插值，
    /// 让换句像"滑上去"而不是"闪过去"（官方 CSS 是 `transition: transform .4s ease-out`）。
    pub(crate) lyrics_scroll_tween: Option<(f32, f32, std::time::Instant, std::time::Duration)>,
    /// 当前过渡动画的目标句。用来判断"目标句有没有变"，
    /// 避免每帧重算目标（`bounds_for_item` 含滚动偏移，滚动中每帧都在变）。
    pub(crate) lyrics_scroll_tween_index: Option<usize>,
    /// 防止快速换歌时旧歌词请求回写。
    lyrics_request_seq: u64,
    pub search_field: TextField,
    /// 搜索框文本区的窗口内 bounds：canvas 绘制时回填，鼠标点击定位用。
    pub(crate) search_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// 当前鼠标拖拽是否按词扩选（双击起步的拖拽为真；松开鼠标复位）。
    pub(crate) search_drag_by_word: bool,
    /// 撤销合并窗口的单调时钟起点（见 [`Root::search_edit_time`]）。
    search_edit_epoch: std::time::Instant,
    pub search_focus: FocusHandle,
    /// 「按下 Ctrl+F，等搜索框进树之后把焦点给它」的待办标记。
    ///
    /// 只在 `app_text::Root::focus_search` 里置位，在 `Root::render` 开头消费。
    pub(crate) search_focus_pending: bool,
    /// 窗口根元素的焦点。
    ///
    /// 必须有：gpui 的上下文栈取自「聚焦节点到根的路径」，没有聚焦元素时栈是空的，
    /// 带上下文谓词的快捷键绑定（播放三键绑在 `workspace && !input` 上）永远不会匹配。
    /// 所以根元素自己持焦点，点搜索框等元素时焦点自然让出去。
    pub workspace_focus: FocusHandle,
    /// 设置页里的快捷键卡片：录制新组合键时持有焦点，并让播放绑定让路。
    pub shortcut_focus: FocusHandle,
    /// 正在录制的动作（None = 没在录制）。
    pub shortcut_capture: Option<crate::shortcuts::Slot>,
    /// 录制时的行内提示（徽标键被拒、按键无法绑定）：下一次按键、取消或离开设置页都会清掉。
    ///
    /// 不走 `status` —— 那个字段现在没有任何地方渲染（见 #40），提示必须长在出问题的行上。
    pub shortcut_note: Option<&'static str>,
    /// 快捷键配置有改动，需要重新注册 keymap（由 main 的托盘循环统一应用）。
    pub shortcuts_dirty: bool,
    /// 搜索结果与加载状态。
    pub results: Arc<Vec<TrackItem>>,
    pub search_results: SearchResults,
    pub search_tab: SearchScope,
    pub searching: bool,
    /// 最近一次真正提交搜索的关键词；用于区分“没搜过”和“搜了没结果”。
    pub search_keyword: String,
    /// 听歌模式页：常用模式与探索歌单。
    pub scenes: Arc<Vec<SceneItem>>,
    pub loading_scenes: bool,
    pub scene_cards: Arc<Vec<SceneCard>>,
    pub loading_scene_cards: bool,
    /// 听歌模式页的变高虚拟列表状态；只渲染可视行，避免几十张封面同时布局。
    pub scenes_list: gpui::ListState,
    /// 详情页使用独立列表状态，避免页面切换时共享状态导致内容/滚动错乱。
    pub liked_detail_list: gpui::ListState,
    pub library_detail_list: gpui::ListState,
    pub search_playlist_detail_list: gpui::ListState,
    pub artist_detail_list: gpui::ListState,
    pub album_detail_list: gpui::ListState,
    scenes_list_items: usize,
    /// 听歌模式页可用宽度；用于动态计算虚拟网格列数。
    pub scenes_width: Arc<Mutex<f32>>,
    pub scenes_common_columns: usize,
    pub scenes_card_columns: usize,
    /// 我的歌单 / 我喜欢的音乐（进入对应页面时懒加载）。
    pub playlists: Vec<PlaylistItem>,
    pub liked: Arc<Vec<TrackItem>>,
    /// 「我喜欢的音乐」的曲目 id 集合（列表里的爱心状态）。
    pub liked_ids: Arc<HashSet<String>>,
    liked_loading: bool,
    /// 收藏 ids 拉取失败（网络等）：置位后等网络就绪信号自动重试。
    pub liked_ids_failed: bool,
    pub loading_library: bool,
    pub loading_liked: bool,
    pub liked_loaded: bool,
    /// 列表滚动句柄：交给 `uniform_list` 之后，UI 滚动位置由 GPUI 维护，
    /// 同时可用于开发期滚动压测（`SODAM_SCROLL_TEST=1`）。
    pub liked_scroll: gpui::UniformListScrollHandle,
    /// 列表可用宽度（由画布测量）：决定折叠哪些列。
    pub list_width: Arc<Mutex<f32>>,
    /// 队列抽屉的滚动句柄（打开时锚到「正在播放」）。
    pub queue_scroll: gpui::UniformListScrollHandle,
    /// 队列抽屉是否展开。
    pub queue_open: bool,
    pub(crate) preferences_writer: sodam_core::preferences::PreferencesWriter,
    /// 播放栏弹层锚点：触发元素（音质徽章/音量/队列）在窗口里的位置，
    /// 由画布测量，弹层据此精确贴住入口。
    /// 进度条轨道在窗口里的位置（画布测量，鼠标事件据此换算 seek 位置）。
    pub progress_track_bounds: Arc<Mutex<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// 音量轨道在窗口里的位置（画布测量，鼠标事件据此换算音量）。
    pub volume_track_bounds: Arc<Mutex<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// 打开中的歌单（点卡片后展示曲目）。
    pub open_playlist: Option<(String, String, Arc<Vec<TrackItem>>)>,
    pub loading_playlist: bool,
    /// 搜索结果里的歌单详情；保留在 Search 上下文，不切去「我的歌单」。
    pub search_playlist: Option<(String, String, Arc<Vec<TrackItem>>)>,
    pub loading_search_playlist: bool,
    /// 音乐人 / 专辑详情页。
    pub open_artist: Option<ArtistDetail>,
    pub loading_artist: bool,
    pub loading_artist_tracks: bool,
    pub open_album: Option<AlbumDetail>,
    pub loading_album: bool,
    /// 渲染期收集到的封面需求（虚拟列表只渲染可见行，所以这里排的就是
    /// 「当前真正看得见」的那些封面）。由后台循环定期消费。
    pub cover_requests: Arc<Mutex<Vec<String>>>,
    /// 真实播放过的曲目 id（最近在后）。用于队列抽屉的「已播放」分区；
    /// 不能用「当前下标之前」来推 —— 那会把跳过的歌也算进去（实测踩过）。
    pub played_history: Vec<String>,
    /// 设置页里的登录 modal 是否展开。
    pub login_modal_open: bool,
    /// 队列右键菜单：被点中的行下标与鼠标位置。
    pub queue_menu: Option<(usize, gpui::Point<gpui::Pixels>)>,
    /// 搜索/歌单/我喜欢等歌曲行右键菜单。
    pub track_menu: Option<TrackMenu>,
    /// 播放请求序号：每次 start_track +1。
    ///
    /// 作用：加载是异步的（下载 + 解密），如果用户在 A 加载途中换了 B，
    /// A 完成时必须**作废**，否则会把正在播的 B 顶掉（实测踩过：
    /// B 播一会儿后突然跳回 A，封面也对不上）。
    pub(crate) play_seq: u64,
    /// 播放错误（显示在播放栏：常驻状态条已移除，错误必须有地方可见）。
    pub playback_error: Option<String>,
    /// 连续装载失败次数（>=3 就停下，避免无限跳歌）。
    ///
    /// 清零判据是「最近一次成功装载真的播出去若干秒」，不是「此刻有声」：
    /// 见 `AttemptProgress::recovered` 与下面的 `progress_token`。
    pub(crate) consecutive_failures: u32,
    /// 最近一次**成功装载**交给引擎的序号；任何一次装载失败都把它作废。
    ///
    /// 连败复位只认「这个序号 == 引擎当前快照的 load_seq」，也就是进展必须归属于
    /// 最新那次尝试。没有它，上一首还在放就会把计数每拍清零，封顶永远触发不了。
    pub(crate) progress_token: Option<u64>,
    /// 连败封顶闸门：打开后自动续播（播完切下一首、预取巡检）全部停住，
    /// 直到用户主动播放。只写 `playing = false` 拦不住——引擎仍在出声，
    /// 「已暂停」就成了谎话。
    pub(crate) playback_halted: bool,
    /// 正在预取的曲目 id 集合（多首并行去重；预取结果只写缓存，不改 UI）。
    pub(crate) prefetch_inflight: HashSet<String>,
    /// 预取失败表（曲目 id →（最近失败时刻，可否重试））。
    ///
    /// 冷却类期满可以再排；**永久失败（下架 / 无权益 / 签名服务鉴权）不再自动排**——
    /// 巡检每 5 秒重排一次就是在持续打扰全体用户共享的那台签名服务。
    pub(crate) prefetch_failed:
        HashMap<String, (std::time::Instant, sodam_core::failure::Retryability)>,
    /// 预取巡检节拍：心跳每 200ms 自增，每 25 拍（约 5s）补一次预取。
    pub(crate) prefetch_patrol: u32,
    /// 缓存统计快照（音频字节, 歌曲数, 封面字节, 封面张数）：
    /// 后台扫盘后回填，设置页只读它（渲染路径不做同步 IO）。
    pub(crate) cache_summary: (u64, usize, u64, usize),
    pub(crate) cache_maintenance_running: bool,
    pub(crate) cache_clearing: bool,
    /// 正在删除诊断日志；只用于禁用重复点击，与缓存清理互不影响。
    pub(crate) log_clearing: bool,
    /// 下载完成后交给音频线程的装载序号；避免同一曲目重载/失败时永久 loading。
    pub(crate) pending_engine_load: Option<u64>,
    /// 正在加载（下载/解密）的曲目：非空时播放栏显示 loading，且进度条不可拖。
    pub pending_track: Option<TrackItem>,
    /// 已处理过的完成事件（曲目结束序号 + 跳转序号）。
    pub(crate) last_finished_seq: u64,
    pub(crate) last_seek_seq: u64,
    /// 拖动进度条时的预览位置（0.0~1.0）：拖动中只改它，松手才真跳。
    pub progress_preview: Option<f32>,
    /// 歌词页大封面：切曲后的 `(track_id, 开始时刻)`；管「至少 200ms 加载态」的门。
    pub lyrics_cover_gate: Option<(String, std::time::Instant)>,
    /// 队列快照（含版本号）：避免抽屉每帧深拷贝整条队列。
    pub queue_cache: (u64, Arc<Vec<TrackItem>>),
    /// 已尝试过的封面 URL（成功在 covers 里，失败的也记下来，避免每帧重复排队）。
    pub cover_attempted: Arc<Mutex<HashSet<String>>>,
    /// 封面缓存：URL → 本地文件。**只由后台任务写入**，渲染路径只读，
    /// 绝不在 render 里发网络请求（否则主线程会被下载卡住）。
    pub covers: Arc<HashMap<String, PathBuf>>,
    pub original_covers: HashMap<String, PathBuf>,
    pub original_cover_attempted: Arc<Mutex<HashSet<String>>>,
    /// “其他”里的贡献者图（contrib.rocks 组合 SVG）；独立缓存，不参与歌曲封面统计。
    pub contributors_image: Option<Arc<Image>>,
    pub(crate) contributors_fetching: bool,
    pub(crate) contributors_failed: bool,
    pub ambient_colors: HashMap<String, (u8, u8, u8)>,
    pub(crate) cover_pool: CoverPool,
    /// 各列表当前渲染的行数（点「显示更多」时递增）。
    /// 扫码登录状态与轮询中的 token。
    pub login: LoginState,
    pub account: Option<AccountInfo>,
    /// 音频播放引擎（rodio，跑在专属线程上）
    pub engine: PlaybackEngine,
    /// 播放进度 / 时长上报（喂引擎快照，内部异步发请求）。
    pub play_reporter: PlayReporter,
}

impl Root {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut settings = Settings::load().merged_with_env();
        let engine = PlaybackEngine::with_volume(settings.volume);
        settings.volume = engine.snapshot().volume;
        // 首次运行就把配置落盘：用户可以直接编辑 ~/.config/sodam/config.json
        // 换签名服务 / 钉死设备指纹（默认值已内置，见 sodam_core::config）
        if !Settings::config_path().exists() {
            let _ = settings.save();
        }
        let theme_follows_system = ThemeKind::from_key(&settings.theme).is_none();
        let theme = ThemeKind::from_key(&settings.theme)
            .unwrap_or_else(|| theme::theme_from_appearance(cx.window_appearance()));
        theme::set_theme(theme);
        let (language, language_follows_system) = match Language::from_key(&settings.language) {
            Some(Language::System) | None => (Language::system_locale(), true),
            Some(language) => (language, false),
        };
        let session = Session::new(settings.clone());
        let status = if settings.is_ready_for_vip() {
            if language.is_zh() {
                language
                    .text("已配置登录与签名服务，可以去「搜索」或「推荐」了")
                    .to_string()
            } else {
                "Signed in and signer ready. Try Discover or Search.".to_string()
            }
        } else {
            if language.is_zh() {
                language.text("还没配好：请在「设置」里填 Cookie 与签名服务地址（或设 SODA_COOKIE / QISHUI_SIGNER_URL）").to_string()
            } else {
                "Not ready: set Cookie and signer URL in Settings (or use SODA_COOKIE / QISHUI_SIGNER_URL)".to_string()
            }
        };
        let mut queue = Queue::new(Vec::<TrackItem>::new());
        // 播放模式跨启动保留（设置页没做这一项，按钮在播放栏）。
        queue.mode = settings.play_mode;
        let engine_progress = engine.snapshot();

        let logged_in = !settings.cookie.trim().is_empty();
        let play_reporter = PlayReporter::new(settings.clone());
        let mut root = Self {
            titlebar_anchor: std::rc::Rc::new(std::cell::Cell::new(None)),
            // 未登录只能待在设置页（登录入口在设置 → 账户）
            nav: if logged_in {
                Nav::Lyrics
            } else {
                Nav::Settings
            },
            nav_history: Vec::new(),
            lyrics_return_nav: if logged_in { Some(Nav::Home) } else { None },
            theme,
            theme_follows_system,
            language,
            language_follows_system,
            settings,
            session: Some(session),
            queue,
            queue_origin: QueueOrigin::None,
            played_history: Vec::new(),
            login_modal_open: false,
            queue_menu: None,
            track_menu: None,
            play_seq: 0,
            playback_error: None,
            consecutive_failures: 0,
            progress_token: None,
            playback_halted: false,
            playing: false,
            recommendation: None,
            loading_recommendation: false,
            recommendation_error: None,
            switching_queue: None,
            switching_label: language.text("推荐队列").to_string(),
            recommendation_request_seq: 0,
            recommendation_load_seq: 0,
            status,
            status_seq: 0,
            status_since: None,
            messages: Vec::new(),
            next_message_id: 0,
            message_timers: HashSet::new(),
            lyrics: Arc::new(Vec::new()),
            lyrics_track_id: String::new(),
            lyrics_title: String::new(),
            lyrics_artist: String::new(),
            lyrics_cover: String::new(),
            loading_lyrics: false,
            lyrics_error: None,
            lyrics_active: None,
            lyrics_position_seconds: 0.0,
            lyrics_position_anchor: None,
            lyrics_demo_position: None,
            lyrics_scroll: gpui::ScrollHandle::new(),
            lyrics_user_scroll_until: None,
            pending_lyrics_scroll: None,
            lyrics_scroll_tween: None,
            lyrics_scroll_tween_index: None,
            lyrics_request_seq: 0,
            search_field: TextField::default(),
            search_drag_by_word: false,
            search_edit_epoch: std::time::Instant::now(),
            search_bounds: std::rc::Rc::new(std::cell::Cell::new(None)),
            search_focus: cx.focus_handle(),
            search_focus_pending: false,
            workspace_focus: cx.focus_handle(),
            shortcut_focus: cx.focus_handle(),
            shortcut_capture: None,
            shortcut_note: None,
            shortcuts_dirty: false,
            results: Arc::new(Vec::new()),
            search_results: SearchResults::default(),
            search_tab: SearchScope::All,
            searching: false,
            search_keyword: String::new(),
            scenes: Arc::new(Vec::new()),
            loading_scenes: false,
            scene_cards: Arc::new(Vec::new()),
            loading_scene_cards: false,
            scenes_list: gpui::ListState::new(0, gpui::ListAlignment::Top, px(theme::space::XL)),
            liked_detail_list: gpui::ListState::new(
                1,
                gpui::ListAlignment::Top,
                px(theme::space::XXL),
            ),
            library_detail_list: gpui::ListState::new(
                1,
                gpui::ListAlignment::Top,
                px(theme::space::XXL),
            ),
            search_playlist_detail_list: gpui::ListState::new(
                1,
                gpui::ListAlignment::Top,
                px(theme::space::XXL),
            ),
            artist_detail_list: gpui::ListState::new(
                1,
                gpui::ListAlignment::Top,
                px(theme::space::XXL),
            ),
            album_detail_list: gpui::ListState::new(
                1,
                gpui::ListAlignment::Top,
                px(theme::space::XXL),
            ),
            scenes_list_items: 0,
            scenes_width: Arc::new(Mutex::new(0.0)),
            scenes_common_columns: 4,
            scenes_card_columns: 4,
            playlists: Vec::new(),
            liked: Arc::new(Vec::new()),
            liked_ids: Arc::new(HashSet::new()),
            liked_loading: false,
            liked_ids_failed: false,
            loading_library: false,
            loading_liked: false,
            liked_loaded: false,
            liked_scroll: gpui::UniformListScrollHandle::new(),
            list_width: Arc::new(Mutex::new(1200.0)),
            queue_scroll: gpui::UniformListScrollHandle::new(),
            open_playlist: None,
            loading_playlist: false,
            search_playlist: None,
            loading_search_playlist: false,
            open_artist: None,
            loading_artist: false,
            loading_artist_tracks: false,
            open_album: None,
            loading_album: false,
            queue_open: false,
            preferences_writer: sodam_core::preferences::PreferencesWriter::new(
                Settings::config_path(),
            ),
            progress_track_bounds: Arc::new(Mutex::new(None)),
            volume_track_bounds: Arc::new(Mutex::new(None)),
            prefetch_inflight: HashSet::new(),
            prefetch_failed: HashMap::new(),
            prefetch_patrol: 0,
            cache_summary: (0, 0, 0, 0),
            cache_maintenance_running: false,
            cache_clearing: false,
            log_clearing: false,
            pending_engine_load: None,
            pending_track: None,
            last_finished_seq: engine_progress.finished_seq,
            last_seek_seq: engine_progress.seek_seq,
            progress_preview: None,
            lyrics_cover_gate: None,
            queue_cache: (0, Arc::new(Vec::new())),
            cover_attempted: Arc::new(Mutex::new(HashSet::new())),
            cover_requests: Arc::new(Mutex::new(Vec::new())),
            covers: Arc::new(HashMap::new()),
            original_covers: HashMap::new(),
            original_cover_attempted: Arc::new(Mutex::new(HashSet::new())),
            contributors_image: None,
            contributors_fetching: false,
            contributors_failed: false,
            ambient_colors: HashMap::new(),
            cover_pool: CoverPool::new(),
            login: LoginState::Idle,
            account: None,
            engine,
            play_reporter,
        };
        cx.on_app_quit(|root, _cx| {
            sodam_core::flush_global();
            root.preferences_writer.flush()
        })
        .detach();
        root.maintain_cache(cx);
        if logged_in {
            root.refresh_account(cx);
            root.load_liked_ids(cx);
            root.start_recommendation(false, cx);
        } else {
            root.set_status_text(root.tr("请先在「设置 → 账户」扫码登录"));
        }
        // 开发/验证便利：`SODAM_QUERY="周杰伦" cargo run -p sodam` 启动即搜索一次
        if let Ok(query) = std::env::var("SODAM_QUERY") {
            if !query.trim().is_empty() {
                root.search_field.set_text(query);
                root.nav = Nav::Search;
                root.run_search(cx);
            }
        }
        // 开发/验证便利：`SODAM_NAV=library|l liked|search|scenes|home` 直接落到某个页面
        if let Ok(nav) = std::env::var("SODAM_NAV") {
            root.nav = match nav.trim().to_lowercase().as_str() {
                "home" => Nav::Home,
                "scenes" => Nav::Scenes,
                "search" => Nav::Search,
                "liked" => Nav::Liked,
                "library" => Nav::Library,
                "artist" => Nav::Artist,
                "album" => Nav::Album,
                "settings" => Nav::Settings,
                _ => root.nav,
            };
            root.on_nav_changed(cx);
            cx.notify();
        }
        // 开发/验证便利：`SODAM_LYRICS_DEMO=<秒>` 直接进入歌词页并注入一份逐字样张，
        // 把播放位置固定在该秒数以观察逐字高亮（#80）。不依赖登录与网络。
        //
        // 和 `SODAM_QUERY` / `SODAM_NAV` 同级：只在显式设置时才生效，生产环境不设就等于不存在。
        // 位置来自环境变量，或临时目录里的 `sodam-lyrics-demo` 文件——后者是为了在
        // GUI 会话（无法注入环境变量）里也能触发。
        let demo_position = std::env::var("SODAM_LYRICS_DEMO")
            .ok()
            .map(|value| value.trim().to_string())
            .or_else(|| {
                std::fs::read_to_string(std::env::temp_dir().join("sodam-lyrics-demo")).ok()
            });
        if let Some(demo_position) = demo_position {
            let position = demo_position.trim().parse::<f64>().unwrap_or(3.0);
            // 折行验证用的一句：内容长、在当前列宽下会折成两行，给足 8 秒以便观察
            // 亮光从第一行推到第二行的全过程（#80）。
            // 行尾空档验证：最后一个词的结束时间早于行结束时间，
            // 用来确认行尾那段空档里亮光仍在推进，而不是一下跳到满格（#80）。
            let wrap_line = "And you think I'm crazy yeah you think I'm crazy";
            let demo = [
                (0.0, "偷偷搭询总没完地坐立难安", 3.0),
                (3.0, "试探说晚安多空泛又心酸", 3.0),
                (7.0, "低头呢喃", 3.0),
                (10.0, "对你的偏爱太过于明目张胆", 3.0),
                (13.0, wrap_line, 8.0),
            ]
            .into_iter()
            .map(|(start, text, duration)| sodam_core::models::LyricLine {
                start_seconds: start,
                end_seconds: start + duration,
                text: text.to_string(),
                // 前 70% 的字只占 40% 时长，最后 30% 的字占 60% 时长。
                //
                // 真实歌词里每字的演唱时长差异很大（拖长音 vs 快词）。这样构造能
                // 回归两件事：末字不能"唰"一下掠过（按字推进才对），以及末字
                // 恰好走到行结束。
                words: {
                    let chars: Vec<char> = text.chars().collect();
                    let split = (chars.len() * 7 / 10).max(1);
                    let mut words = Vec::with_capacity(chars.len());
                    let mut at = start;
                    for (index, ch) in chars.iter().enumerate() {
                        let share = if index < split {
                            0.4 / split as f64
                        } else {
                            0.6 / (chars.len() - split).max(1) as f64
                        };
                        let span = duration * share;
                        words.push(sodam_core::models::LyricWord {
                            start_seconds: at,
                            end_seconds: at + span,
                            text: ch.to_string(),
                        });
                        at += span;
                    }
                    words
                },
                has_word_timeline: true,
            })
            .collect::<Vec<_>>();
            // 歌词页在没有当前曲目时会提前返回占位页，所以样张模式也塞一首假曲目。
            let demo_track = TrackItem {
                id: "sodam-lyrics-demo".to_string(),
                title: "逐字高亮样张".to_string(),
                artist: "SodaM".to_string(),
                duration_seconds: 200,
                ..Default::default()
            };
            root.queue.replace(vec![demo_track], 0);
            root.queue_origin = QueueOrigin::None;
            root.lyrics = Arc::new(demo);
            root.lyrics_position_seconds = position;
            root.lyrics_demo_position = Some(position);
            root.lyrics_active = root
                .lyrics
                .iter()
                .rposition(|line| position + 0.25 >= line.start_seconds);
            root.nav = Nav::Lyrics;
            cx.notify();
        }
        // 封面需求消费循环：渲染只往队列里塞 URL，这里统一喂给请求池
        //（最多 8 并发 + 去重），避免渲染过程中做任何网络/状态更新。
        {
            let queue = root.cover_requests.clone();
            cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(60))
                    .await;
                let pending: Vec<String> = {
                    let mut queue = match queue.lock() {
                        Ok(queue) => queue,
                        Err(_) => continue,
                    };
                    if queue.is_empty() {
                        continue;
                    }
                    std::mem::take(&mut *queue)
                };
                let _ = this.update(cx, |root, cx| root.ensure_covers(&pending, cx));
            })
            .detach();
        }

        // 开发期滚动压测：`SODAM_SCROLL_TEST=1` 时按行推进滚动，
        // 配合 `SODAM_FRAME_LOG=1`（把 app 话题提到 DEBUG）就能量出「滑动是否掉帧」，不用手工滚。
        if std::env::var("SODAM_SCROLL_TEST").is_ok() {
            let handle = root.liked_scroll.clone();
            cx.spawn(async move |_this, cx| {
                let mut item = 0usize;
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(32))
                        .await;
                    item = if item > 380 { 0 } else { item + 2 };
                    handle.scroll_to_item(item, gpui::ScrollStrategy::Center);
                    cx.refresh();
                }
            })
            .detach();
        }
        Self::start_heartbeat(cx);
        // 歌词逐字高亮需要比主心跳更高的刷新率（#80）。
        Self::start_lyrics_ticker(cx);

        // 开发验证用：`SODAM_AUTOPLAY=1` 进收藏页并自动播放第一首；
        // `SODAM_POPUP=quality|queue|volume` 启动即打开对应弹层（便于截图核对）。
        if std::env::var("SODAM_AUTOLOGIN").is_ok() && !root.settings.cookie.trim().is_empty() {
            root.login_modal_open = true;
            root.start_login(cx);
        }
        if std::env::var("SODAM_AUTOPLAY").is_ok() {
            let this = cx.entity();
            cx.spawn(async move |_this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(2500))
                    .await;
                this.update(cx, |root, cx| {
                    if !root.liked.is_empty() {
                        root.play_from_arc(root.liked.clone(), 0, cx);
                    }
                });
            })
            .detach();
        }
        // 竞态复现：先播 A，300ms 后立刻换 B（验证 A 的加载结果会被丢弃）
        if std::env::var("SODAM_RACE_TEST").is_ok() {
            let this = cx.entity();
            cx.spawn(async move |_this, cx| {
                for _ in 0..60 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;
                    let ready = this.update(cx, |root, _cx| root.liked.len() > 8);
                    if ready {
                        break;
                    }
                }
                // 挑两首「不在缓存里」的歌，才能真正制造「A 还在下载时就切 B」
                let picks = this.update(cx, |root, _cx| {
                    let session = root.session.as_ref()?;
                    let tracks = root.liked.as_ref();
                    let mut picks = Vec::new();
                    for (index, track) in tracks.iter().enumerate() {
                        if !session.is_cached(&track.id) {
                            picks.push(index);
                            if picks.len() == 2 {
                                break;
                            }
                        }
                    }
                    (picks.len() == 2).then_some(picks)
                });
                let Some(picks) = picks else {
                    log_event(
                        LogLevel::Debug,
                        LogTopic::Player,
                        "race_probe_skip",
                        &[("reason", "no_uncached_tracks")],
                    );
                    return;
                };
                let (first, second) = (picks[0].to_string(), picks[1].to_string());
                log_event(
                    LogLevel::Debug,
                    LogTopic::Player,
                    "race_probe_pick",
                    &[("a", first.as_str()), ("b", second.as_str())],
                );
                this.update(cx, |root, cx| {
                    root.play_from(root.liked.as_ref().clone(), picks[0], cx);
                });
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                this.update(cx, |root, cx| {
                    root.play_from(root.liked.as_ref().clone(), picks[1], cx);
                });
            })
            .detach();
        }
        if let Ok(index) = std::env::var("SODAM_JUMP_TEST") {
            let index: usize = index.trim().parse().unwrap_or(0);
            let this = cx.entity();
            cx.spawn(async move |_this, cx| {
                // 等收藏列表加载完（队列非空）再模拟点击
                for _ in 0..60 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;
                    let ready = this.update(cx, |root, _cx| root.queue.len() > index);

                    if ready {
                        break;
                    }
                }
                this.update(cx, |root, cx| {
                    root.jump_in_queue(index, cx);
                    cx.notify();
                });
            })
            .detach();
        }
        if std::env::var("SODAM_POPUP")
            .map(|value| value.trim() == "queue")
            .unwrap_or(false)
        {
            root.queue_open = true;
        }
        root
    }
}

/// 搜索框光标的宽度。绘制（`views::search_box`）与滚动判定（`search_fit_caret`、
/// `search_max_scroll`）必须共用这一个值：只按 0 宽度算滚动的话，滚到最右时光标正好
/// 压在 canvas 的裁剪线上，整个被裁掉，看起来就是「末尾没有光标」。
pub(crate) const CARET_WIDTH: gpui::Pixels = gpui::px(2.0);

impl Root {
    /// 按当前 text style 把搜索框内容 shape 成一行。
    ///
    /// 光标位置和点击定位都用**同一条** ShapedLine 量，避免"自己估宽度"和
    /// 实际渲染不一致导致的光标错位（sonora 的 Input 也是这么做的）。
    fn search_shaped_line(&self, window: &Window) -> Option<gpui::ShapedLine> {
        self.search_shaped_line_with(window, theme::text())
    }

    /// 同上，但文字色由调用方给：视图现在要自己 `line.paint` 真实文本（好跟着横向滚动走），
    /// 颜色必须跟 `theme::text()` 一致，不能退回窗口默认色。
    pub(crate) fn search_shaped_line_with(
        &self,
        window: &Window,
        color: gpui::Rgba,
    ) -> Option<gpui::ShapedLine> {
        if self.search_field.is_empty() {
            return None;
        }
        let style = window.text_style();
        let run = gpui::TextRun {
            len: self.search_field.len(),
            font: style.font(),
            color: gpui::Hsla::from(color),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        Some(window.text_system().shape_line(
            self.search_field.text().into(),
            style.font_size.to_pixels(window.rem_size()),
            &[run],
            None,
        ))
    }

    /// 鼠标点击/拖拽位置的窗口内 x → 最近的字符边界（字节下标）。
    ///
    /// 文本是按 `-scroll_x` 绘制的，所以命中测试要先把滚走的那段加回来，否则滚过之后
    /// 点哪儿都会偏。
    pub(crate) fn search_cursor_for_x(&self, x: gpui::Pixels, window: &Window) -> usize {
        let Some(line) = self.search_shaped_line(window) else {
            return 0;
        };
        // canvas 就铺在文本盒上，bounds.left 就是文本起点。
        let origin = self
            .search_bounds
            .get()
            .map(|bounds| bounds.origin.x)
            .unwrap_or_else(|| gpui::px(0.0));
        let local = x - origin + self.search_scroll_x();
        if local <= gpui::px(0.0) {
            return 0;
        }
        line.closest_index_for_x(local).min(self.search_field.len())
    }

    /// 文本坐标系（未计滚动）下某个字节下标的横向位置。
    fn search_text_x(&self, index: usize, window: &Window) -> gpui::Pixels {
        match self.search_shaped_line(window) {
            Some(line) => line.x_for_index(index),
            None => gpui::px(0.0),
        }
    }

    pub(crate) fn search_scroll_x(&self) -> gpui::Pixels {
        gpui::px(self.search_field.scroll_x())
    }

    /// 撤销合并用的时间戳（毫秒，自 `Root` 构造起算）。
    ///
    /// `TextField` 刻意不读时钟：读了就写不出确定性的单测，500ms 合并窗口也没法验证。
    /// 所以时钟留在这里，状态层只接收算好的数。
    pub(crate) fn search_edit_time(&self) -> crate::ui::text_field::Milliseconds {
        self.search_edit_epoch.elapsed().as_millis() as u64
    }

    /// 插入点在输入框内的横向偏移：**已扣掉滚动量**，可以直接当元素的 `left`。
    pub(crate) fn search_caret_x(&self, window: &Window) -> gpui::Pixels {
        let caret = self.search_field.caret();
        self.search_text_x(caret, window) - self.search_scroll_x()
    }

    /// 选区左右两条边（文本坐标系）。无选区时 `None`。
    ///
    /// 单行框的选区必然落在同一行上，所以两端就够，不用像多行编辑器那样逐行切矩形。
    pub(crate) fn search_selection_edges(
        &self,
        window: &Window,
    ) -> Option<(gpui::Pixels, gpui::Pixels)> {
        if !self.search_field.has_selection() {
            return None;
        }
        let range = self.search_field.selection();
        Some((
            self.search_text_x(range.start, window),
            self.search_text_x(range.end, window),
        ))
    }

    /// 文本能横向滚动的最大距离；文本比视口窄时为 0。
    ///
    /// 多留出 `CARET_WIDTH`：插入点在最后一段文字之后时它本身就落在文本宽度上，
    /// 不留这 2px 就永远滚不进视区。
    pub(crate) fn search_max_scroll(&self, window: &Window, width: gpui::Pixels) -> gpui::Pixels {
        let text_width = self
            .search_shaped_line(window)
            .map(|line| line.width())
            .unwrap_or_else(|| gpui::px(0.0));
        (text_width + CARET_WIDTH - width).max(gpui::px(0.0))
    }

    /// 把活动端拉进可视区，只滚到刚好露出（不居中）。返回是否滚了。
    pub(crate) fn search_fit_caret(&mut self, window: &Window, width: gpui::Pixels) -> bool {
        let caret_x = f32::from(self.search_text_x(self.search_field.caret(), window));
        let max = f32::from(self.search_max_scroll(window, width));
        self.search_field
            .ensure_visible(caret_x, f32::from(CARET_WIDTH), f32::from(width), max)
    }

    /// 同上，但宽度取上次绘制记下的文本区宽度（按键路径拿不到布局，只能这样）。
    pub(crate) fn search_fit_caret_in_view(&mut self, window: &Window) {
        let Some(bounds) = self.search_bounds.get() else {
            return;
        };
        self.search_fit_caret(window, bounds.size.width);
    }

    /// 鼠标按下：单击定位、双击选词、三击全选（Chromium 的单行框三击就是 SelectAll）。
    /// 同时记下这次的拖拽粒度，供随后的拖拽沿用。
    pub(crate) fn search_pointer_down(
        &mut self,
        x: gpui::Pixels,
        click_count: usize,
        window: &Window,
    ) -> bool {
        let index = self.search_cursor_for_x(x, window);
        self.search_drag_by_word = click_count >= 2;
        match click_count {
            0 | 1 => self.search_field.set_caret(index),
            2 => self.search_field.select_word_at(index),
            _ => self.search_field.select_all(),
        }
    }

    /// 拖拽扩选：双击起步的拖拽按整词扩（Chromium 会记住 `double_click_word_`，
    /// 否则双击后一拖就退化成字符级），普通拖拽逐字符。
    pub(crate) fn search_pointer_drag(&mut self, x: gpui::Pixels, window: &Window) -> bool {
        let index = self.search_cursor_for_x(x, window);
        if self.search_drag_by_word {
            self.search_field.select_words_through(index)
        } else {
            self.search_field.select_through(index)
        }
    }

    /// 松开鼠标：结束按词拖拽，下一次按下重新判定。
    pub(crate) fn search_pointer_up(&mut self) {
        self.search_drag_by_word = false;
    }

    /// 拖拽选区时按 `delta` 横滚，夹在 `[0, max]` 内。返回是否滚了。
    pub(crate) fn search_scroll_by(
        &mut self,
        delta: gpui::Pixels,
        window: &Window,
        width: gpui::Pixels,
    ) -> bool {
        let max = f32::from(self.search_max_scroll(window, width));
        let desired = self.search_field.scroll_x() + f32::from(delta);
        self.search_field.set_scroll_x(desired, max)
    }
}

/// gpui 的输入协议桥：把平台的 UTF-16 区间换算交给 [`TextField`]，
/// 这里只留「平台说了什么」→「状态怎么变」这一层，外加焦点门禁。
impl EntityInputHandler for Root {
    fn text_for_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        actual_range: &mut Option<std::ops::Range<usize>>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        if !self.search_input_focused(window) {
            return None;
        }
        let range = self.search_field.byte_range_from_utf16(range_utf16);
        actual_range.replace(self.search_field.utf16_range_from_byte(range.clone()));
        Some(self.search_field.slice(range))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if !self.search_input_focused(window) {
            return None;
        }
        // 组合中上报组合区间，否则上报选区（无选区时是活动端的折叠区间）。
        // `reversed` 要照实给：方向决定 Shift 继续扩选时往哪边长，macOS 的候选窗也读它。
        Some(UTF16Selection {
            range: self
                .search_field
                .utf16_range_from_byte(self.search_field.selection_range()),
            reversed: self.search_field.is_reversed(),
        })
    }

    fn marked_text_range(
        &self,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        if !self.search_input_focused(window) {
            return None;
        }
        self.search_field
            .marked()
            .map(|range| self.search_field.utf16_range_from_byte(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.clear_marked() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<std::ops::Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.search_input_focused(window) {
            return;
        }
        let range = range_utf16.map(|range| self.search_field.byte_range_from_utf16(range));
        let now = self.search_edit_time();
        self.search_field.begin_edit();
        self.search_field.replace(range, text);
        self.search_field.end_edit(now);
        self.search_fit_caret_in_view(window);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range_utf16: Option<std::ops::Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.search_input_focused(window) {
            return;
        }
        let range = range_utf16.map(|range| self.search_field.byte_range_from_utf16(range));
        let now = self.search_edit_time();
        self.search_field.begin_edit();
        self.search_field.replace_and_mark(range, new_text);
        self.search_field.end_edit(now);
        self.search_fit_caret_in_view(window);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        element_bounds: gpui::Bounds<gpui::Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        if !self.search_input_focused(window) {
            return None;
        }
        // 候选窗要贴在真正在编辑的那一段下面：整框宽度当锚点会让候选窗一直停在框的左下，
        // 滚过长文本之后更是完全对不上。这里与 `search_cursor_for_x` 共用同一条排版结果。
        let range = self.search_field.byte_range_from_utf16(range_utf16);
        let left = self.search_text_x(range.start, window) - self.search_scroll_x();
        let right = self.search_text_x(range.end, window) - self.search_scroll_x();
        let bounds = self.search_bounds.get().unwrap_or(element_bounds);
        let width = gpui::px(1.0).max(right - left);
        Some(gpui::Bounds {
            origin: gpui::point(bounds.origin.x + left, bounds.origin.y),
            size: gpui::size(width, bounds.size.height.max(gpui::px(1.0))),
        })
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if !self.search_input_focused(window) {
            return None;
        }
        // 语义是 UTF-16 偏移：按点击位置算出字节下标后再换算。
        let byte = self.search_cursor_for_x(point.x, window);
        Some(self.search_field.byte_to_utf16(byte))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.search_input_focused(window) {
            return;
        }
        let range = self.search_field.byte_range_from_utf16(range_utf16);
        // 平台交回的是整段区间，此前只取 `start` 会把选区丢掉；这里按真选区收下，
        // 活动端落在区间末尾（方向由两端的大小关系决定）。
        self.search_field.set_position(range.end, range.start);
        self.search_fit_caret_in_view(window);
        cx.notify();
    }

    fn text_length_utf16(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        if !self.search_input_focused(window) {
            return None;
        }
        Some(self.search_field.utf16_len())
    }

    fn accepts_text_input(&self, window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.search_input_focused(window)
    }
}

impl Root {
    pub fn tr(&self, text: &'static str) -> &'static str {
        self.language.text(text)
    }

    /// 听歌模式展示名：中文环境用服务端名，英文环境用固定映射。
    pub fn scene_display_text(&self, sub_queue_type: &str, fallback: &str) -> String {
        scene_mode_text(self.language, sub_queue_type, fallback).to_string()
    }

    pub fn localized(&self, key: &'static str, args: &[String]) -> String {
        self.language.textf(key, args)
    }

    pub fn set_status(&mut self, key: &'static str, args: &[String]) {
        self.set_status_text(self.localized(key, args));
    }

    /// 普通操作反馈：只进入底部状态行，不触发全局 message。
    pub fn set_status_text(&mut self, text: impl Into<String>) {
        self.status = sodam_core::failure::strip_urls_for_user(&text.into());
        self.show_status();
    }

    /// 技术原文的唯一去处：诊断日志（默认开，写在本机日志目录）。
    /// 界面文案一律不用它——用户只看人话。
    ///
    /// 话题是 `app`：设置保存、登录、搜索、歌单这些非播放链路的失败都归这里。
    /// 播放链路的失败另有 `player` 话题的尝试轨迹，不靠这条。
    /// 进这里的内容不需要自己脱敏：日志出口在写盘前统一切断查询串与凭据。
    pub(crate) fn log_failure(&self, context: &str, error: impl std::fmt::Display) {
        let detail = error.to_string();
        log_event(
            LogLevel::Error,
            LogTopic::App,
            "failure",
            &[("context", context), ("detail", detail.as_str())],
        );
    }

    /// 记录技术原文，并返回给用户看的那一句。
    ///
    /// 界面文案一律走这里，不再复述 `{err}`：普通用户看 `tls connection init failed`
    /// 得不到任何行动信息，而它也不该被藏进折叠区——原文的去处是日志。
    pub(crate) fn failure_text(
        &self,
        context: &str,
        error: impl std::fmt::Display,
        text: &'static str,
    ) -> String {
        self.log_failure(context, error);
        self.tr(text).to_string()
    }

    /// 重要成功结果：状态 + 全局轻提示。
    #[allow(dead_code)] // 公共 API 给不重复的 user-triggered success
    pub fn notify_success(&mut self, text: impl Into<String>) -> u64 {
        self.notify_message(ui::message::MessageKind::Success, text.into(), None)
    }

    /// 同一来源的成功提示只保留一条。
    pub fn notify_success_once(&mut self, key: &str, text: impl Into<String>) -> u64 {
        self.notify_message(ui::message::MessageKind::Success, text.into(), Some(key))
    }

    /// 重要警告：状态 + 全局轻提示。
    #[allow(dead_code)] // 公共 API 给不重复的 user-triggered warning
    pub fn notify_warning(&mut self, text: impl Into<String>) -> u64 {
        self.notify_message(ui::message::MessageKind::Warning, text.into(), None)
    }

    /// 同一来源的警告只保留一条。
    pub fn notify_warning_once(&mut self, key: &str, text: impl Into<String>) -> u64 {
        self.notify_message(ui::message::MessageKind::Warning, text.into(), Some(key))
    }

    /// 需要用户注意的可恢复错误：状态 + 全局轻提示。
    #[allow(dead_code)] // 留给不重复的 user-triggered errors
    pub fn notify_error(&mut self, text: impl Into<String>) -> u64 {
        self.notify_message(ui::message::MessageKind::Error, text.into(), None)
    }

    /// 同一来源的重复错误替换旧提示，避免轮询/重试堆成消息墙。
    pub fn notify_error_once(&mut self, key: &str, text: impl Into<String>) -> u64 {
        self.notify_message(ui::message::MessageKind::Error, text.into(), Some(key))
    }

    fn notify_message(
        &mut self,
        kind: ui::message::MessageKind,
        text: String,
        key: Option<&str>,
    ) -> u64 {
        // 上屏前摘掉接口地址：内部端点对普通用户既无意义又泄露拓扑；带签名 token 的查询串
        // 更早就不该出现。给维护者看的原文留给日志（`PlaybackFailure::detail`）。
        self.status = sodam_core::failure::strip_urls_for_user(&text);
        self.show_status();
        self.push_message_with_key(
            kind,
            self.status.clone(),
            Some(ui::message::DEFAULT_DURATION),
            key,
        )
    }

    /// 记录最新状态。
    pub(crate) fn show_status(&mut self) {
        self.status_seq = self.status_seq.wrapping_add(1);
        self.status_since = Some(std::time::Instant::now());
    }

    pub fn set_language(&mut self, language: Language, cx: &mut Context<Self>) {
        self.language = language;
        self.language_follows_system = false;
        self.settings.language = language.key().to_string();
        let save_result = self.settings.save();
        match save_result {
            Ok(()) => {
                let status = if language.is_zh() {
                    language.text("语言已切换为中文").to_string()
                } else {
                    language.text("Language switched to English").to_string()
                };
                self.set_status_text(status);
            }
            Err(err) => {
                let message = self.failure_text("语言已切换，但保存失败", &err, SAVE_TEXT);
                self.notify_warning_once("language-save-failed", message);
            }
        }
        // macOS 菜单栏的标题也要跟着换语言。
        #[cfg(target_os = "macos")]
        crate::menus::refresh(cx, language);
        cx.notify();
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Windows 自绘标题栏（系统标题栏被 WM_NCCALCSIZE 去掉）；macOS 用系统红绿灯。
        // Linux 交给设置项：默认 auto 跟随桌面协商结果 —— GNOME 这类要求客户端装饰的
        // 桌面需要自绘，Hyprland/sway 这类平铺桌面协商成服务端装饰但不画标题栏，
        // 用户也可以在设置里强制显示或隐藏。
        let own_titlebar = if cfg!(target_os = "windows") {
            true
        } else if cfg!(target_os = "macos") {
            false
        } else {
            sodam_core::config::TitlebarMode::from_key(&self.settings.titlebar).draws_titlebar(
                matches!(window.window_decorations(), Decorations::Client { .. }),
            )
        };
        let titlebar_anchor = self.titlebar_anchor.clone();
        // 没有元素聚焦时把焦点收回根元素：gpui 的上下文栈取自聚焦节点的路径，
        // 空栈会让带谓词的快捷键绑定（播放三键）全部失效。
        if window.focused(cx).is_none() {
            window.focus(&self.workspace_focus, cx);
        }
        // Ctrl+F 的焦点不在动作里落：从别的页过来时搜索框要到这一帧的渲染才进 dispatch
        // tree，早一帧 focus 拿不到 `input` 上下文。这里与上面收回根焦点是同一处时机。
        if self.search_focus_pending {
            self.search_focus_pending = false;
            window.focus(&self.search_focus, cx);
        }
        // 开发期帧耗时探针（`SODAM_FRAME_LOG=1` 把 app 话题提到 DEBUG）：渲染超过 16ms 就打一行。
        // 排查「UI 卡死」先看这里，而不是靠感觉。
        let frame_started = std::time::Instant::now();
        if self.theme_follows_system {
            self.theme = theme::theme_from_appearance(window.appearance());
        }
        theme::set_theme(self.theme);
        if self.nav == Nav::Scenes {
            self.sync_scenes_list();
        }
        let ambient_url = self.queue.current().map(|track| track.cover.clone());
        let ambient_color = ambient_url
            .as_deref()
            .and_then(|url| self.ambient_colors.get(url).copied());
        if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
            let current = self
                .queue
                .current()
                .map(|track| track.title.clone())
                .unwrap_or_default();
            let url = ambient_url.clone().unwrap_or_default();
            let color = ambient_color
                .map(|rgb| format!("{rgb:?}"))
                .unwrap_or_else(|| "none".to_string());
            log_event(
                LogLevel::Debug,
                LogTopic::App,
                "theme_snapshot",
                &[
                    ("current", current.as_str()),
                    ("url", url.as_str()),
                    ("color", color.as_str()),
                ],
            );
        }
        theme::set_ambient_rgb(ambient_color);
        // 上一帧布局未就绪时记下的歌词对齐，在这里补做一次。
        self.flush_pending_lyrics_scroll();
        // 歌词滚动过渡进行中：用**帧回调**驱动下一帧，而不是等背景 timer。
        //
        // `background_executor().timer(16ms)` 不是帧同步的：Windows 默认计时器精度
        // 约 15.6ms，再叠上调度延迟，实际可能只有 30fps 且间隔不匀 —— 观感就是
        // "滑得一顿一顿"。`request_animation_frame` 对齐平台的刷新节奏，
        // 才是真正的逐帧动画。
        if self.lyrics_scroll_animating() {
            // 每帧推进一次插值，再请求下一帧（帧回调天然与 VSync 对齐）。
            if self.advance_lyrics_scroll_tween() {
                window.request_animation_frame();
            }
        }
        if self.nav == Nav::Lyrics {
            let position = if let Some(demo) = self.lyrics_demo_position {
                demo
            } else {
                // 音频线程每 100ms 才更新一次快照，而这里以 60fps 重绘。直接读快照会让
                // 逐字亮光每 100ms 才跳一格——肉眼看就是“一顿一顿”。
                // 这里按本地时钟在两次快照之间做线性外推，让高亮连续推进。
                //
                // 只在**播放中**外推：暂停时位置本就不该变。
                // “不回退”由 `extrapolate_position` 保证：`Instant` 单调，
                // 且快照值变化时会重新锚定，不会把旧外推叠加上去。
                let snapshot = self.engine.snapshot();
                let raw = snapshot.position_seconds;
                let now = std::time::Instant::now();
                if snapshot.playing && snapshot.duration_seconds > 0.0 {
                    let anchor = self.lyrics_position_anchor.unwrap_or((now, raw));
                    let (position, anchor) = sodam_core::models::extrapolate_position(
                        anchor,
                        raw,
                        now,
                        snapshot.duration_seconds,
                        LYRIC_LOOKAHEAD_SECONDS,
                    );
                    self.lyrics_position_anchor = Some(anchor);
                    position
                } else {
                    self.lyrics_position_anchor = Some((now, raw));
                    raw
                }
            };
            // 逐字高亮需要每帧的播放位置：存下来给 `lyrics_view` 用，
            // 不能在渲染函数里再取一次快照（那要多一次锁）。
            self.lyrics_position_seconds = position;
            // 高亮的"当前句"必须严格按**本句的时间区间**判定，不能带提前量：
            // 提前量会让上一句在还差几百毫秒时就被标成"已唱过"，于是它的逐字推进
            // 还没走完就被整行点亮（#80 实测："最后一个字一下亮了，然后立刻换下一句"）。
            //
            // 滚动跟随另有 250ms 提前量（见下），那是为了让下一句提前进入视野，
            // 两件事分开算。
            let active = self
                .lyrics
                .iter()
                .rposition(|line| position >= line.start_seconds)
                .or_else(|| (!self.lyrics.is_empty()).then_some(0));
            self.lyrics_active = active;
            // 用户刚手动滚过就让位，别把视图拽回去（官方同样是滚轮后 5 秒内不跟随）。
            let user_scrolling = self
                .lyrics_user_scroll_until
                .is_some_and(|until| std::time::Instant::now() < until);
            // 滚动比高亮早 250ms：下一句提前进入视野，避免"刚唱到就看它跳上来"。
            //
            // 注意这里的 `position` 是**外推后**的值（见上），可能最多超前
            // `LYRIC_LOOKAHEAD_SECONDS`。滚动以它为基准没关系——滚动本来就要提前；
            // 但高亮不能，否则行尾会提前走满再干等切换，看起来像"跳"。
            let scroll_target = self
                .lyrics
                .iter()
                .rposition(|line| position + 0.25 >= line.start_seconds);
            if let Some(index) = scroll_target {
                if !user_scrolling {
                    self.scroll_lyrics_to(index);
                }
            }
            // 切曲后强制大封面显示至少 200ms 的加载态（即便图已在缓存里）。
            if let Some(track) = self.queue.current() {
                let fresh = self
                    .lyrics_cover_gate
                    .as_ref()
                    .map(|(id, _)| id != &track.id)
                    .unwrap_or(true);
                if fresh {
                    self.lyrics_cover_gate = Some((track.id.clone(), std::time::Instant::now()));
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(220))
                            .await;
                        let _ = this.update(cx, |_, cx| cx.notify());
                    })
                    .detach();
                }
            }
        }
        let element = div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            // 根元素持焦点 + 挂基础上下文：带谓词的快捷键绑定要有非空上下文栈才生效。
            .track_focus(&self.workspace_focus)
            .key_context(crate::shortcuts::BASE_CONTEXT)
            // 焦点在窗口内的任何地方都能命中：根元素是全部聚焦节点的共同祖先。
            .on_action(cx.listener(Root::focus_search))
            .bg(theme::ambient_background())
            .text_color(ui::theme::text())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(ui::sidebar::render(self, cx, own_titlebar))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            // macOS：让红绿灯悬浮在内容之上，内容整体下移 28px。
                            .when(cfg!(target_os = "macos"), |this| this.pt(px(28.0)))
                            // 自绘标题栏：只下移内容，根背景和侧栏仍铺到标题栏顶部。
                            .when(own_titlebar, |this| this.pt(px(ui::titlebar::STRIP_HEIGHT)))
                            .child(views::render(self, window, cx)),
                    )
                    .when(self.queue_open && self.nav != Nav::Lyrics, |this| {
                        this.child(ui::player_bar::queue_drawer(self, cx, own_titlebar))
                    }),
            )
            .child(ui::player_bar::render(self, cx))
            // 标题栏覆盖在各面板背景之上；不再单独占一行而截断侧栏底色。
            // 放在普通内容之后、弹层之前，控制按钮可点击，弹层仍能覆盖标题栏。
            .when(own_titlebar, |this| {
                this.child(ui::titlebar::render(window, &titlebar_anchor))
            })
            .when(self.track_menu.is_some(), |this| {
                this.child(crate::views::track_menu(self, cx))
            })
            .when(
                self.login_modal_open && self.settings.cookie.trim().is_empty(),
                |this| this.child(crate::views::login_modal(self, cx)),
            )
            .when_some(ui::message::render(self, cx), |this, message| {
                this.child(message)
            })
            .when(
                self.queue_menu.is_some() || self.track_menu.is_some(),
                |this| {
                    // 透明遮罩：点任何地方都关掉右键菜单；
                    // 菜单本身层级更高（priority 2 > 1），点菜单不会走到这里。
                    this.child(
                        gpui::deferred(
                            div()
                                .id("menu-scrim")
                                .absolute()
                                .top(px(0.0))
                                .left(px(0.0))
                                .size_full()
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(|root, _event, _window, cx| {
                                        if root.queue_menu.is_some() || root.track_menu.is_some() {
                                            root.queue_menu = None;
                                            root.track_menu = None;
                                            cx.notify();
                                        }
                                    }),
                                ),
                        )
                        .with_priority(1),
                    )
                },
            );
        if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
            let elapsed = frame_started.elapsed().as_millis();
            if elapsed > 16 {
                let rows = match self.nav {
                    Nav::Search => self.results.len(),
                    Nav::Liked => self.liked.len(),
                    Nav::Library => self.playlists.len(),
                    _ => 0,
                };
                let elapsed_ms = elapsed.to_string();
                let nav = format!("{:?}", self.nav);
                let rows = rows.to_string();
                log_event(
                    LogLevel::Debug,
                    LogTopic::App,
                    "frame_slow",
                    &[
                        ("elapsed_ms", elapsed_ms.as_str()),
                        ("nav", nav.as_str()),
                        ("rows", rows.as_str()),
                    ],
                );
            }
        }
        element
    }
}
