//! 图标：内置 Lucide 线性 SVG（ISC 许可），统一从 `path()` 引用。
//!
//! 全部同一套描边、同一个尺寸档（`theme::ICON` 等），不再用 emoji 或文字符号 ——
//! emoji 的字形、基线、颜色由字体决定，大小和对齐都没法控。

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub mod scene_icons;

const BRAND_LOGO_SVG: &[u8] = include_bytes!("../../assets/brand/sodam-logo.svg");

const IMAGES: &[(&str, &[u8])] = &[
    (
        "sodam-logo",
        include_bytes!("../../assets/brand/sodam-logo.png"),
    ),
    (
        "familiar-mode",
        include_bytes!("../../assets/icons/familiar-mode.png"),
    ),
    (
        "fresh-mode",
        include_bytes!("../../assets/icons/fresh-mode.png"),
    ),
    (
        "familiar-mode-dark",
        include_bytes!("../../assets/icons/familiar-mode-dark.png"),
    ),
    (
        "fresh-mode-dark",
        include_bytes!("../../assets/icons/fresh-mode-dark.png"),
    ),
];

macro_rules! icon_set {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $(
                (
                    $name,
                    include_bytes!(concat!("../../assets/icons/", $name, ".svg")),
                ),
            )*
        ];

        /// 注册给 GPUI 的资产源（`Application::new().with_assets(Assets)`）。
        pub struct Assets;

        impl AssetSource for Assets {
            fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
                if path == "icons/sodam-logo.svg" {
                    return Ok(Some(Cow::Borrowed(BRAND_LOGO_SVG)));
                }

                let name = if path.starts_with("icons/") && path.ends_with(".png") {
                    path
                } else {
                    path.trim_start_matches("icons/")
                        .trim_end_matches(".svg")
                };
                if let Some(bytes) = ICONS
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, bytes)| Cow::Borrowed(*bytes))
                {
                    return Ok(Some(bytes));
                }
                if path.starts_with("icons/") && path.ends_with(".png") {
                    let name = path
                        .trim_start_matches("icons/")
                        .trim_end_matches(".png");
                    if let Some(bytes) = IMAGES
                        .iter()
                        .find(|(key, _)| *key == name)
                        .map(|(_, bytes)| Cow::Borrowed(*bytes))
                    {
                        return Ok(Some(bytes));
                    }
                }
                if path.starts_with("icons/scenes/") && path.ends_with(".png") {
                    let name = path
                        .trim_start_matches("icons/scenes/")
                        .trim_end_matches(".png");
                    if let Some(bytes) = if name.ends_with("-dark") {
                        scene_icons::dark_icon_bytes(name)
                    } else {
                        scene_icons::icon_bytes(name)
                    } {
                        return Ok(Some(Cow::Borrowed(bytes)));
                    }
                }
                Ok(None)
            }

            fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
                Ok(ICONS
                    .iter()
                    .map(|(name, _)| SharedString::from(format!("icons/{name}.svg")))
                    .collect())
            }
        }
    };
}

icon_set!(
    "house",
    "radio",
    "search",
    "heart",
    "heart-filled",
    "list-music",
    "log-in",
    "settings",
    "play",
    "pause",
    "skip-back",
    "skip-forward",
    "repeat",
    "shuffle",
    "volume-2",
    "volume-x",
    "list-end",
    "music",
    "spinner",
    "chevron-left",
    "chevron-right",
    "plus",
    "x",
    "disc-3",
);

/// 听歌模式内置图标路径。
pub fn scene_image_path(sub_queue_type: &str, dark: bool) -> Option<SharedString> {
    let key = if dark {
        format!("{sub_queue_type}-dark")
    } else {
        sub_queue_type.to_string()
    };
    let exists = if dark {
        scene_icons::dark_icon_bytes(&key).is_some()
    } else {
        scene_icons::icon_bytes(sub_queue_type).is_some()
    };
    if !exists {
        return None;
    }
    Some(SharedString::from(format!("icons/scenes/{key}.png")))
}

/// 图标的资产路径，喂给 `svg().path(icons::path("play"))`。
pub fn path(name: &str) -> SharedString {
    SharedString::from(format!("icons/{name}.svg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_scene_icons_exist_for_both_themes() {
        assert!(scene_image_path("scene_mode_emo", false).is_some());
        assert!(scene_image_path("scene_mode_emo", true).is_some());
    }

    #[test]
    fn asset_source_loads_sodam_brand_png() {
        let bytes = Assets
            .load("icons/sodam-logo.png")
            .expect("brand asset load")
            .expect("brand PNG bytes");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn asset_source_loads_multicolor_sodam_brand_svg() {
        let bytes = Assets
            .load("icons/sodam-logo.svg")
            .expect("brand SVG asset load")
            .expect("brand SVG bytes");
        assert!(bytes.starts_with(b"<?xml"));
    }

    #[test]
    fn asset_source_loads_builtin_scene_icons() {
        let light = Assets
            .load("icons/scenes/scene_mode_emo-dark.png")
            .expect("asset load")
            .expect("light icon bytes");
        let dark = Assets
            .load("icons/scenes/scene_mode_emo.png")
            .expect("asset load")
            .expect("dark icon bytes");
        assert!(!light.is_empty());
        assert!(!dark.is_empty());
    }
}
