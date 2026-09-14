//! 文件操作命令：删除 / 重启后延迟删除（参照 LockHunter 的"删除"与
//! "下次重启时删除"两种处置方式；后者用 MoveFileEx 的
//! MOVEFILE_DELAY_UNTIL_REBOOT 标志，由会话管理器在下次启动时执行删除）。

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    DeleteFileW, MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT,
};

use crate::winutil::{to_wide, win32_err};

/// 统一的路径校验：非空、绝对路径、真实存在（重启删除允许不存在，
/// 因为用户可能计划删除一个已被移动/重命名的路径，故单独放宽）。
fn validate_path(path: &str, must_exist: bool) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("文件路径为空".into());
    }
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("需要绝对路径，收到：{path}"));
    }
    if must_exist && !p.exists() {
        return Err(format!("文件不存在：{path}"));
    }
    Ok(())
}

/// 判断路径是否为系统关键位置——这些位置禁止删除，防止误操作损坏系统。
/// 只覆盖操作系统与全局程序目录；用户目录（C:\Users\...）是本工具的
/// 主战场，不在保护之列。
fn is_protected_location(path: &str) -> bool {
    const PROTECTED: [&str; 3] = [
        "c:\\windows",
        "c:\\program files",
        "c:\\program files (x86)",
    ];
    let lower = path.to_lowercase();
    PROTECTED.iter().any(|dir| {
        lower == *dir || lower.starts_with(&format!("{dir}\\"))
    })
}

/// 立即删除文件。占用未释放或权限不足时返回可读错误。
pub fn delete_file(path: &str) -> Result<(), String> {
    validate_path(path, true)?;
    if is_protected_location(path) {
        return Err(format!("拒绝删除：{path} 位于系统受保护目录"));
    }

    let wide = to_wide(path);
    unsafe { DeleteFileW(PCWSTR(wide.as_ptr())) }.map_err(|e| {
        let msg = win32_err(&e);
        if msg.contains("被另一进程使用") {
            format!("删除失败：文件仍被占用；可先结束全部占用进程，或改用\"重启后删除\"")
        } else {
            format!("删除失败：{msg}")
        }
    })
}

/// 计划在下次系统重启时删除文件（对被锁定的文件同样有效，
/// 因为删除动作由内核会话管理器在启动早期执行，早于绝大多数进程启动）。
pub fn delete_on_reboot(path: &str) -> Result<(), String> {
    validate_path(path, false)?;
    if is_protected_location(path) {
        return Err(format!("拒绝计划删除：{path} 位于系统受保护目录"));
    }

    let wide = to_wide(path);
    // MoveFileEx 到空目标 + DELAY_UNTIL_REBOOT = 重启时删除；需要管理员身份，
    // 本程序默认管理员启动，满足条件。
    unsafe {
        MoveFileExW(
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    }
    .map_err(|e| format!("计划重启删除失败：{}", win32_err(&e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_path() {
        assert!(delete_file("").is_err());
        assert!(delete_on_reboot("   ").is_err());
    }

    #[test]
    fn rejects_relative_path() {
        assert!(delete_file("foo.txt").is_err());
        assert!(delete_on_reboot("foo\\bar.txt").is_err());
    }

    #[test]
    fn rejects_protected_locations() {
        assert!(delete_file("C:\\Windows\\System32\\kernel32.dll").is_err());
        assert!(delete_on_reboot("c:\\Program Files\\x\\y.dll").is_err());
        // 保护规则的错误应是"受保护"而非"不存在"——注意 must_exist 校验
        // 先行，因此用确实存在的系统文件断言错误类型
        let err = delete_file("C:\\Windows\\explorer.exe").unwrap_err();
        assert!(err.contains("受保护"), "unexpected: {err}");
    }

    #[test]
    fn allows_user_files() {
        // 不存在的用户目录文件：路径校验通过，实际删除时才报"不存在"
        let err = delete_file("C:\\Users\\NonExistent\\a.txt").unwrap_err();
        assert!(err.contains("不存在"), "unexpected: {err}");
        assert!(!err.contains("受保护"));
    }

    #[test]
    fn deletes_real_user_file() {
        // 正向用例：用户目录下真实存在的文件应可删除（工具的核心场景）
        let dir = std::env::temp_dir().join("file_unlocker_del_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("victim.txt");
        std::fs::write(&path, b"to be deleted").unwrap();

        delete_file(path.to_string_lossy().as_ref()).expect("删除用户文件失败");
        assert!(!path.exists(), "文件删除后仍存在");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
