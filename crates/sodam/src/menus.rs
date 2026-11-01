//! macOS 菜单栏与标准快捷键：⌘Q 退出、⌘M 最小化、⌘W 关闭窗口。
//!
//! 为什么要有菜单栏：macOS 上这三组快捷键由 AppKit 的菜单 key equivalent 承担，
//! gpui 的 macOS 后端按 `App::set_menus` 逐项建 NSMenu —— 一个菜单项都没有时，
//! ⌘Q/⌘M/⌘W 在系统侧就是空白键，应用收不到任何动作。菜单项右侧显示的快捷键
//! 取自 keymap 里对该 action 的绑定，所以「绑定 + 菜单项」缺一不可。
//!
//! ⌘W 只关窗不退出：应用是常驻托盘的（`QuitMode::Explicit`），关窗后可以从托盘
//! 「显示主窗口」再打开，⌘Q 才真正退出进程。

use gpui::{actions, App, KeyBinding, Menu, MenuItem, Window};

use crate::ui::i18n::Language;

actions!(sodam, [QuitApp, MinimizeWindow, CloseWindow]);

/// 注册动作处理器与菜单栏。启动时调用一次。
pub fn install(cx: &mut App, language: Language) {
    // 三个动作都走应用级监听器：⌘W 关掉窗口后没有窗口可派发，菜单仍要能退出。
    cx.on_action(|_: &QuitApp, cx: &mut App| cx.quit());
    cx.on_action(|_: &MinimizeWindow, cx: &mut App| {
        with_main_window(cx, |window| window.minimize_window())
    });
    cx.on_action(|_: &CloseWindow, cx: &mut App| {
        with_main_window(cx, |window| window.remove_window())
    });

    refresh(cx, language);
}

/// macOS 的窗口/应用快捷键绑定。
///
/// 单独暴露给 `shortcuts::apply`：重绑播放快捷键时会整表重建 keymap，
/// 这里必须跟着一起注册，否则 ⌘Q/⌘M/⌘W 会失效。
/// 加上 `!capturing` 让录制态先收到按键；否则 keymap 会在 `on_key_down` 前
/// 直接执行退出/最小化/关闭，根本走不到冲突提示。
pub fn window_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", QuitApp, Some("!capturing")),
        KeyBinding::new("cmd-m", MinimizeWindow, Some("!capturing")),
        KeyBinding::new("cmd-w", CloseWindow, Some("!capturing")),
    ]
}

/// 按当前语言重建菜单栏。语言切换后调用。
pub fn refresh(cx: &App, language: Language) {
    cx.set_menus(vec![
        Menu::new("SodaM").items(vec![MenuItem::action(language.text("退出"), QuitApp)]),
        Menu::new(language.text("窗口")).items(vec![
            MenuItem::action(language.text("最小化"), MinimizeWindow),
            MenuItem::action(language.text("关闭窗口"), CloseWindow),
        ]),
    ]);
}

/// 对主窗口执行一次操作。
///
/// 两处都不能想当然：
/// * 窗口从 `cx.windows()` 取。实测 ⌘M/⌘W 这条路上 `App::active_window()` 是 `None`
///   （点菜单时才有值），照着它写会静默什么都不做。这里假设单窗口（sodam 主窗口）。
/// * 操作要 defer 到本轮更新之后。菜单命令是在窗口更新过程中派发的，此时对同一个
///   窗口再调 `update` 会被拒，`let _ =` 会把这个失败吞掉。
fn with_main_window(cx: &mut App, action: impl FnOnce(&mut Window) + 'static) {
    if let Some(window) = cx.windows().first().copied() {
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| action(window));
        });
    }
}
