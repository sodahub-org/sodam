//! 应用根视图：状态定义、初始化、输入处理与顶层渲染。

#[path = "app_actions.rs"]
mod actions;
#[path = "app_search.rs"]
mod search_actions;

use gpui::prelude::*;
use gpui::{div, px, Context, EntityInputHandler, FocusHandle, Render, UTF16Selection, Window};
use sodam_core::{
    library::{RecommendationSource, SceneCard, SearchScope},
    login::{LoginStatus, QrMatrix},
    models::{
        AlbumDetail, ArtistDetail, LyricLine, PlaylistItem, SceneItem, SearchResults, TrackItem,
    },
    queue::Queue,
    session::{AccountInfo, Session},
    PlaybackEngine, Settings,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

/// 歌曲右键菜单状态：目标曲目与弹出位置。
pub struct TrackMenu {
    pub track: TrackItem,
    pub position: gpui::Point<gpui::Pixels>,
}

/// 根视图状态。
pub struct Root {
    pub nav: Nav,
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
    /// 官方推荐队列分页状态。
    pub recommendation: Option<RecommendationSource>,
    pub loading_recommendation: bool,
    /// 点推荐后的整页切换 loading；后台 load-more 不置位。
    pub switching_queue: Option<QueueOrigin>,
    pub switching_label: String,
    pub(crate) recommendation_request_seq: u64,
    /// 搜索框内容与焦点。
    pub lyrics: Arc<Vec<LyricLine>>,
    pub lyrics_track_id: String,
    pub lyrics_title: String,
    pub lyrics_artist: String,
    pub lyrics_cover: String,
    pub loading_lyrics: bool,
    pub lyrics_error: Option<String>,
    pub lyrics_active: Option<usize>,
    pub lyrics_scroll: gpui::UniformListScrollHandle,
    /// 防止快速换歌时旧歌词请求回写。
    lyrics_request_seq: u64,
    pub search_input: String,
    pub search_focus: FocusHandle,
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
    pub volume_before_mute: f32,
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
    /// Fcitx/IBus 等 IME 的搜索框预编辑范围（UTF-8 byte range）。
    pub(crate) search_marked_range: Option<std::ops::Range<usize>>,
    /// 播放请求序号：每次 start_track +1。
    ///
    /// 作用：加载是异步的（下载 + 解密），如果用户在 A 加载途中换了 B，
    /// A 完成时必须**作废**，否则会把正在播的 B 顶掉（实测踩过：
    /// B 播一会儿后突然跳回 A，封面也对不上）。
    pub(crate) play_seq: u64,
    /// 播放错误（显示在播放栏：常驻状态条已移除，错误必须有地方可见）。
    pub playback_error: Option<String>,
    /// 连续拉流失败次数（>=3 就停下，避免无限跳歌）。
    pub(crate) consecutive_failures: u32,
    /// 正在预取的曲目 id（避免重复排队；预取结果只写缓存，不改 UI）。
    pub(crate) prefetch_inflight: Option<String>,
    /// 正在加载（下载/解密）的曲目：非空时播放栏显示 loading，且进度条不可拖。
    pub pending_track: Option<TrackItem>,
    /// 已处理过的「播完」序号（配合引擎的 finished_seq 自动切歌）。
    pub(crate) last_finished_seq: u64,
    /// 拖动进度条时的预览位置（0.0~1.0）：拖动中只改它，松手才真跳。
    pub progress_preview: Option<f32>,
    /// 队列快照（含版本号）：避免抽屉每帧深拷贝整条队列。
    pub queue_cache: (u64, Arc<Vec<TrackItem>>),
    /// 已尝试过的封面 URL（成功在 covers 里，失败的也记下来，避免每帧重复排队）。
    pub cover_attempted: Arc<Mutex<HashSet<String>>>,
    /// 封面缓存：URL → 本地文件。**只由后台任务写入**，渲染路径只读，
    /// 绝不在 render 里发网络请求（否则主线程会被下载卡住）。
    pub covers: Arc<HashMap<String, PathBuf>>,
    pub original_covers: HashMap<String, PathBuf>,
    pub original_cover_attempted: Arc<Mutex<HashSet<String>>>,
    pub ambient_colors: HashMap<String, (u8, u8, u8)>,
    pub(crate) cover_pool: CoverPool,
    /// 各列表当前渲染的行数（点「显示更多」时递增）。
    /// 扫码登录状态与轮询中的 token。
    pub login: LoginState,
    pub account: Option<AccountInfo>,
    /// 音频播放引擎（rodio，跑在专属线程上）
    pub engine: PlaybackEngine,
}

impl Root {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = Settings::load().merged_with_env();
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
        let queue = Queue::new(Vec::<TrackItem>::new());

        let logged_in = !settings.cookie.trim().is_empty();
        let mut root = Self {
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
            search_marked_range: None,
            play_seq: 0,
            playback_error: None,
            consecutive_failures: 0,
            playing: false,
            recommendation: None,
            loading_recommendation: false,
            switching_queue: None,
            switching_label: language.text("推荐队列").to_string(),
            recommendation_request_seq: 0,
            status,
            lyrics: Arc::new(Vec::new()),
            lyrics_track_id: String::new(),
            lyrics_title: String::new(),
            lyrics_artist: String::new(),
            lyrics_cover: String::new(),
            loading_lyrics: false,
            lyrics_error: None,
            lyrics_active: None,
            lyrics_scroll: gpui::UniformListScrollHandle::new(),
            lyrics_request_seq: 0,
            search_input: String::new(),
            search_focus: cx.focus_handle(),
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
            volume_before_mute: 1.0,
            progress_track_bounds: Arc::new(Mutex::new(None)),
            volume_track_bounds: Arc::new(Mutex::new(None)),
            prefetch_inflight: None,
            pending_track: None,
            last_finished_seq: 0,
            progress_preview: None,
            queue_cache: (0, Arc::new(Vec::new())),
            cover_attempted: Arc::new(Mutex::new(HashSet::new())),
            cover_requests: Arc::new(Mutex::new(Vec::new())),
            covers: Arc::new(HashMap::new()),
            original_covers: HashMap::new(),
            original_cover_attempted: Arc::new(Mutex::new(HashSet::new())),
            ambient_colors: HashMap::new(),
            cover_pool: CoverPool::new(),
            login: LoginState::Idle,
            account: None,
            engine: PlaybackEngine::new(),
        };
        if logged_in {
            root.refresh_account(cx);
            root.load_liked_ids(cx);
            root.start_recommendation(false, cx);
        } else {
            root.status = root.tr("请先在「设置 → 账户」扫码登录").to_string();
        }
        // 开发/验证便利：`SODAM_QUERY="周杰伦" cargo run -p sodam` 启动即搜索一次
        if let Ok(query) = std::env::var("SODAM_QUERY") {
            if !query.trim().is_empty() {
                root.search_input = query;
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
        // 配合 `SODAM_FRAME_LOG=1` 就能量出「滑动是否掉帧」，不用手工滚。
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
                    if std::env::var("SODAM_PLAYER_LOG").is_ok() {
                        eprintln!("[player] 竞态测试：缓存里找不到两首未下载的歌，跳过");
                    }
                    return;
                };
                if std::env::var("SODAM_PLAYER_LOG").is_ok() {
                    eprintln!("[player] 竞态测试：A=#{}, B=#{}", picks[0], picks[1]);
                }
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
        let range = self.search_byte_range_from_utf16(range_utf16);
        actual_range.replace(self.search_utf16_range_from_byte(range.clone()));
        Some(self.search_input[range].to_string())
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
        let range = self
            .search_marked_range
            .clone()
            .unwrap_or(self.search_input.len()..self.search_input.len());
        Some(UTF16Selection {
            range: self.search_utf16_range_from_byte(range),
            reversed: false,
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
        self.search_marked_range
            .as_ref()
            .map(|range| self.search_utf16_range_from_byte(range.clone()))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.search_marked_range.take().is_some() {
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
        let range = self.search_replacement_range(range_utf16);
        self.search_input.replace_range(range, text);
        self.search_marked_range = None;
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
        let range = self.search_replacement_range(range_utf16);
        let start = range.start;
        self.search_input.replace_range(range, new_text);
        let end = start + new_text.len();
        self.search_marked_range = Some(start..end);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: gpui::Bounds<gpui::Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.search_input_focused(window).then_some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if !self.search_input_focused(window) {
            return None;
        }
        Some(self.search_input.chars().map(char::len_utf16).sum())
    }

    fn text_length_utf16(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        if !self.search_input_focused(window) {
            return None;
        }
        Some(self.search_input.chars().map(char::len_utf16).sum())
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
        self.status = self.localized(key, args);
    }

    pub fn set_language(&mut self, language: Language, cx: &mut Context<Self>) {
        self.language = language;
        self.language_follows_system = false;
        self.settings.language = language.key().to_string();
        let save_result = self.settings.save();
        self.status = match save_result {
            Ok(()) => {
                if language.is_zh() {
                    language.text("语言已切换为中文").to_string()
                } else {
                    language.text("Language switched to English").to_string()
                }
            }
            Err(err) => {
                if self.language.is_zh() {
                    language.textf("语言已切换，但保存失败：{err}", &[err.to_string()])
                } else {
                    format!("Language switched, but saving failed: {err}")
                }
            }
        };
        cx.notify();
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 开发期帧耗时探针（`SODAM_FRAME_LOG=1`）：渲染超过 16ms 就打一行。
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
        if std::env::var("SODAM_THEME_LOG").is_ok() {
            eprintln!(
                "[theme] current={:?} url={:?} color={:?}",
                self.queue.current().map(|track| track.title.clone()),
                ambient_url,
                ambient_color
            );
        }
        theme::set_ambient_rgb(ambient_color);
        if self.nav == Nav::Lyrics {
            let position = self.engine.snapshot().position_seconds;
            let active = self
                .lyrics
                .iter()
                .rposition(|line| position + 0.25 >= line.start_seconds)
                .or_else(|| (!self.lyrics.is_empty()).then_some(0));
            self.lyrics_active = active;
            if let Some(index) = active {
                self.lyrics_scroll
                    .scroll_to_item_strict(index, gpui::ScrollStrategy::Center);
            }
        }
        let element = div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            // macOS 隐藏了系统标题栏，顶部留出红绿灯的高度。
            .when(cfg!(target_os = "macos"), |this| this.pt(px(28.0)))
            .bg(theme::ambient_background())
            .text_color(ui::theme::text())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(ui::sidebar::render(self, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(views::render(self, window, cx)),
                    )
                    .when(self.queue_open && self.nav != Nav::Lyrics, |this| {
                        this.child(ui::player_bar::queue_drawer(self, cx))
                    }),
            )
            .child(ui::player_bar::render(self, cx))
            .when(self.track_menu.is_some(), |this| {
                this.child(crate::views::track_menu(self, cx))
            })
            .when(
                self.login_modal_open && self.settings.cookie.trim().is_empty(),
                |this| this.child(crate::views::login_modal(self, cx)),
            )
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
        if std::env::var("SODAM_FRAME_LOG").is_ok() {
            let elapsed = frame_started.elapsed().as_millis();
            if elapsed > 16 {
                let rows = match self.nav {
                    Nav::Search => self.results.len(),
                    Nav::Liked => self.liked.len(),
                    Nav::Library => self.playlists.len(),
                    _ => 0,
                };
                eprintln!("[frame] {elapsed}ms nav={:?} rows={rows}", self.nav);
            }
        }
        element
    }
}
