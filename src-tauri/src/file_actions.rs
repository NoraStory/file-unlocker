//! 文件操作命令：删除 / 重启后延迟删除（参照 LockHunter 的"删除"与
//! "下次重启时删除"两种处置方式；后者用 MoveFileEx 的
//! MOVEFILE_DELAY_UNTIL_REBOOT 标志，由会话管理器在下次启动时执行删除）。

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    DeleteFileW, MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT,
};

use crate::winutil::to_wide;

/// 立即删除文件。占用未释放时返回可读错误。
pub fn delete_file(path: &str) -> Result<(), String> {
    let wide = to_wide(path);
    unsafe { DeleteFileW(PCWSTR(wide.as_ptr())) }.map_err(|e| format!("删除失败：{e}"))
}

/// 计划在下次系统重启时删除文件（对被锁定的文件同样有效，
/// 因为删除动作由内核会话管理器在启动早期执行，早于绝大多数进程启动）。
pub fn delete_on_reboot(path: &str) -> Result<(), String> {
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
    .map_err(|e| format!("计划重启删除失败：{e}"))
}
