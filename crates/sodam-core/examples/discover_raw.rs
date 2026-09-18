use sodam_core::{Session, Settings};
fn main() -> anyhow::Result<()> {
    let s = Session::new(Settings::load().merged_with_env());
    let v = s.soda().fetch_discover()?;
    println!("cards={:?}", sodam_core::library::scene_discover_cards(&s)?);
    for (bi, b) in v
        .get("blocks")
        .and_then(|x| x.as_array())
        .unwrap_or(&vec![])
        .iter()
        .enumerate()
    {
        println!(
            "B {bi} type={:?} items={:?}",
            b.get("type"),
            b.get("inner_block")
                .and_then(|x| x.as_array())
                .map(|x| x.len())
        );
        if let Some(items) = b.get("inner_block").and_then(|x| x.as_array()) {
            if let Some(i) = items.first() {
                println!(
                    "keys={:?}",
                    i.as_object().map(|x| x.keys().collect::<Vec<_>>())
                );
                println!(
                    "id={:?} entitykeys={:?}",
                    i.get("inner_block_id"),
                    i.get("entity")
                        .and_then(|x| x.as_object())
                        .map(|x| x.keys().collect::<Vec<_>>())
                );
            }
        }
    }
    Ok(())
}
