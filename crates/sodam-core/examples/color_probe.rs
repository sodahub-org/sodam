//! 检查封面主色提取结果（开发排查用）。
//! 用法：`cargo run -p sodam-core --example color_probe -- <cover-file>...`

fn main() {
    for path in std::env::args().skip(1) {
        let path = std::path::PathBuf::from(path);
        match sodam_core::color::dominant_color(&path) {
            Some((r, g, b)) => println!("{} -> #{r:02x}{g:02x}{b:02x}", path.display()),
            None => println!("{} -> None", path.display()),
        }
    }
}
