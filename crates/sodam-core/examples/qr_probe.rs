//! 验证：带默认签名服务的扫码登录能否创建二维码 / 轮询状态。
//!
//! `cargo run -p sodam-core --example qr_probe`

use sodam_core::{Session, Settings};

fn main() {
    let settings = Settings::load().merged_with_env();
    println!(
        "signer={} token={} cookie={}",
        settings.signer_url,
        if settings.signer_token.is_empty() {
            "(空)"
        } else {
            "(已设置)"
        },
        if settings.cookie.is_empty() {
            "(空)"
        } else {
            "(已设置)"
        }
    );
    let session = Session::new(settings);

    match session.create_qr_login() {
        Ok(created) => {
            println!("token={}", created.token);
            println!("scan_url={}", created.scan_url);
            match session.check_qr_login(&created.token) {
                Ok(result) => println!("首次轮询: {:?} {}", result.status, result.message),
                Err(err) => println!("轮询失败: {err}"),
            }
        }
        Err(err) => println!("创建二维码失败: {err}"),
    }
}
