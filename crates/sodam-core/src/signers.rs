//! 签名服务列表的领域助手：展示名、token 掩码与延时测量（#321）。
//!
//! 测延时是手动触发的一次性轻量请求：不走鉴权、不读响应体、不做并发与轮询。
//! 内置公共服务常量在 [`crate::config`]（`BUILTIN_COMMUNITY_SIGNER_*`）。

use crate::config::SignerService;
use std::time::{Duration, Instant};

/// 测延时的超时上限：超过就算失败，不再等。
pub const MEASURE_TIMEOUT: Duration = Duration::from_secs(5);

/// 从签名端点地址取展示名：去掉 scheme、userinfo 与路径，只留主机（含端口）。
///
/// 解析不出主机时原样返回，界面至少还能看到完整地址。userinfo（`user:{{PASSWORD_4wml3ra2}}@`）
/// 与 query/fragment 不进展示名（PR #348 二轮审查，安全项）：那两段可能带凭据。
pub fn signer_display_name(url: &str) -> String {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host.is_empty() {
        trimmed.to_string()
    } else {
        host.to_string()
    }
}

/// 展示用的端点地址：去掉 userinfo 与 query/fragment（可能带凭据的两段），
/// scheme + 主机 + 路径原样保留（PR #348 二轮审查，安全项）。
pub fn display_endpoint(url: &str) -> String {
    let trimmed = url.trim();
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (scheme, rest),
        None => return trimmed.to_string(),
    };
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let authority_and_path = &rest[..end];
    let authority_end = authority_and_path
        .find('/')
        .unwrap_or(authority_and_path.len());
    let authority = &authority_and_path[..authority_end];
    let path = &authority_and_path[authority_end..];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{host}{path}")
}

/// 签名服务地址校验（添加 / 表单提交共用一条规则，PR #348 二轮审查）：
/// - `https://` 任意主机都收；
/// - `http://` 只收环回主机（`127.0.0.1` / `localhost` / `::1` / `[::1]`）——
///   与 `HttpSignature::sign` 的拒绝语义一致，别让用户存一个必然发不出去的
///   明文远程地址；
/// - 其他一律拒绝，返回 i18n 稳定 key 交给界面翻译。
pub fn validate_signer_url(url: &str) -> Result<(), &'static str> {
    let trimmed = url.trim();
    let (scheme, rest) = match trimmed.split_once("://") {
        Some(pair) => pair,
        None => return Err("签名服务地址要以 http(s):// 开头"),
    };
    if scheme != "http" && scheme != "https" {
        return Err("签名服务地址要以 http(s):// 开头");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // userinfo（user:{{PASSWORD_4wml3ra2}}@host）一律拒绝（PR #348 三轮审查）：表单里不该有
    // 凭据，写进配置等于明文多存一份密码，客户端也不会拿它当 Bearer 用。
    if authority.contains('@') {
        return Err("签名服务地址不能包含用户名密码（user:{{PASSWORD_4wml3ra2}}@host）");
    }
    // 主机名必填（PR #348 三轮审查）：`https:///sign` 这种没有主机的地址
    // 之前会被放过去，存下来必然发不出去。
    let host = host_without_port(authority);
    if host.is_empty() {
        return Err("签名服务地址缺少主机名");
    }
    match scheme {
        "https" => Ok(()),
        // 环回判断大小写不敏感（PR #348 三轮审查，与固定签名提供者对齐）：
        // `http://LOCALHOST/sign` 也是本机明文。
        "http" => {
            let loopback = matches!(
                host.to_ascii_lowercase().as_str(),
                "127.0.0.1" | "localhost" | "[::1]" | "::1"
            );
            if loopback {
                Ok(())
            } else {
                Err("明文 http 只能指向本机（127.0.0.1 / localhost / ::1）；远程服务请用 https")
            }
        }
        _ => Err("签名服务地址要以 http(s):// 开头"),
    }
}

/// authority → 主机段：剥掉端口；IPv6 括号优先（`[::1]:9000` → `[::1]`）。
fn host_without_port(authority: &str) -> &str {
    if authority.starts_with('[') {
        let end = authority
            .find(']')
            .map(|index| index + 1)
            .unwrap_or(authority.len());
        return &authority[..end];
    }
    authority.split(':').next().unwrap_or_default()
}

/// token 掩码展示（#321 红线）：只露末四位，不足四位整体打码。
///
/// 判断性信息（长度）也不完全隐藏：打码的圆点数按真实长度给，方便确认
/// 「确实填了东西」，又不至于把短 token 泄出去。
pub fn mask_token(token: &str) -> String {
    let token = token.trim();
    let chars: Vec<char> = token.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    if chars.len() <= 4 {
        return "•".repeat(chars.len());
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••{tail}")
}

/// 删除一条签名服务：返回删除后的列表与调整过的激活下标
/// （PR #348 三轮审查：删除 + 下标重映射统一收口在这一个纯函数里）。
///
/// 规则：删除**激活项之前**的项，激活下标减一（还是同一个服务）；删的就是
/// 激活项，回 0（内置项兜底，归一化保障下标有效）；删激活项**之后**的项，
/// 下标不变。调用方把返回的 `(列表, 下标)` 交给 `normalize_signers` 收尾——
/// 归一化会按地址重新定位激活项，重排（内置置顶）不会改变用户在用的服务。
pub fn remove_signer_entry(
    mut signers: Vec<SignerService>,
    active: usize,
    index: usize,
) -> (Vec<SignerService>, usize) {
    if index >= signers.len() {
        return (signers, active);
    }
    signers.remove(index);
    let active = if index < active {
        active - 1
    } else if index == active {
        0
    } else {
        active
    };
    (signers, active)
}

/// 删除一条服务后激活下标应该变成多少（PR #348 审查意见）。
///
/// 删除会移动后续条目的下标：删激活项**之前**的项，激活下标减一（同一服务）；
/// 删的恰好是激活项，回 0（内置项兜底，归一化保障下标有效）；删激活项
/// **之后**的项，下标不变。调用方先调这里再删列表项，交给归一化收尾。
pub fn active_index_after_removal(active: usize, removed: usize) -> usize {
    if removed < active {
        active - 1
    } else if removed == active {
        0
    } else {
        active
    }
}

/// 对签名端点发一次轻量 GET 测往返耗时（毫秒）。
///
/// 只测「网络可达 + 服务应答」：拿到任何 HTTP 应答（含 3xx/4xx/5xx）都算成功——
/// 签名端点只认带完整载荷的 POST，GET 被拒不影响时延结论；连接失败或超时
/// 才返回 `Err`。请求不带 token：测时延不需要鉴权，也不把凭据发到非必要路径上。
/// 不跟随重定向（PR #348 二轮审查）：302 说明配置的端点本身应答了，重定向
/// 目标不可达不该算成这条服务失败。
///
/// 含 userinfo（`user:{{PASSWORD_4wml3ra2}}@host` 形式）的地址直接拒绝：ureq 会把它变成
/// Basic 认证头发出去，明文 http 下等于把凭据白送给网络上的旁观者；测时延
/// 不需要凭据。
pub fn measure_signer_latency(url: &str) -> Result<u128, String> {
    let url = url.trim();
    let url = reject_userinfo_url(url)?;
    // 测速用**无凭据端点**（PR #348 三轮审查）：剥掉 query 与 fragment——
    // 面板展示也看不见它们，但 ureq 会把 query 原样放进请求行，明文 http
    // 下等于把 query 里的凭据白送给路径上的旁观者。签名端点靠主机与路径
    // 就能识别，剥掉不影响时延结论。
    let url = probe_endpoint(url);
    let start = Instant::now();
    let agent = ureq::AgentBuilder::new()
        // 302 也算「配置的端点应答了」，不要追着重定向目标打。
        .redirects(0)
        .build();
    let result = agent.get(url).timeout(MEASURE_TIMEOUT).call();
    let elapsed = start.elapsed();
    match result {
        Ok(_) => Ok(elapsed.as_millis()),
        // 服务器的 3xx/4xx/5xx 应答也算「应答了」：时延有效。
        Err(ureq::Error::Status(_, _)) => Ok(elapsed.as_millis()),
        Err(err) => Err(err.to_string()),
    }
}

/// 拒绝带 userinfo（`user:{{PASSWORD_4wml3ra2}}@host`）的地址：测时延不需要凭据，
/// ureq 会拿 userinfo 生成 Basic 认证头，明文传输等于泄露（PR #348 二轮审查）。
/// 测速用的端点：剥掉 query 与 fragment（可能带凭据的两段）。
fn probe_endpoint(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

fn reject_userinfo_url(url: &str) -> Result<&str, String> {
    let without_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("签名服务地址缺少 scheme：{url}"))?;
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.contains('@') {
        return Err(
            "签名服务地址不能包含用户名密码（user:{{PASSWORD_4wml3ra2}}@host）".to_string(),
        );
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Settings;

    #[test]
    fn display_name_keeps_host_and_port() {
        assert_eq!(
            signer_display_name("https://signer.example.com:8443/sign"),
            "signer.example.com:8443"
        );
        assert_eq!(
            signer_display_name("http://127.0.0.1:8899/sign"),
            "127.0.0.1:8899"
        );
        assert_eq!(
            signer_display_name("signer.example.com"),
            "signer.example.com"
        );
        // 解析不出主机时原样返回，不给出空串。
        assert_eq!(signer_display_name("https:///sign"), "https:///sign");
    }

    /// 展示名与展示端点都不得带 userinfo / query（PR #348 二轮审查，安全项）：
    /// 那两段可能携带凭据，不能上屏。
    #[test]
    fn display_helpers_strip_userinfo_and_query() {
        assert_eq!(
            signer_display_name("https://alice:{{PASSWORD_4wml3ra2}}@signer.example.com/sign"),
            "signer.example.com"
        );
        assert_eq!(
            display_endpoint("https://alice:{{PASSWORD_4wml3ra2}}@signer.example.com/sign"),
            "https://signer.example.com/sign"
        );
        assert_eq!(
            display_endpoint("https://signer.example.com/sign?token=1#frag"),
            "https://signer.example.com/sign"
        );
        assert_eq!(
            display_endpoint("http://127.0.0.1:8899/sign"),
            "http://127.0.0.1:8899/sign"
        );
        assert_eq!(display_endpoint("signer.example.com"), "signer.example.com");
    }

    /// 地址校验：https 任意主机；http 仅环回（与 HttpSignature::sign 的拒绝
    /// 语义一致，PR #348 二轮审查）。
    #[test]
    fn validate_signer_url_accepts_https_and_loopback_http_only() {
        assert_eq!(
            validate_signer_url("https://signer.example.com/sign"),
            Ok(())
        );
        assert_eq!(validate_signer_url("http://127.0.0.1:8899/sign"), Ok(()));
        assert_eq!(validate_signer_url("http://localhost/sign"), Ok(()));
        assert_eq!(validate_signer_url("http://[::1]:9000/sign"), Ok(()));
        assert_eq!(validate_signer_url("http://[::1]/sign"), Ok(()));
        assert_eq!(validate_signer_url("  https://a.example.com  "), Ok(()));
        // 明文远程 http 一律拒绝。
        assert!(validate_signer_url("http://signer.example.com/sign").is_err());
        assert!(validate_signer_url("http://192.168.1.10:8899/sign").is_err());
        // userinfo 一律拒绝（PR #348 三轮审查）：表单里不该有凭据，环回
        // 主机也不例外——环回判断不看 @ 之后的主机段。
        assert!(validate_signer_url("http://user@localhost/sign").is_err());
        assert!(validate_signer_url("https://user@signer.example.com/sign").is_err());
        assert!(validate_signer_url("http://user@remote.example.com/sign").is_err());
        // 无 scheme / 其他 scheme 拒绝。
        assert!(validate_signer_url("signer.example.com").is_err());
        assert!(validate_signer_url("ftp://signer.example.com").is_err());
        assert!(validate_signer_url("").is_err());
    }

    #[test]
    fn mask_shows_only_last_four() {
        assert_eq!(mask_token(""), "");
        assert_eq!(mask_token("abc"), "•••");
        assert_eq!(mask_token("abcd"), "••••");
        assert_eq!(mask_token("abcde"), "••••bcde");
        assert_eq!(mask_token("05f8089b8c5f"), "••••8c5f");
        // 长度按字符数算，不按字节：中文 token 也不会被切成半个字符。
        assert_eq!(mask_token("令牌一二三四五"), "••••二三四五");
    }

    #[test]
    fn measure_rejects_empty_url_without_network() {
        assert!(measure_signer_latency("  ").is_err());
    }

    /// 带 userinfo 的地址在发请求之前就被拒绝（PR #348 二轮审查，安全项）：
    /// ureq 会把 userinfo 变成 Basic 认证头，测时延不需要凭据。
    #[test]
    fn measure_rejects_userinfo_urls_before_request() {
        let err =
            measure_signer_latency("http://user:{{PASSWORD_4wml3ra2}}@signer.example.com/sign")
                .expect_err("userinfo 地址必须被拒绝");
        assert!(err.contains("用户名密码"), "{err}");
    }

    /// 合法地址能通过 userinfo 检查（PR #348 三轮审查）：直接测纯函数，
    /// 不依赖真实 DNS——真发 GET 的话，测试环境的 DNS 覆盖/代理一旦回了
    /// 任何 HTTP 应答，测速就会返回 Ok，断言失败。
    #[test]
    fn userinfo_check_passes_credential_free_urls() {
        assert!(reject_userinfo_url("https://signer.example.com/sign").is_ok());
        assert!(reject_userinfo_url("http://127.0.0.1:8899/sign").is_ok());
    }

    /// 测速端点剥掉 query / fragment（PR #348 三轮审查，安全项）：query 里
    /// 可能带凭据，ureq 会原样放进请求行。
    #[test]
    fn probe_endpoint_strips_query_and_fragment() {
        assert_eq!(
            probe_endpoint("https://signer.example.com/sign?token=abc#frag"),
            "https://signer.example.com/sign"
        );
        assert_eq!(
            probe_endpoint("https://signer.example.com/sign"),
            "https://signer.example.com/sign"
        );
        assert_eq!(
            probe_endpoint("https://signer.example.com/sign#frag"),
            "https://signer.example.com/sign"
        );
    }

    /// 删除服务时激活下标的三条规则（PR #348 审查意见）：删激活项之前的减一、
    /// 删激活项回 0、删激活项之后的保持——否则删掉前置条目后旧下标会指向
    /// 别的服务，归一化把「没选过」的服务当激活项。
    #[test]
    fn active_index_rules_after_removal() {
        // [默认, 社区, A, B, C]，激活 B（下标 3）。
        // 删激活项之前的 A（下标 2）：B 前移到 2，还是 B。
        assert_eq!(active_index_after_removal(3, 2), 2);
        // 删的正是激活项 B（下标 3）：回 0，归一化落内置默认服务。
        assert_eq!(active_index_after_removal(3, 3), 0);
        // 删激活项之后的 C（下标 4）：B 仍在 3。
        assert_eq!(active_index_after_removal(3, 4), 3);
        // 边界：删 0 号（内置默认）且激活在 0——回 0。
        assert_eq!(active_index_after_removal(0, 0), 0);
    }

    /// 删除 + 重映射 + 归一化的组合路径（PR #348 三轮审查）：审查方给的
    /// 场景 `[默认, 社区, A, B, C]`、B 激活（下标 3）、删 A——激活的必须是
    /// B 而不是移位后下标 3 指到的 C。列表收口在 `remove_signer_entry`，
    /// 归一化按地址重定位，两步合起来不丢激活服务。
    #[test]
    fn remove_then_normalize_keeps_active_service() {
        // 真实列表形态：两台内置项置顶，用户服务排其后。
        let list = |names: &[&str]| -> Vec<SignerService> {
            [
                (
                    crate::config::DEFAULT_SIGNER_URL,
                    crate::config::DEFAULT_SIGNER_TOKEN,
                ),
                (
                    crate::config::BUILTIN_COMMUNITY_SIGNER_URL,
                    crate::config::BUILTIN_COMMUNITY_SIGNER_TOKEN,
                ),
            ]
            .into_iter()
            .map(|(url, token)| SignerService {
                url: url.to_string(),
                token: token.to_string(),
            })
            .chain(names.iter().map(|name| SignerService {
                url: format!("http://{name}.example.com/sign"),
                token: String::new(),
            }))
            .collect()
        };
        // [默认, 社区, A, B, C]（内置置顶），B 激活在下标 3。
        let (signers, active) = remove_signer_entry(
            list(&["a", "b", "c"]),
            3,
            2, // 删 A（激活项之前）
        );
        let mut settings = Settings {
            signers,
            active_signer: active,
            ..Default::default()
        };
        assert!(settings.normalize_signers());
        let active_url = settings.signers[settings.active_signer].url.clone();
        assert_eq!(active_url, "http://b.example.com/sign");
        assert_eq!(settings.signer_url, active_url);

        // 删的就是激活项 B：回 0，归一化落内置默认服务。
        let (signers, active) = remove_signer_entry(list(&["a", "b", "c"]), 3, 3);
        let mut settings = Settings {
            signers,
            active_signer: active,
            ..Default::default()
        };
        // 删激活项回 0 后状态已是归一化形态（normalize 可能返回 false）。
        settings.normalize_signers();
        assert!(Settings::is_builtin_signer(
            &settings.signers[settings.active_signer].url
        ));
    }
}
