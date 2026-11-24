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
// 帧日志循环计数，见 loop_probe.rs。
mod loop_probe;
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::loop_probe::LoopProbe;
use sodam_core::settings_writer::StartupFailureSlot;

/// 更紧凑的启动窗口尺寸（逻辑像素）；通过 Bounds::centered 在主屏幕居中。
const DEFAULT_SIZE: (f32, f32) = (980.0, 640.0);
const MIN_SIZE: (f32, f32) = (900.0, 560.0);
const MAX_ACTIVATIONS_PER_TICK: usize = 16;

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

struct TraySignals {
    media_commands: Option<Arc<AtomicBool>>,
    shortcuts_dirty: Arc<AtomicBool>,
    startup_save: StartupFailureSlot,
}

/// 100ms 托盘拍：转发托盘和媒体命令，处理单实例激活、快捷键重绑、
/// 启动回存失败与配置巡检，并同步托盘和系统媒体状态。
/// 双平台共用；平台差异只在 `sync` 闭包（见 tray.rs）。
fn start_tray_service(
    rx: std::sync::mpsc::Receiver<tray::TrayCommand>,
    activations: std::sync::mpsc::Receiver<()>,
    app: Entity<app::Root>,
    signals: TraySignals,
    cx: &mut App,
    mut sync: impl FnMut(tray::TrayState) + 'static,
) {
    cx.spawn(async move |cx| {
        let mut tick = 0u32;
        let mut tray_probe = LoopProbe::new("tray");
        let mut single_probe = LoopProbe::new_with_timer("single", "shared:tray");
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
            tick = tick.wrapping_add(1);
            tray_probe.tick();

            if tick % 2 == 0 {
                single_probe.check();
                if activations.try_recv().is_ok() {
                    for _ in 1..MAX_ACTIVATIONS_PER_TICK {
                        if activations.try_recv().is_err() {
                            break;
                        }
                    }
                    single_probe.app_entry();
                    cx.update(|cx| show_window(cx, &app));
                }
            }

            while let Ok(command) = rx.try_recv() {
                tray_probe.app_entry();
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
                    // 先等启动期落盘做完再退（#198）；再按一次会直接退出。
                    tray::TrayCommand::Quit => {
                        cx.update(|cx| app.update(cx, |root, cx| root.request_quit(cx)))
                    }
                };
            }

            if signals
                .media_commands
                .as_ref()
                .is_some_and(|pending| pending.load(Ordering::Acquire))
            {
                tray_probe.signal_app_entry();
                cx.update(|cx| {
                    let commands = media::drain_commands(cx);
                    app.update(cx, |root, cx| {
                        for command in commands {
                            tray_probe.engine_snapshot();
                            let playing = root.engine.snapshot().playing;
                            match command {
                                media::Command::Play if !playing => root.toggle_play(cx),
                                media::Command::Pause if playing => root.toggle_play(cx),
                                media::Command::Toggle => root.toggle_play(cx),
                                media::Command::Next => root.next_track(cx),
                                media::Command::Previous => root.prev_track(cx),
                                _ => {}
                            }
                        }
                    });
                });
            }

            if signals.shortcuts_dirty.load(Ordering::Acquire) {
                tray_probe.signal_app_entry();
                cx.update(|cx| {
                    let settings = app.update(cx, |root, _cx| {
                        root.shortcuts_dirty.store(false, Ordering::Release);
                        root.settings.clone()
                    });
                    shortcuts::apply(cx, &settings);
                });
            }

            if signals.startup_save.is_pending() {
                tray_probe.signal_app_entry();
                cx.update(|cx| {
                    app.update(cx, |root, cx| root.report_startup_persist_result(cx));
                });
            }

            // 配置热重载巡检：每 ~500ms 一次，读盘与解析放后台线程，结果回 UI 应用（#185）。
            // 错开托盘同步的那一拍，避免同帧做两批后台活。
            if tick % 5 == 2 {
                tray_probe.app_entry();
                let request = cx.update(|cx| app.update(cx, |root, _cx| root.begin_config_poll()));
                if let Some(request) = request {
                    let app = app.clone();
                    let app_entries = tray_probe.app_entry_counter();
                    cx.spawn(async move |cx| {
                        let (event, snapshot) = cx
                            .background_executor()
                            .spawn(async move { request.poll() })
                            .await;
                        app_entries.fetch_add(1, Ordering::Relaxed);
                        app.update(cx, |root, cx| root.finish_config_poll(event, snapshot, cx));
                    })
                    .detach();
                }
            }
            if tick % 5 == 0 {
                tray_probe.app_entry();
                cx.update(|cx| {
                    let now = app.update(cx, |root, _cx| {
                        tray_probe.engine_snapshot();
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
                    });
                    media::publish(cx, &now);
                });
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

    sodam_core::log_outlet::init();

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
                menus::install(cx, language, app.downgrade());
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

                let signals = TraySignals {
                    media_commands: media::command_signal(cx),
                    shortcuts_dirty: app.read(cx).shortcuts_dirty.clone(),
                    startup_save: app.read(cx).settings_writer.startup_failure_slot(),
                };
                start_tray_service(tray_rx, activations, app.clone(), signals, cx, sync);
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
            let _ = activations;

            cx.activate(true);
        });
}
