//! 句柄枚举扫描 —— 参照 Microsoft PowerToys File Locksmith 的实现原理：
//! 通过 NtQuerySystemInformation(SystemExtendedHandleInformation) 遍历全系统句柄表，
//! 复制目标进程的句柄并比对最终路径，找出 Restart Manager 可能漏报的占用进程
//! （例如 SYSTEM 进程持有的句柄、非标准共享模式的文件句柄等）。

use std::path::Path;

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, STATUS_INFO_LENGTH_MISMATCH,
};
use windows::Win32::Storage::FileSystem::{
    GetFinalPathNameByHandleW, GetFileType, FILE_NAME_NORMALIZED, FILE_TYPE_DISK,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
};

use crate::winutil::enable_debug_privilege;

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

/// 两阶段 NtQuerySystemInformation 调用：
/// 先小缓冲探测所需大小，再精确分配——避免旧实现直接清零 64MB 缓冲的浪费
/// （句柄表实测约 11MB，64MB 的 memset 与重复拷贝是主要开销之一）。
fn query_system_info(class: u32, max_len: u32) -> Option<Vec<u8>> {
    let mut len = 0x1000u32; // 4KB 探测
    let mut needed = 0u32;
    for _ in 0..8 {
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
            len = if needed > len { needed } else { len * 2 };
            if len > max_len {
                return None;
            }
            continue;
        }
        if status.0 != 0 {
            return None;
        }
        buf.truncate(needed as usize);
        return Some(buf);
    }
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
    let mut name_buf = [0u16; 1024];

    for i in 0..count {
        let entry = unsafe {
            *(buf.as_ptr().add(std::mem::size_of::<HandleInfoHeader>() + i * entry_size)
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
        let mut path_hit = false;
        if is_disk {
            let n = unsafe { GetFinalPathNameByHandleW(handle, &mut name_buf, FILE_NAME_NORMALIZED) };
            path_hit = n > 0 && (n as usize) <= name_buf.len();
        }
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
    scan(path)
}

/// 枚举全系统句柄，返回当前持有 `path` 的进程 PID 集合。
///
/// 比对规则：`\\?\` 前缀归一化 + 大小写不敏感的完整路径匹配。
/// File 对象类型索引通过"探针句柄"在**同一次句柄表快照**上测定——
/// 类型索引的数值不稳定（每次查询可能漂移），必须与扫描共用同一快照。
/// 失败时返回空集合（调用方仍有 Restart Manager 的结果兜底）。
pub fn scan(path: &Path) -> Vec<u32> {
    let target = normalize_path(path);
    collect_handle_paths_if(&|resolved: &str| resolved.to_lowercase() == target)
        .map(|map| map.into_keys().collect())
        .unwrap_or_default()
}

/// 目录模式批量扫描：一次句柄表遍历，找出持有 `dir` 下任意文件句柄的
/// 进程，返回 pid → 命中的句柄路径列表（调用方据此统计各进程锁定的文件数）。
///
/// 精度与逐文件 scan() 完全一致——同一次遍历解析出的完整路径做前缀匹配；
/// 只是把 N 次全表遍历合并为 1 次，N 个文件从 O(N×全表) 降为 O(1×全表)。
pub fn scan_directory(
    dir: &Path,
    on_progress: &(dyn Fn(usize, usize) + Sync),
    total_hint: usize,
) -> std::collections::HashMap<u32, Vec<String>> {
    let mut prefix = normalize_path(dir);
    if !prefix.ends_with('\\') {
        prefix.push('\\');
    }
    // 进度语义：句柄表遍历顺序与文件清单无关，用命中数近似上报并钳在
    // total_hint 内，结束时推满，保证进度条走完
    let reported = std::sync::atomic::AtomicUsize::new(0);
    let result = collect_handle_paths_if(&|resolved: &str| {
        let hit = resolved.to_lowercase().starts_with(&prefix);
        if hit {
            let r = reported.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if r % 20 == 0 {
                on_progress(r.min(total_hint), total_hint);
            }
        }
        hit
    });
    on_progress(total_hint, total_hint);
    result.unwrap_or_default()
}

/// 归一化：小写、去掉 `\\?\` 前缀
fn normalize_path(path: &Path) -> String {
    let mut s = path.to_string_lossy().to_lowercase();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        s = stripped.to_string();
    }
    s
}

/// 跨线程传递的进程句柄包装：HANDLE 仅按数值语义使用，
/// 复制到各工作线程是安全的
#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

/// 一次全系统句柄表遍历：解析每个 File 句柄的 DOS 路径（去掉 `\\?\`），
/// 对 `hit` 返回 true 的路径按 pid 收集。
///
/// 性能设计（精度不变的前提下提速）：
/// 1. 两阶段快照查询，只分配恰好大小的缓冲（省 64MB memset）
/// 2. 类型索引探测复用同一快照，自身句柄免复制
/// 3. 进程句柄按 pid 缓存：旧实现每个句柄 OpenProcess 一次，而一个进程
///    可能持数百个 File 句柄——现在每个 pid 只开一次
/// 4. 路径解析（DuplicateHandle + GetFinalPathNameByHandleW）多线程并行，
///    系统调用是主要开销，CPU 核越多收益越大
fn collect_handle_paths_if(
    hit: &(dyn Fn(&str) -> bool + Sync),
) -> Option<std::collections::HashMap<u32, Vec<String>>> {
    enable_debug_privilege();

    // 拉取一次句柄表快照，类型探测与匹配共用这份数据
    let buf = query_system_info(SYSTEM_EXTENDED_HANDLE_INFORMATION, 0x2000_0000)?;
    if buf.len() < std::mem::size_of::<HandleInfoHeader>() {
        return None;
    }
    let count = unsafe { (*(buf.as_ptr() as *const HandleInfoHeader)).number_of_handles };
    let entry_size = std::mem::size_of::<HandleEntry>();
    if std::mem::size_of::<HandleInfoHeader>() + count * entry_size > buf.len() {
        return None; // 数据在两次调用间变化，宁可放弃也不越界
    }

    // 在同一快照上测定 File 类型索引（能解析出磁盘路径的索引即 File）
    let file_index = file_type_index_in(&buf, count, entry_size)?;

    // 收集 File 类型候选句柄（一次纯内存遍历，微秒级）
    let mut candidates: Vec<(u32, usize, u32)> = Vec::new();
    for i in 0..count {
        let entry = unsafe {
            *(buf.as_ptr().add(std::mem::size_of::<HandleInfoHeader>() + i * entry_size)
                as *const HandleEntry)
        };
        let pid = entry.process_id as u32;
        if pid == 0 || entry.object_type_index != file_index || entry.granted_access == 0 {
            continue;
        }
        candidates.push((pid, entry.handle, entry.granted_access));
    }

    // 并行解析：进程句柄按 pid 缓存（Mutex），null 表示该 pid 打不开
    let results: std::sync::Mutex<std::collections::HashMap<u32, Vec<String>>> = Default::default();
    let proc_cache: std::sync::Mutex<std::collections::HashMap<u32, SendHandle>> =
        Default::default();
    let next = std::sync::atomic::AtomicUsize::new(0);

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                let mut name_buf = [0u16; 1024];
                loop {
                    let idx = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if idx >= candidates.len() {
                        break;
                    }
                    let (pid, handle, _granted) = candidates[idx];

                    let proc = {
                        let mut cache = proc_cache.lock().unwrap();
                        cache
                            .entry(pid)
                            .or_insert_with(|| SendHandle(
                                unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }
                                    .unwrap_or(HANDLE::default()),
                            ))
                            .0
                    };
                    if proc.is_invalid() {
                        continue; // 该 pid 打不开（已缓存失败结果）
                    }

                    let mut dup = HANDLE::default();
                    // GetCurrentProcess 返回伪句柄，线程内直接调用零开销
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
                        continue;
                    }

                    // 只解析磁盘文件句柄，跳过命名管道等可能阻塞的对象类型
                    if unsafe { GetFileType(dup) } == FILE_TYPE_DISK {
                        let n = unsafe {
                            // FILE_NAME_NORMALIZED(0) 与 VOLUME_NAME_DOS(0) 都是 0，
                            // 等价于默认值组合
                            GetFinalPathNameByHandleW(dup, &mut name_buf, FILE_NAME_NORMALIZED)
                        };
                        if n > 0 && (n as usize) <= name_buf.len() {
                            let mut resolved = String::from_utf16_lossy(&name_buf[..n as usize]);
                            if let Some(stripped) = resolved.strip_prefix(r"\\?\") {
                                resolved = stripped.to_string();
                            }
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
                }
            });
        }
    });

    // 释放缓存的进程句柄
    for SendHandle(proc) in proc_cache.into_inner().unwrap().into_values() {
        if !proc.is_invalid() {
            unsafe {
                let _ = CloseHandle(proc);
            }
        }
    }

    Some(results.into_inner().unwrap())
}
