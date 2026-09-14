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

/// 从 `windows::core::Error` 提取 Win32 错误码（HRESULT 0x8007xxxx 的低 16 位）
pub fn win32_code(e: &windows::core::Error) -> u32 {
    (e.code().0 & 0xFFFF) as u32
}

/// 将 Win32 错误翻译为可读中文，常见错误码给出针对性提示
pub fn win32_err(e: &windows::core::Error) -> String {
    match win32_code(e) {
        5 => "拒绝访问（可能需要管理员权限）".into(),
        87 => "参数无效（进程可能已退出）".into(),
        1168 => "找不到对应进程（可能已退出）".into(),
        2 => "系统找不到指定的文件".into(),
        3 => "系统找不到指定的路径".into(),
        32 => "文件正被另一进程使用".into(),
        _ => format!("{e} (错误码 {})", win32_code(e)),
    }
}

/// 端到端验证用的直接删除探针（不走 file_actions 的系统目录保护，
/// 专门验证"占用中删除失败、释放后删除成功"的原始语义）
#[cfg(feature = "e2e")]
pub fn e2e_delete_probe(path: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::DeleteFileW;
    let wide = to_wide(path);
    match unsafe { DeleteFileW(PCWSTR(wide.as_ptr())) } {
        Ok(()) => Ok(()),
        Err(e) => Err(win32_err(&e)),
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

#[cfg(test)]
mod tests {    use super::*;

    #[test]
    fn to_wide_appends_nul_terminator() {
        let w = to_wide("ab");
        assert_eq!(w, vec![b'a' as u16, b'b' as u16, 0]);
    }

    #[test]
    fn to_wide_keeps_non_ascii() {
        let w = to_wide("中文");
        assert_eq!(w, vec!['中' as u16, '文' as u16, 0]);
    }

    #[test]
    fn utf16_string_stops_at_nul() {
        assert_eq!(utf16_string(&[b'a' as u16, 0, b'b' as u16]), "a");
    }

    #[test]
    fn utf16_string_handles_missing_nul() {
        assert_eq!(utf16_string(&[b'x' as u16]), "x");
    }

    #[test]
    fn win32_code_maps_access_denied() {
        // 0x80070005 → Win32 错误 5（拒绝访问）
        let e = windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_0005u32 as i32));
        assert_eq!(win32_code(&e), 5);
        let msg = win32_err(&e);
        assert!(msg.contains("拒绝访问"), "unexpected: {msg}");
    }
}
