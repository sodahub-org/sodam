//! 有预算下限、**按显示尺寸解码**的封面缓存（照 sonora 的 `ArtworkCache` 移植）。
//!
//! gpui 的默认图片加载是「原分辨率解码」：歌词页原图 960×960 解出来就是 ~3.7MB，
//! 几十张就吃掉几百 MB 显存/堆。sonora 的做法是让缓存 key 带上**目标边长 `edge`**，
//! 解码后先 `resize_exact` 到实际显示尺寸再交给 gpui，于是 48px 的缩略图只占 64×64。
//!
//! 预算 64MiB 起步，并随一个满屏像素增长；再加 512 条的条数上限。
//! （sonora 实测 footprint 67MB / IOAccelerator 8.4MB。）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use gpui::{
    App, AppContext, Asset, AssetLogger, Entity, Global, ImageCache, ImageCacheError, RenderImage,
    Resource, Window,
};
use image::{imageops, DynamicImage, Frame, ImageFormat, RgbaImage};
use sodam_core::log_outlet::{self, log_event, LogLevel, LogTopic};

/// 缓存预算下限（超出后淘汰）；实际预算 = `max(这个, 一个满屏像素)`。
const CACHE_BYTES: usize = 64 * 1024 * 1024;
/// 条数上限。
const CACHE_ITEMS: usize = 512;
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
    /// 内存缓存 LRU 用的单调时钟；系统挂起期间可能不走。
    used: Instant,
    /// 磁盘淘汰用的墙钟显示时间。与 `used` 分开存，挂起恢复后不会把旧记录“推新”。
    shown_at: SystemTime,
}

/// 全局安装的封面缓存实体。
struct Installed(Entity<CoverImageCache>);

impl Global for Installed {}

/// 封面缓存。`cover()` 在渲染时拿不到 `cx`，所以实例通过 `Global` 暴露。
pub struct CoverImageCache {
    items: HashMap<Key, Cached>,
    /// 这台机器上查询过的全部 (资源, 边长) 组合（PR #348 三轮审查）：
    /// gpui 的 asset 缓存按这个 key 记结果（含失败），而失败结果可能还没进
    /// `items` 就被下载成功的回调撞上——失效时要把**查询过**的 key 全部
    /// 从 gpui 缓存里清掉，不能只看 items 里已落账的那些。
    touched: Vec<Key>,
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
            touched: Vec::new(),
            bytes: 0,
            screen: 0,
        });
        cx.set_global(Installed(entity.clone()));
        entity
    }

    /// 这个路径的封面重新下好了：清掉它之前留下的**失败**缓存条目，并把
    /// gpui 侧的对应 asset 一并移除（PR #348 二轮审查）。不清的话读错误会
    /// 永久滞留——`load_at` 缓存错误、`evict` 排除失败条目、gpui 的 asset
    /// 缓存也只认第一次结果，重新下好的封面在同一尺寸上永远拿不回来。
    /// 失败条目没有纹理要还（`drop_image` 分支只是防御），不需要 window。
    pub fn invalidate_failed(&mut self, path: &Path, cx: &mut App) {
        let resource = Resource::from(path.to_path_buf());
        // gpui 侧按「查询过」的全部边长清 asset（PR #348 三轮审查）：失败结果
        // 可能还在解码任务里没落进 items，只清 items 会漏——在飞解码回来会
        // 把旧失败重新发布到 asset 缓存，路径已被替换也救不回来。
        let edges: Vec<u32> = self
            .touched
            .iter()
            .filter(|(res, _)| *res == resource)
            .map(|(_, edge)| *edge)
            .collect();
        for edge in &edges {
            cx.remove_asset::<AssetLogger<CoverLoader>>(&CoverSource {
                resource: resource.clone(),
                edge: *edge,
            });
        }
        // 本缓存里该资源的条目全部作废（含成功的：路径内容已被替换）。
        let stale: Vec<Key> = self
            .items
            .keys()
            .filter(|(res, _)| *res == resource)
            .cloned()
            .collect();
        for key in stale {
            if let Some(cached) = self.items.remove(&key) {
                self.bytes = self.bytes.saturating_sub(cached.bytes);
                if let Ok(image) = cached.value {
                    cx.drop_image(image, None);
                }
            }
        }
        // 已失效的查询记录清掉，touched 不无限增长；之后同边长的新查询会
        // 重新入账。
        self.touched.retain(|(res, _)| *res != resource);
    }

    fn budget(&self) -> usize {
        CACHE_BYTES.max(self.screen)
    }

    /// 最近显示时间快照：`路径 → 最后一次布局/绘制时见到的墙钟时间`。
    ///
    /// `load_at` 在每次布局/绘制命中时记录墙钟时间；这里不做换算，直接导出，避免
    /// `Instant` 在系统挂起期间停走导致旧记录被“推新”。只在渲染线程只读采样，不加锁、
    /// 不碰文件系统。同一路径可能因目标边长不同而有多条缓存，取其中最近的一次；解码失败
    /// 的条目没画出来，不算显示。快照只覆盖仍在内存缓存里的封面，其余封面会退回 mtime。
    pub fn last_shown_times(&self) -> HashMap<PathBuf, SystemTime> {
        let mut latest: HashMap<&Path, SystemTime> = HashMap::new();
        for ((resource, _edge), cached) in &self.items {
            if cached.value.is_err() {
                continue;
            }
            let Resource::Path(path) = resource else {
                continue;
            };
            let path = path.as_ref();
            let seen = latest.entry(path).or_insert(cached.shown_at);
            *seen = (*seen).max(cached.shown_at);
        }
        latest
            .into_iter()
            .map(|(path, shown)| (path.to_path_buf(), shown))
            .collect()
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
        if !self
            .touched
            .iter()
            .any(|seen| seen.0 == key.0 && seen.1 == key.1)
        {
            self.touched.push(key.clone());
        }
        if let Some(cached) = self.items.get_mut(&key) {
            cached.used = Instant::now();
            cached.shown_at = SystemTime::now();
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
                shown_at: SystemTime::now(),
            },
        );
        self.bytes = self.bytes.saturating_add(bytes);
        self.evict(window, cx);
        Some(value)
    }

    /// 预算和条数都没超就不用淘汰；条数按含新插入的这一张算。
    fn needs_eviction(bytes: usize, items: usize, budget: usize) -> bool {
        bytes > budget || items > CACHE_ITEMS
    }

    /// 超出预算就按 LRU 淘汰，并把纹理交还 gpui（`drop_image`）。
    ///
    /// 严格按预算淘汰：正在显示的封面每帧都会被 touch、排在队尾，自然最后被选中。
    fn evict(&mut self, window: &mut Window, cx: &mut App) {
        let budget = self.budget();
        if !Self::needs_eviction(self.bytes, self.items.len(), budget) {
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
            if !Self::needs_eviction(self.bytes, self.items.len(), budget) {
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

/// 在渲染线程采样一次「最近显示时间」，供后台封面淘汰使用。
///
/// 不要在 `CoverImageCache::update` 内部调用：那里已经可变借用了缓存实体。
pub fn shown_snapshot(cx: &mut App) -> HashMap<PathBuf, SystemTime> {
    CoverImageCache::entity(cx).read(cx).last_shown_times()
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

    fn empty_cache(screen: usize) -> CoverImageCache {
        CoverImageCache {
            items: HashMap::new(),
            touched: Vec::new(),
            bytes: 0,
            screen,
        }
    }

    #[test]
    fn budget_floor_is_64_mib() {
        assert_eq!(empty_cache(0).budget(), 64 * 1024 * 1024);
    }

    #[test]
    fn budget_still_follows_larger_screen() {
        let full_screen = 100 * 1024 * 1024;
        assert_eq!(empty_cache(full_screen).budget(), full_screen);
    }

    #[test]
    fn item_limit_needs_eviction_at_513() {
        // 这里只关心条数，预算直接用生产值；64MiB 本身由 budget_floor 测试锁住。
        let budget = CACHE_BYTES;
        // 512 放得下，513 就超；没到条数但超预算也要淘汰。
        assert!(
            !CoverImageCache::needs_eviction(0, 512, budget),
            "512 条不淘汰"
        );
        assert!(
            CoverImageCache::needs_eviction(0, 513, budget),
            "第 513 条触发淘汰"
        );
        assert!(
            CoverImageCache::needs_eviction(budget + 1, 1, budget),
            "没到条数但超预算也要淘汰"
        );
    }

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

    fn decoded_1x1() -> Result<Arc<RenderImage>, ImageCacheError> {
        Ok(Arc::new(RenderImage::new(vec![Frame::new(
            RgbaImage::new(1, 1),
        )])))
    }

    /// 同一路径的不同解码尺寸取最近一次显示；解码失败不算显示。
    #[test]
    fn last_shown_times_reports_latest_display_per_path() {
        use std::time::Duration;

        let mut cache = empty_cache(0);
        let path = PathBuf::from("/cache/covers/0000-160.img");
        let failed = PathBuf::from("/cache/covers/0001-160.img");
        let now = Instant::now();
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let entry = |value, used, shown_at| Cached {
            value,
            bytes: 0,
            used,
            shown_at,
        };

        cache.items.insert(
            (Resource::from(path.clone()), 64),
            entry(
                decoded_1x1(),
                now - Duration::from_secs(30),
                wall - Duration::from_secs(30),
            ),
        );
        cache.items.insert(
            (Resource::from(path.clone()), 160),
            entry(
                decoded_1x1(),
                now - Duration::from_secs(5),
                wall - Duration::from_secs(5),
            ),
        );
        cache.items.insert(
            (Resource::from(failed.clone()), 64),
            entry(
                Err(ImageCacheError::Io(Arc::new(std::io::Error::other(
                    "decode failed",
                )))),
                now,
                wall,
            ),
        );

        let shown = cache.last_shown_times();
        assert_eq!(
            shown.get(&path),
            Some(&(wall - Duration::from_secs(5))),
            "同一路径取最近一次显示"
        );
        assert!(!shown.contains_key(&failed), "没画出来的不能当成显示");
    }
}
