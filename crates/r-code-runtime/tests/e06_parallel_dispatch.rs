//! E06 — bounded parallel WorkUnit dispatch, durable attempt records, and
//! read-set drift revalidation, proven through real daemon managers and the
//! native harness (the m03 fixtures' idioms: one staged native package, one
//! workspace checkout, one journal-observing store handle per profile).
//!
//! One approved plan now drives the WHOLE WorkUnit DAG in a single wave
//! (`run_execution_wave`): the ready set (Pending AND writable AND
//! dependencies Completed AND read-set revalidation passing) is dispatched up
//! to the bound of 2, every dispatch writes its durable `work_unit_attempts`
//! row before any spawn, and each unit completes strictly from its own
//! attempt's durable settle.

mod p_gate_support;

use p_gate_support::{compose_with_builtin, profile, stage_native, wait_for_kind, write_workspace};
use r_code_harness_protocol::services::{ModelStreamRequest, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::{
    TaskExecution, TaskKind, TaskPreferences, TaskState, UnitSettlement, ValidationOutcome,
    WorkUnitStatus,
};
use r_code_runtime::services::artifacts::sha256_hex;
use r_code_runtime::RuntimeProfile;
use r_code_store::v1::{V1Store, WorkUnitAttemptError, WorkUnitAttemptPhase, WorkUnitAttemptSeed};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A 4-unit diamond: base -> {left, right} -> join. The two arms carry
/// disjoint write scopes so the bounded dispatcher can hold both leases at
/// once; every acceptance is check-free so the units verify trivially and the
/// wave can reach ReviewReady.
const DIAMOND_PLAN: &str = r#"{"work_units":[
    {"id":"base","description":"prepare the diamond root","dependencies":[],"acceptance":["read-only"],"write_paths":["base"]},
    {"id":"left","description":"left arm","dependencies":["base"],"acceptance":["read-only"],"write_paths":["left"]},
    {"id":"right","description":"right arm","dependencies":["base"],"acceptance":["read-only"],"write_paths":["right"]},
    {"id":"join","description":"join both arms","dependencies":["left","right"],"acceptance":["read-only"],"write_paths":["final"]}
]}"#;

/// Two independent arms plus their join: left's write scope is the drift
/// target, right is the untouched sibling, join is the re-blocked dependent.
const DRIFT_PLAN: &str = r#"{"work_units":[
    {"id":"left","description":"drifting dependency","dependencies":[],"acceptance":["read-only"],"write_paths":["left"]},
    {"id":"right","description":"untouched sibling","dependencies":[],"acceptance":["read-only"],"write_paths":["right"]},
    {"id":"join","description":"depends on both arms","dependencies":["left","right"],"acceptance":["read-only"],"write_paths":["final"]}
]}"#;

/// Two independent units with no checks: the smallest wave that proves
/// concurrent dispatch and one-row-per-completion attribution.
const PAIR_PLAN: &str = r#"{"work_units":[
    {"id":"alpha","description":"independent unit one","dependencies":[],"acceptance":["read-only"],"write_paths":["alpha-scope"]},
    {"id":"beta","description":"independent unit two","dependencies":[],"acceptance":["read-only"],"write_paths":["beta-scope"]}
]}"#;

/// A trivially-completing two-unit chain: the dependent is frontier-held
/// (seeded InFlight) before its dependency settles, then really started.
const CHAIN_PLAN: &str = r#"{"work_units":[
    {"id":"alpha","description":"chain head","dependencies":[],"acceptance":["read-only"],"write_paths":["alpha-scope"]},
    {"id":"beta","description":"chain dependent","dependencies":["alpha"],"acceptance":["read-only"],"write_paths":["beta-scope"]}
]}"#;

/// One independent unit fails verification (its acceptance names a check that
/// has no definition in this fixture); the sibling completes.
const FAILING_PAIR_PLAN: &str = r#"{"work_units":[
    {"id":"good","description":"completing sibling","dependencies":[],"acceptance":["read-only"],"write_paths":["good-scope"]},
    {"id":"bad","description":"fails verification","dependencies":[],"acceptance":["check:missing"],"write_paths":["bad-scope"]}
]}"#;

/// The chain head fails verification AFTER the dependent was frontier-seeded:
/// the seed must not hold the aggregate open past the wave's end.
const FAILING_CHAIN_PLAN: &str = r#"{"work_units":[
    {"id":"alpha","description":"fails verification","dependencies":[],"acceptance":["check:missing"],"write_paths":["alpha-scope"]},
    {"id":"beta","description":"never-started dependent","dependencies":["alpha"],"acceptance":["read-only"],"write_paths":["beta-scope"]}
]}"#;

/// A slow scripted model that also measures how many harness runs overlap.
/// Each execution run streams exactly once (the native loop's text-only turn),
/// so `max_active` is the wave's observed concurrency over the model seam.
struct WaveModel {
    final_text: String,
    delay: Duration,
    active: AtomicUsize,
    max_active: AtomicUsize,
    streams: AtomicUsize,
}

impl WaveModel {
    fn new(final_text: &str, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            final_text: final_text.to_string(),
            delay,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            streams: AtomicUsize::new(0),
        })
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }

    fn streams(&self) -> usize {
        self.streams.load(Ordering::SeqCst)
    }
}

struct ActiveGuard<'a>(&'a AtomicUsize);

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl ModelService for WaveModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.streams.fetch_add(1, Ordering::SeqCst);
        let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now_active, Ordering::SeqCst);
        let _guard = ActiveGuard(&self.active);
        tokio::time::sleep(self.delay).await;
        let stream_id = format!("e06-stream-{}", self.streams.load(Ordering::SeqCst));
        sink.send(StreamEvent {
            stream_id: stream_id.clone(),
            sequence: 1,
            payload: StreamPayload::TextDelta {
                text: self.final_text.clone(),
            },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: stream_id.clone(),
            sequence: 2,
            payload: StreamPayload::Finish {
                reason: "end_turn".into(),
                usage: Default::default(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id,
            finish_reason: Some("done".into()),
            usage: Default::default(),
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn workspace_key(workspace: &Path) -> String {
    let canonical = std::fs::canonicalize(workspace).unwrap();
    format!(
        "sha256:{}",
        sha256_hex(canonical.to_string_lossy().as_bytes())
    )
}

/// The deterministic dispatch identity for one (task, plan revision, unit).
fn expected_attempt_id(task_id: &str, revision_hash: &str, unit_id: &str) -> String {
    let short = revision_hash.trim_start_matches("sha256:");
    format!("attempt-{task_id}-{}-{unit_id}", &short[..12])
}

/// One durable attempt row read straight from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttemptRow {
    attempt_id: String,
    task_id: String,
    plan_revision: String,
    work_unit_id: String,
    phase: String,
    content_sha256: String,
}

fn attempt_rows(profile: &RuntimeProfile) -> Vec<AttemptRow> {
    let connection = rusqlite::Connection::open(profile.database_path()).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT attempt_id, task_id, plan_revision, work_unit_id, phase, content_sha256
             FROM work_unit_attempts ORDER BY attempt_id",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok(AttemptRow {
                attempt_id: row.get(0)?,
                task_id: row.get(1)?,
                plan_revision: row.get(2)?,
                work_unit_id: row.get(3)?,
                phase: row.get(4)?,
                content_sha256: row.get(5)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    rows
}

/// Journal events of one kind, with their durable sequence numbers.
fn events_of(store: &V1Store, task_id: &str, kind: &str) -> Vec<(u64, serde_json::Value)> {
    store
        .task_events(task_id)
        .into_iter()
        .filter(|event| event.kind == kind)
        .map(|event| (event.seq, event.payload))
        .collect()
}

fn started_units(store: &V1Store, task_id: &str) -> Vec<(u64, String, String, String)> {
    events_of(store, task_id, "execution.started")
        .into_iter()
        .map(|(seq, payload)| {
            (
                seq,
                payload
                    .get("workUnitId")
                    .and_then(|v| v.as_str())
                    .unwrap()
                    .to_string(),
                payload
                    .get("attemptId")
                    .and_then(|v| v.as_str())
                    .unwrap()
                    .to_string(),
                payload
                    .get("snapshotId")
                    .and_then(|v| v.as_str())
                    .unwrap()
                    .to_string(),
            )
        })
        .collect()
}

/// Wait until the task leaves the wave with a task-level aggregate
/// (ReviewReady or RepairRequired). Panics with the full state and journal
/// otherwise — a wave that stalls in Running/Verifying is a failed pin, not
/// an inconvenient fixture.
async fn wait_for_aggregate(store: &V1Store, task_id: &str) -> TaskState {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let state = store.load_task(task_id).await.unwrap();
        match &state.execution {
            TaskExecution::ReviewReady { .. } | TaskExecution::RepairRequired { .. } => {
                return state
            }
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "task {task_id} never reached a task-level aggregate; state={:?}; events={:?}",
            state.execution,
            store.task_events(task_id)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Drive one task to an approved plan and return its revision hash.
async fn approved_task(
    service: &r_code_runtime::application::ApplicationService,
    task_id: &str,
    workspace: &Path,
) -> String {
    service
        .create_task(task_id, task_id, TaskKind::Implementation, vec![])
        .await
        .unwrap();
    service
        .set_task_preferences(
            task_id,
            TaskPreferences {
                workspace_path: Some(workspace.display().to_string()),
                ..TaskPreferences::default()
            },
        )
        .await
        .unwrap();
    service.send_message(task_id, "plan").await.unwrap();
    wait_for_kind(service, task_id, "plan.awaiting-approval").await;
    let plan = service.plan(task_id).await.unwrap();
    service
        .approve_plan(
            task_id,
            &plan.revision_hash,
            &format!("approval-{task_id}"),
            "local-user",
            &format!("session-{task_id}"),
        )
        .await
        .unwrap();
    plan.revision_hash
}

/// Wait until one exact unit has a durable execution.started event.
async fn wait_for_unit_start(store: &V1Store, task_id: &str, unit_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !started_units(store, task_id)
        .iter()
        .any(|(_, unit, _, _)| unit == unit_id)
    {
        assert!(
            Instant::now() < deadline,
            "unit {unit_id} of {task_id} never started; events={:?}",
            store.task_events(task_id)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn assert_no_inflight_records(task_id: &str, state: &TaskState) {
    let leaked: Vec<String> = state
        .unit_records
        .iter()
        .filter(|(_, record)| record.settlement == UnitSettlement::InFlight)
        .map(|(unit, _)| unit.clone())
        .collect();
    assert!(
        leaked.is_empty(),
        "{task_id} left frontier-hold seed records in flight ({leaked:?}) after the wave; state={:?}",
        state.execution
    );
}

// ---------------------------------------------------------------------------
// E06 arm 1 — the diamond
// ---------------------------------------------------------------------------

/// A diamond DAG dispatches both arms concurrently (bound respected), the
/// join starts only after both arms verified, the task aggregates to
/// ReviewReady with every unit Completed, and every completion attributes to
/// exactly one durable attempt row with the deterministic id format.
#[tokio::test]
async fn diamond_dag_dispatches_arms_concurrently_and_joins_only_after_both() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let runtime_profile = profile("e06-diamond", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let model = WaveModel::new(DIAMOND_PLAN, Duration::from_millis(1200));
    let service = compose_with_builtin(&runtime_profile, &package, model.clone());
    let revision_hash = approved_task(&service, "diamond", &workspace).await;
    let store = V1Store::open(&runtime_profile.database_path()).unwrap();

    service
        .send_message("diamond", "implement the whole dag")
        .await
        .unwrap();
    let state = wait_for_aggregate(&store, "diamond").await;
    assert!(
        matches!(state.execution, TaskExecution::ReviewReady { .. }),
        "diamond must aggregate to ReviewReady, got {:?}; events={:?}",
        state.execution,
        store.task_events("diamond")
    );

    // Both arms started — and the whole DAG ran: one start per unit.
    let starts = started_units(&store, "diamond");
    let mut started_ids = starts
        .iter()
        .map(|(_, unit, _, _)| unit.clone())
        .collect::<Vec<_>>();
    started_ids.sort();
    assert_eq!(
        started_ids,
        vec![
            "base".to_string(),
            "join".to_string(),
            "left".to_string(),
            "right".to_string()
        ],
        "one execution.started per diamond unit"
    );

    // The bound is never exceeded and the two arms genuinely overlapped:
    // the model seam observed exactly 2 concurrent runs (never 1-only, never
    // 3+). Planning + base + join run alone; the arms run together.
    assert_eq!(
        model.max_active(),
        2,
        "the two diamond arms must run concurrently within the bound of 2"
    );
    assert_eq!(model.streams(), 5, "one planning run plus one run per unit");

    // The join starts only after BOTH arms verified: its durable start is
    // sequenced after both arms' mid-wave unit.completed settles.
    let join_start_seq = starts
        .iter()
        .find(|(_, unit, _, _)| unit == "join")
        .map(|(seq, _, _, _)| *seq)
        .expect("join started");
    for arm in ["left", "right"] {
        let completed = events_of(&store, "diamond", "unit.completed")
            .into_iter()
            .find(|(_, payload)| payload.get("workUnitId").and_then(|v| v.as_str()) == Some(arm))
            .unwrap_or_else(|| panic!("arm {arm} must settle with a mid-wave unit.completed"));
        assert!(
            join_start_seq > completed.0,
            "join (seq {join_start_seq}) must not start before arm {arm} verified (seq {})",
            completed.0
        );
    }

    // All units Completed; nothing left in flight; review still pending.
    assert!(state
        .work_units
        .iter()
        .all(|unit| unit.status == WorkUnitStatus::Completed));
    assert_no_inflight_records("diamond", &state);
    for unit in ["base", "left", "right", "join"] {
        assert!(
            matches!(
                state.unit_record(unit).map(|record| &record.verification),
                Some(ValidationOutcome::Verified { .. })
            ),
            "unit {unit} must be Verified in the aggregate"
        );
    }
    assert!(store
        .active_leases(&workspace_key(&workspace))
        .unwrap()
        .is_empty());

    // Every completion attributes to exactly one durable attempt row, all
    // settled-completed, with the contract's deterministic id format.
    let rows = attempt_rows(&runtime_profile);
    assert_eq!(rows.len(), 4, "one attempt row per diamond unit: {rows:?}");
    for (_, unit, attempt_id, snapshot_id) in &starts {
        let expected = expected_attempt_id("diamond", &revision_hash, unit);
        assert_eq!(attempt_id, &expected, "attempt id format for unit {unit}");
        let row = rows
            .iter()
            .find(|row| &row.work_unit_id == unit)
            .unwrap_or_else(|| panic!("durable attempt row for unit {unit}"));
        assert_eq!(row.attempt_id, expected);
        assert_eq!(row.phase, "settled-completed", "row for {unit}: {row:?}");
        assert_eq!(
            &row.content_sha256, snapshot_id,
            "content pins the frozen snapshot"
        );
        assert_eq!(row.plan_revision, revision_hash);
        // Single attribution: exactly one row and exactly one completion
        // event (mid-wave unit.completed or the final review-ready) name it.
        let completions = events_of(&store, "diamond", "unit.completed")
            .into_iter()
            .chain(events_of(&store, "diamond", "review-ready"))
            .filter(|(_, payload)| {
                payload.get("attemptId").and_then(|v| v.as_str()) == Some(attempt_id.as_str())
            })
            .count();
        assert_eq!(completions, 1, "unit {unit} completes exactly once");
    }
}

// ---------------------------------------------------------------------------
// E06 arm 2 — external edit during flight re-blocks the dependent
// ---------------------------------------------------------------------------

/// An external edit inside a dependency's write scope during its flight is
/// unexplained at settle: the dependent is re-blocked (never started, never
/// overwritten), the task aggregates to RepairRequired naming the dependent,
/// the untouched sibling completes, and the external edit survives verbatim.
#[tokio::test]
async fn external_edit_during_flight_re_blocks_the_dependent() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let runtime_profile = profile("e06-drift", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let model = WaveModel::new(DRIFT_PLAN, Duration::from_millis(1200));
    let service = compose_with_builtin(&runtime_profile, &package, model.clone());
    let revision_hash = approved_task(&service, "drift", &workspace).await;
    let store = V1Store::open(&runtime_profile.database_path()).unwrap();

    service
        .send_message("drift", "implement the drift dag")
        .await
        .unwrap();

    // Land an external edit inside `left`'s write scope WHILE it is in
    // flight (its run sleeps inside the model stream), far outside any
    // journaled mutation.
    wait_for_unit_start(&store, "drift", "left").await;
    let external = workspace.join("left").join("external-edit.txt");
    std::fs::create_dir_all(workspace.join("left")).unwrap();
    std::fs::write(&external, b"EXTERNAL EDIT DURING FLIGHT\n").unwrap();

    let state = wait_for_aggregate(&store, "drift").await;
    match &state.execution {
        TaskExecution::RepairRequired { work_unit_id, .. } => {
            assert_eq!(
                work_unit_id.as_deref(),
                Some("join"),
                "the aggregate must name the re-blocked dependent"
            );
        }
        other => panic!(
            "external drift must re-block the dependent into RepairRequired, got {other:?}; events={:?}",
            store.task_events("drift")
        ),
    }

    // The drifting dependency and the untouched sibling both complete; the
    // dependent is Blocked with a Failed record naming the external drift.
    for unit in ["left", "right"] {
        let record = state.unit_record(unit).expect("settled arm record");
        assert!(
            matches!(record.verification, ValidationOutcome::Verified { .. }),
            "{unit} must still complete: {record:?}"
        );
        assert_eq!(record.settlement, UnitSettlement::Completed);
        assert!(state
            .work_units
            .iter()
            .any(|u| u.id == unit && u.status == WorkUnitStatus::Completed));
    }
    let join = state
        .unit_record("join")
        .expect("re-blocked dependent record");
    assert_eq!(join.settlement, UnitSettlement::Failed);
    match &join.verification {
        ValidationOutcome::Unverified { reason } => {
            assert!(
                reason.contains("drifted externally"),
                "the failure must name the external drift: {reason}"
            );
            assert!(
                reason.contains("left/external-edit.txt"),
                "the failure must name the externally edited path: {reason}"
            );
        }
        other => panic!("re-blocked dependent must carry the drift reason: {other:?}"),
    }
    assert!(state
        .work_units
        .iter()
        .any(|u| u.id == "join" && u.status == WorkUnitStatus::Blocked));

    // The dependent never started and owns no attempt row.
    let starts = started_units(&store, "drift");
    assert_eq!(starts.len(), 2, "only the two arms may start: {starts:?}");
    assert!(starts.iter().all(|(_, unit, _, _)| unit != "join"));
    let rows = attempt_rows(&runtime_profile);
    assert_eq!(
        rows.len(),
        2,
        "no attempt row may exist for the join: {rows:?}"
    );
    for unit in ["left", "right"] {
        let row = rows
            .iter()
            .find(|row| row.work_unit_id == unit)
            .unwrap_or_else(|| panic!("attempt row for {unit}"));
        assert_eq!(
            row.attempt_id,
            expected_attempt_id("drift", &revision_hash, unit)
        );
        assert_eq!(row.phase, "settled-completed");
    }

    // INV-10: nothing overwrote the external edit, and no lease survived.
    assert_eq!(
        std::fs::read(&external).unwrap(),
        b"EXTERNAL EDIT DURING FLIGHT\n",
        "the external edit must survive verbatim"
    );
    assert!(store
        .active_leases(&workspace_key(&workspace))
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------
// E06 arm 3 — idempotent attempt records, single attribution
// ---------------------------------------------------------------------------

/// Attempt records are idempotent under identical replay (whatever phase the
/// row is in), refuse divergent content or a same-triple-different-id replay
/// (ContentConflict), settle exactly once (AlreadySettled on any second
/// settle), and every completion of a real wave attributes to exactly one
/// row.
#[tokio::test]
async fn attempt_records_are_idempotent_and_single_attribution() {
    // -- The repository contract, straight against a durable store ---------
    let store_temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&store_temp.path().join("attempts.db")).unwrap();
    let revision = format!("sha256:{}", "c1".repeat(32));
    let seed = WorkUnitAttemptSeed {
        attempt_id: "attempt-e06-idem-trial-alpha".to_string(),
        task_id: "e06-idem".to_string(),
        plan_revision: revision.clone(),
        work_unit_id: "alpha".to_string(),
        content_sha256: "sha256:frozen-snapshot-1".to_string(),
    };

    let first = store.prepare_work_unit_attempt(&seed).unwrap();
    assert_eq!(first.phase, WorkUnitAttemptPhase::Prepared);
    // Identical replay converges on the existing row.
    let replay = store.prepare_work_unit_attempt(&seed).unwrap();
    assert_eq!(replay, first, "identical replay must converge");

    // Divergent content under the same attempt id refuses.
    let divergent = WorkUnitAttemptSeed {
        content_sha256: "sha256:a-different-snapshot".to_string(),
        ..seed.clone()
    };
    assert_eq!(
        store.prepare_work_unit_attempt(&divergent).unwrap_err(),
        WorkUnitAttemptError::ContentConflict {
            attempt_id: seed.attempt_id.clone()
        }
    );
    // The same (task, plan, unit) under a different attempt id refuses.
    let reidentified = WorkUnitAttemptSeed {
        attempt_id: "attempt-e06-idem-trial-alpha-2".to_string(),
        ..seed.clone()
    };
    assert_eq!(
        store.prepare_work_unit_attempt(&reidentified).unwrap_err(),
        WorkUnitAttemptError::ContentConflict {
            attempt_id: reidentified.attempt_id.clone()
        }
    );

    // Dispatch is idempotent while unsettled.
    let dispatched = store
        .mark_work_unit_attempt_dispatched(&seed.attempt_id)
        .unwrap();
    assert_eq!(dispatched.phase, WorkUnitAttemptPhase::Dispatched);
    assert_eq!(
        store
            .mark_work_unit_attempt_dispatched(&seed.attempt_id)
            .unwrap()
            .phase,
        WorkUnitAttemptPhase::Dispatched
    );
    let in_flight = store
        .list_in_flight_work_unit_attempts("e06-idem", &revision)
        .unwrap();
    assert_eq!(in_flight.len(), 1);
    assert_eq!(in_flight[0].attempt_id, seed.attempt_id);

    // Settle is exactly once: any second settle refuses, and a settled row
    // no longer accepts dispatch or divergent replays — but an IDENTICAL
    // replay still converges (replay never resurrects).
    let settled = store
        .settle_work_unit_attempt(&seed.attempt_id, true)
        .unwrap();
    assert_eq!(settled.phase, WorkUnitAttemptPhase::SettledCompleted);
    assert!(settled.settled_at_ms.is_some());
    assert!(matches!(
        store
            .settle_work_unit_attempt(&seed.attempt_id, true)
            .unwrap_err(),
        WorkUnitAttemptError::AlreadySettled(_)
    ));
    assert!(matches!(
        store
            .settle_work_unit_attempt(&seed.attempt_id, false)
            .unwrap_err(),
        WorkUnitAttemptError::AlreadySettled(_)
    ));
    assert!(matches!(
        store
            .mark_work_unit_attempt_dispatched(&seed.attempt_id)
            .unwrap_err(),
        WorkUnitAttemptError::AlreadySettled(_)
    ));
    assert_eq!(
        store.prepare_work_unit_attempt(&seed).unwrap().phase,
        WorkUnitAttemptPhase::SettledCompleted,
        "identical replay after settle converges on the settled row"
    );
    assert_eq!(
        store.prepare_work_unit_attempt(&divergent).unwrap_err(),
        WorkUnitAttemptError::ContentConflict {
            attempt_id: seed.attempt_id.clone()
        }
    );
    assert!(store
        .list_in_flight_work_unit_attempts("e06-idem", &revision)
        .unwrap()
        .is_empty());

    // -- The same contract over a real wave --------------------------------
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let runtime_profile = profile("e06-idem", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let model = WaveModel::new(PAIR_PLAN, Duration::from_millis(1500));
    let service = compose_with_builtin(&runtime_profile, &package, model.clone());
    let revision_hash = approved_task(&service, "idem", &workspace).await;
    let wave_store = V1Store::open(&runtime_profile.database_path()).unwrap();

    service
        .send_message("idem", "implement both units")
        .await
        .unwrap();
    let state = wait_for_aggregate(&wave_store, "idem").await;
    assert!(
        matches!(state.execution, TaskExecution::ReviewReady { .. }),
        "both units must complete, got {:?}; events={:?}",
        state.execution,
        wave_store.task_events("idem")
    );

    let starts = started_units(&wave_store, "idem");
    let mut started_ids = starts
        .iter()
        .map(|(_, unit, _, _)| unit.clone())
        .collect::<Vec<_>>();
    started_ids.sort();
    assert_eq!(started_ids, vec!["alpha".to_string(), "beta".to_string()]);
    assert_eq!(model.max_active(), 2, "the independent pair must overlap");

    // Each completion attributes to exactly one durable row.
    let rows = attempt_rows(&runtime_profile);
    assert_eq!(rows.len(), 2, "exactly one row per unit: {rows:?}");
    for (_, unit, attempt_id, snapshot_id) in &starts {
        let expected = expected_attempt_id("idem", &revision_hash, unit);
        assert_eq!(attempt_id, &expected, "deterministic id for unit {unit}");
        let row = rows
            .iter()
            .find(|row| &row.work_unit_id == unit)
            .unwrap_or_else(|| panic!("row for {unit}"));
        assert_eq!(row.phase, "settled-completed");
        assert_eq!(&row.content_sha256, snapshot_id);
        let completions = events_of(&wave_store, "idem", "unit.completed")
            .into_iter()
            .chain(events_of(&wave_store, "idem", "review-ready"))
            .filter(|(_, payload)| {
                payload.get("attemptId").and_then(|v| v.as_str()) == Some(attempt_id.as_str())
            })
            .count();
        assert_eq!(completions, 1, "unit {unit} completes exactly once");
        // Re-preparing the REAL dispatch material (same attempt id, same
        // frozen snapshot digest) converges on the settled row instead of
        // minting or mutating anything.
        let replay = wave_store
            .prepare_work_unit_attempt(&WorkUnitAttemptSeed {
                attempt_id: attempt_id.clone(),
                task_id: "idem".to_string(),
                plan_revision: revision_hash.clone(),
                work_unit_id: unit.clone(),
                content_sha256: snapshot_id.clone(),
            })
            .unwrap();
        assert_eq!(replay.phase, WorkUnitAttemptPhase::SettledCompleted);
        assert!(matches!(
            wave_store
                .prepare_work_unit_attempt(&WorkUnitAttemptSeed {
                    content_sha256: "sha256:not-the-frozen-snapshot".to_string(),
                    attempt_id: attempt_id.clone(),
                    task_id: "idem".to_string(),
                    plan_revision: revision_hash.clone(),
                    work_unit_id: unit.clone(),
                })
                .unwrap_err(),
            WorkUnitAttemptError::ContentConflict { .. }
        ));
    }
    assert_eq!(
        attempt_rows(&runtime_profile).len(),
        2,
        "replays add no rows"
    );
}

// ---------------------------------------------------------------------------
// Extra pin — frontier-hold seeds must not leak past the wave's end
// ---------------------------------------------------------------------------

/// After a wave that completes AND after waves that fail one unit, no unit
/// record is left settlement=InFlight while the task is out of Running /
/// Verifying: the frontier-hold seeds exist only to keep the aggregate open
/// inside a live wave, so a wave that ends without starting a seeded
/// dependent must retract its seed.
#[tokio::test]
async fn no_leaked_inflight_seed_records_at_wave_end() {
    // (a) A completed chain: the dependent is seeded while its dependency
    // settles, then really started; ReviewReady leaves nothing in flight.
    {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("checkout");
        write_workspace(&workspace);
        let runtime_profile = profile("e06-seed-complete", temp.path());
        let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
        let service = compose_with_builtin(
            &runtime_profile,
            &package,
            WaveModel::new(CHAIN_PLAN, Duration::from_millis(200)),
        );
        approved_task(&service, "seed-complete", &workspace).await;
        let store = V1Store::open(&runtime_profile.database_path()).unwrap();
        service
            .send_message("seed-complete", "implement the chain")
            .await
            .unwrap();
        let state = wait_for_aggregate(&store, "seed-complete").await;
        assert!(matches!(state.execution, TaskExecution::ReviewReady { .. }));
        assert!(state
            .work_units
            .iter()
            .all(|unit| unit.status == WorkUnitStatus::Completed));
        assert_no_inflight_records("seed-complete", &state);
    }

    // (b) A wave that fails one independent unit: the sibling completes, the
    // aggregate lands RepairRequired naming the failed unit, and no seeded
    // record survives the failure.
    {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("checkout");
        write_workspace(&workspace);
        let runtime_profile = profile("e06-seed-fail-pair", temp.path());
        let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
        let service = compose_with_builtin(
            &runtime_profile,
            &package,
            WaveModel::new(FAILING_PAIR_PLAN, Duration::from_millis(200)),
        );
        approved_task(&service, "seed-fail-pair", &workspace).await;
        let store = V1Store::open(&runtime_profile.database_path()).unwrap();
        service
            .send_message("seed-fail-pair", "implement the failing pair")
            .await
            .unwrap();
        let state = wait_for_aggregate(&store, "seed-fail-pair").await;
        match &state.execution {
            TaskExecution::RepairRequired { work_unit_id, .. } => {
                assert_eq!(work_unit_id.as_deref(), Some("bad"));
            }
            other => panic!("the failing pair must land RepairRequired, got {other:?}"),
        }
        assert!(state
            .work_units
            .iter()
            .any(|u| u.id == "good" && u.status == WorkUnitStatus::Completed));
        assert_no_inflight_records("seed-fail-pair", &state);
    }

    // (c) The leak probe: the chain head fails verification AFTER the
    // dependent was frontier-seeded. The wave is over (nothing can start:
    // the dependency failed), so the seed must be retracted and the
    // aggregate must land RepairRequired — never stall in Running with an
    // InFlight seed holding the task open.
    {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("checkout");
        write_workspace(&workspace);
        let runtime_profile = profile("e06-seed-fail-chain", temp.path());
        let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
        let service = compose_with_builtin(
            &runtime_profile,
            &package,
            WaveModel::new(FAILING_CHAIN_PLAN, Duration::from_millis(200)),
        );
        approved_task(&service, "seed-fail-chain", &workspace).await;
        let store = V1Store::open(&runtime_profile.database_path()).unwrap();
        service
            .send_message("seed-fail-chain", "implement the failing chain")
            .await
            .unwrap();
        let state = wait_for_aggregate(&store, "seed-fail-chain").await;
        match &state.execution {
            TaskExecution::RepairRequired { work_unit_id, .. } => {
                assert_eq!(
                    work_unit_id.as_deref(),
                    Some("alpha"),
                    "the failed head must be named"
                );
            }
            other => panic!(
                "a failed head with a seeded dependent must land RepairRequired, got {other:?}"
            ),
        }
        assert_no_inflight_records("seed-fail-chain", &state);
        assert!(state
            .work_units
            .iter()
            .any(|u| u.id == "beta" && u.status == WorkUnitStatus::Blocked));
    }
}
