//! 进程终止：身份校验、PID 复用防护、进程树收集与终止。
//!
//! 安全设计：
//! - PID 会在进程退出后被复用，因此不能只凭 PID 操作。创建时间和 exe
//!   路径必须与扫描结果一致才允许终止
//! - 关键系统进程和本程序始终拒绝结束
//! - 进程句柄 RAII 包装，所有错误路径都能释放句柄
//! - 进程树终止时每个后代都用快照中的创建时间二次校验，避免误杀

use std::collections::{HashMap, VecDeque};

use windows::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE, WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_ACCESS_RIGHTS,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

use crate::winutil::{
    normalize_path_string, process_creation_time_from_handle, process_image_from_handle, win32_err,
};

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

/// 从 exe 完整路径提取进程名；exe 路径为空时回退到 fallback。
/// 注意："".rsplit().next() 是 Some("") 而非 None，必须显式过滤空串。
fn process_name_of(exe_path: &str, fallback: &str) -> String {
    exe_path
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
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
    let mut parents: HashMap<u32, Vec<u32>> = HashMap::new();
    for entry in snapshot {
        parents.entry(entry.parent_pid).or_default().push(entry.pid);
    }

    let mut seen = std::collections::HashSet::new();
    let mut queue = VecDeque::from(vec![ancestor_pid]);
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

    let mut children: HashMap<u32, Vec<ProcessSnapshotEntry>> = HashMap::new();
    for entry in &snapshot {
        children.entry(entry.parent_pid).or_default().push(*entry);
    }

    let mut queue = VecDeque::from([(pid, expected_creation_time)]);
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
    use crate::winutil::{process_creation_time, process_image_full};

    #[test]
    fn parse_creation_time_rejects_garbage() {
        assert!(parse_creation_time("133901234567890000").is_ok());
        assert!(parse_creation_time("not-a-time").is_err());
        assert!(parse_creation_time("").is_err());
        assert!(parse_creation_time("-1").is_err());
    }

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
