//! 磁盘缓存维护。所有扫描/删除由后台调用；租约保护下载、待装载和正在播放的音频。

use std::collections::{hash_map, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::log_outlet::{log_event, LogLevel, LogTopic};

pub const DEFAULT_AUDIO_LIMIT_MB: u64 = 10 * 1024;
/// 下载临时文件（含可续传暂存族）的保留窗口：窗口内不参与淘汰。
/// 失败留下的暂存至少能在这段时间内被下一次播放接上（#243）。
const PART_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// 探测记录的有效期：记的是「用当时账号向服务端要过这个档位、没拿到更高的流」。
///
/// 记录不是曲库永久结论：补档、换号或会员权益变化都不会立刻作废，30 天后重新问一次，
/// 避免每轮批量都整首重下一遍。杜比 / Hi-Res 也走这一条：账号只报一个 VIP 布尔，
/// 分不清它们更高的权益等级，高档位记下的「没拿到更高流」可能只是当时权益不够。
pub const UPGRADE_CHECK_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// 档位未知时的探测退避有效期。
pub const UNKNOWN_PROBE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// 一次完整流探测的独立退避记录；不代表缓存文件的实际档位已测出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownProbe {
    /// 这次探测请求的目标档位。
    pub target_rank: i64,
    /// 探测时间（Unix 秒）。
    pub at: u64,
    /// 账号与会员状态的稳定范围指纹。
    pub scope: String,
}

impl UnknownProbe {
    /// 只抑制同目标、同范围的下一次探测；范围缺失时返回 `false`，宁可重试也不跨账号误挡。
    pub fn covers(&self, target_rank: i64, scope: Option<&str>) -> bool {
        scope.is_some_and(|scope| self.target_rank == target_rank && self.scope == scope)
    }
}

/// 账号与会员状态的稳定本地区分指纹；不落原始账号 ID 或 Cookie，但可被枚举，不是匿名化或安全哈希。
pub fn unknown_probe_scope(user_id: Option<&str>, is_vip: Option<bool>) -> Option<String> {
    let user_id = user_id?.trim();
    if user_id.is_empty() {
        return None;
    }
    let vip = match is_vip {
        Some(true) => 1,
        Some(false) => 0,
        None => 2,
    };
    let mut lanes = [
        (0xcbf29ce484222325u64, 0x100000001b3u64),
        (0x9e3779b97f4a7c15u64, 0x100000001b3u64),
    ];
    for byte in b"sodam-unknown-probe-v1\0"
        .iter()
        .chain((user_id.len() as u64).to_le_bytes().iter())
        .chain(user_id.as_bytes())
        .chain(std::iter::once(&vip))
    {
        for (hash, prime) in &mut lanes {
            *hash = (*hash ^ u64::from(*byte)).wrapping_mul(*prime);
        }
    }
    Some(format!("{:016x}{:016x}", lanes[0].0, lanes[1].0))
}

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

// libresoda 可续传路径在暂存目标旁落盘的一族文件后缀。保护键与 24 小时
// 新鲜窗共用这一份，上游改命名表时只改这里。每项带 `.m4a` 前缀：保护键
// 摘掉整个后缀就能还原出交付目标，封面那类 `{hash}-160.img.*.part` 没有
// `.m4a` 前缀，落在映射之外。
//
// 命名来源分两种：除 `.m4a.part.dec.tmp` 外都来自上游 `part_record` 的
// `staged_paths` 命名表；`.m4a.part.dec.tmp` 是本仓库推测的第二种 dec 拼法
// （解密临时文件挂在 `.part` 后面），未与 libresoda 核对——真机落盘名对不上
// 时改这一条。
const STAGED_SUFFIXES: [&str; 6] = [
    ".m4a.part.meta.tmp",
    ".m4a.part.dec.tmp",
    ".m4a.part.meta",
    ".m4a.part.lock",
    ".m4a.dec.tmp",
    ".m4a.part",
];

/// 文件名；读不出来（非 UTF-8 或没有名字）当空串，后缀判断自然落在「不是」上。
fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
}

// 同一个暂存目标的一族文件与它的交付文件共享保护键：
// 租约挂在暂存目标上时，这几个文件都不会被维护线程删掉，淘汰也按这一键整组走。
fn protection_key(path: &Path) -> PathBuf {
    // 长后缀在前：`.part.meta.tmp` 必须先于 `.part.meta` 被摘掉。
    let name = file_name(path);
    if let Some(stem) = STAGED_SUFFIXES.iter().find_map(|s| name.strip_suffix(s)) {
        return path.with_file_name(format!("{stem}.m4a"));
    }
    if path.extension().is_some_and(|ext| ext == "quality") {
        path.with_extension("m4a")
    } else {
        path.to_owned()
    }
}

/// 这一条的名字落在可续传暂存族的命名表里（不含交付目标本身，含锁）。
///
/// 这里摘掉 `.m4a` 前缀再匹配，所以封面的 `{hash}-160.img.*.part` 也算
/// 暂存族、24 小时内不淘汰；[`protection_key`] 则必须要求 `.m4a` 前缀，
/// 否则封面临时会挂到音频暂存目标上。两边判据不一样是刻意的。
fn is_staged_part(path: &Path) -> bool {
    let name = file_name(path);
    STAGED_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix.strip_prefix(".m4a").unwrap_or(suffix)))
}

/// 这一条是不是 `.part.lock`。
fn is_lock(path: &Path) -> bool {
    file_name(path).ends_with(".part.lock")
}

/// 暂存里真正占地方的那拨：数据、记录、解密临时，锁除外
/// （锁为什么不算，见 [`Family::staged_payload`]）。
fn is_staged_payload(path: &Path) -> bool {
    is_staged_part(path) && !is_lock(path)
}

fn remove_idle(path: &Path) -> bool {
    // `.part.lock` 不走任何删除路径：它的 mtime 停在首次下载那一刻，
    // 按龄判定会把「复用中的锁」误判成残留；跨进程时租约又看不见内核锁，
    // 删掉正被挂着的锁文件，后到的下载者会在同名位置另立新锁，两把锁
    // 罩同一个目标，互斥就破了。锁是 0 字节，留着没有任何代价。
    if is_lock(path) {
        return false;
    }
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

/// 目录扫描得到的一个缓存文件：路径、字节数、最后修改时间。
pub(crate) struct ScannedFile {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) modified: SystemTime,
}

/// 扫描缺口落在哪一层：整个目录打不开、这一层有条目读不到、文件元数据取不到。
///
/// 只记「哪一层」，不记操作系统的原始错误：同一类失败在不同平台上的错误码并不一致，
/// 日志里留一个稳定的层名，比留一个随平台变来变去的 `io::Error` 文案更好查。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanGap {
    Dir,
    Entry,
    Metadata,
}

impl ScanGap {
    /// 日志字段用的稳定名字。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::Entry => "entries",
            Self::Metadata => "metadata",
        }
    }
}

/// 一次递归扫描的结果。
///
/// `incomplete` 记录「有地方没读到」：目录打不开、条目读错、元数据取不到。
/// 这种情况下 `files` 只是可读部分的视图，总量偏小，不能当成完整统计。
/// `gap` 是第一次没读到的位置落在哪一层，给报告与日志用；`gap_paths` 记下每一处缺口
/// 的位置（目录缺口是目录本身，元数据缺口是那个文件），用来判断「某一层是不是整个
/// 看见了」——只看层名不够。同一目录的元数据缺口只留第一条，路径不重复堆。
#[derive(Default)]
pub(crate) struct Scan {
    pub(crate) files: Vec<ScannedFile>,
    pub(crate) incomplete: bool,
    pub(crate) gap: Option<ScanGap>,
    pub(crate) gap_paths: Vec<PathBuf>,
    /// 已经记过元数据缺口的目录：同一层反复读不到只留第一条文件路径。
    metadata_gap_dirs: HashSet<PathBuf>,
}

impl Scan {
    pub(crate) fn total_bytes(&self) -> u64 {
        self.files
            .iter()
            .fold(0u64, |sum, file| sum.saturating_add(file.bytes))
    }

    /// 记一处扫描缺口；只保留第一处的层名，`incomplete` 与 `gap` 一起置上。
    ///
    /// 缺口路径保留原样：目录缺口记目录本身，元数据缺口记那个文件——判断某一层看没看全
    /// 要分得清「这层没打开」和「这层里有个文件读不到」，兄弟文件的缺口不该连累整层。
    /// 同一目录反复读不到元数据只留第一条，半坏的挂载点不会白存几千条路径。
    fn mark_incomplete(&mut self, gap: ScanGap, path: &Path) {
        self.incomplete = true;
        match gap {
            ScanGap::Metadata => {
                let dir = path.parent().unwrap_or(path);
                if self.metadata_gap_dirs.insert(dir.to_owned()) {
                    self.gap_paths.push(path.to_owned());
                }
            }
            ScanGap::Dir | ScanGap::Entry => self.gap_paths.push(path.to_owned()),
        }
        if self.gap.is_none() {
            self.gap = Some(gap);
        }
    }

    /// 这棵子树是不是整个看见了：缺口既不在它下面，也不在它的祖先上。
    ///
    /// 只看「缺口在不在子树里」不够：音频根那一层没读到时，staging 可能根本
    /// 没被列到；反过来，缺口落在子树里某个文件上时，这层也不算读完。
    fn subtree_fully_listed(&self, dir: &Path) -> bool {
        self.gap_paths
            .iter()
            .all(|gap| !gap.starts_with(dir) && !dir.starts_with(gap))
    }
}

/// 一处缓存目录的用量：字节、文件数、歌曲数，以及这一轮读全了没有。
///
/// `incomplete` 为真时前三个数都只是下界，设置页必须写成「至少」，
/// 不能当成准确值。维护这一轮的盘点与进设置页的一次性统计都产出它，
/// 两处数字的口径因此只有一份。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUsage {
    pub bytes: u64,
    pub files: usize,
    /// 顶层 `.m4a` 才算歌；边车、暂存都不算。封面树不数歌，恒为 0。
    pub songs: usize,
    pub incomplete: bool,
}

impl CacheUsage {
    /// 扫一遍目录得到用量。和维护共用同一份递归扫描，不另写一套遍历。
    pub fn scan(dir: &Path) -> Self {
        Self::of_scan(dir, scan_files(dir))
    }

    /// 把一次扫描折成用量；一次性统计走这里，口径和维护报告保持一致。
    fn of_scan(dir: &Path, scan: Scan) -> Self {
        let songs = scan
            .files
            .iter()
            .filter(|file| is_top_level_audio(dir, file.path.as_path()))
            .count();
        Self {
            bytes: scan.total_bytes(),
            files: scan.files.len(),
            songs,
            incomplete: scan.incomplete,
        }
    }

    /// 这棵树一个文件都没读到，而且确实有地方没读到：这个数不是 0，是不知道。
    ///
    /// 和「目录本来就是空的」分开：空的读全了（`incomplete` 为假），照实写 0。
    pub fn unreadable(&self) -> bool {
        self.incomplete && self.files == 0
    }
}

/// 音频、封面两棵树的用量：设置页读这一份，维护一轮结束后整份覆盖。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUsageSummary {
    pub audio: CacheUsage,
    pub covers: CacheUsage,
}

impl CacheUsageSummary {
    /// 两棵树加起来多少字节；任一处没读全时这只是下界。
    pub fn total_bytes(&self) -> u64 {
        self.audio.bytes.saturating_add(self.covers.bytes)
    }

    /// 任一处没读全：数字只是下界，界面要写成「至少」。
    pub fn incomplete(&self) -> bool {
        self.audio.incomplete || self.covers.incomplete
    }
}

#[derive(Clone, Copy)]
enum EntryKind {
    Dir,
    File,
    /// 符号链接、套接字等：一律不碰，免得走到缓存目录外。
    Other,
}

/// 目录里的一条：路径、类型，普通文件顺带带上枚举时取到的元数据。
struct ListedEntry {
    path: PathBuf,
    kind: EntryKind,
    /// 普通文件的元数据；取不到留 `None`，由遍历那边决定算不算扫描缺口。
    ///
    /// 这里用 `DirEntry::metadata`：Windows 上它是目录枚举的副产品，不再为每个文件
    /// 开一次句柄。换成按路径 `fs::metadata` 就是一次 open + close，几千个文件时
    /// 这一个口径就是每轮维护的主成本。
    meta: Option<std::fs::Metadata>,
}

/// 一层目录的读取结果。
struct Listing {
    entries: Vec<ListedEntry>,
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
        let (kind, meta) = match entry.file_type() {
            Ok(kind) if kind.is_dir() => (EntryKind::Dir, None),
            Ok(kind) if kind.is_file() => (EntryKind::File, entry.metadata().ok()),
            Ok(_) => (EntryKind::Other, None),
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        entries.push(ListedEntry {
            path: entry.path(),
            kind,
            meta,
        });
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
            scan.mark_incomplete(ScanGap::Dir, dir);
            return;
        }
    };
    if listing.incomplete {
        scan.mark_incomplete(ScanGap::Entry, dir);
    }
    for entry in listing.entries {
        match entry.kind {
            EntryKind::Dir => scan_into(&entry.path, scan, list),
            EntryKind::File => match entry.meta {
                Some(meta) => scan.files.push(ScannedFile {
                    path: entry.path,
                    bytes: meta.len(),
                    modified: meta.modified().unwrap_or_else(|_| SystemTime::now()),
                }),
                // 枚举时取不到元数据：可能是已经被删掉，也可能是读不到。按路径再问一次
                // 把两者分开——这条路径只在出错时才走，多一次 stat 换回原来的判定口径。
                None => match std::fs::metadata(&entry.path) {
                    Ok(meta) => scan.files.push(ScannedFile {
                        path: entry.path,
                        bytes: meta.len(),
                        modified: meta.modified().unwrap_or_else(|_| SystemTime::now()),
                    }),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => scan.mark_incomplete(ScanGap::Metadata, &entry.path),
                },
            },
            EntryKind::Other => {}
        }
    }
}

/// 「一家子」缓存文件：同一个交付目标派生出来的文件。
///
/// 「哪些文件算一家子」由三个归族入口定死，「这家子老不老」「能不能删」
/// 「怎么删」也只在这里定义；四个删除点各自只说这一轮挑哪几家（哪几条）：
///
/// - 清空缓存挑「整家都不新鲜的」（[`Family::staged`]）；
/// - 配额淘汰按最后使用时间从旧到新挑（[`Family::by_target`]）；
/// - 旧格式残留清扫挑过了 24 小时窗口的那条（[`Family::staged`]）；
/// - 暂存过期清扫挑整族都过了保留期的（[`Family::staged_payload`]）。
///
/// 认族只看保护键（[`protection_key`]），不要求配套文件齐全：下到一半、
/// 记录还没落下的时候，只有数据那一条也照样是一家。
struct Family<'a> {
    files: Vec<&'a ScannedFile>,
}

impl<'a> Family<'a> {
    /// 同一交付目标下的全部文件算一家：交付目标、音质边车、可续传的数据与记录、
    /// 解密临时、锁一起判老。配额淘汰按这一口径——还在下的那拨不能被配额拆开。
    /// 封面淘汰也走这个入口，只是分组键换成 [`cover_group_key`]。
    fn by_target<'f, K>(
        files: impl IntoIterator<Item = &'f ScannedFile>,
        key: impl Fn(&Path) -> K,
    ) -> Vec<Family<'f>>
    where
        K: Eq + std::hash::Hash + Clone,
    {
        Self::group(files, |_: &Path| true, key)
    }

    /// 只有可续传的那拨算一家：正式文件、音质边车各自成家、各看各的年纪。
    /// 清空缓存与旧残留清扫按这一口径。
    fn staged<'f>(files: impl IntoIterator<Item = &'f ScannedFile>) -> Vec<Family<'f>> {
        Self::group(files, is_staged_part, protection_key)
    }

    /// 暂存过期清扫的归族：可续传那拨，锁除外（其余文件各自成家）。
    ///
    /// 锁不进来，是因为它的 mtime 停在首次下载，答不了「这批残留放够保留期没有」。
    /// 清空那边反过来：一把 24 小时内的锁说明这家子最近才开始下，续传对要留着，
    /// 所以 [`Family::staged`] 把它算在族里。
    fn staged_payload<'f>(files: impl IntoIterator<Item = &'f ScannedFile>) -> Vec<Family<'f>> {
        Self::group(files, is_staged_payload, protection_key)
    }

    /// 按 `key` 归族；`member` 之外的文件一条自成一家。
    ///
    /// 先出现的成员族排在前面，独族追加在后面：同一批扫描结果每次归出来都一样。
    fn group<'f, K>(
        files: impl IntoIterator<Item = &'f ScannedFile>,
        member: impl Fn(&Path) -> bool,
        key: impl Fn(&Path) -> K,
    ) -> Vec<Family<'f>>
    where
        K: Eq + std::hash::Hash + Clone,
    {
        let mut order: Vec<K> = Vec::new();
        let mut groups: HashMap<K, Vec<&'f ScannedFile>> = HashMap::new();
        let mut alone: Vec<Family<'f>> = Vec::new();
        for file in files {
            if !member(&file.path) {
                alone.push(Family { files: vec![file] });
                continue;
            }
            match groups.entry(key(&file.path)) {
                hash_map::Entry::Occupied(mut group) => group.get_mut().push(file),
                hash_map::Entry::Vacant(entry) => {
                    order.push(entry.key().clone());
                    entry.insert(vec![file]);
                }
            }
        }
        let mut families: Vec<Family<'f>> = order
            .into_iter()
            .filter_map(|key| groups.remove(&key).map(|files| Family { files }))
            .collect();
        families.extend(alone);
        families
    }

    /// 这家子还新不新鲜：任一条成员还落在 24 小时新鲜窗里就算
    /// （新鲜窗只认暂存族名与 `.part` 后缀，见 [`is_fresh_part`]）。
    ///
    /// 续传的数据和记录必须一起判：中断重写的记录 mtime 比数据新，
    /// 逐条判新鲜会把续传对拆成半套。
    fn fresh(&self) -> bool {
        self.files
            .iter()
            .any(|file| is_fresh_part(&file.path, file.modified))
    }

    /// 这家子是不是整族都过了 `max_age`；任一条还年轻就是 `false`。
    fn expired(&self, max_age: Duration, now: SystemTime) -> bool {
        self.files
            .iter()
            .all(|file| aged_out(file.modified, max_age, now))
    }

    /// 这家子最近一次活动的时间：mtime 与最近显示时间取较晚者，配额按它排 LRU。
    fn used_at(&self, last_seen: &HashMap<PathBuf, SystemTime>) -> SystemTime {
        self.files
            .iter()
            .fold(SystemTime::UNIX_EPOCH, |latest, file| {
                let shown = last_seen.get(&file.path).copied();
                let used_at = shown.map_or(file.modified, |shown| file.modified.max(shown));
                latest.max(used_at)
            })
    }

    /// 删掉这家子里挑中的成员，返回真正删掉的路径。
    ///
    /// `unlink` 只说这一层怎么落盘：音频走 [`Family::remove`] 的锁与租约，
    /// 封面没有这层、直接用这个入口传自己的删除函数。
    fn remove_with(
        &self,
        pick: impl Fn(&ScannedFile) -> bool,
        mut unlink: impl FnMut(&Path) -> bool,
    ) -> Vec<&'a ScannedFile> {
        let mut removed = Vec::new();
        for &file in &self.files {
            if pick(file) && unlink(&file.path) {
                removed.push(file);
            }
        }
        removed
    }

    /// 音频这一家子的删除：`.part.lock` 永不动；每条删之前各查一次租约，
    /// 被租用的那条留在原地。
    fn remove(&self, pick: impl Fn(&ScannedFile) -> bool) -> Vec<&'a ScannedFile> {
        self.remove_with(pick, remove_idle)
    }
}

/// 这一条过了 `max_age` 没；mtime 在未来说明时钟回拨，按还没到算。
/// 贡献者图的 TTL 判定走同一套口径（#341），所以做成 crate 内共享。
pub(crate) fn aged_out(modified: SystemTime, max_age: Duration, now: SystemTime) -> bool {
    now.duration_since(modified).is_ok_and(|age| age >= max_age)
}

/// 24 小时内的下载临时文件不参与淘汰（还在下载，或留着给下次续传）。
///
/// 覆盖封面的 `.part` 临时名和 libresoda 暂存族的整族名字；调用方再按各自
/// 的分组键（音频 `protection_key`、封面 `cover_group_key`）把族聚合，
/// 保证数据和记录一起保护。过了窗口的暂存由配额淘汰与保留期清扫处理，
/// libresoda 下载时也会清掉 7 天过期对。
fn is_fresh_part(path: &Path, modified: SystemTime) -> bool {
    if aged_out(modified, PART_MAX_AGE, SystemTime::now()) {
        return false;
    }
    // 暂存族按与 [`protection_key`] 共用的那份后缀表认领：
    // 数据和记录必须一起保护，删掉一边续传对就再也对不上了。
    if is_staged_part(path) {
        return true;
    }
    // 兜底：非 UTF-8 文件名读不出名字，写了一半的封面/音频临时照样按
    // `.part` 后缀护住。
    path.extension().is_some_and(|ext| ext == "part")
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
/// 组里任一成员是 24 小时内的临时文件，**整组**计入总量但不参与淘汰（还在下载）：
/// 中断重写的记录 mtime 比数据新，按文件各自判新鲜会在配额超额时把续传对拆成半套。
///
/// `last_seen` 是「文件路径 → 最近一次显示时间」；没有记录的路径按 mtime 排。
///
/// 低于上限直接返回：分组、排序、逐组尝试删除全是白做，稳态下（缓存没满）
/// 这一条就是每轮维护最大的省项。
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
    K: Clone + Eq + std::hash::Hash,
    G: Fn(&Path) -> K,
    R: FnMut(&Path) -> bool,
{
    let scanned_total = files
        .iter()
        .fold(0u64, |sum, file| sum.saturating_add(file.bytes));
    if scanned_total <= limit_bytes {
        return (scanned_total, Vec::new());
    }
    let mut families = Family::by_target(&files, group_key);
    families.sort_by_key(|family| family.used_at(last_seen));
    let mut total = scanned_total;
    let mut removed = Vec::new();
    for family in &families {
        if total <= limit_bytes {
            break;
        }
        // 新鲜的一家子整族计入总量但不参与淘汰：还在下的续传对不能拆。
        if family.fresh() {
            continue;
        }
        for file in family.remove_with(|_: &ScannedFile| true, &mut remove) {
            total = total.saturating_sub(file.bytes);
            removed.push(file.path.clone());
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
/// 扫描一直不完整、动作一直是 `skip` 时，每轮维护都写一条 Warn 就是刷屏；
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

/// 一次手动清空的结论。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClearReport {
    pub removed: usize,
    /// 这一轮有没有地方没读到（目录、条目或元数据）：读不全时，没看到的那几个文件清不掉。
    pub incomplete: bool,
}

/// 递归清理缓存，保留正在使用的音频和近期临时文件。
///
/// 挑法是「整家不新鲜就删」：可续传的那拨按保护键成族（数据和记录必须一起判，
/// 拆开就接不上续传），正式文件、边车、封面各自成家、各看各的。
/// 租约护着这一家时一条都不删（[`Family::remove`]）。
///
/// 清空不靠扫描结果决定「删不删」（看到就照常清），但要把这一轮读没读全交回去，
/// 好在提示里说清有没有留下没清到的文件。
pub fn clear_in(root: &Path) -> ClearReport {
    clear_with(root, list_dir)
}

/// 清空实现，目录读取可注入：测试用它模拟「某个子目录读不到」。
fn clear_with<L>(root: &Path, list: L) -> ClearReport
where
    L: FnMut(&Path) -> ListingResult,
{
    let scan = scan_with(root, list);
    let removed = Family::staged(&scan.files)
        .iter()
        .filter(|family| !family.fresh())
        .map(|family| family.remove(|_: &ScannedFile| true).len())
        .sum();
    ClearReport {
        removed,
        incomplete: scan.incomplete,
    }
}

/// 按最后使用时间淘汰音频（命中缓存时更新 mtime）。租约文件可暂时超额。
/// sidecar 与音频成组淘汰；失败的删除不计入释放空间。调用方必须在后台执行。
///
/// 扫描不完整时照旧只按可读部分淘汰：音频有租约保护、又是用户主动设定的额度，
/// 少删（停在额度之上）比不删更接近预期。封面则在可读部分已超限时才这么做、否则本轮
/// 不删——这点差异是有意保留的：音频额度是用户可见的硬需求，封面是「别无限涨」的软约束。
fn trim_audio_scan(scan: Scan, limit_bytes: u64) -> (u64, Vec<PathBuf>) {
    evict_lru(
        scan.files,
        limit_bytes,
        protection_key,
        &HashMap::new(),
        remove_idle,
    )
}

/// 清音频根下旧格式暂存残留，只列顶层、不递归。
///
/// 「旧格式」指暂存挪进 `staging/` 之前的老实现落下的文件（#241）：当时
/// `{曲目}-{档位}-{pid}-{seq}.m4a.part(.part)` 这类文件直接躺在音频根下，
/// 改到子目录后就没人管了。族名认领用 [`STAGED_SUFFIXES`]，超过
/// [`PART_MAX_AGE`] 的交给 [`remove_idle`]（租约与 `.part.lock` 的例外都在那儿）。
///
/// 不递归进 `staging/`：那里归 [`purge_stale_staging_files`] 的 7 天保留期按
/// 整族清扫，这里按成员年龄动手会把续传对拆开。
///
/// 能摘回交付目标的尾巴（`.part`、`.part.meta`、`.dec.tmp` 等）照样受租约
/// 保护；只有 `.m4a.part.part` 这种双尾巴摘不回去，保护键就是文件自己——
/// 好在产生它们的旧进程早已不在，不会有租约指向它们。
///
/// 返回 `Some(删除数)`；`None` 表示这轮目录没读全（条目、类型或元数据
/// 读不到），交给下轮重试。
fn sweep_legacy_residue_once(audio_root: &Path) -> Option<usize> {
    let entries = match std::fs::read_dir(audio_root) {
        Ok(entries) => entries,
        // 目录不存在是正常状态（首次启动、从没下载过）：按空目录处理，别当缺口，
        // 否则全新机器会白跑满重试轮次再留一条放弃 Warn。
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    };
    let mut complete = true;
    let mut candidates: Vec<ScannedFile> = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            complete = false;
            continue;
        };
        // 只动顶层普通文件；目录（含 staging 子目录）与符号链接一律不碰。
        if !file_type.is_file() {
            continue;
        }
        // 锁永不删（#243）：mtime 停在首次下载，删掉正被内核锁挂着的锁
        // 会让后到的下载者另立新锁，同一目标出现两把锁，互斥就破了。
        // 连元数据都不取，读不到也不算这轮的缺口。
        if is_lock(&path) {
            continue;
        }
        if !is_staged_part(&path) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            // 扫描与取元数据之间被删掉是正常竞争；还在却读不到才算缺口。
            if path.exists() {
                complete = false;
            }
            continue;
        };
        candidates.push(ScannedFile {
            path,
            bytes: meta.len(),
            modified: meta.modified().unwrap_or_else(|_| SystemTime::now()),
        });
    }
    let mut removed = 0;
    for family in Family::staged(&candidates) {
        // 挑法落在成员这一层，不按整家判：只剩半截数据、记录还是新的时候，
        // 按家会把它一起留下。老残留没有能续上的配对了，就该各自清掉。
        // 归族只为跟其他删除点共用同一条删除路，族界不影响谁被删。
        removed += family
            .remove(|file: &ScannedFile| !is_fresh_part(&file.path, file.modified))
            .len();
    }
    if removed > 0 {
        log_event(
            LogLevel::Info,
            LogTopic::App,
            "legacy_residue_swept",
            &[("removed", removed.to_string().as_str())],
        );
    }
    complete.then_some(removed)
}

/// 首轮巡检的连续不完整轮数上限：到顶就放弃，进程内不再重试。
/// 5 轮 ≈ 一天半（常规 6 小时一轮维护），目录长期读不到时不至于每轮都白扫
/// 一遍永不停止——那正是进程级一次性门要省的开销。
const SWEEP_MAX_INCOMPLETE_ROUNDS: usize = 5;

/// 首轮巡检的门：进程级状态（连续不完整轮数 + 是否已结束）独立成结构体，
/// 让「不完整到上限就置完成」「完成后再喂结果直接返回 0」这两条关键行为
/// 能在单测里直接固定——进程级静态本身依赖 root_dir，测不到。
struct SweepGate {
    incomplete_rounds: usize,
    finished: bool,
}

impl SweepGate {
    const fn new() -> Self {
        Self {
            incomplete_rounds: 0,
            finished: false,
        }
    }

    /// 门是否已结束（完成或放弃）。
    ///
    /// 调用方先问一句再扫：门关了就别再列目录，省掉每轮维护
    /// 白付的 read_dir 与逐文件 metadata。
    fn is_finished(&self) -> bool {
        self.finished
    }

    /// 喂一轮结果：`Some(删除数)` 就收工关门；`None` 就跳过本轮、计数加一。
    /// 连续到 [`SWEEP_MAX_INCOMPLETE_ROUNDS`] 轮放弃并关门。
    /// 日志只写两条：首轮一条 `legacy_residue_sweep_incomplete`，
    /// 放弃时一条 `legacy_residue_sweep_gave_up`，中间静默。
    /// 返回删除的文件数。
    fn advance(&mut self, outcome: Option<usize>) -> usize {
        if self.finished {
            return 0;
        }
        let Some(removed) = outcome else {
            self.incomplete_rounds += 1;
            let rounds = self.incomplete_rounds;
            if rounds == 1 {
                log_event(
                    LogLevel::Warn,
                    LogTopic::App,
                    "legacy_residue_sweep_incomplete",
                    &[],
                );
            }
            if rounds >= SWEEP_MAX_INCOMPLETE_ROUNDS {
                let rounds_text = rounds.to_string();
                log_event(
                    LogLevel::Warn,
                    LogTopic::App,
                    "legacy_residue_sweep_gave_up",
                    &[("rounds", rounds_text.as_str())],
                );
                self.finished = true;
            }
            return 0;
        };
        self.finished = true;
        removed
    }
}

/// 音频根下旧格式暂存残留的启动巡检，返回删除的文件数。
///
/// 只在启动后的首轮维护实际执行：维护常规 6 小时一轮，跨过年龄门槛的老残留
/// 不会每轮变多，每轮重扫几乎全是空跑。
///
/// 门用互斥锁 + [`std::sync::Mutex::try_lock`] 串行化：拿不到锁说明另一轮
/// 巡检正在进行，本轮空转返回，不排队干等。锁中毒按「拿回状态继续跑」处理，
/// 别让一次 panic 把进程级一次性门永久关掉。这是 pub 函数，正确性不押在
/// 调用方恰好单飞上。
///
/// 锁文件永不删，staging 子目录属于 [`purge_stale_staging_files`] 的 7 天保留期。
/// 调用方必须在后台执行。
pub fn sweep_legacy_residue() -> usize {
    static SWEEP_GATE: Mutex<SweepGate> = Mutex::new(SweepGate::new());
    let mut gate = match SWEEP_GATE.try_lock() {
        Ok(gate) => gate,
        Err(std::sync::TryLockError::Poisoned(err)) => err.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return 0,
    };
    // 先问门再列目录：门关了就别每轮白付一次 read_dir + 每文件 metadata。
    // 参数立即求值的话，扫完进 advance 才发现已结束，门等于只省了删除不省扫描。
    if gate.is_finished() {
        return 0;
    }
    gate.advance(sweep_legacy_residue_once(&root_dir().join("audio")))
}

/// staging 子目录名。下载把暂存放这里（`session.rs` 的暂存目标也用它），
/// 维护把超过保留期的残留从这里清走。
pub(crate) const STAGING_DIR_NAME: &str = "staging";

/// 音频树里的暂存子目录：可续传的临时文件放这里，维护按保留期从这里清残留。
///
/// 只暴露目录本身，不暴露它的名字：以后换布局，调用方不用跟着改。
pub fn staging_dir(audio_dir: &Path) -> PathBuf {
    audio_dir.join(STAGING_DIR_NAME)
}

/// 暂存残留保留 7 天，和 libresoda 自己的过期清扫同一条口径（#243）；
/// 过了 24 小时新鲜窗就不再受保护，配额超额时按 LRU 先走。
const STALE_STAGING_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// 把 staging 里超过保留期的残留整族清掉，返回删掉的路径。
///
/// `files` 是音频树这一轮的扫描结果，只认 `staging_dir` 的直接子项（staging 是平的，
/// 旧实现也只列这一层）。过期按**整族**判：族里任一成员还年轻就整族不动——中断重写的
/// 记录 mtime 比数据新，逐文件判龄会在边界上拆开续传对；族全过期才整族删。
/// 锁不进族（为什么见 [`Family::staged_payload`]）；锁本身也不会被删
/// （[`remove_idle`] 的第一条）。
///
/// `staging_fully_listed` 是 staging 这一层有没有整个看见（不是整棵音频树读全了没有）。
/// 这层没看全时，看不到的可能是族里最新的一员，见过的那几条会显得「整族都过期」，
/// 这一轮就一个都不清；缺口补上下轮再说。缺口在别处、这层看全了，照常清。
///
/// 正式文件与音质边车不跟着族走：它们各自成家、各看各的年纪。
fn purge_stale_staging_files(
    files: &[ScannedFile],
    staging_dir: &Path,
    max_age: Duration,
    now: SystemTime,
    staging_fully_listed: bool,
) -> Vec<PathBuf> {
    if !staging_fully_listed {
        return Vec::new();
    }
    let in_staging = files
        .iter()
        .filter(|file| file.path.parent() == Some(staging_dir));
    let mut removed = Vec::new();
    for family in Family::staged_payload(in_staging) {
        if !family.expired(max_age, now) {
            continue;
        }
        removed.extend(
            family
                .remove(|_: &ScannedFile| true)
                .into_iter()
                .map(|file| file.path.clone()),
        );
    }
    removed
}

/// 一轮维护的两条上限（字节）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintenanceLimits {
    pub audio_bytes: u64,
    pub cover_bytes: u64,
}

impl MaintenanceLimits {
    pub fn from_mb(audio_limit_mb: u64, cover_limit_mb: u64) -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            audio_bytes: audio_limit_mb.saturating_mul(MIB),
            cover_bytes: cover_limit_mb.saturating_mul(MIB),
        }
    }
}

/// 上一次维护的结论：「到期没」和「已知占多少」都读它，进程重启不清零。
///
/// 只存结论不存明细：它是判断依据，不是缓存目录的索引。用户手动清空缓存目录后
/// 这份记录会偏大，最坏结果是提前跑一轮，不会让上限失效。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintenanceMark {
    pub completed_at: SystemTime,
    pub audio_bytes: u64,
    pub cover_bytes: u64,
    /// 这一轮有没有读不全的子树。
    ///
    /// 读不全时两个字节数只是下界，不能当成一次完整的盘点把下一次检查推到常规间隔：
    /// 没读到的那部分可能让缓存停在额度之上。带这个标记的下一轮按
    /// [`crate::cache_check::MAINTENANCE_NEAR_LIMIT_INTERVAL`] 重查：既不放到常规间隔，
    /// 也不按 [`crate::cache_check::MAINTENANCE_PRESSURE_RETRY`] 反复扫。
    pub incomplete: bool,
}

/// 记录上次维护结论的文件名，放在缓存根目录（音频、封面两棵树之外，扫描不会碰到它）。
const MAINTENANCE_MARK_FILE: &str = "maintenance.tsv";

/// 被替换掉的旧文件：正式音频与它的边车各多大。
#[derive(Clone, Copy, Default)]
pub(crate) struct Replaced {
    pub audio: u64,
    pub sidecar: u64,
}

/// 一次落地写进正式缓存的确切字节：正常路径先量后写；边车写坏了按实际落盘量补。
pub(crate) struct Landing {
    /// 这次尝试开始前暂存族在盘上多大（含马上会被清掉的残留交付目标）。
    pub staged_before: u64,
    /// 落地的音频字节。服务器判「早就下完了」、这次传输量为 0 时，是盘上完整文件的大小。
    pub audio: u64,
    /// 落地的边车字节。
    pub sidecar: u64,
    /// 被换掉的旧文件。
    pub replaced: Replaced,
}

/// 上次真看盘之后的音频净增量（字节）。
///
/// 只累计「又多了多少」：每次下载尝试只加这次长出来（或缩掉）的部分；真看盘一开始
/// 就清零，扫描结论永远是基准。只算音频——下载只往音频树写，封面有固定上限，不掺进来。
///
/// 两个方向都会有误差，但都只影响「提前多少」，不改扫描结论，下一轮扫描自然纠正：
/// - 偏大：下载途中正好赶上一次看盘，这次尝试记的差额可能把扫描已经算过的一小截
///   再加一遍；
/// - 偏小：落地时减掉的旧文件若是上次看盘之后才出现（比如进程重启后账本从零开始），
///   减掉就等于凭空少算那一份。
static AUDIO_DELTA: AtomicI64 = AtomicI64::new(0);

fn as_i64(bytes: u64) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

/// `after` 比 `before` 多了多少：负数表示缩了。续传、覆盖都只按这个差额记账。
fn size_change(before: u64, after: u64) -> i64 {
    as_i64(after).saturating_sub(as_i64(before))
}

/// 一次下载落进正式缓存时的净变化：暂存长到交付大小、旧音频与旧边车被换掉。
fn landed_delta(landing: Landing) -> i64 {
    size_change(landing.staged_before, landing.audio)
        .saturating_sub(as_i64(landing.replaced.audio))
        .saturating_add(size_change(landing.replaced.sidecar, landing.sidecar))
}

/// 上次看盘之后又写进来的音频字节（净增量，可为负）。
pub fn pending_audio_bytes() -> i64 {
    AUDIO_DELTA.load(Ordering::Relaxed)
}

/// 一个暂存目标对应的整族文件：交付目标自己，加上 libresoda 按同一目标推导的那些名字。
fn staged_family_paths(target: &Path) -> Vec<PathBuf> {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut paths = vec![target.to_owned()];
    for suffix in STAGED_SUFFIXES {
        // 后缀表统一带 `.m4a` 前缀（暂存目标自己已经带上了），拼上去时摘掉它。
        let tail = suffix.strip_prefix(".m4a").unwrap_or(suffix);
        paths.push(target.with_file_name(format!("{name}{tail}")));
    }
    paths
}

/// 一整族暂存文件在盘上占的字节数：交付目标、密文、记录与锁。
///
/// 下载开始前量一次，失败或结束时再量一次，两次的差额就是要记的增量。
pub(crate) fn staged_family_bytes(target: &Path) -> u64 {
    staged_family_paths(target)
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .fold(0u64, |sum, meta| sum.saturating_add(meta.len()))
}

/// 一次尝试结束（失败、取消、重试、没换成更好流）：只把暂存族这次的差额记进账。
///
/// 不能把整族再加一遍——跨过一次看盘之后，`before` 那部分已经在扫描结论里了。
/// 缩了也照记（负数），旧的大数不会一直挂着。只 stat，不读内容。
pub(crate) fn note_staged_change(target: &Path, before: u64) {
    let after = staged_family_bytes(target);
    AUDIO_DELTA.fetch_add(size_change(before, after), Ordering::Relaxed);
}

/// 一次下载把字节落进正式缓存（点播与升级共用）。
pub(crate) fn note_download_landed(landing: Landing) {
    AUDIO_DELTA.fetch_add(landed_delta(landing), Ordering::Relaxed);
}

/// 量一个文件有多大，取不到当 0。
///
/// 用在两处，误差都不改扫描结论，下一轮扫描自然纠正：
/// - 被换掉的旧文件：取不到就少减，账偏大；
/// - 写失败后半截的边车：取不到就少记，账偏小。
pub(crate) fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// 这次真看盘：账本整本作废，扫描结果就是新的起点。
pub(crate) fn reset_cache_accounting() {
    AUDIO_DELTA.store(0, Ordering::Relaxed);
}

/// 一轮维护的结论：设置页的数字、界面要同步的删除清单、日志要的指标都在这里。
pub struct MaintenanceReport {
    pub audio_bytes: u64,
    /// 这一轮删完之后音频树里还剩几个文件（含边车与暂存）。
    pub audio_files: usize,
    pub songs: usize,
    pub cover_bytes: u64,
    pub cover_files: usize,
    /// 这轮真正删掉的封面路径：界面只同步这几张，不在主线程全表扫盘。
    pub removed_covers: Vec<PathBuf>,
    pub audio_incomplete: bool,
    pub cover_incomplete: bool,
    /// 第一次扫描缺口落在哪一层；读全了是 `None`。
    pub audio_gap: Option<ScanGap>,
    pub cover_gap: Option<ScanGap>,
    pub scanned_files: usize,
    pub removed_files: usize,
    /// 这一轮跑完之后，任一棵树仍在上限之上（多半是租约或锁挡住了删除）。
    /// 和「这轮删了几个」解耦：删了一张封面不代表音频那边已经收口。
    pub over_limit: bool,
    pub elapsed: Duration,
    /// 这一轮的结论缩写，日志直接用：`none`（删了东西且已收口）/ `below_limit` /
    /// `blocked`（超限且一个都没删掉）/ `over_limit`（删过但仍未收口）/
    /// `audio_incomplete` / `covers_incomplete`。扫描缺口优先：读不全时数字只是下界。
    pub skipped_reason: &'static str,
}

impl MaintenanceReport {
    /// 音频树的用量；读不全时数字只是下界。
    fn audio_usage(&self) -> CacheUsage {
        CacheUsage {
            bytes: self.audio_bytes,
            files: self.audio_files,
            songs: self.songs,
            incomplete: self.audio_incomplete,
        }
    }

    /// 封面树的用量；封面不数歌。
    fn cover_usage(&self) -> CacheUsage {
        CacheUsage {
            bytes: self.cover_bytes,
            files: self.cover_files,
            songs: 0,
            incomplete: self.cover_incomplete,
        }
    }

    /// 设置页要的两棵树用量：一次覆盖，别让两处各拼一份。
    pub fn summary(&self) -> CacheUsageSummary {
        CacheUsageSummary {
            audio: self.audio_usage(),
            covers: self.cover_usage(),
        }
    }

    /// 这一轮读不全的原因，日志用：没读全时写明是哪棵树、哪一层，
    /// 不是只留一个布尔。读全了是 `none`。
    pub fn audio_gap_label(&self) -> &'static str {
        self.audio_gap.map_or("none", ScanGap::label)
    }

    pub fn cover_gap_label(&self) -> &'static str {
        self.cover_gap.map_or("none", ScanGap::label)
    }

    pub fn mark(&self, completed_at: SystemTime) -> MaintenanceMark {
        MaintenanceMark {
            completed_at,
            audio_bytes: self.audio_bytes,
            cover_bytes: self.cover_bytes,
            incomplete: self.audio_incomplete || self.cover_incomplete,
        }
    }
}

/// 跑一轮缓存维护。调用方必须在后台线程里调。
///
/// 音频树、封面树各只遍历一次：配额淘汰、staging 清扫、统计都吃同一份盘点结果。
/// 旧格式残留在进程首轮顺手清一次（门在列目录之前，之后每轮只付一次 `try_lock`）。
pub fn maintain(
    limits: MaintenanceLimits,
    last_seen: &HashMap<PathBuf, SystemTime>,
) -> MaintenanceReport {
    // 这次真看盘：账本整本作废。扫描的结论从这一轮起是新的基准，账本只记它之后的增量。
    reset_cache_accounting();
    sweep_legacy_residue();
    let audio_dir = crate::audio::cache_dir();
    let covers_dir = crate::audio::cover_cache_dir();
    let report = maintain_with(&audio_dir, &covers_dir, limits, last_seen, list_dir);
    // 每轮都记一条 Info：常规一天 4 条，贴着上限 48 条，超限又删不动时最多 144 条。
    // 成本曲线要能直接看出来，不能只在排障日志里才有。
    if crate::log_outlet::enabled(LogLevel::Info, LogTopic::App) {
        let elapsed = report.elapsed.as_millis().to_string();
        let scanned = report.scanned_files.to_string();
        let removed = report.removed_files.to_string();
        let audio_bytes = report.audio_bytes.to_string();
        // 只有音频树读不全时 audio_bytes 才是下界：单独给一个字段，不用自己对 skipped_reason。
        let audio_incomplete = u8::from(report.audio_incomplete).to_string();
        let over_limit = u8::from(report.over_limit).to_string();
        log_event(
            LogLevel::Info,
            LogTopic::App,
            "cache_maintenance",
            &[
                ("duration_ms", elapsed.as_str()),
                ("scanned_files", scanned.as_str()),
                ("removed_files", removed.as_str()),
                ("audio_bytes", audio_bytes.as_str()),
                ("skipped_reason", report.skipped_reason),
                ("audio_gap", report.audio_gap_label()),
                ("cover_gap", report.cover_gap_label()),
                ("audio_incomplete", audio_incomplete.as_str()),
                ("over_limit", over_limit.as_str()),
            ],
        );
    }
    report
}

/// 在指定目录上跑一轮维护：规模基准与测试用。
///
/// 正式入口 [`maintain`] 固定读全局缓存根，还会先跑一次进程级的旧残留巡检、把账本清零、
/// 写维护日志；这条入口只做「扫两棵树、淘汰、统计」这一轮本身，不写日志、不清账本、
/// 不碰全局缓存根。它仍会经过封面告警门和租约表——和任何一轮维护一样。
pub fn maintain_in(
    audio_dir: &Path,
    covers_dir: &Path,
    limits: MaintenanceLimits,
    last_seen: &HashMap<PathBuf, SystemTime>,
) -> MaintenanceReport {
    maintain_with(audio_dir, covers_dir, limits, last_seen, list_dir)
}

/// 顶层 `.m4a` 才算一首歌：sidecar、暂存、封面原图都不是。
fn is_top_level_audio(audio_dir: &Path, path: &Path) -> bool {
    path.parent() == Some(audio_dir) && path.extension().is_some_and(|ext| ext == "m4a")
}

/// 一轮维护的实现：目录和目录读取都可注入，单测用临时目录加计数闭包盯住
/// 「每棵树只遍历一次」。
///
/// 暂存清扫排在配额淘汰前面：它只碰 7 天以上的死文件（淘汰本来也不会动 24 小时内的
/// 族），先摘掉能让配额少算一份，超限时也就不用为这些死文件多删正式缓存。
fn maintain_with<L>(
    audio_dir: &Path,
    covers_dir: &Path,
    limits: MaintenanceLimits,
    last_seen: &HashMap<PathBuf, SystemTime>,
    list: L,
) -> MaintenanceReport
where
    L: FnMut(&Path) -> ListingResult + Copy,
{
    let started = Instant::now();

    let mut audio = scan_with(audio_dir, list);
    let audio_incomplete = audio.incomplete;
    let audio_gap = audio.gap;
    let audio_scanned = audio.files.len();

    let staging = staging_dir(audio_dir);
    let staging_complete = audio.subtree_fully_listed(&staging);
    let purged = purge_stale_staging_files(
        &audio.files,
        &staging,
        STALE_STAGING_MAX_AGE,
        SystemTime::now(),
        staging_complete,
    );
    let purged_files = purged.len();
    if !purged.is_empty() {
        let gone: HashSet<PathBuf> = purged.into_iter().collect();
        audio.files.retain(|file| !gone.contains(&file.path));
    }

    let audio_after_purge = audio.files.len();
    let songs_before = audio
        .files
        .iter()
        .filter(|file| is_top_level_audio(audio_dir, file.path.as_path()))
        .count();
    let (audio_bytes, removed_audio) = trim_audio_scan(audio, limits.audio_bytes);
    let audio_files = audio_after_purge.saturating_sub(removed_audio.len());
    let songs_removed = removed_audio
        .iter()
        .filter(|path| is_top_level_audio(audio_dir, path.as_path()))
        .count();

    let covers = scan_with(covers_dir, list);
    let cover_incomplete = covers.incomplete;
    let cover_gap = covers.gap;
    let cover_before = covers.files.len();
    let (cover_bytes, removed_covers) = trim_covers_scan(
        covers,
        limits.cover_bytes,
        last_seen,
        cover_alert_gate(),
        |alert| alert.log(),
        |path| std::fs::remove_file(path).is_ok(),
    );

    let removed_files = purged_files + removed_audio.len() + removed_covers.len();
    let over_limit = audio_bytes > limits.audio_bytes || cover_bytes > limits.cover_bytes;
    let skipped_reason = if audio_incomplete {
        "audio_incomplete"
    } else if cover_incomplete {
        "covers_incomplete"
    } else if over_limit && removed_files == 0 {
        // 超了却一个都没删掉：多半是租约或锁挡住了，别在日志里写成「没超限」。
        "blocked"
    } else if over_limit {
        // 删过东西但还没收口（比如封面清掉了、音频全被租约挡着）。
        "over_limit"
    } else if removed_files == 0 {
        "below_limit"
    } else {
        "none"
    };

    MaintenanceReport {
        audio_bytes,
        audio_files,
        songs: songs_before.saturating_sub(songs_removed),
        cover_bytes,
        cover_files: cover_before.saturating_sub(removed_covers.len()),
        removed_covers,
        audio_incomplete,
        cover_incomplete,
        audio_gap,
        cover_gap,
        scanned_files: audio_scanned + cover_before,
        removed_files,
        over_limit,
        elapsed: started.elapsed(),
        skipped_reason,
    }
}

/// 读取上次维护结论。文件不存在、内容坏了都当没有记录，最坏多扫一轮。
pub fn load_mark() -> Option<MaintenanceMark> {
    load_mark_in(&root_dir())
}

pub(crate) fn load_mark_in(root: &Path) -> Option<MaintenanceMark> {
    let text = std::fs::read_to_string(root.join(MAINTENANCE_MARK_FILE)).ok()?;
    parse_mark(&text)
}

fn parse_mark(text: &str) -> Option<MaintenanceMark> {
    let mut fields = text.trim().split('\t');
    let secs: i64 = fields.next()?.parse().ok()?;
    let audio_bytes: u64 = fields.next()?.parse().ok()?;
    let cover_bytes: u64 = fields.next()?.parse().ok()?;
    // 第四段是后来加的「这轮读全了没」：老记录只有三段，按完整处理。
    let incomplete = match fields.next() {
        None => false,
        Some("0") => false,
        Some("1") => true,
        Some(_) => return None,
    };
    // 只认 1970–2096：再往后一律当坏记录，别让一个坏数字把 `SystemTime` 加起来炸掉。
    if fields.next().is_some() || !(0..=4_000_000_000).contains(&secs) {
        return None;
    }
    let completed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64);
    Some(MaintenanceMark {
        completed_at,
        audio_bytes,
        cover_bytes,
        incomplete,
    })
}

/// 写下这轮维护的结论。写失败不影响本轮结果，最坏下次启动多扫一轮。
pub fn save_mark(mark: &MaintenanceMark) -> std::io::Result<()> {
    save_mark_in(&root_dir(), mark)
}

pub(crate) fn save_mark_in(root: &Path, mark: &MaintenanceMark) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let secs = mark
        .completed_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let incomplete = u8::from(mark.incomplete);
    let text = format!(
        "{secs}\t{}\t{}\t{incomplete}\n",
        mark.audio_bytes, mark.cover_bytes
    );
    write_cache_file_atomic(&root.join(MAINTENANCE_MARK_FILE), text.as_bytes())
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

/// 解析 sidecar `{档位}\t{字节}\t{0|1}\t{档位分数}\t{已探测目标档位}\t{探测时间}\t{补整曲尝试时间}\t{未知档探测目标}\t{探测时间}\t{账号范围}`，
/// 与实际字节数核对后给出完整记录。
///
/// 尺寸不符或档位为空一律返回 `None`。
/// 第四到第十个字段都是后加的：旧格式缺少哪个，对应字段就是 `None`，
/// 不影响前三字段的命中判定。第 4 字段也可能是空串——档位未知的旧缓存
/// 记下「已探测」结论之后就是这个样子，读回来仍然是「档位未知」。
/// 第 7 字段只对试听片段有意义：它记的是「上次为补整曲重新取流的时间」，
/// 与第 5 字段「已探测过目标档位」是两件事，不能互相冒充。
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
    let unknown_probe_target = parse_rank(fields.next());
    let unknown_probe_at = fields
        .next()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let unknown_probe_scope = fields
        .next()
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_string);
    // 只改显示名，不动文件与分数：50 分档曾经叫「标准」，与 libresoda 的 standard(70) 撞名。
    let quality = normalize_quality_label(&quality, actual_rank);
    Some(SidecarMeta {
        quality,
        preview,
        actual_rank,
        checked_rank,
        checked_at,
        preview_attempt_at,
        unknown_probe_target,
        unknown_probe_at,
        unknown_probe_scope,
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

/// 一条 sidecar 记录。前四个字段是旧格式，后面分别记录确认探测、
/// 试听补整曲尝试，以及档位未知时的独立探测退避。
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
    /// 档位未知时探测的目标档位
    pub unknown_probe_target: Option<i64>,
    /// 档位未知时的探测时间（Unix 秒）
    pub unknown_probe_at: Option<u64>,
    /// 档位未知时的账号与会员范围指纹
    pub unknown_probe_scope: Option<String>,
}

impl SidecarMeta {
    /// 档位未知时仍有效的探测退避；字段缺失、过期或时钟回拨时返回 `None`。
    pub fn fresh_unknown_probe(&self, now: u64) -> Option<UnknownProbe> {
        let target_rank = self.unknown_probe_target?;
        let at = self.unknown_probe_at?;
        let scope = self.unknown_probe_scope.as_deref()?;
        if self.preview
            || target_rank <= 0
            || at == 0
            || scope.trim().is_empty()
            || now < at
            || now - at >= UNKNOWN_PROBE_TTL.as_secs()
        {
            return None;
        }
        Some(UnknownProbe {
            target_rank,
            at,
            scope: scope.to_string(),
        })
    }

    /// 仍然有效的「已探测过该目标档位」分数；没记录、过期或时钟回拨时返回 `None`。
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

/// 已经问过的记录是否覆盖本轮目标档位：记录的目标不低于本次目标，就不必再问。
///
/// 记录的是取流时的**上限**：请求 110 时服务端会在不超过 110 的候选里挑最优，
/// 所以「110 没拿到高于 N 的流」也证明更低目标 N 不会有更好的流；反过来只问过低目标
/// 不能代表更高目标。杜比 88 与无损 100 也按 libresoda 的同一套刻度排序。
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
    let unknown_probe_tail = sidecar_unknown_probe_tail(&fields);
    let content = match preview_attempt {
        Some(attempted_at) => format!(
            "{}\t{}\t{preview_field}\t{rank_field}\t{checked_rank}\t{now}\t{attempted_at}{unknown_probe_tail}",
            quality_field,
            fields[1].trim(),
        ),
        None if unknown_probe_tail.is_empty() => format!(
            "{}\t{}\t{preview_field}\t{rank_field}\t{checked_rank}\t{now}",
            quality_field,
            fields[1].trim(),
        ),
        None => format!(
            "{}\t{}\t{preview_field}\t{rank_field}\t{checked_rank}\t{now}\t{unknown_probe_tail}",
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
    let unknown_probe_tail = sidecar_unknown_probe_tail(&fields);
    let content = format!(
        "{}\t{}\t1\t{rank_field}\t{checked_rank}\t{checked_at}\t{now}{unknown_probe_tail}",
        quality_field,
        fields[1].trim(),
    );
    replace_sidecar_file(sidecar_path, &content).map(|()| true)
}

fn sidecar_unknown_probe_tail(fields: &[&str]) -> String {
    fields
        .iter()
        .skip(7)
        .take(3)
        .map(|field| format!("\t{field}"))
        .collect()
}

/// 在 sidecar 上记录一次独立探测退避，不改实际档位与确认探测字段。
///
/// `expected_actual_rank` 的用法与 [`record_upgrade_check`] 相同；试听或文件已被替换时
/// 直接跳过。调用方必须持有该文件的 `download_lock`。
pub(crate) fn record_unknown_probe(
    sidecar_path: &Path,
    expected_actual_rank: Option<i64>,
    target_rank: i64,
    scope: &str,
    now: u64,
) -> std::io::Result<bool> {
    let scope = scope.trim();
    if target_rank <= 0 || scope.is_empty() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(sidecar_path)?;
    let trimmed = text.trim_end_matches(['\r', '\n']);
    let fields: Vec<&str> = trimmed.split('\t').collect();
    if fields.len() < 2 || fields[1].trim().parse::<u64>().is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sidecar is missing the quality or size field",
        ));
    }
    let actual_rank = parse_rank(fields.get(3).copied());
    if actual_rank != expected_actual_rank {
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
    if preview_field == "1" {
        return Ok(false);
    }
    let rank_field = fields.get(3).copied().unwrap_or("");
    let checked_rank = fields.get(4).copied().unwrap_or("");
    let checked_at = fields.get(5).copied().unwrap_or("");
    let preview_attempt_at = fields.get(6).copied().unwrap_or("").trim();
    let content = format!(
        "{quality_field}\t{}\t{preview_field}\t{rank_field}\t{checked_rank}\t{checked_at}\t{preview_attempt_at}\t{target_rank}\t{now}\t{scope}",
        fields[1].trim(),
    );
    replace_sidecar_file(sidecar_path, &content)?;
    Ok(true)
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
    /// 仍在有效期内、已探测过的目标档位分数；过期记录不入此字段
    pub checked_rank: Option<i64>,
    /// 档位未知时仍在有效期内的探测退避
    pub unknown_probe: Option<UnknownProbe>,
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
    /// 排除试听片段、已达到或超过目标的记录，以及最近已经探测过该目标档位的记录：
    /// 档位未知时还排除同账号范围、同目标的独立退避，避免每轮批量都重新探测。
    /// 试听片段仍不纳入批量升级（#130）：补整曲由播放命中时的自动路径负责，
    /// 并有自己的退避记录；批量升级纳入与否是单独的产品决定，不能在这里悄悄扩大流量。
    /// 档位未知的旧缓存计入可升级——它证明不了自己达标；其中标签已证明达标的
    /// （旧「无损」标签）由 [`CachedAudioRecord::needs_higher_stream`] 挡掉，不白花流量。
    /// 同时受该缓存文件自身的 quality_tag 意图上限约束：
    /// 例如用户在省流模式下缓存的 `-low.m4a`（上限 50），不会被冲高为无损（100），
    /// 以免后续省流播放时命中数十兆的庞大文件破坏省流意图。
    pub fn is_upgradeable_to(&self, target_rank: i64, scope: Option<&str>) -> bool {
        let within_cap = crate::quality::QualityIntent::parse(&self.quality_tag)
            .max_rank()
            .is_none_or(|cap| target_rank <= cap);
        !self.preview
            && within_cap
            && self.needs_higher_stream(target_rank)
            && !upgrade_check_covers(self.checked_rank, Some(target_rank))
            && !self
                .unknown_probe
                .as_ref()
                .is_some_and(|probe| probe.covers(target_rank, scope))
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
    let now = now_epoch_secs();
    let checked_rank = meta.fresh_checked_rank(now);
    let unknown_probe = meta.fresh_unknown_probe(now);
    Some(CachedAudioRecord {
        track_id,
        quality_tag,
        path: m4a_path.to_owned(),
        quality_label: meta.quality,
        preview: meta.preview,
        actual_rank: meta.actual_rank,
        checked_rank,
        unknown_probe,
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
pub fn estimate_upgrade(
    records: &[CachedAudioRecord],
    target_rank: i64,
    scope: Option<&str>,
) -> UpgradeEstimate {
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
        // 试听片段、超出意图上限，以及同目标的确认探测或未知档退避都不计入升级。
        if record.is_upgradeable_to(target_rank, scope) {
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
    use std::sync::atomic::AtomicU64;
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
        assert_eq!(clear_in(dir.path()).removed, 3);
        assert!(audio.exists());
        assert!(audio.with_extension("quality").exists());
        assert!(dir.path().join("audio/download.part").exists());
        drop(lease);
        assert_eq!(clear_in(dir.path()).removed, 2);
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

        assert_eq!(clear_in(&cache).removed, 2);
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
        assert_eq!(trim_audio_dir(dir.path(), 40), 40);
        assert!(!old.exists());
        assert!(!old.with_extension("quality").exists());
        assert!(active.exists() && recent.exists());
        assert_eq!(trim_audio_dir(dir.path(), 0), 20);
        drop(lease);
        assert_eq!(trim_audio_dir(dir.path(), 0), 0);
    }

    /// #217：没超上限一个都不删，统计口径含原图子目录里的文件。
    #[test]
    fn trim_covers_below_limit_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let thumb = file(dir.path(), "covers/0000000000000001-160.img", 100);
        let original = file(dir.path(), "covers/original/0000000000000001.img", 100);
        let (remaining, removed) = trim_covers_dir(dir.path(), 1000, &HashMap::new());
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

        let (remaining, removed) = trim_covers_dir(dir.path(), 20, &HashMap::new());
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

        let (remaining, removed) = trim_covers_dir(dir.path(), 20, &last_seen);
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

        let (remaining, removed) = trim_covers_dir(dir.path(), 10, &last_seen);
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

        assert_eq!(trim_covers_dir(dir.path(), 10, &HashMap::new()).0, 10);
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
            trim_covers_dir(&dir.path().join("missing"), 0, &HashMap::new()).0,
            0
        );
        assert_eq!(trim_covers_dir(dir.path(), 0, &HashMap::new()).0, 0);
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
        scan_with(root, denying(denied))
    }

    fn denying(denied: &Path) -> impl FnMut(&Path) -> ListingResult + Copy + '_ {
        move |path| {
            if path == denied {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "denied",
                ))
            } else {
                list_dir(path)
            }
        }
    }

    /// 读不全时记下缺口在哪一层；目录整个打不开就是 `dir`。
    #[test]
    fn scan_records_where_it_could_not_read() {
        let dir = tempfile::tempdir().unwrap();
        file(dir.path(), "audio/keep.m4a", 0);
        // 目录得先存在，扫描才会去列它——不然拒绝闭包一次都不会被调用。
        file(dir.path(), "audio/staging/two.m4a.part", 0);
        let audio = dir.path().join("audio");
        let denied = audio.join("staging");
        let scan = scan_denying(dir.path(), &denied);

        assert!(scan.incomplete);
        assert_eq!(scan.gap, Some(ScanGap::Dir), "整层目录打不开要记成 dir");
        assert_eq!(
            scan.gap_paths,
            vec![denied.clone()],
            "缺口路径要留下来，判断某一层看没看全靠它"
        );
        assert!(!scan.subtree_fully_listed(&audio), "缺口就在这棵子树里");
        assert!(!scan.subtree_fully_listed(&denied), "缺口就是这层本身");
        assert!(
            scan.subtree_fully_listed(&audio.join("other")),
            "缺口不在这棵子树里，这层看全了"
        );

        // 缺口落在上层：这层可能根本没被列到，不能当成看全了。
        let root_denied = scan_with(dir.path(), denying(&audio));
        assert!(!root_denied.subtree_fully_listed(&audio.join("staging")));
    }

    /// 用量一次给出字节、文件数、歌曲数；目录不存在算空的，不算没读全。
    #[test]
    fn usage_scan_counts_bytes_files_and_songs() {
        let dir = tempfile::tempdir().unwrap();
        file(dir.path(), "audio/one.m4a", 0);
        file(dir.path(), "audio/one.quality", 0);
        file(dir.path(), "audio/staging/two.m4a.part", 0);

        let usage = CacheUsage::scan(&dir.path().join("audio"));
        assert_eq!(usage.bytes, 30);
        assert_eq!(usage.files, 3);
        assert_eq!(usage.songs, 1, "边车和暂存不是歌");
        assert!(!usage.incomplete);

        let missing = CacheUsage::scan(&dir.path().join("missing"));
        assert_eq!(missing, CacheUsage::default(), "目录不存在是空的");
    }

    /// 整棵树一个文件都没读到，和真的空树要分得开：前者是「不知道」，不是 0。
    #[test]
    fn usage_tells_an_unreadable_tree_from_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        file(dir.path(), "audio/keep.m4a", 0);
        let audio = dir.path().join("audio");
        let denied = audio.join("staging");
        file(dir.path(), "audio/staging/two.m4a.part", 0);

        let partial = CacheUsage::of_scan(&audio, scan_denying(dir.path(), &denied));
        assert!(partial.incomplete);
        assert!(!partial.unreadable(), "读到了东西就不是整棵读不到");

        let nothing = CacheUsage::of_scan(&audio, scan_with(dir.path(), denying(&audio)));
        assert!(nothing.incomplete);
        assert!(nothing.unreadable(), "一个文件都没读到，不能当成空树");

        let empty = CacheUsage::scan(&dir.path().join("missing"));
        assert!(!empty.unreadable(), "目录不存在是空的，不是读不到");
    }

    /// 同一目录的元数据缺口只留第一条，但兄弟文件的缺口不能算到整层头上。
    #[test]
    fn scan_gaps_keep_one_entry_per_directory() {
        let staging = Path::new("/cache/audio/staging");
        let mut scan = Scan::default();
        scan.mark_incomplete(ScanGap::Metadata, Path::new("/cache/audio/a.m4a"));
        scan.mark_incomplete(ScanGap::Metadata, Path::new("/cache/audio/b.m4a"));
        assert_eq!(scan.gap_paths, vec![PathBuf::from("/cache/audio/a.m4a")]);
        assert_eq!(scan.gap, Some(ScanGap::Metadata), "层名仍是第一处");
        assert!(
            scan.subtree_fully_listed(staging),
            "兄弟文件的元数据缺口不算 staging 的缺口"
        );

        scan.mark_incomplete(ScanGap::Metadata, Path::new("/cache/audio/staging/x.part"));
        assert!(
            !scan.subtree_fully_listed(staging),
            "staging 里的文件读不到，这层就不算看全"
        );

        scan.mark_incomplete(ScanGap::Dir, Path::new("/cache/audio/other"));
        assert_eq!(
            scan.gap_paths,
            vec![
                PathBuf::from("/cache/audio/a.m4a"),
                PathBuf::from("/cache/audio/staging/x.part"),
                PathBuf::from("/cache/audio/other"),
            ]
        );
    }

    /// 清空照常删看得到的文件，但要把「这轮没读全」交回去。
    #[test]
    fn clear_report_says_when_some_files_could_not_be_seen() {
        let dir = tempfile::tempdir().unwrap();
        let old = file(dir.path(), "audio/old.m4a", 90_000);
        let hidden = file(dir.path(), "audio/staging/hidden.m4a.part", 90_000);
        let denied = dir.path().join("audio/staging");

        let report = clear_with(dir.path(), denying(&denied));

        assert_eq!(report.removed, 1, "看得到的老文件照常清");
        assert!(report.incomplete, "没读到的要报出来，别报成清干净了");
        assert!(!old.exists());
        assert!(hidden.exists(), "读不到的目录里的文件原样保留");
    }

    /// 对目录跑一次封面淘汰（测试用：告警门独立，不碰进程级状态）。
    fn trim_covers_dir(
        dir: &Path,
        limit_bytes: u64,
        last_seen: &HashMap<PathBuf, SystemTime>,
    ) -> (u64, Vec<PathBuf>) {
        trim_covers_scan(
            scan_files(dir),
            limit_bytes,
            last_seen,
            &CoverAlertGate::default(),
            |_alert| {},
            |path| std::fs::remove_file(path).is_ok(),
        )
    }

    /// 对目录跑一次音频配额淘汰（测试用）。
    fn trim_audio_dir(dir: &Path, limit_bytes: u64) -> u64 {
        trim_audio_scan(scan_files(dir), limit_bytes).0
    }

    /// 对 staging 目录跑一次保留期清扫（测试用）：只认这一层的直接子项。
    fn purge_staging(staging: &Path) -> usize {
        let scan = scan_files(staging);
        let fully_listed = scan.subtree_fully_listed(staging);
        let files: Vec<ScannedFile> = scan
            .files
            .into_iter()
            .filter(|file| file.path.parent() == Some(staging))
            .collect();
        purge_stale_staging_files(
            &files,
            staging,
            STALE_STAGING_MAX_AGE,
            SystemTime::now(),
            fully_listed,
        )
        .len()
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

    /// 「动作」只在变化时上报一次：一直不完整也不会每轮写一条 Warn。
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
        assert_eq!(trim_audio_scan(scan, 20).0, 10);
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
        assert_eq!(clear_in(dir.path()).removed, 0);
        drop(clone);
        assert_eq!(clear_in(dir.path()).removed, 1);
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

    #[test]
    fn parse_sidecar_supports_legacy_and_unknown_probe_layouts() {
        for (text, size) in [
            ("无损 · 试听\t9000", 9000),
            ("无损 871k\t9000\t0", 9000),
            ("无损 871k\t9000\t0\t100", 9000),
            ("未知 128k\t9000\t1\t0\t\t\t1700000000", 9000),
        ] {
            let parsed = parse_sidecar(text, size).unwrap();
            assert_eq!(parsed.unknown_probe_target, None, "{text}");
            assert_eq!(parsed.unknown_probe_at, None, "{text}");
            assert_eq!(parsed.unknown_probe_scope, None, "{text}");
        }

        let parsed =
            parse_sidecar("极高 320k\t2000\t0\t\t\t\t\t100\t1700000000\tscope-a", 2000).unwrap();
        assert_eq!(parsed.actual_rank, None);
        assert_eq!(parsed.checked_rank, None);
        assert_eq!(parsed.unknown_probe_target, Some(100));
        assert_eq!(parsed.unknown_probe_at, Some(1_700_000_000));
        assert_eq!(parsed.unknown_probe_scope.as_deref(), Some("scope-a"));
        assert_eq!(
            parsed.fresh_unknown_probe(1_700_000_001),
            Some(UnknownProbe {
                target_rank: 100,
                at: 1_700_000_000,
                scope: "scope-a".into(),
            })
        );
    }

    #[test]
    fn unknown_probe_scope_is_stable_scoped_and_does_not_leak_account() {
        assert_eq!(unknown_probe_scope(None, Some(true)), None);
        assert_eq!(unknown_probe_scope(Some(" \t"), Some(true)), None);

        let scope = unknown_probe_scope(Some(" private-account "), Some(true)).unwrap();
        assert_eq!(scope.len(), 32);
        assert!(scope
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch)));
        assert!(!scope.contains("private-account"));
        assert_eq!(
            unknown_probe_scope(Some("private-account"), Some(true)),
            Some(scope.clone())
        );

        let non_vip = unknown_probe_scope(Some("private-account"), Some(false)).unwrap();
        let unknown_vip = unknown_probe_scope(Some("private-account"), None).unwrap();
        let other_account = unknown_probe_scope(Some("private-account-2"), Some(true)).unwrap();
        assert_ne!(scope, non_vip);
        assert_ne!(scope, unknown_vip);
        assert_ne!(non_vip, unknown_vip);
        assert_ne!(scope, other_account);
    }

    #[test]
    fn unknown_probe_freshness_checks_ttl_rollback_and_required_fields() {
        let at = 1_700_000_000;
        let meta = SidecarMeta {
            quality: "极高 320k".into(),
            preview: false,
            actual_rank: None,
            checked_rank: None,
            checked_at: None,
            preview_attempt_at: None,
            unknown_probe_target: Some(100),
            unknown_probe_at: Some(at),
            unknown_probe_scope: Some("scope-a".into()),
        };
        let probe = UnknownProbe {
            target_rank: 100,
            at,
            scope: "scope-a".into(),
        };
        assert_eq!(meta.fresh_unknown_probe(at), Some(probe.clone()));
        assert_eq!(
            meta.fresh_unknown_probe(at + UNKNOWN_PROBE_TTL.as_secs() - 1),
            Some(probe.clone())
        );
        assert_eq!(
            meta.fresh_unknown_probe(at + UNKNOWN_PROBE_TTL.as_secs()),
            None
        );
        assert_eq!(meta.fresh_unknown_probe(at - 1), None);
        let known = SidecarMeta {
            actual_rank: Some(80),
            ..meta.clone()
        };
        assert_eq!(known.fresh_unknown_probe(at + 1), Some(probe));

        for invalid in [
            SidecarMeta {
                preview: true,
                ..meta.clone()
            },
            SidecarMeta {
                unknown_probe_target: Some(0),
                ..meta.clone()
            },
            SidecarMeta {
                unknown_probe_target: Some(-1),
                ..meta.clone()
            },
            SidecarMeta {
                unknown_probe_target: None,
                ..meta.clone()
            },
            SidecarMeta {
                unknown_probe_at: Some(0),
                ..meta.clone()
            },
            SidecarMeta {
                unknown_probe_at: None,
                ..meta.clone()
            },
            SidecarMeta {
                unknown_probe_scope: Some(String::new()),
                ..meta.clone()
            },
            SidecarMeta {
                unknown_probe_scope: None,
                ..meta.clone()
            },
        ] {
            assert!(invalid.fresh_unknown_probe(at + 1).is_none());
        }
    }

    /// 写一对缓存文件（音频 + sidecar）。sidecar 内容原样落盘。
    fn write_cached(dir: &Path, stem: &str, sidecar: &str, bytes: usize) {
        std::fs::write(dir.join(format!("{stem}.m4a")), vec![0u8; bytes]).unwrap();
        std::fs::write(dir.join(format!("{stem}.quality")), sidecar).unwrap();
    }

    #[test]
    fn read_cache_record_loads_only_fresh_unknown_probes() {
        let dir = tempfile::tempdir().unwrap();
        let now = now_epoch_secs();
        write_cached(
            dir.path(),
            "7005-auto",
            &format!("极高 320k\t3000\t0\t\t\t\t\t100\t{now}\tscope-a"),
            3000,
        );
        write_cached(
            dir.path(),
            "7006-auto",
            &format!(
                "极高 320k\t3000\t0\t\t\t\t\t100\t{}\tscope-a",
                now + UNKNOWN_PROBE_TTL.as_secs()
            ),
            3000,
        );

        let records = list_cached_audio(dir.path());
        let fresh = records
            .iter()
            .find(|record| record.track_id == "7005")
            .unwrap();
        assert_eq!(fresh.actual_rank, None);
        assert_eq!(fresh.checked_rank, None);
        assert_eq!(
            fresh.unknown_probe,
            Some(UnknownProbe {
                target_rank: 100,
                at: now,
                scope: "scope-a".into(),
            })
        );
        let rolled_back = records
            .iter()
            .find(|record| record.track_id == "7006")
            .unwrap();
        assert_eq!(rolled_back.unknown_probe, None);
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
        let est = estimate_upgrade(&records, 100, None);
        assert_eq!(est.upgradeable_count, 1);
        assert_eq!(
            est.unknown_rank_count, 3,
            "发现的未知档位与本轮能不能升无关（试听那段不报）"
        );
        assert_eq!(est.current_bytes, 3000);

        // 目标 Hi-Res 无损（110）：无损标签只证明到 100，三首都要重新探。
        let est_hires = estimate_upgrade(&records, 110, None);
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
        let est_lossless = estimate_upgrade(&records, 100, None);
        assert_eq!(est_lossless.upgradeable_count, 1);
        assert_eq!(est_lossless.current_bytes, 1000);
        assert_eq!(est_lossless.estimated_traffic_bytes, 35 * 1024 * 1024);
        assert_eq!(
            est_lossless.net_disk_delta_bytes(),
            (35 * 1024 * 1024 - 1000) as i64
        );

        // 目标为流畅（50）：两者都已达到，没有可升级
        let est_standard = estimate_upgrade(&records, 50, None);
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
        assert_eq!(trim_audio_dir(dir.path(), 0), 30);
        // 正在升级的 target、sidecar 以及 part 均被保护
        assert!(target.exists());
        assert!(target.with_extension("quality").exists());
        assert!(part.exists());

        // 清空缓存操作同样不能删除租约文件
        assert_eq!(clear_in(dir.path()).removed, 0);
        assert!(target.exists());
        assert!(part.exists());

        // 升级完成后释放租约
        drop(target_lease);
        drop(part_lease);

        // 释放租约后，淘汰可以正常回收 target 与 sidecar（临时 part 仍受 24h 保护机制保持）
        assert_eq!(trim_audio_dir(dir.path(), 0), 10);
        assert!(!target.exists());
        assert!(!target.with_extension("quality").exists());
        assert!(part.exists());

        // 临时文件被删除后，缓存占用归零
        std::fs::remove_file(&part).unwrap();
        assert_eq!(trim_audio_dir(dir.path(), 0), 0);
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
                unknown_probe: None,
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
                unknown_probe: None,
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
                unknown_probe: None,
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
                unknown_probe: None,
                file_size: 1024 * 1024,
            },
        ];
        let est = estimate_upgrade(&records, 100, None);
        // preview 试听文件不应被计入升级，也不该被报成「档位未知的旧缓存」。
        assert_eq!(est.upgradeable_count, 1);
        assert_eq!(est.unknown_rank_count, 0);
        assert_eq!(est.current_bytes, 4 * 1024 * 1024);
        assert!(!records[1].is_upgradeable_to(100, None));
        assert!(!records[2].is_upgradeable_to(100, None));
        assert!(!records[3].is_upgradeable_to(100, None));
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
            unknown_probe: None,
            file_size: 4 * 1024 * 1024,
        };
        let unreadable = record("1", "未知 128k", Some(0));
        assert_eq!(unreadable.actual_tier(), Some("未知"));
        assert!(!unreadable.needs_higher_stream(100));
        assert!(!unreadable.is_upgradeable_to(100, None));

        let records = vec![
            unreadable,
            record("2", "极高 320k", None),
            record("3", "流畅 128k", Some(50)),
        ];
        let est = estimate_upgrade(&records, 100, None);
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
            unknown_probe: None,
            file_size: 4 * 1024 * 1024,
        };
        // auto 无上限，actual 50 可以升级至 100
        assert!(auto_record.is_upgradeable_to(100, None));

        let low_record = CachedAudioRecord {
            track_id: "2".into(),
            quality_tag: "low".into(),
            path: PathBuf::from("2-low.m4a"),
            quality_label: "流畅 128k".into(),
            preview: false,
            actual_rank: Some(20),
            checked_rank: None,
            unknown_probe: None,
            file_size: 4 * 1024 * 1024,
        };
        // low 标签上限为 50，不允许越级升级至 100（无损）或 80（极高）
        assert!(!low_record.is_upgradeable_to(100, None));
        assert!(!low_record.is_upgradeable_to(80, None));
        // 目标为 50 且实际为 20 时，在 low 上限内，允许升级至 50
        assert!(low_record.is_upgradeable_to(50, None));
        // 目标为 20 时实际已达 20，不允许升级
        assert!(!low_record.is_upgradeable_to(20, None));

        let lossless_downgraded = CachedAudioRecord {
            track_id: "3".into(),
            quality_tag: "lossless".into(),
            path: PathBuf::from("3-lossless.m4a"),
            quality_label: "极高 320k".into(),
            preview: false,
            actual_rank: Some(50),
            checked_rank: None,
            unknown_probe: None,
            file_size: 10 * 1024 * 1024,
        };
        // lossless 标签上限为 100，实际为 50（曾发生降档），允许升级至 100
        assert!(lossless_downgraded.is_upgradeable_to(100, None));
    }

    /// #90：探测过「该目标没有更高流」的档位不再计入升级与预估，但更高目标仍可探测。
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
            unknown_probe: None,
            file_size: 10 * 1024 * 1024,
        };
        // 已经问过无损、没拿到更高流：同一个目标不再计入
        assert!(!confirmed.is_upgradeable_to(100, None));
        // 更高目标还没问过，仍要允许探测：历史结论不能代表更高档位
        assert!(confirmed.is_upgradeable_to(110, None));

        let low_confirmed = CachedAudioRecord {
            actual_rank: Some(50),
            checked_rank: Some(70),
            ..confirmed.clone()
        };
        // 只确认过 70 档：再问 70 没有意义，问 80 仍应尝试
        assert!(!low_confirmed.is_upgradeable_to(70, None));
        assert!(low_confirmed.is_upgradeable_to(80, None));

        let estimate = estimate_upgrade(&[confirmed], 100, None);
        assert_eq!(estimate.upgradeable_count, 0);

        assert!(upgrade_check_covers(Some(100), Some(100)));
        assert!(!upgrade_check_covers(Some(70), Some(80)));
        assert!(!upgrade_check_covers(None, Some(80)));
        assert!(!upgrade_check_covers(Some(80), None));
    }

    #[test]
    fn unknown_probe_suppresses_only_the_same_target_and_scope() {
        let record = CachedAudioRecord {
            track_id: "1".into(),
            quality_tag: "auto".into(),
            path: PathBuf::from("1-auto.m4a"),
            quality_label: "极高 320k".into(),
            preview: false,
            actual_rank: None,
            checked_rank: None,
            unknown_probe: Some(UnknownProbe {
                target_rank: 100,
                at: 1_700_000_000,
                scope: "scope-a".into(),
            }),
            file_size: 10 * 1024 * 1024,
        };
        let probe = record.unknown_probe.as_ref().unwrap();
        assert!(probe.covers(100, Some("scope-a")));
        assert!(!probe.covers(100, None));
        assert!(!probe.covers(110, Some("scope-a")));
        assert!(!probe.covers(100, Some("scope-b")));

        assert!(!record.is_upgradeable_to(100, Some("scope-a")));
        assert!(record.is_upgradeable_to(110, Some("scope-a")));
        assert!(record.is_upgradeable_to(100, Some("scope-b")));
        assert!(record.is_upgradeable_to(100, None));

        let suppressed = estimate_upgrade(std::slice::from_ref(&record), 100, Some("scope-a"));
        assert_eq!(suppressed.upgradeable_count, 0);
        assert_eq!(suppressed.unknown_rank_count, 1);
        assert_eq!(suppressed.current_bytes, 0);
        for (target, scope) in [(110, Some("scope-a")), (100, Some("scope-b")), (100, None)] {
            let estimate = estimate_upgrade(std::slice::from_ref(&record), target, scope);
            assert_eq!(estimate.upgradeable_count, 1);
            assert_eq!(estimate.unknown_rank_count, 1);
            assert_eq!(estimate.current_bytes, 10 * 1024 * 1024);
        }
    }

    /// 杜比 / Hi-Res 的探测结论也要挡住批量与预估，不能再整首重下一遍。
    #[test]
    fn high_tier_check_skips_upgrade_and_estimate() {
        let confirmed = CachedAudioRecord {
            track_id: "1".into(),
            quality_tag: "hires".into(),
            path: PathBuf::from("1-hires.m4a"),
            quality_label: "无损 871k".into(),
            preview: false,
            actual_rank: Some(100),
            checked_rank: Some(110),
            unknown_probe: None,
            file_size: 10 * 1024 * 1024,
        };
        assert!(!confirmed.is_upgradeable_to(110, None));
        let estimate = estimate_upgrade(std::slice::from_ref(&confirmed), 110, None);
        assert_eq!(estimate.upgradeable_count, 0);

        // 记录到期后（`fresh_checked_rank` 返回 None）重新算作可升级。
        let expired = CachedAudioRecord {
            checked_rank: None,
            ..confirmed
        };
        assert!(expired.is_upgradeable_to(110, None));
        assert_eq!(estimate_upgrade(&[expired], 110, None).upgradeable_count, 1);
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
            unknown_probe_target: None,
            unknown_probe_at: None,
            unknown_probe_scope: None,
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
            unknown_probe_target: None,
            unknown_probe_at: None,
            unknown_probe_scope: None,
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

    #[test]
    fn record_unknown_probe_writes_backoff_without_claiming_rank() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("song-auto.quality");
        std::fs::write(&sidecar, "极高 320k\t2000\t0").unwrap();

        assert!(record_unknown_probe(&sidecar, None, 100, "scope-a", 1_700_000_000).unwrap());
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(text, "极高 320k\t2000\t0\t\t\t\t\t100\t1700000000\tscope-a");
        let parsed = parse_sidecar(&text, 2000).unwrap();
        assert_eq!(parsed.actual_rank, None);
        assert_eq!(parsed.checked_rank, None);
        assert_eq!(parsed.checked_at, None);
        assert_eq!(parsed.unknown_probe_target, Some(100));
        assert_eq!(parsed.unknown_probe_at, Some(1_700_000_000));
        assert_eq!(parsed.unknown_probe_scope.as_deref(), Some("scope-a"));

        std::fs::write(&sidecar, "极高 320k\t2000\t0\t\t70\t1600000000\t1600000100").unwrap();
        assert!(record_unknown_probe(&sidecar, None, 110, "scope-b", 1_700_000_001).unwrap());
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            "极高 320k\t2000\t0\t\t70\t1600000000\t1600000100\t110\t1700000001\tscope-b"
        );

        std::fs::write(&sidecar, "极高 320k\t2000\t0\t80").unwrap();
        assert!(record_unknown_probe(&sidecar, Some(80), 100, "scope-a", 1_700_000_002).unwrap());
        let parsed = parse_sidecar(&std::fs::read_to_string(&sidecar).unwrap(), 2000).unwrap();
        assert_eq!(parsed.actual_rank, Some(80), "旧文件的实际档位原样保留");
        assert_eq!(parsed.checked_rank, None);
        assert_eq!(parsed.unknown_probe_target, Some(100));
    }

    #[test]
    fn record_unknown_probe_skips_previews_changed_rank_and_missing_scope() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("song-auto.quality");
        for (original, expected_rank, target, scope) in [
            ("未知 128k\t2000\t1", None, 100, "scope-a"),
            ("无损 · 试听\t2000", None, 100, "scope-a"),
            ("流畅 128k\t2000\t0\t50", Some(80), 100, "scope-a"),
            ("极高 320k\t2000\t0", Some(80), 100, "scope-a"),
            ("极高 320k\t2000\t0", None, 0, "scope-a"),
            ("极高 320k\t2000\t0", None, 100, ""),
            ("极高 320k\t2000\t0", None, 100, " \t"),
        ] {
            std::fs::write(&sidecar, original).unwrap();
            let recorded =
                record_unknown_probe(&sidecar, expected_rank, target, scope, 1_700_000_000)
                    .unwrap();
            assert!(!recorded);
            assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), original);
        }

        let missing = dir.path().join("missing.quality");
        assert!(!record_unknown_probe(&missing, None, 0, "scope-a", 1).unwrap());
        assert!(!record_unknown_probe(&missing, None, 100, "", 1).unwrap());
        assert!(!missing.exists());
    }

    #[test]
    fn other_sidecar_writers_preserve_unknown_probe_fields() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("song-auto.quality");
        std::fs::write(
            &sidecar,
            "未知 128k\t9000\t1\t0\t\t\t1700000000\t100\t1700000100\tscope-a",
        )
        .unwrap();

        assert!(record_preview_attempt(&sidecar, Some(0), 1_700_000_200).unwrap());
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            "未知 128k\t9000\t1\t0\t\t\t1700000200\t100\t1700000100\tscope-a"
        );

        record_upgrade_check(&sidecar, Some(0), 110, 1_700_000_300).unwrap();
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(
            text,
            "未知 128k\t9000\t1\t0\t110\t1700000300\t1700000200\t100\t1700000100\tscope-a"
        );
        let parsed = parse_sidecar(&text, 9000).unwrap();
        assert_eq!(parsed.preview_attempt_at, Some(1_700_000_200));
        assert_eq!(parsed.unknown_probe_target, Some(100));
        assert_eq!(parsed.unknown_probe_at, Some(1_700_000_100));
        assert_eq!(parsed.unknown_probe_scope.as_deref(), Some("scope-a"));

        // 第 7 字段为空时也要留位，不能把后面的未知档退避整体左移。
        std::fs::write(
            &sidecar,
            "极高 320k\t9000\t0\t\t\t\t\t100\t1700000100\tscope-a",
        )
        .unwrap();
        record_upgrade_check(&sidecar, None, 110, 1_700_000_300).unwrap();
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(
            text,
            "极高 320k\t9000\t0\t\t110\t1700000300\t\t100\t1700000100\tscope-a"
        );
        let parsed = parse_sidecar(&text, 9000).unwrap();
        assert_eq!(parsed.preview_attempt_at, None);
        assert_eq!(parsed.unknown_probe_target, Some(100));
        assert_eq!(parsed.unknown_probe_at, Some(1_700_000_100));
        assert_eq!(parsed.unknown_probe_scope.as_deref(), Some("scope-a"));
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

    /// 同一个暂存目标的文件共享保护键：租约挂在暂存目标上时，
    /// 可续传路径写下的整族文件（数据 / 记录 / 记录临时 / 锁 / 解密临时）
    /// 都不会被维护线程当废弃文件删掉。
    #[test]
    fn audio_staging_files_share_the_target_lease() {
        let target = Path::new("/cache/audio/staging/123-lossless-normal.m4a");
        for staged in [
            "/cache/audio/staging/123-lossless-normal.m4a.part",
            "/cache/audio/staging/123-lossless-normal.m4a.part.meta",
            "/cache/audio/staging/123-lossless-normal.m4a.part.meta.tmp",
            "/cache/audio/staging/123-lossless-normal.m4a.part.lock",
            "/cache/audio/staging/123-lossless-normal.m4a.dec.tmp",
            "/cache/audio/staging/123-lossless-normal.m4a.part.dec.tmp",
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

    /// 可续传的暂存整族在 24 小时窗口内都算「还在用」：数据和记录少一边，
    /// 下次就接不上上次那段字节了。
    #[test]
    fn resumable_staging_family_counts_as_fresh_within_the_window() {
        let dir = "/cache/audio/staging/123-lossless-normal.m4a";
        for staged in [
            format!("{dir}.part"),
            format!("{dir}.part.meta"),
            format!("{dir}.part.meta.tmp"),
            format!("{dir}.dec.tmp"),
        ] {
            assert!(
                is_fresh_part(Path::new(&staged), SystemTime::now()),
                "{staged} 在窗口内必须受保护"
            );
        }
        // 过了 24 小时窗口就不再保护：残留交给配额淘汰、保留期清扫
        // 与 libresoda 的 7 天清扫。
        let old = SystemTime::now() - PART_MAX_AGE;
        assert!(!is_fresh_part(Path::new(&format!("{dir}.part")), old));
        assert!(!is_fresh_part(Path::new(&format!("{dir}.part.meta")), old));
    }

    /// 超过保留期的暂存残留由维护无条件清掉：交付目标、数据、记录、解密临时
    /// 都算残留；`.part.lock` 与租约保护的整族除外（#243）。
    #[test]
    fn purge_stale_staging_removes_expired_residue_but_spares_lock_and_leases() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join(STAGING_DIR_NAME);
        let old = STALE_STAGING_MAX_AGE.as_secs() + 60;
        let expired_target = file(&staging, "123-lossless-normal.m4a", old);
        let expired_data = file(&staging, "123-lossless-normal.m4a.part", old);
        let expired_meta = file(&staging, "123-lossless-normal.m4a.part.meta", old);
        let expired_tmp = file(&staging, "123-lossless-normal.m4a.dec.tmp", old);
        let stale_lock = file(&staging, "123-lossless-normal.m4a.part.lock", old);
        let fresh_data = file(&staging, "456-high-normal.m4a.part", 0);
        // 混合年龄的族：数据过期、记录还年轻（中断重写了记录）——整族不动，
        // 留给下一次下载由 libresoda 的 load_record 判定。
        let mixed_data = file(&staging, "666-lossless-normal.m4a.part", old);
        let mixed_record = file(&staging, "666-lossless-normal.m4a.part.meta", 0);
        // 租约挂在暂存目标上时，它的整族过期文件也受保护。`_lease` 只是为了
        // 让租约活到断言结束：这里不需要读它。
        let leased_target = file(&staging, "789-lossless-normal.m4a", old);
        let leased_data = file(&staging, "789-lossless-normal.m4a.part", old);
        let _lease = CacheLease::acquire(&leased_target);

        let removed = purge_staging(&staging);

        assert_eq!(removed, 4, "删掉的：交付目标 + 数据 + 记录 + 解密临时");
        assert!(!expired_target.exists(), "过期的交付目标必须清掉");
        assert!(!expired_data.exists(), "过期的 .part 必须清掉");
        assert!(!expired_meta.exists(), "过期的记录必须清掉");
        assert!(!expired_tmp.exists(), "过期的解密临时必须清掉");
        assert!(
            stale_lock.exists(),
            ".part.lock 的 mtime 停在首次下载，年久不代表失效，永不动"
        );
        assert!(fresh_data.exists(), "保留期窗口内的暂存不能动");
        assert!(
            mixed_data.exists() && mixed_record.exists(),
            "族里任一成员还年轻就整族不动，不在边界上拆开续传对"
        );
        assert!(
            leased_target.exists() && leased_data.exists(),
            "租约保护的整族（正在下载/待交付）不能动"
        );
    }

    /// `.part.lock` 不走任何删除路径：配额淘汰只看 mtime 会把复用中的锁
    /// 误判成残留，跨进程时租约又看不见内核锁。
    #[test]
    fn quota_eviction_never_touches_staged_lock_files() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join(STAGING_DIR_NAME);
        let lock = file(&staging, "123-lossless-normal.m4a.part.lock", 90_000);
        assert!(!remove_idle(&lock), "过期的锁也不能由维护删");
        assert!(lock.exists(), "锁文件只能由 libresoda 的下载路径作废");
    }

    /// 配额淘汰按整族判新鲜：记录年轻、数据过期的续传对整族存活，
    /// 不能在配额压力下删掉 `.part` 留下对不上的记录（#243）。
    #[test]
    fn quota_eviction_keeps_mixed_age_staging_family_whole() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join(STAGING_DIR_NAME);
        let old_data = file(&staging, "123-lossless-normal.m4a.part", 90_000);
        let fresh_meta = file(&staging, "123-lossless-normal.m4a.part.meta", 0);
        let victim = file(&staging, "456-high-normal.m4a.part", 90_000);
        // 三份各 10 字节，限 10 字节：唯一可淘汰的是全过期的 victim 那组。
        // 受保护的族留在总量里，淘汰合法地停在额度之上——与租约文件
        // 「可暂时超额」是同一条既有语义。
        let remaining = trim_audio_scan(scan_files(dir.path()), 10).0;
        assert_eq!(remaining, 20);
        assert!(!victim.exists(), "全过期的族照常按 LRU 淘汰");
        assert!(
            old_data.exists() && fresh_meta.exists(),
            "混合年龄的族整族存活：删了数据就接不上了"
        );
    }

    /// 清缓存同样按整族保留混合年龄的续传对。
    #[test]
    fn clear_keeps_mixed_age_staging_family_whole() {
        let dir = tempfile::tempdir().unwrap();
        let old_data = file(dir.path(), "staging/123-lossless-normal.m4a.part", 90_000);
        let fresh_meta = file(dir.path(), "staging/123-lossless-normal.m4a.part.meta", 0);
        assert_eq!(
            clear_in(dir.path()).removed,
            0,
            "族里有新鲜成员就一件都不删"
        );
        assert!(old_data.exists() && fresh_meta.exists(), "清缓存不拆续传对");
    }

    /// 半家子：只有数据没有记录（或者只有记录）的残留也认成一家。
    /// 认族只看保护键，配套文件齐不齐是另一回事；过期了照清，锁不动。
    #[test]
    fn purge_removes_half_family_residue_but_spares_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join(STAGING_DIR_NAME);
        let old = STALE_STAGING_MAX_AGE.as_secs() + 60;
        let lone_data = file(&staging, "123-lossless-normal.m4a.part", old);
        let lone_record = file(&staging, "456-lossless-normal.m4a.part.meta", old);
        let lone_lock = file(&staging, "789-lossless-normal.m4a.part.lock", old);

        assert_eq!(purge_staging(&staging), 2, "半家子过期照清，锁不算");
        assert!(!lone_data.exists() && !lone_record.exists());
        assert!(lone_lock.exists(), "锁永不动");
    }

    /// 一家子里混着新的：暂存清扫按整族留，旧残留清扫按条看年纪。
    /// 后者是刻意的——老残留已经配不上续传，留着只占地方。
    #[test]
    fn mixed_age_family_is_kept_whole_but_legacy_sweep_goes_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let old = STALE_STAGING_MAX_AGE.as_secs() + 60;
        // 暂存里混龄的一族：数据老、记录新。
        let staged_old = file(&audio, "staging/123-lossless-normal.m4a.part", old);
        let staged_fresh = file(&audio, "staging/123-lossless-normal.m4a.part.meta", 0);
        // 音频根下同一族名的旧格式残留：同一个后缀表，但按条判龄。
        let legacy_old = file(&audio, "456-lossless-normal.m4a.part", old);
        let legacy_fresh = file(&audio, "456-lossless-normal.m4a.part.meta", 0);

        assert_eq!(
            purge_staging(&audio.join(STAGING_DIR_NAME)),
            0,
            "族里有一条年轻就整族不动"
        );
        assert!(staged_old.exists() && staged_fresh.exists());

        assert_eq!(
            sweep_legacy_residue_once(&audio),
            Some(1),
            "旧残留逐条看年纪：老的数据清掉，新的记录留着"
        );
        assert!(!legacy_old.exists());
        assert!(legacy_fresh.exists());
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

    /// 保护键的映射必须与 [`STAGED_SUFFIXES`] 同源：名单里每条尾巴挂在交付目标
    /// `.m4a` 后面都能折回交付目标。加尾巴只改数组，这条测试守住保护键不漏。
    #[test]
    fn protection_key_maps_every_staging_tail_to_the_target() {
        let target = Path::new("/cache/audio/staging/123-lossless-normal.m4a");
        for suffix in STAGED_SUFFIXES {
            let staged = format!("/cache/audio/staging/123-lossless-normal{suffix}");
            assert_eq!(protection_key(Path::new(&staged)), target, "{staged}");
        }
        // 挂在别处（封面那类 `.img.*.part`）不进映射，保持原样。
        assert_eq!(
            protection_key(Path::new("/cache/audio/abcd-160.img.123.part")),
            Path::new("/cache/audio/abcd-160.img.123.part")
        );
    }

    /// #242：音频根下旧格式暂存残留按族名清掉，锁、正式缓存、子目录一个不碰。
    #[test]
    fn legacy_residue_sweep_removes_old_staging_but_spares_locks_and_cache() {
        let dir = tempfile::tempdir().unwrap();
        // 旧格式残留（#241 之前直接落在音频根下）：过期才清。
        let stale = [
            "111-auto-4242-7.m4a.part",
            "111-auto-4242-7.m4a.part.part",
            "111-auto-4242-7.m4a.part.meta",
            "111-auto-4242-7.m4a.part.meta.tmp",
            "111-auto-4242-7.m4a.dec.tmp",
            "111-auto-4242-7.m4a.part.dec.tmp",
        ];
        for name in stale {
            file(dir.path(), name, 90_000);
        }
        // 不碰的：锁（#243 永不删）、24 小时内的残留、正式缓存与 sidecar、
        // 封面式名字、不相干文件，以及 staging 子目录里的东西（归 7 天保留期管）。
        let keep = [
            "111-auto-4242-7.m4a.part.lock",
            "222-auto.m4a.part",
            "333-auto.m4a",
            "333-auto.quality",
            "abcd-160.img",
            "notes.txt",
            "staging/444-auto-normal.m4a.part",
        ];
        for name in keep {
            let age = if name == "222-auto.m4a.part" {
                3_600
            } else {
                90_000
            };
            file(dir.path(), name, age);
        }

        assert_eq!(
            sweep_legacy_residue_once(dir.path()),
            Some(stale.len()),
            "过期的旧格式残留全部清掉"
        );
        for name in stale {
            assert!(!dir.path().join(name).exists(), "{name} 该清掉");
        }
        for name in keep {
            assert!(dir.path().join(name).exists(), "{name} 不能被巡检误删");
        }
    }

    /// #242：音频目录不存在（首次启动）按空目录处理，不算缺口。
    ///
    /// 读不全的 `None` 语义由上面的门测试固定；这里钉死「全新安装别白跑重试」。
    #[test]
    fn legacy_residue_sweep_treats_missing_dir_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            sweep_legacy_residue_once(&dir.path().join("missing")),
            Some(0)
        );
    }

    /// #242：首轮巡检没读全就跳过本轮，连续到上限就放弃并结束。
    ///
    /// 一次性门绝不能在不完整的结果上落位：读完再清；放弃后不再动文件。
    #[test]
    fn sweep_gate_skips_incomplete_rounds_until_limit_then_gives_up() {
        let mut gate = SweepGate::new();
        // 连续不完整：跳过、计数，上限前不结束。
        for _ in 0..SWEEP_MAX_INCOMPLETE_ROUNDS - 1 {
            assert_eq!(gate.advance(None), 0);
            assert!(!gate.is_finished());
        }
        // 到上限：放弃并结束，扫描与日志都有上界。
        assert_eq!(gate.advance(None), 0);
        assert!(gate.is_finished());
        // 放弃后再喂什么都不再干活。
        assert_eq!(gate.advance(Some(3)), 0);
        assert!(gate.is_finished());
    }

    /// #242：门在读完一轮后就结束，之后的调用直接返回 0。
    ///
    /// 「第二次调用返回 0」的回归保护：进程级静态测不到，这层必须钉死。
    #[test]
    fn sweep_gate_stops_after_finishing() {
        let mut gate = SweepGate::new();
        assert_eq!(gate.advance(Some(2)), 2);
        assert!(gate.is_finished());
        assert_eq!(gate.advance(Some(2)), 0, "门已关闭，不再清扫");
        assert!(gate.is_finished());
    }

    /// #242：巡检不碰租约保护下的旧格式残留，放开后下一轮清走。
    #[test]
    fn legacy_residue_sweep_respects_download_leases() {
        let dir = tempfile::tempdir().unwrap();
        // 旧格式残留挂在交付目标名下，租约也挂在同一个交付目标上。
        let target = dir.path().join("777-auto-4242-7.m4a");
        let part = file(dir.path(), "777-auto-4242-7.m4a.part", 90_000);
        let meta = file(dir.path(), "777-auto-4242-7.m4a.part.meta", 90_000);
        let lease = CacheLease::acquire(&target);

        assert_eq!(sweep_legacy_residue_once(dir.path()), Some(0));
        assert!(part.exists() && meta.exists(), "正在下载的残留不能被清");

        drop(lease);
        assert_eq!(sweep_legacy_residue_once(dir.path()), Some(2));
        assert!(!part.exists() && !meta.exists());
    }

    #[cfg(unix)]
    #[test]
    fn clear_does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = file(outside.path(), "keep.img", 0);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("outside")).unwrap();
        assert_eq!(clear_in(dir.path()).removed, 0);
        assert!(path.exists());
    }

    /// 超过上限时照旧整组淘汰，报告里的数字是删完之后的。
    #[test]
    fn maintenance_round_trims_over_limit_and_reports_after() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let covers = dir.path().join("covers");
        let old = file(dir.path(), "audio/old.m4a", 1000);
        file(dir.path(), "audio/old.quality", 1000);
        let recent = file(dir.path(), "audio/recent.m4a", 0);
        file(dir.path(), "audio/recent.quality", 0);

        let report = maintain_with(
            &audio,
            &covers,
            MaintenanceLimits {
                audio_bytes: 20,
                cover_bytes: 512 * 1024 * 1024,
            },
            &HashMap::new(),
            list_dir,
        );

        assert_eq!(report.audio_bytes, 20, "报告的是淘汰之后的字节数");
        assert_eq!(report.removed_files, 2, "整组 sidecar 一起删");
        assert_eq!(report.songs, 1);
        assert_eq!(report.audio_files, 2, "剩下的音频与边车各算一个文件");
        let summary = report.summary();
        assert_eq!(summary.audio.bytes, 20);
        assert_eq!(summary.audio.files, 2);
        assert_eq!(summary.audio.songs, 1);
        assert!(!summary.audio.incomplete);
        assert!(!old.exists() && !old.with_extension("quality").exists());
        assert!(recent.exists() && recent.with_extension("quality").exists());
    }

    /// 暂存残留和配额吃同一份扫描：过期的整族清掉，音频字节里不再算它们。
    #[test]
    fn maintenance_round_purges_staging_from_the_same_scan() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let covers = dir.path().join("covers");
        let old = STALE_STAGING_MAX_AGE.as_secs() + 60;
        let expired = file(dir.path(), "audio/staging/1-lossless-normal.m4a.part", old);
        file(
            dir.path(),
            "audio/staging/1-lossless-normal.m4a.part.meta",
            old,
        );
        let fresh = file(dir.path(), "audio/staging/2-lossless-normal.m4a.part", 0);
        file(dir.path(), "audio/a.m4a", 0);

        let report = maintain_with(
            &audio,
            &covers,
            MaintenanceLimits::from_mb(1024, 512),
            &HashMap::new(),
            list_dir,
        );

        assert_eq!(report.removed_files, 2, "过期整族一起清");
        assert!(!expired.exists());
        assert!(fresh.exists(), "保留期内的暂存不动");
        assert_eq!(report.audio_bytes, 20, "剩下的只有 a.m4a 与新鲜暂存");
        assert_eq!(report.songs, 1);
    }

    /// staging 这一层没整个看见：见过的过期族看着「整族过期」也不能清。
    #[test]
    fn maintenance_round_holds_back_visible_staging_when_the_layer_is_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let covers = dir.path().join("covers");
        let staging = audio.join("staging");
        let old = STALE_STAGING_MAX_AGE.as_secs() + 60;
        let expired = file(dir.path(), "audio/staging/1-lossless-normal.m4a.part", old);
        let expired_meta = file(
            dir.path(),
            "audio/staging/1-lossless-normal.m4a.part.meta",
            old,
        );
        file(dir.path(), "audio/keep.m4a", 0);
        file(dir.path(), "covers/one.img", 0);

        let report = maintain_with(
            &audio,
            &covers,
            MaintenanceLimits::from_mb(1024, 512),
            &HashMap::new(),
            |path: &Path| -> ListingResult {
                let mut listing = list_dir(path)?;
                if path == staging.as_path() {
                    // 这一层有条目读不到：族里可能还有一个成员没看见。
                    listing.incomplete = true;
                }
                Ok(listing)
            },
        );

        assert!(report.audio_incomplete);
        assert_eq!(report.skipped_reason, "audio_incomplete");
        assert_eq!(
            report.audio_gap_label(),
            "entries",
            "报告要写清缺口落在哪一层"
        );
        assert_eq!(report.cover_gap_label(), "none");
        assert_eq!(report.removed_files, 0, "这层没看全，过期族一件都不清");
        assert!(
            expired.exists() && expired_meta.exists(),
            "过期暂存原样留着"
        );

        let summary = report.summary();
        assert!(summary.audio.incomplete, "读不全的音频用量只是下界");
        assert_eq!(summary.audio.songs, 1);
        assert!(!summary.covers.incomplete, "封面树读全了");
        assert_eq!(summary.covers.files, 1);
    }

    /// 缺口在别处、staging 整个看见了：过期族照清，别让一条坏目录冻住 7 天清扫。
    #[test]
    fn maintenance_round_still_purges_staging_when_the_gap_is_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let covers = dir.path().join("covers");
        let old = STALE_STAGING_MAX_AGE.as_secs() + 60;
        let expired = file(dir.path(), "audio/staging/1-lossless-normal.m4a.part", old);
        let expired_meta = file(
            dir.path(),
            "audio/staging/1-lossless-normal.m4a.part.meta",
            old,
        );
        file(dir.path(), "audio/other/hidden.m4a", 0);
        let denied = audio.join("other");

        let report = maintain_with(
            &audio,
            &covers,
            MaintenanceLimits::from_mb(1024, 512),
            &HashMap::new(),
            denying(&denied),
        );

        assert!(report.audio_incomplete);
        assert_eq!(report.audio_gap_label(), "dir");
        assert_eq!(report.removed_files, 2, "staging 整层看得见，过期族照清");
        assert!(!expired.exists() && !expired_meta.exists());
    }

    /// 维护结论落盘后读得回来；内容坏了当作没有记录，最坏多扫一轮。
    #[test]
    fn maintenance_mark_round_trips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mark = MaintenanceMark {
            completed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            audio_bytes: 123,
            cover_bytes: 456,
            incomplete: true,
        };
        save_mark_in(dir.path(), &mark).unwrap();
        assert_eq!(load_mark_in(dir.path()), Some(mark));

        // 三段的老记录没有缺口标记，按「读全了」处理。
        std::fs::write(
            dir.path().join(MAINTENANCE_MARK_FILE),
            b"1700000000\t12\t34\n",
        )
        .unwrap();
        let legacy = load_mark_in(dir.path()).expect("三段老记录照样读");
        assert!(!legacy.incomplete);

        std::fs::write(dir.path().join(MAINTENANCE_MARK_FILE), b"broken").unwrap();
        assert_eq!(load_mark_in(dir.path()), None);
    }

    /// 代表规模（1000 个封面文件、1400 个音频/暂存文件）下，维护主体对每棵树
    /// 只遍历一次，四个数字来自同一次盘点。
    ///
    /// 这里打的是 `maintain_with`，不含进程内首轮的旧格式残留巡检（`maintain()` 会先
    /// 调它，那一趟只列音频根顶层、进程内只跑一次，另有单测盯着）。
    #[test]
    fn maintenance_round_scans_each_tree_once_at_representative_size() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let covers = dir.path().join("covers");
        std::fs::create_dir_all(audio.join(STAGING_DIR_NAME)).unwrap();
        std::fs::create_dir_all(covers.join("original")).unwrap();
        for index in 0..500 {
            std::fs::write(audio.join(format!("{index}.m4a")), [0u8; 8]).unwrap();
            std::fs::write(audio.join(format!("{index}.quality")), [0u8; 8]).unwrap();
            std::fs::write(covers.join(format!("{index:016}-160.img")), [0u8; 8]).unwrap();
            std::fs::write(covers.join(format!("original/{index:016}.img")), [0u8; 8]).unwrap();
        }
        for index in 0..400 {
            std::fs::write(audio.join(format!("staging/{index}.m4a.part")), [0u8; 8]).unwrap();
        }

        let visits = std::cell::RefCell::new(Vec::new());
        let report = maintain_with(
            &audio,
            &covers,
            MaintenanceLimits::from_mb(1024, 512),
            &HashMap::new(),
            |path| {
                visits.borrow_mut().push(path.to_path_buf());
                list_dir(path)
            },
        );

        let audio_visits = visits
            .borrow()
            .iter()
            .filter(|path| path.as_path() == audio.as_path())
            .count();
        let cover_visits = visits
            .borrow()
            .iter()
            .filter(|path| path.as_path() == covers.as_path())
            .count();
        assert_eq!(audio_visits, 1, "1400 个文件的音频树一轮只扫一遍");
        assert_eq!(cover_visits, 1, "1000 个文件的封面树一轮只扫一遍");
        assert_eq!(report.scanned_files, 2400);
        assert_eq!(report.songs, 500);
        assert_eq!(report.cover_files, 1000);
        assert_eq!(report.audio_bytes, 1400 * 8);
        assert_eq!(report.cover_bytes, 1000 * 8);
        assert_eq!(report.removed_files, 0);
        assert_eq!(report.skipped_reason, "below_limit");
    }

    /// 超了却全被租约挡住：报 `blocked`，别在日志里写成「没超限」。
    #[test]
    fn maintenance_round_reports_blocked_when_everything_is_leased() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let covers = dir.path().join("covers");
        let playing = file(dir.path(), "audio/playing.m4a", 0);
        file(dir.path(), "audio/playing.quality", 0);
        let lease = CacheLease::acquire(&playing);

        let report = maintain_with(
            &audio,
            &covers,
            MaintenanceLimits {
                audio_bytes: 0,
                cover_bytes: 512 * 1024 * 1024,
            },
            &HashMap::new(),
            list_dir,
        );

        assert_eq!(report.removed_files, 0, "租约保护的整组一个都不能删");
        assert_eq!(report.skipped_reason, "blocked");
        assert_eq!(report.audio_bytes, 20, "受保护的整组仍算在总量里");
        assert!(playing.exists());
        drop(lease);
    }

    /// 记录文件的坏值一律当没有记录：多一段、1970 之前、离谱的时间戳都不认。
    #[test]
    fn parse_mark_rejects_bad_values() {
        assert!(parse_mark("1700000000\t12\t34\n").is_some());
        assert!(parse_mark("1700000000\t12\t34\t1").is_some());
        assert!(parse_mark("1700000000\t12\t34\t2").is_none());
        assert!(parse_mark("1700000000\t12\t34\textra").is_none());
        assert!(parse_mark("-1\t12\t34").is_none());
        assert!(parse_mark("4000000001\t12\t34").is_none());
        assert!(parse_mark("").is_none());
        assert!(parse_mark("1700000000\t12").is_none());
    }

    /// 删了点东西但音频仍超限：`skipped_reason` 要报 `over_limit`，
    /// 不能因为「这轮删过文件」就写成 `none` 把超限状态盖掉。
    #[test]
    fn maintenance_round_reports_over_limit_after_partial_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let covers = dir.path().join("covers");
        let playing = file(dir.path(), "audio/playing.m4a", 0);
        file(dir.path(), "audio/playing.quality", 0);
        let lease = CacheLease::acquire(&playing);
        let stale = file(dir.path(), "audio/stale.m4a", 1000);
        file(dir.path(), "audio/stale.quality", 1000);

        let report = maintain_with(
            &audio,
            &covers,
            MaintenanceLimits {
                audio_bytes: 10,
                cover_bytes: 512 * 1024 * 1024,
            },
            &HashMap::new(),
            list_dir,
        );

        assert_eq!(report.removed_files, 2, "没被租约的老组照常淘汰");
        assert!(!stale.exists());
        assert!(report.over_limit, "播放中的那组还压在上限之上");
        assert_eq!(report.skipped_reason, "over_limit");
        drop(lease);
    }

    /// 失败、重试只补差额：第一次 400、续传到 900，两次加起来是 900，不是 1300。
    #[test]
    fn size_change_counts_only_growth() {
        assert_eq!(size_change(0, 400) + size_change(400, 900), 900);
        assert_eq!(size_change(900, 600), -300, "缩了记负数，旧的大数不会挂着");
    }

    /// 落进正式缓存：暂存只记这次长到交付大小的部分，正式文件再按「加新减旧」算。
    #[test]
    fn landed_delta_counts_new_minus_old() {
        assert_eq!(
            landed_delta(Landing {
                staged_before: 0,
                audio: 1_000,
                sidecar: 40,
                replaced: Replaced {
                    audio: 800,
                    sidecar: 30
                }
            }),
            1_000 + 40 - 800 - 30,
            "换高音质的净变化正好是加新减旧"
        );
    }

    /// 断点续传跨过一次看盘：暂存那 500 已经在扫描结论里，这次只记长到 1000 的差额。
    #[test]
    fn landed_delta_resume_after_a_scan_counts_only_the_growth() {
        assert_eq!(
            landed_delta(Landing {
                staged_before: 500,
                audio: 1_000,
                sidecar: 0,
                replaced: Replaced::default()
            }),
            500,
            "不能把盘上的 1000 再加一遍"
        );
    }

    /// 服务器判「早就下完了」（这次传输量为 0）也要按盘上完整文件记账，不能记成 0。
    #[test]
    fn landed_delta_records_full_file_when_nothing_transferred() {
        assert_eq!(
            landed_delta(Landing {
                staged_before: 0,
                audio: 700,
                sidecar: 20,
                replaced: Replaced::default()
            }),
            720
        );
    }

    /// 旧文件比新文件还大时账会变成负数；往外报已知占用时按 0 封底，不报负的占用。
    #[test]
    fn landed_delta_never_reports_negative_usage() {
        let delta = landed_delta(Landing {
            staged_before: 0,
            audio: 100,
            sidecar: 10,
            replaced: Replaced {
                audio: 5_000,
                sidecar: 40,
            },
        });
        assert!(delta < 0, "旧文件减过头时账本可以为负");
        assert_eq!(
            3_000u64.saturating_add_signed(delta),
            0,
            "已知占用按 0 封底"
        );
    }

    /// 暂存族枚举与 libresoda 的命名表对得上：交付目标加五个派生名。
    #[test]
    fn staged_family_lists_every_staged_name() {
        for suffix in STAGED_SUFFIXES {
            assert!(
                suffix.starts_with(".m4a"),
                "staged_family_paths 靠这个前缀拼名字，缺了会静默少算：{suffix}"
            );
        }
        let target = PathBuf::from("audio/staging/1-lossless-normal.m4a");
        let paths = staged_family_paths(&target);
        assert_eq!(paths.len(), 1 + STAGED_SUFFIXES.len());
        for expected in [
            "audio/staging/1-lossless-normal.m4a",
            "audio/staging/1-lossless-normal.m4a.part",
            "audio/staging/1-lossless-normal.m4a.part.meta",
            "audio/staging/1-lossless-normal.m4a.part.meta.tmp",
            "audio/staging/1-lossless-normal.m4a.part.lock",
            "audio/staging/1-lossless-normal.m4a.dec.tmp",
        ] {
            assert!(paths.contains(&PathBuf::from(expected)), "缺少 {expected}");
        }
    }

    /// 暂存族按盘上真实文件量字节：几份真文件相加，0 字节的锁不影响。
    #[test]
    fn staged_family_bytes_sums_real_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("1-lossless-normal.m4a");
        std::fs::write(&base, [0u8; 4]).unwrap();
        std::fs::write(dir.path().join("1-lossless-normal.m4a.part"), [0u8; 100]).unwrap();
        std::fs::write(
            dir.path().join("1-lossless-normal.m4a.part.meta"),
            [0u8; 30],
        )
        .unwrap();
        std::fs::write(dir.path().join("1-lossless-normal.m4a.dec.tmp"), [0u8; 10]).unwrap();
        std::fs::write(dir.path().join("1-lossless-normal.m4a.part.lock"), b"").unwrap();
        assert_eq!(staged_family_bytes(&base), 144);

        let only_lock = dir.path().join("2-lossless-normal.m4a");
        std::fs::write(dir.path().join("2-lossless-normal.m4a.part.lock"), b"").unwrap();
        assert_eq!(
            staged_family_bytes(&only_lock),
            0,
            "只有 0 字节的锁，量出来还是 0"
        );
    }
}
