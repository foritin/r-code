//! R-Code Tool Gateway：工具注册、权限引擎、审计账本。
//!
//! 本 crate 实现 [doc-02] 定义的 Tool Gateway 与权限引擎：
//! - [`permission::PermissionEngine`]：风险分级与审批流程
//! - [`classifier`]：命令动态分类（文本注入 + shell 命令分级）
//! - [`gateway::ToolGateway`]：工具注册、权限检查、审计记账（实现 `agent_contract::ToolHost`）
//! - [`tools`]：文件读写工具集
//! - [`tools_search`]：内嵌 ripgrep 引擎的 `search` / `glob`
//! - [`tools_command`]：平台自适应的 `bash`

#![allow(clippy::doc_nested_refdefs)]

pub mod classifier;
pub mod diagnosis;
pub mod gateway;
pub mod permission;
pub mod tools;
pub mod tools_command;
pub mod tools_search;
#[cfg(windows)]
pub mod win_shell;

pub use classifier::{
    classify_command, classify_shell_command, has_control_chars, is_agent_process,
    CommandClassification,
};
pub use diagnosis::{
    append_diagnosis, classify_failure, codex_shell_dialect, diagnosis_counters,
    reset_diagnosis_counters, DiagnosisKind,
};
pub use gateway::{
    subagent_read_only_tool_allowed, tool_outcome_directive, PathArity, PathBinding, Tool,
    ToolExecutionContext, ToolExecutionDirective, ToolExecutionResult, ToolGateway,
    ToolOutcomeMetadata, ToolPolicyGuard,
};
pub use permission::{PermissionCheckResult, PermissionEngine, StandingRuleKey};
pub use tools::{
    ApplyPatchTool, CreateFileTool, DeleteFileTool, EditTool, GitDiffStatTool, GitStatusTool,
    ListFilesTool, LoadSkillTool, ReadFileTool,
};
pub use tools_command::{
    current_shell_dialect, current_shell_dialect_label, BashTool, ShellDialect,
};
pub use tools_search::{GlobTool, SearchHit, SearchTool};
#[cfg(windows)]
pub use win_shell::{resolve_windows_shell, ResolvedShell};
