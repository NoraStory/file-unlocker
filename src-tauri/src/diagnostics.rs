//! 自检（诊断）模块：一键检测程序运行所需的各项前提，
//! 结果以结构化列表返回给前端展示，同时写入日志。

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct DiagItem {
    pub name: String,
    /// "ok" | "warn" | "fail"
    pub status: String,
    pub detail: String,
}

fn ok(name: &str, detail: impl Into<String>) -> DiagItem {
    DiagItem { name: name.into(), status: "ok".into(), detail: detail.into() }
}
fn warn(name: &str, detail: impl Into<String>) -> DiagItem {
    DiagItem { name: name.into(), status: "warn".into(), detail: detail.into() }
}
fn fail(name: &str, detail: impl Into<String>) -> DiagItem {
    DiagItem { name: name.into(), status: "fail".into(), detail: detail.into() }
}

/// 执行全部自检项
pub fn run_diagnostics() -> Vec<DiagItem> {
    let mut items = Vec::new();
    items.push(check_admin());
    items.push(check_debug_privilege());
    items.push(check_restart_manager());
    items.push(check_handle_scan());
    items.push(check_context_menu());
    items.push(check_log_writable());
    items.push(check_os_version());
    for item in &items {
        log::info!("[自检] {}: {} — {}", item.name, item.status, item.detail);
    }
    items
}

/// 1. 管理员权限（结束系统进程、扫描句柄的前提）
fn check_admin() -> DiagItem {
    let is_admin = unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool();
    if is_admin {
        ok("管理员权限", "已以管理员身份运行")
    } else {
        fail("管理员权限", "未提权：结束系统进程与完整句柄扫描将受限")
    }
}

/// 2. SeDebugPrivilege（枚举 SYSTEM 进程句柄的前提）
fn check_debug_privilege() -> DiagItem {
    use windows::Win32::Security::{PrivilegeCheck, SE_DEBUG_NAME, SE_PRIVILEGE_ENABLED, LUID_AND_ATTRIBUTES, PRIVILEGE_SET, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::Foundation::{LUID, CloseHandle, HANDLE};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return warn("调试特权 (SeDebug)", "无法打开进程令牌，跳过检查");
        }
        let mut luid = LUID::default();
        if windows::Win32::Security::LookupPrivilegeValueW(
            windows::core::PCWSTR::null(),
            SE_DEBUG_NAME,
            &mut luid,
        )
        .is_err()
        {
            let _ = CloseHandle(token);
            return warn("调试特权 (SeDebug)", "无法解析特权名");
        }
        let mut priv_set = PRIVILEGE_SET {
            PrivilegeCount: 1,
            Control: 0,
            Privilege: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let mut result = windows::core::BOOL::default();
        let outcome = match PrivilegeCheck(token, &mut priv_set, &mut result) {
            Ok(()) if result.as_bool() => ok("调试特权 (SeDebug)", "已启用"),
            Ok(_) => warn(
                "调试特权 (SeDebug)",
                "未启用（管理员账户默认禁用，扫描时会自动尝试开启）",
            ),
            Err(e) => warn("调试特权 (SeDebug)", format!("检查失败：{e}")),
        };
        let _ = CloseHandle(token);
        outcome
    }
}

/// 3. Restart Manager 可用性（本机可能遇到 RM 服务异常，仅降级不致命）
fn check_restart_manager() -> DiagItem {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows::Win32::System::RestartManager::{
        RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
        RM_PROCESS_INFO,
    };

    // 探针文件：create(true) 一步创建并独占打开，失败带完整 OS 错误入日志
    let probe = std::env::temp_dir().join(format!("fu_diag_{}.tmp", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .share_mode(0) // 独占，模拟真实占用
        .open(&probe);
    let Ok(_file) = file else {
        let e = file.unwrap_err();
        log::warn!("[自检] Restart Manager 探针文件创建失败: {e}");
        let _ = std::fs::remove_file(&probe);
        return warn("Restart Manager", format!("探针文件创建失败：{e}"));
    };
    let wide = crate::winutil::to_wide(&probe.to_string_lossy());

    let result = unsafe {
        let mut handle = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        if RmStartSession(&mut handle, None, PWSTR(key.as_mut_ptr())) != ERROR_SUCCESS {
            return fail("Restart Manager", "RmStartSession 失败：服务不可用");
        }
        let outcome = if RmRegisterResources(
            handle,
            Some(&[PCWSTR(wide.as_ptr())]),
            None,
            None,
        ) != ERROR_SUCCESS
        {
            fail("Restart Manager", "RmRegisterResources 失败")
        } else {
            let mut needed = 0u32;
            let mut count = 16u32;
            let mut buf = vec![RM_PROCESS_INFO::default(); count as usize];
            match RmGetList(handle, &mut needed, &mut count, Some(buf.as_mut_ptr()), std::ptr::null_mut()) {
                ERROR_SUCCESS => ok("Restart Manager", format!("正常（检出 {count} 个占用进程）")),
                ERROR_MORE_DATA => ok("Restart Manager", "正常（占用数超过探测缓冲）"),
                err => warn(
                    "Restart Manager",
                    format!("查询异常（错误码 {}），本机 RM 服务受限；句柄扫描引擎独立兜底，不影响使用", err.0),
                ),
            }
        };
        let _ = RmEndSession(handle);
        outcome
    };
    drop(_file);
    let _ = std::fs::remove_file(&probe);
    result
}

/// 4. 句柄扫描引擎（核心检测引擎，必须工作）
fn check_handle_scan() -> DiagItem {
    use std::os::windows::fs::OpenOptionsExt;
    let probe = std::env::temp_dir().join(format!("fu_diag_scan_{}.tmp", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&probe);
    let Ok(file) = file else {
        let e = file.unwrap_err();
        log::warn!("[自检] 句柄扫描探针文件创建失败: {e}");
        let _ = std::fs::remove_file(&probe);
        return fail("句柄扫描引擎", format!("探针文件创建失败：{e}"));
    };

    let pids = crate::handle_scan::scan(&probe);
    drop(file);
    let _ = std::fs::remove_file(&probe);

    if pids.contains(&std::process::id()) {
        ok("句柄扫描引擎", "正常（成功检出本进程占用的探针文件）")
    } else {
        fail(
            "句柄扫描引擎",
            "未能检出本进程的句柄——检测功能可能失效（可尝试以管理员运行）",
        )
    }
}

/// 5. 右键菜单注册状态
fn check_context_menu() -> DiagItem {
    let output = std::process::Command::new("reg")
        .args(["query", r"HKEY_CLASSES_ROOT\*\shell\FileUnlocker", "/ve"])
        .output();
    match output {
        Ok(o) if o.status.success() => ok("右键菜单", "已注册"),
        Ok(_) => warn("右键菜单", "未注册（运行 scripts\\register-context-menu.bat 可注册）"),
        Err(e) => warn("右键菜单", format!("无法查询注册表：{e}")),
    }
}

/// 6. 日志目录可写
fn check_log_writable() -> DiagItem {
    let dir = std::env::temp_dir();
    let probe = dir.join(format!("fu_log_test_{}.tmp", std::process::id()));
    match std::fs::write(&probe, b"test") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            ok("日志写入", format!("可写（日志目录：{}）", dir.display()))
        }
        Err(e) => warn("日志写入", format!("临时目录写入失败：{e}")),
    }
}

/// 7. 系统版本
fn check_os_version() -> DiagItem {
    let get = |key: &str| -> Option<String> {
        let output = std::process::Command::new("reg")
            .args(["query", r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "/v", key])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text.lines().find(|l| l.contains(key))?;
        let value = line.split("REG_SZ").last()?.trim();
        Some(value.to_string())
    };
    match (get("ProductName"), get("CurrentBuildNumber")) {
        (Some(name), Some(build)) => ok("系统版本", format!("{name} (Build {build})")),
        _ => warn("系统版本", "无法读取注册表版本信息"),
    }
}
