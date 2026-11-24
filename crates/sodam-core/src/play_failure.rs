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
    /// 不写不可播集合。
    #[default]
    None,
    /// 调用方把当前曲目写进不可播集合。
    Unplayable,
}

/// 常驻提示放哪个槽位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeSlot {
    /// 不写常驻错误位。
    #[default]
    None,
    /// 调用方写 `Root.playback_error`。
    PlaybackError,
    /// 调用方写整圈结论与 `Root.playback_error`。
    RoundExhausted,
}

/// 一次性提示走哪个渠道。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeChannel {
    /// 不发一次性提示。
    #[default]
    None,
    /// 调用方发一次警告。
    Warning,
    /// 调用方发一次错误。
    Error,
    /// 调用方先发失败警告，再发连败封顶错误。
    WarningThenError,
}

/// 提示的槽位与渠道；文案由调用方按原词典选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FailureNotice {
    /// 常驻提示写哪个槽位。
    pub slot: NoticeSlot,
    /// 一次性提示走哪些渠道。
    pub channel: NoticeChannel,
}

/// 队列怎么前进。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Advance {
    /// 调用方不换歌。
    #[default]
    Stay,
    /// 调用方按当前模式重试当前曲目。
    RetrySame,
    /// 调用方先真实推进队列，再装载下一首。
    Next,
}

/// 本轮怎么落闸。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Halt {
    /// 调用方不停机。
    #[default]
    None,
    /// 调用方把声音和本轮自动续播都停下。
    Playback,
    /// 调用方停本轮，并留下整圈无歌结论。
    ExhaustedRound,
    /// 调用方停机，并留下连败封顶结论。
    ConsecutiveFailures,
}

/// 一次失败决策的四样输出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayFailureDecision {
    /// 调用方要记哪种账。
    pub record: FailureRecord,
    /// 调用方要写和发哪些提示。
    pub notice: FailureNotice,
    /// 调用方怎么移动队列。
    pub advance: Advance,
    /// 调用方怎么停本轮。
    pub halt: Halt,
}

impl PlayFailureDecision {
    /// 身份已变：这次尝试不产生任何结论。
    #[cfg(test)]
    pub(crate) const fn invalidated() -> Self {
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
}

/// 纯决策的输入；身份校验和预取挂起留在调用方。
pub struct PlayFailureInput<'a> {
    /// 现在要声音，还是只预装第一首。
    pub intent: PlayIntent,
    /// 已经分类好的失败。
    pub failure: &'a PlaybackFailure,
    /// 记账后的集合里是否还有可播候选。
    pub has_candidate: bool,
    /// 本次失败之前的连败数。
    pub consecutive_failures: u32,
    /// 当前队列模式。
    pub mode: PlayMode,
}

/// 连续几次装载失败后停止本轮。
pub const PLAY_FAILURE_HALT_AFTER: u32 = 3;

/// 连败 +1 属于调用方清理，只有点播的抖动类失败计数。
pub fn counts_consecutive_failure(intent: PlayIntent, failure: &PlaybackFailure) -> bool {
    intent == PlayIntent::Now && failure.action == RecoveryAction::Nothing
}

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

    if !counts_consecutive_failure(input.intent, input.failure) {
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
            notice: FailureNotice {
                channel: NoticeChannel::Warning,
                ..FailureNotice::default()
            },
            advance: Advance::Next,
            ..PlayFailureDecision::default()
        }
    } else {
        PlayFailureDecision {
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
    fn policy_rows_match_current_behavior() {
        let per_track = RecoveryAction::TrackUnavailable;
        let global = RecoveryAction::Relogin;
        let transient = RecoveryAction::Nothing;
        // 第 13 行身份变化由调用门测试覆盖，纯决策不接身份。
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
                    FailureRecord::None,
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
                    FailureRecord::None,
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
                    FailureRecord::None,
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
                    FailureRecord::None,
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
                    FailureRecord::None,
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
            let failure = failure(action);
            for intent in [PlayIntent::Now, PlayIntent::PrepareFirst] {
                assert!(!counts_consecutive_failure(intent, &failure), "{action:?}");
            }
        }

        let transient = failure(RecoveryAction::Nothing);
        assert!(counts_consecutive_failure(PlayIntent::Now, &transient));
        assert!(!counts_consecutive_failure(
            PlayIntent::PrepareFirst,
            &transient
        ));
        for mode in [PlayMode::ListLoop, PlayMode::RepeatOne, PlayMode::Shuffle] {
            let result = decide(PlayIntent::Now, RecoveryAction::Nothing, false, 0, mode);
            assert_eq!(result.record, FailureRecord::None, "{mode:?}");
        }
    }

    #[test]
    fn identity_invalidation_leaves_no_decision_or_visible_consequence() {
        let normal = decide(
            PlayIntent::Now,
            RecoveryAction::TrackUnavailable,
            true,
            0,
            PlayMode::ListLoop,
        );
        let result = PlayFailureDecision::invalidated();
        assert_ne!(result, normal);
        assert_eq!(result.record, FailureRecord::None);
        assert_eq!(result.notice, FailureNotice::default());
        assert_eq!(result.advance, Advance::Stay);
        assert_eq!(result.halt, Halt::None);
    }
}
