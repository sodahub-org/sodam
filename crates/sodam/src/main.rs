//! SodaM——GPUI 原生客户端入口。

mod app;
// 托盘：Linux 走 ksni/SNI，macOS 走 NSStatusItem，见 tray.rs。
mod tray;
mod ui;
mod views;

use gpui::{
    px, size, App, AppContext as _, Bounds, Entity, TitlebarOptions, WindowBounds, WindowOptions,
};
use std::sync::Arc;

/// 启动窗口尺寸（参考官方客户端的 16:10 主窗口）。
const DEFAULT_SIZE: (f32, f32) = (1180.0, 760.0);
const MIN_SIZE: (f32, f32) = (900.0, 560.0);

fn main_window_options(cx: &mut App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        titlebar: Some(TitlebarOptions {
            title: Some("SodaM".into()),
            // macOS 隐藏系统标题栏，内容延伸到红绿灯下，由应用自绘顶部留白；
            // Linux 走 compositor 窗口装饰，保持不动。
            #[cfg(target_os = "macos")]
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

            tick += 1;
            if tick % 5 == 0 {
                cx.update(|cx| {
                    app.update(cx, |root, _cx| {
                        let snapshot = root.engine.snapshot();
                        let title = if snapshot.track_id.is_empty() {
                            "SodaM".to_string()
                        } else {
                            snapshot.title
                        };
                        let subtitle = if snapshot.track_id.is_empty() {
                            root.tr("就绪").to_string()
                        } else {
                            root.queue
                                .current()
                                .map(|track| track.artist.clone())
                                .unwrap_or_default()
                        };
                        sync(tray::TrayState {
                            language: root.language,
                            title,
                            subtitle,
                            playing: snapshot.playing,
                        });
                    });
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
    gpui_platform::application()
        .with_assets(ui::icons::Assets)
        .with_quit_mode(gpui::QuitMode::Explicit)
        .run(|cx: &mut App| {
            #[allow(clippy::redundant_closure)]
            let app = cx.new(|cx| app::Root::new(cx));
            let options = main_window_options(cx);
            cx.open_window(options, |_window, _cx| app.clone())
                .expect("打开主窗口失败");

            #[cfg(any(target_os = "linux", target_os = "macos"))]
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
                        let _ = tray_handle.update(|tray| {
                            tray.language = state.language;
                            tray.title = state.title;
                            tray.subtitle = state.subtitle;
                            tray.playing = state.playing;
                        });
                    }
                };
                #[cfg(target_os = "macos")]
                let sync =
                    tray::create_status_item(tray_tx, crate::ui::i18n::Language::system_locale());

                start_tray_service(tray_rx, app.clone(), cx, sync);
            }

            cx.activate(true);
        });
}
