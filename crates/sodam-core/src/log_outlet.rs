//! 诊断日志出口：本机日志目录解析、按大小轮转、保留份数与内存降级。
//!
//! 设计规则（见 Issue #57 / #58 / #61 / #53）：
//! - 目录在用户数据目录，默认开启（INFO 级）；
//! - 单文件 5 MB 轮转，最多保留 5 份旧文件；
//! - 目录不可写时不 panic、不阻塞，只在内存中降级缓冲；
//! - 行格式为 `<时间> <级别> <话题> <事件> key=value …`，字段顺序固定，便于 grep；
//! - 级别仅 ERROR / WARN / INFO / DEBUG；时间戳带本地时区偏移；
//! - 首版六个话题：player / prefetch / audio / report / media / app；
//! - 旧的环境变量开关（`SODAM_*_LOG`）只决定「写到多细」，把所属话题提到 DEBUG；
//! - 写盘前切断 URL 查询串，抹去 Cookie、Authorization、x-helios、x-medusa 与签名 token；
//! - 账号 user_id 在 Info 级不写，只在 Debug 级写；
//! - 连续重复的行折叠并附计数，避免错误风暴把日志撑爆。

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::SystemTime;

pub const DEFAULT_MAX_FILE_SIZE: u64 = 5 * 1024 * 1024; // 5 MB
pub const DEFAULT_MAX_OLD_FILES: usize = 5;
pub const LOG_FILE_NAME: &str = "sodam.log";
pub const MEMORY_BUFFER_LIMIT: usize = 500;

/// 话题级覆盖的「未设置」哨兵：`LogLevel` 从 1 开始，0 不会与任何级别冲突。
const NO_TOPIC_OVERRIDE: u8 = 0;

/// 旧的环境变量开关，现在的语义是「把所属话题提到 DEBUG」（默认 INFO）。
///
/// 粒度是话题而不是事件：`scene / theme / cover / frame` 都归 `app`，
/// 设其中任意一个，该话题的 DEBUG 行就都会记下来。
pub const TOPIC_VERBOSITY_ENV: [(&str, LogTopic); 7] = [
    ("SODAM_PLAYER_LOG", LogTopic::Player),
    ("SODAM_PREFETCH_LOG", LogTopic::Prefetch),
    ("SODAM_REPORT_LOG", LogTopic::Report),
    ("SODAM_SCENE_LOG", LogTopic::App),
    ("SODAM_THEME_LOG", LogTopic::App),
    ("SODAM_COVER_LOG", LogTopic::App),
    ("SODAM_FRAME_LOG", LogTopic::App),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
    MacOS,
}

/// 诊断日志级别（四种级别）。
///
/// 约束规则：
/// - 仅支持 ERROR / WARN / INFO / DEBUG 四种级别；
/// - 不提供「完全关闭日志」的选项，至少 ERROR 级不可关闭；
/// - 账号 user_id 仅在 DEBUG 级输出，INFO/WARN/ERROR 级不输出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl LogLevel {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.eq_ignore_ascii_case("debug") {
            Ok(Self::Debug)
        } else if trimmed.eq_ignore_ascii_case("info") {
            Ok(Self::Info)
        } else if trimmed.eq_ignore_ascii_case("warn") || trimmed.eq_ignore_ascii_case("warning") {
            Ok(Self::Warn)
        } else if trimmed.eq_ignore_ascii_case("error") {
            Ok(Self::Error)
        } else {
            Err(format!(
                "invalid log level '{trimmed}', expected ERROR/WARN/INFO/DEBUG"
            ))
        }
    }
}

/// 把 `AtomicU8` 里的原始值还原成级别；未知值安全回落到 INFO。
fn level_from_u8(value: u8) -> LogLevel {
    match value {
        value if value == LogLevel::Warn as u8 => LogLevel::Warn,
        value if value == LogLevel::Error as u8 => LogLevel::Error,
        value if value == LogLevel::Debug as u8 => LogLevel::Debug,
        _ => LogLevel::Info,
    }
}

/// 诊断日志首版六个话题。
///
/// 约束规则（见 Issue #58）：
/// - 六个话题都在，且没有第七个未登记的话题名出现；
/// - 话题名为小写单词，不带前缀，不复用模块路径；
/// - 新增话题需在此处登记。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogTopic {
    Player,
    Prefetch,
    Audio,
    Report,
    Media,
    App,
}

impl LogTopic {
    pub const ALL: [LogTopic; 6] = [
        LogTopic::Player,
        LogTopic::Prefetch,
        LogTopic::Audio,
        LogTopic::Report,
        LogTopic::Media,
        LogTopic::App,
    ];

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Player => "player",
            Self::Prefetch => "prefetch",
            Self::Audio => "audio",
            Self::Report => "report",
            Self::Media => "media",
            Self::App => "app",
        }
    }

    /// 在 `ALL` 里的下标，也是话题级过滤器的数组下标。
    pub const fn index(&self) -> usize {
        match self {
            Self::Player => 0,
            Self::Prefetch => 1,
            Self::Audio => 2,
            Self::Report => 3,
            Self::Media => 4,
            Self::App => 5,
        }
    }
}

impl std::fmt::Display for LogTopic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for LogTopic {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "player" => Ok(Self::Player),
            "prefetch" => Ok(Self::Prefetch),
            "audio" => Ok(Self::Audio),
            "report" => Ok(Self::Report),
            "media" => Ok(Self::Media),
            "app" => Ok(Self::App),
            other => Err(format!("unregistered topic '{other}'")),
        }
    }
}

/// 解析指定平台下的日志根目录。
pub fn resolve_platform_log_dir(
    platform: Platform,
    local_app_data: Option<&Path>,
    state_dir: Option<&Path>,
    data_dir: Option<&Path>,
    home_dir: Option<&Path>,
) -> PathBuf {
    match platform {
        Platform::Windows => local_app_data
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir)
            .join("sodam")
            .join("logs"),
        Platform::MacOS => data_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir)
            .join("sodam")
            .join("logs"),
        Platform::Linux => state_dir
            .map(Path::to_path_buf)
            .or_else(|| home_dir.map(|h| h.join(".local").join("state")))
            .unwrap_or_else(std::env::temp_dir)
            .join("sodam")
            .join("logs"),
    }
}

/// 获取当前平台的日志目录；若设置了 `SODAM_LOG_DIR` 环境变量则优先使用。
pub fn log_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SODAM_LOG_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    default_log_dir()
}

fn default_log_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    let platform = Platform::Windows;
    #[cfg(target_os = "macos")]
    let platform = Platform::MacOS;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let platform = Platform::Linux;

    resolve_platform_log_dir(
        platform,
        dirs::data_local_dir().as_deref(),
        dirs::state_dir().as_deref(),
        dirs::data_dir().as_deref(),
        dirs::home_dir().as_deref(),
    )
}

// Howard Hinnant 民用历算法转换：将自 UNIX_EPOCH 以来的天数转换为年月日
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097; // 400 年周期序号
    let doe = (z - era * 146097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month index [0, 11] (3月起计)
    let d = doy - (153 * mp + 2) / 5 + 1; // day of month [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // 还原公历月 [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// 获取指定时间戳的本地时区偏移秒数（无外部依赖，跨平台调用系统接口）。
pub fn get_local_offset_seconds(unix_secs: i64) -> i32 {
    #[cfg(windows)]
    {
        let _ = unix_secs;
        #[repr(C)]
        #[allow(clippy::upper_case_acronyms)]
        struct SYSTEMTIME {
            w_year: u16,
            w_month: u16,
            w_day_of_week: u16,
            w_day: u16,
            w_hour: u16,
            w_minute: u16,
            w_second: u16,
            w_milliseconds: u16,
        }
        #[repr(C)]
        #[allow(clippy::upper_case_acronyms)]
        struct TIME_ZONE_INFORMATION {
            bias: i32,
            standard_name: [u16; 32],
            standard_date: SYSTEMTIME,
            standard_bias: i32,
            daylight_name: [u16; 32],
            daylight_date: SYSTEMTIME,
            daylight_bias: i32,
        }
        extern "system" {
            fn GetTimeZoneInformation(lp_time_zone_information: *mut TIME_ZONE_INFORMATION) -> u32;
        }
        unsafe {
            let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
            let res = GetTimeZoneInformation(&mut tz);
            let extra = match res {
                2 => tz.daylight_bias,
                _ => tz.standard_bias,
            };
            // Windows: UTC = local + bias + extra => local - UTC = -(bias + extra) 分钟
            -(tz.bias + extra) * 60
        }
    }
    #[cfg(not(windows))]
    {
        #[repr(C)]
        struct tm {
            tm_sec: std::os::raw::c_int,
            tm_min: std::os::raw::c_int,
            tm_hour: std::os::raw::c_int,
            tm_mday: std::os::raw::c_int,
            tm_mon: std::os::raw::c_int,
            tm_year: std::os::raw::c_int,
            tm_wday: std::os::raw::c_int,
            tm_yday: std::os::raw::c_int,
            tm_isdst: std::os::raw::c_int,
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
            tm_gmtoff: std::os::raw::c_long,
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
            tm_zone: *const std::os::raw::c_char,
        }
        extern "C" {
            fn localtime_r(time_p: *const i64, result: *mut tm) -> *mut tm;
        }
        unsafe {
            let mut out: tm = std::mem::zeroed();
            let t = unix_secs;
            if !localtime_r(&t, &mut out).is_null() {
                #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
                {
                    return out.tm_gmtoff as i32;
                }
            }
            0
        }
    }
}

/// 将 SystemTime 格式化为指定时区偏移的 ISO-8601 本地时间戳：`YYYY-MM-DDThh:mm:ss.mmm±hh:mm`。
pub fn format_timestamp_with_offset(now: SystemTime, offset_secs: i32) -> String {
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let unix_secs = duration.as_secs() as i64;
    let millis = duration.subsec_millis();

    let local_secs = unix_secs + (offset_secs as i64);
    let day_secs = local_secs.rem_euclid(86400) as u32;
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;

    let days = local_secs.div_euclid(86400);
    let (y, m, d) = civil_from_days(days);

    let (sign, abs_offset) = if offset_secs >= 0 {
        ('+', offset_secs as u32)
    } else {
        ('-', (-offset_secs) as u32)
    };
    let tz_hour = abs_offset / 3600;
    let tz_min = (abs_offset % 3600) / 60;

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}{sign}{tz_hour:02}:{tz_min:02}")
}

/// 格式化本地时间戳：`YYYY-MM-DDThh:mm:ss.mmm±hh:mm`（带本地时区偏移）。
pub fn format_local_timestamp(now: SystemTime) -> String {
    let unix_secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let offset = get_local_offset_seconds(unix_secs);
    format_timestamp_with_offset(now, offset)
}

/// 转义键值对的值：若含空格、换行、双引号、等号或为空，用双引号包裹并转义换行符及引号。
pub fn escape_field_value(value: &str) -> String {
    let needs_quote = value.is_empty() || value.contains([' ', '\t', '"', '\n', '\r', '=']);

    if !needs_quote {
        return value.to_string();
    }

    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// 格式化单个 key=value 字段。
pub fn format_field(key: &str, value: &str) -> String {
    format!("{key}={}", escape_field_value(value))
}

/// 判断是否为凭据类敏感字段键。
pub fn is_sensitive_credential_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower == "cookie"
        || lower == "set-cookie"
        || lower.ends_with("cookie")
        || lower.ends_with("_cookie")
        || lower.ends_with("-cookie")
        || lower == "token"
        || lower.ends_with("token")
        || lower.ends_with("_token")
        || lower.ends_with("-token")
        || lower == "authorization"
        || lower == "proxy-authorization"
        || lower == "auth"
        || lower.starts_with("auth_")
        || lower.starts_with("auth-")
        || lower.ends_with("_auth")
        || lower.ends_with("-auth")
        || lower == "x-helios"
        || lower == "x-medusa"
        || lower == "signer_token"
        || lower == "signer token"
}

/// 判断是否为 user_id 键名。
pub fn is_user_id_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower == "user_id" || lower == "uid" || lower == "account_id"
}

/// 对文本行彻底剔除 Cookie、Authorization、x-helios、x-medusa 与签名 token。
pub fn redact_sensitive_credentials(text: &str) -> String {
    let url_redacted = crate::failure::redact_url_secrets(text);
    let lower = url_redacted.to_ascii_lowercase();
    let has_sensitive = lower.contains("cookie")
        || lower.contains("authorization")
        || lower.contains("x-helios")
        || lower.contains("x-medusa")
        || lower.contains("token");

    if !has_sensitive {
        return url_redacted;
    }

    let mut out = String::with_capacity(url_redacted.len());
    let bytes = url_redacted.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        let is_boundary_start = i == 0
            || (!bytes[i - 1].is_ascii_alphanumeric()
                && bytes[i - 1] != b'_'
                && bytes[i - 1] != b'-');

        let mut matched: Option<(&str, usize)> = None;

        if is_boundary_start {
            let remaining_lower = &lower[i..];

            // 1. 特殊短语：signer token / signer-token / signer_token（不强制要求冒号/等号分隔符，直接吃掉后面一个词）
            if remaining_lower.starts_with("signer token")
                || remaining_lower.starts_with("signer-token")
                || remaining_lower.starts_with("signer_token")
            {
                let sep = i + 12;
                if sep < len && matches!(bytes[sep], b' ' | b'\t' | b':' | b'=') {
                    let mut val_start = sep;
                    while val_start < len && (bytes[val_start] == b' ' || bytes[val_start] == b'\t')
                    {
                        val_start += 1;
                    }
                    if val_start < len && (bytes[val_start] == b':' || bytes[val_start] == b'=') {
                        val_start += 1;
                        while val_start < len
                            && (bytes[val_start] == b' ' || bytes[val_start] == b'\t')
                        {
                            val_start += 1;
                        }
                    }
                    if val_start < len && bytes[val_start] != b'\n' && bytes[val_start] != b'\r' {
                        matched = Some(("signer token", val_start));
                    }
                }
            } else if remaining_lower.starts_with("authorization")
                || remaining_lower.starts_with("proxy-authorization")
            {
                // 2. 特殊头部：authorization / proxy-authorization
                let pat_len = if remaining_lower.starts_with("authorization") {
                    13
                } else {
                    19
                };
                let mut sep = i + pat_len;
                while sep < len && (bytes[sep] == b' ' || bytes[sep] == b'\t') {
                    sep += 1;
                }
                if sep < len && (bytes[sep] == b':' || bytes[sep] == b'=') {
                    matched = Some(("authorization", sep + 1));
                } else if lower[sep..].starts_with("bearer ") || lower[sep..].starts_with("basic ")
                {
                    matched = Some(("authorization", sep));
                }
            } else {
                // 3. 通用标识符扫描：如 user_token=..., x-token:..., cookie=..., token=...
                let mut k = i;
                while k < len
                    && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'_' || bytes[k] == b'-')
                {
                    k += 1;
                }
                if k > i {
                    let cand = &lower[i..k];
                    let mut sep = k;
                    while sep < len && (bytes[sep] == b' ' || bytes[sep] == b'\t') {
                        sep += 1;
                    }
                    if sep < len
                        && (bytes[sep] == b':' || bytes[sep] == b'=')
                        && is_sensitive_credential_key(cand)
                    {
                        matched = Some((cand, sep + 1));
                    }
                }
            }
        }

        if let Some((kind, val_start)) = matched {
            let mut j = val_start;

            // 1. 跳过值前面的空白符
            while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }

            // 2. 处理引号包裹的值（支持转义字符）
            if j < len && (bytes[j] == b'"' || bytes[j] == b'\'') {
                let quote = bytes[j];
                j += 1;
                let mut escaped = false;
                while j < len {
                    let b = bytes[j];
                    if escaped {
                        escaped = false;
                        let ch = url_redacted[j..].chars().next().unwrap();
                        j += ch.len_utf8();
                    } else if b == b'\\' {
                        escaped = true;
                        j += 1;
                    } else if b == quote {
                        j += 1;
                        break;
                    } else if b == b'\n' || b == b'\r' {
                        break;
                    } else {
                        let ch = url_redacted[j..].chars().next().unwrap();
                        j += ch.len_utf8();
                    }
                }
            } else if kind == "authorization" || kind == "proxy-authorization" {
                // 3. 处理 Authorization: Bearer <token> 或 Basic <token> 或普通 token
                let rem_lower = &lower[j..];
                if rem_lower.starts_with("bearer ") {
                    j += 7;
                    while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        j += 1;
                    }
                } else if rem_lower.starts_with("basic ") {
                    j += 6;
                    while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        j += 1;
                    }
                }
                while j < len {
                    let b = bytes[j];
                    if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b';' | b',') {
                        break;
                    }
                    let ch = url_redacted[j..].chars().next().unwrap();
                    j += ch.len_utf8();
                }
            } else if kind == "cookie" || kind == "set-cookie" {
                // 4. 处理多键值对 Cookie，例如 "Cookie: a=1; b=2; c=3"
                loop {
                    while j < len {
                        let b = bytes[j];
                        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b';' | b',') {
                            break;
                        }
                        let ch = url_redacted[j..].chars().next().unwrap();
                        j += ch.len_utf8();
                    }
                    if j < len && (bytes[j] == b';' || bytes[j] == b',') {
                        let sep_pos = j;
                        let mut peek = j + 1;
                        while peek < len && (bytes[peek] == b' ' || bytes[peek] == b'\t') {
                            peek += 1;
                        }
                        let mut has_eq = false;
                        let mut k = peek;
                        while k < len {
                            let b = bytes[k];
                            if matches!(
                                b,
                                b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b';' | b','
                            ) {
                                break;
                            }
                            if b == b'=' {
                                has_eq = true;
                                break;
                            }
                            let ch = url_redacted[k..].chars().next().unwrap();
                            k += ch.len_utf8();
                        }
                        if has_eq {
                            j = peek;
                            continue;
                        } else {
                            j = sep_pos + 1;
                            break;
                        }
                    } else {
                        break;
                    }
                }
            } else {
                // 5. 其他凭据字段（如 user_token, x-token, x-helios, x-medusa, signer token）
                while j < len {
                    let b = bytes[j];
                    if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b';' | b',') {
                        break;
                    }
                    let ch = url_redacted[j..].chars().next().unwrap();
                    j += ch.len_utf8();
                }
                if j < len && (bytes[j] == b'"' || bytes[j] == b';') {
                    j += 1;
                }
            }

            out.push_str("[REDACTED]");
            i = j;
        } else {
            let ch = url_redacted[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }

    out
}

fn scrub_user_id_if_not_debug(line: &str) -> String {
    if line.split(' ').nth(1) == Some("DEBUG") {
        return line.to_string();
    }

    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        while i < len && bytes[i] == b' ' {
            i += 1;
        }
        if i >= len {
            break;
        }
        let token_start = i;
        let mut in_quote = false;
        let mut escaped = false;
        while i < len {
            let b = bytes[i];
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_quote = !in_quote;
            } else if b == b' ' && !in_quote {
                break;
            }
            let ch = line[i..].chars().next().unwrap();
            i += ch.len_utf8();
        }

        let token = &line[token_start..i];
        let lower = token.to_ascii_lowercase();
        let is_user_id = lower.starts_with("user_id=")
            || lower.starts_with("uid=")
            || lower.starts_with("account_id=");

        if !is_user_id {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(token);
        }
    }

    out
}

fn extract_line_key(line: &str) -> String {
    if let Some((first, rest)) = line.split_once(' ') {
        if first.len() >= 19 && first.contains('T') && first.contains(':') {
            return rest.to_string();
        }
    }
    line.to_string()
}

/// 结构化诊断日志记录。
#[derive(Debug, Clone)]
pub struct LogRecord<'a> {
    pub timestamp: SystemTime,
    pub level: LogLevel,
    pub topic: LogTopic,
    pub event: &'a str,
    pub fields: Vec<(&'a str, String)>,
}

impl<'a> LogRecord<'a> {
    pub fn new(level: LogLevel, topic: LogTopic, event: &'a str) -> Self {
        Self {
            timestamp: SystemTime::now(),
            level,
            topic,
            event,
            fields: Vec::new(),
        }
    }

    pub fn with_time(mut self, time: SystemTime) -> Self {
        self.timestamp = time;
        self
    }

    pub fn field(mut self, key: &'a str, value: impl ToString) -> Self {
        if is_user_id_key(key) && self.level != LogLevel::Debug {
            return self;
        }
        if is_sensitive_credential_key(key) {
            return self;
        }
        self.fields.push((key, value.to_string()));
        self
    }

    pub fn dedup_key(&self) -> String {
        let mut key = format!(
            "{} {} {}",
            self.level.as_str(),
            self.topic.as_str(),
            self.event
        );
        for (k, v) in &self.fields {
            key.push(' ');
            key.push_str(&format_field(k, v));
        }
        key
    }

    pub fn format_line(&self, offset_secs: i32) -> String {
        let ts = format_timestamp_with_offset(self.timestamp, offset_secs);
        let level = self.level.as_str();
        let topic = self.topic.as_str();
        let event = self.event;

        if self.fields.is_empty() {
            format!("{ts} {level} {topic} {event}")
        } else {
            let mut out = format!("{ts} {level} {topic} {event}");
            for (k, v) in &self.fields {
                out.push(' ');
                out.push_str(&format_field(k, v));
            }
            out
        }
    }
}

struct PendingRecord {
    key: String,
    line: String,
    count: usize,
}

impl PendingRecord {
    fn into_output_line(self) -> String {
        if self.count > 1 {
            format!("{} repeat={}", self.line, self.count)
        } else {
            self.line
        }
    }
}

fn rotate_files(log_dir: &Path, max_old_files: usize) -> std::io::Result<()> {
    if max_old_files > 0 {
        let oldest = log_dir.join(format!("{LOG_FILE_NAME}.{max_old_files}"));
        if oldest.exists() {
            let _ = fs::remove_file(&oldest);
        }
        for i in (1..max_old_files).rev() {
            let src = log_dir.join(format!("{LOG_FILE_NAME}.{i}"));
            if src.exists() {
                let dst = log_dir.join(format!("{LOG_FILE_NAME}.{}", i + 1));
                let _ = fs::remove_file(&dst);
                fs::rename(&src, &dst)?;
            }
        }
        let active = log_dir.join(LOG_FILE_NAME);
        if active.exists() {
            let dst = log_dir.join(format!("{LOG_FILE_NAME}.1"));
            let _ = fs::remove_file(&dst);
            fs::rename(&active, &dst)?;
        }
    } else {
        let active = log_dir.join(LOG_FILE_NAME);
        if active.exists() {
            let _ = fs::remove_file(&active);
        }
    }
    Ok(())
}

/// 删除日志文件并保留第一个真实错误；文件本来就不存在不算失败。
fn remove_log_file(path: &Path, first_error: &mut Option<std::io::Error>) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            if first_error.is_none() {
                *first_error = Some(err);
            }
        }
    }
}

enum OutletState {
    File { file: File, current_size: u64 },
    Memory { buffer: VecDeque<String> },
}

/// 写盘线程独占的状态：折叠缓冲与文件句柄都只在写盘线程里读写。
///
/// 调用方（含渲染线程与播放链路）只做脱敏并投递到通道，不再触碰文件 IO。
/// 每行 `flush()` 且全程持锁的写法放在播放路径上，就是让渲染线程做同步文件 IO。
struct WriterState {
    log_dir: PathBuf,
    max_file_size: u64,
    max_old_files: usize,
    pending: Option<PendingRecord>,
    inner: OutletState,
}

impl WriterState {
    fn new(log_dir: PathBuf, max_file_size: u64, max_old_files: usize) -> Self {
        let mut state = Self {
            log_dir,
            max_file_size,
            max_old_files,
            pending: None,
            inner: OutletState::Memory {
                buffer: VecDeque::with_capacity(MEMORY_BUFFER_LIMIT),
            },
        };

        if fs::create_dir_all(&state.log_dir).is_err() {
            return state;
        }

        let active = state.log_dir.join(LOG_FILE_NAME);
        let existing_size = fs::metadata(&active).map(|m| m.len()).unwrap_or(0);
        if existing_size >= max_file_size && rotate_files(&state.log_dir, max_old_files).is_err() {
            return state;
        }

        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&active) {
            let size = fs::metadata(&active).map(|m| m.len()).unwrap_or(0);
            state.inner = OutletState::File {
                file,
                current_size: size,
            };
        }
        state
    }

    fn is_degraded(&self) -> bool {
        matches!(self.inner, OutletState::Memory { .. })
    }

    fn memory_records(&mut self) -> Vec<String> {
        self.flush();
        match &self.inner {
            OutletState::Memory { buffer } => buffer.iter().cloned().collect(),
            OutletState::File { .. } => Vec::new(),
        }
    }

    fn flush(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.write_physical_line(pending.into_output_line());
        }
    }

    fn write_line(&mut self, sanitized: String) {
        let key = extract_line_key(&sanitized);
        if let Some(pending) = self.pending.as_mut() {
            if pending.key == key {
                pending.count += 1;
                return;
            }
        }
        let old = self.pending.replace(PendingRecord {
            key,
            line: sanitized,
            count: 1,
        });
        if let Some(old) = old {
            self.write_physical_line(old.into_output_line());
        }
    }

    fn write_physical_line(&mut self, line: String) {
        // 内存降级自愈检测
        if let OutletState::Memory { buffer } = &mut self.inner {
            let active = self.log_dir.join(LOG_FILE_NAME);
            if fs::create_dir_all(&self.log_dir).is_ok() {
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&active) {
                    let mut current_size = fs::metadata(&active).map(|m| m.len()).unwrap_or(0);
                    let mut write_failed = false;
                    while let Some(front) = buffer.front() {
                        let add = front.len() as u64 + 1;
                        if file.write_all(front.as_bytes()).is_ok() && file.write_all(b"\n").is_ok()
                        {
                            current_size += add;
                            buffer.pop_front();
                        } else {
                            write_failed = true;
                            break;
                        }
                    }
                    if !write_failed && file.flush().is_ok() {
                        self.inner = OutletState::File { file, current_size };
                    }
                }
            }
        }

        let add_len = line.len() as u64 + 1;

        let need_rotate = match &self.inner {
            OutletState::File { current_size, .. } => {
                *current_size + add_len > self.max_file_size && *current_size > 0
            }
            OutletState::Memory { .. } => false,
        };

        if need_rotate {
            self.inner = OutletState::Memory {
                buffer: VecDeque::with_capacity(MEMORY_BUFFER_LIMIT),
            };

            if rotate_files(&self.log_dir, self.max_old_files).is_ok() {
                let active = self.log_dir.join(LOG_FILE_NAME);
                if let Ok(file) = OpenOptions::new().create(true).append(true).open(&active) {
                    self.inner = OutletState::File {
                        file,
                        current_size: 0,
                    };
                }
            }
        }

        match &mut self.inner {
            OutletState::File { file, current_size } => {
                let mut ok = false;
                if file.write_all(line.as_bytes()).is_ok()
                    && file.write_all(b"\n").is_ok()
                    && file.flush().is_ok()
                {
                    *current_size += add_len;
                    ok = true;
                }
                if !ok {
                    let mut buffer = VecDeque::with_capacity(MEMORY_BUFFER_LIMIT);
                    buffer.push_back(line);
                    self.inner = OutletState::Memory { buffer };
                }
            }
            OutletState::Memory { buffer } => {
                if buffer.len() >= MEMORY_BUFFER_LIMIT {
                    buffer.pop_front();
                }
                buffer.push_back(line);
            }
        }
    }

    fn clear(&mut self) -> std::io::Result<()> {
        self.pending = None;
        self.inner = OutletState::Memory {
            buffer: VecDeque::with_capacity(MEMORY_BUFFER_LIMIT),
        };
        let active = self.log_dir.join(LOG_FILE_NAME);
        let mut first_error = None;
        remove_log_file(&active, &mut first_error);
        for i in 1..=self.max_old_files {
            remove_log_file(
                &self.log_dir.join(format!("{LOG_FILE_NAME}.{i}")),
                &mut first_error,
            );
        }
        match OpenOptions::new().create(true).append(true).open(&active) {
            Ok(file) => {
                let current_size = fs::metadata(&active).map(|m| m.len()).unwrap_or(0);
                self.inner = OutletState::File { file, current_size };
            }
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

enum WriterMsg {
    Line(String),
    Flush(Sender<()>),
    Snapshot(Sender<WriterSnapshot>),
    Clear(Sender<std::io::Result<()>>),
    Shutdown,
}

struct WriterSnapshot {
    degraded: bool,
    records: Vec<String>,
}

fn writer_loop(rx: Receiver<WriterMsg>, mut state: WriterState) {
    while let Ok(msg) = rx.recv() {
        match msg {
            WriterMsg::Line(line) => state.write_line(line),
            WriterMsg::Flush(ack) => {
                state.flush();
                let _ = ack.send(());
            }
            WriterMsg::Snapshot(ack) => {
                // 先 flush：内存降级可能因为目录恢复可写而自愈，degraded 要反映刷完后的状态。
                let records = state.memory_records();
                let degraded = state.is_degraded();
                let _ = ack.send(WriterSnapshot { degraded, records });
            }
            WriterMsg::Clear(ack) => {
                let result = state.clear();
                let _ = ack.send(result);
            }
            WriterMsg::Shutdown => {
                state.flush();
                break;
            }
        }
    }
    state.flush();
}

/// 诊断日志出口。
///
/// 所有写盘动作都落在专用线程；这里的公开方法只做脱敏、投递与（需要时）等待确认。
pub struct LogOutlet {
    log_dir: PathBuf,
    level: AtomicU8,
    /// 设置页的「详细日志」开关：开 = 六个话题全部提到 DEBUG。
    ///
    /// 与环境变量是并集关系：两者都只能让某个话题记更多，谁也不会把另一个关掉，
    /// 所以这里单独存一个标志，而不是把值写进 `topic_levels` 去覆盖环境变量的结果。
    detailed: AtomicBool,
    /// 每个话题的详细度覆盖；`NO_TOPIC_OVERRIDE` 表示跟随全局 `level`。
    topic_levels: [AtomicU8; LogTopic::ALL.len()],
    tx: Mutex<Option<Sender<WriterMsg>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

impl LogOutlet {
    /// 以默认上限（5 MB 单文件、保留 5 份旧文件）创建日志出口。
    pub fn new(log_dir: PathBuf) -> Self {
        Self::new_with_limits(log_dir, DEFAULT_MAX_FILE_SIZE, DEFAULT_MAX_OLD_FILES)
    }

    /// 以指定单文件大小与保留份数创建日志出口。
    pub fn new_with_limits(log_dir: PathBuf, max_file_size: u64, max_old_files: usize) -> Self {
        let state = WriterState::new(log_dir.clone(), max_file_size, max_old_files);
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("sodam-log-writer".to_string())
            .spawn(move || writer_loop(rx, state))
        {
            Ok(handle) => Self {
                log_dir,
                level: AtomicU8::new(LogLevel::Info as u8),
                detailed: AtomicBool::new(false),
                topic_levels: std::array::from_fn(|_| AtomicU8::new(NO_TOPIC_OVERRIDE)),
                tx: Mutex::new(Some(tx)),
                writer: Mutex::new(Some(handle)),
            },
            Err(_) => Self {
                log_dir,
                level: AtomicU8::new(LogLevel::Info as u8),
                detailed: AtomicBool::new(false),
                topic_levels: std::array::from_fn(|_| AtomicU8::new(NO_TOPIC_OVERRIDE)),
                tx: Mutex::new(None),
                writer: Mutex::new(None),
            },
        }
    }

    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    pub fn log_file_path(&self) -> PathBuf {
        self.log_dir.join(LOG_FILE_NAME)
    }

    fn send_line(&self, line: String) {
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(WriterMsg::Line(line));
        }
    }

    fn send_snapshot_request(&self) -> Option<Receiver<WriterSnapshot>> {
        let (ack_tx, ack_rx) = mpsc::channel();
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let tx = guard.as_ref()?;
        tx.send(WriterMsg::Snapshot(ack_tx)).ok()?;
        Some(ack_rx)
    }

    pub fn is_degraded(&self) -> bool {
        let Some(ack) = self.send_snapshot_request() else {
            return true;
        };
        ack.recv().map(|snapshot| snapshot.degraded).unwrap_or(true)
    }

    pub fn memory_records(&self) -> Vec<String> {
        let Some(ack) = self.send_snapshot_request() else {
            return Vec::new();
        };
        ack.recv()
            .map(|snapshot| snapshot.records)
            .unwrap_or_default()
    }

    /// 设置全局过滤级别；不提供完全关闭日志的选项，最高限制至 ERROR 级。
    pub fn set_level(&self, level: LogLevel) {
        self.level.store(level as u8, Ordering::Relaxed);
    }

    /// 获取当前生效的全局最低级别。
    pub fn level(&self) -> LogLevel {
        level_from_u8(self.level.load(Ordering::Relaxed))
    }

    /// 设置某个话题的过滤级别。
    ///
    /// 话题覆盖只能比全局更细（记更多），不会比全局更粗：环境变量只用来「多记」，
    /// 不用来「少记」，因此这里不会让某话题比默认更安静。
    pub fn set_topic_level(&self, topic: LogTopic, level: LogLevel) {
        self.topic_levels[topic.index()].store(level as u8, Ordering::Relaxed);
    }

    /// 某个话题实际生效的最低级别。
    ///
    /// 取三者的最小（= 最细）：全局级别、「详细日志」开关、话题覆盖。
    pub fn topic_level(&self, topic: LogTopic) -> LogLevel {
        let mut level = self.level();
        if self.detailed() {
            level = level.min(LogLevel::Debug);
        }
        let raw = self.topic_levels[topic.index()].load(Ordering::Relaxed);
        if raw != NO_TOPIC_OVERRIDE {
            level = level.min(level_from_u8(raw));
        }
        level
    }

    /// 设置页的「详细日志」开关；`true` = 六个话题全部记到 DEBUG。
    ///
    /// 即时生效，不需要重启；关掉只撤销这个开关，不会撤销环境变量提出的详细度。
    pub fn set_detailed(&self, detailed: bool) {
        self.detailed.store(detailed, Ordering::Relaxed);
    }

    pub fn detailed(&self) -> bool {
        self.detailed.load(Ordering::Relaxed)
    }

    /// 刷新待写盘的折叠缓冲区；返回时此前投递的行都已落到文件（或内存降级缓冲）。
    pub fn flush(&self) {
        let ack = {
            let (ack_tx, ack_rx) = mpsc::channel();
            let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
            let Some(tx) = guard.as_ref() else {
                return;
            };
            if tx.send(WriterMsg::Flush(ack_tx)).is_err() {
                return;
            }
            ack_rx
        };
        let _ = ack.recv();
    }

    /// 该话题的该级别是否会真的写出：调用方可以据此跳过昂贵的字段构造。
    pub fn enabled(&self, level: LogLevel, topic: LogTopic) -> bool {
        level >= self.topic_level(topic)
    }

    /// 让旧的环境变量生效：变量存在且不是 `0`，就把对应话题提到 DEBUG。
    ///
    /// 启动时由 [`init_with_dir`] 自动调用；测试里可以单独调用。
    pub fn apply_env_verbosity(&self) {
        for (name, topic) in TOPIC_VERBOSITY_ENV {
            let value = std::env::var(name).unwrap_or_default();
            if !value.trim().is_empty() && value.trim() != "0" {
                self.set_topic_level(topic, LogLevel::Debug);
            }
        }
    }

    /// 记录结构化诊断记录。
    pub fn record(&self, record: &LogRecord<'_>) {
        if !self.enabled(record.level, record.topic) {
            return;
        }
        let duration = record
            .timestamp
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let unix_secs = duration.as_secs() as i64;
        let offset = get_local_offset_seconds(unix_secs);
        let line = record.format_line(offset);
        self.write_line_internal(&line);
    }

    /// 便捷记录结构化日志。
    pub fn log(&self, level: LogLevel, topic: LogTopic, event: &str, fields: &[(&str, &str)]) {
        if !self.enabled(level, topic) {
            return;
        }
        let mut record = LogRecord::new(level, topic, event);
        for (k, v) in fields {
            record = record.field(k, *v);
        }
        self.record(&record);
    }

    /// 写入单行日志，自动执行 URL 脱敏、凭据抹除与重复行折叠。
    pub fn write_line(&self, line: &str) {
        self.write_line_internal(line);
    }

    fn write_line_internal(&self, raw_line: &str) {
        // 1. 写盘前切断 URL 查询参数与敏感凭据
        let redacted = redact_sensitive_credentials(raw_line);

        // 2. 将嵌入的换行符转义，保障单条记录在物理文件中占且仅占单行
        let sanitized = if redacted.contains('\n') || redacted.contains('\r') {
            redacted
                .replace("\r\n", "\\n")
                .replace('\n', "\\n")
                .replace('\r', "")
        } else {
            redacted
        };

        // 3. user_id 在非 DEBUG 级别不写入
        let sanitized = scrub_user_id_if_not_debug(&sanitized);

        // 4. 投递给写盘线程：重复行折叠与文件 IO 都在那边完成
        self.send_line(sanitized);
    }

    /// 记录应用启动第一行诊断记录。
    pub fn record_startup(&self) {
        let record = LogRecord::new(LogLevel::Info, LogTopic::App, "startup")
            .field("version", env!("CARGO_PKG_VERSION"))
            .field("os", std::env::consts::OS)
            .field("arch", std::env::consts::ARCH);
        self.record(&record);
        self.flush();
    }

    /// 清理所有日志文件（sodam.log 与所有轮转备份），供诊断清理功能使用。
    ///
    /// 删除失败或重建活跃文件失败都返回第一个 io error：只读目录、Windows 上被其他
    /// 程序占用时，调用方不能把「没删掉」当成成功。个别文件失败不中断其余清理。
    pub fn clear_logs(&self) -> std::io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::channel();
        {
            let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
            let Some(tx) = guard.as_ref() else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "diagnostic log writer is not running",
                ));
            };
            if tx.send(WriterMsg::Clear(ack_tx)).is_err() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "diagnostic log writer is not running",
                ));
            }
        }
        ack_rx.recv().unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "diagnostic log writer stopped before clearing",
            ))
        })
    }
}

impl Drop for LogOutlet {
    fn drop(&mut self) {
        let sender = self.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(tx) = sender {
            let _ = tx.send(WriterMsg::Shutdown);
        }
        let writer = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(handle) = writer {
            let _ = handle.join();
        }
    }
}

static GLOBAL_OUTLET: OnceLock<LogOutlet> = OnceLock::new();
static PANIC_HOOK_SET: OnceLock<()> = OnceLock::new();

fn ensure_panic_hook_registered() {
    PANIC_HOOK_SET.get_or_init(|| {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            flush_global();
            prev_hook(info);
        }));
    });
}

/// 初始化全局默认日志出口并记录启动首行记录。
pub fn init() -> &'static LogOutlet {
    init_with_dir(log_dir())
}

/// 使用指定日志目录初始化全局日志出口。
pub fn init_with_dir(dir: PathBuf) -> &'static LogOutlet {
    ensure_panic_hook_registered();
    GLOBAL_OUTLET.get_or_init(|| {
        let outlet = LogOutlet::new(dir);
        outlet.apply_env_verbosity();
        outlet.record_startup();
        outlet
    })
}

/// 获取已初始化的全局日志出口引用。
pub fn global() -> Option<&'static LogOutlet> {
    GLOBAL_OUTLET.get()
}

/// 刷新全局日志出口缓冲区。
pub fn flush_global() {
    if let Some(outlet) = global() {
        outlet.flush();
    }
}

/// 应用设置页的「详细日志」开关；全局出口尚未初始化时什么也不做。
pub fn set_global_detailed(detailed: bool) {
    if let Some(outlet) = global() {
        outlet.set_detailed(detailed);
    }
}

/// 全局记录诊断日志事件。
pub fn log_event(level: LogLevel, topic: LogTopic, event: &str, fields: &[(&str, &str)]) {
    if let Some(outlet) = global() {
        outlet.log(level, topic, event, fields);
    }
}

/// 全局判断该话题的该级别是否会写出；未初始化时一律为否。
///
/// 昂贵的字段构造（格式化、序列化）应该先问这里，避免关着还付构造成本。
pub fn enabled(level: LogLevel, topic: LogTopic) -> bool {
    global().is_some_and(|outlet| outlet.enabled(level, topic))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_path_resolution() {
        let base_win = Path::new("test_local_app_data");
        let win = resolve_platform_log_dir(Platform::Windows, Some(base_win), None, None, None);
        assert_eq!(win, base_win.join("sodam").join("logs"));

        let base_mac = Path::new("/Users/test/Library/Application Support");
        let mac = resolve_platform_log_dir(Platform::MacOS, None, None, Some(base_mac), None);
        assert_eq!(mac, base_mac.join("sodam").join("logs"));

        let base_state = Path::new("/home/test/.local/state");
        let linux_state =
            resolve_platform_log_dir(Platform::Linux, None, Some(base_state), None, None);
        assert_eq!(linux_state, base_state.join("sodam").join("logs"));

        let base_home = Path::new("/home/test");
        let linux_fallback =
            resolve_platform_log_dir(Platform::Linux, None, None, None, Some(base_home));
        assert_eq!(
            linux_fallback,
            base_home
                .join(".local")
                .join("state")
                .join("sodam")
                .join("logs")
        );
    }

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard<'a>(&'a str, Option<std::ffi::OsString>);
    impl<'a> Drop for EnvGuard<'a> {
        fn drop(&mut self) {
            if let Some(val) = &self.1 {
                std::env::set_var(self.0, val);
            } else {
                std::env::remove_var(self.0);
            }
        }
    }

    #[test]
    fn test_env_log_dir_override() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let orig = std::env::var_os("SODAM_LOG_DIR");
        let _guard = EnvGuard("SODAM_LOG_DIR", orig);

        let custom_dir = std::env::temp_dir().join("sodam_test_custom_logs");
        std::env::set_var("SODAM_LOG_DIR", &custom_dir);
        assert_eq!(log_dir(), custom_dir);
    }

    #[test]
    fn test_civil_from_days_and_timestamp() {
        // 1970-01-01 00:00:00 UTC
        let t0 = SystemTime::UNIX_EPOCH;
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(
            format_timestamp_with_offset(t0, 0),
            "1970-01-01T00:00:00.000+00:00"
        );

        // 2026-10-25 00:00:00 UTC (1792886400 / 86400 = 20751 days)
        let t_2026 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1792886400);
        assert_eq!(civil_from_days(1792886400 / 86400), (2026, 10, 25));
        assert_eq!(
            format_timestamp_with_offset(t_2026, 0),
            "2026-10-25T00:00:00.000+00:00"
        );
    }

    #[test]
    fn test_log_rotation_and_retention() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");

        // 阈值设为 40 字节，保留 3 份旧文件
        let outlet = LogOutlet::new_with_limits(log_dir.clone(), 40, 3);
        assert!(!outlet.is_degraded());

        // 写入 6 条不同记录
        for i in 1..=6 {
            outlet.write_line(&format!("line-{i:02}: 0123456789abcdef"));
        }
        outlet.flush();

        let active = log_dir.join(LOG_FILE_NAME);
        assert!(active.exists(), "活跃文件 sodam.log 必须存在");

        let b1 = log_dir.join(format!("{LOG_FILE_NAME}.1"));
        let b2 = log_dir.join(format!("{LOG_FILE_NAME}.2"));
        let b3 = log_dir.join(format!("{LOG_FILE_NAME}.3"));
        let b4 = log_dir.join(format!("{LOG_FILE_NAME}.4"));

        assert!(b1.exists(), "旧文件 1 必须存在");
        assert!(b2.exists(), "旧文件 2 必须存在");
        assert!(b3.exists(), "旧文件 3 必须存在");
        assert!(!b4.exists(), "旧文件数量不能超过保留份数 3");
    }

    #[test]
    fn test_startup_record_written() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");

        let outlet = LogOutlet::new(log_dir.clone());
        outlet.record_startup();

        let log_file = log_dir.join(LOG_FILE_NAME);
        assert!(log_file.exists());
        let content = fs::read_to_string(&log_file).unwrap();
        assert!(
            content.contains("INFO app startup version="),
            "启动记录必须为固定字段格式：{content}"
        );
        assert!(content.contains("os="));
        assert!(content.contains("arch="));
    }

    #[test]
    fn test_unwritable_directory_memory_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let block_file = temp.path().join("blocked_path");
        fs::write(&block_file, b"cannot be a directory").unwrap();
        let impossible_dir = block_file.join("sub_logs");

        let outlet = LogOutlet::new(impossible_dir);
        assert!(outlet.is_degraded(), "不可写目录应降级到内存模式");

        outlet.write_line("fallback entry 1");
        outlet.write_line("fallback entry 2");

        let records = outlet.memory_records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], "fallback entry 1");
        assert_eq!(records[1], "fallback entry 2");
    }

    #[test]
    fn test_clear_logs() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");

        let outlet = LogOutlet::new_with_limits(log_dir.clone(), 30, 2);
        outlet.write_line("entry-1: 0123456789abcdef");
        outlet.write_line("entry-2: 0123456789abcdef");
        outlet.write_line("entry-3: 0123456789abcdef");
        outlet.flush();

        assert!(log_dir.join(format!("{LOG_FILE_NAME}.1")).exists());

        outlet.clear_logs().unwrap();
        assert!(!log_dir.join(format!("{LOG_FILE_NAME}.1")).exists());
        let active_content = fs::read_to_string(log_dir.join(LOG_FILE_NAME)).unwrap();
        assert!(active_content.is_empty());
    }

    #[test]
    fn clear_logs_leaves_cache_and_unrelated_files_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");
        let cache_dir = temp.path().join("cache");
        fs::create_dir_all(cache_dir.join("audio")).unwrap();
        fs::create_dir_all(cache_dir.join("covers")).unwrap();
        let audio = cache_dir.join("audio").join("song.m4a");
        let cover = cache_dir.join("covers").join("cover.img");
        fs::write(&audio, b"audio").unwrap();
        fs::write(&cover, b"cover").unwrap();

        let outlet = LogOutlet::new(log_dir.clone());
        outlet.write_line("diagnostic entry");
        let unrelated = log_dir.join("notes.txt");
        fs::write(&unrelated, b"keep me").unwrap();

        outlet.clear_logs().unwrap();

        assert!(log_dir.join(LOG_FILE_NAME).exists());
        assert!(unrelated.exists(), "非日志文件不能被当成诊断日志删掉");
        assert!(audio.exists(), "音频缓存不受清除日志影响");
        assert!(cover.exists(), "封面缓存不受清除日志影响");
    }

    #[cfg(windows)]
    fn lock_deletion(path: &Path) -> File {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
            .unwrap()
    }

    #[test]
    fn clear_logs_reports_error_when_log_file_cannot_be_removed() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");
        let outlet = LogOutlet::new(log_dir.clone());
        outlet.write_line("entry-1");
        let active = log_dir.join(LOG_FILE_NAME);
        assert!(active.exists());

        // Unix 用只读目录挡住 unlink；Windows 用不共享 DELETE 的句柄模拟文件被占用。
        #[cfg(unix)]
        {
            fs::set_permissions(&log_dir, fs::Permissions::from_mode(0o555)).unwrap();
            if fs::write(log_dir.join("probe"), b"x").is_ok() {
                // root 不受权限位限制，构造不出删除失败。
                fs::remove_file(log_dir.join("probe")).ok();
                fs::set_permissions(&log_dir, fs::Permissions::from_mode(0o755)).unwrap();
                return;
            }
        }
        #[cfg(windows)]
        let _lock = lock_deletion(&active);

        let result = outlet.clear_logs();

        #[cfg(unix)]
        fs::set_permissions(&log_dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err(), "删除失败必须返回 Err，不能报成功");
        assert!(active.exists(), "删除失败时原始日志必须还在盘上");
    }

    #[test]
    fn test_write_line_redacts_url_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());
        outlet.write_line(
            "下载失败 https://api.example.com/audio?id=1&token=SECRET Cookie=session=abc",
        );
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        assert!(content.contains("https://api.example.com/audio?…"));
        assert!(!content.contains("token=SECRET"));
        assert!(!content.contains("Cookie="));
        assert!(!content.contains("session=abc"));
    }

    #[test]
    fn test_write_line_escapes_embedded_newlines() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());
        outlet.write_line("first half\nsecond half\r\nthird");
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1, "嵌入换行必须转义在单行内");
        assert!(lines[0].contains(r"first half\nsecond half\nthird"));
    }

    #[test]
    fn test_memory_degradation_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let block_file = temp.path().join("block");
        fs::write(&block_file, b"file").unwrap();
        let blocked_dir = block_file.join("sub");

        let outlet = LogOutlet::new(blocked_dir.clone());
        assert!(outlet.is_degraded());
        outlet.write_line("buffered 1");
        outlet.write_line("buffered 2");
        assert_eq!(outlet.memory_records().len(), 2);

        // 移除阻断文件，使目录恢复可写
        fs::remove_file(&block_file).unwrap();
        outlet.write_line("buffered 3");
        outlet.flush();

        assert!(!outlet.is_degraded(), "写入恢复后应自愈退出内存降级");
        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        assert!(content.contains("buffered 1"));
        assert!(content.contains("buffered 2"));
        assert!(content.contains("buffered 3"));
    }

    // ==========================================
    // Issue #58 专项验收测试
    // ==========================================

    #[test]
    fn test_log_levels_and_filter() {
        assert_eq!(LogLevel::Error.as_str(), "ERROR");
        assert_eq!(LogLevel::Warn.as_str(), "WARN");
        assert_eq!(LogLevel::Info.as_str(), "INFO");
        assert_eq!(LogLevel::Debug.as_str(), "DEBUG");

        assert_eq!("ERROR".parse::<LogLevel>().unwrap(), LogLevel::Error);
        assert_eq!("warn".parse::<LogLevel>().unwrap(), LogLevel::Warn);
        assert_eq!("INFO".parse::<LogLevel>().unwrap(), LogLevel::Info);
        assert_eq!("debug".parse::<LogLevel>().unwrap(), LogLevel::Debug);
        assert!("invalid".parse::<LogLevel>().is_err());

        // 级别顺序：Debug < Info < Warn < Error
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);

        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());
        outlet.set_level(LogLevel::Warn);

        outlet.log(LogLevel::Debug, LogTopic::App, "debug_evt", &[]);
        outlet.log(LogLevel::Info, LogTopic::App, "info_evt", &[]);
        outlet.log(LogLevel::Warn, LogTopic::App, "warn_evt", &[]);
        outlet.log(LogLevel::Error, LogTopic::App, "error_evt", &[]);
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        assert!(!content.contains("debug_evt"));
        assert!(!content.contains("info_evt"));
        assert!(content.contains("WARN app warn_evt"));
        assert!(content.contains("ERROR app error_evt"));
    }

    #[test]
    fn test_six_topics_and_no_unregistered_topic() {
        assert_eq!(LogTopic::ALL.len(), 6);
        let expected = ["player", "prefetch", "audio", "report", "media", "app"];
        for topic in &LogTopic::ALL {
            assert!(
                expected.contains(&topic.as_str()),
                "话题名不在首版六个话题定义中：{}",
                topic.as_str()
            );
            assert_eq!(*topic, topic.as_str().parse::<LogTopic>().unwrap());
        }

        // 检验第七个未登记话题解析必然失败
        assert!("network".parse::<LogTopic>().is_err());
        assert!("ui".parse::<LogTopic>().is_err());
        assert!("custom_topic".parse::<LogTopic>().is_err());

        for (index, topic) in LogTopic::ALL.iter().enumerate() {
            assert_eq!(topic.index(), index, "话题下标必须与 ALL 顺序一致：{topic}");
        }
    }

    #[test]
    fn test_topic_override_only_raises_verbosity() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());

        // 默认全局 INFO：DEBUG 不记。
        assert!(!outlet.enabled(LogLevel::Debug, LogTopic::Player));

        // 话题覆盖把 player 提到 DEBUG，其他话题不受影响。
        outlet.set_topic_level(LogTopic::Player, LogLevel::Debug);
        assert_eq!(outlet.topic_level(LogTopic::Player), LogLevel::Debug);
        assert!(outlet.enabled(LogLevel::Debug, LogTopic::Player));
        assert!(!outlet.enabled(LogLevel::Debug, LogTopic::App));

        // 覆盖只能更细，不能把话题调得比全局更安静。
        outlet.set_topic_level(LogTopic::Audio, LogLevel::Error);
        assert_eq!(outlet.topic_level(LogTopic::Audio), LogLevel::Info);

        outlet.log(LogLevel::Debug, LogTopic::Player, "player_debug", &[]);
        outlet.log(LogLevel::Debug, LogTopic::App, "app_debug", &[]);
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        assert!(content.contains("DEBUG player player_debug"));
        assert!(!content.contains("app_debug"));
    }

    #[test]
    fn test_env_verbosity_maps_flags_to_topics() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 先把所有相关变量清空并记下原值，避免本机环境影响断言。
        let mut guards = Vec::new();
        for (name, _) in TOPIC_VERBOSITY_ENV {
            let orig = std::env::var_os(name);
            std::env::remove_var(name);
            guards.push(EnvGuard(name, orig));
        }
        std::env::set_var("SODAM_PLAYER_LOG", "1");
        std::env::set_var("SODAM_FRAME_LOG", "1");
        std::env::set_var("SODAM_PREFETCH_LOG", "0");

        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());
        outlet.apply_env_verbosity();

        assert_eq!(outlet.topic_level(LogTopic::Player), LogLevel::Debug);
        // frame 属于 app 话题：设了它，app 的 DEBUG 行都会记。
        assert_eq!(outlet.topic_level(LogTopic::App), LogLevel::Debug);
        // `0` 等于没设。
        assert_eq!(outlet.topic_level(LogTopic::Prefetch), LogLevel::Info);
        assert!(!outlet.enabled(LogLevel::Debug, LogTopic::Prefetch));
    }

    #[test]
    fn test_detailed_switch_raises_all_topics_and_unions_with_env() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut guards = Vec::new();
        for (name, _) in TOPIC_VERBOSITY_ENV {
            let orig = std::env::var_os(name);
            std::env::remove_var(name);
            guards.push(EnvGuard(name, orig));
        }

        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());

        // 关：六个话题都是 INFO
        for topic in LogTopic::ALL {
            assert_eq!(outlet.topic_level(topic), LogLevel::Info);
        }

        // 开：六个话题全部 DEBUG，即时生效
        outlet.set_detailed(true);
        for topic in LogTopic::ALL {
            assert_eq!(outlet.topic_level(topic), LogLevel::Debug);
            assert!(outlet.enabled(LogLevel::Debug, topic));
        }

        // 环境变量在开关关闭时仍然生效；开与关是并集，不是互相覆盖
        outlet.set_detailed(false);
        std::env::set_var("SODAM_PLAYER_LOG", "1");
        outlet.apply_env_verbosity();
        outlet.set_detailed(true);
        outlet.set_detailed(false);
        assert_eq!(
            outlet.topic_level(LogTopic::Player),
            LogLevel::Debug,
            "关掉开关不能撤销环境变量提出的详细度"
        );

        outlet.log(LogLevel::Debug, LogTopic::App, "while_off", &[]);
        outlet.set_detailed(true);
        outlet.log(LogLevel::Debug, LogTopic::App, "while_on", &[]);
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        assert!(!content.contains("while_off"), "开关关闭时 DEBUG 不落盘");
        assert!(content.contains("DEBUG app while_on"));
    }

    #[test]
    fn test_log_line_format_and_field_order() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1792886400123);
        let record = LogRecord::new(LogLevel::Info, LogTopic::Player, "start")
            .with_time(t)
            .field("track_id", "12345")
            .field("quality", "lossless")
            .field("action", "play");

        // +08:00 (东八区：28800 秒)
        let line = record.format_line(28800);
        assert_eq!(
            line,
            "2026-10-25T08:00:00.123+08:00 INFO player start track_id=12345 quality=lossless action=play"
        );

        let parts: Vec<&str> = line.split(' ').collect();
        assert_eq!(parts[0], "2026-10-25T08:00:00.123+08:00");
        assert_eq!(parts[1], "INFO");
        assert_eq!(parts[2], "player");
        assert_eq!(parts[3], "start");
        assert_eq!(parts[4], "track_id=12345");
        assert_eq!(parts[5], "quality=lossless");
        assert_eq!(parts[6], "action=play");
    }

    #[test]
    fn test_url_secrets_redacted_at_first_question_mark() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());

        outlet.log(
            LogLevel::Error,
            LogTopic::Player,
            "transfer_failed",
            &[(
                "url",
                "https://api.qishui.com/stream/v1?token=SECRET_TOKEN&expire=99999",
            )],
        );
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        assert!(content.contains("https://api.qishui.com/stream/v1?…"));
        assert!(!content.contains("SECRET_TOKEN"));
        assert!(!content.contains("expire=99999"));
    }

    #[test]
    fn test_credentials_not_in_any_line() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());

        // 尝试通过字段或原始内容注入敏感头与凭据
        let record = LogRecord::new(LogLevel::Error, LogTopic::Audio, "auth_error")
            .field("Cookie", "session_token=abcdef12345")
            .field("Authorization", "Bearer my_secret_token")
            .field("x-helios", "helios_secret_xyz")
            .field("x-medusa", "medusa_secret_abc")
            .field("signer_token", "remote_signer_token_val")
            .field("error", "handshake failed");
        outlet.record(&record);

        outlet.write_line(
            "raw request with Cookie: auth=123; sec=999; other=888 and Authorization: Bearer jwt_secret_value x-helios: h789 x-medusa: m012 token: tok345",
        );
        outlet.write_line(r#"raw token="quoted secret value" signer_token=sig999"#);
        outlet.write_line("request with user_token=SECRET123 and x-token: SECRET456");
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        // 绝不出现 Cookie / Authorization / x-helios / x-medusa / token 及其密钥值
        assert!(!content.contains("Cookie"));
        assert!(!content.contains("Authorization"));
        assert!(!content.contains("x-helios"));
        assert!(!content.contains("x-medusa"));
        assert!(!content.contains("session_token"));
        assert!(!content.contains("my_secret_token"));
        assert!(!content.contains("jwt_secret_value"));
        assert!(!content.contains("sec=999"));
        assert!(!content.contains("other=888"));
        assert!(!content.contains("quoted secret value"));
        assert!(!content.contains("helios_secret"));
        assert!(!content.contains("medusa_secret"));
        assert!(!content.contains("remote_signer_token"));
        assert!(!content.contains("SECRET123"));
        assert!(!content.contains("SECRET456"));
    }

    #[test]
    fn test_author_and_other_legitimate_fields_not_redacted() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());

        let record = LogRecord::new(LogLevel::Info, LogTopic::Player, "track_loaded")
            .field("author", "Jay Chou")
            .field("token_count", "42")
            .field("title", "Mojito");
        outlet.record(&record);

        // 验证英文句子中的散文单词 token 与 cookie 不会被误脱敏
        outlet.write_line(
            r#"2026-10-25T12:00:00.000+08:00 INFO app track event="token refresh failed""#,
        );
        outlet.write_line("2026-10-25T12:00:01.000+08:00 WARN player cookie dismissed");
        outlet.write_line(r#"2026-10-25T12:00:02.000+08:00 DEBUG player status reason="token expired while refreshing""#);
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        assert!(
            content.contains(r#"author="Jay Chou""#),
            "author 字段绝不能被误杀"
        );
        assert!(
            content.contains("token_count=42"),
            "token_count 字段绝不能被误杀"
        );
        assert!(content.contains("title=Mojito"));
        assert!(
            content.contains(r#"event="token refresh failed""#),
            "散文中的 token 词汇不带冒号/等号绝不能被误杀"
        );
        assert!(
            content.contains("cookie dismissed"),
            "散文中的 cookie 词汇不带冒号/等号绝不能被误杀"
        );
        assert!(
            content.contains(r#"reason="token expired while refreshing""#),
            "带空格的英文说明绝不能被误杀"
        );
    }

    #[test]
    fn test_user_id_omitted_in_info_and_included_in_debug() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());
        outlet.set_level(LogLevel::Debug);

        // 1. INFO 级别：user_id 必须被剔除（即使内容中包含 "DEBUG" 文本）
        let info_record = LogRecord::new(LogLevel::Info, LogTopic::Player, "start")
            .field("query", "DEBUG mode")
            .field("user_id", "user_10001")
            .field("track_id", "song_999");
        outlet.record(&info_record);

        // 2. DEBUG 级别：user_id 允许输出
        let debug_record = LogRecord::new(LogLevel::Debug, LogTopic::Player, "debug_trace")
            .field("user_id", "user_10001")
            .field("track_id", "song_999");
        outlet.record(&debug_record);

        // 3. user_id 带空格且引号包裹时必须被整段剔除，不能遗漏后半截
        outlet.set_level(LogLevel::Info);
        outlet.write_line(
            r#"2026-10-25T12:00:00.000+08:00 INFO player play user_id="abc 123" track=7"#,
        );

        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);

        // 第一行 INFO：无 user_id
        assert!(lines[0].contains("INFO player start"));
        assert!(!lines[0].contains("user_10001"));
        assert!(lines[0].contains(r#"query="DEBUG mode""#));
        assert!(lines[0].contains("track_id=song_999"));

        // 第二行 DEBUG：有 user_id
        assert!(lines[1].contains("DEBUG player debug_trace"));
        assert!(lines[1].contains("user_id=user_10001"));
        assert!(lines[1].contains("track_id=song_999"));

        // 第三行 INFO：带空格引号 user_id="abc 123" 整段剔除，不漏 123"
        assert!(!lines[2].contains("user_id"));
        assert!(!lines[2].contains("123\""));
        assert!(!lines[2].contains("abc"));
        assert!(lines[2].contains("INFO player play track=7"));
    }

    #[test]
    fn test_pending_flush_preserves_last_error_on_exit() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());
        outlet.log(
            LogLevel::Error,
            LogTopic::Player,
            "playback_failed",
            &[("phase", "stream"), ("err", "timeout")],
        );
        // 未写入下一条前直接 flush（模拟退出钩子）
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        assert!(content.contains("ERROR player playback_failed phase=stream err=timeout"));
    }

    #[test]
    fn test_value_escaping_with_spaces_and_newlines() {
        assert_eq!(escape_field_value("simple"), "simple");
        assert_eq!(escape_field_value("0.3.5"), "0.3.5");
        assert_eq!(escape_field_value(""), "\"\"");
        assert_eq!(escape_field_value("hello world"), "\"hello world\"");
        assert_eq!(
            escape_field_value("line1\nline2\r\nline3"),
            "\"line1\\nline2\\r\\nline3\""
        );
        assert_eq!(
            escape_field_value("with \"quotes\""),
            "\"with \\\"quotes\\\"\""
        );

        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());
        outlet.log(
            LogLevel::Error,
            LogTopic::Player,
            "failure",
            &[
                ("reason", "connection timed out"),
                ("detail", "first\nsecond"),
            ],
        );
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1, "即使值中包含换行，落盘也必须为单行物理行");
        assert!(lines[0].contains(r#"reason="connection timed out""#));
        assert!(lines[0].contains(r#"detail="first\nsecond""#));
    }

    #[test]
    fn test_repeated_lines_folded_with_count() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());

        // 连续写入 100 条相同的错误
        for _ in 0..100 {
            outlet.log(
                LogLevel::Error,
                LogTopic::Audio,
                "engine_stall",
                &[("device", "default")],
            );
        }

        // 写入一条不同的事件
        outlet.log(
            LogLevel::Info,
            LogTopic::Audio,
            "engine_rebuilt",
            &[("device", "default")],
        );
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "100 条重复日志必须折叠为 1 行，总计 2 行");

        assert!(
            lines[0].contains("ERROR audio engine_stall device=default repeat=100"),
            "折叠行必须带有 repeat=100 计数：{}",
            lines[0]
        );
        assert!(lines[1].contains("INFO audio engine_rebuilt device=default"));
        assert!(!lines[1].contains("repeat="));
    }

    #[test]
    fn test_timestamp_format_with_local_offset() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1792886400456);

        // 东八区 (+08:00, 28800s)
        let ts_cn = format_timestamp_with_offset(t, 28800);
        assert_eq!(ts_cn, "2026-10-25T08:00:00.456+08:00");

        // 西五区 (-05:00, -18000s)
        let ts_us = format_timestamp_with_offset(t, -18000);
        assert_eq!(ts_us, "2026-10-24T19:00:00.456-05:00");

        // 零时区 (+00:00)
        let ts_utc = format_timestamp_with_offset(t, 0);
        assert_eq!(ts_utc, "2026-10-25T00:00:00.456+00:00");

        // 半小时时区 (+05:30 印度，19800s)
        let ts_in = format_timestamp_with_offset(t, 19800);
        assert_eq!(ts_in, "2026-10-25T05:30:00.456+05:30");
    }

    #[test]
    fn test_set_level_cannot_disable_errors() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = LogOutlet::new(temp.path().to_path_buf());

        // 尝试关闭所有级别，最高只能到 ERROR 级
        outlet.set_level(LogLevel::Error);
        assert_eq!(outlet.level(), LogLevel::Error);

        outlet.log(LogLevel::Info, LogTopic::App, "info_drop", &[]);
        outlet.log(LogLevel::Error, LogTopic::App, "error_keep", &[]);
        outlet.flush();

        let content = fs::read_to_string(outlet.log_file_path()).unwrap();
        assert!(!content.contains("info_drop"));
        assert!(content.contains("error_keep"));
    }

    #[test]
    fn test_prose_not_redacted() {
        let cases = [
            r#"event="token refresh failed""#,
            "WARN player cookie dismissed",
            r#"reason="token expired while refreshing""#,
            "token refresh error occurred",
            "cookie banner dismissed by user",
        ];
        for case in cases {
            assert_eq!(
                redact_sensitive_credentials(case),
                case,
                "英文散文中的 token / cookie 绝不能被误杀: {case}"
            );
        }
    }

    #[test]
    fn test_user_id_scrub_with_quotes() {
        let raw = r#"2026-10-25T12:00:00.000+08:00 INFO player play user_id="abc 123" track=7"#;
        let scrubbed = scrub_user_id_if_not_debug(raw);
        assert!(!scrubbed.contains("user_id"));
        assert!(!scrubbed.contains("123\""));
        assert!(!scrubbed.contains("abc"));
        assert_eq!(
            scrubbed,
            "2026-10-25T12:00:00.000+08:00 INFO player play track=7"
        );

        let raw_escaped = r#"2026-10-25T12:00:00.000+08:00 WARN player play user_id="user \"special\" 99" status=ok"#;
        let scrubbed_escaped = scrub_user_id_if_not_debug(raw_escaped);
        assert!(!scrubbed_escaped.contains("user_id"));
        assert!(!scrubbed_escaped.contains("special"));
        assert_eq!(
            scrubbed_escaped,
            "2026-10-25T12:00:00.000+08:00 WARN player play status=ok"
        );
    }

    #[test]
    fn test_token_suffixes_redacted_in_raw_text() {
        let cases = [
            ("user_token=SECRETVALUE", "[REDACTED]"),
            ("x-token: SECRETVALUE", "[REDACTED]"),
            ("refresh-token=SECRETVALUE", "[REDACTED]"),
            ("session_token=SECRETVALUE", "[REDACTED]"),
            ("x-helios: HELIOS_VAL", "[REDACTED]"),
            ("x-medusa: MEDUSA_VAL", "[REDACTED]"),
        ];
        for (input, expected) in cases {
            let res = redact_sensitive_credentials(input);
            assert_eq!(res, expected, "raw line 中的敏感凭据必须被脱敏: {input}");
        }
    }

    #[test]
    fn test_signer_token_redacted_without_colon() {
        let cases = [
            ("signer token abc123", "[REDACTED]"),
            ("signer token: abc123", "[REDACTED]"),
            ("signer-token abc123", "[REDACTED]"),
            ("signer-token: abc123", "[REDACTED]"),
            ("signer_token abc123", "[REDACTED]"),
            ("signer_token: abc123", "[REDACTED]"),
            ("signer_token=abc123", "[REDACTED]"),
            (r#"signer token "quoted secret""#, "[REDACTED]"),
            (r#"signer-token: "quoted secret""#, "[REDACTED]"),
        ];
        for (input, expected) in cases {
            let res = redact_sensitive_credentials(input);
            assert_eq!(
                res, expected,
                "signer token 无论有无冒号/连字符均必须被脱敏: {input}"
            );
        }
    }

    #[test]
    fn test_quit_flushes_pending_log() {
        let temp = tempfile::tempdir().unwrap();
        let outlet = init_with_dir(temp.path().to_path_buf());

        log_event(
            LogLevel::Error,
            LogTopic::Player,
            "playback_failed",
            &[("phase", "stream"), ("err", "timeout")],
        );

        let before = fs::read_to_string(outlet.log_file_path()).unwrap_or_default();
        assert!(
            !before.contains("playback_failed"),
            "[BEFORE-FLUSH] pending 日志尚未刷入磁盘"
        );

        flush_global();

        let after = fs::read_to_string(outlet.log_file_path()).unwrap_or_default();
        assert!(
            after.contains("ERROR player playback_failed phase=stream err=timeout"),
            "[AFTER-FLUSH] flush_global() 确保退出时日志完整落盘"
        );
    }
}
