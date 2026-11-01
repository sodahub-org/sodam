//! 设置页贡献者墙：直接复用 contrib.rocks 的仓库组合图。

use std::io::Read;

const CONTRIBUTORS_URL: &str = "https://contrib.rocks/image?repo=sodahub-org/sodam&columns=8";
const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;

fn is_svg(bytes: &[u8]) -> bool {
    let body = bytes.trim_ascii_start();
    body.starts_with(b"<svg") || body.starts_with(b"<?xml")
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
}
