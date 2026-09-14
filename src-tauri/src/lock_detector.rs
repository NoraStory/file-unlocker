//! 文件占用检测与进程结束。
//!
//! 检测采用双引擎（参照 PowerToys File Locksmith 与 LockHunter 的策略）：
//! 1. **Restart Manager**（与资源管理器"文件正在使用"对话框同源）：结果权威，
//!    附带应用显示名，但对 SYSTEM 进程 / 非常规共享模式可能漏报；
//! 2. **句柄枚举扫描**（`handle_scan` 模块）：遍历全系统句柄表补漏。
//! 两引擎结果按 PID 合并去重。
//!
//! 所有 `unsafe` FFI 细节收敛在本模块与子模块内部；
//! Restart Manager 会话句柄通过 RAII（`Drop`）保证释放，杜绝泄漏。

use std::path::Path;

use serde::Serialize;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, ERROR_SUCCESS, HANDLE, WIN32_ERROR};
use windows::Win32::System::RestartManager::{
    RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
    RM_PROCESS_INFO,
};
use windows::Win32::System::Threading::{TerminateProcess, PROCESS_TERMINATE};

use crate::handle_scan;
use crate::winutil::{file_description, process_image_full, to_wide, utf16_string};

/// 正在锁定文件的进程信息
#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    /// 进程 ID
    pub pid: u32,
    /// 进程可执行文件名（如 WINWORD.EXE）
    pub process_name: String,
    /// 进程可执行文件完整路径（查询失败时为空）
    pub exe_path: String,
    /// exe 版本信息中的文件描述（如 "Microsoft Word"，查询失败时为空）
    pub description: String,
    /// Restart Manager 报告的应用显示名（可能为空）
    pub app_name: String,
    /// 检测来源：Restart Manager / 句柄扫描 / 两者皆有
    pub source: String,
}

/// RAII 守卫：无论中途如何退出（包括 `?` 早退），都保证 `RmEndSession` 被调用。
struct RmSession {
    handle: u32,
}

impl RmSession {
    fn start() -> Result<Self, String> {
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

/// 引擎 1：Restart Manager 查询，返回 (pid → app_name)
fn query_restart_manager(file_path: &str) -> Result<Vec<(u32, String)>, String> {
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

    // 两次调用 RmGetList——第一次探测所需缓冲区条目数，第二次取数据
    let mut needed = 0u32;
    let mut count = 0u32;
    let err = unsafe {
        RmGetList(
            session.handle,
            &mut needed,
            &mut count,
            None,
            std::ptr::null_mut(),
        )
    };
    if err != ERROR_MORE_DATA && err != ERROR_SUCCESS {
        return Err(format!("RmGetList(探测大小) 失败 (错误码 {})", err.0));
    }
    if needed == 0 {
        return Ok(Vec::new());
    }

    let mut buf = vec![RM_PROCESS_INFO::default(); needed as usize];
    let mut count = needed;
    let err = unsafe {
        RmGetList(
            session.handle,
            &mut needed,
            &mut count,
            Some(buf.as_mut_ptr()),
            std::ptr::null_mut(),
        )
    };
    if err != ERROR_SUCCESS {
        return Err(format!("RmGetList 失败 (错误码 {})", err.0));
    }
    buf.truncate(count as usize);

    Ok(buf
        .iter()
        .map(|raw| (raw.Process.dwProcessId, utf16_string(&raw.strAppName)))
        .collect())
}

/// 查询锁定 `file_path` 的所有进程（双引擎合并）。
pub fn get_locking_processes(file_path: &str) -> Result<Vec<ProcessInfo>, String> {
    // 引擎 1：Restart Manager（失败不致命，继续走句柄扫描）
    let rm_result = query_restart_manager(file_path);
    let rm_list = rm_result.clone().unwrap_or_default();

    // 引擎 2：句柄枚举扫描补漏
    let hm_pids = handle_scan::scan(Path::new(file_path));

    // 合并去重：rm 为主（有 app_name），句柄扫描补漏
    use std::collections::HashMap;
    let mut merged: HashMap<u32, ProcessInfo> = HashMap::new();

    for (pid, app_name) in rm_list {
        let exe_path = process_image_full(pid).unwrap_or_default();
        let description = file_description(&exe_path).unwrap_or_default();
        let process_name = exe_path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&app_name)
            .to_string();
        merged.insert(
            pid,
            ProcessInfo {
                pid,
                process_name,
                exe_path,
                description,
                app_name,
                source: "restart_manager".into(),
            },
        );
    }

    for pid in hm_pids {
        merged
            .entry(pid)
            .or_insert_with(|| {
                let exe_path = process_image_full(pid).unwrap_or_default();
                let description = file_description(&exe_path).unwrap_or_default();
                let process_name = exe_path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or_default()
                    .to_string();
                ProcessInfo {
                    pid,
                    process_name,
                    exe_path,
                    description,
                    app_name: String::new(),
                    source: "handle_scan".into(),
                }
            })
            .source = "both".into();
    }

    // rm 查询彻底失败且句柄扫描也没结果时，把错误带出去
    if merged.is_empty() {
        rm_result?;
    }

    let mut list: Vec<ProcessInfo> = merged.into_values().collect();
    list.sort_by_key(|p| p.pid);
    Ok(list)
}

/// 强制结束单个进程（TerminateProcess）。
///
/// 以管理员运行时可结束绝大多数进程；普通权限下对系统/提权进程会失败。
pub fn kill_process(pid: u32) -> Result<(), String> {
    if pid == 0 {
        return Err("无效的进程 ID".into());
    }

    let handle = use_winapi_open_process_terminate(pid)?;
    let result = unsafe { TerminateProcess(handle, 1) };
    unsafe {
        let _ = CloseHandle(handle);
    }

    result.map_err(|e| format!("结束进程 {pid} 失败：{e}"))
}

fn use_winapi_open_process_terminate(
    pid: u32,
) -> Result<HANDLE, String> {
    unsafe { windows::Win32::System::Threading::OpenProcess(PROCESS_TERMINATE, false, pid) }
        .map_err(|e| format!("打开进程 {pid} 失败（可能需要管理员权限）：{e}"))
}

/// 结束进程树：先结束子进程再结束父进程（参照 LockHunter / taskkill /T）。
///
/// 通过 NtQuerySystemInformation 的进程快照以 ParentProcessId 归组，
/// 递归收集指定 pid 的全部后代后逐个 TerminateProcess。
pub fn kill_process_tree(pid: u32) -> Result<(), String> {
    const SYSTEM_PROCESS_INFORMATION: u32 = 5;

    // 拉取全系统进程快照（带自动增长重试）
    let mut len = 0x100_0000u32; // 16MB 起步
    let snapshot = loop {
        let mut buf = vec![0u8; len as usize];
        let mut ret = 0u32;
        let status = unsafe {
            windows::Wdk::System::SystemInformation::NtQuerySystemInformation(
                windows::Wdk::System::SystemInformation::SYSTEM_INFORMATION_CLASS(
                    SYSTEM_PROCESS_INFORMATION as i32,
                ),
                buf.as_mut_ptr().cast(),
                len,
                &mut ret,
            )
        };
        if status == windows::Win32::Foundation::STATUS_INFO_LENGTH_MISMATCH {
            len = if ret > len { ret } else { len * 2 };
            if len > 0x400_0000 {
                return Err("进程快照过大，无法枚举".into());
            }
            continue;
        }
        if status.0 != 0 {
            return Err(format!("NtQuerySystemInformation 失败 (NTSTATUS 0x{:08x})", status.0));
        }
        buf.truncate(ret as usize);
        break buf;
    };

    // x64 SYSTEM_PROCESS_INFORMATION 头部关键字段偏移：
    // 0: NextEntryOffset, 8: NumberOfThreads(u32), 12: pad, 16: WorkingSetSize...(跳过),
    // 实际布局：NextEntryOffset(4)+NumberOfThreads(4)+...+UniqueProcessId(偏移64)+...+ParentProcessId(偏移88)+ImageName(偏移96, UNICODE_STRING)
    // 直接用偏移常量遍历，避免声明完整结构体。
    const OFF_NEXT: usize = 0;
    const OFF_PID: usize = 64;
    const OFF_PARENT_PID: usize = 88;

    let mut parents: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    let mut offset = 0usize;
    while offset + 96 <= snapshot.len() {
        let base = unsafe { snapshot.as_ptr().add(offset) };
        let next = unsafe { *(base.add(OFF_NEXT) as *const u32) } as usize;
        let pid_now = unsafe { *(base.add(OFF_PID) as *const u32) };
        let parent = unsafe { *(base.add(OFF_PARENT_PID) as *const u32) };
        if pid_now > 0 {
            parents.entry(parent).or_default().push(pid_now);
        }
        if next == 0 {
            break;
        }
        offset += next;
    }

    // BFS 收集后代（含自身）
    let mut to_kill = vec![pid];
    let mut seen = std::collections::HashSet::new();
    seen.insert(pid);
    let mut queue = std::collections::VecDeque::from(vec![pid]);
    while let Some(cur) = queue.pop_front() {
        if let Some(children) = parents.get(&cur) {
            for &c in children {
                if seen.insert(c) {
                    to_kill.push(c);
                    queue.push_back(c);
                }
            }
        }
    }

    let mut errors = Vec::new();
    for &p in to_kill.iter().rev() {
        // 先结束最深的后代
        if let Err(e) = kill_process(p) {
            // 进程可能已自行退出，忽略"找不到进程"类失败
            errors.push(format!("{p}: {e}"));
        }
    }
    if errors.len() == to_kill.len() && !to_kill.is_empty() {
        return Err(format!(
            "结束进程树全部失败：{}",
            errors.join("; ")
        ));
    }
    Ok(())
}
