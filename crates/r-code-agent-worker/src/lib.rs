//! R-Code Agent Worker：Agent Runtime、agent loop、事件流。
//!
//! 基于 `agent-llm` 的 `LlmProvider` trait 实现 agent 循环
//! （model -> tool -> feedback -> model），不使用 Pi SDK（路径 B 决策）。
//!
//! ## 模块结构
//! - [`runtime`]: `AgentRuntime` trait -- R-Code 私有高层抽象
//! - [`mock_runtime`]: `MockAgentRuntime` -- 确定性测试 / 开发后端
//! - [`agent_loop`]: agent 循环单次迭代实现
//! - [`cache_shape`]: DeepSeek 前缀缓存形状归因（P2-H）
//! - [`recovery`]: 崩溃恢复扫描
//!
//! [doc-04 §9, §10]

#![allow(clippy::doc_nested_refdefs)]

pub mod agent_loop;
pub mod cache_shape;
mod checkpoint;
mod delegation_tree;
pub mod llm_runtime;
pub mod mock_runtime;
pub mod recovery;
mod run_guard;
pub mod runtime;

pub use agent_loop::run_agent_loop_iteration;
pub use cache_shape::{CacheChangeCause, PrefixShape, capture, compare};
pub use llm_runtime::{
    AgentPromptPolicy, CodexSubagentEventSink, CodexSubagentOutcome, CodexSubagentRequest,
    CodexSubagentRunner, DEFAULT_MAIN_AGENT_PROMPT, DEFAULT_SUBAGENT_PROMPT, DelegationLimits,
    DelegationRouterMode, ExternalAgentDescriptor, ExternalAgentEventSink, ExternalAgentId,
    ExternalAgentOutcome, ExternalAgentRequest, ExternalAgentRunner, FrozenSubagentCandidatePool,
    FrozenSubagentSlot, FrozenSubagentSlotDescriptor, LlmAgentRuntime, MAX_ACTIVE_DESCENDANTS,
    MAX_DESCENDANTS_PER_TREE, MAX_SUBAGENT_DEPTH, NativeSubagentRuntimeOptions,
    OrchestrationPolicy, QualityLoopMode, QualityReviewer, RCodeSubagentOutcome,
    RCodeSubagentRequest, RCodeSubagentRunner, SubagentCandidateOutcome, SubagentCandidateRequest,
    SubagentCandidateRunner, SubagentCandidateSource, SubagentProviderCapabilities,
    native_parent_subagent_access,
};
pub use checkpoint::GreenCheckpoint;
pub use mock_runtime::MockAgentRuntime;
pub use recovery::{RecoveryState, scan_orphaned_runs};
pub use run_guard::RunBudgetPolicy;
pub use runtime::{AgentRuntime, SteerResult};
