//! 句柄枚举扫描 —— 参照 Microsoft PowerToys File Locksmith 的实现原理：
//! 通过 NtQuerySystemInformation(SystemExtendedHandleInformation) 遍历全系统句柄表，
//! 复制目标进程的句柄并比对最终路径，找出 Restart Manager 可能漏报的占用进程
//! （例如 SYSTEM 进程持有的句柄、非标准共享模式的文件句柄等）。

use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
    STATUS_INFO_LENGTH_MISMATCH,
};
use windows::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_DISK};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

use crate::winutil::{enable_debug_privilege, final_path_from_handle};

pub type HitPredicate = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type ProgressCallback = Arc<dyn Fn(usize, usize) + Send + Sync>;

/// SystemExtendedHandleInformation 的信息类别码
const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;
/// SystemObjectTypeInformation 的信息类别码
#[allow(dead_code)]
const SYSTEM_OBJECT_TYPE_INFORMATION: u32 = 3;

/// SYSTEM_HANDLE_INFORMATION_EX 头部（x64）
#[repr(C)]
struct HandleInfoHeader {
    number_of_handles: usize,
    _reserved: usize,
}

/// SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX（x64，40 字节）
#[repr(C)]
#[derive(Clone, Copy)]
struct HandleEntry {
    object: usize,
    process_id: usize,
    handle: usize,
    granted_access: u32,
    creator_back_trace_index: u16,
    object_type_index: u16,
    handle_attributes: u32,
    _reserved: u32,
}

/// 稳健的两阶段 NtQuerySystemInformation 调用：
/// 先小缓冲探测所需大小，内核返回可靠大小时一次到位；
/// 部分系统 ret 不可靠（返回 0），此时按 4 倍增长重试并记录日志。
/// 重试上限 12 次（64KB×4^11 远超 max_len，由 max_len 截断），
/// 保证 11MB 级句柄表在 ret=0 的环境下也能在数次内命中。
fn query_system_info(class: u32, max_len: u32) -> Option<Vec<u8>> {
    let mut len = 0x1_0000u32; // 64KB 起步
    let mut needed = 0u32;
    for attempt in 0..12 {
        let mut buf = vec![0u8; len as usize];
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_INFORMATION_CLASS(class as i32),
                buf.as_mut_ptr().cast(),
                len,
                &mut needed,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            let next = if needed > len { needed } else { len * 4 };
            if next > max_len {
                log::error!("[句柄扫描] 快照查询所需缓冲 {next} 字节超过上限 {max_len}");
                return None;
            }
            log::debug!(
                "[句柄扫描] 快照查询第 {attempt} 次缓冲不足 (needed={needed}, {len}→{next})"
            );
            len = next;
            continue;
        }
        if status.0 != 0 {
            log::error!("[句柄扫描] 快照查询失败 (NTSTATUS 0x{:08x})", status.0);
            return None;
        }
        buf.truncate(needed as usize);
        return Some(buf);
    }
    log::error!("[句柄扫描] 快照查询重试耗尽 (最终 len={len})");
    None
}

/// 在给定句柄表快照上探测 File 对象类型索引：
/// 遍历本进程的全部句柄条目，逐个复制并尝试解析路径——
/// 能通过 GetFinalPathNameByHandleW 解析出磁盘路径的句柄必是 File 对象，
/// 其类型索引即为所求。不依赖任何内核结构体布局，对所有 Windows 版本可靠。
///
/// 性能要点：自身进程的句柄无需 OpenProcess/DuplicateHandle——
/// 条目里的 handle 值就是本进程可直接使用的句柄，直接查询即可。
fn file_type_index_in(buf: &[u8], count: usize, entry_size: usize) -> Option<u16> {
    let my_pid = std::process::id() as usize;
    let mut candidates: std::collections::HashMap<u16, usize> = std::collections::HashMap::new();

    for i in 0..count {
        let entry = unsafe {
            *(buf
                .as_ptr()
                .add(std::mem::size_of::<HandleInfoHeader>() + i * entry_size)
                as *const HandleEntry)
        };
        if entry.process_id != my_pid {
            continue;
        }
        if entry.object_type_index == 0 || entry.granted_access == 0 {
            continue;
        }
        // 自身句柄直接解析，零复制开销
        let handle = HANDLE(entry.handle as *mut core::ffi::c_void);
        let is_disk = unsafe { GetFileType(handle) } == FILE_TYPE_DISK;
        // 能解析出磁盘路径的句柄必是 File 对象；路径解析缓冲按返回值增长
        let path_hit = is_disk && final_path_from_handle(handle).is_some();
        if path_hit {
            *candidates.entry(entry.object_type_index).or_insert(0) += 1;
        }
    }

    // 出现次数最多的候选索引即 File 类型（非 File 句柄不会解析出磁盘路径）
    candidates
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(idx, _)| idx)
}

/// 供性能基准示例调用（内部函数的透传包装）
#[doc(hidden)]
#[cfg(feature = "e2e")]
pub fn bench_scan(path: &std::path::Path) -> Vec<u32> {
    scan(path).unwrap_or_default()
}

/// 枚举全系统句柄，返回当前持有 `path` 的进程 PID 集合。
///
/// 比对规则：`\\?\` 前缀归一化 + 大小写不敏感的完整路径匹配。
/// File 对象类型索引通过"探针句柄"在**同一次句柄表快照**上测定——
/// 类型索引的数值不稳定（每次查询可能漂移），必须与扫描共用同一快照。
///
/// 扫描引擎本身失效（快照查询失败等）返回 Err，与"无占用"的空结果
/// 明确区分——上层不得把失效伪装成"文件未被占用"。
pub fn scan(path: &Path) -> Result<Vec<u32>, String> {
    let target = normalize_path(path);
    let hit: HitPredicate = Arc::new(move |resolved: &str| resolved.to_lowercase() == target);
    let map = collect_handle_paths_if(hit, None)?;
    Ok(map.into_keys().collect())
}

/// 目录模式批量扫描：一次句柄表遍历，找出持有 `dir` 下任意文件句柄的
/// 进程，返回 pid → 命中的句柄路径列表（调用方据此统计各进程锁定的文件数）。
///
/// 精度与逐文件 scan() 完全一致——同一次遍历解析出的完整路径做前缀匹配；
/// 只是把 N 次全表遍历合并为 1 次，N 个文件从 O(N×全表) 降为 O(1×全表)。
/// `on_progress` 按"已解析候选句柄数 / 候选总数"上报：与文件清单无对应
/// 关系，但单调递增、粒度均匀，进度条走势平滑。
pub fn scan_directory(
    dir: &Path,
    on_progress: ProgressCallback,
) -> Result<std::collections::HashMap<u32, Vec<String>>, String> {
    let mut prefix = normalize_path(dir);
    if !prefix.ends_with('\\') {
        prefix.push('\\');
    }
    let hit: HitPredicate =
        Arc::new(move |resolved: &str| resolved.to_lowercase().starts_with(&prefix));
    collect_handle_paths_if(hit, Some(on_progress))
}

/// 归一化：剥离 `\\?\` 设备前缀（UNC 还原为 `\\server\share`）后小写。
/// GetFinalPathNameByHandleW 对 UNC 返回 `\\?\UNC\server\share\...`，
/// 若只剥前缀会得到 `unc\server\share`，与前端传入的 `\\server\share`
/// 永不相等——网络共享文件会恒漏报。
fn normalize_path(path: &Path) -> String {
    crate::winutil::strip_device_prefix(&path.to_string_lossy()).to_lowercase()
}

/// 跨线程传递的进程句柄包装：HANDLE 仅按数值语义使用，
/// 复制到各工作线程是安全的
#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

/// 一次全系统句柄表遍历：解析每个 File 句柄的 DOS 路径（剥离 `\\?\` 设备前缀，
/// UNC 还原为 `\\server\share`），对 `hit` 返回 true 的路径按 pid 收集。
/// `progress` 非空时按"已处理候选数 / 候选总数"周期性上报。
///
/// 性能设计（精度不变的前提下提速）：
/// 1. 两阶段快照查询，只分配恰好大小的缓冲（省 64MB memset）
/// 2. 类型索引探测复用同一快照，自身句柄免复制
/// 3. 进程句柄按 pid 缓存：旧实现每个句柄 OpenProcess 一次，而一个进程
///    可能持数百个 File 句柄——现在每个 pid 只开一次
/// 4. 路径解析（DuplicateHandle + GetFinalPathNameByHandleW）多线程并行，
///    系统调用是主要开销，CPU 核越多收益越大
fn collect_handle_paths_if(
    hit: HitPredicate,
    progress: Option<ProgressCallback>,
) -> Result<std::collections::HashMap<u32, Vec<String>>, String> {
    enable_debug_privilege();

    // 拉取一次句柄表快照，类型探测与匹配共用这份数据
    let buf = query_system_info(SYSTEM_EXTENDED_HANDLE_INFORMATION, 0x2000_0000)
        .ok_or_else(|| "系统句柄表快照查询失败（重试耗尽）".to_string())?;
    if buf.len() < std::mem::size_of::<HandleInfoHeader>() {
        return Err("句柄表快照数据异常（过短）".into());
    }
    let count = unsafe { (*(buf.as_ptr() as *const HandleInfoHeader)).number_of_handles };
    let entry_size = std::mem::size_of::<HandleEntry>();
    if std::mem::size_of::<HandleInfoHeader>() + count * entry_size > buf.len() {
        return Err("句柄表在两次调用间变化，快照不一致".into());
    }

    // 在同一快照上测定 File 类型索引（能解析出磁盘路径的索引即 File）
    let file_index = file_type_index_in(&buf, count, entry_size)
        .ok_or_else(|| "无法测定文件句柄类型索引（句柄表快照不完整）".to_string())?;

    // 收集 File 类型候选句柄（一次纯内存遍历，微秒级）
    let mut candidate_vec: Vec<(u32, usize, u32)> = Vec::new();
    for i in 0..count {
        let entry = unsafe {
            *(buf
                .as_ptr()
                .add(std::mem::size_of::<HandleInfoHeader>() + i * entry_size)
                as *const HandleEntry)
        };
        let pid = entry.process_id as u32;
        if pid == 0 || entry.object_type_index != file_index || entry.granted_access == 0 {
            continue;
        }
        candidate_vec.push((pid, entry.handle, entry.granted_access));
    }
    let candidates = Arc::new(candidate_vec);

    // 并行解析：采用 PowerToys 同源的“工作线程 + watchdog”策略。
    // NtQuerySystemInformation 快照是只读数据；真正可能卡死的是
    // DuplicateHandle 后的 GetFileType / GetFinalPathNameByHandleW。
    // 我们为每个候选句柄创建独立 worker，主线程持续监控进度，
    // 超时后跳过该候选并重建 worker，而不是让一个坏句柄毁掉整次扫描。
    let results: Arc<Mutex<std::collections::HashMap<u32, Vec<String>>>> = Default::default();
    let proc_cache: Arc<Mutex<std::collections::HashMap<u32, SendHandle>>> = Default::default();
    let done = Arc::new(AtomicUsize::new(0));

    const WATCHDOG_MS: u64 = 1000; // PowerToys 用 200ms；这里放宽减少误判
    const PROGRESS_EVERY: usize = 256;

    let parallelism = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);
    let total = candidates.len();

    std::thread::scope(|scope| {
        // 多个独立 slot 并行消费；每个 slot 遇到卡死就终止并重建
        let mut slot_handles = Vec::with_capacity(parallelism);
        for slot in 0..parallelism {
            let hit = hit.clone();
            let progress = progress.clone();
            let candidates = candidates.clone();
            let results = results.clone();
            let proc_cache = proc_cache.clone();
            let done = done.clone();
            let counter = Arc::new(AtomicUsize::new(slot));

            slot_handles.push(scope.spawn(move || {
                loop {
                    let idx = counter.fetch_add(parallelism, Ordering::Relaxed);
                    if idx >= total {
                        break;
                    }
                    let (pid, handle, _granted) = candidates[idx];

                    // 每个候选在独立线程执行，watchdog 观察进度
                    let worker = {
                        let hit = hit.clone();
                        let results = results.clone();
                        let proc_cache = proc_cache.clone();
                        std::thread::spawn(move || {
                            // OpenProcess/DuplicateHandle 本身通常快速完成；
                            // 若真的卡死，整个候选 worker 会被 watchdog 跳过。
                            let proc = {
                                let mut cache = proc_cache.lock().unwrap();
                                cache
                                    .entry(pid)
                                    .or_insert_with(|| {
                                        SendHandle(
                                            unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }
                                                .unwrap_or(INVALID_HANDLE_VALUE),
                                        )
                                    })
                                    .0
                            };
                            if proc.is_invalid() {
                                return;
                            }

                            let mut dup = HANDLE::default();
                            let me = unsafe { GetCurrentProcess() };
                            let ok = unsafe {
                                DuplicateHandle(
                                    proc,
                                    HANDLE(handle as _),
                                    me,
                                    &mut dup,
                                    0,
                                    false,
                                    DUPLICATE_SAME_ACCESS,
                                )
                            }
                            .is_ok();
                            if !ok {
                                return;
                            }

                            // 只解析磁盘文件句柄，跳过命名管道等可能阻塞的对象类型
                            if unsafe { GetFileType(dup) } == FILE_TYPE_DISK {
                                if let Some(resolved) = final_path_from_handle(dup) {
                                    if hit(&resolved) {
                                        results
                                            .lock()
                                            .unwrap()
                                            .entry(pid)
                                            .or_default()
                                            .push(resolved);
                                    }
                                }
                            }
                            unsafe {
                                let _ = CloseHandle(dup);
                            }
                        })
                    };

                    // watchdog：候选级保护。超时则跳过该候选；worker 卡在系统调用时
                    // 不 join，让内核在其解除阻塞后自然退出并释放资源。
                    let deadline = Instant::now() + Duration::from_millis(WATCHDOG_MS);
                    while !worker.is_finished() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if worker.is_finished() {
                        let _ = worker.join();
                    } else {
                        log::warn!(
                            "[句柄扫描] 候选句柄解析超时（pid={pid}, handle={handle:#x}），已跳过"
                        );
                        std::mem::forget(worker);
                    }

                    let processed = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if let Some(cb) = &progress {
                        if processed % PROGRESS_EVERY == 0 || processed == total {
                            cb(processed, total);
                        }
                    }
                }
            }));
        }

        for h in slot_handles {
            let _ = h.join();
        }
    });

    if let Some(cb) = &progress {
        cb(total, total); // 收尾推满，保证进度条走完
    }

    // 释放缓存的进程句柄
    for (_pid, SendHandle(proc)) in proc_cache.lock().unwrap().drain() {
        if !proc.is_invalid() {
            unsafe {
                let _ = CloseHandle(proc);
            }
        }
    }

    // 忽略被 watchdog 跳过但尚未退出的 worker 持有的引用，
    // 返回当前已收集到的部分结果。
    let map = results.lock().unwrap().clone();
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_device_prefix() {
        assert_eq!(
            normalize_path(std::path::Path::new(r"\\?\C:\Data\F.TXT")),
            r"c:\data\f.txt"
        );
    }

    #[test]
    fn normalize_restores_unc_prefix() {
        // GetFinalPathNameByHandleW 对 UNC 返回 \\?\UNC\server\share，
        // 必须还原为 \\server\share，否则网络共享文件恒漏报
        assert_eq!(
            normalize_path(std::path::Path::new(r"\\?\UNC\NAS\Docs\f.txt")),
            r"\\nas\docs\f.txt"
        );
    }

    #[test]
    fn normalize_keeps_plain_unc_and_local() {
        assert_eq!(
            normalize_path(std::path::Path::new(r"\\NAS\Docs\f.txt")),
            r"\\nas\docs\f.txt"
        );
        assert_eq!(
            normalize_path(std::path::Path::new(r"C:\Data\f.txt")),
            r"c:\data\f.txt"
        );
    }
}
