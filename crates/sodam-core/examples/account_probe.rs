//! 验证登录态与音质意图：读账号信息（昵称 / VIP），并如实打印配置里的意图。
//!
//! 音质不再按会员等级推导——「自动」就是不设上限，由服务端收敛。
//!
//! `cargo run -p sodam-core --example account_probe`

use sodam_core::{Session, Settings};

fn main() {
    let settings = Settings::load().merged_with_env();
    let intent = settings.quality_intent();
    println!(
        "配置：cookie={} signer={} quality={}",
        if settings.cookie.trim().is_empty() {
            "(空)"
        } else {
            "(已设置)"
        },
        if settings.signer_url.trim().is_empty() {
            "(空)"
        } else {
            "(已设置)"
        },
        intent.as_setting()
    );
    let session = Session::new(settings);
    match session.fetch_account() {
        Ok(account) => {
            println!(
                "账号：昵称={} user_id={} VIP={}",
                if account.nickname.is_empty() {
                    "(未返回)"
                } else {
                    account.nickname.as_str()
                },
                account.user_id,
                account.vip
            );
            println!("音质意图保持为 {}（不按 VIP 改写）", intent.as_setting());
        }
        Err(err) => println!("读取账号失败：{err}"),
    }
}
