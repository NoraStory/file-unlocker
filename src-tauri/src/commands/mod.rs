//! Tauri 命令层：按域拆分的 IPC 入口。
//!
//! 命令只做参数转发与 spawn_blocking 调度，业务逻辑在
//! `core/`（编排/终止）、`engines/`（检测引擎）与各业务模块中。
//! 重活类命令一律 async + spawn_blocking：Tauri 的同步 command 在
//! WebView2 UI 线程内联执行，耗时超过约 200ms 就会饿死事件循环。

pub mod file;
pub mod process;
pub mod scan;
pub mod system;
pub mod update;
