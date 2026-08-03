//! R-Code Host：主进程、IPC Server、进程编排。
//!
//! 基于 agent-core 公共层构建：
//! - 使用 `hermes-ipc` 的跨平台 `IpcServer`/`IpcClient`（Unix Socket / Named Pipe）
//! - 使用 `hermes-tauri` 的 `AppState` + commands 基础
//! - 在公共层基础上注册 R-Code 专属 method handler
//!
//! [doc-08] [agent-core/12 §6] [agent-core/10]

pub mod codex_mcp;
pub mod codex_permissions;
pub mod commands;
#[cfg(unix)] // Control Door 仅 Unix（Windows 不编译且 main.rs 未启动）
pub mod control_door;
pub mod ipc;
pub mod legacy_memory;
pub mod log_buffer;
pub mod logging;
pub mod mcp_manager;
pub mod mcp_server;
pub mod mcp_settings;
pub mod memory_runtime;
pub mod migration;
pub mod packaging;
pub mod plan_review_tools;
pub mod plan_tools;
pub mod provider_catalog;
pub mod provider_models;
pub mod recovery;
pub mod replay;
pub mod search;
pub mod security_config;
pub mod settings;
pub mod skills;
pub mod support_bundle;
pub mod system_integration;
pub mod work_card;
pub mod workflow_skills;

// 重新导出核心类型
pub use commands::{
    CommandState, RecoveryPageData as CmdRecoveryPageData, SearchMatch as CmdSearchMatch,
    TerminalInfo as CmdTerminalInfo,
};
pub use legacy_memory::{LegacyMemoryGitTracking, LegacyMemoryStatus};
pub use migration::{MigrationManager, MigrationResult, MigrationStep};
pub use packaging::{
    BundleTarget, LicenseEntry, LinuxConfig, MacOSConfig, PackagingConfig, SbomGenerator,
    UpdateChannel, UpdateConfig, WindowsConfig,
};
pub use provider_catalog::{
    AuthStyle as ProviderAuthStyle, Category as ProviderCategory, Endpoint as ProviderEndpoint,
    Preset as ProviderPreset, Protocol as ProviderProtocol,
};
pub use recovery::{InterruptedTask, RecoveryManager, RecoveryPageData};
pub use replay::{EvidenceLevel, ReplayDepth, ReplayEntry, ReplayService};
pub use search::{ReplacePreview, SearchMatch, SearchService};
pub use security_config::{should_block_navigation, should_block_window_open, SecurityConfig};
pub use skills::{SkillManager, SkillStatus};
pub use support_bundle::{
    BundleContents, ConfigSummary, DbStats, LogEntry, McpServerSupportSummary, SupportBundle,
};
pub use work_card::{
    EvidenceItem, FailureState, RequiredTest, RollbackPlan, TestStatus, TestType, WorkCard,
    WorkCardBoundary, WorkCardContract,
};
pub use workflow_skills::{
    SaveWorkflowSkillTool, WorkflowSkill, WorkflowSkillCatalog, WorkflowSkillDraft,
    WorkflowSkillSource,
};

/// 初始化结构化日志框架。
///
/// 使用 `tracing` crate；日志格式结构化 JSON；支持日志级别动态调整。
/// [doc-14 阶段1]
pub fn init_logging() {
    logging::init();
}
