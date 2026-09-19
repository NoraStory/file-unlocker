//! 进程终止命令。
#![forbid(unsafe_code)]

/// 强制结束单个进程（终止前做 PID 复用防护校验）
#[tauri::command]
pub async fn kill_process(pid: u32, creation_time: String, exe_path: String) -> Result<(), String> {
    // kill 后要等待最长 3 秒确认退出，必须在后台线程
    tauri::async_runtime::spawn_blocking(move || {
        crate::lock_detector::kill_process(pid, creation_time, exe_path)
    })
    .await
    .map_err(|e| format!("后台任务异常：{e}"))?
}

/// 结束整个进程树（含全部子进程）
#[tauri::command]
pub async fn kill_process_tree(
    pid: u32,
    creation_time: String,
    exe_path: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::lock_detector::kill_process_tree(pid, creation_time, exe_path)
    })
    .await
    .map_err(|e| format!("后台任务异常：{e}"))?
}
