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

/// 遍历 SystemObjectTypeInformation 列表，找到 "File" 对象类型的索引。
/// 句柄表条目中的 ObjectTypeIndex 与该列表序号对应。
fn find_file_type_index() -> Option<u16> {
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
/// 失败时返回空集合（调用方仍有 Restart Manager 的结果兜底）。
pub fn scan(path: &Path) -> Vec<u32> {
    enable_debug_privilege();

    let Some(file_index) = find_file_type_index() else {
        return Vec::new();
    };

    // 归一化目标路径
    let mut target = path.to_string_lossy().to_lowercase();
    if let Some(stripped) = target.strip_prefix(r"\\?\") {
        target = stripped.to_string();
    }
    let target = target;

    let Some(buf) = query_system_info(SYSTEM_EXTENDED_HANDLE_INFORMATION, 0x400_0000, 0x2000_0000)
    else {
        return Vec::new();
    };
    if buf.len() < std::mem::size_of::<HandleInfoHeader>() {
        return Vec::new();
    }

    let count = unsafe { (*(buf.as_ptr() as *const HandleInfoHeader)).number_of_handles };
    let entry_size = std::mem::size_of::<HandleEntry>();
    let entries_end = std::mem::size_of::<HandleInfoHeader>() + count * entry_size;
    if entries_end > buf.len() {
        return Vec::new(); // 数据在两次调用间变化，宁可放弃也不越界
    }

    let me = unsafe { GetCurrentProcess() };
    let mut found: HashSet<u32> = HashSet::new();
    let mut opened: HashSet<u32> = HashSet::new();
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

        // 复制句柄前先尝试打开目标进程（按 pid 缓存失败结果避免重复开销）
        if opened.contains(&pid) {
            continue;
        }
        opened.insert(pid);
        let Ok(proc) = (unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }) else {
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
