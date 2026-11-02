fn main() {
    match sodam_core::signer::start_service(None) {
        Ok(()) => println!("spawned"),
        Err(err) => {
            println!("start failed: {err}");
            return;
        }
    }
    for _ in 0..90 {
        if sodam_core::signer::is_ready() {
            println!("ready");
            sodam_core::signer::stop_service();
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    println!("not ready");
    println!("--- log ---");
    if let Ok(text) = std::fs::read_to_string(sodam_core::signer::log_path()) {
        for line in text.lines().rev().take(20).collect::<Vec<_>>().iter().rev() {
            println!("{line}");
        }
    }
    sodam_core::signer::stop_service();
}
