//! R-Code Host：主进程、IPC Server、进程编排。
//!
//! 基于 agent-contracts 公共层构建：
//! - 使用 `agent-ipc` 的跨平台 `IpcServer`/`IpcClient`（Unix Socket / Named Pipe）
//! - 使用 `agent-tauri` 的 `AppState` + commands 基础
//! - 在公共层基础上注册 R-Code 专属 method handler
//!
//! [doc-08] [agent-contracts/12 §6] [agent-contracts/10]

// clippy 1.99 对 async_trait 展开的 boxing 方法报 double_must_use（方法与其返回
// 的 BoxFuture 同时标 must_use）——宏输出不可控，crate 级豁免。
#![allow(clippy::double_must_use)]

pub mod app_paths;
pub mod attachment_migration;
pub mod automation;
pub mod browser;
pub mod close_gate;
pub mod codex_app_server;
pub mod codex_interaction;
pub mod codex_mcp;
pub mod codex_permissions;
pub mod commands;
#[cfg(unix)] // Control Door 仅 Unix（Windows 不编译且 main.rs 未启动）
pub mod control_door;
pub mod event_coalesce;
pub mod feature_flags;
pub mod fs_util;
pub mod harness_v1;
pub mod harness_v1_chat;
mod harness_v1_routes;

/// FR-7 desktop-side memory freeze (see harness_v1_routes) — re-exported for
/// the bin crate command surface.
pub use harness_v1_routes::frozen_memory_payload;
pub mod ipc;
pub mod legacy_memory;
pub mod lifecycle_commands;
pub mod log_buffer;
pub mod logging;
#[cfg(target_os = "macos")]
mod mac_ocr;
pub mod mcp_manager;
pub mod mcp_server;
pub mod mcp_settings;
pub mod memory_runtime;
pub mod migration;
pub mod model_availability;
pub mod model_capabilities;
pub mod model_pricing;
pub mod native_notification;
pub mod packaging;
pub mod plan_entry_commands;
pub mod plan_policy;
pub mod plan_review_tools;
pub mod plan_tools;
pub use r_code_runtime::services::provider_catalog;
pub mod provider_compat;
pub mod provider_decl;
pub mod provider_models;
pub mod provider_readiness;
pub use r_code_runtime::services::provider_support;
pub mod recovery;
pub mod replay;
pub mod rtk;
pub mod search;
pub mod security_config;
pub mod settings;
pub mod shutdown_coordinator;
pub mod skill_resources;
// SkillManager 已搬至 r-code-runtime（Codex CLI 探测/登录共用）；re-export 保持
// `r_code_host::skills::*` 路径不变。
pub use r_code_runtime::services::skills;
pub mod subagent_providers;
pub mod support_bundle;
pub mod system_integration;
pub mod task_workspace_binding;
pub mod updater;
#[cfg(target_os = "windows")]
mod windows_ocr;
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
pub use workflow_skills::{
    SaveWorkflowSkillTool, ScopedWorkflowSkill, ScopedWorkflowSkillDraft, WorkflowSkill,
    WorkflowSkillCatalog, WorkflowSkillDraft, WorkflowSkillScope, WorkflowSkillSource,
};

/// P19B-C：本轮注册的桌面命令（`main.rs` 的 `invoke_handler` 逐条列出，
/// 这里给出唯一权威清单，避免命令与前端 `ipc.ts` 悄悄漂移）。
///
/// 效果授权四件套：请求 / 清单 / 撤销 / 决策。都只转发给本机 daemon，
/// actor 恒为已认证连接身份，客户端无法自选。
pub const HARNESS_V1_EFFECT_COMMANDS: [&str; 4] = [
    "cmd_harness_v1_effect_request",
    "cmd_harness_v1_effect_list",
    "cmd_harness_v1_effect_revoke",
    "cmd_harness_v1_approval_decide",
];

/// E09-C：unverified-override 审计的桌面命令（只读投影——四端同一份
/// canonical 材料）。注意：`main.rs` 的 `invoke_handler` 一行注册不在
/// 本任务文件集内，属已记录的裁量缺口（见 o-gate ledger iter 7）。
pub const HARNESS_V1_OVERRIDE_COMMANDS: [&str; 1] = ["cmd_harness_v1_overrides_list"];

/// 初始化结构化日志框架。
///
/// 使用 `tracing` crate；日志格式结构化 JSON；支持日志级别动态调整。
/// [doc-14 阶段1]
pub fn init_logging() {
    if app_paths::AppFlavor::current() == app_paths::AppFlavor::Development {
        logging::init_dev();
    } else {
        logging::init();
    }
}
