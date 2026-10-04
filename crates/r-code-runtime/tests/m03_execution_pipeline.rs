//! m03 执行管线端到端。macOS：daemon→native 链路依赖 P13 安全激活报告，
//! 本 wave 固定 Unsupported——按设计拒绝启动；用例由 linux/windows 腿运行。
#![cfg(not(target_os = "macos"))]

mod p_gate_support;

use p_gate_support::{
    compose_with_builtin, profile, stage_native, tool_names, wait_for_kind, write_workspace,
    ScriptedModel, STRICT_PLAN,
};
use r_code_gateway::execution_backend::{CommandExecutionBackend, LocalShellBackend};
use r_code_harness_protocol::services::{ModelStreamRequest, StreamEvent, StreamPayload};
use r_code_harness_protocol::{HarnessId, PackageRef, Provenance};
use r_code_kernel::plans::PlanRevision;
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::{
    Actor, Attempt, EvidenceRequirement, NetworkCeiling, PlanApprovalRef, PlanRevisionRef,
    ReviewDisposition, RunSnapshotId, RunSnapshotPhase, TaskContract, TaskExecution, TaskKind,
    TaskPreferences, TaskState, TransitionError, ValidationOutcome, WorkUnit, WorkUnitEffectClass,
    WorkUnitStatus,
};
use r_code_kernel::verification::{CheckDefinition, CheckEntrypoint};
use r_code_runtime::process_guard::BootIdentity;
use r_code_runtime::services::artifacts::sha256_hex;
use r_code_runtime::services::execution::{CheckSandboxGate, SandboxedCheckBackend};
use r_code_runtime::services::sandbox::current_platform_material;
use r_code_runtime::services::verification::{CheckOutcome, CheckStatus, VerificationRunner};
use r_code_runtime::services::verification_inputs::FrozenControlStore;
use r_code_runtime::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_store::v1::{LeaseRequest, V1Store};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

struct DelayedModel;

#[async_trait::async_trait]
impl ModelService for DelayedModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        tokio::time::sleep(Duration::from_secs(2)).await;
        sink.send(StreamEvent {
            stream_id: "delayed".into(),
            sequence: 1,
            payload: StreamPayload::TextDelta {
                text: "done".into(),
            },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: "delayed".into(),
            sequence: 2,
            payload: StreamPayload::Finish {
                reason: "end_turn".into(),
                usage: Default::default(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id: "delayed".into(),
            finish_reason: Some("done".into()),
            usage: Default::default(),
            reasoning: None,
        })
    }
}

fn writable_plan(acceptance: &[&str]) -> String {
    serde_json::json!({
        "work_units": [{
            "id": "implement",
            "description": "implement approved unit",
            "dependencies": [],
            "acceptance": acceptance,
            "write_paths": ["src"]
        }]
    })
    .to_string()
}

/// E06 semantic rebase of STRICT_PLAN for the two single-send execution
/// tests: one explicit send now drives the WHOLE two-unit DAG, so the
/// implement unit's `check:test` acceptance — never exercised pre-E06 (only
/// the first unit ran) — would send this fixture to RepairRequired (the
/// platform sandbox gate is honestly closed here, so even a defined
/// check:test can only come back CheckUnavailable). The check acceptance is
/// dropped from the FIXTURE PLAN (same units, same dependencies) so the
/// review-ready end-state stays pinned under the E06 contract.
const PIPELINE_PLAN: &str = r#"{"work_units":[{"id":"inspect","description":"inspect current checkout","dependencies":[],"acceptance":["read-only"],"write_paths":["src"]},{"id":"implement","description":"implement after approval","dependencies":["inspect"],"acceptance":["read-only"],"write_paths":["src"]}]}"#;

/// The deterministic E06 dispatch identity for one (task, plan, unit).
fn expected_attempt_id(task_id: &str, revision_hash: &str, unit_id: &str) -> String {
    let short = revision_hash.trim_start_matches("sha256:");
    format!("attempt-{task_id}-{}-{unit_id}", &short[..12])
}

/// The durable work_unit_attempts rows of one profile, ordered by attempt id.
fn attempt_rows(profile: &r_code_runtime::RuntimeProfile) -> Vec<(String, String, String)> {
    let connection = rusqlite::Connection::open(profile.database_path()).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT attempt_id, work_unit_id, phase FROM work_unit_attempts ORDER BY attempt_id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn node_definition() -> CheckDefinition {
    CheckDefinition {
        check_id: "check:node".into(),
        entrypoint: CheckEntrypoint::Command {
            program: "node".into(),
            argv: vec!["verify.js".into()],
        },
        control_files: vec![],
        source_roots: vec![".".into()],
        dependency_locks: vec![],
        toolchain: "node-test".into(),
        declared_external_inputs: vec![],
        entrypoint_bytes: None,
    }
}

fn workspace_key(workspace: &Path) -> String {
    let canonical = std::fs::canonicalize(workspace).unwrap();
    format!(
        "sha256:{}",
        sha256_hex(canonical.to_string_lossy().as_bytes())
    )
}

async fn wait_for_execution_state(store: &V1Store, task_id: &str, expected_event: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let state = store.load_task(task_id).await.unwrap();
        let matched = matches!(
            (&state.execution, expected_event),
            (TaskExecution::ReviewReady { .. }, "review-ready")
                | (TaskExecution::RepairRequired { .. }, "repair-required")
        );
        if matched {
            return;
        }
        if matches!(
            state.execution,
            TaskExecution::ReviewReady { .. } | TaskExecution::RepairRequired { .. }
        ) || std::time::Instant::now() >= deadline
        {
            panic!(
                "unexpected execution state {state:?}; events={:?}",
                store.task_events(task_id)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn prepare_ready(
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

#[tokio::test]
async fn ready_waits_for_explicit_send_then_dispatches_one_exact_execution_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("m03-explicit", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let model = ScriptedModel::fixed(PIPELINE_PLAN);
    let service = compose_with_builtin(&profile, &package, model.clone());
    let revision_hash = prepare_ready(&service, "execute", &workspace).await;
    let store = V1Store::open(&profile.database_path()).unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(matches!(
        store.load_task("execute").await.unwrap().execution,
        TaskExecution::Ready { .. }
    ));
    assert!(!store
        .task_events("execute")
        .iter()
        .any(|event| event.kind == "execution.started"));

    let (first, second) = tokio::join!(
        service.send_message("execute", "implement now"),
        service.send_message("execute", "must remain queued")
    );
    assert!(first.is_ok() || second.is_ok());
    wait_for_execution_state(&store, "execute", "review-ready").await;

    // E06: one explicit send drives the WHOLE two-unit DAG — both STRICT
    // units now run (inspect first, implement after its dependency
    // verified), each with exactly one execution attempt.
    let events = store.task_events("execute");
    let starts = events
        .iter()
        .filter(|event| event.kind == "execution.started")
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 2, "one execution attempt per plan unit");
    assert_eq!(
        starts[0].payload.get("workUnitId").and_then(|v| v.as_str()),
        Some("inspect"),
        "the dependency-ready unit dispatches first"
    );
    assert_eq!(
        starts[1].payload.get("workUnitId").and_then(|v| v.as_str()),
        Some("implement")
    );
    for (index, expected_unit) in [(0usize, "inspect"), (1usize, "implement")] {
        let attempt_id = starts[index]
            .payload
            .get("attemptId")
            .and_then(|value| value.as_str())
            .unwrap();
        assert_eq!(
            attempt_id,
            expected_attempt_id("execute", &revision_hash, expected_unit),
            "durable attempt ids follow the E06 deterministic format"
        );
        let snapshot_id = starts[index]
            .payload
            .get("snapshotId")
            .and_then(|value| value.as_str())
            .unwrap();
        let snapshot = store
            .load_run_snapshot(RunSnapshotId::parse(snapshot_id).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.material().task_id, "execute");
        assert_eq!(
            snapshot.material().work_unit_id.as_deref(),
            Some(expected_unit)
        );
        match snapshot.phase() {
            RunSnapshotPhase::Execution { approval } => {
                assert_eq!(approval.plan_revision.as_str(), revision_hash)
            }
            other => panic!("expected Execution snapshot, got {other:?}"),
        }
    }

    // E06 settle vocabulary: the mid-wave settle journals unit.completed; the
    // task-level aggregate still journals review-ready when it fires.
    let unit_completions = events
        .iter()
        .filter(|event| event.kind == "unit.completed")
        .collect::<Vec<_>>();
    assert_eq!(unit_completions.len(), 1, "inspect settles mid-wave");
    assert_eq!(
        unit_completions[0]
            .payload
            .get("workUnitId")
            .and_then(|value| value.as_str()),
        Some("inspect")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "review-ready")
            .count(),
        1,
        "the final aggregate journals review-ready exactly once"
    );

    let state = store.load_task("execute").await.unwrap();
    assert!(matches!(state.execution, TaskExecution::ReviewReady { .. }));
    // E05: the verification outcome lives on the settled units' per-unit
    // records; the task-level digest is derived from them.
    for unit in ["inspect", "implement"] {
        let settled_record = state
            .unit_record(unit)
            .unwrap_or_else(|| panic!("per-unit record for {unit}"));
        assert!(matches!(
            settled_record.verification,
            ValidationOutcome::Verified { .. }
        ));
    }
    assert_eq!(state.review, ReviewDisposition::Pending);
    let candidate = state.task_candidate_digest().expect("host candidate");
    assert!(store.evidence_for_candidate(&candidate).unwrap().is_empty());
    assert!(store
        .active_leases(&workspace_key(&workspace))
        .unwrap()
        .is_empty());

    // Every completion attributes to exactly one durable attempt row.
    let rows = attempt_rows(&profile);
    assert_eq!(rows.len(), 2, "one row per dispatched unit: {rows:?}");
    for (attempt_id, unit, phase) in &rows {
        assert_eq!(
            attempt_id,
            &expected_attempt_id("execute", &revision_hash, unit)
        );
        assert_eq!(phase, "settled-completed", "row {attempt_id}: {rows:?}");
    }

    let execution_request = model.requests.lock().unwrap().last().cloned().unwrap();
    let names = tool_names(&execution_request.tools);
    assert!(names.contains(&"create_file".to_string()));
    assert!(names.iter().all(|name| !matches!(
        name.as_str(),
        "bash"
            | "shell"
            | "git"
            | "process.open"
            | "process.write"
            | "process.close"
            | "children.spawn"
            | "verification.run"
            | "plan.publish"
            | "plan.update"
    )));

    let before_rejected_send = store.load_task("execute").await.unwrap();
    assert!(service
        .send_message("execute", "cannot bypass review")
        .await
        .is_err());
    assert_eq!(
        store.load_task("execute").await.unwrap(),
        before_rejected_send
    );
    assert_eq!(
        store
            .task_events("execute")
            .iter()
            .filter(|event| event.kind == "execution.started")
            .count(),
        2
    );

    drop(service);
    let restarted = compose_with_builtin(&profile, &package, ScriptedModel::fixed(PIPELINE_PLAN));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        store
            .task_events("execute")
            .iter()
            .filter(|event| event.kind == "execution.started")
            .count(),
        2,
        "restart must not redispatch settled execution"
    );
    assert!(restarted
        .send_message("execute", "still requires review")
        .await
        .is_err());
}

#[tokio::test]
async fn superseded_approval_cannot_start_or_acquire_an_execution_lease() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("m03-stale", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let service = compose_with_builtin(&profile, &package, ScriptedModel::fixed(STRICT_PLAN));
    prepare_ready(&service, "stale", &workspace).await;
    let store = V1Store::open(&profile.database_path()).unwrap();
    let current = store.current_plan_revision("stale").unwrap().unwrap();
    let mut replacement = current.material().clone();
    replacement.revision += 1;
    replacement.parent_revision = Some(current.reference().clone());
    replacement.current_base_hash = "sha256:replacement".into();
    let replacement = PlanRevision::new(replacement).unwrap();
    store
        .publish_plan_revision(&replacement, Some(current.reference()))
        .unwrap();

    assert!(service
        .send_message("stale", "must reject stale approval")
        .await
        .is_err());
    assert!(!store
        .task_events("stale")
        .iter()
        .any(|event| event.kind == "execution.started"));
    assert!(store
        .active_leases(&workspace_key(&workspace))
        .unwrap()
        .is_empty());
    assert!(matches!(
        store.load_task("stale").await.unwrap().execution,
        TaskExecution::Ready { .. }
    ));
}

#[tokio::test]
async fn restart_style_stale_lease_is_not_silently_regranted() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("m03-stale-lease", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let service = compose_with_builtin(&profile, &package, ScriptedModel::fixed(STRICT_PLAN));
    prepare_ready(&service, "stale-lease", &workspace).await;
    let store = V1Store::open(&profile.database_path()).unwrap();
    let key = workspace_key(&workspace);
    let old = store
        .acquire_lease(LeaseRequest {
            workspace_key: key.clone(),
            operation_id: "old-crashed-attempt".into(),
            owner_id: "old-owner".into(),
            read_paths: vec![],
            write_paths: vec!["src".into()],
            repo_exclusive: false,
        })
        .unwrap();

    service
        .send_message("stale-lease", "must wait for old fence")
        .await
        .unwrap();
    wait_for_kind(&service, "stale-lease", "run.failed").await;
    assert!(!store
        .task_events("stale-lease")
        .iter()
        .any(|event| event.kind == "execution.started"));
    assert_eq!(
        store.active_leases(&key).unwrap(),
        std::slice::from_ref(&old)
    );
    assert!(matches!(
        store.load_task("stale-lease").await.unwrap().execution,
        TaskExecution::Ready { .. }
    ));
    assert!(store
        .release_lease(&old.lease_id, "old-owner", old.fencing_epoch)
        .unwrap());
}

#[tokio::test]
async fn two_daemon_managers_competing_on_ready_produce_one_execution_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("m03-cas", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let first = compose_with_builtin(&profile, &package, ScriptedModel::fixed(PIPELINE_PLAN));
    prepare_ready(&first, "cas", &workspace).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = compose_with_builtin(&profile, &package, ScriptedModel::fixed(PIPELINE_PLAN));
    let store = V1Store::open(&profile.database_path()).unwrap();

    let (left, right) = tokio::join!(
        first.send_message("cas", "left contender"),
        second.send_message("cas", "right contender")
    );
    assert!(left.is_ok() || right.is_ok());
    wait_for_execution_state(&store, "cas", "review-ready").await;
    // E06: the race now spans BOTH plan units — one send drives the whole
    // DAG — but the essence holds per unit: the CAS/write-lease race
    // converges on exactly ONE execution attempt identity per unit.
    let starts = store
        .task_events("cas")
        .into_iter()
        .filter(|event| event.kind == "execution.started")
        .map(|event| {
            (
                event
                    .payload
                    .get("workUnitId")
                    .and_then(|value| value.as_str())
                    .unwrap()
                    .to_string(),
                event
                    .payload
                    .get("attemptId")
                    .and_then(|value| value.as_str())
                    .unwrap()
                    .to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        starts.len(),
        2,
        "one execution attempt per unit: {starts:?}"
    );
    let revision_hash = store
        .current_plan_revision("cas")
        .unwrap()
        .expect("approved plan")
        .reference()
        .as_str()
        .to_string();
    for (unit, attempt_id) in &starts {
        assert_eq!(
            attempt_id,
            &expected_attempt_id("cas", &revision_hash, unit),
            "the deterministic attempt identity converges across managers"
        );
    }
    // ... and each converged identity names exactly one durable attempt row.
    let rows = attempt_rows(&profile);
    assert_eq!(rows.len(), 2, "one row per unit: {rows:?}");
    for (attempt_id, unit, phase) in &rows {
        assert_eq!(
            attempt_id,
            &expected_attempt_id("cas", &revision_hash, unit)
        );
        assert_eq!(phase, "settled-completed", "row {attempt_id}: {rows:?}");
        assert!(starts
            .iter()
            .any(|(started, id)| started == unit && id == attempt_id));
    }
    assert!(store
        .active_leases(&workspace_key(&workspace))
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn cancellation_releases_execution_lease_before_terminal_state() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("m03-cancel", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let planning = compose_with_builtin(&profile, &package, ScriptedModel::fixed(STRICT_PLAN));
    prepare_ready(&planning, "cancel", &workspace).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(planning);
    let execution = compose_with_builtin(&profile, &package, std::sync::Arc::new(DelayedModel));
    let store = V1Store::open(&profile.database_path()).unwrap();
    execution
        .send_message("cancel", "start then cancel")
        .await
        .unwrap();
    wait_for_kind(&execution, "cancel", "execution.started").await;
    assert!(execution.cancel_task("cancel").await.unwrap());
    wait_for_kind(&execution, "cancel", "run.cancelled").await;
    assert!(matches!(
        store.load_task("cancel").await.unwrap().execution,
        TaskExecution::Terminal { .. }
    ));
    assert!(store
        .active_leases(&workspace_key(&workspace))
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn harness_start_failure_after_task_cas_repairs_owned_attempt_and_releases_lease() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("m03-start-failure", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let service = compose_with_builtin(&profile, &package, ScriptedModel::fixed(STRICT_PLAN));
    prepare_ready(&service, "start-failure", &workspace).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let store = V1Store::open(&profile.database_path()).unwrap();
    let install_dir: String = rusqlite::Connection::open(profile.database_path())
        .unwrap()
        .query_row(
            "SELECT install_dir FROM plugin_catalog WHERE id = 'native.r-code' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let executable = Path::new(&install_dir).join("bin").join(if cfg!(windows) {
        "r-code-harness-native.exe"
    } else {
        "r-code-harness-native"
    });
    std::fs::write(&executable, b"not an executable").unwrap();

    service
        .send_message("start-failure", "start broken harness")
        .await
        .unwrap();
    wait_for_kind(&service, "start-failure", "run.failed").await;
    let state = store.load_task("start-failure").await.unwrap();
    assert!(matches!(
        state.execution,
        TaskExecution::RepairRequired { .. }
    ));
    assert!(store
        .active_leases(&workspace_key(&workspace))
        .unwrap()
        .is_empty());
}

async fn run_checked_case(
    label: &str,
    mut script: String,
    expected_event: &str,
) -> (tempfile::TempDir, V1Store, String) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    script = script.replace(
        "__WORKSPACE_TRACKED__",
        &workspace
            .join("tracked.txt")
            .to_string_lossy()
            .replace('\\', "/"),
    );
    std::fs::write(workspace.join("verify.js"), script).unwrap();
    let profile = profile(label, temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let service = compose_with_builtin(
        &profile,
        &package,
        ScriptedModel::fixed(writable_plan(&["check:node"])),
    );
    service
        .create_task(
            label,
            label,
            TaskKind::Implementation,
            vec!["check:node".into()],
        )
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
    let store = V1Store::open(&profile.database_path()).unwrap();
    store.save_check_definition(&node_definition()).unwrap();
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
    wait_for_execution_state(&store, label, expected_event).await;
    let state = store.load_task(label).await.unwrap();
    let candidate = state
        .task_candidate_digest()
        .unwrap_or_else(|| panic!("missing candidate in settled state: {state:?}"));
    assert!(store
        .active_leases(&workspace_key(&workspace))
        .unwrap()
        .is_empty());
    (temp, store, candidate)
}

// P20 migrated the two check-driven cases below. Since P20 the production
// required-checks site (`run_manager::verify_candidate`) builds its runner
// with `VerificationRunner::sandboxed`, which resolves the backend from the
// platform safety report for this boot identity. This wave's report is
// honestly Unsupported everywhere (no native sandbox backend is bound until
// P21+), so the gate is closed and the pipeline spawns nothing: required
// checks come back `Unavailable` with zero evidence. Restoring the pre-P20
// expectation would mean re-adding an unsandboxed local spawn, which INV-07
// forbids. The passing/failed proofs therefore run on the injection seam —
// an activated gate with a native backend bound, i.e. exactly the shape
// production takes once a platform activates.

/// The activated-gate route: a native backend is bound, so checks execute.
fn gated_native() -> Arc<dyn CommandExecutionBackend> {
    let native: Arc<dyn CommandExecutionBackend> = Arc::new(LocalShellBackend::new());
    Arc::new(SandboxedCheckBackend::new(
        CheckSandboxGate::Activated,
        Some(native),
    ))
}

/// One real check run on that seam: real candidate capture, real runner with
/// the production platform identity bound, real store holding the frozen
/// definition, real output collection. `__WORKSPACE_TRACKED__` expands to the
/// live candidate file so a case can prove what the check process touched.
struct InjectedCheck {
    _temp: tempfile::TempDir,
    store: V1Store,
    manifest: CandidateManifest,
    outcome: CheckOutcome,
    workspace: std::path::PathBuf,
    database: std::path::PathBuf,
}

async fn injected_check(label: &str, script: &str) -> InjectedCheck {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let tracked = workspace.join("tracked.txt");
    let script = script.replace(
        "__WORKSPACE_TRACKED__",
        &tracked.to_string_lossy().replace('\\', "/"),
    );
    std::fs::write(workspace.join("verify.js"), script).unwrap();
    let binding = TaskWorkspaceBinding::bind_local(label, &workspace, &[]).unwrap();
    let manifest = CandidateManifest::capture(&binding).unwrap();
    let app_profile = profile(label, temp.path());
    let database = app_profile.database_path();
    let store = V1Store::open(&database).unwrap();
    let definition = node_definition();
    store.save_check_definition(&definition).unwrap();
    let boot = BootIdentity::current().unwrap();
    let identity = current_platform_material(boot.as_str()).digest();
    let outcome = VerificationRunner::with_backend_identity(gated_native(), identity)
        .run(
            &binding,
            &manifest,
            &FrozenControlStore::new(temp.path().join("frozen-controls")),
            &definition,
            &temp.path().join(format!("verify-{label}")),
            Duration::from_secs(60),
        )
        .await;
    InjectedCheck {
        _temp: temp,
        store,
        manifest,
        outcome,
        workspace,
        database,
    }
}

/// The kernel state the run manager holds while it runs required checks.
fn verifying_state(task_id: &str, candidate_digest: &str) -> TaskState {
    let snapshot = RunSnapshotId::parse(format!("sha256:{}", "b".repeat(64))).unwrap();
    let attempt = |id: &str| {
        Attempt {
            attempt_id: id.into(),
            task_id: task_id.into(),
            branch_id: "branch-1".into(),
            package: PackageRef {
                id: HarnessId::new("native.r-code"),
                version: semver::Version::new(1, 0, 0),
                content_digest: "sha256:package".into(),
            },
            contract_revision: 1,
            config_hash: "legacy".into(),
            workspace_identity: "workspace-1".into(),
            run_id: format!("run-{id}"),
        }
        .with_run_snapshot(&snapshot)
    };
    let unit = WorkUnit {
        id: "implement".into(),
        description: "implement approved unit".into(),
        dependencies: vec![],
        acceptance: vec!["check:node".into()],
        read_paths: vec![],
        write_paths: vec!["src".into()],
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class: WorkUnitEffectClass::ReadOnly,
        network_ceiling: NetworkCeiling::Offline,
        status: WorkUnitStatus::Pending,
    };
    let mut state = TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind: TaskKind::Implementation,
        objective: task_id.into(),
        constraints: vec![],
        required_checks: vec!["check:node".into()],
        revision: 1,
        memory: None,
    });
    state.start_attempt(&attempt("planning")).unwrap();
    let approval = PlanApprovalRef {
        approval_id: format!("approval-{task_id}"),
        plan_revision: PlanRevisionRef(format!("sha256:{}", "a".repeat(64))),
    };
    state
        .await_plan_approval(Actor::Host, "planning", 1, approval.plan_revision.clone())
        .unwrap();
    state
        .mark_plan_ready(Actor::Host, approval.clone())
        .unwrap();
    state
        .start_execution_attempt(
            Actor::Host,
            &attempt("execution"),
            &approval,
            vec![unit],
            "implement",
        )
        .unwrap();
    state
        .begin_verification(
            Actor::Host,
            "execution",
            "implement",
            candidate_digest.into(),
        )
        .unwrap();
    state
}

fn requirement(evidence: &r_code_kernel::task::EvidenceRecord) -> EvidenceRequirement {
    EvidenceRequirement {
        check_id: evidence.check_id.clone(),
        definition_identity: evidence.definition_identity.clone(),
        environment_fingerprint: evidence.environment_fingerprint.clone(),
    }
}

#[tokio::test]
async fn passing_check_persists_bound_host_evidence_before_review_ready() {
    let case = injected_check("m03-pass", "process.exit(0);").await;
    assert_eq!(
        case.outcome.status,
        CheckStatus::Passed,
        "{:?}",
        case.outcome
    );
    assert_eq!(case.outcome.exit_code, Some(0));
    let evidence = case
        .outcome
        .evidence
        .clone()
        .expect("a passing check mints host evidence");
    assert!(evidence.passed);

    case.store.save_evidence(&evidence).unwrap();
    let records = case
        .store
        .evidence_for_candidate(&case.manifest.candidate_id)
        .unwrap();
    assert_eq!(records.len(), 1);
    let evidence = &records[0];
    assert_eq!(evidence.task_id, "m03-pass");
    assert_eq!(evidence.check_id, "check:node");
    assert_eq!(evidence.definition_identity, node_definition().identity());
    assert_eq!(evidence.candidate_digest, case.manifest.candidate_id);
    assert!(!evidence.environment_fingerprint.is_empty());
    assert!(evidence.passed);
    assert!(matches!(evidence.recorded_by, Provenance::Host));

    // ReviewReady is only reachable after that bound evidence is proven: the
    // requirement is the exact (definition identity, environment fingerprint)
    // pair the runner minted for this candidate and platform identity.
    let requirement = requirement(evidence);
    let mut state = verifying_state("m03-pass", &evidence.candidate_digest);
    assert_eq!(
        state.finish_verification(
            Actor::Host,
            "execution",
            "implement",
            std::slice::from_ref(&requirement)
        ),
        Err(TransitionError::EvidenceRequired),
        "review is unreachable before the check result is recorded"
    );
    state.record_evidence(evidence.clone()).unwrap();
    state
        .finish_verification(Actor::Host, "execution", "implement", &[requirement])
        .unwrap();
    assert!(matches!(state.execution, TaskExecution::ReviewReady { .. }));
    assert_eq!(state.review, ReviewDisposition::Pending);
    // E05: the verification outcome lives on the unit's per-unit record.
    assert_eq!(
        state.unit_record("implement").expect("record").verification,
        ValidationOutcome::Verified {
            candidate_digest: evidence.candidate_digest.clone()
        }
    );
}

#[tokio::test]
async fn failed_check_records_unpassed_evidence_and_requires_repair() {
    let case = injected_check(
        "m03-fail",
        "console.error('assertion failed: widget count'); process.exit(1);",
    )
    .await;
    match &case.outcome.status {
        CheckStatus::Failed { repair_feedback } => {
            assert!(repair_feedback.contains("check:node"), "{repair_feedback}");
            assert!(
                repair_feedback.contains("exited with 1"),
                "{repair_feedback}"
            );
            assert!(
                repair_feedback.contains("assertion failed: widget count"),
                "{repair_feedback}"
            );
        }
        other => panic!("a real exit-1 check must be Failed, got {other:?}"),
    }
    assert_eq!(case.outcome.exit_code, Some(1));
    let evidence = case
        .outcome
        .evidence
        .clone()
        .expect("a failed check still records evidence for the repair loop");
    assert!(!evidence.passed);
    case.store.save_evidence(&evidence).unwrap();

    // Failed is a candidate failure, not an unavailable environment: the run
    // manager marks repair with unavailable=false, which keeps the
    // Failed/Unavailable distinction observable in the task state.
    let mut state = verifying_state("m03-fail", &evidence.candidate_digest);
    state.record_evidence(evidence.clone()).unwrap();
    state
        .require_repair(
            Actor::Host,
            Some("execution".into()),
            Some("implement".into()),
            "required check failed".into(),
            false,
        )
        .unwrap();
    assert!(matches!(
        state.execution,
        TaskExecution::RepairRequired { .. }
    ));
    // E05: the failure outcome and reason live on the failed unit's
    // per-unit record.
    assert_eq!(
        state.unit_record("implement").expect("record").verification,
        ValidationOutcome::Unverified {
            reason: "required check failed".into()
        }
    );
    assert!(state.evidence.iter().any(|record| !record.passed));
    // The failed row is durable (the candidate query only returns passes).
    let passed: i64 = rusqlite::Connection::open(&case.database)
        .unwrap()
        .query_row(
            "SELECT passed FROM evidence WHERE evidence_id = ?1",
            rusqlite::params![evidence.evidence_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(passed, 0, "the failed check must stay recorded");
    assert!(case
        .store
        .evidence_for_candidate(&case.manifest.candidate_id)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn closed_gate_spawns_nothing_and_blocks_more_input() {
    // Production path since P20: run_manager resolves the backend from the
    // activation gate, which is closed on this wave. Nothing spawns, so no
    // evidence exists at all — that is what distinguishes an Unavailable run
    // from the Failed run above, and no amount of loosening may hide it.
    let (_pass_temp, pass_store, pass_candidate) = run_checked_case(
        "m03-gated-pass",
        "process.exit(0);".into(),
        "repair-required",
    )
    .await;
    let gated = pass_store.load_task("m03-gated-pass").await.unwrap();
    assert!(matches!(
        gated.execution,
        TaskExecution::RepairRequired { .. }
    ));
    assert_eq!(
        gated.unit_record("implement").expect("record").verification,
        ValidationOutcome::CheckUnavailable {
            reason: "required check unavailable".into()
        }
    );
    assert!(gated.evidence.is_empty(), "a refused check records nothing");
    assert!(pass_store
        .evidence_for_candidate(&pass_candidate)
        .unwrap()
        .is_empty());

    // A script that used to drift the live candidate mid-check cannot do so
    // any more: the gate refused the spawn before any process started.
    let script =
        "require('fs').writeFileSync('__WORKSPACE_TRACKED__', 'user-drift'); process.exit(0);"
            .to_string();
    let (drift_temp, drift_store, drift_candidate) =
        run_checked_case("m03-gated-drift", script, "repair-required").await;
    assert_eq!(
        std::fs::read(drift_temp.path().join("checkout/tracked.txt")).unwrap(),
        b"TRACKED_SENTINEL\n",
        "the closed gate must not run the check"
    );
    let drifted = drift_store.load_task("m03-gated-drift").await.unwrap();
    assert!(matches!(
        drifted.execution,
        TaskExecution::RepairRequired { .. }
    ));
    assert!(drifted.evidence.is_empty());
    assert!(drift_store
        .evidence_for_candidate(&drift_candidate)
        .unwrap()
        .is_empty());

    // A toolchain the environment cannot provide still lands the same
    // unavailable repair state and blocks further input (pre-P20 arm, kept).
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    std::fs::write(workspace.join("verify.js"), "process.exit(0);").unwrap();
    let app_profile = profile("m03-unavailable", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let service = compose_with_builtin(
        &app_profile,
        &package,
        ScriptedModel::fixed(writable_plan(&["check:node"])),
    );
    service
        .create_task(
            "m03-unavailable",
            "unavailable",
            TaskKind::Implementation,
            vec!["check:node".into()],
        )
        .await
        .unwrap();
    service
        .set_task_preferences(
            "m03-unavailable",
            TaskPreferences {
                workspace_path: Some(workspace.display().to_string()),
                ..TaskPreferences::default()
            },
        )
        .await
        .unwrap();
    let store = V1Store::open(&app_profile.database_path()).unwrap();
    let mut unavailable_definition = node_definition();
    unavailable_definition.entrypoint = CheckEntrypoint::Command {
        program: "definitely-not-a-real-tool-m03".into(),
        argv: vec![],
    };
    store
        .save_check_definition(&unavailable_definition)
        .unwrap();
    service
        .send_message("m03-unavailable", "plan")
        .await
        .unwrap();
    wait_for_kind(&service, "m03-unavailable", "plan.awaiting-approval").await;
    let plan = service.plan("m03-unavailable").await.unwrap();
    service
        .approve_plan(
            "m03-unavailable",
            &plan.revision_hash,
            "approval-unavailable",
            "local-user",
            "session-unavailable",
        )
        .await
        .unwrap();
    service
        .send_message("m03-unavailable", "execute")
        .await
        .unwrap();
    wait_for_execution_state(&store, "m03-unavailable", "repair-required").await;
    let unavailable = store.load_task("m03-unavailable").await.unwrap();
    assert!(matches!(
        unavailable
            .unit_record("implement")
            .expect("record")
            .verification,
        ValidationOutcome::CheckUnavailable { .. }
    ));
    assert!(unavailable.evidence.is_empty());
    let before = unavailable.clone();
    assert!(service
        .send_message("m03-unavailable", "must remain blocked")
        .await
        .is_err());
    assert_eq!(store.load_task("m03-unavailable").await.unwrap(), before);
    assert!(store
        .active_leases(&workspace_key(&workspace))
        .unwrap()
        .is_empty());
}

/// The drift detection the pre-P20 pipeline used to prove by letting the
/// check mutate the live checkout: only reachable where a check actually
/// runs, so it is proven on the injection seam. A check that moves the
/// candidate invalidates itself instead of minting reusable evidence.
#[tokio::test]
async fn drifted_inputs_from_a_running_check_require_repair() {
    let case = injected_check(
        "m03-drift",
        "require('fs').writeFileSync('__WORKSPACE_TRACKED__', 'user-drift'); process.exit(0);",
    )
    .await;
    assert_eq!(case.outcome.status, CheckStatus::InputsChanged);
    assert!(case.outcome.evidence.is_none());
    assert_eq!(
        std::fs::read(case.workspace.join("tracked.txt")).unwrap(),
        b"user-drift"
    );

    // run_manager maps InputsChanged to repair with unavailable=false: the
    // candidate moved, so its evidence can never be reused for review.
    let mut state = verifying_state("m03-drift", &case.manifest.candidate_id);
    state
        .require_repair(
            Actor::Host,
            Some("execution".into()),
            Some("implement".into()),
            "candidate inputs changed".into(),
            false,
        )
        .unwrap();
    assert!(matches!(
        state.execution,
        TaskExecution::RepairRequired { .. }
    ));
    assert!(state.evidence.is_empty());
}
