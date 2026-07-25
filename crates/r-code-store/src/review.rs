//! ReviewService -- 审查状态管理与文件 / Hunk 级接受 / 拒绝。 [doc-18 M8-05]
//!
//! 管理任务进入审查就绪（`ReviewReady`）状态的条件检查，以及用户对变更的
//! 接受（Accept）/ 拒绝（Reject）操作。
//!
//! ## 核心职责
//! - **就绪检查**：无 pending 权限请求 + 所有文件变更有基线
//! - **接受 / 拒绝**：文件级 / Hunk 级操作，拒绝时回滚到基线
//! - **接受就绪**：Auto 模式验证通过自动接受；Edit 模式需用户确认
//!
//! [doc-18 M8-05] [doc-06 §3.2]

use std::path::{Path, PathBuf};

use r_code_core::dto::{ReviewState, TaskMode, VerificationStatus};
use r_code_core::error::ProductError;
use rusqlite::params;

use crate::change_service::{ChangeService, RollbackResult};
use crate::repositories::AgentRunRepository;
use crate::Database;

/// ReviewService -- 管理审查状态与文件 / Hunk 级接受 / 拒绝。
///
/// 持有 `&Database` 引用与 `blobs_dir`（拒绝操作时构造 [`ChangeService`] 回滚文件）。
pub struct ReviewService<'a> {
    db: &'a Database,
    blobs_dir: PathBuf,
}

/// 审查操作 -- 对文件或 Hunk 的接受 / 拒绝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewAction {
    /// 接受所有变更
    AcceptAll,
    /// 接受指定文件的变更
    AcceptFile(String),
    /// 拒绝（回滚）指定文件
    RejectFile(String),
    /// 接受指定文件中的某个 Hunk
    AcceptHunk {
        /// 文件路径
        path: String,
        /// Hunk 索引（0-based）
        hunk_index: usize,
    },
    /// 拒绝（回滚）指定文件中的某个 Hunk
    RejectHunk {
        /// 文件路径
        path: String,
        /// Hunk 索引（0-based）
        hunk_index: usize,
    },
}

/// 审查就绪检查结果。
#[derive(Debug, Clone)]
pub struct ReviewReadiness {
    /// 是否就绪
    pub ready: bool,
    /// 阻塞原因列表（空 = 就绪）
    pub blockers: Vec<String>,
}

/// 应用审查操作的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewActionResult {
    /// 已接受（任务或文件级别的接受完成）
    Accepted,
    /// 已拒绝（文件已回滚到基线）
    Rejected,
    /// 需要更多操作（仍有未审查的文件）
    NeedsMoreActions,
}

/// 接受就绪检查结果。
#[derive(Debug, Clone)]
pub struct AcceptReadiness {
    /// 是否可以接受
    pub can_accept: bool,
    /// 是否需要用户确认
    pub needs_user_confirm: bool,
    /// 原因说明
    pub reason: String,
}

impl<'a> ReviewService<'a> {
    /// 创建 ReviewService。
    ///
    /// - `db`：SQLite 数据库引用
    /// - `blobs_dir`：Blob 存储目录（用于拒绝操作时回滚文件）
    pub fn new(db: &'a Database, blobs_dir: PathBuf) -> Self {
        Self { db, blobs_dir }
    }

    /// 检查任务是否可以进入 `ReviewReady` 状态。
    ///
    /// 验证：
    /// 1. 无 pending 权限请求（高风险工具调用已全部审批）
    /// 2. 所有文件变更有基线（可安全回滚）
    pub async fn can_enter_review(&self, task_id: &str) -> Result<ReviewReadiness, ProductError> {
        let mut blockers = Vec::new();

        // 1. 检查无 pending 权限请求
        let pending_count = self.count_pending_permissions(task_id)?;
        if pending_count > 0 {
            blockers.push(format!("{pending_count} pending permission request(s)"));
        }

        // 2. 检查所有文件变更有基线
        let missing = self.find_changes_without_baselines(task_id)?;
        if !missing.is_empty() {
            blockers.push(format!(
                "{} file(s) without baseline: {}",
                missing.len(),
                missing.join(", ")
            ));
        }

        Ok(ReviewReadiness {
            ready: blockers.is_empty(),
            blockers,
        })
    }

    /// 应用审查操作。
    ///
    /// - `AcceptAll`：将活跃 Run 的审查状态设为 `Accepted`
    /// - `AcceptFile` / `AcceptHunk`：保留变更（无磁盘操作）
    /// - `RejectFile` / `RejectHunk`：回滚文件到基线（via [`ChangeService`])
    pub async fn apply_action(
        &self,
        task_id: &str,
        action: ReviewAction,
    ) -> Result<ReviewActionResult, ProductError> {
        match action {
            ReviewAction::AcceptAll => {
                self.set_review_state(task_id, ReviewState::Accepted)?;
                Ok(ReviewActionResult::Accepted)
            }
            ReviewAction::AcceptFile(_) => {
                // 接受 = 保留变更，无需磁盘操作
                Ok(ReviewActionResult::Accepted)
            }
            ReviewAction::RejectFile(path) => {
                let svc = self.change_service();
                let result = svc.rollback_file(task_id, Path::new(&path)).await?;
                self.classify_rollback_result(result)
            }
            ReviewAction::AcceptHunk { .. } => {
                // Hunk 级接受 = 保留变更（简化实现；精确 hunk 保留需要 patch 合并）
                Ok(ReviewActionResult::Accepted)
            }
            ReviewAction::RejectHunk { path, .. } => {
                // Hunk 级拒绝：简化为整文件回滚（精确 hunk 回滚需要 patch 反向应用）
                let svc = self.change_service();
                let result = svc.rollback_file(task_id, Path::new(&path)).await?;
                self.classify_rollback_result(result)
            }
        }
    }

    /// 检查任务是否可以被接受。
    ///
    /// - **Auto 模式**：最新验证 `Passed` -> 可自动接受；否则不可接受
    /// - **Edit 模式**：验证 `Passed` -> 可接受；否则需用户确认
    /// - **Ask 模式**：无文件变更，可直接接受
    pub async fn check_accept_readiness(
        &self,
        task_id: &str,
        mode: &TaskMode,
    ) -> Result<AcceptReadiness, ProductError> {
        let latest_status = self.latest_verification_status(task_id)?;
        match mode {
            TaskMode::Auto => match latest_status {
                Some(VerificationStatus::Passed) => Ok(AcceptReadiness {
                    can_accept: true,
                    needs_user_confirm: false,
                    reason: "verification passed; auto-accept".to_string(),
                }),
                Some(status) => Ok(AcceptReadiness {
                    can_accept: false,
                    needs_user_confirm: false,
                    reason: format!("verification status: {status}"),
                }),
                None => Ok(AcceptReadiness {
                    can_accept: false,
                    needs_user_confirm: false,
                    reason: "no verification run".to_string(),
                }),
            },
            TaskMode::Edit => match latest_status {
                Some(VerificationStatus::Passed) => Ok(AcceptReadiness {
                    can_accept: true,
                    needs_user_confirm: false,
                    reason: "verification passed".to_string(),
                }),
                _ => Ok(AcceptReadiness {
                    can_accept: true,
                    needs_user_confirm: true,
                    reason: "user confirmation required (not verified)".to_string(),
                }),
            },
            TaskMode::Ask => Ok(AcceptReadiness {
                can_accept: true,
                needs_user_confirm: false,
                reason: "ask mode; no changes to verify".to_string(),
            }),
        }
    }

    // ------------------------------------------------------------------
    // 内部辅助
    // ------------------------------------------------------------------

    /// 构造临时 ChangeService（用于回滚文件）。
    fn change_service(&self) -> ChangeService<'_> {
        ChangeService::new(self.db, self.blobs_dir.clone())
    }

    /// 将 `RollbackResult` 分类为 `ReviewActionResult` 或错误。
    fn classify_rollback_result(
        &self,
        result: RollbackResult,
    ) -> Result<ReviewActionResult, ProductError> {
        match result {
            RollbackResult::Restored { .. } | RollbackResult::AlreadyClean { .. } => {
                Ok(ReviewActionResult::Rejected)
            }
            RollbackResult::ConflictDetected { reason, .. } => {
                Err(ProductError::RollbackError(reason))
            }
            RollbackResult::NoBaseline { path } => Err(ProductError::BaselineError(format!(
                "no baseline for {path}"
            ))),
        }
    }

    /// 统计任务的 pending 权限请求数量。
    fn count_pending_permissions(&self, task_id: &str) -> Result<i64, ProductError> {
        let conn = self.db.conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM permission_requests \
                 WHERE task_id = ?1 AND decision = 'pending'",
                params![task_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        Ok(count)
    }

    /// 查找有变更但无基线的文件路径。
    fn find_changes_without_baselines(&self, task_id: &str) -> Result<Vec<String>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT fc.path FROM file_changes fc \
                 LEFT JOIN file_baselines fb \
                   ON fb.task_id = fc.task_id AND fb.path = fc.path \
                 WHERE fc.task_id = ?1 AND fb.id IS NULL",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        let mut paths = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            let path: String = row.get(0).map_err(db_err)?;
            paths.push(path);
        }
        Ok(paths)
    }

    /// 设置任务活跃 Run 的审查状态。
    fn set_review_state(
        &self,
        task_id: &str,
        review_state: ReviewState,
    ) -> Result<(), ProductError> {
        let repo = AgentRunRepository::new(self.db);
        let run = repo.get_active_run(task_id)?.ok_or_else(|| {
            ProductError::StateMachineError(format!("no active run for task {task_id}"))
        })?;
        repo.update_review_state(&run.id, review_state)?;
        Ok(())
    }

    /// 查询任务的最新验证状态。
    fn latest_verification_status(
        &self,
        task_id: &str,
    ) -> Result<Option<VerificationStatus>, ProductError> {
        let conn = self.db.conn()?;
        let status_str: Option<String> = conn
            .query_row(
                "SELECT status FROM verifications WHERE task_id = ?1 \
                 ORDER BY started_at DESC, id ASC LIMIT 1",
                params![task_id],
                |row| row.get(0),
            )
            .ok();
        match status_str {
            Some(s) => Ok(Some(parse_verification_status(&s)?)),
            None => Ok(None),
        }
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 将 rusqlite 错误转换为 ProductError。
fn db_err(e: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(e.to_string())
}

/// 解析 `VerificationStatus` 字符串。
fn parse_verification_status(s: &str) -> Result<VerificationStatus, ProductError> {
    match s {
        "running" => Ok(VerificationStatus::Running),
        "passed" => Ok(VerificationStatus::Passed),
        "failed" => Ok(VerificationStatus::Failed),
        "superseded" => Ok(VerificationStatus::Superseded),
        "stale" => Ok(VerificationStatus::Stale),
        "timeout" => Ok(VerificationStatus::Timeout),
        _ => Err(ProductError::DatabaseError(format!(
            "invalid verification status: {s}"
        ))),
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use r_code_core::dto::{
        AgentRun, FileChangeType, Task, TaskMode, VerificationStatus as VStatus,
    };
    use tempfile::TempDir;

    use crate::change_service::ChangeService;
    use crate::repositories::{AgentRunRepository, TaskRepository};

    /// 测试夹具：内存数据库 + 临时 blob 目录 + 已创建的任务与 Run。
    struct Fixture {
        db: Database,
        blobs_dir: PathBuf,
        _tmp: TempDir,
        task: Task,
        run: AgentRun,
    }

    impl Fixture {
        fn new() -> Self {
            let db = Database::open_in_memory().unwrap();
            let tmp = TempDir::new().unwrap();
            let blobs_dir = tmp.path().join("blobs");
            std::fs::create_dir_all(&blobs_dir).unwrap();
            let task = Task::new("/proj", "Test", "test goal", TaskMode::Edit);
            TaskRepository::new(&db).create(&task).unwrap();
            let run = AgentRun::new(&task.id, "test-model");
            AgentRunRepository::new(&db).create(&run).unwrap();
            Self {
                db,
                blobs_dir,
                _tmp: tmp,
                task,
                run,
            }
        }

        fn service(&self) -> ReviewService<'_> {
            ReviewService::new(&self.db, self.blobs_dir.clone())
        }

        /// 创建 tool_call 记录以满足 permission_requests 的外键约束。
        fn create_tool_call(&self, id: &str) -> String {
            let conn = self.db.conn().unwrap();
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO tool_calls (id, run_id, task_id, tool_name, input_json, risk_level, started_at) \
                 VALUES (?1, ?2, ?3, 'test_tool', '{}', 'R2', ?4)",
                params![id, self.run.id, self.task.id, now],
            )
            .unwrap();
            id.to_string()
        }

        /// 插入一条 pending 权限请求。
        fn create_pending_permission(&self, tool_call_id: &str) {
            let conn = self.db.conn().unwrap();
            let now = Utc::now().to_rfc3339();
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO permission_requests \
                 (id, task_id, tool_call_id, tool_name, risk_level, input_summary, decision, created_at) \
                 VALUES (?1, ?2, ?3, 'test_tool', 'R2', 'test input', 'pending', ?4)",
                params![id, self.task.id, tool_call_id, now],
            )
            .unwrap();
        }

        /// 插入一条验证记录。
        fn insert_verification(&self, status: VStatus) {
            let conn = self.db.conn().unwrap();
            let now = Utc::now();
            let started = (now - Duration::seconds(5)).to_rfc3339();
            let ended = now.to_rfc3339();
            let id = uuid::Uuid::new_v4().to_string();
            let exit_code = if status == VStatus::Passed {
                Some(0)
            } else {
                Some(1)
            };
            conn.execute(
                "INSERT INTO verifications \
                 (id, task_id, run_id, command, status, exit_code, started_at, ended_at) \
                 VALUES (?1, ?2, ?3, 'test', ?4, ?5, ?6, ?7)",
                params![
                    id,
                    self.task.id,
                    self.run.id,
                    status.to_string(),
                    exit_code,
                    started,
                    ended
                ],
            )
            .unwrap();
        }
    }

    // ── can_enter_review ───────────────────────────────────────────

    #[tokio::test]
    async fn can_enter_review_ready() {
        let fx = Fixture::new();
        // 无 pending 权限、无文件变更 -> 就绪
        let readiness = fx.service().can_enter_review(&fx.task.id).await.unwrap();
        assert!(readiness.ready);
        assert!(readiness.blockers.is_empty());
    }

    #[tokio::test]
    async fn can_enter_review_blocked_by_pending_permission() {
        let fx = Fixture::new();
        let tc = fx.create_tool_call("tc-pending");
        fx.create_pending_permission(&tc);

        let readiness = fx.service().can_enter_review(&fx.task.id).await.unwrap();
        assert!(!readiness.ready);
        assert!(readiness
            .blockers
            .iter()
            .any(|b| b.contains("pending permission")));
    }

    #[tokio::test]
    async fn can_enter_review_blocked_by_missing_baseline() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hello").unwrap();

        // 记录变更但不捕获基线
        let tc = fx.create_tool_call("tc-nobaseline");
        let change_svc = ChangeService::new(&fx.db, fx.blobs_dir.clone());
        change_svc
            .record_change(
                &fx.task.id,
                &file,
                FileChangeType::Modify,
                Some(&tc),
                Some(b"hello"),
                Some(b"modified"),
                None,
            )
            .await
            .unwrap();

        let readiness = fx.service().can_enter_review(&fx.task.id).await.unwrap();
        assert!(!readiness.ready);
        assert!(readiness
            .blockers
            .iter()
            .any(|b| b.contains("without baseline")));
    }

    #[tokio::test]
    async fn can_enter_review_ready_with_baseline() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hello").unwrap();

        let tc = fx.create_tool_call("tc-withbaseline");
        let change_svc = ChangeService::new(&fx.db, fx.blobs_dir.clone());
        // 先捕获基线
        change_svc
            .capture_baseline(&fx.task.id, &file)
            .await
            .unwrap();
        // 再记录变更
        std::fs::write(&file, b"modified").unwrap();
        change_svc
            .record_change(
                &fx.task.id,
                &file,
                FileChangeType::Modify,
                Some(&tc),
                Some(b"hello"),
                Some(b"modified"),
                None,
            )
            .await
            .unwrap();

        // 有基线 -> 就绪
        let readiness = fx.service().can_enter_review(&fx.task.id).await.unwrap();
        assert!(readiness.ready);
    }

    // ── apply_action ───────────────────────────────────────────────

    #[tokio::test]
    async fn apply_action_accept_all() {
        let fx = Fixture::new();
        let result = fx
            .service()
            .apply_action(&fx.task.id, ReviewAction::AcceptAll)
            .await
            .unwrap();
        assert_eq!(result, ReviewActionResult::Accepted);

        // 验证 Run 审查状态已更新
        let run = AgentRunRepository::new(&fx.db)
            .get(&fx.run.id)
            .unwrap()
            .unwrap();
        assert_eq!(run.review_state, ReviewState::Accepted);
    }

    #[tokio::test]
    async fn apply_action_accept_file() {
        let fx = Fixture::new();
        let result = fx
            .service()
            .apply_action(
                &fx.task.id,
                ReviewAction::AcceptFile("src/main.rs".to_string()),
            )
            .await
            .unwrap();
        assert_eq!(result, ReviewActionResult::Accepted);
    }

    #[tokio::test]
    async fn apply_action_reject_file() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"original").unwrap();

        let change_svc = ChangeService::new(&fx.db, fx.blobs_dir.clone());
        change_svc
            .capture_baseline(&fx.task.id, &file)
            .await
            .unwrap();
        std::fs::write(&file, b"modified").unwrap();
        let tc = fx.create_tool_call("tc-reject");
        change_svc
            .record_change(
                &fx.task.id,
                &file,
                FileChangeType::Modify,
                Some(&tc),
                Some(b"original"),
                Some(b"modified"),
                None,
            )
            .await
            .unwrap();

        let path_str = file.to_string_lossy().to_string();
        let result = fx
            .service()
            .apply_action(&fx.task.id, ReviewAction::RejectFile(path_str))
            .await
            .unwrap();
        assert_eq!(result, ReviewActionResult::Rejected);

        // 文件应被回滚到基线内容
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "original");
    }

    #[tokio::test]
    async fn apply_action_reject_file_no_baseline() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"content").unwrap();

        let path_str = file.to_string_lossy().to_string();
        let result = fx
            .service()
            .apply_action(&fx.task.id, ReviewAction::RejectFile(path_str))
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProductError::BaselineError(_)
        ));
    }

    #[tokio::test]
    async fn apply_action_accept_hunk() {
        let fx = Fixture::new();
        let result = fx
            .service()
            .apply_action(
                &fx.task.id,
                ReviewAction::AcceptHunk {
                    path: "src/main.rs".to_string(),
                    hunk_index: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(result, ReviewActionResult::Accepted);
    }

    #[tokio::test]
    async fn apply_action_reject_hunk() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"original").unwrap();

        let change_svc = ChangeService::new(&fx.db, fx.blobs_dir.clone());
        change_svc
            .capture_baseline(&fx.task.id, &file)
            .await
            .unwrap();
        std::fs::write(&file, b"modified").unwrap();
        let tc = fx.create_tool_call("tc-hunk");
        change_svc
            .record_change(
                &fx.task.id,
                &file,
                FileChangeType::Modify,
                Some(&tc),
                Some(b"original"),
                Some(b"modified"),
                None,
            )
            .await
            .unwrap();

        let path_str = file.to_string_lossy().to_string();
        let result = fx
            .service()
            .apply_action(
                &fx.task.id,
                ReviewAction::RejectHunk {
                    path: path_str,
                    hunk_index: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(result, ReviewActionResult::Rejected);

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "original");
    }

    // ── check_accept_readiness ────────────────────────────────────

    #[tokio::test]
    async fn check_accept_readiness_auto_passed() {
        let fx = Fixture::new();
        fx.insert_verification(VStatus::Passed);

        let readiness = fx
            .service()
            .check_accept_readiness(&fx.task.id, &TaskMode::Auto)
            .await
            .unwrap();
        assert!(readiness.can_accept);
        assert!(!readiness.needs_user_confirm);
    }

    #[tokio::test]
    async fn check_accept_readiness_auto_failed() {
        let fx = Fixture::new();
        fx.insert_verification(VStatus::Failed);

        let readiness = fx
            .service()
            .check_accept_readiness(&fx.task.id, &TaskMode::Auto)
            .await
            .unwrap();
        assert!(!readiness.can_accept);
    }

    #[tokio::test]
    async fn check_accept_readiness_auto_no_verification() {
        let fx = Fixture::new();
        let readiness = fx
            .service()
            .check_accept_readiness(&fx.task.id, &TaskMode::Auto)
            .await
            .unwrap();
        assert!(!readiness.can_accept);
    }

    #[tokio::test]
    async fn check_accept_readiness_edit_unverified() {
        let fx = Fixture::new();
        // 无验证 -> Edit 模式需用户确认
        let readiness = fx
            .service()
            .check_accept_readiness(&fx.task.id, &TaskMode::Edit)
            .await
            .unwrap();
        assert!(readiness.can_accept);
        assert!(readiness.needs_user_confirm);
    }

    #[tokio::test]
    async fn check_accept_readiness_edit_verified() {
        let fx = Fixture::new();
        fx.insert_verification(VStatus::Passed);

        let readiness = fx
            .service()
            .check_accept_readiness(&fx.task.id, &TaskMode::Edit)
            .await
            .unwrap();
        assert!(readiness.can_accept);
        assert!(!readiness.needs_user_confirm);
    }

    #[tokio::test]
    async fn check_accept_readiness_ask() {
        let fx = Fixture::new();
        let readiness = fx
            .service()
            .check_accept_readiness(&fx.task.id, &TaskMode::Ask)
            .await
            .unwrap();
        assert!(readiness.can_accept);
        assert!(!readiness.needs_user_confirm);
    }
}
