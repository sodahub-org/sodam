fn main() {
    println!("before: {:?}", sodam_core::signer::detect());
    if let Err(err) = sodam_core::signer::install_node() {
        println!("node download failed: {err}");
        return;
    }
    if let Err(err) = sodam_core::signer::install_libmssdk() {
        println!("libmssdk download failed: {err}");
        return;
    }
    println!("after: {:?}", sodam_core::signer::detect());
}
