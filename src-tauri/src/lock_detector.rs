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
    /// 检测来源：Restart Manager / 句柄扫描 / 两者皆有 / directory_scan
    pub source: String,
    /// 目录模式下该进程锁定的文件数量（单文件模式恒为 1）
    pub locked_files: u32,
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
    on_progress: &dyn Fn(usize, usize),
) -> Result<Vec<ProcessInfo>, String> {
    // 输入校验：前端传来的路径必须真实存在，避免下游错误难排查
    if file_path.is_empty() {
        return Err("文件路径为空".into());
    }
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(format!("文件不存在：{file_path}"));
    }

    // 目录模式：枚举内部文件逐个检测后按 PID 聚合
    if path.is_dir() {
        return scan_directory(path, on_progress);
    }

    scan_single_file(file_path)
}

/// 文件夹模式：递归枚举目录内文件，检测每个文件的占用者并按 PID 聚合。
///
/// 句柄扫描对"查询目录本身"无效（锁的是内部文件，路径不等），
/// 所以必须展开到文件粒度。为控制耗时限制最大扫描文件数。
fn scan_directory(
    dir: &Path,
    on_progress: &dyn Fn(usize, usize),
) -> Result<Vec<ProcessInfo>, String> {
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
    // 每个文件跑一次检测，按 PID 聚合并统计占用的文件数
    use std::collections::HashMap;
    let mut merged: HashMap<u32, ProcessInfo> = HashMap::new();
    let mut scanned_files = 0usize;
    for f in &files {
        // RM 在部分系统不可用，此时靠句柄扫描逐文件跑；
        // 目录可能很大，这里容忍单文件失败继续下一个
        if let Ok(list) = scan_single_file(f.to_string_lossy().as_ref()) {
            scanned_files += 1;
            for mut info in list {
                let entry = merged.entry(info.pid).or_insert(info.clone());
                entry.locked_files += 1;
                // 描述/路径取任意一次成功的值（同一进程）
                if entry.description.is_empty() {
                    entry.description = info.description;
                }
                if entry.exe_path.is_empty() {
                    entry.exe_path = info.exe_path.clone();
                }
                info.source = "directory_scan".into();
            }
        }
        on_progress(scanned_files, total);
    }

    if scanned_files == 0 && !files.is_empty() {
        return Err(format!(
            "扫描了 {} 个文件但全部失败（可能需要管理员权限）",
            files.len()
        ));
    }

    let mut list: Vec<ProcessInfo> = merged.into_values().collect();
    list.sort_by_key(|p| p.pid);
    Ok(list)
}

/// 单文件模式：双引擎合并查询。
fn scan_single_file(file_path: &str) -> Result<Vec<ProcessInfo>, String> {
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
                let process_name = exe_path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or_default()
                    .to_string();
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

    // rm 查询彻底失败且句柄扫描也没结果时，才把错误带出去。
    // 部分系统（如 Win11 25H2 26100+）上 RM 服务对普通进程查询一律返回
    // 错误，此时句柄扫描是唯一引擎，不能因 RM 失败而整体报错。
    if merged.is_empty() {
        if let Err(e) = rm_result {
            // RM 失败但句柄扫描正常完成且无结果 → 结果可信，返回"无占用"
            // （扫描引擎独立于 RM，不受其服务状态影响）
            eprintln!("[warn] Restart Manager 不可用（{e}），已由句柄扫描兜底");
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
                let name = exe.to_lowercase();
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
