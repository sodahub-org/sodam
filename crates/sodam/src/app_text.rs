//! 搜索框的按键分发：动作定义、键位绑定、处理器。
//!
//! 走 gpui 的 keymap + action，而不是在 `on_key_down` 里自己判修饰键：
//! 这是 gpui/Zed 的惯用法（见 gpui 自带 `examples/input.rs` 与 `docs/key_dispatch.md`），
//! 好处是每个组合键有精确的修饰键匹配，绑定表也能一处读全。
//! 全部绑在 `input` 上下文上（见 [`crate::shortcuts::INPUT_CONTEXT`]），
//! 只有搜索框聚焦时才命中，所以不会和绑在 `!input` 上的播放快捷键互相抢键。
//!
//! 状态本身在 [`crate::ui::text_field::TextField`]：这里只负责把动作翻译成状态操作。

use crate::app::Root;
use crate::shortcuts::INPUT_CONTEXT;
use crate::ui::text_field::{Direction, Edge, TextField, Unit};
use gpui::{actions, Context, KeyBinding, Window};

actions!(
    text_input,
    [
        Backspace,
        Delete,
        DeleteWordBackward,
        Left,
        Right,
        WordLeft,
        WordRight,
        Home,
        End,
        Submit,
        Cancel
    ]
);

/// 搜索框的全部键位绑定。
///
/// [`crate::shortcuts::apply`] 每次重建 keymap（`clear_key_bindings` 是整表擦除，
/// gpui 没有按作用域清除的 API）之后都必须把这份重新绑上，否则编辑键会静默失效。
pub fn bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, Some(INPUT_CONTEXT)),
        KeyBinding::new("delete", Delete, Some(INPUT_CONTEXT)),
        KeyBinding::new("left", Left, Some(INPUT_CONTEXT)),
        KeyBinding::new("right", Right, Some(INPUT_CONTEXT)),
        KeyBinding::new("home", Home, Some(INPUT_CONTEXT)),
        KeyBinding::new("end", End, Some(INPUT_CONTEXT)),
        KeyBinding::new("enter", Submit, Some(INPUT_CONTEXT)),
        KeyBinding::new("escape", Cancel, Some(INPUT_CONTEXT)),
    ];
    if cfg!(target_os = "macos") {
        // macOS 的惯例：`⌥←/→` 按词移动，`⌘←/→` 是行首/行尾（与 shortcuts.rs
        // 顶部注释里「不把 ⌘←/⌘→ 用作播放键」的理由一致），`⌥退格` 删一个词。
        bindings.extend([
            KeyBinding::new("alt-left", WordLeft, Some(INPUT_CONTEXT)),
            KeyBinding::new("alt-right", WordRight, Some(INPUT_CONTEXT)),
            KeyBinding::new("alt-backspace", DeleteWordBackward, Some(INPUT_CONTEXT)),
            KeyBinding::new("cmd-left", Home, Some(INPUT_CONTEXT)),
            KeyBinding::new("cmd-right", End, Some(INPUT_CONTEXT)),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-left", WordLeft, Some(INPUT_CONTEXT)),
            KeyBinding::new("ctrl-right", WordRight, Some(INPUT_CONTEXT)),
            KeyBinding::new("ctrl-backspace", DeleteWordBackward, Some(INPUT_CONTEXT)),
        ]);
    }
    bindings
}

impl Root {
    /// 改一处文本状态：真有变化才通知重绘。
    fn edit_text(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut TextField) -> bool) {
        if edit(&mut self.search_field) {
            cx.notify();
        }
    }

    pub(crate) fn text_backspace(
        &mut self,
        _: &Backspace,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(cx, |field| field.delete(Direction::Backward, Unit::Char));
    }

    pub(crate) fn text_delete(&mut self, _: &Delete, _window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(cx, |field| field.delete(Direction::Forward, Unit::Char));
    }

    pub(crate) fn text_delete_word_backward(
        &mut self,
        _: &DeleteWordBackward,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(cx, |field| field.delete(Direction::Backward, Unit::Word));
    }

    pub(crate) fn text_left(&mut self, _: &Left, _window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(cx, |field| {
            field.move_caret(Direction::Backward, Unit::Char)
        });
    }

    pub(crate) fn text_right(&mut self, _: &Right, _window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(cx, |field| field.move_caret(Direction::Forward, Unit::Char));
    }

    pub(crate) fn text_word_left(
        &mut self,
        _: &WordLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(cx, |field| {
            field.move_caret(Direction::Backward, Unit::Word)
        });
    }

    pub(crate) fn text_word_right(
        &mut self,
        _: &WordRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(cx, |field| field.move_caret(Direction::Forward, Unit::Word));
    }

    pub(crate) fn text_home(&mut self, _: &Home, _window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(cx, |field| field.move_caret_to(Edge::Start));
    }

    pub(crate) fn text_end(&mut self, _: &End, _window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(cx, |field| field.move_caret_to(Edge::End));
    }

    pub(crate) fn text_submit(&mut self, _: &Submit, _window: &mut Window, cx: &mut Context<Self>) {
        self.run_search(cx);
    }

    pub(crate) fn text_cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(cx, |field| field.clear());
    }
}
