//! 跨模块共享的 Win32 小工具：宽字符转换、进程路径查询、
//! 版本信息（文件描述）读取、SeDebugPrivilege 启用。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// UTF-8 → 以 NUL 结尾的 UTF-16
pub fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 截断固定长度的 UTF-16 缓冲区到首个 NUL 并转为 String
pub fn utf16_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 查询进程可执行文件的完整路径（需要 PROCESS_QUERY_LIMITED_INFORMATION）
pub fn process_image_full(pid: u32) -> Option<String> {
    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            HANDLE(handle.0),
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    unsafe {
        let _ = CloseHandle(HANDLE(handle.0));
    }
    result.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// 读取 exe 的版本信息中的 FileDescription（如 "Microsoft Word"）。
/// 参照 PowerToys File Locksmith 的做法，让用户看到的是应用名而非进程名。
pub fn file_description(exe_path: &str) -> Option<String> {
    let wide = to_wide(exe_path);
    let path = PCWSTR(wide.as_ptr());

    let size = unsafe { GetFileVersionInfoSizeW(path, None) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    unsafe {
        GetFileVersionInfoW(path, None, size, data.as_mut_ptr().cast()).ok()?;
    }

    // 先取翻译表，得到 lang/codepage，再拼出具体字符串键
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    let query = |sub: &str, ptr: &mut *mut std::ffi::c_void, len: &mut u32| -> bool {
        let sub_wide = to_wide(sub);
        unsafe {
            VerQueryValueW(
                data.as_ptr().cast(),
                PCWSTR(sub_wide.as_ptr()),
                ptr,
                len,
            )
        }
        .as_bool()
    };

    if !query("\\VarFileInfo\\Translation", &mut ptr, &mut len) || len < 4 {
        return None;
    }
    let trans = unsafe { std::slice::from_raw_parts(ptr as *const u16, 2) };
    let key = format!(
        "\\StringFileInfo\\{:04x}{:04x}\\FileDescription",
        trans[0], trans[1]
    );

    if !query(&key, &mut ptr, &mut len) || len == 0 {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u16, len as usize) };
    let desc = utf16_string(slice);
    if desc.is_empty() {
        None
    } else {
        Some(desc)
    }
}

/// 启用 SeDebugPrivilege（管理员默认持有但处于禁用状态）。
/// 启用后才能枚举/复制 SYSTEM 等高权限进程的句柄，参照 handle.exe / File Locksmith。
pub fn enable_debug_privilege() {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_err()
        {
            return;
        }
        let mut luid = windows::Win32::Foundation::LUID::default();
        let name = to_wide("SeDebugPrivilege");
        if LookupPrivilegeValueW(
            PCWSTR::null(),
            PCWSTR(name.as_ptr()),
            &mut luid,
        )
        .is_err()
        {
            let _ = CloseHandle(token);
            return;
        }
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [windows::Win32::Security::LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        // AdjustTokenPrivileges 在权限未实际变化时也可能返回"成功但未调整"，
        // 这里尽力而为，失败不影响主流程（Restart Manager 仍然可用）。
        let _ = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        let _ = CloseHandle(token);
    }
}
