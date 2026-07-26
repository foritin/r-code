//! VerificationService -- 验证命令执行与结果追踪。 [doc-18 M9]
//!
//! 运行项目验证命令（如 `npm test`、`cargo test`），追踪结果状态。
//! 支持超时、自动检测项目类型、以及前序验证的废弃/过期标记。
//!
//! ## 状态流转
//! - `Running` -> `Passed` / `Failed` / `Timeout`
//! - 新验证启动时：前序 `Running` -> `Superseded`
//! - 文件变更后：前序 `Passed` / `Failed` / `Timeout` -> `Stale`
//!
//! [doc-18 M9] [doc-06 §3.9]

use std::path::{Path, PathBuf};
use std::process::Stdio;

use chrono::{DateTime, Utc};
use r_code_core::dto::{VerificationRecord, VerificationStatus};
use r_code_core::error::ProductError;
use rusqlite::params;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::repositories::BlobStore;
use crate::Database;

/// VerificationService -- 运行验证命令并追踪结果。
///
/// 持有 `&Database` 引用与 `blobs_dir`（用于存储验证输出到 BlobStore）。
/// 与 [`crate::change_service::ChangeService`] 采用相同的持有模式。
pub struct VerificationService<'a> {
    db: &'a Database,
    blobs_dir: PathBuf,
}

/// 验证配置 -- 命令字符串与超时秒数。
#[derive(Debug, Clone)]
pub struct VerificationConfig {
    /// 验证命令（如 `"npm test"`、`"cargo test"`）
    pub command: String,
    /// 超时秒数
    pub timeout_secs: u64,
}

impl<'a> VerificationService<'a> {
    /// 创建 VerificationService。
    ///
    /// - `db`：SQLite 数据库引用
    /// - `blobs_dir`：Blob 存储目录（用于存储验证输出）
    pub fn new(db: &'a Database, blobs_dir: PathBuf) -> Self {
        Self { db, blobs_dir }
    }

    /// 检测项目类型并建议验证命令。
    ///
    /// 检查顺序：`package.json` -> `Cargo.toml` -> `pyproject.toml` / `setup.py`
    /// -> `Makefile`。均不匹配时返回 `None`。
    pub fn detect_config(project_root: &Path) -> Option<VerificationConfig> {
        if project_root.join("package.json").exists() {
            Some(VerificationConfig {
                command: "npm test".into(),
                timeout_secs: 300,
            })
        } else if project_root.join("Cargo.toml").exists() {
            Some(VerificationConfig {
                command: "cargo test".into(),
                timeout_secs: 600,
            })
        } else if project_root.join("pyproject.toml").exists()
            || project_root.join("setup.py").exists()
        {
            Some(VerificationConfig {
                command: "pytest".into(),
                timeout_secs: 300,
            })
        } else if project_root.join("Makefile").exists() {
            Some(VerificationConfig {
                command: "make test".into(),
                timeout_secs: 300,
            })
        } else {
            None
        }
    }

    /// 运行验证命令。
    ///
    /// 流程：
    /// 1. 将该任务下所有 `Running` 状态的验证标记为 `Superseded`。
    /// 2. 若文件在验证后发生变更，将前序验证标记为 `Stale`。
    /// 3. 创建新的 `Running` 验证记录。
    /// 4. 执行命令（带超时），捕获 stdout/stderr。
    /// 5. 将输出存入 BlobStore。
    /// 6. 更新记录为最终状态（`Passed` / `Failed` / `Timeout`）。
    pub async fn run_verification(
        &self,
        task_id: &str,
        run_id: &str,
        config: &VerificationConfig,
        working_dir: &Path,
    ) -> Result<VerificationRecord, ProductError> {
        // 1. 标记前序 Running 验证为 Superseded
        self.mark_running_superseded(task_id)?;

        // 2. 标记文件变更后的前序验证为 Stale
        self.mark_stale_if_changed(task_id)?;

        // 3. 创建新记录（Running）
        let mut record = VerificationRecord::new(task_id, run_id, &config.command);
        self.insert_record(&record)?;

        // 4. 执行命令
        let (exit_code, status, output) = execute_command(config, working_dir).await;

        // 5. 存储输出到 BlobStore（失败不阻塞记录更新）
        let blob_key = self.store_output(&output);

        // 6. 更新记录为最终状态
        record.exit_code = exit_code;
        record.status = status;
        record.output_blob_key = blob_key;
        record.ended_at = Some(Utc::now());
        self.update_record_final(&record)?;

        Ok(record)
    }

    /// 列出任务的所有验证记录（按开始时间降序、id 升序）。
    pub async fn list_for_task(
        &self,
        task_id: &str,
    ) -> Result<Vec<VerificationRecord>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, run_id, command, status, output_blob_key, exit_code, started_at, ended_at \
                 FROM verifications WHERE task_id = ?1 ORDER BY started_at DESC, id ASC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            records.push(row_to_verification(row)?);
        }
        Ok(records)
    }

    /// 获取任务的最新验证记录。
    pub async fn latest_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<VerificationRecord>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, run_id, command, status, output_blob_key, exit_code, started_at, ended_at \
                 FROM verifications WHERE task_id = ?1 ORDER BY started_at DESC, id ASC LIMIT 1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_verification(row)?)),
            None => Ok(None),
        }
    }

    // ------------------------------------------------------------------
    // 内部辅助
    // ------------------------------------------------------------------

    /// 将任务下所有 `Running` 状态的验证标记为 `Superseded`，并设置 `ended_at`。
    fn mark_running_superseded(&self, task_id: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE verifications SET status = 'superseded', ended_at = ?1 \
             WHERE task_id = ?2 AND status = 'running'",
            params![Utc::now().to_rfc3339(), task_id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 若文件在验证后变更，将前序 `Passed` / `Failed` / `Timeout` 验证标记为 `Stale`。
    ///
    /// 通过关联子查询检查是否存在 `created_at > verifications.started_at` 的
    /// `file_changes` 记录。RFC 3339 字符串比较在此有效（统一格式）。
    fn mark_stale_if_changed(&self, task_id: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE verifications SET status = 'stale' \
             WHERE task_id = ?1 AND status IN ('passed', 'failed', 'timeout') \
             AND EXISTS (\
                 SELECT 1 FROM file_changes \
                 WHERE file_changes.task_id = ?1 \
                   AND file_changes.created_at > verifications.started_at\
             )",
            params![task_id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 插入验证记录（初始 `Running` 状态）。
    fn insert_record(&self, record: &VerificationRecord) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO verifications \
             (id, task_id, run_id, command, status, output_blob_key, exit_code, started_at, ended_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.id,
                record.task_id,
                record.run_id,
                record.command,
                record.status.to_string(),
                record.output_blob_key,
                record.exit_code,
                record.started_at.to_rfc3339(),
                record.ended_at.map(|dt| dt.to_rfc3339()),
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 更新验证记录为最终状态。
    fn update_record_final(&self, record: &VerificationRecord) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE verifications SET status = ?1, output_blob_key = ?2, exit_code = ?3, ended_at = ?4 \
             WHERE id = ?5",
            params![
                record.status.to_string(),
                record.output_blob_key,
                record.exit_code,
                record.ended_at.map(|dt| dt.to_rfc3339()),
                record.id,
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 将输出存入 BlobStore，返回 blob key。存储失败时返回 `None`（不阻塞验证记录）。
    fn store_output(&self, output: &str) -> Option<String> {
        let blob_store = BlobStore::new(self.db, self.blobs_dir.clone());
        match blob_store.put(output.as_bytes()) {
            Ok(hash) => {
                if let Err(e) = blob_store.increment_ref(&hash) {
                    tracing::warn!(
                        error = %e,
                        "failed to increment blob ref for verification output"
                    );
                }
                Some(hash)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to store verification output in blob store"
                );
                None
            }
        }
    }
}

// ============================================================================
// 自由函数
// ============================================================================

/// 执行验证命令，返回 `(退出码, 状态, 合并输出)`。
///
/// 命令由当前平台的系统 shell 执行，因而能处理带引号、重定向与组合命令；
/// 该入口只用于用户显式发起的、已在受信任工作区内的验证。stdout 与 stderr 合并存储。
/// 超时后子进程被 `kill` 并返回 `Timeout` 状态。
async fn execute_command(
    config: &VerificationConfig,
    working_dir: &Path,
) -> (Option<i32>, VerificationStatus, String) {
    if config.command.trim().is_empty() {
        return (
            None,
            VerificationStatus::Failed,
            "empty command".to_string(),
        );
    }

    #[cfg(windows)]
    let mut cmd = {
        let mut command = Command::new("cmd.exe");
        command.arg("/C").arg(&config.command);
        command
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut command = Command::new("/bin/sh");
        command.arg("-lc").arg(&config.command);
        command
    };
    cmd.current_dir(working_dir);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                None,
                VerificationStatus::Failed,
                format!("failed to spawn command '{}': {e}", config.command),
            )
        }
    };

    // 不使用 `wait_with_output()`：超时场景下必须保留 Child 句柄，Windows 才能
    // 用 taskkill 结束 cmd.exe 拉起的整棵进程树，而不留下 ping / node 等后代。
    let stdout_task = child.stdout.take().map(|mut pipe| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes).await;
            bytes
        })
    });
    let stderr_task = child.stderr.take().map(|mut pipe| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes).await;
            bytes
        })
    });
    let pid = child.id();
    let timeout_dur = std::time::Duration::from_secs(config.timeout_secs);
    let (exit_code, status, timeout_message) =
        match tokio::time::timeout(timeout_dur, child.wait()).await {
            Ok(Ok(exit_status)) => (
                exit_status.code(),
                if exit_status.success() {
                    VerificationStatus::Passed
                } else {
                    VerificationStatus::Failed
                },
                None,
            ),
            Ok(Err(e)) => (
                None,
                VerificationStatus::Failed,
                Some(format!("command wait failed: {e}")),
            ),
            Err(_) => {
                #[cfg(windows)]
                if let Some(pid) = pid {
                    let _ = Command::new("taskkill")
                        .args(["/PID", &pid.to_string(), "/T", "/F"])
                        .output()
                        .await;
                }
                #[cfg(not(windows))]
                {
                    let _ = child.kill().await;
                }
                let _ = child.wait().await;
                (
                    None,
                    VerificationStatus::Timeout,
                    Some("verification timed out".to_string()),
                )
            }
        };

    let stdout = match stdout_task {
        Some(task) => task.await.unwrap_or_default(),
        None => Vec::new(),
    };
    let stderr = match stderr_task {
        Some(task) => task.await.unwrap_or_default(),
        None => Vec::new(),
    };
    let mut combined =
        timeout_message.unwrap_or_else(|| String::from_utf8_lossy(&stdout).to_string());
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push_str("\n--- stderr ---\n");
        }
        combined.push_str(&String::from_utf8_lossy(&stderr));
    }
    (exit_code, status, combined)
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 将 rusqlite 错误转换为 ProductError。
fn db_err(e: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(e.to_string())
}

/// 解析 RFC 3339 时间戳为 `DateTime<Utc>`。
fn parse_ts(s: &str) -> Result<DateTime<Utc>, ProductError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| ProductError::DatabaseError(format!("timestamp parse error: {e}")))
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

/// 将数据库行映射为 `VerificationRecord`。
///
/// 列顺序：id, task_id, run_id, command, status, output_blob_key, exit_code,
/// started_at, ended_at
fn row_to_verification(row: &rusqlite::Row<'_>) -> Result<VerificationRecord, ProductError> {
    let status_str: String = row.get(4).map_err(db_err)?;
    let status = parse_verification_status(&status_str)?;
    let started_str: String = row.get(7).map_err(db_err)?;
    let started_at = parse_ts(&started_str)?;
    let ended_str: Option<String> = row.get(8).map_err(db_err)?;
    let ended_at = match ended_str {
        Some(s) => Some(parse_ts(&s)?),
        None => None,
    };

    Ok(VerificationRecord {
        id: row.get(0).map_err(db_err)?,
        task_id: row.get(1).map_err(db_err)?,
        run_id: row.get(2).map_err(db_err)?,
        command: row.get(3).map_err(db_err)?,
        status,
        output_blob_key: row.get(5).map_err(db_err)?,
        exit_code: row.get(6).map_err(db_err)?,
        started_at,
        ended_at,
    })
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
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
            let task = Task::new(Some("/proj".into()), "Test", "test goal", TaskMode::Auto);
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

        fn service(&self) -> VerificationService<'_> {
            VerificationService::new(&self.db, self.blobs_dir.clone())
        }

        /// 创建 tool_call 记录（含关联的 agent_run）以满足外键约束。
        fn create_tool_call(&self, id: &str) -> String {
            let conn = self.db.conn().unwrap();
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO tool_calls (id, run_id, task_id, tool_name, input_json, risk_level, started_at) \
                 VALUES (?1, ?2, ?3, 'test_tool', '{}', 'R1', ?4)",
                params![id, self.run.id, self.task.id, now],
            )
            .unwrap();
            id.to_string()
        }
    }

    /// 直接向 verifications 表插入一条记录（绕过服务层，用于测试前置状态）。
    fn insert_verification_raw(db: &Database, record: &VerificationRecord) {
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO verifications \
             (id, task_id, run_id, command, status, output_blob_key, exit_code, started_at, ended_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.id,
                record.task_id,
                record.run_id,
                record.command,
                record.status.to_string(),
                record.output_blob_key,
                record.exit_code,
                record.started_at.to_rfc3339(),
                record.ended_at.map(|dt| dt.to_rfc3339()),
            ],
        )
        .unwrap();
    }

    fn successful_command() -> &'static str {
        #[cfg(windows)]
        {
            "exit /B 0"
        }
        #[cfg(not(windows))]
        {
            "true"
        }
    }

    fn failing_command() -> &'static str {
        #[cfg(windows)]
        {
            "exit /B 7"
        }
        #[cfg(not(windows))]
        {
            "false"
        }
    }

    fn sleeping_command() -> &'static str {
        #[cfg(windows)]
        {
            "ping -n 11 127.0.0.1 > NUL"
        }
        #[cfg(not(windows))]
        {
            "sleep 10"
        }
    }

    // ── detect_config ──────────────────────────────────────────────

    #[test]
    fn detect_config_npm() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let config = VerificationService::detect_config(dir.path()).unwrap();
        assert_eq!(config.command, "npm test");
    }

    #[test]
    fn detect_config_cargo() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let config = VerificationService::detect_config(dir.path()).unwrap();
        assert_eq!(config.command, "cargo test");
    }

    #[test]
    fn detect_config_python_pyproject() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\n").unwrap();
        let config = VerificationService::detect_config(dir.path()).unwrap();
        assert_eq!(config.command, "pytest");
    }

    #[test]
    fn detect_config_python_setup_py() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("setup.py"),
            "from setuptools import setup\n",
        )
        .unwrap();
        let config = VerificationService::detect_config(dir.path()).unwrap();
        assert_eq!(config.command, "pytest");
    }

    #[test]
    fn detect_config_makefile() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Makefile"), "test:\n\techo hi\n").unwrap();
        let config = VerificationService::detect_config(dir.path()).unwrap();
        assert_eq!(config.command, "make test");
    }

    #[test]
    fn detect_config_none() {
        let dir = TempDir::new().unwrap();
        assert!(VerificationService::detect_config(dir.path()).is_none());
    }

    #[test]
    fn detect_config_priority_npm_over_cargo() {
        // package.json 优先于 Cargo.toml
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let config = VerificationService::detect_config(dir.path()).unwrap();
        assert_eq!(config.command, "npm test");
    }

    // ── run_verification ───────────────────────────────────────────

    #[tokio::test]
    async fn run_verification_pass() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let config = VerificationConfig {
            command: successful_command().into(),
            timeout_secs: 5,
        };
        let record = fx
            .service()
            .run_verification(&fx.task.id, &fx.run.id, &config, dir.path())
            .await
            .unwrap();

        assert_eq!(record.status, VStatus::Passed);
        assert_eq!(record.exit_code, Some(0));
        assert!(record.ended_at.is_some());
        assert!(record.output_blob_key.is_some());
    }

    #[tokio::test]
    async fn run_verification_fail() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let config = VerificationConfig {
            command: failing_command().into(),
            timeout_secs: 5,
        };
        let record = fx
            .service()
            .run_verification(&fx.task.id, &fx.run.id, &config, dir.path())
            .await
            .unwrap();

        assert_eq!(record.status, VStatus::Failed);
        assert!(record.exit_code.is_some());
        assert_ne!(record.exit_code, Some(0));
        assert!(record.ended_at.is_some());
    }

    #[tokio::test]
    async fn run_verification_timeout() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let config = VerificationConfig {
            command: sleeping_command().into(),
            timeout_secs: 1,
        };
        let record = fx
            .service()
            .run_verification(&fx.task.id, &fx.run.id, &config, dir.path())
            .await
            .unwrap();

        assert_eq!(record.status, VStatus::Timeout);
        assert!(record.exit_code.is_none());
        assert!(record.ended_at.is_some());
    }

    #[tokio::test]
    async fn run_verification_spawn_error() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let config = VerificationConfig {
            command: "this-command-does-not-exist-xyz".into(),
            timeout_secs: 5,
        };
        let record = fx
            .service()
            .run_verification(&fx.task.id, &fx.run.id, &config, dir.path())
            .await
            .unwrap();

        assert_eq!(record.status, VStatus::Failed);
        // Shell 会为「找不到命令」返回非零退出码；直接 spawn 失败时则没有退出码。
        assert!(record.exit_code.map(|code| code != 0).unwrap_or(true));
    }

    // ── supersede 行为 ─────────────────────────────────────────────

    #[tokio::test]
    async fn run_verification_supersedes_running() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();

        // 手动插入一条 Running 状态的验证
        let mut running = VerificationRecord::new(&fx.task.id, &fx.run.id, "sleep 100");
        running.status = VStatus::Running;
        insert_verification_raw(&fx.db, &running);

        // 运行新验证
        let config = VerificationConfig {
            command: successful_command().into(),
            timeout_secs: 5,
        };
        let record = fx
            .service()
            .run_verification(&fx.task.id, &fx.run.id, &config, dir.path())
            .await
            .unwrap();

        // 旧记录应被标记为 Superseded
        let verifications = fx.service().list_for_task(&fx.task.id).await.unwrap();
        let old = verifications.iter().find(|v| v.id == running.id).unwrap();
        assert_eq!(old.status, VStatus::Superseded);
        assert!(old.ended_at.is_some());

        // 新记录应为 Passed
        assert_eq!(record.status, VStatus::Passed);
    }

    // ── stale 标记 ─────────────────────────────────────────────────

    #[tokio::test]
    async fn run_verification_marks_stale() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();

        // 插入一条 Passed 验证，started_at 设为 10 秒前
        let mut passed = VerificationRecord::new(&fx.task.id, &fx.run.id, "true");
        passed.status = VStatus::Passed;
        passed.exit_code = Some(0);
        passed.started_at = Utc::now() - Duration::seconds(10);
        passed.ended_at = Some(Utc::now() - Duration::seconds(9));
        insert_verification_raw(&fx.db, &passed);

        // 记录一条文件变更（created_at = now，晚于验证的 started_at）
        let tool_call_id = fx.create_tool_call("tc-stale");
        let change_svc = ChangeService::new(&fx.db, fx.blobs_dir.clone());
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hello").unwrap();
        change_svc
            .record_change(
                &fx.task.id,
                &file,
                FileChangeType::Modify,
                Some(&tool_call_id),
                Some(b"hello"),
                Some(b"modified"),
                None,
            )
            .await
            .unwrap();

        // 运行新验证
        let config = VerificationConfig {
            command: "true".into(),
            timeout_secs: 5,
        };
        let _record = fx
            .service()
            .run_verification(&fx.task.id, &fx.run.id, &config, dir.path())
            .await
            .unwrap();

        // 旧验证应被标记为 Stale
        let verifications = fx.service().list_for_task(&fx.task.id).await.unwrap();
        let old = verifications.iter().find(|v| v.id == passed.id).unwrap();
        assert_eq!(old.status, VStatus::Stale);
    }

    #[tokio::test]
    async fn run_verification_no_stale_without_changes() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();

        // 插入一条 Passed 验证（started_at = 10 秒前），但不记录任何文件变更
        let mut passed = VerificationRecord::new(&fx.task.id, &fx.run.id, "true");
        passed.status = VStatus::Passed;
        passed.exit_code = Some(0);
        passed.started_at = Utc::now() - Duration::seconds(10);
        passed.ended_at = Some(Utc::now() - Duration::seconds(9));
        insert_verification_raw(&fx.db, &passed);

        let config = VerificationConfig {
            command: "true".into(),
            timeout_secs: 5,
        };
        fx.service()
            .run_verification(&fx.task.id, &fx.run.id, &config, dir.path())
            .await
            .unwrap();

        // 无文件变更 -> 旧验证不应被标记为 Stale
        let verifications = fx.service().list_for_task(&fx.task.id).await.unwrap();
        let old = verifications.iter().find(|v| v.id == passed.id).unwrap();
        assert_eq!(old.status, VStatus::Passed);
    }

    // ── list / latest ──────────────────────────────────────────────

    #[tokio::test]
    async fn list_for_task_returns_sorted() {
        let fx = Fixture::new();

        // 插入两条验证（手动设置不同时间）
        let mut v1 = VerificationRecord::new(&fx.task.id, &fx.run.id, "cmd1");
        v1.status = VStatus::Passed;
        v1.exit_code = Some(0);
        v1.started_at = Utc::now() - Duration::seconds(20);
        v1.ended_at = Some(Utc::now() - Duration::seconds(19));
        insert_verification_raw(&fx.db, &v1);

        let mut v2 = VerificationRecord::new(&fx.task.id, &fx.run.id, "cmd2");
        v2.status = VStatus::Failed;
        v2.exit_code = Some(1);
        v2.started_at = Utc::now() - Duration::seconds(10);
        v2.ended_at = Some(Utc::now() - Duration::seconds(9));
        insert_verification_raw(&fx.db, &v2);

        let list = fx.service().list_for_task(&fx.task.id).await.unwrap();
        assert_eq!(list.len(), 2);
        // 按 started_at DESC -> v2 在前
        assert_eq!(list[0].id, v2.id);
        assert_eq!(list[1].id, v1.id);
    }

    #[tokio::test]
    async fn latest_for_task() {
        let fx = Fixture::new();

        // 无验证时返回 None
        assert!(fx
            .service()
            .latest_for_task(&fx.task.id)
            .await
            .unwrap()
            .is_none());

        // 插入两条验证
        let mut v1 = VerificationRecord::new(&fx.task.id, &fx.run.id, "old");
        v1.status = VStatus::Passed;
        v1.exit_code = Some(0);
        v1.started_at = Utc::now() - Duration::seconds(10);
        v1.ended_at = Some(Utc::now() - Duration::seconds(9));
        insert_verification_raw(&fx.db, &v1);

        let mut v2 = VerificationRecord::new(&fx.task.id, &fx.run.id, "new");
        v2.status = VStatus::Failed;
        v2.exit_code = Some(1);
        v2.started_at = Utc::now();
        v2.ended_at = Some(Utc::now());
        insert_verification_raw(&fx.db, &v2);

        let latest = fx
            .service()
            .latest_for_task(&fx.task.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, v2.id);
    }
}
