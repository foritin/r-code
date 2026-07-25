//! Agent Runtime trait -- R-Code 私有高层抽象。
//!
//! 拥有 `Box<dyn LlmProvider>` + `Box<dyn ToolHost>` + `SessionStore`，
//! 在 `LlmProvider` 之上实现 agent 循环（model -> tool -> feedback -> model），
//! 而非重新定义 Provider 抽象。
//!
//! [doc-04 §9]

use async_trait::async_trait;
use hermes_core::Session;
use r_code_core::dto::{AgentEvent, CreateSessionInput};
use r_code_core::error::ProductError;

/// Agent Runtime -- R-Code 私有高层 trait。
///
/// 实现方拥有 `Box<dyn LlmProvider>` + `Box<dyn ToolHost>` + `SessionStore`，
/// 在 `LlmProvider` 之上实现 agent 循环（model -> tool -> feedback -> model），
/// 而非重新定义 Provider 抽象。这样可以复用公共层的流式 / 工具协议，
/// 产品层只负责编排、状态机与权限。
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// 创建新会话。
    async fn create_session(&mut self, input: CreateSessionInput) -> Result<Session, ProductError>;

    /// 启动一次 run -- 开始 agent 循环。返回 run id。
    async fn start_run(&mut self, session_id: &str, goal: &str) -> Result<String, ProductError>;

    /// Steer -- 在 run 中途注入用户消息。
    async fn steer(&mut self, session_id: &str, message: &str) -> Result<(), ProductError>;

    /// 中止当前 run。
    async fn abort(&mut self, session_id: &str) -> Result<(), ProductError>;

    /// 获取当前 run 的事件（ drain 语义：取出后清空）。
    async fn poll_events(&mut self) -> Result<Vec<AgentEvent>, ProductError>;
}
