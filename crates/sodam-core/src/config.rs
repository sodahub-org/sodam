//! 应用配置（`~/.config/sodam/config.json`）。

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(crate) static SETTINGS_WRITE_LOCK: Mutex<()> = Mutex::new(());

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
    /// 音质偏好：只存**意图**——`auto` 或一个具体档位（`lossless` / `highest` / `medium` / `low`）。
    ///
    /// 空串是历史写法，按 `auto` 读；新写入一律写 `auto`。实际拿到哪一档由服务端按
    /// 账号权益与曲目可用性决定，不落盘（见 [`crate::quality`]）。
    pub quality: String,
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
pub const DEFAULT_SIGNER_URL: &str = "http://103.236.75.83:8899/sign";
/// 上面那台服务的 token（等同取流能力，别外传；换服务时在设置里替换）。
pub const DEFAULT_SIGNER_TOKEN: &str = "b356ad402bc31ee9e5a99cfdbf41eaa39961a562251c3359";

/// 换服务前的那台内置实例，2026-10 起已下线。
/// 老版本的默认值会落进 config.json，所以读取时要把它们迁到当前默认值，
/// 否则老用户会一直指着一台死掉的服务。
const LEGACY_SIGNER_URL: &str = "http://222.186.10.201:8921/sign";
const LEGACY_SIGNER_TOKEN: &str = "05f8089b8c5f60c63f2a6dcfe1028d28ee2725a504f3e59b";

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

impl Default for Settings {
    fn default() -> Self {
        Self {
            signer_url: DEFAULT_SIGNER_URL.to_string(),
            signer_token: DEFAULT_SIGNER_TOKEN.to_string(),
            cookie: String::new(),
            device_id: String::new(),
            iid: String::new(),
            fp: String::new(),
            // 只存意图，不存实际档位：新写入一律 auto，空串只作历史写法来读
            //（见 crate::quality）。不再按会员等级推导一个具体档位写回。
            quality: crate::quality::AUTO.to_string(),
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
    pub fn load() -> Self {
        Self::load_from(&Self::config_path())
    }

    pub fn load_from(path: &Path) -> Self {
        let Some(settings) = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
        else {
            return Self::default();
        };
        let (mut settings, migrated) = settings.migrated();
        settings.normalize_playback();
        settings.normalize_search_history();
        // 迁移后立刻落盘：用户下次打开配置文件看到的、以及设置页显示的，都是真正生效的那份。
        // 写失败不影响启动，内存里的值已经是迁移后的。
        if migrated {
            let _ = settings.save_to(path);
        }
        settings
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
        self.save_to_locked(path)
    }

    pub(crate) fn save_to_locked(&self, path: &Path) -> anyhow::Result<()> {
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        // 同目录原子替换，避免退出/并发保存留下半份 JSON（Windows 也支持替换旧文件）。
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.persist(path).map_err(|err| err.error)?;
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

    /// 写入音质意图：只落意图，不落解析出来的实际档位。
    pub fn set_quality_intent(&mut self, intent: &crate::quality::QualityIntent) {
        self.quality = intent.as_setting().to_string();
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
    /// 的规则它会永远填不进去。所以它按**意图**判断——只有当前意图是自动时才让环境变量
    /// 覆盖，用户手选的具体档位不会被开发变量悄悄改掉。
    pub fn merged_with_env(mut self) -> Self {
        let pairs = [
            ("SODA_COOKIE", &mut self.cookie),
            ("QISHUI_SIGNER_URL", &mut self.signer_url),
            ("QISHUI_SIGNER_TOKEN", &mut self.signer_token),
            ("SODA_DEVICE_ID", &mut self.device_id),
            ("SODA_IID", &mut self.iid),
            ("SODA_FP", &mut self.fp),
        ];
        for (key, slot) in pairs {
            if slot.trim().is_empty() {
                if let Ok(value) = std::env::var(key) {
                    if !value.trim().is_empty() {
                        *slot = value.trim().to_string();
                    }
                }
            }
        }
        if self.quality_intent() == crate::quality::QualityIntent::Auto {
            if let Ok(value) = std::env::var("SODAM_QUALITY") {
                if !value.trim().is_empty() {
                    self.quality = value.trim().to_string();
                }
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 音质只存意图：写 `auto` 读回仍是 `auto`，旧文件的空串读成自动，且不做改写。
    #[test]
    fn quality_setting_round_trips_intent_without_rewriting_legacy_values() {
        use crate::quality::QualityIntent;

        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("config.json");

        let mut settings = Settings::default();
        settings.set_quality_intent(&QualityIntent::Auto);
        settings.save_to(&path).expect("save");
        assert_eq!(
            Settings::load_from(&path).quality,
            "auto",
            "自动要落成 auto 这一种写法"
        );

        // 旧配置里的空串：读成自动，但文件内容原样保留（我们不猜它是不是用户选的）
        std::fs::write(&path, r#"{"quality":""}"#).expect("写入旧配置");
        let legacy = Settings::load_from(&path);
        assert_eq!(legacy.quality, "");
        assert_eq!(legacy.quality_intent(), QualityIntent::Auto);
        assert_eq!(
            std::fs::read_to_string(&path).expect("读回"),
            r#"{"quality":""}"#,
            "没有别的迁移时不应改写文件"
        );

        // 用户（或旧版本）写的具体档位一律不动
        std::fs::write(&path, r#"{"quality":"lossless"}"#).expect("写入旧配置");
        let kept = Settings::load_from(&path);
        assert_eq!(kept.quality, "lossless");
        assert_eq!(
            kept.quality_intent(),
            QualityIntent::Tier("lossless".into())
        );
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
}
