//! CDN Range 能力探针：验证汽水播放流（签名 URL）是否支持 HTTP `Range` 续传，
//! 以及**换发新签名 URL 后续传是否仍然成立**。
//!
//! 背景：`Session::download_to_cache` 在大文件（无损整曲 25–54MB）下载被慢 CDN
//! 掐断后只会从字节 0 重试；要做续传，必须先拿到 CDN 对 `Range` 的真实行为证据：
//!
//! * Probe 1：`Range: bytes=0-0` + `Accept-Encoding: identity` —— 能力探测、
//!   总长（`Content-Range` 的 `N` 对比 `info.size`）、可用校验器（强 ETag /
//!   Last-Modified；弱 ETag `W/` 按 RFC 9110 §13.1.5 不能用于 `If-Range`）。
//! * Probe 2：同一 URL `Range: bytes=<N/2>-` + `If-Range: <校验器>` —— 期望 206，
//!   且 `Content-Range` 起点等于请求偏移。
//! * Probe 3：重新调用 `get_download_info` 拿**新签名 URL**，带旧校验器发同样的
//!   Range 请求 —— 决定续传能否跨重试使用，还是只能用在单次下载尝试内。
//!
//! 只打印非敏感信息（曲目 id、音质标签、字节数、状态码、头部、CDN 主机名），
//! 绝不打印 URL 查询串（携带签名 token）、Cookie 或签名器凭证。
//!
//! 这是**诊断工具，不是测试**：它读本机已登录的配置并打真实网络，所以任何测试都
//! 不得依赖它；CI 只会随 `--all-targets` 编译它，不会运行它。
//!
//! 运行（直连、无代理）：
//! ```text
//! cargo build -p sodam-core --example range_probe
//! env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u http_proxy -u https_proxy \
//!     -u all_proxy NO_PROXY='*' cargo run -p sodam-core --example range_probe
//! ```

use anyhow::Result;
use sodam_core::{Session, Settings};
use std::collections::BTreeMap;
use std::net::ToSocketAddrs;
use std::time::Duration;

/// 一次 HEAD/GET 之后能拿到的全部非敏感头部。
#[derive(Default)]
struct Headers {
    status: u16,
    content_range: Option<String>,
    accept_ranges: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
    content_length: Option<String>,
}

impl Headers {
    /// 可用于 `If-Range` 的校验器：强 ETag 优先，其次 Last-Modified，都没有则无。
    fn validator(&self) -> Option<(&'static str, String)> {
        if let Some(etag) = &self.etag {
            let weak = etag.starts_with("W/") || etag.starts_with("w/");
            if !weak {
                return Some(("If-Range(strong ETag)", etag.clone()));
            }
        }
        self.last_modified
            .as_ref()
            .map(|v| ("If-Range(Last-Modified)", v.clone()))
    }

    fn print(&self) {
        println!("  status         = {}", self.status);
        println!("  Content-Range  = {}", opt(&self.content_range));
        println!("  Accept-Ranges  = {}", opt(&self.accept_ranges));
        println!("  ETag           = {}", opt(&self.etag));
        println!("  Last-Modified  = {}", opt(&self.last_modified));
        println!("  Content-Length = {}", opt(&self.content_length));
        match self.validator() {
            Some((kind, _)) => println!("  validator      = {kind}"),
            None => println!("  validator      = none"),
        }
    }
}

fn opt(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "<absent>".to_string())
}

/// `bytes <start>-<end>/<total>` 的解析结果。
fn parse_content_range(value: Option<&String>) -> Option<(u64, u64, Option<u64>)> {
    let raw = value?.trim().strip_prefix("bytes ")?.trim();
    let (span, total) = raw.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let total = if total.trim() == "*" {
        None
    } else {
        total.trim().parse().ok()
    };
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?, total))
}

fn url_host(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or(url);
    after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string()
}

fn url_path(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or(url);
    let rest = after_scheme.split(['?', '#']).next().unwrap_or_default();
    rest.split_once('/')
        .map(|(_, path)| path)
        .unwrap_or_default()
        .to_string()
}

/// 只用于本地日志脱敏：绝不允许查询串进入输出。
fn redact(text: &str) -> String {
    match text.split_once('?') {
        Some((head, _)) => format!("{head}?"),
        None => text.to_string(),
    }
}

/// 发一次带 `Range` 的 GET，只读头部、不读响应体（`call()` 在头部就绪时返回）。
/// ureq 2.x 中非 2xx 走 `Err(Error::Status(code, response))`——416/200 都是数据。
fn range_get(url: &str, range: &str, if_range: Option<&str>) -> Result<Headers> {
    let mut req = ureq::get(url)
        .set("Range", range)
        .set("Accept-Encoding", "identity")
        .timeout(Duration::from_secs(20));
    if let Some(validator) = if_range {
        req = req.set("If-Range", validator);
    }
    let (status, response) = match req.call() {
        Ok(response) => (response.status(), response),
        Err(ureq::Error::Status(code, response)) => (code, response),
        Err(other) => anyhow::bail!("请求失败: {}", redact(&other.to_string())),
    };
    let header = |name: &str| response.header(name).map(|v| v.to_string());
    let mut result = Headers {
        status,
        content_range: header("Content-Range"),
        accept_ranges: header("Accept-Ranges"),
        etag: header("ETag"),
        last_modified: header("Last-Modified"),
        content_length: header("Content-Length"),
    };
    if result.status == 0 {
        result.status = response.status();
    }
    // 显式丢弃响应体：不下载、不解密、不落盘。
    drop(response);
    Ok(result)
}

fn song_for_track_id(id: &str) -> libresoda::Song {
    // 与 session.rs::download_to_cache 构造一致：libresoda 只依赖
    // extra.track_id / song.id 解析整曲流，元数据字段留空不影响。
    libresoda::Song {
        id: id.to_string(),
        source: libresoda::model::SOURCE_SODA.to_string(),
        link: format!("https://www.qishui.com/track/{id}"),
        extra: BTreeMap::from([("track_id".to_string(), id.to_string())]),
        ..Default::default()
    }
}

/// 从音频缓存的 sidecar 找候选曲目：文件名前缀是 track id，内容第一列是音质标签。
/// 优先返回标签不含「试听」的（即曾拿到整曲流的）。
fn cached_full_candidates() -> Vec<(String, String)> {
    let dir = sodam_core::audio::cache_dir();
    let mut candidates: Vec<((String, String), u64)> = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("无法读取缓存目录 {err}");
            return Vec::new();
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".quality") else {
            continue;
        };
        let Some((track_id, _)) = stem.split_once('-') else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let label = content.split('\t').next().unwrap_or_default().to_string();
        if label.contains("试听") {
            continue;
        }
        let size: u64 = content
            .split('\t')
            .nth(1)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        candidates.push(((track_id.to_string(), label), size));
    }
    // 大文件优先——它们才是续传真正要救的对象。
    candidates.sort_by_key(|(_, size)| std::cmp::Reverse(*size));
    candidates.into_iter().map(|(pair, _)| pair).collect()
}

/// 兜底：像 track_raw.rs 那样从推荐流里取曲目 id。
fn feed_candidates(soda: &libresoda::soda::Soda) -> Vec<(String, String)> {
    let mut body = serde_json::json!({
        "played_media": [],
        "is_first_request": true,
        "is_did_first_request": false,
        "feed_counts": { "mix_session_count": 1 }
    });
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        body["did_first_use_time"] = serde_json::json!(now.as_secs());
    }
    let value = match soda.fetch_feed_song_tab(&body) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("推荐流获取失败: {}", redact(&err.to_string()));
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    if let Some(items) = value.get("items").and_then(|v| v.as_array()) {
        for item in items {
            let track = item
                .pointer("/entity/track_wrapper/track")
                .or_else(|| item.pointer("/entity/track"));
            let Some(track) = track else { continue };
            let id = track
                .get("id_str")
                .or_else(|| track.get("id"))
                .and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                });
            if let Some(id) = id.filter(|id| !id.trim().is_empty()) {
                out.push((id, "feed".to_string()));
            }
        }
    }
    out
}

fn main() -> Result<()> {
    let session = Session::new(Settings::load().merged_with_env());
    let soda = session.soda();

    // ---- 选曲：要 !is_preview 且 url 非空的整曲流 ----
    let mut chosen: Option<(String, libresoda::soda::types::DownloadInfo)> = None;
    let mut tried = 0usize;
    let mut consider = |id: String,
                        label: String,
                        source: &str|
     -> Option<(String, libresoda::soda::types::DownloadInfo)> {
        tried += 1;
        let song = song_for_track_id(&id);
        match libresoda::soda::download::get_download_info(soda, &song) {
            Ok(info) => {
                let ok = !info.is_preview && !info.url.trim().is_empty();
                println!(
                    "[{source}] track {id} ({label}): is_preview={} url_empty={} size={} -> {}",
                    info.is_preview,
                    info.url.trim().is_empty(),
                    info.size,
                    if ok { "ACCEPTED" } else { "rejected" }
                );
                if ok {
                    return Some((id, info));
                }
            }
            Err(err) => println!(
                "[{source}] track {id} ({label}): get_download_info 失败: {}",
                redact(&err.to_string())
            ),
        }
        None
    };

    for (id, label) in cached_full_candidates().into_iter().take(5) {
        if let Some(found) = consider(id, label, "cache") {
            chosen = Some(found);
            break;
        }
    }
    if chosen.is_none() {
        println!("缓存候选里没找到整曲流，退回推荐流取样…");
        for (id, label) in feed_candidates(soda).into_iter().take(5) {
            if let Some(found) = consider(id, label, "feed") {
                chosen = Some(found);
                break;
            }
        }
    }
    let (track_id, info) =
        chosen.ok_or_else(|| anyhow::anyhow!("没有拿到任何非试听整曲流，探针终止"))?;

    println!();
    println!("== 选中 track {} ==", track_id);
    println!(
        "  format={} bitrate={} duration={:.1}s size={} play_auth_present={} is_preview={}",
        info.format,
        info.bitrate,
        info.duration,
        info.size,
        !info.play_auth.trim().is_empty(),
        info.is_preview,
    );
    println!("  url#1 host = {}", url_host(&info.url));

    // ---- DNS：确认连接目标是真实公网 IP ----
    let host = url_host(&info.url);
    let port = if info.url.starts_with("http://") {
        80
    } else {
        443
    };
    match (host.as_str(), port).to_socket_addrs() {
        Ok(addrs) => {
            let list: Vec<String> = addrs.map(|a| a.to_string()).collect();
            println!("  DNS {host}:{port} -> {}", list.join(", "));
        }
        Err(err) => println!("  DNS {host}:{port} 解析失败: {err}"),
    }

    // ---- Probe 1：能力 + 总长 + 校验器 ----
    println!("\n== Probe 1: GET Range bytes=0-0 (Accept-Encoding: identity) ==");
    let probe1 = range_get(&info.url, "bytes=0-0", None)?;
    probe1.print();
    let cr1 = parse_content_range(probe1.content_range.as_ref());
    if let Some((_, _, Some(total))) = cr1 {
        println!(
            "  total {total} vs info.size {} -> {}",
            info.size,
            if total == info.size as u64 {
                "MATCH"
            } else {
                "MISMATCH"
            }
        );
    }
    let validator = probe1.validator();
    let offset = cr1
        .and_then(|(_, _, total)| total)
        .or(if info.size > 0 {
            Some(info.size as u64)
        } else {
            None
        })
        .map(|n| n / 2)
        .unwrap_or(1_000_000);

    // ---- Probe 2：同 URL 续传（带校验器）----
    println!("\n== Probe 2: GET Range bytes={offset}- + If-Range, SAME url ==");
    let range2 = format!("bytes={offset}-");
    let probe2 = range_get(
        &info.url,
        &range2,
        validator.as_ref().map(|(_, v)| v.as_str()),
    )?;
    probe2.print();
    match parse_content_range(probe2.content_range.as_ref()) {
        Some((start, _, total)) => {
            println!(
                "  resume start={start} (requested {offset}) -> {} ; declared total = {:?}",
                if start == offset { "OK" } else { "MISMATCH" },
                total,
            );
        }
        None => println!("  没有 Content-Range（若 status=200 说明 Range 被忽略）"),
    }

    // ---- Probe 3：换发新签名 URL 后，旧校验器 + Range 还认吗 ----
    println!("\n== Probe 3: 重新 get_download_info 拿新 URL，再发同一 Range + 旧 If-Range ==");
    let song = song_for_track_id(&track_id);
    let info2 = libresoda::soda::download::get_download_info(soda, &song)
        .map_err(|err| anyhow::anyhow!("重新取流失败: {}", redact(&err.to_string())))?;
    println!("  url#2 host = {}", url_host(&info2.url));
    println!(
        "  host(url#1)==host(url#2): {}",
        url_host(&info.url) == url_host(&info2.url)
    );
    println!(
        "  path(url#1)==path(url#2): {}",
        url_path(&info.url) == url_path(&info2.url)
    );
    println!(
        "  url#1 != url#2 (query 必然轮换): {}",
        info.url != info2.url
    );
    let probe3 = range_get(
        &info2.url,
        &range2,
        validator.as_ref().map(|(_, v)| v.as_str()),
    )?;
    probe3.print();
    match parse_content_range(probe3.content_range.as_ref()) {
        Some((start, _, total)) => {
            println!("  new-url resume: start={start} (requested {offset}), total={total:?}")
        }
        None => println!("  新 URL 上 Range 未被满足（status {}）", probe3.status),
    }

    println!("\n探针完成。判读要点：");
    println!("  Probe1 206 + Probe2 206 起点一致 => CDN 支持 Range；");
    println!("  Probe3 206 => 续传可跨「重新签名」；Probe3 200 => 只能单次尝试内续传；");
    println!("  任一 Probe 返回 200 => Range 被忽略，不要建续传状态机。");
    Ok(())
}
