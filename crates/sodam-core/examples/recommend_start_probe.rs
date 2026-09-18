use sodam_core::{Session, Settings};
fn main() -> anyhow::Result<()> {
    let s = Session::new(Settings::load().merged_with_env());
    let start = sodam_core::library::recommended_start(&s)?;
    println!(
        "tracks={} has_more={} counter={}",
        start.tracks.len(),
        start.source.has_more,
        start.source.fetch_counter
    );
    for t in start.tracks {
        println!("{} - {}", t.title, t.artist);
    }
    Ok(())
}
