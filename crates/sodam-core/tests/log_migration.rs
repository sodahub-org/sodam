//! #61 的回归：发布版里所有诊断都必须从同一个日志出口出去。
//!
//! 这两种退化都不靠人眼守：
//! * 又有人在业务代码里 `eprintln!`——发布版是 GUI 子系统程序，这一行等于扔掉；
//! * 又有人读 `SODAM_*_LOG` 去开关某段输出——语义应该是「调级别」，开关只在
//!   `log_outlet` 里登记一次。
//!
//! 示例程序（`examples/`）与测试代码（`tests/`）不受约束：前者是给人手动跑的探针，
//! 后者不会进发布版。

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if matches!(name, "examples" | "tests" | "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// 集成测试的 manifest 在 `crates/sodam-core`，上一级就是整个 `crates/`。
fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/sodam-core 之上应当是 crates 目录")
        .to_path_buf()
}

fn scanned_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    rust_files(&crates_dir(), &mut files);
    assert!(!files.is_empty(), "没扫到任何源文件，路径解析错了");
    let outlet = crates_dir().join("sodam-core/src/log_outlet.rs");
    assert!(files.contains(&outlet), "扫描范围漏掉了日志出口本身");
    files
}

#[test]
fn no_direct_terminal_output_in_publishable_code() {
    let mut offenders = Vec::new();
    for file in scanned_files() {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let hits: Vec<&str> = ["eprintln!", "println!", "dbg!"]
            .into_iter()
            .filter(|needle| text.contains(needle))
            .collect();
        if !hits.is_empty() {
            offenders.push(format!("{}: {}", file.display(), hits.join(", ")));
        }
    }

    assert!(
        offenders.is_empty(),
        "诊断输出必须走 sodam_core::log_outlet，不能直接写终端：\n{}",
        offenders.join("\n")
    );
}

#[test]
fn legacy_log_switches_are_read_only_by_the_outlet() {
    let outlet = crates_dir().join("sodam-core/src/log_outlet.rs");
    let mut offenders = Vec::new();

    for file in scanned_files() {
        if file == outlet {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (name, _) in sodam_core::log_outlet::TOPIC_VERBOSITY_ENV {
            // 只认带引号的写法：注释里提到变量名（`SODAM_FRAME_LOG=1`）不算读取。
            if text.contains(&format!("\"{name}\"")) {
                offenders.push(format!("{}: {name}", file.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "环境变量只用来调该话题的详细级别，必须在 log_outlet 里统一处理：\n{}",
        offenders.join("\n")
    );
}
