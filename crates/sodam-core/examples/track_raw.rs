use sodam_core::{Session, Settings};
fn main() -> anyhow::Result<()> {
    let s = Session::new(Settings::load().merged_with_env());
    let mut body = serde_json::json!({"played_media":[],"is_first_request":true,"is_did_first_request":false,"feed_counts":{"mix_session_count":1}});
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        body["did_first_use_time"] = serde_json::json!(now.as_secs());
    }
    let value = s.soda().fetch_feed_song_tab(&body)?;
    let item = value
        .pointer("/items/0")
        .ok_or_else(|| anyhow::anyhow!("no item"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(item.pointer("/entity/track_wrapper/track").unwrap())?
    );
    Ok(())
}
