//! 播放尝试的失败分类与重试预算。
//!
//! 一次播放尝试串联四个阶段：地址解析、签名、音频体传输、解密落盘。**只有串联它们
//! 的这一层知道失败发生在哪儿**，所以分类在这里产出，UI 只做翻译。
//!
//! 已知上限：libresoda 把签名服务与元数据接口的网络失败都压成 `SodaError::Http(String)`
//! 这类**无相位**的变体，因此这里只能报到「地址解析（含签名）」这一粒度，
//! 「签名」要单列得等上游补结构化相位。

use libresoda::SodaError;
use std::fmt;
use std::time::Duration;

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
    /// 当前账号对该曲目没有整曲权益。
    CheckEntitlement,
    /// 曲目已下架或取不到。
    PickAnother,
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
            Self::PickAnother => "pick_another",
            Self::FreeSpace => "free_space",
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

/// 把 `SodaError` 翻译成播放尝试的失败分类。
///
/// 判据来自 libresoda 实际的产出形态：签名服务的回包错误以 `signer error:` 开头、
/// 鉴权失败带「要求鉴权」，HTTP 状态错误是 `http request failed: status {code} (…)`，
/// 业务错误保留 `Api { status_code, .. }`。**未匹配的形态一律不重试**——
/// 「不默认重试」比「猜一次也许能好」更省那台共享签名服务。
pub fn classify(error: &SodaError) -> PlaybackFailure {
    let detail = error.to_string();

    if error.is_missing_entitlement() || error.is_cookie_required() {
        return PlaybackFailure::new(
            Stage::ResolveStream,
            Retryability::Never,
            RecoveryAction::CheckEntitlement,
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

    match error {
        SodaError::InvalidInput(_) => PlaybackFailure::new(
            Stage::Input,
            Retryability::Never,
            RecoveryAction::Nothing,
            detail,
        ),
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
        SodaError::Crypto(_) => PlaybackFailure::new(
            Stage::Unpack,
            Retryability::Never,
            RecoveryAction::Nothing,
            detail,
        ),
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
                RecoveryAction::CheckEntitlement,
            ),
            (
                SodaError::invalid_input("track id is empty"),
                RecoveryAction::Nothing,
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
