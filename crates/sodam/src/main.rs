//! SodaM——GPUI 原生客户端入口。
//
// 发布版不分配控制台窗口（双击/快捷方式启动不再弹 terminal）；
// debug 构建保留终端，便于查看 eprintln 日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
// 搜索框按键分发（动作 + 键位绑定），见 app_text.rs。
mod app_text;
// 托盘：Linux 走 ksni/SNI，macOS 走 NSStatusItem，见 tray.rs。
mod tray;
mod ui;
mod views;
// 系统媒体控制（媒体键 / 桌面媒体挂件），见 media.rs。
mod media;
// 播放控制快捷键（可重绑），见 shortcuts.rs。
mod shortcuts;
// 单实例防护：第二次启动只把已有窗口提到前台，见 single_instance.rs。
mod single_instance;
// macOS 菜单栏与标准快捷键（⌘Q/⌘M/⌘W），见 menus.rs。
#[cfg(target_os = "macos")]
mod menus;

use gpui::{
    px, size, App, AppContext as _, Bounds, Entity, TitlebarOptions, WindowBounds, WindowOptions,
};
use std::sync::Arc;

/// 更紧凑的启动窗口尺寸（逻辑像素）；通过 Bounds::centered 在主屏幕居中。
const DEFAULT_SIZE: (f32, f32) = (980.0, 640.0);
const MIN_SIZE: (f32, f32) = (900.0, 560.0);

fn main_window_options(cx: &mut App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        titlebar: Some(TitlebarOptions {
            title: Some("SodaM".into()),
            // macOS 隐藏系统标题栏（红绿灯悬浮）；Windows 同样隐藏，
            // 由 ui::titlebar 自绘拖拽区与最小化/最大化/关闭按钮。
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            appears_transparent: true,
            ..Default::default()
        }),
        app_id: Some("SodaM".into()),
        icon: Some(Arc::new(
            image::load_from_memory(include_bytes!("../assets/brand/sodam-logo-tray.png"))
                .expect("内置应用图标应为有效 PNG")
                .to_rgba8(),
        )),
        ..Default::default()
    }
}

/// Windows 上取窗口的原生 HWND（系统媒体控件挂在窗口上）；其它平台没有这个概念。
#[cfg(target_os = "windows")]
fn native_handle(window: &gpui::Window) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // 必须显式写 trait 名：gpui 的 `Window` 自带一个同名的 `window_handle()`（返回
    // `AnyWindowHandle`），方法语法会优先命中那个，拿不到原生句柄。
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as *mut std::ffi::c_void),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn native_handle(_window: &gpui::Window) -> Option<*mut std::ffi::c_void> {
    None
}

/// 托盘事件循环：把命令转发为应用动作，并周期同步播放状态到 `sync`。
/// 双平台共用；平台差异只在 `sync` 闭包（见 tray.rs）。
fn start_tray_service(
    rx: std::sync::mpsc::Receiver<tray::TrayCommand>,
    app: Entity<app::Root>,
    cx: &mut App,
    mut sync: impl FnMut(tray::TrayState) + 'static,
) {
    cx.spawn(async move |cx| {
        let mut tick = 0u32;
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
            while let Ok(command) = rx.try_recv() {
                match command {
                    tray::TrayCommand::Show => cx.update(|cx| show_window(cx, &app)),
                    tray::TrayCommand::Toggle => {
                        cx.update(|cx| app.update(cx, |root, cx| root.toggle_play(cx)))
                    }
                    tray::TrayCommand::Previous => {
                        cx.update(|cx| app.update(cx, |root, cx| root.prev_track(cx)))
                    }
                    tray::TrayCommand::Next => {
                        cx.update(|cx| app.update(cx, |root, cx| root.next_track(cx)))
                    }
                    tray::TrayCommand::Quit => cx.update(|cx| cx.quit()),
                };
            }

            // 系统媒体控件（媒体键 / 桌面挂件按钮）的请求 → 播放动作。
            for command in cx.update(|cx| media::drain_commands(cx)) {
                cx.update(|cx| {
                    app.update(cx, |root, cx| {
                        let playing = root.engine.snapshot().playing;
                        match command {
                            media::Command::Play if !playing => root.toggle_play(cx),
                            media::Command::Pause if playing => root.toggle_play(cx),
                            media::Command::Toggle => root.toggle_play(cx),
                            media::Command::Next => root.next_track(cx),
                            media::Command::Previous => root.prev_track(cx),
                            _ => {}
                        }
                    })
                });
            }

            // 设置页重绑快捷键后置位，这里重新注册（清空 + 重建 keymap，见 shortcuts::apply）。
            let settings_changed = cx.update(|cx| {
                app.update(cx, |root, _cx| {
                    let dirty = root.shortcuts_dirty;
                    root.shortcuts_dirty = false;
                    dirty
                })
            });
            if settings_changed {
                let settings = cx.update(|cx| app.read(cx).settings.clone());
                cx.update(|cx| shortcuts::apply(cx, &settings));
            }

            tick += 1;
            if tick % 5 == 0 {
                let now = cx.update(|cx| {
                    app.update(cx, |root, _cx| {
                        let snapshot = root.engine.snapshot();
                        let track = root.queue.current();
                        let title = if snapshot.track_id.is_empty() {
                            "SodaM".to_string()
                        } else {
                            snapshot.title.clone()
                        };
                        let subtitle = if snapshot.track_id.is_empty() {
                            root.tr("就绪").to_string()
                        } else {
                            track.map(|track| track.artist.clone()).unwrap_or_default()
                        };
                        sync(tray::TrayState {
                            language: root.language,
                            title,
                            subtitle,
                            playing: snapshot.playing,
                            has_track: !snapshot.track_id.is_empty(),
                            cover_path: track.and_then(|track| root.cover_of(&track.cover)),
                        });
                        // 顺手发布给系统媒体面板（内容没变时 publish 内部会跳过）。
                        media::NowPlaying {
                            track_id: snapshot.track_id.clone(),
                            title: snapshot.title.clone(),
                            artist: track.map(|t| t.artist.clone()).unwrap_or_default(),
                            album: track.map(|t| t.album.clone()).unwrap_or_default(),
                            cover_path: track.and_then(|t| root.cover_of(&t.cover)),
                            duration_seconds: snapshot.duration_seconds,
                            position_seconds: snapshot.position_seconds,
                            playing: snapshot.playing,
                        }
                    })
                });
                cx.update(|cx| media::publish(cx, &now));
            }
        }
    })
    .detach();
}

/// 打开或激活主窗口；由托盘的 Show 命令调用。
fn show_window(cx: &mut App, app: &Entity<app::Root>) {
    if let Some(window) = cx.windows().first() {
        let _ = window.update(cx, |_, window, _| window.activate_window());
    } else {
        let app = app.clone();
        let options = main_window_options(cx);
        cx.open_window(options, |_window, _cx| app).ok();
    }
    cx.activate(true);
}

/// 第二个实例启动时发来的「把窗口提到前台」请求。
///
/// socket 监听在独立线程，这里用轮询把信号搬到 UI 线程 —— 不能在渲染线程上
/// 阻塞 `recv()`，200ms 的延迟对这个操作完全够用。
fn start_single_instance_service(
    activations: std::sync::mpsc::Receiver<()>,
    app: Entity<app::Root>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| loop {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(200))
            .await;
        while activations.try_recv().is_ok() {
            cx.update(|cx| show_window(cx, &app));
        }
    })
    .detach();
}

fn main() {
    // 单实例：第二次启动不再开第二个窗口，只把已在跑的窗口提到前台。
    // Secondary 时已经代为请求前台，本进程直接退出（静默，符合 issue #14）。
    let instance = single_instance::acquire();
    let single_instance::Instance::Primary {
        _guard,
        activations,
    } = instance
    else {
        return;
    };

    gpui_platform::application()
        .with_assets(ui::icons::Assets)
        .with_quit_mode(gpui::QuitMode::Explicit)
        .run(|cx: &mut App| {
            #[allow(clippy::redundant_closure)]
            let app = cx.new(|cx| app::Root::new(cx));
            // macOS：装好菜单栏与 ⌘Q/⌘M/⌘W（菜单标题跟随当前语言）。
            #[cfg(target_os = "macos")]
            {
                let language = app.read(cx).language;
                menus::install(cx, language);
            }
            // 播放快捷键：应用内三键（可在设置页重绑）。
            let settings = app.read(cx).settings.clone();
            shortcuts::install(cx, &settings, app.clone());
            let options = main_window_options(cx);
            cx.open_window(options, |window, cx| {
                // 系统媒体控制要等窗口存在：Windows 的 SMTC 挂在 HWND 上。
                media::attach(native_handle(window), cx);
                app.clone()
            })
            .expect("打开主窗口失败");

            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            {
                let (tray_tx, tray_rx) = std::sync::mpsc::channel();

                // 平台各自的托盘创建 + 状态同步闭包。
                #[cfg(target_os = "linux")]
                let sync = {
                    let tray_service = ksni::TrayService::new(tray::linux::SodaTray::new(
                        tray_tx,
                        crate::ui::i18n::Language::system_locale(),
                    ));
                    let tray_handle = tray_service.handle();
                    tray_service.spawn();
                    move |state: tray::TrayState| {
                        // ksni 0.2 的 Handle::update 返回 ()，直接调用。
                        tray_handle.update(|tray| {
                            tray.language = state.language;
                            tray.title = state.title;
                            tray.subtitle = state.subtitle;
                            tray.playing = state.playing;
                            // 之前漏了这两个：菜单顶部「正在播放」那行由 has_track 决定，
                            // 不同步的话播着歌也一直显示「未在播放」。
                            tray.has_track = state.has_track;
                            tray.cover_path = state.cover_path.clone();
                        });
                    }
                };
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                let sync =
                    tray::create_status_item(tray_tx, crate::ui::i18n::Language::system_locale());

                start_tray_service(tray_rx, app.clone(), cx, sync);
            }

            start_single_instance_service(activations, app.clone(), cx);

            cx.activate(true);
        });
}
