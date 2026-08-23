//! R-Code 专属错误类型。
//!
//! 这些错误在 r-code crate 中定义，不修改公共 `agent_error::Error` 枚举。
//! 通过 `From` 实现与公共 `Error` 的互转。

use thiserror::Error;

/// Stable IPC-facing code for the per-project active conversation limit.
pub const PROJECT_CONVERSATION_LIMIT_REACHED_CODE: &str = "PROJECT_CONVERSATION_LIMIT_REACHED";

/// R-Code 产品专属错误。
///
/// 不修改公共 `agent_error::Error` 枚举。
/// 通过 `From<ProductError> for agent_error::Error` 互转。
#[derive(Debug, Clone, Error)]
pub enum ProductError {
    /// A workspace already owns the maximum number of unarchived conversations.
    #[error("该项目最多保留 {limit} 个未归档对话，请先归档一个后再新建")]
    ProjectConversationLimitReached { limit: usize },

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

    /// 模型可以根据稳定错误码和上下文自行修正的工具错误。
    ///
    /// `details` 只允许放入可直接返回给模型的非敏感诊断信息。普通数据库、凭据和
    /// IPC 错误仍使用各自的错误变体，不能借此绕过上层的脱敏逻辑。
    #[error("{message}")]
    RecoverableToolError {
        tool: String,
        code: String,
        message: String,
        details: serde_json::Value,
    },

    /// Provider 正常结束了一个模型回合，但没有返回任何可持久化的 assistant 内容。
    ///
    /// 这是可被运行协调层精确识别的协议结果：若前序工具已经执行，协调层可以进行
    /// 一次禁用工具的最终总结恢复；普通线路错误不能冒充这一结果。
    #[error("模型服务未返回可显示内容，请重试或检查模型线路配置")]
    EmptyAssistantResponse,

    /// 模型输出在 max_tokens 处被截断且自动升档重试后仍无正文——推理耗尽了
    /// 全部输出预算。这是配置/任务规模问题，不是线路故障，文案必须把用户
    /// 引向调大「最大输出」而不是检查接口。
    #[error("输出预算被推理耗尽（尝试 {attempted} / 配置 {configured} / 服务上限 {provider_ceiling}）。请在设置中调大「每轮最大输出」或降低推理强度")]
    OutputBudgetExhausted {
        attempted: u32,
        /// 用户/自动配置的单轮输出上限。
        configured: u32,
        /// Provider 声明的服务端上限（0 = 未声明）。
        provider_ceiling: u32,
        /// 冻结请求的 reasoning effort（None = 未配置）。
        reasoning_effort: Option<String>,
    },

    /// 请求在派发前已因上下文 headroom 被钳制，随后仍收到空 `MaxTokens` 截断。
    /// 这是上下文预算问题：重放只会再次超窗，不重试（docs §6.5）。
    #[error("CONTEXT_CONSTRAINED_OUTPUT_EXHAUSTED：上下文余量已把输出钳制到 {effective_output}，推理耗尽后无产物。请压缩上下文或降低推理强度")]
    ContextConstrainedOutputExhausted { effective_output: u32 },

    /// 发送前硬闸门无法获得最低可执行输出额度。Provider 调用次数必须为 0，
    /// 不得把额度强制改成 1（docs §2.3）。
    #[error("OUTPUT_HEADROOM_BELOW_MINIMUM：有效输出额度 {effective_output} 低于 {minimum} 的最低要求，已取消发送。请压缩上下文或降低每轮输出上限")]
    OutputHeadroomBelowMinimum { effective_output: u32, minimum: u32 },

    /// 整理（最多两次强制折叠/裁剪）后输入+输出仍超窗。Provider 调用次数为 0，
    /// 不得跳出循环后继续发送（docs §6.4）。
    #[error("CONTEXT_PREFLIGHT_FAILED：强制整理后请求仍超出模型窗口（估算输入 {estimated_input} + 输出 {output_reserve} > 窗口 {window}）。请开启新会话或手动压缩历史")]
    ContextPreflightFailed {
        estimated_input: u64,
        output_reserve: u32,
        window: u32,
    },

    /// 附件 ref 不存在（docs §11）。
    #[error("ATTACHMENT_NOT_FOUND：附件 {attachment_id} 不存在或已被清理")]
    AttachmentNotFound { attachment_id: String },

    /// 附件 ref 不属于当前 task（docs §11）。
    #[error("ATTACHMENT_OWNERSHIP_MISMATCH：附件 {attachment_id} 不属于当前会话")]
    AttachmentOwnershipMismatch { attachment_id: String },

    /// 消息内附件元数据与数据库权威元数据不一致（docs §11）。
    #[error(
        "ATTACHMENT_METADATA_MISMATCH：附件 {attachment_id} 的元数据与存储记录不一致（{detail}）"
    )]
    AttachmentMetadataMismatch {
        attachment_id: String,
        detail: String,
    },

    /// 排队消息或 Plan 冻结的图片路由与当前任务路由不一致（docs §11）。
    #[error("ATTACHMENT_ROUTE_DRIFT：排队消息的图片路由与当前任务不一致，已标记失败；不会改用其他引擎重新解释")]
    AttachmentRouteDrift { detail: String },

    /// 已确认多模态但目录缺少视觉预算 profile（docs §6.2）。不得回退到
    /// Base64 字符估算或 OCR。
    #[error("VISION_BUDGET_PROFILE_MISSING：模型 {model} 声明支持图片但缺少视觉预算 profile，已取消发送")]
    VisionBudgetProfileMissing { model: String },

    /// 协议适配器不能发送图片块（docs §5.1）。不得改走 OCR。
    #[error("VISION_WIRE_UNSUPPORTED：协议 {protocol} 无法序列化图片输入，请在设置中选择支持图片的协议；不会自动改用 OCR")]
    VisionWireUnsupported { protocol: String },

    /// Provider 拒绝了已确认的图片能力（能力声明漂移，docs §5.3）。此路径
    /// OCR/helper 调用计数必须保持 0。
    #[error("VISION_CAPABILITY_DRIFT：服务 {provider}（{provider_kind}）的模型 {model} 经协议 {protocol} 拒绝了图片输入，但目录已确认其支持。请核对模型/协议后重试；不会自动改用 OCR（route {route_revision}）")]
    VisionCapabilityDrift {
        provider: String,
        provider_kind: String,
        model: String,
        protocol: String,
        route_revision: String,
    },

    /// 活动 Plan 的 Provider route 漂移（docs §8.7）。
    #[error("PLAN_ANCHOR_ROUTE_DRIFT：活动 Plan 冻结的 Provider 路由与当前任务不一致（{detail}）。请恢复原路由或重新创建 Plan")]
    PlanAnchorRouteDrift { detail: String },

    /// ExecutionFull 首个请求仍看到 5/8 收窄目录（docs §8.6）——fail closed。
    #[error("PLAN_FULL_CATALOG_NOT_RESTORED：Plan 批准后的实施请求仍看到收窄目录（{tool_count} 项），已取消发送")]
    PlanFullCatalogNotRestored { tool_count: usize },

    /// Git 错误
    #[error("git error: {0}")]
    GitError(String),

    /// IPC 错误（产品层）
    #[error("ipc error: {0}")]
    IpcError(String),

    /// 密钥 / 平台凭据存储错误 [doc-07 §5]
    #[error("secret error: {0}")]
    SecretError(String),

    /// 未实现
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// 其他
    #[error("{0}")]
    Other(String),
}

impl From<ProductError> for agent_error::Error {
    fn from(err: ProductError) -> Self {
        match err {
            ProductError::ProjectConversationLimitReached { limit } => Self::Other(format!(
                "该项目最多保留 {limit} 个未归档对话，请先归档一个后再新建"
            )),
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
            ProductError::RecoverableToolError {
                tool,
                code,
                message,
                details,
            } => Self::ToolHost(
                serde_json::json!({
                    "status": "error",
                    "tool": tool,
                    "code": code,
                    "message": message,
                    "details": details,
                })
                .to_string(),
            ),
            ProductError::EmptyAssistantResponse => {
                Self::Other("模型服务未返回可显示内容，请重试或检查模型线路配置".to_string())
            }
            ProductError::OutputBudgetExhausted { .. }
            | ProductError::ContextConstrainedOutputExhausted { .. }
            | ProductError::OutputHeadroomBelowMinimum { .. }
            | ProductError::ContextPreflightFailed { .. }
            | ProductError::AttachmentNotFound { .. }
            | ProductError::AttachmentOwnershipMismatch { .. }
            | ProductError::AttachmentMetadataMismatch { .. }
            | ProductError::AttachmentRouteDrift { .. }
            | ProductError::VisionBudgetProfileMissing { .. }
            | ProductError::VisionWireUnsupported { .. }
            | ProductError::VisionCapabilityDrift { .. }
            | ProductError::PlanAnchorRouteDrift { .. }
            | ProductError::PlanFullCatalogNotRestored { .. } => {
                // 新错误组携带稳定的可操作文案；跨边界传递时保留完整 display。
                Self::Other(err.to_string())
            }
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
    fn product_error_to_agent_error() {
        let err = ProductError::PathEscape("/etc/passwd".to_string());
        let agent_err: agent_error::Error = err.into();
        assert!(matches!(agent_err, agent_error::Error::PermissionDenied(_)));
    }

    #[test]
    fn product_error_database() {
        let err = ProductError::DatabaseError("connection failed".to_string());
        let agent_err: agent_error::Error = err.into();
        assert!(matches!(agent_err, agent_error::Error::Storage(_)));
    }

    #[test]
    fn project_conversation_limit_exposes_stable_code_and_limit() {
        assert_eq!(
            PROJECT_CONVERSATION_LIMIT_REACHED_CODE,
            "PROJECT_CONVERSATION_LIMIT_REACHED"
        );
        let error = ProductError::ProjectConversationLimitReached { limit: 5 };
        assert_eq!(
            error.to_string(),
            "该项目最多保留 5 个未归档对话，请先归档一个后再新建"
        );
        assert!(matches!(
            error,
            ProductError::ProjectConversationLimitReached { limit: 5 }
        ));
    }

    #[test]
    fn io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let product_err: ProductError = io_err.into();
        assert!(matches!(product_err, ProductError::Other(_)));
    }
}
