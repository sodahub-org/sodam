//! 搜索框的按键分发：动作定义、键位绑定、处理器。
//!
//! 走 gpui 的 keymap + action，而不是在 `on_key_down` 里自己判修饰键：
//! 这是 gpui/Zed 的惯用法（见 gpui 自带 `examples/input.rs` 与 `docs/key_dispatch.md`），
//! 好处是每个组合键有精确的修饰键匹配，绑定表也能一处读全。
//! 全部编辑键绑在 `input` 上下文上（见 [`crate::shortcuts::INPUT_CONTEXT`]），
//! 只有搜索框聚焦时才命中，所以不会和绑在 `!input` 上的播放快捷键互相抢键。
//! 唯一的例外是 [`focus_bindings`]：那是「把焦点交给搜索框」的全局键，反过来绑在
//! `!input` 上 —— 已经在框里打字时不该再被它抢走。
//!
//! 状态本身在 [`crate::ui::text_field::TextField`]：这里只负责把动作翻译成状态操作，
//! 以及与系统剪贴板打交道 —— `TextField` 刻意不碰剪贴板，它才能脱离窗口单测。
//!
//! gpui 匹配修饰键是**精确**的：`ctrl-a` 不会命中 `ctrl-shift-a`，所以每个变体都得单独绑一条。

use crate::app::{Nav, Root};
use crate::shortcuts::{GLOBAL_CONTEXT, INPUT_CONTEXT};
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
        Undo,
        Redo,
        Submit,
        Cancel,
        FocusSearch
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
        KeyBinding::new("secondary-z", Undo, ctx),
        KeyBinding::new("secondary-shift-z", Redo, ctx),
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
            // Windows/Linux 的第二个重做键（`Ctrl+Y`）；macOS 只有 `⌘⇧Z`。
            KeyBinding::new("ctrl-y", Redo, ctx),
        ]);
    }
    bindings
}

/// 「聚焦搜索框」的固定绑定（不可在设置页重绑，见 `shortcuts::reserved_feature`）。
///
/// `secondary-` 由 gpui 在编译期解释成 macOS 的 ⌘ / 其他平台的 Ctrl，所以不必分平台写两条。
/// [`crate::shortcuts::apply`] 重建 keymap 之后必须一起重发，理由同 [`bindings()`]。
pub fn focus_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new(
        crate::shortcuts::FOCUS_SEARCH_SOURCE,
        FocusSearch,
        Some(GLOBAL_CONTEXT),
    )]
}

impl Root {
    /// 改一处文本状态：先开撤销事务，真有变化才通知重绘。
    ///
    /// 事务的边界就放在这个唯一的收口处：`begin_edit` 记下改动之前的完整状态，`end_edit`
    /// 按类别决定并入上一条还是新开一条。只动光标不动文本的按键（方向键、Shift 扩选）
    /// 会被 `end_edit` 整条丢弃，不会污染撤销历史。
    fn edit_text(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut TextField) -> bool,
    ) {
        let now = self.search_edit_time();
        self.search_field.begin_edit();
        let changed = edit(&mut self.search_field);
        self.search_field.end_edit(now);
        if changed {
            self.search_fit_caret_in_view(window);
            cx.notify();
        }
    }

    /// 撤销与重做不走 `edit_text`：要恢复哪一份快照由状态层自己决定，不需要再开事务。
    pub(crate) fn text_undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.undo() {
            self.search_fit_caret_in_view(window);
            cx.notify();
        }
    }

    pub(crate) fn text_redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.redo() {
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

    /// `Ctrl+F` / `⌘F`：切到搜索页，并把焦点交给搜索框。
    ///
    /// 焦点不在这里直接落。从别的页过来时，搜索框是**下一帧**才进 dispatch tree 的
    /// （`track_focus` 在渲染时才登记），此刻 `window.focus` 只能记下 id，`input` 上下文
    /// 要等树里有这个节点才组得出来。所以只置标记，由 `Root::render` 开头那处去 focus ——
    /// 那里已经是本仓库验证过的时序（没有元素聚焦时收回 `workspace_focus`）。
    pub(crate) fn focus_search(
        &mut self,
        _: &FocusSearch,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 走统一页面切换：状态行、录制态这些「页内残留」都由 `on_nav_changed` 收走。
        self.set_nav(Nav::Search, cx);
        self.search_focus_pending = true;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 键串解析不出 keystroke 时，`KeyBinding` 会静默变成死绑定（上下文串才是直接 unwrap 的），
    /// 所以这里数的不是「有没有一条绑定」，而是「它到底带不带一个能按的键」。
    #[test]
    fn focus_binding_resolves_to_a_keystroke() {
        let bindings = focus_bindings();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].keystrokes().len(), 1);
    }
}
