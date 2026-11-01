//! 有上界、**按显示尺寸解码**的封面缓存（照 sonora 的 `ArtworkCache` 移植）。
//!
//! gpui 的默认图片加载是「原分辨率解码」：歌词页原图 960×960 解出来就是 ~3.7MB，
//! 几十张就吃掉几百 MB 显存/堆。sonora 的做法是让缓存 key 带上**目标边长 `edge`**，
//! 解码后先 `resize_exact` 到实际显示尺寸再交给 gpui，于是 48px 的缩略图只占 64×64。
//!
//! 预算也小得多：16MB 起步，并随「一个满屏像素」增长；再加 256 条的条数上限。
//! （sonora 实测 footprint 67MB / IOAccelerator 8.4MB。）

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, AppContext, Asset, AssetLogger, Entity, Global, ImageCache, ImageCacheError, RenderImage,
    Resource, Window,
};
use image::{imageops, DynamicImage, Frame, ImageFormat, RgbaImage};
use sodam_core::log_outlet::{self, log_event, LogLevel, LogTopic};

/// 缓存预算下限（超出后淘汰）；实际预算 = `max(这个, 一个满屏像素)`。
const CACHE_BYTES: usize = 16 * 1024 * 1024;
/// 条数上限。
const CACHE_ITEMS: usize = 256;
/// 解码边长向上取整到 64 的倍数（2 的幂会让像素数最多翻 4 倍）。
const EDGE_STEP: u32 = 64;
/// 解码边长上限：只用来拦住几千像素的本地原图整张保留。
const MAX_SAMPLE_EDGE: u32 = 4096;

type Key = (Resource, u32);

/// 一个 widget 的显示尺寸 → 解码边长（物理像素，向上取整到 [`EDGE_STEP`]）。
pub fn sample_edge(size: f32, scale: f32) -> u32 {
    let physical = (size * scale).ceil().max(1.0) as u32;
    physical.next_multiple_of(EDGE_STEP).min(MAX_SAMPLE_EDGE)
}

/// 解码源：`(资源, 目标边长)`，因此同一张图在不同尺寸下是不同缓存项。
#[derive(Clone, Hash)]
struct CoverSource {
    resource: Resource,
    edge: u32,
}

enum CoverLoader {}

impl Asset for CoverLoader {
    type Source = CoverSource;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let asset_source = cx.asset_source().clone();
        let svg_renderer = cx.svg_renderer();
        async move {
            let bytes = match &source.resource {
                Resource::Path(path) => std::fs::read(path.as_ref())
                    .map_err(|err| ImageCacheError::Io(Arc::new(err)))?,
                Resource::Embedded(path) => asset_source
                    .load(path)
                    .ok()
                    .flatten()
                    .map(|data| data.into_owned())
                    .ok_or_else(|| {
                        ImageCacheError::Asset(format!("封面资源不存在：{path}").into())
                    })?,
                Resource::Uri(uri) => {
                    return Err(ImageCacheError::Asset(
                        format!("封面缓存不支持远程 URL：{uri}").into(),
                    ));
                }
            };
            // 非栅格（SVG 图标之类）交给 gpui 的 svg 渲染器。
            let Ok(format) = image::guess_format(&bytes) else {
                return svg_renderer
                    .render_single_frame(&bytes, 1.0)
                    .map_err(|err| {
                        ImageCacheError::Asset(format!("渲染矢量图失败：{err}").into())
                    });
            };
            let image = decode_resized(&bytes, format, source.edge)?;
            Ok(Arc::new(RenderImage::new(vec![Frame::new(image)])))
        }
    }
}

/// 解码 + 居中裁方 + 缩到 `edge` + RGBA→BGRA（gpui 要 BGRA）。
fn decode_resized(
    bytes: &[u8],
    format: ImageFormat,
    edge: u32,
) -> Result<RgbaImage, ImageCacheError> {
    let image = image::load_from_memory_with_format(bytes, format)
        .map_err(|err| ImageCacheError::Image(Arc::new(err)))?;
    Ok(artwork_frame(image, edge))
}

/// 裁成中心正方形并缩到 `edge`；原图不大于 `edge`（或 `edge == 0`）时保持原样。
fn artwork_frame(image: DynamicImage, edge: u32) -> RgbaImage {
    let (width, height) = (image.width(), image.height());
    let side = width.min(height);
    let image = if edge == 0 || side <= edge {
        image
    } else {
        let square = if width == height {
            image
        } else {
            image.crop_imm((width - side) / 2, (height - side) / 2, side, side)
        };
        if side > edge.saturating_mul(2) {
            square.thumbnail_exact(edge, edge)
        } else {
            square.resize_exact(edge, edge, imageops::FilterType::Triangle)
        }
    };
    let mut rgba = image.into_rgba8();
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    rgba
}

struct Cached {
    value: Result<Arc<RenderImage>, ImageCacheError>,
    bytes: usize,
    used: Instant,
}

/// 全局安装的封面缓存实体。
struct Installed(Entity<CoverImageCache>);

impl Global for Installed {}

/// 封面缓存。`cover()` 在渲染时拿不到 `cx`，所以实例通过 `Global` 暴露。
pub struct CoverImageCache {
    items: HashMap<Key, Cached>,
    bytes: usize,
    /// 见过的最大的窗口像素字节数（预算会向它看齐）。
    screen: usize,
}

impl CoverImageCache {
    /// 取全局实例（没有就创建）。启动时调一次即可。
    pub fn entity(cx: &mut App) -> Entity<Self> {
        if let Some(installed) = cx.try_global::<Installed>() {
            return installed.0.clone();
        }
        let entity = cx.new(|_cx| Self {
            items: HashMap::new(),
            bytes: 0,
            screen: 0,
        });
        cx.set_global(Installed(entity.clone()));
        entity
    }

    fn budget(&self) -> usize {
        CACHE_BYTES.max(self.screen)
    }

    /// 按「资源 + 目标边长」取图；未解码完返回 `None`（gpui 会在完成后重绘）。
    pub fn load_at(
        &mut self,
        resource: &Resource,
        edge: u32,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = (resource.clone(), edge);
        if let Some(cached) = self.items.get_mut(&key) {
            cached.used = Instant::now();
            return Some(cached.value.clone());
        }

        let viewport = window.viewport_size();
        let scale = window.scale_factor();
        let screen =
            (viewport.width.as_f32() * viewport.height.as_f32() * scale * scale) as usize * 4;
        self.screen = self.screen.max(screen);

        let source = CoverSource {
            resource: resource.clone(),
            edge,
        };
        let Some(value) = window.use_asset::<AssetLogger<CoverLoader>>(&source, cx) else {
            // 还在解码；完成后 gpui 会主动重绘，下一帧再问一次。
            return None;
        };

        let bytes = value
            .as_ref()
            .ok()
            .and_then(|image| image.as_bytes(0).map(<[u8]>::len))
            .unwrap_or(0);
        self.items.insert(
            key,
            Cached {
                value: value.clone(),
                bytes,
                used: Instant::now(),
            },
        );
        self.bytes = self.bytes.saturating_add(bytes);
        self.evict(window, cx);
        Some(value)
    }

    /// 超出预算就按 LRU 淘汰，并把纹理交还 gpui（`drop_image`）。
    ///
    /// 严格按预算淘汰：正在显示的封面每帧都会被 touch、排在队尾，自然最后被选中。
    fn evict(&mut self, window: &mut Window, cx: &mut App) {
        let budget = self.budget();
        if self.bytes <= budget && self.items.len() <= CACHE_ITEMS {
            return;
        }

        let mut candidates: Vec<(Key, Instant)> = self
            .items
            .iter()
            .filter(|(_, cached)| cached.value.is_ok())
            .map(|(key, cached)| (key.clone(), cached.used))
            .collect();
        candidates.sort_unstable_by_key(|(_, used)| *used);

        for (key, _) in candidates {
            if self.bytes <= budget && self.items.len() <= CACHE_ITEMS {
                break;
            }
            // 至少留一张（正在显示的那个）。
            if self.items.len() <= 1 {
                break;
            }
            if let Some(cached) = self.items.remove(&key) {
                self.bytes = self.bytes.saturating_sub(cached.bytes);
                if let Ok(image) = cached.value {
                    cx.drop_image(image, Some(window));
                }
            }
        }

        if log_outlet::enabled(LogLevel::Debug, LogTopic::App) {
            let held = self.items.len().to_string();
            let kib = (self.bytes / 1024).to_string();
            let budget_kib = (budget / 1024).to_string();
            log_event(
                LogLevel::Debug,
                LogTopic::App,
                "cover_cache_stats",
                &[
                    ("held", held.as_str()),
                    ("kib", kib.as_str()),
                    ("budget_kib", budget_kib.as_str()),
                ],
            );
        }
    }
}

impl ImageCache for CoverImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        // 不带尺寸的调用（兜底）：edge = 0 表示保留原图。
        self.load_at(resource, 0, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_edge_rounds_up_to_step() {
        assert_eq!(sample_edge(48.0, 1.0), 64);
        assert_eq!(sample_edge(48.0, 2.0), 128);
        assert_eq!(sample_edge(160.0, 1.0), 192);
    }

    #[test]
    fn sample_edge_caps_and_floors() {
        assert_eq!(sample_edge(0.0, 1.0), 64, "最小 1 物理像素 → 64");
        assert_eq!(sample_edge(10_000.0, 2.0), MAX_SAMPLE_EDGE);
    }

    #[test]
    fn artwork_frame_downsizes_and_swaps_channels() {
        // 200x100 红图 → edge 64：裁方后缩到 64x64，且 R/B 互换。
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            200,
            100,
            image::Rgba([10, 20, 30, 255]),
        ));
        let out = artwork_frame(image, 64);
        assert_eq!(out.width(), 64);
        assert_eq!(out.height(), 64);
        let px = out.get_pixel(0, 0).0;
        assert_eq!([px[0], px[2]], [30, 10], "R 与 B 应互换");
    }

    #[test]
    fn artwork_frame_keeps_small_images() {
        let image =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(32, 32, image::Rgba([1, 2, 3, 255])));
        let out = artwork_frame(image, 128);
        assert_eq!((out.width(), out.height()), (32, 32));
    }
}
