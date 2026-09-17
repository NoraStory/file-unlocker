//! 针对指定真实路径的占用检测/删除防护冒烟测试。
//! 用法：cargo run --manifest-path src-tauri/Cargo.toml --example path_probe -- "C:\\Users\\story\\Desktop\\youtube-digest"

#![windows_subsystem = "console"]

use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;

#[path = "../src/handle_scan.rs"]
mod handle_scan;
#[allow(dead_code)]
#[path = "../src/lock_detector.rs"]
mod lock_detector;
#[path = "../src/winutil.rs"]
mod winutil;

use lock_detector::get_locking_processes;

fn main() {
    let target = std::env::args().nth(1).expect("请传入目标路径");
    let target_path = std::path::Path::new(&target);
    assert!(target_path.exists(), "目标不存在: {target}");

    // 1. 空路径必须报错，不得返回“无占用”
    let empty = get_locking_processes("", std::sync::Arc::new(|_, _| {}));
    assert!(empty.is_err(), "空路径应报错");

    // 2. 不存在路径必须报错
    let missing = get_locking_processes(
        r"C:\__file_unlocker_missing__.tmp",
        std::sync::Arc::new(|_, _| {}),
    );
    assert!(missing.is_err(), "不存在路径应报错");

    // 3. 真实目录基线扫描：应完成且不 panic；记录文件数与当前占用
    let outcome = get_locking_processes(
        &target,
        std::sync::Arc::new(|done, total| {
            if total > 0 && done % ((total / 10).max(1)) == 0 {
                println!("progress {done}/{total}");
            }
        }),
    )
    .unwrap_or_else(|e| panic!("目录扫描失败: {e}"));
    println!(
        "baseline files={} locks={} truncated={}",
        outcome.file_count,
        outcome.processes.len(),
        outcome.truncated
    );

    // 4. 真实目录内创建一次性锁定探针并验证双引擎目录扫描
    let probe = target_path.join("__file_unlocker_probe__.tmp");
    let _ = std::fs::remove_file(&probe);
    std::fs::write(&probe, b"probe").expect("写探针失败");
    let mut locked = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&probe)
        .expect("打开探针失败");
    writeln!(locked, "locked").unwrap();

    let outcome_locked = get_locking_processes(&target, std::sync::Arc::new(|_, _| {}))
        .unwrap_or_else(|e| panic!("锁定目录扫描失败: {e}"));
    let me = std::process::id();
    let hit = outcome_locked
        .processes
        .iter()
        .find(|p| p.pid == me)
        .expect("未检出本进程对目录内探针的占用");
    println!(
        "locked hit pid={} source={} files={}",
        hit.pid, hit.source, hit.locked_files
    );

    // 5. 单文件扫描也应命中
    let single = get_locking_processes(&probe.to_string_lossy(), std::sync::Arc::new(|_, _| {}))
        .unwrap_or_else(|e| panic!("单文件扫描失败: {e}"));
    assert!(single.processes.iter().any(|p| p.pid == me));

    // 6. 清理探针
    drop(locked);
    std::fs::remove_file(&probe).expect("清理探针失败");
    let cleaned = get_locking_processes(&target, std::sync::Arc::new(|_, _| {}))
        .unwrap_or_else(|e| panic!("清理后目录扫描失败: {e}"));
    println!(
        "cleanup files={} locks={}",
        cleaned.file_count,
        cleaned.processes.len()
    );
    println!("path-probe PASS");
}
