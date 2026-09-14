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

/// 重活类命令一律 async + spawn_blocking：
/// Tauri 的同步 command 在 WebView2 UI 线程内联执行，耗时超过约 200ms
/// 就会饿死事件循环，Windows 判定"程序未响应"。
#[tauri::command]
async fn get_locking_processes(
    window: tauri::Window<tauri::Wry>,
    file_path: String,
) -> Result<Vec<ProcessInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // 目录模式可能扫描数千文件、耗时数秒，向前端发进度事件；
        // 节流：按已完成的 2% 或每 20 个发一次
        let last = std::sync::atomic::AtomicUsize::new(0);
        let on_progress = |done: usize, total: usize| {
            let step = (total / 50).max(1);
            let now = done / step;
            if now != last.swap(now, std::sync::atomic::Ordering::Relaxed) || done == total {
                let _ = window.emit("scan-progress", (done, total));
            }
        };
        lock_detector::get_locking_processes(&file_path, &on_progress)
    })
    .await
    .map_err(|e| format!("后台任务异常：{e}"))?
}

#[tauri::command]
async fn kill_process(pid: u32) -> Result<(), String> {
    // kill 后要等待最长 3 秒确认退出，必须在后台线程
    tauri::async_runtime::spawn_blocking(move || lock_detector::kill_process(pid))
        .await
        .map_err(|e| format!("后台任务异常：{e}"))?
}

/// 结束整个进程树（含全部子进程）
#[tauri::command]
async fn kill_process_tree(pid: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || lock_detector::kill_process_tree(pid))
        .await
        .map_err(|e| format!("后台任务异常：{e}"))?
}

/// 立即删除文件（delete=true 双重确认，防止误触；路径/系统目录校验在后端执行）
#[tauri::command]
fn delete_file(file_path: String, delete: bool) -> Result<(), String> {
    if !delete {
        return Err("缺少确认参数".into());
    }
    file_actions::delete_file(&file_path)
}

/// 计划下次重启时删除文件（delete=true 双重确认，防止误触）
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
