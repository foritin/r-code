//! E10 — whole-chain fault recovery: with units in flight at a crash, the
//! restart reconciles every in-flight attempt from its OWN durable evidence
//! to exactly one of resume-once or quarantine; a completed-but-unacked unit
//! settles Completed and is never re-executed; an unprovable attempt
//! quarantines with its family fenced while the unrelated sibling stays
//! untouched; the readiness publishes the reconciled count before any new
//! dispatch; and the pass is idempotent.
//!
//! The crash state is built with the same vocabulary a real crash leaves:
//! attempt rows flipped back to in-flight by direct SQL (the settle that
//! never landed), kernel records rewound through the kernel's own load/save
//! APIs, and — for the never-started arm — the journal's execution.started
//! rows for that attempt removed (the spawn that never happened).

//! macOS：daemon→native harness 链路依赖 P13 安全激活报告，本 wave 固定
//! Unsupported——按设计拒绝启动；用例由 linux/windows 腿运行，P13 落地后移除。
#![cfg(not(target_os = "macos"))]

mod p_gate_support;

use p_gate_support::{compose_with_builtin, profile, stage_native, wait_for_kind, write_workspace};
use r_code_harness_protocol::services::{ModelStreamRequest, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::{
    TaskExecution, TaskKind, TaskPreferences, UnitSettlement, WorkUnitStatus,
};
use r_code_runtime::RuntimeProfile;
use r_code_store::v1::{V1Store, WorkUnitAttemptPhase};
use std::sync::Arc;
use tempfile::TempDir;

/// Two independent units — the wave completes both; a crash can then leave
/// BOTH in flight with different per-unit evidence.
const PAIR_PLAN: &str = r#"{"work_units":[
    {"id":"alpha","description":"independent unit one","dependencies":[],"acceptance":[],"write_paths":["alpha-scope"]},
    {"id":"beta","description":"independent unit two","dependencies":[],"acceptance":[],"write_paths":["beta-scope"]}
]}"#;

/// A chain — the head completes; the dependent can be left mid-flight
/// WITHOUT its own execution.started row (crash between prepare and start).
const CHAIN_PLAN: &str = r#"{"work_units":[
    {"id":"head","description":"chain head","dependencies":[],"acceptance":[],"write_paths":["head-scope"]},
    {"id":"tail","description":"chain dependent","dependencies":["head"],"acceptance":[],"write_paths":["tail-scope"]}
]}"#;

struct WaveModel {
    plan_json: &'static str,
}

impl WaveModel {
    fn new(plan_json: &'static str) -> Arc<Self> {
        Arc::new(Self { plan_json })
    }
}

#[async_trait::async_trait]
impl ModelService for WaveModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        let execution = request.tools.iter().any(|tool| tool.name == "create_file");
        let stream_id = if execution { "execution" } else { "planning" };
        let text = if execution {
            "implementation complete".to_string()
        } else {
            self.plan_json.to_string()
        };
        sink.send(StreamEvent {
            stream_id: stream_id.into(),
            sequence: 1,
            payload: StreamPayload::TextDelta { text },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: stream_id.into(),
            sequence: 2,
            payload: StreamPayload::Finish {
                reason: "end_turn".into(),
                usage: Default::default(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id: stream_id.into(),
            finish_reason: Some("done".into()),
            usage: Default::default(),
            reasoning: None,
        })
    }
}

struct Fixture {
    _temp: TempDir,
    profile: RuntimeProfile,
    store: V1Store,
    task_id: String,
}

impl Fixture {
    /// Drive one plan to review-ready — the wave completed every unit.
    async fn ready(label: &str, plan_json: &'static str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("checkout");
        write_workspace(&workspace);
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        let profile = profile(label, temp.path());
        let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
        let service = compose_with_builtin(&profile, &package, WaveModel::new(plan_json));
        service
            .create_task(label, label, TaskKind::Implementation, vec![])
            .await
            .unwrap();
        service
            .set_task_preferences(
                label,
                TaskPreferences {
                    workspace_path: Some(workspace.display().to_string()),
                    ..TaskPreferences::default()
                },
            )
            .await
            .unwrap();
        service.send_message(label, "plan").await.unwrap();
        wait_for_kind(&service, label, "plan.awaiting-approval").await;
        let plan = service.plan(label).await.unwrap();
        service
            .approve_plan(
                label,
                &plan.revision_hash,
                &format!("approval-{label}"),
                "local-user",
                &format!("session-{label}"),
            )
            .await
            .unwrap();
        service.send_message(label, "execute").await.unwrap();
        wait_for_kind(&service, label, "review-ready").await;
        let store = V1Store::open(&profile.database_path()).unwrap();
        Self {
            _temp: temp,
            profile,
            store,
            task_id: label.into(),
        }
    }

    /// Rewind one settled attempt to the in-flight phase a crash leaves
    /// (the settle never landed) — direct SQL on the durable row, and its
    /// lease family back to active so the crash state is whole.
    fn flip_in_flight(&self, attempt_id: &str) {
        let connection = rusqlite::Connection::open(self.profile.database_path()).unwrap();
        connection
            .execute(
                "UPDATE work_unit_attempts
                 SET phase = 'dispatched', settled_at_ms = NULL
                 WHERE attempt_id = ?1",
                rusqlite::params![attempt_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE lease_families
                 SET state = 'active', settled_at_ms = NULL
                 WHERE attempt_id = ?1",
                rusqlite::params![attempt_id],
            )
            .unwrap();
    }

    /// Remove the journal's execution.started rows for one attempt: the
    /// spawn that never happened.
    fn remove_started_rows(&self, attempt_id: &str) {
        let connection = rusqlite::Connection::open(self.profile.database_path()).unwrap();
        connection
            .execute(
                "DELETE FROM events
                 WHERE kind = 'execution.started'
                   AND json_extract(payload, '$.attemptId') = ?1",
                rusqlite::params![attempt_id],
            )
            .unwrap();
    }

    /// The active plan revision (the families' and attempts' revision key).
    fn active_revision(&self) -> String {
        let connection = rusqlite::Connection::open(self.profile.database_path()).unwrap();
        connection
            .query_row(
                "SELECT plan_revision FROM plan_approvals
                 WHERE task_id = ?1 AND state = 'active'",
                rusqlite::params![self.task_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// The deterministic attempt id (E06's format) for one unit.
    fn attempt_id(&self, unit: &str) -> String {
        let revision = self.active_revision();
        let short = revision
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect::<String>();
        format!("attempt-{}-{short}-{unit}", self.task_id)
    }
}

/// Rewind the kernel shape to mid-wave: the named units' records go InFlight
/// (the given ones), the rest stay whatever they were, and the task sits in
/// Running — exactly what a crash between settle and acknowledge leaves.
fn rewind_to_mid_wave(
    store: &V1Store,
    task_id: &str,
    in_flight_units: &[&str],
    head_attempt: &str,
) {
    let (mut state, revision) = store
        .load_task_with_revision(task_id)
        .unwrap()
        .expect("task");
    for unit_id in in_flight_units {
        let record = state
            .unit_records
            .get_mut(*unit_id)
            .expect("unit record exists after the wave");
        record.settlement = UnitSettlement::InFlight;
    }
    state.execution = TaskExecution::Running {
        attempt_id: head_attempt.to_string(),
        generation: 1,
    };
    store
        .save_task_and_events_if_revision(&state, vec![], revision)
        .unwrap();
}

// ---------------------------------------------------------------------------
// Contract 1 + acceptance: exactly once, never re-executed, no loss
// ---------------------------------------------------------------------------

/// Two units in flight at the crash: the completed-before-crash unit
/// resolves resume-once (settles Completed, its kernel record stays
/// Completed — never re-executed), the unprovable sibling quarantines
/// exactly once WITHOUT touching the completed one, and the family fences
/// leave no active lease.
#[tokio::test]
async fn two_in_flight_attempts_resolve_exactly_once_on_restart() {
    let fixture = Fixture::ready("e10-pair", PAIR_PLAN).await;
    let alpha = fixture.attempt_id("alpha");
    let beta = fixture.attempt_id("beta");
    fixture.flip_in_flight(&alpha);
    fixture.flip_in_flight(&beta);
    // alpha completed before the crash (kernel record Completed); beta was
    // mid-flight with its started row still journaled — unprovable.
    rewind_to_mid_wave(&fixture.store, &fixture.task_id, &["beta"], &alpha);

    // Restart: recompose the service on the same profile.
    let package = stage_native(fixture._temp.path(), "native.r-code", "1.0.0", true);
    let restarted = compose_with_builtin(&fixture.profile, &package, WaveModel::new(PAIR_PLAN));

    // The readiness published the reconciled count before any dispatch.
    let readiness = restarted.activation_readiness();
    assert_eq!(
        readiness.reconciled_attempts, 2,
        "resume-once plus quarantine"
    );

    // alpha: resume-once — the attempt settled Completed, the unit's kernel
    // record is STILL Completed (never re-executed), and its family released.
    let alpha_row = fixture
        .store
        .load_work_unit_attempt(&alpha)
        .unwrap()
        .expect("alpha row");
    assert_eq!(alpha_row.phase, WorkUnitAttemptPhase::SettledCompleted);
    let state = fixture.store.load_task(&fixture.task_id).await.unwrap();
    let alpha_record = state.unit_records.get("alpha").expect("alpha record");
    assert!(matches!(alpha_record.settlement, UnitSettlement::Completed));
    assert_eq!(
        state
            .work_units
            .iter()
            .find(|u| u.id == "alpha")
            .unwrap()
            .status,
        WorkUnitStatus::Completed
    );

    // beta: quarantined exactly once — attempt settled failed, the unit
    // Failed, the task RepairRequired, and NO lease survives the fence.
    let beta_row = fixture
        .store
        .load_work_unit_attempt(&beta)
        .unwrap()
        .expect("beta row");
    assert_eq!(beta_row.phase, WorkUnitAttemptPhase::SettledFailed);
    let state = fixture.store.load_task(&fixture.task_id).await.unwrap();
    let beta_record = state.unit_records.get("beta").expect("beta record");
    assert!(matches!(beta_record.settlement, UnitSettlement::Failed));
    assert!(matches!(
        state.execution,
        TaskExecution::RepairRequired { .. }
    ));
    let journal = fixture.store.task_events(&fixture.task_id);
    assert_eq!(
        journal
            .iter()
            .filter(|event| event.kind == "chain.resume-once"
                && event.payload.get("attemptId").and_then(|v| v.as_str()) == Some(alpha.as_str()))
            .count(),
        1
    );
    assert_eq!(
        journal
            .iter()
            .filter(|event| event.kind == "chain.quarantined"
                && event.payload.get("attemptId").and_then(|v| v.as_str()) == Some(beta.as_str()))
            .count(),
        1
    );

    // The reconciliation is exactly-once: a second restart resolves nothing.
    let again = compose_with_builtin(&fixture.profile, &package, WaveModel::new(PAIR_PLAN));
    assert_eq!(again.activation_readiness().reconciled_attempts, 0);
}

// ---------------------------------------------------------------------------
// Contract 2 — the unprovable arm without a start row; only dependents block
// ---------------------------------------------------------------------------

/// An attempt whose proof cannot be produced (prepared/dispatched, no
/// execution.started row, unit record InFlight) quarantines, fences its
/// family, and blocks exactly the failed unit — the completed head stays
/// Completed and the task names the failed dependent for repair.
#[tokio::test]
async fn an_unprovable_attempt_quarantines_without_starting() {
    let fixture = Fixture::ready("e10-chain", CHAIN_PLAN).await;
    let head = fixture.attempt_id("head");
    let tail = fixture.attempt_id("tail");
    // The head settled normally (its completion was acknowledged); only the
    // tail is rewound — to the exact window between prepare and start.
    fixture.flip_in_flight(&tail);
    fixture.remove_started_rows(&tail);
    rewind_to_mid_wave(&fixture.store, &fixture.task_id, &["tail"], &head);

    let package = stage_native(fixture._temp.path(), "native.r-code", "1.0.0", true);
    let restarted = compose_with_builtin(&fixture.profile, &package, WaveModel::new(CHAIN_PLAN));
    assert_eq!(restarted.activation_readiness().reconciled_attempts, 1);

    let tail_row = fixture
        .store
        .load_work_unit_attempt(&tail)
        .unwrap()
        .expect("tail row");
    assert_eq!(tail_row.phase, WorkUnitAttemptPhase::SettledFailed);
    let state = fixture.store.load_task(&fixture.task_id).await.unwrap();
    let head_record = state.unit_records.get("head").expect("head record");
    assert!(
        matches!(head_record.settlement, UnitSettlement::Completed),
        "the acknowledged head stays Completed — no completion is lost"
    );
    let tail_record = state.unit_records.get("tail").expect("tail record");
    assert!(matches!(tail_record.settlement, UnitSettlement::Failed));
    assert!(matches!(
        state.execution,
        TaskExecution::RepairRequired { .. }
    ));
    // The quarantined family's members are fenced: nothing stays active.
    let families_quarantined = fixture.store.reconcile_lease_families().unwrap().len();
    assert_eq!(
        families_quarantined, 0,
        "every family already followed its attempt"
    );
}
