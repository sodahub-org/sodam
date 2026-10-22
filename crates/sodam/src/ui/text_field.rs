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

/// 单行输入框的状态。下标一律是 UTF-8 字节位置，且永远落在字符边界上。
///
/// 选区用「锚点 + 活动端」表示，而不是 `Option<Range>`：Shift 扩选和鼠标拖拽都要固定一端、
/// 只动另一端，锚点模型能直接表达，也天然带方向（`reversed`），gpui 的 `UTF16Selection.reversed`
/// 要的就是这个。
///
/// `scroll_x` 是文本被向左滚动的逻辑像素数（绘制原点 = `-scroll_x`）。这里存 `f32` 而不是
/// `gpui::Pixels`，是为了让整套状态能脱离窗口构造并单测；换算留在视图层。
#[derive(Debug, Clone, Default)]
pub struct TextField {
    text: String,
    /// 选区固定端。
    anchor: usize,
    /// 选区活动端；与锚点重合即没有选区。
    cursor: usize,
    /// 活动端在锚点左侧（往回 Shift/拖拽时为真）。
    reversed: bool,
    /// Fcitx/IBus 等 IME 的预编辑区间。
    marked: Option<Range<usize>>,
    scroll_x: f32,
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

    /// 活动端（对不关心选区方向的调用方来说就是「光标」位置）。
    pub fn caret(&self) -> usize {
        self.cursor
    }

    pub fn is_reversed(&self) -> bool {
        self.reversed
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    /// 规范化的选区区间（`start <= end`），无选区时是活动端的折叠区间。
    pub fn selection(&self) -> Range<usize> {
        let start = self.anchor.min(self.cursor);
        let end = self.anchor.max(self.cursor);
        start..end
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.cursor
    }

    pub fn selected_text(&self) -> String {
        self.slice(self.selection())
    }

    pub fn scroll_x(&self) -> f32 {
        self.scroll_x
    }

    /// 整体替换文本（程序化赋值，例如启动时的 `SODAM_QUERY`）；两端下标都夹进新文本里。
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = clamp_cursor(&self.text, self.cursor);
        self.anchor = clamp_cursor(&self.text, self.anchor);
        self.reversed = self.cursor < self.anchor;
        self.marked = None;
        self.scroll_x = 0.0;
    }

    /// 清空文本，返回是否真有内容被清掉。
    pub fn clear(&mut self) -> bool {
        let changed = !self.text.is_empty();
        self.text.clear();
        self.cursor = 0;
        self.anchor = 0;
        self.reversed = false;
        self.marked = None;
        self.scroll_x = 0.0;
        changed
    }

    /// 把插入点设到指定字节位置，并折叠掉选区（自动夹到字符边界）。
    ///
    /// 返回「界面要不要重绘」：光标点进一段高亮里虽然没移动，高亮也得消失，所以判定里
    /// 带上「原本有没有选区」。按键移动与鼠标点击都会清掉 IME 组合区间：预编辑还挂在
    /// 原处会误导候选窗定位。
    pub fn set_caret(&mut self, caret: usize) -> bool {
        let next = clamp_cursor(&self.text, caret);
        let changed = next != self.cursor || self.has_selection();
        self.cursor = next;
        self.anchor = next;
        self.reversed = false;
        self.marked = None;
        changed
    }

    /// 按字符或按词移动插入点。
    ///
    /// 有选区时先按方向塌到选区的一端（左移塌到起点、右移塌到终点），与 GTK/Chromium 一致；
    /// 否则按字符或词前进后退。
    pub fn move_caret(&mut self, direction: Direction, unit: Unit) -> bool {
        if self.has_selection() {
            let target = match direction {
                Direction::Backward => self.selection().start,
                Direction::Forward => self.selection().end,
            };
            return self.set_caret(target);
        }
        let next = self.offset_in_direction(direction, unit);
        self.set_caret(next)
    }

    /// Shift 扩选：锚点固定不动，活动端按字符或词前进/后退。
    pub fn extend_selection(&mut self, direction: Direction, unit: Unit) -> bool {
        let next = self.offset_in_direction(direction, unit);
        self.set_position(next, self.anchor)
    }

    /// 指定方向上的下一个/上一个目标位置（纯移动，不碰状态）。
    fn offset_in_direction(&self, direction: Direction, unit: Unit) -> usize {
        match unit {
            Unit::Char => step_char(&self.text, self.cursor, direction == Direction::Forward),
            Unit::Word => next_word_offset(&self.text, self.cursor, direction),
        }
    }

    /// 跳到文本首/尾，返回界面是否需要重绘。
    pub fn move_caret_to(&mut self, edge: Edge) -> bool {
        let next = match edge {
            Edge::Start => 0,
            Edge::End => self.text.len(),
        };
        self.set_caret(next)
    }

    /// Shift + Home/End：把活动端扩到文本首/尾，锚点不动。
    pub fn extend_to_edge(&mut self, edge: Edge) -> bool {
        let next = match edge {
            Edge::Start => 0,
            Edge::End => self.text.len(),
        };
        self.set_position(next, self.anchor)
    }

    /// 全选。返回界面是否需要重绘。
    pub fn select_all(&mut self) -> bool {
        self.set_position(self.text.len(), 0)
    }

    /// 折叠选区（活动端保留在原处），返回是否本来有选区。
    pub fn collapse_selection(&mut self) -> bool {
        let had = self.has_selection();
        self.anchor = self.cursor;
        self.reversed = false;
        had
    }

    /// 同时指定活动端与锚点：鼠标按下、拖拽、双击选词都走这条。
    ///
    /// 方向由两端的大小关系决定，调用方不必自己维护 `reversed`。
    pub fn set_position(&mut self, cursor: usize, anchor: usize) -> bool {
        let cursor = clamp_cursor(&self.text, cursor);
        let anchor = clamp_cursor(&self.text, anchor);
        let changed = cursor != self.cursor || anchor != self.anchor;
        self.cursor = cursor;
        self.anchor = anchor;
        self.reversed = cursor < anchor;
        self.marked = None;
        changed
    }

    /// 拖拽扩选：锚点保持鼠标按下时那一点，活动端跟到当前位置。
    pub fn select_through(&mut self, cursor: usize) -> bool {
        self.set_position(cursor, self.anchor)
    }

    /// 按词拖拽（双击起步的那种）：把「锚点所在词」与「指针所在词」之间的整段选中，
    /// 活动端落在靠指针的一侧。起点落在空格/标点里时退化成字符级扩选。
    pub fn select_words_through(&mut self, index: usize) -> bool {
        let index = clamp_cursor(&self.text, index);
        let anchor = self.anchor;
        let Some(anchor_word) = self.word_at(anchor) else {
            return self.select_through(index);
        };
        let target = self.word_at(index);
        let target_start = target.as_ref().map_or(index, |range| range.start);
        let target_end = target.map_or(index, |range| range.end);
        let start = anchor_word.start.min(target_start);
        let end = anchor_word.end.max(target_end);
        if index >= anchor {
            self.set_position(end, start)
        } else {
            self.set_position(start, end)
        }
    }

    /// 下标所在的那个词（`None` = 落在分隔符里，比如空格串中间）。
    pub fn word_at(&self, index: usize) -> Option<Range<usize>> {
        let index = clamp_cursor(&self.text, index);
        segments(&self.text)
            .into_iter()
            .find(|segment| {
                segment.kind == SegmentKind::Word
                    && index >= segment.range.start
                    && index < segment.range.end
            })
            .map(|segment| segment.range)
    }

    /// 双击选词：命中词则整词选中，落在空格/标点里则退化成「选中该位置所在的那个词边界」，
    /// 与浏览器/记事本一样什么都不选。返回界面是否需要重绘。
    pub fn select_word_at(&mut self, index: usize) -> bool {
        match self.word_at(index) {
            Some(word) => self.set_position(word.end, word.start),
            None => self.set_caret(index),
        }
    }

    /// 粘贴外部文本：换行与制表符都归一成空格，并剥掉首尾空白——搜索框里没有多行，
    /// 带换行的粘贴（比如从歌单里复制一段）在接口上是无意义的。
    pub fn paste(&mut self, raw: &str) -> bool {
        let normalized = raw
            .chars()
            .map(|ch| match ch {
                '\r' | '\n' | '\t' => ' ',
                other => other,
            })
            .collect::<String>();
        let trimmed = normalized.trim();
        self.replace(None, trimmed)
    }

    /// 把滚动量设到 `desired`，并夹在 `[0, max]` 内（`max` 由视图层用文本宽度算）。
    pub fn set_scroll_x(&mut self, desired: f32, max: f32) -> bool {
        let next = desired.clamp(0.0, max.max(0.0));
        let changed = (next - self.scroll_x).abs() > f32::EPSILON;
        self.scroll_x = next;
        changed
    }

    /// 让文本坐标区间 `[x, x + span]` 落到可视区 `[scroll_x, scroll_x + width]` 内，
    /// 只滚到刚好露出为止。
    ///
    /// `span` 是「要露出的东西有多宽」：光标不是一条无宽度的线，它就是 2px 宽的实心矩形，
    /// 只按左边缘算的话，右边缘会被视口的裁剪线切掉。取 `span = 0` 时退化成「露出一个位置」。
    ///
    /// 刻意不做居中：Chromium 的 `Textfield::Scroll` 就是 "applies the minimum scrolling
    /// change necessary to just bring the position into view"，Blink 还专门把「滚到中间」
    /// 改成「滚到最近边缘」去对齐 Firefox/Edge。
    pub fn ensure_visible(&mut self, x: f32, span: f32, width: f32, max: f32) -> bool {
        if width <= 0.0 {
            return false;
        }
        if x < self.scroll_x {
            self.set_scroll_x(x, max)
        } else if x + span > self.scroll_x + width {
            self.set_scroll_x(x + span - width, max)
        } else {
            false
        }
    }

    /// 删掉一段内容：有选区时删掉整段（此时 Backspace 与 Delete 同义，与所有正常输入框一致），
    /// 否则删掉活动端与目标位置之间的一段。
    pub fn delete(&mut self, direction: Direction, unit: Unit) -> bool {
        if self.has_selection() {
            let range = self.selection();
            return self.apply_removal(range);
        }
        let from = self.offset_in_direction(direction, unit);
        let range = normalized_range(&self.text, self.cursor..from);
        self.apply_removal(range)
    }

    /// 删除区间并把两端收口到删除起点；空区间返回 `false`（不需要重绘）。
    fn apply_removal(&mut self, range: Range<usize>) -> bool {
        if range.start == range.end {
            return false;
        }
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.anchor = range.start;
        self.reversed = false;
        self.marked = None;
        true
    }

    /// 替换一段文本，活动端落到插入内容之后。
    ///
    /// 没有显式区间时的兜底顺序是「组合区间 → 选区 → 活动端」。**不能兜底到文本末尾**：
    /// Windows 的可输入字符走 WM_CHAR，带的是 `None`，兜底成末尾会让「点到中间再打字」
    /// 变成追加到尾部；而选了整段再打字必须替换掉整段，所以选区排在这里。
    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str) -> bool {
        let range = normalized_range(
            &self.text,
            range
                .or_else(|| self.marked.clone())
                .or_else(|| self.has_selection().then(|| self.selection()))
                .unwrap_or(self.cursor..self.cursor),
        );
        let changed = !text.is_empty() || range.start != range.end;
        let start = range.start;
        self.text.replace_range(range, text);
        self.cursor = clamp_cursor(&self.text, start + text.len());
        self.anchor = self.cursor;
        self.reversed = false;
        self.marked = None;
        changed
    }

    /// IME 上屏中间态：替换后把新内容记成组合区间，候选窗据此定位。
    pub fn replace_and_mark(&mut self, range: Option<Range<usize>>, text: &str) -> bool {
        let changed = self.replace(range, text);
        let end = self.cursor;
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

    /// 该上报给系统的区间：组合中用组合区间，否则就是选区（无选区时是活动端的折叠区间）。
    pub fn selection_range(&self) -> Range<usize> {
        self.marked.clone().unwrap_or_else(|| self.selection())
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

    #[test]
    fn selection_is_normalized_and_carries_direction() {
        let mut field = field_with("hello world");
        // 鼠标从右往左拖：活动端在左，`reversed` 为真，但区间本身永远 start <= end。
        assert!(field.set_position(2, 7));
        assert_eq!(field.selection(), 2..7);
        assert_eq!(field.caret(), 2);
        assert_eq!(field.anchor, 7);
        assert!(field.is_reversed());
        assert!(field.has_selection());
        assert_eq!(field.selected_text(), "llo w");
    }

    #[test]
    fn plain_movement_collapses_selection_to_the_facing_end() {
        let mut field = field_with("hello");
        field.select_all();
        // 有选区时按右：先塌到选区末尾，而不是继续往右走。
        assert!(field.move_caret(Direction::Forward, Unit::Char));
        assert_eq!(field.caret(), 5);
        assert!(!field.has_selection());
        field.select_all();
        assert!(field.move_caret(Direction::Backward, Unit::Char));
        assert_eq!(field.caret(), 0, "按左塌到选区开头");
        assert!(!field.has_selection());
    }

    #[test]
    fn shift_extension_keeps_the_anchor_fixed() {
        let mut field = field_with("hello world");
        field.set_caret(5);
        assert!(field.extend_selection(Direction::Forward, Unit::Char));
        assert_eq!(field.selection(), 5..6);
        assert_eq!(field.anchor, 5, "锚点必须钉在按下 Shift 之前的位置");
        assert!(field.extend_selection(Direction::Forward, Unit::Word));
        assert_eq!(field.selection(), 5..11);
        assert!(!field.is_reversed());
        // 反向一步先退回到「world」的词首一侧：区间收缩，锚点仍然不动。
        assert!(field.extend_selection(Direction::Backward, Unit::Word));
        assert_eq!(field.selection(), 5..6);
        assert_eq!(field.caret(), 6);
        // 再反向一步越过锚点，方向才翻过来。
        assert!(field.extend_selection(Direction::Backward, Unit::Word));
        assert_eq!(field.selection(), 0..5);
        assert_eq!(field.caret(), 0);
        assert!(field.is_reversed());
    }

    #[test]
    fn typing_and_pasting_replace_the_whole_selection() {
        let mut field = field_with("hello");
        field.select_all();
        // 打字走的是 range=None 的通道，兜底顺序必须包含选区。
        assert!(field.replace(None, "X"));
        assert_eq!(field.text(), "X");
        assert_eq!(field.caret(), 1);
        assert!(!field.has_selection());

        let mut field = field_with("abc");
        field.set_position(3, 1);
        assert!(field.paste("  周杰伦  Jay  "));
        assert_eq!(
            field.text(),
            "a周杰伦  Jay",
            "替换掉 [1,3) 的 bc；词中间的空格不动"
        );
        assert_eq!(field.caret(), 15);
    }

    #[test]
    fn delete_takes_the_whole_selection_in_both_directions() {
        for direction in [Direction::Backward, Direction::Forward] {
            let mut field = field_with("abcdef");
            field.set_position(4, 1);
            assert!(field.delete(direction, Unit::Char));
            assert_eq!(field.text(), "aef", "选区里的 bcd 整体消失");
            assert_eq!(field.caret(), 1);
            assert!(!field.has_selection());
        }
    }

    #[test]
    fn paste_flattens_newlines_and_trims_edges() {
        let mut field = TextField::default();
        assert!(field.paste("  a\nb\tc  "));
        assert_eq!(
            field.text(),
            "a b c",
            "换行与制表符归一成空格，首尾空白剥掉"
        );
        assert_eq!(field.caret(), 5);
        assert!(!field.paste(""), "粘贴空内容不算改动");
    }

    #[test]
    fn copy_and_selection_text_are_empty_without_a_selection() {
        let mut field = field_with("hello");
        assert_eq!(field.selected_text(), "", "无选区时不复制整串查询");
        assert!(!field.collapse_selection());
        field.select_all();
        assert!(field.collapse_selection());
        assert!(!field.has_selection());
        assert_eq!(field.caret(), 5, "折叠不搬动活动端");
    }

    #[test]
    fn double_click_selects_the_word_under_the_pointer() {
        let mut field = field_with("hello world");
        assert_eq!(field.word_at(3), Some(0..5));
        assert_eq!(field.word_at(8), Some(6..11));
        assert_eq!(field.word_at(5), None, "空格里没有词");
        assert!(field.select_word_at(8));
        assert_eq!(field.selection(), 6..11);
        assert_eq!(field.anchor, 6);
        assert_eq!(field.caret(), 11);
        // 落在空白上时退化成定位，不会把整串吞掉。
        assert!(field.select_word_at(5));
        assert!(!field.has_selection());
        assert_eq!(field.caret(), 5);
    }

    #[test]
    fn word_drag_extends_by_whole_words_in_both_directions() {
        let mut field = field_with("hello world 我喜欢的音乐");
        // 双击 hello 后往右拖：两端各自扩到整词。
        field.select_word_at(3);
        assert!(field.select_words_through(8));
        assert_eq!(field.selection(), 0..11);
        // 往左拖过起点：活动端跟到左侧那一头，方向翻转。
        let mut field = field_with("hello world");
        field.select_word_at(8);
        assert!(field.select_words_through(1));
        assert_eq!(field.selection(), 0..11);
        assert_eq!(field.caret(), 0);
        assert!(field.is_reversed());
        // 起笔在空白上：没有词可扩，退回字符级。
        let mut field = field_with("a  b");
        field.set_caret(1);
        assert!(field.select_words_through(3));
        assert_eq!(field.selection(), 1..3);
    }

    #[test]
    fn cjk_run_is_selected_as_one_word_and_dragged_as_one_word() {
        // 字节布局：「我喜欢的音乐」6 个汉字 = 0..18，空格 18..19，`abc` 19..22。
        let mut field = field_with("我喜欢的音乐 abc");
        assert_eq!(field.word_at(0), Some(0..18));
        assert!(field.select_word_at(6));
        assert_eq!(field.selection(), 0..18);
        assert!(field.select_words_through(19));
        assert_eq!(field.selection(), 0..22);
        assert_eq!(field.caret(), 22);
    }

    #[test]
    fn scroll_follow_moves_only_far_enough_and_stays_clamped() {
        let mut field = field_with("hello world");
        // 视口宽 100：右侧出界 120 → 只滚到刚好露出（20），不居中。
        assert!(field.ensure_visible(120.0, 0.0, 100.0, 1000.0));
        assert_eq!(field.scroll_x(), 20.0);
        // 已经在窗口里 → 不动。
        assert!(!field.ensure_visible(50.0, 0.0, 100.0, 1000.0));
        // 左侧出界 → 滚到该位置。
        assert!(field.ensure_visible(5.0, 0.0, 100.0, 1000.0));
        assert_eq!(field.scroll_x(), 5.0);
        // 夹在 [0, max]：文本比视口窄时 max 为 0，怎么滚都是原地。
        assert!(field.set_scroll_x(9999.0, 50.0));
        assert_eq!(field.scroll_x(), 50.0);
        assert!(field.set_scroll_x(-5.0, 50.0));
        assert_eq!(field.scroll_x(), 0.0);
        assert!(!field.set_scroll_x(0.0, 0.0));
    }

    #[test]
    fn scroll_follow_accounts_for_the_caret_width() {
        // 光标是 2px 宽的实心矩形：只把左边缘滚进视口的话，右边缘会被裁剪线切掉。
        let mut field = field_with("hello world");
        assert!(field.ensure_visible(120.0, 2.0, 100.0, 1002.0));
        assert_eq!(field.scroll_x(), 22.0, "要多滚 2px，让光标整体进视区");
        // 上一步之后右边缘正好贴视口右线 → 不再动（否则会来回抖）。
        assert!(!field.ensure_visible(120.0, 2.0, 100.0, 1002.0));
        // 视图层没给光标留出那 2px（max 只到 text_width - width）时只能夹住，
        // 这就是 search_max_scroll 也要加上 CARET_WIDTH 的原因。
        assert!(field.ensure_visible(500.0, 2.0, 100.0, 300.0));
        assert_eq!(field.scroll_x(), 300.0);
    }

    #[test]
    fn replacing_text_resets_scroll() {
        let mut field = field_with("hello world");
        field.set_scroll_x(30.0, 100.0);
        field.set_text("abc");
        assert_eq!(field.scroll_x(), 0.0, "整体换词之后旧偏移没有意义");
    }
}
