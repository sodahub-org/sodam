//! 验证登录态与自动音质：读账号信息（昵称 / VIP）并按权益给出音质档位。
//!
//! `cargo run -p sodam-core --example account_probe`

use sodam_core::{Session, Settings};

fn main() {
    let settings = Settings::load().merged_with_env();
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
        if settings.quality.trim().is_empty() {
            "auto"
        } else {
            settings.quality.as_str()
        }
    );
    let mut session = Session::new(settings);
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
            println!(
                "按权益应选音质：{}（非 VIP 时才是 highest）",
                Session::auto_quality_for(account.vip)
            );
            match session.refresh_quality(account.vip) {
                Ok(Some(quality)) => println!("已写入音质偏好：{quality}"),
                Ok(None) => println!("用户已手动指定音质，保持不动"),
                Err(err) => println!("写入音质偏好失败：{err}"),
            }
        }
        Err(err) => println!("读取账号失败：{err}"),
    }
}
