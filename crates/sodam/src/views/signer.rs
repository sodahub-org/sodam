//! 设置页里的「签名服务」卡片：环境检测、依赖下载、启动/停止。

use gpui::prelude::*;
use gpui::{div, px, AnyElement, ClickEvent, Context, IntoElement};

use crate::app::Root;
use crate::ui::theme;

fn status_row(label: String, ok: bool, detail: String) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .py(px(6.0))
        .text_size(theme::Text::Small.size())
        .child(div().text_color(theme::text()).child(label))
        .child(
            div()
                .text_color(if ok {
                    theme::accent()
                } else {
                    theme::text_muted()
                })
                .child(detail),
        )
        .into_any_element()
}

fn button(
    id: &'static str,
    label: String,
    cx: &mut Context<Root>,
    action: fn(&mut Root, &mut Context<Root>),
) -> AnyElement {
    div()
        .id(id)
        .h(px(30.0))
        .px(px(12.0))
        .rounded(px(8.0))
        .bg(theme::surface_hover())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_selected()))
        .flex()
        .items_center()
        .text_size(theme::Text::Small.size())
        .text_color(theme::text())
        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
            action(root, cx);
            cx.notify();
        }))
        .child(label)
        .into_any_element()
}

/// 签名服务卡片。
pub fn signer_card(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let status = root.signer_status.clone();
    let busy = root.signer_busy;
    let detail = |ok: bool, text: &str| {
        if ok {
            text.to_string()
        } else {
            "未就绪".to_string()
        }
    };

    let rows = vec![
        status_row(
            root.tr("wine").to_string(),
            status.wine.is_some(),
            status.wine.clone().unwrap_or_else(|| "未安装".to_string()),
        ),
        status_row(
            root.tr("Node (Windows)").to_string(),
            status.node,
            detail(status.node, "已就绪"),
        ),
        status_row(
            root.tr("签名文件").to_string(),
            status.vendor,
            detail(status.vendor, "已就绪"),
        ),
        status_row(
            root.tr("libmssdk").to_string(),
            status.libmssdk,
            detail(status.libmssdk, "已就绪"),
        ),
        status_row(
            root.tr("7z").to_string(),
            status.seven_zip,
            detail(status.seven_zip, "已就绪"),
        ),
        status_row(
            root.tr("服务").to_string(),
            status.ready,
            if status.ready {
                status.url.clone()
            } else if status.running {
                root.tr("启动中…").to_string()
            } else {
                root.tr("未运行").to_string()
            },
        ),
    ];

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .p(px(16.0))
        .rounded(px(theme::radius::CARD))
        .bg(theme::surface_elevated())
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .text_size(theme::Text::Body.size())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(root.tr("签名服务（写接口 / 整曲取流）")),
        )
        .child(
            div()
                .text_size(theme::Text::Tiny.size())
                .text_color(theme::text_faint())
                .child(root.tr(
                    "需要 wine、Windows 版 Node、libmssdk 与官方客户端里的 bdms.node/metasecml.dll。\
安装包不随本软件分发：请自行下载官方安装包，放到下面路径后点「提取签名模块」。\
签名覆盖整曲取流与收藏；评论/点赞还需客户端的 bdticket 会话签名，当前未接入。",
                )),
        )
        .child(
            div()
                .text_size(theme::Text::Tiny.size())
                .text_color(theme::text_faint())
                .child(format!(
                    "{}：{}",
                    root.tr("安装包路径"),
                    sodam_core::signer::installer_cache().display()
                )),
        )
        .children(rows)
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(8.0))
                .child(button("signer-refresh", root.tr("重新检测").to_string(), cx, |root, cx| {
                    root.refresh_signer(cx);
                }))
                .child(button("signer-node", root.tr("下载 Node").to_string(), cx, |root, cx| {
                    root.signer_install_node(cx);
                }))
                .child(button(
                    "signer-libmssdk",
                    root.tr("下载 libmssdk").to_string(),
                    cx,
                    |root, cx| root.signer_install_libmssdk(cx),
                ))
                .child(button(
                    "signer-extract",
                    root.tr("提取签名模块").to_string(),
                    cx,
                    |root, cx| root.signer_extract_installer(cx),
                ))
                .child(button("signer-start", root.tr("启动服务").to_string(), cx, |root, cx| {
                    root.signer_start(cx);
                }))
                .child(button("signer-stop", root.tr("停止服务").to_string(), cx, |root, cx| {
                    root.signer_stop(cx);
                })),
        )
        .when(busy, |this| {
            this.child(
                div()
                    .text_size(theme::Text::Tiny.size())
                    .text_color(theme::accent())
                    .child(root.tr("正在执行…")),
            )
        })
        .when_some(status.last_error.clone(), |this, error| {
            this.child(
                div()
                    .text_size(theme::Text::Tiny.size())
                    .text_color(theme::text_muted())
                    .child(error),
            )
        })
        .into_any_element()
}
