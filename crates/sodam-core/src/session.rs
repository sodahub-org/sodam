//! 会话层：把 [`SessionInputs`] 变成可用的 libresoda 客户端。

use crate::cache::encode_track_id;
use crate::config::SessionInputs;
use crate::upgrade::NotImprovedReason;
use libresoda::soda::signature::HttpSignature;
use libresoda::{AppCredentials, Soda};
use std::io::Read;
use std::sync::Arc;

/// 封面 URL → 缓存文件名用的稳定哈希（FNV-1a 64）。
/// 封面缓存的最大边长：列表缩略图 40px、卡片 156px，160 足够且解码开销极小。
const COVER_MAX_EDGE: u32 = 160;
/// 原图落盘目标边长：歌词页封面按窗口高度 40% 显示，1280 覆盖到约 2×
/// 缩放的常见场景，更高缩放下会轻微放大（父票 #215 挂了 4K 真机复核假设）。
const COVER_ORIGINAL_MAX_EDGE: u32 = 1280;
/// 落盘编码质量：不透明的封面统一 JPEG，85 在清晰度与体积之间取平衡。
/// 体积随画面复杂度与源图分辨率浮动，不写死数字。带透明的图落 PNG（见 shrink_cover）。
const COVER_JPEG_QUALITY: u8 = 85;
/// 下载字节解码的边长上限：超限当解码失败，走占位图重试。
/// 单张最坏解码内存：8 位 RGBA 约 64 MiB，16 位图翻倍到约 128 MiB。
const MAX_DECODE_EDGE: u32 = 4096;

/// 缓存文件能不能用：是个普通文件，而且不是 0 字节（0 字节多半是上次写坏的）。
fn cover_file_usable(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
}

/// 封面落盘前的纯处理：解码 → 等比缩小（只缩不放，长边 ≤ `edge`）→ 编码。
///
/// 解码/编码失败返回 `None`，调用方不写缓存文件，界面继续显示占位图。
/// 不透明图落 JPEG（专辑封面实际均为不透明）；真有透明像素的图落 PNG
/// 保透明——这个缓存也服务音乐人/用户头像，转 JPEG 会变黑底。
/// 只带 alpha 通道、像素全不透明的图（大量导出工具的默认形态）照旧 JPEG。
fn shrink_cover(bytes: &[u8], edge: u32) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ImageEncoder;

    // 解码带上限：CDN 字节不可全信，下载体积拦不住解压后的内存；
    // 边长超限一律当解码失败，走占位图重试。
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE);
    limits.max_image_height = Some(MAX_DECODE_EDGE);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
    reader.limits(limits);
    let decoded = reader.with_guessed_format().ok()?.decode().ok()?;
    let (width, height) = (decoded.width(), decoded.height());
    // 等比缩到长边 ≤ edge；比目标小的图原样保留，不放大。
    let (w, h) = if width <= edge && height <= edge {
        (width, height)
    } else if width >= height {
        let short = (height as u64 * edge as u64 / width as u64).max(1) as u32;
        (edge, short)
    } else {
        let short = (width as u64 * edge as u64 / height as u64).max(1) as u32;
        (short, edge)
    };
    let resized = decoded.thumbnail(w, h);
    let mut encoded = std::io::Cursor::new(Vec::new());
    // 带 alpha 通道 ≠ 有透明像素：全不透明的 RGBA 源图照旧走 JPEG，
    // 不然「带通道就落 PNG」会把无损 PNG 的体积又吐回去。
    let transparent =
        resized.color().has_alpha() && resized.to_rgba8().pixels().any(|pixel| pixel.0[3] < 255);
    if transparent {
        // 头像等真带透明的图：落 PNG，别把透明变黑底。
        resized
            .write_to(&mut encoded, image::ImageFormat::Png)
            .ok()?;
    } else {
        let rgb = resized.to_rgb8();
        JpegEncoder::new_with_quality(&mut encoded, COVER_JPEG_QUALITY)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .ok()?;
    }
    Some(encoded.into_inner())
}

/// 缩放/编码失败记一条 warn：整会话只剩占位图，得留排查线索（对齐 `cover_write_failed`）。
/// `shrink_cover` 本体保持纯函数，日志在调用方落；文件名从缓存路径取，避免拼法漂移。
fn log_cover_shrink_failed(path: &std::path::Path) {
    use crate::log_outlet::{log_event, LogLevel, LogTopic};
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    log_event(
        LogLevel::Warn,
        LogTopic::App,
        "cover_shrink_failed",
        &[("file", file.as_str())],
    );
}

/// 命中磁盘缓存时把修改时间推到当前。
///
/// 这只是封面淘汰的**兜底**信号：界面命中内存缓存时不会走到这里，所以 mtime 大多只代表
/// 「最后下载/落盘」。真正的最近显示时间由界面在布局/绘制时记录、每次后台维护前采样后
/// 传给淘汰器，两者取较晚者排序。
///
/// 更新失败（只读、刚好被淘汰等）就算了，本次照样用这张图。
fn touch_cover_file(path: &std::path::Path) {
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(std::time::SystemTime::now()));
}

/// 封面落盘。写成功返回 true。
///
/// 失败时记一条 warn 的 `cover_write_failed`（文件名 + 错误），然后看目标这会儿能不能用：
/// 能用（另一个写者刚写完）就返回 true，否则返回 false，由调用方显示占位图。
fn save_cover_file(path: &std::path::Path, bytes: &[u8]) -> bool {
    use crate::log_outlet::{log_event, LogLevel, LogTopic};
    match crate::cache::write_cache_file_atomic(path, bytes) {
        Ok(()) => true,
        Err(err) => {
            let file = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let error = err.to_string();
            log_event(
                LogLevel::Warn,
                LogTopic::App,
                "cover_write_failed",
                &[("file", file.as_str()), ("error", error.as_str())],
            );
            cover_file_usable(path)
        }
    }
}

/// 音频下载交给 libresoda 的暂存目标名（不含目录）。
///
/// 名字只由曲目、档位标签和用途决定，不带进程号/序号：同一首歌反复下、重试多少次
/// 都复用同一套名字，缓存目录里也就只留这一套 `.part` 与 `.part.lock`。
///
/// 不能带 `.part` 后缀：libresoda 拿到这个路径当下载目标，自己会在后面拼 `.part`
/// （暂存数据）和 `.part.lock`（锁）。目标名再带 `.part` 就会落盘成 `.part.part`。
///
/// 用途必须进名字：正常下载与升级会同时下同一首（升级刻意不和播放抢锁），
/// 同名会让后启动那条被 libresoda 的目标锁顶回来。
fn part_file_name(track_id: &str, tag: &str, purpose: crate::cache::DownloadPurpose) -> String {
    format!(
        "{}-{}-{}.m4a",
        encode_track_id(track_id),
        tag,
        purpose.suffix()
    )
}

/// 暂存文件单独放一个子目录，别和正式缓存混在一起。
/// 档位标签可以是用户自定义值，`auto-normal` 这种恰好会撞上正常下载的暂存名；
/// 放子目录后，清理暂存就不会删掉别人已经下好的正式缓存。
/// 目录名与维护侧（`cache::STAGING_DIR_NAME`）共用一份。
fn staging_dir(audio_dir: &std::path::Path) -> std::path::PathBuf {
    audio_dir.join(crate::cache::STAGING_DIR_NAME)
}

/// 准备这组暂存目标：清掉上次被杀留下的交付目标，再拿租约挡住缓存维护。
/// 正常下载与升级都要走这一步，差异只在用途。
///
/// 可续传下载的续传来源就是上次失败留下的 `.part`（密文）与 `.part.meta` 记录，
/// 所以这里**只清交付目标，不动暂存对**：断网被掐断的那份字节就是下次的起点
///（#243）。`.part` 里这段密钥指纹不对（重解析换了流）时，libresoda 的
/// `load_record` 会自己作废重下，不需要这里替它判断。`.part.lock` 也留着：
/// 名字固定后每个目标只有一把锁，删了反而会破坏跨进程的内核锁语义。
fn stage_path(
    dir: &std::path::Path,
    track_id: &str,
    tag: &str,
    purpose: crate::cache::DownloadPurpose,
) -> Result<(std::path::PathBuf, crate::cache::CacheLease), crate::failure::PlaybackFailure> {
    use crate::failure::{PlaybackFailure, RecoveryAction, Retryability, Stage};

    let stage = staging_dir(dir);
    // 建不出暂存目录就没法下载：直接报确定的失败，别拿着一条父目录都不存在的
    // 路径往下走。错误里带上路径，报问题时不用回头猜是哪一层缓存目录。
    std::fs::create_dir_all(&stage).map_err(|err| {
        PlaybackFailure::new(
            Stage::Unpack,
            Retryability::Never,
            RecoveryAction::FreeSpace,
            format!("{}: {err}", stage.display()),
        )
    })?;
    let name = part_file_name(track_id, tag, purpose);
    let part = stage.join(&name);
    // 只清交付目标：上次被杀若停在「libresoda 已交付、本侧还没改名到正式缓存」，
    // 这个文件就是残留，而 Windows 的 rename 不覆盖既有文件，不清掉会挡住这次交付。
    // 此时调用方已持有该用途的下载锁。删不掉记日志，但日志不是护栏：
    // Unix 上正在被写的同名文件也能 unlink 成功，这时没有日志，
    // 靠的是调用方持有的那把锁保证没人在写。
    if let Err(err) = std::fs::remove_file(&part) {
        if err.kind() != std::io::ErrorKind::NotFound {
            use crate::log_outlet::{log_event, LogLevel, LogTopic};
            let file = part
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let error = err.to_string();
            log_event(
                LogLevel::Warn,
                LogTopic::Audio,
                "stage_cleanup_failed",
                &[("file", file.as_str()), ("error", error.as_str())],
            );
        }
    }
    let lease = crate::cache::CacheLease::acquire(&part);
    Ok((part, lease))
}

#[cfg(test)]
mod part_tests {
    /// 曲目 id 拼进文件名后还要交给 remove_file。带 / 或 .. 的 id 必须被净化，
    /// 不能让清理路径指到缓存目录外面。
    #[test]
    fn part_name_neutralizes_path_characters_in_track_id() {
        let name = super::part_file_name(
            "../../etc/passwd",
            "lossless",
            crate::cache::DownloadPurpose::Normal,
        );
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(".."), "{name}");
        assert!(!name.contains(std::path::MAIN_SEPARATOR), "{name}");

        let dir = std::path::Path::new("/tmp/sodam-cache/audio");
        let joined = dir.join(&name);
        assert_eq!(joined.parent(), Some(dir), "拼出的路径必须仍在缓存目录里");
    }

    /// 同一曲目 + 档位 + 用途的暂存名必须每次一样。
    /// 名字里不能再带进程号或序号，否则每下一次就在缓存目录留一把新锁。
    #[test]
    fn part_name_is_fixed_for_same_track_tag_and_purpose() {
        let first = super::part_file_name("123", "lossless", crate::cache::DownloadPurpose::Normal);
        let second =
            super::part_file_name("123", "lossless", crate::cache::DownloadPurpose::Normal);
        assert_eq!(first, second, "同一曲目两次下载必须复用同一个暂存名");
        assert_eq!(first, "123-lossless-normal.m4a");
    }

    /// 传给 libresoda 的目标名不能以 `.part` 结尾。
    /// libresoda 自己会在目标名后拼 `.part` / `.part.lock`，目标名再带 `.part`
    /// 就会落盘成 `.part.part` / `.part.part.lock`。
    #[test]
    fn part_name_has_no_part_suffix() {
        for purpose in [
            crate::cache::DownloadPurpose::Normal,
            crate::cache::DownloadPurpose::Upgrade,
        ] {
            let name = super::part_file_name("123", "lossless", purpose);
            assert!(!name.ends_with(".part"), "{name} 不能再带 .part 后缀");
        }
    }

    /// 正常下载与升级补音质必须两个名字。
    /// 升级刻意不抢播放的锁，两条路会同时下同一首；名字相同的话后启动那条会被
    /// libresoda 用「目标正被别的下载占着」顶回来。
    #[test]
    fn part_name_separates_normal_download_from_upgrade() {
        let normal =
            super::part_file_name("123", "lossless", crate::cache::DownloadPurpose::Normal);
        let upgrade =
            super::part_file_name("123", "lossless", crate::cache::DownloadPurpose::Upgrade);
        assert_ne!(normal, upgrade, "正常下载与升级必须落在不同暂存名上");
        assert_eq!(normal, "123-lossless-normal.m4a");
        assert_eq!(upgrade, "123-lossless-upgrade.m4a");
    }

    /// 上次被杀留下的交付目标会挂在下次要用的同一组名字上，stage_path 清掉它；
    /// 续传暂存（`.part` 数据 / `.part.meta` 记录 / `.part.lock`）必须原样保留，
    /// 那是下次接上缺口的起点（#243）。租约照旧挂在暂存目标上挡住维护线程。
    #[test]
    fn stage_path_discards_stale_target_but_keeps_resumable_staging() {
        let dir = tempfile::tempdir().unwrap();
        let stage = super::staging_dir(dir.path());
        std::fs::create_dir_all(&stage).unwrap();
        let stale = stage.join("123-lossless-normal.m4a");
        let stale_part = stage.join("123-lossless-normal.m4a.part");
        let stale_meta = stage.join("123-lossless-normal.m4a.part.meta");
        let stale_lock = stage.join("123-lossless-normal.m4a.part.lock");
        std::fs::write(&stale, b"leftover target").unwrap();
        std::fs::write(&stale_part, b"leftover part data").unwrap();
        std::fs::write(&stale_meta, b"leftover record").unwrap();
        std::fs::write(&stale_lock, b"").unwrap();

        // 正式缓存里放一个同名文件：自定义档位 auto-normal 会撞上这个暂存名，
        // 清理暂存绝不能删掉它。
        let committed = dir.path().join("123-lossless-normal.m4a");
        std::fs::write(&committed, b"committed cache").unwrap();

        let (part, lease) = super::stage_path(
            dir.path(),
            "123",
            "lossless",
            crate::cache::DownloadPurpose::Normal,
        )
        .expect("建暂存目录该成功");

        assert_eq!(part, stale, "暂存目标落在独立的 staging 子目录");
        assert!(!stale.exists(), "上次留下的交付目标必须清掉");
        assert_eq!(
            std::fs::read(&stale_part).unwrap(),
            b"leftover part data",
            "半截的 .part 必须保留：那是下次续传的起点"
        );
        assert_eq!(
            std::fs::read(&stale_meta).unwrap(),
            b"leftover record",
            ".part.meta 记录必须保留：没有它续传对就对不上"
        );
        assert!(stale_lock.exists(), ".part.lock 留着：内核锁靠这个文件挂名");
        assert_eq!(
            std::fs::read(&committed).unwrap(),
            b"committed cache",
            "同名正式缓存不能被清理误删"
        );
        assert_eq!(lease.0.as_path(), part.as_path(), "租约挂在暂存目标上");
    }

    /// 暂存目录建不出来时不能拿着写不进去的路径往下走，要给确定性的落盘失败。
    #[test]
    fn stage_path_reports_unpack_failure_when_staging_dir_cannot_be_created() {
        let dir = tempfile::tempdir().unwrap();
        // 用同名普通文件占住 staging，create_dir_all 必然失败。
        std::fs::write(dir.path().join("staging"), b"occupied").unwrap();

        let err = match super::stage_path(
            dir.path(),
            "123",
            "lossless",
            crate::cache::DownloadPurpose::Normal,
        ) {
            Ok(_) => panic!("建不出目录该报错，不能继续"),
            Err(err) => err,
        };

        assert_eq!(err.stage, crate::failure::Stage::Unpack);
        assert_eq!(err.action, crate::failure::RecoveryAction::FreeSpace);
        assert!(
            err.detail.contains("staging"),
            "错误里要带上路径: {}",
            err.detail
        );
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

/// 读取 sidecar 后、构造 [`CachedTrack`] 前的中间结果。
struct CachedInfo {
    path: std::path::PathBuf,
    quality: String,
    preview: bool,
    quality_rank: Option<i64>,
    checked_rank: Option<i64>,
    unknown_probe: Option<crate::cache::UnknownProbe>,
    /// 上次补整曲的尝试仍在退避期内；只对试听片段可能为真。
    preview_retry_blocked: bool,
}

impl Session {
    /// 当前音质**意图**对应的缓存标签。
    ///
    /// 缓存按意图命名，所以「自动」下同一首歌始终命中同一个文件：
    /// 权益升级后仍会先播到非会员时期缓存下来的档位，直到清空缓存或换意图。
    fn quality_tag(&self) -> String {
        self.inputs.quality_intent().cache_tag()
    }

    /// 解析 sidecar 并与实际字节数核对。字段布局见 [`crate::cache::parse_sidecar`]。
    ///
    /// 只认「非空文件」会让被截断的半截下载永久冒充有效缓存，所以尺寸不符或档位为空一律 miss。
    /// 实际档位缺失时视为未知，不据此标记降档；确认探测、试听补整曲和未知档探测各自记账，
    /// 不能互相冒充。更早把试听拼在档位里的格式仍能读回。
    fn cached_info(&self, track_id: &str) -> Option<CachedInfo> {
        let safe_id = encode_track_id(track_id);
        let path = crate::audio::cache_dir().join(format!("{safe_id}-{}.m4a", self.quality_tag()));
        let actual = std::fs::metadata(&path).ok()?.len();
        if actual == 0 {
            return None;
        }
        let quality_path =
            crate::audio::cache_dir().join(format!("{safe_id}-{}.quality", self.quality_tag()));
        let text = std::fs::read_to_string(&quality_path).ok()?;
        let meta = crate::cache::parse_sidecar(&text, actual)?;
        let now = crate::cache::now_epoch_secs();
        let checked = meta.fresh_checked_rank(now);
        let unknown_probe = meta.fresh_unknown_probe(now);
        let preview_retry_blocked = meta.fresh_preview_attempt_at(now).is_some();
        Some(CachedInfo {
            path,
            quality: meta.quality,
            preview: meta.preview,
            quality_rank: meta.actual_rank,
            checked_rank: checked,
            unknown_probe,
            preview_retry_blocked,
        })
    }

    pub fn cached_track(&self, track_id: &str) -> Option<CachedTrack> {
        let safe_id = encode_track_id(track_id);
        let path = crate::audio::cache_dir().join(format!("{safe_id}-{}.m4a", self.quality_tag()));
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
            unknown_probe: info.unknown_probe,
            preview_retry_blocked: info.preview_retry_blocked,
            lease,
        })
    }

    /// 这首是否已经在本地缓存里（**按当前音质档位**判断，
    /// 且要求 sidecar 大小校验一致；切换音质后旧档位的缓存不算命中）。
    pub fn is_cached(&self, track_id: &str) -> bool {
        // 这里只是调度提示，不返回可播放文件，也不建立租约或刷新 LRU 时间。
        self.cached_info(track_id).is_some()
    }

    /// 这首是否有**整曲**缓存：试听片段不算。
    ///
    /// 显式点播的不可播拦截只给已落盘的整曲让路（父票边界）；试听片段是
    /// 「这一次只拿到试听」的证据，不能反过来当作整曲可播。
    pub fn has_full_track_cached(&self, track_id: &str) -> bool {
        Self::full_track_cached(self.cached_info(track_id))
    }

    /// 判定单独拆出来，让「试听片段不算整曲」这条边界能被离线测试。
    fn full_track_cached(info: Option<CachedInfo>) -> bool {
        info.is_some_and(|info| !info.preview)
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
    /// 这条流的档位分数（与取流筛选同一套刻度）；`None` = 旧缓存没记，`Some(0)` = 记了但
    /// 认不出档位。两者都算未知，但只有前者值得重下一次去量（见 [`CachedTrack::needs_upgrade`]）。
    ///
    /// 与所选是否相符是**每次读取时现算**的：用当前意图与这里的实际档位比较，
    /// 结论不写回缓存——否则用户改了意图，缓存里的旧结论就成了假话。
    pub quality_rank: Option<i64>,
    /// 仍在有效期内、已探测过该目标档位的记录；`None` = 没问过或记录已过期。
    ///
    /// 只用于决定是否再发起升级，不改变降档提示：曲目确实还在低档位播放。
    pub checked_rank: Option<i64>,
    /// 完整流探测过但档位未知的独立退避记录；不是确认性 miss，也不代表旧文件已测量。
    pub unknown_probe: Option<crate::cache::UnknownProbe>,
    /// 上次补整曲的尝试还在退避期内（见 [`crate::cache::PREVIEW_RETRY_TTL`]）。
    ///
    /// 只可能对试听片段为真。它挡的是「应用每次启动、每次命中都整曲下一遍」，
    /// 不是「以后都不再试」：退避期一过仍会再要一次。
    pub preview_retry_blocked: bool,
    pub(crate) lease: crate::cache::CacheLease,
}

impl CachedTrack {
    /// 实际档位对应的显示名（见 [`crate::quality::tier_label`]；档位未知返回 `None`）。
    pub fn actual_tier(&self) -> Option<&'static str> {
        self.quality_rank.map(crate::quality::tier_label)
    }

    /// 能拿来比大小的档位：记下来的分数，或旧「无损」标签给出的下限。
    ///
    /// `None` = 旧三字段缓存，标签也证不出档位——这种记录没有可比的分数，只能看码率。
    fn known_rank(&self) -> Option<i64> {
        self.quality_rank
            .or_else(|| crate::quality::legacy_label_floor_rank(&self.quality))
    }

    /// 命中缓存后是否还需要向服务端要一次流。
    ///
    /// * 试听片段（#130）：目标是整曲，不是更高档位。只要上次补整曲的尝试已过退避期
    ///   就再试一次，不看 `quality_rank`——尤其不能被认不出的 0 分挡住。试听本来只有
    ///   半条流，重下不是白跑；权益到手后拿到整曲是实打实的提升。
    /// * 整曲：实际档位已达目标、最近已经向服务端要过该目标档位，或同账号范围、同目标的
    ///   未知档探测还在退避期时，都不再请求。没记分数的旧缓存按 sidecar 的档位标签
    ///   判断：证明不了达标就重下；记了 0 分的不重下。
    pub fn needs_upgrade(
        &self,
        intent: &crate::quality::QualityIntent,
        is_vip: Option<bool>,
        scope: Option<&str>,
    ) -> bool {
        if self.preview {
            return !self.preview_retry_blocked;
        }
        let target_rank = crate::quality::target_rank(intent, is_vip);
        crate::quality::is_tier_below_selected(self.quality_rank, &self.quality, intent, is_vip)
            && !crate::cache::upgrade_check_covers(self.checked_rank, target_rank)
            && !self.unknown_probe_covers(target_rank, scope)
    }

    /// 播放命中时是否值得**自动**发起后台升级。
    ///
    /// 整曲与 [`Self::needs_upgrade`] 只差一种情形：档位未知的旧缓存，如果这次探测的结论
    /// 根本没法写进 sidecar（账号权益未确认，见 `can_record_unavailable`），那这个结论
    /// 就留不下来——本会话内的去重（`cache_upgrade_attempted`）挡得住连续切歌，
    /// 挡不住下次启动再来一遍。对存量缓存来说那等于每次启动都把这首歌整曲下一遍、
    /// 再发现没有更高的流。自动路径不该替用户做这种决定；用户主动点的批量升级仍走
    /// `needs_upgrade`，那一下是他自己同意花流量的。
    ///
    /// 试听补整曲有自己的退避记录（sidecar 第 7 字段），所以不必借用「结论能不能写下来」
    /// 这道闸；但仍要过权益预检：需要会员的档位在账号确认不是会员时不自动重下，
    /// 那只会拿回另一条试听。用户主动批量升级的加入与否是单独的产品决定，本票不改。
    pub fn needs_auto_upgrade(
        &self,
        intent: &crate::quality::QualityIntent,
        is_vip: Option<bool>,
        scope: Option<&str>,
    ) -> bool {
        if self.preview {
            return self.needs_upgrade(intent, is_vip, scope)
                && !crate::quality::batch_upgrade_needs_confirmed_entitlement(intent, is_vip);
        }
        if !self.needs_upgrade(intent, is_vip, scope) {
            return false;
        }
        self.quality_rank.is_some()
            || crate::quality::can_record_unavailable(
                crate::quality::target_rank(intent, is_vip),
                is_vip,
            )
    }

    fn unknown_probe_covers(&self, target_rank: Option<i64>, scope: Option<&str>) -> bool {
        target_rank.is_some_and(|target| {
            self.unknown_probe
                .as_ref()
                .is_some_and(|probe| probe.covers(target, scope))
        })
    }
}

/// 一次升级判定的结果：返回时内存里的缓存记录，以及这次有没有换成更好的流。
///
/// `not_improved_reason` 在判定结果那一刻定下来：换文件、比完流，或者决定这次不发请求。
/// `None` 表示升级成功，`Some` 表示没换成更好的流；未知结果单独记，不当成确认没有更高档。
#[derive(Debug, Clone, PartialEq)]
pub struct UpgradeRun {
    /// 返回时的内存快照；可能已带上新退避，但盘上的完整记录以 sidecar 为准。
    pub track: CachedTrack,
    /// 没换成更好的流时的日志原因；成功升级时为 `None`。
    pub not_improved_reason: Option<NotImprovedReason>,
}

impl UpgradeRun {
    fn not_improved(track: CachedTrack, reason: NotImprovedReason) -> Self {
        Self {
            track,
            not_improved_reason: Some(reason),
        }
    }

    fn improved(track: CachedTrack) -> Self {
        Self {
            track,
            not_improved_reason: None,
        }
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

/// 续传发生时把传输事实记进日志（resumed_from / transferred / total_len）：
/// 「transferred 是否明显小于 total_len」由看日志的人判断——有值就说明这次
/// 是接在上次那段字节后面补的。点播与升级两条路共用（#243）。
fn log_resumed_download(report: &libresoda::soda::download::DownloadReport) {
    let Some(from) = report.resumed_from else {
        return;
    };
    use crate::log_outlet::{log_event, LogLevel, LogTopic};
    let from = from.to_string();
    let transferred = report.transferred.to_string();
    let total = report.total_len.to_string();
    log_event(
        LogLevel::Info,
        LogTopic::Audio,
        "download_resumed",
        &[
            ("resumed_from", from.as_str()),
            ("transferred", transferred.as_str()),
            ("total_len", total.as_str()),
        ],
    );
}

/// 把「已经向服务端要过这个档位、没拿到更高的流」写回 sidecar；失败不影响播放，下次再探。
fn record_upgrade_check(
    quality_path: &std::path::Path,
    audio_path: &std::path::Path,
    actual_rank: Option<i64>,
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

/// 记下「完整流探测过但档位未知」；失败不影响播放，下次再探。
fn record_unknown_probe(
    quality_path: &std::path::Path,
    audio_path: &std::path::Path,
    actual_rank: Option<i64>,
    target_rank: i64,
    scope: &str,
    now: u64,
) -> bool {
    let lock = crate::cache::download_lock(audio_path);
    let _guard = lock.lock().unwrap_or_else(|err| err.into_inner());
    crate::cache::record_unknown_probe(quality_path, actual_rank, target_rank, scope, now)
        .unwrap_or(false)
}

/// 记下「刚为补整曲重新取过一次流」；失败不影响播放，退避没写成下次再试。
fn record_preview_upgrade_attempt(
    quality_path: &std::path::Path,
    audio_path: &std::path::Path,
    actual_rank: Option<i64>,
) -> bool {
    // 与替换路径共用同一把写锁，避免退避时间落到并发替换后的新文件上。
    let lock = crate::cache::download_lock(audio_path);
    let _guard = lock.lock().unwrap_or_else(|err| err.into_inner());
    crate::cache::record_preview_attempt(quality_path, actual_rank, crate::cache::now_epoch_secs())
        .unwrap_or(false)
}

/// 确认性 miss：拿到完整流、档位可识别、目标存在、权益已确认，且实际档位低于目标。
///
/// 返回应写入 sidecar 的 `checked_rank`。试听回退、权益未确认、档位未知、目标未知或
/// 实际已经达标都不写——这些不是「要过这个档位、服务端没给更高流」的确认性结论。
/// 写下来的记录只保 30 天（见 [`crate::cache::UPGRADE_CHECK_TTL`]）。
/// 试听回退也不写：它不是「曲库没有更高档位」，只是「这次只给了半条流」；
/// 补整曲的重试节奏另由 sidecar 第 7 字段的退避记录管理（#130）。
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

/// 档位未知的旧缓存拿回一条正好到目标（或更高）的完整流时，该记下哪个「问过的目标档位」。
///
/// 这种文件证不出更好、不会换，但「问过这个目标」仍要写进 sidecar，否则每轮都整曲重下。
/// 档位已知或拿回试听时走 [`confirmed_miss_rank`]，这里不适用。
fn legacy_probe_covers_target(
    current: &CachedTrack,
    info: &libresoda::soda::types::DownloadInfo,
    target_rank: Option<i64>,
    can_record: bool,
) -> Option<i64> {
    if current.known_rank().is_some() || info.is_preview || !can_record {
        return None;
    }
    let new_rank = crate::quality::stream_rank(info);
    target_rank.filter(|target| new_rank >= *target)
}

/// 新取到的试听流是否会顶掉已有的整曲缓存。
///
/// 这条护栏与档位高低无关：整曲永远比试听完整，哪怕试听那条流的档位分数更高。
/// 单独拆出来是为了让「试听不覆盖整曲」能离线测（`upgrade_cached_track` 本身要网络）。
/// 参数用真实类型而不是两个布尔，调用点写反顺序编译不过。
fn would_downgrade_to_preview(
    info: &libresoda::soda::types::DownloadInfo,
    current: &CachedTrack,
) -> bool {
    info.is_preview && !current.preview
}

/// 这次取到的新流是不是真的比本地已有的那一份更好（值得换文件，也算一次升级）。
///
/// 试听只换整曲：试听换试听不算提升，盘上仍是片段。档位已知（或旧「无损」标签给出下限）
/// 只认分数真变高。档位未知的旧三字段缓存只剩标签可用，直接比标签里记着的码率，严格更高
/// 才算提升；同码率不算，旧标签连码率都没记时也没有可比证据。
///
/// 无损另算：这类旧记录写于 Hi-Res / 杜比命名之前，非无损标签说明旧流不是无损，新流升到
/// 无损就是提升，哪怕码率更低（无损 FLAC 本来就可能比有损的高码率流小）。
fn is_better_stream(info: &libresoda::soda::types::DownloadInfo, current: &CachedTrack) -> bool {
    let new_rank = crate::quality::stream_rank(info);
    if current.preview {
        return !info.is_preview;
    }
    if let Some(old_rank) = current.known_rank() {
        return new_rank > old_rank;
    }
    // 旧标签证不出档位：先看新流是否已经升到无损。
    if crate::quality::lossless_rank().is_some_and(|lossless| new_rank >= lossless) {
        return true;
    }
    // 否则只能比码率，严格更高才算提升；同码率留在旧文件，如实报「没换成更好的流」。
    let Some(old_bitrate) = crate::quality::legacy_label_bitrate_kbps(&current.quality) else {
        return false;
    };
    let new_bitrate = libresoda::soda::quality::normalize_bitrate(info.bitrate.max(0));
    new_rank > 0 && new_bitrate > old_bitrate
}

fn skipped_upgrade_reason(
    current: &CachedTrack,
    target_rank: Option<i64>,
    scope: Option<&str>,
) -> NotImprovedReason {
    if current.preview && current.preview_retry_blocked {
        return NotImprovedReason::PreviewBackoff;
    }
    if current.quality_rank == Some(0) || current.unknown_probe_covers(target_rank, scope) {
        return NotImprovedReason::UnknownRank;
    }
    let Some(target_rank) = target_rank else {
        // 目标档位未知，这里说不出更具体的原因，只能记 not_better。
        return NotImprovedReason::NotBetter;
    };
    if current.known_rank().is_some_and(|rank| rank >= target_rank)
        || crate::cache::upgrade_check_covers(current.checked_rank, Some(target_rank))
    {
        NotImprovedReason::AlreadyChecked
    } else {
        NotImprovedReason::NotBetter
    }
}

fn downloaded_not_improved_reason(
    current: &CachedTrack,
    confirmed_miss: bool,
) -> NotImprovedReason {
    // 调用方已挡掉试听与未知档位；这里只区分确认没有更高档和只是没换成更好的流。
    let old_quality_is_comparable = current.known_rank().is_some()
        || crate::quality::legacy_label_bitrate_kbps(&current.quality).is_some();
    if confirmed_miss && !old_quality_is_comparable {
        NotImprovedReason::NoHigherTier
    } else {
        NotImprovedReason::NotBetter
    }
}

/// 拼出 sidecar 内容。
///
/// 前四字段始终写；有探测结论时补第 5、6 字段。第 7 字段（试听补整曲的尝试时间）
/// 由 [`crate::cache::record_preview_attempt`] 单独维护，这个函数不写它、也不留空位。
fn encode_sidecar(
    quality: &str,
    written: u64,
    preview: bool,
    actual_rank: i64,
    checked_rank: Option<i64>,
) -> String {
    let flag = u8::from(preview);
    match checked_rank {
        Some(rank) => format!(
            "{quality}\t{written}\t{flag}\t{actual_rank}\t{rank}\t{}",
            crate::cache::now_epoch_secs(),
        ),
        None => format!("{quality}\t{written}\t{flag}\t{actual_rank}"),
    }
}

#[derive(Clone, Copy)]
struct ProbeCheck {
    checked_rank: Option<i64>,
    confirmed_miss: bool,
}

fn probe_check(
    current: &CachedTrack,
    info: &libresoda::soda::types::DownloadInfo,
    target_rank: Option<i64>,
    can_record: bool,
) -> ProbeCheck {
    let confirmed_miss = confirmed_miss_rank(
        target_rank,
        crate::quality::stream_rank(info),
        info.is_preview,
        can_record,
    );
    let checked_rank = confirmed_miss
        .or_else(|| legacy_probe_covers_target(current, info, target_rank, can_record));
    ProbeCheck {
        checked_rank,
        confirmed_miss: confirmed_miss.is_some(),
    }
}

fn finish_unimproved_upgrade(
    current: &mut CachedTrack,
    info: &libresoda::soda::types::DownloadInfo,
    target_rank: Option<i64>,
    can_record: bool,
    probe: ProbeCheck,
    scope: Option<&str>,
    quality_path: &std::path::Path,
) -> NotImprovedReason {
    let audio_path = current.path.clone();
    let new_rank = crate::quality::stream_rank(info);
    if current.preview {
        current.preview_retry_blocked |=
            record_preview_upgrade_attempt(quality_path, &audio_path, current.quality_rank);
        return NotImprovedReason::PreviewOnly;
    }
    if new_rank <= 0 {
        if !can_record {
            return NotImprovedReason::UnknownRank;
        }
        let (Some(target), Some(scope)) = (target_rank, scope) else {
            return NotImprovedReason::UnknownRank;
        };
        let now = crate::cache::now_epoch_secs();
        if record_unknown_probe(
            quality_path,
            &audio_path,
            current.quality_rank,
            target,
            scope,
            now,
        ) {
            current.unknown_probe = Some(crate::cache::UnknownProbe {
                target_rank: target,
                at: now,
                scope: scope.to_string(),
            });
        }
        return NotImprovedReason::UnknownRank;
    }
    if let Some(checked_rank) = probe.checked_rank {
        record_upgrade_check(
            quality_path,
            &audio_path,
            current.quality_rank,
            checked_rank,
        );
    }
    downloaded_not_improved_reason(current, probe.confirmed_miss)
}

/// 一次进程内共享的会话（持有 `Soda` 和它要的那几样设置）。
pub struct Session {
    pumpkin: Soda,
    /// 会话就认这一份输入：凭据、签名与设备身份、音质意图。
    inputs: SessionInputs,
}

impl Session {
    /// 用给定设置构建会话：注入 Cookie、设备指纹、签名器与音质偏好。
    ///
    /// 收 `impl Into<SessionInputs>`：`Settings` / `&Settings` 都能直接给，
    /// 现有构造点一处不用改。会话只认会话输入那几个字段，别的拿不进来。
    pub fn new(inputs: impl Into<SessionInputs>) -> Self {
        let inputs = inputs.into();
        let pumpkin = Soda::new(inputs.cookie.clone());
        pumpkin.set_app_credentials(AppCredentials {
            device_id: inputs.device_id.clone(),
            iid: inputs.iid.clone(),
            fp: inputs.fp.clone(),
            ..Default::default()
        });
        // 登录设备身份由 sodam 配置持有；libresoda 只接收，不替应用决定存哪里。
        // 配置异常时兜底生成一份，避免整次登录直接失败（下一次配置保存会修正）。
        if pumpkin.set_qr_device(inputs.login_device()).is_err() {
            let _ = pumpkin.set_qr_device(libresoda::soda::qr_login::QrDevice::generate());
        }

        let url = inputs.signer_url.trim();
        if !url.is_empty() {
            let provider = HttpSignature::new(url).with_token(inputs.signer_token.trim());
            pumpkin.set_signature_provider(Arc::new(provider));
        }
        // 「自动」= 不设上限，交给服务端按权益与曲目可用性收敛；
        // 只有具体档位才需要把上限告诉 libresoda。
        if let crate::quality::QualityIntent::Tier(tier) = inputs.quality_intent() {
            pumpkin.set_quality_preference(tier);
        }
        // 登录走 libresoda 的纯 HTTP 实现：passport 请求由本进程直接发出，Cookie 由本进程
        // 持有；遇到二次验证（2046）时用系统默认浏览器打开本机验证页，验证页的请求经
        // libresoda 的本地桥接代发。不依赖 Chromium / CDP。
        pumpkin.enable_http_login();

        Self { pumpkin, inputs }
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

    /// 把曲目下载并**解密**到本地缓存，返回文件路径（已存在则直接复用）。
    ///
    /// 走 libresoda 的可续传路径 [`Soda::download_resumable`]：密文先暂存到
    /// `{part}.part`，传输被掐断后留下的字节就是下次的起点，重试只补缺口；
    /// 服务器不支持范围请求时库内退回整份。VIP 整曲需要签名服务，由 libresoda 处理。
    ///
    /// 失败返回 [`PlaybackFailure`]（阶段 + 可否重试 + 修复动作），不再压成一句
    /// 「下载失败」：一次播放请求串着地址解析、签名、音频体传输、解密落盘四类失败，
    /// 把它们混在一起既误导归因，也让永久错误白烧重试次数。
    /// 相位归因有上限：`download_resumable` 内部同时做了前三件事，
    /// 所以那一段的失败只能靠 `classify` 从 `SodaError` 的形态尽量还原。
    ///
    /// 失败不清暂存：暂存留不留由 libresoda 按「这段字节以后还有没有用」决定
    /// （下架/字节不可信它自己删，传输中断/磁盘满它保留），本侧不再用
    /// 「瞬时/永久」重判一遍去删数据。
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
                format!("{}: {err}", dir.display()),
            )
        })?;
        // 缓存文件名带音质档位：切换音质后自然 miss，重新拉对应档位的流
        //（正在播放的那首不受影响，因为它已经装载进引擎了）。
        let tag = self.quality_tag();
        let safe_id = encode_track_id(&track.id);
        let path = dir.join(format!("{safe_id}-{tag}.m4a"));
        // sidecar 记录「音质标签 + 实际字节数 + 是否试听」，用于命中时的大小校验。
        let quality_path = dir.join(format!("{safe_id}-{tag}.quality"));
        // 快速路径：若当前已有有效缓存命中，直接返回，无需排队等待锁
        if let Some(cached) = self.cached_track(&track.id) {
            return Ok(cached);
        }
        let download_lock = crate::cache::download_lock(&path);
        let _writer = download_lock.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = self.cached_track(&track.id) {
            return Ok(cached);
        }
        let intent = self.inputs.quality_intent();
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
        // 走可续传路径：密文暂存 `{part}.part`，下完在库内解密交付到 `part`，
        // 校验完整后再改名到正式缓存 —— 下载中断永远留不下「看起来有效」的半截缓存。
        // 暂存名固定为「曲目-档位-用途」，重复下载复用同一套名字与锁；
        // 正常下载与升级分开，两条路并发下同一首时互不撞锁。
        // 失败不删暂存：留住的字节是下一次的起点（#243）。
        let (part, _part_lease) =
            stage_path(&dir, &track.id, &tag, crate::cache::DownloadPurpose::Normal)?;
        let report = self
            .pumpkin
            .download_resumable(&song, &part)
            .map_err(|err| classify(&err))?;
        log_resumed_download(&report);
        // 档位 / 试听标记取自这次传输自己的 `stream_info`：它跟落盘字节来自
        // 同一次取流，不会出现「整曲文件 + 试听标记」的错配（libresoda#55）。
        let info = report.stream_info;

        // 完整性校验：只拒绝「比服务端声明更少」的截断下载。可续传路径已保证
        //「盘上累计 = 这次 Content-Range 总长」，静默 EOF 在库内就会被拦下；
        // 这道闸只剩兜底作用。允许实际字节数更多：个别曲目的 play_info size
        // 元数据会滞后于真实资产（实测存在声明 10,569,897 / 实际 10,660,358 的
        // 完整可播文件），等值校验会误杀。此时暂存对已被交付作废，删的是
        // 交付出来的成品，不是续传数据。
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
        // 首次下载拿到试听时不写退避：下一次播放命中应该立刻再试一次，
        // 用户可能刚补上权益或签名服务刚恢复。退避从「后台升级仍拿回试听」才开始记。
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
            unknown_probe: None,
            preview_retry_blocked: false,
            lease,
        })
    }

    /// 升级单首已缓存曲目的音频流到指定意图。
    ///
    /// 在后台拉取新流到临时文件，拉取并校验成功后通过 [`crate::cache::atomic_replace_cache`]
    /// 原子替换旧文件。
    ///
    /// * 升级期间持有待升级目标与临时文件的 [`crate::cache::CacheLease`]，避免被淘汰删除；
    /// * 若中途失败，暂存留在 staging 子目录给下次续传（#243），绝不破坏原有缓存；
    /// * 若新拉到的档位并不确实高于当前已有档位，清理临时文件，保留原有缓存并如实
    ///   报「没换」（`not_improved_reason.is_some()`）；档位未知的旧三字段缓存按标签能证明的
    ///   下限与码率比较；
    /// * 完整流没提升时写确认探测；拿到认不出的完整流时改写独立的未知探测退避，
    ///   保留实际档位与确认字段，同账号范围、同目标 30 天内不重复下载。
    pub fn upgrade_cached_track(
        &self,
        track: &crate::models::TrackItem,
        target_intent: Option<&crate::quality::QualityIntent>,
        account_is_vip: Option<bool>,
        account_user_id: Option<&str>,
    ) -> Result<UpgradeRun, crate::failure::PlaybackFailure> {
        use crate::failure::{classify, PlaybackFailure, RecoveryAction, Retryability, Stage};

        let dir = crate::audio::cache_dir();
        let tag = self.quality_tag();
        let safe_id = encode_track_id(&track.id);
        let path = dir.join(format!("{safe_id}-{tag}.m4a"));
        let quality_path = dir.join(format!("{safe_id}-{tag}.quality"));

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

        let default_intent = self.inputs.quality_intent();
        let intent = target_intent.unwrap_or(&default_intent);
        // 调用方已经拿到账号权益时直接复用，保证探测与退避使用同一个范围；
        // 没拿到再探一次。非会员请求无损失败不代表曲库没有无损，买会员后必须重新尝试。
        let vip = account_is_vip.or_else(|| self.is_vip().ok());
        let target_rank = crate::quality::target_rank(intent, vip);
        // 没有会员权益时拿不到更高档位，不能据此断言曲库没有；等权益确认后再记录。
        let can_record_check = crate::quality::can_record_unavailable(target_rank, vip);
        let scope = crate::cache::unknown_probe_scope(account_user_id, vip);

        // 如果已有档位并不低于目标档位，直接复用已有缓存。
        // 最近已经向服务端要过该目标档位时同样不再请求，避免跨会话重复下载。
        if !current.needs_upgrade(intent, vip, scope.as_deref()) {
            let reason = skipped_upgrade_reason(&current, target_rank, scope.as_deref());
            return Ok(UpgradeRun::not_improved(current, reason));
        }

        // 串行化升级操作并获取租约：
        // 升级使用独立锁，不与普通点播播放竞争同把锁，确保升级进行中同一曲目重播秒开
        let upgrade_lock = crate::cache::download_lock(&path.with_extension("upgrade-lock"));
        let _writer = upgrade_lock.lock().unwrap_or_else(|err| err.into_inner());

        // 获取锁后重读当前缓存，防止并发升级已将其提升
        let mut current = match self.cached_track(&track.id) {
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
        if !current.needs_upgrade(intent, vip, scope.as_deref()) {
            let reason = skipped_upgrade_reason(&current, target_rank, scope.as_deref());
            return Ok(UpgradeRun::not_improved(current, reason));
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

        // 升级用自己的暂存名，和点播下载并行不撞 libresoda 的目标锁。
        let (part, _part_lease) = stage_path(
            &dir,
            &track.id,
            &tag,
            crate::cache::DownloadPurpose::Upgrade,
        )?;

        // 可续传路径，失败不清暂存：升级被掐断后，下次升级从缺口接着补（#243）。
        // 若指定了目标具体档位，创建临时会话或设置档位偏好以获取更高档位
        let report = if let crate::quality::QualityIntent::Tier(tier) = intent {
            let mut upgrade_inputs = self.inputs.clone();
            upgrade_inputs.quality = tier.clone();
            let upgrade_session = Session::new(upgrade_inputs);
            upgrade_session.pumpkin.download_resumable(&song, &part)
        } else {
            self.pumpkin.download_resumable(&song, &part)
        }
        .map_err(|err| {
            // 补整曲失败也记一次退避：否则应用每次启动都会把同一首整曲下一遍再失败。
            if current.preview {
                record_preview_upgrade_attempt(&quality_path, &path, current.quality_rank);
            }
            classify(&err)
        })?;
        log_resumed_download(&report);
        // 与点播同一条口径：档位 / 试听标记用这次传输自己的 stream_info。
        let info = report.stream_info;

        let written = std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
        if written == 0 || is_truncated_download(info.size, written) {
            let _ = std::fs::remove_file(&part);
            if current.preview {
                record_preview_upgrade_attempt(&quality_path, &path, current.quality_rank);
            }
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
        if would_downgrade_to_preview(&info, &current) {
            let _ = std::fs::remove_file(&part);
            return Ok(UpgradeRun::not_improved(
                current,
                NotImprovedReason::PreviewOnly,
            ));
        }

        let new_rank = crate::quality::stream_rank(&info);
        // 档位已知按确认没有更高档判；档位未知的旧缓存拿回正好到目标的完整流也算「问过」，
        // 不换文件也要记下来，否则每轮都整曲重下。
        let probe = probe_check(&current, &info, target_rank, can_record_check);
        let checked_rank = probe.checked_rank;
        // 若新流并不比本地这份更好，丢弃临时文件保留旧文件。怎么比见 `is_better_stream`。
        if !is_better_stream(&info, &current) {
            let _ = std::fs::remove_file(&part);
            let reason = finish_unimproved_upgrade(
                &mut current,
                &info,
                target_rank,
                can_record_check,
                probe,
                scope.as_deref(),
                &quality_path,
            );
            return Ok(UpgradeRun::not_improved(current, reason));
        }

        // 试听在上面已经被挡住：走到这里一定是整曲替换。
        debug_assert!(!info.is_preview, "试听流不该走到整曲替换");
        let quality = describe_quality(&info);
        let sidecar_content = encode_sidecar(&quality, written, false, new_rank, checked_rank);

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
        Ok(UpgradeRun::improved(CachedTrack {
            path,
            quality,
            preview: false,
            quality_rank: Some(new_rank),
            checked_rank,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease: target_lease,
        }))
    }

    /// 测试用：看一眼会话手里那份输入（界面那份在 `Root.settings`）。
    #[cfg(test)]
    pub(crate) fn inputs(&self) -> &SessionInputs {
        &self.inputs
    }

    /// 换一份会话输入，重建会话：凭据、签名、设备身份、音质这些改了，都得走这里才生效。
    ///
    /// 只重建会话，不碰磁盘——要不要写盘、什么时候写，由调用方决定。
    pub fn adopt(&mut self, inputs: impl Into<SessionInputs>) {
        *self = Self::new(inputs);
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
        self.inputs.has_cookie()
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
    /// 原图按显示需要缩小（长边 ≤ 1280）后落盘 JPEG，缩略图同理 ≤ 160；
    /// 文件名与目录布局不变，旧缓存读取时按内容解码，继续可用。
    pub fn original_cover_path(&self, url: &str) -> Option<std::path::PathBuf> {
        let url = url.trim();
        if url.is_empty() {
            return None;
        }
        let dir = crate::audio::cover_cache_dir().join("original");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("{:016x}.img", hash_url(url)));
        if cover_file_usable(&path) {
            touch_cover_file(&path);
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
        // 按歌词页显示需要缩小再存 JPEG：原始字节直接落盘太占空间。
        // 源图解不开就不落盘：界面显示占位图，本会话不自动重试（重试按钮/重启后再取）。
        let Some(shrunk) = shrink_cover(&bytes, COVER_ORIGINAL_MAX_EDGE) else {
            log_cover_shrink_failed(&path);
            return None;
        };
        // 先写临时文件再整体换上去：写到一半断了也不会留下半张图。
        if !save_cover_file(&path, &shrunk) {
            return None;
        }
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
        if cover_file_usable(&path) {
            touch_cover_file(&path);
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
        // 缩到列表实际需要的尺寸再存 JPEG：40px 的缩略图不需要 500px 的原图。
        // 源图解不开就不落盘：界面显示占位图，本会话不自动重试（重试按钮/重启后再取），
        // 不再把原始字节当兜底缓存。
        let Some(shrunk) = shrink_cover(&bytes, COVER_MAX_EDGE) else {
            log_cover_shrink_failed(&path);
            return None;
        };
        if !save_cover_file(&path, &shrunk) {
            return None;
        }
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

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
        assert_eq!(session.inputs().quality, "lossless");
    }

    /// 0 字节、或者目标根本不是个普通文件，都算没缓存：这种文件解码不出来，得重新下。
    #[test]
    fn unusable_cover_files_are_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0000abcd-160.img");

        assert!(!cover_file_usable(&path), "文件不在就是没缓存");
        std::fs::write(&path, b"").unwrap();
        assert!(!cover_file_usable(&path), "0 字节等于没写");
        std::fs::write(&path, b"cover-bytes").unwrap();
        assert!(cover_file_usable(&path));

        let dir_path = dir.path().join("0000abcd-320.img");
        std::fs::create_dir(&dir_path).unwrap();
        assert!(!cover_file_usable(&dir_path), "目录不是缓存文件");
    }

    /// 造一张 PNG 封面字节，供 `shrink_cover` 离线测试用。
    fn png_cover_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                img.put_pixel(x, y, image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
            }
        }
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .expect("测试 PNG 必须编得出来");
        out.into_inner()
    }

    /// 大图缩到目标长边，且保持等比。
    #[test]
    fn shrink_cover_scales_long_edge_and_keeps_ratio() {
        let shrunk =
            shrink_cover(&png_cover_bytes(640, 400), COVER_MAX_EDGE).expect("PNG 应该解得开");
        let decoded = image::load_from_memory(&shrunk).expect("输出必须还能解");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (160, 100),
            "长边缩到 160，等比"
        );
    }

    /// 小图不许被放大：长边不足目标时保持原尺寸。
    #[test]
    fn shrink_cover_never_upscales() {
        let shrunk = shrink_cover(&png_cover_bytes(100, 37), COVER_ORIGINAL_MAX_EDGE)
            .expect("PNG 应该解得开");
        let decoded = image::load_from_memory(&shrunk).expect("输出必须还能解");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (100, 37),
            "小图原样保留"
        );
    }

    /// 输出是可解码的 JPEG（按内容嗅探），大图长边缩到 1280。
    #[test]
    fn shrink_cover_writes_decodable_jpeg_at_original_edge() {
        let shrunk = shrink_cover(&png_cover_bytes(2000, 1500), COVER_ORIGINAL_MAX_EDGE)
            .expect("PNG 应该解得开");
        assert_eq!(
            image::guess_format(&shrunk).unwrap(),
            image::ImageFormat::Jpeg,
            "落盘内容必须是 JPEG"
        );
        let decoded = image::load_from_memory(&shrunk).expect("JPEG 必须能再解");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (1280, 960),
            "长边缩到 1280，等比"
        );
    }

    /// 边界内的合法大封面必须能解码成功——防止以后调小边长或加回
    /// 别的上限，把这类图重新挡成永久占位图。
    /// 源图边长写死 4096，故意不跟 `MAX_DECODE_EDGE` 走：常量调小后
    /// 这条测试必须红，才算盯住了。
    #[test]
    fn shrink_cover_accepts_large_covers_within_limit() {
        let img = image::RgbImage::from_pixel(4096, 4096, image::Rgb([8, 16, 32]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, image::ImageFormat::Png)
            .expect("测试 PNG 必须编得出来");
        let shrunk = shrink_cover(&bytes.into_inner(), COVER_ORIGINAL_MAX_EDGE)
            .expect("边界内的大图必须能缩");
        let decoded = image::load_from_memory(&shrunk).expect("输出必须还能解");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (COVER_ORIGINAL_MAX_EDGE, COVER_ORIGINAL_MAX_EDGE),
            "4096² 源图要缩到原图目标边长"
        );
    }

    /// 解不开的源图返回 `None`，调用方据此不落盘、界面显示占位图。
    #[test]
    fn shrink_cover_rejects_undecodable_bytes() {
        assert!(shrink_cover(b"not-an-image", COVER_MAX_EDGE).is_none());
    }

    /// 超出解码边长上限的源图当解码失败处理。像素量刻意很小
    /// （一条 1px 的行），编出来只有几 KB，跑得快。
    #[test]
    fn shrink_cover_rejects_images_over_decode_limit() {
        let mut img = image::RgbImage::new(MAX_DECODE_EDGE + 1, 1);
        img.put_pixel(0, 0, image::Rgb([1, 2, 3]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, image::ImageFormat::Png)
            .expect("测试 PNG 必须编得出来");
        assert!(
            shrink_cover(&bytes.into_inner(), COVER_MAX_EDGE).is_none(),
            "宽超上限的图要走解码失败路径"
        );
    }

    /// 带 alpha 的源图落 PNG 保透明（头像走这个缓存，转 JPEG 会变黑底）。
    #[test]
    fn shrink_cover_preserves_alpha_as_png() {
        let mut img = image::RgbaImage::new(80, 80);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgba([10, 20, 30, 128]);
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, image::ImageFormat::Png)
            .expect("测试 PNG 必须编得出来");
        let shrunk = shrink_cover(&bytes.into_inner(), COVER_MAX_EDGE).expect("PNG 应该解得开");
        assert_eq!(
            image::guess_format(&shrunk).unwrap(),
            image::ImageFormat::Png,
            "带透明的图要落 PNG"
        );
        let decoded = image::load_from_memory(&shrunk).expect("PNG 必须能再解");
        // 80×80 ≤ 160：原样保留，不放大。
        assert_eq!((decoded.width(), decoded.height()), (80, 80));
        // PNG 无损：alpha 通道和值都原样保留。
        assert_eq!(decoded.color(), image::ColorType::Rgba8, "透明通道要保留");
        // DynamicImage 没有 get_pixel：经 to_rgba8 落到 ImageBuffer 再取。
        let rgba = decoded.to_rgba8();
        assert_eq!(
            rgba.get_pixel(0, 0),
            &image::Rgba([10, 20, 30, 128]),
            "半透明像素原样保留"
        );
    }

    /// 全不透明的 RGBA 源图（导出工具默认带 alpha 通道）照旧走 JPEG——
    /// 别让「带通道就落 PNG」把无损 PNG 的体积又吐回去。
    #[test]
    fn shrink_cover_keeps_jpeg_for_opaque_rgba_source() {
        let img = image::RgbaImage::from_pixel(320, 200, image::Rgba([8, 16, 32, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, image::ImageFormat::Png)
            .expect("测试 PNG 必须编得出来");
        let shrunk = shrink_cover(&bytes.into_inner(), COVER_MAX_EDGE).expect("PNG 应该解得开");
        assert_eq!(
            image::guess_format(&shrunk).unwrap(),
            image::ImageFormat::Jpeg,
            "全不透明的源图要照旧走 JPEG"
        );
        let decoded = image::load_from_memory(&shrunk).expect("JPEG 必须能再解");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (160, 100),
            "长边缩到 160，等比"
        );
    }

    /// 命中磁盘缓存把修改时间推到当前，作为 LRU 的兜底键。
    #[test]
    fn touching_cover_refreshes_modified_time() {
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0000abcd-160.img");
        std::fs::write(&path, b"cover").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();

        touch_cover_file(&path);

        let refreshed = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(refreshed > old, "命中后修改时间要往前推");
        // 路径已经不存在时也不能 panic（命中与淘汰并发时的竞态）。
        std::fs::remove_file(&path).unwrap();
        touch_cover_file(&path);
    }

    /// 写不进去就返回 false，目标位置上的东西一个都不许动，自己的临时文件也要清干净。
    #[test]
    fn cover_write_failure_reports_unsaved_and_leaves_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0000abcd-160.img");
        // 用目录占住目标路径，换文件这一步必然失败
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), b"old-cover").unwrap();

        assert!(!save_cover_file(&path, b"new-cover"), "换不上就不能报成功");
        assert_eq!(std::fs::read(path.join("keep")).unwrap(), b"old-cover");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "除目标本身以外不许留东西（临时文件要清掉）"
        );
    }

    /// 写盘失败但目标这会儿已经能用时，返回 true 直接用它。
    ///
    /// 目标用一条 253 字节的文件名来造：临时文件会在它后面再拼 `.<进程号>-<序号>.part`，
    /// 超过文件名 255 字节的上限，`fs::write` 这一步直接报错。
    /// 只在 Unix 上跑：Windows 的临时目录路径加上 253 字节的名字会顶到路径长度上限，
    /// 前置条件造不出来。
    #[cfg(unix)]
    #[test]
    fn cover_write_failure_reuses_usable_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{}.img", "a".repeat(249)));
        std::fs::write(&path, b"old-cover").expect("造不出 253 字节文件名的目标");

        assert!(
            save_cover_file(&path, b"new-cover"),
            "目标还能用就该把它交回去"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"old-cover",
            "没写成，旧封面不能被改"
        );
    }

    /// 试听片段不算整曲缓存：显式点播的不可播拦截只给整曲让路（父票边界）。
    #[test]
    fn full_track_cached_excludes_preview_fragments() {
        let info = |preview| CachedInfo {
            path: std::path::PathBuf::new(),
            quality: String::new(),
            preview,
            quality_rank: None,
            checked_rank: None,
            unknown_probe: None,
            preview_retry_blocked: false,
        };
        assert!(Session::full_track_cached(Some(info(false))));
        assert!(!Session::full_track_cached(Some(info(true))));
        assert!(!Session::full_track_cached(None));
    }

    /// 重建会话不写音质配置：登录/换 Cookie 之后「自动」仍然是自动，
    /// 否则会员状态一变，档位就再也跟不上。
    #[test]
    fn rebuilding_session_leaves_quality_setting_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        Settings::default().save_to(&path).unwrap();

        // 走一遍真实顺序：读配置 → 建会话 → 登录写盘 → 按新设置重建会话
        let mut settings = Settings::load_from(&path);
        assert_eq!(settings.quality, crate::quality::AUTO);
        let mut session = Session::new(&settings);
        settings
            .apply_login_cookie_to(&path, "sessionid_ss=x")
            .unwrap();
        session.adopt(&settings);

        assert_eq!(
            session.inputs().quality,
            crate::quality::AUTO,
            "登录后不得被固化成具体档位"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(r#""quality": "auto""#),
            "配置文件里的自动是 auto 这一种写法：{text}"
        );
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
        // 杜比 / Hi-Res 拿到低于目标的整曲流，同样是确认性结论。
        assert_eq!(confirmed_miss_rank(Some(110), 100, false, true), Some(110));
        assert_eq!(confirmed_miss_rank(Some(88), 80, false, true), Some(88));
    }

    /// 升级路径的「问过目标」记账：档位未知的旧缓存拿回正好到目标的完整流也要记，
    /// 否则它不换文件就每轮重下；档位已知、试听、权益未确认都不走这条。
    #[test]
    fn legacy_probe_records_equal_target_only_for_unknown_rank() {
        use libresoda::soda::types::DownloadInfo;

        let lease = crate::cache::CacheLease::acquire(std::path::Path::new("p.m4a"));
        let cached = |rank: Option<i64>| CachedTrack {
            path: std::path::PathBuf::new(),
            quality: "较高 320k".into(),
            preview: false,
            quality_rank: rank,
            checked_rank: None,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease: lease.clone(),
        };
        let info = |quality: &str, bitrate: i64, preview: bool| DownloadInfo {
            quality: quality.into(),
            bitrate,
            is_preview: preview,
            ..Default::default()
        };

        let legacy = cached(None);
        // 正好到目标、或已经超过目标：记下目标，避免每轮重下。
        assert_eq!(
            legacy_probe_covers_target(&legacy, &info("highest", 320_000, false), Some(80), true),
            Some(80)
        );
        assert_eq!(
            legacy_probe_covers_target(&legacy, &info("highest", 320_000, false), Some(70), true),
            Some(70)
        );
        // 还不到目标：交给 `confirmed_miss_rank`，这条不重复记。
        assert_eq!(
            legacy_probe_covers_target(&legacy, &info("highest", 320_000, false), Some(100), true),
            None
        );
        // 档位已知的缓存不靠这条（它有自己的「已探测」记录）。
        assert_eq!(
            legacy_probe_covers_target(
                &cached(Some(80)),
                &info("highest", 320_000, false),
                Some(80),
                true
            ),
            None
        );
        // 试听、权益未确认、目标未知都不记。
        assert_eq!(
            legacy_probe_covers_target(&legacy, &info("highest", 320_000, true), Some(80), true),
            None
        );
        assert_eq!(
            legacy_probe_covers_target(&legacy, &info("highest", 320_000, false), Some(80), false),
            None
        );
        assert_eq!(
            legacy_probe_covers_target(&legacy, &info("highest", 320_000, false), None, true),
            None
        );
    }

    /// 写出与解析必须用同一套字段布局，避免两边各改一处后错位。
    #[test]
    fn encode_sidecar_round_trips_through_parser() {
        let plain = encode_sidecar("极高 320k", 2000, false, 80, None);
        assert_eq!(plain, "极高 320k\t2000\t0\t80");
        let plain_checked = crate::cache::parse_sidecar(&plain, 2000).unwrap();
        assert_eq!(plain_checked.checked_rank, None);
        assert_eq!(plain_checked.preview_attempt_at, None);

        let checked = encode_sidecar("极高 320k", 2000, false, 80, Some(100));
        let parsed = crate::cache::parse_sidecar(&checked, 2000).unwrap();
        assert_eq!(parsed.actual_rank, Some(80));
        assert_eq!(parsed.checked_rank, Some(100));
        assert!(parsed.checked_at.is_some());

        // 试听只写第 3 字段的标记；第 7 字段（补整曲退避时间）由 `record_preview_attempt` 维护。
        let preview = encode_sidecar("未知 128k", 2000, true, 0, None);
        assert_eq!(preview, "未知 128k\t2000\t1\t0");
        let parsed = crate::cache::parse_sidecar(&preview, 2000).unwrap();
        assert!(parsed.preview);
        assert_eq!(parsed.actual_rank, Some(0));
        assert_eq!(parsed.preview_attempt_at, None);
    }

    /// 一条档位已知、没有额外标记的整曲缓存，供命中/升级判定用。
    fn known_rank_track(rank: i64, checked: Option<i64>) -> CachedTrack {
        let lease = crate::cache::CacheLease::acquire(std::path::Path::new("known-rank.m4a"));
        CachedTrack {
            path: std::path::PathBuf::new(),
            quality: "test".into(),
            preview: false,
            quality_rank: Some(rank),
            checked_rank: checked,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease,
        }
    }

    /// 命中缓存时，确认过的目标不再触发升级；目标提高后仍要重新探测。
    #[test]
    fn needs_upgrade_honours_confirmed_target() {
        let lossless = crate::quality::QualityIntent::parse("lossless");
        assert!(known_rank_track(80, None).needs_upgrade(&lossless, None, None));
        assert!(!known_rank_track(80, Some(100)).needs_upgrade(&lossless, None, None));
        assert!(known_rank_track(80, Some(70)).needs_upgrade(&lossless, None, None));
        assert!(!known_rank_track(100, None).needs_upgrade(&lossless, None, None));
    }

    /// 杜比 / Hi-Res 的探测结论同样要挡住下一次升级，否则每轮都整首重下一遍。
    #[test]
    fn high_tier_check_suppresses_the_next_upgrade() {
        let hires = crate::quality::QualityIntent::parse("hires");
        let dolby = crate::quality::QualityIntent::parse("dolby");

        assert!(known_rank_track(100, None).needs_upgrade(&hires, Some(true), None));
        assert!(!known_rank_track(100, Some(110)).needs_upgrade(&hires, Some(true), None));
        assert!(!known_rank_track(100, Some(110)).needs_auto_upgrade(&hires, Some(true), None));
        // 杜比目标没升到 88 档、但已经问过：同一目标不再重下。
        assert!(known_rank_track(80, None).needs_upgrade(&dolby, Some(true), None));
        assert!(!known_rank_track(80, Some(88)).needs_upgrade(&dolby, Some(true), None));
    }

    /// #89：档位未知的旧三字段缓存也要能升级；自动路径再多一道「结论记不住就别探」的闸。
    #[test]
    fn unknown_rank_cache_upgrades_only_when_the_answer_can_be_remembered() {
        let lease = crate::cache::CacheLease::acquire(std::path::Path::new("legacy.m4a"));
        let legacy = |quality: &str, checked: Option<i64>| CachedTrack {
            path: std::path::PathBuf::new(),
            quality: quality.into(),
            preview: false,
            quality_rank: None,
            checked_rank: checked,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease: lease.clone(),
        };
        let lossless = crate::quality::QualityIntent::parse("lossless");
        let hires = crate::quality::QualityIntent::parse("hires");

        // 未知档位且标签证明不了达标 → 重下（旧行为是恒不升级，存量缓存全废）
        assert!(legacy("极高 320k", None).needs_upgrade(&lossless, Some(true), None));
        // 旧标签写明无损 → 目标不超过无损时不值得再花一遍流量
        assert!(!legacy("无损 871k", None).needs_upgrade(&lossless, Some(true), None));
        // 已确认过「无损目标没有更高流」 → 同一目标不再请求
        assert!(!legacy("极高 320k", Some(100)).needs_upgrade(&lossless, Some(true), None));
        // 目标换成 Hi-Res：那次结论不代表更高档位，仍要探
        assert!(legacy("无损 871k", Some(100)).needs_upgrade(&hires, Some(true), None));

        // 自动路径：非会员要无损，负结论写不进 sidecar（买会员后必须重探），
        // 于是每次播放都会整曲重下一遍——这种探测不自动发起。
        assert!(!legacy("极高 320k", None).needs_auto_upgrade(&lossless, Some(false), None));
        // 权益确认为会员 → 结论记得住，可以自动升
        assert!(legacy("极高 320k", None).needs_auto_upgrade(&lossless, Some(true), None));
        // 不依赖权益的目标（较高 70）同样记得住结论
        let medium = crate::quality::QualityIntent::parse("medium");
        assert!(legacy("流畅 128k", None).needs_auto_upgrade(&medium, Some(false), None));
        // 档位已知的记录不受这道闸影响，保持 #78 的原行为
        let measured = CachedTrack {
            quality_rank: Some(50),
            ..legacy("流畅 128k", None)
        };
        assert!(measured.needs_auto_upgrade(&lossless, Some(false), None));
    }

    /// #96：记了 0 分（认不出档位）的缓存两条升级路都不走。重下拿回的还是同一条认不出的流，
    /// 而 0 分写不进「已确认无更高档」（见 `confirmed_miss_rank`），去重挡不住，
    /// 放行就等于每次命中都整曲下一遍再丢掉。没记分数的旧缓存不受影响，仍按 #89 去量一次。
    #[test]
    fn unreadable_zero_rank_never_upgrades() {
        let lease = crate::cache::CacheLease::acquire(std::path::Path::new("unreadable.m4a"));
        let cached = |rank: Option<i64>, quality: &str| CachedTrack {
            path: std::path::PathBuf::new(),
            quality: quality.into(),
            preview: false,
            quality_rank: rank,
            checked_rank: None,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease: lease.clone(),
        };
        let lossless = crate::quality::QualityIntent::parse("lossless");
        let auto = crate::quality::QualityIntent::Auto;
        let unreadable = cached(Some(0), "未知 128k");
        assert!(!unreadable.needs_upgrade(&lossless, Some(true), None));
        assert!(!unreadable.needs_upgrade(&auto, Some(true), None));
        assert!(!unreadable.needs_auto_upgrade(&lossless, Some(true), None));
        // 权益未确认时也不探：本来 `needs_auto_upgrade` 会因「结论记不住」放行已知档位的记录，
        // 0 分必须在那道闸之前就挡住。
        assert!(!unreadable.needs_auto_upgrade(&lossless, Some(false), None));
        assert_eq!(
            skipped_upgrade_reason(&unreadable, Some(100), None),
            NotImprovedReason::UnknownRank
        );
        assert!(cached(None, "极高 320k").needs_upgrade(&lossless, Some(true), None));
    }

    /// #130：试听补整曲有自己的出口，不被 `actual_rank`（尤其 0 分）连坐；
    /// 退避期内不再重下，退避过后仍会再试。
    #[test]
    fn preview_cache_exits_regardless_of_unreadable_rank() {
        let lease = crate::cache::CacheLease::acquire(std::path::Path::new("preview.m4a"));
        let preview = |rank: Option<i64>, blocked: bool| CachedTrack {
            path: std::path::PathBuf::new(),
            quality: "未知 128k".into(),
            preview: true,
            quality_rank: rank,
            checked_rank: None,
            unknown_probe: None,
            preview_retry_blocked: blocked,
            lease: lease.clone(),
        };
        let lossless = crate::quality::QualityIntent::parse("lossless");
        let medium = crate::quality::QualityIntent::parse("medium");

        // 认不出档位的 0 分试听：权益确认后播放命中要自动试一次。
        let unreadable = preview(Some(0), false);
        assert!(unreadable.needs_upgrade(&lossless, Some(true), None));
        assert!(unreadable.needs_auto_upgrade(&lossless, Some(true), None));

        // 认得出档位、且已达所选（50 对 50）的试听同样要补整曲——目标是整曲，不是更高档位。
        assert!(preview(Some(50), false).needs_upgrade(&medium, None, None));
        assert!(preview(Some(50), false).needs_auto_upgrade(&medium, None, None));

        // 上次尝试还在退避期：这次命中不再重下，等退避过了再试。
        let backed_off = preview(Some(0), true);
        assert!(!backed_off.needs_auto_upgrade(&lossless, Some(true), None));
        assert!(!backed_off.needs_upgrade(&lossless, Some(true), None));

        // 需要会员的档位在确认非会员时不自动重下，避免拿回另一条试听。
        assert!(!preview(Some(0), false).needs_auto_upgrade(&lossless, Some(false), None));
        // 不需要会员的档位不受这道闸影响。
        assert!(preview(Some(0), false).needs_auto_upgrade(&medium, Some(false), None));
    }

    /// #130：试听流绝不覆盖已有整曲，哪怕它的档位分数更高。
    #[test]
    fn preview_stream_never_replaces_a_full_track() {
        use libresoda::soda::types::DownloadInfo;

        let lease = crate::cache::CacheLease::acquire(std::path::Path::new("guard.m4a"));
        let cached = |preview: bool| CachedTrack {
            path: std::path::PathBuf::new(),
            quality: "流畅 128k".into(),
            preview,
            quality_rank: Some(50),
            checked_rank: None,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease: lease.clone(),
        };
        let preview_stream = DownloadInfo {
            is_preview: true,
            ..Default::default()
        };
        let full_stream = DownloadInfo {
            is_preview: false,
            ..Default::default()
        };

        assert!(would_downgrade_to_preview(&preview_stream, &cached(false)));
        assert!(!would_downgrade_to_preview(&preview_stream, &cached(true)));
        assert!(!would_downgrade_to_preview(&full_stream, &cached(false)));
        assert!(!would_downgrade_to_preview(&full_stream, &cached(true)));
    }

    /// 新流值不值得换掉本地这份缓存。
    ///
    /// 档位未知的旧文件既不能被更差的流顶掉，也不能被同码率的流顶掉——后者一旦被换掉，
    /// 事后就会把「量出一个正分」当成升了一档。只有证据充分（严格更高码率、或升到无损、
    /// 或试听换整曲）才算更好。
    #[test]
    fn is_better_stream_never_trades_a_legacy_file_for_worse_or_equal_audio() {
        use libresoda::soda::types::DownloadInfo;

        let lease = crate::cache::CacheLease::acquire(std::path::Path::new("s.m4a"));
        let cached = |quality: &str, rank: Option<i64>, preview: bool| CachedTrack {
            path: std::path::PathBuf::new(),
            quality: quality.into(),
            preview,
            quality_rank: rank,
            checked_rank: None,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease: lease.clone(),
        };
        let info = |quality: &str, format: &str, bitrate: i64, preview: bool| DownloadInfo {
            quality: quality.into(),
            format: format.into(),
            bitrate,
            is_preview: preview,
            ..Default::default()
        };

        // 档位已知：只认档位提升。
        let known = cached("极高 320k", Some(80), false);
        assert!(!is_better_stream(
            &info("highest", "m4a", 320_000, false),
            &known
        ));
        assert!(is_better_stream(
            &info("lossless", "flac", 871_000, false),
            &known
        ));
        // 试听片段换成整曲永远是提升，哪怕档位与码率都没变。
        assert!(is_better_stream(
            &info("standard", "m4a", 128_000, false),
            &cached("流畅 128k", Some(50), true)
        ));
        // 试听换更高分的试听不算提升：盘上仍是片段。
        assert!(!is_better_stream(
            &info("lossless", "flac", 871_000, true),
            &cached("流畅 128k", Some(50), true)
        ));

        // 档位未知、标签也证不出下限：只剩标签里的码率可比。
        let legacy = cached("极高 320k", None, false);
        // 新流更差（码率更低）不换。
        assert!(!is_better_stream(
            &info("standard", "m4a", 128_000, false),
            &legacy
        ));
        // 同档同码率不换：服务端回一条 320k 的「极高」时旧文件不该被顶掉，
        // 更不能在事后把「从没记分到量出 80 分」当成升了一档。
        assert!(!is_better_stream(
            &info("highest", "m4a", 320_000, false),
            &legacy
        ));
        assert!(!is_better_stream(
            &info("higher", "m4a", 320_000, false),
            &legacy
        ));
        // 码率严格更高才换。
        assert!(is_better_stream(
            &info("highest", "m4a", 384_000, false),
            &legacy
        ));
        // 升到无损照样算提升：无损 FLAC 的码率本来就可能低于有损高码率流。
        assert!(is_better_stream(
            &info("lossless", "flac", 300_000, false),
            &legacy
        ));
        // 新流档位认不出来（0 档）不换：旧文件至少还有一个能看的标签。
        assert!(!is_better_stream(&info("", "", 0, false), &legacy));

        let legacy_without_bitrate = cached("极高", None, false);
        assert!(!is_better_stream(
            &info("standard", "m4a", 128_000, false),
            &legacy_without_bitrate
        ));
        assert!(is_better_stream(
            &info("lossless", "flac", 871_000, false),
            &legacy_without_bitrate
        ));

        // 旧标签是无损 → 按下限比：无损换无损不算，只有 Hi-Res 才值得换。
        let lossless_legacy = cached("无损 871k", None, false);
        assert!(!is_better_stream(
            &info("lossless", "flac", 871_000, false),
            &lossless_legacy
        ));
        assert!(is_better_stream(
            &info("hires", "flac", 1_200_000, false),
            &lossless_legacy
        ));
    }

    #[test]
    fn unknown_rank_probe_records_backoff_without_forging_confirmed_miss() {
        use libresoda::soda::types::DownloadInfo;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song-auto.m4a");
        let quality_path = dir.path().join("song-auto.quality");
        std::fs::write(&path, vec![0u8; 2000]).unwrap();
        std::fs::write(&quality_path, "极高 320k\t2000\t0").unwrap();
        let mut current = CachedTrack {
            path: path.clone(),
            quality: "极高 320k".into(),
            preview: false,
            quality_rank: None,
            checked_rank: None,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease: crate::cache::CacheLease::acquire(&path),
        };
        let unknown_stream = DownloadInfo {
            quality: String::new(),
            format: String::new(),
            bitrate: 0,
            is_preview: false,
            ..Default::default()
        };

        let scope = crate::cache::unknown_probe_scope(Some("user-1"), Some(true)).unwrap();
        let probe = probe_check(&current, &unknown_stream, Some(100), true);
        let outcome = finish_unimproved_upgrade(
            &mut current,
            &unknown_stream,
            Some(100),
            true,
            probe,
            Some(&scope),
            &quality_path,
        );

        let lossless = crate::quality::QualityIntent::parse("lossless");
        assert_eq!(outcome, NotImprovedReason::UnknownRank);
        assert!(
            !current.needs_upgrade(&lossless, Some(true), Some(&scope)),
            "探测过但档位未知的旧缓存不能每轮再整曲重下"
        );
        let text = std::fs::read_to_string(&quality_path).unwrap();
        assert!(!text.contains("user-1"), "sidecar 不落原始账号 id");
        let fields: Vec<&str> = text.split('\t').collect();
        assert_eq!(fields.len(), 10, "未知探测要单独落 target/time/scope");
        assert_eq!(fields[3], "", "旧文件仍然没有实际档位");
        assert_eq!(fields[4], "", "未知探测不能冒充确认性 miss");
        assert_eq!(fields[7], "100");
        let recorded_at = fields[8].parse::<u64>().unwrap();
        assert!(recorded_at > 0);
        assert_eq!(fields[9], scope);

        let parsed = crate::cache::parse_sidecar(&text, 2000).unwrap();
        assert_eq!(parsed.actual_rank, None);
        assert_eq!(parsed.checked_rank, None);
        let checked_rank = parsed.fresh_checked_rank(recorded_at);
        let unknown_probe = parsed.fresh_unknown_probe(recorded_at);
        let quality = parsed.quality;
        let preview = parsed.preview;
        let quality_rank = parsed.actual_rank;
        let reloaded = CachedTrack {
            path,
            quality,
            preview,
            quality_rank,
            checked_rank,
            unknown_probe,
            preview_retry_blocked: false,
            lease: crate::cache::CacheLease::acquire(std::path::Path::new("reloaded.m4a")),
        };
        assert!(!reloaded.needs_upgrade(&lossless, Some(true), Some(&scope)));
        assert!(!reloaded.needs_auto_upgrade(&lossless, Some(true), Some(&scope)));
        assert!(
            reloaded.needs_upgrade(&lossless, Some(true), None),
            "范围缺失时放行"
        );
        let other_account = crate::cache::unknown_probe_scope(Some("user-2"), Some(true)).unwrap();
        let non_vip = crate::cache::unknown_probe_scope(Some("user-1"), Some(false)).unwrap();
        assert!(reloaded.needs_upgrade(&lossless, Some(true), Some(&other_account)));
        assert!(reloaded.needs_upgrade(&lossless, Some(true), Some(&non_vip)));
        let hires = crate::quality::QualityIntent::parse("hires");
        assert!(
            reloaded.needs_upgrade(&hires, Some(true), Some(&scope)),
            "目标变化后重试"
        );
    }

    #[test]
    fn unknown_rank_probe_backs_off_known_low_tier_without_replacing_it() {
        use libresoda::soda::types::DownloadInfo;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known-auto.m4a");
        let quality_path = dir.path().join("known-auto.quality");
        std::fs::write(&path, b"original-audio").unwrap();
        std::fs::write(&quality_path, "极高 320k\t14\t0\t80").unwrap();
        let mut current = CachedTrack {
            path: path.clone(),
            quality: "极高 320k".into(),
            preview: false,
            quality_rank: Some(80),
            checked_rank: None,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease: crate::cache::CacheLease::acquire(&path),
        };
        let unknown_stream = DownloadInfo {
            is_preview: false,
            ..Default::default()
        };
        let scope = crate::cache::unknown_probe_scope(Some("user-1"), Some(true)).unwrap();

        let probe = probe_check(&current, &unknown_stream, Some(100), true);
        let outcome = finish_unimproved_upgrade(
            &mut current,
            &unknown_stream,
            Some(100),
            true,
            probe,
            Some(&scope),
            &quality_path,
        );

        assert_eq!(outcome, NotImprovedReason::UnknownRank);
        assert_eq!(std::fs::read(&path).unwrap(), b"original-audio");
        let parsed =
            crate::cache::parse_sidecar(&std::fs::read_to_string(&quality_path).unwrap(), 14)
                .unwrap();
        assert_eq!(parsed.actual_rank, Some(80));
        assert_eq!(parsed.checked_rank, None);
        assert!(parsed
            .fresh_unknown_probe(parsed.unknown_probe_at.unwrap())
            .is_some());
        let lossless = crate::quality::QualityIntent::parse("lossless");
        assert!(!current.needs_upgrade(&lossless, Some(true), Some(&scope)));
    }

    #[test]
    fn not_improved_reason_separates_confirmed_miss_from_not_better() {
        let lease = crate::cache::CacheLease::acquire(std::path::Path::new("reason.m4a"));
        let cached = |quality: &str, rank: Option<i64>, preview: bool| CachedTrack {
            path: std::path::PathBuf::new(),
            quality: quality.into(),
            preview,
            quality_rank: rank,
            checked_rank: None,
            unknown_probe: None,
            preview_retry_blocked: false,
            lease: lease.clone(),
        };
        let full = cached("极高 320k", Some(80), false);
        let legacy = cached("极高 320k", None, false);
        assert_eq!(
            downloaded_not_improved_reason(&legacy, true),
            NotImprovedReason::NotBetter
        );
        let scored_without_bitrate = cached("极高", Some(80), false);
        assert_eq!(
            downloaded_not_improved_reason(&scored_without_bitrate, true),
            NotImprovedReason::NotBetter
        );
        let unknown_label = cached("极高", None, false);
        assert_eq!(
            downloaded_not_improved_reason(&unknown_label, true),
            NotImprovedReason::NoHigherTier
        );
        assert_eq!(
            downloaded_not_improved_reason(&unknown_label, false),
            NotImprovedReason::NotBetter
        );

        assert_eq!(
            skipped_upgrade_reason(&cached("极高 320k", Some(80), false), Some(80), None),
            NotImprovedReason::AlreadyChecked
        );
        let checked = CachedTrack {
            checked_rank: Some(100),
            ..cached("极高 320k", Some(80), false)
        };
        assert_eq!(
            skipped_upgrade_reason(&checked, Some(100), None),
            NotImprovedReason::AlreadyChecked
        );
        assert_eq!(
            skipped_upgrade_reason(&full, None, None),
            NotImprovedReason::NotBetter
        );
        assert_eq!(
            skipped_upgrade_reason(&cached("未知 128k", Some(0), false), Some(100), None),
            NotImprovedReason::UnknownRank
        );
        let backed_off_preview = CachedTrack {
            preview_retry_blocked: true,
            ..cached("未知 128k", Some(0), true)
        };
        assert_eq!(
            skipped_upgrade_reason(&backed_off_preview, Some(100), None),
            NotImprovedReason::PreviewBackoff
        );
    }

    #[test]
    fn upgrade_cached_track_fails_fast_when_not_cached() {
        let session = Session::new(Settings::default());
        let track = crate::TrackItem {
            id: "non-existent-track-999".into(),
            ..Default::default()
        };
        let err = session
            .upgrade_cached_track(&track, None, None, None)
            .unwrap_err();
        assert_eq!(err.stage, crate::failure::Stage::Input);
        assert_eq!(err.retryability, crate::failure::Retryability::Never);
    }
}
