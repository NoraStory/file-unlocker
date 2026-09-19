//! 结构化错误模型（ADR-002）。
//!
//! 之前所有命令返回 `Result<T, String>`，前端只能展示文本、无法按错误
//! 类型分支处理（权限错误应引导提权、路径错误应提示重新拖入）。
//! 本模块定义带稳定错误码的错误类型，IPC 边界序列化为 `{ code, message }`：
//!
//! - `code`：稳定的机器可读标识（如 `E_PERM_DENIED`），前端据此分支，
//!   也是未来国际化的映射键；一旦发布即冻结，不得重命名
//! - `message`：面向用户的中文可读消息，仅用于展示与日志
//!
//! 兼容策略：`AppError` 实现 `From<String>` 与 `From<&str>`，
//! 未迁移的旧代码路径继续以裸字符串透传（code 归为 `E_UNKNOWN`），
//! 各命令按需逐步切换，前端对未知 code 一律回退到 message 展示。

use serde::Serialize;

/// 稳定错误码。发布后不得改名或复用；新增变体只能追加。
/// 变体统一带 E 前缀（错误码惯例，如 E_PERM_DENIED 的驼峰形式）；
/// 部分变体暂由后续命令迁移时启用（允许 dead_code）。
#[allow(dead_code)]
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ErrorCode {
    /// 未分类错误（裸字符串透传的兜底分类）
    EUnknown,
    /// 输入校验失败：路径为空 / 非绝对路径 / 格式非法
    EPathInvalid,
    /// 目标文件或目录不存在
    EPathNotFound,
    /// 权限不足（需要管理员或文件 ACL 拒绝）
    EPermDenied,
    /// 目标位于系统受保护位置，操作被拒绝
    EProtected,
    /// 文件仍被占用（删除场景）
    EFileBusy,
    /// 检测引擎失效（双引擎均无结果或句柄扫描异常）
    EEngineFailure,
    /// 进程身份校验失败（PID 复用防护 / 受保护进程 / 已退出）
    EProcessIdentity,
    /// 后台任务（spawn_blocking）异常
    EInternal,
    /// 更新流程失败：网络、清单解析、校验、启动
    EUpdateFailure,
}

impl ErrorCode {
    /// IPC 传输用的稳定字符串形式（snake_case）。
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::EUnknown => "e_unknown",
            ErrorCode::EPathInvalid => "e_path_invalid",
            ErrorCode::EPathNotFound => "e_path_not_found",
            ErrorCode::EPermDenied => "e_perm_denied",
            ErrorCode::EProtected => "e_protected",
            ErrorCode::EFileBusy => "e_file_busy",
            ErrorCode::EEngineFailure => "e_engine_failure",
            ErrorCode::EProcessIdentity => "e_process_identity",
            ErrorCode::EInternal => "e_internal",
            ErrorCode::EUpdateFailure => "e_update_failure",
        }
    }
}

/// IPC 边界的统一错误载荷：序列化为 `{ code, message }`。
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
}

impl AppError {
    #[allow(dead_code)]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.as_str(),
            message: message.into(),
        }
    }
}

impl From<String> for AppError {
    fn from(message: String) -> Self {
        AppError::new(ErrorCode::EUnknown, message)
    }
}

impl From<&str> for AppError {
    fn from(message: &str) -> Self {
        AppError::new(ErrorCode::EUnknown, message)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable_strings() {
        // 错误码一旦发布即冻结：此测试是变更警报，改名字段必须过审
        assert_eq!(ErrorCode::EPermDenied.as_str(), "e_perm_denied");
        assert_eq!(ErrorCode::EPathInvalid.as_str(), "e_path_invalid");
        assert_eq!(ErrorCode::EFileBusy.as_str(), "e_file_busy");
        assert_eq!(ErrorCode::EUnknown.as_str(), "e_unknown");
    }

    #[test]
    fn app_error_serializes_code_and_message() {
        let e = AppError::new(ErrorCode::EPathNotFound, "文件不存在：C:\\a.txt");
        let json = serde_json::to_value(&e).expect("serialize");
        assert_eq!(json["code"], "e_path_not_found");
        assert_eq!(json["message"], "文件不存在：C:\\a.txt");
    }

    #[test]
    fn bare_string_maps_to_unknown_code() {
        let e: AppError = "旧式错误".into();
        assert_eq!(e.code, "e_unknown");
        assert_eq!(e.message, "旧式错误");
        let e2: AppError = String::from("旧式错误2").into();
        assert_eq!(e2.code, "e_unknown");
    }

    #[test]
    fn display_includes_code_for_logs() {
        let e = AppError::new(ErrorCode::EProtected, "拒绝删除");
        assert_eq!(e.to_string(), "[e_protected] 拒绝删除");
    }
}
