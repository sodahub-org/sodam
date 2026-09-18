//! 系统托盘：StatusNotifierItem（KDE/freedesktop 协议）。
//!
//! Linux 桌面大多通过 SNI 展示托盘；这里保持实现很薄：
//! 菜单发送命令，GPUI 实体轮询消费，托盘标题仅同步当前曲目摘要。

use std::sync::mpsc::Sender;

use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, Tray};

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

#[derive(Debug)]
pub struct SodaTray {
    tx: Sender<TrayCommand>,
    pub(crate) language: Language,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) playing: bool,
    icon_cache: std::sync::OnceLock<Icon>,
}

impl SodaTray {
    pub fn new(tx: Sender<TrayCommand>, language: Language) -> Self {
        Self {
            tx,
            language,
            title: "SodaM".to_string(),
            subtitle: language.text("就绪").to_string(),
            playing: false,
            icon_cache: std::sync::OnceLock::new(),
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
        if self.title == "SodaM" {
            self.title.clone()
        } else {
            format!("{} · {}", self.title, self.subtitle)
        }
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
