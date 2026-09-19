//! 引擎 1：Restart Manager 查询。
//!
//! 与资源管理器"文件正在使用"对话框同源：结果权威、附带应用显示名，
//! 但对 SYSTEM 进程 / 非常规共享模式可能漏报。
//! 会话句柄通过 RAII（`RmSession` Drop）保证释放，杜绝泄漏。

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::RestartManager::{
    RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
    RM_PROCESS_INFO,
};

use crate::winutil::{to_wide, utf16_string};

/// RAII 守卫：无论中途如何退出（包括 `?` 早退），都保证 `RmEndSession` 被调用。
pub(crate) struct RmSession {
    pub handle: u32,
}

impl RmSession {
    pub fn start() -> Result<Self, String> {
        let mut handle = 0u32;
        let mut session_key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        let err = unsafe { RmStartSession(&mut handle, None, PWSTR(session_key.as_mut_ptr())) };
        if err != ERROR_SUCCESS {
            return Err(format!("RmStartSession 失败 (错误码 {})", err.0));
        }
        Ok(Self { handle })
    }
}

impl Drop for RmSession {
    fn drop(&mut self) {
        unsafe {
            let _ = RmEndSession(self.handle);
        }
    }
}

/// RmGetList 重试循环：首次探测返回所需条目数（ERROR_MORE_DATA），
/// 随后取数；进程表在两次调用间可能变化，仍报 MORE_DATA 时按新大小重试。
/// 会话句柄由调用方管理（RAII 守卫或手动 RmEndSession）。
///
/// # Safety
/// `handle` 必须是 RmStartSession 返回的有效会话句柄。
unsafe fn rm_get_list_with_retry(handle: u32) -> Result<Vec<(u32, String)>, String> {
    let mut needed = 0u32;
    let mut count = 0u32;
    let mut buf: Vec<RM_PROCESS_INFO> = Vec::new();
    for _ in 0..5 {
        let err = unsafe {
            RmGetList(
                handle,
                &mut needed,
                &mut count,
                if buf.is_empty() {
                    None
                } else {
                    Some(buf.as_mut_ptr())
                },
                std::ptr::null_mut(),
            )
        };
        if err == ERROR_SUCCESS {
            // count 个有效条目已写入 buf
            buf.truncate(count as usize);
            return Ok(buf
                .iter()
                .map(|raw| (raw.Process.dwProcessId, utf16_string(&raw.strAppName)))
                .collect());
        }
        if err != ERROR_MORE_DATA || needed == 0 {
            return Err(format!("RmGetList 失败 (错误码 {})", err.0));
        }
        buf = vec![RM_PROCESS_INFO::default(); needed as usize];
        count = needed;
    }
    Err("进程表持续变化，RmGetList 重试 5 次仍未成功".into())
}

/// 查询锁定 `file_path` 的所有进程，返回 (pid → app_name)。
///
/// 资源注册阶段对"文件不存在"给出明确错误，其余阶段失败带错误码。
pub fn query_restart_manager(file_path: &str) -> Result<Vec<(u32, String)>, String> {
    if file_path.is_empty() {
        return Err("文件路径为空".into());
    }
    if !std::path::Path::new(file_path).exists() {
        return Err(format!("文件不存在：{file_path}"));
    }
    let wide_path = to_wide(file_path);

    let session = RmSession::start()?;

    let err: WIN32_ERROR = unsafe {
        RmRegisterResources(
            session.handle,
            Some(&[PCWSTR(wide_path.as_ptr())]),
            None,
            None,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(format!("RmRegisterResources 失败 (错误码 {})", err.0));
    }

    unsafe { rm_get_list_with_retry(session.handle) }
}

/// RM 批量查询：一次会话注册多个文件资源，返回 pid → app_name。
/// 资源数上限受 RM 实现限制（实测数百可用），超出时分批注册。
pub fn batch_query_restart_manager(
    files: &[std::path::PathBuf],
) -> Result<Vec<(u32, String)>, String> {
    const BATCH: usize = 500;
    let mut all = Vec::new();
    for chunk in files.chunks(BATCH) {
        match query_restart_manager_batch(chunk) {
            Ok(mut list) => all.append(&mut list),
            Err(_) => continue, // 部分系统 RM 不可用，静默交给句柄扫描
        }
    }
    Ok(all)
}

/// 单批 RM 查询（一次会话注册多个资源）
fn query_restart_manager_batch(files: &[std::path::PathBuf]) -> Result<Vec<(u32, String)>, String> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let wide_paths: Vec<Vec<u16>> = files
        .iter()
        .map(|p| to_wide(p.to_string_lossy().as_ref()))
        .collect();
    let pcsz: Vec<PCWSTR> = wide_paths.iter().map(|w| PCWSTR(w.as_ptr())).collect();

    let session = RmSession::start()?;

    let err = unsafe { RmRegisterResources(session.handle, Some(&pcsz), None, None) };
    if err != ERROR_SUCCESS {
        return Err(format!("RmRegisterResources 失败 (错误码 {})", err.0));
    }

    unsafe { rm_get_list_with_retry(session.handle) }
}

/// 供性能基准示例调用（内部函数的透传包装）
#[doc(hidden)]
#[cfg(feature = "e2e")]
pub fn bench_rm_query(path: &str) -> Result<Vec<(u32, String)>, String> {
    query_restart_manager(path)
}
