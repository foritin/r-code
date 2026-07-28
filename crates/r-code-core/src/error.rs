//! R-Code 专属错误类型。
//!
//! 这些错误在 r-code crate 中定义，不修改公共 `hermes_error::Error` 枚举。
//! 通过 `From` 实现与公共 `Error` 的互转。

use thiserror::Error;

/// R-Code 产品专属错误。
///
/// 不修改公共 `hermes_error::Error` 枚举。
/// 通过 `From<ProductError> for hermes_error::Error` 互转。
#[derive(Debug, Clone, Error)]
pub enum ProductError {
    /// Worktree 操作失败
    #[error("worktree error: {0}")]
    WorktreeError(String),

    /// 路径逃逸（试图访问 workspace 外的文件）
    #[error("path escape: {0}")]
    PathEscape(String),

    /// 路径不存在（只读工具要求路径已存在）
    #[error("path not found: {0}")]
    PathNotFound(String),

    /// Blob 存储错误
    #[error("blob error: {0}")]
    BlobError(String),

    /// 数据库错误
    #[error("database error: {0}")]
    DatabaseError(String),

    /// Migration 错误
    #[error("migration error: {0}")]
    MigrationError(String),

    /// 文件基线错误
    #[error("baseline error: {0}")]
    BaselineError(String),

    /// 回滚错误
    #[error("rollback error: {0}")]
    RollbackError(String),

    /// 验证错误
    #[error("verification error: {0}")]
    VerificationError(String),

    /// 外部 CLI 错误
    #[error("external CLI error: {0}")]
    ExternalCliError(String),

    /// 终端错误
    #[error("terminal error: {0}")]
    TerminalError(String),

    /// 配置错误
    #[error("config error: {0}")]
    ConfigError(String),

    /// 状态机错误
    #[error("state machine error: {0}")]
    StateMachineError(String),

    /// 权限错误
    #[error("permission error: {0}")]
    PermissionError(String),

    /// Git 错误
    #[error("git error: {0}")]
    GitError(String),

    /// IPC 错误（产品层）
    #[error("ipc error: {0}")]
    IpcError(String),

    /// 密钥 / Keychain 错误 [doc-07 §5]
    #[error("secret error: {0}")]
    SecretError(String),

    /// 未实现
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// 其他
    #[error("{0}")]
    Other(String),
}

impl From<ProductError> for hermes_error::Error {
    fn from(err: ProductError) -> Self {
        match err {
            ProductError::WorktreeError(msg) => Self::Storage(format!("worktree: {msg}")),
            ProductError::PathEscape(msg) => Self::PermissionDenied(format!("path escape: {msg}")),
            ProductError::PathNotFound(msg) => Self::ToolHost(format!("path not found: {msg}")),
            ProductError::BlobError(msg) => Self::Storage(format!("blob: {msg}")),
            ProductError::DatabaseError(msg) => Self::Storage(msg),
            ProductError::MigrationError(msg) => Self::Storage(format!("migration: {msg}")),
            ProductError::BaselineError(msg) => Self::Storage(format!("baseline: {msg}")),
            ProductError::RollbackError(msg) => Self::Storage(format!("rollback: {msg}")),
            ProductError::VerificationError(msg) => Self::ToolHost(format!("verification: {msg}")),
            ProductError::ExternalCliError(msg) => Self::ToolHost(format!("external CLI: {msg}")),
            ProductError::TerminalError(msg) => Self::ToolHost(format!("terminal: {msg}")),
            ProductError::ConfigError(msg) => Self::Config(msg),
            ProductError::StateMachineError(msg) => Self::Internal(format!("state machine: {msg}")),
            ProductError::PermissionError(msg) => Self::PermissionDenied(msg),
            ProductError::GitError(msg) => Self::Storage(format!("git: {msg}")),
            ProductError::IpcError(msg) => Self::Ipc(msg),
            ProductError::SecretError(msg) => Self::Other(format!("secret: {msg}")),
            ProductError::NotImplemented(msg) => Self::NotImplemented(msg),
            ProductError::Other(msg) => Self::Other(msg),
        }
    }
}

impl From<std::io::Error> for ProductError {
    fn from(err: std::io::Error) -> Self {
        Self::Other(format!("IO error: {err}"))
    }
}

impl From<serde_json::Error> for ProductError {
    fn from(err: serde_json::Error) -> Self {
        Self::Other(format!("JSON error: {err}"))
    }
}

/// R-Code 产品结果类型。
pub type ProductResult<T> = std::result::Result<T, ProductError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_error_to_hermes_error() {
        let err = ProductError::PathEscape("/etc/passwd".to_string());
        let hermes_err: hermes_error::Error = err.into();
        assert!(matches!(
            hermes_err,
            hermes_error::Error::PermissionDenied(_)
        ));
    }

    #[test]
    fn product_error_database() {
        let err = ProductError::DatabaseError("connection failed".to_string());
        let hermes_err: hermes_error::Error = err.into();
        assert!(matches!(hermes_err, hermes_error::Error::Storage(_)));
    }

    #[test]
    fn io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let product_err: ProductError = io_err.into();
        assert!(matches!(product_err, ProductError::Other(_)));
    }
}
