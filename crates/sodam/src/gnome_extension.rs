//! GNOME 顶栏歌词扩展的自动安装。
//!
//! 汽水官方的「任务栏歌词」依赖客户端注入；SodaM 用独立 GNOME Shell 扩展实现，
//! 首次启动时把内置的扩展文件写到用户目录并尝试启用，避免用户手动复制。
//!
//! 只在 GNOME 会话、且系统有 `gnome-extensions` 工具时执行；写文件失败不影响主程序。

use std::path::{Path, PathBuf};
use std::process::Command;

const UUID: &str = "sodam-lyrics@sodahub.local";

const EXTENSION_JS: &str = include_str!(
    "../../../packaging/gnome-shell/sodam-lyrics@sodahub.local/extension.js"
);
const METADATA_JSON: &str = include_str!(
    "../../../packaging/gnome-shell/sodam-lyrics@sodahub.local/metadata.json"
);
const STYLESHEET_CSS: &str = include_str!(
    "../../../packaging/gnome-shell/sodam-lyrics@sodahub.local/stylesheet.css"
);

const ICONS: &[(&str, &[u8])] = &[
    (
        "heart.svg",
        include_bytes!(
            "../../../packaging/gnome-shell/sodam-lyrics@sodahub.local/icons/heart.svg"
        ),
    ),
    (
        "heart-filled.svg",
        include_bytes!(
            "../../../packaging/gnome-shell/sodam-lyrics@sodahub.local/icons/heart-filled.svg"
        ),
    ),
    (
        "play.svg",
        include_bytes!(
            "../../../packaging/gnome-shell/sodam-lyrics@sodahub.local/icons/play.svg"
        ),
    ),
    (
        "pause.svg",
        include_bytes!(
            "../../../packaging/gnome-shell/sodam-lyrics@sodahub.local/icons/pause.svg"
        ),
    ),
    (
        "skip-back.svg",
        include_bytes!(
            "../../../packaging/gnome-shell/sodam-lyrics@sodahub.local/icons/skip-back.svg"
        ),
    ),
    (
        "skip-forward.svg",
        include_bytes!(
            "../../../packaging/gnome-shell/sodam-lyrics@sodahub.local/icons/skip-forward.svg"
        ),
    ),
];

/// 启动时调用：安装/更新扩展并尽量启用。任何失败都只记日志。
pub fn ensure_installed() {
    if std::env::var_os("SODAM_NO_EXTENSION_INSTALL").is_some() {
        return;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if !desktop.to_ascii_lowercase().contains("gnome") {
        return;
    }
    let Some(dir) = target_dir() else {
        crate::debug_log("gnome extension: 无法确定用户数据目录");
        return;
    };
    let extension_js = dir.join("extension.js");
    // 开发环境用的是 impl.js 热加载器：不要覆盖，否则改代码要注销重登。
    let is_dev_loader = std::fs::read_to_string(&extension_js)
        .map(|text| text.contains("impl.js"))
        .unwrap_or(false);
    if !is_dev_loader {
        if let Err(err) = write_files(&dir) {
            crate::debug_log(&format!("gnome extension: 写入失败 {err}"));
            return;
        }
    }
    enable_if_needed();
}

fn target_dir() -> Option<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))?;
    Some(data_home.join("gnome-shell/extensions").join(UUID))
}

fn write_files(dir: &Path) -> std::io::Result<()> {
    write_if_changed(&dir.join("extension.js"), EXTENSION_JS.as_bytes())?;
    write_if_changed(&dir.join("metadata.json"), METADATA_JSON.as_bytes())?;
    write_if_changed(&dir.join("stylesheet.css"), STYLESHEET_CSS.as_bytes())?;
    let icons = dir.join("icons");
    std::fs::create_dir_all(&icons)?;
    for (name, bytes) in ICONS {
        write_if_changed(&icons.join(name), bytes)?;
    }
    Ok(())
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).map(|old| old == bytes).unwrap_or(false) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

fn enable_if_needed() {
    let Ok(output) = Command::new("gnome-extensions")
        .args(["list", "--enabled"])
        .output()
    else {
        return;
    };
    if String::from_utf8_lossy(&output.stdout).lines().any(|line| line.trim() == UUID) {
        return;
    }
    match Command::new("gnome-extensions").args(["enable", UUID]).output() {
        Ok(result) if result.status.success() => {
            crate::debug_log("gnome extension: 已启用顶栏歌词扩展");
        }
        Ok(result) => crate::debug_log(&format!(
            "gnome extension: 启用失败 {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )),
        Err(err) => crate::debug_log(&format!("gnome extension: 调用失败 {err}")),
    }
}
