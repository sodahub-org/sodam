//! 缓存维护的规模基准：造三档文件数，每档跑一轮完整维护，打印耗时 / 扫描数 / 删除数。
//!
//! `cargo run --release -p sodam-core --example cache_bench`
//!
//! 每档的目录构成固定（音频、边车、暂存、封面），音频上限设成这一档音频总量的一半，
//! 暂存全部超过 7 天保留期：同一份输入每次跑出来的删除数是确定的，改动前后可以直接
//! 比同一张表。目录建在系统临时目录里，跑完就删。

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::Path;
use std::time::{Duration, SystemTime};

use sodam_core::cache::{maintain_in, MaintenanceLimits, STAGING_DIR_NAME};

const TIERS: [usize; 3] = [1_000, 5_000, 20_000];
const FILE_BYTES: usize = 8;

fn main() {
    println!("文件数 | 扫描文件 | 删除 | 歌曲 | 封面 | 耗时(ms)");
    for total in TIERS {
        let dir = tempfile::tempdir().expect("建临时目录");
        let audio = dir.path().join("audio");
        let covers = dir.path().join("covers");
        let limits = build(&audio, &covers, total);
        let report = maintain_in(&audio, &covers, limits, &HashMap::new());
        println!(
            "{total:>8} {:>10} {:>8} {:>6} {:>6} {:>10}",
            report.scanned_files,
            report.removed_files,
            report.songs,
            report.cover_files,
            report.elapsed.as_millis()
        );
    }
}

/// 造一档规模的文件，返回这一档的上限。
///
/// 音频、边车、暂存、封面各占固定比例：四分之一首歌、四分之一边车、十分之一暂存、
/// 五分之一缩略图，其余是封面原图。音频与边车都超过 24 小时（可以淘汰），暂存全部
/// 超过 7 天保留期（这一轮会清掉），封面都很新（这一档不淘汰封面）。
fn build(audio: &Path, covers: &Path, total: usize) -> MaintenanceLimits {
    std::fs::create_dir_all(audio.join(STAGING_DIR_NAME)).expect("建音频暂存目录");
    std::fs::create_dir_all(covers.join("original")).expect("建封面原图目录");

    let songs = total / 4;
    let sidecars = total / 4;
    let staging = total / 10;
    let thumbs = total / 5;
    let originals = total - songs - sidecars - staging - thumbs;

    let audio_age = Duration::from_secs(48 * 60 * 60);
    let staging_age = Duration::from_secs(8 * 24 * 60 * 60);
    let fresh = Duration::from_secs(60);

    for index in 0..songs {
        let stem = format!("{index:08}");
        write(&audio.join(format!("{stem}.m4a")), audio_age, index);
        write(&audio.join(format!("{stem}.quality")), audio_age, index);
    }
    for index in 0..staging {
        write(
            &audio
                .join(STAGING_DIR_NAME)
                .join(format!("{index:08}.m4a.part")),
            staging_age,
            index,
        );
    }
    for index in 0..thumbs {
        write(&covers.join(format!("{index:016}-160.img")), fresh, index);
    }
    for index in 0..originals {
        write(
            &covers.join("original").join(format!("{index:016}.img")),
            fresh,
            index,
        );
    }

    // 音频上限取这一档音频总量的一半：淘汰只删装不下的那一半，删除数是定数。
    let audio_bytes = (songs + sidecars + staging) as u64 * FILE_BYTES as u64;
    MaintenanceLimits {
        audio_bytes: audio_bytes / 2,
        cover_bytes: 512 * 1024 * 1024,
    }
}

/// 写一个定长文件，并把 mtime 拨到指定年龄之前（同一档内按序号错开，排序是确定的）。
fn write(path: &Path, age: Duration, index: usize) {
    std::fs::write(path, [0u8; FILE_BYTES]).expect("写缓存文件");
    OpenOptions::new()
        .write(true)
        .open(path)
        .expect("打开缓存文件")
        .set_modified(SystemTime::now() - age - Duration::from_secs(index as u64 % 90))
        .expect("改文件修改时间");
}
