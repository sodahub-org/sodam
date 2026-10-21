//! 自绘输入框的纯状态层：文本、插入点、IME 组合区间、词边界。
//!
//! 这里刻意不出现 gpui 类型，也不读时钟、不碰剪贴板：整套状态能脱离窗口构造并单测，
//! 渲染与按键分发留在 `app_text` 与 `views::search`。选区、滚动偏移、撤销栈之后
//! 继续加在这个结构上，为的就是保住这条可测边界。

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// 移动或删除的单位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Char,
    Word,
}

/// 移动方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Backward,
    Forward,
}

/// 文本端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// 词分段里一段的性质。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// 可整体选中或跳过的内容：一个拉丁词，或一段连续的同种汉字/假名/谚文。
    Word,
    /// 空格与标点。
    Separator,
}

/// 文本分段结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub range: Range<usize>,
    pub kind: SegmentKind,
}

/// 需要按「连续串整体成词」处理的文字种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Han,
    Kana,
    Hangul,
}

fn script_of(ch: char) -> Option<Script> {
    match ch {
        '\u{3400}'..='\u{4dbf}'
        | '\u{4e00}'..='\u{9fff}'
        | '\u{f900}'..='\u{faff}'
        | '\u{20000}'..='\u{2a6df}' => Some(Script::Han),
        '\u{3040}'..='\u{309f}' | '\u{30a0}'..='\u{30ff}' => Some(Script::Kana),
        '\u{1100}'..='\u{11ff}' | '\u{ac00}'..='\u{d7af}' => Some(Script::Hangul),
        _ => None,
    }
}

/// 把插入点夹到合法位置：不超过末尾，也不落在多字节字符中间。
pub fn clamp_cursor(text: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(text.len());
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

/// 前进/后退一个字符后的插入点（按 UTF-8 边界，不会切坏中文）。
fn step_char(text: &str, cursor: usize, forward: bool) -> usize {
    let cursor = clamp_cursor(text, cursor);
    if forward {
        text[cursor..]
            .chars()
            .next()
            .map(|ch| cursor + ch.len_utf8())
            .unwrap_or(cursor)
    } else {
        text[..cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0)
    }
}

/// `rest` 开头那种文字连续有多长（字节）。
fn script_run_len(rest: &str, script: Script) -> usize {
    let mut len = 0usize;
    for ch in rest.chars() {
        if script_of(ch) != Some(script) {
            break;
        }
        len += ch.len_utf8();
    }
    len
}

/// 第一个属于 CJK 文字的字节下标。
fn first_cjk_offset(text: &str) -> Option<usize> {
    text.char_indices()
        .find(|(_, ch)| script_of(*ch).is_some())
        .map(|(index, _)| index)
}

/// 把文本切成词与分隔符。
///
/// 拉丁与数字走 UAX#29，所以 `don't`、`e-mail`、`v2.0` 里的撇号、连字符、数字不会被误切；
/// 汉字/假名/谚文按「同种文字的连续串」整体成词——没有词典可用（jieba 类分词不在依赖树里，
/// 而本仓库是 `--offline` 构建），这与 Windows 记事本和 VS Code 对中文的实际行为一致：
/// 双击「我喜欢的音乐」整串选中，遇到空格、标点或异种文字才断开。
pub fn segments(text: &str) -> Vec<Segment> {
    let mut result = Vec::new();
    let mut index = 0usize;
    while index < text.len() {
        let rest = &text[index..];
        let Some(first) = rest.chars().next() else {
            break;
        };
        if let Some(script) = script_of(first) {
            let len = script_run_len(rest, script);
            result.push(Segment {
                range: index..index + len,
                kind: SegmentKind::Word,
            });
            index += len;
            continue;
        }
        // 非 CJK 区域整段交给 UAX#29；词与词之间的空隙（空格、标点）单独成分隔符段。
        let region_len = first_cjk_offset(rest).unwrap_or(rest.len());
        let region = &rest[..region_len];
        let mut cursor = 0usize;
        for (start, word) in region.unicode_word_indices() {
            let end = start + word.len();
            if start > cursor {
                result.push(Segment {
                    range: index + cursor..index + start,
                    kind: SegmentKind::Separator,
                });
            }
            result.push(Segment {
                range: index + start..index + end,
                kind: SegmentKind::Word,
            });
            cursor = end;
        }
        if cursor < region_len {
            result.push(Segment {
                range: index + cursor..index + region_len,
                kind: SegmentKind::Separator,
            });
        }
        index += region_len;
    }
    result
}

/// 从 `from` 往指定方向走一个词后的字节下标；到头/到底时返回原位置。
pub fn next_word_offset(text: &str, from: usize, direction: Direction) -> usize {
    let from = clamp_cursor(text, from);
    let words = segments(text)
        .into_iter()
        .filter(|segment| segment.kind == SegmentKind::Word);
    match direction {
        // 落在词中间先补到词尾；落在空格/标点里则整段跳过，落到下一个词尾。
        Direction::Forward => words
            .filter(|segment| segment.range.end > from)
            .map(|segment| segment.range.end)
            .next()
            .unwrap_or(text.len()),
        // 反向同理：先回到当前词首，再跳过分隔符落到上一个词首。
        Direction::Backward => words
            .rev()
            .filter(|segment| segment.range.start < from)
            .map(|segment| segment.range.start)
            .next()
            .unwrap_or(0),
    }
}

/// 排序并夹好一段字节区间，供所有替换路径共用。
fn normalized_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = clamp_cursor(text, range.start.min(range.end));
    let end = clamp_cursor(text, range.start.max(range.end));
    start..end.max(start)
}

/// 单行输入框的状态。插入点与组合区间都是 UTF-8 字节下标。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextField {
    text: String,
    caret: usize,
    /// Fcitx/IBus 等 IME 的预编辑区间。
    marked: Option<Range<usize>>,
}

impl TextField {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    /// 整体替换文本（程序化赋值，例如启动时的 `SODAM_QUERY`）；插入点夹进新文本里。
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.caret = clamp_cursor(&self.text, self.caret);
        self.marked = None;
    }

    /// 清空文本，返回是否真有内容被清掉。
    pub fn clear(&mut self) -> bool {
        let changed = !self.text.is_empty();
        self.text.clear();
        self.caret = 0;
        self.marked = None;
        changed
    }

    /// 把插入点设到指定字节位置（自动夹到字符边界），返回是否移动了。
    ///
    /// 按键移动与鼠标点击都会清掉 IME 组合区间：预编辑还挂在原处会误导候选窗定位。
    pub fn set_caret(&mut self, caret: usize) -> bool {
        let next = clamp_cursor(&self.text, caret);
        let moved = next != self.caret;
        self.caret = next;
        self.marked = None;
        moved
    }

    /// 按字符或按词移动插入点，返回是否移动了。
    pub fn move_caret(&mut self, direction: Direction, unit: Unit) -> bool {
        let forward = direction == Direction::Forward;
        let next = match unit {
            Unit::Char => step_char(&self.text, self.caret, forward),
            Unit::Word => next_word_offset(&self.text, self.caret, direction),
        };
        self.set_caret(next)
    }

    /// 跳到文本首/尾，返回是否移动了。
    pub fn move_caret_to(&mut self, edge: Edge) -> bool {
        let next = match edge {
            Edge::Start => 0,
            Edge::End => self.text.len(),
        };
        self.set_caret(next)
    }

    /// 删掉插入点与目标位置之间的一段，返回是否删到了东西。
    pub fn delete(&mut self, direction: Direction, unit: Unit) -> bool {
        let from = match direction {
            Direction::Backward => match unit {
                Unit::Char => step_char(&self.text, self.caret, false),
                Unit::Word => next_word_offset(&self.text, self.caret, Direction::Backward),
            },
            Direction::Forward => match unit {
                Unit::Char => step_char(&self.text, self.caret, true),
                Unit::Word => next_word_offset(&self.text, self.caret, Direction::Forward),
            },
        };
        let range = normalized_range(&self.text, self.caret..from);
        if range.start == range.end {
            return false;
        }
        self.text.replace_range(range.clone(), "");
        self.caret = range.start;
        self.marked = None;
        true
    }

    /// 替换一段文本，插入点落到插入内容之后；没有组合区间时兜底到**插入点**。
    ///
    /// 兜底不能是文本末尾：Windows 的可输入字符走 WM_CHAR，带的是 `None`，
    /// 兜底成末尾会让「点到中间再打字」变成追加到尾部。
    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str) -> bool {
        let range = normalized_range(
            &self.text,
            range
                .or_else(|| self.marked.clone())
                .unwrap_or(self.caret..self.caret),
        );
        let changed = !text.is_empty() || range.start != range.end;
        let start = range.start;
        self.text.replace_range(range, text);
        self.caret = clamp_cursor(&self.text, start + text.len());
        self.marked = None;
        changed
    }

    /// IME 上屏中间态：替换后把新内容记成组合区间，候选窗据此定位。
    pub fn replace_and_mark(&mut self, range: Option<Range<usize>>, text: &str) -> bool {
        let changed = self.replace(range, text);
        let end = self.caret;
        let start = end.saturating_sub(text.len());
        self.marked = (end > start).then_some(start..end);
        changed
    }

    /// 结束组合（`unmark_text`），返回是否本来就在组合中。
    pub fn clear_marked(&mut self) -> bool {
        self.marked.take().is_some()
    }

    pub fn utf16_len(&self) -> usize {
        self.text.chars().map(char::len_utf16).sum()
    }

    /// UTF-16 偏移 → 字节下标（gpui 的输入协议用 UTF-16，中文和 emoji 不等宽）。
    pub fn utf16_to_byte(&self, offset: usize) -> usize {
        let mut units = 0usize;
        for (byte, ch) in self.text.char_indices() {
            if units >= offset {
                return byte;
            }
            units += ch.len_utf16();
        }
        self.text.len()
    }

    /// 字节下标 → UTF-16 偏移。
    pub fn byte_to_utf16(&self, offset: usize) -> usize {
        self.text
            .char_indices()
            .take_while(|(byte, _)| *byte < offset)
            .map(|(_, ch)| ch.len_utf16())
            .sum()
    }

    pub fn byte_range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        normalized_range(
            &self.text,
            self.utf16_to_byte(range.start)..self.utf16_to_byte(range.end),
        )
    }

    pub fn utf16_range_from_byte(&self, range: Range<usize>) -> Range<usize> {
        self.byte_to_utf16(range.start)..self.byte_to_utf16(range.end)
    }

    /// 当前该上报给系统的区间：组合中用组合区间，否则是插入点的折叠区间。
    pub fn selection_range(&self) -> Range<usize> {
        self.marked.clone().unwrap_or(self.caret..self.caret)
    }

    pub fn slice(&self, range: Range<usize>) -> String {
        self.text[normalized_range(&self.text, range)].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用构造器：带内容且插入点在末尾。
    /// 产品代码不走这条路（Root 用 `TextField::default()` + `set_text`），
    /// 所以它留在测试里，不进公共 API。
    fn field_with(text: &str) -> TextField {
        let mut field = TextField::default();
        field.set_text(text);
        field.set_caret(text.len());
        field
    }

    fn word_ranges(text: &str) -> Vec<Range<usize>> {
        segments(text)
            .into_iter()
            .filter(|segment| segment.kind == SegmentKind::Word)
            .map(|segment| segment.range)
            .collect()
    }

    #[test]
    fn char_steps_over_utf8_boundaries() {
        let text = "ab中文";
        assert_eq!(step_char(text, 0, true), 1);
        assert_eq!(step_char(text, 1, true), 2);
        assert_eq!(step_char(text, 2, true), 5, "中 占 3 字节");
        assert_eq!(step_char(text, 5, false), 2);
        assert_eq!(step_char(text, 0, false), 0, "开头再往左不动");
        assert_eq!(
            step_char(text, text.len(), true),
            text.len(),
            "末尾再往右不动"
        );
    }

    #[test]
    fn caret_is_clamped_into_char_boundaries() {
        let text = "ab中";
        assert_eq!(clamp_cursor(text, 3), 2, "3 落在「中」中间，回退到字符边界");
        assert_eq!(clamp_cursor(text, 99), text.len());
        assert_eq!(clamp_cursor(text, 0), 0);
    }

    #[test]
    fn delete_respects_the_caret_position() {
        // 插入点在 5（「中」之后的字节位置），退格删掉「中」（2..5）。
        let mut field = field_with("ab中cd");
        field.set_caret(5);
        assert!(field.delete(Direction::Backward, Unit::Char));
        assert_eq!(field.text(), "abcd");
        assert_eq!(field.caret(), 2);

        let mut field = field_with("abcd");
        field.set_caret(1);
        assert!(field.delete(Direction::Forward, Unit::Char));
        assert_eq!(field.text(), "acd");
        assert_eq!(field.caret(), 1);

        // 开头退格 / 末尾向前删都是空操作。
        let mut field = field_with("abc");
        field.set_caret(0);
        assert!(!field.delete(Direction::Backward, Unit::Char));
        field.set_caret(3);
        assert!(!field.delete(Direction::Forward, Unit::Char));
    }

    #[test]
    fn latin_words_keep_apostrophes_and_numeric_dots() {
        // UAX#29 把撇号与数字里的小数点算进词（crate 自己的例子就是 can't / 32.3），
        // 但 ASCII 连字符是词边界，所以 `e-mail` 拆成两块——与浏览器的双击行为一致。
        assert_eq!(
            word_ranges("don't e-mail v2.0"),
            vec![0..5, 6..7, 8..12, 13..17]
        );
    }

    #[test]
    fn cjk_run_is_one_word_but_scripts_split() {
        assert_eq!(word_ranges("我喜欢的音乐"), vec![0..18]);
        assert_eq!(word_ranges("日本语"), vec![0..9]);
        assert_eq!(word_ranges("周杰伦 Jay"), vec![0..9, 10..13]);
        assert_eq!(word_ranges("你好，世界"), vec![0..6, 9..15]);
        // 汉字与假名分属两种文字，各自成词。
        assert_eq!(word_ranges("日本語テスト"), vec![0..9, 9..18]);
    }

    #[test]
    fn forward_word_jump_skips_separators() {
        let text = "abc   def";
        assert_eq!(next_word_offset(text, 0, Direction::Forward), 3);
        assert_eq!(
            next_word_offset(text, 3, Direction::Forward),
            9,
            "跳过空格落到下一个词尾"
        );
        assert_eq!(next_word_offset(text, 5, Direction::Forward), 9);
        assert_eq!(next_word_offset(text, 9, Direction::Forward), 9, "到底不动");
        // 落在词中间先补到词尾，与 Windows 的 Ctrl+→ 一致。
        assert_eq!(next_word_offset("hello world", 3, Direction::Forward), 5);
    }

    #[test]
    fn backward_word_jump_lands_on_word_start() {
        let text = "hello world";
        assert_eq!(next_word_offset(text, 11, Direction::Backward), 6);
        assert_eq!(
            next_word_offset(text, 3, Direction::Backward),
            0,
            "词中间回到词首"
        );
        assert_eq!(
            next_word_offset(text, 6, Direction::Backward),
            0,
            "跳过空格回到上一个词首"
        );
        assert_eq!(
            next_word_offset(text, 0, Direction::Backward),
            0,
            "到头不动"
        );
        assert_eq!(next_word_offset("我喜欢的音乐", 15, Direction::Backward), 0);
    }

    #[test]
    fn delete_word_backward_removes_exactly_one_word() {
        let mut field = field_with("hello world");
        assert_eq!(field.caret(), 11);
        assert!(field.delete(Direction::Backward, Unit::Word));
        assert_eq!(field.text(), "hello ");
        assert_eq!(field.caret(), 6);
    }

    #[test]
    fn typed_text_lands_on_the_caret_not_at_the_end() {
        // Windows 的 WM_CHAR 通道带的是 range=None，兜底必须是插入点。
        let mut field = field_with("abcd");
        field.set_caret(2);
        assert!(field.replace(None, "X"));
        assert_eq!(field.text(), "abXcd");
        assert_eq!(field.caret(), 3);
    }

    #[test]
    fn marked_range_is_replaced_by_the_next_ime_edit() {
        let mut field = TextField::default();
        assert!(field.replace_and_mark(None, "你好"));
        assert_eq!(field.text(), "你好");
        assert_eq!(field.marked(), Some(0..6));
        // 还在组合中时，下一次上屏整体替换组合区间，而不是追加在它后面。
        assert!(field.replace_and_mark(None, "泥"));
        assert_eq!(field.text(), "泥");
        assert_eq!(field.marked(), Some(0..3));
        assert!(field.clear_marked());
        assert_eq!(field.marked(), None);
        assert!(!field.clear_marked());
    }

    #[test]
    fn utf16_offsets_round_trip_through_multibyte_text() {
        // 字节布局：a=0..1，😀=1..5（2 个 UTF-16 单位），b=5..6。
        let field = field_with("a😀b");
        assert_eq!(field.utf16_len(), 4);
        assert_eq!(field.byte_to_utf16(0), 0);
        assert_eq!(field.byte_to_utf16(1), 1);
        assert_eq!(field.byte_to_utf16(5), 3, "5 是 b 之前，只数得 a 与 emoji");
        assert_eq!(field.byte_to_utf16(6), 4);
        assert_eq!(field.utf16_to_byte(3), 5);
        assert_eq!(field.utf16_to_byte(99), field.len());
        assert_eq!(field.byte_range_from_utf16(1..3), 1..5);
    }

    #[test]
    fn selection_range_follows_marked_then_caret() {
        let mut field = field_with("abc");
        field.set_caret(2);
        assert_eq!(field.selection_range(), 2..2);
        field.replace_and_mark(Some(2..2), "X");
        assert_eq!(field.selection_range(), 2..3);
    }

    #[test]
    fn programmatic_set_text_clamps_the_caret() {
        // 启动注入走的是这条路径：原本插入点在 0，赋值后仍在 0（与迁移前一致）。
        let mut field = TextField::default();
        field.set_text("查询内容");
        assert_eq!(field.text(), "查询内容");
        assert_eq!(field.caret(), 0);
        // 插入点超出新文本时夹回末尾，不会留下越界的下标。
        field.set_caret(12);
        field.set_text("短");
        assert_eq!(field.caret(), 3);
        assert!(field.clear());
        assert!(field.is_empty());
        assert!(!field.clear());
    }
}
