//! 全局 message：Ant Design 风格的多条轻提示队列。

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
    /// 从产生时刻算起，还剩多少显示时间；`None` = 已过期或本就不自动关闭。
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        let duration = self.duration?;
        let elapsed = now.saturating_duration_since(self.created_at);
        let left = duration.saturating_sub(elapsed);
        (left > Duration::ZERO).then_some(left)
    }

    /// 这条消息是否已经过期。不自动关闭的消息永不过期。
    pub fn expired(&self, now: Instant) -> bool {
        self.remaining(now).is_none() && self.duration.is_some()
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
    /// 计时从**产生时刻**算：首帧来得晚就只等剩下的时间，不会重新计满一整段（#212）。
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

/// 最高层浮层：多条消息按插入顺序堆叠，各自独立计时。
pub(crate) fn render(root: &mut Root, cx: &mut Context<Root>) -> Option<AnyElement> {
    // 关窗期间（没有渲染）攒下的消息，到这里先按产生时刻筛一遍：过期的直接丢掉，
    // 不补弹、也不重置计时（#212）。首帧就是过期判定的时机。
    let now = Instant::now();
    root.messages.retain(|message| !message.expired(now));
    root.message_timers
        .retain(|id| root.messages.iter().any(|message| message.id == *id));

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
    root.message_timers
        .retain(|id| root.messages.iter().any(|message| message.id == *id));

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

    fn sample(created_at: Instant, duration: Option<Duration>) -> Message {
        Message {
            id: 1,
            kind: MessageKind::Info,
            text: "test".into(),
            duration,
            created_at,
            key: None,
        }
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
        let first = upsert_message(
            &mut messages,
            &mut next_id,
            MessageKind::Warning,
            "网络错误".into(),
            Some(DEFAULT_DURATION),
            start,
            Some("recommend-more"),
        );
        // 隔了整个时长，旧消息本该已经过期。
        let stale = start + DEFAULT_DURATION;
        assert!(messages[0].expired(stale));

        // 同一来源重试：换一条新消息，从新的产生时刻重新计时。
        let retry = stale + Duration::from_secs(1);
        let second = upsert_message(
            &mut messages,
            &mut next_id,
            MessageKind::Warning,
            "网络错误".into(),
            Some(DEFAULT_DURATION),
            retry,
            Some("recommend-more"),
        );
        assert_ne!(first, second);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].created_at, retry);
        assert_eq!(messages[0].remaining(retry), Some(DEFAULT_DURATION));
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
