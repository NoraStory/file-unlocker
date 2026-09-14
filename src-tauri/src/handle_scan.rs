//! 句柄枚举扫描 —— 参照 Microsoft PowerToys File Locksmith 的实现原理：
//! 通过 NtQuerySystemInformation(SystemExtendedHandleInformation) 遍历全系统句柄表，
//! 复制目标进程的句柄并比对最终路径，找出 Restart Manager 可能漏报的占用进程
//! （例如 SYSTEM 进程持有的句柄、非标准共享模式的文件句柄等）。

use std::collections::HashSet;
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

/// UNICODE_STRING（x64）
#[repr(C)]
struct UnicodeString {
    length: u16,          // 字节数
    maximum_length: u16,
    _pad: u32,
    buffer: *const u16,
}

/// 带缓冲区自动增长的 NtQuerySystemInformation 调用
fn query_system_info(class: u32, initial_len: u32, max_len: u32) -> Option<Vec<u8>> {
    let mut len = initial_len;
    for _ in 0..16 {
        let mut buf = vec![0u8; len as usize];
        let mut ret = 0u32;
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_INFORMATION_CLASS(class as i32),
                buf.as_mut_ptr().cast(),
                len,
                &mut ret,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            // 句柄表在两次调用之间可能变化，ret 有时为 0，则翻倍重试
            len = if ret > len { ret } else { len * 2 };
            if len > max_len {
                return None;
            }
            continue;
        }
        if status.0 == 0 {
            buf.truncate(ret as usize);
            return Some(buf);
        }
        return None;
    }
    None
}

/// 供诊断示例调用（内部函数的透传包装）
#[doc(hidden)]
#[allow(dead_code)]
pub fn dbg_query(class: u32, initial_len: u32, max_len: u32) -> Option<Vec<u8>> {
    query_system_info(class, initial_len, max_len)
}

/// 在给定句柄表快照上探测 File 对象类型索引：
/// 遍历本进程的全部句柄条目，逐个复制并尝试解析路径——
/// 能通过 GetFinalPathNameByHandleW 解析出磁盘路径的句柄必是 File 对象，
/// 其类型索引即为所求。不依赖任何内核结构体布局，对所有 Windows 版本可靠。
fn file_type_index_in(buf: &[u8], count: usize, entry_size: usize) -> Option<u16> {
    let my_pid = std::process::id() as usize;
    // 记录每个候选索引命中的次数
    let mut candidates: std::collections::HashMap<u16, usize> = std::collections::HashMap::new();
    let mut name_buf = [0u16; 1024];
    let me = unsafe { GetCurrentProcess() };

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

        // 复制句柄并尝试解析路径；能解析出磁盘路径 ⇒ 该索引是 File
        let Ok(proc) = (unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, my_pid as u32) }) else {
            return None;
        };
        let mut dup = HANDLE::default();
        let ok = unsafe {
            DuplicateHandle(
                proc,
                HANDLE(entry.handle as _),
                me,
                &mut dup,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .is_ok();
        unsafe {
            let _ = CloseHandle(proc);
        }
        if !ok {
            continue;
        }
        let is_disk = unsafe { GetFileType(dup) } == FILE_TYPE_DISK;
        let mut path_hit = false;
        if is_disk {
            let n = unsafe { GetFinalPathNameByHandleW(dup, &mut name_buf, FILE_NAME_NORMALIZED) };
            path_hit = n > 0 && (n as usize) <= name_buf.len();
        }
        unsafe {
            let _ = CloseHandle(dup);
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

/// （老系统回退）遍历 SystemObjectTypeInformation 列表找 "File" 类型索引。
/// 依赖 x64 SYSTEM_OBJECTTYPE_INFORMATION 布局，部分系统上不可靠。
#[allow(dead_code)]
fn find_file_type_index_by_name() -> Option<u16> {
    let buf = query_system_info(SYSTEM_OBJECT_TYPE_INFORMATION, 0x8000, 0x100000)?;

    let mut offset = 0usize;
    let mut index = 0u16;
    loop {
        if offset + 40 > buf.len() {
            break;
        }
        let base = unsafe { buf.as_ptr().add(offset) };
        let next = unsafe { *(base as *const u32) } as usize;
        // SYSTEM_OBJECTTYPE_INFORMATION: 6 个 ULONG 后跟 UNICODE_STRING（x64 起始偏移 24）
        let us = unsafe { &*(base.add(24) as *const UnicodeString) };
        if us.length > 0 && !us.buffer.is_null() {
            let chars = unsafe { std::slice::from_raw_parts(us.buffer, us.length as usize / 2) };
            let name = String::from_utf16_lossy(chars);
            if name.eq_ignore_ascii_case("File") {
                return Some(index);
            }
        }
        if next == 0 {
            break;
        }
        offset += next;
        index += 1;
    }
    None
}

/// 枚举全系统句柄，返回当前持有 `path` 的进程 PID 集合。
///
/// 比对规则：`\\?\` 前缀归一化 + 大小写不敏感的完整路径匹配。
/// File 对象类型索引通过"探针句柄"在**同一次句柄表快照**上测定——
/// 类型索引的数值不稳定（每次查询可能漂移），必须与扫描共用同一快照。
/// 失败时返回空集合（调用方仍有 Restart Manager 的结果兜底）。
pub fn scan(path: &Path) -> Vec<u32> {
    enable_debug_privilege();

    // 归一化目标路径
    let mut target = path.to_string_lossy().to_lowercase();
    if let Some(stripped) = target.strip_prefix(r"\\?\") {
        target = stripped.to_string();
    }
    let target = target;

    // 拉取一次句柄表快照，后续类型探测与扫描都在这份数据上进行
    let Some(buf) = query_system_info(SYSTEM_EXTENDED_HANDLE_INFORMATION, 0x400_0000, 0x2000_0000)
    else {
        return Vec::new();
    };
    if buf.len() < std::mem::size_of::<HandleInfoHeader>() {
        return Vec::new();
    }
    let count = unsafe { (*(buf.as_ptr() as *const HandleInfoHeader)).number_of_handles };
    let entry_size = std::mem::size_of::<HandleEntry>();
    if std::mem::size_of::<HandleInfoHeader>() + count * entry_size > buf.len() {
        return Vec::new(); // 数据在两次调用间变化，宁可放弃也不越界
    }

    // 在同一快照上用探针句柄测定 File 类型索引
    let Some(file_index) = file_type_index_in(&buf, count, entry_size) else {
        return Vec::new();
    };

    let me = unsafe { GetCurrentProcess() };
    let mut found: HashSet<u32> = HashSet::new();
    // 仅缓存"打开进程失败"的 pid（无权限等），避免对同一失败 pid 反复 OpenProcess；
    // 不能缓存成功打开的 pid——同一进程有多个句柄，第一个未必是目标文件
    let mut open_failed: HashSet<u32> = HashSet::new();
    let mut name_buf = [0u16; 1024];

    for i in 0..count {
        let entry = unsafe {
            *(buf.as_ptr().add(std::mem::size_of::<HandleInfoHeader>() + i * entry_size)
                as *const HandleEntry)
        };

        let pid = entry.process_id as u32;
        if pid == 0 || entry.object_type_index != file_index || entry.granted_access == 0 {
            continue;
        }
        if found.contains(&pid) {
            continue;
        }
        if open_failed.contains(&pid) {
            continue;
        }

        let Ok(proc) = (unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }) else {
            open_failed.insert(pid);
            continue;
        };

        let mut dup = HANDLE::default();
        let ok = unsafe {
            DuplicateHandle(
                proc,
                HANDLE(entry.handle as _),
                me,
                &mut dup,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .is_ok();
        unsafe {
            let _ = CloseHandle(proc);
        }
        if !ok {
            continue;
        }

        // 只解析磁盘文件句柄，跳过命名管道等可能阻塞的对象类型
        if unsafe { GetFileType(dup) } == FILE_TYPE_DISK {
            let n = unsafe {
                // FILE_NAME_NORMALIZED(0) 与 VOLUME_NAME_DOS(0) 都是 0，等价于默认值组合
                GetFinalPathNameByHandleW(dup, &mut name_buf, FILE_NAME_NORMALIZED)
            };
            if n > 0 && (n as usize) <= name_buf.len() {
                let mut resolved = String::from_utf16_lossy(&name_buf[..n as usize]);
                if let Some(stripped) = resolved.strip_prefix(r"\\?\") {
                    resolved = stripped.to_string();
                }
                if resolved.to_lowercase() == target {
                    found.insert(pid);
                }
            }
        }
        unsafe {
            let _ = CloseHandle(dup);
        }
    }

    found.into_iter().collect()
}
