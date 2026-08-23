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
pub use cache_shape::{capture, compare, CacheChangeCause, PrefixShape};
pub use checkpoint::GreenCheckpoint;
pub use llm_runtime::{
    native_parent_subagent_access, AgentPromptPolicy, AttachmentResolver, CodexSubagentEventSink,
    CodexSubagentOutcome, CodexSubagentRequest, CodexSubagentRunner, DelegationLimits,
    DelegationRouterMode, ExternalAgentDescriptor, ExternalAgentEventSink, ExternalAgentId,
    ExternalAgentOutcome, ExternalAgentRequest, ExternalAgentRunner, FrozenSubagentCandidatePool,
    FrozenSubagentSlot, FrozenSubagentSlotDescriptor, LlmAgentRuntime,
    NativeSubagentRuntimeOptions, OrchestrationPolicy, PlanNativeCatalogConfig,
    PlanNativeCatalogPhase, QualityLoopMode, QualityReviewer, RCodeSubagentOutcome,
    RCodeSubagentRequest, RCodeSubagentRunner, RequestBudgetV1, RequestKind, ResolvedAttachment,
    ResolvedOutputBudget, RouteDescriptor, SubagentCandidateOutcome, SubagentCandidateRequest,
    SubagentCandidateRunner, SubagentCandidateSource, SubagentProviderCapabilities,
    DEFAULT_MAIN_AGENT_PROMPT, DEFAULT_SUBAGENT_PROMPT, MAX_ACTIVE_DESCENDANTS,
    MAX_DESCENDANTS_PER_TREE, MAX_DIRECT_SUBAGENTS_PER_RUN, MAX_SUBAGENT_DEPTH,
};
pub use mock_runtime::MockAgentRuntime;
pub use recovery::{scan_orphaned_runs, RecoveryState};
pub use run_guard::RunBudgetPolicy;
pub use runtime::{AgentRuntime, SteerResult};
