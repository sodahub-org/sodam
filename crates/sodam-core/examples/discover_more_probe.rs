use sodam_core::{Session, Settings};
fn ids(v: &serde_json::Value) -> Vec<String> {
    v.get("inner_block")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.get("inner_block_id")
                        .and_then(|x| x.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}
fn main() -> anyhow::Result<()> {
    let s = Session::new(Settings::load().merged_with_env());
    for block_type in ["discover_playlist_mix", "discover_feed_playlist"] {
        let body = serde_json::json!({"block_type":block_type,"sub_channel_id":-2,"exposure_radio_list":[]});
        let first = s.soda().fetch_discover_mix_body(&body)?;
        let a = ids(&first);
        println!(
            "{block_type}: has_more={:?} n={} ids={:?}",
            first.get("has_more"),
            a.len(),
            a
        );
        if a.is_empty() {
            continue;
        }
        let body2 = serde_json::json!({"block_type":block_type,"sub_channel_id":-2,"exposure_radio_list":a});
        let second = s.soda().fetch_discover_mix_body(&body2)?;
        let b = ids(&second);
        println!(
            "  again: has_more={:?} n={} ids={:?}",
            second.get("has_more"),
            b.len(),
            b
        );
        println!("  overlap={}", a.iter().filter(|x| b.contains(x)).count());
    }
    Ok(())
}
