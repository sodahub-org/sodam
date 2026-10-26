//! 会话层：把 [`Settings`] 变成可用的 libresoda 客户端。

use crate::config::Settings;
use anyhow::Context;
use libresoda::soda::signature::HttpSignature;
use libresoda::{AppCredentials, Soda};
use std::io::Read;
use std::sync::Arc;

/// 封面 URL → 缓存文件名用的稳定哈希（FNV-1a 64）。
/// 封面缓存的最大边长：列表缩略图 40px、卡片 156px，160 足够且解码开销极小。
const COVER_MAX_EDGE: u32 = 160;

/// 预取/播放两条路径可能**并发下载同一首**（预取进行中用户又手动点了它），
/// 固定的 .part 名会让两个写者互相踩踏（remove + 交错写 + 错误的 rename 对象）。
/// 用「进程号 + 进程内序号」保证每次下载写自己的临时文件。
fn part_file_name(track_id: &str, tag: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static PART_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = PART_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{track_id}-{tag}-{}-{seq}.m4a.part", std::process::id())
}

#[cfg(test)]
mod part_tests {
    #[test]
    fn part_names_are_unique_and_labeled() {
        let first = super::part_file_name("123", "lossless");
        let second = super::part_file_name("123", "lossless");
        assert_ne!(first, second, "同一曲目两次下载的临时名必须不同");
        assert!(first.starts_with("123-lossless-"), "{first}");
        assert!(first.ends_with(".m4a.part"), "{first}");
    }
}

fn hash_url(url: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 账号信息（登录后展示用）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountInfo {
    pub nickname: String,
    pub user_id: String,
    pub vip: bool,
    /// 登录用户头像 URL（来自 /luna/pc/me 的 larger_avatar_url）
    pub avatar_url: String,
}

/// 解析 sidecar `{档位}\t{字节}\t{0|1}`，与实际字节数核对后给出（档位，是否试听）。
///
/// 只认「非空文件」会让被截断的半截下载永久冒充有效缓存，表现为每次都从中间开始，
/// 所以尺寸不符或档位为空一律 miss，重下自愈。
/// 第三个字段是新加的：缺它说明是旧缓存，旧格式的试听标记拼在档位里（`无损 · 试听`），
/// 这里把它读出来、顺手洗回纯档位——已经下好的整曲不该因为多一个字段就被重下。
fn read_sidecar(text: &str, actual: u64) -> Option<(String, bool)> {
    let mut fields = text.split('\t');
    let quality = fields.next()?.trim().to_string();
    let size = fields.next()?.trim().parse::<u64>().ok()?;
    if size != actual || quality.is_empty() {
        return None;
    }
    let (quality, preview) = match fields.next() {
        Some(flag) => (quality, flag.trim() == "1"),
        None => (quality.replace(" · 试听", ""), quality.contains("试听")),
    };
    Some((quality, preview))
}

impl Session {
    /// 当前音质偏好对应的缓存标签（`auto` 表示未指定）。
    fn quality_tag(&self) -> String {
        let value = self.settings.quality.trim().to_ascii_lowercase();
        if value.is_empty() {
            "auto".to_string()
        } else {
            value
        }
    }

    /// 命中有效缓存：文件存在，且 sidecar 记录的字节数与实际一致。
    /// 只检查「非空文件」会让被截断的半截下载永久冒充有效缓存，
    /// 表现为同一首歌每次都从中间开始/中途跳下一首。
    /// 旧格式 sidecar（只有音质、没记大小）视为 miss：重下一次完成自愈。
    fn cached_info(&self, track_id: &str) -> Option<(std::path::PathBuf, String, bool)> {
        let path = crate::audio::cache_dir().join(format!("{track_id}-{}.m4a", self.quality_tag()));
        let actual = std::fs::metadata(&path).ok()?.len();
        if actual == 0 {
            return None;
        }
        let quality_path =
            crate::audio::cache_dir().join(format!("{track_id}-{}.quality", self.quality_tag()));
        let text = std::fs::read_to_string(&quality_path).ok()?;
        let (quality, preview) = read_sidecar(&text, actual)?;
        Some((path, quality, preview))
    }

    fn cached_track(&self, track_id: &str) -> Option<CachedTrack> {
        let path = crate::audio::cache_dir().join(format!("{track_id}-{}.m4a", self.quality_tag()));
        let lease = crate::cache::CacheLease::acquire(&path);
        let (path, quality, preview) = self.cached_info(track_id)?;
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(std::time::SystemTime::now()));
        Some(CachedTrack {
            path,
            quality,
            preview,
            lease,
        })
    }

    /// 这首是否已经在本地缓存里（**按当前音质档位**判断，
    /// 且要求 sidecar 大小校验一致；切换音质后旧档位的缓存不算命中）。
    pub fn is_cached(&self, track_id: &str) -> bool {
        // 这里只是调度提示，不返回可播放文件，也不建立租约或刷新 LRU 时间。
        self.cached_info(track_id).is_some()
    }
}

/// 一次播放要用的本地文件 + 它实际是什么音质。
#[derive(Debug, Clone, PartialEq)]
pub struct CachedTrack {
    pub path: std::path::PathBuf,
    /// 人类可读的实际音质（例：`无损 871k` / `极高 320k`），**不含试听标记**。
    pub quality: String,
    /// 这次拿到的是不是**试听片段**而不是整曲流。
    ///
    /// 它不是失败——播放确实成功了——但它解释了一个最常见的困惑：
    /// 「为什么三十秒就切歌了」。签名服务不可用或账号没有整曲权益时都会走到这里，
    /// 而且上游是**静默**降级的（返回 `Ok`），所以这个布尔必须一路带到界面。
    pub preview: bool,
    pub(crate) lease: crate::cache::CacheLease,
}

/// 把 `DownloadInfo` 翻成界面用的音质标签（以**实际拉到的流**为准）。
fn describe_quality(info: &libresoda::soda::types::DownloadInfo) -> String {
    let bitrate = info.bitrate.max(0) / 1000;
    let lossless = libresoda::soda::quality::is_lossless(info)
        || info.format.eq_ignore_ascii_case("flac")
        || info.quality.to_ascii_lowercase().contains("lossless");
    let tier = if lossless {
        "无损"
    } else if bitrate >= 300 {
        "极高"
    } else if bitrate >= 192 {
        "较高"
    } else if bitrate > 0 {
        "标准"
    } else {
        "未知"
    };
    // 只描述档位，不描述时长：**是不是试听片段由 `CachedTrack::preview` 作为布尔传递**。
    // 把 `· 试听` 拼进标签会让下游靠 `contains("试听")` 猜状态，那是我们刚消灭的东西。
    if bitrate > 0 {
        format!("{tier} {bitrate}k")
    } else {
        tier.to_string()
    }
}

/// 下载是否被截断：只看「实际字节数是否少于服务端声明」。
///
/// 个别曲目的服务端 `size` 元数据会小于真实资产（例如声明 10,569,897 字节、
/// 实际 CDN 返回 10,660,358 字节且 MP4 box 完整可播）。这类「实际更多」不是
/// 截断，不应拒绝；真正的风险只有实际更少（连接提前断开的静默 EOF）。
fn is_truncated_download(declared: i64, written: u64) -> bool {
    declared > 0 && written < declared as u64
}

/// 一次进程内共享的会话（持有 `Soda` 与当前设置）。
pub struct Session {
    pumpkin: Soda,
    settings: Settings,
}

impl Session {
    /// 用给定设置构建会话：注入 Cookie、设备指纹、签名器与音质偏好。
    pub fn new(settings: Settings) -> Self {
        let pumpkin = Soda::new(settings.cookie.clone());
        pumpkin.set_app_credentials(AppCredentials {
            device_id: settings.device_id.clone(),
            iid: settings.iid.clone(),
            fp: settings.fp.clone(),
            ..Default::default()
        });

        let url = settings.signer_url.trim();
        if !url.is_empty() {
            let provider = HttpSignature::new(url).with_token(settings.signer_token.trim());
            pumpkin.set_signature_provider(Arc::new(provider));
        }
        if !settings.quality.trim().is_empty() {
            pumpkin.set_quality_preference(settings.quality.trim());
        }
        // 登录必须走签名页（等价上游 Meting-API 的 signQishuiRequest）：本地直连护照接口
        // 会一路 error_code=7，且确认后的登录态只能由签名页的浏览器会话承接。
        // sodam 固定使用 libresoda 内置 Rust CDP 签名页：直控 Chromium，无 Node 依赖；
        // 浏览器路径可用 QISHUI_CHROMIUM_PATH 指定。
        pumpkin.enable_cdp_signer();

        Self { pumpkin, settings }
    }

    /// 创建扫码登录二维码（`scan_url` 由 UI 渲染成二维码）。
    pub fn create_qr_login(&self) -> anyhow::Result<libresoda::soda::qr_login::QrCreateResult> {
        self.pumpkin
            .create_qr()
            .map_err(|err| anyhow::anyhow!("创建二维码失败: {err}"))
    }

    /// 轮询扫码状态。
    pub fn check_qr_login(&self, token: &str) -> anyhow::Result<crate::login::LoginProgress> {
        use crate::login::{LoginProgress, LoginStatus};
        use libresoda::model::QRLoginStatus;

        let result = self
            .pumpkin
            .check_qr(token)
            .map_err(|err| anyhow::anyhow!("轮询失败: {err}"))?;
        let status = match result.status {
            QRLoginStatus::Waiting => LoginStatus::Waiting,
            QRLoginStatus::Scanned => LoginStatus::Scanned,
            QRLoginStatus::Success => LoginStatus::Success,
            QRLoginStatus::Expired => LoginStatus::Expired,
            QRLoginStatus::Failed => LoginStatus::Failed,
        };
        let need_second_verify = result
            .extra
            .get("need_second_verify")
            .map(|flag| flag == "true")
            .unwrap_or(false);
        let rate_limited = result
            .extra
            .get("rate_limited")
            .map(|flag| flag == "true")
            .unwrap_or(false);
        Ok(LoginProgress {
            status,
            message: result.message,
            cookie: result.cookie,
            need_second_verify,
            rate_limited,
        })
    }

    /// 打开二次验证窗口（系统浏览器），返回窗口地址。
    ///
    /// 仅在 [`Self::check_qr_login`] 返回 `need_second_verify=true` 后有意义；
    /// 窗口里跑官方验证组件，完成后下一次轮询会自动领取登录结果。
    /// 窗口误关时可重复调用再次打开。
    pub fn open_second_verify(&self, token: &str) -> anyhow::Result<String> {
        self.pumpkin
            .open_second_verify(token)
            .map_err(|err| anyhow::anyhow!("打开二次验证窗口失败: {err}"))
    }

    /// 当前账号是否为 VIP（用 `/luna/pc/me` 的 `my_info.is_vip`，失败时回落探测曲目）。
    pub fn is_vip(&self) -> anyhow::Result<bool> {
        self.pumpkin
            .is_vip_account()
            .map_err(|err| anyhow::anyhow!("VIP 探测失败: {err}"))
    }

    /// 拉取账号信息（昵称 / 用户 id / 是否 VIP）。
    ///
    /// VIP 判定与 libresoda 的 `IsVipAccount` 一致：先看 `/luna/pc/me` 的
    /// `my_info.is_vip` 与 `vip_stage`，拿不到再回落到探测曲目。
    pub fn fetch_account(&self) -> anyhow::Result<AccountInfo> {
        let me = libresoda::soda::user_playlist::fetch_pc_me(&self.pumpkin)
            .map_err(|err| anyhow::anyhow!("读取账号信息失败: {err}"))?;
        let vip = me.my_info.is_vip
            || matches!(
                me.my_info.vip_stage.trim().to_ascii_lowercase().as_str(),
                "vip" | "svip"
            )
            || self.is_vip().unwrap_or(false);
        let image = &me.my_info.larger_avatar_url;
        let mut avatar_url = image.urls.first().cloned().unwrap_or_default();
        if !avatar_url.is_empty() && !image.uri.is_empty() && !avatar_url.contains(&image.uri) {
            avatar_url.push_str(&image.uri);
        }
        Ok(AccountInfo {
            nickname: me.my_info.nickname.trim().to_string(),
            user_id: me.my_info.id.trim().to_string(),
            vip,
            avatar_url,
        })
    }

    /// 登录成功后写入 Cookie（落盘 + 重建会话）。
    pub fn apply_login_cookie(&mut self, cookie: &str) -> anyhow::Result<()> {
        let mut settings = self.settings.clone();
        settings.cookie = cookie.trim().to_string();
        self.apply(settings)
    }

    /// 按账号权益挑能用的最高档音质：
    ///
    /// * VIP：`lossless`（无损封顶；单曲没有无损时按档位从高到低择优）
    /// * 非 VIP：`""`（自动，取免费档里实际可用的最高一档）
    ///
    /// 用户手动设过 `quality` 时不覆盖（返回 `None`）。
    pub fn auto_quality_for(vip: bool) -> &'static str {
        if vip {
            // 无损封顶，但单曲没有无损时会往下降级匹配
            "lossless"
        } else {
            // "auto" 是显式值（和「未设置」区分开），否则每次登录都会被重写
            "auto"
        }
    }

    /// 刷新音质偏好：未手动指定时按 VIP 情况自动选。
    pub fn refresh_quality(&mut self, vip: bool) -> anyhow::Result<Option<String>> {
        if !self.settings.quality.trim().is_empty() {
            return Ok(None);
        }
        let quality = Self::auto_quality_for(vip).to_string();
        let mut settings = self.settings.clone();
        settings.quality = quality.clone();
        self.apply(settings)?;
        Ok(Some(quality))
    }

    /// 把曲目下载并**解密**到本地缓存，返回文件路径（已存在则直接复用）。
    ///
    /// libresoda 的 `download` 会处理加密流（VIP 整曲需要签名服务）。
    ///
    /// 失败返回 [`PlaybackFailure`]（阶段 + 可否重试 + 修复动作），不再压成一句
    /// 「下载失败」：一次播放请求串着地址解析、签名、音频体传输、解密落盘四类失败，
    /// 把它们混在一起既误导归因，也让永久错误白烧重试次数。
    /// 相位归因有上限：`download_with_info` 内部同时做了前三件事，
    /// 所以那一段的失败只能靠 `classify` 从 `SodaError` 的形态尽量还原。
    pub fn download_to_cache(
        &self,
        track: &crate::models::TrackItem,
    ) -> Result<CachedTrack, crate::failure::PlaybackFailure> {
        use crate::failure::{classify, PlaybackFailure, RecoveryAction, Retryability, Stage};

        let dir = crate::audio::cache_dir();
        std::fs::create_dir_all(&dir).map_err(|err| {
            PlaybackFailure::new(
                Stage::Unpack,
                Retryability::Never,
                RecoveryAction::FreeSpace,
                err.to_string(),
            )
        })?;
        // 缓存文件名带音质档位：切换音质后自然 miss，重新拉对应档位的流
        //（正在播放的那首不受影响，因为它已经装载进引擎了）。
        let tag = self.quality_tag();
        let path = dir.join(format!("{0}-{tag}.m4a", track.id));
        // sidecar 记录「音质标签 + 实际字节数 + 是否试听」，用于命中时的大小校验。
        let quality_path = dir.join(format!("{0}-{tag}.quality", track.id));
        let download_lock = crate::cache::download_lock(&path);
        let _writer = download_lock.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = self.cached_track(&track.id) {
            return Ok(cached);
        }
        let lease = crate::cache::CacheLease::acquire(&path);

        let song = libresoda::Song {
            id: track.id.clone(),
            name: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration: track.duration_seconds,
            source: libresoda::model::SOURCE_SODA.to_string(),
            link: format!("https://www.qishui.com/track/{}", track.id),
            extra: std::collections::BTreeMap::from([("track_id".to_string(), track.id.clone())]),
            ..Default::default()
        };
        // 先写临时文件，校验完整后再改名 —— 下载中断永远留不下「看起来有效」的半截缓存。
        // 临时名带进程号+序号：预取与手动播放并发下同一首时互不踩踏。
        // 注意：这里写的还是**明文**，所以 `Range` 续传在结构上还不成立（见 issue 阶段二）。
        let part = dir.join(part_file_name(&track.id, &tag));
        let _part_lease = crate::cache::CacheLease::acquire(&part);
        let info = self
            .pumpkin
            .download_with_info(&song, &part)
            .map_err(|err| {
                let _ = std::fs::remove_file(&part);
                classify(&err)
            })?;

        // 完整性校验：只拒绝「比服务端声明更少」的截断下载（静默 EOF 时可能发生）。
        // 允许实际字节数更多：个别曲目的 play_info size 元数据会滞后于真实资产
        //（实测存在声明 10,569,897 / 实际 10,660,358 的完整可播文件），等值校验会误杀。
        let written = std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
        if written == 0 || is_truncated_download(info.size, written) {
            let _ = std::fs::remove_file(&part);
            // 截断就是「传输没跑完」，属于典型的可重试；报成 Unpack 会把人引向磁盘问题。
            return Err(PlaybackFailure::new(
                Stage::Transfer,
                Retryability::Transient,
                RecoveryAction::Nothing,
                format!(
                    "音频体传输不完整（至少需要 {} 字节，实际 {written} 字节）",
                    info.size
                ),
            ));
        }

        let quality = describe_quality(&info);
        let preview = info.is_preview;
        // 这里只可能是无效旧缓存；Windows rename 不覆盖既有文件，先尝试移除。
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::rename(&part, &path).map_err(|err| {
            let _ = std::fs::remove_file(&part);
            PlaybackFailure::new(
                Stage::Unpack,
                Retryability::Never,
                RecoveryAction::FreeSpace,
                err.to_string(),
            )
        })?;
        std::fs::write(
            &quality_path,
            format!("{quality}\t{written}\t{}", u8::from(preview)),
        )
        .map_err(|err| {
            PlaybackFailure::new(
                Stage::Unpack,
                Retryability::Never,
                RecoveryAction::FreeSpace,
                err.to_string(),
            )
        })?;
        // 无损整曲下载 + 解密会在内存里同时持有加密数据、明文、mdat 多份缓冲，
        // 结束后把已释放的页归还系统，避免 macOS malloc 保留导致 RSS 长期虚高。
        crate::mem::release_malloc_pressure();
        Ok(CachedTrack {
            path,
            quality,
            preview,
            lease,
        })
    }

    /// 本地播放偏好不需要重建 API 客户端，但会话后续保存音质/登录时必须带上最新值。
    pub fn update_playback_preferences(&mut self, settings: &Settings) {
        self.settings.volume = settings.volume;
        self.settings.volume_before_mute = settings.volume_before_mute;
        self.settings.audio_cache_limit_mb = settings.audio_cache_limit_mb;
        // 播放模式也要同步：登录成功、刷新音质都会用 `settings()` 整体回填
        // `root.settings`，漏掉这一项就会把用户刚切的模式覆盖回旧值。
        self.settings.play_mode = settings.play_mode;
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// 换设置（例如登录后写入 Cookie、改签名器/音质）——重建会话以应用全部改动。
    pub fn apply(&mut self, settings: Settings) -> anyhow::Result<()> {
        settings.save().context("保存配置失败")?;
        *self = Self::new(settings);
        Ok(())
    }

    /// 获取歌词，并保留原始增强 LRC 中的逐字时间轴。
    pub fn lyrics(
        &self,
        track: &crate::models::TrackItem,
    ) -> anyhow::Result<Vec<crate::models::LyricLine>> {
        let track_id = if track.id.trim().is_empty() {
            anyhow::bail!("缺少歌曲 id")
        } else {
            track.id.trim()
        };
        let response = libresoda::soda::track::fetch_web_track_v2(&self.pumpkin, track_id)
            .map_err(|err| anyhow::anyhow!("获取歌词失败: {err}"))?;
        Ok(crate::models::parse_lrc(&response.lyric.content))
    }

    pub fn soda(&self) -> &Soda {
        &self.pumpkin
    }

    pub fn has_cookie(&self) -> bool {
        !self.settings.cookie.trim().is_empty()
    }

    /// 简单搜索（同步；UI 侧应放到后台线程执行）。
    pub fn search(&self, keyword: &str) -> anyhow::Result<Vec<crate::models::TrackItem>> {
        let songs = self
            .pumpkin
            .search(keyword)
            .map_err(|err| anyhow::anyhow!("搜索失败: {err}"))?;
        Ok(songs.iter().map(crate::models::TrackItem::from).collect())
    }

    /// 按官方搜索页范围搜索。
    pub fn search_scope(
        &self,
        keyword: &str,
        scope: crate::library::SearchScope,
    ) -> anyhow::Result<crate::models::SearchResults> {
        crate::library::search(self, keyword, scope)
    }

    /// 读取音乐人页数据（同步；UI 侧应放到后台线程执行）。
    pub fn artist_detail(&self, artist_id: &str) -> anyhow::Result<crate::models::ArtistDetail> {
        crate::library::artist_detail(self, artist_id)
    }

    /// 音乐人歌曲翻页（同步；UI 侧应放到后台线程执行）。
    pub fn artist_tracks(
        &self,
        artist_id: &str,
        cursor: &str,
        count: i64,
    ) -> anyhow::Result<crate::models::ArtistTracksPage> {
        crate::library::artist_tracks_page(self, artist_id, cursor, count)
    }

    /// 读取专辑页数据（同步；UI 侧应放到后台线程执行）。
    pub fn album_detail(&self, album_id: &str) -> anyhow::Result<crate::models::AlbumDetail> {
        crate::library::album_detail(self, album_id)
    }

    /// 封面图本地缓存路径：有缓存就直接返回，没有就下载（失败返回 `None`，界面用占位图）。
    ///
    /// 封面走 CDN，和音频一样只依赖 URL；缓存目录 `~/.cache/sodam/covers/`，
    /// 文件名用 URL 的哈希，避免奇怪字符和重复下载。
    /// 原始封面缓存：歌词页使用原图，列表仍使用缩略图。
    pub fn original_cover_path(&self, url: &str) -> Option<std::path::PathBuf> {
        let url = url.trim();
        if url.is_empty() {
            return None;
        }
        let dir = crate::audio::cover_cache_dir().join("original");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("{:016x}.img", hash_url(url)));
        if std::fs::metadata(&path)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false)
        {
            return Some(path);
        }

        let response = ureq::get(url)
            .timeout(std::time::Duration::from_secs(15))
            .call()
            .ok()?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(16 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.is_empty() {
            return None;
        }
        std::fs::write(&path, bytes).ok()?;
        Some(path)
    }

    /// 删除某张缩略图封面缓存；重试时用。
    pub fn invalidate_cover_path(&self, url: &str) -> bool {
        let path = crate::audio::cover_cache_dir()
            .join(format!("{:016x}-{COVER_MAX_EDGE}.img", hash_url(url)));
        std::fs::remove_file(path).is_ok()
    }

    /// 删除某张原图封面缓存；重试时用。
    pub fn invalidate_original_cover(&self, url: &str) -> bool {
        let path = crate::audio::cover_cache_dir()
            .join("original")
            .join(format!("{:016x}.img", hash_url(url)));
        std::fs::remove_file(path).is_ok()
    }

    pub fn cover_path(&self, url: &str) -> Option<std::path::PathBuf> {
        let url = url.trim();
        if url.is_empty() {
            return None;
        }
        let dir = crate::audio::cover_cache_dir();
        std::fs::create_dir_all(&dir).ok()?;
        // 文件名带目标边长：换尺寸/旧的全尺寸缓存不会混用
        let path = dir.join(format!("{:016x}-{COVER_MAX_EDGE}.img", hash_url(url)));
        if std::fs::metadata(&path)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false)
        {
            return Some(path);
        }
        let response = ureq::get(url)
            .timeout(std::time::Duration::from_secs(15))
            .call()
            .ok()?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(8 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.is_empty() {
            return None;
        }
        // 缩到列表实际需要的尺寸：40px 的缩略图不需要 500px 的原图
        let shrunk = image::load_from_memory(&bytes)
            .ok()
            .map(|image| image.thumbnail(COVER_MAX_EDGE, COVER_MAX_EDGE));
        match shrunk {
            Some(image) => {
                let mut encoded = std::io::Cursor::new(Vec::new());
                image.write_to(&mut encoded, image::ImageFormat::Png).ok()?;
                std::fs::write(&path, encoded.into_inner()).ok()?;
            }
            None => {
                // 解码不了（罕见格式）就存原图，至少能显示
                std::fs::write(&path, &bytes).ok()?;
            }
        }
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_builds_without_network() {
        let settings = Settings {
            signer_url: "http://127.0.0.1:8899/sign".into(),
            signer_token: "t".into(),
            cookie: "sessionid_ss=x".into(),
            device_id: "1234567890123456".into(),
            quality: "lossless".into(),
            ..Default::default()
        };
        let session = Session::new(settings);
        assert!(session.has_cookie());
        assert_eq!(session.settings().quality, "lossless");
    }

    /// 试听标记与旧 sidecar 的兼容：这条链一断，界面就会把整曲说成试听（或反之）。
    #[test]
    fn sidecar_reads_preview_flag_and_self_heals_legacy_label() {
        let (quality, preview) = read_sidecar("无损 871k\t2048\t1", 2048).unwrap();
        assert_eq!(quality, "无损 871k");
        assert!(preview);
        let (_, preview) = read_sidecar("无损 871k\t2048\t0", 2048).unwrap();
        assert!(!preview);
        // 旧缓存没有第三个字段：试听拼在档位里，要读出来并洗回纯档位
        let (quality, preview) = read_sidecar("无损 · 试听\t2048", 2048).unwrap();
        assert_eq!(quality, "无损");
        assert!(preview);
        let (quality, preview) = read_sidecar("无损\t2048", 2048).unwrap();
        assert_eq!(quality, "无损");
        assert!(!preview);
    }

    /// 尺寸对不上、档位为空、字段不齐一律 miss——半截文件不许冒充命中。
    #[test]
    fn sidecar_rejects_mismatched_or_empty_records() {
        assert!(read_sidecar("无损 871k\t2047\t0", 2048).is_none());
        assert!(read_sidecar("\t2048\t0", 2048).is_none());
        assert!(read_sidecar("无损 871k\tabc\t0", 2048).is_none());
        assert!(read_sidecar("无损 871k", 2048).is_none());
    }

    #[test]
    fn truncation_check_rejects_only_short_downloads() {
        // 服务端未声明大小：无从判定截断，放行
        assert!(!is_truncated_download(0, 123));
        // 正常：实际等于声明
        assert!(!is_truncated_download(10_569_897, 10_569_897));
        // 真实案例：play_info 声明的 size 滞后于 CDN 实际资产，文件完整可播
        assert!(!is_truncated_download(10_569_897, 10_660_358));
        // 截断：实际少于声明
        assert!(is_truncated_download(10_569_897, 10_569_896));
    }
}
