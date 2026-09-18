//! 应用配置（`~/.config/sodam/config.json`）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    /// 界面主题：`dark` / `light`；空 = 第一次启动跟随系统偏好。
    pub theme: String,
    /// 界面语言：`zh` / `en`；空或 `auto` = 跟随系统语言。
    pub language: String,
}

/// 项目自建的签名服务（sodahub-org 部署）：开箱即用，可在设置里改成别的实例。
pub const DEFAULT_SIGNER_URL: &str = "http://222.186.10.201:8921/sign";
/// 上面那台服务的 token（等同取流能力，别外传；换服务时在设置里替换）。
pub const DEFAULT_SIGNER_TOKEN: &str = "05f8089b8c5f60c63f2a6dcfe1028d28ee2725a504f3e59b";

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
            theme: String::new(),
            language: String::new(),
        }
    }
}

impl Settings {
    pub fn config_path() -> PathBuf {
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
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// 写配置（自动建目录）。
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&Self::config_path())
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// 是否已经具备"能放 VIP 整曲"的条件（cookie + 签名服务）。
    pub fn is_ready_for_vip(&self) -> bool {
        !self.cookie.trim().is_empty() && !self.signer_url.trim().is_empty()
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
}
