//! 封面主色提取：供 UI 做全局自适应主题。
//!
//! 算法对齐 sonora：先转 HSL，再按“色相”建 24 个环形桶；灰、过暗、过亮
//! 和只占小面积的颜色不参与主色。这样不会被黑底/白底或小面积文字带偏。

use std::f32::consts::TAU;
use std::path::Path;

const BINS: usize = 24;
const SAMPLES: usize = 6000;
const MIN_ALPHA: u8 = 128;
const MIN_SATURATION: f32 = 0.14;
const MIN_LIGHTNESS: f32 = 0.20;
const MAX_LIGHTNESS: f32 = 0.94;
const MIN_SHARE: f32 = 0.015;

type Rgb = (u8, u8, u8);
type Hsl = (f32, f32, f32);

#[derive(Clone, Copy, Default)]
struct Bin {
    weight: f32,
    x: f32,
    y: f32,
    saturation: f32,
    lightness: f32,
}

impl Bin {
    fn add(&mut self, color: Hsl, weight: f32) {
        let angle = color.0 * TAU;
        self.weight += weight;
        self.x += angle.cos() * weight;
        self.y += angle.sin() * weight;
        self.saturation += color.1 * weight;
        self.lightness += color.2 * weight;
    }

    fn merge(&mut self, other: &Self) {
        self.weight += other.weight;
        self.x += other.x;
        self.y += other.y;
        self.saturation += other.saturation;
        self.lightness += other.lightness;
    }

    fn colour(&self) -> Option<Hsl> {
        (self.weight > 0.0).then(|| {
            (
                self.y.atan2(self.x).rem_euclid(TAU) / TAU,
                (self.saturation / self.weight).clamp(0.0, 1.0),
                (self.lightness / self.weight).clamp(0.0, 1.0),
            )
        })
    }
}

/// 提取一张图片的主色，返回 RGB（UI 内部会再转 HSL 做主题 wash）。
pub fn dominant_color(path: &Path) -> Option<Rgb> {
    // 封面缓存统一使用 `.img` 扩展名；`ImageReader::open` 会按扩展名猜格式并失败。
    // 必须读 bytes 后按内容解码，才能支持 JPEG/PNG/WebP/GIF。
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let pixels = image.into_raw();
    let stride = (pixels.len() / 4 / SAMPLES).max(1);
    let mut bins = [Bin::default(); BINS];
    let mut sampled = 0.0_f32;

    for pixel in pixels.chunks_exact(4).step_by(stride) {
        let [red, green, blue, alpha] = [pixel[0], pixel[1], pixel[2], pixel[3]];
        if alpha < MIN_ALPHA {
            continue;
        }
        sampled += 1.0;

        let color = rgb_to_hsl(red, green, blue);
        if color.1 < MIN_SATURATION || color.2 < MIN_LIGHTNESS || color.2 > MAX_LIGHTNESS {
            continue;
        }
        let index = ((color.0 * BINS as f32) as usize).min(BINS - 1);
        let weight = color.1 * (1.0 - (color.2 - 0.5).abs());
        bins[index].add(color, weight);
    }

    let peak = (0..BINS).max_by(|&a, &b| score(&bins, a).total_cmp(&score(&bins, b)))?;
    if score(&bins, peak) < sampled * MIN_SHARE {
        return None;
    }
    let mut cluster = bins[peak];
    cluster.merge(&bins[(peak + BINS - 1) % BINS]);
    cluster.merge(&bins[(peak + 1) % BINS]);
    let (hue, saturation, lightness) = cluster.colour()?;
    Some(hsl_to_rgb(hue, saturation, lightness))
}

fn score(bins: &[Bin; BINS], index: usize) -> f32 {
    bins[index].weight
        + (bins[(index + BINS - 1) % BINS].weight + bins[(index + 1) % BINS].weight) * 0.5
}

fn rgb_to_hsl(red: u8, green: u8, blue: u8) -> Hsl {
    let (red, green, blue) = (
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
    );
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let delta = max - min;
    let lightness = (max + min) / 2.0;
    let saturation = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * lightness - 1.0).abs())
    };
    let hue = if delta == 0.0 {
        0.0
    } else if max == red {
        ((green - blue) / delta).rem_euclid(6.0) / 6.0
    } else if max == green {
        ((blue - red) / delta + 2.0) / 6.0
    } else {
        ((red - green) / delta + 4.0) / 6.0
    };
    (hue, saturation, lightness)
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> Rgb {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue = (hue * 6.0).rem_euclid(6.0);
    let x = chroma * (1.0 - ((hue % 2.0) - 1.0).abs());
    let (red, green, blue) = match hue as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let shift = lightness - chroma / 2.0;
    let channel = |value: f32| ((value + shift).clamp(0.0, 1.0) * 255.0).round() as u8;
    (channel(red), channel(green), channel(blue))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(colors: &[([u8; 3], usize)]) -> image::RgbaImage {
        let width: usize = colors.iter().map(|(_, count)| *count).sum();
        let mut pixels = Vec::with_capacity(width * 4);
        for (color, count) in colors {
            for _ in 0..*count {
                pixels.extend_from_slice(&[color[0], color[1], color[2], 255]);
            }
        }
        image::RgbaImage::from_raw(width as u32, 1, pixels).expect("image buffer")
    }

    #[test]
    fn finds_the_dominant_hue() {
        let path = std::env::temp_dir().join(format!("sodam-color-hue-{}.png", std::process::id()));
        image(&[([204, 34, 34], 900), ([32, 32, 32], 100)])
            .save(&path)
            .expect("save image");
        let color = dominant_color(&path).expect("dominant color");
        assert_eq!(color, (204, 34, 34));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ignores_greyscale_artwork() {
        let path =
            std::env::temp_dir().join(format!("sodam-color-grey-{}.png", std::process::id()));
        image(&[([18, 18, 18], 500), ([200, 200, 200], 500)])
            .save(&path)
            .expect("save image");
        assert!(dominant_color(&path).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ignores_a_small_splash_of_colour() {
        let path =
            std::env::temp_dir().join(format!("sodam-color-small-{}.png", std::process::id()));
        image(&[([120, 120, 120], 990), ([0, 180, 255], 10)])
            .save(&path)
            .expect("save image");
        assert!(dominant_color(&path).is_none());
        let _ = std::fs::remove_file(path);
    }
}
