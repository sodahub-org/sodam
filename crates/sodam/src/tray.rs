//! 系统托盘，三平台实现：
//! * Linux：StatusNotifierItem（KDE/freedesktop 协议，ksni）
//! * macOS：原生 NSStatusItem（tray-icon）
//! * Windows：shell 通知区域（tray-icon）
//!
//! 各平台保持同一结构：菜单发送 [`TrayCommand`]，GPUI 实体轮询消费，
//! 播放状态通过 [`TrayState`] 回调同步回托盘。

use std::sync::mpsc::Sender;

use crate::ui::i18n::Language;

// Linux SNI 与 Windows 通知区域使用品牌彩色图标。
#[cfg(any(target_os = "linux", target_os = "windows"))]
const ICON_PNG: &[u8] = include_bytes!("../assets/brand/sodam-logo-tray.png");

/// macOS 菜单栏模板图：纯黑 + 透明，由 NSStatusItem 按系统明暗主题自动反色。
#[cfg(target_os = "macos")]
const ICON_TEMPLATE_PNG: &[u8] = include_bytes!("../assets/brand/sodam-tray-template.png");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    Toggle,
    Previous,
    Next,
    Quit,
}

/// 托盘点击的平台差异收敛在这里，便于三端单测。
///
/// Linux 走 SNI 的 `activate`，不经过 `tray-icon`；macOS/Windows 共用
/// `create_status_item`，但只有 Windows 把左键从菜单里拿出来。
#[cfg(any(target_os = "macos", target_os = "windows", test))]
#[allow(dead_code)]
mod semantics {
    use super::TrayCommand;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Platform {
        Windows,
        Macos,
        Linux,
    }

    impl Platform {
        pub(super) const fn current() -> Self {
            #[cfg(target_os = "windows")]
            {
                Self::Windows
            }
            #[cfg(target_os = "macos")]
            {
                Self::Macos
            }
            #[cfg(target_os = "linux")]
            {
                Self::Linux
            }
        }

        /// macOS 的菜单栏图标保持系统惯例：左键打开菜单。
        pub(super) const fn menu_on_left_click(self) -> bool {
            matches!(self, Self::Macos)
        }

        /// Windows/Linux 左键单击聚焦/唤起主窗口。
        #[cfg(any(target_os = "windows", test))]
        pub(super) const fn left_click_focuses_window(self) -> bool {
            matches!(self, Self::Windows | Self::Linux)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum ClickButton {
        Left,
        Right,
        Middle,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum ClickState {
        Up,
        Down,
    }

    /// 只认左键抬起：按下不重复触发，右键/中键永远不负责开窗。
    #[cfg(any(target_os = "windows", test))]
    pub(super) fn command_for_click(button: ClickButton, state: ClickState) -> Option<TrayCommand> {
        if state != ClickState::Up {
            return None;
        }
        match button {
            ClickButton::Left => Some(TrayCommand::Show),
            ClickButton::Right | ClickButton::Middle => None,
        }
    }

    /// 菜单 ID → 命令。保留原有菜单项，不因左键接管而删减。
    pub(super) fn command_for_menu_id(id: &str) -> Option<TrayCommand> {
        match id {
            // 点了「正在播放」那行就把主窗口叫出来（没有曲目详情页可跳）。
            "caption" | "show" => Some(TrayCommand::Show),
            "toggle" => Some(TrayCommand::Toggle),
            "prev" => Some(TrayCommand::Previous),
            "next" => Some(TrayCommand::Next),
            "quit" => Some(TrayCommand::Quit),
            _ => None,
        }
    }
}

/// 从 Root 周期同步到托盘的播放状态摘要。
#[derive(Debug, Clone)]
pub struct TrayState {
    pub language: Language,
    pub title: String,
    pub subtitle: String,
    pub playing: bool,
    /// 当前是否有曲目（决定菜单里那行「正在播放」显示曲名还是「未在播放」）。
    pub has_track: bool,
    /// 本地缓存的封面文件，菜单里那行「正在播放」用它当图标。
    pub cover_path: Option<std::path::PathBuf>,
}

/// 菜单里「正在播放」那行最多显示多少个字符（和 sonora 一样取 26）。
const CAPTION_LIMIT: usize = 26;

/// 菜单行里的封面边长（像素）。macOS 无论如何都按 18pt 画，Windows 按原尺寸画。
const COVER: u32 = 32;

impl TrayState {
    /// 菜单顶部那行「艺人 – 曲名」；没有曲目时给一句「未在播放」。
    ///
    /// macOS/Windows 用它当菜单首行与 tooltip，Linux 用它当 SNI 标题/tooltip。
    pub fn caption(&self) -> String {
        if !self.has_track {
            return self.language.text("未在播放").to_string();
        }
        let full = if self.subtitle.trim().is_empty() {
            self.title.clone()
        } else {
            format!("{} – {}", self.subtitle, self.title)
        };
        clip(&full, CAPTION_LIMIT)
    }
}

/// 按字符截断，超出部分用省略号（多字节字符也安全）。
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(limit.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
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
        pub(crate) has_track: bool,
        pub(crate) cover_path: Option<std::path::PathBuf>,
        /// 封面缩略图（PNG 字节），按源路径缓存（同步每 500ms 一次，不必反复解码）。
        cover_cache: std::cell::RefCell<Option<(std::path::PathBuf, Vec<u8>)>>,
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
                has_track: false,
                cover_path: None,
                cover_cache: std::cell::RefCell::new(None),
                icon_cache: OnceLock::new(),
            }
        }

        fn state(&self) -> TrayState {
            TrayState {
                language: self.language,
                title: self.title.clone(),
                subtitle: self.subtitle.clone(),
                playing: self.playing,
                has_track: self.has_track,
                cover_path: self.cover_path.clone(),
            }
        }

        /// 当前曲目封面的菜单图标。
        ///
        /// DBusMenu 的 `icon-data` 是 **PNG 字节**（不是 SNI 顶栏图标那种裸 ARGB32），
        /// ksni 只是把字节原样透传；之前传 ARGB32，宿主解不出图，菜单第一行就没封面。
        fn cover_png(&self) -> Vec<u8> {
            let state = self.state();
            let Some(path) = state.cover_path.as_ref() else {
                return Vec::new();
            };
            if let Some((cached, data)) = self.cover_cache.borrow().as_ref() {
                if cached == path {
                    return data.clone();
                }
            }
            let data = std::fs::read(path)
                .ok()
                .and_then(|bytes| image::load_from_memory(&bytes).ok())
                .map(|image| {
                    let thumbnail = image.thumbnail(COVER, COVER);
                    let mut png = Vec::new();
                    if thumbnail
                        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                        .is_err()
                    {
                        png.clear();
                    }
                    png
                })
                .unwrap_or_default();
            *self.cover_cache.borrow_mut() = Some((path.clone(), data.clone()));
            data
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
            self.state().caption()
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            vec![self.icon()]
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayCommand::Show);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let caption = self.state().caption();
            vec![
                // 当前播放信息：封面 + 「艺人 – 曲名」。有曲目时点它开主窗口，
                // 没曲目时禁用（避免看起来能点却没反应）。
                StandardItem {
                    label: caption,
                    icon_data: self.cover_png(),
                    enabled: self.has_track,
                    activate: Box::new(|this: &mut Self| this.send(TrayCommand::Show)),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
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

// ------------------------------------------------- macOS / Windows (tray-icon)

/// macOS（NSStatusItem）/ Windows（通知区域）共用：创建托盘图标，
/// 返回把 [`TrayState`] 同步进托盘的回调。
/// 必须在主线程调用（GPUI `Application::run` 闭包内即主线程）。
/// 菜单文案与 tooltip（当前曲目摘要）随每次同步更新，包括语言切换。
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn create_status_item(
    tx: Sender<TrayCommand>,
    language: Language,
) -> impl FnMut(TrayState) + 'static {
    use tray_icon::menu::accelerator::Accelerator;
    use tray_icon::menu::{IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIconBuilder};
    #[cfg(target_os = "windows")]
    use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

    let text = |key: &'static str| language.text(key).to_string();
    // 顶部那行「封面 + 艺人 – 曲名」：不参与动作，点了就开主窗口。
    let caption_item = IconMenuItem::with_id("caption", "SodaM", true, None, None::<Accelerator>);
    let toggle_item = MenuItem::with_id("toggle", text("播放"), true, None::<Accelerator>);
    let prev_item = MenuItem::with_id("prev", text("上一首"), true, None::<Accelerator>);
    let next_item = MenuItem::with_id("next", text("下一首"), true, None::<Accelerator>);
    let show_item = MenuItem::with_id("show", text("显示主窗口"), true, None::<Accelerator>);
    let quit_item = MenuItem::with_id("quit", text("退出"), true, None::<Accelerator>);
    let menu = Menu::new();
    menu.append_items(&[
        &caption_item,
        &PredefinedMenuItem::separator(),
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
        // macOS 状态栏遵循系统惯例使用单色模板图（浅色菜单栏黑色、深色菜单栏白色）；
        // Windows 通知区域保留品牌彩色图标。
        #[cfg(target_os = "macos")]
        let icon_png: &[u8] = ICON_TEMPLATE_PNG;
        #[cfg(not(target_os = "macos"))]
        let icon_png: &[u8] = ICON_PNG;
        let image = image::load_from_memory(icon_png)
            .expect("内置托盘图标应为有效 PNG")
            .to_rgba8();
        Icon::from_rgba(image.to_vec(), image.width(), image.height())
            .expect("托盘图标 RGBA 尺寸应一致")
    };

    let tray = TrayIconBuilder::new()
        .with_id("sodam")
        .with_icon(icon)
        .with_icon_as_template(cfg!(target_os = "macos"))
        .with_menu(Box::new(menu))
        // macOS 保持“左键开菜单”；Windows 把左键留给“聚焦/唤起主窗口”。
        .with_menu_on_left_click(semantics::Platform::current().menu_on_left_click())
        .with_tooltip("SodaM")
        .build()
        .expect("创建 macOS 托盘不应失败");

    // Windows 的左键点击通过 TrayIconEvent 单独接出来；macOS 不注册，
    // 继续由 NSStatusItem 的菜单接管左键。
    #[cfg(target_os = "windows")]
    let click_tx = tx.clone();

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some(command) = semantics::command_for_menu_id(event.id().as_ref()) {
            let _ = tx.send(command);
        }
    }));

    #[cfg(target_os = "windows")]
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        // Linux 不经过这里；macOS 没有注册这个 handler。
        if !semantics::Platform::current().left_click_focuses_window() {
            return;
        }
        let TrayIconEvent::Click {
            button,
            button_state,
            ..
        } = event
        else {
            return;
        };
        let button = match button {
            MouseButton::Left => semantics::ClickButton::Left,
            MouseButton::Right => semantics::ClickButton::Right,
            MouseButton::Middle => semantics::ClickButton::Middle,
        };
        let state = match button_state {
            MouseButtonState::Up => semantics::ClickState::Up,
            MouseButtonState::Down => semantics::ClickState::Down,
        };
        if let Some(command) = semantics::command_for_click(button, state) {
            let _ = click_tx.send(command);
        }
    }));

    // 封面每 500ms 会被同步一次，按源路径缓存解码结果。
    let mut cover_cache: Option<(std::path::PathBuf, tray_icon::menu::Icon)> = None;

    move |state: TrayState| {
        let language = state.language;
        let caption = state.caption();
        caption_item.set_text(caption.clone());
        caption_item.set_enabled(state.has_track);
        let cover = state.cover_path.as_ref().and_then(|path| {
            if cover_cache
                .as_ref()
                .is_some_and(|(cached, _)| cached == path)
            {
                return cover_cache.as_ref().map(|(_, icon)| icon.clone());
            }
            let icon = cover_icon(path)?;
            cover_cache = Some((path.clone(), icon.clone()));
            Some(icon)
        });
        caption_item.set_icon(cover);

        toggle_item.set_text(if state.playing {
            language.text("暂停")
        } else {
            language.text("播放")
        });
        prev_item.set_text(language.text("上一首"));
        next_item.set_text(language.text("下一首"));
        show_item.set_text(language.text("显示主窗口"));
        quit_item.set_text(language.text("退出"));
        // tooltip 跟菜单那行一致，悬停图标就能看到在放什么。
        let _ = tray.set_tooltip(Some(caption));
    }
}

/// 读本地缓存的封面，缩到菜单行尺寸并转成菜单图标。
///
/// 注意类型：菜单项要的是 `tray_icon::menu::Icon`，和状态栏自己的 `tray_icon::Icon`
/// 不是一个类型。失败返回 `None`：那一行只显示文字，不因为封面读不出来影响整个菜单。
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn cover_icon(path: &std::path::Path) -> Option<tray_icon::menu::Icon> {
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory(&bytes)
        .ok()?
        .thumbnail(COVER, COVER)
        .into_rgba8();
    let (width, height) = image.dimensions();
    tray_icon::menu::Icon::from_rgba(image.into_raw(), width, height).ok()
}

#[cfg(test)]
mod tests {
    use super::semantics::{
        command_for_click, command_for_menu_id, ClickButton, ClickState, Platform,
    };
    use super::TrayCommand;

    // D-001 / D-002：Windows/Linux 左键聚焦，macOS 左键保持菜单。
    #[test]
    fn windows_left_click_focuses_window() {
        assert!(Platform::Windows.left_click_focuses_window());
        assert!(!Platform::Windows.menu_on_left_click());
        assert_eq!(
            command_for_click(ClickButton::Left, ClickState::Up),
            Some(TrayCommand::Show)
        );
    }

    #[test]
    fn macos_left_click_keeps_menu() {
        assert!(!Platform::Macos.left_click_focuses_window());
        assert!(Platform::Macos.menu_on_left_click());
    }

    #[test]
    fn linux_left_click_focuses_window() {
        assert!(Platform::Linux.left_click_focuses_window());
    }

    // D-002：只有左键抬起开窗；右键/中键、按下、重复点击都不改变语义。
    #[test]
    fn only_left_button_up_opens_window() {
        for button in [ClickButton::Right, ClickButton::Middle] {
            assert_eq!(command_for_click(button, ClickState::Up), None);
            assert_eq!(command_for_click(button, ClickState::Down), None);
        }
        assert_eq!(command_for_click(ClickButton::Left, ClickState::Down), None);
        assert_eq!(
            command_for_click(ClickButton::Left, ClickState::Up),
            Some(TrayCommand::Show)
        );
        assert_eq!(
            command_for_click(ClickButton::Left, ClickState::Up),
            Some(TrayCommand::Show)
        );
    }

    // D-003：菜单入口不因左键接管而删减。
    #[test]
    fn menu_entries_keep_existing_commands() {
        assert_eq!(command_for_menu_id("caption"), Some(TrayCommand::Show));
        assert_eq!(command_for_menu_id("show"), Some(TrayCommand::Show));
        assert_eq!(command_for_menu_id("toggle"), Some(TrayCommand::Toggle));
        assert_eq!(command_for_menu_id("prev"), Some(TrayCommand::Previous));
        assert_eq!(command_for_menu_id("next"), Some(TrayCommand::Next));
        assert_eq!(command_for_menu_id("quit"), Some(TrayCommand::Quit));
        assert_eq!(command_for_menu_id("unknown"), None);
    }
}
