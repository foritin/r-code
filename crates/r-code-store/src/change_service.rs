//! ChangeService -- 文件基线、变更记录与回滚引擎。 [doc-06 §3.4-3.5] [doc-12 §3]
//!
//! 所有写入操作通过 ChangeService 管理，绝不直接写磁盘。
//!
//! ## 核心职责
//! - **基线捕获**（capture-once）：任务首次可写动作前捕获文件快照
//! - **变更记录**：create/modify/delete/rename 的 before/after hash
//! - **变更集折叠**：合并中间补丁，计算净效果
//! - **回滚**：byte-exact 恢复到基线，带外部变更检测
//!
//! ## 回滚安全
//! 回滚前检查磁盘哈希与预期哈希是否一致。若不一致（外部编辑），
//! 返回 [`RollbackResult::ConflictDetected`]，不执行回滚。
//!
//! [doc-06 §3.4-3.6] [doc-12 §3] [doc-18 M5-04]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use r_code_core::dto::{FileBaseline, FileChange, FileChangeType};
use r_code_core::error::ProductError;
use rusqlite::params;

use crate::patch_engine::hash_content;
use crate::repositories::BlobStore;
use crate::Database;

/// ChangeService -- 管理文件基线、变更记录与回滚。
///
/// 所有写入通过 ChangeService，绝不直接写磁盘。
pub struct ChangeService<'a> {
    db: &'a Database,
    blobs_dir: PathBuf,
}

impl<'a> ChangeService<'a> {
    /// 创建 ChangeService。
    ///
    /// - `db`：SQLite 数据库引用
    /// - `blobs_dir`：Blob 存储目录（与 BlobStore 一致）
    pub fn new(db: &'a Database, blobs_dir: PathBuf) -> Self {
        Self { db, blobs_dir }
    }

    /// 捕获文件基线（capture-once 语义）。
    ///
    /// 若 (task_id, path) 的基线已存在，**不覆盖**，直接返回既有基线。
    /// 读取文件内容，存入 BlobStore，创建 FileBaseline 记录。
    pub async fn capture_baseline(
        &self,
        task_id: &str,
        path: &Path,
    ) -> Result<FileBaseline, ProductError> {
        let path_str = path.to_string_lossy().to_string();

        // capture-once：若已存在则直接返回
        if let Some(existing) = self.get_baseline(task_id, &path_str).await? {
            return Ok(existing);
        }

        // 读取文件内容
        let content = std::fs::read(path).map_err(|e| {
            ProductError::BaselineError(format!("failed to read file {}: {e}", path.display()))
        })?;

        // 存入 BlobStore 并递增引用
        let blob_store = BlobStore::new(self.db, self.blobs_dir.clone());
        let hash = blob_store.put(&content)?;
        blob_store.increment_ref(&hash)?;

        // 创建基线记录
        let baseline = FileBaseline::new(task_id, path_str.clone(), hash.clone());

        // INSERT OR IGNORE 处理竞态（UNIQUE(task_id, path) 约束）
        {
            let conn = self.db.conn()?;
            conn.execute(
                "INSERT OR IGNORE INTO file_baselines \
                 (id, task_id, path, content_hash, blob_key, captured_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    baseline.id,
                    baseline.task_id,
                    baseline.path,
                    baseline.content_hash,
                    baseline.blob_key,
                    baseline.captured_at.to_rfc3339(),
                ],
            )
            .map_err(db_err)?;

            if conn.changes() == 0 {
                // 竞态：另一个线程已插入。撤销刚才的 blob 引用。
                tracing::warn!(
                    task_id,
                    path = %path_str,
                    "baseline race detected; another insert won"
                );
                blob_store.decrement_ref(&hash)?;
                // 释放连接后重新查询
            } else {
                return Ok(baseline);
            }
        }

        // 竞态后重新获取已有基线
        self.get_baseline(task_id, &path_str)
            .await?
            .ok_or_else(|| ProductError::BaselineError("baseline vanished after race".into()))
    }

    /// 记录文件变更（create/modify/delete/rename）。
    ///
    /// 捕获 before_hash 与 after_hash，将内容存入 BlobStore。
    #[allow(clippy::too_many_arguments)]
    pub async fn record_change(
        &self,
        task_id: &str,
        path: &Path,
        change_type: FileChangeType,
        tool_call_id: Option<&str>,
        before_content: Option<&[u8]>,
        after_content: Option<&[u8]>,
        old_path: Option<&Path>,
    ) -> Result<FileChange, ProductError> {
        let blob_store = BlobStore::new(self.db, self.blobs_dir.clone());

        // 哈希并存入 before 内容
        let before_hash = if let Some(content) = before_content {
            let hash = blob_store.put(content)?;
            blob_store.increment_ref(&hash)?;
            Some(hash)
        } else {
            None
        };

        // 哈希并存入 after 内容
        let after_hash = if let Some(content) = after_content {
            let hash = blob_store.put(content)?;
            blob_store.increment_ref(&hash)?;
            Some(hash)
        } else {
            None
        };

        // 构造 FileChange 记录
        let mut change = FileChange::new(task_id, path.to_string_lossy(), change_type);
        change.tool_call_id = tool_call_id.map(|s| s.to_string());
        change.before_hash = before_hash;
        change.after_hash = after_hash;
        change.old_path = old_path.map(|p| p.to_string_lossy().to_string());

        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO file_changes \
             (id, task_id, tool_call_id, path, change_type, before_hash, after_hash, old_path, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                change.id,
                change.task_id,
                change.tool_call_id,
                change.path,
                change.change_type.to_string(),
                change.before_hash,
                change.after_hash,
                change.old_path,
                change.created_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;

        Ok(change)
    }

    /// 获取任务的所有变更（按时间升序）。
    pub async fn list_changes(&self, task_id: &str) -> Result<Vec<FileChange>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, tool_call_id, path, change_type, before_hash, after_hash, old_path, created_at \
                 FROM file_changes WHERE task_id = ?1 ORDER BY created_at ASC, id ASC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        let mut changes = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            changes.push(row_to_change(row)?);
        }
        Ok(changes)
    }

    /// 获取任务中某文件的基线。
    pub async fn get_baseline(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<FileBaseline>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, path, content_hash, blob_key, captured_at \
                 FROM file_baselines WHERE task_id = ?1 AND path = ?2",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id, path]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_baseline(row)?)),
            None => Ok(None),
        }
    }

    /// 获取任务的所有基线。
    pub async fn list_baselines(&self, task_id: &str) -> Result<Vec<FileBaseline>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, path, content_hash, blob_key, captured_at \
                 FROM file_baselines WHERE task_id = ?1 ORDER BY captured_at ASC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        let mut baselines = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            baselines.push(row_to_baseline(row)?);
        }
        Ok(baselines)
    }

    /// 计算净变更集（折叠中间补丁）。
    ///
    /// 返回每个文件路径的净效果。例如：
    /// - create + modify = create
    /// - modify + modify = modify（首 before_hash + 末 after_hash）
    /// - create + delete = no-op（不包含在结果中）
    pub async fn compute_change_set(&self, task_id: &str) -> Result<ChangeSet, ProductError> {
        let changes = self.list_changes(task_id).await?;
        let entries = fold_changes(&changes);
        Ok(ChangeSet {
            task_id: task_id.to_string(),
            entries,
        })
    }

    /// 回滚单个文件到基线（byte-exact 恢复）。
    ///
    /// 预检查外部变更：若磁盘哈希与预期不符，返回
    /// [`RollbackResult::ConflictDetected`]，不执行回滚。
    pub async fn rollback_file(
        &self,
        task_id: &str,
        path: &Path,
    ) -> Result<RollbackResult, ProductError> {
        let path_key = path.to_string_lossy().to_string();
        self.rollback_file_at(task_id, &path_key, path).await
    }

    /// 使用持久化路径键回滚一个文件。
    ///
    /// `path_key` 用于关联历史 baseline / change 记录，`physical_path` 才是实际
    /// IO 目标。调用方在完成路径边界校验后可借此兼容 Windows canonical 路径的
    /// verbatim 前缀，以及历史记录中保留的等价路径写法。
    pub async fn rollback_file_at(
        &self,
        task_id: &str,
        path_key: &str,
        physical_path: &Path,
    ) -> Result<RollbackResult, ProductError> {
        let path_str = path_key.to_string();

        // 1. 获取基线
        let baseline = match self.get_baseline(task_id, &path_str).await? {
            Some(b) => b,
            None => return Ok(RollbackResult::NoBaseline { path: path_str }),
        };

        // 2. 获取基线内容
        let blob_store = BlobStore::new(self.db, self.blobs_dir.clone());
        let baseline_content = blob_store.get(&baseline.blob_key)?.ok_or_else(|| {
            ProductError::RollbackError(format!("baseline blob not found: {}", baseline.blob_key))
        })?;

        // 3. 确定预期哈希（我们以为磁盘上的内容）
        //    无变更 -> 基线 content_hash；有变更 -> 最后一次变更的 after_hash
        let changes = self.list_changes(task_id).await?;
        let path_changes: Vec<&FileChange> =
            changes.iter().filter(|c| c.path == path_str).collect();

        let expected_hash: Option<&str> = if path_changes.is_empty() {
            Some(&baseline.content_hash)
        } else {
            path_changes.last().and_then(|c| c.after_hash.as_deref())
        };

        // 4. 读取实际磁盘状态
        let actual_hash: Option<String> = if physical_path.exists() {
            let content = std::fs::read(physical_path)?;
            Some(hash_content(&content))
        } else {
            None
        };

        // 5. 冲突检测 + 回滚
        match (expected_hash, &actual_hash) {
            (Some(exp), Some(act)) if exp == act => {
                // 磁盘与预期一致。检查是否已在基线状态。
                if *act == baseline.content_hash {
                    Ok(RollbackResult::AlreadyClean { path: path_str })
                } else {
                    // 写回基线内容
                    std::fs::write(physical_path, &baseline_content)?;
                    Ok(RollbackResult::Restored { path: path_str })
                }
            }
            (None, None) => {
                // 文件被我们删除且磁盘上确实不存在 -> 恢复基线
                std::fs::write(physical_path, &baseline_content)?;
                Ok(RollbackResult::Restored { path: path_str })
            }
            (Some(exp), None) => {
                // 文件应存在但磁盘上不存在 -> 外部删除
                Ok(RollbackResult::ConflictDetected {
                    path: path_str,
                    reason: format!("file externally deleted (expected hash: {exp})"),
                })
            }
            (None, Some(act)) => {
                // 文件应被删除但磁盘上存在 -> 外部创建/修改
                Ok(RollbackResult::ConflictDetected {
                    path: path_str,
                    reason: format!("file exists but expected to be deleted (actual hash: {act})"),
                })
            }
            (Some(exp), Some(act)) => {
                // 磁盘与预期不符 -> 外部修改
                Ok(RollbackResult::ConflictDetected {
                    path: path_str,
                    reason: format!("external modification (expected: {exp}, actual: {act})"),
                })
            }
        }
    }

    /// 回滚任务的所有变更（逆序处理各路径）。
    ///
    /// 对每个有变更的路径调用 [`rollback_file`]，返回各路径的结果。
    pub async fn rollback_task(&self, task_id: &str) -> Result<Vec<RollbackResult>, ProductError> {
        let changes = self.list_changes(task_id).await?;

        // 按最后修改的逆序收集去重路径
        let mut paths: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for change in changes.iter().rev() {
            if seen.insert(change.path.clone()) {
                paths.push(change.path.clone());
            }
        }

        let mut results = Vec::with_capacity(paths.len());
        for path_str in &paths {
            let result = self.rollback_file(task_id, Path::new(path_str)).await?;
            results.push(result);
        }
        Ok(results)
    }
}

// ============================================================================
// 变更集折叠
// ============================================================================

/// 折叠变更序列为净变更集。
///
/// 按 path 分组，每组按时间升序处理，根据首尾变更类型决定净效果。
fn fold_changes(changes: &[FileChange]) -> Vec<ChangeSetEntry> {
    let mut groups: HashMap<String, Vec<&FileChange>> = HashMap::new();
    for c in changes {
        groups.entry(c.path.clone()).or_default().push(c);
    }

    // 每组按时间升序排序
    for group in groups.values_mut() {
        group.sort_by_key(|a| a.created_at);
    }

    let mut entries: Vec<ChangeSetEntry> = Vec::new();
    for (path, group) in groups {
        let first = group[0];
        let last = *group.last().unwrap();

        let entry = match (first.change_type, last.change_type) {
            // create + delete = no-op
            (FileChangeType::Create, FileChangeType::Delete) => continue,

            // create + (create|modify|rename) = create（末次 after_hash）
            (FileChangeType::Create, _) => ChangeSetEntry {
                path: path.clone(),
                net_change_type: FileChangeType::Create,
                before_hash: None,
                after_hash: last.after_hash.clone(),
                old_path: None,
            },

            // modify + delete = delete（首次 before_hash）
            (FileChangeType::Modify, FileChangeType::Delete) => ChangeSetEntry {
                path: path.clone(),
                net_change_type: FileChangeType::Delete,
                before_hash: first.before_hash.clone(),
                after_hash: None,
                old_path: None,
            },

            // modify + (modify|create|rename) = modify（首 before + 末 after）
            (FileChangeType::Modify, _) => ChangeSetEntry {
                path: path.clone(),
                net_change_type: FileChangeType::Modify,
                before_hash: first.before_hash.clone(),
                after_hash: last.after_hash.clone(),
                old_path: None,
            },

            // delete + create = modify（原内容 -> 新内容）
            (FileChangeType::Delete, FileChangeType::Create) => ChangeSetEntry {
                path: path.clone(),
                net_change_type: FileChangeType::Modify,
                before_hash: first.before_hash.clone(),
                after_hash: last.after_hash.clone(),
                old_path: None,
            },

            // delete + (delete|modify|rename) = delete
            (FileChangeType::Delete, _) => ChangeSetEntry {
                path: path.clone(),
                net_change_type: FileChangeType::Delete,
                before_hash: first.before_hash.clone(),
                after_hash: None,
                old_path: None,
            },

            // rename + delete = delete（保留 old_path）
            (FileChangeType::Rename, FileChangeType::Delete) => ChangeSetEntry {
                path: path.clone(),
                net_change_type: FileChangeType::Delete,
                before_hash: first.before_hash.clone(),
                after_hash: None,
                old_path: first.old_path.clone(),
            },

            // rename + (rename|modify|create) = rename（首 old_path + 末 after_hash）
            (FileChangeType::Rename, _) => ChangeSetEntry {
                path: path.clone(),
                net_change_type: FileChangeType::Rename,
                before_hash: first.before_hash.clone(),
                after_hash: last.after_hash.clone(),
                old_path: first.old_path.clone(),
            },
        };
        entries.push(entry);
    }

    // 按路径排序，保证结果稳定
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 将 rusqlite 错误转换为 ProductError。
fn db_err(e: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(e.to_string())
}

/// 解析 RFC 3339 时间戳。
fn parse_ts(s: &str) -> Result<DateTime<Utc>, ProductError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| ProductError::DatabaseError(format!("timestamp parse error: {e}")))
}

/// 解析 FileChangeType 字符串。
fn parse_change_type(s: &str) -> Result<FileChangeType, ProductError> {
    match s {
        "create" => Ok(FileChangeType::Create),
        "modify" => Ok(FileChangeType::Modify),
        "delete" => Ok(FileChangeType::Delete),
        "rename" => Ok(FileChangeType::Rename),
        _ => Err(ProductError::DatabaseError(format!(
            "invalid change type: {s}"
        ))),
    }
}

/// 将数据库行映射为 FileChange。
///
/// 列顺序：id, task_id, tool_call_id, path, change_type, before_hash, after_hash, old_path, created_at
fn row_to_change(row: &rusqlite::Row<'_>) -> Result<FileChange, ProductError> {
    let change_type_str: String = row.get(4).map_err(db_err)?;
    let change_type = parse_change_type(&change_type_str)?;
    let created_str: String = row.get(8).map_err(db_err)?;
    let created_at = parse_ts(&created_str)?;

    Ok(FileChange {
        id: row.get(0).map_err(db_err)?,
        task_id: row.get(1).map_err(db_err)?,
        tool_call_id: row.get(2).map_err(db_err)?,
        path: row.get(3).map_err(db_err)?,
        change_type,
        before_hash: row.get(5).map_err(db_err)?,
        after_hash: row.get(6).map_err(db_err)?,
        old_path: row.get(7).map_err(db_err)?,
        created_at,
    })
}

/// 将数据库行映射为 FileBaseline。
///
/// 列顺序：id, task_id, path, content_hash, blob_key, captured_at
fn row_to_baseline(row: &rusqlite::Row<'_>) -> Result<FileBaseline, ProductError> {
    let captured_str: String = row.get(5).map_err(db_err)?;
    let captured_at = parse_ts(&captured_str)?;

    Ok(FileBaseline {
        id: row.get(0).map_err(db_err)?,
        task_id: row.get(1).map_err(db_err)?,
        path: row.get(2).map_err(db_err)?,
        content_hash: row.get(3).map_err(db_err)?,
        blob_key: row.get(4).map_err(db_err)?,
        captured_at,
    })
}

// ============================================================================
// 公共类型
// ============================================================================

/// 任务的净变更集（折叠中间补丁）。
#[derive(Debug, Clone)]
pub struct ChangeSet {
    /// 所属任务 ID
    pub task_id: String,
    /// 净变更条目
    pub entries: Vec<ChangeSetEntry>,
}

/// 单个净变更条目。
#[derive(Debug, Clone)]
pub struct ChangeSetEntry {
    /// 文件路径
    pub path: String,
    /// 净变更类型
    pub net_change_type: FileChangeType,
    /// 变更前哈希（create 时为 None）
    pub before_hash: Option<String>,
    /// 变更后哈希（delete 时为 None）
    pub after_hash: Option<String>,
    /// 重命名时的旧路径
    pub old_path: Option<String>,
}

/// 回滚操作结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackResult {
    /// 成功回滚到基线
    Restored {
        /// 文件路径
        path: String,
    },
    /// 文件已在基线状态（无需回滚）
    AlreadyClean {
        /// 文件路径
        path: String,
    },
    /// 检测到外部变更，回滚被阻止
    ConflictDetected {
        /// 文件路径
        path: String,
        /// 冲突原因
        reason: String,
    },
    /// 无基线（文件未被跟踪）
    NoBaseline {
        /// 文件路径
        path: String,
    },
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::{Task, TaskMode};
    use tempfile::TempDir;

    /// 测试夹具：内存数据库 + 临时 blob 目录 + 已创建的任务。
    struct Fixture {
        db: Database,
        blobs_dir: PathBuf,
        _tmp: TempDir,
        task: Task,
    }

    impl Fixture {
        fn new() -> Self {
            let db = Database::open_in_memory().unwrap();
            let tmp = TempDir::new().unwrap();
            let blobs_dir = tmp.path().join("blobs");
            std::fs::create_dir_all(&blobs_dir).unwrap();
            let task = Task::new(
                Some("/proj".into()),
                "Test Task",
                "test goal",
                TaskMode::Edit,
            );
            TaskRepository::new(&db).create(&task).unwrap();
            Self {
                db,
                blobs_dir,
                _tmp: tmp,
                task,
            }
        }

        fn service(&self) -> ChangeService<'_> {
            ChangeService::new(&self.db, self.blobs_dir.clone())
        }

        fn task_id(&self) -> &str {
            &self.task.id
        }

        /// 创建 tool_call 记录（含关联的 agent_run）以满足外键约束。
        fn create_tool_call(&self, tool_call_id: &str) -> String {
            let conn = self.db.conn().unwrap();
            let run_id = uuid::Uuid::new_v4().to_string();
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO agent_runs (id, task_id, model, review_state, started_at) \
                 VALUES (?1, ?2, 'test', 'pending', ?3)",
                params![run_id, self.task.id, now],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tool_calls (id, run_id, task_id, tool_name, input_json, risk_level, started_at) \
                 VALUES (?1, ?2, ?3, 'test_tool', '{}', 'R1', ?4)",
                params![tool_call_id, run_id, self.task.id, now],
            )
            .unwrap();
            tool_call_id.to_string()
        }
    }

    use crate::repositories::TaskRepository;

    // --------------------------------------------------------------------------
    // capture_baseline 测试
    // --------------------------------------------------------------------------

    #[tokio::test]
    async fn capture_baseline_creates_record() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hello").unwrap();

        let svc = fx.service();
        let baseline = svc.capture_baseline(fx.task_id(), &file).await.unwrap();

        assert_eq!(baseline.task_id, fx.task_id());
        assert_eq!(baseline.path, file.to_string_lossy());
        assert_eq!(baseline.content_hash, hash_content(b"hello"));
        assert_eq!(baseline.blob_key, baseline.content_hash);
    }

    #[tokio::test]
    async fn capture_baseline_once_semantics() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"original").unwrap();

        let svc = fx.service();

        // 第一次捕获
        let b1 = svc.capture_baseline(fx.task_id(), &file).await.unwrap();

        // 修改文件内容
        std::fs::write(&file, b"modified").unwrap();

        // 第二次捕获应返回第一次的基线（capture-once）
        let b2 = svc.capture_baseline(fx.task_id(), &file).await.unwrap();

        assert_eq!(b1.content_hash, b2.content_hash);
        assert_eq!(b1.content_hash, hash_content(b"original"));
        assert_ne!(b2.content_hash, hash_content(b"modified"));
        assert_eq!(b1.id, b2.id);
    }

    #[tokio::test]
    async fn capture_baseline_nonexistent_file() {
        let fx = Fixture::new();
        let svc = fx.service();
        let result = svc
            .capture_baseline(fx.task_id(), Path::new("/nonexistent/file.txt"))
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProductError::BaselineError(_)
        ));
    }

    #[tokio::test]
    async fn get_baseline_returns_captured() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"content").unwrap();

        let svc = fx.service();
        svc.capture_baseline(fx.task_id(), &file).await.unwrap();

        let fetched = svc
            .get_baseline(fx.task_id(), &file.to_string_lossy())
            .await
            .unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().content_hash, hash_content(b"content"));
    }

    #[tokio::test]
    async fn get_baseline_not_found() {
        let fx = Fixture::new();
        let svc = fx.service();
        let result = svc
            .get_baseline(fx.task_id(), "nonexistent.txt")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn list_baselines() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, b"a").unwrap();
        std::fs::write(&f2, b"b").unwrap();

        let svc = fx.service();
        svc.capture_baseline(fx.task_id(), &f1).await.unwrap();
        svc.capture_baseline(fx.task_id(), &f2).await.unwrap();

        let baselines = svc.list_baselines(fx.task_id()).await.unwrap();
        assert_eq!(baselines.len(), 2);
    }

    // --------------------------------------------------------------------------
    // record_change 测试
    // --------------------------------------------------------------------------

    #[tokio::test]
    async fn record_change_create() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("new.txt");

        let change = svc
            .record_change(
                fx.task_id(),
                path,
                FileChangeType::Create,
                None,
                None,
                Some(b"new content"),
                None,
            )
            .await
            .unwrap();

        assert_eq!(change.change_type, FileChangeType::Create);
        assert!(change.before_hash.is_none());
        assert_eq!(change.after_hash, Some(hash_content(b"new content")));
        assert!(change.old_path.is_none());
    }

    #[tokio::test]
    async fn record_change_modify() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("mod.txt");
        fx.create_tool_call("tc-1");

        let change = svc
            .record_change(
                fx.task_id(),
                path,
                FileChangeType::Modify,
                Some("tc-1"),
                Some(b"old"),
                Some(b"new"),
                None,
            )
            .await
            .unwrap();

        assert_eq!(change.change_type, FileChangeType::Modify);
        assert_eq!(change.before_hash, Some(hash_content(b"old")));
        assert_eq!(change.after_hash, Some(hash_content(b"new")));
        assert_eq!(change.tool_call_id, Some("tc-1".to_string()));
    }

    #[tokio::test]
    async fn record_change_delete() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("del.txt");

        let change = svc
            .record_change(
                fx.task_id(),
                path,
                FileChangeType::Delete,
                None,
                Some(b"deleted content"),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(change.change_type, FileChangeType::Delete);
        assert_eq!(change.before_hash, Some(hash_content(b"deleted content")));
        assert!(change.after_hash.is_none());
    }

    #[tokio::test]
    async fn record_change_rename() {
        let fx = Fixture::new();
        let svc = fx.service();
        let new_path = Path::new("renamed.txt");
        let old_path = Path::new("original.txt");

        let change = svc
            .record_change(
                fx.task_id(),
                new_path,
                FileChangeType::Rename,
                None,
                Some(b"content"),
                Some(b"content"),
                Some(old_path),
            )
            .await
            .unwrap();

        assert_eq!(change.change_type, FileChangeType::Rename);
        assert_eq!(change.old_path, Some("original.txt".to_string()));
        assert_eq!(change.path, "renamed.txt");
    }

    #[tokio::test]
    async fn list_changes_chronological() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("chrono.txt");

        let c1 = svc
            .record_change(
                fx.task_id(),
                path,
                FileChangeType::Create,
                None,
                None,
                Some(b"v1"),
                None,
            )
            .await
            .unwrap();

        // 确保 created_at 有差异
        std::thread::sleep(std::time::Duration::from_millis(10));

        let c2 = svc
            .record_change(
                fx.task_id(),
                path,
                FileChangeType::Modify,
                None,
                Some(b"v1"),
                Some(b"v2"),
                None,
            )
            .await
            .unwrap();

        let changes = svc.list_changes(fx.task_id()).await.unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].id, c1.id);
        assert_eq!(changes[1].id, c2.id);
    }

    // --------------------------------------------------------------------------
    // compute_change_set 测试
    // --------------------------------------------------------------------------

    #[tokio::test]
    async fn change_set_single_create() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("a.txt");

        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Create,
            None,
            None,
            Some(b"content"),
            None,
        )
        .await
        .unwrap();

        let cs = svc.compute_change_set(fx.task_id()).await.unwrap();
        assert_eq!(cs.entries.len(), 1);
        assert_eq!(cs.entries[0].net_change_type, FileChangeType::Create);
        assert!(cs.entries[0].before_hash.is_none());
        assert_eq!(cs.entries[0].after_hash, Some(hash_content(b"content")));
    }

    #[tokio::test]
    async fn change_set_create_then_modify_folds_to_create() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("a.txt");

        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Create,
            None,
            None,
            Some(b"v1"),
            None,
        )
        .await
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Modify,
            None,
            Some(b"v1"),
            Some(b"v2"),
            None,
        )
        .await
        .unwrap();

        let cs = svc.compute_change_set(fx.task_id()).await.unwrap();
        assert_eq!(cs.entries.len(), 1);
        assert_eq!(cs.entries[0].net_change_type, FileChangeType::Create);
        assert!(cs.entries[0].before_hash.is_none());
        assert_eq!(cs.entries[0].after_hash, Some(hash_content(b"v2")));
    }

    #[tokio::test]
    async fn change_set_create_then_delete_is_noop() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("a.txt");

        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Create,
            None,
            None,
            Some(b"content"),
            None,
        )
        .await
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Delete,
            None,
            Some(b"content"),
            None,
            None,
        )
        .await
        .unwrap();

        let cs = svc.compute_change_set(fx.task_id()).await.unwrap();
        assert!(cs.entries.is_empty(), "create+delete should be no-op");
    }

    #[tokio::test]
    async fn change_set_multiple_modifies_folds() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("a.txt");

        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Modify,
            None,
            Some(b"original"),
            Some(b"v1"),
            None,
        )
        .await
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Modify,
            None,
            Some(b"v1"),
            Some(b"v2"),
            None,
        )
        .await
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Modify,
            None,
            Some(b"v2"),
            Some(b"v3"),
            None,
        )
        .await
        .unwrap();

        let cs = svc.compute_change_set(fx.task_id()).await.unwrap();
        assert_eq!(cs.entries.len(), 1);
        assert_eq!(cs.entries[0].net_change_type, FileChangeType::Modify);
        assert_eq!(cs.entries[0].before_hash, Some(hash_content(b"original")));
        assert_eq!(cs.entries[0].after_hash, Some(hash_content(b"v3")));
    }

    #[tokio::test]
    async fn change_set_modify_then_delete_folds_to_delete() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("a.txt");

        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Modify,
            None,
            Some(b"orig"),
            Some(b"mod"),
            None,
        )
        .await
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Delete,
            None,
            Some(b"mod"),
            None,
            None,
        )
        .await
        .unwrap();

        let cs = svc.compute_change_set(fx.task_id()).await.unwrap();
        assert_eq!(cs.entries.len(), 1);
        assert_eq!(cs.entries[0].net_change_type, FileChangeType::Delete);
        assert_eq!(cs.entries[0].before_hash, Some(hash_content(b"orig")));
        assert!(cs.entries[0].after_hash.is_none());
    }

    #[tokio::test]
    async fn change_set_delete_then_create_folds_to_modify() {
        let fx = Fixture::new();
        let svc = fx.service();
        let path = Path::new("a.txt");

        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Delete,
            None,
            Some(b"old"),
            None,
            None,
        )
        .await
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        svc.record_change(
            fx.task_id(),
            path,
            FileChangeType::Create,
            None,
            None,
            Some(b"new"),
            None,
        )
        .await
        .unwrap();

        let cs = svc.compute_change_set(fx.task_id()).await.unwrap();
        assert_eq!(cs.entries.len(), 1);
        assert_eq!(cs.entries[0].net_change_type, FileChangeType::Modify);
        assert_eq!(cs.entries[0].before_hash, Some(hash_content(b"old")));
        assert_eq!(cs.entries[0].after_hash, Some(hash_content(b"new")));
    }

    #[tokio::test]
    async fn change_set_rename_preserves_old_path() {
        let fx = Fixture::new();
        let svc = fx.service();

        svc.record_change(
            fx.task_id(),
            Path::new("new.txt"),
            FileChangeType::Rename,
            None,
            Some(b"content"),
            Some(b"content"),
            Some(Path::new("old.txt")),
        )
        .await
        .unwrap();

        let cs = svc.compute_change_set(fx.task_id()).await.unwrap();
        assert_eq!(cs.entries.len(), 1);
        assert_eq!(cs.entries[0].net_change_type, FileChangeType::Rename);
        assert_eq!(cs.entries[0].old_path, Some("old.txt".to_string()));
        assert_eq!(cs.entries[0].path, "new.txt");
    }

    #[tokio::test]
    async fn change_set_multiple_paths() {
        let fx = Fixture::new();
        let svc = fx.service();

        svc.record_change(
            fx.task_id(),
            Path::new("b.txt"),
            FileChangeType::Create,
            None,
            None,
            Some(b"b"),
            None,
        )
        .await
        .unwrap();
        svc.record_change(
            fx.task_id(),
            Path::new("a.txt"),
            FileChangeType::Create,
            None,
            None,
            Some(b"a"),
            None,
        )
        .await
        .unwrap();

        let cs = svc.compute_change_set(fx.task_id()).await.unwrap();
        assert_eq!(cs.entries.len(), 2);
        // 按路径排序
        assert_eq!(cs.entries[0].path, "a.txt");
        assert_eq!(cs.entries[1].path, "b.txt");
    }

    // --------------------------------------------------------------------------
    // rollback_file 测试
    // --------------------------------------------------------------------------

    #[tokio::test]
    async fn rollback_file_restores_baseline() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"original").unwrap();

        let svc = fx.service();
        svc.capture_baseline(fx.task_id(), &file).await.unwrap();

        // 修改文件
        std::fs::write(&file, b"modified").unwrap();
        svc.record_change(
            fx.task_id(),
            &file,
            FileChangeType::Modify,
            None,
            Some(b"original"),
            Some(b"modified"),
            None,
        )
        .await
        .unwrap();

        // 回滚
        let result = svc.rollback_file(fx.task_id(), &file).await.unwrap();
        assert_eq!(
            result,
            RollbackResult::Restored {
                path: file.to_string_lossy().to_string()
            }
        );

        // 验证内容已恢复
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "original");
    }

    #[tokio::test]
    async fn rollback_file_already_clean() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"baseline").unwrap();

        let svc = fx.service();
        svc.capture_baseline(fx.task_id(), &file).await.unwrap();

        // 文件未被修改，直接回滚 -> AlreadyClean
        let result = svc.rollback_file(fx.task_id(), &file).await.unwrap();
        assert_eq!(
            result,
            RollbackResult::AlreadyClean {
                path: file.to_string_lossy().to_string()
            }
        );
    }

    #[tokio::test]
    async fn rollback_file_no_baseline() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("untracked.txt");
        std::fs::write(&file, b"content").unwrap();

        let svc = fx.service();
        let result = svc.rollback_file(fx.task_id(), &file).await.unwrap();
        assert_eq!(
            result,
            RollbackResult::NoBaseline {
                path: file.to_string_lossy().to_string()
            }
        );
    }

    #[tokio::test]
    async fn rollback_file_conflict_external_modification() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"original").unwrap();

        let svc = fx.service();
        svc.capture_baseline(fx.task_id(), &file).await.unwrap();

        // 我们记录一次修改
        std::fs::write(&file, b"our_change").unwrap();
        svc.record_change(
            fx.task_id(),
            &file,
            FileChangeType::Modify,
            None,
            Some(b"original"),
            Some(b"our_change"),
            None,
        )
        .await
        .unwrap();

        // 外部再次修改（我们不知道的变更）
        std::fs::write(&file, b"external_change").unwrap();

        // 回滚应检测到冲突
        let result = svc.rollback_file(fx.task_id(), &file).await.unwrap();
        assert!(matches!(result, RollbackResult::ConflictDetected { .. }));

        // 文件不应被修改
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "external_change");
    }

    #[tokio::test]
    async fn rollback_file_conflict_external_deletion() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"original").unwrap();

        let svc = fx.service();
        svc.capture_baseline(fx.task_id(), &file).await.unwrap();

        // 我们修改文件
        std::fs::write(&file, b"modified").unwrap();
        svc.record_change(
            fx.task_id(),
            &file,
            FileChangeType::Modify,
            None,
            Some(b"original"),
            Some(b"modified"),
            None,
        )
        .await
        .unwrap();

        // 外部删除文件
        std::fs::remove_file(&file).unwrap();

        let result = svc.rollback_file(fx.task_id(), &file).await.unwrap();
        assert!(matches!(result, RollbackResult::ConflictDetected { .. }));
    }

    #[tokio::test]
    async fn rollback_file_restores_after_delete() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"original").unwrap();

        let svc = fx.service();
        svc.capture_baseline(fx.task_id(), &file).await.unwrap();

        // 我们删除文件
        std::fs::remove_file(&file).unwrap();
        svc.record_change(
            fx.task_id(),
            &file,
            FileChangeType::Delete,
            None,
            Some(b"original"),
            None,
            None,
        )
        .await
        .unwrap();

        // 回滚应恢复文件
        let result = svc.rollback_file(fx.task_id(), &file).await.unwrap();
        assert_eq!(
            result,
            RollbackResult::Restored {
                path: file.to_string_lossy().to_string()
            }
        );

        // 文件应被恢复
        assert!(file.exists());
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "original");
    }

    // --------------------------------------------------------------------------
    // rollback_task 测试
    // --------------------------------------------------------------------------

    #[tokio::test]
    async fn rollback_task_multiple_files() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, b"original_a").unwrap();
        std::fs::write(&f2, b"original_b").unwrap();

        let svc = fx.service();
        svc.capture_baseline(fx.task_id(), &f1).await.unwrap();
        svc.capture_baseline(fx.task_id(), &f2).await.unwrap();

        // 修改两个文件
        std::fs::write(&f1, b"modified_a").unwrap();
        svc.record_change(
            fx.task_id(),
            &f1,
            FileChangeType::Modify,
            None,
            Some(b"original_a"),
            Some(b"modified_a"),
            None,
        )
        .await
        .unwrap();
        std::fs::write(&f2, b"modified_b").unwrap();
        svc.record_change(
            fx.task_id(),
            &f2,
            FileChangeType::Modify,
            None,
            Some(b"original_b"),
            Some(b"modified_b"),
            None,
        )
        .await
        .unwrap();

        // 回滚整个任务
        let results = svc.rollback_task(fx.task_id()).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| matches!(r, RollbackResult::Restored { .. })));

        // 验证两个文件都已恢复
        assert_eq!(std::fs::read_to_string(&f1).unwrap(), "original_a");
        assert_eq!(std::fs::read_to_string(&f2).unwrap(), "original_b");
    }

    #[tokio::test]
    async fn rollback_task_no_changes() {
        let fx = Fixture::new();
        let svc = fx.service();
        let results = svc.rollback_task(fx.task_id()).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn rollback_task_mixed_results() {
        let fx = Fixture::new();
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("tracked.txt");
        let f2 = dir.path().join("untracked.txt");
        std::fs::write(&f1, b"original").unwrap();
        std::fs::write(&f2, b"content").unwrap();

        let svc = fx.service();
        svc.capture_baseline(fx.task_id(), &f1).await.unwrap();

        // 只记录 f1 的变更
        std::fs::write(&f1, b"modified").unwrap();
        svc.record_change(
            fx.task_id(),
            &f1,
            FileChangeType::Modify,
            None,
            Some(b"original"),
            Some(b"modified"),
            None,
        )
        .await
        .unwrap();

        // f2 无基线也无变更记录 -> 不在回滚列表中
        let results = svc.rollback_task(fx.task_id()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], RollbackResult::Restored { .. }));
    }

    // --------------------------------------------------------------------------
    // fold_changes 单元测试
    // --------------------------------------------------------------------------

    #[test]
    fn fold_empty() {
        let entries = fold_changes(&[]);
        assert!(entries.is_empty());
    }

    #[test]
    fn fold_single_create() {
        let changes = vec![FileChange::new("t1", "a.txt", FileChangeType::Create)];
        let entries = fold_changes(&changes);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].net_change_type, FileChangeType::Create);
    }
}
