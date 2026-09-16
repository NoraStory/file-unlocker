use std::sync::{Arc, Mutex};

use lock_detector::ScanOutcome;
use tauri::{Emitter, Manager, State};

mod diagnostics;
mod file_actions;
mod handle_scan;
mod lock_detector;
mod probe_utils;
mod updater;
mod winutil;

/// 启动参数中携带、等待前端取走的文件路径。
///
/// 存在的意义：单实例回调或提权启动时，主窗口可能尚未创建，
/// 直接 emit 事件会丢失，所以先落地到 state，由前端挂载后主动取走。
struct PendingFile(Mutex<Option<String>>);

/// 启动静默检查发现的更新信息，同样落地等前端取走——
/// WebView 未加载完成时 emit 的 "update-available" 事件会丢失。
struct PendingUpdate(Mutex<Option<updater::UpdateInfo>>);

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
        // 事件送达后清掉 pending：窗口已存在时 pending 多余，残留到下次
        // mount 会被 take_pending_file 重复取走，旧文件再次被推给前端。
        // 仅当 pending 仍是本次路径时才清，避免误清并发推入的更新路径
        if window.emit("new-file", path.clone()).is_ok() {
            let mut pending = state.0.lock().unwrap();
            if pending.as_deref() == Some(path.as_str()) {
                pending.take();
            }
        }
    }
}

/// 重活类命令一律 async + spawn_blocking：
/// Tauri 的同步 command 在 WebView2 UI 线程内联执行，耗时超过约 200ms
/// 就会饿死事件循环，Windows 判定"程序未响应"。
#[tauri::command]
async fn get_locking_processes(
    window: tauri::Window<tauri::Wry>,
    file_path: String,
) -> Result<ScanOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // 目录模式可能扫描数千文件、耗时数秒，向前端发进度事件；
        // 节流：按已完成的 2% 或每 20 个发一次
        let window = Arc::new(window);
        let on_progress: crate::handle_scan::ProgressCallback =
            Arc::new(move |done: usize, total: usize| {
                let _ = window.emit("scan-progress", (done, total));
            });
        lock_detector::get_locking_processes(&file_path, on_progress)
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
async fn delete_file(file_path: String, delete: bool) -> Result<(), String> {
    if !delete {
        return Err("缺少确认参数".into());
    }
    // 删除大文件或网络盘文件可能耗时，遵守"重活一律 spawn_blocking"的约定
    tauri::async_runtime::spawn_blocking(move || file_actions::delete_file(&file_path))
        .await
        .map_err(|e| format!("后台任务异常：{e}"))?
}

/// 计划下次重启时删除文件（delete=true 双重确认，防止误触）
#[tauri::command]
async fn delete_file_on_reboot(file_path: String, delete: bool) -> Result<(), String> {
    if !delete {
        return Err("缺少确认参数".into());
    }
    tauri::async_runtime::spawn_blocking(move || file_actions::delete_on_reboot(&file_path))
        .await
        .map_err(|e| format!("后台任务异常：{e}"))?
}

/// 判断目标是否为目录（前端据此切换"目录模式"UI，隐藏文件删除按钮）
#[tauri::command]
fn is_directory(path: String) -> bool {
    std::path::Path::new(&path).is_dir()
}

#[tauri::command]
fn take_pending_file(state: State<'_, PendingFile>) -> Option<String> {
    state.0.lock().unwrap().take()
}

/// 取走启动静默检查发现的更新信息（一次性；与 take_pending_file 同理）
#[tauri::command]
fn take_pending_update(state: State<'_, PendingUpdate>) -> Option<updater::UpdateInfo> {
    state.0.lock().unwrap().take()
}

/// 当前版本号
#[tauri::command]
fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

/// 日志目录路径
#[tauri::command]
fn get_log_dir(app: tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;
    app.path()
        .app_log_dir()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("获取日志目录失败：{e}"))
}

/// 打开日志目录（资源管理器）
#[tauri::command]
fn open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let dir = app
        .path()
        .app_log_dir()
        .map_err(|e| format!("获取日志目录失败：{e}"))?;
    log::info!("[日志] 打开日志目录: {}", dir.display());

    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = crate::winutil::to_wide(&dir.to_string_lossy());
    let op = crate::winutil::to_wide("open"); // 先绑定变量，避免临时值悬垂
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // 打开失败时不弹 Shell 重试对话框，避免命令线程被 UI 挂起卡死
        fMask: SEE_MASK_FLAG_NO_UI,
        hwnd: windows::Win32::Foundation::HWND::default(),
        lpVerb: PCWSTR(op.as_ptr()),
        lpFile: PCWSTR(wide.as_ptr()),
        lpParameters: PCWSTR::null(),
        lpDirectory: PCWSTR::null(),
        nShow: SW_SHOWNORMAL.0,
        hInstApp: windows::Win32::Foundation::HINSTANCE::default(),
        lpIDList: std::ptr::null_mut(),
        lpClass: PCWSTR::null(),
        hkeyClass: windows::Win32::System::Registry::HKEY::default(),
        dwHotKey: 0,
        Anonymous: windows::Win32::UI::Shell::SHELLEXECUTEINFOW_0::default(),
        hProcess: windows::Win32::Foundation::HANDLE::default(),
    };
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|e| format!("打开日志目录失败：{}", crate::winutil::win32_err(&e)))?;
    if info.hInstApp.is_invalid() || info.hInstApp.0 as isize <= 32 {
        return Err(format!(
            "打开日志目录失败（错误码 {}）",
            info.hInstApp.0 as isize
        ));
    }
    Ok(())
}

/// 导出日志：把日志目录全部文件复制到用户选择的目录下
#[tauri::command]
async fn export_logs(app: tauri::AppHandle, dest_dir: String) -> Result<String, String> {
    // 日志可能多达 10×1MB，文件复制移出 UI 线程
    tauri::async_runtime::spawn_blocking(move || export_logs_impl(&app, &dest_dir))
        .await
        .map_err(|e| format!("后台任务异常：{e}"))?
}

fn export_logs_impl(app: &tauri::AppHandle, dest_dir: &str) -> Result<String, String> {
    use tauri::Manager;
    let log_dir = app
        .path()
        .app_log_dir()
        .map_err(|e| format!("获取日志目录失败：{e}"))?;
    let dest = std::path::Path::new(dest_dir);
    if !dest.is_dir() {
        return Err("导出目标不是目录".into());
    }

    let stamp = chrono_lite_stamp();
    let out_dir = dest.join(format!("FileUnlocker-logs-{stamp}"));
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("创建导出目录失败：{e}"))?;

    let mut copied = 0usize;
    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            let from = entry.path();
            if !from.is_file() {
                continue;
            }
            let name = entry.file_name();
            let to = out_dir.join(&name);
            if std::fs::copy(&from, &to).is_ok() {
                copied += 1;
            }
        }
    }
    if copied == 0 {
        let _ = std::fs::remove_dir_all(&out_dir);
        return Err("日志目录中没有可导出的文件（程序可能刚启动尚无日志）".into());
    }
    log::info!("[日志] 已导出 {copied} 个日志文件到 {}", out_dir.display());
    Ok(format!(
        "已导出 {copied} 个日志文件到：{}",
        out_dir.display()
    ))
}

/// 可读时间戳（本地时间 YYYYMMDD-HHMMSS）
fn chrono_lite_stamp() -> String {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st: SYSTEMTIME = unsafe { GetLocalTime() };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// 自检：检测权限、检测引擎、右键菜单、日志等必要前提
#[tauri::command]
async fn run_diagnostics(app: tauri::AppHandle) -> Vec<diagnostics::DiagItem> {
    log::info!("[自检] 开始");
    tauri::async_runtime::spawn_blocking(move || diagnostics::run_diagnostics(&app))
        .await
        .unwrap_or_default()
}

/// 检查更新（GitHub → Gitee 依次尝试）。
/// 网络/解析失败返回 Err（原因向上抛给前端），无更新 Ok(None)，有更新 Ok(Some)
#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<Option<updater::UpdateInfo>, String> {
    let current = app.package_info().version.to_string();
    log::info!("[更新] 手动检查，当前版本 {current}");
    tauri::async_runtime::spawn_blocking(move || updater::check_for_update(&current))
        .await
        .map_err(|e| format!("后台任务异常：{e}"))?
}

/// 下载更新安装包（SHA256 校验后启动安装器）
#[tauri::command]
async fn download_update(
    window: tauri::Window<tauri::Wry>,
    asset: updater::UpdateAsset,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let on_progress = |done: u64, total: u64| {
            let _ = window.emit("update-progress", (done, total));
        };
        let path = updater::download_asset(&asset, &on_progress)?;
        log::info!("[更新] 安装包已就绪: {}", path.display());
        updater::launch_installer(&path)
    })
    .await
    .map_err(|e| format!("后台任务异常：{e}"))?
}

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
            get_locking_processes,
            kill_process,
            kill_process_tree,
            delete_file,
            delete_file_on_reboot,
            is_directory,
            take_pending_file,
            take_pending_update,
            app_version,
            get_log_dir,
            open_log_dir,
            export_logs,
            run_diagnostics,
            check_update,
            download_update
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
                app.state::<PendingFile>().0.lock().unwrap().replace(path);
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
