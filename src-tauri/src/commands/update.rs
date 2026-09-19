//! 更新命令：检查更新、下载安装。
#![forbid(unsafe_code)]

use tauri::{Emitter, State};

use crate::state::PendingUpdate;

/// 检查更新（GitHub → Gitee 依次尝试）。
/// 网络/解析失败返回 Err（原因向上抛给前端），无更新 Ok(None)，有更新 Ok(Some)
#[tauri::command]
pub async fn check_update(
    app: tauri::AppHandle,
) -> Result<Option<crate::updater::UpdateInfo>, String> {
    let current = app.package_info().version.to_string();
    log::info!("[更新] 手动检查，当前版本 {current}");
    tauri::async_runtime::spawn_blocking(move || crate::updater::check_for_update(&current))
        .await
        .map_err(|e| format!("后台任务异常：{e}"))?
}

/// 下载更新安装包（SHA256 校验后启动安装器）
#[tauri::command]
pub async fn download_update(
    window: tauri::Window<tauri::Wry>,
    asset: crate::updater::UpdateAsset,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let on_progress = |done: u64, total: u64| {
            let _ = window.emit("update-progress", (done, total));
        };
        let path = crate::updater::download_asset(&asset, &on_progress)?;
        log::info!("[更新] 安装包已就绪: {}", path.display());
        crate::updater::launch_installer(&path)
    })
    .await
    .map_err(|e| format!("后台任务异常：{e}"))?
}

/// 取走启动静默检查发现的更新信息（一次性；与 take_pending_file 同理）
#[tauri::command]
pub fn take_pending_update(state: State<'_, PendingUpdate>) -> Option<crate::updater::UpdateInfo> {
    state.0.lock().unwrap().take()
}
