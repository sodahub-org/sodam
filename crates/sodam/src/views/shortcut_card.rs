//! 设置页的快捷键卡片：展示三个播放动作的绑定，支持录制新组合键与恢复默认。
//!
//! 录制期间卡片自己带上 `capturing` 上下文（见 `shortcuts`），
//! 这样按下新组合键时不会顺带触发一次播放动作。

use super::*;
use crate::shortcuts::{self, Slot};

/// 存储串 → 给人看的按键形式。
///
/// 存储与显示是两套东西，这是刻意的：存储串要喂给 `Keystroke::parse`
/// （见 `shortcuts::canonical_keystroke`），显示形式按平台走 —— macOS 用 Apple 顺序的
/// 符号 `⌃⌥⇧⌘` + 键名且不带 `+`，其他平台用 `Ctrl+Alt+Shift+K`。
fn display_keystroke(raw: &str) -> String {
    let mut control = false;
    let mut alt = false;
    let mut shift = false;
    let mut platform = false;
    let mut function = false;
    let mut key = raw.trim();
    for part in raw.split('-') {
        match part {
            "ctrl" | "control" => control = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "fn" | "function" => function = true,
            // secondary 在 macOS 解析成 ⌘、其他平台解析成 Ctrl：显示跟着解析走。
            "secondary" => {
                if cfg!(target_os = "macos") {
                    platform = true;
                } else {
                    control = true;
                }
            }
            "cmd" | "command" | "super" | "win" => platform = true,
            "" => {}
            other => key = other,
        }
    }
    let key = display_key_name(key);
    if cfg!(target_os = "macos") {
        let mut out = String::new();
        if function {
            out.push_str("fn ");
        }
        if control {
            out.push('⌃');
        }
        if alt {
            out.push('⌥');
        }
        if shift {
            out.push('⇧');
        }
        if platform {
            out.push('⌘');
        }
        out.push_str(&key);
        out
    } else {
        let mut parts: Vec<&str> = Vec::with_capacity(5);
        if function {
            parts.push("Fn");
        }
        if control {
            parts.push("Ctrl");
        }
        if alt {
            parts.push("Alt");
        }
        if shift {
            parts.push("Shift");
        }
        if platform {
            parts.push("Win");
        }
        parts.push(&key);
        parts.join("+")
    }
}

/// 键名的人话形式。单字符键按惯例显示大写，但存储串里仍是小写（大写会被 parse 读成 shift）。
fn display_key_name(key: &str) -> String {
    let named = match key {
        "left" => Some("←"),
        "right" => Some("→"),
        "up" => Some("↑"),
        "down" => Some("↓"),
        "space" => Some("Space"),
        "escape" => Some("Esc"),
        "backspace" => Some("Backspace"),
        "delete" => Some("Del"),
        "enter" => Some("Enter"),
        "pageup" => Some("PgUp"),
        "pagedown" => Some("PgDn"),
        _ => None,
    };
    match named {
        Some(name) => name.to_string(),
        None if key.chars().count() == 1 => key.to_uppercase(),
        None => key.to_string(),
    }
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
    let stored = shortcuts::keystroke_of(&root.settings, slot);
    let current = if capturing {
        root.tr("按下新的组合键…（Esc 取消）").to_string()
    } else {
        display_keystroke(stored)
    };
    let customized = shortcuts::is_customized(&root.settings, slot);
    // 提示长在出问题的这一行上（`status` 现在没有任何渲染出口，见 #40）：录制期的拒绝原因，
    // 以及配置里已有的坏值（手改 config.json、或旧版本存下的符号形式）——后者不绑也不崩。
    let note = match (capturing, root.shortcut_note) {
        (true, Some(note)) => Some(root.tr(note)),
        (false, _) if !shortcuts::is_bindable_source(stored) => {
            Some(root.tr("这一行的按键无法解析，未生效"))
        }
        _ => None,
    };

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
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(theme::Text::Small.size())
                                .text_color(if capturing {
                                    theme::accent()
                                } else {
                                    theme::text_muted()
                                })
                                .child(current),
                        )
                        .children(note.map(|note| {
                            div()
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::accent())
                                .child(note)
                        })),
                )
                .child(
                    action_button(button_id, button_label, is_primary).on_click(cx.listener(
                        move |root, _event: &ClickEvent, window, cx| {
                            root.shortcut_note = None;
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
                    root.shortcut_note = None;
                    // Esc 取消；按住纯修饰键不会走到这里。
                    if keystroke.key == "escape" && keystroke.modifiers.number_of_modifiers() == 0 {
                        root.shortcut_capture = None;
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    if keystroke.modifiers.platform && !cfg!(target_os = "macos") {
                        // Windows / Linux 的徽标键归系统（Win+← 是贴边快捷键、Win+↑ 是最小化），
                        // 应用拿不到干净的按键事件，绑上去只会是个永不触发的死键，所以直接拒。
                        root.shortcut_note = Some("徽标键由系统占用，请换个组合键");
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    let stored = shortcuts::canonical_keystroke(&keystroke);
                    if let Some(conflict) =
                        shortcuts::shortcut_conflict(&root.settings, slot, &stored)
                    {
                        let message = match conflict {
                            shortcuts::ShortcutConflict::Playback(other) => {
                                let label = slot_label(root, other);
                                if root.language.is_zh() {
                                    root.localized("该组合键已被「{}」占用", &[label.to_string()])
                                } else {
                                    format!("Shortcut already used by \"{label}\"")
                                }
                            }
                            shortcuts::ShortcutConflict::MacosMenu => {
                                root.localized("该组合键由 macOS 菜单保留，不能绑定", &[])
                            }
                            shortcuts::ShortcutConflict::Fixed(feature) => {
                                let label = root.tr(feature).to_string();
                                if root.language.is_zh() {
                                    root.localized("该组合键已用于「{}」，不能绑定", &[label])
                                } else {
                                    format!("Shortcut is already used by \"{label}\"")
                                }
                            }
                        };
                        root.status = message.clone();
                        root.open_message(crate::ui::message::MessageKind::Warning, message);
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    // 规范串 #39 已经在 PR #42 中生成和过滤；这里只做绑定与提示。
                    if !shortcuts::is_bindable_source(&stored) {
                        root.shortcut_note = Some("这个键无法作为快捷键，请换个组合键");
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    shortcuts::set_keystroke(&mut root.settings, slot, stored);
                    root.shortcut_capture = None;
                    root.shortcuts_dirty = true;
                    root.status = if root.language.is_zh() {
                        root.tr("快捷键已更新").to_string()
                    } else {
                        root.tr("Shortcut updated").to_string()
                    };
                    root.open_message(
                        crate::ui::message::MessageKind::Success,
                        root.status.clone(),
                    );
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

#[cfg(test)]
mod display_tests {
    use super::*;

    /// `secondary` 必须按平台显示成该平台的主修饰键，不能写成符号糊过去。
    #[test]
    fn secondary_shows_as_the_platform_primary_modifier() {
        let shown = display_keystroke("secondary-left");
        if cfg!(target_os = "macos") {
            assert_eq!(shown, "⌘←");
        } else {
            assert_eq!(shown, "Ctrl+←");
        }
    }

    /// 修饰键顺序、键名的人话形式，按平台分两套。
    #[test]
    fn modifier_order_and_key_names() {
        let combo = display_keystroke("ctrl-shift-a");
        if cfg!(target_os = "macos") {
            assert_eq!(combo, "⌃⇧A");
        } else {
            assert_eq!(combo, "Ctrl+Shift+A");
        }
        assert_eq!(display_keystroke("space"), "Space");
        assert_eq!(display_keystroke("escape"), "Esc");
        assert_eq!(display_keystroke("pageup"), "PgUp");
        let win = display_keystroke("win-left");
        if cfg!(target_os = "macos") {
            assert!(win.contains('⌘'), "macOS 上没有徽标键，按 ⌘ 显示：{win}");
        } else {
            assert!(
                win.contains("Win"),
                "Windows / Linux 要看得出是徽标键：{win}"
            );
        }
    }

    /// 旧版存下来的符号形式不绑，界面也不能装成能用：行的判定就靠这个谓词。
    #[test]
    fn legacy_symbol_forms_are_reported_as_unusable() {
        assert!(!shortcuts::is_bindable_source("⌘←"));
        assert!(!shortcuts::is_bindable_source("^←"));
        assert!(!shortcuts::is_bindable_source("⊞k"));
        assert!(shortcuts::is_bindable_source("secondary-left"));
        assert!(shortcuts::is_bindable_source("ctrl-a"));
    }
}
