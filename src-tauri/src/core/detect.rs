//! 扫描编排：调度双引擎、合并去重、构造对外结果。
//!
//! 检测采用双引擎（参照 PowerToys File Locksmith 与 LockHunter 的策略）：
//! 1. **Restart Manager**（`engines::restart_manager`）：结果权威，
//!    附带应用显示名，但对 SYSTEM 进程 / 非常规共享模式可能漏报；
//! 2. **句柄枚举扫描**（`handle_scan` 模块）：遍历全系统句柄表补漏。
//!    两引擎结果按 PID 合并去重（`merge_engine_hits` 纯策略）。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::engines::restart_manager;
use crate::handle_scan;
use crate::winutil::{
    file_description, process_creation_time, process_image_full,
};

/// 正在锁定文件的进程信息
#[derive(Debug, Clone, serde::Serialize)]
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

/// 扫描结果：占用进程列表 + 目录模式的元信息
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanOutcome {
    /// 占用进程列表（按 PID 排序）
    pub processes: Vec<ProcessInfo>,
    /// 目录模式下枚举文件数达到上限，结果被截断（前端应给出提示）
    pub truncated: bool,
    /// 目录模式实际枚举的文件数（截断时等于上限；单文件模式恒为 1）
    pub file_count: usize,
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

/// 合并策略输入：单个引擎的原始命中（PID + 应用显示名）。
/// 测试与生产共用同一构造路径，避免合并逻辑与 FFI 耦合后不可测。
#[derive(Debug, Clone)]
pub(crate) struct EngineHit {
    pub pid: u32,
    pub app_name: String,
}

/// 引擎来源标记：与 ProcessInfo.source 字段一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HitSource {
    RestartManager,
    HandleScan,
}

impl HitSource {
    fn as_str(self) -> &'static str {
        match self {
            HitSource::RestartManager => "restart_manager",
            HitSource::HandleScan => "handle_scan",
        }
    }
}

/// 合并去重的纯策略（不触碰任何 FFI）：以 RM 为主（有 app_name），
/// 句柄扫描补漏；双引擎同时命中标记 "both"。
/// `locked_files` 由调用方按目录/单文件语义另行覆盖。
///
/// 返回 (pid → (app_name, source))，供 make_process_info 消费。
pub(crate) fn merge_engine_hits(
    rm_hits: &[EngineHit],
    handle_pids: &[u32],
) -> Vec<(u32, String, &'static str)> {
    let mut merged: HashMap<u32, (String, &'static str)> = HashMap::new();
    for hit in rm_hits {
        merged
            .entry(hit.pid)
            .or_insert_with(|| (hit.app_name.clone(), HitSource::RestartManager.as_str()));
    }
    for &pid in handle_pids {
        match merged.entry(pid) {
            // RM 也报了该 PID：标记双引擎命中
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.get_mut().1 = "both";
            }
            // 仅句柄扫描发现（RM 不可用或漏报）
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert((String::new(), HitSource::HandleScan.as_str()));
            }
        }
    }
    let mut list: Vec<(u32, String, &'static str)> =
        merged.into_iter().map(|(pid, (a, s))| (pid, a, s)).collect();
    list.sort_by_key(|(pid, _, _)| *pid);
    list
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

    if let Ok(rm_list) = restart_manager::batch_query_restart_manager(&files) {
        let rm_hits: Vec<EngineHit> = rm_list
            .into_iter()
            .map(|(pid, app_name)| EngineHit { pid, app_name })
            .collect();
        // RM 不提供逐文件计数；至少记 1，防止 RM 独有的检出被误丢弃
        for (pid, app_name, source) in merge_engine_hits(&rm_hits, &[]) {
            merged.insert(pid, make_process_info(pid, app_name, source, 1));
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

/// 单文件模式：双引擎合并查询。
fn scan_single_file(file_path: &str) -> Result<Vec<ProcessInfo>, String> {
    // 引擎 1：Restart Manager（失败不致命，继续走句柄扫描）
    let rm_result = restart_manager::query_restart_manager(file_path);
    let rm_list = rm_result.clone().unwrap_or_default();

    // 引擎 2：句柄枚举扫描补漏（引擎失效时与 RM 结果联合判断）
    let hm_result = handle_scan::scan(Path::new(file_path));
    let hm_pids: Vec<u32> = hm_result.clone().unwrap_or_default();

    // 合并去重：rm 为主（有 app_name），句柄扫描补漏
    let rm_hits: Vec<EngineHit> = rm_list
        .into_iter()
        .map(|(pid, app_name)| EngineHit { pid, app_name })
        .collect();
    let merged_plan = merge_engine_hits(&rm_hits, &hm_pids);

    let mut merged: HashMap<u32, ProcessInfo> = HashMap::new();
    for (pid, app_name, source) in merged_plan {
        // 单文件模式每个进程锁定计数恒为 1
        merged.insert(pid, make_process_info(pid, app_name, source, 1));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(pid: u32, app_name: &str) -> EngineHit {
        EngineHit {
            pid,
            app_name: app_name.to_string(),
        }
    }

    #[test]
    fn merge_dedupes_by_pid_and_marks_both() {
        // RM 与句柄扫描都报了 PID 100：去重为一条，source = both，保留 RM 的 app_name
        let plan = merge_engine_hits(&[hit(100, "Word")], &[100]);
        assert_eq!(plan, vec![(100, "Word".to_string(), "both")]);
    }

    #[test]
    fn merge_keeps_rm_only_and_handle_only_entries() {
        let plan = merge_engine_hits(&[hit(200, "Excel"), hit(300, "")], &[100, 200]);
        assert_eq!(
            plan,
            vec![
                (100, String::new(), "handle_scan"),
                (200, "Excel".to_string(), "both"),
                (300, String::new(), "restart_manager"),
            ]
        );
    }

    #[test]
    fn merge_output_is_sorted_by_pid() {
        let plan = merge_engine_hits(&[hit(300, "c"), hit(100, "a")], &[200, 100]);
        let pids: Vec<u32> = plan.iter().map(|(pid, _, _)| *pid).collect();
        assert_eq!(pids, vec![100, 200, 300]);
    }

    #[test]
    fn merge_empty_inputs_yield_empty_plan() {
        assert!(merge_engine_hits(&[], &[]).is_empty());
        // RM 失败时句柄扫描独立兜底
        let plan = merge_engine_hits(&[], &[42]);
        assert_eq!(plan, vec![(42, String::new(), "handle_scan")]);
    }

    #[test]
    fn merge_does_not_overwrite_first_rm_app_name() {
        // RM 结果中同一 PID 出现两次（理论边界）：保留首个 app_name，不回退为空
        let plan = merge_engine_hits(&[hit(7, "First"), hit(7, "Second")], &[]);
        assert_eq!(plan, vec![(7, "First".to_string(), "restart_manager")]);
    }

    #[test]
    fn process_name_falls_back_when_exe_path_empty() {
        assert_eq!(
            process_name_of(r"C:\Tools\notepad.exe", "fallback"),
            "notepad.exe"
        );
        assert_eq!(
            process_name_of(r"C:/Tools/winword.exe", "fallback"),
            "winword.exe"
        );
        // exe 路径为空 → 回退到 RM 的 app_name
        assert_eq!(process_name_of("", "Microsoft Word"), "Microsoft Word");
        // 两者都为空 → 空串（而非 panic）
        assert_eq!(process_name_of("", ""), "");
    }
}
