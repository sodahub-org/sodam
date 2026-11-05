fn main() {
    let path = std::env::args().nth(1).expect("installer path");
    match sodam_core::signer::extract_vendor_from_installer(std::path::Path::new(&path)) {
        Ok(()) => println!("ok: {:?}", sodam_core::signer::detect()),
        Err(err) => println!("failed: {err}"),
    }
}
