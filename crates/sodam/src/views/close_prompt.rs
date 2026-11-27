//! 关窗询问弹窗：点关闭按钮时问一句「退出还是收进托盘」。

use super::*;
use sodam_core::config::CloseAction;

/// 关窗询问：居中卡片 + 两个选择 + 取消。
///
/// 托盘不可用时不给「隐藏到托盘」：藏起来就找不回来了。
pub fn close_prompt(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    use gpui::deferred;

    let Some(prompt) = root.close_prompt else {
        return div().into_any_element();
    };
    // 现读而不是弹窗打开时快照：托盘可能在弹窗开着的时候下线。
    let tray_available = root.tray_available_now();
    let remember = prompt.remember;

    let mut card = div()
        .w(px(360.0))
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .p(px(theme::space::XL))
        .rounded(px(theme::radius::CARD))
        .bg(theme::surface_elevated())
        .border_1()
        .border_color(theme::border())
        .shadow_lg()
        .child(
            div()
                .text_size(theme::Text::Title.size())
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::text())
                .child(root.tr("关闭 SodaM？")),
        )
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(if tray_available {
                    root.tr("关闭窗口后应用会继续在后台播放，可以从系统托盘重新打开。")
                } else {
                    root.tr("当前桌面没有系统托盘，隐藏后将无法找回窗口，所以只能退出。")
                }),
        )
        .child(
            div()
                .id("close-prompt-remember")
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::space::SM))
                .cursor_pointer()
                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                    if let Some(prompt) = root.close_prompt.as_mut() {
                        prompt.remember = !prompt.remember;
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .size(px(theme::ICON))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(theme::radius::ART))
                        .border_1()
                        .border_color(if remember {
                            theme::accent()
                        } else {
                            theme::border()
                        })
                        .when(remember, |this| this.bg(theme::accent()))
                        .when(remember, |this| {
                            this.child(
                                svg()
                                    .path(icons::path("check"))
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::accent_foreground()),
                            )
                        }),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.tr("记住我的选择")),
                ),
        );

    let cancel = div()
        .id("close-prompt-cancel")
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(theme::size::CONTROL))
        .px(px(theme::space::LG))
        .rounded(px(theme::radius::PILL))
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
            root.cancel_close_prompt(cx);
        }))
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(root.tr("取消")),
        );

    let hide = div()
        .id("close-prompt-hide")
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(theme::size::CONTROL))
        .px(px(theme::space::LG))
        .rounded(px(theme::radius::PILL))
        .bg(theme::surface_hover())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_selected()))
        .on_click(cx.listener(|root, _event: &ClickEvent, window, cx| {
            root.resolve_close_prompt(CloseAction::Hide, window, cx);
        }))
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text())
                .child(root.tr("隐藏到托盘")),
        );

    let quit = div()
        .id("close-prompt-quit")
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(theme::size::CONTROL))
        .px(px(theme::space::LG))
        .rounded(px(theme::radius::PILL))
        .bg(theme::accent())
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .on_click(cx.listener(|root, _event: &ClickEvent, window, cx| {
            root.resolve_close_prompt(CloseAction::Quit, window, cx);
        }))
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::accent_foreground())
                .child(root.tr("退出应用")),
        );

    let mut buttons = div()
        .flex()
        .flex_row()
        .justify_end()
        .gap(px(theme::space::SM))
        .child(cancel);
    if tray_available {
        buttons = buttons.child(hide);
    }
    buttons = buttons.child(quit);
    card = card.child(buttons);

    let scrim = div()
        .id("close-prompt-scrim")
        .absolute()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::black())
        .opacity(0.55)
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|root, _event, _window, cx| {
                root.cancel_close_prompt(cx);
            }),
        );

    deferred(
        div()
            .absolute()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(scrim)
            .child(card),
    )
    .with_priority(10)
    .into_any_element()
}
