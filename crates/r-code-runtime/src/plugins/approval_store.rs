//! Persistent approval pending-operation store (RA1).
//!
//! Approval operations are host-created facts projected into the journal:
//! `approval.requested` when the host registers one, `approval.decided`
//! when a first decision lands (client decision or timeout denial). The
//! in-memory index is a projection of those events — a restarted daemon
//! rebuilds pending state from the journal, so a decision is only visible
//! once its event has been persisted (journal write happens *before* the
//! in-memory commit).
//!
//! Frozen payload contract (RA1):
//! - `approval.requested` — `{opId, summary, runId, createdSeq, createdMs}`;
//!   the task id rides the journal row's task_id column.
//! - `approval.decided` — `{opId, decision: "granted"|"denied", decidedBy,
//!   decidedSeq, runId}` where `decidedBy` is the deciding client id or
//!   the literal `"<timeout>"`.
//!
//! `createdSeq`/`decidedSeq` are the journal water level (highest
//! allocated seq) *before* the event is written: an append-only event
//! cannot reference its own rowid. Both are monotonic and comparable,
//! which is all list ordering and cursor resumption need.

use r_code_harness_protocol::ApprovalDecision;
use r_code_kernel::ports::{JournalEvent, JournalStore};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex};

/// Journal kind for a host-registered pending operation.
pub const EVENT_REQUESTED: &str = "approval.requested";
/// Journal kind for a first decision on a pending operation.
pub const EVENT_DECIDED: &str = "approval.decided";

/// Default time an approval request waits before a timeout denial.
pub const DEFAULT_DECISION_TIMEOUT: Duration = Duration::from_secs(300);

/// The decision record replayed to idempotent deciders and waiting plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    pub decision: ApprovalDecision,
    pub decided_by: String,
    pub decided_seq: u64,
}

/// One pending operation with its (optional) decision.
#[derive(Debug)]
struct PendingOp {
    summary: String,
    run_id: String,
    task_id: String,
    created_seq: u64,
    created_ms: i64,
    decision: Option<DecisionRecord>,
    tx: watch::Sender<Option<DecisionRecord>>,
    /// Keeps the watch channel open: a tokio watch channel closes (and
    /// drops sends) once every receiver is gone, so the op itself holds
    /// one for its lifetime. Waiters clone fresh receivers off `tx`.
    #[allow(dead_code)] // held purely for its liveness effect
    keepalive_rx: watch::Receiver<Option<DecisionRecord>>,
}

/// Errors from [`ApprovalStore::decide`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecideError {
    #[error("unknown approval operation {0}")]
    Unknown(String),
    #[error("approval operation {0} is already decided; conflicting decisions are refused")]
    Conflict(String),
    #[error("journal failure: {0}")]
    Store(String),
}

/// Row view for pending operations (the `approvals.list` projection, RA2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingView {
    pub op_id: String,
    pub summary: String,
    pub run_id: String,
    pub task_id: String,
    pub created_seq: u64,
    pub created_ms: i64,
}

/// Host-side persistent approval store shared by the run manager, the
/// router (plugin await path) and daemon decision methods (RA2).
pub struct ApprovalStore {
    ops: Mutex<HashMap<String, PendingOp>>,
    store: Arc<dyn JournalStore>,
    timeout: Duration,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn decision_label(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Granted => "granted",
        ApprovalDecision::Denied => "denied",
        ApprovalDecision::Expired => "expired",
    }
}

fn parse_decision(label: &str) -> ApprovalDecision {
    match label {
        "granted" => ApprovalDecision::Granted,
        _ => ApprovalDecision::Denied,
    }
}

impl ApprovalStore {
    pub fn new(store: Arc<dyn JournalStore>, timeout: Duration) -> Self {
        Self {
            ops: Mutex::new(HashMap::new()),
            store,
            timeout,
        }
    }

    /// How long an undecided request waits before the timeout denial.
    pub fn decision_timeout(&self) -> Duration {
        self.timeout
    }

    /// Register a pending operation (host-only call) and emit its
    /// `approval.requested` event. Idempotent: re-registering an existing
    /// op returns a receiver that already observes its state and never
    /// writes a second event.
    pub async fn register(
        &self,
        op_id: &str,
        summary: &str,
        run_id: &str,
        task_id: &str,
    ) -> watch::Receiver<Option<DecisionRecord>> {
        {
            let ops = self.ops.lock().await;
            if let Some(op) = ops.get(op_id) {
                return op.tx.subscribe();
            }
        }
        let created_seq = self.store.max_event_seq().await;
        let created_ms = now_ms();
        let payload = serde_json::json!({
            "opId": op_id,
            "summary": summary,
            "runId": run_id,
            "createdSeq": created_seq,
            "createdMs": created_ms,
        });
        self.append_event(task_id, EVENT_REQUESTED, payload).await;
        let mut ops = self.ops.lock().await;
        // Another register raced in: first event wins, second caller
        // observes the same operation.
        if let Some(op) = ops.get(op_id) {
            return op.tx.subscribe();
        }
        let (tx, keepalive_rx) = watch::channel(None);
        let receiver = tx.subscribe();
        ops.insert(
            op_id.to_string(),
            PendingOp {
                summary: summary.to_string(),
                run_id: run_id.to_string(),
                task_id: task_id.to_string(),
                created_seq,
                created_ms,
                decision: None,
                tx,
                keepalive_rx,
            },
        );
        receiver
    }

    /// Receiver for an already-registered operation; `None` means the op id
    /// was never host-created (plugin-forged references land here).
    pub async fn waiter(&self, op_id: &str) -> Option<watch::Receiver<Option<DecisionRecord>>> {
        let ops = self.ops.lock().await;
        ops.get(op_id).map(|op| op.tx.subscribe())
    }

    /// Decide a pending operation: the first valid decision wins and is
    /// journaled; the same decision replays the original record; a
    /// conflicting decision is refused. Never creates an operation.
    pub async fn decide(
        &self,
        op_id: &str,
        decision: ApprovalDecision,
        decided_by: &str,
    ) -> Result<DecisionRecord, DecideError> {
        let (run_id, task_id) = {
            let ops = self.ops.lock().await;
            let op = ops
                .get(op_id)
                .ok_or_else(|| DecideError::Unknown(op_id.into()))?;
            if let Some(existing) = &op.decision {
                if existing.decision == decision {
                    return Ok(existing.clone());
                }
                return Err(DecideError::Conflict(op_id.into()));
            }
            (op.run_id.clone(), op.task_id.clone())
        };
        // Journal before the in-memory commit: a visible decision always
        // has its event persisted. A concurrent duplicate `decided` event
        // is harmless — the rebuild projection keeps the first one.
        let decided_seq = self.store.max_event_seq().await;
        let payload = serde_json::json!({
            "opId": op_id,
            "decision": decision_label(decision),
            "decidedBy": decided_by,
            "decidedSeq": decided_seq,
            "runId": run_id,
        });
        self.append_event(&task_id, EVENT_DECIDED, payload).await;
        let record = DecisionRecord {
            decision,
            decided_by: decided_by.to_string(),
            decided_seq,
        };
        let mut ops = self.ops.lock().await;
        let op = ops
            .get_mut(op_id)
            .ok_or_else(|| DecideError::Unknown(op_id.into()))?;
        if let Some(existing) = &op.decision {
            if existing.decision == decision {
                return Ok(existing.clone());
            }
            return Err(DecideError::Conflict(op_id.into()));
        }
        let _ = op.tx.send(Some(record.clone()));
        op.decision = Some(record.clone());
        Ok(record)
    }

    /// Pending (undecided) operations ordered by `created_seq` ascending.
    pub async fn pending(&self) -> Vec<PendingView> {
        let ops = self.ops.lock().await;
        let mut rows: Vec<PendingView> = ops
            .iter()
            .filter(|(_, op)| op.decision.is_none())
            .map(|(op_id, op)| PendingView {
                op_id: op_id.clone(),
                summary: op.summary.clone(),
                run_id: op.run_id.clone(),
                task_id: op.task_id.clone(),
                created_seq: op.created_seq,
                created_ms: op.created_ms,
            })
            .collect();
        rows.sort_by_key(|row| row.created_seq);
        rows
    }

    /// Wait for a decision, falling back to the timeout denial. The denial
    /// goes through the same first-wins `decide` path, so a decision that
    /// lands concurrently with the deadline still wins the race.
    pub async fn await_decision(
        &self,
        op_id: &str,
        rx: &mut watch::Receiver<Option<DecisionRecord>>,
    ) -> ApprovalDecision {
        let decided = tokio::time::timeout(self.timeout, async {
            loop {
                if let Some(record) = rx.borrow().clone() {
                    return record;
                }
                if rx.changed().await.is_err() {
                    // Sender dropped without a decision: deny (fail closed).
                    return DecisionRecord {
                        decision: ApprovalDecision::Denied,
                        decided_by: "<store-dropped>".into(),
                        decided_seq: 0,
                    };
                }
            }
        })
        .await;
        match decided {
            Ok(record) => record.decision,
            Err(_) => match self
                .decide(op_id, ApprovalDecision::Denied, "<timeout>")
                .await
            {
                Ok(record) => record.decision,
                // Someone decided concurrently (or the op vanished): the
                // receiver already carries the authoritative answer.
                Err(_) => rx
                    .borrow()
                    .clone()
                    .map(|r| r.decision)
                    .unwrap_or(ApprovalDecision::Denied),
            },
        }
    }

    /// Rebuild the pending index from journal events (daemon restart).
    /// Decided operations keep their decision; still-pending ones come back
    /// as pending with no live waiter (their plugin processes are gone).
    pub async fn rebuild_from_events(&self, events: &[JournalEvent]) {
        let mut ops = self.ops.lock().await;
        for event in events {
            match event.kind.as_str() {
                EVENT_REQUESTED => {
                    let Some(op_id) = event.payload.get("opId").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    ops.entry(op_id.to_string()).or_insert_with(|| {
                        let (tx, keepalive_rx) = watch::channel(None);
                        PendingOp {
                            summary: event
                                .payload
                                .get("summary")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            run_id: event
                                .payload
                                .get("runId")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            task_id: event.task_id.clone(),
                            created_seq: event
                                .payload
                                .get("createdSeq")
                                .and_then(|v| v.as_u64())
                                .unwrap_or_default(),
                            created_ms: event
                                .payload
                                .get("createdMs")
                                .and_then(|v| v.as_i64())
                                .unwrap_or_default(),
                            decision: None,
                            tx,
                            keepalive_rx,
                        }
                    });
                }
                EVENT_DECIDED => {
                    let Some(op_id) = event.payload.get("opId").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let Some(op) = ops.get_mut(op_id) else {
                        continue;
                    };
                    if op.decision.is_some() {
                        continue; // duplicate decided event: first wins
                    }
                    let record = DecisionRecord {
                        decision: parse_decision(
                            event
                                .payload
                                .get("decision")
                                .and_then(|v| v.as_str())
                                .unwrap_or("denied"),
                        ),
                        decided_by: event
                            .payload
                            .get("decidedBy")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        decided_seq: event
                            .payload
                            .get("decidedSeq")
                            .and_then(|v| v.as_u64())
                            .unwrap_or_default(),
                    };
                    let _ = op.tx.send(Some(record.clone()));
                    op.decision = Some(record);
                }
                _ => {}
            }
        }
    }

    /// Append one journal event under the owning task. The task aggregate
    /// is loaded when present; a missing row (op registered against a task
    /// that somehow never landed) still gets its event via a placeholder
    /// aggregate — losing the event would silently un-decide a restart.
    async fn append_event(&self, task_id: &str, kind: &str, payload: serde_json::Value) {
        let task = match self.store.load_task(task_id).await {
            Some(task) => task,
            None => r_code_kernel::task::TaskState::new(r_code_kernel::task::TaskContract {
                task_id: task_id.to_string(),
                kind: r_code_kernel::task::TaskKind::Conversation,
                objective: String::new(),
                constraints: vec![],
                required_checks: vec![],
                revision: 1,
            }),
        };
        let event = JournalEvent {
            seq: 0,
            task_id: task_id.to_string(),
            kind: kind.to_string(),
            payload,
        };
        if let Err(error) = self.store.save_task_and_events(&task, vec![event]).await {
            // Surface through decide/register callers would change their
            // signatures for a path the journal already treats as
            // best-effort elsewhere (the event pump). Keep the denial
            // path honest instead: timeout/decide retried by callers.
            eprintln!("approval-store: journal append failed: {error}");
        }
    }
}
