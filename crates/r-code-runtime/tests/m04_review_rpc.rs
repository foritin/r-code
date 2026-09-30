mod p_gate_support;

use p_gate_support::daemon::Daemon;
use p_gate_support::{profile, write_workspace};
use r_code_kernel::ports::JournalStore;
use r_code_kernel::task::{
    NetworkCeiling, ReviewDisposition, TaskContract, TaskExecution, TaskKind, TaskPreferences,
    TaskState, ValidationOutcome, WorkUnit, WorkUnitEffectClass, WorkUnitStatus,
};
use r_code_runtime::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_store::v1::V1Store;

#[tokio::test]
async fn review_override_rpc_uses_authenticated_client_not_caller_actor_field() {
    let temp = tempfile::tempdir().unwrap();
    let profile = profile("m04-rpc-actor", temp.path());
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let binding = TaskWorkspaceBinding::bind_local("override-rpc", &workspace, &[]).unwrap();
    let candidate = CandidateManifest::capture(&binding).unwrap().candidate_id;
    let store = V1Store::open(&profile.database_path()).unwrap();
    let mut state = TaskState::new(TaskContract {
        task_id: "override-rpc".into(),
        kind: TaskKind::Implementation,
        objective: "override".into(),
        constraints: vec![],
        required_checks: vec!["check:required".into()],
        revision: 1,
    });
    state.work_units = vec![WorkUnit {
        id: "unit-1".into(),
        description: "unit".into(),
        dependencies: vec![],
        acceptance: vec!["check:required".into()],
        read_paths: vec![],
        write_paths: vec!["src".into()],
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class: WorkUnitEffectClass::ReadOnly,
        network_ceiling: NetworkCeiling::Offline,
        status: WorkUnitStatus::Blocked,
    }];
    // E05: the candidate digest and failed verification outcome live on
    // the unit's per-unit record; the override anchors on it.
    state.unit_records.insert(
        "unit-1".into(),
        r_code_kernel::task::UnitRecord {
            attempt_id: Some("attempt-1".into()),
            candidate_digest: Some(candidate.clone()),
            verification: ValidationOutcome::CheckUnavailable {
                reason: "missing environment".into(),
            },
            settlement: r_code_kernel::task::UnitSettlement::Failed,
        },
    );
    state.review = ReviewDisposition::Rejected;
    state.execution = TaskExecution::RepairRequired {
        attempt_id: Some("attempt-1".into()),
        work_unit_id: Some("unit-1".into()),
        reason: "missing environment".into(),
    };
    state.preferences = TaskPreferences {
        workspace_path: Some(workspace.display().to_string()),
        ..TaskPreferences::default()
    };
    store.save_task_and_events(&state, vec![]).await.unwrap();
    let revision = store
        .load_task_with_revision("override-rpc")
        .unwrap()
        .unwrap()
        .1;

    let daemon = Daemon::start(&profile, None);
    let mut client = daemon.connect(&profile, "authenticated-client").await;
    let result = client
        .call_with_id(
            "review.override",
            serde_json::json!({
                "taskId": "override-rpc",
                "actionId": "override-rpc-action",
                "expectedTaskRevision": revision,
                "candidateDigest": candidate,
                "actorId": "spoofed-caller-field",
                "sessionId": "rpc-session",
                "reason": "explicitly accept missing environment",
                "checks": ["check:required"]
            }),
            "override-command",
        )
        .await
        .unwrap();
    assert_eq!(result["outcome"], "unverified-accepted");
    let event = store
        .task_events("override-rpc")
        .into_iter()
        .find(|event| event.kind == "review.unverified-accepted")
        .unwrap();
    assert_eq!(event.payload["actorId"], "authenticated-client");
    assert_ne!(event.payload["actorId"], "spoofed-caller-field");
}
