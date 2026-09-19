//! 扫描命令：文件占用检测。
#![forbid(unsafe_code)]

use std::sync::Arc;

use tauri::Emitter;

/// 查询占用指定文件的进程列表（双引擎合并；目录模式含进度事件）。
#[tauri::command]
pub async fn get_locking_processes(
    window: tauri::Window<tauri::Wry>,
    file_path: String,
) -> Result<crate::lock_detector::ScanOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // 目录模式可能扫描数千文件、耗时数秒，向前端发进度事件；
        // 节流：按已完成的 2% 或每 20 个发一次
        let window = Arc::new(window);
        let on_progress: crate::handle_scan::ProgressCallback =
            Arc::new(move |done: usize, total: usize| {
                let _ = window.emit("scan-progress", (done, total));
            });
        crate::lock_detector::get_locking_processes(&file_path, on_progress)
    })
    .await
    .map_err(|e| format!("后台任务异常：{e}"))?
}

/// 判断目标是否为目录（前端据此切换"目录模式"UI，隐藏文件删除按钮）
#[tauri::command]
pub fn is_directory(path: String) -> bool {
    std::path::Path::new(&path).is_dir()
}
