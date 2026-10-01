//! Daemon-side injection ledger (M1a-02): one audit surface for memory,
//! frozen-instruction, and JIT injections per run. Append-only except for
//! the idempotent re-record of the same (run, kind, hash) triple.

use crate::v1::journal::{V1Store, V1StoreError};
use rusqlite::{params, TransactionBehavior};

/// What kind of context got injected into a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionKind {
    /// FR-7: the desktop-frozen memory snapshot segment.
    Memory,
    /// FR-1: the frozen project-instruction bundle.
    Instruction,
    /// FR-1: a just-in-time subdirectory instruction injection.
    Jit,
}

impl InjectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InjectionKind::Memory => "memory",
            InjectionKind::Instruction => "instruction",
            InjectionKind::Jit => "jit",
        }
    }
}

/// One ledger row to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectionRecord {
    /// Run identity in scope at freeze time: the conversation run id, or
    /// the deterministic WorkUnit attempt id for execution sub-runs.
    pub run_id: String,
    pub kind: InjectionKind,
    /// Snapshot/bundle hash the injection was built from.
    pub snapshot_hash: String,
    /// Audit references (memory entry ids, instruction file paths).
    pub refs: Vec<String>,
    pub chars: u64,
}

/// Read-side projection of a recorded row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InjectionRecordView {
    pub run_id: String,
    pub kind: String,
    pub snapshot_hash: String,
    pub refs: Vec<String>,
    pub chars: u64,
    pub created_at_ms: i64,
}

impl V1Store {
    /// Record one injection. Recording the identical (run_id, kind,
    /// snapshot_hash) triple again is an idempotent no-op; the same run
    /// switching to a different hash appends a new audit row.
    pub fn record_injection(&self, record: &InjectionRecord) -> Result<(), V1StoreError> {
        let refs_json = serde_json::to_string(&record.refs)
            .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO injections(run_id, kind, snapshot_hash, refs_json, chars, created_at_ms)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6
             WHERE NOT EXISTS (
                SELECT 1 FROM injections
                WHERE run_id = ?1 AND kind = ?2 AND snapshot_hash = ?3
             )",
            params![
                record.run_id,
                record.kind.as_str(),
                record.snapshot_hash,
                refs_json,
                record.chars as i64,
                now_ms(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// All ledger rows for one run, oldest first.
    pub fn injections_for_run(
        &self,
        run_id: &str,
    ) -> Result<Vec<InjectionRecordView>, V1StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT run_id, kind, snapshot_hash, refs_json, chars, created_at_ms
             FROM injections WHERE run_id = ?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map(params![run_id], |row| {
            let refs_json: String = row.get(3)?;
            Ok(InjectionRecordView {
                run_id: row.get(0)?,
                kind: row.get(1)?,
                snapshot_hash: row.get(2)?,
                refs: Vec::new(),
                chars: row.get::<_, i64>(4)?.max(0) as u64,
                created_at_ms: row.get(5)?,
            }
            .with_refs_json(refs_json))
        })?;
        let mut views = Vec::new();
        for row in rows {
            let view = row?;
            views.push(view);
        }
        Ok(views)
    }
}

impl InjectionRecordView {
    fn with_refs_json(mut self, refs_json: String) -> Self {
        if let Ok(refs) = serde_json::from_str::<Vec<String>>(&refs_json) {
            self.refs = refs;
        }
        self
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
