//! 应用配置（`~/.config/sodam/config.json`）。

use crate::log_outlet::{log_event, LogLevel, LogTopic};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) static SETTINGS_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 最近一次「应用自己写盘」的时刻。热重载轮询靠它区分外部改动与自己保存，
/// 自己写的文件不当作外部改动提示或重载（见 #185）。
static LAST_SELF_WRITE: Mutex<Option<Instant>> = Mutex::new(None);

/// 应用自己保存后，这段窗口内的文件变化都按「自己写的」静默处理。
/// 覆盖整份保存与音量写线程的合并保存（两者都走 [`Self::save_to_locked`]）。
const SELF_WRITE_WINDOW: Duration = Duration::from_millis(1500);

/// 登录设备身份的环境变量名：启动期补全、公共合并与单测共用一份名字。
const ENV_LOGIN_DEVICE_ID: &str = "SODA_LOGIN_DEVICE_ID";
const ENV_LOGIN_INSTALL_ID: &str = "SODA_LOGIN_INSTALL_ID";

/// 登录 Cookie 的环境变量名：补全与「环境里有没有 Cookie」的判断共用。
pub(crate) const ENV_COOKIE_KEY: &str = "SODA_COOKIE";

/// 补环境变量时要不要用 `SODA_COOKIE` 补回登录身份（#192）。
///
/// 用户在界面上显式登出后，本次运行里只跳过 Cookie，其余字段照常补；
/// 启动路径不受影响，重启仍按环境变量走。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvCookieFill {
    /// 与启动时一致：空 Cookie 用环境变量补。
    Allow,
    /// 显式登出后的进程内记号生效：跳过 Cookie。
    Skip,
}

/// 进程环境里是否有非空的 `SODA_COOKIE`：登出提示用，判空口径与补全一致。
pub fn env_cookie_present() -> bool {
    std::env::var(ENV_COOKIE_KEY)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

/// 记录一次应用自身写盘（保存成功后调用）。
fn note_self_write() {
    let mut slot = LAST_SELF_WRITE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    *slot = Some(Instant::now());
}

/// 刚刚（窗口期内）是否发生过应用自身写盘。热重载轮询用来跳过自己的改动。
pub fn recent_self_write() -> bool {
    let slot = LAST_SELF_WRITE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    slot.is_some_and(|at| at.elapsed() < SELF_WRITE_WINDOW)
}

/// 配置文件加载结果：启动与热重载共用，能区分「不存在」「正常」「存在但坏了」。
///
/// 「坏了」时内存保持当前设置、文件绝不覆盖；上层据此提示并尝试备份恢复。
#[derive(Debug, Clone)]
pub enum LoadOutcome {
    /// 文件不存在（携带内置默认值）。
    NotFound(Settings),
    /// 解析成功（已做旧格式迁移、数值钳制与字段容错）。
    Loaded { settings: Settings, migrated: bool },
    /// 文件存在但读不出或解析失败。`error` 带大致出错位置，供界面提示。
    Corrupt { error: String },
}

/// 与 libresoda / libmssdk 对接需要的全部设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 签名服务地址（libmssdk 的 `/sign`，VIP 整曲必需）。
    pub signer_url: String,
    /// 签名服务 token（`Authorization: Bearer`），服务端没开鉴权时留空。
    pub signer_token: String,
    /// 登录 Cookie（扫码登录成功后写入）。
    pub cookie: String,
    /// 设备指纹：必须与签名器一致（签名器 `/config` 可查）。
    pub device_id: String,
    pub iid: String,
    pub fp: String,
    /// 扫码登录用的设备身份（passport），与上面的签名设备 `device_id/iid/fp` 是两套东西。
    ///
    /// 汽水服务端按 `device_id` 做风控；同一个身份持续使用后才补发 `odin_tt`。
    /// 首次生成后写入 config.json，退出登录不清除，只有显式重置登录设备才轮换。
    pub login_device_id: String,
    pub login_install_id: String,
    /// 音质偏好：只存**意图**——`auto` 或一个具体档位
    ///（`hires` / `lossless` / `dolby` / `highest` / `medium` / `low`）。
    ///
    /// 空串是历史写法，按 `auto` 读；新写入一律写 `auto`。实际拿到哪一档由服务端按
    /// 账号权益与曲目可用性决定，不落盘（见 [`crate::quality`]）。
    pub quality: String,
    /// 音质偏好来源：记录是用户选择（`user`）、程序推导（`derived`）还是旧配置未知（`unknown`）。
    #[serde(default)]
    pub quality_source: crate::quality::QualitySource,
    /// 当前音量与静音前音量（0..=1）。旧配置缺字段时沿用 100%。
    pub volume: f32,
    pub volume_before_mute: f32,
    /// 播放模式：列表循环 / 单曲循环 / 随机播放。旧配置缺字段时用列表循环。
    #[serde(default)]
    pub play_mode: crate::queue::PlayMode,
    /// 音频磁盘缓存上限（MiB），默认 10 GiB，设置页可调整。
    pub audio_cache_limit_mb: u64,
    /// 是否保存搜索框历史；关闭时同时清空已有记录。
    pub search_history_enabled: bool,
    /// 最近搜索词（最近在前，最多 10 条，仅保存在本机）。
    pub search_history: Vec<String>,
    /// 界面主题：`dark` / `light`；空 = 第一次启动跟随系统偏好。
    pub theme: String,
    /// 界面语言：`zh` / `en`；空或 `auto` = 跟随系统语言。
    pub language: String,
    /// Linux 下是否自绘窗口控制按钮：`auto`（默认，按桌面装饰能力）/ `always` / `never`。
    /// Windows 与 macOS 由系统负责，此项不生效。
    pub titlebar: String,
    /// 播放/暂停快捷键（gpui 的 keystroke 写法，如 `space`、`ctrl-left`）。
    /// 空串 = 用内置默认值 `DEFAULT_SHORTCUT_*`。
    pub shortcut_toggle: String,
    /// 上一首。
    pub shortcut_previous: String,
    /// 下一首。
    pub shortcut_next: String,
    /// 详细日志：打开后把六个话题都提到 DEBUG，即时生效、重启保持。
    ///
    /// 与环境变量是并集关系——两边都只能「记更多」，不存在谁覆盖谁。
    #[serde(default)]
    pub detailed_logs: bool,
}

/// 播放控制快捷键的内置默认值。
///
/// 播放/暂停三平台都用 `Space`；上一首/下一首按平台分开：
/// * macOS 用 `⌘←` / `⌘→`（Apple Music 就是这两个键）。**不能用 `⌃←`/`⌃→`**：
///   那是系统「切换桌面（Move left/right a space）」的默认快捷键，真键盘按下去
///   会被系统先吃掉，应用根本收不到（`com.apple.symbolichotkeys` 里的 key 79/81）。
/// * Windows / Linux 用 `Ctrl+←` / `Ctrl+→`（浏览器与播放器的通行做法，系统没占用）。
///
/// 播放操作绑在 `!input` 上下文上，所以在文本框里 `⌘←` 仍是「行首」，不受影响。
pub const DEFAULT_SHORTCUT_TOGGLE: &str = "space";
#[cfg(target_os = "macos")]
pub const DEFAULT_SHORTCUT_PREVIOUS: &str = "cmd-left";
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_SHORTCUT_PREVIOUS: &str = "ctrl-left";
#[cfg(target_os = "macos")]
pub const DEFAULT_SHORTCUT_NEXT: &str = "cmd-right";
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_SHORTCUT_NEXT: &str = "ctrl-right";

/// 配置为空（或只有空白）时回退到内置默认值。
fn effective_shortcut<'a>(value: &'a str, default: &'a str) -> &'a str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default
    } else {
        trimmed
    }
}

/// 把旧版本录制时写下的**显示形式**改回可解析的规范存储串。
///
/// 只认三类无歧义的本应用产物，其余一律返回 `None`（保持原样）：
/// * 符号修饰键：`^`→`ctrl`、`⌥`→`alt`、`⇧`→`shift`、`⌘`/`⊞`/`❖`→本平台主修饰键；
/// * 符号键名：`←↑→↓`→`left/right/up/down`；
/// * 被大写化的单字符键：`ctrl-A`→`ctrl-a`（旧 Display 把 `Ctrl+K` 写成大写，
///   而 parse 把大写读成「带 shift」，所以小写化是还原用户本意）。
///
/// 已经是合法 ASCII 写法的（`ctrl-left`、`cmd-left`、`secondary-a`）**一概不改**：
/// 改写用户自己写进配置文件的值，会让他找不到原来写的是什么。认不出的值留给设置页
/// 标成「未生效」，不在这里猜。
fn normalize_legacy_keystroke(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let looks_like_our_output = !value.is_ascii()
        || value.split('-').any(|part| {
            let mut chars = part.chars();
            matches!(chars.next(), Some(ch) if ch.is_ascii_uppercase()) && chars.next().is_none()
        });
    if !looks_like_our_output {
        return None;
    }

    let mut function = false;
    let mut control = false;
    let mut alt = false;
    let mut platform = false;
    let mut shift = false;
    let mut key = String::new();
    for component in value.split('-') {
        match component {
            "" => {}
            "ctrl" | "control" => control = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "fn" | "function" => function = true,
            "secondary" | "cmd" | "command" | "super" | "win" => platform = true,
            "left" | "right" | "up" | "down" => key = component.to_string(),
            other => {
                for ch in other.chars() {
                    match ch {
                        '^' => control = true,
                        '⌥' => alt = true,
                        '⇧' => shift = true,
                        '⌘' | '⊞' | '❖' => platform = true,
                        '←' => key = "left".into(),
                        '→' => key = "right".into(),
                        '↑' => key = "up".into(),
                        '↓' => key = "down".into(),
                        ch if ch.is_ascii_uppercase() => key.push(ch.to_ascii_lowercase()),
                        ch if ch.is_ascii() => key.push(ch),
                        _ => return None,
                    }
                }
            }
        }
    }
    if key.is_empty() {
        return None;
    }

    let mut parts: Vec<&str> = Vec::with_capacity(6);
    if function {
        parts.push("fn");
    }
    if control {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if platform {
        // 与 gpui `Keystroke::unparse` 的写法一致，免得同一个修饰键在配置里有两种拼法
        // （`secondary` 在非 macOS 上会被 parse 成 ctrl，绝不能拿来表示徽标键）。
        parts.push(if cfg!(target_os = "macos") {
            "cmd"
        } else if cfg!(target_os = "windows") {
            "win"
        } else {
            "super"
        });
    }
    if shift {
        parts.push("shift");
    }
    parts.push(key.as_str());
    let normalized = parts.join("-");
    (normalized != value).then_some(normalized)
}

/// 项目自建的签名服务（sodahub-org 部署）：开箱即用，可在设置里改成别的实例。
/// 2026-11 起默认走 Cloudflare Tunnel + HTTPS，源站不再直接暴露公网。
pub const DEFAULT_SIGNER_URL: &str = "https://signer.lingchuanbio.com/sign";
/// 上面那台服务的 token（等同取流能力，别外传；换服务时在设置里替换）。
pub const DEFAULT_SIGNER_TOKEN: &str = "b356ad402bc31ee9e5a99cfdbf41eaa39961a562251c3359";

/// 换服务前的那台内置实例，2026-10 起已下线。
/// 老版本的默认值会落进 config.json，所以读取时要把它们迁到当前默认值，
/// 否则老用户会一直指着一台死掉的服务。
const LEGACY_SIGNER_URL: &str = "http://222.186.10.201:8921/sign";
const LEGACY_SIGNER_TOKEN: &str = "05f8089b8c5f60c63f2a6dcfe1028d28ee2725a504f3e59b";

/// 上一版的内置实例（明文 HTTP，transition 期仍对老客户端开放）。
/// 读取到它时只把地址换成当前默认值，token 不变。
const PREVIOUS_SIGNER_URL: &str = "http://103.236.75.83:8899/sign";

/// Linux 下自绘窗口控制按钮（标题栏）的策略。
///
/// 不同桌面对 xdg-decoration 的回答差别很大：
/// * GNOME 等：协商成客户端装饰，告诉应用「你自己画」——必须自绘，否则没有按钮；
/// * Hyprland / sway 等 wlroots 平铺桌面：协商成服务端装饰，但自己同样不画标题栏，
///   这类桌面上窗口本来就由键盘/手势管理，通常不需要应用再画一套。
///
/// 所以默认 `Auto` 直接跟随协商结果，需要时可在设置里强制开或关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TitlebarMode {
    /// 按桌面协商结果决定（默认）。
    #[default]
    Auto,
    /// 始终自绘。
    Always,
    /// 始终不自绘。
    Never,
}

impl TitlebarMode {
    pub fn from_key(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "always" | "on" | "show" => Self::Always,
            "never" | "off" | "hide" => Self::Never,
            _ => Self::Auto,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    /// `client_decorated` 来自 gpui 的 `Decorations::Client`：桌面是否要求应用自绘。
    pub fn draws_titlebar(self, client_decorated: bool) -> bool {
        match self {
            Self::Auto => client_decorated,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// 解析配置文本：先按整份解析；失败时做字段级容错（类型不对的字段丢掉补默认值，
/// 其余照常生效），只有语法坏或结构对不上才报错。错误信息带大致行列位置
/// （`line X column Y`——领域层不知道界面语言，位置信息用语言无关的技术写法）。
fn parse_settings_text(text: &str) -> Result<Settings, String> {
    match serde_json::from_str::<Settings>(text) {
        Ok(settings) => Ok(settings),
        Err(first) => {
            let location = format!("line {} column {}", first.line(), first.column());
            let mut value: serde_json::Value = match serde_json::from_str(text) {
                Ok(value) => value,
                // 连 JSON 都不是（或写到一半被截断）：语法损坏，原样报出去。
                Err(_) => return Err(format!("{location}: {first}")),
            };
            let dropped = sanitize_config_value(&mut value);
            let settings = serde_json::from_value::<Settings>(value)
                .map_err(|err| format!("{location}: {err}"))?;
            if !dropped.is_empty() {
                log_event(
                    LogLevel::Warn,
                    LogTopic::App,
                    "config_fields_dropped",
                    &[("fields", dropped.join(",").as_str())],
                );
            }
            Ok(settings)
        }
    }
}

/// 解析配置字节（音量写线程的读-改-写走这里）：与文本路径共用字段级容错，
/// 单个字段坏了不连累整份合并写。
pub(crate) fn parse_settings_bytes(bytes: &[u8]) -> Result<Settings, String> {
    let text = std::str::from_utf8(bytes).map_err(|err| format!("invalid utf-8: {err}"))?;
    parse_settings_text(text)
}

/// 字段级容错：类型不符合的字段直接删掉，让 `serde(default)` 补默认值，
/// 不连累同一份文件里的其它字段（尤其 Cookie 与设备身份）。
/// 类型对但值不认识（未知播放模式、未知音质来源）由字段自己的反序列化回退默认。
fn sanitize_config_value(value: &mut serde_json::Value) -> Vec<String> {
    let Some(object) = value.as_object_mut() else {
        return Vec::new();
    };
    let mut dropped = Vec::new();
    let mut drop_unless = |key: &str, keep: &dyn Fn(&serde_json::Value) -> bool| {
        if let Some(field) = object.get(key) {
            if !keep(field) {
                object.remove(key);
                dropped.push(key.to_string());
            }
        }
    };
    let is_string = |value: &serde_json::Value| value.is_string();
    let is_bool = |value: &serde_json::Value| value.is_boolean();
    // 数字字段按目标类型校验：`-1` 是合法 JSON number 但进不了 u64，
    // 放过去会让整份配置解析失败（#185）。
    let is_f32 = |value: &serde_json::Value| serde_json::from_value::<f32>(value.clone()).is_ok();
    let is_u64 = |value: &serde_json::Value| serde_json::from_value::<u64>(value.clone()).is_ok();
    // 字符串数组：元素类型也要对得上。
    let is_string_array = |value: &serde_json::Value| {
        value
            .as_array()
            .is_some_and(|items| items.iter().all(serde_json::Value::is_string))
    };
    for key in [
        "signer_url",
        "signer_token",
        "cookie",
        "device_id",
        "iid",
        "fp",
        "login_device_id",
        "login_install_id",
        "quality",
        "theme",
        "language",
        "titlebar",
        "shortcut_toggle",
        "shortcut_previous",
        "shortcut_next",
        "play_mode",
        "quality_source",
    ] {
        drop_unless(key, &is_string);
    }
    for key in ["volume", "volume_before_mute"] {
        drop_unless(key, &is_f32);
    }
    drop_unless("audio_cache_limit_mb", &is_u64);
    for key in ["search_history_enabled", "detailed_logs"] {
        drop_unless(key, &is_bool);
    }
    drop_unless("search_history", &is_string_array);
    dropped
}

/// 最近可用备份的路径：与主文件同目录（`config.json` → `config.json.bak`）。
pub fn settings_backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

/// 把一份可用配置原子写入备份文件（与主文件同目录）。每次成功加载后调用一次，
/// 主文件损坏时靠它恢复。与保存共用同一把写锁，避免与并发保存互相踩（#185）。
pub fn write_settings_backup(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let _guard = SETTINGS_WRITE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    write_settings_backup_locked(path, settings)
}

fn write_settings_backup_locked(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let backup = settings_backup_path(path);
    let parent = backup
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?)?;
    file.persist(&backup).map_err(|err| err.error)?;
    Ok(())
}

/// 读取主文件的备份（同样做字段级容错）。备份也读不出时返回 `None`。
/// 与保存共用同一把写锁，读到一半不会被并发的隔离改名/保存打断。
pub fn load_settings_backup(path: &Path) -> Option<Settings> {
    let _guard = SETTINGS_WRITE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let text = std::fs::read_to_string(settings_backup_path(path)).ok()?;
    parse_settings_text(&text).ok()
}

/// 把坏掉的主文件改名保留（`config.json` → `config.json.bad-<毫秒时间戳>`），
/// 返回保留路径；改名失败（目录只读等）返回 `None`。
/// 与保存共用同一把写锁：并发保存可能正要覆盖这份坏文件，先到先得。
pub fn quarantine_corrupt_file(path: &Path) -> Option<PathBuf> {
    let _guard = SETTINGS_WRITE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    quarantine_corrupt_file_locked(path)
}

/// [`Self::save_to_locked`] 持锁期间用的隔离变体（锁不可重入，勿混用）。
fn quarantine_corrupt_file_locked(path: &Path) -> Option<PathBuf> {
    let bad_path = quarantine_target(path)?;
    std::fs::rename(path, &bad_path).ok()?;
    Some(bad_path)
}

/// 坏文件隔离的目标名：`config.json` → `config.json.bad-<毫秒时间戳>`。
///
/// 只算名字、不动文件：读盘阶段先定好目标，真正改名留到首帧后的后台修正（#191）。
fn quarantine_target(path: &Path) -> Option<PathBuf> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let stamp = now.as_millis();
    Some(path.with_extension(format!("json.bad-{stamp}")))
}

/// 把坏文件改名到预先算好的目标。调用方必须持有 `SETTINGS_WRITE_LOCK`（锁不可重入）。
fn quarantine_corrupt_file_to_locked(path: &Path, bad_path: &Path) -> std::io::Result<()> {
    std::fs::rename(path, bad_path)
}

/// 启动期加载的恢复结果：主文件损坏时用了哪条路，界面据此提示（#185）。
#[derive(Debug, Clone)]
pub enum StartupRecovery {
    /// 已从备份恢复；坏文件改名保留在 `bad_path`。
    RecoveredFromBackup { bad_path: PathBuf },
    /// 无备份可用：默认值启动，坏文件原样保留；`error` 是损坏详情。
    FellBackToDefaults { error: String },
}

/// 启动加载的内存结果：设置、恢复提示，以及需要在首帧后台补做的落盘修正。
pub struct StartupLoad {
    pub settings: Settings,
    pub recovery: Option<StartupRecovery>,
    /// 启动前主文件不存在（首启）：旧行为会在启动时创建配置文件。
    pub first_run: bool,
    /// 读盘前记下的文件指纹：热重载基线与后台回写的并发保护都用这一份。
    pub baseline: Option<crate::config_watch::Fingerprint>,
    /// 落盘修正（备份刷新 / 迁移回存 / 损坏隔离与恢复写回）。默认值启动时为 `None`。
    pub write_back: Option<StartupWriteBack>,
}

/// 启动期欠下的落盘修正：读盘阶段只收集，首帧后到后台线程执行（#191）。
///
/// 这几步都是有副作用的小文件操作，不能放在 UI 线程；失败只上报，不影响本次运行。
pub struct StartupWriteBack {
    path: PathBuf,
    settings: Settings,
    /// 损坏路径：坏文件改名到这个目标，再做恢复写回。
    quarantine_to: Option<PathBuf>,
    /// 迁移 / 补身份后是否需要回存主文件。
    save_main: bool,
    /// 读盘前的文件指纹：回写前对不上就说明外部改过，放弃覆盖。
    expected: Option<crate::config_watch::Fingerprint>,
}

impl StartupWriteBack {
    /// 首启时把补过环境变量的内存设置作为落盘内容：旧行为首启保存的就是合并后的这份。
    pub fn adopt_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    /// 执行落盘修正。阻塞调用，必须在后台线程跑；返回第一处失败。
    ///
    /// 主文件保存与备份刷新在同一把写锁内完成：要么一起刷新到同一份内容，要么都不动。
    /// 回写前指纹对不上（外部已改过文件）就整体跳过，交给热重载应用新内容。
    pub fn persist(self) -> Result<(), String> {
        let _guard = SETTINGS_WRITE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if crate::config_watch::fingerprint(&self.path) != self.expected {
            log_event(
                LogLevel::Warn,
                LogTopic::App,
                "startup_config_write_back_skipped",
                &[],
            );
            return Ok(());
        }
        let mut first_error: Option<String> = None;
        if let Some(bad_path) = self.quarantine_to.as_deref() {
            match quarantine_corrupt_file_to_locked(&self.path, bad_path) {
                Ok(()) => {}
                // 等待期间坏文件已被删掉：没有要保留的原件，按成功继续恢复写回。
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    first_error.get_or_insert_with(|| format!("隔离坏配置失败：{err}"));
                }
            }
        }
        if self.save_main {
            if let Err(err) = self.settings.save_to_locked(&self.path) {
                first_error.get_or_insert_with(|| format!("回存配置失败：{err}"));
            }
        }
        // `save_to_locked` 内部会吞掉备份写错误，所以这里总是单独刷新一次并报告失败，
        // 避免主文件保存成功、备份没更新却没人知道。
        if let Err(err) = write_settings_backup_locked(&self.path, &self.settings) {
            first_error.get_or_insert_with(|| format!("写配置备份失败：{err}"));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// 启动加载：只读 + 内存修正，不写盘（#191）。
///
/// 正常路径读主文件、补环境变量、生成登录设备身份并做旧格式迁移，全部只在内存里；
/// 该落盘的修正放进 [`StartupWriteBack`]，由调用方在首帧后到后台线程执行。
/// 主文件损坏时先试备份——备份可用就用备份继续、坏文件改名保留；无备份则默认值
/// 启动且绝不碰坏文件。
pub fn load_for_startup(path: &Path) -> StartupLoad {
    // 读盘前先记指纹：热重载基线和后台回写的并发保护都用这一份。
    let baseline = crate::config_watch::fingerprint(path);
    match Settings::try_load_from(path) {
        LoadOutcome::Corrupt { error } => {
            // 备份走与主文件完全相同的加载管线（try_load_from：字段容错 + 迁移 +
            // 数值钳制 + 搜索历史清理），备份里存了什么形态都不能绕过归一化。
            let backup = match Settings::try_load_from(&settings_backup_path(path)) {
                LoadOutcome::Loaded { settings, .. } => Some(settings),
                _ => None,
            };
            match backup {
                Some(backup) => {
                    // 备份里没带登录身份时，环境变量仍然要先生效（#186）；
                    // 备份自带的身份优先。
                    let (backup, _) = Settings::prepare_loaded(backup, false);
                    // 读盘阶段只算隔离目标名，不改名；坏文件等首帧后的后台修正再动。
                    let bad_path = quarantine_target(path).unwrap_or_else(|| path.to_path_buf());
                    StartupLoad {
                        write_back: Some(StartupWriteBack {
                            path: path.to_path_buf(),
                            settings: backup.clone(),
                            quarantine_to: Some(bad_path.clone()),
                            save_main: true,
                            expected: baseline,
                        }),
                        settings: backup,
                        recovery: Some(StartupRecovery::RecoveredFromBackup { bad_path }),
                        first_run: false,
                        baseline,
                    }
                }
                None => {
                    // 不生成设备身份、不回存：坏文件在磁盘上原样保留。
                    let mut defaults = Settings::default();
                    defaults.normalize_playback();
                    StartupLoad {
                        settings: defaults,
                        recovery: Some(StartupRecovery::FellBackToDefaults { error }),
                        first_run: false,
                        baseline,
                        write_back: None,
                    }
                }
            }
        }
        LoadOutcome::NotFound(settings) => startup_load_from(path, settings, false, true, baseline),
        LoadOutcome::Loaded { settings, migrated } => {
            startup_load_from(path, settings, migrated, false, baseline)
        }
    }
}

/// 正常读盘结果 → 内存设置 + 落盘修正（备份必刷；迁移 / 首启要回存主文件）。
fn startup_load_from(
    path: &Path,
    settings: Settings,
    migrated: bool,
    first_run: bool,
    baseline: Option<crate::config_watch::Fingerprint>,
) -> StartupLoad {
    let (settings, changed) = Settings::prepare_loaded(settings, migrated);
    StartupLoad {
        write_back: Some(StartupWriteBack {
            path: path.to_path_buf(),
            settings: settings.clone(),
            quarantine_to: None,
            // 首启即使身份全部来自环境变量（ensure 不生成）也要创建配置文件。
            save_main: changed || first_run,
            expected: baseline,
        }),
        settings,
        recovery: None,
        first_run,
        baseline,
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            signer_url: DEFAULT_SIGNER_URL.to_string(),
            signer_token: DEFAULT_SIGNER_TOKEN.to_string(),
            cookie: String::new(),
            device_id: String::new(),
            iid: String::new(),
            fp: String::new(),
            login_device_id: String::new(),
            login_install_id: String::new(),
            // 只存意图，不存实际档位：新写入一律 auto，空串只作历史写法来读
            //（见 crate::quality）。不再按会员等级推导一个具体档位写回。
            quality: crate::quality::AUTO.to_string(),
            quality_source: crate::quality::QualitySource::Unknown,
            volume: 1.0,
            volume_before_mute: 1.0,
            play_mode: crate::queue::PlayMode::default(),
            audio_cache_limit_mb: crate::cache::DEFAULT_AUDIO_LIMIT_MB,
            search_history_enabled: true,
            search_history: Vec::new(),
            theme: String::new(),
            language: String::new(),
            titlebar: String::new(),
            shortcut_toggle: String::new(),
            shortcut_previous: String::new(),
            shortcut_next: String::new(),
            detailed_logs: false,
        }
    }
}

impl Settings {
    pub fn config_path() -> PathBuf {
        if let Some(path) = std::env::var_os("SODAM_CONFIG_PATH").filter(|path| !path.is_empty()) {
            return path.into();
        }
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sodam")
            .join("config.json")
    }

    /// 读配置；文件不存在时返回默认值（不报错）。
    ///
    /// 同步读写的便捷入口（示例 / 工具用）。应用启动请走 [`load_for_startup`]：
    /// 那条路径只读，落盘修正留给首帧后的后台任务（#191）。
    pub fn load() -> Self {
        Self::load_from(&Self::config_path())
    }

    /// 同步加载：读 + 内存修正 + 立即回存 / 备份（阻塞调用）。
    pub fn load_from(path: &Path) -> Self {
        let (settings, changed) = match Self::try_load_from(path) {
            LoadOutcome::Corrupt { error } => {
                // 坏文件绝不覆盖：用默认值继续，不生成设备身份、不回写。
                // 备份恢复由调用方（应用启动流程）负责。
                log_event(
                    LogLevel::Warn,
                    LogTopic::App,
                    "config_corrupt",
                    &[("error", error.as_str())],
                );
                return Self::default();
            }
            LoadOutcome::NotFound(settings) => Self::prepare_loaded(settings, false),
            LoadOutcome::Loaded { settings, migrated } => Self::prepare_loaded(settings, migrated),
        };
        // 迁移/初始化后立刻落盘：用户下次打开配置文件看到的、以及设置页显示的，
        // 都是真正生效的那份。写失败不影响启动，内存里的值已经是迁移后的。
        if changed {
            let _ = settings.save_to(path);
        }
        // 每次成功加载都留一份最近可用备份（与主文件同目录、原子写）：主文件之后
        // 被改坏时靠它恢复（#185）。写失败不影响本次加载。
        let _ = write_settings_backup(path, &settings);
        settings
    }

    /// 读盘结果的内存修正：补环境变量、生成登录设备身份，返回 `(设置, 是否需要回存)`。
    ///
    /// 不写盘：启动路径把「要不要回存」交给 [`StartupWriteBack`] 在首帧后处理（#191）。
    fn prepare_loaded(mut settings: Self, changed: bool) -> (Self, bool) {
        // 环境变量补在身份生成之前：首启时 `SODA_LOGIN_DEVICE_ID` / `SODA_LOGIN_INSTALL_ID`
        // 才能决定这份身份，否则身份先生成落盘，环境变量永远补不进去（#186）。
        // 已存的身份非空，照旧优先于环境变量。
        settings.fill_login_device_from_env();
        // 老版本把登录设备身份存在 libresoda 自己的文件里；首次升级时搬进 config.json，
        // 保住已经积累的服务端设备信誉。之后二维码只使用配置里的这份身份。
        let changed = if settings.ensure_login_device() {
            // 只记「初始化过」这件事，不记 device_id/install_id 本身。
            log_event(
                LogLevel::Info,
                LogTopic::App,
                "login_device_initialized",
                &[],
            );
            true
        } else {
            changed
        };
        (settings, changed)
    }

    /// 安全读取：不写盘、无副作用，返回三态结果（不存在 / 正常 / 坏了）。
    ///
    /// - 单个字段类型不对：丢掉该字段补默认值，其余字段照常生效（尤其保住 Cookie
    ///   与设备身份）；无法识别的枚举值（播放模式等）由字段自己的反序列化回退默认。
    /// - 数值越界由 `normalize_playback` 钳制。
    /// - 只有语法层面的损坏（连 JSON 都不是、或结构对不上）才算 [`LoadOutcome::Corrupt`]。
    /// - 旧格式迁移只改内存值；是否回存由调用方决定：[`load_for_startup`] 把落盘
    ///   推迟到首帧后，[`Self::load_from`] 同步落盘；热重载路径拿到 `migrated`
    ///   也不回写，避免外部编辑被立刻覆盖。
    pub fn try_load_from(path: &Path) -> LoadOutcome {
        // 与保存共用同一把写锁：轮询读文件不会被并发保存的原子替换序列读到中间态。
        let _guard = SETTINGS_WRITE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return LoadOutcome::NotFound(Self::default());
            }
            Err(err) => {
                return LoadOutcome::Corrupt {
                    error: format!("读取失败：{err}"),
                };
            }
        };
        match parse_settings_text(&text) {
            Ok(settings) => {
                let (mut settings, migrated) = settings.migrated();
                settings.normalize_playback();
                settings.normalize_search_history();
                LoadOutcome::Loaded { settings, migrated }
            }
            Err(error) => LoadOutcome::Corrupt { error },
        }
    }

    /// 确保配置里有合法的扫码登录设备身份；返回是否发生了生成/迁移。
    ///
    /// 优先迁移 libresoda 旧文件里的身份，其次生成一份新的。生成结果由调用方
    /// 随配置落盘（`load_from` 会自动保存）。
    pub fn ensure_login_device(&mut self) -> bool {
        let current = self.login_device();
        if current.is_valid() {
            return false;
        }
        let device = libresoda::soda::qr_login::load_persisted_device()
            .unwrap_or_else(libresoda::soda::qr_login::QrDevice::generate);
        self.login_device_id = device.device_id;
        self.login_install_id = device.install_id;
        true
    }

    /// 当前扫码登录设备身份（可能无效，调用前先 [`Self::ensure_login_device`]）。
    pub fn login_device(&self) -> libresoda::soda::qr_login::QrDevice {
        libresoda::soda::qr_login::QrDevice {
            device_id: self.login_device_id.clone(),
            install_id: self.login_install_id.clone(),
        }
    }

    /// 把老配置里已经失效的写法改到当前形式：内置签名端点、快捷键的显示形式。
    ///
    /// 端点只认「等于旧默认值」这一种情况，用户填过别的地址就原样保留；
    /// token 同理，避免覆盖用户自己给别的实例配的 token。
    /// 返回是否发生了改动（改动后由 `load_from` 回存）。
    fn migrated(mut self) -> (Self, bool) {
        let mut changed = false;

        if self.signer_url.trim() == LEGACY_SIGNER_URL {
            self.signer_url = DEFAULT_SIGNER_URL.to_string();
            if self.signer_token.trim() == LEGACY_SIGNER_TOKEN {
                self.signer_token = DEFAULT_SIGNER_TOKEN.to_string();
            }
            changed = true;
        } else if self.signer_url.trim() == PREVIOUS_SIGNER_URL {
            // 上一版默认地址：只换 URL，token 与当前默认一致，其它字段原样保留。
            self.signer_url = DEFAULT_SIGNER_URL.to_string();
            changed = true;
        }

        // 旧版本把按键的**显示形式**写进了配置（`⌘←`、`^←`、`ctrl-A`），这些串 parse 不回
        // 原本那个键，绑上就是永不触发的死键（#39）。这里只改写「认得出是本应用产物」的值；
        // 认不出的保持原样，交给设置页标成未生效。
        for field in [
            &mut self.shortcut_toggle,
            &mut self.shortcut_previous,
            &mut self.shortcut_next,
        ] {
            if let Some(normalized) = normalize_legacy_keystroke(field) {
                *field = normalized;
                changed = true;
            }
        }

        (self, changed)
    }

    /// 写配置（自动建目录）。
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&Self::config_path())
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        let _guard = SETTINGS_WRITE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        Self::debug_delay_before_write();
        self.save_to_locked(path)
    }

    /// 登出落盘：把**盘上最新那份配置**摘掉 Cookie 再写回去（#197）。
    ///
    /// 读盘、比对、写入在同一把写锁里完成：
    /// - 盘上 Cookie 还是 `expected_cookie`：清掉 Cookie 写回，主文件和备份一起刷新；
    /// - 盘上本来就是空 Cookie（例如内存里的身份来自 `SODA_COOKIE`）：不用写，直接算成功；
    /// - 盘上已经是另一个身份：`Ok(false)`，文件和备份都不动，别拿旧快照盖人。
    ///
    /// `self` 只当兜底：文件读不出来或解析不了时，用它摘掉 Cookie 去写
    /// （坏原件由 `save_to_locked` 走隔离流程）。
    pub fn save_signed_out(&self, expected_cookie: &str) -> anyhow::Result<bool> {
        self.save_signed_out_to(&Self::config_path(), expected_cookie)
    }

    pub fn save_signed_out_to(&self, path: &Path, expected_cookie: &str) -> anyhow::Result<bool> {
        let _guard = SETTINGS_WRITE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        Self::debug_delay_before_write();
        let latest = std::fs::read(path)
            .ok()
            .and_then(|bytes| parse_settings_bytes(&bytes).ok());
        let outcome = latest.map(|mut current| {
            // 盘上本来就没有 Cookie（内存身份来自环境变量补全）：没有可清的东西。
            if current.cookie.trim().is_empty() {
                return Ok(true);
            }
            // 盘上已经是另一个身份：旧结果不许覆盖新身份。
            if current.cookie != expected_cookie {
                return Ok(false);
            }
            // 用盘上最新配置写回，只摘 Cookie：保存期间改过的主题、音量等不会被旧快照盖掉。
            current.cookie.clear();
            current.save_to_locked(path)?;
            Ok(true)
        });
        if let Some(result) = outcome {
            return result;
        }
        let mut fallback = self.clone();
        fallback.cookie.clear();
        fallback.save_to_locked(path)?;
        Ok(true)
    }

    /// 验收用：debug 构建里按 `SODAM_SLOW_SAVE_MS` 拖慢保存（#197）。
    fn debug_delay_before_write() {
        #[cfg(debug_assertions)]
        if let Some(delay) =
            Self::debug_slow_save_delay(std::env::var("SODAM_SLOW_SAVE_MS").ok().as_deref())
        {
            std::thread::sleep(delay);
        }
    }

    /// 调试开关：验收「慢盘 / 写锁被占时界面不冻结」时给每次保存加固定延迟（#197）。
    ///
    /// 写锁是进程内的 Mutex，外部进程占不住。真机验收用 `SODAM_SLOW_SAVE_MS=3000`
    /// 让保存持着锁睡一会儿，看界面是否还跟手。只在 debug 构建里生效。
    #[cfg(debug_assertions)]
    fn debug_slow_save_delay(raw: Option<&str>) -> Option<std::time::Duration> {
        let ms = raw?.trim().parse::<u64>().ok()?;
        if ms == 0 {
            return None;
        }
        Some(std::time::Duration::from_millis(ms))
    }

    pub(crate) fn save_to_locked(&self, path: &Path) -> anyhow::Result<()> {
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        // 目标文件存在但已是坏配置：不直接覆盖。先把坏文件改名保留（上层据此告诉
        // 用户原件在哪），改不动就取消本次保存——用户内容比这次写入重要。
        // 判定与加载共用同一套容错解析：非 UTF-8、`null`、语法坏都算坏原件。
        match std::fs::read(path) {
            Ok(bytes) => {
                if parse_settings_bytes(&bytes).is_err() {
                    match quarantine_corrupt_file_locked(path) {
                        Some(bad_path) => log_event(
                            LogLevel::Warn,
                            LogTopic::App,
                            "config_corrupt_quarantined_on_save",
                            &[("bad_path", bad_path.display().to_string().as_str())],
                        ),
                        None => {
                            anyhow::bail!("配置文件损坏且无法保留原件，已取消本次保存");
                        }
                    }
                }
            }
            // 文件不存在是首次保存的正常情况。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            // 读不出原件就无法确认它是好是坏：宁可取消本次保存，也不盲写。
            Err(err) => {
                anyhow::bail!("配置文件无法读取（{err}），已取消本次保存");
            }
        }
        // 同目录原子替换，避免退出/并发保存留下半份 JSON（Windows 也支持替换旧文件）。
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.persist(path).map_err(|err| err.error)?;
        // 备份同步刷新到最新一份可用配置：备份只在加载时写的话，运行期间的
        // 保存（换 Cookie、改设置）之后主文件损坏，恢复会退回加载时的旧值（#185）。
        // 已在写锁内，走 locked 变体，不重复加锁。
        let _ = write_settings_backup_locked(path, self);
        // 热重载轮询要靠这个区分「自己写的」和「外部改的」。
        note_self_write();
        Ok(())
    }

    pub fn normalize_playback(&mut self) {
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.volume_before_mute =
            if self.volume_before_mute.is_finite() && self.volume_before_mute > 0.0 {
                self.volume_before_mute.min(1.0)
            } else {
                1.0
            };
        self.audio_cache_limit_mb = self.audio_cache_limit_mb.clamp(1024, 1024 * 1024);
    }

    /// 搜索历史最多保留 10 条；去空白、去重，最近搜索保持在前。
    pub const SEARCH_HISTORY_LIMIT: usize = 10;

    pub fn search_history_is_enabled(&self) -> bool {
        self.search_history_enabled
    }

    pub fn record_search(&mut self, query: &str) -> bool {
        if !self.search_history_enabled {
            return false;
        }
        let query = query.trim();
        if query.is_empty() {
            return false;
        }
        let before = self.search_history.clone();
        self.search_history.retain(|item| item != query);
        self.search_history.insert(0, query.to_string());
        self.search_history.truncate(Self::SEARCH_HISTORY_LIMIT);
        self.search_history != before
    }

    pub fn clear_search_history(&mut self) {
        self.search_history.clear();
    }

    pub fn remove_search_history(&mut self, index: usize) -> bool {
        if index >= self.search_history.len() {
            return false;
        }
        self.search_history.remove(index);
        true
    }

    pub fn set_search_history_enabled(&mut self, enabled: bool) -> bool {
        if self.search_history_enabled == enabled {
            return false;
        }
        self.search_history_enabled = enabled;
        if !enabled {
            self.search_history.clear();
        }
        true
    }

    fn normalize_search_history(&mut self) {
        if !self.search_history_enabled {
            self.search_history.clear();
            return;
        }
        let mut seen = std::collections::HashSet::new();
        self.search_history = self
            .search_history
            .drain(..)
            .filter_map(|item| {
                let item = item.trim().to_string();
                if item.is_empty() || !seen.insert(item.clone()) {
                    None
                } else {
                    Some(item)
                }
            })
            .take(Self::SEARCH_HISTORY_LIMIT)
            .collect();
    }

    pub fn set_volume(&mut self, volume: f32) {
        if !volume.is_finite() {
            return;
        }
        self.volume = volume.clamp(0.0, 1.0);
        if self.volume > 0.0 {
            self.volume_before_mute = self.volume;
        }
    }

    pub fn toggle_mute(&mut self) {
        if self.volume > 0.0 {
            self.volume_before_mute = self.volume;
            self.volume = 0.0;
        } else {
            self.set_volume(self.volume_before_mute);
        }
    }

    /// 是否已经具备"能放 VIP 整曲"的条件（cookie + 签名服务）。
    pub fn is_ready_for_vip(&self) -> bool {
        !self.cookie.trim().is_empty() && !self.signer_url.trim().is_empty()
    }

    /// 音质意图（空串按「自动」读）。
    pub fn quality_intent(&self) -> crate::quality::QualityIntent {
        crate::quality::QualityIntent::parse(&self.quality)
    }

    /// 音质偏好来源。
    pub fn quality_source(&self) -> crate::quality::QualitySource {
        self.quality_source
    }

    /// 用户在界面上手动写入音质意图，显式标记来源为 `QualitySource::User`。
    pub fn set_quality_intent_by_user(&mut self, intent: &crate::quality::QualityIntent) {
        self.set_quality_intent_with_source(intent, crate::quality::QualitySource::User);
    }

    /// 写入音质意图并显式记录其来源（用户手动选择或程序推导）。
    pub fn set_quality_intent_with_source(
        &mut self,
        intent: &crate::quality::QualityIntent,
        source: crate::quality::QualitySource,
    ) {
        self.quality = intent.as_setting().to_string();
        self.quality_source = source;
    }

    /// 生效的播放/暂停快捷键（配置为空时用内置默认值）。
    pub fn shortcut_toggle(&self) -> &str {
        effective_shortcut(&self.shortcut_toggle, DEFAULT_SHORTCUT_TOGGLE)
    }

    /// 生效的上一首快捷键。
    pub fn shortcut_previous(&self) -> &str {
        effective_shortcut(&self.shortcut_previous, DEFAULT_SHORTCUT_PREVIOUS)
    }

    /// 生效的下一首快捷键。
    pub fn shortcut_next(&self) -> &str {
        effective_shortcut(&self.shortcut_next, DEFAULT_SHORTCUT_NEXT)
    }

    /// 用环境变量补全空字段（`SODA_COOKIE` / `QISHUI_SIGNER_URL` / `QISHUI_SIGNER_TOKEN`…），
    /// 便于先用 shell 跑起来，再逐步落到配置文件。
    ///
    /// `SODAM_QUALITY` 是唯一例外：音质的「自动」现在写成 `auto` 而不是空串，按「补空字段」
    /// 的规则它会永远填不进去。所以它按**意图**与**来源**判断——只有当前未被用户显式
    /// 选择且意图是自动时才让环境变量覆盖（标记为 `Derived`），用户手选的具体档位或明确选的「自动」不会被开发变量悄悄改掉。
    ///
    /// 补完环境变量后再生成缺失的登录设备身份（见 [`Self::ensure_login_device`]）。
    /// 热重载路径请用 [`Self::fill_empty_from_env`]——轮询里不能现生成设备身份，
    /// 否则外部文件没带身份时会在后台悄悄轮换一份。
    pub fn merged_with_env(self) -> Self {
        let mut settings = self.fill_empty_from_env(EnvCookieFill::Allow);
        settings.ensure_login_device();
        settings
    }

    /// 只补环境变量，不生成设备身份：热重载轮询与启动共用同一套补全逻辑
    /// （优先级一致），设备身份交给调用方决定要不要生成。
    ///
    /// [`EnvCookieFill::Skip`] 时跳过 `SODA_COOKIE`，其余字段照常补：
    /// 用户在界面上显式登出后，本次运行里环境变量不能再把身份补回来（#192）。
    /// 重启后仍按环境变量启动，启动期契约不变。
    pub(crate) fn fill_empty_from_env(mut self, cookie_fill: EnvCookieFill) -> Self {
        self.fill_login_device_from_env();
        if cookie_fill == EnvCookieFill::Allow {
            Self::fill_slot_from_env(&mut self.cookie, ENV_COOKIE_KEY);
        }
        let pairs = [
            ("QISHUI_SIGNER_URL", &mut self.signer_url),
            ("QISHUI_SIGNER_TOKEN", &mut self.signer_token),
            ("SODA_DEVICE_ID", &mut self.device_id),
            ("SODA_IID", &mut self.iid),
            ("SODA_FP", &mut self.fp),
        ];
        for (key, slot) in pairs {
            Self::fill_slot_from_env(slot, key);
        }
        if self.quality_source != crate::quality::QualitySource::User
            && self.quality_intent() == crate::quality::QualityIntent::Auto
        {
            if let Ok(value) = std::env::var("SODAM_QUALITY") {
                if !value.trim().is_empty() {
                    self.quality = value.trim().to_string();
                    self.quality_source = crate::quality::QualitySource::Derived;
                }
            }
        }
        self
    }

    /// 字段为空时用同名环境变量补上；文件里已有的值优先。
    fn fill_slot_from_env(slot: &mut String, key: &str) {
        if slot.trim().is_empty() {
            if let Ok(value) = std::env::var(key) {
                if !value.trim().is_empty() {
                    *slot = value.trim().to_string();
                }
            }
        }
    }

    /// 启动期只补登录设备身份的两个环境变量，且必须跑在
    /// [`Self::ensure_login_device`] 之前：首启时它们要能决定生成什么身份，
    /// 否则身份先生成并落盘，环境变量永远补不进去（#186）。
    fn fill_login_device_from_env(&mut self) {
        Self::fill_slot_from_env(&mut self.login_device_id, ENV_LOGIN_DEVICE_ID);
        Self::fill_slot_from_env(&mut self.login_install_id, ENV_LOGIN_INSTALL_ID);
    }

    /// 会话输入字段是否与另一份设置不同：签名地址/Token、Cookie、签名设备身份
    /// （device_id/iid/fp）、登录设备身份、音质意图。这些字段变了需要重建会话
    /// 才生效（`Session::adopt` / `Session::apply`），其余字段即时读取。
    ///
    /// 「哪些字段算会话输入」是配置的领域知识，收在这里，UI 层不做字段清单。
    pub fn session_inputs_differ(&self, other: &Settings) -> bool {
        self.signer_url != other.signer_url
            || self.signer_token != other.signer_token
            || self.cookie != other.cookie
            || self.device_id != other.device_id
            || self.iid != other.iid
            || self.fp != other.fp
            || self.quality != other.quality
            || self.quality_source != other.quality_source
            || self.login_device_id != other.login_device_id
            || self.login_install_id != other.login_install_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一对合法的登录设备身份（16 位 + 15 位数字，见 libresoda `QrDevice::is_valid`）。
    const DEVICE_ID: &str = "1234567890123456";
    const INSTALL_ID: &str = "123456789012345";

    /// 设置登录身份环境变量，离开作用域时清掉，避免污染同进程的其它用例。
    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }

    fn set_login_device_env(device: &str, install: &str) -> (EnvGuard, EnvGuard) {
        let device_guard = EnvGuard(ENV_LOGIN_DEVICE_ID);
        let install_guard = EnvGuard(ENV_LOGIN_INSTALL_ID);
        std::env::set_var(ENV_LOGIN_DEVICE_ID, device);
        std::env::set_var(ENV_LOGIN_INSTALL_ID, install);
        (device_guard, install_guard)
    }

    #[test]
    fn legacy_config_has_ten_gib_limit_and_audio_defaults() {
        let mut settings: Settings = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert_eq!(settings.volume, 1.0);
        assert_eq!(settings.audio_cache_limit_mb, 10 * 1024);
        assert!(settings.search_history_is_enabled());
        assert!(settings.search_history.is_empty());
        settings.set_volume(0.31);
        settings.toggle_mute();
        settings.toggle_mute();
        assert_eq!(settings.volume, 0.31);
        settings.set_volume(f32::NAN);
        assert_eq!(settings.volume, 0.31);
        settings.volume = -5.0;
        settings.volume_before_mute = 0.0;
        settings.audio_cache_limit_mb = u64::MAX;
        settings.normalize_playback();
        assert_eq!(settings.volume, 0.0);
        assert_eq!(settings.volume_before_mute, 1.0);
        assert_eq!(settings.audio_cache_limit_mb, 1024 * 1024);
    }

    #[test]
    fn search_history_deduplicates_and_keeps_ten_recent_entries() {
        let mut settings = Settings::default();
        for index in 0..12 {
            assert!(settings.record_search(&format!(" query-{index} ")));
        }
        assert_eq!(settings.search_history.len(), 10);
        assert_eq!(settings.search_history[0], "query-11");
        assert_eq!(settings.search_history[9], "query-2");

        assert!(settings.record_search("query-5"));
        assert_eq!(settings.search_history[0], "query-5");
        assert_eq!(
            settings
                .search_history
                .iter()
                .filter(|item| item.as_str() == "query-5")
                .count(),
            1
        );
        assert!(!settings.record_search("   "));
    }

    #[test]
    fn disabling_search_history_clears_and_stops_recording() {
        let mut settings = Settings::default();
        settings.record_search("first");
        assert!(settings.set_search_history_enabled(false));
        assert!(settings.search_history.is_empty());
        assert!(!settings.record_search("second"));
        assert!(!settings.set_search_history_enabled(false));
        assert!(settings.set_search_history_enabled(true));
        assert!(settings.record_search("third"));
        assert_eq!(settings.search_history, vec!["third"]);
    }

    #[test]
    fn login_device_identity_is_generated_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        let mut settings = Settings::default();
        assert!(settings.ensure_login_device(), "空配置应生成登录设备身份");
        let device = settings.login_device();
        assert!(device.is_valid(), "生成的设备身份形态必须合法");
        assert!(!settings.ensure_login_device(), "已有合法身份时不应再轮换");

        settings.save_to(&path).expect("save");
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.login_device(), device, "重载后设备身份必须保持稳定");
    }

    /// 首启（没有 config.json）时，登录身份必须由 `SODA_LOGIN_*` 决定，
    /// 不能先生成一份再让环境变量补不进去（#186）。
    #[test]
    fn login_device_env_wins_on_first_start() {
        let _lock = crate::env_test_lock();
        let _env = set_login_device_env(DEVICE_ID, INSTALL_ID);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.login_device_id, DEVICE_ID);
        assert_eq!(loaded.login_install_id, INSTALL_ID);
        assert!(
            !path.exists(),
            "环境变量已经给全身份，load_from 不该再生成并落盘另一份"
        );
    }

    /// 已有配置里的身份优先于环境变量：环境变量只补空字段（#186）。
    #[test]
    fn existing_login_device_beats_env() {
        let _lock = crate::env_test_lock();
        let _env = set_login_device_env("9999999999999999", "999999999999999");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            format!(r#"{{"login_device_id":"{DEVICE_ID}","login_install_id":"{INSTALL_ID}"}}"#),
        )
        .unwrap();

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.login_device_id, DEVICE_ID);
        assert_eq!(loaded.login_install_id, INSTALL_ID);
    }

    #[test]
    fn round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("sodam-cfg-{}", std::process::id()));
        let path = dir.join("config.json");
        let settings = Settings {
            signer_url: "http://127.0.0.1:8899/sign".into(),
            cookie: "sessionid_ss=x".into(),
            ..Default::default()
        };
        settings.save_to(&path).expect("save");

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.signer_url, settings.signer_url);
        assert_eq!(loaded.cookie, settings.cookie);
        assert!(loaded.theme.is_empty());
        assert!(loaded.language.is_empty());
        assert!(loaded.is_ready_for_vip());
        assert!(!Settings::default().is_ready_for_vip());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 老版本把旧默认地址写进了 config.json，读取时要自动换成当前默认值并落盘。
    #[test]
    fn migrates_legacy_default_signer() {
        let dir = std::env::temp_dir().join(format!("sodam-cfg-legacy-{}", std::process::id()));
        let path = dir.join("config.json");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            &path,
            format!(
                r#"{{"signer_url":"{LEGACY_SIGNER_URL}","signer_token":"{LEGACY_SIGNER_TOKEN}","cookie":"sessionid_ss=x","quality":"lossless"}}"#
            ),
        )
        .expect("write");

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.signer_url, DEFAULT_SIGNER_URL);
        assert_eq!(loaded.signer_token, DEFAULT_SIGNER_TOKEN);
        // 迁移不碰其它字段
        assert_eq!(loaded.cookie, "sessionid_ss=x");
        assert_eq!(loaded.quality, "lossless");

        // 迁移结果要落盘，否则用户看到的配置文件仍是死地址
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains(DEFAULT_SIGNER_URL));
        assert!(!text.contains("222.186.10.201"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 用户自己填的实例原样保留；旧地址配自定义 token 时只换地址，不动 token。
    #[test]
    fn keeps_user_supplied_signer() {
        let dir = std::env::temp_dir().join(format!("sodam-cfg-custom-{}", std::process::id()));
        let path = dir.join("config.json");

        let custom = Settings {
            signer_url: "http://signer.example.com:8899/sign".into(),
            signer_token: "my-own-token".into(),
            ..Default::default()
        };
        custom.save_to(&path).expect("save");
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.signer_url, custom.signer_url);
        assert_eq!(loaded.signer_token, custom.signer_token);

        // 旧地址 + 自定义 token：地址迁移，token 保留
        let mixed = Settings {
            signer_url: LEGACY_SIGNER_URL.into(),
            signer_token: "my-own-token".into(),
            ..Default::default()
        };
        mixed.save_to(&path).expect("save");
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.signer_url, DEFAULT_SIGNER_URL);
        assert_eq!(loaded.signer_token, "my-own-token");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 上一版默认地址（明文 HTTP）迁到新的 HTTPS 默认值，token 保持不变并落盘。
    #[test]
    fn migrates_previous_default_signer_to_https() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"signer_url":"{PREVIOUS_SIGNER_URL}","signer_token":"{DEFAULT_SIGNER_TOKEN}","cookie":"sessionid_ss=x"}}"#
            ),
        )
        .expect("write");

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.signer_url, DEFAULT_SIGNER_URL);
        assert_eq!(loaded.signer_token, DEFAULT_SIGNER_TOKEN);
        assert_eq!(loaded.cookie, "sessionid_ss=x");

        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains(DEFAULT_SIGNER_URL));
        assert!(!text.contains(PREVIOUS_SIGNER_URL));
    }

    /// 窗口按钮策略：默认 auto，未知值也回退 auto，老配置缺字段不影响。
    #[test]
    fn titlebar_mode_parses_and_resolves() {
        assert_eq!(TitlebarMode::default(), TitlebarMode::Auto);
        assert_eq!(TitlebarMode::from_key(""), TitlebarMode::Auto);
        assert_eq!(TitlebarMode::from_key("auto"), TitlebarMode::Auto);
        assert_eq!(TitlebarMode::from_key(" ALWAYS "), TitlebarMode::Always);
        assert_eq!(TitlebarMode::from_key("never"), TitlebarMode::Never);
        assert_eq!(TitlebarMode::from_key("nonsense"), TitlebarMode::Auto);

        // auto 跟随桌面：GNOME 这类要求客户端自绘 → 画；Hyprland 这类 → 不画
        assert!(TitlebarMode::Auto.draws_titlebar(true));
        assert!(!TitlebarMode::Auto.draws_titlebar(false));
        // 强制开关覆盖协商结果
        assert!(TitlebarMode::Always.draws_titlebar(false));
        assert!(!TitlebarMode::Never.draws_titlebar(true));

        // 旧配置没有 titlebar 字段时按默认值读出来
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"theme":"dark","language":"zh"}"#).unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.titlebar, "");
        assert_eq!(TitlebarMode::from_key(&loaded.titlebar), TitlebarMode::Auto);
    }

    /// 快捷键：默认值、空串回退、老配置缺字段也能读。
    #[test]
    fn shortcut_defaults_and_fallback() {
        let settings = Settings::default();
        assert_eq!(settings.shortcut_toggle(), DEFAULT_SHORTCUT_TOGGLE);
        assert_eq!(settings.shortcut_previous(), DEFAULT_SHORTCUT_PREVIOUS);
        assert_eq!(settings.shortcut_next(), DEFAULT_SHORTCUT_NEXT);

        let custom = Settings {
            shortcut_toggle: "alt-k".into(),
            // 只有空白也算未设置
            shortcut_previous: "   ".into(),
            ..Default::default()
        };
        assert_eq!(custom.shortcut_toggle(), "alt-k");
        assert_eq!(custom.shortcut_previous(), DEFAULT_SHORTCUT_PREVIOUS);

        // 老配置文件里没有这三个字段
        let parsed: Settings = serde_json::from_str(r#"{"cookie":"x"}"#).expect("老配置应能解析");
        assert_eq!(parsed.shortcut_next(), DEFAULT_SHORTCUT_NEXT);
        assert!(parsed.shortcut_toggle.is_empty());
    }

    /// gpui `unparse` 在本机给主修饰键用的拼法（macOS = cmd，Windows = win，其余 = super）。
    fn platform_key() -> &'static str {
        if cfg!(target_os = "macos") {
            "cmd"
        } else if cfg!(target_os = "windows") {
            "win"
        } else {
            "super"
        }
    }

    /// 旧 Display 写下来的符号形式被认出来并改写成规范串；合法写法与认不出的值一概不动。
    #[test]
    fn legacy_display_forms_migrate_and_legal_values_stay() {
        let expected = format!("{}-left", platform_key());
        assert_eq!(
            normalize_legacy_keystroke("⌘←").as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            normalize_legacy_keystroke("^←").as_deref(),
            Some("ctrl-left"),
            "macOS 的 control 曾被写成不带连字符的 ^"
        );
        assert_eq!(
            normalize_legacy_keystroke("ctrl-A").as_deref(),
            Some("ctrl-a"),
            "大写单字符是旧 Display 的产物，本意是 Ctrl+A"
        );
        let expected = format!("{}-shift-i", platform_key());
        assert_eq!(
            normalize_legacy_keystroke("⇧⌘I").as_deref(),
            Some(expected.as_str())
        );

        // 已经是合法写法：不改，用户在配置文件里看到的仍是他写的东西。
        assert_eq!(normalize_legacy_keystroke("ctrl-left"), None);
        assert_eq!(normalize_legacy_keystroke("cmd-left"), None);
        assert_eq!(normalize_legacy_keystroke(""), None);
        // 认不出的符号不猜。
        assert_eq!(normalize_legacy_keystroke("☃"), None);
    }

    /// 启动读到旧格式：改写并回存，用户下次打开配置文件看到的就是真正生效的值。
    #[test]
    fn load_from_migrates_legacy_shortcut_in_place() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"cookie":"x","shortcut_previous":"⌘←"}"#).expect("写入临时配置");
        let expected = format!("{}-left", platform_key());

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.shortcut_previous, expected);
        assert_eq!(loaded.cookie, "x", "迁移不该动别的字段");

        let reread = std::fs::read_to_string(&path).expect("迁移后应回存");
        assert!(
            reread.contains(expected.as_str()),
            "配置文件里应已是规范串：{reread}"
        );
    }

    /// 音质只存意图并记录来源：用户选择落 `user`，推导落 `derived`，缺字段的旧配置读成未知。
    #[test]
    fn quality_setting_records_source_and_preserves_legacy_configs() {
        use crate::quality::{QualityIntent, QualitySource};

        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("config.json");

        let mut settings = Settings::default();
        assert_eq!(settings.quality_source(), QualitySource::Unknown);

        // 用户手动设置音质后记为 User
        settings.set_quality_intent_by_user(&QualityIntent::Tier("lossless".into()));
        assert_eq!(settings.quality_source(), QualitySource::User);
        settings.save_to(&path).expect("save");

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.quality, "lossless");
        assert_eq!(loaded.quality_source(), QualitySource::User);

        // 程序推导设定音质后记为 Derived
        settings.set_quality_intent_with_source(
            &QualityIntent::Tier("highest".into()),
            QualitySource::Derived,
        );
        assert_eq!(settings.quality_source(), QualitySource::Derived);
        settings.save_to(&path).expect("save");

        let loaded_derived = Settings::load_from(&path);
        assert_eq!(loaded_derived.quality, "highest");
        assert_eq!(loaded_derived.quality_source(), QualitySource::Derived);

        // 旧配置里的空串：读成自动，来源为未知，且文件不被无故改写
        std::fs::write(
            &path,
            r#"{"quality":"","login_device_id":"1234567890123456","login_install_id":"123456789012345"}"#,
        )
        .expect("写入旧配置");
        let legacy = Settings::load_from(&path);
        assert_eq!(legacy.quality, "");
        assert_eq!(legacy.quality_intent(), QualityIntent::Auto);
        assert_eq!(legacy.quality_source(), QualitySource::Unknown);
        assert_eq!(
            std::fs::read_to_string(&path).expect("读回"),
            r#"{"quality":"","login_device_id":"1234567890123456","login_install_id":"123456789012345"}"#,
            "没有别的迁移时不应改写文件"
        );

        // 缺来源字段的具体档位旧配置：正常读取，来源为未知
        std::fs::write(&path, r#"{"quality":"lossless"}"#).expect("写入旧配置");
        let kept = Settings::load_from(&path);
        assert_eq!(kept.quality, "lossless");
        assert_eq!(
            kept.quality_intent(),
            QualityIntent::Tier("lossless".into())
        );
        assert_eq!(kept.quality_source(), QualitySource::Unknown);

        // 包含未知来源字符串的旧配置：容错回退为未知，不破坏整份配置解析
        std::fs::write(
            &path,
            r#"{"quality":"medium","quality_source":"unrecognized"}"#,
        )
        .expect("写入配置");
        let fallback = Settings::load_from(&path);
        assert_eq!(fallback.quality, "medium");
        assert_eq!(fallback.quality_source(), QualitySource::Unknown);
    }

    /// 环境变量注入音质偏好：未手选时覆盖并标记为 Derived；用户手选后不再被覆盖。
    #[test]
    fn env_quality_override_records_derived_source_and_respects_user_choice() {
        use crate::quality::{QualityIntent, QualitySource};

        let _lock = crate::env_test_lock();

        struct EnvGuard(&'static str);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var(self.0);
            }
        }

        let env_key = "SODAM_QUALITY";
        let _guard = EnvGuard(env_key);
        std::env::set_var(env_key, "lossless");
        let merged = Settings::default().merged_with_env();
        assert_eq!(merged.quality, "lossless");
        assert_eq!(merged.quality_source(), QualitySource::Derived);

        // 用户手选了档位（包括手选「自动」）时，环境变量不能覆盖用户的选择
        let mut user_picked = Settings::default();
        user_picked.set_quality_intent_by_user(&QualityIntent::Auto);
        assert_eq!(user_picked.quality_source(), QualitySource::User);
        let user_merged = user_picked.merged_with_env();
        assert_eq!(user_merged.quality, "auto");
        assert_eq!(user_merged.quality_source(), QualitySource::User);
    }

    /// 显式登出后只停用 Cookie 的环境变量补全，签名地址等其余字段照常补（#192）。
    #[test]
    fn env_fill_can_skip_cookie_only() {
        /// 离开作用域时还原到设值前的状态（本机原本设过这个变量也不丢）。
        struct EnvGuard {
            key: &'static str,
            previous: Option<std::ffi::OsString>,
        }
        impl EnvGuard {
            fn set(key: &'static str, value: &str) -> Self {
                let previous = std::env::var_os(key);
                std::env::set_var(key, value);
                Self { key, previous }
            }
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match self.previous.take() {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }

        let _lock = crate::env_test_lock();
        let _cookie_guard = EnvGuard::set(ENV_COOKIE_KEY, "cookie-from-env");
        let _signer_guard = EnvGuard::set("QISHUI_SIGNER_URL", "https://signer.example");

        // 签名地址有内置默认值，先清空，才能验证「Skip 只跳过 Cookie、其余字段照补」。
        let mut base = Settings::default();
        base.signer_url.clear();

        let skipped = base.clone().fill_empty_from_env(EnvCookieFill::Skip);
        assert!(
            skipped.cookie.is_empty(),
            "登出记号置位时不能用环境变量补 Cookie"
        );
        assert_eq!(skipped.signer_url, "https://signer.example");

        // 对照：没有记号时照旧补 Cookie，证明跳过不是「环境变量没设」造成的。
        let allowed = base.fill_empty_from_env(EnvCookieFill::Allow);
        assert_eq!(allowed.cookie, "cookie-from-env");
    }

    /// 详细日志：新配置默认关闭，旧配置缺字段也能读。
    #[test]
    fn detailed_logs_defaults_off_and_reads_old_configs() {
        assert!(!Settings::default().detailed_logs);
        let parsed: Settings = serde_json::from_str(r#"{"cookie":"x"}"#).expect("老配置应能解析");
        assert!(!parsed.detailed_logs);
        let enabled: Settings = serde_json::from_str(r#"{"detailed_logs":true}"#).expect("解析");
        assert!(enabled.detailed_logs);
    }

    /// 三态加载：不存在 / 正常 / 损坏要能区分开，损坏时报错带大致行列（#185）。
    #[test]
    fn try_load_distinguishes_missing_parsed_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        assert!(
            matches!(Settings::try_load_from(&path), LoadOutcome::NotFound(_)),
            "文件不存在应为 NotFound"
        );

        std::fs::write(&path, r#"{"cookie":"sessionid_ss=x"}"#).unwrap();
        match Settings::try_load_from(&path) {
            LoadOutcome::Loaded { settings, migrated } => {
                assert_eq!(settings.cookie, "sessionid_ss=x");
                assert!(!migrated);
            }
            _ => panic!("合法 JSON 应为 Loaded"),
        }

        std::fs::write(&path, "{oops").unwrap();
        match Settings::try_load_from(&path) {
            LoadOutcome::Corrupt { error } => {
                assert!(error.contains("line"), "损坏提示应带行列位置：{error}");
            }
            _ => panic!("非法 JSON 应为 Corrupt"),
        }
    }

    /// 坏文件：load_from 用默认值继续，磁盘上的坏内容原样保留（绝不覆盖）。
    #[test]
    fn corrupt_file_falls_back_to_defaults_without_touching_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let bad = r#"{"cookie":"sessionid_ss=x""#;
        std::fs::write(&path, bad).unwrap();

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded, Settings::default());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bad,
            "坏文件必须原样保留"
        );
        assert!(
            !settings_backup_path(&path).exists(),
            "没有成功加载就不该产生备份"
        );
    }

    /// 字段级容错：类型不对的字段丢掉补默认值，其它字段（尤其 Cookie 与设备身份）照常生效。
    #[test]
    fn bad_field_type_dropped_without_losing_other_fields() {
        let text = r#"{"cookie":"sessionid_ss=x","device_id":"dev","volume":"loud","detailed_logs":"yes","search_history":"nope","audio_cache_limit_mb":"x"}"#;
        let settings = parse_settings_text(text).expect("字段容错后应能解析");
        assert_eq!(settings.cookie, "sessionid_ss=x");
        assert_eq!(settings.device_id, "dev");
        assert_eq!(settings.volume, 1.0, "类型坏的字段回默认值");
        assert!(!settings.detailed_logs);
        assert!(settings.search_history.is_empty());
        assert_eq!(
            settings.audio_cache_limit_mb,
            crate::cache::DEFAULT_AUDIO_LIMIT_MB
        );

        // 目标类型校验：负数/小数进不了 u64，混型数组进不了 Vec<String>；
        // 两者都不该把整份配置打成损坏，丢掉该字段继续加载。
        let text = r#"{"cookie":"sessionid_ss=x","device_id":"dev","audio_cache_limit_mb":-1}"#;
        let settings = parse_settings_text(text).expect("负数缓存上限应被丢弃而不是整份失败");
        assert_eq!(settings.cookie, "sessionid_ss=x");
        assert_eq!(settings.device_id, "dev");
        assert_eq!(
            settings.audio_cache_limit_mb,
            crate::cache::DEFAULT_AUDIO_LIMIT_MB
        );
        let text = r#"{"cookie":"sessionid_ss=x","search_history":["有效记录",42]}"#;
        let settings = parse_settings_text(text).expect("混型搜索历史应被丢弃而不是整份失败");
        assert_eq!(settings.cookie, "sessionid_ss=x");
        assert!(settings.search_history.is_empty());
    }

    /// 无法识别的播放模式回退列表循环，不连累整份配置解析。
    #[test]
    fn unknown_play_mode_falls_back_to_list_loop() {
        let settings: Settings = serde_json::from_str(r#"{"play_mode":"Ultra"}"#).unwrap();
        assert_eq!(settings.play_mode, crate::queue::PlayMode::ListLoop);
        let shuffle: Settings = serde_json::from_str(r#"{"play_mode":"Shuffle"}"#).unwrap();
        assert_eq!(shuffle.play_mode, crate::queue::PlayMode::Shuffle);
    }

    /// 数值越界：加载路径上就钳制（音量 / 静音前音量 / 缓存上限）。
    #[test]
    fn load_clamps_out_of_range_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"volume":5.0,"volume_before_mute":-1.0,"audio_cache_limit_mb":1}"#,
        )
        .unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.volume, 1.0);
        assert_eq!(loaded.volume_before_mute, 1.0);
        assert_eq!(loaded.audio_cache_limit_mb, 1024);
    }

    /// 备份恢复：主文件坏了、备份可用 → 备份内容可用；坏文件改名保留、原件内容不丢。
    #[test]
    fn corrupt_main_recovers_from_backup_and_quarantines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        std::fs::write(&path, r#"{"cookie":"sessionid_ss=x","theme":"dark"}"#).unwrap();
        let good = parse_settings_text(&std::fs::read_to_string(&path).unwrap()).unwrap();
        write_settings_backup(&path, &good).expect("写备份");

        std::fs::write(&path, "not json at all").unwrap();
        let recovered = load_settings_backup(&path).expect("备份应可读");
        assert_eq!(recovered.cookie, "sessionid_ss=x");
        assert_eq!(recovered.theme, "dark");

        let bad = quarantine_corrupt_file(&path).expect("坏文件应能改名保留");
        assert!(bad.exists(), "坏文件应保留在 {}", bad.display());
        assert!(!path.exists(), "原路径应腾空给恢复内容");
        recovered.save_to(&path).expect("恢复内容写回主文件");
        assert_eq!(Settings::load_from(&path).cookie, "sessionid_ss=x");
        assert_eq!(
            std::fs::read_to_string(&bad).unwrap(),
            "not json at all",
            "坏文件内容应原样保留"
        );
    }

    /// 备份恢复走同一套加载管线：备份里的越界数值同样钳制，
    /// 关闭搜索历史后残留的记录同样清理，不会被原样启用（#185 审计）。
    #[test]
    fn load_for_startup_normalizes_backup_before_recovering() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            settings_backup_path(&path),
            r#"{"cookie":"sessionid_ss=x","volume":5.0,"audio_cache_limit_mb":1,"search_history_enabled":false,"search_history":["stale"]}"#,
        )
        .unwrap();
        std::fs::write(&path, "corrupt!").unwrap();

        let load = load_for_startup(&path);
        assert!(
            matches!(
                load.recovery,
                Some(StartupRecovery::RecoveredFromBackup { .. })
            ),
            "备份可用应走恢复分支"
        );
        assert_eq!(load.settings.cookie, "sessionid_ss=x");
        assert_eq!(load.settings.volume, 1.0, "备份里的越界音量要钳制");
        assert_eq!(load.settings.audio_cache_limit_mb, 1024);
        assert!(
            load.settings.search_history.is_empty(),
            "关闭搜索历史后不得残留旧记录"
        );
        // 恢复的落盘修正要在首帧后由后台执行（#191）。
        load.write_back
            .expect("备份恢复应欠落盘修正")
            .persist()
            .expect("后台修正应成功");
    }

    /// 读盘阶段不写盘：正常路径只在内存里补身份 / 迁移，备份与回存都等 persist（#191）。
    #[test]
    fn load_for_startup_defers_disk_writes_until_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"cookie":"sessionid_ss=x","theme":"dark"}"#).unwrap();
        let original = std::fs::read(&path).unwrap();

        let load = load_for_startup(&path);
        assert!(load.recovery.is_none());
        assert_eq!(load.settings.cookie, "sessionid_ss=x");
        assert!(!settings_backup_path(&path).exists(), "读盘阶段不得写备份");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "读盘阶段不得改主文件"
        );

        load.write_back
            .expect("正常路径应欠一次备份刷新")
            .persist()
            .expect("后台修正应成功");
        assert!(settings_backup_path(&path).exists(), "首帧后应补上备份");
    }

    /// 损坏路径：读盘阶段只算隔离目标，坏文件不动；persist 之后才改名保留并恢复主文件。
    #[test]
    fn load_for_startup_defers_corrupt_quarantine_until_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let backup = Settings {
            cookie: "sessionid_ss=x".into(),
            ..Default::default()
        };
        write_settings_backup(&path, &backup).unwrap();
        std::fs::write(&path, "corrupt!").unwrap();

        let load = load_for_startup(&path);
        let bad_path = match &load.recovery {
            Some(StartupRecovery::RecoveredFromBackup { bad_path }) => bad_path.clone(),
            other => panic!("应走备份恢复分支：{other:?}"),
        };
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "corrupt!",
            "读盘阶段不得动坏文件"
        );
        assert!(!bad_path.exists(), "读盘阶段不得改名");

        load.write_back
            .expect("损坏恢复应欠落盘修正")
            .persist()
            .expect("后台修正应成功");
        assert!(
            bad_path.exists(),
            "persist 后坏文件应保留在 {}",
            bad_path.display()
        );
        assert_eq!(std::fs::read_to_string(&bad_path).unwrap(), "corrupt!");
        assert_eq!(Settings::load_from(&path).cookie, "sessionid_ss=x");
    }

    /// 无备份的损坏：默认值启动，不欠任何写操作，坏文件原样保留。
    #[test]
    fn load_for_startup_defaults_without_scheduling_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "corrupt!").unwrap();

        let load = load_for_startup(&path);
        assert!(
            matches!(
                load.recovery,
                Some(StartupRecovery::FellBackToDefaults { .. })
            ),
            "无备份应回默认值"
        );
        assert!(load.write_back.is_none(), "默认值启动不应安排落盘修正");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "corrupt!",
            "坏文件必须原样保留"
        );
    }

    /// 首启（主文件不存在）必须安排创建配置文件：身份全由环境变量给齐时也要建。
    #[test]
    fn first_run_always_schedules_config_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        let load = load_for_startup(&path);
        assert!(load.first_run, "主文件不存在应标记首启");
        let write_back = load.write_back.expect("首启应安排落盘修正");
        assert!(write_back.save_main, "首启必须创建配置文件");
        write_back.persist().expect("首启落盘应成功");
        assert!(path.exists(), "首启 persist 后应生成配置文件");
    }

    /// 落盘失败只回报错误，不 panic；状态行提示由 UI 侧负责。
    #[test]
    fn startup_write_back_reports_first_error() {
        let dir = tempfile::tempdir().unwrap();
        // 父路径是普通文件：创建目录与写文件都会失败。
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, "file").unwrap();
        let write_back = StartupWriteBack {
            path: blocker.join("config.json"),
            settings: Settings::default(),
            quarantine_to: None,
            save_main: false,
            expected: None,
        };
        assert!(
            write_back.persist().is_err(),
            "写不进去应返回错误而不是 panic"
        );
    }

    /// 读盘后、回写前文件被外部改过：放弃覆盖，磁盘上的新内容保持不动。
    #[test]
    fn startup_write_back_skips_when_file_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"cookie":"sessionid_ss=old"}"#).unwrap();

        let load = load_for_startup(&path);
        let write_back = load.write_back.expect("正常路径应欠备份刷新");
        // 模拟外部在首帧后改写了配置（长度不同，指纹必然变化）。
        std::fs::write(&path, r#"{"cookie":"sessionid_ss=definitely-new"}"#).unwrap();
        write_back.persist().expect("指纹不符应安静跳过");

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"cookie":"sessionid_ss=definitely-new"}"#,
            "外部新内容不能被启动快照覆盖"
        );
        assert!(!settings_backup_path(&path).exists(), "跳过时不应刷新备份");
    }

    /// 保存时目标文件已是坏配置：先改名保留原件，再写新内容，用户能找回原文件。
    /// 非 UTF-8 字节与 `null` 这类「读得出但解析不了」的原件同样要被隔离。
    #[test]
    fn save_quarantines_corrupt_target_before_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let settings = Settings {
            cookie: "sessionid_ss=x".into(),
            ..Default::default()
        };

        // 非法字节序列：read_to_string 会失败，但保存前仍要保留原件。
        let bad_bytes = [0xFF, 0xFE, b'{'];
        std::fs::write(&path, bad_bytes).unwrap();
        settings.save_to(&path).expect("保存应成功");
        assert_eq!(Settings::load_from(&path).cookie, "sessionid_ss=x");
        let quarantined: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".json.bad-"))
            .collect();
        assert_eq!(quarantined.len(), 1, "非 UTF-8 原件应被保留一份");
        assert_eq!(
            std::fs::read(quarantined[0].path()).unwrap(),
            bad_bytes,
            "坏原件内容应原样保留"
        );
        std::fs::remove_file(quarantined[0].path()).unwrap();

        // `null`：语法合法但解析不了配置，同样不许盲覆盖。
        std::fs::write(&path, "null").unwrap();
        settings.save_to(&path).expect("保存应成功");
        let quarantined: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".json.bad-"))
            .collect();
        assert_eq!(quarantined.len(), 1, "null 原件应被保留一份");
        assert_eq!(
            std::fs::read_to_string(quarantined[0].path()).unwrap(),
            "null"
        );
    }

    /// 慢盘调试开关只认正整数毫秒，其它写法一律当没设（#197）。
    #[cfg(debug_assertions)]
    #[test]
    fn slow_save_delay_parsing() {
        assert_eq!(Settings::debug_slow_save_delay(None), None);
        assert_eq!(Settings::debug_slow_save_delay(Some("")), None);
        assert_eq!(Settings::debug_slow_save_delay(Some("0")), None);
        assert_eq!(Settings::debug_slow_save_delay(Some("-5")), None);
        assert_eq!(Settings::debug_slow_save_delay(Some("abc")), None);
        assert_eq!(
            Settings::debug_slow_save_delay(Some(" 250 ")),
            Some(std::time::Duration::from_millis(250))
        );
    }

    /// 登出写盘：盘上身份没变才清 Cookie；换了人不许覆盖，别的字段要留住（#197）。
    #[test]
    fn logout_save_clears_cookie_only_for_the_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let signed_in = Settings {
            cookie: "sessionid_ss=a".into(),
            theme: "dark".into(),
            ..Settings::default()
        };
        signed_in.save_to(&path).expect("保存应成功");

        let snapshot = signed_in.clone();

        // 盘上还是 A：清掉 Cookie 写回，主题这类别的字段原样留住。
        assert!(snapshot
            .save_signed_out_to(&path, "sessionid_ss=a")
            .unwrap());
        let disk = settings_on_disk(&path);
        assert_eq!(disk.cookie, "");
        assert_eq!(disk.theme, "dark");

        // 盘上换成 B：旧结果不许把新身份盖回去。
        let other = Settings {
            cookie: "sessionid_ss=b".into(),
            ..Settings::default()
        };
        other.save_to(&path).expect("保存应成功");
        assert!(!snapshot
            .save_signed_out_to(&path, "sessionid_ss=a")
            .unwrap());
        assert_eq!(settings_on_disk(&path).cookie, "sessionid_ss=b");

        // 盘上没有 Cookie（内存身份来自环境变量补全）：不用写，也算登出成功。
        Settings::default().save_to(&path).expect("保存应成功");
        assert!(snapshot
            .save_signed_out_to(&path, "sessionid_ss=a")
            .unwrap());
        assert_eq!(settings_on_disk(&path).cookie, "");
    }

    /// 直接读盘上 JSON，绕开环境变量补全，免得用例受外部影响。
    fn settings_on_disk(path: &std::path::Path) -> Settings {
        let raw = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&raw).unwrap()
    }
}
