//! 验证音频管线（不播放声音）：下载一首歌 → libresoda 解密 → rodio 解码。
//!
//! `SODA_COOKIE=... QISHUI_SIGNER_URL=... QISHUI_SIGNER_TOKEN=... \
//!  cargo run -p sodam-core --example decode_probe -- <track_id>`

use sodam_core::models::TrackItem;
use sodam_core::{Session, Settings};

fn main() {
    let track_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "7304719759323564095".to_string());
    let settings = Settings::load().merged_with_env();
    println!(
        "signer={} cookie={} quality={}",
        if settings.signer_url.is_empty() {
            "(空)"
        } else {
            "(已配置)"
        },
        if settings.cookie.is_empty() {
            "(空)"
        } else {
            "(已配置)"
        },
        if settings.quality.is_empty() {
            "auto"
        } else {
            settings.quality.as_str()
        }
    );

    let session = Session::new(settings);
    let track = TrackItem {
        id: track_id.clone(),
        title: format!("track-{track_id}"),
        ..Default::default()
    };

    let cached = match session.download_to_cache(&track) {
        Ok(cached) => cached,
        Err(err) => {
            println!("下载失败: {err}");
            std::process::exit(1);
        }
    };
    let cached_quality = cached.quality.clone();
    let path = cached.path;
    let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    println!(
        "已缓存: {} ({} 字节) 实际音质={}",
        path.display(),
        size,
        cached_quality
    );

    // 只解码不输出：确认 rodio 能读这个（已解密的）文件
    let file = std::fs::File::open(&path).expect("打开缓存文件");
    match rodio::Decoder::new(std::io::BufReader::new(file)) {
        Ok(decoder) => {
            use rodio::Source as _;
            println!(
                "解码通过: 采样率={}Hz 声道={} 时长={:?}",
                decoder.sample_rate(),
                decoder.channels(),
                decoder.total_duration()
            );
        }
        Err(err) => {
            println!("解码失败: {err}");
            std::process::exit(1);
        }
    }
}
