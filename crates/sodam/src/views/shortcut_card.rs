//! 设置页的快捷键卡片：展示三个播放动作的绑定，支持录制新组合键与恢复默认。
//!
//! 录制期间卡片自己带上 `capturing` 上下文（见 `shortcuts`），
//! 这样按下新组合键时不会顺带触发一次播放动作。

use super::*;
use crate::shortcuts::{self, Slot};

/// 把 gpui 的 keystroke 写法（`ctrl-left`）显示成人看的样子（`Ctrl+←`）。
fn display_keystroke(raw: &str) -> String {
    raw.split('-')
        .map(|part| match part {
            "cmd" => "⌘",
            "ctrl" => "Ctrl",
            "alt" => "⌥",
            "shift" => "Shift",
            "left" => "←",
            "right" => "→",
            "up" => "↑",
            "down" => "↓",
            "space" => "Space",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

fn slot_label(root: &Root, slot: Slot) -> &'static str {
    match slot {
        Slot::Toggle => root.tr("播放 / 暂停"),
        Slot::Previous => root.tr("上一首"),
        Slot::Next => root.tr("下一首"),
    }
}

/// 一行：动作名 + 当前按键 + 操作按钮。
fn shortcut_row(root: &Root, slot: Slot, cx: &mut Context<Root>) -> AnyElement {
    let capturing = root.shortcut_capture == Some(slot);
    let current = if capturing {
        root.tr("按下新的组合键…（Esc 取消）").to_string()
    } else {
        display_keystroke(shortcuts::keystroke_of(&root.settings, slot))
    };
    let customized = shortcuts::is_customized(&root.settings, slot);

    // 三行必须各有各的 id：gpui 的 `Div::id` 直接把传入值当 ElementId，不混父路径也不报错，
    // 共用字面量的话只有第一行拿到可交互节点，后两行的 on_click 永远不触发（见 #35）。
    let slot_key = match slot {
        Slot::Toggle => "toggle",
        Slot::Previous => "previous",
        Slot::Next => "next",
    };

    let action_button = |id: String, label: &'static str, primary: bool| {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(28.0))
            .px(px(theme::space::SM))
            .rounded(px(theme::radius::ROW))
            .cursor_pointer()
            .text_size(theme::Text::Small.size())
            .text_color(if primary {
                theme::accent()
            } else {
                theme::text_muted()
            })
            .hover(|style| style.bg(theme::surface_hover()))
            .child(label)
    };

    let (button_id, button_label, is_primary) = if capturing {
        (
            format!("shortcut-cancel-{slot_key}"),
            root.tr("取消"),
            false,
        )
    } else {
        (format!("shortcut-start-{slot_key}"), root.tr("重绑"), true)
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(theme::space::MD))
        .px(px(theme::space::MD))
        .py(px(theme::space::SM))
        .child(
            div()
                .text_size(theme::Text::Body.size())
                .text_color(theme::text())
                .child(slot_label(root, slot)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::space::SM))
                .child(
                    div()
                        .min_w(px(96.0))
                        .text_size(theme::Text::Small.size())
                        .text_color(if capturing {
                            theme::accent()
                        } else {
                            theme::text_muted()
                        })
                        .child(current),
                )
                .child(
                    action_button(button_id, button_label, is_primary).on_click(cx.listener(
                        move |root, _event: &ClickEvent, window, cx| {
                            if capturing {
                                root.shortcut_capture = None;
                            } else {
                                root.shortcut_capture = Some(slot);
                                window.focus(&root.shortcut_focus, cx);
                            }
                            cx.notify();
                        },
                    )),
                )
                .when(customized && !capturing, |this| {
                    this.child(
                        action_button(
                            format!("shortcut-reset-{slot_key}"),
                            root.tr("恢复默认"),
                            false,
                        )
                        .on_click(cx.listener(
                            move |root, _event: &ClickEvent, _window, cx| {
                                shortcuts::set_keystroke(&mut root.settings, slot, String::new());
                                root.shortcuts_dirty = true;
                                root.status = if root.language.is_zh() {
                                    root.tr("快捷键已恢复默认").to_string()
                                } else {
                                    root.tr("Shortcut reset to default").to_string()
                                };
                                let _ = root.settings.save();
                                cx.notify();
                            },
                        )),
                    )
                }),
        )
        .into_any_element()
}

/// 设置页 → 快捷键板块。
pub(crate) fn section(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let capturing = root.shortcut_capture.is_some();
    let rows: Vec<AnyElement> = [Slot::Toggle, Slot::Previous, Slot::Next]
        .into_iter()
        .map(|slot| shortcut_row(root, slot, cx))
        .collect();

    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::XS))
        .child(
            div()
                .text_size(theme::Text::Large.size())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(root.tr("快捷键")),
        )
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(if cfg!(target_os = "macos") {
                    // ⌃←/⌃→ 是系统「切换桌面」的默认键，绑了也收不到。
                    root.tr(
                        "播放控制：系统媒体键同样可用；⌃←/⌃→ 被系统占用（切换桌面），别绑这两个",
                    )
                    .to_string()
                } else {
                    root.tr("播放控制：系统媒体键同样可用；这里绑的是应用内快捷键")
                        .to_string()
                }),
        )
        .child(
            div()
                .id("shortcut-card")
                .track_focus(&root.shortcut_focus)
                // 录制期间带上 capturing 上下文，让播放绑定让路（否则按 Space 会顺手切换播放）。
                .when(capturing, |this| {
                    this.key_context(crate::shortcuts::CAPTURING_CONTEXT)
                })
                .on_key_down(cx.listener(|root, event: &KeyDownEvent, _window, cx| {
                    let Some(slot) = root.shortcut_capture else {
                        return;
                    };
                    let keystroke = event.keystroke.clone();
                    // Esc 取消；按住纯修饰键不会走到这里。
                    if keystroke.key == "escape" && keystroke.modifiers.number_of_modifiers() == 0 {
                        root.shortcut_capture = None;
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    let text = keystroke.to_string();
                    shortcuts::set_keystroke(&mut root.settings, slot, text);
                    root.shortcut_capture = None;
                    root.shortcuts_dirty = true;
                    root.status = if root.language.is_zh() {
                        root.tr("快捷键已更新").to_string()
                    } else {
                        root.tr("Shortcut updated").to_string()
                    };
                    let _ = root.settings.save();
                    cx.stop_propagation();
                    cx.notify();
                }))
                .flex()
                .flex_col()
                .rounded(px(theme::radius::CARD))
                .bg(theme::surface_elevated())
                .border_1()
                .border_color(if capturing {
                    theme::accent()
                } else {
                    theme::border()
                })
                .children(rows),
        )
        .into_any_element()
}
