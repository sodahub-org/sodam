//! 预取管线在全局故障期间的挂起与恢复。
//!
//! 预取存在的意义是把「切歌」从网络操作变成本地操作：它约每 5 秒巡检一次队列，
//! 提前下好接下来几首。但全局故障（登录过期、签名服务不可用、磁盘写满、缺 Cookie）
//! 期间，换一首都不会变好，巡检只会一轮轮重打那台**全体用户共享**的签名服务。
//! 所以这里给整条管线加一道闸：故障出现时挂起，恢复信号按故障类分。
//!
//! 恢复信号（票 #112 的成文约定）：
//! * 登录类（登录过期 / 缺 Cookie）—— 重新登录成功为事件；
//! * 磁盘类（本地缓存写不进去）—— 下一次用户主动播放为事件；
//! * 其余全局故障（签名服务不可用）—— 60 秒兜底冷却。
//!
//! 60 秒是**拍的**：各来源只给量级，没有可直接抄的推荐值。它不改变重试策略，
//! 只是让无人值守的管线有一次自愈机会；恢复后的第一次预取若再失败，会重新挂起。
//!
//! 冷却不自己在后台计时：调用方在预取巡检时询问 [`PrefetchGate::expire_cooldown`]。
//! 落闸期间这条巡检不执行，也就不问冷却；实际恢复会顺延到解除落闸后的第一次巡检——
//! 这是调用方的前提，不是闸门本身的隐藏行为。

use crate::failure::{PlaybackFailure, RecoveryAction};
use std::time::{Duration, Instant};

/// 挂起原因，同时也是恢复信号的分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspensionClass {
    /// 登录过期 / 缺 Cookie：等重新登录成功。
    Login,
    /// 本地缓存写不进去：等用户下一次主动播放。
    Disk,
    /// 签名服务不可用：等 60 秒兜底冷却。
    Cooldown,
}

impl SuspensionClass {
    /// 日志字段 `class=` / `reason=` 的稳定取值（小写下划线，不随界面语言变化）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Disk => "disk",
            Self::Cooldown => "cooldown",
        }
    }
}

/// 预取管线的总闸：`allows() == false` 时「一首都不排」。
///
/// 与逐曲失败表正交：那张表回答「这一首要不要再试」，这道闸回答
/// 「整条管线现在该不该跑」。挂起对用户静默，只在日志里留痕。
#[derive(Debug, Clone, Copy, Default)]
pub struct PrefetchGate {
    suspended: Option<(SuspensionClass, Instant)>,
}

impl PrefetchGate {
    /// 签名服务这类「没人能告诉我们什么时候好了」的故障，兜底冷却。
    pub const COOLDOWN: Duration = Duration::from_secs(60);

    /// 现在能不能排新的预取。
    pub fn allows(&self) -> bool {
        self.suspended.is_none()
    }

    /// 当前挂起原因；`None` 表示管线在跑。
    pub fn suspension(&self) -> Option<SuspensionClass> {
        self.suspended.map(|(class, _)| class)
    }

    /// 按失败分类决定要不要挂起。
    ///
    /// 返回 `Some(class)` 表示这一次调用**把管线挂上了**——调用方据此只记一条日志，
    /// 巡检每 5 秒路过一次不会把同一件事反复写进日志。
    /// 已经挂着时再收到别的全局故障返回 `None`：恢复信号按第一次那类走，
    /// 避免后到的冷却类把「等重新登录」的管线提前放行。
    pub fn suspend_for(
        &mut self,
        failure: &PlaybackFailure,
        now: Instant,
    ) -> Option<SuspensionClass> {
        if self.suspended.is_some() {
            return None;
        }
        let class = suspension_class(failure)?;
        self.suspended = Some((class, now));
        Some(class)
    }

    /// 调用时若冷却类已到期则放行；登录 / 磁盘类只认事件，不随时间恢复。
    ///
    /// 闸门不会自己定时：什么时候来问由调用方决定（播放落闸期间预取巡检不执行）。
    /// 返回恢复原因（用于日志），没有变化时返回 `None`。
    pub fn expire_cooldown(&mut self, now: Instant) -> Option<SuspensionClass> {
        let (class, since) = self.suspended?;
        if class != SuspensionClass::Cooldown
            || now.saturating_duration_since(since) < Self::COOLDOWN
        {
            return None;
        }
        self.suspended = None;
        Some(class)
    }

    /// 重新登录成功：只解除登录类挂起，返回恢复原因（没解除则 `None`）。
    pub fn note_login_success(&mut self) -> Option<SuspensionClass> {
        self.resume(SuspensionClass::Login)
    }

    /// 用户主动播放：只解除磁盘类挂起，返回恢复原因（没解除则 `None`）。
    pub fn note_user_playback(&mut self) -> Option<SuspensionClass> {
        self.resume(SuspensionClass::Disk)
    }

    /// 解除指定故障类的挂起：恢复信号只认匹配的那一类，别的类原样保留。
    fn resume(&mut self, class: SuspensionClass) -> Option<SuspensionClass> {
        if self.suspension() == Some(class) {
            self.suspended = None;
            return Some(class);
        }
        None
    }
}

/// 这个失败是否属于会挂起整条管线的全局故障。
///
/// 调用方据此把「挂管线」「进逐曲冷却」分开；不要用 `allows()` 反推——
/// 那会把「已经挂着」和「失败本身不是全局」混成同一个答案。
pub fn is_global_failure(failure: &PlaybackFailure) -> bool {
    suspension_class(failure).is_some()
}

/// 哪些失败算「整条管线都该停」的全局故障。
///
/// 与 [`PlaybackFailure::is_per_track`] 同一判据：结论关于单首曲目的
/// （`TrackUnavailable` / `PickAnother` / `Undecodable`）不挂管线，只进不可播集合；
/// 换一首都可能变好的抖动类（`Nothing`）也不挂——那是逐曲冷却的职责。
fn suspension_class(failure: &PlaybackFailure) -> Option<SuspensionClass> {
    match failure.action {
        RecoveryAction::Relogin | RecoveryAction::CheckEntitlement => Some(SuspensionClass::Login),
        RecoveryAction::FreeSpace => Some(SuspensionClass::Disk),
        RecoveryAction::CheckSigner => Some(SuspensionClass::Cooldown),
        RecoveryAction::Nothing
        | RecoveryAction::TrackUnavailable
        | RecoveryAction::PickAnother
        | RecoveryAction::Undecodable => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::{RecoveryAction, Retryability, Stage};

    fn global(action: RecoveryAction) -> PlaybackFailure {
        PlaybackFailure::new(Stage::ResolveStream, Retryability::Never, action, "x")
    }

    /// 只有四类全局故障挂管线；按曲目类与抖动类都不挂。
    #[test]
    fn only_global_failures_suspend_the_pipeline() {
        let now = Instant::now();
        let mut gate = PrefetchGate::default();
        assert!(gate.allows());

        // 按曲目不可播：结论只关于这一首，只进不可播集合。
        for action in [
            RecoveryAction::TrackUnavailable,
            RecoveryAction::PickAnother,
            RecoveryAction::Undecodable,
        ] {
            assert_eq!(gate.suspend_for(&global(action), now), None, "{action:?}");
            assert!(gate.allows(), "{action:?}");
        }
        // 网络抖动 / 解密这类非确定失败交给逐曲冷却。
        let transient = PlaybackFailure::new(
            Stage::Transfer,
            Retryability::Transient,
            RecoveryAction::Nothing,
            "io: timed out reading response",
        );
        assert_eq!(gate.suspend_for(&transient, now), None);
        assert!(gate.allows());

        // 应用层用同一个谓词分流「挂管线 / 逐曲冷却」，两边必须一致。
        assert!(!is_global_failure(&global(
            RecoveryAction::TrackUnavailable
        )));
        assert!(!is_global_failure(&transient));
        assert!(is_global_failure(&global(RecoveryAction::CheckSigner)));

        let cases = [
            (RecoveryAction::Relogin, SuspensionClass::Login),
            (RecoveryAction::CheckEntitlement, SuspensionClass::Login),
            (RecoveryAction::FreeSpace, SuspensionClass::Disk),
            (RecoveryAction::CheckSigner, SuspensionClass::Cooldown),
        ];
        for (action, expected) in cases {
            let mut gate = PrefetchGate::default();
            assert_eq!(gate.suspend_for(&global(action), now), Some(expected));
            assert!(!gate.allows());
            assert_eq!(gate.suspension(), Some(expected));
            // 已挂着再收到别的全局故障：不重复记日志，也不改恢复信号。
            assert_eq!(
                gate.suspend_for(&global(RecoveryAction::CheckSigner), now),
                None
            );
            assert_eq!(gate.suspension(), Some(expected));
        }
    }

    /// 登录类只认「重新登录成功」；用户播放与时间都不解除。
    #[test]
    fn login_suspension_waits_for_a_relogin() {
        let now = Instant::now();
        let mut gate = PrefetchGate::default();
        assert_eq!(
            gate.suspend_for(&global(RecoveryAction::Relogin), now),
            Some(SuspensionClass::Login)
        );

        assert_eq!(gate.note_user_playback(), None);
        assert_eq!(
            gate.expire_cooldown(now + PrefetchGate::COOLDOWN * 10),
            None
        );
        assert!(!gate.allows());

        assert_eq!(
            gate.note_login_success(),
            Some(SuspensionClass::Login),
            "重新登录成功应解除登录类挂起"
        );
        assert!(gate.allows());
        // 解除之后同一件事不再重复报告。
        assert_eq!(gate.note_login_success(), None);
    }

    /// 磁盘类只认「用户主动播放」；重新登录与时间都不解除。
    #[test]
    fn disk_suspension_waits_for_a_user_play() {
        let now = Instant::now();
        let mut gate = PrefetchGate::default();
        assert_eq!(
            gate.suspend_for(&global(RecoveryAction::FreeSpace), now),
            Some(SuspensionClass::Disk)
        );

        assert_eq!(gate.note_login_success(), None);
        assert_eq!(
            gate.expire_cooldown(now + PrefetchGate::COOLDOWN * 10),
            None
        );
        assert!(!gate.allows());

        assert_eq!(
            gate.note_user_playback(),
            Some(SuspensionClass::Disk),
            "用户主动播放应解除磁盘类挂起"
        );
        assert!(gate.allows());
    }

    /// 签名服务挂起等 60 秒冷却，期满自动恢复；事件类恢复信号不提前放行。
    #[test]
    fn signer_suspension_cools_down_for_sixty_seconds() {
        let now = Instant::now();
        let mut gate = PrefetchGate::default();
        assert_eq!(
            gate.suspend_for(&global(RecoveryAction::CheckSigner), now),
            Some(SuspensionClass::Cooldown)
        );
        assert!(!gate.allows());

        // 用户播放与重新登录都不解除签名服务挂起。
        assert_eq!(gate.note_user_playback(), None);
        assert_eq!(gate.note_login_success(), None);
        // 差 1ms 也还不放行：冷却从挂起时刻算，不从巡检时刻累加。
        assert_eq!(
            gate.expire_cooldown(now + PrefetchGate::COOLDOWN - Duration::from_millis(1)),
            None
        );
        assert!(!gate.allows());

        assert_eq!(
            gate.expire_cooldown(now + PrefetchGate::COOLDOWN),
            Some(SuspensionClass::Cooldown)
        );
        assert!(gate.allows());
        assert_eq!(
            gate.expire_cooldown(now + PrefetchGate::COOLDOWN * 10),
            None
        );
    }

    /// 无法归类的失败既不是按曲目、也不是全局：调用方只能把它送去逐曲冷却。
    ///
    /// 这三类曾经被当作 `Retryability::Never` 写进「永久不再预取」表（#117）。
    /// 解密失败后来归成了「解不开」（按曲目），不再属于这一组。
    #[test]
    fn unclassified_failures_are_neither_per_track_nor_global() {
        use crate::failure::classify;
        use libresoda::SodaError;

        for error in [
            SodaError::api(1, "business error"),
            SodaError::http("http request failed: status 400 (https://x.test)"),
        ] {
            let failure = classify(&error);
            assert!(!failure.is_per_track(), "不该按曲目：{failure:?}");
            assert!(!is_global_failure(&failure), "不该全局挂起：{failure:?}");
        }
    }

    /// 解密失败按「解不开」归成按曲目：不挂管线、不走 60 秒逐曲冷却，
    /// 走的是进不可播集合的按曲目分支。
    #[test]
    fn crypto_failure_is_per_track_and_never_suspends_the_pipeline() {
        use crate::failure::classify;
        use libresoda::SodaError;

        let failure = classify(&SodaError::crypto("decrypted size mismatch"));
        assert!(failure.is_per_track(), "{failure:?}");
        assert!(!is_global_failure(&failure), "{failure:?}");

        // 挂起判定的穷举匹配要显式认下这个名字，而不是靠默认分支蒙混。
        let now = Instant::now();
        let mut gate = PrefetchGate::default();
        assert_eq!(gate.suspend_for(&failure, now), None);
        assert!(gate.allows());
        assert_eq!(gate.suspension(), None);
    }

    /// 日志取值稳定：大小写与连字符不随界面语言变化。
    #[test]
    fn suspension_log_values_are_stable() {
        let values = [
            (SuspensionClass::Login, "login"),
            (SuspensionClass::Disk, "disk"),
            (SuspensionClass::Cooldown, "cooldown"),
        ];
        for (class, expected) in values {
            assert_eq!(class.as_str(), expected);
        }
    }
}
