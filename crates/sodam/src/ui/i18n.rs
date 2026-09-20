//! 极简中英文案层。中文原文作为 key，便于在 UI 里渐进接入。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    System,
    #[default]
    Chinese,
    English,
}

impl Language {
    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "auto" | "system" => Some(Self::System),
            "zh" | "zh-cn" | "chinese" => Some(Self::Chinese),
            "en" | "en-us" | "english" => Some(Self::English),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::System => "auto",
            Self::Chinese => "zh",
            Self::English => "en",
        }
    }

    pub fn system_locale() -> Self {
        sys_locale::get_locale()
            .as_deref()
            .map(|locale| {
                if locale.to_ascii_lowercase().starts_with("zh") {
                    Self::Chinese
                } else {
                    Self::English
                }
            })
            .unwrap_or(Self::Chinese)
    }

    pub fn resolved(self) -> Self {
        match self {
            Self::System => Self::system_locale(),
            language => language,
        }
    }

    pub fn is_zh(self) -> bool {
        self.resolved() == Self::Chinese
    }

    /// 静态文案翻译；中文原文作为稳定 key。
    pub fn text(self, key: &'static str) -> &'static str {
        let language = self.resolved();
        if language.is_zh() {
            return key;
        }
        #[allow(unreachable_patterns)]
        match key {
            "推荐" => "Discover",
            "听歌模式" => "Listening Modes",
            "搜索" => "Search",
            "我喜欢的音乐" => "Liked Music",
            "我的歌单" => "My Playlists",
            "音乐人" => "Artists",
            "专辑" => "Albums",
            "设置" => "Settings",
            "等待扫码" => "Waiting for scan",
            "已扫码" => "Scanned",
            "登录成功" => "Signed in",
            "已过期" => "Expired",
            "失败" => "Failed",
            "已配置登录与签名服务，可以去「搜索」或「推荐」了" => "Signed in and signer ready. Try Discover or Search.",
            "还没配好：请在「设置」里填 Cookie 与签名服务地址（或设 SODA_COOKIE / QISHUI_SIGNER_URL）" => "Not ready: set Cookie and signer URL in Settings (or use SODA_COOKIE / QISHUI_SIGNER_URL).",
            "推荐队列" => "Recommendation queue",
            "请先在「设置 → 账户」扫码登录" => "Sign in with QR code in Settings → Account.",
            "语言已切换为中文" => "Language switched to Chinese",
            "语言已切换，但保存失败：{err}" => "Language switched, but saving failed: {err}",
            "就绪" => "Ready",
            "创建二维码失败：{err}" => "Failed to create QR code: {err}",
            "二维码编码失败：{err}" => "Failed to encode QR code: {err}",
            "等待扫码…" => "Waiting for scan…",
            "轮询异常（会自动重试）：{err}" => "Polling error (retrying): {err}",
            "登录成功但保存失败：{err}" => "Signed in, but saving failed: {err}",
            "已打开二次验证窗口，请在其中完成验证" => {
                "Second-verification window opened. Complete the verification there."
            }
            "打开二次验证窗口失败：{err}，请改用官方客户端导出 Cookie" => {
                "Failed to open the verification window: {err}. Export a cookie from the official client instead."
            }
            "正在播放的曲目不能从队列移除，可直接点「下一首」" => {
                "The playing track can't be removed from the queue. Use Next instead."
            }
            "已登录：{}（{}）" => "Signed in: {} ({})",
            "账号" => "Account",
            "{}；音质已按权益自动设为 {quality}" => "{}; quality set to {}",
            "音质偏好保存失败：{err}" => "Failed to save audio quality: {err}",
            "读取账号信息失败：{err}" => "Failed to load account: {err}",
            "主题已切换为{}" => "Theme switched to {}",
            "浅色" => "Light",
            "深色" => "Dark",
            "主题已切换，但保存失败：{err}" => "Theme switched, but saving failed: {err}",
            "已用系统默认应用打开配置文件" => "Opened config with the default app",
            "打开配置文件失败：{err}" => "Failed to open config: {err}",
            "已在浏览器打开 GitHub 仓库" => "Opened GitHub repository",
            "打开 GitHub 仓库失败：{err}" => "Failed to open GitHub repository: {err}",
            "「{}」没有可用歌词" => "No lyrics available for {}",
            "「{}」歌词已加载" => "Lyrics loaded for {}",
            "已切换到{}队列" => "Switched to {} queue",
            "正在读取听歌模式…" => "Loading listening modes…",
            "正在加载更多探索歌单…" => "Loading more discovery playlists…",
            "没有更多探索歌单" => "No more discovery playlists",
            "探索歌单已追加 {added} 张" => "Added {} discovery playlists",
            "读取听歌模式失败：{err}" => "Failed to load listening modes: {err}",
            "正在加载「{}」队列…" => "Loading {} queue…",
            "加载听歌模式队列失败：{err}" => "Failed to load listening-mode queue: {err}",
            "推荐" => "Discover",
            "加载推荐队列失败：{err}" => "Failed to load recommendation queue: {err}",
            "正在加载电台「{}」…" => "Loading {} radio…",
            "电台「{}」没有可播放曲目" => "Radio {} has no playable tracks",
            "加载电台失败：{err}" => "Failed to load radio: {err}",
            "歌单「{}」没有可播放曲目" => "Playlist {} has no playable tracks",
            "加载歌单失败：{err}" => "Failed to load playlist: {err}",
            "推荐队列已追加 {added} 首" => "Added {} recommendation tracks",
            "加载更多推荐失败：{err}" => "Failed to load more recommendations: {err}",
            "已退出登录" => "Signed out",
            "已退出，但保存配置失败：{err}" => "Signed out, but saving failed: {err}",
            "读取歌单失败：{err}" => "Failed to load playlist: {err}",
            "已收藏：{}" => "Liked: {}",
            "已取消收藏：{}" => "Unliked: {}",
            "读取收藏失败：{err}" => "Failed to load liked music: {err}",
            "已入队：{title}" => "Queued: {title}",
            "搜索歌单共 {count} 首" => "Search playlist: {} tracks",
            "读取搜索歌单失败：{err}" => "Failed to load search playlist: {err}",
            "下一首将播放：{title}" => "Play next: {}",
            "播放中" => "Playing",
            "已暂停" => "Paused",
            "拉流失败" => "Stream failed",
            "「{}」拉流失败：{err}" => "Stream failed for {}: {err}",
            "连续 3 首拉流失败，已暂停（检查网络或签名服务）" => "Paused after 3 stream failures. Check network or signer.",
            "保存失败：{err}" => "Save failed: {err}",
            "暂停" => "Pause",
            "播放" => "Play",
            "上一首" => "Previous Track",
            "下一首" => "Next Track",
            "显示主窗口" => "Show Main Window",
            "退出" => "Quit",
            "「{}」共 {} 条结果" => "{}: {} results",
            "读取音乐人失败：{err}" => "Failed to load artist: {err}",
            "加载更多音乐人歌曲失败：{err}" => "Failed to load more artist tracks: {err}",
            "读取专辑失败：{err}" => "Failed to load album: {err}",
            "回车搜索歌曲、歌手或专辑" => "Press Enter to search songs, artists or albums",
            "来自汽水的推荐" => "Recommended for you",
            "按场景选歌" => "Choose by scene",
            "音乐人详情" => "Artist Details",
            "专辑详情" => "Album Details",
            "我喜欢的音乐" => "Liked Music",
            "{} 首 · {}" => "{} tracks · {}",
            "搜索歌曲 / 歌手（点这里后输入，回车搜索）" => "Search songs / artists (click, type, press Enter)",
            "亿" => "B",
            "万" => "K",
            "{}人关注 · {}首歌" => "{} followers · {} songs",
            "回车搜索歌曲、音乐人、专辑或歌单" => "Search songs, artists, albums or playlists",
            "{} 首" => "{} tracks",
            "用户 {} · {}" => "User {} · {}",
            "语言已恢复为跟随系统" => "Language restored to system",
            "共 {}（歌曲 {} 首 {} / 封面 {} 张 {}）" => "Total {} ({} songs {}, {} covers {})",
            "签名服务：{}" => "Signer: {}",
            "配置：{}" => "Config: {}",
            "加载更多探索歌单（当前 {} 张）" => "Load More Playlists ({} loaded)",
            "会话 {}…" => "Session {}…",
            "下一首播放" => "Play Next",
            "关闭" => "Close",
            "已固定主题；可随时切换深色或浅色" => "Theme pinned; switch anytime",
            "已固定语言；可随时切换中文或 English" => {
                "Language pinned; switch anytime"
            }
            "扫码登录" => "QR Login",
            "封顶无损；单曲没有无损时自动降级" => {
                "Up to lossless; downgrades automatically"
            }
            "封顶极高（≈320k）" => "Up to high (≈320k)",
            "封顶较高" => "Up to medium",
            "正在准备推荐队列…" => "Preparing recommendations…",
            "正在准备播放…" => "Preparing playback…",
            "正在创建二维码…" => "Creating QR code…",
            "正在播放" => "Now Playing",
            "搜索歌曲 / 歌手（点这里后输入，回车搜索）" => {
                "Search songs / artists (click, type, press Enter)"
            }
            "综合" => "All",
            "歌曲" => "Songs",
            "歌单" => "Playlists",
            "输入关键词进行搜索" => "Type a keyword to search",
            "没有找到相关内容" => "No matching results",
            "没有找到相关歌曲" => "No matching songs",
            "没有找到相关音乐人" => "No matching artists",
            "没有找到相关专辑" => "No matching albums",
            "没有找到相关歌单" => "No matching playlists",
            "搜索中…" => "Searching…",
            "歌单已关闭" => "Playlist closed",
            "还没有选择音乐人" => "No artist selected",
            "正在读取音乐人热歌…" => "Loading artist hits…",
            "暂时没有读到音乐人热歌" => "Artist hits unavailable",
            "还没有选择专辑" => "No album selected",
            "正在读取专辑歌曲…" => "Loading album tracks…",
            "暂时没有读到专辑歌曲" => "Album tracks unavailable",
            "加载更多歌曲" => "Load more songs",
            "播放全部" => "Play All",
            "加载更多探索歌单" => "Load More Playlists",
            "加载更多探索歌单（当前 {n} 张）" => "Load More Playlists ({n} loaded)",
            "正在加载…" => "Loading…",
            "重试" => "Retry",
            "重试加载" => "Retry",
            "常用模式" => "Frequently Used",
            "探索更多新模式" => "Explore More Modes",
            "正在读取常用模式…" => "Loading modes…",
            "常用模式暂不可用" => "Modes unavailable",
            "正在读取探索歌单…" => "Loading playlists…",
            "暂无探索歌单" => "No discovery playlists",
            "电台" => "Radio",
            "首" => "tracks",
            "账号" => "Account",
            "账号、签名服务与音质偏好" => "Account, signer and audio quality",
            "已登录" => "Signed in",
            "未登录" => "Signed out",
            "用户 {} · {}" => "User {} · {}",
            "VIP" => "VIP",
            "非 VIP" => "Non-VIP",
            "正在读取账号信息…" => "Loading account…",
            "登录后才能使用搜索、歌单与播放" => {
                "Sign in to use search, playlists and playback"
            }
            "去登录" => "Sign In",
            "退出登录" => "Sign Out",
            "主题" => "Theme",
            "当前跟随系统偏好，手动选择后会固定主题" => {
                "Following system; choose a theme to pin it"
            }
            "深色" => "Dark",
            "浅色" => "Light",
            "语言" => "Language",
            "跟随系统" => "System",
            "音质偏好" => "Audio Quality",
            "按账号权益和单曲实际可用的档位择优" => {
                "Choose the best usable tier for your account and each track"
            }
            "当前账号是 VIP：登录时默认无损，单曲没有无损会自动降级" => {
                "VIP account: lossless by default; downgrades automatically when unavailable"
            }
            "当前账号非 VIP：默认自动，取免费档里实际可用的最高一档" => {
                "Non-VIP account: Auto by default; uses the best free tier available"
            }
            "自动" => "Auto",
            "无损" => "Lossless",
            "极高" => "High",
            "较高" => "Medium",
            "标准" => "Low",
            "省流" => "Data saver",
            "缓存" => "Cache",
            "共 {}（歌曲 {} 首 {} / 封面 {} 张 {}）" => {
                "Total {} ({} songs {}, {} covers {})"
            }
            "清除歌曲缓存" => "Clear Song Cache",
            "其他" => "Other",
            "签名服务：{}" => "Signer: {}",
            "（未配置）" => "(not configured)",
            "配置：{}" => "Config: {}",
            "Edit" => "Edit",
            "GitHub: {}" => "GitHub: {}",
            "Author: ZephyrCheung" => "Author: ZephyrCheung",
            "歌手" => "Artist",
            "时长" => "Time",
            "这里还没有内容" => "Nothing here yet",
            "未登录：先在「登录」页扫码" => "Sign in with QR code first",
            "正在读取…" => "Loading…",
            "来自汽水的推荐" => "Recommended for you",
            "按场景选歌" => "Choose by scene",
            "音乐人详情" => "Artist Details",
            "专辑详情" => "Album Details",
            "还没有歌单" => "No playlists",
            "登录后这里会显示你的歌单" => "Your playlists appear after sign-in",
            "正在读取歌单…" => "Loading playlists…",
            "登录后这里会显示你收藏的歌曲" => "Liked songs appear after sign-in",
            "还没有喜欢的歌曲" => "No liked songs yet",
            "正在读取我喜欢的音乐…" => "Loading liked music…",
            "正在读取歌单曲目…" => "Loading playlist tracks…",
            "播放中" => "Playing",
            "已暂停" => "Paused",
            "请使用汽水音乐 App 扫码登录" => "Scan with the Qishui Music app",
            "会话 {}…" => "Session {}…",
            "等待扫码" => "Waiting for scan",
            "已扫码" => "Scanned",
            "登录成功" => "Signed in",
            "已过期" => "Expired",
            "失败" => "Failed",
            "现在播放" => "Now Playing",
            "歌词" => "Lyrics",
            "队列" => "Queue",
            "播放队列 · {} 首" => "Queue · {} tracks",
            "已播放 · {} 首" => "Played · {}",
            "接下来 · {} 首" => "Up Next · {}",
            "没有可用歌词" => "No lyrics available",
            "正在读取歌词…" => "Loading lyrics…",
            "设置已保存" => "Settings saved",
            "已配置登录与签名服务，可以去「搜索」或「推荐」了" => "Signed in and signer ready. Try Discover or Search.",
            "还没配好：请在「设置」里填 Cookie 与签名服务地址（或设 SODA_COOKIE / QISHUI_SIGNER_URL）" => "Not ready: set Cookie and signer URL in Settings (or use SODA_COOKIE / QISHUI_SIGNER_URL).",
            "推荐队列" => "Recommendation queue",
            "请先在「设置 → 账户」扫码登录" => "Sign in with QR code in Settings → Account.",
            "语言已切换为中文" => "Language switched to Chinese",
            "语言已切换，但保存失败：{err}" => "Language switched, but saving failed: {err}",
            "就绪" => "Ready",
            "创建二维码失败：{err}" => "Failed to create QR code: {err}",
            "二维码编码失败：{err}" => "Failed to encode QR code: {err}",
            "等待扫码…" => "Waiting for scan…",
            "轮询异常（会自动重试）：{err}" => "Polling error (retrying): {err}",
            "登录成功但保存失败：{err}" => "Signed in, but saving failed: {err}",
            "已打开二次验证窗口，请在其中完成验证" => {
                "Second-verification window opened. Complete the verification there."
            }
            "打开二次验证窗口失败：{err}，请改用官方客户端导出 Cookie" => {
                "Failed to open the verification window: {err}. Export a cookie from the official client instead."
            }
            "正在播放的曲目不能从队列移除，可直接点「下一首」" => {
                "The playing track can't be removed from the queue. Use Next instead."
            }
            "已登录：{}（{}）" => "Signed in: {} ({})",
            "非 VIP" => "Non-VIP",
            "{}；音质已按权益自动设为 {quality}" => "{}; quality set to {quality}",
            "音质偏好保存失败：{err}" => "Failed to save audio quality: {err}",
            "读取账号信息失败：{err}" => "Failed to load account: {err}",
            "主题已切换为{}" => "Theme switched to {}",
            "浅色" => "light",
            "深色" => "dark",
            "主题已切换，但保存失败：{err}" => "Theme switched, but saving failed: {err}",
            "已用系统默认应用打开配置文件" => "Opened config with the default app",
            "打开配置文件失败：{err}" => "Failed to open config: {err}",
            "已在浏览器打开 GitHub 仓库" => "Opened GitHub repository",
            "打开 GitHub 仓库失败：{err}" => "Failed to open GitHub repository: {err}",
            "正在读取「{}」歌词…" => "Loading lyrics for “{}”…",
            "「{}」没有可用歌词" => "No lyrics available for “{}”",
            "「{}」歌词已加载" => "Lyrics loaded for “{}”",
            "已切换到{}队列" => "Switched to {} queue",
            "正在读取听歌模式…" => "Loading listening modes…",
            "正在加载更多探索歌单…" => "Loading more discovery playlists…",
            "没有更多探索歌单" => "No more discovery playlists",
            "探索歌单已追加 {} 张" => "Added {} discovery playlists",
            "读取听歌模式失败：{err}" => "Failed to load listening modes: {err}",
            "「{}」队列" => "{} queue",
            "正在加载「{}」队列…" => "Loading {} queue…",
            "加载听歌模式队列失败：{err}" => "Failed to load listening-mode queue: {err}",
            "已准备播放：{}" => "Ready to play: {}",
            "加载推荐队列失败：{err}" => "Failed to load recommendation queue: {err}",
            "「{}」电台" => "{} radio",
            "正在加载电台「{}」…" => "Loading radio “{}”…",
            "电台「{}」没有可播放曲目" => "Radio “{}” has no playable tracks",
            "加载电台失败：{err}" => "Failed to load radio: {err}",
            "正在加载歌单「{}」…" => "Loading playlist “{}”…",
            "歌单「{}」没有可播放曲目" => "Playlist “{}” has no playable tracks",
            "加载歌单失败：{err}" => "Failed to load playlist: {err}",
            "推荐队列已追加 {} 首" => "Added {} recommendation tracks",
            "加载更多推荐失败：{err}" => "Failed to load more recommendations: {err}",
            "已退出登录" => "Signed out",
            "已退出，但保存配置失败：{err}" => "Signed out, but saving failed: {err}",
            "共 {} 个歌单" => "{} playlists",
            "已收藏：{}" => "Liked: {}",
            "已取消收藏：{}" => "Unliked: {}",
            "我喜欢的音乐：{} 首" => "Liked Music: {} tracks",
            "读取收藏失败：{err}" => "Failed to load liked music: {err}",
            "已入队：{}" => "Queued: {}",
            "正在读取歌单「{}」…" => "Loading playlist “{}”…",
            "搜索歌单共 {} 首" => "Search playlist: {} tracks",
            "读取搜索歌单失败：{err}" => "Failed to load search playlist: {err}",
            "下一首将播放：{}" => "Play next: {}",
            "歌单" => "Playlist",
            "歌单共 {} 首" => "Playlist: {} tracks",
            "播放中" => "Playing",
            "已暂停" => "Paused",
            "正在准备播放：{}…" => "Preparing playback: {}…",
            "播放中：{}" => "Playing: {}",
            "「{}」拉流失败：{err}" => "Stream failed for “{}”: {err}",
            "连续 3 首拉流失败，已暂停（检查网络或签名服务）" => "Paused after 3 stream failures. Check network or signer.",
            "保存失败：{err}" => "Save failed: {err}",
            "先输入关键词再回车" => "Type a keyword and press Enter",
            "正在搜索「{}」…" => "Searching “{}”…",
            "「{}」共 {} 条结果" => "“{}”: {} results",
            "搜索失败：{err}" => "Search failed: {err}",
            "音乐人已加载" => "Artist loaded",
            "读取音乐人失败：{err}" => "Failed to load artist: {err}",
            "音乐人歌曲已加载" => "Artist tracks loaded",
            "加载更多音乐人歌曲失败：{err}" => "Failed to load more artist tracks: {err}",
            "专辑共 {} 首" => "Album: {} tracks",
            "读取专辑失败：{err}" => "Failed to load album: {err}",
            "暂停" => "Pause",
            "播放" => "Play",
            "上一首" => "Previous Track",
            "下一首" => "Next Track",
            "显示主窗口" => "Show Main Window",
            "退出" => "Quit",
            "未在播放" => "Not playing",
            "选择一首歌开始播放吧" => "Choose a song to start playback",
            "已播放 · {} 首" => "Played · {}",
            "接下来 · {} 首" => "Up Next · {}",
            "播放队列 · {} 首" => "Play Queue · {} tracks",
            "我的音乐" => "My Music",
            "会话 {}…" => "Session {}…",
            "登录失败：{err}" => "Sign-in failed: {err}",
            "搜索中…" => "Searching…",
            "回车搜索歌曲、歌手或专辑" => "Press Enter to search songs, artists or albums",
            "{} 条结果" => "{} results",
            "未登录：先在「登录」页扫码" => "Sign in with QR code first",
            "正在读取…" => "Loading…",
            "{} 首" => "{} tracks",
            "{} 个歌单" => "{} playlists",
            "账号、签名服务与音质偏好" => "Account, signer and audio quality",
            "来自汽水的推荐" => "Recommended for you",
            "按场景选歌" => "Choose by scene",
            "音乐人详情" => "Artist Details",
            "专辑详情" => "Album Details",
            "搜索歌曲 / 歌手（点这里后输入，回车搜索）" => "Search songs / artists (click, type, press Enter)",
            "亿" => "B",
            "万" => "K",
            "综合" => "All",
            "音乐人" => "Artists",
            "专辑" => "Albums",
            "{}人关注 · {}首歌" => "{} followers · {} songs",
            "搜索" => "Search",
            "回车搜索歌曲、音乐人、专辑或歌单" => "Search songs, artists, albums or playlists",
            "电台" => "Radio",
            "正在加载…" => "Loading…",
            "加载更多探索歌单（当前 {} 张）" => "Load More Playlists ({} loaded)",
            "用户 {} · {}" => "User {} · {}",
            "中文" => "中文",
            "语言已恢复为跟随系统" => "Language restored to system",
            "共 {}（歌曲 {} 首 {} / 封面 {} 张 {}）" => "Total {} ({} songs {}, {} covers {})",
            "已清理缓存：{} 个文件" => "Cleared {} cache files",
            "签名服务：{}" => "Signer: {}",
            "配置：{}" => "Config: {}",
            "创建二维码失败，可点「重新获取二维码」重试" => {
                "Failed to create QR code. Try again."
            }
            "请用汽水音乐 App 扫码并确认" => {
                "Scan and confirm with the Qishui Music app."
            }
            "登录成功，正在读取账号信息…" => "Signed in. Loading account…",
            "登录成功（会话未初始化）" => "Signed in (session not initialized)",
            "二维码已过期，正在自动换新码…" => "QR code expired. Refreshing…",
            "还没有正在播放的歌曲" => "No song is playing",
            "正在读取听歌模式…" => "Loading listening modes…",
            "正在加载更多探索歌单…" => "Loading more discovery playlists…",
            "没有更多探索歌单" => "No more discovery playlists",
            "读取听歌模式失败：" => "Failed to load listening modes: ",
            "正在加载推荐队列…" => "Loading recommendation queue…",
            "加载推荐队列失败：" => "Failed to load recommendation queue: ",
            "加载电台失败：" => "Failed to load radio queue: ",
            "正在加载更多推荐…" => "Loading more recommendations…",
            "加载更多推荐失败：" => "Failed to load more recommendations: ",
            "已退出登录" => "Signed out",
            "需要先登录才能读取歌单" => "Sign in to load playlists",
            "读取歌单失败：" => "Failed to load playlists: ",
            "需要先登录才能读取收藏" => "Sign in to load liked music",
            "读取收藏失败：" => "Failed to load liked music: ",
            "队列是空的：先去搜索或打开一张歌单" => {
                "Queue is empty. Search or open a playlist."
            }
            "连续 3 首拉流失败，已暂停（检查网络或签名服务）" => {
                "Paused after 3 stream failures. Check network or signer."
            }
            "SodaM" => "SodaM",
            "正在搜索…" => "Searching…",
            "从队列移除" => "Remove from Queue",
            "这首歌暂时没有歌词" => "No lyrics for this track",
            "当前跟随系统语言；手动选择后会固定语言" => {
                "Following system; choose a language to pin it"
            }
            "{}；音质已按权益自动设为 {}" => "{}; quality set to {}",
            "收藏失败：{err}" => "Failed to update liked music: {err}",
            _ => key,
        }
    }
}

impl Language {
    fn normalize_placeholders(value: String) -> String {
        let chars: Vec<char> = value.chars().collect();
        let mut out = String::with_capacity(value.capacity());
        let mut index = 0;
        while index < chars.len() {
            if chars[index] != '{' {
                out.push(chars[index]);
                index += 1;
                continue;
            }
            if chars.get(index + 1) == Some(&'}') {
                out.push_str("{}");
                index += 2;
                continue;
            }
            let mut cursor = index + 1;
            while cursor < chars.len()
                && (chars[cursor].is_ascii_alphanumeric() || chars[cursor] == '_')
            {
                cursor += 1;
            }
            if cursor > index + 1 && chars.get(cursor) == Some(&'}') {
                out.push_str("{}");
                index = cursor + 1;
            } else {
                let end = (cursor + 1).min(chars.len());
                out.extend(&chars[index..end]);
                index = end;
            }
        }
        out
    }

    /// 用顺序占位符格式化；词典里的 `{err}` 这类命名占位符也会按顺序填充。
    pub fn textf(self, key: &'static str, args: &[String]) -> String {
        let mut value = Self::normalize_placeholders(self.text(key).to_string());
        for arg in args {
            value = value.replacen("{}", arg, 1);
        }
        value
    }
}

/// 官方 47 种听歌模式是固定集合；服务端只下发中文名，
/// 这里按稳定的 `sub_queue_type` 提供英文展示名。
pub fn scene_mode_text(language: Language, sub_queue_type: &str, fallback: &str) -> String {
    if language.resolved().is_zh() {
        return fallback.to_string();
    }
    // 服务端常见格式是 `scene_mode_emo`；本地熟悉/新鲜没有前缀。
    let sub_queue_type = sub_queue_type
        .strip_prefix("scene_mode_")
        .unwrap_or(sub_queue_type);
    match sub_queue_type {
        "familiar" => "Familiar".to_string(),
        "fresh" => "Fresh".to_string(),
        "bath" => "Bath Time".to_string(),
        "beach" => "Beach".to_string(),
        "bedtime" => "Bedtime".to_string(),
        "breakup" => "Breakup".to_string(),
        "calm" => "Calm".to_string(),
        "cantonese" => "Cantonese".to_string(),
        "car_mode" => "Car Mode".to_string(),
        "child" => "Kids".to_string(),
        "chill" => "Chill".to_string(),
        "chinese_style" => "Chinese Style".to_string(),
        "classic" => "Classical".to_string(),
        "clean_up" => "Clean Up".to_string(),
        "commute" => "Commute".to_string(),
        "country" => "Country".to_string(),
        "dj" => "DJ".to_string(),
        "douyin_roam" => "Douyin Roaming".to_string(),
        "drunk" => "Tipsy".to_string(),
        "electronic" => "Electronic".to_string(),
        "emo" => "Emo".to_string(),
        "english" => "English Songs".to_string(),
        "fish" => "Fishing".to_string(),
        "focus" => "Focus".to_string(),
        "folk" => "Folk".to_string(),
        "game" => "Gaming".to_string(),
        "get_up" => "Wake Up".to_string(),
        "happy" => "Happy".to_string(),
        "heal" => "Healing".to_string(),
        "jpop" => "J-Pop".to_string(),
        "kpop" => "K-Pop".to_string(),
        "ktv" => "KTV".to_string(),
        "library" => "Library".to_string(),
        "love_song" => "Love Songs".to_string(),
        "lucky" => "Lucky".to_string(),
        "lying_flat" => "Lying Flat".to_string(),
        "night_time" => "Late Night".to_string(),
        "non_vocal" => "Non-Vocal".to_string(),
        "nostalgic" => "Nostalgia".to_string(),
        "rain" => "Rain".to_string(),
        "rap" => "Rap".to_string(),
        "rnb" => "R&B".to_string(),
        "rock" => "Rock".to_string(),
        "slow_motion" => "Slow Motion".to_string(),
        "sport" => "Workout".to_string(),
        "sweet_girl" => "Sweet Girl".to_string(),
        "travel" => "Travel".to_string(),
        _ => fallback.to_string(),
    }
}
