//! 签名服务（libmssdk + wine）的本地安装与进程管理。
//!
//! 汽水的写接口与整曲取流需要客户端原生签名（`X-Helios`/`X-Medusa`）。
//! 官方模块 `bdms.node` / `metasecml.dll` 不由本项目分发：用户同意后从官方安装包
//! 本机提取，Node（Windows 版）与 libmssdk 源码按需下载。
//!
//! 这里只做三件事：准备文件、拉起 `wine node serve`、报告状态。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Context, Result};

/// Node（Windows 便携版）版本；`node.exe` 由下载的 zip 提供。
const NODE_ZIP: &str = "https://nodejs.org/dist/v22.11.0/node-v22.11.0-win-x64.zip";
const LIBMSSDK_TARBALL: &str =
    "https://codeload.github.com/sodahub-org/libmssdk/tar.gz/refs/heads/main";

static CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn child_slot() -> &'static Mutex<Option<Child>> {
    CHILD.get_or_init(|| Mutex::new(None))
}

/// 签名相关文件的根目录：`<data>/sodam/signer`。
pub fn signer_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sodam")
        .join("signer")
}

pub fn node_exe() -> PathBuf {
    signer_root().join("node").join("node.exe")
}

pub fn vendor_dir() -> PathBuf {
    signer_root().join("vendor")
}

pub fn libmssdk_dir() -> PathBuf {
    signer_root().join("libmssdk")
}

pub fn installer_cache() -> PathBuf {
    signer_root().join("installer.exe")
}

pub fn log_path() -> PathBuf {
    signer_root().join("signer.log")
}

/// 依赖与运行状态（设置页展示用）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SignerStatus {
    pub wine: Option<String>,
    pub node: bool,
    pub vendor: bool,
    pub libmssdk: bool,
    pub seven_zip: bool,
    pub running: bool,
    pub ready: bool,
    pub url: String,
    pub last_error: Option<String>,
}

/// 探测当前环境。
pub fn detect() -> SignerStatus {
    let wine = Command::new("wine")
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let seven_zip = ["7z", "7zz", "7za"].iter().any(|bin| {
        Command::new(bin)
            .arg("i")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    });
    let mut status = SignerStatus {
        wine,
        node: node_exe().is_file(),
        vendor: vendor_dir().join("bdms.node").is_file()
            && vendor_dir().join("metasecml.dll").is_file(),
        libmssdk: libmssdk_dir().join("src/index.mjs").is_file(),
        seven_zip,
        running: false,
        ready: is_ready(),
        url: "http://127.0.0.1:8899/sign".to_string(),
        last_error: None,
    };
    status.running = child_slot()
        .lock()
        .ok()
        .and_then(|mut slot| {
            slot.as_mut()
                .map(|child| child.try_wait().ok().flatten().is_none())
        })
        .unwrap_or(false);
    status
}

fn http_get(url: &str) -> Result<Vec<u8>> {
    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(120))
        .call()
        .map_err(|err| anyhow!("下载失败 {url}: {err}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .context("读取下载内容失败")?;
    Ok(bytes)
}

/// 下载并解压 Windows 版 `node.exe`。
pub fn install_node() -> Result<()> {
    std::fs::create_dir_all(signer_root().join("node"))?;
    let bytes = http_get(NODE_ZIP)?;
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).context("node zip 解析失败")?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        if entry.name().ends_with("/node.exe") {
            let mut out = std::fs::File::create(node_exe())?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    Err(anyhow!("zip 里没有 node.exe"))
}

/// 下载 libmssdk 源码（tarball）。
pub fn install_libmssdk() -> Result<()> {
    let bytes = http_get(LIBMSSDK_TARBALL)?;
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let staging = signer_root().join("libmssdk.tmp");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    archive.unpack(&staging)?;
    // tarball 顶层是 `libmssdk-main/`
    let inner = std::fs::read_dir(&staging)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.is_dir())
        .ok_or_else(|| anyhow!("tarball 结构异常"))?;
    let _ = std::fs::remove_dir_all(libmssdk_dir());
    std::fs::rename(&inner, libmssdk_dir())?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

/// 从官方安装包里提取 `bdms.node` 与 `metasecml.dll`（需要系统 `7z`）。
pub fn extract_vendor_from_installer(installer: &Path) -> Result<()> {
    let seven = ["7z", "7zz", "7za"]
        .into_iter()
        .find(|bin| {
            Command::new(bin)
                .arg("i")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow!("未找到 7z，请先安装 p7zip-full"))?;
    let staging = signer_root().join("installer.tmp");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let status = Command::new(seven)
        .arg("x")
        .arg("-y")
        .arg(format!("-o{}", staging.display()))
        .arg(installer)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("调用 7z 失败")?;
    if !status.success() {
        return Err(anyhow!("7z 解包失败"));
    }
    // 官方安装包是 NSIS 套一层 app.7z：第二层再解一次。
    for archive in find_files(&staging, "7z") {
        let _ = Command::new(seven)
            .arg("x")
            .arg("-y")
            .arg(format!("-o{}", staging.display()))
            .arg(&archive)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let bdms = find_file(&staging, "bdms.node").ok_or_else(|| anyhow!("安装包里没有 bdms.node"))?;
    let metasec = find_file(&staging, "metasecml.dll")
        .ok_or_else(|| anyhow!("安装包里没有 metasecml.dll"))?;
    std::fs::create_dir_all(vendor_dir())?;
    std::fs::copy(&bdms, vendor_dir().join("bdms.node"))?;
    std::fs::copy(&metasec, vendor_dir().join("metasecml.dll"))?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

fn find_files(root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
                found.push(path);
            }
        }
    }
    found
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
                return Some(path);
            }
        }
    }
    None
}

pub fn is_ready() -> bool {
    ureq::get("http://127.0.0.1:8899/readyz")
        .timeout(std::time::Duration::from_secs(2))
        .call()
        .map(|response| response.status() == 200)
        .unwrap_or(false)
}

/// 启动签名服务：`wine node src/index.mjs serve`。
pub fn start_service(token: Option<&str>) -> Result<()> {
    if detect().running {
        return Ok(());
    }
    let seven_z = detect();
    if seven_z.wine.is_none() {
        return Err(anyhow!("未安装 wine"));
    }
    if !node_exe().is_file() {
        return Err(anyhow!("缺少 node.exe"));
    }
    if !vendor_dir().join("bdms.node").is_file() || !vendor_dir().join("metasecml.dll").is_file() {
        return Err(anyhow!("缺少 bdms.node / metasecml.dll"));
    }
    let entry = libmssdk_dir().join("src/index.mjs");
    if !entry.is_file() {
        return Err(anyhow!("缺少 libmssdk 源码"));
    }
    std::fs::create_dir_all(signer_root())?;
    let log = std::fs::File::create(log_path())?;
    let log_err = log.try_clone()?;
    let mut command = Command::new("wine");
    command
        .arg(node_exe())
        .arg(&entry)
        .arg("serve")
        .current_dir(libmssdk_dir())
        .env("LIBMSSDK_BDMS_MODULE", vendor_dir().join("bdms.node"))
        .env("LIBMSSDK_STATE_DIR", signer_root().join("state"))
        .env("LIBMSSDK_WORKERS", "1")
        .env("WINEDEBUG", "-all")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    if let Some(token) = token.filter(|value| !value.trim().is_empty()) {
        command.env("LIBMSSDK_TOKEN", token.trim());
    }
    let child = command.spawn().context("启动签名服务失败")?;
    *child_slot().lock().unwrap() = Some(child);
    Ok(())
}

/// 停止签名服务。
pub fn stop_service() {
    if let Ok(mut slot) = child_slot().lock() {
        if let Some(mut child) = slot.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 调本地签名服务，返回要附加的请求头。
///
/// 入参必须是**实际要发送**的 URL 与请求头：签名覆盖它们，事后改头会失效。
pub fn request_signature(
    signer_url: &str,
    token: &str,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: &str,
) -> Option<Vec<(String, String)>> {
    if signer_url.trim().is_empty() {
        return None;
    }
    let payload = serde_json::json!({
        "url": url,
        "method": method,
        "body": body,
        "headers": headers.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
        "ts_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_millis() as u64)
            .unwrap_or_default(),
    });
    let mut request = ureq::post(signer_url.trim())
        .timeout(std::time::Duration::from_secs(20))
        .set("content-type", "application/json");
    if !token.trim().is_empty() {
        request = request.set("Authorization", &format!("Bearer {}", token.trim()));
    }
    let response = request.send_string(&payload.to_string()).ok()?;
    let value: serde_json::Value = response.into_string().ok()?.parse().ok()?;
    if value.get("ok").and_then(|ok| ok.as_bool()) == Some(false) {
        return None;
    }
    let headers = value.get("headers")?.as_object()?;
    Some(
        headers
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
            .collect(),
    )
}

/// 官方安装包下载地址（由调用方负责用户同意与地址来源）。
pub fn installer_url_hint() -> &'static str {
    "https://qishui.douyin.com/"
}
