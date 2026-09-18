use sodam_core::{Session, Settings};
fn main() -> anyhow::Result<()> {
    let s = Session::new(Settings::load().merged_with_env());
    let v = s.soda().fetch_discover()?;
    println!(
        "keys={:?}",
        v.as_object().map(|x| x.keys().collect::<Vec<_>>())
    );
    println!(
        "blocks={:?}",
        v.get("blocks").map(|x| x.as_array().map(|a| a.len()))
    );
    if let Some(blocks) = v.get("blocks").and_then(|x| x.as_array()) {
        for b in blocks.iter().take(10) {
            println!(
                "block type={:?} keys={:?} inner={:?}",
                b.get("type"),
                b.as_object().map(|x| x.keys().collect::<Vec<_>>()),
                b.get("inner_block")
                    .and_then(|x| x.as_array().map(|a| a.len()))
            );
        }
    }
    let m = s.soda().feed_mode()?;
    println!("feed blocks={}", m.feed_mode_block.len());
    for (bi, b) in m.feed_mode_block.iter().enumerate().take(4) {
        println!(
            "feed block {bi} title={:?} type={:?} n={}",
            b.title,
            b.block_type,
            b.feed_mode.len()
        );
        for item in b.feed_mode.iter().take(8) {
            println!(
                "  {:?} id={} q={} cover={:?}",
                item.text,
                item.entity.feed_scene_mode.scene_mode_id,
                item.entity.feed_scene_mode.sub_queue_type,
                item.url_info
            );
        }
    }
    Ok(())
}
