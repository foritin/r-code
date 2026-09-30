//! S28 — expose ONLY the exact-approved sandboxed Shell.
//!
//! Proves the acceptance set: no mutable/global authority (the surface
//! resolves from the frozen WorkUnit effect fields plus ONE six-column
//! exact effect approval; settings are never an input), Offline default
//! with exact network selection (PublicInternetClient only under an
//! exactly matching active approval; HostNetwork and read-only-network
//! refuse), and all local effects durable (the spec hash, ceiling and the
//! honest SafeDisabled refusal land in the operation receipts; a replay
//! returns the recorded outcome and never re-decides).

use r_code_harness_protocol::services::{
    NetworkCeiling, ToolCallRequest, WorkUnitEffectClass, WorkUnitWire,
};
use r_code_harness_protocol::OperationKey;
use r_code_kernel::plans::{PlanRevision, PlanRevisionMaterial};
use r_code_kernel::ports::{JournalStore as _, ToolService as _};
use r_code_kernel::task::WorkUnit;
use r_code_runtime::services::artifacts::ArtifactStore;
use r_code_runtime::services::authorization::{
    resolve_shell_authority, ShellAuthority, SHELL_DENIED_CLASS_MISMATCH,
    SHELL_DENIED_HOST_NETWORK, SHELL_DENIED_NO_EXACT_APPROVAL, SHELL_DENIED_READ_ONLY,
    SHELL_DENIED_READ_ONLY_NETWORK,
};
use r_code_runtime::services::tools::ExecutionToolService;
use r_code_runtime::services::workspaces::TaskWorkspaceBinding;
use r_code_store::v1::plans::{EffectApprovalRecord, EffectApprovalState};
use r_code_store::v1::V1Store;
use std::path::PathBuf;
use std::sync::Arc;

const TASK_ID: &str = "task-s28";
const ATTEMPT_ID: &str = "attempt-s28";

fn plan_and_unit(
    effect_class: WorkUnitEffectClass,
    network: NetworkCeiling,
) -> (PlanRevision, WorkUnit) {
    let plan = PlanRevision::new(PlanRevisionMaterial {
        task_id: TASK_ID.into(),
        revision: 1,
        parent_revision: None,
        current_base_hash: "sha256:base-1".into(),
        workspace_baseline: "sha256:workspace-1".into(),
        route_digest: "sha256:route".into(),
        prompt_digest: "sha256:prompt".into(),
        permission_digest: "sha256:permissions".into(),
        check_digest: "sha256:checks".into(),
        required_checks: vec!["check:test".into()],
        work_units: vec![WorkUnitWire {
            id: "unit-1".into(),
            description: "shell-capable unit".into(),
            dependencies: vec![],
            acceptance: vec!["check:test".into()],
            read_paths: vec![],
            write_paths: vec!["src".into()],
            repo_exclusive: true,
            ephemeral_roots: vec![],
            effect_class,
            network_ceiling: network,
        }],
    })
    .expect("valid plan");
    let wire = plan.material().work_units[0].clone();
    let unit = WorkUnit {
        id: wire.id,
        description: wire.description,
        dependencies: wire.dependencies,
        acceptance: wire.acceptance,
        read_paths: wire.read_paths,
        write_paths: wire.write_paths,
        repo_exclusive: wire.repo_exclusive,
        ephemeral_roots: wire.ephemeral_roots,
        effect_class: wire.effect_class,
        network_ceiling: wire.network_ceiling,
        status: r_code_kernel::task::WorkUnitStatus::Pending,
    };
    (plan, unit)
}

struct Fixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    store: Arc<V1Store>,
    artifacts: Arc<ArtifactStore>,
    plan: PlanRevision,
    unit: WorkUnit,
    wire: WorkUnitWire,
}

impl Fixture {
    fn new(effect_class: WorkUnitEffectClass, network: NetworkCeiling) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("src")).expect("src");
        let store = Arc::new(V1Store::open(&temp.path().join("store.db")).expect("store"));
        let artifacts = Arc::new(ArtifactStore::for_task(
            temp.path().join("artifacts"),
            TASK_ID,
        ));
        let (plan, unit) = plan_and_unit(effect_class, network);
        let wire = plan.material().work_units[0].clone();
        Self {
            _temp: temp,
            workspace,
            store,
            artifacts,
            plan,
            unit,
            wire,
        }
    }

    fn binding(&self) -> TaskWorkspaceBinding {
        TaskWorkspaceBinding::bind_local(TASK_ID, &self.workspace, &[]).expect("binding")
    }

    fn service(&self) -> ExecutionToolService {
        ExecutionToolService::new(
            &self.plan,
            &self.unit,
            self.binding(),
            self.store.clone(),
            self.artifacts.clone(),
            ATTEMPT_ID,
        )
        .expect("execution tools")
    }

    fn save_approval(&self, class: &str, network: &str) {
        self.store
            .save_effect_approval(EffectApprovalRecord {
                approval_id: format!("approval-{class}-{network}"),
                task_id: TASK_ID.into(),
                plan_revision: self.plan.reference().0.clone(),
                work_unit_id: "unit-1".into(),
                effect_class: class.into(),
                network: network.into(),
                actor_id: "actor-1".into(),
                session_id: "session-1".into(),
                scope: "effect.approve".into(),
                payload_hash: r_code_harness_protocol::services::work_unit_payload_hash(&self.wire),
                state: EffectApprovalState::Active,
                created_at_ms: 1,
                superseded_at_ms: None,
            })
            .expect("save approval");
    }

    fn shell_call(command: &str, key: &str) -> ToolCallRequest {
        ToolCallRequest {
            tool: "shell".into(),
            input: serde_json::json!({"command": command, "cwd": "src"}),
            operation_key: Some(OperationKey(key.into())),
        }
    }
}

fn token() -> r_code_kernel::ports::GenerationToken {
    r_code_kernel::ports::GenerationToken::new("run-s28", 1)
}

#[tokio::test]
async fn offline_default_shell_is_exact_durable_and_never_spawns() {
    let fixture = Fixture::new(
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::Offline,
    );
    let service = fixture
        .service()
        .with_shell_authority(ShellAuthority::OfflineExact);

    // Exposed: the exact offline surface needs no approval.
    let tools = service.list(token()).await.expect("list");
    assert!(
        tools.iter().any(|tool| tool.name == "shell"),
        "the offline shell surface is discoverable"
    );

    // The call refuses the spawn (no sandbox backend this wave) but the
    // decision is durable: spec hash + ceiling + refusal in the receipt.
    let reply = service
        .call(token(), Fixture::shell_call("cargo test", "op-1"))
        .await
        .expect("call");
    let error = reply.error.expect("the spawn is honestly refused");
    assert_eq!(error.code, "safe-disabled");
    let payload = serde_json::to_string(&reply.output).expect("output");
    assert!(payload.contains("inputHash"), "{payload}");
    assert!(payload.contains("offline"), "{payload}");

    let receipt = fixture
        .store
        .load_receipt(ATTEMPT_ID, &OperationKey("op-1".into()))
        .await
        .expect("the decision is durable");
    assert_eq!(receipt.method, "shell");
    match receipt.outcome {
        r_code_kernel::task::ReceiptOutcome::Rejected { reason } => {
            assert!(reason.contains("SafeDisabled"), "{reason}");
        }
        other => panic!("expected the durable refusal, got {other:?}"),
    }

    // The replay returns the recorded outcome and never re-decides.
    let replay = service
        .call(token(), Fixture::shell_call("cargo test", "op-1"))
        .await
        .expect("replay");
    let replayed = serde_json::to_string(&replay.output).expect("replay output");
    assert!(replayed.contains("replayed"), "{replayed}");

    // A guessed DIFFERENT spec under the same key is refused.
    let stale = service
        .call(token(), Fixture::shell_call("rm -rf /", "op-1"))
        .await
        .expect("stale call");
    let error = stale.error.expect("stale refused");
    assert!(
        error.message.contains("different spec"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn unexposed_shell_denies_every_call() {
    let fixture = Fixture::new(
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::Offline,
    );
    // The run manager never injected an authority: dormant surface.
    let service = fixture.service();
    let tools = service.list(token()).await.expect("list");
    assert!(
        tools.iter().all(|tool| tool.name != "shell"),
        "an uninjected authority keeps the shell undiscoverable"
    );
    let reply = service
        .call(token(), Fixture::shell_call("cargo test", "op-x"))
        .await
        .expect("call");
    let error = reply.error.expect("denied");
    assert!(error.message.contains("not approved"), "{}", error.message);
}

#[test]
fn authority_resolution_is_exact_and_settings_free() {
    use r_code_harness_protocol::services::{NetworkCeiling, WorkUnitEffectClass};

    // A read-only unit never carries Shell authority (a shell spawns local
    // effects by nature), offline or not.
    assert_eq!(
        resolve_shell_authority(WorkUnitEffectClass::ReadOnly, NetworkCeiling::Offline, None),
        ShellAuthority::Denied {
            reason: SHELL_DENIED_READ_ONLY
        }
    );
    // Offline is the default needing no proof — for effect-capable units.
    assert_eq!(
        resolve_shell_authority(
            WorkUnitEffectClass::WorkspaceMutation,
            NetworkCeiling::Offline,
            None
        ),
        ShellAuthority::OfflineExact
    );
    // HostNetwork is platform-incompatible in v1: refused for everyone.
    assert_eq!(
        resolve_shell_authority(
            WorkUnitEffectClass::WorkspaceMutation,
            NetworkCeiling::HostNetwork,
            Some((
                WorkUnitEffectClass::WorkspaceMutation,
                NetworkCeiling::HostNetwork
            ))
        ),
        ShellAuthority::Denied {
            reason: SHELL_DENIED_HOST_NETWORK
        }
    );
    // A read-only unit cannot ask for network at all.
    assert_eq!(
        resolve_shell_authority(
            WorkUnitEffectClass::ReadOnly,
            NetworkCeiling::PublicInternetClient,
            Some((
                WorkUnitEffectClass::ReadOnly,
                NetworkCeiling::PublicInternetClient
            ))
        ),
        ShellAuthority::Denied {
            reason: SHELL_DENIED_READ_ONLY_NETWORK
        }
    );
    // Networked shell without an approval: refused.
    assert_eq!(
        resolve_shell_authority(
            WorkUnitEffectClass::WorkspaceMutation,
            NetworkCeiling::PublicInternetClient,
            None
        ),
        ShellAuthority::Denied {
            reason: SHELL_DENIED_NO_EXACT_APPROVAL
        }
    );
    // An approval for a DIFFERENT class never matches (the exact store rule
    // already refuses six-column mismatches; this is the defense-in-depth).
    assert_eq!(
        resolve_shell_authority(
            WorkUnitEffectClass::WorkspaceMutation,
            NetworkCeiling::PublicInternetClient,
            Some((
                WorkUnitEffectClass::DependencyPreparation,
                NetworkCeiling::PublicInternetClient
            ))
        ),
        ShellAuthority::Denied {
            reason: SHELL_DENIED_CLASS_MISMATCH
        }
    );
    // The exact match — and only the exact match — approves.
    assert_eq!(
        resolve_shell_authority(
            WorkUnitEffectClass::WorkspaceMutation,
            NetworkCeiling::PublicInternetClient,
            Some((
                WorkUnitEffectClass::WorkspaceMutation,
                NetworkCeiling::PublicInternetClient
            ))
        ),
        ShellAuthority::ApprovedExact {
            ceiling: NetworkCeiling::PublicInternetClient
        }
    );

    // The resolver is a pure function of its inputs: it takes no settings,
    // no environment, no globals — pinned at the type level by the call
    // shapes above (nothing else is even a parameter).
}

#[tokio::test]
async fn run_manager_surface_follows_the_exact_six_column_lookup() {
    let fixture = Fixture::new(
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::PublicInternetClient,
    );

    // Unapproved: the networked ceiling keeps the surface hidden.
    assert_eq!(
        r_code_runtime::run_manager::RunManager::resolve_shell_surface(
            &fixture.store,
            TASK_ID,
            fixture.plan.reference().0.as_str(),
            &fixture.wire,
        ),
        None,
        "PublicInternetClient without an exact active approval stays hidden"
    );

    // The exact approval flips the surface on.
    fixture.save_approval("workspace-mutation", "public-internet-client");
    assert_eq!(
        r_code_runtime::run_manager::RunManager::resolve_shell_surface(
            &fixture.store,
            TASK_ID,
            fixture.plan.reference().0.as_str(),
            &fixture.wire,
        ),
        Some(ShellAuthority::ApprovedExact {
            ceiling: NetworkCeiling::PublicInternetClient
        })
    );

    // A superseded (stale) approval flips it back off.
    fixture
        .store
        .supersede_effect_approval(TASK_ID, "unit-1", 2)
        .expect("supersede");
    assert_eq!(
        r_code_runtime::run_manager::RunManager::resolve_shell_surface(
            &fixture.store,
            TASK_ID,
            fixture.plan.reference().0.as_str(),
            &fixture.wire,
        ),
        None,
        "a superseded approval is stale authority"
    );
}

#[tokio::test]
async fn approved_networked_shell_is_durable_and_still_refuses_to_spawn() {
    let fixture = Fixture::new(
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::PublicInternetClient,
    );
    fixture.save_approval("workspace-mutation", "public-internet-client");
    let authority = r_code_runtime::run_manager::RunManager::resolve_shell_surface(
        &fixture.store,
        TASK_ID,
        fixture.plan.reference().0.as_str(),
        &fixture.wire,
    )
    .expect("the exact approval exposes the surface");
    let service = fixture.service().with_shell_authority(authority);

    let reply = service
        .call(token(), Fixture::shell_call("cargo fetch", "op-net"))
        .await
        .expect("call");
    let error = reply.error.expect("spawn refused");
    assert_eq!(error.code, "safe-disabled");
    let payload = serde_json::to_string(&reply.output).expect("output");
    assert!(
        payload.contains("publicinternetclient"),
        "the exact approved ceiling is what the audit records: {payload}"
    );
    // Durable: the receipt names the networked decision.
    let receipt = fixture
        .store
        .load_receipt(ATTEMPT_ID, &OperationKey("op-net".into()))
        .await
        .expect("durable networked decision");
    assert_eq!(receipt.input_hash.len(), 64);
}
