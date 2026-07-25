//! 崩溃恢复管理 -- 启动时扫描中断的任务 [doc-18 M10]。
//!
//! Worker / 主进程崩溃后，可能留下 `ended_at IS NULL` 的 agent run 记录与
//! `decision = 'pending'` 的权限请求。本模块在启动时扫描这些中断状态，
//! 供 UI 展示恢复页面，并清理孤儿权限。
//!
//! 注意：本模块直接通过 `rusqlite::Connection` 访问数据库（不走连接池），
//! 因为它运行在启动早期、连接池尚未初始化之时。
//!
//! [doc-18 M10] [doc-04 §3.3]

use std::collections::HashSet;
use std::path::PathBuf;

use r_code_core::dto::TaskState;
use r_code_core::error::ProductError;
use rusqlite::Connection;

/// 一个被中断的任务（worker 崩溃后重建）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct InterruptedTask {
    /// 所属 Task ID
    pub task_id: String,
    /// 关联的 Session ID（SQLite schema 不存储，恒为 None；JSONL 恢复时填充）
    pub session_id: Option<String>,
    /// 崩溃前的最后任务状态
    pub last_state: TaskState,
    /// 崩溃时活跃的 Agent Run ID
    pub last_run_id: Option<String>,
}

/// 恢复管理器 -- 启动时扫描中断的任务。
pub struct RecoveryManager {
    db_path: PathBuf,
}

impl RecoveryManager {
    /// 创建恢复管理器。`db_path` 指向产品 SQLite 数据库文件。
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    /// 打开一个直接连接（非连接池），设置 busy_timeout 以容忍并发读。
    fn connect(&self) -> Result<Connection, ProductError> {
        let conn = Connection::open(&self.db_path)
            .map_err(|e| ProductError::DatabaseError(format!("recovery open failed: {e}")))?;
        conn.execute_batch("PRAGMA busy_timeout=5000;")
            .map_err(|e| ProductError::DatabaseError(format!("set busy_timeout: {e}")))?;
        Ok(conn)
    }

    /// 扫描中断的任务。
    ///
    /// 查询状态非 `Idle`/`Archived` 且存在活跃 run（`ended_at IS NULL`）的任务。
    /// 这些任务在崩溃前正在进行，需要恢复。
    ///
    /// On startup, marks orphaned pending permissions as cancelled.
    /// Returns list of tasks that need recovery.
    pub async fn scan_interrupted(&self) -> Result<Vec<InterruptedTask>, ProductError> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT t.id, t.state, ar.id \
                 FROM tasks t \
                 JOIN agent_runs ar ON ar.task_id = t.id \
                 WHERE t.state NOT IN ('idle', 'archived') AND ar.ended_at IS NULL \
                 ORDER BY t.updated_at DESC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query([]).map_err(db_err)?;

        let mut tasks = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            let task_id: String = row.get(0).map_err(db_err)?;
            if !seen.insert(task_id.clone()) {
                // 同一 task 的重复活跃 run（违反不变量时防御性去重）
                continue;
            }
            let state_str: String = row.get(1).map_err(db_err)?;
            let last_state = TaskState::try_from_str(&state_str).ok_or_else(|| {
                ProductError::DatabaseError(format!("invalid task state: {state_str}"))
            })?;
            let last_run_id: Option<String> = row.get(2).map_err(db_err)?;
            tasks.push(InterruptedTask {
                task_id,
                session_id: None,
                last_state,
                last_run_id,
            });
        }
        Ok(tasks)
    }

    /// 生成恢复页面数据结构（供 UI 展示）。
    pub async fn recovery_page_data(&self) -> Result<RecoveryPageData, ProductError> {
        let interrupted_tasks = self.scan_interrupted().await?;
        let orphaned_permissions = self.count_orphaned_permissions()?;
        Ok(RecoveryPageData {
            total_items: interrupted_tasks.len() + orphaned_permissions as usize,
            interrupted_tasks,
            orphaned_permissions,
        })
    }

    /// 取消孤儿权限请求 -- 将所有 `pending` 权限请求标记为 `deny`。
    ///
    /// 返回受影响的行数。
    pub async fn cancel_orphaned_permissions(&self) -> Result<u64, ProductError> {
        let conn = self.connect()?;
        let affected = conn
            .execute(
                "UPDATE permission_requests \
                 SET decision = 'deny', decided_at = datetime('now') \
                 WHERE decision = 'pending'",
                [],
            )
            .map_err(db_err)?;
        Ok(affected as u64)
    }

    /// 统计孤儿权限请求数量（不修改）。
    fn count_orphaned_permissions(&self) -> Result<u64, ProductError> {
        let conn = self.connect()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM permission_requests WHERE decision = 'pending'",
                [],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        Ok(count.max(0) as u64)
    }
}

/// 恢复页面数据 -- 供 UI 渲染恢复界面。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecoveryPageData {
    /// 中断的任务列表
    pub interrupted_tasks: Vec<InterruptedTask>,
    /// 孤儿权限请求数量
    pub orphaned_permissions: u64,
    /// 总待恢复条目数（中断任务 + 孤儿权限）
    pub total_items: usize,
}

/// 将 rusqlite 错误转换为 ProductError。
fn db_err(e: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::{AgentRun, Task, TaskMode};
    use r_code_store::{AgentRunRepository, Database, TaskRepository};
    use tempfile::TempDir;

    /// 构造一个已迁移的临时数据库并返回 (TempDir, db_path, Database)。
    /// 保持 Database 存活以持有连接池；RecoveryManager 通过独立连接访问同一文件。
    fn setup_db() -> (TempDir, PathBuf, Database) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (dir, db_path, db)
    }

    fn create_task(db: &Database, id: &str, state: TaskState) -> Task {
        let mut task = Task::new("/proj", format!("task {id}"), "goal", TaskMode::Edit);
        task.id = id.to_string();
        task.state = state;
        TaskRepository::new(db).create(&task).unwrap();
        task
    }

    fn create_active_run(db: &Database, task_id: &str, run_id: &str) {
        let mut run = AgentRun::new(task_id, "test-model");
        run.id = run_id.to_string();
        // ended_at 留 None -> 活跃 run
        AgentRunRepository::new(db).create(&run).unwrap();
    }

    fn create_finished_run(db: &Database, task_id: &str, run_id: &str) {
        use r_code_core::dto::ReviewState;
        let mut run = AgentRun::new(task_id, "test-model");
        run.id = run_id.to_string();
        run.finish(ReviewState::Accepted);
        AgentRunRepository::new(db).create(&run).unwrap();
    }

    // ── scan_interrupted ──────────────────────────────────────────

    #[tokio::test]
    async fn scan_finds_interrupted_task_with_active_run() {
        let (_dir, db_path, db) = setup_db();
        create_task(&db, "t1", TaskState::InProgress);
        create_active_run(&db, "t1", "r1");

        let mgr = RecoveryManager::new(db_path);
        let interrupted = mgr.scan_interrupted().await.unwrap();

        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].task_id, "t1");
        assert_eq!(interrupted[0].last_state, TaskState::InProgress);
        assert_eq!(interrupted[0].last_run_id.as_deref(), Some("r1"));
        assert!(interrupted[0].session_id.is_none());
    }

    #[tokio::test]
    async fn scan_ignores_idle_task_even_with_active_run() {
        let (_dir, db_path, db) = setup_db();
        create_task(&db, "t1", TaskState::Idle);
        create_active_run(&db, "t1", "r1");

        let mgr = RecoveryManager::new(db_path);
        let interrupted = mgr.scan_interrupted().await.unwrap();
        assert!(interrupted.is_empty());
    }

    #[tokio::test]
    async fn scan_ignores_archived_task() {
        let (_dir, db_path, db) = setup_db();
        create_task(&db, "t1", TaskState::Archived);
        create_active_run(&db, "t1", "r1");

        let mgr = RecoveryManager::new(db_path);
        let interrupted = mgr.scan_interrupted().await.unwrap();
        assert!(interrupted.is_empty());
    }

    #[tokio::test]
    async fn scan_ignores_task_with_finished_run() {
        let (_dir, db_path, db) = setup_db();
        create_task(&db, "t1", TaskState::InProgress);
        create_finished_run(&db, "t1", "r1");

        let mgr = RecoveryManager::new(db_path);
        let interrupted = mgr.scan_interrupted().await.unwrap();
        assert!(interrupted.is_empty());
    }

    #[tokio::test]
    async fn scan_returns_multiple_interrupted_tasks() {
        let (_dir, db_path, db) = setup_db();
        create_task(&db, "t1", TaskState::InProgress);
        create_active_run(&db, "t1", "r1");
        create_task(&db, "t2", TaskState::Exploring);
        create_active_run(&db, "t2", "r2");

        let mgr = RecoveryManager::new(db_path);
        let interrupted = mgr.scan_interrupted().await.unwrap();
        let ids: Vec<&str> = interrupted.iter().map(|t| t.task_id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"t1"));
        assert!(ids.contains(&"t2"));
    }

    #[tokio::test]
    async fn scan_empty_db_returns_empty() {
        let (_dir, db_path, _db) = setup_db();
        let mgr = RecoveryManager::new(db_path);
        let interrupted = mgr.scan_interrupted().await.unwrap();
        assert!(interrupted.is_empty());
    }

    #[tokio::test]
    async fn scan_review_ready_task_with_active_run() {
        let (_dir, db_path, db) = setup_db();
        create_task(&db, "t1", TaskState::ReviewReady);
        create_active_run(&db, "t1", "r1");

        let mgr = RecoveryManager::new(db_path);
        let interrupted = mgr.scan_interrupted().await.unwrap();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].last_state, TaskState::ReviewReady);
    }

    // ── cancel_orphaned_permissions ───────────────────────────────

    #[tokio::test]
    async fn cancel_orphaned_permissions_denies_pending() {
        let (_dir, db_path, db) = setup_db();
        insert_pending_permission(&db, "t1", "r1", "tc1");
        insert_pending_permission(&db, "t2", "r2", "tc2");

        let mgr = RecoveryManager::new(db_path);
        let affected = mgr.cancel_orphaned_permissions().await.unwrap();
        assert_eq!(affected, 2);

        // 再次取消应返回 0（已无 pending）
        let again = mgr.cancel_orphaned_permissions().await.unwrap();
        assert_eq!(again, 0);
    }

    #[tokio::test]
    async fn cancel_orphaned_permissions_skips_decided() {
        let (_dir, db_path, db) = setup_db();
        insert_pending_permission(&db, "t1", "r1", "tc1");
        insert_decided_permission(&db, "t2", "r2", "tc2");

        let mgr = RecoveryManager::new(db_path);
        let affected = mgr.cancel_orphaned_permissions().await.unwrap();
        assert_eq!(affected, 1);
    }

    #[tokio::test]
    async fn cancel_orphaned_permissions_empty_db() {
        let (_dir, db_path, _db) = setup_db();
        let mgr = RecoveryManager::new(db_path);
        let affected = mgr.cancel_orphaned_permissions().await.unwrap();
        assert_eq!(affected, 0);
    }

    // ── recovery_page_data ────────────────────────────────────────

    #[tokio::test]
    async fn recovery_page_data_aggregates() {
        let (_dir, db_path, db) = setup_db();
        create_task(&db, "t1", TaskState::InProgress);
        create_active_run(&db, "t1", "r1");
        insert_pending_permission(&db, "t1", "r1", "tc1");
        insert_pending_permission(&db, "t1", "r1", "tc2");

        let mgr = RecoveryManager::new(db_path);
        let page = mgr.recovery_page_data().await.unwrap();
        assert_eq!(page.interrupted_tasks.len(), 1);
        assert_eq!(page.orphaned_permissions, 2);
        assert_eq!(page.total_items, 3);
    }

    #[tokio::test]
    async fn recovery_page_data_empty() {
        let (_dir, db_path, _db) = setup_db();
        let mgr = RecoveryManager::new(db_path);
        let page = mgr.recovery_page_data().await.unwrap();
        assert!(page.interrupted_tasks.is_empty());
        assert_eq!(page.orphaned_permissions, 0);
        assert_eq!(page.total_items, 0);
    }

    #[test]
    fn recovery_page_data_is_serializable() {
        let data = RecoveryPageData {
            interrupted_tasks: vec![InterruptedTask {
                task_id: "t1".into(),
                session_id: None,
                last_state: TaskState::InProgress,
                last_run_id: Some("r1".into()),
            }],
            orphaned_permissions: 2,
            total_items: 3,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("interrupted_tasks"));
        assert!(json.contains("in_progress"));
        assert!(json.contains("orphaned_permissions"));
    }

    // ---- 辅助：插入权限请求 ----

    fn insert_pending_permission(db: &Database, task_id: &str, run_id: &str, tc_id: &str) {
        insert_permission(db, task_id, run_id, tc_id, "pending");
    }

    fn insert_decided_permission(db: &Database, task_id: &str, run_id: &str, tc_id: &str) {
        insert_permission(db, task_id, run_id, tc_id, "allow");
    }

    fn insert_permission(db: &Database, task_id: &str, run_id: &str, tc_id: &str, decision: &str) {
        let conn = db.conn().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        // 确保依赖行存在（幂等，不覆盖已有状态）
        conn.execute(
            "INSERT OR IGNORE INTO tasks (id, project_id, title, goal, mode, state, created_at, updated_at) \
             VALUES (?1, '/proj', 't', 'g', 'ask', 'in_progress', ?2, ?2)",
            rusqlite::params![task_id, now],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO agent_runs (id, task_id, model, review_state, started_at) \
             VALUES (?1, ?2, 'test-model', 'pending', ?3)",
            rusqlite::params![run_id, task_id, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tool_calls (id, run_id, task_id, tool_name, input_json, risk_level, status, started_at) \
             VALUES (?1, ?2, ?3, 'test_tool', '{}', 'r2', 'running', ?4)",
            rusqlite::params![tc_id, run_id, task_id, now],
        )
        .unwrap();
        let decided_at: Option<&str> = if decision == "pending" {
            None
        } else {
            Some(&now)
        };
        let perm_id = format!("perm-{tc_id}");
        conn.execute(
            "INSERT INTO permission_requests (id, task_id, tool_call_id, tool_name, risk_level, input_summary, decision, created_at, decided_at) \
             VALUES (?1, ?2, ?3, 'test_tool', 'r2', 'summary', ?4, ?5, ?6)",
            rusqlite::params![perm_id, task_id, tc_id, decision, now, decided_at],
        )
        .unwrap();
    }
}
