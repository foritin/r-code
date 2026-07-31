//! Agent Runtime trait -- R-Code 私有高层抽象。
//!
//! 拥有 `Box<dyn LlmProvider>` + `Box<dyn ToolHost>` + `SessionStore`，
//! 在 `LlmProvider` 之上实现 agent 循环（model -> tool -> feedback -> model），
//! 而非重新定义 Provider 抽象。
//!
//! [doc-04 §9]

use async_trait::async_trait;
use hermes_core::{Message, Session};
use r_code_core::dto::{AgentEvent, CreateSessionInput, ProjectAccessMode};
use r_code_core::error::ProductError;

/// 运行中引导消息的接纳结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerResult {
    /// 当前运行仍处于可接纳阶段，消息会在下一次模型请求前并入会话历史。
    Accepted,
    /// 运行已越过终止门；调用方应将消息转为新的持久化队列项。
    RunFinished,
}

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

    /// 以完整协议消息启动 run。默认降级到文本，保持 Mock 与第三方 runtime 兼容；
    /// 多模态 runtime 应覆盖此方法以保留 Image 等内容块。
    async fn start_run_with_message(
        &mut self,
        session_id: &str,
        message: Message,
    ) -> Result<String, ProductError> {
        self.start_run(session_id, &message.text_content()).await
    }

    /// Steer -- 在 run 中途提交用户消息。
    ///
    /// 该操作不会修改已经发出的 provider HTTP 请求；实现方必须在下一次 agent
    /// iteration 发起模型请求前将消息并入 history。若当前轮以无工具文本结束但
    /// 已接纳引导，runtime 必须继续下一轮。运行已结束时返回 `RunFinished`，
    /// 由调用方转为新的持久化队列项，避免消息丢失或重复。
    async fn steer(&mut self, session_id: &str, message: &str)
        -> Result<SteerResult, ProductError>;

    /// 中止当前 run。
    async fn abort(&mut self, session_id: &str) -> Result<(), ProductError>;

    /// 中止当前主运行下的一个子代理。返回 `true` 表示已找到并发出取消请求。
    ///
    /// 默认实现保持测试 runtime 兼容；不支持子代理的实现安全地返回 `false`。
    async fn abort_subagent(
        &mut self,
        _session_id: &str,
        _subagent_id: &str,
    ) -> Result<bool, ProductError> {
        Ok(false)
    }

    /// 使用持久化的会话前缀重建运行时工作集。用于应用重启恢复以及“编辑后分叉重跑”。
    ///
    /// 默认实现保持兼容：不维护本地历史的测试 runtime 可安全忽略该调用。
    async fn replace_history(
        &mut self,
        _session_id: &str,
        _messages: Vec<Message>,
    ) -> Result<(), ProductError> {
        Ok(())
    }

    /// 返回当前会话的完整协议工作集，用于持久化恢复。
    ///
    /// 实现必须包含 Text、ToolUse 与 ToolResult 的原始顺序。默认实现服务于不保存
    /// 本地历史的测试 runtime，返回 `None` 表示调用方继续使用普通事件日志。
    async fn history_snapshot(
        &mut self,
        _session_id: &str,
    ) -> Result<Option<Vec<Message>>, ProductError> {
        Ok(None)
    }

    /// 更新会话绑定的工作区作用域。`None` 表示纯聊天，不向模型暴露本地工具。
    ///
    /// 默认实现服务于不维护工作区状态的测试 runtime；真实 runtime 会即时收紧
    /// 或扩展该会话可见的工具集。
    async fn update_workspace_scope(
        &mut self,
        _session_id: &str,
        _workspace_path: Option<String>,
        _access_mode: ProjectAccessMode,
    ) -> Result<(), ProductError> {
        Ok(())
    }

    /// 获取当前 run 的事件（ drain 语义：取出后清空）。
    async fn poll_events(&mut self) -> Result<Vec<AgentEvent>, ProductError>;
}
