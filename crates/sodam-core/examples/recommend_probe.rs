//! 推荐队列接口探针（只打印 id/类型/has_more，不输出 Cookie/签名）。

use sodam_core::{Session, Settings};

fn main() -> anyhow::Result<()> {
    let session = Session::new(Settings::load().merged_with_env());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let body = serde_json::json!({
        "played_media": [],
        "did_first_use_time": now,
        "is_first_request": true,
        "is_did_first_request": false,
        "feed_counts": { "mix_session_count": 1 },
    });
    let value = session.soda().fetch_feed_song_tab(&body)?;
    println!(
        "keys: {:?}",
        value
            .as_object()
            .map(|value| value.keys().collect::<Vec<_>>())
    );
    println!("has_more: {:?}", value.get("has_more"));
    println!("status_code: {:?}", value.get("status_code"));
    let items = value.get("items").and_then(serde_json::Value::as_array);
    println!("items: {}", items.map(Vec::len).unwrap_or(0));
    if let Some(items) = items {
        for item in items.iter().take(5) {
            println!(
                "item type={:?} id={:?} name={:?}",
                item.get("type"),
                item.pointer("/entity/track_wrapper/track/id"),
                item.pointer("/entity/track_wrapper/track/name"),
            );
        }
    }
    Ok(())
}
