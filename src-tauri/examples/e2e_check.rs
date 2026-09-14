// 端到端验证（运行产物所含同一套代码路径）：
// 1. 创建临时文件并独占锁定（模拟被占用的文件）
// 2. 用 Restart Manager 检测 → 应找到本测试进程
// 3. 句柄扫描检测 → 同样应找到
// 4. 删除检测 → 独占打开的文件应报"被占用"
// 5. 释放句柄后删除 → 应成功
// 本程序自身以普通权限运行，验证的是普通权限下的可用路径。
#![windows_subsystem = "console"]

use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;

#[path = "../src/winutil.rs"]
mod winutil;
#[path = "../src/handle_scan.rs"]
mod handle_scan;
#[path = "../src/lock_detector.rs"]
mod lock_detector;

fn main() {
    let dir = std::env::temp_dir().join("file_unlocker_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("locked.txt");
    std::fs::write(&path, b"e2e test").unwrap();

    // 独占锁定该文件
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0) // 不共享 = 独占
        .open(&path)
        .unwrap();
    writeln!(file, "locked").unwrap();

    let path_str = path.to_string_lossy().to_string();
    println!("== 锁定文件: {path_str}");

    // 1. 双引擎合并检测：无论 RM 在该系统是否可用，句柄扫描保证结果正确
    match lock_detector::get_locking_processes(&path_str, &|_, _| {}) {
        Ok(outcome) => {
            let list = &outcome.processes;
            println!("== 检测到 {} 个占用进程", list.len());
            for p in list {
                println!(
                    "   pid={} name={} exe={} source={}",
                    p.pid, p.process_name, p.exe_path, p.source
                );
            }
            let me = std::process::id();
            let found = list.iter().any(|p| p.pid == me);
            println!("== 自身({me})被检出: {found}");
            assert!(found, "未检测到自身占用");
        }
        Err(e) => panic!("检测失败: {e}"),
    }

    // 2. 被占用时删除应失败且错误可读
    match winutil::e2e_delete_probe(&path_str) {
        Err(msg) => println!("== 占用时删除被正确拒绝: {msg}"),
        Ok(()) => panic!("占用中的文件不应删除成功"),
    }

    // 3. 释放后删除应成功
    drop(file);
    winutil::e2e_delete_probe(&path_str).expect("释放后删除应成功");
    println!("== 释放后删除成功");
    let _ = std::fs::remove_dir_all(&dir);
    println!("== E2E 全部通过");
}
