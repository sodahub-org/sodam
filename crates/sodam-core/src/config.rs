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
    /// 音质偏好：`best` / `lossless` / `highest` / `medium` / `low`。
    pub quality: String,
    /// 当前音量与静音前音量（0..=1）。旧配置缺字段时沿用 100%。
    pub volume: f32,
    pub volume_before_mute: f32,
    /// 音频磁盘缓存上限（MiB），默认 10 GiB，设置页可调整。
    pub audio_cache_limit_mb: u64,
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
            // 空 = 自动：登录后按账号 VIP 情况挑能用的最高档（见 Session::refresh_quality）
            quality: String::new(),
            volume: 1.0,
            volume_before_mute: 1.0,
            audio_cache_limit_mb: crate::cache::DEFAULT_AUDIO_LIMIT_MB,
            theme: String::new(),
            language: String::new(),
            titlebar: String::new(),
            shortcut_toggle: String::new(),
            shortcut_previous: String::new(),
            shortcut_next: String::new(),
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
        // 迁移后立刻落盘：用户下次打开配置文件看到的、以及设置页显示的，都是真正生效的那份。
        // 写失败不影响启动，内存里的值已经是迁移后的。
        if migrated {
            let _ = settings.save_to(path);
        }
        settings
    }

    /// 把老版本的默认端点迁到当前默认值。
    ///
    /// 只认「等于旧默认值」这一种情况，用户填过别的地址就原样保留；
    /// token 同理，避免覆盖用户自己给别的实例配的 token。
    /// 返回是否发生了改动。
    fn migrated(mut self) -> (Self, bool) {
        if self.signer_url.trim() != LEGACY_SIGNER_URL {
            return (self, false);
        }
        self.signer_url = DEFAULT_SIGNER_URL.to_string();
        if self.signer_token.trim() == LEGACY_SIGNER_TOKEN {
            self.signer_token = DEFAULT_SIGNER_TOKEN.to_string();
        }
        (self, true)
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
    pub fn merged_with_env(mut self) -> Self {
        let pairs = [
            ("SODA_COOKIE", &mut self.cookie),
            ("QISHUI_SIGNER_URL", &mut self.signer_url),
            ("QISHUI_SIGNER_TOKEN", &mut self.signer_token),
            ("SODA_DEVICE_ID", &mut self.device_id),
            ("SODA_IID", &mut self.iid),
            ("SODA_FP", &mut self.fp),
            ("SODAM_QUALITY", &mut self.quality),
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
}
