//! 播放控制快捷键：`Space` / `Ctrl+←` / `Ctrl+→`，可在设置页重绑。
//!
//! 三平台用同一套默认值，与 sonora 的选择一致：macOS 上也不用 `⌘←`/`⌘→`，
//! 那两个键在文本框里是「行首/行尾」（Home/End）。
//!
//! 绑定写在 `!input && !capturing` 上下文上：
//! * `input` —— 搜索框（见 `views::search`），打字时让路；
//! * `capturing` —— 设置页正在录制新组合键，避免按下新键时顺手触发一次动作。
//!
//! 系统媒体键（媒体面板 / 播放器控件）走 `media` 模块，和这里共用同一组动作。

use gpui::{actions, App, Entity, KeyBinding};

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

/// 播放动作的生效条件。
const PLAYBACK_CONTEXT: &str = "workspace && !input && !capturing";

/// 三个可重绑的播放动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Toggle,
    Previous,
    Next,
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
pub fn set_keystroke(settings: &mut Settings, slot: Slot, keystroke: String) {
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
    cx.bind_keys(bindings(settings));
}

/// 按配置生成三个播放动作的绑定。
fn bindings(settings: &Settings) -> Vec<KeyBinding> {
    vec![
        KeyBinding::new(
            settings.shortcut_toggle(),
            TogglePlayback,
            Some(PLAYBACK_CONTEXT),
        ),
        KeyBinding::new(
            settings.shortcut_previous(),
            SongPrevious,
            Some(PLAYBACK_CONTEXT),
        ),
        KeyBinding::new(settings.shortcut_next(), SongNext, Some(PLAYBACK_CONTEXT)),
    ]
}
