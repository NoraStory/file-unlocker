//! 系统命令：pending 状态、版本、日志管理、自检。
//!
//! 本文件包含 ShellExecuteW / GetLocalTime 两处 FFI 调用，
//! 不适用 #![forbid(unsafe_code)]（其余 commands 文件均为纯安全代码）。

use tauri::State;

use crate::state::{take_pending_file_impl, PendingFile};

#[tauri::command]
pub fn take_pending_file(state: State<'_, PendingFile>) -> Option<String> {
    take_pending_file_impl(&state)
}

/// 当前版本号
#[tauri::command]
pub fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

/// 日志目录路径
#[tauri::command]
pub fn get_log_dir(app: tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;
    app.path()
        .app_log_dir()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("获取日志目录失败：{e}"))
}

/// 打开日志目录（资源管理器）
#[tauri::command]
pub fn open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
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
pub async fn export_logs(app: tauri::AppHandle, dest_dir: String) -> Result<String, String> {
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
pub async fn run_diagnostics(app: tauri::AppHandle) -> Vec<crate::diagnostics::DiagItem> {
    log::info!("[自检] 开始");
    tauri::async_runtime::spawn_blocking(move || crate::diagnostics::run_diagnostics(&app))
        .await
        .unwrap_or_default()
}
