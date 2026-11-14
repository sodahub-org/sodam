//! 播放尝试的失败分类与重试预算。
//!
//! 一次播放尝试串联四个阶段：地址解析、签名、音频体传输、解密落盘。**只有串联它们
//! 的这一层知道失败发生在哪儿**，所以分类在这里产出，UI 只做翻译。
//!
//! 已知上限：libresoda 把签名服务与元数据接口的网络失败都压成 `SodaError::Http(String)`
//! 这类**无相位**的变体，因此这里只能报到「地址解析（含签名）」这一粒度，
//! 「签名」要单列得等上游补结构化相位。

use libresoda::SodaError;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 失败发生的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 取回可用流及其地址，含向签名服务请求签名（阶段一 无法把两者分开）。
    ResolveStream,
    /// 把音频体的字节从 CDN 搬到本地。
    Transfer,
    /// 解密并落盘到缓存。
    Unpack,
    /// 调用方参数本身就不合法。
    Input,
    /// 传输层出错，但分不清是地址解析还是音频体读取——握手/DNS 类失败两处都可能发生，
    /// 与其猜一个阶段，不如如实说「网络传输」。上游补了相位之后这个变体应当消失。
    Network,
}

/// 该失败能否自动重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retryability {
    /// 网络抖动类，可以立刻再来一次。
    Transient,
    /// 被限流，可以重试但要等得更久。
    Throttled,
    /// 重试无意义：再打只是打扰签名服务和 CDN，不会变好。
    Never,
}

/// 失败绑定的用户侧修复动作。UI 用它选文案，不是靠解析错误字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// 无需用户做什么（等一会再试即可）。
    Nothing,
    /// 登录态问题，需要重新登录。
    Relogin,
    /// 签名服务不可用或鉴权失败，可在设置页换地址 / Token，或本机跑 libmssdk。
    CheckSigner,
    /// 登录态缺失（缺 Cookie / 缺登录用户 id），换一首结论不变。
    CheckEntitlement,
    /// 按曲目不可播：当前账号拿不到这一首的整曲流（上游只回了试听流等）。
    /// 与 [`Self::PickAnother`] 同属「结论关于单个曲目」，区别只在成因文案。
    TrackUnavailable,
    /// 按曲目不可播的另一半：曲目已下架或取不到（含非 Cookie 的参数不合法）。
    /// 与 [`Self::TrackUnavailable`] 一起由 [`PlaybackFailure::is_per_track`] 判定。
    PickAnother,
    /// 按曲目不可播的第三种成因：音频解不开（解密失败，如多轨样本的
    /// 「decrypted size mismatch」）。同一首每次播都这样，换网络换时间都不变好；
    /// 与磁盘写失败无关，跳过这首即可，上游修好解密后重启即恢复。
    /// 与 [`Self::TrackUnavailable`] / [`Self::PickAnother`] 一起由
    /// [`PlaybackFailure::is_per_track`] 判定。
    Undecodable,
    /// 本地磁盘或文件写入问题。
    FreeSpace,
}

impl Stage {
    /// 日志字段 `stage=` 的稳定取值（小写下划线，不随界面语言变化）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ResolveStream => "resolve_stream",
            Self::Transfer => "transfer",
            Self::Unpack => "unpack",
            Self::Input => "input",
            Self::Network => "network",
        }
    }
}

impl Retryability {
    /// 日志字段 `retryability=` 的稳定取值。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Throttled => "throttled",
            Self::Never => "never",
        }
    }
}

impl RecoveryAction {
    /// 日志字段 `action=` 的稳定取值。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Nothing => "nothing",
            Self::Relogin => "relogin",
            Self::CheckSigner => "check_signer",
            Self::CheckEntitlement => "check_entitlement",
            Self::TrackUnavailable => "track_unavailable",
            Self::PickAnother => "pick_another",
            Self::Undecodable => "undecodable",
            Self::FreeSpace => "free_space",
        }
    }
}

/// 已确认的「按曲目不可播」成因。
///
/// 不可播集合只存这三种，`PlaybackFailure::is_per_track` 也由同一映射判定，
/// 避免「判据说关于单曲、集合却不知道成因」的两处真相。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnplayableReason {
    /// 当前账号拿不到这一首的整曲流（上游只回试听流 / 没有可用试听地址）。
    NoFullTrack,
    /// 曲目已下架或当前取不到。
    Unavailable,
    /// 音频解不开：解密失败，同一首每次播都一样（如多轨样本表导致的大小不符）。
    DecodeFailed,
}

impl UnplayableReason {
    /// 修复动作 → 成因；非按曲目类返回 `None`。
    pub const fn from_action(action: RecoveryAction) -> Option<Self> {
        match action {
            RecoveryAction::TrackUnavailable => Some(Self::NoFullTrack),
            RecoveryAction::PickAnother => Some(Self::Unavailable),
            RecoveryAction::Undecodable => Some(Self::DecodeFailed),
            _ => None,
        }
    }

    /// 日志字段 `reason=` 的稳定取值（小写下划线，不随界面语言变化）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NoFullTrack => "no_full_track",
            Self::Unavailable => "unavailable",
            Self::DecodeFailed => "decode_failed",
        }
    }
}

/// 一次播放尝试的失败：阶段 + 可否重试 + 修复动作 + 已脱敏的原始信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackFailure {
    pub stage: Stage,
    pub retryability: Retryability,
    pub action: RecoveryAction,
    /// 给日志与「展开详情」用的原文：已按 [`redact_url_secrets`] 切断查询串，但仍保留接口地址。
    /// 界面文案不要直接用它——用户只看 [`strip_urls_for_user`] 之后的说法，
    /// 而真正给人读的那句由界面按 `stage` / `action` 组出来。
    pub detail: String,
}

impl PlaybackFailure {
    pub fn new(
        stage: Stage,
        retryability: Retryability,
        action: RecoveryAction,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            stage,
            retryability,
            action,
            detail: redact_url_secrets(&detail.into()),
        }
    }

    /// 这个结论是否关于单个曲目（换一首大概率能播）？
    ///
    /// 判据是**结论对象**（修复动作），不是可重试性：签名服务鉴权同样是
    /// [`Retryability::Never`]，但它是全局故障；反过来，按曲目不可播里也可能出现
    /// 可重试形态。调用方据此决定「跳过」还是「落闸」，两者不能互相推导。
    pub fn is_per_track(&self) -> bool {
        UnplayableReason::from_action(self.action).is_some()
    }
}

impl fmt::Display for PlaybackFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.stage {
            Stage::ResolveStream => "地址解析",
            Stage::Transfer => "音频体传输",
            Stage::Unpack => "解密落盘",
            Stage::Input => "参数",
            Stage::Network => "网络传输",
        };
        write!(f, "{stage}失败：{}", self.detail)
    }
}

impl std::error::Error for PlaybackFailure {}

/// 一条不可播结论：何时确认、当时的可重试性与成因。
///
/// 时间用来做容量淘汰的先进先出；可重试性用来决定「先淘汰谁」；
/// 成因用来让显式点播不必重打网络就能说出「为什么」。
#[derive(Debug, Clone, Copy)]
struct UnplayableEntry {
    recorded_at: Instant,
    retryability: Retryability,
    reason: UnplayableReason,
}

/// 结论集合的只读快照，供列表渲染共享。
///
/// 只回答「这首有没有已确认的不可播结论」；不含本轮试过、但已被容量淘汰的
/// 扫描游标——标记说的是「放不出来」，不是「这一轮试过它」。
#[derive(Debug, Clone, Default)]
pub struct UnplayableSnapshot(Arc<HashMap<String, UnplayableEntry>>);

impl UnplayableSnapshot {
    pub fn contains(&self, track_id: &str) -> bool {
        self.0.contains_key(track_id)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// 会话内的「按曲目不可播」集合：只回答「要不要再为这首打网络」。
///
/// 作用域是**账号 × 本次会话**：
/// * 只由按曲目类失败写入（[`PlaybackFailure::is_per_track`]），全局故障写进来会把
///   整屏曲目误标成不可播；
/// * 账号 id 或会员状态任一变化即整份作废（[`Self::invalidate_if_account_changed`]）；
/// * 不落盘，也不参与缓存文件的有效性判断——缓存命中发生在任何网络请求之前。
#[derive(Debug, Clone, Default)]
pub struct UnplayableTracks {
    /// 结论集合。用 `Arc` 让渲染快照零拷贝共享：写入走 `Arc::make_mut`，
    /// 只在快照还活着时安静地复制一次，不产生第二份可写真相。
    entries: Arc<HashMap<String, UnplayableEntry>>,
    /// 本轮扫描已经尝试过的曲目 id。
    ///
    /// 容量上限只约束上面的结论集合；队列比上限更长时，被淘汰的旧条目会重新
    /// 变成「可播候选」，一次扫描就永远走不完。这份游标不受 256 限制，但只会
    /// 增长到队列长度，并在成功装载 / 换队列 / 账号作废时清空——它不是结论，
    /// 只是「这一轮别再试同一首」的记忆。
    attempted: HashSet<String>,
}

impl UnplayableTracks {
    /// 容量上限沿用预取失败表的既有上限（256），超界时淘汰最旧的条目。
    pub const CAPACITY: usize = 256;

    pub fn contains(&self, track_id: &str) -> bool {
        self.entries.contains_key(track_id)
    }

    /// 这首在本轮扫描里还要不要跳过：已有结论，或本轮已经试过一次。
    pub fn is_unavailable(&self, track_id: &str) -> bool {
        self.entries.contains_key(track_id) || self.attempted.contains(track_id)
    }

    /// 已确认结论的成因（供显式点播不重打网络就给出原因）。
    ///
    /// 只认结论集合，不含「本轮试过但结论已被容量淘汰」的扫描游标——
    /// 那种情况已经不知道当初为什么放不出来，不能凭游标编一个理由。
    pub fn reason(&self, track_id: &str) -> Option<UnplayableReason> {
        self.entries.get(track_id).map(|entry| entry.reason)
    }

    /// 已确认「当前账号放不出来」的只读快照，供列表渲染共享。
    ///
    /// 返回 `Arc` 的克隆（零拷贝）：虚拟列表闭包要求 `'static`，
    /// 而渲染路径不能每帧深拷贝集合（UI-SPEC 的性能约束）。
    pub fn snapshot(&self) -> UnplayableSnapshot {
        UnplayableSnapshot(self.entries.clone())
    }

    /// 开始新一轮扫描：保留结论，只忘掉「本轮试过哪些」。
    ///
    /// 成功装载或换队列时调用；账号作废用 [`Self::clear`] 连结论一起清。
    pub fn reset_scan(&mut self) {
        self.attempted.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries = Arc::default();
        self.attempted.clear();
    }

    /// 记下「这首当前账号放不出来」。重复确认同一首会刷新失败时刻与成因。
    ///
    /// 私有：外部只能经 [`Self::record_failure`] 进来，避免绕开「只收按曲目类」的判据。
    fn record(&mut self, track_id: &str, retryability: Retryability, reason: UnplayableReason) {
        self.record_at(track_id, retryability, reason, Instant::now());
    }

    /// 按失败分类写入：**只有按曲目类写**，全局故障一律不写。
    ///
    /// 一次签名服务故障若写进来，整屏曲目都会被标成「账号不能放」，
    /// 所以这条判据必须留在集合自己的入口上，而不是指望每个调用点记得先判断。
    /// 返回是否真的写入。
    pub fn record_failure(&mut self, track_id: &str, failure: &PlaybackFailure) -> bool {
        let Some(reason) = UnplayableReason::from_action(failure.action) else {
            return false;
        };
        self.attempted.insert(track_id.to_string());
        self.record(track_id, failure.retryability, reason);
        true
    }

    fn record_at(
        &mut self,
        track_id: &str,
        retryability: Retryability,
        reason: UnplayableReason,
        at: Instant,
    ) {
        Arc::make_mut(&mut self.entries).insert(
            track_id.to_string(),
            UnplayableEntry {
                recorded_at: at,
                retryability,
                reason,
            },
        );
        self.evict_over_capacity();
    }

    /// 账号 id 或会员状态任一变化 → 整份作废。返回是否真的作废过。
    ///
    /// `previous` 为 `None` 表示此前没有账号身份（启动 / 退出登录后）。
    pub fn invalidate_if_account_changed(
        &mut self,
        previous: Option<(&str, bool)>,
        current: (&str, bool),
    ) -> bool {
        if previous.is_some_and(|previous| previous != current) {
            self.clear();
            return true;
        }
        false
    }

    /// 超界时**先**清瞬时类条目，再按失败时刻对剩余条目先进先出。
    ///
    /// 瞬时类（网络抖动被误判成按曲目时）更可能自行恢复，先丢它们；
    /// 确定性的按曲目结论留得久一点，避免刚确认过又去白打一次网络。
    fn evict_over_capacity(&mut self) {
        while self.entries.len() > Self::CAPACITY {
            let entries = Arc::make_mut(&mut self.entries);
            let transient = entries
                .iter()
                .filter(|(_, entry)| entry.retryability != Retryability::Never)
                .min_by_key(|(_, entry)| entry.recorded_at)
                .map(|(track_id, _)| track_id.clone());
            let oldest = entries
                .iter()
                .min_by_key(|(_, entry)| entry.recorded_at)
                .map(|(track_id, _)| track_id.clone());
            let Some(victim) = transient.or(oldest) else {
                break;
            };
            entries.remove(&victim);
        }
    }
}

/// 把 `SodaError` 翻译成播放尝试的失败分类。
///
/// 判据来自 libresoda 实际的产出形态：签名服务的回包错误以 `signer error:` 开头、
/// 鉴权失败带「要求鉴权」，HTTP 状态错误是 `http request failed: status {code} (…)`，
/// 业务错误保留 `Api { status_code, .. }`。**未匹配的形态一律不重试**——
/// 「不默认重试」比「猜一次也许能好」更省那台共享签名服务。
pub fn classify(error: &SodaError) -> PlaybackFailure {
    let detail = error.to_string();

    // 下面这些上游子串是 libresoda **文档化的字符串契约**，不是本仓库临时造的：上游通过
    // 字符串匹配判断错误种类，libresoda 的 `AGENTS.md` 要求 `"requires cookie"`、
    // `"returned preview stream"`、`"full stream unavailable"` 原样保留
    // （模块文档同样点名这些子串）。`"require cookie"` / `"logged-in user id"` 只取
    // 稳定短语：上游 `user_playlist.rs` 实际产出的是 `"require cookie"` 与
    // `"require logged-in user id"`（都没有 s），而 libresoda 自己的
    // `is_missing_entitlement` 检查的是带 s 的形态，两种都要认。
    if detail.contains("require cookie")
        || detail.contains("requires cookie")
        || detail.contains("logged-in user id")
    {
        // 登录态缺失是全局故障：换一首结论不变。
        return PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::CheckEntitlement,
            detail,
        );
    }

    // 前两个子串来自上面的上游契约；后两个是 libresoda 在地址解析阶段自产的
    // 「没有可用试听地址」形态（不是上游 Go 契约），但同样只说明这一首拿不到整曲。
    if detail.contains("full stream unavailable")
        || detail.contains("returned preview stream")
        || detail.contains("missing player info url")
        || detail.contains("player info url not found")
    {
        // 按曲目不可播：这一首在本账号拿不到整曲流，队列里换一首大概率能播。
        return PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::TrackUnavailable,
            detail,
        );
    }

    // 签名服务的错误形态不止一种：`signer error:`（回包 ok:false）、`signer response decode`
    // （回包不是预期 JSON）、`signer spawn/stdin/wait`（本机签名器没跑起来）。共同点是我们对
    // 那台共享服务没有杠杆，所以一律不自动重试，并把出口指向设置页或本机 libmssdk。
    if detail.contains("signer") {
        return PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::CheckSigner,
            detail,
        );
    }

    // 下架 / 取不到 / 非 Cookie 参数错误仍走既有 PickAnother：它们与上面的
    // TrackUnavailable 同属按曲目不可播（由 `is_per_track` 统一判定），
    // 但给用户的成因文案是「这首当前取不到」。
    match error {
        SodaError::InvalidInput(_) => {
            // 缺 Cookie 的 InvalidInput 已在上面归为全局；其余参数不合法只影响当前这一首，
            // 按「取不到」处理，别停掉整轮自动连播。
            PlaybackFailure::new(
                Stage::Input,
                Retryability::Never,
                RecoveryAction::PickAnother,
                detail,
            )
        }
        SodaError::NotFound(_) | SodaError::Unsupported(_) => PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::PickAnother,
            detail,
        ),
        SodaError::Io(_) => PlaybackFailure::new(
            Stage::Unpack,
            Retryability::Never,
            RecoveryAction::FreeSpace,
            detail,
        ),
        SodaError::Crypto(_) => {
            // 解密失败不是磁盘问题：同一首每次播都解不开（见 libresoda#42 的多轨样本），
            // 归成按曲目不可播的第三种成因，跳过这首即可，别再为它反复下载。
            PlaybackFailure::new(
                Stage::Unpack,
                Retryability::Never,
                RecoveryAction::Undecodable,
                detail,
            )
        }
        SodaError::Api { status_code, .. } => {
            // 汽水业务码：未登录一类要重新登录。业务码语义我们没实测过，
            // 所以一律不自动重试——猜错方向的代价是打爆接口，收益只是省一次重登。
            let action = matches!(*status_code, 7 | 401 | 403)
                .then_some(RecoveryAction::Relogin)
                .unwrap_or(RecoveryAction::Nothing);
            PlaybackFailure::new(Stage::ResolveStream, Retryability::Never, action, detail)
        }
        SodaError::Json(_) => PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Transient,
            RecoveryAction::Nothing,
            detail,
        ),
        SodaError::Http(_) => classify_http(&detail),
    }
}

/// `SodaError::Http` 只剩字符串，相位信息已经在上游丢失；这里只解析结构化的状态码。
fn classify_http(detail: &str) -> PlaybackFailure {
    let status = detail
        .split("status ")
        .nth(1)
        .and_then(|rest| rest.chars().take(3).collect::<String>().parse::<u16>().ok());
    let (retryability, action) = match status {
        Some(429) => (Retryability::Throttled, RecoveryAction::Nothing),
        Some(401) => (Retryability::Never, RecoveryAction::Relogin),
        // 403 既可能是 Cookie 权益，也可能是签名 URL 过期：后者换一次地址就能好，
        // 所以按可重试处理，让它走一次「重新解析」而不是永久失败。
        Some(403) => (Retryability::Transient, RecoveryAction::Nothing),
        Some(404) => (Retryability::Never, RecoveryAction::PickAnother),
        Some(500..=599) => (Retryability::Transient, RecoveryAction::Nothing),
        Some(_) => (Retryability::Never, RecoveryAction::Nothing),
        None => {
            // 没有状态码的都是传输层。判据必须盖住真实的握手类文本——本机实测到的就是
            // `Connection Failed: tls connection init failed: unexpected end of file`；
            // 只认 "timeout" 会把这类抖动误判成永久错误，一次都不重试。
            let lower = detail.to_ascii_lowercase();
            let transport = [
                "timed out",
                "timeout",
                "tls",
                "connection",
                "dns",
                "failed to lookup address",
                "end of file",
                "broken pipe",
                "reset by peer",
            ]
            .iter()
            .any(|marker| lower.contains(marker));
            if transport {
                (Retryability::Transient, RecoveryAction::Nothing)
            } else {
                (Retryability::Never, RecoveryAction::Nothing)
            }
        }
    };
    let stage = if detail.contains("http request failed") {
        // 这个前缀是 `get_full` / `get_reader` 在**拿到状态码**时产出的，归地址解析；
        Stage::ResolveStream
    } else if detail.contains("reading response") || detail.contains("reading body") {
        // 已经在读响应体：明确的音频体传输故障。
        Stage::Transfer
    } else {
        // 握手、DNS、连接重置这类错误在解析地址和读体两处都会出现，
        // 上游没带相位就是分不出来，别装作知道。
        Stage::Network
    };
    PlaybackFailure::new(stage, retryability, action, detail)
}

/// 抹掉文本里任何 URL 的查询串：播放地址的 token 就在其中，绝不能进用户可见文案。
///
/// 这是**给日志与详情用的**那一层——保留接口地址本身，只切断查询串。
/// 界面用的是 [`strip_urls_for_user`]。
pub fn redact_url_secrets(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|token| {
            let body = token.trim_end_matches(char::is_whitespace);
            let whitespace = &token[body.len()..];
            let cut = match body.find("://") {
                Some(_) => body.find('?'),
                None => None,
            };
            match cut {
                Some(at) => format!("{}?…{}", &body[..at], whitespace),
                None => token.to_string(),
            }
        })
        .collect()
}

/// 用户可见文本：**任何**接口地址整段去掉，不给普通用户看内部拓扑。
///
/// 判据是这个信息能不能让眼前这个人采取行动——不能，它就不该出现在弹窗和状态行里。
/// 地址没有消失：它还在 [`PlaybackFailure::detail`]（已按 [`redact_url_secrets`] 切断查询串），
/// 等日志出口落地就直接写那边。
///
/// 边界按 `://` 定位而不是按空格切：文案里 `加载歌单失败：https://…` 这种前缀与地址之间
/// 没有空格，按 token 删会把人话前半句一起删掉。
pub fn strip_urls_for_user(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        // 起点：scheme 的第一个字符，往左只吃 ASCII（遇到中文标点就停，保证落在字符边界上）。
        let bytes = rest.as_bytes();
        let mut start = at;
        while start > 0 {
            let prev = bytes[start - 1];
            if prev.is_ascii_alphanumeric() || matches!(prev, b'+' | b'-' | b'.') {
                start -= 1;
            } else {
                break;
            }
        }
        out.push_str(&rest[..start]);
        // 终点：下一个空白或右括号。地址尾部粘连的 `:` 属于同一个片段，一并摘掉。
        let tail = &rest[at..];
        let span = tail
            .find(|c: char| c.is_whitespace() || c == ')' || c == '）')
            .unwrap_or(tail.len());
        rest = &rest[at + span..];
    }
    out.push_str(rest);
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    while out.ends_with(&[':', '：', ' '][..]) {
        out.pop();
    }
    out
}

/// 一次播放尝试的重试预算：**只有一层，且只按「这次失败值不值得再来一次」判**。
///
/// 不设总墙钟时长：那等于把刚从 libresoda 拆掉的「整首下载总时限」从另一侧装回来，
/// 慢但仍在推进的传输会被误杀。不设「无进展时长」也不是疏忽——阶段一 里根本没有可观测的
/// 进展信号：`download_to_cache` 是一次阻塞调用，失败即删掉 `.part`，跨尝试零进度。
/// 单次尝试内部的「卡住」由 libresoda 的空闲读超时（20s）负责；等阶段二有了
/// 密文暂存 + `Range` 续传，字节偏移才第一次成为可读的进度，那时才谈得上无进展封顶。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayBudget {
    pub max_attempts: u8,
}

impl Default for PlayBudget {
    fn default() -> Self {
        Self { max_attempts: 3 }
    }
}

impl PlayBudget {
    /// 这次失败后要不要再来一次，以及该等多久。
    ///
    /// `attempt` 是**已经做完的尝试次数**；`jitter` 取 `[0,1)` 由调用方提供，
    /// 这样纯函数在单测里可复现（full jitter：`rand(0, min(cap, base × 2^n))`）。
    pub fn next_delay(
        &self,
        attempt: u8,
        failure: &PlaybackFailure,
        jitter: f32,
    ) -> Option<Duration> {
        if failure.retryability == Retryability::Never || attempt >= self.max_attempts {
            return None;
        }
        let base = match failure.retryability {
            Retryability::Throttled => Duration::from_secs(4),
            _ => Duration::from_secs(1),
        };
        let cap = Duration::from_secs(20);
        let ceiling = (base * 2u32.pow(attempt.saturating_sub(1).min(4) as u32)).min(cap);
        Some(ceiling.mul_f32(jitter.clamp(0.0, 0.999)))
    }
}

/// 取一个免依赖的抖动源：用「距离上一次有进展的时长」的纳秒尾数，
/// 每次失败的值都不同，足以把重试错开，又不用引入随机数依赖。
pub fn jitter_from_elapsed(since: Duration) -> f32 {
    (since.subsec_nanos() % 1000) as f32 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(error: SodaError) -> PlaybackFailure {
        classify(&error)
    }

    #[test]
    fn permanent_shapes_are_never_retried_and_carry_an_action() {
        let cases = [
            (
                SodaError::http(
                    "signer error: bad token（签名服务要求鉴权：设置 QISHUI_SIGNER_TOKEN）",
                ),
                RecoveryAction::CheckSigner,
            ),
            // 回包不是预期 JSON 同样算签名服务故障：也不该自动重试。
            (
                SodaError::json("signer response decode: expected value at line 1 column 1"),
                RecoveryAction::CheckSigner,
            ),
            (
                SodaError::http("http request failed: status 404 (https://x.test/a)"),
                RecoveryAction::PickAnother,
            ),
            (SodaError::api(7, "need login"), RecoveryAction::Relogin),
            (SodaError::Io("disk full".into()), RecoveryAction::FreeSpace),
            (
                SodaError::not_found("track not found"),
                RecoveryAction::PickAnother,
            ),
            (
                SodaError::http("soda pc track_v2 returned preview stream"),
                RecoveryAction::TrackUnavailable,
            ),
            (
                SodaError::invalid_input("track id is empty"),
                RecoveryAction::PickAnother,
            ),
            // 解密失败归「解不开」：不落磁盘话术，也不落「什么都不用做」。
            (
                SodaError::crypto("decrypted size mismatch"),
                RecoveryAction::Undecodable,
            ),
        ];
        for (error, action) in cases {
            let classified = failure(error);
            assert_eq!(
                classified.retryability,
                Retryability::Never,
                "不该自动重试：{classified:?}"
            );
            assert_eq!(classified.action, action, "修复动作错位：{classified:?}");
        }
    }

    /// 「结论是否关于单个曲目」只看结论对象，不看可重试性。
    ///
    /// 这些上游错误文本来自 libresoda 的文档化字符串契约；测试用它们钉住分类，
    /// 也钉住「签名服务同样的 Never 不是按曲目」这条边界。
    #[test]
    fn per_track_predicate_uses_the_conclusion_not_retryability() {
        let per_track = [
            // 只有试听流 / 拿不到整曲：上游只回试听流。
            SodaError::not_found("soda pc track_v2 returned preview stream"),
            SodaError::not_found("full stream unavailable"),
            // 曲目下架 / 取不到。
            SodaError::not_found("track not found"),
            SodaError::unsupported("track unsupported"),
            SodaError::http("http request failed: status 404 (https://x.test/track)"),
            // 非 Cookie 类的参数不合法。
            SodaError::invalid_input("track id is empty"),
            // 音频解不开：结论只关于这一首，与磁盘无关。
            SodaError::crypto("decrypted size mismatch"),
        ];
        for error in per_track {
            let failure = failure(error);
            assert!(failure.is_per_track(), "应判为按曲目：{failure:?}");
        }

        // 只有试听流且没有可用试听地址：归到「拿不到整曲」，不按曲目下架处理。
        for text in [
            "soda pc track_v2 missing player info url",
            "player info url not found",
        ] {
            let failure = failure(SodaError::not_found(text));
            assert_eq!(
                failure.action,
                RecoveryAction::TrackUnavailable,
                "{failure:?}"
            );
            assert!(failure.is_per_track(), "{failure:?}");
        }

        // 解密失败归「解不开」：阶段仍是解密落盘，上游原文留在日志详情里，
        // 界面话术由 UI 层翻译（不在这里混进来）。
        let crypto = failure(SodaError::crypto("decrypted size mismatch"));
        assert_eq!(crypto.action, RecoveryAction::Undecodable);
        assert_eq!(crypto.stage, Stage::Unpack);
        assert_eq!(crypto.retryability, Retryability::Never);
        assert!(
            crypto.detail.contains("decrypted size mismatch"),
            "{}",
            crypto.detail
        );

        // 样本曲目的实际形态：不可重试，但结论只关于这一首。
        let sample = failure(SodaError::not_found(
            "soda pc track_v2 returned preview stream",
        ));
        assert_eq!(sample.action, RecoveryAction::TrackUnavailable);
        assert_eq!(sample.retryability, Retryability::Never);
        assert!(sample.is_per_track());

        let global = [
            // 缺 Cookie / 缺登录用户 id：换一首结论不变。上游两种拼写都要认。
            SodaError::invalid_input("feed requires cookie"),
            SodaError::invalid_input("soda my playlists require cookie"),
            // 上游实际产出的是 require（无 s），libresoda 自己的判定检查的是 requires。
            // 两种形态都必须归为全局，否则缺登录会退化成按曲目跳过。
            SodaError::not_found("soda user playlists require logged-in user id"),
            SodaError::not_found("soda user playlists requires logged-in user id"),
            SodaError::http("signer error: bad token"),
            SodaError::Io("disk full".into()),
            SodaError::api(7, "need login"),
            SodaError::http("http request failed: status 401 (https://x.test)"),
        ];
        for error in global {
            let failure = failure(error);
            assert!(!failure.is_per_track(), "应判为全局：{failure:?}");
        }

        // 网络抖动不可重试性与结论对象无关：它既不是按曲目，也不影响上面的判定。
        let network = failure(SodaError::http("io: timed out reading response"));
        assert!(!network.is_per_track());
        assert_eq!(network.retryability, Retryability::Transient);
    }

    /// 日志里的动作值与成因值既有的一字不改，新增的固定为 `undecodable` / `decode_failed`。
    #[test]
    fn action_log_values_are_stable_and_additive() {
        let values = [
            (RecoveryAction::Nothing, "nothing"),
            (RecoveryAction::Relogin, "relogin"),
            (RecoveryAction::CheckSigner, "check_signer"),
            (RecoveryAction::CheckEntitlement, "check_entitlement"),
            (RecoveryAction::TrackUnavailable, "track_unavailable"),
            (RecoveryAction::PickAnother, "pick_another"),
            (RecoveryAction::Undecodable, "undecodable"),
            (RecoveryAction::FreeSpace, "free_space"),
        ];
        for (action, expected) in values {
            assert_eq!(action.as_str(), expected);
        }

        let reasons = [
            (UnplayableReason::NoFullTrack, "no_full_track"),
            (UnplayableReason::Unavailable, "unavailable"),
            (UnplayableReason::DecodeFailed, "decode_failed"),
        ];
        for (reason, expected) in reasons {
            assert_eq!(reason.as_str(), expected);
        }
    }

    #[test]
    fn transient_shapes_stay_retriable() {
        // 传输层空闲读超时：没有状态码，落在音频体传输。
        let timeout = failure(SodaError::http("io: timed out reading response"));
        assert_eq!(timeout.retryability, Retryability::Transient);
        assert_eq!(timeout.stage, Stage::Transfer);

        // 签名 URL 过期：换一次地址就能好，按可重试处理。
        let forbidden = failure(SodaError::http(
            "http request failed: status 403 (https://cdn.test/p?token=abc)",
        ));
        assert_eq!(forbidden.retryability, Retryability::Transient);
        assert_eq!(forbidden.stage, Stage::ResolveStream);

        // 限流要等得更久。
        let throttled = failure(SodaError::http(
            "http request failed: status 429 (https://x)",
        ));
        assert_eq!(throttled.retryability, Retryability::Throttled);
    }

    #[test]
    fn unknown_shapes_default_to_no_retry() {
        // 「不默认重试」：认不出形态、也不是传输层的错误不能变成盲试。
        let odd = failure(SodaError::http("unexpected status line from upstream"));
        assert_eq!(odd.retryability, Retryability::Never);
    }

    #[test]
    fn tls_handshake_failures_are_transient_not_permanent() {
        // 断网注入时实测到的错误文本（歌词接口）。它作为**测试输入**有效——这类握手断流
        // 真机上确实会出现——但不代表用户的真实故障分布，别拿它当归因证据。
        let tls = SodaError::http(
            "soda track_v2 failed: https://api.qishui.com/luna/pc/track_v2?aid=386088&track_id=6696408449359562754: Connection Failed: tls connection init failed: unexpected end of file",
        );
        let classified = failure(tls);
        assert_eq!(classified.retryability, Retryability::Transient);
        // 地址解析与音频体传输都可能发生握手失败，分不出来就明说分不出来。
        assert_eq!(classified.stage, Stage::Network);
        // 同一条文本里的接口查询串必须被脱敏。
        let detail = classified.detail.as_str();
        assert!(!detail.contains("aid=386088"), "{detail}");
        assert!(detail.contains("api.qishui.com/luna/pc/track_v2?…"));
    }

    #[test]
    fn redact_keeps_host_and_drops_signed_query() {
        let text =
            "http request failed: status 403 (https://v26-luna.test/asset/m4a?rid=1&sign=SECRET)";
        let redacted = redact_url_secrets(text);
        assert!(
            redacted.contains("https://v26-luna.test/asset/m4a?…"),
            "{redacted}"
        );
        assert!(!redacted.contains("SECRET"), "{redacted}");
        // 不带 ? 的 URL 与普通问号都不该被动到。
        assert_eq!(
            redact_url_secrets("是不是失败了? 再试吗"),
            "是不是失败了? 再试吗"
        );
        // 分类结果里存的一定是脱敏后的文本。
        let failure = failure(SodaError::http(text));
        assert!(!failure.detail.contains("SECRET"), "{}", failure.detail);
    }

    #[test]
    fn budget_gates_on_retryability_and_attempts() {
        let budget = PlayBudget::default();
        let transient = PlaybackFailure::new(
            Stage::Transfer,
            Retryability::Transient,
            RecoveryAction::Nothing,
            "io: timed out reading response",
        );
        let permanent = PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::Relogin,
            "signer error",
        );

        // 永久错误一次都不重试：再打只是打扰那台共享签名服务。
        assert_eq!(budget.next_delay(1, &permanent, 0.5), None);
        assert!(budget.next_delay(1, &transient, 0.5).is_some());
        // 次数封顶是唯一的上限（阶段一 没有可观测的字节进度，见 PlayBudget 文档）。
        assert_eq!(budget.next_delay(3, &transient, 0.5), None);
        // full jitter：等待时长受抖动系数缩放，且不超过上限。
        let ceiling = budget.next_delay(1, &transient, 0.999).unwrap();
        let floor = budget.next_delay(1, &transient, 0.0).unwrap();
        assert!(floor < ceiling, "{floor:?} {ceiling:?}");
        assert!(ceiling <= Duration::from_secs(20), "{ceiling:?}");
        // 限流基数比瞬时更长。
        let throttled = PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Throttled,
            RecoveryAction::Nothing,
            "429",
        );
        assert!(budget.next_delay(1, &throttled, 0.9) > budget.next_delay(1, &transient, 0.9));
    }

    #[test]
    fn default_budget_allows_three_attempts() {
        assert_eq!(PlayBudget::default().max_attempts, 3);
    }

    /// issue #51 的“永久失败一次远程尝试”与默认三次预算必须有可执行断言，
    /// 不能只靠 `max_attempts` 常量碰巧保持正确。
    #[test]
    fn default_budget_exact_remote_attempt_counts() {
        let budget = PlayBudget::default();
        let permanent = PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::CheckSigner,
            "signer error",
        );
        let transient = PlaybackFailure::new(
            Stage::Network,
            Retryability::Transient,
            RecoveryAction::Nothing,
            "connection reset",
        );

        // 永久失败：第 1 次尝试后就不再发生远程请求。
        assert_eq!(budget.next_delay(1, &permanent, 0.5), None);
        // 瞬时失败：前两次失败后重试，完成第 3 次尝试后停止。
        assert!(budget.next_delay(1, &transient, 0.5).is_some());
        assert!(budget.next_delay(2, &transient, 0.5).is_some());
        assert_eq!(budget.next_delay(3, &transient, 0.5), None);
    }

    #[test]
    fn user_text_drops_endpoints_but_the_log_detail_keeps_them() {
        let raw = "soda track_v2 failed: https://api.qishui.com/luna/pc/feed/song-tab?aid=386088&track_id=7: Connection Failed: tls connection init failed: unexpected end of file (seo fallback: https://beta-luna.douyin.com/luna/h5/seo-track?track_id=7: Connection Failed: tls connection init failed: unexpected end of file)";
        let failure = failure(SodaError::http(raw));
        let shown = strip_urls_for_user(&failure.detail);
        // 界面上不该有内部端点，也不该有查询串。
        assert!(!shown.contains("://"), "{shown}");
        assert!(!shown.contains("qishui.com"), "{shown}");
        assert!(!shown.contains("aid=386088"), "{shown}");
        // 但人话部分与因果必须留下，且括号不塌。
        assert!(shown.contains("tls connection init failed"), "{shown}");
        assert!(shown.contains("seo fallback"), "{shown}");
        assert!(shown.contains("(seo fallback"), "{shown}");
        // 日志侧仍保留地址，只切断查询串。
        assert!(
            failure
                .detail
                .contains("api.qishui.com/luna/pc/feed/song-tab?…"),
            "{}",
            failure.detail
        );
    }

    #[test]
    fn strip_handles_urls_without_a_leading_space() {
        // 「加载歌单失败：https://…」这类模板里前缀与地址之间没有空格，前半句不能被吞。
        assert_eq!(
            strip_urls_for_user("加载歌单失败：https://x.test/a?b=1"),
            "加载歌单失败"
        );
        assert_eq!(strip_urls_for_user("没有地址的文案"), "没有地址的文案");
        assert_eq!(
            strip_urls_for_user("回退（https://x.test/a）后失败"),
            "回退（）后失败"
        );
    }

    #[test]
    fn unplayable_set_records_and_clears() {
        let mut set = UnplayableTracks::default();
        assert!(set.is_empty());
        set.record(
            "track-1",
            Retryability::Never,
            UnplayableReason::NoFullTrack,
        );
        set.record(
            "track-2",
            Retryability::Never,
            UnplayableReason::Unavailable,
        );
        assert!(set.contains("track-1"));
        assert!(set.contains("track-2"));
        assert_eq!(set.len(), 2);
        assert!(!set.contains("track-3"));
        set.clear();
        assert!(set.is_empty());
    }

    /// 显式点播要能不重打网络就说出原因：集合记住的是成因，不只是「不可播」。
    #[test]
    fn unplayable_set_remembers_the_reason() {
        let mut set = UnplayableTracks::default();
        let no_full = failure(SodaError::not_found(
            "soda pc track_v2 returned preview stream",
        ));
        let gone = failure(SodaError::not_found("track removed"));
        let decode = failure(SodaError::crypto("decrypted size mismatch"));
        assert!(set.record_failure("no-full", &no_full));
        assert!(set.record_failure("gone", &gone));
        assert!(set.record_failure("decode", &decode));
        assert_eq!(set.reason("no-full"), Some(UnplayableReason::NoFullTrack));
        assert_eq!(set.reason("gone"), Some(UnplayableReason::Unavailable));
        assert_eq!(set.reason("decode"), Some(UnplayableReason::DecodeFailed));

        // 全局故障不写结论，也就没有原因可说。
        let signer = failure(SodaError::http("signer error: bad token"));
        assert!(!set.record_failure("signer", &signer));
        assert_eq!(set.reason("signer"), None);
        assert_eq!(set.reason("never-seen"), None);
    }

    /// 成因映射与「是否按曲目」必须共用同一判据，否则集合会收进不知道成因的条目。
    #[test]
    fn per_track_verdict_and_reason_share_one_mapping() {
        let at = |action| {
            PlaybackFailure::new(Stage::ResolveStream, Retryability::Never, action, "detail")
        };
        for action in [
            RecoveryAction::TrackUnavailable,
            RecoveryAction::PickAnother,
            RecoveryAction::Undecodable,
        ] {
            assert!(at(action).is_per_track(), "{action:?}");
            assert!(UnplayableReason::from_action(action).is_some());
        }
        for action in [
            RecoveryAction::Nothing,
            RecoveryAction::Relogin,
            RecoveryAction::CheckSigner,
            RecoveryAction::CheckEntitlement,
            RecoveryAction::FreeSpace,
        ] {
            assert!(!at(action).is_per_track(), "{action:?}");
            assert_eq!(UnplayableReason::from_action(action), None);
        }
    }

    /// 集合只由按曲目类失败写入：一次签名服务故障不能把整屏曲目标成不可播。
    #[test]
    fn unplayable_set_only_accepts_per_track_failures() {
        let mut set = UnplayableTracks::default();
        let per_track = failure(SodaError::not_found(
            "soda pc track_v2 returned preview stream",
        ));
        // 先放一条按曲目结论，再让全局 / 非确定性失败经过入口：已有条目必须原样保留。
        assert!(set.record_failure("keep", &per_track));

        for error in [
            SodaError::http("signer error: bad token"),
            SodaError::http("http request failed: status 401 (https://x.test)"),
            SodaError::invalid_input("feed requires cookie"),
            SodaError::Io("disk full".into()),
            SodaError::http("io: timed out reading response"),
        ] {
            let failure = failure(error);
            assert!(
                !set.record_failure("track-1", &failure),
                "全局 / 非确定性失败不该写入：{failure:?}"
            );
            assert_eq!(set.len(), 1, "{failure:?}");
            assert!(set.contains("keep"), "{failure:?}");
            assert!(!set.contains("track-1"), "{failure:?}");
        }

        assert!(set.record_failure("track-1", &per_track));
        assert_eq!(set.len(), 2);
        assert!(set.contains("track-1"));

        // 解密失败同样是按曲目类：写进集合，快照能标出这首。
        let decode = failure(SodaError::crypto("decrypted size mismatch"));
        assert!(set.record_failure("track-2", &decode));
        assert!(set.contains("track-2"));
        assert!(set.snapshot().contains("track-2"));
    }

    /// 账号 id 或会员状态任一变化就整份作废；没变时一条都不能丢。
    #[test]
    fn unplayable_set_invalidates_only_when_account_identity_changes() {
        let mut set = UnplayableTracks::default();
        set.record(
            "track-1",
            Retryability::Never,
            UnplayableReason::NoFullTrack,
        );

        // 同一账号、同一会员状态：保留。
        assert!(!set.invalidate_if_account_changed(Some(("user-a", true)), ("user-a", true)));
        assert!(set.contains("track-1"));

        // 会员状态变化：作废。
        assert!(set.invalidate_if_account_changed(Some(("user-a", true)), ("user-a", false)));
        assert!(set.is_empty());

        // 换账号：作废。
        set.record(
            "track-2",
            Retryability::Never,
            UnplayableReason::NoFullTrack,
        );
        assert!(set.invalidate_if_account_changed(Some(("user-a", false)), ("user-b", false)));
        assert!(set.is_empty());

        // 此前没有账号身份（刚启动 / 退出登录后）：没有可作废的结论。
        set.record(
            "track-3",
            Retryability::Never,
            UnplayableReason::NoFullTrack,
        );
        assert!(!set.invalidate_if_account_changed(None, ("user-b", false)));
        assert!(set.contains("track-3"));
    }

    /// 超界时先清瞬时类，再按失败时刻先进先出。
    #[test]
    fn unplayable_set_evicts_transient_entries_before_permanent_ones() {
        let base = Instant::now();
        let at = |millis: u64| base + Duration::from_millis(millis);
        let mut set = UnplayableTracks::default();
        // 两条确定性结论最早写入，但不应先于瞬时条目被淘汰。
        set.record_at(
            "keep-a",
            Retryability::Never,
            UnplayableReason::NoFullTrack,
            at(0),
        );
        set.record_at(
            "keep-b",
            Retryability::Never,
            UnplayableReason::NoFullTrack,
            at(1),
        );
        for index in 0..(UnplayableTracks::CAPACITY - 2) {
            set.record_at(
                &format!("transient-{index}"),
                Retryability::Transient,
                UnplayableReason::NoFullTrack,
                at(10 + index as u64),
            );
        }
        assert_eq!(set.len(), UnplayableTracks::CAPACITY);

        set.record_at(
            "overflow",
            Retryability::Transient,
            UnplayableReason::NoFullTrack,
            at(9_000),
        );
        assert_eq!(set.len(), UnplayableTracks::CAPACITY);
        assert!(set.contains("keep-a"), "确定性条目不该先被淘汰");
        assert!(set.contains("keep-b"), "确定性条目不该先被淘汰");
        assert!(!set.contains("transient-0"), "最旧的瞬时条目应先出局");
        assert!(set.contains("overflow"));
    }

    #[test]
    fn unplayable_set_evicts_permanent_entries_first_in_first_out() {
        let base = Instant::now();
        let mut set = UnplayableTracks::default();
        for index in 0..UnplayableTracks::CAPACITY {
            set.record_at(
                &format!("track-{index}"),
                Retryability::Never,
                UnplayableReason::NoFullTrack,
                base + Duration::from_millis(index as u64),
            );
        }
        set.record_at(
            "newest",
            Retryability::Never,
            UnplayableReason::NoFullTrack,
            base + Duration::from_millis(9_000),
        );
        assert_eq!(set.len(), UnplayableTracks::CAPACITY);
        assert!(!set.contains("track-0"), "最旧的确定性条目应先出局");
        assert!(set.contains("track-1"));
        assert!(set.contains("newest"));
    }

    /// 队列比容量长时，被淘汰的旧条目在本轮扫描里仍算「试过了」；
    /// 换一轮扫描（成功装载 / 换队列）才忘掉。
    #[test]
    fn scan_cursor_survives_capacity_eviction() {
        let mut set = UnplayableTracks::default();
        let per_track = failure(SodaError::not_found(
            "soda pc track_v2 returned preview stream",
        ));
        for index in 0..=UnplayableTracks::CAPACITY {
            set.record_failure(&format!("track-{index}"), &per_track);
        }
        // 最早的结论已被容量淘汰……
        assert_eq!(set.len(), UnplayableTracks::CAPACITY);
        assert!(!set.contains("track-0"));
        // ……但本轮扫描仍记得它，不会立刻回头重试。
        assert!(set.is_unavailable("track-0"));

        set.reset_scan();
        assert!(!set.is_unavailable("track-0"), "新一轮扫描应忘掉试过的曲目");
        assert!(
            set.is_unavailable("track-1"),
            "结论不能被 reset_scan 清掉：它仍在容量内"
        );

        set.clear();
        assert!(!set.is_unavailable("track-1"));
    }

    /// 列表标记的数据源只认「有结论」的条目：被容量淘汰、只剩扫描游标的曲目不算。
    #[test]
    fn snapshot_mirrors_recorded_conclusions_not_the_scan_cursor() {
        let mut set = UnplayableTracks::default();
        let per_track = failure(SodaError::not_found(
            "soda pc track_v2 returned preview stream",
        ));
        set.record_failure("marked", &per_track);
        let ids = set.snapshot();
        assert_eq!(ids.len(), 1);
        assert!(ids.contains("marked"));

        // 塞到容量之外：最早的结论被淘汰。它仍在扫描游标里（本轮不再重试），
        // 但「放不出来」的结论已经不在表上，列表不该继续标它。
        for index in 0..UnplayableTracks::CAPACITY {
            set.record_failure(&format!("filler-{index}"), &per_track);
        }
        let ids = set.snapshot();
        assert_eq!(ids.len(), set.len());
        assert!(!ids.contains("marked"), "被容量淘汰的结论不该继续标记");
        assert!(set.is_unavailable("marked"), "扫描游标仍应记得试过它");

        // 账号作废：标记与结论一起消失。
        set.clear();
        assert!(set.snapshot().is_empty());
    }

    /// 队列比容量长、且整队都按曲目不可播时，扫描必须停在整圈处，
    /// 不能因为旧结论被淘汰就无限回头重试。
    #[test]
    fn scan_cursor_terminates_a_queue_longer_than_capacity() {
        use crate::models::TrackItem;
        use crate::queue::{PlayMode, Queue};

        let total = UnplayableTracks::CAPACITY + 44;
        let tracks: Vec<TrackItem> = (0..total)
            .map(|index| TrackItem {
                id: format!("track-{index}"),
                ..Default::default()
            })
            .collect();
        let mut queue = Queue::new(tracks);
        queue.mode = PlayMode::ListLoop;
        let failure = PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::TrackUnavailable,
            "returned preview stream",
        );
        let mut set = UnplayableTracks::default();
        let mut attempts = 0;
        loop {
            let current = queue.current().expect("队列非空").id.clone();
            set.record_failure(&current, &failure);
            attempts += 1;
            assert!(attempts <= total, "扫描应在整圈内结束");
            if queue
                .advance_playable(|id| set.is_unavailable(id))
                .is_none()
            {
                break;
            }
        }
        assert_eq!(attempts, total, "整队每首恰好尝试一次");
        assert_eq!(
            set.len(),
            UnplayableTracks::CAPACITY,
            "结论集合仍受容量上限约束"
        );
    }

    #[test]
    fn every_variant_classifies_without_panicking() {
        let variants = [
            SodaError::http("x"),
            SodaError::json("x"),
            SodaError::api(1, "x"),
            SodaError::crypto("x"),
            SodaError::unsupported("x"),
            SodaError::invalid_input("x"),
            SodaError::not_found("x"),
            SodaError::Io("x".into()),
        ];
        for error in variants {
            let _ = classify(&error);
        }
    }
}
