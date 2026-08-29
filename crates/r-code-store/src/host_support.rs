//! Host 层迁移而来的 store 侧查询/更新 API（FX-08，F-arch-02 收敛）。
//!
//! 根因：commands.rs / plan_review_tools.rs / recovery.rs / support_bundle.rs
//! 直接对 store 所属表写裸 SQL（约 20 处生产位点），与 repositories 形成
//! 同表双写路径——schema 变更时两处必炸其一。本模块把这些 SQL 收敛回
//! store：宿主只表达**意图**（收束遗留 run / 查派发元数据 / 计数），语句
//! 与事务边界归 store 所有。
//!
//! 统一约定：函数入参 `&rusqlite::Connection`（宿主从 `Database::conn()`
//! 或自有连接传入，recovery 的旁路连接策略保持不变）；事务型函数用
//! `unchecked_transaction` 自管事务。schema 归属仍以 migrations.rs 为唯一
//! 事实源；一次性数据迁移（attachment_migration）与 schema DDL 不在本
//! 模块收敛范围。

use rusqlite::Connection;

use r_code_core::error::ProductError;
use rusqlite::OptionalExtension;

fn db_err(error: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(error.to_string())
}

// ---------------------------------------------------------------------------
// 派发队列（queued_messages）
// ---------------------------------------------------------------------------

/// 派发成功后的对账：仅当消息仍处于 dispatching/failed 时标记 sent。
/// 返回实际对账条数。
pub fn reconcile_queued_messages_sent(
    conn: &Connection,
    queue_ids: &[&str],
    now: &str,
) -> Result<u64, ProductError> {
    let mut reconciled = 0u64;
    for queue_id in queue_ids {
        let changed = conn
            .execute(
                "UPDATE queued_messages SET state = 'sent', updated_at = ?1 \
                 WHERE id = ?2 AND state IN ('dispatching', 'failed')",
                rusqlite::params![now, queue_id],
            )
            .map_err(db_err)?;
        reconciled += changed as u64;
    }
    Ok(reconciled)
}

/// CAS 改写排队消息的 attachments_json：仍是期望值（含 NULL）才更新。
/// 返回是否成功改写。
pub fn set_queued_attachments_json_cas(
    conn: &Connection,
    queue_id: &str,
    new_json: &str,
    expected_json: Option<&str>,
    now: &str,
) -> Result<bool, ProductError> {
    let changed = conn
        .execute(
            "UPDATE queued_messages SET attachments_json = ?2, updated_at = ?3 \
             WHERE id = ?1 AND attachments_json IS ?4",
            rusqlite::params![queue_id, new_json, now, expected_json],
        )
        .map_err(db_err)?;
    Ok(changed > 0)
}

// ---------------------------------------------------------------------------
// 工具调用（tool_calls）
// ---------------------------------------------------------------------------

/// 父 run 已结束但仍处 running 的工具调用统一收尾为 error（启动恢复前置修复）。
pub fn fail_running_tool_calls_with_ended_parent(
    conn: &Connection,
    tool_error_json: &str,
    now: &str,
) -> Result<u64, ProductError> {
    let repaired = conn
        .execute(
            "UPDATE tool_calls
             SET status = 'error',
                 output_json = COALESCE(output_json, ?1),
                 ended_at = COALESCE(ended_at, ?2)
             WHERE status = 'running'
               AND EXISTS (
                   SELECT 1 FROM agent_runs
                   WHERE agent_runs.id = tool_calls.run_id
                     AND agent_runs.ended_at IS NOT NULL
               )",
            rusqlite::params![tool_error_json, now],
        )
        .map_err(db_err)?;
    Ok(repaired as u64)
}

/// 读取工具调用的审计输入 JSON（子代理 goal 提取等）。
pub fn tool_call_input_json(conn: &Connection, call_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT input_json FROM tool_calls WHERE id = ?1",
        rusqlite::params![call_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

/// call_id 是否确为该父 run 下的真实记录（外部 callId 复用的防串扰守卫）。
pub fn tool_call_exists_in_run(conn: &Connection, call_id: &str, run_id: &str) -> bool {
    let count: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM tool_calls WHERE id = ?1 AND run_id = ?2",
        rusqlite::params![call_id, run_id],
        |row| row.get(0),
    );
    count.map(|count| count > 0).unwrap_or(false)
}

/// 某任务下按工具名过滤的成功调用输入 JSON（时间升序；变更面提取用）。
pub fn list_tool_inputs_ok_for_tools(
    conn: &Connection,
    task_id: &str,
    tool_names: &[&str],
) -> Result<Vec<String>, ProductError> {
    // 工具名清单是宿主策略，占位符按清单长度展开；名字本身不含引号
    //（内部工具标识），仍做防御性校验。
    for name in tool_names {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(ProductError::ConfigError(format!(
                "invalid tool name for query: {name}"
            )));
        }
    }
    let placeholders = tool_names
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT input_json FROM tool_calls \
         WHERE task_id = ?1 AND status = 'ok' AND tool_name IN ({placeholders}) \
         ORDER BY started_at ASC"
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&task_id];
    for name in tool_names {
        params.push(name);
    }
    let rows = stmt
        .query_map(params.as_slice(), |row| row.get::<_, String>(0))
        .map_err(db_err)?;
    let mut inputs = Vec::new();
    for row in rows {
        inputs.push(row.map_err(db_err)?);
    }
    Ok(inputs)
}

// ---------------------------------------------------------------------------
// 启动恢复（agent_runs / permission_requests / tasks / task_events）
// ---------------------------------------------------------------------------

/// 启动恢复 run 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupRecoveryRunRow {
    pub run_id: String,
    pub task_id: String,
    pub branch_id: String,
}

/// 启动前遗留的未结束 run（任务未处于 idle/archived 终态）。
pub fn list_startup_recovery_runs(
    conn: &Connection,
) -> Result<Vec<StartupRecoveryRunRow>, ProductError> {
    let mut stmt = conn
        .prepare(
            "SELECT ar.id, ar.task_id, ar.branch_id \
         FROM agent_runs ar \
         INNER JOIN tasks t ON t.id = ar.task_id \
         WHERE ar.ended_at IS NULL AND t.state NOT IN ('idle', 'archived') \
         ORDER BY ar.started_at ASC, ar.id ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(StartupRecoveryRunRow {
                run_id: row.get(0)?,
                task_id: row.get(1)?,
                branch_id: row.get(2)?,
            })
        })
        .map_err(db_err)?;
    let mut runs = Vec::new();
    for row in rows {
        runs.push(row.map_err(db_err)?);
    }
    Ok(runs)
}

/// 全部 pending 权限请求 id（启动恢复快照）。
pub fn list_pending_permission_ids(conn: &Connection) -> Result<Vec<String>, ProductError> {
    let mut stmt = conn
        .prepare("SELECT id FROM permission_requests WHERE decision = 'pending' ORDER BY created_at ASC, id ASC")
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(db_err)?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.map_err(db_err)?);
    }
    Ok(ids)
}

/// run 是否仍未结束。
pub fn run_is_active(conn: &Connection, run_id: &str) -> Result<bool, ProductError> {
    let active: i64 = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_runs WHERE id = ?1 AND ended_at IS NULL)",
            rusqlite::params![run_id],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    Ok(active != 0)
}

/// 权限请求是否仍 pending。
pub fn permission_is_pending(conn: &Connection, permission_id: &str) -> Result<bool, ProductError> {
    let pending: i64 = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM permission_requests WHERE id = ?1 AND decision = 'pending')",
            rusqlite::params![permission_id],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    Ok(pending != 0)
}

/// 权限请求是否仍属于指定任务且 pending（任务级收束前校验）。
pub fn permission_is_pending_for_task(
    conn: &Connection,
    permission_id: &str,
    task_id: &str,
) -> Result<bool, ProductError> {
    let pending: i64 = conn
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM permission_requests
                WHERE id = ?1 AND task_id = ?2 AND decision = 'pending'
            )",
            rusqlite::params![permission_id, task_id],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    Ok(pending != 0)
}

/// 遗留收束的输入：run 清单 + 文案 + 权限清单。
#[derive(Debug, Clone)]
pub struct RecoveryCleanupInputs<'a> {
    pub run_ids: &'a [&'a str],
    pub run_summary: &'a str,
    pub tool_error: &'a str,
    pub permission_ids: &'a [&'a str],
    pub interrupted_state: &'a str,
    pub now: &'a str,
}

/// 遗留收束的计数结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryCleanupCounts {
    pub runs_closed: u64,
    pub tool_calls_closed: u64,
    pub tasks_interrupted: u64,
    pub permissions_denied: u64,
}

/// 单事务收束全部启动前遗留：关 run → 关其 running 工具调用 → 无活跃 run
/// 的任务落 Interrupted → 补 RunAborted/RunEnded 事件 → deny 遗留 pending
/// 权限。幂等：ended_at/decided_at 的 COALESCE 与状态守卫保证重复执行无副作用。
pub fn apply_recovery_cleanup(
    conn: &Connection,
    inputs: &RecoveryCleanupInputs<'_>,
) -> Result<RecoveryCleanupCounts, ProductError> {
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    let mut counts = RecoveryCleanupCounts::default();
    let mut closed_branches: Vec<(String, String)> = Vec::new();

    for run_id in inputs.run_ids {
        let closed = tx
            .execute(
                "UPDATE agent_runs
                 SET review_state = CASE WHEN review_state = 'pending' THEN 'aborted' ELSE review_state END,
                     ended_at = COALESCE(ended_at, ?1),
                     summary = COALESCE(summary, ?2)
                 WHERE id = ?3 AND ended_at IS NULL",
                rusqlite::params![inputs.now, inputs.run_summary, run_id],
            )
            .map_err(db_err)?;
        if closed == 0 {
            continue;
        }
        counts.runs_closed += closed as u64;
        counts.tool_calls_closed += tx
            .execute(
                "UPDATE tool_calls
                 SET status = 'error',
                     output_json = COALESCE(output_json, ?1),
                     ended_at = COALESCE(ended_at, ?2)
                 WHERE run_id = ?3 AND status = 'running'",
                rusqlite::params![inputs.tool_error, inputs.now, run_id],
            )
            .map_err(db_err)? as u64;
        let branch: Option<(String, String)> = tx
            .query_row(
                "SELECT task_id, branch_id FROM agent_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        if let Some(branch) = branch {
            if !closed_branches.contains(&branch) {
                closed_branches.push(branch);
            }
        }
    }

    // 任务收束：仅当该任务已无任何活跃 run 且不在终态时落 Interrupted。
    let mut interrupted_task_ids: Vec<String> = Vec::new();
    for (task_id, _) in &closed_branches {
        if interrupted_task_ids.contains(task_id) {
            continue;
        }
        let updated = tx
            .execute(
                "UPDATE tasks
                 SET state = ?1, updated_at = ?2
                 WHERE id = ?3
                   AND state NOT IN ('idle', 'archived', 'interrupted')
                   AND NOT EXISTS (
                       SELECT 1 FROM agent_runs AS live
                       WHERE live.task_id = tasks.id AND live.ended_at IS NULL
                   )",
                rusqlite::params![inputs.interrupted_state, inputs.now, task_id],
            )
            .map_err(db_err)?;
        if updated != 0 {
            counts.tasks_interrupted += updated as u64;
        }
        interrupted_task_ids.push(task_id.clone());
    }

    for (task_id, branch_id) in &closed_branches {
        tx.execute(
            "INSERT INTO task_events (task_id, branch_id, event_type, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![task_id, branch_id, "run_aborted", inputs.now],
        )
        .map_err(db_err)?;
        tx.execute(
            "INSERT INTO task_events (task_id, branch_id, event_type, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![task_id, branch_id, "run_ended", inputs.now],
        )
        .map_err(db_err)?;
    }

    for permission_id in inputs.permission_ids {
        counts.permissions_denied += tx
            .execute(
                "UPDATE permission_requests
                 SET decision = 'deny', decided_at = COALESCE(decided_at, ?1)
                 WHERE id = ?2 AND decision = 'pending'",
                rusqlite::params![inputs.now, permission_id],
            )
            .map_err(db_err)? as u64;
    }

    tx.commit().map_err(db_err)?;
    Ok(counts)
}

// ---------------------------------------------------------------------------
// 恢复页（recovery.rs 的旁路连接消费同一 API）
// ---------------------------------------------------------------------------

/// 恢复页扫描：未到终态任务 + 其活跃 run（同任务多 run 防御性去重由调用方做）。
pub fn scan_interrupted_task_rows(
    conn: &Connection,
) -> Result<Vec<(String, String, String)>, ProductError> {
    let mut stmt = conn
        .prepare(
            "SELECT t.id, t.state, ar.id \
             FROM tasks t \
             JOIN agent_runs ar ON ar.task_id = t.id \
             WHERE t.state NOT IN ('idle', 'archived') AND ar.ended_at IS NULL \
             ORDER BY t.updated_at DESC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(db_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(db_err)?);
    }
    Ok(out)
}

/// 遗留 pending 权限计数（恢复页角标）。
pub fn count_pending_permissions(conn: &Connection) -> Result<u64, ProductError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM permission_requests WHERE decision = 'pending'",
            [],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    Ok(count.max(0) as u64)
}

// ---------------------------------------------------------------------------
// 诊断与元数据查询
// ---------------------------------------------------------------------------

/// 支持包统计：白名单表的行数。表名在 store 侧做标识符校验，
/// 宿主只传固定清单，拼接不接收外部输入。
pub fn table_row_counts(conn: &Connection, tables: &[&str]) -> Result<Vec<u64>, ProductError> {
    let mut counts = Vec::with_capacity(tables.len());
    for table in tables {
        let valid = !table.is_empty() && table.chars().all(|c| c.is_ascii_lowercase() || c == '_');
        if !valid {
            return Err(ProductError::ConfigError(format!(
                "invalid table name for diagnostics: {table}"
            )));
        }
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(db_err)?;
        counts.push(count.max(0) as u64);
    }
    Ok(counts)
}

/// 附件派发元数据（附件解析器消费）：按 id 取归属任务与 blob 定位信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentDispatchMetadata {
    pub task_id: String,
    pub blob_hash: String,
    pub name: String,
    pub media_type: String,
}

pub fn attachment_dispatch_metadata(
    conn: &Connection,
    attachment_id: &str,
) -> Result<Option<AttachmentDispatchMetadata>, ProductError> {
    let row = conn
        .query_row(
            "SELECT task_id, blob_hash, name, media_type FROM attachments WHERE id = ?1",
            rusqlite::params![attachment_id],
            |row| {
                Ok(AttachmentDispatchMetadata {
                    task_id: row.get(0)?,
                    blob_hash: row.get(1)?,
                    name: row.get(2)?,
                    media_type: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(db_err)?;
    Ok(row)
}

/// 验证记录的输出 blob key（VerificationService 无单条查询场景的补充读取）。
/// `Ok(None)` = 记录不存在；`Some(None)` = 记录存在但尚未写出输出 blob。
pub fn verification_output_blob_key(
    conn: &Connection,
    verification_id: &str,
) -> Result<Option<Option<String>>, ProductError> {
    conn.query_row(
        "SELECT output_blob_key FROM verifications WHERE id = ?1",
        rusqlite::params![verification_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map_err(db_err)
}

// ---------------------------------------------------------------------------
// Plan 执行门（plan_review_tools.rs）
// ---------------------------------------------------------------------------

/// Plan 执行是否处于暂停：存在 executing 计划，且其 approved_revision 的
/// 全部条目都不在 in_progress（即没有正在执行的条目）。
pub fn plan_execution_paused(conn: &Connection, task_id: &str) -> Result<bool, ProductError> {
    let paused: bool = conn
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM plans plan
               WHERE plan.task_id = ?1 AND plan.state = 'executing'
                 AND NOT EXISTS (
                   SELECT 1 FROM plan_items item
                   WHERE item.plan_id = plan.id
                     AND item.revision = plan.approved_revision
                     AND item.state = 'in_progress'
                 )
             )",
            rusqlite::params![task_id],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    Ok(paused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use rusqlite::params;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn insert_task(conn: &Connection, id: &str, state: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, goal, state, mode, agent_engine, created_at, updated_at) \
             VALUES (?1, 't', 'g', ?2, 'auto', 'r_code', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            params![id, state],
        )
        .unwrap();
    }

    fn insert_run(conn: &Connection, id: &str, task_id: &str, ended: Option<&str>) {
        conn.execute(
            "INSERT INTO agent_runs (id, task_id, model, review_state, started_at, ended_at, branch_id) \
             VALUES (?1, ?2, 'm', 'pending', '2026-01-01T00:00:00Z', ?3, 'b1')",
            params![id, task_id, ended],
        )
        .unwrap();
    }

    fn insert_tool_call(conn: &Connection, id: &str, run_id: &str, task_id: &str, status: &str) {
        conn.execute(
            "INSERT INTO tool_calls (id, run_id, task_id, tool_name, input_json, risk_level, status, started_at) \
             VALUES (?1, ?2, ?3, 'create_file', '{}', 'r2', ?4, '2026-01-01T00:00:00Z')",
            params![id, run_id, task_id, status],
        )
        .unwrap();
    }

    fn insert_permission(conn: &Connection, id: &str, task_id: &str, decision: &str) {
        // permission_requests.tool_call_id 对 tool_calls 有 FK；补一条真实链
        // task → run → tool_call 再挂权限。链上的 run 用已结束状态，避免
        // 干扰"任务无活跃 run"类断言。
        let run_id = format!("r-fix-{id}");
        let call_id = format!("tc-{id}");
        insert_run(conn, &run_id, task_id, Some("2026-01-02T00:00:00Z"));
        insert_tool_call(conn, &call_id, &run_id, task_id, "running");
        conn.execute(
            "INSERT INTO permission_requests (id, task_id, tool_call_id, tool_name, risk_level, input_summary, decision, created_at) \
             VALUES (?1, ?2, ?3, 't', 'r2', 's', ?4, '2026-01-01T00:00:00Z')",
            params![id, task_id, call_id, decision],
        )
        .unwrap();
    }

    #[test]
    fn reconcile_only_touches_dispatching_or_failed_messages() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "idle");
        for (id, state) in [("q1", "dispatching"), ("q2", "failed"), ("q3", "sent")] {
            conn.execute(
                "INSERT INTO queued_messages (id, task_id, branch_id, message, priority, state, created_at, updated_at, attachments_json) \
                 VALUES (?1, 't1', 'b1', 'hi', 0, ?2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', NULL)",
                params![id, state],
            )
            .unwrap();
        }
        let reconciled = reconcile_queued_messages_sent(&conn, &["q1", "q2", "q3"], "now").unwrap();
        assert_eq!(reconciled, 2);
    }

    #[test]
    fn queued_attachments_cas_matches_null_expected() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "idle");
        conn.execute(
            "INSERT INTO queued_messages (id, task_id, branch_id, message, priority, state, created_at, updated_at, attachments_json) \
             VALUES ('q1', 't1', 'b1', 'hi', 0, 'queued', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', NULL)",
            [],
        )
        .unwrap();
        assert!(set_queued_attachments_json_cas(&conn, "q1", "[1]", None, "now").unwrap());
        // 期望值不再匹配 → 不更新
        assert!(!set_queued_attachments_json_cas(&conn, "q1", "[2]", None, "now2").unwrap());
    }

    #[test]
    fn fail_running_tool_calls_only_for_ended_parents() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "in_progress");
        insert_run(&conn, "r1", "t1", Some("2026-01-02T00:00:00Z"));
        insert_run(&conn, "r2", "t1", None);
        insert_tool_call(&conn, "tc1", "r1", "t1", "running");
        insert_tool_call(&conn, "tc2", "r2", "t1", "running");
        insert_tool_call(&conn, "tc3", "r1", "t1", "ok");
        let repaired =
            fail_running_tool_calls_with_ended_parent(&conn, "{\"error\":\"x\"}", "now").unwrap();
        assert_eq!(repaired, 1);
        let status: String = conn
            .query_row(
                "SELECT status FROM tool_calls WHERE id = 'tc1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "error");
    }

    #[test]
    fn tool_call_helpers_read_and_scope_correctly() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "idle");
        insert_run(&conn, "r1", "t1", None);
        insert_tool_call(&conn, "tc1", "r1", "t1", "running");
        assert_eq!(tool_call_input_json(&conn, "tc1"), Some("{}".to_string()));
        assert_eq!(tool_call_input_json(&conn, "missing"), None);
        assert!(tool_call_exists_in_run(&conn, "tc1", "r1"));
        assert!(!tool_call_exists_in_run(&conn, "tc1", "other-run"));
    }

    #[test]
    fn mutation_inputs_filter_by_task_tool_and_status() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "idle");
        insert_run(&conn, "r1", "t1", None);
        insert_tool_call(&conn, "tc1", "r1", "t1", "ok");
        let inputs = list_tool_inputs_ok_for_tools(&conn, "t1", &["create_file"]).unwrap();
        assert_eq!(inputs, vec!["{}".to_string()]);
        assert!(list_tool_inputs_ok_for_tools(&conn, "t1", &["read_file"])
            .unwrap()
            .is_empty());
        assert!(list_tool_inputs_ok_for_tools(&conn, "t1", &["bad-name"]).is_err());
    }

    #[test]
    fn startup_recovery_queries_filter_by_state() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "in_progress");
        insert_task(&conn, "t2", "idle");
        insert_run(&conn, "r1", "t1", None);
        insert_run(&conn, "r2", "t2", Some("2026-01-02T00:00:00Z"));
        let runs = list_startup_recovery_runs(&conn).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "r1");
        assert!(run_is_active(&conn, "r1").unwrap());
        assert!(!run_is_active(&conn, "r2").unwrap());
        insert_permission(&conn, "p1", "t1", "pending");
        insert_permission(&conn, "p2", "t1", "deny");
        assert_eq!(list_pending_permission_ids(&conn).unwrap(), vec!["p1"]);
        assert!(permission_is_pending(&conn, "p1").unwrap());
        assert!(!permission_is_pending(&conn, "p2").unwrap());
        assert!(permission_is_pending_for_task(&conn, "p1", "t1").unwrap());
        assert!(!permission_is_pending_for_task(&conn, "p1", "other").unwrap());
    }

    #[test]
    fn recovery_cleanup_is_transactional_and_idempotent() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "in_progress");
        insert_run(&conn, "r1", "t1", None);
        insert_tool_call(&conn, "tc1", "r1", "t1", "running");
        insert_permission(&conn, "p1", "t1", "pending");

        let inputs = RecoveryCleanupInputs {
            run_ids: &["r1", "missing-run"],
            run_summary: "aborted by recovery",
            tool_error: "{\"error\":\"parent ended\"}",
            permission_ids: &["p1"],
            interrupted_state: "interrupted",
            now: "2026-08-29T00:00:00Z",
        };
        let counts = apply_recovery_cleanup(&conn, &inputs).unwrap();
        assert_eq!(counts.runs_closed, 1);
        assert_eq!(counts.tool_calls_closed, 1);
        assert_eq!(counts.tasks_interrupted, 1);
        assert_eq!(counts.permissions_denied, 1);

        // 事件成对写入
        let events: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_events WHERE task_id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(events, 2);

        // 幂等重放：全部为 0
        let counts = apply_recovery_cleanup(&conn, &inputs).unwrap();
        assert_eq!(counts, RecoveryCleanupCounts::default());
    }

    #[test]
    fn recovery_page_scans_and_counts() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "in_progress");
        insert_task(&conn, "t2", "archived");
        insert_run(&conn, "r1", "t1", None);
        insert_run(&conn, "r2", "t2", None);
        let rows = scan_interrupted_task_rows(&conn).unwrap();
        assert_eq!(rows, vec![("t1".into(), "in_progress".into(), "r1".into())]);
        insert_permission(&conn, "p1", "t1", "pending");
        assert_eq!(count_pending_permissions(&conn).unwrap(), 1);
    }

    #[test]
    fn table_counts_validate_names_and_count() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "idle");
        let counts = table_row_counts(&conn, &["tasks"]).unwrap();
        assert_eq!(counts, vec![1]);
        assert!(table_row_counts(&conn, &["tasks; DROP TABLE tasks"]).is_err());
    }

    #[test]
    fn plan_pause_requires_executing_plan_without_active_items() {
        let db = db();
        let conn = db.conn().expect("conn");
        insert_task(&conn, "t1", "in_progress");
        conn.execute(
            "INSERT INTO plans (id, task_id, revision, state, approved_revision, approved_at, created_at, updated_at) \
             VALUES ('pl1', 't1', 1, 'executing', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        assert!(plan_execution_paused(&conn, "t1").unwrap());
        conn.execute(
            "INSERT INTO plan_items (id, plan_id, revision, ordinal, title, description, state, created_at, updated_at) \
             VALUES ('pi1', 'pl1', 1, 0, 'step', 'd', 'in_progress', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        assert!(!plan_execution_paused(&conn, "t1").unwrap());
        assert!(!plan_execution_paused(&conn, "other-task").unwrap());
    }
}
