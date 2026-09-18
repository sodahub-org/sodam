//! 无界面扫码登录（带日志）：适合排查 GUI 里的时序问题。
//!
//! 用法：`cargo run -p sodam-core --example login_probe`
//! 流程：创建二维码 → 把扫码地址写到 /tmp/sodam-login-url.txt → 每 3 秒轮询并打印状态
//!      → 成功后把 Cookie 写进 ~/.config/sodam/config.json。

use sodam_core::{Session, Settings};
use std::time::Duration;

/// 轮询间隔：官方限流很敏感（每 3 秒会触发 error_code=7，确认握手会丢），
/// 今天手工验证成功那次是 6 秒一次。
const POLL_SECS: u64 = 6;

fn main() {
    let mut settings = Settings::load().merged_with_env();
    // 对照开关：`SODAM_NO_SIGNER=1` 时登录请求不带应用级签名
    // （今天实测过：不带签名的扫码流程能走完；签名绑定的设备号与二维码会话的设备号不同套）
    if std::env::var("SODAM_NO_SIGNER").ok().as_deref() == Some("1") {
        println!("（本次登录不带应用级签名）");
        settings.signer_url = String::new();
        settings.signer_token = String::new();
    }
    let session = Session::new(settings.clone());

    let created = match session.create_qr_login() {
        Ok(created) => created,
        Err(err) => {
            eprintln!("创建二维码失败: {err}");
            std::process::exit(1);
        }
    };
    println!("token       = {}", created.token);
    println!("scan_url    = {}", created.scan_url);
    let _ = std::fs::write("/tmp/sodam-login-url.txt", &created.scan_url);
    println!("扫码地址已写入 /tmp/sodam-login-url.txt");

    // 只建会话不轮询（用来统计服务端下发的 token 形态）
    if std::env::var("SODAM_CREATE_ONLY").ok().as_deref() == Some("1") {
        return;
    }

    for attempt in 1..=90 {
        std::thread::sleep(Duration::from_secs(POLL_SECS));
        // 直接用 libresoda 结果，方便打印服务端原始状态（status_code / error_code）
        match session.soda().check_qr(&created.token) {
            Ok(result) => {
                let extra: Vec<String> = result
                    .extra
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                println!(
                    "#{attempt:<3} status={:?} message={} cookie={} extra=[{}]",
                    result.status,
                    result.message,
                    if result.cookie.trim().is_empty() {
                        "无"
                    } else {
                        "有"
                    },
                    extra.join(", ")
                );
                if matches!(result.status, libresoda::model::QRLoginStatus::Success)
                    && !result.cookie.trim().is_empty()
                {
                    let mut updated = settings.clone();
                    updated.cookie = result.cookie.trim().to_string();
                    match updated.save() {
                        Ok(()) => println!("★ 登录成功，Cookie 已写入 ~/.config/sodam/config.json"),
                        Err(err) => println!("★ 登录成功，但保存配置失败: {err}"),
                    }
                    return;
                }
                if matches!(
                    result.status,
                    libresoda::model::QRLoginStatus::Expired
                        | libresoda::model::QRLoginStatus::Failed
                ) {
                    println!("二维码已失效（{:?}），请重新运行本程序", result.status);
                    std::process::exit(2);
                }
                if matches!(result.status, libresoda::model::QRLoginStatus::Waiting)
                    && attempt % 10 == 0
                {
                    println!("    （已等待 {} 秒，仍在等待扫码）", attempt * 3);
                }
            }
            Err(err) => {
                println!("#{attempt:<3} 轮询失败: {err}");
                // 会话过期（libresoda 3 分钟 TTL）就退出，让用户重跑
                if err.to_string().contains("过期") {
                    std::process::exit(2);
                }
            }
        }
    }
    println!("超时退出（未检测到扫码确认）");
}
