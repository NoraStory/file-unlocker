//! 库入口：模块装配与 Tauri 应用启动。
//!
//! 分层结构（Phase 1 热点切割后）：
//! - `commands/`：Tauri IPC 命令层（参数转发 + spawn_blocking 调度）
//! - `core/`：业务编排（扫描合并、进程终止）
//! - `engines/`：检测引擎（Restart Manager；句柄扫描见 `handle_scan`）
//! - `state.rs`：共享状态（pending 文件/更新）
//! - 平台 FFI 细节收敛在 `winutil`/`handle_scan`/`probe_utils`

mod commands;
mod core;
mod diagnostics;
mod engines;
mod error;
mod file_actions;
mod handle_scan;
mod lock_detector;
mod probe_utils;
mod state;
mod updater;
mod winutil;

use std::sync::Mutex;

use tauri::{Emitter, Manager};

use state::{extract_path_from_args, push_file, set_pending_file, PendingFile, PendingUpdate};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // panic 兜底：写日志（logger 未就绪时至少进 stderr，便于崩溃排查）
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("PANIC: {info}");
        log::error!("{msg}");
        eprintln!("{msg}");
    }));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // 文件日志：轮转保留 10 个 × 1MB，写入系统日志目录
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(1_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepAll)
                .build(),
        )
        // 单实例：资源管理器里连续右键多个文件时，不开新窗口，
        // 而是把新路径转发给已存在的窗口
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            log::info!("[单实例] 二次启动参数: {argv:?}");
            if let Some(path) = extract_path_from_args(&argv) {
                push_file(app, path);
            } else if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .manage(PendingFile(Mutex::new(None)))
        .manage(PendingUpdate(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            commands::scan::get_locking_processes,
            commands::process::kill_process,
            commands::process::kill_process_tree,
            commands::file::delete_file,
            commands::file::delete_file_on_reboot,
            commands::scan::is_directory,
            commands::system::take_pending_file,
            commands::update::take_pending_update,
            commands::system::app_version,
            commands::system::get_log_dir,
            commands::system::open_log_dir,
            commands::system::export_logs,
            commands::system::run_diagnostics,
            commands::update::check_update,
            commands::update::download_update
        ])
        .setup(|app| {
            log::info!(
                "[启动] FileUnlocker v{} (win {})",
                app.package_info().version,
                std::env::consts::OS
            );
            // 首次启动即带路径参数（右键菜单 "%1"）时，交给前端
            let args: Vec<String> = std::env::args().collect();
            if let Some(path) = extract_path_from_args(&args) {
                log::info!("[启动] 携带文件参数: {path}");
                set_pending_file(&app.state::<PendingFile>(), path);
            }
            // 启动后静默检查更新（非阻塞；HTTP 是阻塞调用，须进 spawn_blocking，
            // 直接放在 async task 里会占住运行时的工作线程）
            let handle = app.handle().clone();
            let current = app.package_info().version.to_string();
            tauri::async_runtime::spawn(async move {
                let result = tauri::async_runtime::spawn_blocking(move || {
                    updater::check_for_update(&current)
                })
                .await
                .unwrap_or_else(|e| Err(format!("后台任务异常：{e}")));
                match result {
                    Ok(Some(info)) => {
                        log::info!("[更新] 发现新版本 {}", info.version);
                        // 先落地再 emit：窗口未就绪时事件会丢，前端挂载后主动取走
                        handle
                            .state::<PendingUpdate>()
                            .0
                            .lock()
                            .unwrap()
                            .replace(info.clone());
                        let _ = handle.emit("update-available", info);
                    }
                    Ok(None) => {}
                    Err(e) => log::warn!("[更新] 静默检查失败: {e}"),
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    #[test]
    fn register_script_forwards_absolute_argument_on_uac() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("scripts")
            .join("register-context-menu.bat");
        let bytes = std::fs::read(path).expect("read register script");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("-ArgumentList @('%~f1')"));
        assert!(text.contains("set \"EXE=%~f1\""));
    }
}
