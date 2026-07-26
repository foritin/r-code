//! R-Code Tool Gateway：工具注册、权限引擎、审计账本。
//!
//! 本 crate 实现 [doc-02] 定义的 Tool Gateway 与权限引擎：
//! - [`permission::PermissionEngine`]：风险分级与审批流程
//! - [`classifier`]：命令动态分类
//! - [`gateway::ToolGateway`]：工具注册、权限检查、审计记账（实现 `hermes_core::ToolHost`）
//! - [`tools`]：内置 R0/R1 只读工具集

#![allow(clippy::doc_nested_refdefs)]

pub mod classifier;
pub mod gateway;
pub mod permission;
pub mod tools;

pub use classifier::{classify_command, has_control_chars, is_agent_process};
pub use gateway::{subagent_read_only_tool_allowed, Tool, ToolGateway};
pub use permission::{PermissionCheckResult, PermissionEngine, StandingRuleKey};
pub use tools::{
    ApplyPatchTool, CreateFileTool, DeleteFileTool, GitStatusTool, ListFilesTool, LoadSkillTool,
    ReadFileTool, SearchTool,
};
