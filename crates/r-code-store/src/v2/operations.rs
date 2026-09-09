//! V2 persistence for operation receipts (attempt view) and writer barriers.

use crate::v2::V2Store;
use r_code_kernel::task::{OperationReceipt, ReceiptOutcome};
use rusqlite::{params, OptionalExtension};

impl V2Store {
    /// All receipts for one attempt (dedup history; generations never erase it).
    pub fn receipts_for_attempt(
        &self,
        attempt_id: &str,
    ) -> Result<Vec<OperationReceipt>, rusqlite::Error> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT operation_key, method, input_hash, state_json FROM operation_receipts
             WHERE attempt_id = ?1",
        )?;
        let rows = statement.query_map(params![attempt_id], |row| {
            let operation_key: String = row.get(0)?;
            let method: String = row.get(1)?;
            let input_hash: String = row.get(2)?;
            let state_json: String = row.get(3)?;
            Ok((operation_key, method, input_hash, state_json))
        })?;
        let mut receipts = Vec::new();
        for row in rows {
            let (operation_key, method, input_hash, state_json) = row?;
            let outcome: ReceiptOutcome =
                serde_json::from_str(&state_json).unwrap_or(ReceiptOutcome::Rejected {
                    reason: "unreadable state".into(),
                });
            receipts.push(OperationReceipt {
                attempt_id: attempt_id.to_string(),
                operation_key: r_code_harness_protocol::OperationKey(operation_key),
                method,
                input_hash,
                outcome,
            });
        }
        Ok(receipts)
    }

    /// Persist a writer barrier for indeterminate effects.
    pub fn save_writer_barrier(
        &self,
        barrier_id: &str,
        workspace_key: &str,
        owner_pid: u32,
        owner_start: &str,
        reason: &str,
    ) -> Result<(), rusqlite::Error> {
        self.connection().execute(
            "INSERT OR REPLACE INTO writer_barriers(
                barrier_id, workspace_key, owner_pid, owner_start, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                barrier_id,
                workspace_key,
                owner_pid,
                owner_start,
                reason,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Active barriers for a workspace.
    pub fn writer_barriers(
        &self,
        workspace_key: &str,
    ) -> Result<Vec<(String, u32, String, String)>, rusqlite::Error> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT barrier_id, owner_pid, owner_start, reason FROM writer_barriers
             WHERE workspace_key = ?1",
        )?;
        let rows = statement.query_map(params![workspace_key], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// Clear a barrier once termination is proven.
    pub fn clear_writer_barrier(&self, barrier_id: &str) -> Result<bool, rusqlite::Error> {
        let changed = self.connection().execute(
            "DELETE FROM writer_barriers WHERE barrier_id = ?1",
            params![barrier_id],
        )?;
        Ok(changed > 0)
    }

    /// Whether the attempt has a pinned plugin available (join with the
    /// catalog by digest).
    pub fn attempt_plugin_available(
        &self,
        id: &str,
        content_digest: &str,
    ) -> Result<bool, rusqlite::Error> {
        let found: Option<i64> = self
            .connection()
            .query_row(
                "SELECT 1 FROM plugin_catalog WHERE id = ?1 AND content_digest = ?2",
                params![id, content_digest],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
