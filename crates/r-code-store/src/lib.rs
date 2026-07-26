//! R-Code SQLite 持久化层。
//!
//! ✅ 双存储架构：
//! - `hermes-store` (JSONL) 是会话内容源（source of truth）
//! - `r-code-store` (SQLite) 是产品状态源
//!
//! ## 数据流
//! - Agent 产生 SessionEvent -> 先写 JSONL（crash-safe）-> 再投影元数据到 SQLite
//! - SQLite 存：Task 状态机、Run 生命周期、tool_calls 元数据、permission_requests、
//!   file_changes/baselines/blobs
//! - 崩溃恢复：先读 JSONL `recover()`（恢复对话），再读 SQLite（恢复 Task 状态）
//! - UI 查询：任务列表/权限/审计从 SQLite；时间线/Replay 从 JSONL
//!
//! [doc-06 §2-8] [agent-core/12 §2,§5]

pub mod backup;
pub mod change_service;
pub mod database;
pub mod document_store;
pub mod git_service;
pub mod migrations;
pub mod patch_engine;
pub mod repositories;
pub mod review;
pub mod verification;
pub mod workspace_service;

// 重新导出核心类型
pub use backup::BackupManager;
pub use change_service::{ChangeService, ChangeSet, ChangeSetEntry, RollbackResult};
pub use database::Database;
pub use document_store::{BomKind, ConflictStatus, DocumentEntry, DocumentStore};
pub use git_service::{GitDiffResult, GitFileStatus, GitService, GitStatusKind};
pub use patch_engine::{apply_patch, hash_content, PatchError};
pub use repositories::{
    AgentRunRepository, BlobStore, QueuedMessageRepository, SessionBranchRepository,
    TaskEventStore, TaskRepository, ToolCallRepository, WorkspaceRepository,
};
pub use review::{
    AcceptReadiness, ReviewAction, ReviewActionResult, ReviewReadiness, ReviewService,
};
pub use verification::{VerificationConfig, VerificationService};
pub use workspace_service::WorkspaceService;
