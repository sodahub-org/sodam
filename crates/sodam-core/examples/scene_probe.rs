use sodam_core::{Session, Settings};
fn main() -> anyhow::Result<()> {
    let s = Session::new(Settings::load().merged_with_env());
    let cards = sodam_core::library::scene_discover_cards(&s)?;
    println!("cards={}", cards.len());
    for c in cards.iter().take(5) {
        println!(
            "{} | {} | {} | {}",
            c.inner_block_id, c.playlist.id, c.playlist.title, c.playlist.track_count
        );
    }
    println!("modes={}", sodam_core::library::scene_modes(&s)?.len());
    Ok(())
}
