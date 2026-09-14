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

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_MORE_DATA, ERROR_SUCCESS, STILL_ACTIVE, WAIT_TIMEOUT, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows::Win32::System::RestartManager::{
    RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
    RM_PROCESS_INFO,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, TerminateProcess, WaitForSingleObject,
    PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

use crate::handle_scan;
use crate::winutil::{file_description, process_image_full, to_wide, utf16_string, win32_err};

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
    on_progress: &(dyn Fn(usize, usize) + Sync),
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

/// 文件夹模式：递归枚举目录内文件，检测每个文件的占用者并按 PID 聚合。
///
/// 句柄扫描对"查询目录本身"无效（锁的是内部文件，路径不等），
/// 所以必须展开到文件粒度。为控制耗时限制最大扫描文件数。
fn scan_directory(
    dir: &Path,
    on_progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<ScanOutcome, String> {
    const MAX_FILES: usize = 2000;

    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue; // 无权限的子目录跳过
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
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
    let truncated = total >= MAX_FILES;

    // 性能关键路径：一次句柄表遍历解析出全部文件句柄路径，
    // 与目录前缀做匹配——等价于逐文件 scan() 但从 O(N×全表) 降为 O(1×全表)。
    // RM 引擎对目录模式同样批量化：一次会话注册全部文件资源。
    let mut merged: HashMap<u32, ProcessInfo> = HashMap::new();

    if let Ok(rm_list) = batch_query_restart_manager(&files) {
        for (pid, app_name) in rm_list {
            let exe_path = process_image_full(pid).unwrap_or_default();
            let description = file_description(&exe_path).unwrap_or_default();
            let process_name = process_name_of(&exe_path, &app_name);
            merged.insert(
                pid,
                ProcessInfo {
                    pid,
                    process_name,
                    exe_path,
                    description,
                    app_name,
                    source: "restart_manager".into(),
                    // RM 不提供逐文件计数；至少记 1，防止 RM 独有的检出被误丢弃
                    locked_files: 1,
                },
            );
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
                truncated,
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
                let exe_path = process_image_full(pid).unwrap_or_default();
                let description = file_description(&exe_path).unwrap_or_default();
                let process_name = process_name_of(&exe_path, "");
                e.insert(ProcessInfo {
                    pid,
                    process_name,
                    exe_path,
                    description,
                    app_name: String::new(),
                    source: "handle_scan".into(),
                    locked_files: count,
                });
            }
        }
    }

    if merged.is_empty() && !files.is_empty() {
        // 句柄扫描正常完成但无命中 = 目录下无锁定文件，这是可信结果
        return Ok(ScanOutcome {
            processes: Vec::new(),
            truncated,
            file_count: total,
        });
    }

    let mut list: Vec<ProcessInfo> = merged.into_values().collect();
    list.sort_by_key(|p| p.pid);
    Ok(ScanOutcome {
        processes: list,
        truncated,
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
        let err = unsafe {
            RmRegisterResources(handle, Some(&pcsz), None, None)
        };
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
                    if buf.is_empty() { None } else { Some(buf.as_mut_ptr()) },
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
        let exe_path = process_image_full(pid).unwrap_or_default();
        let description = file_description(&exe_path).unwrap_or_default();
        let process_name = process_name_of(&exe_path, &app_name);
        merged.insert(
            pid,
            ProcessInfo {
                pid,
                process_name,
                exe_path,
                description,
                app_name,
                source: "restart_manager".into(),
                locked_files: 1,
            },
        );
    }

    for pid in hm_pids {
        match merged.entry(pid) {
            // RM 也报了该 PID：标记双引擎命中
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.get_mut().source = "both".into();
            }
            // 仅句柄扫描发现（RM 不可用或漏报）
            std::collections::hash_map::Entry::Vacant(e) => {
                let exe_path = process_image_full(pid).unwrap_or_default();
                let description = file_description(&exe_path).unwrap_or_default();
                let process_name = process_name_of(&exe_path, "");
                e.insert(ProcessInfo {
                    pid,
                    process_name,
                    exe_path,
                    description,
                    app_name: String::new(),
                    source: "handle_scan".into(),
                    locked_files: 1,
                });
            }
        }
    }

    // 结果判定：
    // - 句柄扫描引擎失效（Err）且 RM 无结果 → 整体报错，不得伪装"无占用"
    // - 句柄扫描正常（Ok 空）→ 结果可信
    // - RM 失败但句柄扫描正常（部分系统 RM 服务异常）→ 句柄扫描独立兜底
    if merged.is_empty() {
        if let Err(e) = hm_result {
            let rm_failed = rm_result.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
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

/// 强制结束单个进程（TerminateProcess）。
///
/// 以管理员运行时可结束绝大多数进程；普通权限下对系统/提权进程会失败。
/// 发出终止请求后等待最多 3 秒确认进程真正退出；
/// "进程不存在" 视为已结束（目标早已退出是合法的成功场景）。
pub fn kill_process(pid: u32) -> Result<(), String> {
    log::info!("[结束进程] pid={pid}");
    if pid == 0 {
        return Err("无效的进程 ID".into());
    }
    // System 空闲进程等系统关键进程不可终止，提前拦截给出可读错误
    if pid == 4 {
        return Err("无法结束 System 进程（PID 4）".into());
    }

    // TERMINATE：终止请求；SYNCHRONIZE：WaitForSingleObject 确认退出；
    // QUERY_LIMITED_INFORMATION：读取退出码判断进程是否已不存在
    let access = PROCESS_TERMINATE
        | PROCESS_ACCESS_RIGHTS(SYNCHRONIZE.0)
        | PROCESS_QUERY_LIMITED_INFORMATION;
    let handle = unsafe { OpenProcess(access, false, pid) }.map_err(|e| {
        let msg = win32_err(&e);
        if msg.contains("拒绝访问") {
            format!("打开进程 {pid} 失败：{msg}；请确认程序以管理员身份运行")
        } else {
            format!("打开进程 {pid} 失败：{msg}")
        }
    })?;

    // 目标可能早已退出（PID 残留时 OpenProcess 仍成功）：先查退出码，
    // 已退出的进程视为"已结束"，避免对残留 PID 误报"拒绝访问"
    let mut exit_code = 0u32;
    if unsafe { GetExitCodeProcess(handle, &mut exit_code) }.is_ok()
        && exit_code != STILL_ACTIVE.0 as u32
    {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Ok(());
    }

    let result = unsafe { TerminateProcess(handle, 1) };
    if let Err(e) = result {
        let code = crate::winutil::win32_code(&e);
        unsafe {
            let _ = CloseHandle(handle);
        }
        // 1168 (ERROR_NOT_FOUND)：进程不存在，视为已结束
        if code == 1168 {
            return Ok(());
        }
        return Err(format!("结束进程 {pid} 失败：{}", win32_err(&e)));
    }

    // 等待最多 3 秒确认进程真正退出（TerminateProcess 是异步请求）
    let mut exit_code = 0u32;
    let mut exited = false;
    for _ in 0..30 {
        let wait = unsafe { WaitForSingleObject(handle, 100) };
        if wait != WAIT_TIMEOUT {
            exited = true;
            break;
        }
        // 句柄无 SYNCHRONIZE 时等待可能立即返回失败，用退出码兜底判断
        if unsafe { GetExitCodeProcess(handle, &mut exit_code) }.is_ok()
            && exit_code != STILL_ACTIVE.0 as u32
        {
            exited = true;
            break;
        }
    }
    unsafe {
        let _ = CloseHandle(handle);
    }

    if exited {
        Ok(())
    } else {
        Err(format!(
            "结束进程 {pid} 的请求已发出，但 3 秒内未确认退出；进程可能受系统保护"
        ))
    }
}

/// 拉取全系统进程快照，构建 parent → children 映射。
fn build_parent_map() -> Result<std::collections::HashMap<u32, Vec<u32>>, String> {
    const SYSTEM_PROCESS_INFORMATION: u32 = 5;

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

    // x64 SYSTEM_PROCESS_INFORMATION 关键字段偏移（Windows 内核布局）：
    // NextEntryOffset(0) → ... → ImageName(56, UNICODE_STRING 16B) →
    // BasePriority(72) → UniqueProcessId(80) → InheritedFromUniqueProcessId(88)
    // → SessionId(96)。PID 读取点必须落在 80，此前误用 64 会读到
    // ImageName.Buffer 指针的随机低位，导致进程树收集到垃圾 PID。
    const OFF_NEXT: usize = 0;
    const OFF_PID: usize = 80;
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
    Ok(parents)
}

/// 判断快照中是否仍有 parent 链指向 `ancestor_pid` 的存活进程
/// （仅用于测试：验证进程树终止效果）
#[cfg(test)]
fn descendants_still_alive(ancestor_pid: u32) -> Option<Vec<u32>> {
    let parents = build_parent_map().ok()?;
    let mut seen = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::from(vec![ancestor_pid]);
    let mut alive = Vec::new();
    while let Some(cur) = queue.pop_front() {
        if let Some(children) = parents.get(&cur) {
            for &c in children {
                if seen.insert(c) {
                    queue.push_back(c);
                    // PID 仍出现在快照中即视为存活（终止后的 PID 会从快照消失）
                    alive.push(c);
                }
            }
        }
    }
    if alive.is_empty() { None } else { Some(alive) }
}

/// 结束进程树：先结束子进程再结束父进程（参照 LockHunter / taskkill /T）。
///
/// 通过 NtQuerySystemInformation 的进程快照以 ParentProcessId 归组，
/// 递归收集指定 pid 的全部后代后逐个 TerminateProcess。
pub fn kill_process_tree(pid: u32) -> Result<(), String> {
    log::info!("[结束进程树] 根 pid={pid}");
    if pid == 0 || pid == 4 {
        return Err(format!("无效的进程树根 PID：{pid}"));
    }
    let parents = build_parent_map()?;

    // BFS 收集后代（含自身）；过滤会造成自伤的目标
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

    // 防自伤：绝不结束自己；explorer.exe 是 shell，误杀会让桌面崩溃重建
    let self_pid = std::process::id();
    let to_kill: Vec<u32> = to_kill
        .into_iter()
        .filter(|&p| p != 4 && p != self_pid)
        .collect();
    let mut explorer_killed = false;
    let to_kill: Vec<u32> = to_kill
        .into_iter()
        .filter(|&p| {
            if let Some(exe) = process_image_full(p) {
                // exe 是完整路径（如 C:\Windows\explorer.exe），必须取文件名再比较，
                // 否则该防护永远命中不了
                let name = exe
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                if name == "explorer.exe" {
                    explorer_killed = true;
                    return false; // 跳过 shell 进程
                }
            }
            true
        })
        .collect();

    if to_kill.is_empty() {
        return Err("没有可结束的进程（目标包含本程序或系统 Shell，已拦截）".into());
    }

    let mut errors = Vec::new();
    let mut killed = 0usize;
    for &p in to_kill.iter().rev() {
        // 先结束最深的后代
        match kill_process(p) {
            Ok(()) => killed += 1,
            Err(e) => errors.push(format!("{p}: {e}")),
        }
    }
    if killed == 0 && !to_kill.is_empty() {
        return Err(format!("结束进程树全部失败：{}", errors.join("; ")));
    }
    let _ = explorer_killed; // explorer 被跳过的事实随 Ok 静默返回，无需提示
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_rejects_zero_pid() {
        assert!(kill_process(0).is_err());
    }

    #[test]
    fn kill_rejects_system_process() {
        let err = kill_process(4).unwrap_err();
        assert!(err.contains("System"), "unexpected: {err}");
    }

    #[test]
    fn kill_nonexistent_process_is_success() {
        // 启动一个几乎立即退出的短命进程（Windows 会回收 PID），
        // 等待退出后其 PID 视为"进程不存在"，kill 应返回成功而非报错。
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit 0"])
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let _ = child.wait();

        // 稍等让系统完成 PID 清理
        std::thread::sleep(std::time::Duration::from_millis(200));
        match kill_process(pid) {
            Ok(()) => {}
            Err(e) => panic!("pid {pid} kill failed: {e}"),
        }
    }

    #[test]
    fn kill_tree_takes_out_child() {
        // 父进程（cmd）持有子进程（powershell 挂起数秒）：
        // kill_tree(父) 后轮询断言子进程也退出了——这同时验证
        // 进程快照的 PID 偏移解析正确（偏移错则收集不到子进程）。
        let mut parent = std::process::Command::new("cmd")
            .args([
                "/c",
                "start /b powershell -NoProfile -Command \"Start-Sleep 30\" & ping -n 60 127.0.0.1 > nul",
            ])
            .spawn()
            .expect("spawn parent");
        let parent_pid = parent.id();
        std::thread::sleep(std::time::Duration::from_millis(1200));

        kill_process_tree(parent_pid).expect("kill_tree failed");

        // 找到那个 powershell 子进程：通过再次枚举快照验证它已不在
        let mut gone = false;
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if descendants_still_alive(parent_pid).is_none() {
                gone = true;
                break;
            }
        }
        let _ = parent.kill();
        assert!(gone, "子进程在 kill_tree 后仍存活——PID 偏移或树收集有误");
    }
}
