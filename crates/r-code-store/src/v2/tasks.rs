//! V2 persistence helpers for task branches and queue reconstruction.
//!
//! Queue entries live in the ordered event journal (`input.queued` /
//! `input.delivered`); these helpers rebuild the pending set after a daemon
//! restart and record canonical branch lineage.

use crate::v2::V2Store;
use r_code_harness_protocol::InputMessage;
use rusqlite::params;

/// Canonical branch lineage row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskBranch {
    pub task_id: String,
    pub parent_task_id: Option<String>,
}

impl V2Store {
    /// Record the lineage of a branch created from another task (roots carry
    /// no parent).
    pub fn record_branch(
        &self,
        task_id: &str,
        parent_task_id: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        self.connection()
            .execute(
                "INSERT OR REPLACE INTO task_branches(task_id, parent_task_id, created_at_ms)
                 VALUES (?1, ?2, ?3)",
                params![
                    task_id,
                    parent_task_id,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0)
                ],
            )
            .map(|_| ())
    }

    /// All recorded branches.
    pub fn branches(&self) -> Result<Vec<TaskBranch>, rusqlite::Error> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT task_id, parent_task_id FROM task_branches ORDER BY created_at_ms, task_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(TaskBranch {
                task_id: row.get(0)?,
                parent_task_id: row.get(1)?,
            })
        })?;
        rows.collect()
    }

    /// The parent of one task, when recorded.
    pub fn branch_parent(&self, task_id: &str) -> Result<Option<String>, rusqlite::Error> {
        match self.connection().query_row(
            "SELECT parent_task_id FROM task_branches WHERE task_id = ?1",
            params![task_id],
            |row| row.get::<_, Option<String>>(0),
        ) {
            Ok(parent) => Ok(parent),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// Rebuild a task's pending input queue from the event journal: queued inputs
/// minus delivered ones. Returns `(last_seq, pending)` in delivery order.
pub fn rebuild_queue(store: &V2Store, task_id: &str) -> (u64, Vec<InputMessage>) {
    let events = store.task_events(task_id);
    let mut last_seq = 0u64;
    let mut queued: Vec<InputMessage> = Vec::new();
    let mut delivered = std::collections::HashSet::new();
    for event in events {
        match event.kind.as_str() {
            "input.queued" => {
                if let Ok(message) = serde_json::from_value::<InputMessage>(event.payload.clone()) {
                    last_seq = last_seq.max(message.input_seq);
                    queued.push(message);
                }
            }
            "input.delivered" => {
                if let Some(id) = event.payload.get("message_id").and_then(|v| v.as_str()) {
                    delivered.insert(id.to_string());
                }
            }
            _ => {}
        }
    }
    queued.retain(|message| !delivered.contains(&message.message_id));
    (last_seq, queued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_kernel::ports::{JournalEvent, JournalStore};

    #[tokio::test]
    async fn branches_and_queues_rebuild_from_the_journal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = V2Store::open(&temp.path().join("tasks.sqlite3")).expect("open");
        store
            .record_branch("task-2", Some("task-1"))
            .expect("record");
        store.record_branch("task-1", None).expect("record root");
        assert_eq!(
            store.branch_parent("task-2").expect("parent"),
            Some("task-1".to_string())
        );
        assert_eq!(store.branch_parent("task-1").expect("root"), None);
        assert_eq!(store.branches().expect("branches").len(), 2);

        let state = r_code_kernel::task::TaskState::new(r_code_kernel::task::TaskContract {
            task_id: "task-1".into(),
            kind: r_code_kernel::task::TaskKind::Conversation,
            objective: "o".into(),
            constraints: vec![],
            required_checks: vec![],
            revision: 1,
        });
        let messages: Vec<r_code_harness_protocol::InputMessage> = (1..=3)
            .map(|seq| r_code_harness_protocol::InputMessage {
                message_id: format!("m{seq}"),
                input_seq: seq,
                kind: r_code_harness_protocol::InputKind::User,
                text: format!("t{seq}"),
            })
            .collect();
        let mut events = vec![JournalEvent {
            seq: 0,
            task_id: "task-1".into(),
            kind: "task.created".into(),
            payload: serde_json::json!({}),
        }];
        for message in &messages {
            events.push(JournalEvent {
                seq: 0,
                task_id: "task-1".into(),
                kind: "input.queued".into(),
                payload: serde_json::to_value(message).unwrap(),
            });
        }
        events.push(JournalEvent {
            seq: 0,
            task_id: "task-1".into(),
            kind: "input.delivered".into(),
            payload: serde_json::json!({"message_id": "m1"}),
        });
        store
            .save_task_and_events(&state, events)
            .await
            .expect("save");

        let (last_seq, pending) = rebuild_queue(&store, "task-1");
        assert_eq!(last_seq, 3);
        assert_eq!(
            pending.iter().map(|m| m.input_seq).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }
}
