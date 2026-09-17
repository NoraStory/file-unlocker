//! 文件占用检测与进程结束。
//!
//! 检测采用双引擎（参照 PowerToys File Locksmith 与 LockHunter 的策略）：
//! 1. **Restart Manager**（与资源管理器"文件正在使用"对话框同源）：结果权威，
//!    附带应用显示名，但对 SYSTEM 进程 / 非常规共享模式可能漏报；
//! 2. **句柄枚举扫描**（`handle_scan` 模块）：遍历全系统句柄表补漏。
//!    两引擎结果按 PID 合并去重。
//!
//! 所有 `unsafe` FFI 细节收敛在本模块与子模块内部；
//! Restart Manager 会话句柄通过 RAII（`Drop`）保证释放，杜绝泄漏。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_MORE_DATA, ERROR_SUCCESS, HANDLE, STILL_ACTIVE, WAIT_FAILED, WAIT_OBJECT_0,
    WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows::Win32::System::RestartManager::{
    RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
    RM_PROCESS_INFO,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_ACCESS_RIGHTS,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

use crate::handle_scan;
use crate::winutil::{
    file_description, normalize_path_string, process_creation_time,
    process_creation_time_from_handle, process_image_from_handle, process_image_full, to_wide,
    utf16_string, win32_err,
};

/// 正在锁定文件的进程信息
#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    /// 进程 ID
    pub pid: u32,
    /// 进程创建时间（FILETIME 100ns 计数，字符串传输避免 JS 精度丢失）
    pub creation_time: String,
    /// 进程可执行文件名（如 WINWORD.EXE）
    pub process_name: String,
    /// 进程可执行文件完整路径（查询失败时为空）
    pub exe_path: String,
    /// exe 版本信息中的文件描述（如 "Microsoft Word"，查询失败时为空）
    pub description: String,
    /// Restart Manager 报告的应用显示名（可能为空）
    pub app_name: String,
    /// 检测来源：restart_manager / handle_scan / both
    pub source: String,
    /// 目录模式下该进程锁定的文件数量（单文件模式恒为 1）
    pub locked_files: u32,
}

/// 从 exe 完整路径提取进程名；exe 路径为空时回退到 fallback（如 RM 的 app_name）。
/// 注意："".rsplit().next() 是 Some("") 而非 None，必须显式过滤空串。
fn process_name_of(exe_path: &str, fallback: &str) -> String {
    exe_path
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// 统一构造进程信息，确保所有检测来源都带上稳定的创建时间。
fn make_process_info(pid: u32, app_name: String, source: &str, locked_files: u32) -> ProcessInfo {
    let exe_path = process_image_full(pid).unwrap_or_default();
    let description = file_description(&exe_path).unwrap_or_default();
    let process_name = process_name_of(&exe_path, &app_name);
    ProcessInfo {
        pid,
        creation_time: process_creation_time(pid).unwrap_or(0).to_string(),
        process_name,
        exe_path,
        description,
        app_name,
        source: source.to_string(),
        locked_files,
    }
}

/// 扫描结果：占用进程列表 + 目录模式的元信息
#[derive(Debug, Clone, Serialize)]
pub struct ScanOutcome {
    /// 占用进程列表（按 PID 排序）
    pub processes: Vec<ProcessInfo>,
    /// 目录模式下枚举文件数达到上限，结果被截断（前端应给出提示）
    pub truncated: bool,
    /// 目录模式实际枚举的文件数（截断时等于上限；单文件模式恒为 1）
    pub file_count: usize,
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

/// 引擎 1：Restart Manager 查询，返回 (pid → app_name)。
///
/// 资源注册阶段对"文件不存在"给出明确错误，其余阶段失败带错误码。
fn query_restart_manager(file_path: &str) -> Result<Vec<(u32, String)>, String> {
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

    // RmGetList 重试循环：首次探测返回所需条目数（ERROR_MORE_DATA），
    // 随后取数；进程表在两次调用间可能变化，仍报 MORE_DATA 时按新大小重试。
    let mut needed = 0u32;
    let mut count = 0u32;
    let mut buf: Vec<RM_PROCESS_INFO> = Vec::new();
    for _ in 0..5 {
        let err = unsafe {
            RmGetList(
                session.handle,
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

/// 查询锁定 `file_path` 的所有进程（双引擎合并）。
///
/// 目标是文件夹时：递归收集目录内（含子目录）被锁定的文件，
/// 结果按进程聚合——"谁占了文件夹里的东西"，附带各进程占用的文件数。
/// `on_progress` 在目录模式下被调用（已完成/总数），
/// 单文件模式不产生进度事件。
pub fn get_locking_processes(
    file_path: &str,
    on_progress: crate::handle_scan::ProgressCallback,
) -> Result<ScanOutcome, String> {
    let started = std::time::Instant::now();
    log::info!("[检测] 目标: {file_path}");
    // 输入校验：前端传来的路径必须真实存在，避免下游错误难排查
    if file_path.is_empty() {
        return Err("文件路径为空".into());
    }
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(format!("文件不存在：{file_path}"));
    }

    // 目录模式：枚举内部文件逐个检测后按 PID 聚合
    let result = if path.is_dir() {
        scan_directory(path, on_progress)
    } else {
        scan_single_file(file_path).map(|processes| ScanOutcome {
            processes,
            truncated: false,
            file_count: 1,
        })
    };
    match &result {
        Ok(outcome) => log::info!(
            "[检测] 完成: {} 个占用进程{}，耗时 {:?}",
            outcome.processes.len(),
            if outcome.truncated {
                "（目录文件数超上限，结果已截断）"
            } else {
                ""
            },
            started.elapsed()
        ),
        Err(e) => log::warn!("[检测] 失败: {e}，耗时 {:?}", started.elapsed()),
    }
    result
}

/// 文件夹模式：按目录前缀批量检测内部文件占用，并按 PID 聚合。
///
/// 句柄扫描对"查询目录本身"无效（锁的是内部文件，路径不等），
/// 因此使用目录前缀匹配，一次句柄表遍历即可覆盖所有内部文件。
/// 递归枚举仅用于统计文件数和检测可读性；为避免超大目录卡 UI，
/// 最多枚举 2000 个路径，且枚举数量不影响扫描覆盖范围。
fn scan_directory(
    dir: &Path,
    on_progress: crate::handle_scan::ProgressCallback,
) -> Result<ScanOutcome, String> {
    const MAX_FILES: usize = 2000;

    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue; // 无权限的子目录跳过
        };
        for entry in entries.flatten() {
            // entry.file_type() 不跟随重解析点；path.is_dir() 会跟随链接，
            // junction/symlink 环会死循环，同一文件也会被重复统计到触顶 MAX_FILES。
            // Windows 上 junction（MOUNT_POINT）与 symlink 的 is_symlink() 都为 true
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue; // junction / 符号链接目录：不下钻、不统计
            }
            let p = entry.path();
            if ft.is_dir() {
                stack.push(p);
            } else if files.len() < MAX_FILES {
                files.push(p);
            }
        }
        if files.len() >= MAX_FILES {
            break;
        }
    }

    let total = files.len();

    // 性能关键路径：一次句柄表遍历解析出全部文件句柄路径，
    // 与目录前缀做匹配——等价于逐文件 scan() 但从 O(N×全表) 降为 O(1×全表)。
    // RM 引擎对目录模式同样批量化：一次会话注册全部文件资源。
    let mut merged: HashMap<u32, ProcessInfo> = HashMap::new();

    if let Ok(rm_list) = batch_query_restart_manager(&files) {
        for (pid, app_name) in rm_list {
            // RM 不提供逐文件计数；至少记 1，防止 RM 独有的检出被误丢弃
            merged.insert(pid, make_process_info(pid, app_name, "restart_manager", 1));
        }
    }

    // 句柄扫描：单次遍历 + 目录前缀匹配；引擎失效时不得伪装成"无占用"
    let hit_map = match handle_scan::scan_directory(dir, on_progress) {
        Ok(m) => m,
        Err(e) => {
            log::error!("[检测] 句柄扫描引擎失效: {e}");
            // RM 侧也失败则整体报错，否则以 RM 结果为准
            if merged.is_empty() {
                return Err(format!("检测引擎异常：{e}"));
            }
            let mut list: Vec<ProcessInfo> = merged.into_values().collect();
            list.sort_by_key(|p| p.pid);
            return Ok(ScanOutcome {
                processes: list,
                truncated: false,
                file_count: total,
            });
        }
    };
    for (pid, paths) in hit_map {
        // 按实际命中的、确实位于目录内的路径数计数（去重：同一路径多句柄算一个文件）
        let uniq: HashSet<&String> = paths.iter().collect();
        let count = uniq.len() as u32;
        match merged.entry(pid) {
            // RM 也报了该 PID：标记双引擎命中，文件数以句柄扫描的精确计数为准
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let info = e.get_mut();
                info.source = "both".into();
                info.locked_files = count;
            }
            // 仅句柄扫描发现（RM 不可用或漏报）
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(make_process_info(pid, String::new(), "handle_scan", count));
            }
        }
    }

    // 目录模式真正依赖句柄扫描：一次遍历即可匹配整个目录前缀，
    // 文件枚举只用于确认目录可读和统计展示，扫描覆盖并不受 2000 上限截断。
    let mut list: Vec<ProcessInfo> = merged.into_values().collect();
    list.sort_by_key(|p| p.pid);
    Ok(ScanOutcome {
        processes: list,
        truncated: false,
        file_count: total,
    })
}

/// RM 批量查询：一次会话注册多个文件资源，返回 pid → app_name。
/// 资源数上限受 RM 实现限制（实测数百可用），超出时分批注册。
fn batch_query_restart_manager(files: &[std::path::PathBuf]) -> Result<Vec<(u32, String)>, String> {
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
    use windows::core::PCWSTR;
    use windows::Win32::System::RestartManager::{
        RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
        RM_PROCESS_INFO,
    };

    if files.is_empty() {
        return Ok(Vec::new());
    }
    let wide_paths: Vec<Vec<u16>> = files
        .iter()
        .map(|p| to_wide(p.to_string_lossy().as_ref()))
        .collect();
    let pcsz: Vec<PCWSTR> = wide_paths.iter().map(|w| PCWSTR(w.as_ptr())).collect();

    let mut handle = 0u32;
    let mut session_key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    let err = unsafe { RmStartSession(&mut handle, None, PWSTR(session_key.as_mut_ptr())) };
    if err != ERROR_SUCCESS {
        return Err(format!("RmStartSession 失败 (错误码 {})", err.0));
    }
    // 手动管理会话释放（批量路径不走 RAII 守卫，因注册资源数动态）
    let result = (|| -> Result<Vec<(u32, String)>, String> {
        let err = unsafe { RmRegisterResources(handle, Some(&pcsz), None, None) };
        if err != ERROR_SUCCESS {
            return Err(format!("RmRegisterResources 失败 (错误码 {})", err.0));
        }

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
    })();

    unsafe {
        let _ = RmEndSession(handle);
    }
    result
}

/// 供性能基准示例调用（内部函数的透传包装）
#[doc(hidden)]
#[cfg(feature = "e2e")]
pub fn bench_rm_query(path: &str) -> Result<Vec<(u32, String)>, String> {
    query_restart_manager(path)
}

/// 单文件模式：双引擎合并查询。
fn scan_single_file(file_path: &str) -> Result<Vec<ProcessInfo>, String> {
    // 引擎 1：Restart Manager（失败不致命，继续走句柄扫描）
    let rm_result = query_restart_manager(file_path);
    let rm_list = rm_result.clone().unwrap_or_default();

    // 引擎 2：句柄枚举扫描补漏（引擎失效时与 RM 结果联合判断）
    let hm_result = handle_scan::scan(Path::new(file_path));
    let hm_pids: Vec<u32> = hm_result.clone().unwrap_or_default();

    // 合并去重：rm 为主（有 app_name），句柄扫描补漏
    use std::collections::HashMap;
    let mut merged: HashMap<u32, ProcessInfo> = HashMap::new();

    for (pid, app_name) in rm_list {
        merged.insert(pid, make_process_info(pid, app_name, "restart_manager", 1));
    }

    for pid in hm_pids {
        match merged.entry(pid) {
            // RM 也报了该 PID：标记双引擎命中
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.get_mut().source = "both".into();
            }
            // 仅句柄扫描发现（RM 不可用或漏报）
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(make_process_info(pid, String::new(), "handle_scan", 1));
            }
        }
    }

    // 结果判定：
    // - 句柄扫描引擎失效（Err）且 RM 无结果 → 整体报错，不得伪装"无占用"
    // - 句柄扫描正常（Ok 空）→ 结果可信
    // - RM 失败但句柄扫描正常（部分系统 RM 服务异常）→ 句柄扫描独立兜底
    if merged.is_empty() {
        if let Err(e) = hm_result {
            let rm_failed = rm_result
                .as_ref()
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            log::error!("[检测] 双引擎均无结果：句柄扫描={e}；RM={rm_failed}");
            return Err(format!(
                "检测引擎异常（句柄扫描：{e}；Restart Manager：{rm_failed}）。请运行自检并导出日志反馈"
            ));
        }
        if let Err(e) = rm_result {
            // RM 失败但句柄扫描正常完成且无结果 → 结果可信，返回"无占用"
            log::warn!("[检测] Restart Manager 不可用（{e}），已由句柄扫描兜底");
        }
        return Ok(Vec::new());
    }

    let mut list: Vec<ProcessInfo> = merged.into_values().collect();
    list.sort_by_key(|p| p.pid);
    Ok(list)
}

/// 将前端传来的创建时间字符串解析为 FILETIME 计数。
fn parse_creation_time(raw: &str) -> Result<u64, String> {
    raw.parse::<u64>()
        .map_err(|_| format!("无效的进程创建时间：{raw}"))
}

/// 进程句柄 RAII 包装，确保所有错误路径都能释放句柄。
struct ProcessHandle(HANDLE);

impl ProcessHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

enum ProcessOpenError {
    Gone,
    Rejected(String),
}

/// 打开并校验进程身份后再允许终止。
///
/// PID 会在进程退出后被复用，因此不能只凭 PID 操作。创建时间和 exe 路径
/// 必须与扫描结果一致；关键系统进程和本程序始终拒绝结束。
fn open_verified_process(
    pid: u32,
    expected_creation_time: u64,
    expected_exe_path: Option<&str>,
) -> Result<ProcessHandle, ProcessOpenError> {
    if pid == 0 || pid == 4 {
        return Err(ProcessOpenError::Rejected(format!(
            "拒绝结束系统进程 PID {pid}"
        )));
    }
    if pid == std::process::id() {
        return Err(ProcessOpenError::Rejected("拒绝结束本程序自身".into()));
    }

    let access = PROCESS_TERMINATE
        | PROCESS_ACCESS_RIGHTS(SYNCHRONIZE.0)
        | PROCESS_QUERY_LIMITED_INFORMATION;
    let handle = match unsafe { OpenProcess(access, false, pid) } {
        Ok(handle) => handle,
        Err(e) if matches!(crate::winutil::win32_code(&e), 87 | 1168) => {
            return Err(ProcessOpenError::Gone);
        }
        Err(e) => {
            return Err(ProcessOpenError::Rejected(format!(
                "打开进程 {pid} 失败：{}",
                win32_err(&e)
            )));
        }
    };
    let proc = ProcessHandle(handle);

    let current_creation_time = process_creation_time_from_handle(proc.raw()).ok_or_else(|| {
        ProcessOpenError::Rejected(format!("无法读取进程 {pid} 的创建时间，已拒绝结束"))
    })?;
    if current_creation_time != expected_creation_time {
        return Err(ProcessOpenError::Rejected(format!(
            "进程 {pid} 已变化（PID 可能被复用），请重新检测后再操作"
        )));
    }

    let current_exe = process_image_from_handle(proc.raw()).ok_or_else(|| {
        ProcessOpenError::Rejected(format!("无法确认进程 {pid} 的可执行文件路径，已拒绝结束"))
    })?;
    if let Some(expected) = expected_exe_path {
        if expected.trim().is_empty()
            || normalize_path_string(&current_exe) != normalize_path_string(expected)
        {
            return Err(ProcessOpenError::Rejected(format!(
                "进程 {pid} 的可执行文件已变化，请重新检测后再操作"
            )));
        }
    }
    if is_protected_process_path(&current_exe) {
        return Err(ProcessOpenError::Rejected(format!(
            "拒绝结束受保护进程：{current_exe}"
        )));
    }

    Ok(proc)
}

fn is_protected_process_path(exe_path: &str) -> bool {
    let name = process_name_of(exe_path, "").to_ascii_lowercase();
    matches!(
        name.as_str(),
        "explorer.exe"
            | "winlogon.exe"
            | "csrss.exe"
            | "services.exe"
            | "lsass.exe"
            | "smss.exe"
            | "wininit.exe"
            | "dwm.exe"
            | "sihost.exe"
            | "fontdrvhost.exe"
    )
}

/// 结束已经完成身份校验的进程句柄。
fn terminate_verified_process(proc: &ProcessHandle, pid: u32) -> Result<(), String> {
    let mut exit_code = 0u32;
    if unsafe { GetExitCodeProcess(proc.raw(), &mut exit_code) }.is_ok()
        && exit_code != STILL_ACTIVE.0 as u32
    {
        return Ok(());
    }

    unsafe { TerminateProcess(proc.raw(), 1) }.map_err(|e| {
        if crate::winutil::win32_code(&e) == 1168 {
            "进程已退出".to_string()
        } else {
            format!("结束进程 {pid} 失败：{}", win32_err(&e))
        }
    })?;

    for _ in 0..30 {
        let wait = unsafe { WaitForSingleObject(proc.raw(), 100) };
        if wait == WAIT_OBJECT_0 {
            return Ok(());
        }
        if wait == WAIT_FAILED {
            return Err(format!("等待进程 {pid} 退出失败"));
        }
        if unsafe { GetExitCodeProcess(proc.raw(), &mut exit_code) }.is_ok()
            && exit_code != STILL_ACTIVE.0 as u32
        {
            return Ok(());
        }
    }

    Err(format!(
        "结束进程 {pid} 的请求已发出，但 3 秒内未确认退出；进程可能受系统保护"
    ))
}

/// 强制结束单个进程（TerminateProcess）。
///
/// `creation_time` 和 `exe_path` 来自检测结果；终止前会再次核对，防止 PID
/// 复用后误杀其他进程。
pub fn kill_process(pid: u32, creation_time: String, exe_path: String) -> Result<(), String> {
    let expected_creation_time = parse_creation_time(&creation_time)?;
    log::info!("[结束进程] pid={pid}");
    let proc = match open_verified_process(pid, expected_creation_time, Some(&exe_path)) {
        Ok(proc) => proc,
        Err(ProcessOpenError::Gone) => return Ok(()),
        Err(ProcessOpenError::Rejected(e)) => return Err(e),
    };
    terminate_verified_process(&proc, pid)
}

/// 进程快照中的稳定身份信息。
#[derive(Debug, Clone, Copy)]
struct ProcessSnapshotEntry {
    pid: u32,
    parent_pid: u32,
    creation_time: u64,
}

/// 判断后代创建时间是否早于当前父进程实例。
///
/// 孤儿进程会保留旧父 PID；如果该 PID 被复用，后代创建时间通常早于新
/// 父进程创建时间。这个检查可以阻断把旧父 PID 的后代并入新进程树。
fn is_descendant_stale(
    child: &ProcessSnapshotEntry,
    parent_pid: u32,
    parent_creation_time: u64,
) -> bool {
    child.parent_pid == parent_pid && child.creation_time < parent_creation_time
}

/// 拉取全系统进程快照，并读取 PID、父 PID 和创建时间。
fn build_process_snapshot() -> Result<Vec<ProcessSnapshotEntry>, String> {
    const SYSTEM_PROCESS_INFORMATION: u32 = 5;
    const MIN_ENTRY_SIZE: usize = 96;

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
            return Err(format!(
                "NtQuerySystemInformation 失败 (NTSTATUS 0x{:08x})",
                status.0
            ));
        }
        if ret == 0 {
            return Err("进程快照返回长度无效".into());
        }
        buf.truncate(ret as usize);
        break buf;
    };

    // x64 SYSTEM_PROCESS_INFORMATION 关键字段偏移（Windows 内核布局）：
    // CreateTime(32) → ImageName(56) → BasePriority(72) → UniqueProcessId(80)
    // → InheritedFromUniqueProcessId(88)。CreateTime 与 PID 一起构成稳定身份。
    const OFF_NEXT: usize = 0;
    const OFF_CREATE_TIME: usize = 32;
    const OFF_PID: usize = 80;
    const OFF_PARENT_PID: usize = 88;

    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset + MIN_ENTRY_SIZE <= snapshot.len() {
        let base = unsafe { snapshot.as_ptr().add(offset) };
        let next = unsafe { *(base.add(OFF_NEXT) as *const u32) } as usize;
        if next != 0 && (next < MIN_ENTRY_SIZE || offset + next > snapshot.len()) {
            return Err("进程快照条目边界异常".into());
        }

        let pid = unsafe { *(base.add(OFF_PID) as *const u32) };
        let parent_pid = unsafe { *(base.add(OFF_PARENT_PID) as *const u32) };
        let creation_time = unsafe { *(base.add(OFF_CREATE_TIME) as *const u64) };
        if pid > 0 {
            entries.push(ProcessSnapshotEntry {
                pid,
                parent_pid,
                creation_time,
            });
        }
        if next == 0 {
            break;
        }
        offset += next;
    }
    Ok(entries)
}

/// 判断快照中是否仍有 parent 链指向 `ancestor_pid` 的存活进程
/// （仅用于测试：验证进程树终止效果）
#[cfg(test)]
fn descendants_still_alive(ancestor_pid: u32) -> Option<Vec<u32>> {
    let snapshot = build_process_snapshot().ok()?;
    let mut parents: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    for entry in snapshot {
        parents.entry(entry.parent_pid).or_default().push(entry.pid);
    }

    let mut seen = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::from(vec![ancestor_pid]);
    let mut alive = Vec::new();
    while let Some(cur) = queue.pop_front() {
        if let Some(children) = parents.get(&cur) {
            for &c in children {
                if seen.insert(c) {
                    queue.push_back(c);
                    alive.push(c);
                }
            }
        }
    }
    if alive.is_empty() {
        None
    } else {
        Some(alive)
    }
}

/// 结束进程树：先结束子进程再结束父进程。
///
/// 根进程先通过句柄完成身份校验；每个后代也使用快照中的创建时间二次校验，
/// 避免 PID 复用或旧 PPID 导致误杀。
pub fn kill_process_tree(pid: u32, creation_time: String, exe_path: String) -> Result<(), String> {
    let expected_creation_time = parse_creation_time(&creation_time)?;
    if pid == 0 || pid == 4 {
        return Err(format!("无效的进程树根 PID：{pid}"));
    }
    log::info!("[结束进程树] 根 pid={pid}");

    let root = match open_verified_process(pid, expected_creation_time, Some(&exe_path)) {
        Ok(proc) => proc,
        Err(ProcessOpenError::Gone) => return Ok(()),
        Err(ProcessOpenError::Rejected(e)) => return Err(e),
    };
    let snapshot = build_process_snapshot()?;

    match snapshot.iter().find(|entry| entry.pid == pid) {
        Some(entry) if entry.creation_time == expected_creation_time => {}
        Some(_) => {
            return Err("目标进程已变化（PID 可能被复用），请重新检测后再操作".into());
        }
        None => return Ok(()),
    }

    let mut children: std::collections::HashMap<u32, Vec<ProcessSnapshotEntry>> =
        std::collections::HashMap::new();
    for entry in &snapshot {
        children.entry(entry.parent_pid).or_default().push(*entry);
    }

    let mut queue = std::collections::VecDeque::from([(pid, expected_creation_time)]);
    let mut seen = std::collections::HashSet::new();
    seen.insert(pid);
    let mut verified_children: Vec<(ProcessHandle, u32)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    while let Some((parent_pid, parent_creation_time)) = queue.pop_front() {
        let Some(kids) = children.get(&parent_pid) else {
            continue;
        };
        for child in kids {
            if !seen.insert(child.pid) {
                continue;
            }
            if is_descendant_stale(child, parent_pid, parent_creation_time) {
                log::warn!(
                    "[结束进程树] 跳过可疑后代 pid={}：创建时间早于父进程",
                    child.pid
                );
                continue;
            }
            match open_verified_process(child.pid, child.creation_time, None) {
                Ok(proc) => {
                    verified_children.push((proc, child.pid));
                    queue.push_back((child.pid, child.creation_time));
                }
                Err(ProcessOpenError::Gone) => {}
                Err(ProcessOpenError::Rejected(e)) => {
                    errors.push(format!("{}: {e}", child.pid));
                }
            }
        }
    }

    let mut killed = 0usize;
    for (proc, child_pid) in verified_children.iter().rev() {
        match terminate_verified_process(proc, *child_pid) {
            Ok(()) => killed += 1,
            Err(e) => errors.push(format!("{child_pid}: {e}")),
        }
    }
    match terminate_verified_process(&root, pid) {
        Ok(()) => killed += 1,
        Err(e) => errors.push(format!("{pid}: {e}")),
    }

    if !errors.is_empty() {
        return Err(format!(
            "结束进程树部分失败（成功 {killed}/{}）：{}",
            verified_children.len() + 1,
            errors.join("; ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_of(pid: u32) -> (String, String) {
        (
            process_creation_time(pid)
                .expect("creation time")
                .to_string(),
            process_image_full(pid).expect("process image"),
        )
    }

    #[test]
    fn kill_rejects_invalid_identity() {
        assert!(kill_process(1234, "not-a-time".into(), "x.exe".into()).is_err());
    }

    #[test]
    fn kill_rejects_zero_pid() {
        assert!(kill_process(0, "0".into(), String::new()).is_err());
    }

    #[test]
    fn kill_rejects_system_process() {
        let err = kill_process(4, "0".into(), String::new()).unwrap_err();
        assert!(
            err.contains("System") || err.contains("系统"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn kill_rejects_self() {
        let pid = std::process::id();
        let (creation, exe) = identity_of(pid);
        let err = kill_process(pid, creation, exe).unwrap_err();
        assert!(err.contains("自身"), "unexpected: {err}");
    }

    #[test]
    fn protected_process_names_are_rejected() {
        assert!(is_protected_process_path(r"C:\Windows\explorer.exe"));
        assert!(is_protected_process_path(r"C:\Windows\System32\lsass.exe"));
        assert!(!is_protected_process_path(r"C:\Tools\notepad.exe"));
    }

    #[test]
    fn kill_rejects_identity_mismatch_without_terminating_target() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 > nul"])
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        let (creation, exe) = identity_of(pid);
        let wrong_creation = (creation.parse::<u64>().unwrap() + 1).to_string();

        let err = kill_process(pid, wrong_creation, exe).unwrap_err();
        assert!(err.contains("已变化"), "unexpected: {err}");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn stale_descendant_is_filtered_by_creation_time() {
        let child = ProcessSnapshotEntry {
            pid: 123,
            parent_pid: 456,
            creation_time: 100,
        };
        assert!(is_descendant_stale(&child, 456, 101));
        assert!(!is_descendant_stale(&child, 456, 100));
        assert!(!is_descendant_stale(&child, 789, 101));
    }

    #[test]
    fn snapshot_contains_self_with_creation_time() {
        let pid = std::process::id();
        let snapshot = build_process_snapshot().expect("snapshot");
        let entry = snapshot
            .iter()
            .find(|entry| entry.pid == pid)
            .expect("self");
        assert!(entry.creation_time > 0);
    }

    #[test]
    fn kill_tree_takes_out_child() {
        let mut parent = std::process::Command::new("cmd")
            .args([
                "/c",
                "start /b powershell -NoProfile -Command \"Start-Sleep 30\" & ping -n 60 127.0.0.1 > nul",
            ])
            .spawn()
            .expect("spawn parent");
        let parent_pid = parent.id();
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let (creation, exe) = identity_of(parent_pid);

        kill_process_tree(parent_pid, creation, exe).expect("kill_tree failed");

        let mut gone = false;
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if descendants_still_alive(parent_pid).is_none() {
                gone = true;
                break;
            }
        }
        let _ = parent.kill();
        let _ = parent.wait();
        assert!(gone, "子进程在 kill_tree 后仍存活——PID 偏移或树收集有误");
    }
}
