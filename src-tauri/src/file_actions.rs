//! 文件操作命令：删除 / 重启后延迟删除（参照 LockHunter 的"删除"与
//! "下次重启时删除"两种处置方式；后者用 MoveFileEx 的
//! MOVEFILE_DELAY_UNTIL_REBOOT 标志，由会话管理器在下次启动时执行删除）。

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{DeleteFileW, MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};

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

/// 解析待校验路径的真实形态，堵住纯字符串前缀匹配的绕过面：
/// 打开句柄用 GetFinalPathNameByHandleW 取最终路径——跟随 junction/symlink、
/// 展开 8.3 短名（C:\Progra~1 → C:\Program Files）。
/// 目标不存在（重启删除允许不存在）时逐级对存在的祖先目录解析后拼回剩余部分；
/// 祖先都不存在时退化为 GetLongPathNameW 仅展开短名。
fn resolved_for_check(path: &str) -> String {
    if let Some(final_path) = crate::winutil::final_path_of(path) {
        return final_path;
    }
    let p = Path::new(path);
    let mut ancestor = p.parent();
    while let Some(dir) = ancestor {
        if let Some(base) = crate::winutil::final_path_of(&dir.to_string_lossy()) {
            match p.strip_prefix(dir) {
                Ok(rel) => {
                    let rel = rel.to_string_lossy().replace('/', "\\");
                    return if rel.is_empty() {
                        base
                    } else {
                        format!("{base}\\{rel}")
                    };
                }
                Err(_) => break,
            }
        }
        ancestor = dir.parent();
    }
    crate::winutil::long_path_name(path)
}

/// 判断路径是否为系统关键位置——这些位置禁止删除，防止误操作损坏系统。
/// 只覆盖操作系统与全局程序目录；用户目录（C:\Users\...）是本工具的
/// 主战场，不在保护之列。
///
/// 比较前必须先归一化：先做真实路径解析（跟随 junction、展开 8.3 短名），
/// 再处理 `C:/Windows/...`（正斜杠）与 `\\?\C:\Windows`（NT 前缀）等形式——
/// 它们都能逃过朴素的文本前缀匹配，但 DeleteFileW 都接受。
fn is_protected_location(path: &str) -> bool {
    // 解析后的路径已是长名最终形态（\\?\ 前缀已在解析层剥离）
    let resolved = resolved_for_check(path);
    let mut lower = resolved.trim().to_lowercase().replace('/', "\\");
    for prefix in [r"\\?\", r"\\.\"] {
        if let Some(stripped) = lower.strip_prefix(prefix) {
            lower = stripped.to_string();
        }
    }
    let lower = lower.trim_end_matches('\\');

    let mut protected: Vec<String> = vec![
        "c:\\windows".into(),
        "c:\\program files".into(),
        "c:\\program files (x86)".into(),
    ];
    // 系统不一定装在 C 盘：以环境变量报告的真实目录为准
    for var in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_lowercase().replace('/', "\\");
            let v = v.trim_end_matches('\\');
            if !v.is_empty() {
                protected.push(v.to_string());
            }
        }
    }
    protected
        .iter()
        .any(|dir| lower == dir || lower.starts_with(&format!("{dir}\\")))
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
            "删除失败：文件仍被占用；可先结束全部占用进程，或改用\"重启后删除\"".to_string()
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
        // 正斜杠与 \\?\ 前缀形式不得绕过系统目录保护
        assert!(delete_on_reboot("C:/Windows/System32/x.dll").is_err());
        assert!(delete_on_reboot("\\\\?\\C:\\Windows\\x.dll").is_err());
        // 保护规则的错误应是"受保护"而非"不存在"——注意 must_exist 校验
        // 先行，因此用确实存在的系统文件断言错误类型
        let err = delete_file("C:\\Windows\\explorer.exe").unwrap_err();
        assert!(err.contains("受保护"), "unexpected: {err}");
    }

    #[test]
    fn rejects_short_name_bypass() {
        // 8.3 短名（C:\Progra~1）经最终路径解析后必须命中 Program Files 保护；
        // 仅在标准布局（Program Files 位于 C 盘）机器上断言
        let pf = std::env::var("ProgramFiles")
            .unwrap_or_default()
            .to_lowercase();
        if !pf.starts_with("c:\\program files") {
            return;
        }
        let err = delete_on_reboot("C:\\Progra~1\\fu-bypass-probe.dll").unwrap_err();
        assert!(err.contains("受保护"), "unexpected: {err}");
    }

    #[test]
    fn rejects_junction_bypass() {
        // 指向系统目录的 junction 经最终路径解析后同样命中保护
        let link = std::env::temp_dir().join("fu_junction_probe");
        let _ = std::fs::remove_dir(&link);
        let out = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(r"C:\Windows")
            .output();
        if out.map(|o| !o.status.success()).unwrap_or(true) {
            return; // 无法创建 junction 的环境上跳过
        }
        let probe = link.join("fu-junction-probe.dll");
        let err = delete_on_reboot(&probe.to_string_lossy()).unwrap_err();
        let _ = std::fs::remove_dir(&link); // 删 junction 本身，不影响目标
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
