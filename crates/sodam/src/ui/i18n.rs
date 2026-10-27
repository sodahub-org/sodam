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
        Self::english_text(key).unwrap_or(key)
    }

    /// 英文词典查表；`None` 表示没有这一条，界面会回退中文原文。
    #[allow(unreachable_patterns)]
    fn english_text(key: &str) -> Option<&'static str> {
        Some(match key {
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
            "就绪" => "Ready",
            "等待扫码…" => "Waiting for scan…",
            "已打开二次验证窗口，请在其中完成验证" => {
                "Second-verification window opened. Complete the verification there."
            }
            "正在播放的曲目不能从队列移除，可直接点「下一首」" => {
                "The playing track can't be removed from the queue. Use Next instead."
            }
            "已登录：{}（{}）" => "Signed in: {} ({})",
            "账号" => "Account",
            "{}；音质已按权益自动设为 {quality}" => "{}; quality set to {}",
            "主题已切换为{}" => "Theme switched to {}",
            "浅色" => "Light",
            "深色" => "Dark",
            "窗口" => "Window",
            "Linux 下是否自绘最小化 / 最大化 / 关闭按钮" => {
                "Whether SodaM draws its own minimize / maximize / close buttons on Linux"
            }
            "自动（跟随桌面）" => "Auto (follow desktop)",
            "由桌面是否要求客户端自绘决定：GNOME 等显示，Hyprland 等平铺桌面不显示" => {
                "Decided by whether the desktop asks apps to draw their own decorations: shown on GNOME, hidden on tiling desktops like Hyprland"
            }
            "总是显示" => "Always show",
            "即使桌面自己不管窗口，也显示最小化 / 最大化 / 关闭按钮" => {
                "Draw the buttons even when the desktop manages windows itself"
            }
            "不显示" => "Never show",
            "完全交给桌面管理窗口，不画任何按钮" => {
                "Leave window management entirely to the desktop"
            }
            "窗口按钮：自动（跟随桌面）" => "Window controls: auto (follow desktop)",
            "窗口按钮：总是显示" => "Window controls: always shown",
            "窗口按钮：不显示" => "Window controls: hidden",
            "主题已恢复为跟随系统" => "Theme restored to system",
            "「{}」没有可用歌词" => "No lyrics available for {}",
            "「{}」歌词已加载" => "Lyrics loaded for {}",
            "已切换到{}队列" => "Switched to {} queue",
            "正在读取听歌模式…" => "Loading listening modes…",
            "正在加载更多探索歌单…" => "Loading more discovery playlists…",
            "没有更多探索歌单" => "No more discovery playlists",
            "探索歌单已追加 {added} 张" => "Added {} discovery playlists",
            "正在加载「{}」队列…" => "Loading {} queue…",
            "推荐" => "Discover",
            "正在加载电台「{}」…" => "Loading {} radio…",
            "电台「{}」没有可播放曲目" => "Radio {} has no playable tracks",
            "歌单「{}」没有可播放曲目" => "Playlist {} has no playable tracks",
            "推荐队列已追加 {added} 首" => "Added {} recommendation tracks",
            "已退出登录" => "Signed out",
            "已收藏：{}" => "Liked: {}",
            "已取消收藏：{}" => "Unliked: {}",
            "已入队：{title}" => "Queued: {title}",
            "搜索歌单共 {count} 首" => "Search playlist: {} tracks",
            "下一首将播放：{title}" => "Play next: {}",
            "播放中" => "Playing",
            "已暂停" => "Paused",
            "试听" => "Preview",
            "地址解析（含签名）" => "stream resolution",
            "音频体传输" => "audio transfer",
            "网络传输" => "network transfer",
            "解密落盘" => "unpacking",
            "参数" => "input",
            "连续 3 首没播出来，已暂停" => "Paused after 3 tracks failed.",
            "暂时连不上，检查网络后重试。" => "Can't connect. Check your network and retry.",
            "出现了一点问题，检查网络后重试。" => "Something went wrong. Check your network.",
            "出现了一点问题，请重试。" => "Something went wrong. Try again.",
            "没能保存，检查磁盘空间后重试。" => "Couldn't save. Check your disk space.",
            "登录已过期，请重新登录。" => "Sign-in expired. Sign in again.",
            "这首歌暂时放不出来。签名服务不可用，可在设置里更换。" => {
                "This song can't play now. Change the signer service in Settings."
            }
            "这首歌暂时放不出来。" => "This song can't play now.",
            "这首歌在你的账号下无法播放。" => "Your account can't play this song.",
            "本地缓存写入失败，检查磁盘空间。" => "Cache write failed. Check disk space.",
            "打开验证窗口失败，请改用官方客户端导出 Cookie。" => {
                "Couldn't open the verification window. Export your cookie from the official app instead."
            }
            "暂停" => "Pause",
            "播放" => "Play",
            "上一首" => "Previous Track",
            "下一首" => "Next Track",
            "显示主窗口" => "Show Main Window",
            "退出" => "Quit",
            "窗口" => "Window",
            "最小化" => "Minimize",
            "关闭窗口" => "Close Window",
            "快捷键" => "Shortcuts",
            "播放 / 暂停" => "Play / Pause",
            "重绑" => "Rebind",
            "恢复默认" => "Reset",
            "取消" => "Cancel",
            "按下新的组合键…（Esc 取消）" => "Press a new shortcut… (Esc to cancel)",
            "播放控制：系统媒体键同样可用；这里绑的是应用内快捷键" => {
                "Playback: system media keys work too; these bind in-app shortcuts"
            }
            "播放控制：系统媒体键同样可用；⌃←/⌃→ 被系统占用（切换桌面），别绑这两个" => {
                "Playback: system media keys work too; ⌃←/⌃→ are taken by macOS (switch spaces)"
            }
            "快捷键已更新" => "Shortcut updated",
            "快捷键已恢复默认" => "Shortcut reset to default",
            "聚焦搜索框" => "Focus the search box",
            "徽标键由系统占用，请换个组合键" => {
                "The Windows/Super key belongs to the system — pick another combination"
            }
            "这个键无法作为快捷键，请换个组合键" => {
                "This key can't be used as a shortcut — pick another combination"
            }
            "这一行的按键无法解析，未生效" => {
                "This row's key can't be parsed, so it is not bound"
            }
            "该组合键已被「{}」占用" => "Shortcut already used by \"{}\"",
            "该组合键由 macOS 菜单保留，不能绑定" => "Reserved by the macOS menu",
            "未在播放" => "Nothing playing",
            "「{}」共 {} 条结果" => "{}: {} results",
            "来自汽水的推荐" => "Recommended for you",
            "按场景选歌" => "Choose by scene",
            "音乐人详情" => "Artist Details",
            "专辑详情" => "Album Details",
            "我喜欢的音乐" => "Liked Music",
            "{} 首 · {}" => "{} tracks · {}",
            "搜索歌曲、歌手、专辑或歌单" => "Search songs, artists, albums or playlists",
            "亿" => "B",
            "万" => "K",
            "{}人关注 · {}首歌" => "{} followers · {} songs",
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
            "维护者" => "Maintainer",
            "贡献者" => "Contributors",
            "正在加载贡献者…" => "Loading contributors…",
            "加载失败，点击重试" => "Failed to load. Click to retry.",
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
            "就绪" => "Ready",
            "等待扫码…" => "Waiting for scan…",
            "已打开二次验证窗口，请在其中完成验证" => {
                "Second-verification window opened. Complete the verification there."
            }
            "正在播放的曲目不能从队列移除，可直接点「下一首」" => {
                "The playing track can't be removed from the queue. Use Next instead."
            }
            "已登录：{}（{}）" => "Signed in: {} ({})",
            "非 VIP" => "Non-VIP",
            "{}；音质已按权益自动设为 {quality}" => "{}; quality set to {quality}",
            "主题已切换为{}" => "Theme switched to {}",
            "浅色" => "light",
            "深色" => "dark",
            "主题已恢复为跟随系统" => "Theme restored to system",
            "正在读取「{}」歌词…" => "Loading lyrics for “{}”…",
            "「{}」没有可用歌词" => "No lyrics available for “{}”",
            "「{}」歌词已加载" => "Lyrics loaded for “{}”",
            "已切换到{}队列" => "Switched to {} queue",
            "正在读取听歌模式…" => "Loading listening modes…",
            "正在加载更多探索歌单…" => "Loading more discovery playlists…",
            "没有更多探索歌单" => "No more discovery playlists",
            "探索歌单已追加 {} 张" => "Added {} discovery playlists",
            "「{}」队列" => "{} queue",
            "正在加载「{}」队列…" => "Loading {} queue…",
            "已准备播放：{}" => "Ready to play: {}",
            "「{}」电台" => "{} radio",
            "正在加载电台「{}」…" => "Loading radio “{}”…",
            "电台「{}」没有可播放曲目" => "Radio “{}” has no playable tracks",
            "正在加载歌单「{}」…" => "Loading playlist “{}”…",
            "歌单「{}」没有可播放曲目" => "Playlist “{}” has no playable tracks",
            "推荐队列已追加 {} 首" => "Added {} recommendation tracks",
            "已退出登录" => "Signed out",
            "共 {} 个歌单" => "{} playlists",
            "已收藏：{}" => "Liked: {}",
            "已取消收藏：{}" => "Unliked: {}",
            "我喜欢的音乐：{} 首" => "Liked Music: {} tracks",
            "已入队：{}" => "Queued: {}",
            "播放模式：{}" => "Play mode: {}",
            "列表循环" => "Loop queue",
            "单曲循环" => "Repeat one",
            "随机播放" => "Shuffle",
            "正在读取歌单「{}」…" => "Loading playlist “{}”…",
            "搜索歌单共 {} 首" => "Search playlist: {} tracks",
            "下一首将播放：{}" => "Play next: {}",
            "歌单" => "Playlist",
            "歌单共 {} 首" => "Playlist: {} tracks",
            "播放中" => "Playing",
            "已暂停" => "Paused",
            "正在准备播放：{}…" => "Preparing playback: {}…",
            "播放中：{}" => "Playing: {}",
            "先输入关键词再回车" => "Type a keyword and press Enter",
            "搜索历史" => "Search History",
            "仅保存在本机，最多保留 10 条；关闭后会清空记录" => {
                "Stored only on this device. Keeps up to 10 entries. Turning it off clears the history."
            }
            "保存搜索历史" => "Save search history",
            "已保存 {} 条搜索历史" => "{} saved searches",
            "搜索历史已清空" => "Search history cleared",
            "搜索历史已开启" => "Search history enabled",
            "搜索历史已关闭，记录已清空" => "Search history disabled; history cleared",
            "清空" => "Clear",
            "正在搜索「{}」…" => "Searching “{}”…",
            "「{}」共 {} 条结果" => "“{}”: {} results",
            "音乐人已加载" => "Artist loaded",
            "音乐人歌曲已加载" => "Artist tracks loaded",
            "专辑共 {} 首" => "Album: {} tracks",
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
            "搜索中…" => "Searching…",
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
            "亿" => "B",
            "万" => "K",
            "综合" => "All",
            "音乐人" => "Artists",
            "专辑" => "Albums",
            "{}人关注 · {}首歌" => "{} followers · {} songs",
            "搜索" => "Search",
            "电台" => "Radio",
            "正在加载…" => "Loading…",
            "加载更多探索歌单（当前 {} 张）" => "Load More Playlists ({} loaded)",
            "用户 {} · {}" => "User {} · {}",
            "中文" => "中文",
            "语言已恢复为跟随系统" => "Language restored to system",
            "共 {}（歌曲 {} 首 {} / 封面 {} 张 {}）" => "Total {} ({} songs {}, {} covers {})",
            "已清理缓存：{} 个文件" => "Cleared {} cache files",
            "音频缓存上限：{}" => "Audio cache limit: {}",
            "自动清理较早使用的歌曲；正在播放或下载的文件暂时保留" => "Older songs are removed automatically; files in use are kept until released",
            "清空缓存" => "Clear Cache",
            "正在清理…" => "Clearing…",
            "诊断日志" => "Diagnostic Logs",
            "默认记录在本机，用于排查播放问题；「清空缓存」不会删除它们" => {
                "Stored on this device to help diagnose playback problems; Clear Cache never removes them"
            }
            "日志目录：{}" => "Log directory: {}",
            "日志可能含接口地址（查询串已移除），请勿公开上传" => {
                "Logs may contain service URLs (query strings removed). Don't upload them publicly."
            }
            "打开日志目录" => "Open Log Folder",
            "清除诊断日志" => "Clear Diagnostic Logs",
            "正在清除…" => "Clearing…",
            "已清除诊断日志" => "Diagnostic logs cleared",
            "清除诊断日志失败，文件可能被其他程序占用" => {
                "Couldn't clear diagnostic logs. Another program may be using the files."
            }
            "音频输出长时间无响应，请重试播放" => "Audio output stopped responding. Try playing again.",
            "音频线程已退出，请重启应用" => "The audio worker stopped. Please restart the app.",
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
            "推荐队列还在加载，稍等一下…" => "Recommendations are still loading. One moment…",
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
            "SodaM" => "SodaM",
            "正在搜索…" => "Searching…",
            "从队列移除" => "Remove from Queue",
            "这首歌暂时没有歌词" => "No lyrics for this track",
            "当前跟随系统语言；手动选择后会固定语言" => {
                "Following system; choose a language to pin it"
            }
            "{}；音质已按权益自动设为 {}" => "{}; quality set to {}",
            _ => return None,
        })
    }

    /// 引擎错误文案的查表入口。
    ///
    /// 单独一张表是因为形状不同：`PlaybackSnapshot::error` 是运行时 `String`，而
    /// [`Self::text`] 的键是编译期 `&'static str`；放宽 `text` 的签名会连带
    /// `status_label` 等一批函数的返回类型，属于别人家的代码，不在本改动里。
    /// 表里的键必须与 `sodam-core/src/audio.rs` 写进快照的说法逐字一致。
    pub fn engine_error(self, message: &str) -> String {
        if self.resolved().is_zh() {
            return message.to_string();
        }
        let english = match message {
            "音频播放没能启动，请重启应用。" => {
                "Audio playback couldn't start. Restart the app."
            }
            "音频线程已退出，请重启应用" => {
                "The audio worker stopped. Please restart the app."
            }
            "音频输出中断了。" => "Audio output stopped.",
            "找不到可用的音频设备。" => "No audio device is available.",
            "音频设备不可用，请重新播放。" => {
                "The audio device is unavailable. Press play to try again."
            }
            "音频输出长时间无响应，请重试播放" => {
                "Audio output stopped responding. Try playing again."
            }
            "切换音频设备后没能继续播放。" => {
                "Playback didn't resume after switching the audio device."
            }
            "这首歌的文件读不出来，请重新播放。" => {
                "This song's file can't be read. Press play to try again."
            }
            "当前曲目没有可重新打开的缓存" => {
                "The current track has no cached file to reopen"
            }
            "音频源不支持后退/跳转" => {
                "This track can't seek backward or jump to a position"
            }
            "还没有可播放的曲目" => "Nothing to play yet",
            _ => message,
        };
        english.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 这几条的英文由调用点自己拼，词典里故意没有。
    const CALL_SITE_ONLY: &[&str] = &[
        "Shortcut reset to default",
        "Shortcut updated",
        "该组合键已用于「{}」，不能绑定",
    ];

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rs_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    /// 取出 `needle`（如 `.tr(`）后紧跟的字符串字面量；允许换行缩进。
    fn literal_keys<'a>(source: &'a str, needle: &str) -> Vec<&'a str> {
        let mut keys = Vec::new();
        let mut index = 0;
        while let Some(found) = source[index..].find(needle) {
            let after_needle = index + found + needle.len();
            let rest = &source[after_needle..];
            let trimmed = rest.trim_start();
            let skipped = rest.len() - trimmed.len();
            let Some(body) = trimmed.strip_prefix('"') else {
                index = after_needle;
                continue;
            };
            let Some(end) = body.find('"') else {
                break;
            };
            let key = &body[..end];
            if !key.chars().any(|c| matches!(c, '\\' | '\n' | '\r')) {
                keys.push(key);
            }
            index = after_needle + skipped + end + 2;
        }
        keys
    }

    /// 从 `start`（开括号之后）按顶层逗号切分参数；字符串字面量整体跳过。
    fn call_arguments(source: &str, start: usize) -> Option<Vec<&str>> {
        let bytes = source.as_bytes();
        let mut args = Vec::new();
        let mut depth = 0usize;
        let mut arg_start = start;
        let mut i = start;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    i += 1;
                    while i < bytes.len() {
                        match bytes[i] {
                            b'\\' => i += 2,
                            b'"' => {
                                i += 1;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' => {
                    if depth == 0 {
                        args.push(&source[arg_start..i]);
                        return Some(args);
                    }
                    depth -= 1;
                }
                b']' | b'}' => depth = depth.saturating_sub(1),
                b',' if depth == 0 => {
                    args.push(&source[arg_start..i]);
                    arg_start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// 取出 `needle`（如 `.failure_text(`）调用的第 `index` 个参数；只接受纯字符串字面量。
    fn nth_argument_literal<'a>(source: &'a str, needle: &str, index: usize) -> Vec<&'a str> {
        let mut keys = Vec::new();
        let mut search = 0;
        while let Some(found) = source[search..].find(needle) {
            let after_needle = search + found + needle.len();
            if let Some(arg) =
                call_arguments(source, after_needle).and_then(|args| args.get(index).copied())
            {
                let arg = arg.trim();
                let literal = arg
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .filter(|body| !body.chars().any(|c| matches!(c, '\\' | '"' | '\n' | '\r')));
                if let Some(body) = literal {
                    keys.push(body);
                }
            }
            search = after_needle;
        }
        keys
    }

    /// `failure_text` 的第三个参数才是用户可见文案；常量参数不参与字典检查。
    #[test]
    fn failure_text_visible_literal_is_scanned() {
        let source = r#"
            let a = root.failure_text("上下文", &err, "可见文案");
            let b = root.failure_text("上下文", &err, RETRY_TEXT);
            let c = root.failure_text(
                "多行上下文",
                &err,
                "多行可见文案",
            );
        "#;
        assert_eq!(
            nth_argument_literal(source, ".failure_text(", 2),
            vec!["可见文案", "多行可见文案"]
        );
    }

    /// 英文界面的静态文案必须都能在词典里查到，否则会静默回退中文。
    #[test]
    fn every_static_ui_key_has_an_english_entry() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        assert!(files.len() > 20, "没扫到源码文件，扫描逻辑可能已失效");

        let mut scanned = 0;
        let mut missing = Vec::new();
        for file in files {
            if file.ends_with("ui/i18n.rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            let mut keys = literal_keys(&source, ".tr(");
            keys.extend(literal_keys(&source, ".localized("));
            keys.extend(nth_argument_literal(&source, ".failure_text(", 2));
            for key in keys {
                scanned += 1;
                if Language::english_text(key).is_none() && !CALL_SITE_ONLY.contains(&key) {
                    missing.push(format!("{}: {key}", file.display()));
                }
            }
        }

        assert!(scanned > 100, "没扫到 UI 文案，扫描逻辑可能已失效");
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "英文词典缺少这些 key：\n{}",
            missing.join("\n")
        );
    }
}
