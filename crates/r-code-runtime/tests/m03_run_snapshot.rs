use r_code_harness_protocol::services::{NetworkCeiling, WorkUnitEffectClass, WorkUnitWire};
use r_code_harness_protocol::{HarnessId, HostService, PackageRef};
use r_code_kernel::plans::{PlanRevision, PlanRevisionMaterial};
use r_code_kernel::ports::{ModelService, RunGuard, ToolService};
use r_code_kernel::task::{
    PlanApprovalRef, PromptSnapshotMode, RunSnapshotPhase, TaskContract, TaskExecution, TaskKind,
    TaskState, WorkUnit, WorkUnitStatus, WorkspaceSnapshotRef,
};
use r_code_kernel::testing::{FakeModelService, FakeToolService};
use r_code_runtime::services::run_snapshots::RunSnapshotBuilder;
use r_code_runtime::services::settings_store::SettingsStore;
use std::sync::Arc;

fn package() -> PackageRef {
    PackageRef {
        id: HarnessId::new("native.r-code"),
        version: "1.0.0".parse().unwrap(),
        content_digest: "sha256:package".into(),
    }
}

fn plan(task_id: &str) -> (PlanRevision, WorkUnit, PlanApprovalRef) {
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
        required_checks: vec![],
        work_units: vec![WorkUnitWire {
            id: "unit-1".into(),
            description: "implement".into(),
            dependencies: vec![],
            acceptance: vec![],
            read_paths: vec![],
            write_paths: vec!["src".into()],
            repo_exclusive: false,
            ephemeral_roots: vec![],
            effect_class: WorkUnitEffectClass::ReadOnly,
            network_ceiling: NetworkCeiling::Offline,
        }],
    })
    .unwrap();
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
        status: WorkUnitStatus::Pending,
    };
    let approval = PlanApprovalRef {
        approval_id: "approval-1".into(),
        plan_revision: plan.reference().clone(),
    };
    (plan, unit, approval)
}

fn ready_state(task_id: &str, kind: TaskKind, approval: PlanApprovalRef) -> TaskState {
    let mut state = TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind,
        objective: "implement".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
        memory: None,
    });
    state.execution = TaskExecution::Ready { approval };
    state
}

fn workspace() -> WorkspaceSnapshotRef {
    WorkspaceSnapshotRef {
        canonical_root: "D:/workspace".into(),
        workspace_identity: "workspace-1".into(),
        baseline_sha256: "sha256:workspace".into(),
    }
}

#[tokio::test]
async fn builder_binds_exact_work_unit_and_selects_execution_or_repair_phase() {
    let temp = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(temp.path());
    let models: Arc<dyn ModelService> = Arc::new(FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let builder = RunSnapshotBuilder::new(&settings, &models, &tools, true);
    let grants = [
        HostService::ModelStream,
        HostService::ToolsList,
        HostService::ToolsCall,
    ];
    let guard = RunGuard::new("run-1", 1);

    for (kind, repair) in [(TaskKind::Implementation, false), (TaskKind::Repair, true)] {
        let task_id = if repair { "repair" } else { "execution" };
        let (plan, unit, approval) = plan(task_id);
        let state = ready_state(task_id, kind, approval.clone());
        let frozen = builder
            .freeze_execution_with_workspace(
                &state,
                &package(),
                &grants,
                &guard,
                workspace(),
                &plan,
                &unit,
            )
            .await
            .unwrap();
        assert_eq!(
            frozen.snapshot.material().work_unit_id.as_deref(),
            Some("unit-1")
        );
        match frozen.snapshot.phase() {
            RunSnapshotPhase::Execution { approval: frozen } if !repair => {
                assert_eq!(frozen, &approval)
            }
            RunSnapshotPhase::Repair { approval: frozen } if repair => {
                assert_eq!(frozen, &approval)
            }
            other => panic!("wrong frozen phase: {other:?}"),
        }

        let mut wrong_unit = unit.clone();
        wrong_unit.id = "missing-unit".into();
        assert!(builder
            .freeze_execution_with_workspace(
                &state,
                &package(),
                &grants,
                &guard,
                workspace(),
                &plan,
                &wrong_unit,
            )
            .await
            .is_err());
    }
}

#[tokio::test]
async fn planning_snapshot_remains_planless() {
    let temp = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(temp.path());
    let models: Arc<dyn ModelService> = Arc::new(FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let builder = RunSnapshotBuilder::new(&settings, &models, &tools, true);
    let state = TaskState::new(TaskContract {
        task_id: "planning".into(),
        kind: TaskKind::Implementation,
        objective: "plan".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
        memory: None,
    });
    let frozen = builder
        .freeze_with_workspace(
            &state,
            &package(),
            &[HostService::ModelStream],
            &RunGuard::new("run-plan", 1),
            workspace(),
        )
        .await
        .unwrap();
    assert!(matches!(
        frozen.snapshot.phase(),
        RunSnapshotPhase::Planning
    ));
    assert!(frozen.snapshot.material().work_unit_id.is_none());
    assert_eq!(
        frozen.snapshot.material().prompt.mode,
        PromptSnapshotMode::Default
    );
}
