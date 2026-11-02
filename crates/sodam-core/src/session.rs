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
    /// 会员档位（`vip` / `svip`，空串表示未知或非会员）。
    pub vip_stage: String,
    /// 关注数。
    pub following: u64,
    /// 粉丝数。
    pub follower: u64,
    /// 获赞数（分享与评论）。
    pub likes: u64,
    /// 登录用户头像 URL（来自 /luna/pc/me 的 larger_avatar_url）
    pub avatar_url: String,
}

/// 读取 sidecar 后、构造 [`CachedTrack`] 前的中间结果。
struct CachedInfo {
    path: std::path::PathBuf,
    quality: String,
    preview: bool,
    quality_rank: Option<i64>,
    checked_rank: Option<i64>,
}

impl Session {
    /// 当前音质**意图**对应的缓存标签。
    ///
    /// 缓存按意图命名，所以「自动」下同一首歌始终命中同一个文件：
    /// 权益升级后仍会先播到非会员时期缓存下来的档位，直到清空缓存或换意图。
    fn quality_tag(&self) -> String {
        self.settings.quality_intent().cache_tag()
    }

    /// 解析 sidecar `{档位}\t{字节}\t{0|1}\t{档位分数}\t{已探测目标档位}\t{探测时间}`，
    /// 与实际字节数核对后给出完整记录。
    ///
    /// 只认「非空文件」会让被截断的半截下载永久冒充有效缓存，表现为每次都从中间开始，
    /// 所以尺寸不符或档位为空一律 miss，重下自愈。
    /// 第三个字段记录试听标记；更早的旧格式把试听拼在档位里（`无损 · 试听`），
    /// 这里把它读出来、顺手洗回纯档位——已经下好的整曲不该因为多一个字段就被重下。
    /// 第四到第六个字段后加：实际档位缺失时视为未知，不据此标记降档；
    /// 第五、六个字段记录升级探测结论，过期记录不参与跳过判断。
    fn cached_info(&self, track_id: &str) -> Option<CachedInfo> {
        let path = crate::audio::cache_dir().join(format!("{track_id}-{}.m4a", self.quality_tag()));
        let actual = std::fs::metadata(&path).ok()?.len();
        if actual == 0 {
            return None;
        }
        let quality_path =
            crate::audio::cache_dir().join(format!("{track_id}-{}.quality", self.quality_tag()));
        let text = std::fs::read_to_string(&quality_path).ok()?;
        let meta = crate::cache::parse_sidecar(&text, actual)?;
        let checked = meta.fresh_checked_rank(crate::cache::now_epoch_secs());
        Some(CachedInfo {
            path,
            quality: meta.quality,
            preview: meta.preview,
            quality_rank: meta.actual_rank,
            checked_rank: checked,
        })
    }

    pub fn cached_track(&self, track_id: &str) -> Option<CachedTrack> {
        let path = crate::audio::cache_dir().join(format!("{track_id}-{}.m4a", self.quality_tag()));
        let lease = crate::cache::CacheLease::acquire(&path);
        let info = self.cached_info(track_id)?;
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(&info.path)
            .and_then(|file| file.set_modified(std::time::SystemTime::now()));
        Some(CachedTrack {
            path: info.path,
            quality: info.quality,
            preview: info.preview,
            quality_rank: info.quality_rank,
            checked_rank: info.checked_rank,
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
    /// 这条流的档位分数（与取流筛选同一套刻度）；`None` = 旧缓存没记，未知。
    ///
    /// 与所选是否相符是**每次读取时现算**的：用当前意图与这里的实际档位比较，
    /// 结论不写回缓存——否则用户改了意图，缓存里的旧结论就成了假话。
    pub quality_rank: Option<i64>,
    /// 仍在有效期内、已确认没有更高档位的目标分数；`None` = 没问过或记录已过期。
    ///
    /// 只用于决定是否再发起升级，不改变降档提示：曲目确实还在低档位播放。
    pub checked_rank: Option<i64>,
    pub(crate) lease: crate::cache::CacheLease,
}

impl CachedTrack {
    /// 实际档位对应的显示名（见 [`crate::quality::tier_label`]；档位未知返回 `None`）。
    pub fn actual_tier(&self) -> Option<&'static str> {
        self.quality_rank.map(crate::quality::tier_label)
    }

    /// 命中缓存后是否还需要向服务端要更高档位。
    ///
    /// 实际档位已达目标、或最近已经确认过该目标没有更高流时，都不再请求。
    pub fn needs_upgrade(
        &self,
        intent: &crate::quality::QualityIntent,
        is_vip: Option<bool>,
    ) -> bool {
        let target_rank = crate::quality::target_rank(intent, is_vip);
        crate::quality::is_tier_below_selected(self.quality_rank, intent, is_vip)
            && !crate::cache::upgrade_check_covers(self.checked_rank, target_rank)
    }
}

/// 把 `DownloadInfo` 翻成界面用的音质标签（以**实际拉到的流**为准）。
///
/// 档位名走 [`crate::quality::tier_label`]，与不符判定共用 libresoda 的那套刻度；
/// 这里只负责补上码率。两个函数各按各的边界算，就会出现「徽章显示极高、
/// 不符判定却按较高比」这种自相矛盾的显示。
fn describe_quality(info: &libresoda::soda::types::DownloadInfo) -> String {
    let bitrate = info.bitrate.max(0) / 1000;
    let tier = crate::quality::tier_label(crate::quality::stream_rank(info));
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

/// 把「确认没有更高档位」写回 sidecar；失败不影响播放，下次再探。
fn record_upgrade_check(
    quality_path: &std::path::Path,
    audio_path: &std::path::Path,
    actual_rank: i64,
    checked_rank: i64,
) {
    // 与替换路径共用同一把写锁，避免结论落到并发替换后的新文件上。
    let lock = crate::cache::download_lock(audio_path);
    let _guard = lock.lock().unwrap_or_else(|err| err.into_inner());
    let _ = crate::cache::record_upgrade_check(
        quality_path,
        actual_rank,
        checked_rank,
        crate::cache::now_epoch_secs(),
    );
}

/// 确认性 miss：拿到完整流、档位可识别、目标存在、权益已确认，且实际档位低于目标。
///
/// 返回应写入 sidecar 的 `checked_rank`。试听回退、权益未确认、档位未知、目标未知或
/// 实际已经达标都不写——这些结论会随账号权益或曲库变化，不能持久化。
fn confirmed_miss_rank(
    target_rank: Option<i64>,
    actual_rank: i64,
    preview: bool,
    can_record: bool,
) -> Option<i64> {
    if preview || !can_record || actual_rank <= 0 {
        return None;
    }
    let target = target_rank?;
    (actual_rank < target).then_some(target)
}

/// 拼出 sidecar 内容；有探测结论时写第 5、6 字段。
fn encode_sidecar(
    quality: &str,
    written: u64,
    preview: bool,
    actual_rank: i64,
    checked_rank: Option<i64>,
) -> String {
    match checked_rank {
        Some(rank) => format!(
            "{quality}\t{written}\t{}\t{actual_rank}\t{rank}\t{}",
            u8::from(preview),
            crate::cache::now_epoch_secs(),
        ),
        None => format!("{quality}\t{written}\t{}\t{actual_rank}", u8::from(preview)),
    }
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
        // 「自动」= 不设上限，交给服务端按权益与曲目可用性收敛；
        // 只有具体档位才需要把上限告诉 libresoda。
        if let crate::quality::QualityIntent::Tier(tier) = settings.quality_intent() {
            pumpkin.set_quality_preference(tier);
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
        let url = libresoda::soda::pc_me_url();
        let response = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .set("Cookie", &self.pumpkin.cookie())
            .set(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) SodaMusic/3.8.0 Chrome/122.0.0.0 Safari/537.36",
            )
            .set("x-luna-background-type", "foreground")
            .set("x-luna-is-background-req", "0")
            .set("x-luna-is-local-user", "1")
            .call()
            .map_err(|err| anyhow::anyhow!("读取账号信息失败: {err}"))?;
        let body = response
            .into_string()
            .map_err(|err| anyhow::anyhow!("读取账号信息失败: {err}"))?;
        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|err| anyhow::anyhow!("账号信息解析失败: {err}"))?;
        if json
            .get("status_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            != 0
        {
            let message = json
                .pointer("/status_info/status_msg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("读取账号信息失败: {message}");
        }
        let info = json.get("my_info").cloned().unwrap_or_default();
        let vip_stage = info
            .get("vip_stage")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let vip = info
            .get("is_vip")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || matches!(vip_stage.as_str(), "vip" | "svip");
        let avatar = info.get("larger_avatar_url").cloned().unwrap_or_default();
        let mut avatar_url = avatar
            .get("urls")
            .and_then(|v| v.as_array())
            .and_then(|urls| urls.first())
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let uri = avatar
            .get("uri")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !avatar_url.is_empty() && !uri.is_empty() && !avatar_url.contains(uri) {
            avatar_url.push_str(uri);
        }
        let stat = |key: &str| -> u64 {
            json.pointer(&format!("/my_stats/{key}"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        let likes = json
            .pointer("/my_stats/all_comments_liked_detail/count")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(AccountInfo {
            nickname: info
                .get("nickname")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string(),
            user_id: info
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string(),
            vip,
            vip_stage,
            following: stat("count_following"),
            follower: stat("count_follower"),
            likes,
            avatar_url,
        })
    }

    /// 登录成功后写入 Cookie（落盘 + 重建会话）。
    pub fn apply_login_cookie(&mut self, cookie: &str) -> anyhow::Result<()> {
        let mut settings = self.settings.clone();
        settings.cookie = cookie.trim().to_string();
        self.apply(settings)
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
        account_is_vip: Option<bool>,
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
        // 快速路径：若当前已有有效缓存命中，直接返回，无需排队等待锁
        if let Some(cached) = self.cached_track(&track.id) {
            return Ok(cached);
        }
        let download_lock = crate::cache::download_lock(&path);
        let _writer = download_lock.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = self.cached_track(&track.id) {
            return Ok(cached);
        }
        let intent = self.settings.quality_intent();
        let target_rank = crate::quality::target_rank(&intent, account_is_vip);
        let can_record_check = crate::quality::can_record_unavailable(target_rank, account_is_vip);
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
        let quality_rank = crate::quality::stream_rank(&info);
        // 首次下载就已经知道服务端给不出目标的更高流时，直接落盘结论，
        // 下次播放不必再完整下载一次来重复验证。
        let checked_rank =
            confirmed_miss_rank(target_rank, quality_rank, preview, can_record_check);
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
        let sidecar_content =
            encode_sidecar(&quality, written, preview, quality_rank, checked_rank);
        std::fs::write(&quality_path, sidecar_content).map_err(|err| {
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
            quality_rank: Some(quality_rank),
            checked_rank,
            lease,
        })
    }

    /// 升级单首已缓存曲目的音频流到指定意图。
    ///
    /// 在后台拉取新流到临时文件，拉取并校验成功后通过 [`crate::cache::atomic_replace_cache`]
    /// 原子替换旧文件。
    ///
    /// * 升级期间持有待升级目标与临时文件的 [`crate::cache::CacheLease`]，避免被淘汰删除；
    /// * 若中途失败，清理临时文件，绝不破坏原有缓存；
    /// * 若新拉到的档位并不高于当前已有档位，清理临时文件，保留原有缓存并返回原有缓存；
    /// * 只有拿到完整流且档位没有提升时，才把这次目标档位写进 sidecar 的「已探测」
    ///   字段，30 天内不再为同一目标重复请求；依赖会员权益的档位只在确认账号有权益时记录。
    pub fn upgrade_cached_track(
        &self,
        track: &crate::models::TrackItem,
        target_intent: Option<&crate::quality::QualityIntent>,
        account_is_vip: Option<bool>,
    ) -> Result<CachedTrack, crate::failure::PlaybackFailure> {
        use crate::failure::{classify, PlaybackFailure, RecoveryAction, Retryability, Stage};

        let dir = crate::audio::cache_dir();
        let tag = self.quality_tag();
        let path = dir.join(format!("{0}-{tag}.m4a", track.id));
        let quality_path = dir.join(format!("{0}-{tag}.quality", track.id));

        // 若本地缓存原本就不存在，拒绝升级（升级只针对已有缓存曲目）
        let current = match self.cached_track(&track.id) {
            Some(c) => c,
            None => {
                return Err(PlaybackFailure::new(
                    Stage::Input,
                    Retryability::Never,
                    RecoveryAction::Nothing,
                    "track is not in local cache",
                ));
            }
        };

        let default_intent = self.settings.quality_intent();
        let intent = target_intent.unwrap_or(&default_intent);
        // 明确指定 Tier 时目标分数与会员状态无关，但记录「没有更高档位」需要知道权益：
        // 非会员请求无损失败并不代表曲库没有无损，买会员后必须重新尝试。
        let vip = match intent {
            crate::quality::QualityIntent::Tier(_) => account_is_vip,
            _ => self.is_vip().ok(),
        };
        let target_rank = crate::quality::target_rank(intent, vip);
        // 没有会员权益时拿不到更高档位，不能据此断言曲库没有；等权益确认后再记录。
        let can_record_check = crate::quality::can_record_unavailable(target_rank, vip);

        // 如果已有档位并不低于目标档位，直接复用已有缓存。
        // 最近已经确认过该目标没有更高流时同样不再请求，避免跨会话重复下载。
        if !current.needs_upgrade(intent, vip) {
            return Ok(current);
        }

        // 串行化升级操作并获取租约：
        // 升级使用独立锁，不与普通点播播放竞争同把锁，确保升级进行中同一曲目重播秒开
        let upgrade_lock = crate::cache::download_lock(&path.with_extension("upgrade-lock"));
        let _writer = upgrade_lock.lock().unwrap_or_else(|err| err.into_inner());

        // 获取锁后重读当前缓存，防止并发升级已将其提升
        let current = match self.cached_track(&track.id) {
            Some(c) => c,
            None => {
                return Err(PlaybackFailure::new(
                    Stage::Input,
                    Retryability::Never,
                    RecoveryAction::Nothing,
                    "track was removed from local cache before upgrade lock",
                ));
            }
        };
        if !current.needs_upgrade(intent, vip) {
            return Ok(current);
        }

        let target_lease = crate::cache::CacheLease::acquire(&path);

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

        let part = dir.join(part_file_name(&track.id, &tag));
        let _part_lease = crate::cache::CacheLease::acquire(&part);

        // 若指定了目标具体档位，创建临时会话或设置档位偏好以获取更高档位
        let info = if let crate::quality::QualityIntent::Tier(tier) = intent {
            let mut upgrade_settings = self.settings.clone();
            upgrade_settings.quality = tier.clone();
            let upgrade_session = Session::new(upgrade_settings);
            upgrade_session.pumpkin.download_with_info(&song, &part)
        } else {
            self.pumpkin.download_with_info(&song, &part)
        }
        .map_err(|err| {
            let _ = std::fs::remove_file(&part);
            classify(&err)
        })?;

        let written = std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
        if written == 0 || is_truncated_download(info.size, written) {
            let _ = std::fs::remove_file(&part);
            return Err(PlaybackFailure::new(
                Stage::Transfer,
                Retryability::Transient,
                RecoveryAction::Nothing,
                format!(
                    "升级音频体传输不完整（至少需要 {} 字节，实际 {written} 字节）",
                    info.size
                ),
            ));
        }

        // 核心安全护栏：试听流绝不允许覆盖已有非试听整曲缓存（即便试听流档位分数更高）
        if info.is_preview && !current.preview {
            let _ = std::fs::remove_file(&part);
            return Ok(current);
        }

        let new_rank = crate::quality::stream_rank(&info);
        // 成功拿到完整流但仍在目标之下，同样是确认性结论：服务端已给出它能给的最好档位。
        let checked_rank =
            confirmed_miss_rank(target_rank, new_rank, info.is_preview, can_record_check);
        // 若新流档位并未高于旧档位，且未发生「由试听升级为整曲」，丢弃临时文件，保留旧文件
        if let Some(old_rank) = current.quality_rank {
            let is_improved = new_rank > old_rank || (current.preview && !info.is_preview);
            if !is_improved {
                let _ = std::fs::remove_file(&part);
                // 只有「拿到完整流、档位仍没提升」才是确认性结论。试听回退与下载失败
                // 会随会员权益或网络变化，不能写成「没有更高档位」。
                if let Some(checked_rank) = checked_rank {
                    record_upgrade_check(&quality_path, &path, old_rank, checked_rank);
                }
                return Ok(current);
            }
        }

        let quality = describe_quality(&info);
        let preview = info.is_preview;
        let sidecar_content = encode_sidecar(&quality, written, preview, new_rank, checked_rank);

        // N1 修复：在执行原子替换重命名序列前，获取 download_lock(&path)，
        // 保证在极其短暂的重命名窗口期内与点播下载互斥，防止交错写同一组文件。
        // 锁序为「先 upgrade-lock，后 download-lock」，绝不构成环死锁。
        let file_lock = crate::cache::download_lock(&path);
        let _replace_guard = file_lock.lock().unwrap_or_else(|err| err.into_inner());

        crate::cache::atomic_replace_cache(&part, &path, &quality_path, &sidecar_content).map_err(
            |err| {
                let _ = std::fs::remove_file(&part);
                PlaybackFailure::new(
                    Stage::Unpack,
                    Retryability::Never,
                    RecoveryAction::FreeSpace,
                    err.to_string(),
                )
            },
        )?;

        crate::mem::release_malloc_pressure();
        Ok(CachedTrack {
            path,
            quality,
            preview,
            quality_rank: Some(new_rank),
            checked_rank,
            lease: target_lease,
        })
    }

    /// 本地播放偏好不需要重建 API 客户端，但会话后续保存音质/登录时必须带上最新值。
    pub fn update_playback_preferences(&mut self, settings: &Settings) {
        self.settings.volume = settings.volume;
        self.settings.volume_before_mute = settings.volume_before_mute;
        self.settings.audio_cache_limit_mb = settings.audio_cache_limit_mb;
        // 播放模式也要同步：登录成功、改音质都会用 `settings()` 整体回填
        // `root.settings`，漏掉这一项就会把用户刚切的模式覆盖回旧值。
        self.settings.play_mode = settings.play_mode;
        // 详细日志同理：登录走 `apply_login_cookie` → `apply` 会把会话里的副本整体落盘，
        // 副本漏掉这一项就会把用户刚打开的开关写回 false，重启后失效。
        self.settings.detailed_logs = settings.detailed_logs;
        // 音质与偏好来源同步：主路径 `set_quality` 已整体 `session.apply`，此处做防御性
        // 冗余兜底，确保在其他触发偏好落盘的分支上会话副本也与外部一致。
        self.settings.quality = settings.quality.clone();
        self.settings.quality_source = settings.quality_source;
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

    /// 重建会话不写音质配置：登录/换 Cookie 之后「自动」仍然是自动，
    /// 否则会员状态一变，档位就再也跟不上。
    #[test]
    fn rebuilding_session_leaves_quality_setting_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        Settings::default().save_to(&path).unwrap();

        // 模拟「读配置 → 建会话 → 登录后回填设置 → 再建会话」这一整圈
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.quality, crate::quality::AUTO);
        let mut session = Session::new(loaded);
        let mut after_login = session.settings().clone();
        after_login.cookie = "sessionid_ss=x".into();
        session.update_playback_preferences(&after_login);
        let rebuilt = Session::new(session.settings().clone());

        assert_eq!(
            rebuilt.settings().quality,
            crate::quality::AUTO,
            "登录后不得被固化成具体档位"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(r#""quality": "auto""#),
            "配置文件里的自动是 auto 这一种写法：{text}"
        );
    }

    /// 偏好同步带上详细日志，并防御性同步音质意图与来源，确保会话副本时刻一致。
    #[test]
    fn playback_preferences_sync_carries_detailed_logs_and_defensively_syncs_quality() {
        use crate::quality::QualitySource;

        let mut session = Session::new(Settings::default());
        let mut current = session.settings().clone();
        current.detailed_logs = true;
        current.quality = "lossless".into();
        current.quality_source = QualitySource::User;
        session.update_playback_preferences(&current);

        assert!(
            session.settings().detailed_logs,
            "同步后会话副本要带上用户刚打开的详细日志开关"
        );
        assert_eq!(session.settings().quality, "lossless");
        assert_eq!(session.settings().quality_source, QualitySource::User);
    }

    /// 缓存标签按意图取：自动与空串同一个文件，换档位就换路径。
    #[test]
    fn cache_tag_follows_quality_intent() {
        let auto = Session::new(Settings::default());
        let blank = Session::new(Settings {
            quality: "  ".into(),
            ..Default::default()
        });
        assert_eq!(auto.quality_tag(), "auto");
        assert_eq!(blank.quality_tag(), "auto");

        let explicit = Session::new(Settings {
            quality: "Lossless".into(),
            ..Default::default()
        });
        assert_eq!(explicit.quality_tag(), "lossless");
    }

    /// 试听标记与旧 sidecar 的兼容：这条链一断，界面就会把整曲说成试听（或反之）。
    #[test]
    fn sidecar_reads_preview_flag_and_self_heals_legacy_label() {
        let meta = crate::cache::parse_sidecar("无损 871k\t2048\t1", 2048).unwrap();
        assert_eq!(meta.quality, "无损 871k");
        assert!(meta.preview);
        assert_eq!(meta.actual_rank, None, "旧格式没有档位分数，视为未知");
        let meta = crate::cache::parse_sidecar("无损 871k\t2048\t0", 2048).unwrap();
        assert!(!meta.preview);
        // 旧缓存没有第三个字段：试听拼在档位里，要读出来并洗回纯档位
        let meta = crate::cache::parse_sidecar("无损 · 试听\t2048", 2048).unwrap();
        assert_eq!(meta.quality, "无损");
        assert!(meta.preview);
        let meta = crate::cache::parse_sidecar("无损\t2048", 2048).unwrap();
        assert_eq!(meta.quality, "无损");
        assert!(!meta.preview);
        // 旧缓存把 50 分档写成「标准」：读取时洗成新显示名，不再与 standard(70) 撞名。
        let meta = crate::cache::parse_sidecar("标准 128k\t2048\t0\t50", 2048).unwrap();
        assert_eq!(meta.quality, "流畅 128k");
        assert_eq!(meta.actual_rank, Some(50));
    }

    /// 第四个字段（实际档位分数）能读出来；缺失或不是数字时一律未知。
    #[test]
    fn sidecar_reads_actual_rank_and_treats_missing_as_unknown() {
        let meta = crate::cache::parse_sidecar("无损 871k\t2048\t0\t100", 2048).unwrap();
        assert_eq!(meta.actual_rank, Some(100));
        let meta = crate::cache::parse_sidecar("极高 320k\t2048\t0\tabc", 2048).unwrap();
        assert_eq!(meta.actual_rank, None);
    }

    /// 尺寸对不上、档位为空、字段不齐一律 miss——半截文件不许冒充命中。
    #[test]
    fn sidecar_rejects_mismatched_or_empty_records() {
        assert!(crate::cache::parse_sidecar("无损 871k\t2047\t0", 2048).is_none());
        assert!(crate::cache::parse_sidecar("\t2048\t0", 2048).is_none());
        assert!(crate::cache::parse_sidecar("无损 871k\tabc\t0", 2048).is_none());
        assert!(crate::cache::parse_sidecar("无损 871k", 2048).is_none());
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

    /// 只有完整流、目标存在、权益已确认且实际低于目标时才写负结果。
    #[test]
    fn confirmed_miss_needs_full_stream_target_and_entitlement() {
        assert_eq!(confirmed_miss_rank(Some(100), 80, false, true), Some(100));
        assert_eq!(confirmed_miss_rank(Some(100), 100, false, true), None);
        assert_eq!(confirmed_miss_rank(Some(100), 80, true, true), None);
        assert_eq!(confirmed_miss_rank(Some(100), 80, false, false), None);
        assert_eq!(confirmed_miss_rank(None, 80, false, true), None);
        // 档位算不出来（未知，0 分）时不是确认性结论。
        assert_eq!(confirmed_miss_rank(Some(100), 0, false, true), None);
    }

    /// 写出与解析必须用同一套字段布局，避免两边各改一处后错位。
    #[test]
    fn encode_sidecar_round_trips_through_parser() {
        let plain = encode_sidecar("极高 320k", 2000, false, 80, None);
        assert_eq!(plain, "极高 320k\t2000\t0\t80");
        let plain_checked = crate::cache::parse_sidecar(&plain, 2000).unwrap();
        assert_eq!(plain_checked.checked_rank, None);

        let checked = encode_sidecar("极高 320k", 2000, false, 80, Some(100));
        let parsed = crate::cache::parse_sidecar(&checked, 2000).unwrap();
        assert_eq!(parsed.actual_rank, Some(80));
        assert_eq!(parsed.checked_rank, Some(100));
        assert!(parsed.checked_at.is_some());
    }

    /// 命中缓存时，确认过的目标不再触发升级；目标提高后仍要重新探测。
    #[test]
    fn needs_upgrade_honours_confirmed_target() {
        let lease = crate::cache::CacheLease::acquire(std::path::Path::new("t.m4a"));
        let track = |rank: i64, checked: Option<i64>| CachedTrack {
            path: std::path::PathBuf::new(),
            quality: "test".into(),
            preview: false,
            quality_rank: Some(rank),
            checked_rank: checked,
            lease: lease.clone(),
        };
        let lossless = crate::quality::QualityIntent::parse("lossless");
        assert!(track(80, None).needs_upgrade(&lossless, None));
        assert!(!track(80, Some(100)).needs_upgrade(&lossless, None));
        assert!(track(80, Some(70)).needs_upgrade(&lossless, None));
        assert!(!track(100, None).needs_upgrade(&lossless, None));
    }

    #[test]
    fn upgrade_cached_track_fails_fast_when_not_cached() {
        let session = Session::new(Settings::default());
        let track = crate::TrackItem {
            id: "non-existent-track-999".into(),
            ..Default::default()
        };
        let err = session
            .upgrade_cached_track(&track, None, None)
            .unwrap_err();
        assert_eq!(err.stage, crate::failure::Stage::Input);
        assert_eq!(err.retryability, crate::failure::Retryability::Never);
    }
}
