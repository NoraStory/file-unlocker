use std::sync::Mutex;

use lock_detector::ProcessInfo;
use tauri::{Emitter, Manager, State};

mod file_actions;
mod handle_scan;
mod lock_detector;
mod winutil;

/// 启动参数中携带、等待前端取走的文件路径。
///
/// 存在的意义：单实例回调或提权启动时，主窗口可能尚未创建，
/// 直接 emit 事件会丢失，所以先落地到 state，由前端挂载后主动取走。
struct PendingFile(Mutex<Option<String>>);

/// 从命令行参数中找出第一个真实存在的文件/目录路径
fn extract_path_from_args(args: &[String]) -> Option<String> {
    args.iter()
        .skip(1) // 跳过 argv[0]（程序自身路径）
        .find(|a| std::path::Path::new(a).exists())
        .cloned()
}

/// 推送新文件路径给主窗口（焦点 + 事件；窗口未就绪时仅落地 pending state）
fn push_file(app: &tauri::AppHandle, path: String) {
    let state = app.state::<PendingFile>();
    state.0.lock().unwrap().replace(path.clone());
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_focus();
        let _ = window.emit("new-file", path);
    }
}

#[tauri::command]
fn get_locking_processes(file_path: String) -> Result<Vec<ProcessInfo>, String> {
    lock_detector::get_locking_processes(&file_path)
}

#[tauri::command]
fn kill_process(pid: u32) -> Result<(), String> {
    lock_detector::kill_process(pid)
}

/// 结束整个进程树（含全部子进程）
#[tauri::command]
fn kill_process_tree(pid: u32) -> Result<(), String> {
    lock_detector::kill_process_tree(pid)
}

/// 立即删除文件（需要文件未被占用；需确认 delete=true 防误触）
#[tauri::command]
fn delete_file(file_path: String, delete: bool) -> Result<(), String> {
    if !delete {
        return Err("缺少确认参数".into());
    }
    file_actions::delete_file(&file_path)
}

/// 计划下次重启时删除文件（对被锁定的文件有效；需确认 delete=true 防误触）
#[tauri::command]
fn delete_file_on_reboot(file_path: String, delete: bool) -> Result<(), String> {
    if !delete {
        return Err("缺少确认参数".into());
    }
    file_actions::delete_on_reboot(&file_path)
}

#[tauri::command]
fn take_pending_file(state: State<'_, PendingFile>) -> Option<String> {
    state.0.lock().unwrap().take()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // 单实例：资源管理器里连续右键多个文件时，不开新窗口，
        // 而是把新路径转发给已存在的窗口
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(path) = extract_path_from_args(&argv) {
                push_file(app, path);
            } else if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .manage(PendingFile(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            get_locking_processes,
            kill_process,
            kill_process_tree,
            delete_file,
            delete_file_on_reboot,
            take_pending_file
        ])
        .setup(|app| {
            // 首次启动即带路径参数（右键菜单 "%1"）时，交给前端
            let args: Vec<String> = std::env::args().collect();
            if let Some(path) = extract_path_from_args(&args) {
                app.state::<PendingFile>()
                    .0
                    .lock()
                    .unwrap()
                    .replace(path);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
