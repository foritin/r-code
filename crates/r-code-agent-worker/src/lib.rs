//! R-Code Agent Worker：Agent Runtime、agent loop、事件流。
//!
//! 基于 `hermes-llm` 的 `LlmProvider` trait 实现 agent 循环
//! （model -> tool -> feedback -> model），不使用 Pi SDK（路径 B 决策）。
//!
//! ## 模块结构
//! - [`runtime`]: `AgentRuntime` trait -- R-Code 私有高层抽象
//! - [`mock_runtime`]: `MockAgentRuntime` -- 确定性测试 / 开发后端
//! - [`agent_loop`]: agent 循环单次迭代实现
//! - [`recovery`]: 崩溃恢复扫描
//!
//! [doc-04 §9, §10]

#![allow(clippy::doc_nested_refdefs)]

pub mod agent_loop;
pub mod llm_runtime;
pub mod mock_runtime;
pub mod recovery;
pub mod runtime;

pub use agent_loop::run_agent_loop_iteration;
pub use llm_runtime::{
    AgentPromptPolicy, CodexSubagentEventSink, CodexSubagentOutcome, CodexSubagentRequest,
    CodexSubagentRunner, DelegationRouterMode, LlmAgentRuntime, OrchestrationPolicy,
    QualityLoopMode, QualityReviewer, DEFAULT_MAIN_AGENT_PROMPT, DEFAULT_SUBAGENT_PROMPT,
};
pub use mock_runtime::MockAgentRuntime;
pub use recovery::{scan_orphaned_runs, RecoveryState};
pub use runtime::{AgentRuntime, SteerResult};
