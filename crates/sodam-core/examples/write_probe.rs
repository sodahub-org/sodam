use sodam_core::{library, session::Session};

fn main() {
    let settings = sodam_core::config::Settings::load();
    let session = Session::new(settings);
    let track = "6696534426169378817";
    match library::list_comments(&session, track, "0", 1) {
        Ok(page) => {
            if let Some(comment) = page.items.first() {
                println!(
                    "like: {:?}",
                    library::like_comment(&session, track, &comment.id, true)
                );
                std::thread::sleep(std::time::Duration::from_millis(500));
                println!(
                    "unlike: {:?}",
                    library::like_comment(&session, track, &comment.id, false)
                );
            } else {
                println!("no comments");
            }
        }
        Err(err) => println!("list failed: {err}"),
    }
}
