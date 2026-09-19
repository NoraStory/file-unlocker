//! 跨模块共享的 Win32 小工具：宽字符转换、进程路径查询、
//! 版本信息（文件描述）读取、SeDebugPrivilege 启用。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
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

/// 从已打开的进程句柄查询可执行文件完整路径。
pub fn process_image_from_handle(handle: HANDLE) -> Option<String> {
    // 映像路径可超 MAX_PATH（长路径/\\?\ 形式）：1024 起步，缓冲不足按失败翻倍，上限 32767
    let mut len = 1024usize;
    loop {
        let mut buf = vec![0u16; len];
        let mut size = buf.len() as u32;
        let r = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
        };
        if r.is_ok() {
            return Some(String::from_utf16_lossy(&buf[..size as usize]));
        }
        use windows::Win32::Foundation::{GetLastError, ERROR_INSUFFICIENT_BUFFER};
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || len >= 32767 {
            return None;
        }
        len *= 2;
    }
}

/// 查询进程可执行文件的完整路径（需要 PROCESS_QUERY_LIMITED_INFORMATION）
pub fn process_image_full(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let result = process_image_from_handle(HANDLE(handle.0));
    unsafe {
        let _ = CloseHandle(HANDLE(handle.0));
    }
    result
}

/// 读取已打开进程句柄的创建时间，返回 FILETIME 的 100ns 计数。
pub fn process_creation_time_from_handle(handle: HANDLE) -> Option<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }.ok()?;
    Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
}

/// 查询进程创建时间，用于避免 PID 复用后误操作无关进程。
pub fn process_creation_time(pid: u32) -> Option<u64> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let result = process_creation_time_from_handle(HANDLE(handle.0));
    unsafe {
        let _ = CloseHandle(HANDLE(handle.0));
    }
    result
}

/// 去掉 Win32 设备路径前缀：`\\?\C:\...` → `C:\...`；
/// UNC 设备路径还原为 `\\server\share`（`\\?\UNC\server\share` → `\\server\share`）。
/// 大小写保持原样，需要归一化的调用方自行处理。
pub fn strip_device_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        // GetFinalPathNameByHandleW 固定返回大写 "UNC"，用户传入可能为小写，两种都认
        let unc = rest
            .strip_prefix(r"UNC\")
            .or_else(|| rest.strip_prefix(r"unc\"));
        if let Some(unc) = unc {
            return format!(r"\\{unc}");
        }
        return rest.to_string();
    }
    path.to_string()
}

/// 将路径转换为统一的可比较形式：剥离设备前缀、统一分隔符并转小写。
pub fn normalize_path_string(path: &str) -> String {
    strip_device_prefix(path).replace('/', "\\").to_lowercase()
}

/// 使用 GetLongPathNameW 展开 8.3 短名；失败时保留原路径。
pub fn long_path_name(path: &str) -> String {
    use windows::Win32::Storage::FileSystem::GetLongPathNameW;
    let wide = to_wide(path);
    let mut len = 1024usize;
    loop {
        let mut buf = vec![0u16; len];
        let n = unsafe { GetLongPathNameW(PCWSTR(wide.as_ptr()), Some(&mut buf)) } as usize;
        if n == 0 {
            return path.to_string();
        }
        if n <= buf.len() {
            return utf16_string(&buf[..n]);
        }
        if n > 32767 {
            return path.to_string();
        }
        len = n;
    }
}

/// 将目标路径解析到最终形态后，再做大小写不敏感比较。
pub fn normalize_path_for_compare(path: &str) -> String {
    let resolved = final_path_of(path).unwrap_or_else(|| long_path_name(path));
    normalize_path_string(&resolved)
}

/// 从已打开的句柄取最终路径（跟随 junction/symlink）。
/// GetFinalPathNameByHandleW 缓冲不足时返回所需长度（含 NUL），
/// 按返回值增长重试（1024 起步、上限 32767），长路径不漏报。
pub fn final_path_from_handle(handle: HANDLE) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED};
    let mut len = 1024usize;
    loop {
        let mut buf = vec![0u16; len];
        // FILE_NAME_NORMALIZED(0) 与 VOLUME_NAME_DOS(0) 都是 0，等价于默认值组合
        let n =
            unsafe { GetFinalPathNameByHandleW(handle, &mut buf, FILE_NAME_NORMALIZED) } as usize;
        if n == 0 {
            return None;
        }
        if n <= buf.len() {
            return Some(strip_device_prefix(&String::from_utf16_lossy(&buf[..n])));
        }
        if n > 32767 {
            log::warn!("[路径] 最终路径超过 32767 字符上限（需要 {n}）");
            return None;
        }
        len = n;
    }
}

/// 打开文件/目录并解析其最终路径（跟随 junction/symlink、展开 8.3 短名）。
/// 仅需元数据查询（访问权限 0），共享模式放到最宽，尽量打开成功；
/// 打不开（不存在/被独占占用）时返回 None，由调用方降级处理。
pub fn final_path_of(path: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let wide = to_wide(path);
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0, // 无访问权限需求（仅查询路径）
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS, // 允许打开目录
            None,
        )
    }
    .ok()?;
    let result = final_path_from_handle(handle);
    unsafe {
        let _ = CloseHandle(handle);
    }
    result
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
        unsafe { VerQueryValueW(data.as_ptr().cast(), PCWSTR(sub_wide.as_ptr()), ptr, len) }
            .as_bool()
    };

    // 注意：VerQueryValueW 的 puLen 单位是字节。
    // Translation 表每项 4 字节（LANGID + CODEPAGE 两个 u16），故下限 4
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
    // puLen 是字节数：按 u16 读取必须 /2，直接当字符数会越界一倍（UB）
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u16, len as usize / 2) };
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
/// 专门验证"占用中删除失败、释放后删除成功"的原始语义）。
/// 注意：不能 cfg(feature = "e2e") 门控——examples/e2e_check.rs 用 #[path]
/// 内嵌本文件，`cargo test` 不带 feature 构建示例时会编译失败
#[allow(dead_code)] // 主程序不调用；仅示例（#[path] 内嵌另一编译单元）使用
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
        if LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(name.as_ptr()), &mut luid).is_err() {
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
mod tests {
    use super::*;

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

    #[test]
    fn strip_device_prefix_handles_local_and_unc() {
        assert_eq!(strip_device_prefix(r"\\?\C:\Windows"), r"C:\Windows");
        assert_eq!(
            strip_device_prefix(r"\\?\UNC\server\share"),
            r"\\server\share"
        );
        assert_eq!(
            strip_device_prefix(r"\\?\unc\server\share"),
            r"\\server\share"
        );
        assert_eq!(strip_device_prefix(r"C:\plain"), r"C:\plain");
    }

    #[test]
    fn normalize_path_string_handles_slashes_and_prefixes() {
        assert_eq!(
            normalize_path_string(r"\\?\C:/Windows/System32"),
            r"c:\windows\system32"
        );
        assert_eq!(
            normalize_path_string(r"\\?\UNC\NAS\Docs/F.txt"),
            r"\\nas\docs\f.txt"
        );
    }

    #[test]
    fn normalize_path_string_edge_cases() {
        // 空串：安全返回空，不 panic
        assert_eq!(normalize_path_string(""), "");
        // 混合分隔符与大小写归一
        assert_eq!(
            normalize_path_string("D:/Data/Report.PDF"),
            r"d:\data\report.pdf"
        );
        // 比较语义：仅大小写/分隔符差异的路径归一后相等
        assert_eq!(
            normalize_path_string(r"C:\Windows\explorer.exe"),
            normalize_path_string("c:/windows/EXPLORER.EXE")
        );
        // 非设备前缀的双反斜杠 UNC 路径保持原样（仅归一化大小写与分隔符）
        assert_eq!(
            normalize_path_string(r"\\Server\Share\File"),
            r"\\server\share\file"
        );
    }

    #[test]
    fn strip_device_prefix_keeps_relative_paths() {
        // 相对路径与盘符根：原样返回，不做任何改写
        assert_eq!(strip_device_prefix("relative\\path.txt"), "relative\\path.txt");
        assert_eq!(strip_device_prefix(r"C:\"), r"C:\");
        // 仅前缀本身：剥离后为空串
        assert_eq!(strip_device_prefix(r"\\?\"), "");
        assert_eq!(strip_device_prefix(r"\\?\UNC\"), r"\\");
    }

    #[test]
    fn process_creation_time_is_available_for_self() {
        let creation = process_creation_time(std::process::id()).expect("self creation time");
        assert!(creation > 0);
    }

    #[test]
    fn process_creation_time_rejects_zero_pid() {
        assert_eq!(process_creation_time(0), None);
    }
}
