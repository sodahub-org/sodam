//! 全局 message：Ant Design 风格的多条轻提示队列。

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context};

use crate::app::Root;
use crate::ui::theme;

/// 默认显示时长，与 Ant Design message 的默认值一致。
pub const DEFAULT_DURATION: Duration = Duration::from_secs(3);
/// 即使来自不同来源，也不让提示铺满整块界面。
const MAX_VISIBLE_MESSAGES: usize = 4;

#[allow(dead_code)] // public message API: Error is used by callers outside this crate's binary tests
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: u64,
    pub kind: MessageKind,
    pub text: String,
    /// `None` 表示不自动关闭；默认见 [`DEFAULT_DURATION`]。
    pub duration: Option<Duration>,
    /// 产生时刻。显示时长从这里扣，不从首次渲染算起——窗口关着时消息照样在变旧（#212）。
    pub created_at: Instant,
    /// 同一来源的提示会替换旧的，避免定时失败等重复事件堆成消息墙。
    pub key: Option<String>,
}

impl Message {
    /// 这类消息关窗期间不能被吞，重开时补弹一次完整时长（#212）。
    /// 失败多半发生在后台、正好赶窗口关着，按产生时刻算会在重开时被裁掉。
    fn replays_on_reopen(&self) -> bool {
        matches!(self.kind, MessageKind::Error | MessageKind::Warning)
    }

    /// 还剩多少显示时间；`None` = 已过期或本就不自动关闭。
    ///
    /// Info/Success 按产生时刻扣减：首帧来得晚就只显示剩余时间。
    /// [`Message::replays_on_reopen`] 的类型不看产生时刻，首帧拿到完整时长。
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        let duration = self.duration?;
        if self.replays_on_reopen() {
            return Some(duration);
        }
        let left = duration.saturating_sub(now.saturating_duration_since(self.created_at));
        (left > Duration::ZERO).then_some(left)
    }

    /// 这条消息是否该在首帧被裁掉。不自动关闭的消息、以及会补弹的类型都不裁
    ///（理由见 [`Message::replays_on_reopen`]）。
    pub fn expired(&self, now: Instant) -> bool {
        let Some(duration) = self.duration else {
            return false;
        };
        if self.replays_on_reopen() {
            return false;
        }
        now.saturating_duration_since(self.created_at) >= duration
    }

    fn accent(&self) -> gpui::Rgba {
        match self.kind {
            MessageKind::Info => theme::text_muted(),
            MessageKind::Success => theme::accent(),
            MessageKind::Warning => theme::warning(),
            MessageKind::Error => theme::danger(),
        }
    }

    fn icon(&self) -> &'static str {
        match self.kind {
            MessageKind::Info => "i",
            MessageKind::Success => "✓",
            MessageKind::Warning => "!",
            MessageKind::Error => "×",
        }
    }
}

impl Root {
    /// 打开一条全局 message；返回 ID，可用于 [`Root::close_message`]。
    #[allow(dead_code)] // 公共 API 保留给不重复的事件
    pub fn open_message(&mut self, kind: MessageKind, text: impl Into<String>) -> u64 {
        self.push_message(kind, text, Some(DEFAULT_DURATION))
    }

    #[allow(dead_code)] // 公共 API 保留给自定义时长
    pub fn push_message(
        &mut self,
        kind: MessageKind,
        text: impl Into<String>,
        duration: Option<Duration>,
    ) -> u64 {
        self.push_message_with_key(kind, text, duration, None)
    }

    /// 同 key 的消息替换旧消息；频率由调用方按事件类型控制。
    pub fn push_message_with_key(
        &mut self,
        kind: MessageKind,
        text: impl Into<String>,
        duration: Option<Duration>,
        key: Option<&str>,
    ) -> u64 {
        let id = upsert_message(
            &mut self.messages,
            &mut self.next_message_id,
            kind,
            text.into(),
            duration,
            Instant::now(),
            key,
        );
        self.message_timers.retain(|message_id| {
            self.messages
                .iter()
                .any(|message| message.id == *message_id)
        });
        id
    }

    pub fn close_message(&mut self, id: u64) -> bool {
        let before = self.messages.len();
        self.messages.retain(|message| message.id != id);
        self.messages.len() != before
    }

    /// 同一 key 的现有消息被替换，返回新消息 id。
    pub fn push_message_once(
        &mut self,
        kind: MessageKind,
        text: impl Into<String>,
        duration: Option<Duration>,
        key: &str,
    ) -> u64 {
        self.push_message_with_key(kind, text, duration, Some(key))
    }

    /// 启动该条消息的计时器；消息被替换/关闭后计时器会安全退出。
    ///
    /// 等的时间就是该条 [`Message::remaining`]：Info/Success 从产生时刻扣，首帧
    /// 来得晚就只等剩下的；Error/Warning 从首帧起算完整时长，保证补弹一次看得见（#212）。
    /// `now` 由渲染方传入，与"筛掉过期消息"用同一个时刻，避免同一帧两次取时间
    /// 让刚过期的消息多活一帧。
    pub(crate) fn schedule_message_close(&mut self, id: u64, now: Instant, cx: &mut Context<Self>) {
        let Some(message) = self.messages.iter().find(|message| message.id == id) else {
            return;
        };
        let Some(remaining) = message.remaining(now) else {
            // 已过期或本就不自动关闭的消息都不排计时。
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(remaining).await;
            let _ = this.update(cx, |root, cx| {
                if root.close_message(id) {
                    root.message_timers.remove(&id);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

fn upsert_message(
    messages: &mut Vec<Message>,
    next_id: &mut u64,
    kind: MessageKind,
    text: String,
    duration: Option<Duration>,
    created_at: Instant,
    key: Option<&str>,
) -> u64 {
    let key = key.map(str::to_string);
    if let Some(key) = key.as_deref() {
        if let Some(index) = messages
            .iter()
            .position(|message| message.key.as_deref() == Some(key))
        {
            messages.remove(index);
        }
    }
    *next_id = next_id.wrapping_add(1);
    let id = *next_id;
    messages.push(Message {
        id,
        kind,
        text,
        duration,
        created_at,
        key,
    });
    if messages.len() > MAX_VISIBLE_MESSAGES {
        messages.drain(0..messages.len() - MAX_VISIBLE_MESSAGES);
    }
    id
}

/// 裁掉过期消息，并同步清掉它们对应的计时器 id。
///
/// 只裁 Info/Success（按产生时刻过期）；Error/Warning 留着重开补弹一次。
/// 这是渲染路径上的清理点，配合另外三处：入队时清被替换/挤出队列的
///（`push_message_with_key`）、计时到期和点击关闭时各清自己的 id。
/// 这几处都不在渲染中途删消息，所以排完计时不用再清一遍。
fn prune_expired(messages: &mut Vec<Message>, timers: &mut HashSet<u64>, now: Instant) {
    messages.retain(|message| !message.expired(now));
    timers.retain(|id| messages.iter().any(|message| message.id == *id));
}

/// 最高层浮层：多条消息按插入顺序堆叠，各自独立计时。
pub(crate) fn render(root: &mut Root, cx: &mut Context<Root>) -> Option<AnyElement> {
    // 关窗期间攒下的消息，首帧筛一遍：Info/Success 按产生时刻裁掉，
    // Error/Warning 留着重开补弹一次（#212）。计时器按 id 去重，每条只排一次。
    let now = Instant::now();
    prune_expired(&mut root.messages, &mut root.message_timers, now);

    if root.messages.is_empty() {
        return None;
    }
    let ids: Vec<u64> = root
        .messages
        .iter()
        .filter_map(|message| message.duration.map(|_| message.id))
        .collect();
    for id in ids {
        if root.message_timers.insert(id) {
            root.schedule_message_close(id, now, cx);
        }
    }

    let messages = root.messages.clone();
    Some(
        gpui::deferred(
            div()
                .id("global-messages")
                .absolute()
                .top(px(48.0))
                .right(px(24.0))
                .flex()
                .flex_col()
                .items_end()
                .gap(px(theme::space::SM))
                .children(messages.into_iter().map(|message| {
                    let accent = message.accent();
                    let id = message.id;
                    div()
                        .id(("message", id as usize))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::SM))
                        .max_w(px(520.0))
                        .px(px(theme::space::LG))
                        .py(px(theme::space::SM))
                        .rounded(px(theme::radius::CARD))
                        .border_1()
                        .border_color(accent)
                        .bg(theme::surface_elevated())
                        .text_color(theme::text())
                        .text_size(theme::Text::Small.size())
                        .shadow_lg()
                        .child(div().flex_none().text_color(accent).child(message.icon()))
                        .child(message.text)
                        .on_click(cx.listener(move |root, _, _, cx| {
                            if root.close_message(id) {
                                root.message_timers.remove(&id);
                                cx.notify();
                            }
                        }))
                        .into_any_element()
                })),
        )
        .with_priority(usize::MAX)
        .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_kind(kind: MessageKind, created_at: Instant, duration: Option<Duration>) -> Message {
        Message {
            id: 1,
            kind,
            text: "test".into(),
            duration,
            created_at,
            key: None,
        }
    }

    fn sample(created_at: Instant, duration: Option<Duration>) -> Message {
        sample_kind(MessageKind::Info, created_at, duration)
    }

    #[test]
    fn default_message_uses_three_seconds() {
        assert_eq!(DEFAULT_DURATION, Duration::from_secs(3));
    }

    #[test]
    fn message_kinds_have_distinct_visual_tokens() {
        let base = sample(Instant::now(), None);
        let success = Message {
            kind: MessageKind::Success,
            ..base.clone()
        };
        let warning = Message {
            kind: MessageKind::Warning,
            ..base.clone()
        };
        let error = Message {
            kind: MessageKind::Error,
            ..base
        };
        assert_ne!(success.icon(), warning.icon());
        assert_ne!(warning.icon(), error.icon());
    }

    #[test]
    fn a_fresh_message_has_its_whole_duration_left() {
        let now = Instant::now();
        let message = sample(now, Some(DEFAULT_DURATION));
        assert_eq!(message.remaining(now), Some(DEFAULT_DURATION));
    }

    #[test]
    fn a_message_first_seen_late_only_gets_the_remaining_time() {
        let now = Instant::now();
        let message = sample(now, Some(DEFAULT_DURATION));
        // 首次画到它时已经过去 2 秒：只能再留 1 秒，不能重新计满 3 秒。
        let late = now + Duration::from_secs(2);
        assert_eq!(message.remaining(late), Some(Duration::from_secs(1)));
    }

    #[test]
    fn an_expired_message_is_not_shown_anymore() {
        let now = Instant::now();
        let message = sample(now, Some(DEFAULT_DURATION));
        let later = now + DEFAULT_DURATION;
        assert!(message.expired(later));
        assert_eq!(message.remaining(later), None);
    }

    #[test]
    fn an_old_error_still_shows_a_whole_window_on_reopen() {
        // 签名超时发生在关窗期间，隔了 1 小时才重开首帧。
        // 用加法从过去推到现在，不用 now - 1h：CI 新机器 uptime 不足 1h 时后者会 panic。
        let created = Instant::now();
        let now = created + Duration::from_secs(3600);
        let error = sample_kind(MessageKind::Error, created, Some(DEFAULT_DURATION));
        assert!(!error.expired(now), "Error 不按产生时刻过期");
        assert_eq!(
            error.remaining(now),
            Some(DEFAULT_DURATION),
            "补弹一次要给完整时长"
        );
    }

    #[test]
    fn an_old_warning_is_kept_just_like_an_error() {
        let created = Instant::now();
        let now = created + Duration::from_secs(3600);
        let warning = sample_kind(MessageKind::Warning, created, Some(DEFAULT_DURATION));
        assert!(!warning.expired(now));
        assert_eq!(warning.remaining(now), Some(DEFAULT_DURATION));
    }

    #[test]
    fn pruning_keeps_old_errors_but_drops_old_info() {
        let old = Instant::now();
        let now = old + Duration::from_secs(3600);
        let mut messages = vec![
            sample_kind(MessageKind::Error, old, Some(DEFAULT_DURATION)),
            sample_kind(MessageKind::Warning, old, Some(DEFAULT_DURATION)),
            sample_kind(MessageKind::Info, old, Some(DEFAULT_DURATION)),
            sample_kind(MessageKind::Success, old, Some(DEFAULT_DURATION)),
        ];
        for (index, message) in messages.iter_mut().enumerate() {
            message.id = index as u64 + 1;
        }
        let mut timers = HashSet::from([1, 2, 3, 4]);

        prune_expired(&mut messages, &mut timers, now);

        assert_eq!(messages.len(), 2, "Error/Warning 留着，Info/Success 裁掉");
        assert_eq!(messages[0].kind, MessageKind::Error);
        assert_eq!(messages[1].kind, MessageKind::Warning);
        assert_eq!(timers, HashSet::from([1, 2]), "被裁消息的计时 id 同步清掉");
    }

    #[test]
    fn a_message_without_a_deadline_never_expires_or_claims_a_timer() {
        let now = Instant::now();
        let message = sample(now, None);
        let much_later = now + Duration::from_secs(3600);
        assert!(!message.expired(much_later));
        assert_eq!(message.remaining(much_later), None);
    }

    #[test]
    fn pushing_a_message_stamps_when_it_was_created() {
        let now = Instant::now();
        let mut messages = Vec::new();
        let mut next_id = 0;
        upsert_message(
            &mut messages,
            &mut next_id,
            MessageKind::Info,
            "hi".into(),
            Some(DEFAULT_DURATION),
            now,
            None,
        );
        assert_eq!(messages[0].created_at, now);
    }

    #[test]
    fn replacing_the_same_key_starts_a_fresh_lifetime() {
        let start = Instant::now();
        let mut messages = Vec::new();
        let mut next_id = 0;
        // 用 Info：Error/Warning 本就不按产生时刻过期，测不出「重新计时」。
        let first = upsert_message(
            &mut messages,
            &mut next_id,
            MessageKind::Info,
            "同一来源提示".into(),
            Some(DEFAULT_DURATION),
            start,
            Some("same-source"),
        );
        // 隔了整个时长，旧消息本该已经过期。
        let stale = start + DEFAULT_DURATION;
        assert!(messages[0].expired(stale));

        // 同一来源重试：换一条新消息，从新的产生时刻重新计时。
        let retry = stale + Duration::from_secs(1);
        let second = upsert_message(
            &mut messages,
            &mut next_id,
            MessageKind::Info,
            "同一来源提示，更新".into(),
            Some(DEFAULT_DURATION),
            retry,
            Some("same-source"),
        );
        assert_ne!(first, second);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].created_at, retry);
        assert_eq!(messages[0].remaining(retry), Some(DEFAULT_DURATION));
    }

    #[test]
    fn pruning_drops_expired_messages_and_their_timer_ids() {
        let fresh = Instant::now();
        let now = fresh + DEFAULT_DURATION + Duration::from_secs(1);
        let mut messages = vec![
            sample(fresh, Some(DEFAULT_DURATION)),
            sample(now, Some(DEFAULT_DURATION)),
        ];
        messages[0].id = 10;
        messages[1].id = 20;
        let mut timers = HashSet::from([10, 20]);

        prune_expired(&mut messages, &mut timers, now);

        assert_eq!(messages.len(), 1, "过期的那条要被丢掉");
        assert_eq!(messages[0].id, 20, "没过期的那条留着");
        assert_eq!(timers, HashSet::from([20]), "被丢消息的计时 id 要同步清掉");
    }

    #[test]
    fn pruning_keeps_messages_that_never_expire() {
        let created = Instant::now();
        let now = created + Duration::from_secs(3600);
        let mut messages = vec![sample(created, None)];
        messages[0].id = 7;
        let mut timers = HashSet::from([7]);

        prune_expired(&mut messages, &mut timers, now);

        assert_eq!(messages.len(), 1, "不自动关闭的消息永不过期");
        assert_eq!(timers, HashSet::from([7]));
    }

    #[test]
    fn same_message_key_replaces_the_existing_entry_and_restarts_timer() {
        let now = Instant::now();
        let mut messages = Vec::new();
        let mut next_id = 0;
        let first = upsert_message(
            &mut messages,
            &mut next_id,
            MessageKind::Error,
            "same failure".into(),
            Some(DEFAULT_DURATION),
            now,
            Some("save-failed"),
        );
        let second = upsert_message(
            &mut messages,
            &mut next_id,
            MessageKind::Warning,
            "same failure, newer".into(),
            Some(DEFAULT_DURATION),
            now,
            Some("save-failed"),
        );
        assert_ne!(first, second);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].kind, MessageKind::Warning);
        assert_eq!(messages[0].text, "same failure, newer");
        assert_eq!(messages[0].id, second);
    }

    #[test]
    fn message_buffer_keeps_only_the_newest_entries() {
        let mut messages = Vec::new();
        let mut next_id = 0;
        for index in 0..=MAX_VISIBLE_MESSAGES {
            upsert_message(
                &mut messages,
                &mut next_id,
                MessageKind::Info,
                format!("message {index}"),
                Some(DEFAULT_DURATION),
                Instant::now(),
                None,
            );
        }
        assert_eq!(messages.len(), MAX_VISIBLE_MESSAGES);
        assert_eq!(messages[0].text, "message 1");
        assert_eq!(messages[MAX_VISIBLE_MESSAGES - 1].text, "message 4");
    }
}
