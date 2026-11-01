//! 底部状态行：`Root::status_line` 的唯一渲染出口（#83）。
//!
//! 普通操作反馈（切档、开关、收藏、加载中…）有六十多处写入点。它们以前只写不画，
//! 用户看不到；这里给它们一个固定位置：播放栏上方左侧一行，停留 [`VISIBLE_FOR`]
//! 后自行消失。重要的、需要打断的事件仍然走 [`crate::ui::message`] 的全局轻提示，
//! 两条通道各管一件事——轻提示负责「叫住你」，状态行负责「留下来说过什么」。

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context};

use crate::app::Root;
use crate::ui::theme;

/// 状态行停留时长。
///
/// 比全局 message 的 3 秒长一点：它不抢注意力，用户往往在处理别的事，
/// 错过就没有第二次机会；但也不能久留，否则会变成一条没人收走的旧文案。
pub const VISIBLE_FOR: Duration = Duration::from_secs(5);

/// 状态行的全部状态：文案、写入序号、写入时刻、已排过计时器的序号。
///
/// 四者必须一起变：单独写文案而不动序号，到期计时器就还在替旧文案计时。
/// 收在一个类型里，写入方只调 [`StatusLine::set`]，看不到这些细节。
#[derive(Default)]
pub(crate) struct StatusLine {
    text: String,
    /// 每写一条状态加一；计时器拿它判断「到期时还是不是同一条」。
    seq: u64,
    /// 写入时刻。`None` = 没有要显示的内容。
    since: Option<Instant>,
    /// 已经为哪个 `seq` 排过计时器。渲染每帧都跑，靠它避免每帧起一个后台任务。
    scheduled_seq: u64,
}

impl StatusLine {
    /// 写一条新状态：顶掉旧文案，重新开始计时。空文案等同于清空。
    pub(crate) fn set(&mut self, text: String) {
        self.seq = self.seq.wrapping_add(1);
        self.since = Some(Instant::now());
        self.text = text;
    }

    /// 清空。切页时调用：状态行是全局反馈，不属于某一页，
    /// 留在新页面上就成了上一页的残留文案。
    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.since = None;
    }

    /// 现在该显示什么：空文案、没写过、或已过显示窗口都不显示。
    pub(crate) fn visible(&self, now: Instant) -> Option<&str> {
        let since = self.since?;
        if now.saturating_duration_since(since) >= VISIBLE_FOR {
            return None;
        }
        let text = self.text.trim();
        (!text.is_empty()).then_some(text)
    }

    /// 为当前这条状态认领一次到期计时：同一条状态只认领一次。
    ///
    /// 返回 `(序号, 剩余时长)`。剩余按**写入时刻**算，不按首次渲染时刻重新起算：
    /// 没有别的重绘时，收走这一行靠的就是这个计时器到期后的 `notify`；首帧来得越晚，
    /// 按整段重排就会让它多挂越久。
    pub(crate) fn claim_timer(&mut self, now: Instant) -> Option<(u64, Duration)> {
        if self.scheduled_seq == self.seq {
            return None;
        }
        let since = self.since?;
        self.scheduled_seq = self.seq;
        Some((
            self.seq,
            VISIBLE_FOR.saturating_sub(now.saturating_duration_since(since)),
        ))
    }

    /// 计时到期：如果到期时还是同一条状态，收走它并返回 `true`。
    ///
    /// 期间写入过新状态（序号变了）或已经被切页清空，就什么都不做。
    pub(crate) fn expire(&mut self, seq: u64) -> bool {
        if self.seq != seq || self.since.is_none() {
            return false;
        }
        self.clear();
        true
    }
}

impl Root {
    /// 给当前这条状态挂到期计时器；计时器在渲染路径上按序号去重。
    pub(crate) fn schedule_status_clear(&mut self, cx: &mut Context<Self>) {
        let Some((seq, remaining)) = self.status_line.claim_timer(Instant::now()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(remaining).await;
            let _ = this.update(cx, |root, cx| {
                if root.status_line.expire(seq) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// 渲染状态行；没有可显示的内容时返回 `None`。
///
/// 位置固定在播放栏上方、内容区左侧：与播放栏同一个视觉底部，又不挤占播放栏
/// 自己的三栏布局。用绝对定位，出现与消失都不推挤页面。
///
/// 它不挂任何交互处理器（与 `ui::message` 的浮层同型），预期不会产生命中框，
/// 压在页面上的这几秒不该挡住下面列表的点击——这一点要在真机验收时确认。
pub(crate) fn render(root: &mut Root, cx: &mut Context<Root>) -> Option<AnyElement> {
    let text = root.status_line.visible(Instant::now())?.to_string();
    root.schedule_status_clear(cx);
    Some(
        div()
            .id("status-line")
            .absolute()
            .bottom(px(theme::PLAYER_H + theme::space::SM))
            .left(px(theme::SIDEBAR_W + theme::space::LG))
            .max_w(px(theme::STATUS_LINE_MAX_W))
            .truncate()
            .px(px(theme::space::MD))
            .py(px(theme::space::SM))
            .rounded(px(theme::radius::CARD))
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_elevated())
            .shadow_lg()
            .text_size(theme::Text::Small.size())
            .text_color(theme::text())
            .child(text)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_status_is_visible() {
        let mut line = StatusLine::default();
        line.set("已收藏：晴天".to_string());
        assert_eq!(line.visible(Instant::now()), Some("已收藏：晴天"));
    }

    #[test]
    fn empty_or_never_written_status_never_shows() {
        let mut line = StatusLine::default();
        assert_eq!(line.visible(Instant::now()), None);
        line.set("   ".to_string());
        assert_eq!(line.visible(Instant::now()), None);
    }

    #[test]
    fn expired_status_disappears() {
        let mut line = StatusLine::default();
        line.set("正在加载推荐队列…".to_string());
        assert_eq!(line.visible(Instant::now() + VISIBLE_FOR), None);
    }

    #[test]
    fn status_survives_until_the_deadline() {
        let mut line = StatusLine::default();
        line.set("已退出登录".to_string());
        assert_eq!(
            line.visible(Instant::now() + (VISIBLE_FOR / 2)),
            Some("已退出登录")
        );
    }

    #[test]
    fn a_new_status_replaces_the_old_one_and_restarts_the_timer() {
        let mut line = StatusLine::default();
        line.set("音质已设为 无损，从下一首生效".to_string());
        let first = line.claim_timer(Instant::now());
        line.set("已开启详细日志".to_string());
        let second = line.claim_timer(Instant::now());
        let (first, second) = (first.expect("first claims"), second.expect("second claims"));
        assert_ne!(first.0, second.0, "新状态要重新认领一次到期计时");
        assert_eq!(line.visible(Instant::now()), Some("已开启详细日志"));
    }

    #[test]
    fn only_the_latest_status_timer_can_expire_it() {
        let mut line = StatusLine::default();
        line.set("第一条".to_string());
        let (first, _) = line
            .claim_timer(Instant::now())
            .expect("first claims a timer");
        line.set("第二条".to_string());
        let (second, _) = line
            .claim_timer(Instant::now())
            .expect("second claims its own timer");
        assert!(!line.expire(first), "旧计时器不该收走新文案");
        assert!(line.expire(second));
        assert_eq!(line.visible(Instant::now()), None);
    }

    #[test]
    fn clearing_now_hides_the_status_for_good() {
        let mut line = StatusLine::default();
        line.set("正在搜索「晴天」…".to_string());
        let (seq, _) = line
            .claim_timer(Instant::now())
            .expect("status claims a timer");
        line.clear();
        assert_eq!(line.visible(Instant::now()), None);
        assert!(!line.expire(seq));
    }

    #[test]
    fn each_status_claims_exactly_one_timer() {
        let mut line = StatusLine::default();
        line.set("正在读取歌单…".to_string());
        assert!(line.claim_timer(Instant::now()).is_some());
        assert!(
            line.claim_timer(Instant::now()).is_none(),
            "同一条状态不该每帧再排一次"
        );
    }

    #[test]
    fn an_on_time_first_render_gets_the_whole_window() {
        let mut line = StatusLine::default();
        line.set("正在读取歌单…".to_string());
        let (_, remaining) = line
            .claim_timer(Instant::now())
            .expect("status claims a timer");
        assert!(
            remaining <= VISIBLE_FOR && remaining > VISIBLE_FOR - Duration::from_millis(100),
            "首帧准时应当排满整段窗口，实得 {remaining:?}"
        );
    }

    #[test]
    fn a_late_first_render_only_gets_the_remaining_time() {
        let mut line = StatusLine::default();
        line.set("正在加载歌单…".to_string());
        // 首次渲染晚了 4 秒：计时器只能排剩下的那 1 秒，不能重新排整段 5 秒。
        let now = Instant::now() + (VISIBLE_FOR * 4 / 5);
        let (_, remaining) = line.claim_timer(now).expect("status claims a timer");
        assert!(
            remaining <= VISIBLE_FOR / 5
                && remaining > VISIBLE_FOR / 5 - Duration::from_millis(100),
            "首帧来晚了应当只排剩余时长，实得 {remaining:?}"
        );
    }

    #[test]
    fn an_already_expired_status_claims_a_zero_timer() {
        // 渲染入口先过 `visible()`，正常路径走不到这里；真走到了就让计时器立刻到期，
        // 而不是再排一整段。
        let mut line = StatusLine::default();
        line.set("正在搜索…".to_string());
        let (_, remaining) = line
            .claim_timer(Instant::now() + VISIBLE_FOR)
            .expect("status claims a timer");
        assert_eq!(remaining, Duration::ZERO);
    }

    #[test]
    fn a_status_never_written_claims_no_timer() {
        let mut line = StatusLine::default();
        assert!(line.claim_timer(Instant::now()).is_none());
    }
}
