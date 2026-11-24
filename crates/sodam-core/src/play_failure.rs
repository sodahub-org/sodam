//! 播放失败后的纯决策：只回该记什么、怎么提示、怎么前进、要不要落闸。

use crate::failure::{PlaybackFailure, RecoveryAction};
use crate::queue::PlayMode;

/// 这次装载想做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayIntent {
    /// 点播或自动续播：现在就要声音。
    #[default]
    Now,
    /// 换队列后只备好第一首，等用户自己按播放。
    PrepareFirst,
}

/// 这次失败要记哪种账。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureRecord {
    #[default]
    None,
    Unplayable,
    Consecutive,
}

/// 常驻提示放哪个槽位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeSlot {
    #[default]
    None,
    PlaybackError,
    RoundExhausted,
}

/// 一次性提示走哪个渠道。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeChannel {
    #[default]
    None,
    Warning,
    Error,
    WarningThenError,
}

/// 提示的槽位与渠道；文案由调用方按原词典选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FailureNotice {
    pub slot: NoticeSlot,
    pub channel: NoticeChannel,
}

/// 队列怎么前进。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Advance {
    #[default]
    Stay,
    RetrySame,
    Next,
}

/// 本轮怎么落闸。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Halt {
    #[default]
    None,
    Playback,
    ExhaustedRound,
    ConsecutiveFailures,
}

/// 一次失败决策的四样输出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayFailureDecision {
    pub record: FailureRecord,
    pub notice: FailureNotice,
    pub advance: Advance,
    pub halt: Halt,
}

impl PlayFailureDecision {
    /// 身份已变：这次尝试不产生任何结论。
    pub const fn invalidated() -> Self {
        Self {
            record: FailureRecord::None,
            notice: FailureNotice {
                slot: NoticeSlot::None,
                channel: NoticeChannel::None,
            },
            advance: Advance::Stay,
            halt: Halt::None,
        }
    }

    pub const fn has_conclusions(&self) -> bool {
        !matches!(self.record, FailureRecord::None)
            || !matches!(self.notice.slot, NoticeSlot::None)
            || !matches!(self.notice.channel, NoticeChannel::None)
            || !matches!(self.advance, Advance::Stay)
            || !matches!(self.halt, Halt::None)
    }
}

/// 纯决策的输入；身份校验和预取挂起留在调用方。
pub struct PlayFailureInput<'a> {
    pub intent: PlayIntent,
    pub failure: &'a PlaybackFailure,
    pub has_candidate: bool,
    pub consecutive_failures: u32,
    pub mode: PlayMode,
}

pub const PLAY_FAILURE_HALT_AFTER: u32 = 3;

/// 按现状决定一次播放失败的后果。
///
/// 预装仍只试一次，不走点播的连败封顶；这里记录差异，不替预装改行为。
pub fn decide_play_failure(input: PlayFailureInput<'_>) -> PlayFailureDecision {
    if input.failure.is_per_track() {
        return if input.has_candidate {
            PlayFailureDecision {
                record: FailureRecord::Unplayable,
                notice: FailureNotice {
                    slot: NoticeSlot::None,
                    channel: NoticeChannel::Warning,
                },
                advance: Advance::Next,
                halt: Halt::None,
            }
        } else {
            PlayFailureDecision {
                record: FailureRecord::Unplayable,
                notice: FailureNotice {
                    slot: NoticeSlot::RoundExhausted,
                    channel: NoticeChannel::Error,
                },
                advance: Advance::Stay,
                halt: Halt::ExhaustedRound,
            }
        };
    }

    if input.intent == PlayIntent::PrepareFirst {
        return PlayFailureDecision {
            notice: FailureNotice {
                slot: NoticeSlot::PlaybackError,
                channel: NoticeChannel::None,
            },
            ..PlayFailureDecision::default()
        };
    }

    if input.failure.action != RecoveryAction::Nothing {
        return PlayFailureDecision {
            notice: FailureNotice {
                slot: NoticeSlot::PlaybackError,
                channel: NoticeChannel::Warning,
            },
            halt: Halt::Playback,
            ..PlayFailureDecision::default()
        };
    }

    if input.consecutive_failures.saturating_add(1) >= PLAY_FAILURE_HALT_AFTER {
        return PlayFailureDecision {
            record: FailureRecord::Consecutive,
            notice: FailureNotice {
                slot: NoticeSlot::PlaybackError,
                channel: NoticeChannel::WarningThenError,
            },
            halt: Halt::ConsecutiveFailures,
            ..PlayFailureDecision::default()
        };
    }

    if input.mode == PlayMode::RepeatOne {
        return PlayFailureDecision {
            record: FailureRecord::Consecutive,
            notice: FailureNotice {
                channel: NoticeChannel::Warning,
                ..FailureNotice::default()
            },
            advance: Advance::RetrySame,
            ..PlayFailureDecision::default()
        };
    }

    if input.has_candidate {
        PlayFailureDecision {
            record: FailureRecord::Consecutive,
            notice: FailureNotice {
                channel: NoticeChannel::Warning,
                ..FailureNotice::default()
            },
            advance: Advance::Next,
            ..PlayFailureDecision::default()
        }
    } else {
        PlayFailureDecision {
            record: FailureRecord::Consecutive,
            notice: FailureNotice {
                slot: NoticeSlot::PlaybackError,
                channel: NoticeChannel::Warning,
            },
            halt: Halt::Playback,
            ..PlayFailureDecision::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::{Retryability, Stage};

    fn failure(action: RecoveryAction) -> PlaybackFailure {
        PlaybackFailure::new(Stage::Network, Retryability::Transient, action, "detail")
    }

    fn decide(
        intent: PlayIntent,
        action: RecoveryAction,
        has_candidate: bool,
        consecutive_failures: u32,
        mode: PlayMode,
    ) -> PlayFailureDecision {
        decide_play_failure(PlayFailureInput {
            intent,
            failure: &failure(action),
            has_candidate,
            consecutive_failures,
            mode,
        })
    }

    fn decision(
        record: FailureRecord,
        slot: NoticeSlot,
        channel: NoticeChannel,
        advance: Advance,
        halt: Halt,
    ) -> PlayFailureDecision {
        PlayFailureDecision {
            record,
            notice: FailureNotice { slot, channel },
            advance,
            halt,
        }
    }

    #[test]
    fn thirteen_row_acceptance_table_matches_current_behavior() {
        let per_track = RecoveryAction::TrackUnavailable;
        let global = RecoveryAction::Relogin;
        let transient = RecoveryAction::Nothing;
        let cases = [
            (
                "点播·按曲目·有候选",
                decide(PlayIntent::Now, per_track, true, 0, PlayMode::ListLoop),
                decision(
                    FailureRecord::Unplayable,
                    NoticeSlot::None,
                    NoticeChannel::Warning,
                    Advance::Next,
                    Halt::None,
                ),
            ),
            (
                "点播·按曲目·无候选",
                decide(PlayIntent::Now, per_track, false, 0, PlayMode::ListLoop),
                decision(
                    FailureRecord::Unplayable,
                    NoticeSlot::RoundExhausted,
                    NoticeChannel::Error,
                    Advance::Stay,
                    Halt::ExhaustedRound,
                ),
            ),
            (
                "预装·按曲目·有候选",
                decide(
                    PlayIntent::PrepareFirst,
                    per_track,
                    true,
                    0,
                    PlayMode::ListLoop,
                ),
                decision(
                    FailureRecord::Unplayable,
                    NoticeSlot::None,
                    NoticeChannel::Warning,
                    Advance::Next,
                    Halt::None,
                ),
            ),
            (
                "预装·按曲目·无候选",
                decide(
                    PlayIntent::PrepareFirst,
                    per_track,
                    false,
                    0,
                    PlayMode::ListLoop,
                ),
                decision(
                    FailureRecord::Unplayable,
                    NoticeSlot::RoundExhausted,
                    NoticeChannel::Error,
                    Advance::Stay,
                    Halt::ExhaustedRound,
                ),
            ),
            (
                "点播·全局",
                decide(PlayIntent::Now, global, false, 0, PlayMode::ListLoop),
                decision(
                    FailureRecord::None,
                    NoticeSlot::PlaybackError,
                    NoticeChannel::Warning,
                    Advance::Stay,
                    Halt::Playback,
                ),
            ),
            (
                "预装·全局",
                decide(
                    PlayIntent::PrepareFirst,
                    global,
                    false,
                    0,
                    PlayMode::ListLoop,
                ),
                decision(
                    FailureRecord::None,
                    NoticeSlot::PlaybackError,
                    NoticeChannel::None,
                    Advance::Stay,
                    Halt::None,
                ),
            ),
            (
                "点播·抖动·单曲·未封顶",
                decide(PlayIntent::Now, transient, false, 0, PlayMode::RepeatOne),
                decision(
                    FailureRecord::Consecutive,
                    NoticeSlot::None,
                    NoticeChannel::Warning,
                    Advance::RetrySame,
                    Halt::None,
                ),
            ),
            (
                "点播·抖动·单曲·封顶",
                decide(PlayIntent::Now, transient, false, 2, PlayMode::RepeatOne),
                decision(
                    FailureRecord::Consecutive,
                    NoticeSlot::PlaybackError,
                    NoticeChannel::WarningThenError,
                    Advance::Stay,
                    Halt::ConsecutiveFailures,
                ),
            ),
            (
                "点播·抖动·有候选·未封顶",
                decide(PlayIntent::Now, transient, true, 0, PlayMode::ListLoop),
                decision(
                    FailureRecord::Consecutive,
                    NoticeSlot::None,
                    NoticeChannel::Warning,
                    Advance::Next,
                    Halt::None,
                ),
            ),
            (
                "点播·抖动·有候选·封顶",
                decide(PlayIntent::Now, transient, true, 2, PlayMode::ListLoop),
                decision(
                    FailureRecord::Consecutive,
                    NoticeSlot::PlaybackError,
                    NoticeChannel::WarningThenError,
                    Advance::Stay,
                    Halt::ConsecutiveFailures,
                ),
            ),
            (
                "点播·抖动·无候选",
                decide(PlayIntent::Now, transient, false, 0, PlayMode::ListLoop),
                decision(
                    FailureRecord::Consecutive,
                    NoticeSlot::PlaybackError,
                    NoticeChannel::Warning,
                    Advance::Stay,
                    Halt::Playback,
                ),
            ),
            (
                "预装·抖动",
                decide(
                    PlayIntent::PrepareFirst,
                    transient,
                    false,
                    0,
                    PlayMode::ListLoop,
                ),
                decision(
                    FailureRecord::None,
                    NoticeSlot::PlaybackError,
                    NoticeChannel::None,
                    Advance::Stay,
                    Halt::None,
                ),
            ),
            (
                "任意·身份变了",
                PlayFailureDecision::invalidated(),
                PlayFailureDecision::invalidated(),
            ),
        ];

        for (name, actual, expected) in cases {
            assert_eq!(actual, expected, "{name}");
        }
    }

    #[test]
    fn only_transient_point_failures_increment_consecutive_failures() {
        for action in [
            RecoveryAction::TrackUnavailable,
            RecoveryAction::PickAnother,
            RecoveryAction::Undecodable,
            RecoveryAction::Relogin,
            RecoveryAction::CheckSigner,
            RecoveryAction::CheckEntitlement,
            RecoveryAction::FreeSpace,
        ] {
            for intent in [PlayIntent::Now, PlayIntent::PrepareFirst] {
                let result = decide(intent, action, false, 0, PlayMode::ListLoop);
                assert_ne!(result.record, FailureRecord::Consecutive, "{action:?}");
            }
        }

        for mode in [PlayMode::ListLoop, PlayMode::RepeatOne, PlayMode::Shuffle] {
            let result = decide(PlayIntent::Now, RecoveryAction::Nothing, false, 0, mode);
            assert_eq!(result.record, FailureRecord::Consecutive, "{mode:?}");
        }
        assert_ne!(
            decide(
                PlayIntent::PrepareFirst,
                RecoveryAction::Nothing,
                false,
                0,
                PlayMode::ListLoop
            )
            .record,
            FailureRecord::Consecutive
        );
    }

    #[test]
    fn identity_invalidation_leaves_no_decision_or_visible_consequence() {
        let result = PlayFailureDecision::invalidated();
        assert!(!result.has_conclusions());
        assert_eq!(result.record, FailureRecord::None);
        assert_eq!(result.notice, FailureNotice::default());
        assert_eq!(result.advance, Advance::Stay);
        assert_eq!(result.halt, Halt::None);
    }
}
