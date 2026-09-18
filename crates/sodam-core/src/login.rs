//! 扫码登录：把 libresoda 的二维码会话转成 UI 能直接画的「点阵」。

/// 二维码点阵：`matrix[y][x] == true` 表示该格是黑块。
pub type QrMatrix = Vec<Vec<bool>>;

/// 扫码状态（把 libresoda 的状态机简化成 UI 直接可用的枚举）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoginStatus {
    /// 等待扫码
    #[default]
    Waiting,
    /// 已扫码，等手机确认（或需要二次验证）
    Scanned,
    /// 成功，`cookie` 里带回会话
    Success,
    /// 二维码过期，需要重新获取
    Expired,
    /// 失败
    Failed,
}

/// 一次轮询的结果。
#[derive(Debug, Clone, Default)]
pub struct LoginProgress {
    pub status: LoginStatus,
    pub message: String,
    pub cookie: String,
    /// 服务端要求二次验证（MFA）——当前 libresoda 只识别不闭环，见其 docs/QR-LOGIN.md
    pub need_second_verify: bool,
    /// 服务端限流（`error_code=7`）：调用方应拉长轮询间隔，否则确认握手会被丢掉
    pub rate_limited: bool,
}

impl LoginProgress {
    pub fn is_finished(&self) -> bool {
        self.status == LoginStatus::Success && !self.cookie.trim().is_empty()
    }

    pub fn is_terminal_failure(&self) -> bool {
        matches!(self.status, LoginStatus::Expired | LoginStatus::Failed)
    }
}

/// 把扫码地址编码成二维码点阵（纯计算，不渲染）。
pub fn qr_matrix(text: &str) -> anyhow::Result<QrMatrix> {
    if text.trim().is_empty() {
        anyhow::bail!("二维码内容为空");
    }
    let code = qrcode::QrCode::new(text.as_bytes())
        .map_err(|err| anyhow::anyhow!("二维码编码失败: {err}"))?;
    let width = code.width();
    let colors = code.to_colors();
    Ok((0..width)
        .map(|y| {
            (0..width)
                .map(|x| colors[y * width + x] == qrcode::Color::Dark)
                .collect()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_is_square_and_non_trivial() {
        let matrix =
            qr_matrix("https://bff-pc.qishui.com/light/invoke/scan_login?token=abc_lq&os=Windows")
                .expect("qr");
        assert!(
            matrix.len() >= 21,
            "二维码尺寸应有 21 以上: {}",
            matrix.len()
        );
        assert!(
            matrix.iter().all(|row| row.len() == matrix.len()),
            "应是正方形"
        );
        let dark = matrix.iter().flatten().filter(|cell| **cell).count();
        assert!(dark > 0, "应该有点亮的模块");
    }

    #[test]
    fn empty_text_is_rejected() {
        assert!(qr_matrix("   ").is_err());
    }
}
