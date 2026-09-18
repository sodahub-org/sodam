//! 登录二维码弹层。

use super::*;
/// 登录 modal：居中卡片 + 二维码 + 扫码提示 + 关闭。
pub fn login_modal(root: &Root, cx: &mut Context<Root>) -> gpui::AnyElement {
    use gpui::deferred;

    let card = match &root.login {
        LoginState::Idle | LoginState::Loading => div()
            .w(px(320.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(theme::space::LG))
            .p(px(theme::space::XL))
            .rounded(px(theme::radius::CARD))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .child(crate::ui::spinner::loading_state(
                root.tr("正在创建二维码…"),
            ))
            .child(close_button(root.language, cx))
            .into_any_element(),
        LoginState::Waiting {
            matrix,
            status,
            token,
        } => div()
            .w(px(360.0))
            .flex()
            .flex_col()
            .items_center()
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
                    .child(root.tr("扫码登录")),
            )
            .child(
                div()
                    .p(px(theme::space::SM))
                    .rounded(px(theme::radius::CARD))
                    .bg(gpui::white())
                    .child(qr_canvas(matrix.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(theme::space::XS))
                    .child(
                        div()
                            .text_size(theme::Text::Small.size())
                            .text_color(theme::text())
                            .child(root.tr("请使用汽水音乐 App 扫码登录")),
                    )
                    .child(
                        div()
                            .text_size(theme::Text::Tiny.size())
                            .text_color(theme::text_muted())
                            .child(status.clone()),
                    )
                    .child(
                        div()
                            .text_size(theme::Text::Tiny.size())
                            .text_color(theme::text_faint())
                            .child(root.language.textf(
                                root.tr("会话 {}…"),
                                &[token.chars().take(10).collect::<String>()],
                            )),
                    ),
            )
            .child(close_button(root.language, cx))
            .into_any_element(),
        LoginState::LoggedIn(_) | LoginState::Failed(_) => div()
            .w(px(320.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(theme::space::LG))
            .p(px(theme::space::XL))
            .rounded(px(theme::radius::CARD))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .child(
                div()
                    .text_size(theme::Text::Small.size())
                    .text_color(theme::text_muted())
                    .child(match &root.login {
                        LoginState::Failed(err) => root
                            .language
                            .textf("登录失败：{err}", std::slice::from_ref(err)),
                        _ => root.tr("已登录").to_string(),
                    }),
            )
            .child(close_button(root.language, cx))
            .into_any_element(),
    };

    let scrim = div()
        .id("login-modal")
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
                root.login_modal_open = false;
                cx.notify();
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

fn close_button(language: crate::ui::i18n::Language, cx: &mut Context<Root>) -> impl IntoElement {
    div()
        .id("login-close")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::space::XS))
        .px(px(theme::space::MD))
        .h(px(theme::size::CONTROL_SM))
        .rounded(px(theme::radius::PILL))
        .bg(theme::surface_hover())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_selected()))
        .text_size(theme::Text::Tiny.size())
        .text_color(theme::text_muted())
        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
            root.login_modal_open = false;
            cx.notify();
        }))
        .child(language.text("关闭"))
}
