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
pub mod lifecycle_purge;
pub mod memory_store;
pub mod migrations;
pub mod patch_engine;
pub mod plan_review;
pub mod plan_store;
pub mod repositories;
pub mod review;
pub mod review_git;
pub mod verification;
pub mod workspace_service;

// 重新导出核心类型
pub use backup::BackupManager;
pub use change_service::{
    ChangeService, ChangeSet, ChangeSetEntry, NewRunWorkspaceSnapshot, RollbackResult,
    RunWorkspaceSnapshot,
};
pub use database::Database;
pub use document_store::{BomKind, ConflictStatus, DocumentEntry, DocumentStore};
pub use git_service::{
    GitDiffResult, GitFileStatus, GitService, GitStatusKind, GitTreeChange, GitTreeChangeKind,
};
pub use lifecycle_purge::{
    AppDataPruneReport, LifecyclePurgeResult, LifecyclePurgeStore, PURGE_REJECT_IN_PROGRESS,
};
pub use memory_store::{
    render_snapshot, sanitize_review_text, CapturedMemoryTurn, LoadedMemorySnapshot,
    MemoryCandidateView, MemoryEntryDraft, MemoryEntryEdit, MemoryOverview, MemoryReviewClaim,
    MemoryReviewJobView, MemoryStore,
};
pub use patch_engine::{apply_patch, hash_content, PatchError};
pub use plan_review::{
    CoordinatedWriteGuard, EnhancedReviewEventView, EnhancedReviewFileView,
    EnhancedReviewGroupView, EnhancedReviewTarget, EnhancedReviewView, FinishPlanWriteInput,
    OsPlanReviewFileSystem, PathCoordinator, PathLease, PlanFeatureRef, PlanRejectRecoveryReport,
    PlanRejectResult, PlanReviewFileSystem, PlanReviewStore, PlanRevisionRef, PlanWriteGuard,
    RecordPlanWriteOutcome, PLAN_REVIEW_FEATURE_NOT_TERMINAL,
};
pub use plan_store::{
    PlanStore, PLAN_CONTINUATION_INTERRUPTED, PLAN_IMPLEMENTATION_DISPATCH_INTERRUPTED,
};
pub use repositories::{
    AgentRunRepository, BlobStore, NotificationRepository, QueuedMessageRepository,
    SessionBranchRepository, TaskEventStore, TaskRepository, ToolCallRepository,
    WorkspaceRepository,
};
pub use review::{
    AcceptReadiness, ReviewAction, ReviewActionResult, ReviewReadiness, ReviewService,
};
pub use review_git::{
    review_line_id, GitCommitResult, GitDeliveryStatus, GitPushResult, ReviewAcceptResult,
    ReviewDecision, ReviewDiffLine, ReviewDiffLineKind, ReviewFileSnapshot, ReviewGitService,
    ReviewGitStatus, ReviewLedgerService, ReviewPathStatus, ReviewStatus,
};
pub use verification::{VerificationConfig, VerificationService};
pub use workspace_service::WorkspaceService;
