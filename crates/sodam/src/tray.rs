//! 系统托盘，双平台实现：
//! * Linux：StatusNotifierItem（KDE/freedesktop 协议，ksni）
//! * macOS：原生 NSStatusItem（tray-icon）
//!
//! 两边保持同一结构：菜单发送 [`TrayCommand`]，GPUI 实体轮询消费，
//! 播放状态通过 [`TrayState`] 回调同步回托盘。

use std::sync::mpsc::Sender;

use crate::ui::i18n::Language;

const ICON_PNG: &[u8] = include_bytes!("../assets/brand/sodam-logo-tray.png");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    Toggle,
    Previous,
    Next,
    Quit,
}

/// 从 Root 周期同步到托盘的播放状态摘要。
#[derive(Debug, Clone)]
pub struct TrayState {
    pub language: Language,
    pub title: String,
    pub subtitle: String,
    pub playing: bool,
}

impl TrayState {
    /// 托盘标题/tooltip 的单行摘要；未播放时只显示应用名。
    pub fn summary(&self) -> String {
        if self.title == "SodaM" {
            self.title.clone()
        } else {
            format!("{} · {}", self.title, self.subtitle)
        }
    }
}

// ---------------------------------------------------------------- Linux (SNI)

#[cfg(target_os = "linux")]
pub mod linux {
    use super::*;
    use ksni::menu::StandardItem;
    use ksni::{Icon, MenuItem, Tray};
    use std::sync::OnceLock;

    #[derive(Debug)]
    pub struct SodaTray {
        tx: Sender<TrayCommand>,
        pub(crate) language: Language,
        pub(crate) title: String,
        pub(crate) subtitle: String,
        pub(crate) playing: bool,
        icon_cache: OnceLock<Icon>,
    }

    impl SodaTray {
        pub fn new(tx: Sender<TrayCommand>, language: Language) -> Self {
            Self {
                tx,
                language,
                title: "SodaM".to_string(),
                subtitle: language.text("就绪").to_string(),
                playing: false,
                icon_cache: OnceLock::new(),
            }
        }

        fn send(&self, command: TrayCommand) {
            let _ = self.tx.send(command);
        }

        fn icon(&self) -> Icon {
            self.icon_cache
                .get_or_init(|| {
                    let image = image::load_from_memory(ICON_PNG)
                        .expect("内置托盘图标应为有效 PNG")
                        .to_rgba8();
                    // SNI ARGB32 按 32-bit 网络序读取：字节顺序是 A,R,G,B。
                    let mut data = Vec::with_capacity(image.as_raw().len());
                    for pixel in image.pixels() {
                        data.extend_from_slice(&[pixel[3], pixel[0], pixel[1], pixel[2]]);
                    }
                    Icon {
                        width: image.width() as i32,
                        height: image.height() as i32,
                        data,
                    }
                })
                .clone()
        }
    }

    impl Tray for SodaTray {
        fn id(&self) -> String {
            // 避免 SNI 宿主按相同 id 缓存旧图标。
            format!("sodam-{}", std::process::id())
        }

        fn title(&self) -> String {
            TrayState {
                language: self.language,
                title: self.title.clone(),
                subtitle: self.subtitle.clone(),
                playing: self.playing,
            }
            .summary()
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            vec![self.icon()]
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayCommand::Show);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                StandardItem {
                    label: if self.playing {
                        self.language.text("暂停")
                    } else {
                        self.language.text("播放")
                    }
                    .into(),
                    activate: Box::new(|this: &mut Self| this.send(TrayCommand::Toggle)),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: self.language.text("上一首").into(),
                    activate: Box::new(|this: &mut Self| this.send(TrayCommand::Previous)),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: self.language.text("下一首").into(),
                    activate: Box::new(|this: &mut Self| this.send(TrayCommand::Next)),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: self.language.text("显示主窗口").into(),
                    activate: Box::new(|this: &mut Self| this.send(TrayCommand::Show)),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: self.language.text("退出").into(),
                    activate: Box::new(|this: &mut Self| this.send(TrayCommand::Quit)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }
}

// ---------------------------------------------------------------- macOS (NSStatusItem)

/// macOS 上创建菜单栏状态项，返回把 [`TrayState`] 同步进托盘的回调。
/// 必须在主线程调用（GPUI `Application::run` 闭包内即主线程）。
/// 菜单文案与 tooltip（当前曲目摘要）随每次同步更新，包括语言切换。
#[cfg(target_os = "macos")]
pub fn create_status_item(
    tx: Sender<TrayCommand>,
    language: Language,
) -> impl FnMut(TrayState) + 'static {
    use tray_icon::menu::accelerator::Accelerator;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIconBuilder};

    let text = |key: &'static str| language.text(key).to_string();
    let toggle_item = MenuItem::with_id("toggle", text("播放"), true, None::<Accelerator>);
    let prev_item = MenuItem::with_id("prev", text("上一首"), true, None::<Accelerator>);
    let next_item = MenuItem::with_id("next", text("下一首"), true, None::<Accelerator>);
    let show_item = MenuItem::with_id("show", text("显示主窗口"), true, None::<Accelerator>);
    let quit_item = MenuItem::with_id("quit", text("退出"), true, None::<Accelerator>);
    let menu = Menu::new();
    menu.append_items(&[
        &toggle_item,
        &prev_item,
        &next_item,
        &PredefinedMenuItem::separator(),
        &show_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ])
    .expect("构建 macOS 托盘菜单不应失败");

    let icon = {
        let image = image::load_from_memory(ICON_PNG)
            .expect("内置托盘图标应为有效 PNG")
            .to_rgba8();
        Icon::from_rgba(image.to_vec(), image.width(), image.height())
            .expect("托盘图标 RGBA 尺寸应一致")
    };

    let tray = TrayIconBuilder::new()
        .with_id("sodam")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true)
        .with_tooltip("SodaM")
        .build()
        .expect("创建 macOS 托盘不应失败");

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let command = match event.id().as_ref() {
            "toggle" => TrayCommand::Toggle,
            "prev" => TrayCommand::Previous,
            "next" => TrayCommand::Next,
            "show" => TrayCommand::Show,
            "quit" => TrayCommand::Quit,
            _ => return,
        };
        let _ = tx.send(command);
    }));

    move |state: TrayState| {
        let language = state.language;
        toggle_item.set_text(if state.playing {
            language.text("暂停")
        } else {
            language.text("播放")
        });
        prev_item.set_text(language.text("上一首"));
        next_item.set_text(language.text("下一首"));
        show_item.set_text(language.text("显示主窗口"));
        quit_item.set_text(language.text("退出"));
        let _ = tray.set_tooltip(Some(state.summary()));
    }
}
