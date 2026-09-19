//! 检测引擎层：文件占用的具体检测实现。
//!
//! - `restart_manager`：Restart Manager 查询（权威、带应用名，可能漏报）
//! - `handle_scan`（既有模块）：NT 句柄表枚举扫描（补漏、覆盖目录前缀）
//!
//! 两引擎结果的合并去重策略在 `core::detect` 编排器中实现。

pub mod restart_manager;
