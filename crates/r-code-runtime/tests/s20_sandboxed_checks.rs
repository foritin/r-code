//! P20 — required checks execute only behind the platform activation gate.
//!
//! Real components throughout: a real `V1Store` in a tempdir, the real boot
//! identity, the real P12/P13 activation-gate material and the real kernel
//! verification transitions. Acceptance ① is the production constructor:
//! `VerificationRunner::sandboxed` resolves to the `sandbox-gated` backend,
//! which refuses every spawn while the platform safety report is not
//! Activated, so a check whose script would write a sentinel never writes it.
//! INV-07 forbids restoring an unsandboxed local fallback to make a check
//! pass; the passing/evidence proofs below therefore run on the injection
//! seam, which is what production uses once a native backend is bound.

use r_code_core::error::ProductError;
use r_code_gateway::execution_backend::{
    CollectedOutput, CommandExecutionBackend, CommandHandle, CommandSpec, LocalShellBackend,
};
use r_code_harness_protocol::services::{PermissionCeiling, WorkUnitWire};
use r_code_harness_protocol::{HarnessId, PackageRef, Provenance};
use r_code_kernel::plans::{
    PlanApprovalActor, PlanRevision, PlanRevisionMaterial, PLAN_APPROVE_SCOPE,
};
use r_code_kernel::ports::JournalStore as _;
use r_code_kernel::task::{
    Actor, Attempt, EvidenceRecord, EvidenceRequirement, NetworkCeiling, PlanApprovalRef,
    PlanRevisionRef, ReviewDisposition, RunSnapshotId, TaskContract, TaskExecution, TaskKind,
    TaskState, TransitionError, ValidationOutcome, WorkUnit, WorkUnitEffectClass, WorkUnitStatus,
};
use r_code_kernel::verification::{CheckDefinition, CheckEntrypoint};
use r_code_runtime::process_guard::BootIdentity;
use r_code_runtime::services::authorization::{
    AuthorizationService, EffectivePermissions, OperationDescriptor, WorkspaceCapability,
};
use r_code_runtime::services::execution::{CheckSandboxGate, SandboxedCheckBackend};
use r_code_runtime::services::execution::{
    CheckSpawnSpec, CheckSpecError, ExecutionError, ExecutionService,
};
use r_code_runtime::services::sandbox::{
    current_platform_material, platform_activation_gate, SafetyActivation,
};
use r_code_runtime::services::verification::{
    CheckOutcome, CheckStatus, VerificationRunner, CHECK_NETWORK_CEILING, SANDBOX_DISABLED_REASON,
};
use r_code_runtime::services::verification_inputs::FrozenControlStore;
use r_code_runtime::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_store::v1::V1Store;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CHECK_ID: &str = "check:s20";
const TASK_ID: &str = "task-s20";
const UNIT_ID: &str = "implement";
const GATED_BACKEND: &str = "sandbox-gated";
const NATIVE_BACKEND: &str = "sandbox-native";
const PASSING_SCRIPT: &str = "process.exit(0);";

/// A native-equivalent backend: it records what it was asked to run, then
/// runs it for real, standing in for the platform sandbox backend that an
/// Activated report would bind. `CommandHandle` only ever wraps a real child,
/// so an injected backend that succeeds must start a process.
#[derive(Default)]
struct FakeNativeBackend {
    inner: LocalShellBackend,
    commands: Mutex<Vec<String>>,
}

impl FakeNativeBackend {
    fn recorded(&self) -> Vec<String> {
        self.commands.lock().expect("commands").clone()
    }
}

#[async_trait::async_trait]
impl CommandExecutionBackend for FakeNativeBackend {
    fn backend_id(&self) -> &'static str {
        "fake-native"
    }

    async fn spawn(
        &self,
        spec: &CommandSpec,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<CommandHandle, ProductError> {
        self.commands
            .lock()
            .expect("commands")
            .push(spec.command.clone());
        self.inner.spawn(spec, abort_flag).await
    }

    async fn collect(
        &self,
        handle: CommandHandle,
        spec: &CommandSpec,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<CollectedOutput, ProductError> {
        self.inner.collect(handle, spec, abort_flag).await
    }
}

/// An activated gate with a native backend bound: the shape production takes
/// once a platform sandbox activates, reachable today only by injection.
fn gated_native() -> (Arc<FakeNativeBackend>, Arc<dyn CommandExecutionBackend>) {
    let fake = Arc::new(FakeNativeBackend::default());
    let native: Arc<dyn CommandExecutionBackend> = fake.clone();
    let backend = SandboxedCheckBackend::new(CheckSandboxGate::Activated, Some(native));
    (fake, Arc::new(backend))
}

fn node_definition() -> CheckDefinition {
    CheckDefinition {
        check_id: CHECK_ID.into(),
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

/// A script that writes an observable side effect outside the workspace and
/// outside the private verification dir, then passes. Its absence proves no
/// check process was ever started.
fn sentinel_script(sentinel: &Path) -> String {
    let display = sentinel.to_string_lossy().replace('\\', "/");
    format!("require('fs').writeFileSync({display:?}, 'spawned'); process.exit(0);")
}

/// Candidate workspace + durable store + frozen check definition.
struct Fixture {
    temp: tempfile::TempDir,
    store: V1Store,
    binding: TaskWorkspaceBinding,
    manifest: CandidateManifest,
    definition: CheckDefinition,
    sentinel: PathBuf,
}

impl Fixture {
    fn new(task_id: &str, script: impl Fn(&Path) -> String) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let sentinel = temp.path().join("spawned-sentinel.txt");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).expect("project dir");
        std::fs::write(root.join("verify.js"), script(&sentinel)).expect("verifier");
        std::fs::write(root.join("tracked.txt"), "candidate\n").expect("candidate file");
        let binding = TaskWorkspaceBinding::bind_local(task_id, &root, &[]).expect("bind");
        let manifest = CandidateManifest::capture(&binding).expect("capture");
        let store = V1Store::open(&temp.path().join("harness-v1.db")).expect("open store");
        let definition = node_definition();
        store
            .save_check_definition(&definition)
            .expect("persist check definition");
        Self {
            temp,
            store,
            binding,
            manifest,
            definition,
            sentinel,
        }
    }

    /// A check that would leave a footprint on the host if it ever ran.
    fn spawn_probe(task_id: &str) -> Self {
        Self::new(task_id, sentinel_script)
    }

    fn passing(task_id: &str) -> Self {
        Self::new(task_id, |_| PASSING_SCRIPT.to_string())
    }

    fn project(&self) -> PathBuf {
        self.temp.path().join("project")
    }

    fn controls(&self) -> FrozenControlStore {
        FrozenControlStore::new(self.temp.path().join("frozen-controls"))
    }

    async fn run(&self, runner: &VerificationRunner, label: &str) -> CheckOutcome {
        runner
            .run(
                &self.binding,
                &self.manifest,
                &self.controls(),
                &self.definition,
                &self.temp.path().join(format!("verify-{label}")),
                Duration::from_secs(60),
            )
            .await
    }

    fn passing_evidence(&self) -> Vec<EvidenceRecord> {
        self.store
            .evidence_for_candidate(&self.manifest.candidate_id)
            .expect("query evidence")
    }
}

fn boot_identity() -> String {
    BootIdentity::current()
        .expect("real boot identity")
        .as_str()
        .to_string()
}

/// This wave's honest verdict: no native sandbox backend exists anywhere, so
/// every platform reports Unsupported and the check gate stays closed.
fn not_activated_verdict(store: &V1Store, boot: &str) -> (String, String) {
    match platform_activation_gate(store, boot) {
        SafetyActivation::NotActivated { reason, status } => {
            (reason.to_string(), status.unwrap_or_else(|| "none".into()))
        }
        SafetyActivation::Activated { report_id } => {
            panic!("P20 requires required checks to stay gated; {report_id} activated")
        }
    }
}

/// The kernel state exactly as `run_manager::verify_candidate` holds it while
/// running required checks: executing attempt, verification begun on this
/// candidate digest.
fn verifying_state(task_id: &str, candidate_digest: &str) -> TaskState {
    let snapshot = RunSnapshotId::parse(format!("sha256:{}", "b".repeat(64))).expect("snapshot");
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
        id: UNIT_ID.into(),
        description: "run the frozen check".into(),
        dependencies: vec![],
        acceptance: vec![CHECK_ID.into()],
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
        objective: "run the frozen check".into(),
        constraints: vec![],
        required_checks: vec![CHECK_ID.into()],
        revision: 1,
    });
    state.start_attempt(&attempt("planning")).expect("planning");
    let approval = PlanApprovalRef {
        approval_id: format!("approval-{task_id}"),
        plan_revision: PlanRevisionRef(format!("sha256:{}", "a".repeat(64))),
    };
    state
        .await_plan_approval(Actor::Host, "planning", 1, approval.plan_revision.clone())
        .expect("await plan approval");
    state
        .mark_plan_ready(Actor::Host, approval.clone())
        .expect("plan ready");
    state
        .start_execution_attempt(
            Actor::Host,
            &attempt("execution"),
            &approval,
            vec![unit],
            UNIT_ID,
        )
        .expect("start execution attempt");
    state
        .begin_verification(Actor::Host, "execution", UNIT_ID, candidate_digest.into())
        .expect("begin verification");
    state
}

fn requirement(evidence: &EvidenceRecord) -> EvidenceRequirement {
    EvidenceRequirement {
        check_id: evidence.check_id.clone(),
        definition_identity: evidence.definition_identity.clone(),
        environment_fingerprint: evidence.environment_fingerprint.clone(),
    }
}

/// Publish and approve a plan whose work unit carries the widest authority a
/// v1 plan may grant: the settings ceiling a task could try to widen with.
async fn escalate_plan(store: &V1Store, task_id: &str) -> WorkUnitWire {
    let seed = TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind: TaskKind::Implementation,
        objective: "escalation fixture".into(),
        constraints: vec![],
        required_checks: vec![CHECK_ID.into()],
        revision: 1,
    });
    store
        .save_task_and_events(&seed, vec![])
        .await
        .expect("seed task");
    let plan = PlanRevision::new(PlanRevisionMaterial {
        task_id: task_id.into(),
        revision: 1,
        parent_revision: None,
        current_base_hash: "sha256:base".into(),
        workspace_baseline: "sha256:workspace".into(),
        route_digest: "sha256:route".into(),
        prompt_digest: "sha256:prompt".into(),
        permission_digest: "sha256:permissions".into(),
        check_digest: "sha256:checks".into(),
        required_checks: vec![CHECK_ID.into()],
        work_units: vec![WorkUnitWire {
            id: UNIT_ID.into(),
            description: "widest authority a v1 plan can grant".into(),
            dependencies: vec![],
            acceptance: vec![CHECK_ID.into()],
            read_paths: vec![],
            write_paths: vec!["src".into()],
            repo_exclusive: false,
            ephemeral_roots: vec![],
            effect_class: WorkUnitEffectClass::WorkspaceMutation,
            network_ceiling: NetworkCeiling::PublicInternetClient,
        }],
    })
    .expect("valid plan");
    store.publish_plan_revision(&plan, None).expect("publish");
    let actor =
        PlanApprovalActor::new("plan-actor", "plan-session", PLAN_APPROVE_SCOPE).expect("actor");
    store
        .approve_plan_revision(task_id, plan.reference(), "approval-s20", actor)
        .expect("approve plan");
    plan.material().work_units[0].clone()
}

fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// `Result::expect_err` needs `Debug`, which `CommandHandle` deliberately
/// does not implement, so a refused spawn is unwrapped by matching.
fn refused(result: Result<CommandHandle, ProductError>) -> ProductError {
    match result {
        Ok(_handle) => panic!("a refusing gate must never return a live handle"),
        Err(error) => error,
    }
}

/// Acceptance ① / test ①: the production resolution is sandbox-gated, its
/// refusal carries the honest activation-gate text, and nothing spawns.
#[tokio::test]
async fn production_checks_resolve_sandbox_gated_and_spawn_nothing() {
    let fixture = Fixture::spawn_probe(TASK_ID);
    let boot = boot_identity();
    let (gate_reason, gate_status) = not_activated_verdict(&fixture.store, &boot);
    assert_eq!(gate_reason, "status-unsupported", "{gate_reason}");
    assert_eq!(gate_status, "unsupported", "{gate_status}");

    let runner = VerificationRunner::sandboxed(&fixture.store, &boot);
    assert_eq!(runner.backend_id(), GATED_BACKEND);
    assert_ne!(runner.backend_id(), "local", "checks may not resolve local");

    let outcome = fixture.run(&runner, "gated").await;
    let reason = match &outcome.status {
        CheckStatus::Unavailable { reason } => reason.clone(),
        other => panic!("expected Unavailable behind a closed gate, got {other:?}"),
    };
    assert!(reason.starts_with("backend failure:"), "{reason}");
    assert!(reason.contains(SANDBOX_DISABLED_REASON), "{reason}");
    assert!(
        reason.contains(&format!("activation-gate: {gate_reason}")),
        "{reason}"
    );
    assert!(
        reason.contains(&format!("report-status: {gate_status}")),
        "{reason}"
    );
    assert!(
        outcome.evidence.is_none(),
        "a refused check mints no evidence"
    );
    assert!(
        !fixture.sentinel.exists(),
        "the gated check started a process and wrote its sentinel"
    );
    assert!(fixture.passing_evidence().is_empty(), "no evidence");

    // Control: the very same fixture and script does leave a footprint once
    // a native backend is bound, so the assertion above is not vacuous.
    let (fake, backend) = gated_native();
    let control = fixture
        .run(&VerificationRunner::with_backend(backend), "control")
        .await;
    assert_eq!(control.status, CheckStatus::Passed, "{control:?}");
    assert!(
        fixture.sentinel.exists(),
        "the probe script never writes a sentinel, so it proves nothing"
    );
    assert_eq!(fake.recorded(), vec!["node verify.js".to_string()]);
}

/// Acceptance ① / INV-07: every non-activated verdict refuses the command
/// verbatim, and an Activated gate with no native backend stays fail-closed.
#[tokio::test]
async fn a_closed_or_unbound_gate_refuses_every_command() {
    let probe = Fixture::spawn_probe(TASK_ID);
    let spec = CommandSpec {
        command: "node verify.js".to_string(),
        cwd: probe.project(),
        timeout: Duration::from_secs(5),
    };
    for refusal in [
        "sandbox-disabled: (activation-gate: status-unsupported; report-status: unsupported)",
        "sandbox-disabled: (activation-gate: status-safe-disabled; report-status: safe-disabled)",
        "sandbox-disabled: (activation-gate: report-missing; report-status: none)",
    ] {
        let backend = SandboxedCheckBackend::closed(refusal);
        assert_eq!(backend.backend_id(), GATED_BACKEND);
        let error = refused(backend.spawn(&spec, None).await);
        assert_eq!(
            error.to_string(),
            refusal,
            "the refusal must survive verbatim, never rewritten into a run"
        );
        assert!(
            !probe.sentinel.exists(),
            "refused command {refusal} still reached a process"
        );
    }

    // Second fail-closed layer: activated verdict, but no native backend.
    let unbound: Arc<dyn CommandExecutionBackend> = Arc::new(SandboxedCheckBackend::new(
        CheckSandboxGate::Activated,
        None,
    ));
    assert_eq!(unbound.backend_id(), NATIVE_BACKEND);
    let error = refused(unbound.spawn(&spec, None).await);
    assert!(
        error.to_string().contains("no native sandbox backend"),
        "{error}"
    );
    assert!(!probe.sentinel.exists());

    let passing = Fixture::passing("task-s20-collect");
    let handle = LocalShellBackend::new()
        .spawn(
            &CommandSpec {
                command: "node verify.js".to_string(),
                cwd: passing.project(),
                timeout: Duration::from_secs(30),
            },
            None,
        )
        .await
        .expect("spawn a handle to hand to the unbound backend");
    let error = unbound
        .collect(handle, &spec, None)
        .await
        .expect_err("an unbound backend holds nothing to collect");
    assert!(error.to_string().contains("no bound backend"), "{error}");
}

/// Test ②: an activated gate runs the frozen entrypoint on the bound native
/// backend and on no other route.
#[tokio::test]
async fn an_activated_gate_runs_only_on_the_bound_native_backend() {
    let fixture = Fixture::passing(TASK_ID);
    let (fake, backend) = gated_native();
    assert_eq!(backend.backend_id(), NATIVE_BACKEND);
    let runner = VerificationRunner::with_backend(backend);
    assert_ne!(runner.backend_id(), "local", "the injected route is local");

    let outcome = fixture.run(&runner, "native").await;
    assert_eq!(outcome.status, CheckStatus::Passed, "{outcome:?}");
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(
        fake.recorded(),
        vec!["node verify.js".to_string()],
        "the bound native backend is the only route a check takes"
    );
    assert!(!fixture.sentinel.exists());
}

/// Acceptance ①/③ in the shape the run manager uses: a genuinely passing
/// check binds host evidence and the kernel advances to ReviewReady.
#[tokio::test]
async fn passing_check_binds_host_evidence_and_reaches_review_ready() {
    let boot = boot_identity();
    let identity = current_platform_material(&boot).digest();
    let fixture = Fixture::passing(TASK_ID);
    let (_, backend) = gated_native();
    let runner = VerificationRunner::with_backend_identity(backend, identity);

    let outcome = fixture.run(&runner, "ready").await;
    assert_eq!(outcome.status, CheckStatus::Passed, "{outcome:?}");
    let evidence = outcome
        .evidence
        .clone()
        .expect("a passing check mints host evidence");
    assert!(evidence.passed);
    assert_eq!(evidence.task_id, TASK_ID);
    assert_eq!(evidence.check_id, CHECK_ID);
    assert_eq!(evidence.definition_identity, fixture.definition.identity());
    assert_eq!(evidence.candidate_digest, fixture.manifest.candidate_id);
    assert!(!evidence.environment_fingerprint.is_empty());
    assert!(matches!(evidence.recorded_by, Provenance::Host));

    fixture
        .store
        .save_evidence(&evidence)
        .expect("persist bound evidence");
    let records = fixture.passing_evidence();
    assert_eq!(records.len(), 1);
    assert_eq!(&records[0], &evidence, "the stored row is the bound record");

    let mut state = verifying_state(TASK_ID, &evidence.candidate_digest);
    assert_eq!(
        state.finish_verification(Actor::Host, "execution", UNIT_ID, &[requirement(&evidence)]),
        Err(TransitionError::EvidenceRequired),
        "review is unreachable before the check result is proven"
    );
    state
        .record_evidence(evidence.clone())
        .expect("record evidence");
    state
        .finish_verification(Actor::Host, "execution", UNIT_ID, &[requirement(&evidence)])
        .expect("proof accepted");
    assert!(matches!(state.execution, TaskExecution::ReviewReady { .. }));
    assert_eq!(state.review, ReviewDisposition::Pending);
    // E05: the verification outcome lives on the unit's per-unit record.
    assert_eq!(
        state
            .unit_record(UNIT_ID)
            .expect("per-unit record")
            .verification,
        ValidationOutcome::Verified {
            candidate_digest: evidence.candidate_digest.clone()
        }
    );
}

/// Acceptance ③: the P12/P13 report identity folded into the environment
/// fingerprint stales prior evidence when that identity changes.
#[tokio::test]
async fn evidence_stales_when_the_platform_identity_changes() {
    let fixture = Fixture::passing(TASK_ID);
    let boot = boot_identity();
    let prior_identity = current_platform_material(&boot).digest();
    let rotated_identity = current_platform_material("boot-identity-rotated-by-reboot").digest();
    assert_ne!(
        prior_identity, rotated_identity,
        "the report identity digest binds the boot identity"
    );

    let (_, before_backend) = gated_native();
    let before = fixture
        .run(
            &VerificationRunner::with_backend_identity(before_backend, prior_identity),
            "before",
        )
        .await
        .evidence
        .expect("evidence minted under the prior identity");
    let (_, after_backend) = gated_native();
    let after = fixture
        .run(
            &VerificationRunner::with_backend_identity(after_backend, rotated_identity),
            "after",
        )
        .await
        .evidence
        .expect("evidence minted under the rotated identity");

    assert_eq!(before.candidate_digest, after.candidate_digest);
    assert_eq!(before.definition_identity, after.definition_identity);
    assert_ne!(
        before.environment_fingerprint, after.environment_fingerprint,
        "the same candidate and check under a new identity is a new environment"
    );
    assert_ne!(before.evidence_id, after.evidence_id);
    assert!(
        !after.is_valid_for_binding(&requirement(&before), TASK_ID, &before.candidate_digest),
        "post-change evidence must not satisfy the prior requirement"
    );
    assert!(
        !before.is_valid_for_binding(&requirement(&after), TASK_ID, &after.candidate_digest),
        "prior evidence must not be reusable after the identity changes"
    );

    let mut stale = verifying_state(TASK_ID, &after.candidate_digest);
    stale.record_evidence(before.clone()).expect("record");
    assert_eq!(
        stale.finish_verification(Actor::Host, "execution", UNIT_ID, &[requirement(&after)]),
        Err(TransitionError::EvidenceRequired),
        "pre-change evidence cannot satisfy the rotated requirement"
    );

    let mut current = verifying_state(TASK_ID, &before.candidate_digest);
    current.record_evidence(before.clone()).expect("record");
    current
        .finish_verification(Actor::Host, "execution", UNIT_ID, &[requirement(&before)])
        .expect("matching identity still proves");
    assert!(matches!(
        current.execution,
        TaskExecution::ReviewReady { .. }
    ));
}

/// Acceptance ②: checks are Offline on every construction path, and no plan
/// or settings authority widens them or reopens the gate.
#[tokio::test]
async fn checks_are_offline_and_no_setting_widens_them() {
    assert_eq!(CHECK_NETWORK_CEILING, NetworkCeiling::Offline);
    assert!(CHECK_NETWORK_CEILING.is_offline());

    let fixture = Fixture::spawn_probe(TASK_ID);
    let boot = boot_identity();
    let granted = escalate_plan(&fixture.store, TASK_ID).await;
    assert_eq!(
        granted.network_ceiling,
        NetworkCeiling::PublicInternetClient
    );
    assert_eq!(granted.effect_class, WorkUnitEffectClass::WorkspaceMutation);

    let (_, injected) = gated_native();
    for runner in [
        VerificationRunner::sandboxed(&fixture.store, &boot),
        VerificationRunner::with_backend(injected),
        VerificationRunner::new(),
    ] {
        assert_eq!(runner.network_ceiling(), CHECK_NETWORK_CEILING);
        assert_ne!(
            runner.network_ceiling(),
            granted.network_ceiling,
            "a work unit ceiling never becomes the check ceiling"
        );
    }

    // Widening the durable state never opens the gate: the escalated plan
    // still yields a refused check, no process and no evidence.
    let production = VerificationRunner::sandboxed(&fixture.store, &boot);
    assert_eq!(production.backend_id(), GATED_BACKEND);
    let outcome = fixture.run(&production, "escalated").await;
    match &outcome.status {
        CheckStatus::Unavailable { reason } => {
            assert!(reason.contains(SANDBOX_DISABLED_REASON), "{reason}")
        }
        other => panic!("an escalated plan must not open the check gate: {other:?}"),
    }
    assert!(
        !fixture.sentinel.exists(),
        "an escalated plan spawned a check"
    );
    assert!(outcome.evidence.is_none());
}

/// Acceptance ① pinned where it is decided: the shipped sources route the
/// required checks only through the gate and withhold network authority.
#[tokio::test]
async fn production_sources_keep_the_checks_path_off_the_local_shell() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let run_manager = std::fs::read_to_string(src.join("run_manager.rs")).expect("run manager");
    assert!(
        run_manager.contains("VerificationRunner::sandboxed(&self.store, boot.as_str())"),
        "the production required-checks site must resolve from the gate"
    );
    assert!(
        !run_manager.contains("VerificationRunner::new("),
        "INV-07: the run manager may not construct the real-execution runner"
    );

    let verification = include_str!("../src/services/verification.rs");
    let seam = verification
        .find("pub fn new() -> Self {")
        .expect("documented test-only real-execution seam");
    let seam_end = verification[seam..]
        .find("\n    /// Runner over an explicitly injected backend")
        .map(|offset| seam + offset)
        .expect("the seam is followed by the injection docs");
    assert_eq!(
        verification[seam..seam_end]
            .matches("LocalShellBackend")
            .count(),
        1,
        "the local shell may only appear inside the test-only real-execution seam"
    );
    assert_eq!(
        verification.matches("LocalShellBackend").count(),
        1,
        "no other route to the local shell exists in the checks service"
    );
    let permissions = verification
        .find("fn check_permissions()")
        .expect("check permission constructor");
    let permissions_end = verification[permissions..]
        .find("\n/// The fail-closed reason")
        .map(|offset| permissions + offset)
        .expect("check_permissions ends before the reason helper");
    let body = &verification[permissions..permissions_end];
    assert!(body.contains("allow_network: false"), "{body}");
    assert!(body.contains("allow_processes: true"), "{body}");
    assert!(
        verification.contains("let permissions = check_permissions();"),
        "run() must authorize checks with the offline permission set"
    );
    assert!(
        !verification.contains("EffectivePermissions::full()"),
        "the checks service may not grant itself full authority"
    );

    let execution = include_str!("../src/services/execution.rs");
    let gate_impl = &execution[execution
        .find("impl CommandExecutionBackend for SandboxedCheckBackend")
        .expect("gated backend impl")..];
    assert!(
        !gate_impl.contains("LocalShellBackend"),
        "INV-07: the required-checks backend has no local route"
    );

    // Dead today, load-bearing if ever wired: dependency preparation builds
    // its own local shell backend, so it must stay unreferenced by the
    // checks path until a native backend gates it the same way.
    let mut sources = Vec::new();
    collect_rust_sources(&src, &mut sources);
    let mut callers = Vec::new();
    for path in &sources {
        let content = std::fs::read_to_string(path).expect("source");
        let hits = content.matches("prepare_dependencies").count();
        if path.ends_with("verification_inputs.rs") {
            assert_eq!(hits, 1, "prepare_dependencies must stay a definition");
        } else if hits > 0 {
            callers.push(
                path.strip_prefix(&src)
                    .expect("relative path")
                    .to_path_buf(),
            );
        }
    }
    assert!(
        callers.is_empty(),
        "unsandboxed prep must stay unwired: {callers:?}"
    );
}

/// The toolchain identity the frozen material below declares.
const SPEC_TOOLCHAIN: &str = "node-test";
/// The baseline frozen argv of the node check.
const ARGV: &[&str] = &["verify.js"];
/// The baseline declared source root, present in every private tree.
const ROOTS: &[&str] = &["src"];
/// Bytes that let one element rewrite the command once it reaches a
/// shell-shaped route, plus the double quote quoting would have to invent.
const SHELL_BYTES: [char; 10] = [';', '&', '|', '\n', '\r', '<', '>', '`', '$', '"'];
/// Declared root forms that would hand a check reads above the private tree:
/// an empty name selects nothing, the rest leave it.
const ESCAPING_ROOTS: [&str; 5] = ["", "/", "/etc/passwd", "..", "src/../../escape"];
/// Declared roots that would make the git directory readable (INV-06), named
/// every way a caller can spell one.
const GIT_VISIBLE_ROOTS: [&str; 4] = [".git", ".git/HEAD", "./.GIT/config", "sub/.Git/hooks"];

/// A real private verify tree: absolute, holding the verifier and a source
/// root, and carrying a git directory so an INV-06 refusal is proven against
/// material that exists rather than a mock path.
fn private_tree() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
    std::fs::write(temp.path().join("src/module.js"), "export const ok = 1;\n").expect("source");
    std::fs::write(temp.path().join("verify.js"), PASSING_SCRIPT).expect("verifier");
    std::fs::create_dir_all(temp.path().join(".git")).expect("git dir");
    std::fs::write(temp.path().join(".git/HEAD"), "ref: refs/heads/main\n").expect("git head");
    temp
}

/// The forward-slash canonical form a spec commits for every path it holds.
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The owned argument list a frozen entrypoint declares.
fn owned(values: &[&str]) -> Vec<String> {
    values.iter().copied().map(String::from).collect()
}

/// Freeze one check's spawn material, reporting the refusal instead of panicking.
fn frozen_spec(
    tree: &Path,
    program: &str,
    arguments: &[&str],
    toolchain: &str,
    roots: &[&str],
) -> Result<CheckSpawnSpec, CheckSpecError> {
    CheckSpawnSpec::build(program, &owned(arguments), tree, toolchain, &owned(roots))
}

/// Spawnable material over a real tree, varying only what a case names.
fn frozen_with(
    tree: &Path,
    program: &str,
    arguments: &[&str],
    toolchain: &str,
    roots: &[&str],
) -> CheckSpawnSpec {
    frozen_spec(tree, program, arguments, toolchain, roots).expect("spawnable material")
}

/// The same frozen check with different entrypoint argv.
fn with_argv(definition: &CheckDefinition, argv: &[&str]) -> CheckDefinition {
    let mut copy = definition.clone();
    copy.entrypoint = CheckEntrypoint::Command {
        program: "node".into(),
        argv: owned(argv),
    };
    copy
}

/// The same frozen check with a different toolchain identity.
fn with_toolchain(definition: &CheckDefinition, toolchain: &str) -> CheckDefinition {
    let mut copy = definition.clone();
    copy.toolchain = toolchain.to_string();
    copy
}

/// The same frozen check with different declared source roots.
fn with_source_roots(definition: &CheckDefinition, roots: &[&str]) -> CheckDefinition {
    let mut copy = definition.clone();
    copy.source_roots = owned(roots);
    copy
}

/// One check run against an explicit definition and private directory. The
/// directory is emptied first because the runner only accepts a private and
/// empty one, while P20.1 needs the same absolute path to repeat.
async fn run_definition_at(
    fixture: &Fixture,
    runner: &VerificationRunner,
    definition: &CheckDefinition,
    dir: &Path,
) -> CheckOutcome {
    if dir.exists() {
        std::fs::remove_dir_all(dir).expect("reset the private verify directory");
    }
    runner
        .run(
            &fixture.binding,
            &fixture.manifest,
            &fixture.controls(),
            definition,
            dir,
            Duration::from_secs(60),
        )
        .await
}

/// A genuinely passing run's bound evidence, so every fingerprint compared
/// below comes out of the real runner and never out of a hand-built record.
async fn evidence_at(
    fixture: &Fixture,
    runner: &VerificationRunner,
    definition: &CheckDefinition,
    dir: &Path,
) -> EvidenceRecord {
    let outcome = run_definition_at(fixture, runner, definition, dir).await;
    assert_eq!(outcome.status, CheckStatus::Passed, "{outcome:?}");
    outcome
        .evidence
        .clone()
        .expect("a passing check mints host evidence")
}

/// P20.1: the executable is a bare toolchain program, never a path. If a
/// rooted or relative program were accepted, a candidate could ship bytes at
/// a location the host would then exec as the check toolchain, and the
/// toolchain identity the evidence binds would be candidate-chosen.
#[tokio::test]
async fn a_check_executable_must_be_a_bare_toolchain_program() {
    let tree = private_tree();
    for program in [
        "",
        "./verify.js",
        "/usr/bin/node",
        "C:/x/node.exe",
        "src/node",
        // Refused on every platform: the rule is "no path separator", not
        // "the host's own path parser says this is a file name", so one frozen
        // definition cannot be a bare program here and a path there.
        "C:\\x\\node.exe",
        "\\\\host\\share\\node.exe",
    ] {
        let error = frozen_spec(tree.path(), program, ARGV, SPEC_TOOLCHAIN, ROOTS)
            .expect_err("a non-bare executable is a refusal, never a default");
        assert_eq!(
            error,
            CheckSpecError::Executable(program.to_string()),
            "executable {program:?} must be refused as itself"
        );
        assert!(
            error
                .to_string()
                .contains("is not a bare toolchain program"),
            "{error}"
        );
    }

    let bare = frozen_with(tree.path(), "node", ARGV, SPEC_TOOLCHAIN, ROOTS);
    assert_eq!(bare.executable, PathBuf::from("node"));
    assert_eq!(
        bare.arguments,
        owned(ARGV),
        "the frozen arguments never carry the executable"
    );
}

/// P20.1: material carrying a shell-significant byte is refused instead of
/// escaped. If those bytes were quoted or passed through, one frozen argument
/// would decide what the shell really runs, and a check could execute host
/// commands while still reporting the candidate's own argv.
#[tokio::test]
async fn a_shell_reinterpretable_byte_refuses_the_material() {
    let tree = private_tree();
    for byte in SHELL_BYTES {
        let expected = CheckSpecError::Reinterpretable(byte.to_string());
        let program = format!("node{byte}");
        let in_program = frozen_spec(tree.path(), &program, ARGV, SPEC_TOOLCHAIN, ROOTS)
            .expect_err("a program carrying a shell byte is refused, never escaped");
        assert_eq!(in_program, expected, "program carrying {byte:?}");

        let argument = format!("--out={byte}file");
        let in_argument = frozen_spec(
            tree.path(),
            "node",
            &["verify.js", &argument],
            SPEC_TOOLCHAIN,
            ROOTS,
        )
        .expect_err("an argument carrying a shell byte is refused");
        assert_eq!(in_argument, expected, "argument carrying {byte:?}");
        assert!(
            in_argument
                .to_string()
                .contains("which a shell could reinterpret"),
            "{in_argument}"
        );

        let declared = format!("src/{byte}");
        let in_root = frozen_spec(tree.path(), "node", ARGV, SPEC_TOOLCHAIN, &[&declared])
            .expect_err("a declared root carrying a shell byte is refused");
        assert_eq!(in_root, expected, "declared root carrying {byte:?}");
    }
}

/// P20.1: escaping roots, a missing toolchain identity and a relative working
/// directory are all refusals. If any became a silent default, the sandbox
/// would enforce a read set or a directory the host never froze, so the
/// evidence would describe an environment nobody actually pinned.
#[tokio::test]
async fn escaping_roots_and_missing_identity_are_refused() {
    let tree = private_tree();
    for root in ESCAPING_ROOTS {
        let error = frozen_spec(tree.path(), "node", ARGV, SPEC_TOOLCHAIN, &[root])
            .expect_err("an escaping read root is a refusal, never a narrowing");
        assert_eq!(
            error,
            CheckSpecError::ReadRootEscape(root.to_string()),
            "declared root {root:?}"
        );
        assert!(
            error.to_string().contains("escapes the materialized tree"),
            "{error}"
        );
    }
    for toolchain in ["", " ", "\t", "  \n "] {
        let error = frozen_spec(tree.path(), "node", ARGV, toolchain, ROOTS)
            .expect_err("no toolchain identity is never a default");
        assert_eq!(
            error,
            CheckSpecError::EmptyToolchain,
            "toolchain {toolchain:?}"
        );
    }

    let relative = Path::new("relative").join("private-verify-dir");
    let error = frozen_spec(&relative, "node", ARGV, SPEC_TOOLCHAIN, ROOTS)
        .expect_err("a relative cwd cannot be confined by a sandbox");
    assert_eq!(
        error,
        CheckSpecError::CwdNotAbsolute(relative.to_string_lossy().to_string())
    );
    assert!(error.to_string().contains("is not absolute"), "{error}");
}

/// P20.1: the readable set is exactly the private tree plus what the check
/// declared, in the canonical absolute form the sandbox enforces. If a
/// declared root silently normalized onto the tree, or an entry went missing,
/// a check would read files the host never granted while its evidence still
/// claimed a confined read set.
#[tokio::test]
async fn read_roots_are_the_private_tree_plus_exactly_the_declared_roots() {
    let tree = private_tree();
    let spec = frozen_with(
        tree.path(),
        "node",
        ARGV,
        SPEC_TOOLCHAIN,
        &["src", "verify.js"],
    );
    assert_eq!(
        spec.read_roots.len(),
        1 + 2,
        "the tree itself plus one entry per declaration"
    );
    assert_eq!(
        spec.read_roots[0],
        slash(tree.path()),
        "the private verify dir is readable, first"
    );
    assert_eq!(spec.read_roots[1], slash(&tree.path().join("src")));
    assert_eq!(spec.read_roots[2], slash(&tree.path().join("verify.js")));
    let root = slash(tree.path());
    assert!(
        spec.read_roots.iter().all(|entry| !entry.contains('\\')),
        "every root is forward-slash canonical: {:?}",
        spec.read_roots
    );
    assert!(
        spec.read_roots
            .iter()
            .all(|entry| entry.starts_with(root.as_str())),
        "no declared root escapes above the private tree: {:?}",
        spec.read_roots
    );
    assert!(
        spec.read_roots
            .iter()
            .all(|entry| Path::new(entry).is_absolute()),
        "a relative read root cannot be enforced: {:?}",
        spec.read_roots
    );

    let undeclared = frozen_with(tree.path(), "node", ARGV, SPEC_TOOLCHAIN, &[]);
    assert_eq!(
        undeclared.read_roots,
        vec![slash(tree.path())],
        "no declaration narrows nothing"
    );
}

/// P20.1 / INV-06: no read root may make the git directory visible, and the
/// refusal is case-insensitive. If it were lifted, a required check could read
/// the repository's objects, refs and hooks, and a check could rewrite or
/// exfiltrate history while still minting host evidence.
#[tokio::test]
async fn no_read_root_ever_makes_the_git_directory_visible() {
    let tree = private_tree();
    for root in GIT_VISIBLE_ROOTS {
        let error = frozen_spec(tree.path(), "node", ARGV, SPEC_TOOLCHAIN, &[root])
            .expect_err("a git-visible read root is refused (INV-06)");
        assert_eq!(
            error,
            CheckSpecError::ReadRootVisibleGit(slash(&tree.path().join(root))),
            "declared root {root:?}"
        );
        assert!(
            error
                .to_string()
                .contains("would make the git directory visible"),
            "{error}"
        );
    }

    // Second layer: a private tree that itself sits below a git directory is
    // refused with no declared root at all, because the tree is always
    // readable. That refusal names the path in the host's own separators while
    // the declared-root layer canonicalizes, so the pin here is path identity.
    let nested = tree.path().join(".git").join("private-verify-dir");
    std::fs::create_dir_all(&nested).expect("nested private dir");
    let error = frozen_spec(&nested, "node", ARGV, SPEC_TOOLCHAIN, &[])
        .expect_err("a tree inside .git grants git visibility by construction");
    match error {
        CheckSpecError::ReadRootVisibleGit(offending) => assert_eq!(
            offending.replace('\\', "/"),
            slash(&nested),
            "the refusal must name the tree it refuses"
        ),
        other => panic!("expected a git-visibility refusal, got {other:?}"),
    }
}

/// P20.1: the spec digest is how evidence names the exact material, so every
/// field must feed it and identical material must digest identically. If a
/// field stopped reaching the digest, a check that ran a different executable,
/// argument, private tree, toolchain or read root would keep reusing prior
/// host evidence and a stale environment would read as still verified.
#[tokio::test]
async fn the_digest_binds_every_field_of_the_frozen_material() {
    let tree = private_tree();
    let elsewhere = private_tree();
    let base = frozen_with(tree.path(), "node", ARGV, SPEC_TOOLCHAIN, ROOTS);
    assert_eq!(
        base.network,
        NetworkCeiling::Offline,
        "offline is the only ceiling a check can express"
    );
    assert!(base.network.is_offline());
    assert_eq!(
        base.admits_spawn(),
        Ok(()),
        "material from build is spawnable"
    );
    assert_eq!(base.digest().len(), 64, "the digest is a sha256 hex");

    let twin = frozen_with(tree.path(), "node", ARGV, SPEC_TOOLCHAIN, ROOTS);
    assert_eq!(
        base.digest(),
        twin.digest(),
        "identical material must digest identically"
    );

    let variants = [
        (
            "the executable",
            frozen_with(tree.path(), "python", ARGV, SPEC_TOOLCHAIN, ROOTS),
        ),
        (
            "an argument",
            frozen_with(
                tree.path(),
                "node",
                &["verify.js", "--strict"],
                SPEC_TOOLCHAIN,
                ROOTS,
            ),
        ),
        (
            "the working directory",
            frozen_with(elsewhere.path(), "node", ARGV, SPEC_TOOLCHAIN, ROOTS),
        ),
        (
            "the toolchain",
            frozen_with(tree.path(), "node", ARGV, "node 22.x", ROOTS),
        ),
        (
            "a substituted read root",
            frozen_with(tree.path(), "node", ARGV, SPEC_TOOLCHAIN, &["verify.js"]),
        ),
        (
            "a widened read root set",
            frozen_with(
                tree.path(),
                "node",
                ARGV,
                SPEC_TOOLCHAIN,
                &["src", "verify.js"],
            ),
        ),
    ];
    for (field, variant) in variants {
        assert_ne!(
            base.digest(),
            variant.digest(),
            "changing only {field} must restate the digest"
        );
    }
}

/// P20.1: Offline is validated at the spawn boundary, not just at
/// construction, and it is digest-bound. If these fields were widen-able after
/// the build, or the check moved behind backend selection, a required check
/// could reach the network while its evidence still claimed an offline run.
#[tokio::test]
async fn a_widened_ceiling_is_refused_before_a_backend_is_chosen() {
    let fixture = Fixture::spawn_probe(TASK_ID);
    let spec = frozen_with(&fixture.project(), "node", ARGV, SPEC_TOOLCHAIN, &[]);
    assert_eq!(spec.admits_spawn(), Ok(()));
    let mut widened = spec.clone();
    widened.network = NetworkCeiling::PublicInternetClient;
    assert_eq!(
        widened.admits_spawn(),
        Err(CheckSpecError::NetworkNotOffline),
        "a widened ceiling never admits a spawn"
    );
    assert_ne!(
        spec.digest(),
        widened.digest(),
        "the ceiling is part of the frozen material"
    );

    let (fake, backend) = gated_native();
    let service = ExecutionService::with_backend(backend, Arc::new(AuthorizationService::new()));
    let descriptor = OperationDescriptor::verification_preparation(
        "node",
        owned(ARGV),
        Some(fixture.project().to_string_lossy().to_string()),
    );
    let workspace = WorkspaceCapability::WriteWithin {
        root: slash(&fixture.project()),
    };
    let permissions = EffectivePermissions {
        ceiling: PermissionCeiling::Full,
        allow_processes: true,
        allow_network: false,
    };
    let error = service
        .run_check(
            &widened,
            &descriptor,
            &workspace,
            &permissions,
            Duration::from_secs(30),
        )
        .await
        .expect_err("an offline-only spec must never spawn");
    assert!(matches!(error, ExecutionError::Spec(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("network ceiling must be offline"),
        "{error}"
    );
    assert!(
        fake.recorded().is_empty(),
        "the refusal precedes backend selection"
    );
    assert!(
        !fixture.sentinel.exists(),
        "a widened ceiling spawned nothing"
    );
}

/// P20.1: `command_line` is the only place frozen material becomes a
/// shell-shaped string, so quoting may only ever cover whitespace. If an
/// argument stopped being passed verbatim, the string the backend carries
/// would no longer be the argv the check was frozen with, and the evidence
/// would describe a command nobody built.
#[tokio::test]
async fn command_line_carries_the_frozen_arguments_verbatim() {
    let tree = private_tree();
    let bare = frozen_with(tree.path(), "node", &[], SPEC_TOOLCHAIN, &[]);
    assert_eq!(
        bare.command_line(),
        "node",
        "no arguments means the program alone"
    );

    let arguments = ["verify.js", "a b", "--flag", "tab\tsep"];
    let spaced = frozen_with(tree.path(), "node", &arguments, SPEC_TOOLCHAIN, &[]);
    assert_eq!(
        spaced.command_line(),
        "node verify.js \"a b\" --flag \"tab\tsep\"",
        "only whitespace is quoted; everything else is verbatim"
    );
    assert!(!spaced.command_line().contains("  "), "single-space joined");
    assert_eq!(spaced.executable, PathBuf::from("node"));
    assert_eq!(
        spaced.arguments,
        owned(&arguments),
        "the executable never enters argv"
    );
    assert!(
        !spaced.arguments.iter().any(|argument| argument == "node"),
        "{:?}",
        spaced.arguments
    );
}

/// P20.1: the fingerprint minted into host evidence binds the exact spawn
/// material end to end. If a changed argv, toolchain or declared read root
/// yielded the same fingerprint, a check that verified a different environment
/// would keep satisfying the kernel's evidence requirement, and ReviewReady
/// would be reachable on proof of a run that never happened.
#[tokio::test]
async fn the_evidence_fingerprint_binds_the_frozen_spawn_material() {
    let fixture = Fixture::passing(TASK_ID);
    let (_, backend) = gated_native();
    let runner = VerificationRunner::with_backend(backend);
    let dir = fixture.temp.path().join("verify-material");
    let base = node_definition();

    let first = evidence_at(&fixture, &runner, &base, &dir).await;
    let repeat = evidence_at(&fixture, &runner, &base, &dir).await;
    assert_eq!(
        first.environment_fingerprint, repeat.environment_fingerprint,
        "one candidate, one check and one private tree digest identically"
    );
    assert_eq!(first.definition_identity, repeat.definition_identity);

    let restated = with_argv(&base, &["verify.js", "--strict"]);
    let argv = evidence_at(&fixture, &runner, &restated, &dir).await;
    assert_ne!(
        first.environment_fingerprint, argv.environment_fingerprint,
        "argv reaches the fingerprint only through the frozen spawn spec"
    );
    let stale = EvidenceRequirement {
        check_id: argv.check_id.clone(),
        definition_identity: argv.definition_identity.clone(),
        environment_fingerprint: first.environment_fingerprint.clone(),
    };
    assert!(
        !argv.is_valid_for_binding(&stale, TASK_ID, &first.candidate_digest),
        "changed spawn material cannot reuse the pre-change environment"
    );
    let current = EvidenceRequirement {
        environment_fingerprint: argv.environment_fingerprint.clone(),
        ..stale
    };
    assert!(
        argv.is_valid_for_binding(&current, TASK_ID, &argv.candidate_digest),
        "the same requirement restated at the new fingerprint is satisfiable"
    );

    let toolchain_check = with_toolchain(&base, "node 22.x");
    let toolchain = evidence_at(&fixture, &runner, &toolchain_check, &dir).await;
    assert_ne!(
        first.environment_fingerprint, toolchain.environment_fingerprint,
        "a different toolchain identity is a different environment"
    );

    let roots_check = with_source_roots(&base, &["tracked.txt"]);
    let roots = evidence_at(&fixture, &runner, &roots_check, &dir).await;
    assert_ne!(
        first.environment_fingerprint, roots.environment_fingerprint,
        "a declared read root reaches the fingerprint through the spawn spec"
    );
    assert_ne!(
        argv.environment_fingerprint, roots.environment_fingerprint,
        "each material change restates the environment on its own"
    );
}

/// P20.1: entrypoint material a shell could reinterpret becomes an unavailable
/// check with zero spawn. If the frozen build stopped refusing it, one
/// argument carrying a `;` would run whatever followed it on the host as a
/// required check, and the minted evidence would credit the candidate for it.
#[tokio::test]
async fn a_reinterpretable_entrypoint_is_refused_with_zero_spawn() {
    let fixture = Fixture::spawn_probe(TASK_ID);
    let (fake, backend) = gated_native();
    let runner = VerificationRunner::with_backend(backend);

    let dirty = with_argv(&node_definition(), &["verify.js;echo p20-reinterpreted"]);
    let argv_dir = fixture.temp.path().join("verify-argv");
    let outcome = run_definition_at(&fixture, &runner, &dirty, &argv_dir).await;
    let reason = match &outcome.status {
        CheckStatus::Unavailable { reason } => reason.clone(),
        other => panic!("a reinterpretable argv must never run, got {other:?}"),
    };
    assert!(
        reason.contains("check material is not spawnable"),
        "{reason}"
    );
    assert!(
        reason.contains("which a shell could reinterpret"),
        "{reason}"
    );
    assert!(
        outcome.evidence.is_none(),
        "a refused check mints no evidence"
    );
    assert!(
        fake.recorded().is_empty(),
        "the refusal happens before a backend sees the command"
    );
    assert!(!fixture.sentinel.exists(), "no check process ran");

    for (index, byte) in [';', '|', '&', '$'].into_iter().enumerate() {
        let swept = with_argv(&node_definition(), &["verify.js", &format!("--echo{byte}")]);
        let dir = fixture.temp.path().join(format!("verify-sweep-{index}"));
        let outcome = run_definition_at(&fixture, &runner, &swept, &dir).await;
        match &outcome.status {
            CheckStatus::Unavailable { reason } => {
                assert!(
                    reason.contains("check material is not spawnable"),
                    "byte {byte:?}: {reason}"
                )
            }
            other => panic!("argv carrying {byte:?} ran as {other:?}"),
        }
    }
    assert!(
        fake.recorded().is_empty(),
        "no swept case reached a backend"
    );
    assert!(
        !fixture.sentinel.exists(),
        "no swept case spawned a process"
    );

    // Control: the same fixture and probe script does spawn once the material
    // is clean, so every refusal above is a refusal and not an inert fixture.
    let clean_dir = fixture.temp.path().join("verify-clean");
    let control = run_definition_at(&fixture, &runner, &node_definition(), &clean_dir).await;
    assert_eq!(control.status, CheckStatus::Passed, "{control:?}");
    assert!(
        fixture.sentinel.exists(),
        "the probe script never writes a sentinel, so it proves nothing"
    );
    assert_eq!(fake.recorded(), vec!["node verify.js".to_string()]);
}

/// P20.1: an activated gate re-admits material before delegating, so a caller
/// that bypassed the spec still cannot hand the native backend a shell-shaped
/// command or a relative directory. Without this layer the Activated verdict
/// would buy nothing: the bound backend would run any string a caller built.
#[tokio::test]
async fn an_activated_gate_re_admits_material_before_delegating() {
    let probe = Fixture::spawn_probe(TASK_ID);
    let (fake, backend) = gated_native();
    for (index, byte) in [';', '&', '|', '<', '>', '$', '`'].into_iter().enumerate() {
        let dirty = CommandSpec {
            command: format!("node verify.js{byte}echo p20-{index}"),
            cwd: probe.project(),
            timeout: Duration::from_secs(5),
        };
        let error = refused(backend.spawn(&dirty, None).await);
        assert!(
            error
                .to_string()
                .contains("which a sandbox cannot execute verbatim"),
            "byte {byte:?}: {error}"
        );
        assert!(
            fake.recorded().is_empty(),
            "byte {byte:?} reached the native backend before refusal"
        );
        assert!(!probe.sentinel.exists(), "byte {byte:?} started a process");
    }

    let relative = CommandSpec {
        command: "node verify.js".to_string(),
        cwd: PathBuf::from("relative-private-verify-dir"),
        timeout: Duration::from_secs(5),
    };
    let error = refused(backend.spawn(&relative, None).await);
    assert!(error.to_string().contains("is not absolute"), "{error}");
    assert!(
        fake.recorded().is_empty(),
        "a refused working directory reached the native backend"
    );
    assert!(!probe.sentinel.exists(), "a relative cwd started a process");

    let clean = CommandSpec {
        command: "node verify.js".to_string(),
        cwd: probe.project(),
        timeout: Duration::from_secs(30),
    };
    let handle = backend
        .spawn(&clean, None)
        .await
        .expect("clean material reaches the bound native backend");
    let output = backend
        .collect(handle, &clean, None)
        .await
        .expect("collect the spawned check");
    assert_eq!(output.exit_code, Some(0), "{output:?}");
    assert_eq!(fake.recorded(), vec!["node verify.js".to_string()]);
    assert!(
        probe.sentinel.exists(),
        "the clean command never actually ran"
    );
}

/// The gate guards collection as well as spawning. A Closed gate that also
/// carries a bound native backend must refuse to drain a handle, so binding a
/// native backend can never on its own turn a refusal into a delivered check
/// result. Were this arm to match only on `native`, a forged handle could slip
/// through a gate that had already said no.
#[tokio::test]
async fn a_closed_gate_refuses_to_collect_even_with_a_native_backend_bound() {
    let refusal = format!("{SANDBOX_DISABLED_REASON}: activation-gate: status-unsupported");
    let (fake, native) = gated_native();
    let backend = SandboxedCheckBackend::new(
        CheckSandboxGate::Closed {
            reason: refusal.clone(),
        },
        Some(native),
    );
    assert_eq!(backend.backend_id(), GATED_BACKEND);
    let probe = Fixture::passing("task-s20-collect-gate");
    let spec = CommandSpec {
        command: "node verify.js".to_string(),
        cwd: probe.project(),
        timeout: Duration::from_secs(30),
    };
    let handle = LocalShellBackend::new()
        .spawn(&spec, None)
        .await
        .expect("a real handle, the way a bound native backend would produce one");
    let error = match backend.collect(handle, &spec, None).await {
        Ok(output) => panic!("a closed gate delivered check output: {output:?}"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), refusal, "the refusal must stay verbatim");
    assert!(
        fake.recorded().is_empty(),
        "a closed gate delegated collection to the native backend"
    );
}
