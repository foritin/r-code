//! S19A — snapshot expansion is gated on exact persisted effect approvals.

use r_code_harness_protocol::services::{
    work_unit_payload_hash, NetworkCeiling, WorkUnitEffectClass, WorkUnitWire,
};
use r_code_harness_protocol::{HarnessId, HostService, PackageRef};
use r_code_kernel::plans::{PlanRevision, PlanRevisionMaterial};
use r_code_kernel::ports::{ModelService, RunGuard, ToolService};
use r_code_kernel::task::{
    PlanApprovalRef, TaskContract, TaskExecution, TaskKind, TaskState, WorkUnit, WorkUnitStatus,
    WorkspaceSnapshotRef,
};
use r_code_kernel::testing::{FakeModelService, FakeToolService};
use r_code_runtime::application::StoreEffectApprovals;
use r_code_runtime::services::run_snapshots::{
    EffectApprovalSource, FrozenRun, RunSnapshotBuilder,
};
use r_code_runtime::services::settings_store::SettingsStore;
use r_code_store::v1::plans::{EffectApprovalRecord, EffectApprovalState};
use r_code_store::v1::V1Store;
use std::sync::Arc;

const CREATED_AT_MS: i64 = 1_700_000_000_000;
const GRANTS: [HostService; 3] = [
    HostService::ModelStream,
    HostService::ToolsList,
    HostService::ToolsCall,
];

fn wire_unit(
    effect_class: WorkUnitEffectClass,
    network_ceiling: NetworkCeiling,
    write_path: &str,
) -> WorkUnitWire {
    WorkUnitWire {
        id: "unit-shell".into(),
        description: "run the pinned build".into(),
        dependencies: vec![],
        acceptance: vec![],
        read_paths: vec![],
        write_paths: vec![write_path.into()],
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class,
        network_ceiling,
    }
}

/// One real store, one real published plan and the unit the gate examines.
struct Scenario {
    root: tempfile::TempDir,
    store: Arc<V1Store>,
    task_id: String,
    plan: PlanRevision,
    unit: WorkUnit,
    payload_hash: String,
}

impl Scenario {
    fn new(
        task_id: &str,
        effect_class: WorkUnitEffectClass,
        network_ceiling: NetworkCeiling,
    ) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            V1Store::open(&root.path().join("harness-v1").join("tasks.sqlite3"))
                .expect("open store"),
        );
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
            work_units: vec![wire_unit(effect_class, network_ceiling, "target")],
        })
        .expect("valid plan");
        store
            .publish_plan_revision(&plan, None)
            .expect("publish plan");
        let wire = plan.material().work_units[0].clone();
        let payload_hash = work_unit_payload_hash(&wire);
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
        Self {
            root,
            store,
            task_id: task_id.into(),
            plan,
            unit,
            payload_hash,
        }
    }

    /// The PRODUCTION source P19B-R wired into the runtime freeze path.
    fn approval_source(&self) -> StoreEffectApprovals {
        StoreEffectApprovals::new(self.store.clone())
    }

    fn record(&self, approval_id: &str) -> EffectApprovalRecord {
        EffectApprovalRecord {
            approval_id: approval_id.into(),
            task_id: self.task_id.clone(),
            plan_revision: self.plan.reference().as_str().into(),
            work_unit_id: self.unit.id.clone(),
            effect_class: self.unit.effect_class.as_str().into(),
            network: self.unit.network_ceiling.as_str().into(),
            actor_id: "actor-user".into(),
            session_id: "session-1".into(),
            scope: "effect.approve".into(),
            payload_hash: self.payload_hash.clone(),
            state: EffectApprovalState::Active,
            created_at_ms: CREATED_AT_MS,
            superseded_at_ms: None,
        }
    }

    async fn freeze(
        &self,
        source: Option<&StoreEffectApprovals>,
        unit: &WorkUnit,
    ) -> Result<FrozenRun, String> {
        let settings = SettingsStore::new(self.root.path());
        let models: Arc<dyn ModelService> = Arc::new(FakeModelService::default());
        let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
        let mut builder = RunSnapshotBuilder::new(&settings, &models, &tools, true);
        if let Some(source) = source {
            builder = builder.with_effect_approvals(source);
        }
        let state = self.ready_state();
        let package = PackageRef {
            id: HarnessId::new("native.r-code"),
            version: "1.0.0".parse().expect("valid semver"),
            content_digest: "sha256:package".into(),
        };
        let workspace = WorkspaceSnapshotRef {
            canonical_root: "D:/workspace".into(),
            workspace_identity: "workspace-1".into(),
            baseline_sha256: "sha256:workspace".into(),
        };
        builder
            .freeze_execution_with_workspace(
                &state,
                &package,
                &GRANTS,
                &RunGuard::new("run-1", 1),
                workspace,
                &self.plan,
                unit,
            )
            .await
    }

    fn ready_state(&self) -> TaskState {
        let mut state = TaskState::new(TaskContract {
            task_id: self.task_id.clone(),
            kind: TaskKind::Implementation,
            objective: "run the pinned build".into(),
            constraints: vec![],
            required_checks: vec![],
            revision: 1,
            memory: None,
        });
        state.execution = TaskExecution::Ready {
            approval: PlanApprovalRef {
                approval_id: "plan-approval-1".into(),
                plan_revision: self.plan.reference().clone(),
            },
        };
        state
    }
}

/// A stored approval that differs from the request in exactly one column.
#[derive(Debug, Clone, Copy)]
enum NearMiss {
    ForeignTask,
    StaleRevision,
    ForeignUnit,
    WeakerClass,
    WeakerCeiling,
    EditedPayload,
}

impl NearMiss {
    const ALL: [Self; 6] = [
        Self::ForeignTask,
        Self::StaleRevision,
        Self::ForeignUnit,
        Self::WeakerClass,
        Self::WeakerCeiling,
        Self::EditedPayload,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::ForeignTask => "a foreign task",
            Self::StaleRevision => "a stale plan revision",
            Self::ForeignUnit => "a foreign work unit",
            Self::WeakerClass => "a weaker effect class",
            Self::WeakerCeiling => "a weaker network ceiling",
            Self::EditedPayload => "an edited payload hash",
        }
    }

    fn record(self, scenario: &Scenario) -> EffectApprovalRecord {
        let mut record = scenario.record("effect-approval-near-miss");
        match self {
            Self::ForeignTask => record.task_id = "task-other".into(),
            Self::StaleRevision => {
                record.plan_revision = format!("sha256:{}", "a".repeat(64));
            }
            Self::ForeignUnit => record.work_unit_id = "unit-other".into(),
            Self::WeakerClass => record.effect_class = "read-only".into(),
            Self::WeakerCeiling => record.network = "offline".into(),
            Self::EditedPayload => {
                record.payload_hash = work_unit_payload_hash(&wire_unit(
                    WorkUnitEffectClass::WorkspaceMutation,
                    NetworkCeiling::PublicInternetClient,
                    "crates",
                ));
            }
        }
        record
    }
}

fn assert_refused(result: Result<FrozenRun, String>, context: &str) {
    match result {
        Ok(frozen) => panic!(
            "{context} expanded without an exact approval: {:?}",
            frozen.snapshot.phase()
        ),
        Err(error) => assert!(
            error.contains("exact active effect approval"),
            "{context}: {error}"
        ),
    }
}

#[tokio::test]
async fn floor_units_freeze_without_any_effect_approval() {
    let scenario = Scenario::new(
        "task-floor",
        WorkUnitEffectClass::ReadOnly,
        NetworkCeiling::Offline,
    );
    let source = scenario.approval_source();
    let frozen = scenario
        .freeze(Some(&source), &scenario.unit)
        .await
        .expect("a read-only offline unit needs no effect approval");
    assert_eq!(
        frozen.snapshot.material().work_unit_id.as_deref(),
        Some("unit-shell")
    );
    scenario
        .freeze(None, &scenario.unit)
        .await
        .expect("the floor freezes even with no source attached");
}

#[tokio::test]
async fn effect_authority_is_refused_when_no_source_is_attached() {
    for (task_id, effect_class, network_ceiling) in [
        (
            "task-no-source-mutation",
            WorkUnitEffectClass::WorkspaceMutation,
            NetworkCeiling::Offline,
        ),
        (
            "task-no-source-network",
            WorkUnitEffectClass::ReadOnly,
            NetworkCeiling::PublicInternetClient,
        ),
        (
            "task-no-source-dependency",
            WorkUnitEffectClass::DependencyPreparation,
            NetworkCeiling::Offline,
        ),
    ] {
        let scenario = Scenario::new(task_id, effect_class, network_ceiling);
        assert_refused(
            scenario.freeze(None, &scenario.unit).await,
            "an escalated unit with no approval source",
        );
    }
}

#[tokio::test]
async fn only_the_exact_approval_expands_a_unit() {
    let scenario = Scenario::new(
        "task-exact",
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::PublicInternetClient,
    );
    let source = scenario.approval_source();
    assert_refused(
        scenario.freeze(Some(&source), &scenario.unit).await,
        "an unapproved unit",
    );

    scenario
        .store
        .save_effect_approval(scenario.record("effect-approval-exact"))
        .expect("save");
    let frozen = scenario
        .freeze(Some(&source), &scenario.unit)
        .await
        .expect("the exact approval expands the unit");
    assert_eq!(
        frozen.snapshot.material().work_unit_id.as_deref(),
        Some("unit-shell")
    );
}

#[tokio::test]
async fn stale_foreign_weaker_and_edited_approvals_never_expand() {
    for near_miss in NearMiss::ALL {
        let scenario = Scenario::new(
            &format!("task-{}", near_miss.name().replace(' ', "-")),
            WorkUnitEffectClass::WorkspaceMutation,
            NetworkCeiling::PublicInternetClient,
        );
        let stored = near_miss.record(&scenario);
        scenario
            .store
            .save_effect_approval(stored.clone())
            .expect("save the near miss");
        let audit = scenario
            .store
            .effect_approvals_for_task(&stored.task_id)
            .expect("audit");
        assert_eq!(
            audit,
            vec![stored],
            "{} was persisted against its own identity",
            near_miss.name()
        );

        let source = scenario.approval_source();
        assert_refused(
            scenario.freeze(Some(&source), &scenario.unit).await,
            near_miss.name(),
        );
    }
}

#[tokio::test]
async fn superseding_an_approval_stops_the_next_freeze() {
    let scenario = Scenario::new(
        "task-revoked",
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::PublicInternetClient,
    );
    scenario
        .store
        .save_effect_approval(scenario.record("effect-approval-revoked"))
        .expect("save");
    let source = scenario.approval_source();
    scenario
        .freeze(Some(&source), &scenario.unit)
        .await
        .expect("approved before revocation");

    assert!(scenario
        .store
        .supersede_effect_approval(&scenario.task_id, "unit-shell", CREATED_AT_MS + 5_000)
        .expect("supersede"));
    assert_refused(
        scenario.freeze(Some(&source), &scenario.unit).await,
        "a superseded approval",
    );
}

#[tokio::test]
async fn a_unit_stronger_than_its_approved_plan_is_refused() {
    let scenario = Scenario::new(
        "task-drift",
        WorkUnitEffectClass::ReadOnly,
        NetworkCeiling::Offline,
    );
    let source = scenario.approval_source();
    scenario
        .store
        .save_effect_approval(scenario.record("effect-approval-drift"))
        .expect("save");

    let mut escalated = scenario.unit.clone();
    escalated.effect_class = WorkUnitEffectClass::WorkspaceMutation;
    let error = match scenario.freeze(Some(&source), &escalated).await {
        Ok(_) => panic!("a class stronger than the approved plan was accepted"),
        Err(error) => error,
    };
    assert!(
        error.contains("does not match the approved WorkUnit"),
        "{error}"
    );

    let mut networked = scenario.unit.clone();
    networked.network_ceiling = NetworkCeiling::PublicInternetClient;
    let error = match scenario.freeze(Some(&source), &networked).await {
        Ok(_) => panic!("a ceiling above the approved plan was accepted"),
        Err(error) => error,
    };
    assert!(
        error.contains("does not match the approved WorkUnit"),
        "{error}"
    );
}

#[test]
fn the_store_backed_source_answers_only_the_exact_identity() {
    let scenario = Scenario::new(
        "task-source",
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::PublicInternetClient,
    );
    let source = scenario.approval_source();
    let exact = |source: &StoreEffectApprovals| {
        source.has_exact_approval(
            &scenario.task_id,
            scenario.plan.reference().as_str(),
            &scenario.unit.id,
            scenario.unit.effect_class.as_str(),
            scenario.unit.network_ceiling.as_str(),
            &scenario.payload_hash,
        )
    };
    assert!(!exact(&source), "nothing is approved yet");

    scenario
        .store
        .save_effect_approval(scenario.record("effect-approval-source"))
        .expect("save");
    assert!(exact(&source));
    assert!(!source.has_exact_approval(
        &scenario.task_id,
        scenario.plan.reference().as_str(),
        &scenario.unit.id,
        "read-only",
        scenario.unit.network_ceiling.as_str(),
        &scenario.payload_hash,
    ));
    assert!(!source.has_exact_approval(
        &scenario.task_id,
        scenario.plan.reference().as_str(),
        "unit-other",
        scenario.unit.effect_class.as_str(),
        scenario.unit.network_ceiling.as_str(),
        &scenario.payload_hash,
    ));
}
