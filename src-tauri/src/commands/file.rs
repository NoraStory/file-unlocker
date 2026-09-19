//! 文件操作命令：删除 / 重启后删除。
#![forbid(unsafe_code)]

use crate::error::{AppError, ErrorCode};

/// 立即删除文件（delete=true 双重确认，防止误触；路径/系统目录校验在后端执行）
/// 错误为结构化 AppError（ADR-002）：序列化为 {code, message}
#[tauri::command]
pub async fn delete_file(file_path: String, delete: bool) -> Result<(), AppError> {
    if !delete {
        return Err(AppError::new(ErrorCode::EPathInvalid, "缺少确认参数"));
    }
    // 删除大文件或网络盘文件可能耗时，遵守"重活一律 spawn_blocking"的约定
    tauri::async_runtime::spawn_blocking(move || crate::file_actions::delete_file(&file_path))
        .await
        .map_err(|e| AppError::new(ErrorCode::EInternal, format!("后台任务异常：{e}")))?
}

/// 计划下次重启时删除文件（delete=true 双重确认，防止误触）
#[tauri::command]
pub async fn delete_file_on_reboot(file_path: String, delete: bool) -> Result<(), AppError> {
    if !delete {
        return Err(AppError::new(ErrorCode::EPathInvalid, "缺少确认参数"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        crate::file_actions::delete_on_reboot(&file_path)
    })
    .await
    .map_err(|e| AppError::new(ErrorCode::EInternal, format!("后台任务异常：{e}")))?
}
