//! 磁盘缓存维护。所有扫描/删除由后台调用；租约保护下载、待装载和正在播放的音频。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use crate::log_outlet::{log_event, LogLevel, LogTopic};

pub const DEFAULT_AUDIO_LIMIT_MB: u64 = 10 * 1024;
const PART_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// 「已确认没有更高档位」记录的有效期。
///
/// 曲库补档与会员权益变化都没有推送通知，永久保留一次探测结论会让那些曲目
/// 再也不参与升级；30 天后重新问一次，代价最多是一次请求。
pub const UPGRADE_CHECK_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// 「上次为补整曲重新取流」记录的有效期。
///
/// 试听不是「没有更高档位」的结论：权益变化或签名服务恢复后同一首歌就能拿到整曲，
/// 所以不能借用 30 天的确认记录。这里只做退避，挡住应用每次启动、每次播放命中都
/// 把整曲下一遍再丢掉；六小时后自动再试，用户买了会员不必等到第二天。
pub const PREVIEW_RETRY_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// 音频下载的用途。正常下载与升级补音质要落到不同的暂存名上：
/// 升级刻意不抢播放的锁，两条路会并发下同一首，同名会让后启动那条被
/// libresoda 的「目标正被别的下载占着」顶回来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DownloadPurpose {
    /// 点播、预取的正常下载。
    Normal,
    /// 后台补音质升级。
    Upgrade,
}

impl DownloadPurpose {
    pub(crate) fn suffix(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Upgrade => "upgrade",
        }
    }
}

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

// 同一个暂存目标的全部暂存尾巴（见 [`STAGING_TAILS`]）与它的交付文件共享保护键：
// 租约挂在暂存目标上时，这几个文件都不会被维护线程当废弃文件删掉。
// meta 与解密临时文件不带自己的租约，也必须跟着交付目标走，否则下载进行中被
// 淘汰收尾删掉，断点续传信息就毁了。名单从 [`STAGING_TAILS`] 单点派生，
// 将来加尾巴不用记得来这里补。
fn protection_key(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // 尾巴必须长在交付目标的 `.m4a` 后面（`{交付目标}{尾巴}`）才算同一组：
    // 先按尾巴剥一层，再看剩下的部分是否以 `.m4a` 收尾。封面那类 `.img.*.part`
    // 的剩余部分不带 `.m4a`，不进映射，别误套音频租约。
    let stem = STAGING_TAILS.iter().find_map(|tail| {
        let rest = name.strip_suffix(tail)?;
        rest.ends_with(".m4a")
            .then(|| &rest[..rest.len() - ".m4a".len()])
    });
    if let Some(stem) = stem {
        return path.with_file_name(format!("{stem}.m4a"));
    }
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

/// 音频下载暂存子目录（session.rs 的下载暂存都落在这里，与正式缓存分开）。
///
/// 单独放子目录是必须的：档位标签可以是用户自定义值，`auto-normal` 这种恰好会
/// 撞上正常下载的暂存名；分开之后，清理暂存绝不会删掉已经下好的正式缓存。
/// 淘汰收尾按这个名字找暂存文件；启动巡检扫整个音频目录时自然覆盖它。
pub(crate) fn staging_dir(audio_dir: &Path) -> PathBuf {
    audio_dir.join("staging")
}

/// 我们自己的暂存文件尾巴（最长在前），三处共用的单一出处：
/// 启动巡检的白名单（[`is_staging_residue_name`]）、租约保护键的映射
/// （[`protection_key`]，按「交付目标 + 尾巴」认）、淘汰收尾的名字反推
/// （[`staging_committed_name`]，逐层剥尾巴）。加新尾巴只需改这里，
/// 三条路同时生效；测试 `protection_key_maps_every_staging_tail_to_the_target`
/// 会守住保护键这层。
///
/// libresoda 以「交付目标 + 后缀」落暂存：数据 `.part`、锁 `.part.lock`、
/// 断点元数据 `.part.meta`（含 `.tmp` 中间态）、解密中间文件 `.dec.tmp`
///（尾巴挂在交付目标或 `.part` 上都见过，两种拼法都收）。历史版本把目标名带
/// `.part` 传下去，还留下过 `.part.part` / `.part.part.lock`。清理只认这份名单：
/// 正式缓存（`.m4a`）、`.quality` 记录、封面和名字不相干的文件一律不碰。
/// 一个名字最多匹配一条尾巴（后缀精确到结尾），顺序只影响可读性。
const STAGING_TAILS: [&str; 6] = [
    ".part.meta.tmp",
    ".part.dec.tmp",
    ".part.meta",
    ".part.lock",
    ".dec.tmp",
    ".part",
];

/// 文件名是否落在我们自己的暂存尾巴上。只看后缀：历史残留（`*.part.part`、
/// `*.part.part.lock`，在音频目录根下）和新暂存（staging 子目录）都靠它认出来。
/// 缓存目录里除 libresoda 暂存外没有别的写者会用这些后缀。
fn is_staging_residue_name(name: &str) -> bool {
    STAGING_TAILS.iter().any(|tail| name.ends_with(tail))
}

/// 暂存子目录里的文件名 → 它归属的正式缓存文件名（`{id}-{tag}.m4a`）。
///
/// 先剥掉所有暂存尾巴得到交付目标名（`{id}-{tag}-{用途}.m4a`），再剥掉用途后缀。
/// 名字对不上这套命名的（没有用途尾巴、或剥完不是 `.m4a`）一律 `None`：
/// 不是我们命名的暂存，绝不碰。曲目 id 编码后不含 `-`，按结尾剥用途不会剥错位置；
/// 自定义档位里带 `-normal` 时，交付名末尾是「标签-用途」两层，剥掉最后一层正好回到正式名。
fn staging_committed_name(name: &str) -> Option<String> {
    let mut rest = name;
    while let Some(tail) = STAGING_TAILS.iter().find(|tail| rest.ends_with(*tail)) {
        rest = &rest[..rest.len() - tail.len()];
    }
    if !rest.ends_with(".m4a") {
        return None;
    }
    for purpose in [DownloadPurpose::Normal, DownloadPurpose::Upgrade] {
        let tail = format!("-{}.m4a", purpose.suffix());
        if let Some(base) = rest.strip_suffix(&tail) {
            return Some(format!("{base}.m4a"));
        }
    }
    None
}

/// 目录扫描得到的一个缓存文件：路径、字节数、最后修改时间。
pub(crate) struct ScannedFile {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) modified: SystemTime,
}

/// 一次递归扫描的结果。
///
/// `incomplete` 记录「有地方没读到」：目录打不开、条目读错、元数据取不到。
/// 这种情况下 `files` 只是可读部分的视图，总量偏小，不能当成完整统计。
#[derive(Default)]
pub(crate) struct Scan {
    pub(crate) files: Vec<ScannedFile>,
    pub(crate) incomplete: bool,
}

impl Scan {
    pub(crate) fn total_bytes(&self) -> u64 {
        self.files
            .iter()
            .fold(0u64, |sum, file| sum.saturating_add(file.bytes))
    }
}

#[derive(Clone, Copy)]
enum EntryKind {
    Dir,
    File,
    /// 符号链接、套接字等：一律不碰，免得走到缓存目录外。
    Other,
}

/// 一层目录的读取结果。
struct Listing {
    entries: Vec<(PathBuf, EntryKind)>,
    /// 这一层有条目读不到：条目本身或它的类型取失败。
    incomplete: bool,
}

type ListingResult = std::io::Result<Listing>;

fn list_dir(dir: &Path) -> ListingResult {
    let mut entries = Vec::new();
    let mut incomplete = false;
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else {
            incomplete = true;
            continue;
        };
        let kind = match entry.file_type() {
            Ok(kind) if kind.is_dir() => EntryKind::Dir,
            Ok(kind) if kind.is_file() => EntryKind::File,
            Ok(_) => EntryKind::Other,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        entries.push((entry.path(), kind));
    }
    Ok(Listing {
        entries,
        incomplete,
    })
}

/// 递归扫描 `dir`，不跟随符号链接。所有维护入口共用这一份遍历。
pub(crate) fn scan_files(dir: &Path) -> Scan {
    scan_with(dir, list_dir)
}

/// 遍历实现，目录读取可注入：测试用它模拟「某个子目录读不到」，
/// 不必依赖权限位（root 下无效、Windows 上不生效）。
fn scan_with<L>(dir: &Path, list: L) -> Scan
where
    L: FnMut(&Path) -> ListingResult,
{
    let mut scan = Scan::default();
    let mut list = list;
    scan_into(dir, &mut scan, &mut list);
    scan
}

fn scan_into<L>(dir: &Path, scan: &mut Scan, list: &mut L)
where
    L: FnMut(&Path) -> ListingResult,
{
    let listing = match list(dir) {
        Ok(listing) => listing,
        // 目录不存在是正常状态（首次启动、用户没看过封面、正在被并发删掉），
        // 不算「扫描不完整」，也不该让封面淘汰在这一轮停摆。
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            scan.incomplete = true;
            return;
        }
    };
    scan.incomplete |= listing.incomplete;
    for (path, kind) in listing.entries {
        match kind {
            EntryKind::Dir => scan_into(&path, scan, list),
            EntryKind::File => match std::fs::metadata(&path) {
                Ok(meta) => scan.files.push(ScannedFile {
                    path,
                    bytes: meta.len(),
                    modified: meta.modified().unwrap_or_else(|_| SystemTime::now()),
                }),
                // 扫描与取元数据之间被删掉：正常竞争，不算扫描缺口。
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => scan.incomplete = true,
            },
            EntryKind::Other => {}
        }
    }
}

fn is_fresh_part(path: &Path, modified: SystemTime) -> bool {
    path.extension().is_some_and(|ext| ext == "part")
        && modified.elapsed().unwrap_or_default() < PART_MAX_AGE
}

/// 封面分组键：文件名开头的 URL 哈希。缩略图 `{hash}-160.img`、原图 `{hash}.img`、
/// 临时文件 `{hash}-160.img.*.part` 都以同一个哈希开头，这样整组能一起淘汰，
/// 不会出现只剩缩略图或只剩原图的半套缓存。
fn cover_group_key(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let hash: String = name.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if hash.is_empty() {
        name
    } else {
        hash
    }
}

/// 封面缓存硬上限：缩略图加原图合计，固定 512 MiB，不做用户配置项。
pub const COVER_LIMIT_MB: u64 = 512;

/// 按组内最后使用时间从旧到新淘汰，直到总量不超 `limit_bytes`。
///
/// 音频与封面共用的那一份淘汰器：两者只在「怎么分组」「怎么删」和「最后使用时间从哪来」
/// 上不同。`group_key` 把同一份缓存的文件归到一组（音频是文件 + `.quality` sidecar，
/// 封面是缩略图 + 原图），组内取最新使用时间当 LRU 键，整组一起删，不会留下半套缓存。
/// 24 小时内的 `.part` 计入总量但不参与淘汰（还在下载）。
///
/// `last_seen` 是「文件路径 → 最近一次显示时间」；没有记录的路径按 mtime 排。
///
/// 返回 `(淘汰后的字节数, 真正删掉的路径)`：删除成功才算释放空间，失败不中断，
/// 失败的文件留在总量里等下轮再试。
fn evict_lru<K, G, R>(
    files: Vec<ScannedFile>,
    limit_bytes: u64,
    group_key: G,
    last_seen: &HashMap<PathBuf, SystemTime>,
    mut remove: R,
) -> (u64, Vec<PathBuf>)
where
    K: Eq + std::hash::Hash,
    G: Fn(&Path) -> K,
    R: FnMut(&Path) -> bool,
{
    let mut groups: HashMap<K, (SystemTime, Vec<(PathBuf, u64)>)> = HashMap::new();
    let mut total = 0u64;
    for file in files {
        total = total.saturating_add(file.bytes);
        if is_fresh_part(&file.path, file.modified) {
            continue;
        }
        let used_at = last_seen
            .get(&file.path)
            .copied()
            .map_or(file.modified, |seen| file.modified.max(seen));
        let group = groups
            .entry(group_key(&file.path))
            .or_insert((used_at, Vec::new()));
        group.0 = group.0.max(used_at);
        group.1.push((file.path, file.bytes));
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|(_, (used_at, _))| *used_at);
    let mut removed = Vec::new();
    for (_, (_, group_files)) in groups {
        if total <= limit_bytes {
            break;
        }
        for (path, bytes) in group_files {
            if remove(&path) {
                total = total.saturating_sub(bytes);
                removed.push(path);
            }
        }
    }
    (total, removed)
}

/// 扫描不完整时采取的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoverTrimAction {
    /// 可读部分自己没超限，判断不了真实占用，本轮不删。
    Skip,
    /// 可读部分自己已超限，按它淘汰最旧的组。
    EvictReadable,
}

impl CoverTrimAction {
    const fn label(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::EvictReadable => "evict_readable",
        }
    }
}

/// 一次「扫描不完整」的上报内容。
#[derive(Debug, PartialEq)]
struct CoverTrimAlert {
    readable_bytes: u64,
    readable_files: usize,
    action: CoverTrimAction,
}

impl CoverTrimAlert {
    fn log(&self) {
        let bytes = self.readable_bytes.to_string();
        let files = self.readable_files.to_string();
        log_event(
            LogLevel::Warn,
            LogTopic::App,
            "cover_trim_incomplete",
            &[
                ("readable_bytes", bytes.as_str()),
                ("readable_files", files.as_str()),
                ("action", self.action.label()),
            ],
        );
    }
}

/// 上报门：只在动作变化时放行。
///
/// 扫描一直不完整、动作一直是 `skip` 时，每轮维护（约 60 秒）都写一条 Warn 就是刷屏；
/// 日志出口的折叠只吃「连续且完全相同」的行，字段里的字节数一变就断。所以由这里记住
/// 上次报告的动作，同一个动作只报一次；扫描恢复完整（`None`）后复位，下次出问题再报。
///
/// 注意这是**进程内单个封面缓存目录**的告警状态：两个目录共用一个门会互相压制告警，
/// 也会互相复位。要按目录各自告警，就为每个目录注入自己的门（`trim_covers_scan` 就是这么收的）。
#[derive(Default)]
struct CoverAlertGate(Mutex<Option<CoverTrimAction>>);

impl CoverAlertGate {
    fn admit(&self, action: Option<CoverTrimAction>) -> bool {
        let mut last = self.0.lock().unwrap_or_else(|err| err.into_inner());
        if *last == action {
            return false;
        }
        *last = action;
        true
    }
}

fn cover_alert_gate() -> &'static CoverAlertGate {
    static GATE: OnceLock<CoverAlertGate> = OnceLock::new();
    GATE.get_or_init(CoverAlertGate::default)
}

/// 封面淘汰的决策层：读不全时，只有在可读部分自己就超限时才删。
///
/// 读不到的子树让总量偏小，所以可读部分的字节数只是下界：下界已超限说明真实占用必然
/// 也超限，按可读部分淘汰最旧的组是对的；下界没超限则判断不了，本轮跳过并上报
/// `cover_trim_incomplete`（`readable_bytes` / `readable_files` / `action`，动作变化时才写）。
fn trim_covers_scan<F, L>(
    scan: Scan,
    limit_bytes: u64,
    last_seen: &HashMap<PathBuf, SystemTime>,
    gate: &CoverAlertGate,
    mut alert: L,
    remove: F,
) -> (u64, Vec<PathBuf>)
where
    F: FnMut(&Path) -> bool,
    L: FnMut(CoverTrimAlert),
{
    if !scan.incomplete {
        gate.admit(None);
        return evict_lru(scan.files, limit_bytes, cover_group_key, last_seen, remove);
    }
    let readable_bytes = scan.total_bytes();
    let action = if readable_bytes > limit_bytes {
        CoverTrimAction::EvictReadable
    } else {
        CoverTrimAction::Skip
    };
    if gate.admit(Some(action)) {
        alert(CoverTrimAlert {
            readable_bytes,
            readable_files: scan.files.len(),
            action,
        });
    }
    if action == CoverTrimAction::Skip {
        (readable_bytes, Vec::new())
    } else {
        evict_lru(scan.files, limit_bytes, cover_group_key, last_seen, remove)
    }
}

/// 按字节上限淘汰封面缓存目录，返回 `(剩余字节数, 实际删掉的路径)`。
///
/// `last_seen` 是调用方采样的「路径 → 最近显示时间」；没有记录的路径退回文件 mtime。
/// 扫描完整时剩余字节数就是目录真实占用；扫描不完整时它只是**可读部分**的字节数
/// （读不到的子目录不计入），所以调用方不要拿它当磁盘占用的准确值。
pub fn trim_covers_in(
    dir: &Path,
    limit_bytes: u64,
    last_seen: &HashMap<PathBuf, SystemTime>,
) -> (u64, Vec<PathBuf>) {
    trim_covers_scan(
        scan_files(dir),
        limit_bytes,
        last_seen,
        cover_alert_gate(),
        |alert| alert.log(),
        |path| std::fs::remove_file(path).is_ok(),
    )
}

/// 对封面缓存目录施加 MB 上限，返回这次实际删掉的文件路径。调用方必须在后台执行。
///
/// 和 `trim_audio` 一样收 MB。`last_seen` 由调用方在渲染线程采样后传入，用来把最近
/// 显示过的封面排到后面（与 mtime 取较晚者）；传空表则退回按 mtime 淘汰。
/// 返回删掉的路径，让界面只同步这几张封面的内存状态。
pub fn trim_covers(limit_mb: u64, last_seen: &HashMap<PathBuf, SystemTime>) -> Vec<PathBuf> {
    let (_, removed) = trim_covers_in(
        &crate::audio::cover_cache_dir(),
        limit_mb.saturating_mul(1024 * 1024),
        last_seen,
    );
    removed
}

/// 递归清理缓存，保留正在使用的音频和近期临时文件。返回成功删除的文件数。
pub fn clear_in(root: &Path) -> usize {
    scan_files(root)
        .files
        .into_iter()
        .filter(|file| !is_fresh_part(&file.path, file.modified) && remove_idle(&file.path))
        .count()
}

/// 按最后使用时间淘汰音频（命中缓存时更新 mtime）。租约文件可暂时超额。
/// sidecar 与音频成组淘汰；失败的删除不计入释放空间。调用方必须在后台执行。
///
/// 扫描不完整时照旧只按可读部分淘汰：音频有租约保护、又是用户主动设定的额度，
/// 少删（停在额度之上）比不删更接近预期。封面则在可读部分已超限时才这么做、否则本轮
/// 不删——这点差异是有意保留的：音频额度是用户可见的硬需求，封面是「别无限涨」的软约束。
pub fn trim_audio_in(dir: &Path, limit_bytes: u64) -> u64 {
    trim_audio_scan(scan_files(dir), limit_bytes, &staging_dir(dir))
}

fn trim_audio_scan(scan: Scan, limit_bytes: u64, staging: &Path) -> u64 {
    let (total, removed) = evict_lru(
        scan.files,
        limit_bytes,
        protection_key,
        &HashMap::new(),
        remove_idle,
    );
    // 扫描与收尾之间暂存文件可能新出现或长大，收尾释放的字节可能不在 total 里，
    // 用饱和减法兜底，绝不让统计下溢。
    total.saturating_sub(remove_evicted_staging(&removed, staging))
}

/// 被淘汰的正式音频，把它留在暂存子目录的整组残余一并拿走。
///
/// 暂存与正式缓存分居两个目录，扫描里各自成组：超额淘汰删掉正式文件时，
/// 那首的锁和半截 `.part` 不会跟着走，会在目录里单独立着。这里按名字把被淘汰
/// 曲目的交付目标、`.part`、`.part.lock`、`.part.meta`、`.dec.tmp` 一次收齐。
/// 正在进行的下载仍由租约挡住（remove_idle 的检查与 unlink 在同一临界区），
/// 暂存目录不存在（从来没有过下载）就是没有残余，正常返回 0 字节。
///
/// 24 小时内的 `.part` 平时不参与淘汰（可能还在下载），但正式文件已经淘汰时
/// 这首的暂存就是纯垃圾：租约挡得住真正在下的，挡不住的说明早没人下了，一起删。
/// 返回实际释放的字节数。
///
/// 已知边界：收尾释放的字节不计入选组时的预算。`evict_lru` 只看正式文件组，
/// 新鲜暂存的字节数要等收尾才知道，于是「旧曲目 + 大块无租约新鲜暂存」同时
/// 命中时可能多淘汰一到两组——多删的按 LRU 序正好是下一顺位，下轮自愈。
/// 要把字节数算进选组，得给与封面共用的 `evict_lru` 加每组钩子，不值得。
fn remove_evicted_staging(removed: &[PathBuf], staging: &Path) -> u64 {
    let audio_root = match staging.parent() {
        Some(root) => root,
        None => return 0,
    };
    let evicted: std::collections::HashSet<String> = removed
        .iter()
        .filter(|path| {
            path.parent() == Some(audio_root) && path.extension().is_some_and(|ext| ext == "m4a")
        })
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();
    if evicted.is_empty() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(staging) else {
        return 0;
    };
    let mut freed = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(committed) = staging_committed_name(name) else {
            continue;
        };
        if !evicted.contains(&committed) {
            continue;
        }
        // 先量后删：删成功就没了，量不出字节的删了也不计入释放空间。
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if remove_idle(&path) {
            freed = freed.saturating_add(meta.len());
        }
    }
    freed
}

/// 残留清扫：清掉指定目录里超过 [`PART_MAX_AGE`]、没被租约占用的暂存残留。
///
/// 底层库从不删锁文件（进程退出靠内核解锁，一个下载目标留一把空锁是它写明的设计），
/// 它自带的 7 天清扫也只认「数据 + 记录成对、两边都够老」的文件——只有 `.part`
/// 没有记录的孤儿它认不出，锁文件更是完全不认。历史残留只能在这里按后缀白名单补刀：
/// `.part` / `.part.meta` / `.part.meta.tmp` / `.dec.tmp` / `.part.lock`，超过 24 小时
/// 且租约放行的才删；清完之后锁文件数量就降到「最近 24 小时内下载过的曲目数」的量级。
///
/// 正式缓存、`.quality` 记录、封面和名字不相干的文件一律不碰；正在下载的暂存由
/// 租约挡住（检查与 unlink 在同一临界区），24 小时的年龄门槛再兜一层底。
/// 暂存子目录里自己命名的交付目标（`.m4a`）不在白名单里：它由下次下载的
/// `stage_path` 或淘汰收尾负责，巡检不凭后缀判断 `.m4a` 的归属。
/// 返回删除的文件数。调用方必须在后台执行。
pub fn sweep_stale_staging_in(dir: &Path) -> usize {
    let removed = scan_files(dir)
        .files
        .into_iter()
        .filter(|file| {
            is_staging_residue_name(
                file.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(""),
            ) && file.modified.elapsed().unwrap_or_default() >= PART_MAX_AGE
        })
        .filter(|file| remove_idle(&file.path))
        .count();
    if removed > 0 {
        log_event(
            LogLevel::Info,
            LogTopic::App,
            "stale_staging_swept",
            &[("removed", removed.to_string().as_str())],
        );
    }
    removed
}

/// 对整个音频缓存目录（含暂存子目录）跑一轮残留清扫，返回删除的文件数。
///
/// 只在启动后的首轮维护实际执行：主心跳约 60 秒就调一轮维护，而残留的年龄门槛
/// 是 24 小时，会话期间新产生的残留本来就不满 24 小时，每轮重扫几乎全是空跑；
/// 音频目录可能上万文件，这个代价省得掉。会话期间攒下的残留留给下次启动清，
/// 与「每次启动的缓存巡检顺手清掉」的口径一致。调用方必须在后台执行。
pub fn sweep_stale_staging() -> usize {
    static STALE_SWEEP_DONE: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if STALE_SWEEP_DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return 0;
    }
    sweep_stale_staging_in(&root_dir().join("audio"))
}

pub fn trim_audio(limit_mb: u64) -> u64 {
    trim_audio_in(
        &root_dir().join("audio"),
        limit_mb.saturating_mul(1024 * 1024),
    )
}

/// 曲目 id 写进缓存文件名前先过这一步：字母数字原样留着，其余字节写成 `_XX`
/// （字节的十六进制，小写）。
///
/// 要挡住两件事：一是带 `/` 或 `..` 的 id 把文件拼到缓存目录外面；二是编码不能
/// 把两个不同的 id 压成同一个名字——`a/` 和 `a_2f` 撞在一起的话，两首歌共用一个
/// 缓存文件，播第二首会直接命中第一首的音频。
///
/// 和 [`decode_track_id`] 是严格的一对，谁改谁都得一起改：文件名读回来时靠它反解，
/// 解不出原来那个 id，下次拼的路径就对不上，缓存永远 miss。
pub(crate) fn encode_track_id(track_id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(track_id.len());
    for byte in track_id.bytes() {
        if byte.is_ascii_alphanumeric() {
            out.push(byte as char);
        } else {
            // `_` 自己也编码，所以编码结果里出现的每个 `_` 都一定是转义序列的开头，
            // 反解时不用猜「这串是不是已经编码过了」。
            out.push('_');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

/// [`encode_track_id`] 的反函数，把缓存文件名里的 id 还原成原始值。
///
/// 非 ASCII 的 id 也是逐字节还原成原始字节，再按 UTF-8 拼回字符串。
/// 遇到不成对的下划线（畸形文件名）就原样保留，不给坏数据瞎猜。
pub(crate) fn decode_track_id(encoded: &str) -> String {
    let src = encoded.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'_' && i + 2 < src.len() {
            let hex = std::str::from_utf8(&src[i + 1..i + 3])
                .ok()
                .and_then(|text| u8::from_str_radix(text, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| encoded.to_string())
}

/// 解析音频缓存主文件名 stem，分离出 `(track_id, quality_tag)`。
/// 文件名格式形如 `{track_id}-{quality_tag}`（如 `7427204361343191057-auto`）。
///
/// id 一侧是转义过的，这里顺手反解，返回的 track_id 就是能直接拿去拼路径的原始 id。
/// 转义后的 id 不含 `-`（连字符会写成 `_2d`），所以按第一个 `-` 切分不会切错位置。
pub fn parse_audio_cache_stem(stem: &str) -> Option<(String, String)> {
    let (track_id, tag) = stem.split_once('-')?;
    if track_id.is_empty() || tag.is_empty() {
        return None;
    }
    Some((decode_track_id(track_id), tag.to_string()))
}

/// 解析 sidecar `{档位}\t{字节}\t{0|1}\t{档位分数}\t{已探测目标档位}\t{探测时间}\t{补整曲尝试时间}`，
/// 与实际字节数核对后给出完整记录。
///
/// 尺寸不符或档位为空一律返回 `None`。
/// 第四到第七个字段都是后加的：旧格式缺少哪个，对应字段就是 `None`，
/// 不影响前三字段的命中判定。第 4 字段也可能是空串——档位未知的旧缓存
/// 记下「已探测」结论之后就是这个样子，读回来仍然是「档位未知」。
/// 第 7 字段只对试听片段有意义：它记的是「上次为补整曲重新取流的时间」，
/// 与第 5 字段「已确认没有更高档位」是两件事，不能互相冒充。
/// 旧缓存把 50 分档写作「标准」，这里按记录里的实际分数洗成当前显示名
///（50 分档是「流畅」、更低分是「省流」），只换档位前缀、保留码率，旧文件不必重下。
pub fn parse_sidecar(text: &str, actual: u64) -> Option<SidecarMeta> {
    let mut fields = text.split('\t');
    let quality = fields.next()?.trim().to_string();
    let size = fields.next()?.trim().parse::<u64>().ok()?;
    if size != actual || quality.is_empty() {
        return None;
    }
    let (quality, preview) = match fields.next() {
        Some(flag) => (quality, flag.trim() == "1"),
        None => (strip_preview_marker(&quality), preview_from_label(&quality)),
    };
    let actual_rank = parse_rank(fields.next());
    let checked_rank = parse_rank(fields.next());
    let checked_at = fields
        .next()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let preview_attempt_at = fields
        .next()
        .and_then(|value| value.trim().parse::<u64>().ok());
    // 只改显示名，不动文件与分数：50 分档曾经叫「标准」，与 libresoda 的 standard(70) 撞名。
    let quality = normalize_quality_label(&quality, actual_rank);
    Some(SidecarMeta {
        quality,
        preview,
        actual_rank,
        checked_rank,
        checked_at,
        preview_attempt_at,
    })
}

fn parse_rank(field: Option<&str>) -> Option<i64> {
    field.and_then(|value| value.trim().parse::<i64>().ok())
}

/// 更早的两字段格式把试听拼在档位里（`无损 · 试听`），这里把它还原成布尔。
fn preview_from_label(quality: &str) -> bool {
    quality.contains("试听")
}

/// 去掉档位文本里的试听标记，只留档位与码率。
fn strip_preview_marker(quality: &str) -> String {
    quality.replace(" · 试听", "")
}

/// 一条 sidecar 记录。前四个字段是旧格式，后三个记录升级探测结果：
/// 曾经向服务端要过哪个目标档位、什么时候确认没有更高流，以及上次为补整曲
/// 重新取流的时间（只对试听片段）。后两件事不能混：试听不是曲库结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarMeta {
    /// 档位显示文本（含码率，不含试听标记）
    pub quality: String,
    /// 是否为试听片段
    pub preview: bool,
    /// 实际档位分数（旧缓存缺失时为 `None`）
    pub actual_rank: Option<i64>,
    /// 上次探测时请求的目标档位分数
    pub checked_rank: Option<i64>,
    /// 上次探测时间（Unix 秒）
    pub checked_at: Option<u64>,
    /// 上次为补整曲重新取流的时间（Unix 秒）；整曲不用这个字段
    pub preview_attempt_at: Option<u64>,
}

impl SidecarMeta {
    /// 仍然有效的「已确认没有更高档位」分数；没记录、过期或时钟回拨时返回 `None`。
    ///
    /// 过期后重新探测，避免服务端补档或会员权益变化后客户端永久不再尝试升级。
    pub fn fresh_checked_rank(&self, now: u64) -> Option<i64> {
        let (rank, checked_at) = (self.checked_rank?, self.checked_at?);
        if now >= checked_at && now - checked_at < UPGRADE_CHECK_TTL.as_secs() {
            Some(rank)
        } else {
            None
        }
    }

    /// 上次补整曲的尝试是否还在退避期内；没记录、过期或时钟回拨时返回 `None`。
    ///
    /// 只在试听片段上使用。返回 `Some(时间)` 表示这段时间内不必再自动重下。
    pub fn fresh_preview_attempt_at(&self, now: u64) -> Option<u64> {
        let attempted_at = self.preview_attempt_at?;
        if self.preview && now >= attempted_at && now - attempted_at < PREVIEW_RETRY_TTL.as_secs() {
            Some(attempted_at)
        } else {
            None
        }
    }
}

/// 当前时间（Unix 秒），用于判断探测记录是否仍在有效期内。
pub(crate) fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// 已确认的记录是否覆盖本轮目标档位：记录的目标不低于本次目标，就不必再问。
///
/// 只问过较低档位、现在想升到更高档位时仍应探测——那次结论并不能代表更高档位。
pub fn upgrade_check_covers(checked_rank: Option<i64>, target_rank: Option<i64>) -> bool {
    match (checked_rank, target_rank) {
        (Some(checked), Some(target)) => checked >= target,
        _ => false,
    }
}

/// 在 sidecar 上记录「已经向服务端要过 `checked_rank` 档、没有更高流」。
///
/// 只应在确认性结果后调用：下载成功、拿到完整流、档位没有提升。
/// 网络失败与试听回退都不写，它们会随网络或会员权益变化。
///
/// `expected_actual_rank` 与 sidecar 现有分数不一致时直接跳过——说明文件已经被
/// 另一个写者替换，不能再把旧结论写到新文件上。传 `None` 表示这条记录档位未知
/// （旧三字段缓存），此时 sidecar 也不许有分数；写入时第 4 字段留空，
/// 探测结论照样落得下去，否则这类文件每轮批量升级都要白下一遍。
/// 更早的两字段格式连试听标记都没有，补第 3 字段时从档位文本还原，顺手把文本洗干净。
/// 调用方必须持有该文件的 `download_lock`，避免与替换序列交错。
pub(crate) fn record_upgrade_check(
    sidecar_path: &Path,
    expected_actual_rank: Option<i64>,
    checked_rank: i64,
    now: u64,
) -> std::io::Result<()> {
    let text = std::fs::read_to_string(sidecar_path)?;
    let trimmed = text.trim_end_matches(['\r', '\n']);
    let fields: Vec<&str> = trimmed.split('\t').collect();
    // 尺寸字段读不出来就别改写：写回去的 sidecar 会让每次读取都 miss，等于删掉这份缓存。
    if fields.len() < 2 || fields[1].trim().parse::<u64>().is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sidecar is missing the quality or size field",
        ));
    }
    if parse_rank(fields.get(3).copied()) != expected_actual_rank {
        return Ok(());
    }
    // 前四个字段原样保留（旧格式缺的第 3、4 字段补上），丢掉旧的探测结论后重新写。
    let (quality_field, preview_field) = match fields.get(2).copied() {
        Some(flag) => (fields[0].trim().to_string(), flag.trim().to_string()),
        None => {
            let raw = fields[0].trim();
            (
                strip_preview_marker(raw),
                u8::from(preview_from_label(raw)).to_string(),
            )
        }
    };
    let rank_field = fields.get(3).copied().unwrap_or("").trim();
    // 第 7 字段（补整曲尝试时间）与本次结论无关，原样保留，免得一次「无更高档位」
    // 记录把试听的退避时间抹掉。
    let preview_attempt = fields
        .get(6)
        .copied()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let content = match preview_attempt {
        Some(attempted_at) => format!(
            "{}\t{}\t{preview_field}\t{rank_field}\t{checked_rank}\t{now}\t{attempted_at}",
            quality_field,
            fields[1].trim(),
        ),
        None => format!(
            "{}\t{}\t{preview_field}\t{rank_field}\t{checked_rank}\t{now}",
            quality_field,
            fields[1].trim(),
        ),
    };
    replace_sidecar_file(sidecar_path, &content)
}

/// 在 sidecar 上记录「刚刚为补整曲重新取过一次流」。
///
/// 只写时间，不写「没有更高档位」：这次拿回来的可能还是试听，而试听会随账号权益
/// 或签名服务恢复变成整曲，不能当成曲库结论（见 `session::confirmed_miss_rank`）。
/// 下一次自动尝试要等 [`PREVIEW_RETRY_TTL`] 之后，避免每次启动、每次命中都整曲下一遍。
///
/// `expected_actual_rank` 的用法与 [`record_upgrade_check`] 相同：与现有分数不一致
/// 说明文件已经被另一个写者替换，旧尝试记录不该落到新文件上。
/// 返回 `Ok(true)` 表示退避确实写进了 sidecar；`Ok(false)` 表示按上面的规则跳过。
/// 调用方必须持有该文件的 `download_lock`，避免与替换序列交错。
pub(crate) fn record_preview_attempt(
    sidecar_path: &Path,
    expected_actual_rank: Option<i64>,
    now: u64,
) -> std::io::Result<bool> {
    let text = std::fs::read_to_string(sidecar_path)?;
    let trimmed = text.trim_end_matches(['\r', '\n']);
    let fields: Vec<&str> = trimmed.split('\t').collect();
    if fields.len() < 2 || fields[1].trim().parse::<u64>().is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sidecar is missing the quality or size field",
        ));
    }
    if parse_rank(fields.get(3).copied()) != expected_actual_rank {
        return Ok(false);
    }
    let (quality_field, preview_field) = match fields.get(2).copied() {
        Some(flag) => (fields[0].trim().to_string(), flag.trim().to_string()),
        None => {
            let raw = fields[0].trim();
            (
                strip_preview_marker(raw),
                u8::from(preview_from_label(raw)).to_string(),
            )
        }
    };
    // 只有试听记录才需要退避；文件已经被换成整曲时什么都不写。
    if preview_field != "1" {
        return Ok(false);
    }
    let rank_field = fields.get(3).copied().unwrap_or("").trim();
    let checked_rank = fields.get(4).copied().unwrap_or("").trim();
    let checked_at = fields.get(5).copied().unwrap_or("").trim();
    // 第 4、5 字段可能为空，但位置必须留着，后面的时间才不会错位。
    let content = format!(
        "{}\t{}\t1\t{rank_field}\t{checked_rank}\t{checked_at}\t{now}",
        quality_field,
        fields[1].trim(),
    );
    replace_sidecar_file(sidecar_path, &content).map(|()| true)
}

/// 先写临时文件再备份替换；失败回滚，绝不把有效的缓存元数据删掉换一条记录。
/// 备份名与 `atomic_replace_cache` 共用，二者都由同一把 `download_lock` 串行化。
fn replace_sidecar_file(sidecar_path: &Path, content: &str) -> std::io::Result<()> {
    let tmp = sidecar_path.with_extension("quality.tmp");
    if let Err(err) = std::fs::write(&tmp, content) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    let backup = sidecar_path.with_extension("quality.old");
    let _ = std::fs::remove_file(&backup);
    let had_sidecar = sidecar_path.exists();
    if had_sidecar {
        if let Err(err) = std::fs::rename(sidecar_path, &backup) {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
    }
    if let Err(err) = std::fs::rename(&tmp, sidecar_path) {
        if had_sidecar {
            let _ = std::fs::rename(&backup, sidecar_path);
        }
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

/// 把旧 sidecar 里的档位名洗成当前显示名。
///
/// 有分数时按分数重写档位前缀（`标准 128k` + 50 → `流畅 128k`，+ 20 → `省流 96k` 同理），
/// 这样旧缓存的徽章与新缓存的命名一致；没有分数（旧三字段记录）就只把撞名的「标准」
/// 换成 50 分档的新名字，至少不再是 libresoda 的 `standard`(70)。
fn normalize_quality_label(label: &str, rank: Option<i64>) -> String {
    let Some(rank) = rank else {
        return label.replace("标准", crate::quality::tier_label(50));
    };
    let tier = crate::quality::tier_label(rank);
    // `TIER_LABELS` 已按长度从长到短排列；「标准」是历史写法，不在表里。
    for known in crate::quality::TIER_LABELS.iter().copied().chain(["标准"]) {
        if let Some(rest) = label.strip_prefix(known) {
            return format!("{tier}{rest}");
        }
    }
    label.to_string()
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
    /// 实际档位分数（旧缓存缺失时为 `None`；记了 0 分表示认不出档位，两者都算未知）
    pub actual_rank: Option<i64>,
    /// 仍在有效期内、已确认没有更高档位的目标分数；过期记录不入此字段
    pub checked_rank: Option<i64>,
    /// 本地文件字节数
    pub file_size: u64,
}

impl CachedAudioRecord {
    /// 实际档位对应的显示名（见 [`crate::quality::tier_label`]；档位未知返回 `None`）。
    pub fn actual_tier(&self) -> Option<&'static str> {
        self.actual_rank.map(crate::quality::tier_label)
    }

    /// 这份缓存需不需要重新取一次更高的流。
    ///
    /// 没记分数（旧三字段缓存）时不再算「不用重下」：按 sidecar 的档位标签看能否证明达标，
    /// 证明不了就当作需要重下。记了 0 分（认不出档位）的不重下——再取一次还是同一条流
    /// （见 [`crate::quality::cache_needs_higher_stream`]）。
    pub fn needs_higher_stream(&self, target_rank: i64) -> bool {
        crate::quality::cache_needs_higher_stream(
            self.actual_rank,
            &self.quality_label,
            target_rank,
        )
    }

    /// 当前记录是否允许批量升级至目标档位。
    ///
    /// 排除试听片段、已达到或超过目标的记录，以及最近已经确认过「没有更高档位」的目标：
    /// 最后一条避免每次批量升级都重新拉取同一批无更高流的曲目。
    /// 试听片段仍不纳入批量升级（#130）：补整曲由播放命中时的自动路径负责，
    /// 并有自己的退避记录；批量升级纳入与否是单独的产品决定，不能在这里悄悄扩大流量。
    /// 档位未知的旧缓存计入可升级——它证明不了自己达标；其中标签已证明达标的
    /// （旧「无损」标签）由 [`CachedAudioRecord::needs_higher_stream`] 挡掉，不白花流量。
    /// 同时受该缓存文件自身的 quality_tag 意图上限约束：
    /// 例如用户在省流模式下缓存的 `-low.m4a`（上限 50），不会被冲高为无损（100），
    /// 以免后续省流播放时命中数十兆的庞大文件破坏省流意图。
    pub fn is_upgradeable_to(&self, target_rank: i64) -> bool {
        let within_cap = crate::quality::QualityIntent::parse(&self.quality_tag)
            .max_rank()
            .is_none_or(|cap| target_rank <= cap);
        !self.preview
            && within_cap
            && self.needs_higher_stream(target_rank)
            && !upgrade_check_covers(self.checked_rank, Some(target_rank))
    }
}

/// 读取单个音频缓存文件及其边车，构造记录。
pub fn read_cache_record(m4a_path: &Path) -> Option<CachedAudioRecord> {
    if m4a_path.extension().and_then(|ext| ext.to_str()) != Some("m4a") {
        return None;
    }
    let file_meta = std::fs::metadata(m4a_path).ok()?;
    if !file_meta.is_file() || file_meta.len() == 0 {
        return None;
    }
    let file_size = file_meta.len();
    let stem = m4a_path.file_stem()?.to_str()?;
    let (track_id, quality_tag) = parse_audio_cache_stem(stem)?;
    let quality_path = m4a_path.with_extension("quality");
    let text = std::fs::read_to_string(&quality_path).ok()?;
    let meta = parse_sidecar(&text, file_size)?;
    let checked_rank = meta.fresh_checked_rank(now_epoch_secs());
    Some(CachedAudioRecord {
        track_id,
        quality_tag,
        path: m4a_path.to_owned(),
        quality_label: meta.quality,
        preview: meta.preview,
        actual_rank: meta.actual_rank,
        checked_rank,
        file_size,
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
    /// 扫到的档位未知的旧缓存数（非试听，与能否升级无关）：设置页要如实说出「发现 X 首」，
    /// 哪怕这一轮它们都被抑制了。
    pub unknown_rank_count: usize,
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
    const MIB: u64 = 1024 * 1024;
    // 按档位分数估算，而不是按 `tier_label` 的显示名：显示名属于 UI，
    // 改一次文案不该让缓存升级估算跟着失准。
    match target_rank {
        // Hi-Res 无损：FLAC 24bit，通常约 50-80 MB，取 60 MiB
        rank if rank >= 110 => 60 * MIB,
        // 无损 FLAC 通常约 30-45 MB，取 35 MiB
        rank if rank >= 100 => 35 * MIB,
        // Hi-Res 非无损 / 杜比全景声，取 25 MiB
        rank if rank >= 88 => 25 * MIB,
        // 极高 320k 通常约 8-12 MB，取 10 MiB
        rank if rank >= 80 => 10 * MIB,
        // 较高 通常约 5-8 MB，取 7 MiB
        rank if rank >= 70 => 7 * MIB,
        // 流畅/低档 通常约 3-5 MB，取 4 MiB
        _ => 4 * MIB,
    }
}

/// 评估将已有缓存批量升级到指定档位所需流量与容量变化。
///
/// `unknown_rank_count` 统计的是**档位未知的旧缓存有多少**，与本轮能不能升无关：
/// 界面要把「有 X 首缓存根本不知道是什么档」如实报出来，不能在它们被目标档位或探测记录
/// 抑制之后就当作不存在（#89）。试听片段不报——它本来就不参与升级，报了也没法处理。
pub fn estimate_upgrade(records: &[CachedAudioRecord], target_rank: i64) -> UpgradeEstimate {
    let mut count = 0usize;
    let mut unknown_rank_count = 0usize;
    let mut current_bytes = 0u64;
    let mut estimated_new_bytes = 0u64;
    let single_est = estimated_single_track_bytes(target_rank);

    for record in records {
        // 试听片段不计入未知档位的提示：它本来就不参与升级，报了也没法处理。
        if record.actual_rank.is_none() && !record.preview {
            unknown_rank_count += 1;
        }
        // 试听片段、超出意图上限、以及已确认无更高档位的记录都不计入升级。
        if record.is_upgradeable_to(target_rank) {
            count += 1;
            current_bytes = current_bytes.saturating_add(record.file_size);
            estimated_new_bytes = estimated_new_bytes.saturating_add(single_est);
        }
    }

    UpgradeEstimate {
        upgradeable_count: count,
        unknown_rank_count,
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

/// 每次写都用一个新的临时名：进程号 + 进程内序号。
///
/// 同一张封面有可能被两条路同时写（列表的请求池、歌词页的原图下载），
/// 共用一个 `.part` 名就会打架：一个刚把文件清空，另一个接着往后面写，
/// 最后换上去的就是一张对不上的图。
fn unique_tag() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}-{seq}", std::process::id())
}

/// 写缓存文件：先往同目录的临时文件里写，整份写好了才换到正式路径。
///
/// 换用的是 `std::fs::rename`，三个平台上它都会把已经存在的目标整份换掉
/// （Windows 上走 `MoveFileExW`），所以正式路径上永远不会出现写了一半的图：
/// 要么旧封面原样还在，要么整份新封面。
/// 换不动时（目标被别的程序占着、目录没权限）只删自己的临时文件，旧封面一动不动，
/// 下次重试还能用。
/// 临时名以 `.part` 结尾，跟下载中的音频一样受「24 小时内不淘汰」保护。
/// 函数自己会用 `download_lock` 把同一目标的写者排队，调用方别再持着这把锁调进来。
/// 调用方本来就在后台线程里跑，这里只做同步文件操作。
pub fn write_cache_file_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "缓存路径没带文件名")
        })?;
    // 临时文件必须和正式文件在同一个目录：跨盘 rename 就不是一步到位了。
    let parent = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let tmp = parent.join(format!("{name}.{}.part", unique_tag()));

    // 同一张封面的两路写者在这里排队，一个换完另一个再动。
    let lock = download_lock(target);
    let _writer = lock.lock().unwrap_or_else(|err| err.into_inner());

    if let Err(err) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
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

    /// #217：没超上限一个都不删，统计口径含原图子目录里的文件。
    #[test]
    fn trim_covers_below_limit_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let thumb = file(dir.path(), "covers/0000000000000001-160.img", 100);
        let original = file(dir.path(), "covers/original/0000000000000001.img", 100);
        let (remaining, removed) = trim_covers_in(dir.path(), 1000, &HashMap::new());
        assert_eq!(remaining, 20);
        assert!(removed.is_empty(), "没超限不该报任何删除");
        assert!(thumb.exists());
        assert!(original.exists());
    }

    /// #217：超限时按组内最后访问时间排序，最旧那组的缩略图和原图一起删，不剩半套。
    #[test]
    fn trim_covers_evicts_oldest_group_as_a_whole() {
        let dir = tempfile::tempdir().unwrap();
        let old_thumb = file(dir.path(), "covers/0000000000000001-160.img", 100);
        let old_original = file(dir.path(), "covers/original/0000000000000001.img", 100);
        let new_thumb = file(dir.path(), "covers/0000000000000002-160.img", 0);
        let new_original = file(dir.path(), "covers/original/0000000000000002.img", 0);

        let (remaining, removed) = trim_covers_in(dir.path(), 20, &HashMap::new());
        assert_eq!(remaining, 20);
        assert_eq!(removed.len(), 2, "整组两个文件都报出来");
        assert!(removed.contains(&old_thumb) && removed.contains(&old_original));
        assert!(!old_thumb.exists(), "最旧的缩略图要删");
        assert!(!old_original.exists(), "最旧的原图要一起删");
        assert!(
            new_thumb.exists() && new_original.exists(),
            "最近看过的组要留住"
        );
    }

    /// 最近显示过、但 mtime 很旧的封面，要排在从没显示过的较新封面之后，
    /// 整组（缩略图 + 原图）一起保住。
    #[test]
    fn trim_covers_prefers_last_shown_over_stale_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let shown_thumb = file(dir.path(), "covers/0000000000000001-160.img", 3600);
        let shown_original = file(dir.path(), "covers/original/0000000000000001.img", 3600);
        let never_shown = file(dir.path(), "covers/0000000000000002-160.img", 30);

        let mut last_seen = HashMap::new();
        last_seen.insert(
            shown_thumb.clone(),
            SystemTime::now() - Duration::from_secs(1),
        );

        let (remaining, removed) = trim_covers_in(dir.path(), 20, &last_seen);
        assert_eq!(remaining, 20);
        assert_eq!(removed, vec![never_shown.clone()], "只淘汰从没显示过的那组");
        assert!(
            shown_thumb.exists() && shown_original.exists(),
            "最近显示的组，缩略图和原图都要留"
        );
        assert!(!never_shown.exists());
    }

    /// 显示之后又重新下载过的封面，mtime 比显示时间新，排序键必须取较晚的 mtime。
    /// 若实现退化成「有 last_seen 就只看 last_seen」，这张会被当成最旧的一组删掉。
    #[test]
    fn trim_covers_keeps_redownloaded_cover_over_older_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let redownloaded = file(dir.path(), "covers/0000000000000001-160.img", 60);
        let stale = file(dir.path(), "covers/0000000000000002-160.img", 3600);

        let mut last_seen = HashMap::new();
        last_seen.insert(
            redownloaded.clone(),
            SystemTime::now() - Duration::from_secs(7200),
        );

        let (remaining, removed) = trim_covers_in(dir.path(), 10, &last_seen);
        assert_eq!(remaining, 10);
        assert_eq!(removed, vec![stale.clone()], "旧 mtime 的那组先淘汰");
        assert!(
            redownloaded.exists(),
            "重新下载后的 mtime 要能压过旧显示时间"
        );
        assert!(!stale.exists());
    }

    /// #217：24 小时内的临时文件计入总量但受保护；过期临时文件可以淘汰。
    #[test]
    fn trim_covers_protects_fresh_parts_and_evicts_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = file(dir.path(), "covers/0000000000000001-160.img.123-0.part", 0);
        let stale = file(
            dir.path(),
            "covers/0000000000000002-160.img.456-0.part",
            90_000,
        );
        let cover = file(dir.path(), "covers/0000000000000003-160.img", 50);

        assert_eq!(trim_covers_in(dir.path(), 10, &HashMap::new()).0, 10);
        assert!(fresh.exists(), "24 小时内的临时文件不能删");
        assert!(!stale.exists(), "过期临时文件可删");
        assert!(!cover.exists(), "普通封面按顺序删");
    }

    /// #217：删除失败的文件不减统计，也不挡着后面的组继续淘汰。
    #[test]
    fn trim_covers_delete_failure_does_not_count_or_stop() {
        let dir = tempfile::tempdir().unwrap();
        let old = file(dir.path(), "covers/0000000000000001-160.img", 100);
        let recent = file(dir.path(), "covers/0000000000000002-160.img", 0);
        let attempts = std::cell::Cell::new(0);

        let (remaining, removed) = trim_covers_with(dir.path(), 10, |path| {
            attempts.set(attempts.get() + 1);
            if path == old.as_path() {
                false // 模拟这个删不掉
            } else {
                std::fs::remove_file(path).is_ok()
            }
        });
        assert_eq!(remaining, 10, "删失败的那 10 字节不能算释放");
        assert_eq!(removed.len(), 1, "只有删成功的才报进删除列表");
        assert!(removed[0] == recent, "删失败的不能报进去");
        assert!(old.exists(), "删失败的文件还在");
        assert!(!recent.exists(), "不能因为一个删失败就中断后续淘汰");
        assert_eq!(attempts.get(), 2, "两组都要尝试过");
    }

    /// #217：目录不存在或为空都返回 0，不 panic。
    #[test]
    fn trim_covers_missing_or_empty_dir_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            trim_covers_in(&dir.path().join("missing"), 0, &HashMap::new()).0,
            0
        );
        assert_eq!(trim_covers_in(dir.path(), 0, &HashMap::new()).0, 0);
    }

    /// #225：「根目录不存在」是正常状态，不能算成扫描不完整，否则封面淘汰会永久停摆。
    #[test]
    fn scan_treats_missing_root_as_complete_and_empty() {
        let dir = tempfile::tempdir().unwrap();
        let scan = scan_files(&dir.path().join("missing"));
        assert!(scan.files.is_empty());
        assert!(!scan.incomplete, "目录还不存在不等于扫描失败");
    }

    /// 模拟「某个子目录读不到」的扫描：权限位在 root 下无效、Windows 上不生效，
    /// 所以直接替换目录读取，而不是去 chmod。
    fn scan_denying(root: &Path, denied: &Path) -> Scan {
        scan_with(root, |path| {
            if path == denied {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "denied",
                ))
            } else {
                list_dir(path)
            }
        })
    }

    /// 静默跑一轮封面淘汰：只关心删了什么、不关心日志的测试用它。
    fn trim_covers_with<F: FnMut(&Path) -> bool>(
        dir: &Path,
        limit_bytes: u64,
        remove: F,
    ) -> (u64, Vec<PathBuf>) {
        trim_covers_scan(
            scan_files(dir),
            limit_bytes,
            &HashMap::new(),
            &CoverAlertGate::default(),
            |_alert| {},
            remove,
        )
    }

    /// 子目录读不到、可读部分也没超限：本轮不删。
    ///
    /// 可读部分只有一个 10 字节的文件，上限远大于它，所以「超没超限」根本判断不了；
    /// 判定靠的是删除回调一次都没被调用，跟两边各多少字节无关。
    #[test]
    fn trim_covers_skips_eviction_when_scan_is_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let thumb = file(dir.path(), "covers/0000000000000001-160.img", 100);
        let hidden = file(dir.path(), "covers/original/0000000000000002.img", 500);
        let scan = scan_denying(dir.path(), &dir.path().join("covers/original"));
        assert!(scan.incomplete, "读不到的子目录要标记出来");
        assert_eq!(scan.total_bytes(), 10, "只统计到可读部分");

        let attempts = std::cell::Cell::new(0);
        let alerts = std::cell::RefCell::new(Vec::new());
        let (remaining, removed) = trim_covers_scan(
            scan,
            1000,
            &HashMap::new(),
            &CoverAlertGate::default(),
            |alert| alerts.borrow_mut().push(alert),
            |_path| {
                attempts.set(attempts.get() + 1);
                true
            },
        );

        assert_eq!(attempts.get(), 0, "下界没超限时不能凭残缺总量去删");
        assert!(removed.is_empty());
        assert_eq!(remaining, 10);
        assert_eq!(
            alerts.borrow().as_slice(),
            &[CoverTrimAlert {
                readable_bytes: 10,
                readable_files: 1,
                action: CoverTrimAction::Skip,
            }],
            "跳过这一轮要留痕，且字段写的是可读部分"
        );
        assert!(thumb.exists(), "可读的封面也不能删");
        assert!(hidden.exists(), "读不到的部分更没动过");
    }

    /// 子目录读不到、但可读部分自己就超限：照删不误，否则一条坏条目能把整轮淘汰冻住。
    #[test]
    fn trim_covers_still_evicts_when_readable_part_alone_overflows() {
        let dir = tempfile::tempdir().unwrap();
        let old = file(dir.path(), "covers/0000000000000001-160.img", 100);
        let recent = file(dir.path(), "covers/0000000000000002-160.img", 0);
        let hidden = file(dir.path(), "covers/original/0000000000000003.img", 500);
        let scan = scan_denying(dir.path(), &dir.path().join("covers/original"));
        assert!(scan.incomplete);

        // 可读 20 字节、上限 10：可读部分已经超限，真实占用必然也超限。
        let (remaining, removed) = trim_covers_scan(
            scan,
            10,
            &HashMap::new(),
            &CoverAlertGate::default(),
            |_alert| {},
            |path| std::fs::remove_file(path).is_ok(),
        );
        assert_eq!(remaining, 10);
        assert_eq!(removed, vec![old.clone()], "只删可读部分里最旧的那组");
        assert!(!old.exists());
        assert!(recent.exists(), "最近看过的要留住");
        assert!(hidden.exists(), "读不到的部分不动");
    }

    /// 「动作」只在变化时上报一次：一直不完整也不会每分钟写一条 Warn。
    #[test]
    fn cover_trim_alerts_once_per_action_change() {
        let dir = tempfile::tempdir().unwrap();
        let thumb = file(dir.path(), "covers/0000000000000001-160.img", 100);
        let hidden = file(dir.path(), "covers/original/0000000000000002.img", 500);
        let denied = dir.path().join("covers/original");
        let gate = CoverAlertGate::default();
        let alerts = std::cell::RefCell::new(Vec::new());

        // 可读 10 字节、上限 1000：判断不了，跳过；同一动作的第二轮不再上报。
        for _ in 0..2 {
            let scan = scan_denying(dir.path(), &denied);
            let _ = trim_covers_scan(
                scan,
                1000,
                &HashMap::new(),
                &gate,
                |alert| alerts.borrow_mut().push(alert),
                |_| false,
            );
        }
        assert_eq!(alerts.borrow().len(), 1, "同一个动作只报一次");

        // 上限降到 0：动作变成按可读部分淘汰，再报一次，并且这一轮确实删了。
        let scan = scan_denying(dir.path(), &denied);
        let (remaining, removed) = trim_covers_scan(
            scan,
            0,
            &HashMap::new(),
            &gate,
            |alert| alerts.borrow_mut().push(alert),
            |path| std::fs::remove_file(path).is_ok(),
        );
        let alerts = alerts.into_inner();
        assert_eq!(alerts.len(), 2, "动作变了要重新上报");
        assert_eq!(alerts[1].action, CoverTrimAction::EvictReadable);
        assert_eq!(alerts[1].readable_bytes, 10);
        assert_eq!(remaining, 0);
        assert_eq!(removed, vec![thumb.clone()]);
        assert!(!thumb.exists());
        assert!(hidden.exists(), "读不到的部分不动");
    }

    /// 复位接线要端到端验证：`admit(None)` 那行被删掉时，一次 skip 之后就永久哑火。
    #[test]
    fn cover_trim_realerts_after_a_complete_scan() {
        let dir = tempfile::tempdir().unwrap();
        file(dir.path(), "covers/0000000000000001-160.img", 100);
        file(dir.path(), "covers/original/0000000000000002.img", 500);
        let denied = dir.path().join("covers/original");
        let gate = CoverAlertGate::default();
        let alerts = std::cell::RefCell::new(Vec::new());

        // 1) 子目录读不到：跳过并报一次。
        let scan = scan_denying(dir.path(), &denied);
        let _ = trim_covers_scan(
            scan,
            1000,
            &HashMap::new(),
            &gate,
            |alert| alerts.borrow_mut().push(alert),
            |_| false,
        );
        assert_eq!(alerts.borrow().len(), 1);

        // 2) 完整扫一轮：不报，但这一轮必须把门复位。
        let scan = scan_files(dir.path());
        assert!(!scan.incomplete);
        let _ = trim_covers_scan(
            scan,
            1000,
            &HashMap::new(),
            &gate,
            |alert| alerts.borrow_mut().push(alert),
            |_| false,
        );
        assert_eq!(alerts.borrow().len(), 1, "扫描完整时不该报");

        // 3) 又读不到：门复位过了，必须重新报。
        let scan = scan_denying(dir.path(), &denied);
        let _ = trim_covers_scan(
            scan,
            1000,
            &HashMap::new(),
            &gate,
            |alert| alerts.borrow_mut().push(alert),
            |_| false,
        );
        assert_eq!(alerts.borrow().len(), 2, "恢复完整后再出问题要重新上报");
    }

    /// 扫描恢复完整后复位，下次再出问题要重新上报。
    #[test]
    fn cover_alert_gate_resets_after_a_complete_scan() {
        let gate = CoverAlertGate::default();
        assert!(gate.admit(Some(CoverTrimAction::Skip)));
        assert!(!gate.admit(Some(CoverTrimAction::Skip)), "同一动作只报一次");
        assert!(gate.admit(None), "恢复完整是一次状态变化");
        assert!(!gate.admit(None));
        assert!(
            gate.admit(Some(CoverTrimAction::Skip)),
            "再次出问题要重新上报"
        );
        assert!(
            gate.admit(Some(CoverTrimAction::EvictReadable)),
            "动作变了也要报"
        );
    }

    /// 音频遇到同样的扫描缺口时保持既有口径（按可读部分淘汰），
    /// 这条差异是刻意的：音频额度是用户设定、有租约保护，改它要单独评估。
    #[test]
    fn trim_audio_still_evicts_from_incomplete_scan() {
        let dir = tempfile::tempdir().unwrap();
        let old = file(dir.path(), "old.m4a", 100);
        file(dir.path(), "old.quality", 100);
        let recent = file(dir.path(), "recent.m4a", 0);
        file(dir.path(), "deep/unreadable.m4a", 100);

        let scan = scan_denying(dir.path(), &dir.path().join("deep"));
        assert!(scan.incomplete);
        // 可读部分共 30 字节，上限 20：淘汰最旧那组（音频 + sidecar 20 字节），停在 10。
        let staging = dir.path().join("staging");
        assert_eq!(trim_audio_scan(scan, 20, &staging), 10);
        assert!(!old.exists(), "音频照旧淘汰可读部分里最旧的");
        assert!(recent.exists());
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
        let parsed = parse_sidecar(sidecar, 1024).unwrap();
        assert_eq!(parsed.quality, "无损 871k");
        assert!(!parsed.preview);
        assert_eq!(parsed.actual_rank, Some(100));
        assert_eq!(parsed.checked_rank, None);
        assert_eq!(parsed.checked_at, None);
        assert_eq!(parsed.preview_attempt_at, None);

        // 试听标志为1
        let preview = parse_sidecar("标准 128k\t500\t1\t50", 500).unwrap();
        assert_eq!(
            preview.quality, "流畅 128k",
            "旧缓存里的「标准」按 50 分档洗名"
        );
        assert!(preview.preview);
        assert_eq!(preview.actual_rank, Some(50));

        // 第 7 字段是补整曲的尝试时间：认不出档位的 0 分试听也能被读出来。
        let attempted = parse_sidecar("未知 128k\t500\t1\t0\t\t\t1700000000", 500).unwrap();
        assert!(attempted.preview);
        assert_eq!(attempted.actual_rank, Some(0));
        assert_eq!(attempted.checked_rank, None);
        assert_eq!(attempted.preview_attempt_at, Some(1_700_000_000));
        assert_eq!(
            attempted.fresh_preview_attempt_at(1_700_000_000 + 60),
            Some(1_700_000_000)
        );
        assert_eq!(
            attempted.fresh_preview_attempt_at(1_700_000_000 + PREVIEW_RETRY_TTL.as_secs() + 1),
            None,
            "过了退避期就该再试一次"
        );

        // 尺寸不符返回None
        assert_eq!(parse_sidecar(sidecar, 999), None);

        // 旧3字段sidecar（缺档位分数）
        let legacy = parse_sidecar("极高 320k\t2000\t0", 2000).unwrap();
        assert_eq!(legacy.quality, "极高 320k");
        assert_eq!(legacy.actual_rank, None);
        assert_eq!(legacy.checked_rank, None);

        // 没有分数时认不出具体档位，只把撞名的「标准」换成 50 分档的新名字。
        let legacy_low = parse_sidecar("标准 96k\t2000\t0", 2000).unwrap();
        assert_eq!(legacy_low.quality, "流畅 96k");

        // 有分数就按分数洗：同一个旧「标准」在 50 分档叫流畅、在 20 分档叫省流。
        let low = parse_sidecar("标准 128k\t500\t0\t50", 500).unwrap();
        assert_eq!(low.quality, "流畅 128k");
        assert_eq!(low.actual_rank, Some(50));
        let saver = parse_sidecar("标准 96k\t500\t0\t20", 500).unwrap();
        assert_eq!(saver.quality, "省流 96k");
        // 旧「Hi-Res 无损」在 100 分档要降级写成「无损」，前缀按长度从长到短匹配。
        let hi_res = parse_sidecar("Hi-Res 无损 871k\t500\t0\t100", 500).unwrap();
        assert_eq!(hi_res.quality, "无损 871k");

        // 第 5、6 字段：已探测目标档位与探测时间
        let checked = parse_sidecar("极高 320k\t2000\t0\t80\t100\t1700000000", 2000);
        let checked = checked.unwrap();
        assert_eq!(checked.actual_rank, Some(80));
        assert_eq!(checked.checked_rank, Some(100));
        assert_eq!(checked.checked_at, Some(1_700_000_000));
    }

    /// 写一对缓存文件（音频 + sidecar）。sidecar 内容原样落盘。
    fn write_cached(dir: &Path, stem: &str, sidecar: &str, bytes: usize) {
        std::fs::write(dir.join(format!("{stem}.m4a")), vec![0u8; bytes]).unwrap();
        std::fs::write(dir.join(format!("{stem}.quality")), sidecar).unwrap();
    }

    /// #89：旧三字段缓存（没有档位分数）要计入批量升级，标签已证明达标的除外。
    #[test]
    fn legacy_three_field_sidecars_join_upgrade_estimate() {
        let dir = tempfile::tempdir().unwrap();
        // 档位未知，标签也证明不了达标：重下一次才知道它到底是什么档。
        write_cached(dir.path(), "7001-auto", "极高 320k\t3000\t0", 3000);
        // 档位未知，但旧标签写明无损：目标不超过无损时不值得再下一遍。
        write_cached(dir.path(), "7002-auto", "无损 871k\t9000\t0", 9000);
        // 档位未知，且已确认过无损目标没有更高流（第 4 字段留空）：同一目标不再计入。
        let fresh = now_epoch_secs();
        write_cached(
            dir.path(),
            "7003-auto",
            &format!("较高 256k\t4000\t0\t\t100\t{fresh}"),
            4000,
        );

        // 档位未知，且是试听片段：本来就不能参与升级，也不该报给用户。
        write_cached(dir.path(), "7004-auto", "极高 320k\t500\t1", 500);

        let records = list_cached_audio(dir.path());
        assert_eq!(records.len(), 4);
        let checked = records
            .iter()
            .find(|r| r.track_id == "7003")
            .expect("空第 4 字段的记录仍能读回");
        assert_eq!(
            checked.actual_rank, None,
            "第 4 字段留空读回来仍是档位未知，不是 0 档"
        );
        assert_eq!(checked.checked_rank, Some(100));

        // 目标无损（100）：只有证明不了达标、也没确认过的那首进入升级。
        let est = estimate_upgrade(&records, 100);
        assert_eq!(est.upgradeable_count, 1);
        assert_eq!(
            est.unknown_rank_count, 3,
            "发现的未知档位与本轮能不能升无关（试听那段不报）"
        );
        assert_eq!(est.current_bytes, 3000);

        // 目标 Hi-Res 无损（110）：无损标签只证明到 100，三首都要重新探。
        let est_hires = estimate_upgrade(&records, 110);
        assert_eq!(est_hires.upgradeable_count, 3);
        assert_eq!(est_hires.unknown_rank_count, 3);
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
        assert!(song1.needs_higher_stream(100));
        assert!(!song1.needs_higher_stream(50));

        let song2 = records.iter().find(|r| r.track_id == "song2").unwrap();
        assert_eq!(song2.actual_rank, Some(100));
        assert_eq!(song2.actual_tier(), Some("无损"));
        assert!(!song2.needs_higher_stream(100));

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

    /// 编码必须单射：两个不同 id 压成同一个文件名，两首歌就共用一份缓存，
    /// 播第二首会直接命中第一首的音频。
    #[test]
    fn track_id_encoding_keeps_distinct_ids_apart() {
        assert_ne!(encode_track_id("a/"), encode_track_id("a_2f"));
        assert_ne!(encode_track_id("a/b"), encode_track_id("a_b"));
        assert_ne!(
            encode_track_id("../../etc/passwd"),
            encode_track_id("______etc_passwd")
        );
        assert_eq!(encode_track_id("123"), "123", "数字 id 原样保留");
        let dashed = encode_track_id("a-b");
        assert!(!dashed.contains('-'), "编码后不能含分隔符: {dashed}");
    }

    /// 编码要能反解：文件名读回来的 id 必须还是原来那个。解不回来，批量升级拿它
    /// 拼出来的路径就找不到缓存，这首歌每轮白跑。
    #[test]
    fn track_id_encoding_round_trips() {
        for id in [
            "123",
            "7427204361343191057",
            "a_b",
            "a/",
            "a_2f",
            "a-b",
            "曲目",
        ] {
            let encoded = encode_track_id(id);
            assert_eq!(decode_track_id(&encoded), id, "{id} 编码后反解不回原值");
        }
    }

    /// 走一遍真实链路：写下去的文件名再解析回来，id 和档位都得对。
    /// 这里特意用非数字 id，也就是以前会 miss 的那条路。
    #[test]
    fn cache_stem_round_trips_original_track_id() {
        for id in ["a_b", "a/", "a_2f", "7427204361343191057"] {
            let stem = format!("{}-hi-res", encode_track_id(id));
            assert_eq!(
                parse_audio_cache_stem(&stem),
                Some((id.to_string(), "hi-res".to_string())),
                "{stem}"
            );
        }
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
                checked_rank: None,
                file_size: 4 * 1024 * 1024,
            },
            CachedAudioRecord {
                track_id: "2".into(),
                quality_tag: "auto".into(),
                path: PathBuf::from("2-auto.m4a"),
                quality_label: "试听 128k".into(),
                preview: true,
                actual_rank: Some(50),
                checked_rank: None,
                file_size: 1024 * 1024,
            },
            // #130 的两种试听：认得出档位与认不出（0 分）都不进批量升级。
            CachedAudioRecord {
                track_id: "3".into(),
                quality_tag: "auto".into(),
                path: PathBuf::from("3-auto.m4a"),
                quality_label: "未知 128k".into(),
                preview: true,
                actual_rank: Some(0),
                checked_rank: None,
                file_size: 1024 * 1024,
            },
            // 旧格式试听没有档位分数：同样不能报成「档位未知的旧缓存」，
            // 它本来就不参与批量升级，报了也没法处理。
            CachedAudioRecord {
                track_id: "4".into(),
                quality_tag: "auto".into(),
                path: PathBuf::from("4-auto.m4a"),
                quality_label: "试听 128k".into(),
                preview: true,
                actual_rank: None,
                checked_rank: None,
                file_size: 1024 * 1024,
            },
        ];
        let est = estimate_upgrade(&records, 100);
        // preview 试听文件不应被计入升级，也不该被报成「档位未知的旧缓存」。
        assert_eq!(est.upgradeable_count, 1);
        assert_eq!(est.unknown_rank_count, 0);
        assert_eq!(est.current_bytes, 4 * 1024 * 1024);
        assert!(!records[1].is_upgradeable_to(100));
        assert!(!records[2].is_upgradeable_to(100));
        assert!(!records[3].is_upgradeable_to(100));
    }

    /// #96：sidecar 记了 0 分（认不出档位）的缓存不计入批量升级——重下拿回的还是同一条
    /// 认不出的流，白跑一遍流量。同一批里没记分数的那首仍按 #89 参与升级。
    #[test]
    fn estimate_upgrade_skips_unreadable_zero_rank() {
        let record = |id: &str, label: &str, rank: Option<i64>| CachedAudioRecord {
            track_id: id.into(),
            quality_tag: "auto".into(),
            path: PathBuf::from(format!("{id}-auto.m4a")),
            quality_label: label.into(),
            preview: false,
            actual_rank: rank,
            checked_rank: None,
            file_size: 4 * 1024 * 1024,
        };
        let unreadable = record("1", "未知 128k", Some(0));
        assert_eq!(unreadable.actual_tier(), Some("未知"));
        assert!(!unreadable.needs_higher_stream(100));
        assert!(!unreadable.is_upgradeable_to(100));

        let records = vec![
            unreadable,
            record("2", "极高 320k", None),
            record("3", "流畅 128k", Some(50)),
        ];
        let est = estimate_upgrade(&records, 100);
        assert_eq!(est.upgradeable_count, 2, "0 分那首不进批量升级");
        assert_eq!(est.current_bytes, 8 * 1024 * 1024);
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
            checked_rank: None,
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
            checked_rank: None,
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
            checked_rank: None,
            file_size: 10 * 1024 * 1024,
        };
        // lossless 标签上限为 100，实际为 50（曾发生降档），允许升级至 100
        assert!(lossless_downgraded.is_upgradeable_to(100));
    }

    /// #90：确认过「没有更高档位」的目标不再计入升级与预估，但更高目标仍可探测。
    #[test]
    fn checked_target_is_skipped_until_the_target_rises() {
        let confirmed = CachedAudioRecord {
            track_id: "1".into(),
            quality_tag: "auto".into(),
            path: PathBuf::from("1-auto.m4a"),
            quality_label: "极高 320k".into(),
            preview: false,
            actual_rank: Some(80),
            checked_rank: Some(100),
            file_size: 10 * 1024 * 1024,
        };
        // 已经问过无损、确认没有更高流：同一个目标不再计入
        assert!(!confirmed.is_upgradeable_to(100));
        // 更高目标还没问过，仍要允许探测：历史结论不能代表更高档位
        assert!(confirmed.is_upgradeable_to(110));

        let low_confirmed = CachedAudioRecord {
            actual_rank: Some(50),
            checked_rank: Some(70),
            ..confirmed.clone()
        };
        // 只确认过 70 档：再问 70 没有意义，问 80 仍应尝试
        assert!(!low_confirmed.is_upgradeable_to(70));
        assert!(low_confirmed.is_upgradeable_to(80));

        let estimate = estimate_upgrade(&[confirmed], 100);
        assert_eq!(estimate.upgradeable_count, 0);

        assert!(upgrade_check_covers(Some(100), Some(100)));
        assert!(!upgrade_check_covers(Some(70), Some(80)));
        assert!(!upgrade_check_covers(None, Some(80)));
        assert!(!upgrade_check_covers(Some(80), None));
    }

    /// 探测结论只在 30 天内有效，过期后重新探测。
    #[test]
    fn checked_rank_expires_after_ttl() {
        let meta = SidecarMeta {
            quality: "极高 320k".into(),
            preview: false,
            actual_rank: Some(80),
            checked_rank: Some(100),
            checked_at: Some(1_700_000_000),
            preview_attempt_at: None,
        };
        assert_eq!(meta.fresh_checked_rank(1_700_000_001), Some(100));
        let edge = 1_700_000_000 + UPGRADE_CHECK_TTL.as_secs() - 1;
        assert_eq!(meta.fresh_checked_rank(edge), Some(100));
        let expired = 1_700_000_000 + UPGRADE_CHECK_TTL.as_secs();
        assert_eq!(meta.fresh_checked_rank(expired), None);
        // 时钟回拨时无法判断新鲜度，按过期处理：多探一次比永久不探安全。
        assert_eq!(meta.fresh_checked_rank(1_700_000_000 - 1), None);
        // 缺时间戳的记录不参与跳过。
        let no_time = SidecarMeta {
            checked_at: None,
            ..meta
        };
        assert_eq!(no_time.fresh_checked_rank(1_700_000_001), None);
    }

    /// #130：补整曲的退避只对试听生效，过期、时钟回拨或整曲记录都不挡路。
    #[test]
    fn preview_retry_backoff_expires_and_only_applies_to_previews() {
        let meta = SidecarMeta {
            quality: "未知 128k".into(),
            preview: true,
            actual_rank: Some(0),
            checked_rank: None,
            checked_at: None,
            preview_attempt_at: Some(1_700_000_000),
        };
        assert_eq!(
            meta.fresh_preview_attempt_at(1_700_000_001),
            Some(1_700_000_000)
        );
        let expired = 1_700_000_000 + PREVIEW_RETRY_TTL.as_secs();
        assert_eq!(meta.fresh_preview_attempt_at(expired), None);
        assert_eq!(meta.fresh_preview_attempt_at(1_700_000_000 - 1), None);

        let full_track = SidecarMeta {
            preview: false,
            ..meta
        };
        assert_eq!(
            full_track.fresh_preview_attempt_at(1_700_000_001),
            None,
            "整曲不需要补整曲退避"
        );
    }

    /// 写回探测结论时保留原来的前四个字段，且不覆盖已被替换的文件。
    #[test]
    fn record_upgrade_check_preserves_fields_and_skips_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("song-auto.quality");
        std::fs::write(&sidecar, "极高 320k\t2000\t0\t80").unwrap();
        record_upgrade_check(&sidecar, Some(80), 100, 1_700_000_000).unwrap();
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(text, "极高 320k\t2000\t0\t80\t100\t1700000000");
        let parsed = parse_sidecar(&text, 2000).unwrap();
        assert_eq!(parsed.actual_rank, Some(80));
        assert_eq!(parsed.checked_rank, Some(100));
        assert_eq!(parsed.checked_at, Some(1_700_000_000));

        // 文件已经被别的写者换成另一档：旧结论不能盖到新文件上。
        record_upgrade_check(&sidecar, Some(50), 110, 1_700_000_001).unwrap();
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), text);

        // 换一个更高目标重探后，记录被替换而不是叠加。
        record_upgrade_check(&sidecar, Some(80), 110, 1_700_000_002).unwrap();
        let updated = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(updated, "极高 320k\t2000\t0\t80\t110\t1700000002");

        // 旧三字段（无第 4 字段）：档位未知也能记下结论，第 4 字段留空位。
        std::fs::write(&sidecar, "极高 320k\t2000\t0").unwrap();
        record_upgrade_check(&sidecar, None, 100, 1_700_000_003).unwrap();
        let legacy = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(legacy, "极高 320k\t2000\t0\t\t100\t1700000003");
        let parsed = parse_sidecar(&legacy, 2000).unwrap();
        assert_eq!(parsed.actual_rank, None, "第 4 字段留空读回来仍是档位未知");
        assert_eq!(parsed.checked_rank, Some(100));
        assert_eq!(parsed.checked_at, Some(1_700_000_003));

        // 档位未知不能盖到有分数的记录上，反之亦然。
        std::fs::write(&sidecar, "极高 320k\t2000\t0\t80").unwrap();
        record_upgrade_check(&sidecar, None, 100, 1_700_000_004).unwrap();
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            "极高 320k\t2000\t0\t80",
            "已知分数的记录不被未知身份的结论覆盖"
        );
        std::fs::write(&sidecar, "极高 320k\t2000\t0").unwrap();
        record_upgrade_check(&sidecar, Some(80), 100, 1_700_000_005).unwrap();
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            "极高 320k\t2000\t0",
            "档位未知的记录不被声称知道分数的结论覆盖"
        );
    }

    /// 写回失败或字段不完整时只报错，不碰原 sidecar。
    #[test]
    fn record_upgrade_check_errors_without_touching_existing_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.quality");
        assert!(record_upgrade_check(&missing, Some(80), 100, 1).is_err());

        let short = dir.path().join("short.quality");
        std::fs::write(&short, "x\ty").unwrap();
        assert!(record_upgrade_check(&short, Some(80), 100, 1).is_err());
        assert_eq!(std::fs::read_to_string(&short).unwrap(), "x\ty");

        // 只剩一个字段：连尺寸都没有，不能改写。
        let one = dir.path().join("one.quality");
        std::fs::write(&one, "极高 320k").unwrap();
        assert!(record_upgrade_check(&one, None, 100, 1).is_err());
        assert_eq!(std::fs::read_to_string(&one).unwrap(), "极高 320k");
    }

    /// 更早的两字段格式（试听拼在档位里）补得齐，改写完仍能按现在的布局读回来。
    #[test]
    fn record_upgrade_check_fills_the_missing_preview_field() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("song-auto.quality");
        std::fs::write(&sidecar, "无损 · 试听\t9000").unwrap();
        record_upgrade_check(&sidecar, None, 100, 1_700_000_000).unwrap();
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(
            text, "无损\t9000\t1\t\t100\t1700000000",
            "试听标记换成第 3 字段"
        );
        let parsed = parse_sidecar(&text, 9000).unwrap();
        assert!(parsed.preview);
        assert_eq!(parsed.quality, "无损", "档位文本不再拖着试听标记");
        assert_eq!(parsed.actual_rank, None);
        assert_eq!(parsed.checked_rank, Some(100));
    }

    /// #130：补整曲的尝试只写时间，不写「没有更高档位」，也不碰第 5、6 字段。
    #[test]
    fn record_preview_attempt_writes_backoff_without_forging_checked_rank() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("song-auto.quality");
        std::fs::write(&sidecar, "未知 128k\t9000\t1\t0").unwrap();

        assert!(record_preview_attempt(&sidecar, Some(0), 1_700_000_000).unwrap());
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(text, "未知 128k\t9000\t1\t0\t\t\t1700000000");
        let parsed = parse_sidecar(&text, 9000).unwrap();
        assert!(parsed.preview);
        assert_eq!(parsed.actual_rank, Some(0));
        assert_eq!(parsed.checked_rank, None, "试听不能冒充「无更高档位」");
        assert_eq!(parsed.checked_at, None);
        assert_eq!(
            parsed.fresh_preview_attempt_at(1_700_000_000 + 60),
            Some(1_700_000_000)
        );

        // 文件已经被替换成整曲时，旧尝试记录不能落到新文件上。
        std::fs::write(&sidecar, "无损 871k\t9000\t0\t100").unwrap();
        assert!(!record_preview_attempt(&sidecar, Some(0), 1_700_000_100).unwrap());
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            "无损 871k\t9000\t0\t100"
        );

        // 档位分数变了（换成了另一条试听）同样跳过，避免给新流贴上旧退避。
        std::fs::write(&sidecar, "流畅 128k\t9000\t1\t50").unwrap();
        assert!(!record_preview_attempt(&sidecar, Some(0), 1_700_000_200).unwrap());
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            "流畅 128k\t9000\t1\t50"
        );
    }

    /// 试听拼接在档位里的旧格式也能补出试听标志并写入退避。
    #[test]
    fn record_preview_attempt_fills_the_missing_preview_field() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("song-auto.quality");
        std::fs::write(&sidecar, "无损 · 试听\t9000").unwrap();
        assert!(record_preview_attempt(&sidecar, None, 1_700_000_000).unwrap());
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(text, "无损\t9000\t1\t\t\t\t1700000000");
        let parsed = parse_sidecar(&text, 9000).unwrap();
        assert!(parsed.preview);
        assert_eq!(parsed.preview_attempt_at, Some(1_700_000_000));
    }

    /// 写「没有更高档位」时不能顺手抹掉第 7 字段的补整曲退避时间。
    #[test]
    fn record_upgrade_check_preserves_preview_attempt_time() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("song-auto.quality");
        std::fs::write(&sidecar, "未知 128k\t2000\t1\t0\t\t\t1700000000").unwrap();
        record_upgrade_check(&sidecar, Some(0), 100, 1_700_000_100).unwrap();
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(text, "未知 128k\t2000\t1\t0\t100\t1700000100\t1700000000");
        let parsed = parse_sidecar(&text, 2000).unwrap();
        assert_eq!(parsed.checked_rank, Some(100));
        assert_eq!(parsed.preview_attempt_at, Some(1_700_000_000));
    }

    /// 写回成功时清掉可能遗留的旧备份，不留下临时文件。
    #[test]
    fn record_upgrade_check_cleans_stale_backup() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("song-auto.quality");
        let backup = dir.path().join("song-auto.quality.old");
        std::fs::write(&sidecar, "极高 320k\t2000\t0\t80").unwrap();
        std::fs::write(&backup, "stale").unwrap();
        record_upgrade_check(&sidecar, Some(80), 100, 1_700_000_000).unwrap();
        assert!(!backup.exists());
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(text, "极高 320k\t2000\t0\t80\t100\t1700000000");
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

    /// 目录里除了点名的正式文件之外还剩什么：用来查临时文件和备份有没有留下。
    fn stray_files(dir: &Path, keep: &[&str]) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !keep.iter().any(|kept| kept == name))
            .collect();
        names.sort();
        names
    }

    /// #216：本地还没有这张封面时，一次写成，写的就是刚拿到的字节。
    #[test]
    fn atomic_write_creates_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let covers = dir.path().join("covers");
        let target = covers.join("0000abcd-160.img");
        std::fs::create_dir_all(&covers).unwrap();

        write_cache_file_atomic(&target, b"cover-bytes").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"cover-bytes");
        assert!(
            stray_files(&covers, &["0000abcd-160.img"]).is_empty(),
            "写完整份就该只剩正式文件"
        );
    }

    /// #216：已经有旧封面时整份换掉，不能还留着上一版的长尾巴。
    #[test]
    fn atomic_write_replaces_existing_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("0000abcd-160.img");
        std::fs::write(&target, vec![b'o'; 4096]).unwrap();

        write_cache_file_atomic(&target, b"new").unwrap();

        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"new",
            "旧内容要整份换掉，不能只把开头覆盖掉"
        );
        assert!(stray_files(dir.path(), &["0000abcd-160.img"]).is_empty());
    }

    /// #216：换不上正式文件时，目标原地不动，临时文件也不留下。
    ///
    /// 用一个占着目标路径的目录来制造「换不动」：`rename` 到一个已有目录上会失败，
    /// 和生产上「目标文件被别的程序占着换不动」是同一类结果。
    #[test]
    fn atomic_write_failure_keeps_target_and_drops_temp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("0000abcd-160.img");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"old-cover").unwrap();

        assert!(write_cache_file_atomic(&target, b"new-cover").is_err());

        assert_eq!(
            std::fs::read(target.join("keep")).unwrap(),
            b"old-cover",
            "换不动就一点都别动，老封面不能被弄丢"
        );
        assert!(
            stray_files(dir.path(), &["0000abcd-160.img"]).is_empty(),
            "临时文件要清理掉"
        );
    }

    /// 目录不在时直接报错，不 panic，也不留下半个文件。
    #[test]
    fn atomic_write_missing_dir_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nope").join("0000abcd-160.img");
        assert!(write_cache_file_atomic(&target, b"x").is_err());
        assert!(!target.exists());
    }

    /// 同一个缓存目标的文件共享保护键：租约挂在暂存目标上时，
    /// 它的全部暂存尾巴（.part / .part.lock / .part.meta / .part.meta.tmp / .dec.tmp）
    /// 不会被维护线程当废弃文件删掉。
    #[test]
    fn audio_staging_files_share_the_target_lease() {
        let target = Path::new("/cache/audio/staging/123-lossless-normal.m4a");
        for staged in [
            "/cache/audio/staging/123-lossless-normal.m4a.part",
            "/cache/audio/staging/123-lossless-normal.m4a.part.lock",
            "/cache/audio/staging/123-lossless-normal.m4a.part.meta",
            "/cache/audio/staging/123-lossless-normal.m4a.part.meta.tmp",
            "/cache/audio/staging/123-lossless-normal.m4a.part.dec.tmp",
            "/cache/audio/staging/123-lossless-normal.m4a.dec.tmp",
        ] {
            assert_eq!(protection_key(Path::new(staged)), target, "{staged}");
        }
        // .quality 边车仍映射到它自己的正式文件。
        assert_eq!(
            protection_key(Path::new("/cache/audio/123-lossless.quality")),
            Path::new("/cache/audio/123-lossless.m4a")
        );
        // 封面那类 `.img.*.part` 不在映射范围内，保持原样，别误套音频租约。
        assert_eq!(
            protection_key(Path::new("/cache/audio/abcd-160.img.123.part")),
            Path::new("/cache/audio/abcd-160.img.123.part")
        );
    }

    /// 临时名每次都不一样：同一张封面的两个写者不会写到同一个文件里。
    /// 名字以 `.part` 结尾，所以写了一半的临时文件也享有「24 小时内不淘汰」的保护。
    #[test]
    fn atomic_write_temp_names_are_unique_and_protected() {
        let first = unique_tag();
        let second = unique_tag();
        assert_ne!(first, second, "同一张封面两次写入的临时名必须不同");
        let name = format!("0000abcd-160.img.{first}.part");
        assert!(
            is_fresh_part(Path::new(&name), SystemTime::now()),
            "写了一半的封面不能被维护任务顺手删掉"
        );
    }

    /// 保护键的映射必须与 [`STAGING_TAILS`] 同源：名单里每条尾巴挂在交付目标
    /// `.m4a` 后面都能折回交付目标。加尾巴只改数组，这条测试守住保护键不漏。
    #[test]
    fn protection_key_maps_every_staging_tail_to_the_target() {
        let target = Path::new("/cache/audio/staging/123-lossless-normal.m4a");
        for tail in STAGING_TAILS {
            let staged = format!("/cache/audio/staging/123-lossless-normal.m4a{tail}");
            assert_eq!(protection_key(Path::new(&staged)), target, "{staged}");
        }
        // 挂在别处（封面那类 `.img.*.part`）不进映射，保持原样。
        assert_eq!(
            protection_key(Path::new("/cache/audio/abcd-160.img.123.part")),
            Path::new("/cache/audio/abcd-160.img.123.part")
        );
    }

    /// 暂存子目录里的文件名要能映射回正式缓存名：所有暂存尾巴都剥得掉，
    /// 名字对不上我们命名规则的一律 None。
    #[test]
    fn staging_committed_name_maps_all_residue_suffixes() {
        for residue in [
            "100-old-auto-normal.m4a",
            "100-old-auto-normal.m4a.part",
            "100-old-auto-normal.m4a.part.lock",
            "100-old-auto-normal.m4a.part.meta",
            "100-old-auto-normal.m4a.part.meta.tmp",
            "100-old-auto-normal.m4a.dec.tmp",
            "100-old-auto-upgrade.m4a.part.lock",
        ] {
            assert_eq!(
                staging_committed_name(residue),
                Some("100-old-auto.m4a".to_string()),
                "{residue}"
            );
        }
        // 档位标签可以带连字符，剥掉的必须是末尾的用途层。
        assert_eq!(
            staging_committed_name("7427204361343191057-hi-res-normal.m4a.part.lock"),
            Some("7427204361343191057-hi-res.m4a".to_string())
        );
        // 标签自己叫 foo-normal 时，交付名是两层 -normal，剥掉最后一层正好回到正式名。
        assert_eq!(
            staging_committed_name("x-foo-normal-normal.m4a"),
            Some("x-foo-normal.m4a".to_string())
        );
        // 不是我们命名的暂存：没有用途尾巴、剥完不是 .m4a，都不许碰。
        assert_eq!(staging_committed_name("x-auto.m4a"), None, "正式名不映射");
        assert_eq!(staging_committed_name("x-auto.m4a.part"), None);
        assert_eq!(staging_committed_name("x-auto.m4a.old"), None);
        assert_eq!(staging_committed_name("notes.txt"), None);
        // 历史残留（无用途后缀）只归启动巡检管，映射不出正式名。
        assert_eq!(staging_committed_name("222-low-4242-7.m4a.part.part"), None);
    }

    /// #242：启动巡检按「后缀白名单 + 24 小时 + 租约」清暂存残留。
    /// 过期的各种后缀（含历史 `.part.part` / `.part.part.lock`）全清；
    /// 正式缓存、sidecar、不相干文件、24 小时内的残留、暂存里的交付目标一律保留。
    #[test]
    fn sweep_removes_only_stale_whitelisted_residue() {
        let dir = tempfile::tempdir().unwrap();
        // 过期残留：五种后缀各一个，外加历史格式的孤儿数据和锁、新暂存子目录里的锁。
        let stale = [
            "111-auto.m4a.part",
            "111-auto.m4a.part.lock",
            "111-auto.m4a.part.meta",
            "111-auto.m4a.part.meta.tmp",
            "111-auto.m4a.dec.tmp",
            "222-low-4242-7.m4a.part.part",
            "222-low-4242-7.m4a.part.part.lock",
            "staging/333-auto-normal.m4a.part.lock",
        ];
        for name in stale {
            file(dir.path(), name, 90_000);
        }
        // 不能碰的：正式缓存与 sidecar（过期也留着）、封面式名字、不相干文件、
        // 暂存里的交付目标（不在白名单，由 stage_path 与淘汰收尾负责），
        // 以及 24 小时内的残留。
        let keep = [
            "111-auto.m4a",
            "111-auto.quality",
            "abcd-160.img",
            "notes.txt",
            "staging/444-auto-upgrade.m4a",
            "555-auto.m4a.part.lock",
            "staging/666-auto-normal.m4a.part",
        ];
        for name in keep {
            // 暂存尾巴的文件用新鲜时间（1 小时）：年龄门槛挡住的正是它们。
            let age = if name.ends_with(".part.lock") || name.ends_with(".part") {
                3_600
            } else {
                90_000
            };
            file(dir.path(), name, age);
        }
        assert_eq!(sweep_stale_staging_in(dir.path()), stale.len());
        for name in stale {
            assert!(!dir.path().join(name).exists(), "{name} 该清掉");
        }
        for name in keep {
            assert!(dir.path().join(name).exists(), "{name} 不能被巡检误删");
        }
    }

    /// #242：巡检的租约保护。租约挂在交付目标上（与 stage_path 同一处）时，
    /// 同一组暂存的所有尾巴过期也不删；租约放开后下一轮清走。
    #[test]
    fn sweep_respects_download_leases() {
        let dir = tempfile::tempdir().unwrap();
        let stage = staging_dir(dir.path());
        let part = file(&stage, "777-auto-normal.m4a.part", 90_000);
        let lock = file(&stage, "777-auto-normal.m4a.part.lock", 90_000);
        let meta = file(&stage, "777-auto-normal.m4a.part.meta", 90_000);
        let dec_tmp = file(&stage, "777-auto-normal.m4a.dec.tmp", 90_000);
        let lease = CacheLease::acquire(&stage.join("777-auto-normal.m4a"));

        assert_eq!(sweep_stale_staging_in(dir.path()), 0);
        assert!(
            part.exists() && lock.exists() && meta.exists() && dec_tmp.exists(),
            "正在下载的暂存不能被清"
        );

        drop(lease);
        assert_eq!(sweep_stale_staging_in(dir.path()), 4);
        assert!(!part.exists() && !lock.exists());
        assert!(!meta.exists() && !dec_tmp.exists());
    }

    /// #242：超额淘汰正式音频时，该曲目在暂存子目录的整组残余同批消失，
    /// 包括 24 小时内、本来不参与淘汰的 `.part`（租约挡得住的除外）；
    /// 别的曲目、别的用途的暂存一概不沾。
    #[test]
    fn evicted_audio_takes_its_staging_group_along() {
        let dir = tempfile::tempdir().unwrap();
        let old = file(dir.path(), "100-old-auto.m4a", 90_000);
        file(dir.path(), "100-old-auto.quality", 90_000);
        let recent = file(dir.path(), "200-new-auto.m4a", 0);
        file(dir.path(), "200-new-auto.quality", 0);
        let stage = staging_dir(dir.path());
        // 被淘汰那首的整组暂存：过期的交付目标、数据、锁、元数据，另一用途的锁，
        // 以及 24 小时内不参与淘汰的 `.part` 和新锁——它们只能靠淘汰收尾带走。
        let gone_with_it = [
            file(&stage, "100-old-auto-normal.m4a", 90_000),
            file(&stage, "100-old-auto-normal.m4a.part", 90_000),
            file(&stage, "100-old-auto-normal.m4a.part.lock", 90_000),
            file(&stage, "100-old-auto-normal.m4a.part.meta", 90_000),
            file(&stage, "100-old-auto-upgrade.m4a.part.lock", 90_000),
            file(&stage, "100-old-auto-upgrade.m4a.part", 3_600),
            file(&stage, "100-old-auto-upgrade.m4a.part.lock", 3_600),
        ];
        // 别的曲目的暂存：正式文件没被淘汰，它的锁必须留着。
        let others = file(&stage, "300-other-auto-normal.m4a.part.lock", 0);

        // 全部文件各 10 字节、共 120。过期组只有两组（正式对 20、交付目标+part+锁+
        // 元数据 40），上限 60 让淘汰正好停在过期组收完的那一刻；升级锁过期和新锁
        // 同组，组的使用时间取较新的，落进新鲜段不动，只能靠收尾按名字带走。
        // 新鲜的升级 `.part` 不参与分组淘汰，同样靠收尾。收尾共带走 30 字节，剩 30。
        assert_eq!(trim_audio_in(dir.path(), 60), 30);
        assert!(!old.exists() && !old.with_extension("quality").exists());
        for path in gone_with_it {
            assert!(!path.exists(), "{} 该跟着正式文件一起走", path.display());
        }
        assert!(recent.exists() && recent.with_extension("quality").exists());
        assert!(others.exists(), "别的曲目的暂存不属于这批淘汰，不能顺手删");
    }

    /// #242：淘汰时正在进行的下载仍受租约保护——正式文件可以被淘汰，
    /// 它名下的暂存要留到下载收尾（完成即交付、失败下轮再清）。
    #[test]
    fn eviction_spares_staging_of_running_download() {
        let dir = tempfile::tempdir().unwrap();
        let committed = file(dir.path(), "500-old-auto.m4a", 90_000);
        file(dir.path(), "500-old-auto.quality", 90_000);
        let stage = staging_dir(dir.path());
        let part = file(&stage, "500-old-auto-normal.m4a.part", 90_000);
        let lock = file(&stage, "500-old-auto-normal.m4a.part.lock", 90_000);
        let meta = file(&stage, "500-old-auto-normal.m4a.part.meta", 90_000);
        let dec_tmp = file(&stage, "500-old-auto-normal.m4a.dec.tmp", 90_000);
        // 与 stage_path 一致：租约挂在交付目标上。meta 与 dec.tmp 没有自己的租约，
        // 也必须跟着交付目标走，否则下载进行中被收尾删掉，断点续传信息就毁了。
        let lease = CacheLease::acquire(&stage.join("500-old-auto-normal.m4a"));

        // 正式文件 + sidecar（20 字节）被淘汰；租约挡着的 40 字节暂存留在统计里。
        assert_eq!(trim_audio_in(dir.path(), 0), 40);
        assert!(!committed.exists(), "正式缓存没租约，照常淘汰");
        assert!(
            part.exists() && lock.exists() && meta.exists() && dec_tmp.exists(),
            "正在下载的暂存不能被淘汰收尾删掉"
        );

        drop(lease);
        assert_eq!(trim_audio_in(dir.path(), 0), 0);
        assert!(!part.exists() && !lock.exists());
        assert!(!meta.exists() && !dec_tmp.exists());
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
