//! 搜索框的按键分发：动作定义、键位绑定、处理器。
//!
//! 走 gpui 的 keymap + action，而不是在 `on_key_down` 里自己判修饰键：
//! 这是 gpui/Zed 的惯用法（见 gpui 自带 `examples/input.rs` 与 `docs/key_dispatch.md`），
//! 好处是每个组合键有精确的修饰键匹配，绑定表也能一处读全。
//! 全部绑在 `input` 上下文上（见 [`crate::shortcuts::INPUT_CONTEXT`]），
//! 只有搜索框聚焦时才命中，所以不会和绑在 `!input` 上的播放快捷键互相抢键。
//!
//! 状态本身在 [`crate::ui::text_field::TextField`]：这里只负责把动作翻译成状态操作，
//! 以及与系统剪贴板打交道 —— `TextField` 刻意不碰剪贴板，它才能脱离窗口单测。
//!
//! gpui 匹配修饰键是**精确**的：`ctrl-a` 不会命中 `ctrl-shift-a`，所以每个变体都得单独绑一条。

use crate::app::Root;
use crate::shortcuts::INPUT_CONTEXT;
use crate::ui::text_field::{Direction, Edge, TextField, Unit};
use gpui::{actions, ClipboardItem, Context, KeyBinding, Window};

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
        SelectLeft,
        SelectRight,
        SelectWordLeft,
        SelectWordRight,
        Home,
        End,
        SelectHome,
        SelectEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Submit,
        Cancel
    ]
);

/// 搜索框的全部键位绑定。
///
/// [`crate::shortcuts::apply`] 每次重建 keymap（`clear_key_bindings` 是整表擦除，
/// gpui 没有按作用域清除的 API）之后都必须把这份重新绑上，否则编辑键会静默失效。
pub fn bindings() -> Vec<KeyBinding> {
    let ctx = Some(INPUT_CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new("escape", Cancel, ctx),
        // Shift 扩选与移动键一一对应，语义是「锚点不动、活动端走」。
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        // 全选与剪贴板：`secondary-` 在编译期就是 macOS 的 ⌘ / 其他平台的 Ctrl。
        KeyBinding::new("secondary-a", SelectAll, ctx),
        KeyBinding::new("secondary-c", Copy, ctx),
        KeyBinding::new("secondary-x", Cut, ctx),
        KeyBinding::new("secondary-v", Paste, ctx),
    ];
    if cfg!(target_os = "macos") {
        // macOS 的惯例：`⌥←/→` 按词移动，`⌘←/→` 是行首/行尾（与 shortcuts.rs 顶部注释里
        // 「不把 ⌘←/⌘→ 用作播放键」的理由一致），`⌥退格` 删一个词。
        bindings.extend([
            KeyBinding::new("alt-left", WordLeft, ctx),
            KeyBinding::new("alt-right", WordRight, ctx),
            KeyBinding::new("alt-backspace", DeleteWordBackward, ctx),
            KeyBinding::new("cmd-left", Home, ctx),
            KeyBinding::new("cmd-right", End, ctx),
            KeyBinding::new("alt-shift-left", SelectWordLeft, ctx),
            KeyBinding::new("alt-shift-right", SelectWordRight, ctx),
            KeyBinding::new("cmd-shift-left", SelectHome, ctx),
            KeyBinding::new("cmd-shift-right", SelectEnd, ctx),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-left", WordLeft, ctx),
            KeyBinding::new("ctrl-right", WordRight, ctx),
            KeyBinding::new("ctrl-backspace", DeleteWordBackward, ctx),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, ctx),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, ctx),
        ]);
    }
    bindings
}

impl Root {
    /// 改一处文本状态：真有变化才把活动端拉进可视区并重绘。
    fn edit_text(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut TextField) -> bool,
    ) {
        if edit(&mut self.search_field) {
            self.search_fit_caret_in_view(window);
            cx.notify();
        }
    }

    pub(crate) fn text_backspace(
        &mut self,
        _: &Backspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| {
            field.delete(Direction::Backward, Unit::Char)
        });
    }

    pub(crate) fn text_delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(window, cx, |field| {
            field.delete(Direction::Forward, Unit::Char)
        });
    }

    pub(crate) fn text_delete_word_backward(
        &mut self,
        _: &DeleteWordBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| {
            field.delete(Direction::Backward, Unit::Word)
        });
    }

    pub(crate) fn text_left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(window, cx, |field| {
            field.move_caret(Direction::Backward, Unit::Char)
        });
    }

    pub(crate) fn text_right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(window, cx, |field| {
            field.move_caret(Direction::Forward, Unit::Char)
        });
    }

    pub(crate) fn text_word_left(
        &mut self,
        _: &WordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| {
            field.move_caret(Direction::Backward, Unit::Word)
        });
    }

    pub(crate) fn text_word_right(
        &mut self,
        _: &WordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| {
            field.move_caret(Direction::Forward, Unit::Word)
        });
    }

    pub(crate) fn text_select_left(
        &mut self,
        _: &SelectLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| {
            field.extend_selection(Direction::Backward, Unit::Char)
        });
    }

    pub(crate) fn text_select_right(
        &mut self,
        _: &SelectRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| {
            field.extend_selection(Direction::Forward, Unit::Char)
        });
    }

    pub(crate) fn text_select_word_left(
        &mut self,
        _: &SelectWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| {
            field.extend_selection(Direction::Backward, Unit::Word)
        });
    }

    pub(crate) fn text_select_word_right(
        &mut self,
        _: &SelectWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| {
            field.extend_selection(Direction::Forward, Unit::Word)
        });
    }

    pub(crate) fn text_home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(window, cx, |field| field.move_caret_to(Edge::Start));
    }

    pub(crate) fn text_end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_text(window, cx, |field| field.move_caret_to(Edge::End));
    }

    pub(crate) fn text_select_home(
        &mut self,
        _: &SelectHome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| field.extend_to_edge(Edge::Start));
    }

    pub(crate) fn text_select_end(
        &mut self,
        _: &SelectEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| field.extend_to_edge(Edge::End));
    }

    pub(crate) fn text_select_all(
        &mut self,
        _: &SelectAll,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_text(window, cx, |field| field.select_all());
    }

    /// 无选区时什么都不复制：把整串查询悄悄写进剪贴板，用户下次粘贴时会吃个意外。
    pub(crate) fn text_copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.search_field.selected_text();
        if selected.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(selected));
    }

    pub(crate) fn text_cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.search_field.selected_text();
        if selected.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(selected));
        // 有选区时 delete 走「删掉整段」那条分支，两个方向等价。
        self.edit_text(window, cx, |field| {
            field.delete(Direction::Backward, Unit::Char)
        });
    }

    pub(crate) fn text_paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.edit_text(window, cx, |field| field.paste(&text));
    }

    pub(crate) fn text_submit(&mut self, _: &Submit, _window: &mut Window, cx: &mut Context<Self>) {
        self.run_search(cx);
    }

    /// Esc 的三级优先：先撤 IME 组合，再折叠选区，最后才清空整串（三者都不夺焦点）。
    pub(crate) fn text_cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.clear_marked() {
            cx.notify();
            return;
        }
        if self.search_field.collapse_selection() {
            cx.notify();
            return;
        }
        self.edit_text(window, cx, |field| field.clear());
    }
}
