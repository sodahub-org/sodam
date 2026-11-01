//! 播放控制快捷键：`Space` 加一对「上一首 / 下一首」，可在设置页重绑。
//!
//! 默认值是分平台的（见 `sodam_core::config` 的注释）：macOS 用 `⌘←`/`⌘→`，
//! 因为 `⌃←`/`⌃→` 被系统「切换桌面」抢先吃掉；其他平台用 `Ctrl+←`/`Ctrl+→`。
//!
//! 绑定写在 `!input && !capturing` 上下文上：
//! * `input` —— 搜索框（见 `views::search`），打字时让路；
//! * `capturing` —— 设置页正在录制新组合键，避免按下新键时顺手触发一次动作。
//!
//! 系统媒体键（媒体面板 / 播放器控件）走 `media` 模块，和这里共用同一组动作。

use gpui::{actions, Action, App, Entity, KeyBinding, Keystroke};

use crate::app::Root;
use sodam_core::Settings;

actions!(sodam, [TogglePlayback, SongPrevious, SongNext]);

/// 文本输入元素挂的 key context。
pub const INPUT_CONTEXT: &str = "input";
/// 设置页录制新快捷键时挂在卡片上的 key context。
pub const CAPTURING_CONTEXT: &str = "capturing";
/// 窗口根元素挂的基础上下文。
///
/// 必须有它：gpui 的上下文谓词在**空**上下文栈上恒为 false（`eval_inner` 里
/// `contexts.last()` 为空就直接返回 false），所以带谓词的绑定在「没有元素聚焦」
/// 时根本匹配不上。根元素挂上基础上下文后，栈至少有一层。
pub const BASE_CONTEXT: &str = "workspace";

/// 播放动作与「聚焦搜索框」共同的生效条件。
///
/// 名字不叫播放专用：`app_text::focus_bindings()` 也用它 —— 那是个全局键，
/// 同样不该在打字或录制快捷键时冒出来。
pub(crate) const GLOBAL_CONTEXT: &str = "workspace && !input && !capturing";

/// 「聚焦搜索框」的绑定串（见 `app_text::focus_bindings`）。
///
/// 固定功能，不可重绑。`secondary-` 由 gpui 按平台解释成 macOS 的 ⌘ / 其他平台的 Ctrl，
/// 所以要拦哪个键得先把它规范成具体写法，见下面的 `reserved_feature`。
pub const FOCUS_SEARCH_SOURCE: &str = "secondary-f";

/// 三个可重绑的播放动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Toggle,
    Previous,
    Next,
}

impl Slot {
    const ALL: [Self; 3] = [Self::Toggle, Self::Previous, Self::Next];
}

/// 录制候选键不能生效的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutConflict {
    /// 同一个组合键已经绑给了另一个播放动作。
    Playback(Slot),
    /// macOS 菜单保留键。
    MacosMenu,
    /// 已被应用内固定功能占用（携带该功能名的中文 key）。
    Fixed(&'static str),
}

/// 读取某个动作当前生效的按键（gpui keystroke 写法）。
pub fn keystroke_of(settings: &Settings, slot: Slot) -> &str {
    match slot {
        Slot::Toggle => settings.shortcut_toggle(),
        Slot::Previous => settings.shortcut_previous(),
        Slot::Next => settings.shortcut_next(),
    }
}

/// 某个动作的配置字段（空串 = 用默认值）。
pub fn configured_of(settings: &Settings, slot: Slot) -> &str {
    match slot {
        Slot::Toggle => &settings.shortcut_toggle,
        Slot::Previous => &settings.shortcut_previous,
        Slot::Next => &settings.shortcut_next,
    }
}

/// 把按键写进配置字段。
///
/// 存之前先 trim：`effective_shortcut` 把纯空白当作未设置，若这里留着空格，
/// 「是否显示恢复默认」与「显示的是哪个键」就会各说一套。
pub fn set_keystroke(settings: &mut Settings, slot: Slot, keystroke: String) {
    let keystroke = keystroke.trim().to_string();
    match slot {
        Slot::Toggle => settings.shortcut_toggle = keystroke,
        Slot::Previous => settings.shortcut_previous = keystroke,
        Slot::Next => settings.shortcut_next = keystroke,
    }
}

/// 该动作是否被用户改过（改过才显示「恢复默认」）。
pub fn is_customized(settings: &Settings, slot: Slot) -> bool {
    !configured_of(settings, slot).trim().is_empty()
}

/// 把录制到的按键转成规范存储串（全小写、修饰键顺序固定，能被 `Keystroke::parse` 吃回去）。
///
/// 不能用 gpui 的 `to_string()`：那是**显示形式**，与 `Keystroke::parse` 不互逆
/// （单字符会被大写化成 `ctrl-A`，而 parse 把大写读成「带 shift」；macOS 的 control
/// 写成不带连字符的 `^`、主修饰键写成 `⌘`）。旧版直接存 `to_string()`，于是 macOS 上
/// 几乎任何重绑、其他平台上的字母键重绑都会变成永不匹配的死键（#39）。
///
/// 形式本身交给 gpui 的 `unparse`（`cmd` / `super` / `win` 都表示主修饰键），免得我们
/// 自己排一遍修饰键顺序、再和 parse 的实现漂移出只在某个平台生效的写法。
pub fn canonical_keystroke(keystroke: &Keystroke) -> String {
    keystroke.unparse()
}

/// 这个串能不能当应用内快捷键绑定。
///
/// 三道关：非空、纯 ASCII、每个组合都能被 `Keystroke::parse` 接受。ASCII 这一条专门用来
/// 挡住 `⌘←` / `⊞k` 这种符号形式——它们 parse 不成修饰键，却会被 gpui 当成一个名叫
/// `⌘←` 的键，于是静默永不触发；`KeyBinding::new` 又是在解析失败时 panic 的入口
/// （`keymap/binding.rs` 的文档明写），配置文件用户可以手改，崩在启动上违反
/// AGENTS.md 的「错误不 panic」，所以坏值一律走这里挡掉。
pub fn is_bindable_source(source: &str) -> bool {
    let source = source.trim();
    !source.is_empty()
        && source.is_ascii()
        && source
            .split_whitespace()
            .all(|part| Keystroke::parse(part).is_ok())
}

/// 候选键是否已被应用内某个**固定功能**占用；返回该功能名的中文 key。
///
/// 规范串用 `parse` + `unparse` 现算，不手写 `"cmd-f"` / `"ctrl-f"`：手写就等于自己
/// 再排一遍平台规则，正是 #42 之前那类「只在某个平台生效」的坑。录制界面存的也是
/// `canonical_keystroke` 的产物，两边同一套归一化才比得准。
fn reserved_feature(candidate: &str) -> Option<&'static str> {
    Keystroke::parse(FOCUS_SEARCH_SOURCE)
        .ok()
        .filter(|keystroke| canonical_keystroke(keystroke).eq_ignore_ascii_case(candidate))
        .map(|_| "聚焦搜索框")
}

/// 检查候选组合键能否绑定到 `slot`。
///
/// 比较的是生效值而不是配置原文：空串代表内置默认，手改配置造成的重复
/// 也应在下次录制时被发现。按键写法忽略大小写和前后空白。
pub fn shortcut_conflict(
    settings: &Settings,
    slot: Slot,
    candidate: &str,
) -> Option<ShortcutConflict> {
    let candidate = candidate.trim();
    if cfg!(target_os = "macos")
        && matches!(
            candidate.to_ascii_lowercase().as_str(),
            "cmd-q" | "cmd-m" | "cmd-w"
        )
    {
        return Some(ShortcutConflict::MacosMenu);
    }
    // 固定功能先判：两种原因同时命中时，「已被聚焦搜索框占用」才是能照着换键的那句。
    if let Some(feature) = reserved_feature(candidate) {
        return Some(ShortcutConflict::Fixed(feature));
    }

    Slot::ALL
        .into_iter()
        .filter(|other| *other != slot)
        .find(|other| keystroke_of(settings, *other).eq_ignore_ascii_case(candidate))
        .map(ShortcutConflict::Playback)
}

/// 装好动作处理器，并按配置应用一次绑定。启动时调用一次。
pub fn install(cx: &mut App, settings: &Settings, root: Entity<Root>) {
    cx.on_action({
        let root = root.clone();
        move |_: &TogglePlayback, cx: &mut App| {
            root.update(cx, |root, cx| root.toggle_play(cx));
        }
    });
    cx.on_action({
        let root = root.clone();
        move |_: &SongPrevious, cx: &mut App| {
            root.update(cx, |root, cx| root.prev_track(cx));
        }
    });
    cx.on_action({
        let root = root.clone();
        move |_: &SongNext, cx: &mut App| {
            root.update(cx, |root, cx| root.next_track(cx));
        }
    });

    apply(cx, settings);
}

/// 重新应用全部绑定（设置页改完快捷键后调用）。
///
/// 整表重建而不是增量添加：gpui 的 keymap 是累加的，只加新绑定的话老键位会继续生效。
/// 也正因为 `clear_key_bindings` 是整表擦除（gpui 没有按作用域清除的 API），
/// 搜索框的编辑键位必须在这里一起重发，否则用户重绑一次播放快捷键就把它抹掉了。
pub fn apply(cx: &mut App, settings: &Settings) {
    cx.clear_key_bindings();
    #[cfg(target_os = "macos")]
    cx.bind_keys(crate::menus::window_bindings());
    cx.bind_keys(crate::app_text::bindings());
    cx.bind_keys(crate::app_text::focus_bindings());
    cx.bind_keys(bindings(settings));
}

/// 按配置生成三个播放动作的绑定。
///
/// 坏值只丢它自己那一条，另外两条照常生效：`KeyBinding::new` 解析失败会 panic，
/// 而这个字符串来自用户可以手改的 `config.json`（设置页还给了「打开配置文件」入口）。
fn bindings(settings: &Settings) -> Vec<KeyBinding> {
    let mut bindings = Vec::with_capacity(3);
    push_binding(&mut bindings, settings.shortcut_toggle(), TogglePlayback);
    push_binding(&mut bindings, settings.shortcut_previous(), SongPrevious);
    push_binding(&mut bindings, settings.shortcut_next(), SongNext);
    bindings
}

fn push_binding<A: Action>(bindings: &mut Vec<KeyBinding>, source: &str, action: A) {
    if !is_bindable_source(source) {
        return;
    }
    bindings.push(KeyBinding::new(source.trim(), action, Some(GLOBAL_CONTEXT)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 规范串必须能被 parse 吃回去，并且再规范化一次不变（幂等）——#39 的锁。
    #[test]
    fn canonical_keystrokes_are_stable_under_the_parser() {
        for source in [
            "space",
            "ctrl-left",
            "cmd-left",
            "secondary-left",
            "ctrl-alt-delete",
            "shift-tab",
            "fn-f12",
            "backspace",
        ] {
            let keystroke = Keystroke::parse(source).unwrap();
            let stored = canonical_keystroke(&keystroke);
            assert!(
                is_bindable_source(&stored),
                "{source} 规范化成 {stored} 后应当可绑定"
            );
            let again = Keystroke::parse(&stored).unwrap();
            assert_eq!(
                canonical_keystroke(&again),
                stored,
                "{source} → {stored} 必须幂等"
            );
        }
    }

    /// 旧 Display 写出的 `ctrl-A` 表示的是「Ctrl+Shift+A」，规范串不会把这件事搞混。
    #[test]
    fn uppercase_letter_means_shift_so_legacy_storage_was_wrong() {
        let plain = Keystroke::parse("ctrl-a").unwrap();
        assert!(!plain.modifiers.shift, "ctrl-a 不该带 shift");
        assert_eq!(canonical_keystroke(&plain), "ctrl-a");

        let legacy = Keystroke::parse("ctrl-A").unwrap();
        assert!(
            legacy.modifiers.shift,
            "旧 Display 的大写字母会被读成 shift"
        );
        assert_eq!(canonical_keystroke(&legacy), "ctrl-shift-a");
    }

    /// 符号形式（旧版在 macOS / Windows 上存下来的样子）不可绑定，也不该进 keymap。
    #[test]
    fn symbol_forms_are_rejected() {
        assert!(!is_bindable_source("^←"));
        assert!(!is_bindable_source("⌘←"));
        assert!(!is_bindable_source("⊞k"));
        assert!(!is_bindable_source("   "), "空白等于未设置");
        assert!(!is_bindable_source(""));
        assert!(is_bindable_source("ctrl-a"));
        assert!(is_bindable_source("secondary-left"));
    }

    /// 一条坏值只作废它自己：另外两个动作照常绑上，且全程不 panic。
    #[test]
    fn one_bad_value_drops_only_its_own_binding() {
        let settings = Settings {
            shortcut_toggle: String::new(),
            shortcut_previous: "⌘←".into(),
            shortcut_next: String::new(),
            ..Default::default()
        };
        assert_eq!(
            bindings(&settings).len(),
            2,
            "坏掉的上一首被跳过，播放/暂停与下一首仍在"
        );

        let all_bad = Settings {
            shortcut_toggle: "⌘Q".into(),
            shortcut_previous: "^←".into(),
            shortcut_next: "⊞".into(),
            ..Default::default()
        };
        assert!(bindings(&all_bad).is_empty());
    }

    fn settings_with(previous: &str, next: &str) -> Settings {
        Settings {
            shortcut_previous: previous.to_owned(),
            shortcut_next: next.to_owned(),
            ..Settings::default()
        }
    }

    #[test]
    fn playback_slots_reject_each_others_effective_shortcuts() {
        let settings = settings_with("ctrl-up", "ctrl-down");
        assert_eq!(
            shortcut_conflict(&settings, Slot::Next, "CTRL-UP"),
            Some(ShortcutConflict::Playback(Slot::Previous))
        );
        assert_eq!(
            shortcut_conflict(&settings, Slot::Toggle, "ctrl-down"),
            Some(ShortcutConflict::Playback(Slot::Next))
        );
        assert_eq!(
            shortcut_conflict(&settings, Slot::Toggle, "alt-space"),
            None
        );
    }

    #[test]
    fn defaults_participate_in_conflict_detection() {
        let settings = Settings::default();
        let slot = if settings.shortcut_toggle() == settings.shortcut_previous() {
            Slot::Next
        } else {
            Slot::Previous
        };
        assert_eq!(
            shortcut_conflict(&settings, slot, settings.shortcut_toggle()),
            Some(ShortcutConflict::Playback(Slot::Toggle))
        );
    }

    #[test]
    fn same_slot_can_be_recorded_again() {
        let settings = settings_with("ctrl-up", "");
        assert_eq!(
            shortcut_conflict(&settings, Slot::Previous, "ctrl-up"),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_menu_shortcuts_are_reserved() {
        let settings = Settings::default();
        for key in ["cmd-q", "CMD-M", "cmd-w"] {
            assert_eq!(
                shortcut_conflict(&settings, Slot::Toggle, key),
                Some(ShortcutConflict::MacosMenu)
            );
        }
    }

    #[test]
    fn fixed_feature_shortcuts_are_reserved() {
        // 与录制界面同一条归一化路径（parse + unparse）：手写 "ctrl-f" 在 macOS 上就验错了键。
        let reserved = canonical_keystroke(&Keystroke::parse(FOCUS_SEARCH_SOURCE).unwrap());
        let upper = reserved.to_uppercase();
        let padded = format!("  {reserved} ");
        let settings = Settings::default();
        for candidate in [reserved.as_str(), upper.as_str(), padded.as_str()] {
            assert_eq!(
                shortcut_conflict(&settings, Slot::Toggle, candidate),
                Some(ShortcutConflict::Fixed("聚焦搜索框")),
                "{candidate} 该被固定功能挡住"
            );
        }
        // 修饰键匹配是精确的：加一个 Shift 就没人占用，照常可绑。
        let shifted = format!("shift-{reserved}");
        assert_eq!(shortcut_conflict(&settings, Slot::Toggle, &shifted), None);
    }
}
