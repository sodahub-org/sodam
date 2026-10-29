//! 磁盘缓存维护。所有扫描/删除由后台调用；租约保护下载、待装载和正在播放的音频。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

pub const DEFAULT_AUDIO_LIMIT_MB: u64 = 10 * 1024;
const PART_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

type Leases = HashMap<PathBuf, std::sync::Weak<PathBuf>>;
fn leases() -> &'static Mutex<Leases> {
    static LEASES: OnceLock<Mutex<Leases>> = OnceLock::new();
    LEASES.get_or_init(Mutex::default)
}

// 释放租约仅减 Arc 引用计数，绝不在 UI 丢弃过期加载结果时等待维护线程的文件 IO。
#[derive(Debug, Clone, PartialEq)]
pub struct CacheLease(pub(crate) Arc<PathBuf>);

impl CacheLease {
    pub fn acquire(path: &Path) -> Self {
        let mut active = leases().lock().unwrap_or_else(|err| err.into_inner());
        active.retain(|_, lease| lease.strong_count() > 0);
        if let Some(lease) = active.get(path).and_then(|lease| lease.upgrade()) {
            return Self(lease);
        }
        let lease = Arc::new(path.to_owned());
        active.insert(path.to_owned(), Arc::downgrade(&lease));
        Self(lease)
    }
}

/// 同一缓存目标的写者串行化，避免预取与手动播放互相覆盖文件/sidecar。
pub(crate) fn download_lock(path: &Path) -> Arc<Mutex<()>> {
    type Locks = HashMap<PathBuf, std::sync::Weak<Mutex<()>>>;
    static LOCKS: OnceLock<Mutex<Locks>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(|lock| lock.upgrade()) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_owned(), Arc::downgrade(&lock));
    lock
}

/// 解码失败时成对丢弃坏缓存；不能与另一个下载写者交错删除新文件/sidecar。
/// 音频线程不等待网络下载持有的锁，忙时留待下次失败重试。
pub(crate) fn invalidate_audio(path: &Path) {
    let lock = download_lock(path);
    let Ok(_writer) = lock.try_lock() else {
        return;
    };
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("quality"));
}

pub fn root_dir() -> PathBuf {
    std::env::var_os("SODAM_CACHE_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("sodam")
        })
}

// .quality 与音频共享保护键，避免播放/下载时只删了 sidecar。
fn protection_key(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|ext| ext == "quality") {
        path.with_extension("m4a")
    } else {
        path.to_owned()
    }
}

fn remove_idle(path: &Path) -> bool {
    let active = leases().lock().unwrap_or_else(|err| err.into_inner());
    if active
        .get(&protection_key(path))
        .is_some_and(|lease| lease.strong_count() > 0)
    {
        return false;
    }
    // 检查和 unlink 在同一临界区，避免租约在二者之间建立。
    std::fs::remove_file(path).is_ok()
}

fn files_in(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // 不沿符号链接走到缓存目录外。
        if kind.is_dir() {
            files_in(&entry.path(), files);
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
}

fn is_fresh_part(path: &Path, modified: SystemTime) -> bool {
    path.extension().is_some_and(|ext| ext == "part")
        && modified.elapsed().unwrap_or_default() < PART_MAX_AGE
}

/// 递归清理缓存，保留正在使用的音频和近期临时文件。返回成功删除的文件数。
pub fn clear_in(root: &Path) -> usize {
    let mut files = Vec::new();
    files_in(root, &mut files);
    files
        .into_iter()
        .filter(|path| {
            let modified = std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::now());
            !is_fresh_part(path, modified) && remove_idle(path)
        })
        .count()
}

/// 按最后使用时间淘汰音频（命中缓存时更新 mtime）。租约文件可暂时超额。
/// sidecar 与音频成组淘汰；失败的删除不计入释放空间。调用方必须在后台执行。
pub fn trim_audio_in(dir: &Path, limit_bytes: u64) -> u64 {
    let mut files = Vec::new();
    files_in(dir, &mut files);
    let mut groups: HashMap<PathBuf, (SystemTime, Vec<(PathBuf, u64)>)> = HashMap::new();
    let mut total = 0u64;
    for path in files {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let bytes = meta.len();
        total = total.saturating_add(bytes);
        let modified = meta.modified().unwrap_or(SystemTime::now());
        if is_fresh_part(&path, modified) {
            continue;
        }
        let key = protection_key(&path);
        let group = groups.entry(key).or_insert((modified, Vec::new()));
        group.0 = group.0.max(modified);
        group.1.push((path, bytes));
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|(_, (modified, _))| *modified);
    for (_, (_, files)) in groups {
        if total <= limit_bytes {
            break;
        }
        for (path, bytes) in files {
            if remove_idle(&path) {
                total = total.saturating_sub(bytes);
            }
        }
    }
    total
}

pub fn trim_audio(limit_mb: u64) -> u64 {
    trim_audio_in(
        &root_dir().join("audio"),
        limit_mb.saturating_mul(1024 * 1024),
    )
}

/// 解析音频缓存主文件名 stem，分离出 `(track_id, quality_tag)`。
/// 文件名格式形如 `{track_id}-{quality_tag}`（如 `7427204361343191057-auto`）。
pub fn parse_audio_cache_stem(stem: &str) -> Option<(String, String)> {
    let (track_id, tag) = stem.split_once('-')?;
    if track_id.is_empty() || tag.is_empty() {
        return None;
    }
    Some((track_id.to_string(), tag.to_string()))
}

/// 解析 sidecar `{档位}\t{字节}\t{0|1}\t{档位分数}`，与实际字节数核对后给出
/// （档位显示名，是否试听，实际档位分数）。
///
/// 尺寸不符或档位为空一律返回 `None`。
/// 第四个字段是实际档位分数，旧格式缺失时为 `None`。
/// 旧缓存把 50 分档写作「标准」，这里统一洗成当前显示名「流畅」，旧文件不必重下。
pub fn parse_sidecar(text: &str, actual: u64) -> Option<(String, bool, Option<i64>)> {
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
    let rank = fields
        .next()
        .and_then(|value| value.trim().parse::<i64>().ok());
    // 只改显示名，不动文件与分数：50 分档曾经叫「标准」，与 libresoda 的 standard(70) 撞名。
    let quality = quality.replace("标准", "流畅");
    Some((quality, preview, rank))
}

/// 本地缓存中的音频记录信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedAudioRecord {
    /// 曲目 ID
    pub track_id: String,
    /// 缓存文件名后缀（意图标签，如 auto / lossless / highest）
    pub quality_tag: String,
    /// 音频文件本地绝对路径
    pub path: PathBuf,
    /// 记录在 sidecar 的实际音质文本（如 "无损 871k" 或 "流畅 128k"）
    pub quality_label: String,
    /// 是否为试听片段
    pub preview: bool,
    /// 实际档位分数（旧缓存缺失时为 None）
    pub actual_rank: Option<i64>,
    /// 本地文件字节数
    pub file_size: u64,
}

impl CachedAudioRecord {
    /// 实际档位对应的显示名（如 "无损"、"极高"、"较高"、"流畅"；未知返回 None）。
    pub fn actual_tier(&self) -> Option<&'static str> {
        self.actual_rank.map(crate::quality::tier_label)
    }

    /// 当前实际档位是否低于目标档位分数。
    pub fn is_below_target(&self, target_rank: i64) -> bool {
        crate::quality::is_below_target(self.actual_rank, target_rank)
    }

    /// 当前记录是否允许批量升级至目标档位。
    ///
    /// 排除试听片段、已达到或超过目标的记录；同时受该缓存文件自身的 quality_tag 意图上限约束：
    /// 例如用户在省流模式下缓存的 `-low.m4a`（上限 50），不会被冲高为无损（100），
    /// 以免后续省流播放时命中数十兆的庞大文件破坏省流意图。
    pub fn is_upgradeable_to(&self, target_rank: i64) -> bool {
        let within_cap = crate::quality::QualityIntent::parse(&self.quality_tag)
            .max_rank()
            .is_none_or(|cap| target_rank <= cap);
        !self.preview && within_cap && self.is_below_target(target_rank)
    }
}

/// 读取单个音频缓存文件及其边车，构造记录。
pub fn read_cache_record(m4a_path: &Path) -> Option<CachedAudioRecord> {
    if m4a_path.extension().and_then(|ext| ext.to_str()) != Some("m4a") {
        return None;
    }
    let meta = std::fs::metadata(m4a_path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let stem = m4a_path.file_stem()?.to_str()?;
    let (track_id, quality_tag) = parse_audio_cache_stem(stem)?;
    let quality_path = m4a_path.with_extension("quality");
    let text = std::fs::read_to_string(&quality_path).ok()?;
    let (quality_label, preview, actual_rank) = parse_sidecar(&text, meta.len())?;
    Some(CachedAudioRecord {
        track_id,
        quality_tag,
        path: m4a_path.to_owned(),
        quality_label,
        preview,
        actual_rank,
        file_size: meta.len(),
    })
}

/// 扫描音频缓存目录，读取全部有效音频的缓存记录与实际档位。
pub fn list_cached_audio(audio_dir: &Path) -> Vec<CachedAudioRecord> {
    let Ok(entries) = std::fs::read_dir(audio_dir) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "m4a") {
            if let Some(record) = read_cache_record(&path) {
                records.push(record);
            }
        }
    }
    records
}

/// 批量升级预估容量与网络流量。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpgradeEstimate {
    /// 能够升级的曲目数
    pub upgradeable_count: usize,
    /// 可升级曲目当前所占字节总数
    pub current_bytes: u64,
    /// 升级后预计所占字节总数
    pub estimated_new_bytes: u64,
    /// 升级所需下载的网络流量（新流字节总数预估）
    pub estimated_traffic_bytes: u64,
}

impl UpgradeEstimate {
    /// 存储空间净变化（正数增加，负数减少）。
    pub fn net_disk_delta_bytes(&self) -> i64 {
        self.estimated_new_bytes as i64 - self.current_bytes as i64
    }
}

/// 估算目标档位单曲的平均字节数。
pub fn estimated_single_track_bytes(target_rank: i64) -> u64 {
    match crate::quality::tier_label(target_rank) {
        // 无损 FLAC 通常约 30-45 MB，取 35 MiB
        "无损" => 35 * 1024 * 1024,
        // 极高 320k 通常约 8-12 MB，取 10 MiB
        "极高" => 10 * 1024 * 1024,
        // 较高 通常约 5-8 MB，取 7 MiB
        "较高" => 7 * 1024 * 1024,
        // 流畅/低档 通常约 3-5 MB，取 4 MiB
        _ => 4 * 1024 * 1024,
    }
}

/// 评估将已有缓存批量升级到指定档位所需流量与容量变化。
pub fn estimate_upgrade(records: &[CachedAudioRecord], target_rank: i64) -> UpgradeEstimate {
    let mut count = 0usize;
    let mut current_bytes = 0u64;
    let mut estimated_new_bytes = 0u64;
    let single_est = estimated_single_track_bytes(target_rank);

    for record in records {
        // 试听片段不计入可升级整曲，且受 quality_tag 意图上限约束
        if record.is_upgradeable_to(target_rank) {
            count += 1;
            current_bytes = current_bytes.saturating_add(record.file_size);
            estimated_new_bytes = estimated_new_bytes.saturating_add(single_est);
        }
    }

    UpgradeEstimate {
        upgradeable_count: count,
        current_bytes,
        estimated_new_bytes,
        estimated_traffic_bytes: estimated_new_bytes,
    }
}

/// 将临时文件原子替换为正式缓存文件，并写入 sidecar。
/// 中途失败不影响现有缓存文件，保持旧文件完好（含回滚保护）。
pub fn atomic_replace_cache(
    part_path: &Path,
    target_path: &Path,
    sidecar_path: &Path,
    sidecar_content: &str,
) -> std::io::Result<()> {
    if !part_path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "part file does not exist",
        ));
    }
    // 1. 写临时 sidecar 文件，并清理可能遗留的旧临时/备份文件
    let tmp_sidecar = sidecar_path.with_extension("quality.tmp");
    let backup_path = target_path.with_extension("m4a.old");
    let backup_sidecar = sidecar_path.with_extension("quality.old");
    let _ = std::fs::remove_file(&tmp_sidecar);
    let _ = std::fs::remove_file(&backup_path);
    let _ = std::fs::remove_file(&backup_sidecar);

    std::fs::write(&tmp_sidecar, sidecar_content)?;

    // 2. 备份现有 sidecar 与音频文件（在 Windows 上若目标存在，rename 不会直接覆盖；
    // 且若中途失败需完整回滚，避免留下「音频已换但 sidecar 是旧的」导致校验失败）。
    let has_target = target_path.exists();
    let has_sidecar = sidecar_path.exists();

    if has_sidecar {
        let _ = std::fs::remove_file(&backup_sidecar);
        if let Err(err) = std::fs::rename(sidecar_path, &backup_sidecar) {
            let _ = std::fs::remove_file(&tmp_sidecar);
            return Err(err);
        }
    }

    if has_target {
        let _ = std::fs::remove_file(&backup_path);
        if let Err(err) = std::fs::rename(target_path, &backup_path) {
            if has_sidecar {
                let _ = std::fs::rename(&backup_sidecar, sidecar_path);
            }
            let _ = std::fs::remove_file(&tmp_sidecar);
            return Err(err);
        }
    }

    // 3. 移入新音频文件
    if let Err(err) = std::fs::rename(part_path, target_path) {
        if has_target {
            let _ = std::fs::rename(&backup_path, target_path);
        }
        if has_sidecar {
            let _ = std::fs::rename(&backup_sidecar, sidecar_path);
        }
        let _ = std::fs::remove_file(&tmp_sidecar);
        return Err(err);
    }

    // 4. 移入新 sidecar
    if let Err(err) = std::fs::rename(&tmp_sidecar, sidecar_path) {
        if has_target {
            let _ = std::fs::remove_file(target_path);
            let _ = std::fs::rename(&backup_path, target_path);
        }
        if has_sidecar {
            let _ = std::fs::rename(&backup_sidecar, sidecar_path);
        }
        let _ = std::fs::remove_file(&tmp_sidecar);
        return Err(err);
    }

    // 5. 替换成功：清理旧备份文件
    if has_target {
        let _ = std::fs::remove_file(&backup_path);
    }
    if has_sidecar {
        let _ = std::fs::remove_file(&backup_sidecar);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, age_seconds: u64) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, [0; 10]).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(age_seconds))
            .unwrap();
        path
    }

    #[test]
    fn clear_recurses_and_preserves_leases_and_fresh_parts() {
        let dir = tempfile::tempdir().unwrap();
        let audio = file(dir.path(), "audio/playing.m4a", 0);
        file(dir.path(), "audio/playing.quality", 0);
        let lease = CacheLease::acquire(&audio);
        file(dir.path(), "audio/download.part", 0);
        file(dir.path(), "audio/abandoned.part", 90_000);
        file(dir.path(), "covers/thumb.img", 0);
        file(dir.path(), "covers/original/full.img", 0);
        assert_eq!(clear_in(dir.path()), 3);
        assert!(audio.exists());
        assert!(audio.with_extension("quality").exists());
        assert!(dir.path().join("audio/download.part").exists());
        drop(lease);
        assert_eq!(clear_in(dir.path()), 2);
    }

    /// #60 的回归断言：清空缓存不能顺手删掉排障证据。
    #[test]
    fn clearing_cache_keeps_diagnostic_logs() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let log_dir = temp.path().join("logs");
        file(&cache, "audio/song.m4a", 0);
        file(&cache, "covers/cover.img", 0);
        let outlet = crate::log_outlet::LogOutlet::new(log_dir.clone());
        outlet.write_line("diagnostic entry");

        assert_eq!(clear_in(&cache), 2);
        assert!(log_dir.join(crate::log_outlet::LOG_FILE_NAME).exists());
    }

    #[test]
    fn trim_evicts_oldest_pairs_but_not_active_audio() {
        let dir = tempfile::tempdir().unwrap();
        let active = file(dir.path(), "active.m4a", 100);
        file(dir.path(), "active.quality", 100);
        let lease = CacheLease::acquire(&active);
        let old = file(dir.path(), "old.m4a", 50);
        file(dir.path(), "old.quality", 50);
        let recent = file(dir.path(), "recent.m4a", 0);
        file(dir.path(), "recent.quality", 0);
        assert_eq!(trim_audio_in(dir.path(), 40), 40);
        assert!(!old.exists());
        assert!(!old.with_extension("quality").exists());
        assert!(active.exists() && recent.exists());
        assert_eq!(trim_audio_in(dir.path(), 0), 20);
        drop(lease);
        assert_eq!(trim_audio_in(dir.path(), 0), 0);
    }

    #[test]
    fn cloned_and_independent_leases_keep_files_alive() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(dir.path(), "a.m4a", 0);
        let first = CacheLease::acquire(&path);
        let clone = first.clone();
        let second = CacheLease::acquire(&path);
        drop(first);
        drop(second);
        assert_eq!(clear_in(dir.path()), 0);
        drop(clone);
        assert_eq!(clear_in(dir.path()), 1);
    }

    #[test]
    fn invalidation_does_not_overlap_a_cache_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(dir.path(), "a.m4a", 0);
        let quality = file(dir.path(), "a.quality", 0);
        let lock = download_lock(&path);
        let writer = lock.lock().unwrap();
        invalidate_audio(&path);
        assert!(path.exists() && quality.exists());
        drop(writer);
        invalidate_audio(&path);
        assert!(!path.exists() && !quality.exists());
    }

    #[test]
    fn parse_stem_and_sidecar_and_record() {
        assert_eq!(
            parse_audio_cache_stem("7427204361343191057-auto"),
            Some(("7427204361343191057".into(), "auto".into()))
        );
        assert_eq!(
            parse_audio_cache_stem("track1-lossless"),
            Some(("track1".into(), "lossless".into()))
        );
        assert_eq!(parse_audio_cache_stem("invalid"), None);

        // 标准格式的 4 字段 sidecar
        let sidecar = "无损 871k\t1024\t0\t100";
        assert_eq!(
            parse_sidecar(sidecar, 1024),
            Some(("无损 871k".into(), false, Some(100)))
        );

        // 试听标志为1
        let preview_sidecar = "标准 128k\t500\t1\t50";
        assert_eq!(
            parse_sidecar(preview_sidecar, 500),
            Some(("流畅 128k".into(), true, Some(50))),
            "旧缓存里的「标准」是 50 分档，读取时洗成「流畅」"
        );

        // 尺寸不符返回None
        assert_eq!(parse_sidecar(sidecar, 999), None);

        // 旧3字段sidecar（缺档位分数）
        let legacy_sidecar = "极高 320k\t2000\t0";
        assert_eq!(
            parse_sidecar(legacy_sidecar, 2000),
            Some(("极高 320k".into(), false, None))
        );
    }

    #[test]
    fn list_cached_audio_and_upgrade_estimation() {
        let dir = tempfile::tempdir().unwrap();
        let song1_path = dir.path().join("song1-auto.m4a");
        std::fs::write(&song1_path, [0u8; 1000]).unwrap();
        std::fs::write(
            dir.path().join("song1-auto.quality"),
            "标准 128k\t1000\t0\t50",
        )
        .unwrap();

        let song2_path = dir.path().join("song2-auto.m4a");
        std::fs::write(&song2_path, [0u8; 5000]).unwrap();
        std::fs::write(
            dir.path().join("song2-auto.quality"),
            "无损 871k\t5000\t0\t100",
        )
        .unwrap();

        let records = list_cached_audio(dir.path());
        assert_eq!(records.len(), 2);

        let song1 = records.iter().find(|r| r.track_id == "song1").unwrap();
        assert_eq!(song1.actual_rank, Some(50));
        assert_eq!(song1.actual_tier(), Some("流畅"));
        assert!(song1.is_below_target(100));
        assert!(!song1.is_below_target(50));

        let song2 = records.iter().find(|r| r.track_id == "song2").unwrap();
        assert_eq!(song2.actual_rank, Some(100));
        assert_eq!(song2.actual_tier(), Some("无损"));
        assert!(!song2.is_below_target(100));

        // 目标为无损（100）：song1 可升级，song2 不需升级
        let est_lossless = estimate_upgrade(&records, 100);
        assert_eq!(est_lossless.upgradeable_count, 1);
        assert_eq!(est_lossless.current_bytes, 1000);
        assert_eq!(est_lossless.estimated_traffic_bytes, 35 * 1024 * 1024);
        assert_eq!(
            est_lossless.net_disk_delta_bytes(),
            (35 * 1024 * 1024 - 1000) as i64
        );

        // 目标为流畅（50）：两者都已达到，没有可升级
        let est_standard = estimate_upgrade(&records, 50);
        assert_eq!(est_standard.upgradeable_count, 0);
        assert_eq!(est_standard.current_bytes, 0);
    }

    #[test]
    fn atomic_replace_preserves_old_on_abort_and_updates_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("song-auto.m4a");
        let sidecar = dir.path().join("song-auto.quality");
        std::fs::write(&target, b"original-content").unwrap();
        std::fs::write(&sidecar, "标准 128k\t16\t0\t50").unwrap();

        // 失败情况：part 文件不存在，原有文件不受任何影响
        let fake_part = dir.path().join("fake.part");
        let fail_res = atomic_replace_cache(&fake_part, &target, &sidecar, "新边车");
        assert!(fail_res.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"original-content");
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            "标准 128k\t16\t0\t50"
        );

        // 成功情况：写入新 part 并原子替换
        let real_part = dir.path().join("song-auto.part");
        std::fs::write(&real_part, b"upgraded-content-flac").unwrap();
        let succ_res = atomic_replace_cache(&real_part, &target, &sidecar, "无损 871k\t21\t0\t100");
        assert!(succ_res.is_ok());
        assert_eq!(std::fs::read(&target).unwrap(), b"upgraded-content-flac");
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            "无损 871k\t21\t0\t100"
        );
        assert!(!real_part.exists());
    }

    /// 验收判据：正在升级的文件持有租约，不会被缓存淘汰或清理删掉。
    #[test]
    fn files_under_upgrade_protected_from_eviction_and_clearing() {
        let dir = tempfile::tempdir().unwrap();
        let target = file(dir.path(), "upgrading-song-auto.m4a", 100);
        file(dir.path(), "upgrading-song-auto.quality", 100);
        let part = file(dir.path(), "upgrading-song-auto.part", 100);

        // 模拟升级开始：获取租约
        let target_lease = CacheLease::acquire(&target);
        let part_lease = CacheLease::acquire(&part);

        // 淘汰超额缓存：limit_bytes 为 0，但因为3个文件（各10字节）均受保护，剩余30字节
        assert_eq!(trim_audio_in(dir.path(), 0), 30);
        // 正在升级的 target、sidecar 以及 part 均被保护
        assert!(target.exists());
        assert!(target.with_extension("quality").exists());
        assert!(part.exists());

        // 清空缓存操作同样不能删除租约文件
        assert_eq!(clear_in(dir.path()), 0);
        assert!(target.exists());
        assert!(part.exists());

        // 升级完成后释放租约
        drop(target_lease);
        drop(part_lease);

        // 释放租约后，淘汰可以正常回收 target 与 sidecar（临时 part 仍受 24h 保护机制保持）
        assert_eq!(trim_audio_in(dir.path(), 0), 10);
        assert!(!target.exists());
        assert!(!target.with_extension("quality").exists());
        assert!(part.exists());

        // 临时文件被删除后，缓存占用归零
        std::fs::remove_file(&part).unwrap();
        assert_eq!(trim_audio_in(dir.path(), 0), 0);
    }

    #[test]
    fn parse_audio_cache_stem_handles_tags_with_dashes() {
        assert_eq!(
            parse_audio_cache_stem("7427204361343191057-hi-res"),
            Some(("7427204361343191057".to_string(), "hi-res".to_string()))
        );
        assert_eq!(
            parse_audio_cache_stem("7427204361343191057-auto"),
            Some(("7427204361343191057".to_string(), "auto".to_string()))
        );
        assert_eq!(parse_audio_cache_stem("invalid"), None);
    }

    #[test]
    fn estimate_upgrade_ignores_previews() {
        let records = vec![
            CachedAudioRecord {
                track_id: "1".into(),
                quality_tag: "auto".into(),
                path: PathBuf::from("1-auto.m4a"),
                quality_label: "流畅 128k".into(),
                preview: false,
                actual_rank: Some(50),
                file_size: 4 * 1024 * 1024,
            },
            CachedAudioRecord {
                track_id: "2".into(),
                quality_tag: "auto".into(),
                path: PathBuf::from("2-auto.m4a"),
                quality_label: "试听 128k".into(),
                preview: true,
                actual_rank: Some(50),
                file_size: 1024 * 1024,
            },
        ];
        let est = estimate_upgrade(&records, 100);
        // preview 试听文件不应被计入升级
        assert_eq!(est.upgradeable_count, 1);
        assert_eq!(est.current_bytes, 4 * 1024 * 1024);
    }

    #[test]
    fn is_upgradeable_to_respects_tag_intent_cap() {
        let auto_record = CachedAudioRecord {
            track_id: "1".into(),
            quality_tag: "auto".into(),
            path: PathBuf::from("1-auto.m4a"),
            quality_label: "流畅 128k".into(),
            preview: false,
            actual_rank: Some(50),
            file_size: 4 * 1024 * 1024,
        };
        // auto 无上限，actual 50 可以升级至 100
        assert!(auto_record.is_upgradeable_to(100));

        let low_record = CachedAudioRecord {
            track_id: "2".into(),
            quality_tag: "low".into(),
            path: PathBuf::from("2-low.m4a"),
            quality_label: "流畅 128k".into(),
            preview: false,
            actual_rank: Some(20),
            file_size: 4 * 1024 * 1024,
        };
        // low 标签上限为 50，不允许越级升级至 100（无损）或 80（极高）
        assert!(!low_record.is_upgradeable_to(100));
        assert!(!low_record.is_upgradeable_to(80));
        // 目标为 50 且实际为 20 时，在 low 上限内，允许升级至 50
        assert!(low_record.is_upgradeable_to(50));
        // 目标为 20 时实际已达 20，不允许升级
        assert!(!low_record.is_upgradeable_to(20));

        let lossless_downgraded = CachedAudioRecord {
            track_id: "3".into(),
            quality_tag: "lossless".into(),
            path: PathBuf::from("3-lossless.m4a"),
            quality_label: "极高 320k".into(),
            preview: false,
            actual_rank: Some(50),
            file_size: 10 * 1024 * 1024,
        };
        // lossless 标签上限为 100，实际为 50（曾发生降档），允许升级至 100
        assert!(lossless_downgraded.is_upgradeable_to(100));
    }

    #[test]
    fn atomic_replace_cleans_leftover_backups_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("song-auto.m4a");
        let sidecar = dir.path().join("song-auto.quality");
        let leftover_backup = dir.path().join("song-auto.m4a.old");
        let leftover_sidecar = dir.path().join("song-auto.quality.old");
        let leftover_tmp = dir.path().join("song-auto.quality.tmp");
        std::fs::write(&target, b"original-content").unwrap();
        std::fs::write(&sidecar, "标准 128k\t16\t0\t50").unwrap();
        std::fs::write(&leftover_backup, b"stale-backup").unwrap();
        std::fs::write(&leftover_sidecar, b"stale-sidecar").unwrap();
        std::fs::write(&leftover_tmp, b"stale-tmp").unwrap();

        let part = dir.path().join("song-auto.part");
        std::fs::write(&part, b"new-content").unwrap();

        let res = atomic_replace_cache(&part, &target, &sidecar, "无损 871k\t11\t0\t100");
        assert!(res.is_ok());
        assert_eq!(std::fs::read(&target).unwrap(), b"new-content");
        assert!(!leftover_backup.exists());
        assert!(!leftover_sidecar.exists());
        assert!(!leftover_tmp.exists());
    }

    #[cfg(unix)]
    #[test]
    fn clear_does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = file(outside.path(), "keep.img", 0);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("outside")).unwrap();
        assert_eq!(clear_in(dir.path()), 0);
        assert!(path.exists());
    }
}
