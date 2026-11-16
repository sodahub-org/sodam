//! 设置页贡献者墙：直接复用 contrib.rocks 的仓库组合图。

use std::io::Read;

const CONTRIBUTORS_URL: &str = "https://contrib.rocks/image?repo=sodahub-org/sodam&columns=8";
const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;

fn is_svg(bytes: &[u8]) -> bool {
    let body = bytes.trim_ascii_start();
    body.starts_with(b"<svg") || body.starts_with(b"<?xml")
}

/// 是否发起一次贡献者图拉取（#135 C 的反死循环决策）。
///
/// 纯函数、无副作用，UI 层与单测共用。设置页每次重绘都会问一次，
/// 所以「失败后默认停手」是关键：否则失败 → `cx.notify()` → 重绘 → 再拉取，
/// 会形成无限请求循环，同时让失败态一闪而过、永远显示不出来。
///
/// * `has_image`：已缓存图片，不再拉；
/// * `fetching`：正在拉，不重复发起；
/// * `failed`：上次拉取失败；
/// * `force`：用户显式点击「重试」，此时允许越过 `failed` 再拉一次。
pub fn should_fetch_contributors(
    has_image: bool,
    fetching: bool,
    failed: bool,
    force: bool,
) -> bool {
    if has_image || fetching {
        return false;
    }
    !failed || force
}

/// 下载 contrib.rocks 动态生成的 SVG；返回原始矢量字节。
pub fn fetch_contributors_svg() -> Option<Vec<u8>> {
    let response = ureq::get(CONTRIBUTORS_URL)
        .timeout(std::time::Duration::from_secs(12))
        .call()
        .ok()?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_IMAGE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    is_svg(&bytes).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_svg_documents_only() {
        let svg = b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        let direct_svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        assert!(is_svg(svg));
        assert!(is_svg(direct_svg));
        assert!(!is_svg(b"<html>not an image</html>"));
    }

    /// 首次进设置页：无图、未在拉、未失败 —— 应当拉取一次。
    #[test]
    fn first_visit_fetches_once() {
        assert!(should_fetch_contributors(false, false, false, false));
    }

    /// 已缓存图片：再多次重绘也不重复拉取。
    #[test]
    fn cached_image_never_refetches() {
        assert!(!should_fetch_contributors(true, false, false, false));
        assert!(!should_fetch_contributors(true, false, true, true));
    }

    /// 拉取进行中：重绘不得再发起第二个请求。
    #[test]
    fn in_flight_never_refetches() {
        assert!(!should_fetch_contributors(false, true, false, false));
        assert!(!should_fetch_contributors(false, true, true, true));
    }

    /// 反死循环核心：失败后，渲染路径的自动询问必须停手。
    /// 否则「失败 → notify → 重绘 → 再拉取」无限循环。
    #[test]
    fn failure_without_force_stays_idle() {
        assert!(!should_fetch_contributors(false, false, true, false));
    }

    /// 用户点击「重试」：force 越过失败态，允许再拉一次。
    #[test]
    fn failure_with_force_retries() {
        assert!(should_fetch_contributors(false, false, true, true));
    }

    /// force 只越过 failed，不越过「已有图」或「正在拉」。
    #[test]
    fn force_does_not_override_cache_or_inflight() {
        assert!(!should_fetch_contributors(true, false, false, true));
        assert!(!should_fetch_contributors(false, true, false, true));
    }
}
