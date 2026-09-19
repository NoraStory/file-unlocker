//! 文件占用检测与进程结束（兼容门面）。
//!
//! Phase 1 热点切割后，本模块的全部实现已迁移：
//! - 扫描编排（双引擎合并、目录聚合）→ [`crate::core::detect`]
//! - 进程终止（身份校验、PID 复用防护、进程树）→ [`crate::core::kill`]
//! - Restart Manager 引擎 → [`crate::engines::restart_manager`]
//!
//! 这里仅保留类型与函数的 re-export，保证 `lib.rs` 命令层与既有测试
//! 的引用路径不变（对外 API 零变化）。

pub use crate::core::detect::{get_locking_processes, ScanOutcome};
pub use crate::core::kill::{kill_process, kill_process_tree};
