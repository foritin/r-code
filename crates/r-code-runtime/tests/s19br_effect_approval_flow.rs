//! S19B-R — authenticated effect approval runtime flow.
//!
//! Host-owned pending requests bind to the exact approved-plan payload, a
//! granted decision materializes EXACTLY ONE immutable approval carrying the
//! authenticated actor/session/scope, deny and expiry materialize nothing,
//! replays converge on the first persisted row, stale requests refuse without
//! consuming themselves, revocation is future-runs-only (a frozen RunSnapshot
//! is never rewritten), and the RPC surface exposes the one canonical payload
//! P19B-C clients render. Real components only: real `V1Store` in a tempdir,
//! the real `ApplicationService`/`ApprovalStore`, the real daemon binary for
//! the wire surface, and the real `RunSnapshotBuilder` freeze path through
//! the production `StoreEffectApprovals` source.

mod p_gate_support;

use p_gate_support::daemon::Daemon;
use r_code_harness_protocol::rpc::{error_code, is_known_method, RpcId, RpcRequest};
use r_code_harness_protocol::services::{
    work_unit_payload_hash, NetworkCeiling, WorkUnitEffectClass, WorkUnitWire,
};
use r_code_harness_protocol::{ApprovalDecision, HarnessId, HostService, PackageRef, RunIdentity};
use r_code_kernel::plans::{
    PlanApprovalActor, PlanRevision, PlanRevisionMaterial, PLAN_APPROVE_SCOPE,
};
use r_code_kernel::ports::{JournalEvent, JournalStore as _, ModelService, RunGuard, ToolService};
use r_code_kernel::task::{
    PlanApprovalRef, TaskContract, TaskExecution, TaskKind, TaskState, WorkUnit, WorkUnitStatus,
    WorkspaceSnapshotRef,
};
use r_code_kernel::testing::{FakeModelService, FakeProcessService, FakeToolService};
use r_code_runtime::application::{
    ApplicationError, ApplicationService, CommandSource, StoreEffectApprovals, EFFECT_APPROVE_SCOPE,
};
use r_code_runtime::plugins::approval_store::EffectBinding;
use r_code_runtime::plugins::{service_for_method, ApprovalStore, HostRouter, IgnoreQuestions};
use r_code_runtime::remote::capabilities::{required_capability, Capability};
use r_code_runtime::services::run_snapshots::{FrozenRun, RunSnapshotBuilder};
use r_code_runtime::services::settings_store::SettingsStore;
use r_code_runtime::RuntimeProfile;
use r_code_store::v1::plans::{EffectApprovalRecord, EffectApprovalState};
use r_code_store::v1::V1Store;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const UNIT: &str = "unit-shell";
const SECOND_UNIT: &str = "unit-second";
const GRANTS: [HostService; 3] = [
    HostService::ModelStream,
    HostService::ToolsList,
    HostService::ToolsCall,
];
/// The canonical `EffectApprovalView` wire keys (camelCase, no credentials).
const VIEW_KEYS: [&str; 12] = [
    "actorId",
    "approvalId",
    "createdAtMs",
    "effectClass",
    "network",
    "payloadHash",
    "planRevision",
    "scope",
    "sessionId",
    "state",
    "taskId",
    "workUnitId",
];

/// The effect flow never streams a model; a call here is a test bug.
struct UnusedModel;

#[async_trait::async_trait]
impl ModelService for UnusedModel {
    async fn stream(
        &self,
        _token: r_code_kernel::ports::GenerationToken,
        _request: r_code_harness_protocol::ModelStreamRequest,
        _sink: &mut dyn r_code_kernel::ports::StreamSink,
    ) -> Result<r_code_kernel::ports::ModelStreamOutcome, r_code_kernel::ports::ServiceError> {
        unreachable!("the effect-approval flow never streams the model")
    }
}

fn escalated_wire(id: &str, write_path: &str) -> WorkUnitWire {
    WorkUnitWire {
        id: id.into(),
        description: "run the pinned build".into(),
        dependencies: vec![],
        acceptance: vec![],
        read_paths: vec![],
        write_paths: vec![write_path.into()],
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class: WorkUnitEffectClass::WorkspaceMutation,
        network_ceiling: NetworkCeiling::PublicInternetClient,
    }
}

fn kernel_unit(wire: &WorkUnitWire) -> WorkUnit {
    WorkUnit {
        id: wire.id.clone(),
        description: wire.description.clone(),
        dependencies: wire.dependencies.clone(),
        acceptance: wire.acceptance.clone(),
        read_paths: wire.read_paths.clone(),
        write_paths: wire.write_paths.clone(),
        repo_exclusive: wire.repo_exclusive,
        ephemeral_roots: wire.ephemeral_roots.clone(),
        effect_class: wire.effect_class,
        network_ceiling: wire.network_ceiling,
        status: WorkUnitStatus::Pending,
    }
}

fn revision(
    task_id: &str,
    number: u64,
    parent: Option<r_code_kernel::task::PlanRevisionRef>,
    units: Vec<WorkUnitWire>,
) -> PlanRevision {
    PlanRevision::new(PlanRevisionMaterial {
        task_id: task_id.into(),
        revision: number,
        parent_revision: parent,
        current_base_hash: "sha256:base".into(),
        workspace_baseline: "sha256:workspace".into(),
        route_digest: "sha256:route".into(),
        prompt_digest: "sha256:prompt".into(),
        permission_digest: "sha256:permissions".into(),
        check_digest: "sha256:checks".into(),
        required_checks: vec![],
        work_units: units,
    })
    .expect("valid plan")
}

/// Publish revision 1 and approve it, so the head is a real approved plan.
fn publish_approved_plan(store: &V1Store, task_id: &str, units: Vec<WorkUnitWire>) -> PlanRevision {
    let plan = revision(task_id, 1, None, units);
    store
        .publish_plan_revision(&plan, None)
        .expect("publish plan");
    let actor =
        PlanApprovalActor::new("plan-actor", "plan-session", PLAN_APPROVE_SCOPE).expect("actor");
    store
        .approve_plan_revision(task_id, plan.reference(), "plan-approval-1", actor)
        .expect("approve plan");
    plan
}

async fn seed_task(store: &V1Store, task_id: &str) {
    let state = TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind: TaskKind::Implementation,
        objective: "effect approval fixture".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    });
    store
        .save_task_and_events(&state, vec![])
        .await
        .expect("seed task");
}

fn assert_keys(value: &serde_json::Value, expected: &[&str]) {
    let object = value.as_object().expect("json object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(keys, expected, "exact wire key set");
}

/// The wire view is field-for-field the persisted store record (the one
/// canonical payload P19B-C renders); an active row omits `supersededAtMs`.
fn assert_view_matches_record(view: &serde_json::Value, record: &EffectApprovalRecord) {
    assert_eq!(view["approvalId"], record.approval_id);
    assert_eq!(view["taskId"], record.task_id);
    assert_eq!(view["planRevision"], record.plan_revision);
    assert_eq!(view["workUnitId"], record.work_unit_id);
    assert_eq!(view["effectClass"], record.effect_class);
    assert_eq!(view["network"], record.network);
    assert_eq!(view["payloadHash"], record.payload_hash);
    assert_eq!(view["actorId"], record.actor_id);
    assert_eq!(view["sessionId"], record.session_id);
    assert_eq!(view["scope"], record.scope);
    assert_eq!(view["scope"], EFFECT_APPROVE_SCOPE);
    assert_eq!(view["createdAtMs"], record.created_at_ms);
    let state = match record.state {
        EffectApprovalState::Active => "active",
        EffectApprovalState::Superseded => "superseded",
    };
    assert_eq!(view["state"], state);
    let mut expected = VIEW_KEYS.to_vec();
    match record.superseded_at_ms {
        Some(ms) => {
            assert_eq!(view["supersededAtMs"], ms);
            expected.push("supersededAtMs");
        }
        None => assert!(
            view.get("supersededAtMs").is_none(),
            "an active approval must omit supersededAtMs"
        ),
    }
    assert_keys(view, &expected);
}

/// One real service over one real store with a published, approved plan.
struct Fixture {
    _root: tempfile::TempDir,
    service: ApplicationService,
    store: V1Store,
    task_id: String,
    plan: PlanRevision,
}

impl Fixture {
    async fn new(name: &str, units: Vec<WorkUnitWire>) -> Self {
        Self::with_timeout(name, units, Duration::from_secs(300)).await
    }

    async fn with_timeout(name: &str, units: Vec<WorkUnitWire>, timeout: Duration) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let profile = p_gate_support::profile(name, root.path());
        let models: Arc<dyn ModelService> = Arc::new(UnusedModel);
        let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
        let service =
            ApplicationService::compose_with_approval_timeout(&profile, models, tools, timeout)
                .expect("compose");
        let store = V1Store::open(&profile.database_path()).expect("open store");
        let task_id = format!("task-{name}");
        service
            .create_task(&task_id, "effect fixture", TaskKind::Implementation, vec![])
            .await
            .expect("create task");
        let plan = publish_approved_plan(&store, &task_id, units);
        Self {
            _root: root,
            service,
            store,
            task_id,
            plan,
        }
    }

    fn wire(&self, unit_id: &str) -> WorkUnitWire {
        self.plan
            .material()
            .work_units
            .iter()
            .find(|wire| wire.id == unit_id)
            .expect("unit in plan")
            .clone()
    }

    fn effect_binding(&self, unit_id: &str) -> EffectBinding {
        let wire = self.wire(unit_id);
        EffectBinding {
            plan_revision: self.plan.reference().as_str().into(),
            work_unit_id: unit_id.into(),
            effect_class: wire.effect_class.as_str().into(),
            network: wire.network_ceiling.as_str().into(),
            payload_hash: work_unit_payload_hash(&wire),
        }
    }

    fn request_material(&self, unit_id: &str) -> serde_json::Value {
        self.effect_binding(unit_id).material(&self.task_id)
    }

    async fn request(
        &self,
        op: &str,
        unit_id: &str,
    ) -> Result<serde_json::Value, ApplicationError> {
        self.service
            .effect_approval_request(&self.task_id, unit_id, op, "run-fixture")
            .await
    }

    async fn decide(
        &self,
        op: &str,
        decision: &str,
        actor: &str,
        session: &str,
    ) -> Result<serde_json::Value, ApplicationError> {
        self.service
            .approvals_decide_with_session(
                &json!({"operationId": op, "decision": decision}),
                actor,
                session,
                CommandSource::Local,
            )
            .await
    }

    async fn revoke(
        &self,
        actor: &str,
        session: &str,
    ) -> Result<serde_json::Value, ApplicationError> {
        self.service
            .effect_approval_revoke(&self.task_id, UNIT, actor, session)
            .await
    }

    async fn list(&self) -> serde_json::Value {
        self.service
            .effect_approval_list(&self.task_id)
            .await
            .expect("list")
    }

    fn rows(&self) -> Vec<EffectApprovalRecord> {
        self.store
            .effect_approvals_for_task(&self.task_id)
            .expect("audit rows")
    }

    fn rows_for(&self, unit_id: &str) -> Vec<EffectApprovalRecord> {
        self.rows()
            .into_iter()
            .filter(|row| row.work_unit_id == unit_id)
            .collect()
    }

    fn events_of_kind(&self, kind: &str) -> Vec<JournalEvent> {
        self.store
            .task_events(&self.task_id)
            .into_iter()
            .filter(|event| event.kind == kind)
            .collect()
    }

    /// Move the head to revision 2 (auto-supersedes the active plan
    /// approval) with a different payload for `UNIT`.
    async fn publish_next_revision(&self, write_path: &str) -> PlanRevision {
        let next = revision(
            &self.task_id,
            2,
            Some(self.plan.reference().clone()),
            vec![escalated_wire(UNIT, write_path)],
        );
        self.store
            .publish_plan_revision(&next, Some(self.plan.reference()))
            .expect("publish revision 2");
        next
    }

    fn approve_head(&self, plan: &PlanRevision, approval_id: &str) {
        let actor = PlanApprovalActor::new("plan-actor-2", "plan-session-2", PLAN_APPROVE_SCOPE)
            .expect("actor");
        self.store
            .approve_plan_revision(&self.task_id, plan.reference(), approval_id, actor)
            .expect("approve head");
    }
}

fn ready_state(task_id: &str, plan: &PlanRevision) -> TaskState {
    let mut state = TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind: TaskKind::Implementation,
        objective: "run the pinned build".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    });
    state.execution = TaskExecution::Ready {
        approval: PlanApprovalRef {
            approval_id: "plan-approval-1".into(),
            plan_revision: plan.reference().clone(),
        },
    };
    state
}

/// The real execution freeze path with the PRODUCTION store-backed source
/// (the same wiring the run manager attaches at the freeze site).
async fn freeze_execution(
    store: &Arc<V1Store>,
    root: &Path,
    state: &TaskState,
    plan: &PlanRevision,
    unit: &WorkUnit,
) -> Result<FrozenRun, String> {
    let settings = SettingsStore::new(root);
    let models: Arc<dyn ModelService> = Arc::new(FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let source = StoreEffectApprovals::new(store.clone());
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
    RunSnapshotBuilder::new(&settings, &models, &tools, true)
        .with_effect_approvals(&source)
        .freeze_execution_with_workspace(
            state,
            &package,
            &GRANTS,
            &RunGuard::new("run-1", 1),
            workspace,
            plan,
            unit,
        )
        .await
}

// -- grant: exactly one immutable approval with authenticated attribution --

#[tokio::test]
async fn grant_persists_one_immutable_approval_with_authenticated_attribution() {
    let fixture = Fixture::new("s19br-grant", vec![escalated_wire(UNIT, "target")]).await;
    let requested = fixture.request("op-grant-1", UNIT).await.expect("request");
    assert_eq!(requested["status"], "pending");
    assert_eq!(requested["operationId"], "op-grant-1");
    assert_eq!(requested["request"], fixture.request_material(UNIT));
    assert!(requested.get("approval").is_none());

    // The journaled request binds the exact payload (durable across restarts)
    // and the RA2 list row carries the same binding for every client.
    let journaled = fixture.events_of_kind("approval.requested");
    assert_eq!(journaled.len(), 1);
    assert_eq!(
        journaled[0].payload["effect"],
        serde_json::to_value(fixture.effect_binding(UNIT)).expect("binding json")
    );
    let rows = fixture.service.approvals_list().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["opId"], "op-grant-1");
    assert_eq!(
        rows[0]["effect"],
        serde_json::to_value(fixture.effect_binding(UNIT)).expect("binding json")
    );

    let decided = fixture
        .decide("op-grant-1", "granted", "client-tui", "cmd-77")
        .await
        .expect("grant");
    assert_eq!(decided["decision"], "granted");
    assert_eq!(decided["decidedBy"], "client-tui");
    let view = &decided["approval"];
    assert_eq!(view["approvalId"], "op-grant-1", "one op = one approval id");
    assert_eq!(view["actorId"], "client-tui");
    assert_eq!(view["sessionId"], "cmd-77");
    assert_eq!(view["state"], "active");

    let records = fixture.rows();
    assert_eq!(records.len(), 1, "exactly one approval persists");
    assert_view_matches_record(view, &records[0]);

    // A re-request while the approval is active replays it; no second
    // pending question is queued.
    let replay = fixture
        .request("op-grant-2", UNIT)
        .await
        .expect("re-request");
    assert_eq!(replay["status"], "granted");
    assert!(replay.get("operationId").is_none());
    assert_view_matches_record(&replay["approval"], &records[0]);
    assert!(fixture.service.approvals_list().await.is_empty());

    let decided_events = fixture.events_of_kind("approval.decided");
    assert_eq!(decided_events.len(), 1);
    assert_eq!(decided_events[0].payload["decidedBy"], "client-tui");
    assert_eq!(decided_events[0].payload["decision"], "granted");
}

// -- deny + expiry: no approval, and the first decision wins forever -------

#[tokio::test]
async fn deny_and_timeout_expiry_materialize_nothing_and_first_decision_wins() {
    let fixture = Fixture::with_timeout(
        "s19br-deny",
        vec![escalated_wire(UNIT, "target")],
        Duration::from_millis(150),
    )
    .await;

    fixture.request("op-deny", UNIT).await.expect("request");
    let denied = fixture
        .decide("op-deny", "denied", "client-tui", "cmd-d")
        .await
        .expect("deny");
    assert_eq!(denied["decision"], "denied");
    assert!(
        denied.get("approval").is_none(),
        "a denial carries no approval"
    );
    assert!(fixture.rows().is_empty(), "denial persists no approval");

    // The first decision wins forever: a later grant on the same op is a
    // structural conflict and still materializes nothing.
    let error = fixture
        .decide("op-deny", "granted", "client-tui", "cmd-g")
        .await
        .expect_err("grant after denial")
        .to_string();
    assert!(error.contains("approval_conflict: op-deny"), "{error}");
    assert!(fixture.rows().is_empty());

    // Expiry: the real timeout path inside ApprovalStore denies with
    // "<timeout>" and never touches the effect materialization code.
    fixture.request("op-expire", UNIT).await.expect("request");
    let mut rx = fixture
        .service
        .approvals()
        .waiter("op-expire")
        .await
        .expect("host-created op has a waiter");
    let decision = fixture
        .service
        .approvals()
        .await_decision("op-expire", &mut rx)
        .await;
    assert_eq!(decision, ApprovalDecision::Denied);
    assert!(fixture.rows().is_empty(), "expiry persists no approval");
    let timeout = fixture
        .events_of_kind("approval.decided")
        .into_iter()
        .find(|event| event.payload["decidedBy"] == "<timeout>")
        .expect("timeout denial journaled");
    assert_eq!(timeout.payload["decision"], "denied");

    let error = fixture
        .decide("op-expire", "granted", "client-tui", "cmd-x")
        .await
        .expect_err("grant after expiry")
        .to_string();
    assert!(error.contains("approval_conflict: op-expire"), "{error}");
    assert!(fixture.rows().is_empty());

    // A denied unit is requestable again — with a NEW operation id.
    let fresh = fixture.request("op-fresh", UNIT).await.expect("re-request");
    assert_eq!(fresh["status"], "pending");
}

// -- replays: converge on the first persisted row, attribution never drifts -

#[tokio::test]
async fn grant_replays_converge_on_the_first_persisted_row() {
    let fixture = Fixture::new("s19br-replay", vec![escalated_wire(UNIT, "target")]).await;
    fixture.request("op-r1", UNIT).await.expect("request");
    let first = fixture
        .decide("op-r1", "granted", "actor-a", "session-a")
        .await
        .expect("first grant");
    let view = first["approval"].clone();

    // Identical replay (the heal-on-retry path when materialization failed
    // after journaling): Ok, same row, no second persist.
    let replay = fixture
        .decide("op-r1", "granted", "actor-a", "session-a")
        .await
        .expect("identical replay");
    assert_eq!(replay["approval"], view);
    assert_eq!(replay["decidedBy"], "actor-a");
    assert_eq!(replay["decidedSeq"], first["decidedSeq"]);
    assert_eq!(fixture.rows().len(), 1);

    // A replay under a DIFFERENT actor/session must not drift attribution.
    let drifted = fixture
        .decide("op-r1", "granted", "actor-b", "session-b")
        .await
        .expect("foreign replay");
    assert_eq!(drifted["approval"], view);
    assert_eq!(drifted["approval"]["actorId"], "actor-a");
    let record = &fixture.rows()[0];
    assert_eq!(record.actor_id, "actor-a");
    assert_eq!(record.session_id, "session-a");

    // The 3-arg compatibility entry keeps its exact signature and passes
    // decided_by as BOTH actor and session.
    let compat = Fixture::new("s19br-compat", vec![escalated_wire(UNIT, "target")]).await;
    compat.request("op-c1", UNIT).await.expect("request");
    let decided = compat
        .service
        .approvals_decide(
            &json!({"operationId": "op-c1", "decision": "granted"}),
            "actor-only",
            CommandSource::Local,
        )
        .await
        .expect("3-arg grant");
    assert_eq!(decided["approval"]["actorId"], "actor-only");
    assert_eq!(decided["approval"]["sessionId"], "actor-only");
    assert_eq!(compat.rows().len(), 1);

    assert_eq!(
        fixture.events_of_kind("approval.decided").len(),
        1,
        "replays never re-journal the decision"
    );
}

// -- stale: refuse BEFORE journaling; the pending op survives --------------

#[tokio::test]
async fn stale_pending_requests_refuse_without_consuming_themselves() {
    let fixture = Fixture::new("s19br-stale", vec![escalated_wire(UNIT, "target")]).await;
    fixture.request("op-s1", UNIT).await.expect("request");

    // The head moves past the pending binding (publishing rev2 supersedes
    // the rev1 plan approval): a grant is refused as plan_not_approved…
    let rev2 = fixture.publish_next_revision("target-v2").await;
    let error = fixture
        .decide("op-s1", "granted", "client", "cmd-s1")
        .await
        .expect_err("stale grant refused")
        .to_string();
    assert!(error.contains("plan_not_approved"), "{error}");
    assert!(fixture.rows().is_empty(), "a stale grant persists nothing");

    // …and re-approving the new head still refuses: the pending binding no
    // longer matches the approved payload.
    fixture.approve_head(&rev2, "plan-approval-2");
    let error = fixture
        .decide("op-s1", "granted", "client", "cmd-s2")
        .await
        .expect_err("payload mismatch refused")
        .to_string();
    assert!(error.contains("effect_approval_stale"), "{error}");
    assert!(fixture.rows().is_empty());

    // The refusal happened BEFORE journaling: nothing was decided and the
    // op is still pending (it did not consume itself).
    assert!(fixture.events_of_kind("approval.decided").is_empty());
    let listed = fixture.list().await;
    assert_eq!(listed["pending"].as_array().expect("pending").len(), 1);
    assert_eq!(listed["pending"][0]["operationId"], "op-s1");

    // A denial still lands on the stale op; it materializes nothing.
    fixture
        .decide("op-s1", "denied", "client", "cmd-s3")
        .await
        .expect("stale op stays deniable");
    assert!(fixture.rows().is_empty());
}

// -- carve-out: a materialized approval answers replays after plan moves ---

#[tokio::test]
async fn a_materialized_approval_answers_replays_after_the_plan_moves() {
    let fixture = Fixture::new("s19br-moved", vec![escalated_wire(UNIT, "target")]).await;
    fixture.request("op-m1", UNIT).await.expect("request");
    let granted = fixture
        .decide("op-m1", "granted", "actor-m", "cmd-m1")
        .await
        .expect("grant");
    let view = granted["approval"].clone();

    let rev2 = fixture.publish_next_revision("target-v2").await;
    fixture.approve_head(&rev2, "plan-approval-2");

    // The exact approval is already materialized, so the replay succeeds
    // even though the pending binding no longer matches the new head.
    let replay = fixture
        .decide("op-m1", "granted", "actor-m", "cmd-m2")
        .await
        .expect("replay after plan move");
    assert_eq!(replay["approval"], view);
    assert_eq!(fixture.rows().len(), 1);

    // A new request for the moved payload is refused while the OLD approval
    // is still active for the unit: revoke first, one active per unit.
    let error = fixture
        .request("op-m2", UNIT)
        .await
        .expect_err("active approval blocks a divergent request")
        .to_string();
    assert!(error.contains("effect_approval_active_exists"), "{error}");
    assert!(error.contains("revoke it first"), "{error}");

    // After revocation the new request binds to the NEW head payload…
    let revoked = fixture.revoke("revoker", "cmd-rev").await.expect("revoke");
    assert_eq!(revoked["revoked"], json!(true));
    assert_eq!(revoked["approvalId"], "op-m1");
    let requested = fixture.request("op-m2", UNIT).await.expect("re-request");
    assert_eq!(requested["status"], "pending");
    assert_eq!(
        requested["request"]["planRevision"],
        rev2.reference().as_str()
    );
    assert_eq!(
        requested["request"]["payloadHash"],
        work_unit_payload_hash(&rev2.material().work_units[0])
    );

    // …the revoked op is stale now (binding mismatches AND nothing active)…
    let error = fixture
        .decide("op-m1", "granted", "actor-m", "cmd-m3")
        .await
        .expect_err("revoked op replay")
        .to_string();
    assert!(error.contains("effect_approval_stale"), "{error}");

    // …and the fresh op mints a NEW immutable approval id.
    let granted2 = fixture
        .decide("op-m2", "granted", "actor-m2", "cmd-m4")
        .await
        .expect("grant rev2");
    assert_eq!(granted2["approval"]["approvalId"], "op-m2");
    assert_eq!(
        granted2["approval"]["planRevision"],
        rev2.reference().as_str()
    );
    let rows = fixture.rows();
    assert_eq!(rows.len(), 2, "superseded history plus one active row");
    assert_eq!(rows[0].approval_id, "op-m1");
    assert_eq!(rows[0].state, EffectApprovalState::Superseded);
    assert_eq!(rows[1].approval_id, "op-m2");
    assert_eq!(rows[1].state, EffectApprovalState::Active);
    assert_view_matches_record(&granted2["approval"], &rows[1]);
}

// -- a revoked approval id is never re-minted -------------------------------

#[tokio::test]
async fn a_revoked_approval_id_is_never_reminted() {
    let fixture = Fixture::new("s19br-remint", vec![escalated_wire(UNIT, "target")]).await;
    fixture.request("op-x", UNIT).await.expect("request");
    fixture
        .decide("op-x", "granted", "actor-x", "cmd-x")
        .await
        .expect("grant");
    fixture.revoke("revoker", "cmd-rev").await.expect("revoke");

    // The plan did NOT move, so the replay reaches materialization — and the
    // superseded id surfaces a conflict instead of being re-minted.
    let error = fixture
        .decide("op-x", "granted", "actor-x", "cmd-x2")
        .await
        .expect_err("replay of a revoked id")
        .to_string();
    assert!(error.contains("effect_approval_conflict: op-x"), "{error}");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, EffectApprovalState::Superseded);

    // Re-authorization requires a NEW operation id → a NEW approval id.
    let requested = fixture.request("op-y", UNIT).await.expect("re-request");
    assert_eq!(requested["status"], "pending");
    let granted = fixture
        .decide("op-y", "granted", "actor-y", "cmd-y")
        .await
        .expect("fresh grant");
    assert_eq!(granted["approval"]["approvalId"], "op-y");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].approval_id, "op-x");
    assert_eq!(rows[0].state, EffectApprovalState::Superseded);
    assert_eq!(rows[1].approval_id, "op-y");
    assert_eq!(rows[1].state, EffectApprovalState::Active);
    assert!(rows[1].created_at_ms >= rows[0].created_at_ms);
}

// -- concurrency: one active row per (task, work unit) ----------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_grants_leave_exactly_one_active_row_per_unit() {
    let fixture = Fixture::new(
        "s19br-concurrent",
        vec![
            escalated_wire(UNIT, "target"),
            escalated_wire(SECOND_UNIT, "target-second"),
        ],
    )
    .await;

    // Two DIFFERENT operations for the same (task, work unit), decided
    // simultaneously: both succeed and converge on one persisted row.
    fixture.request("op-a", UNIT).await.expect("request a");
    fixture.request("op-b", UNIT).await.expect("request b");
    let (a, b) = tokio::join!(
        fixture.decide("op-a", "granted", "actor-a", "sess-a"),
        fixture.decide("op-b", "granted", "actor-b", "sess-b"),
    );
    let (a, b) = (a.expect("grant a"), b.expect("grant b"));
    assert_eq!(a["approval"], b["approval"], "both converge on one row");
    let rows = fixture.rows_for(UNIT);
    assert_eq!(rows.len(), 1, "exactly one active row for the unit");
    assert_view_matches_record(&a["approval"], &rows[0]);

    // The SAME operation decided twice simultaneously: one journal record
    // wins, one row persists, both callers observe it.
    fixture
        .request("op-c", SECOND_UNIT)
        .await
        .expect("request c");
    let (c1, c2) = tokio::join!(
        fixture.decide("op-c", "granted", "actor-c1", "sess-c1"),
        fixture.decide("op-c", "granted", "actor-c2", "sess-c2"),
    );
    let (c1, c2) = (c1.expect("grant c1"), c2.expect("grant c2"));
    assert_eq!(c1["approval"], c2["approval"]);
    let rows = fixture.rows_for(SECOND_UNIT);
    assert_eq!(rows.len(), 1);
    assert_view_matches_record(&c1["approval"], &rows[0]);
    assert_eq!(fixture.rows().len(), 2, "one active row per work unit");
}

// -- revocation: future-runs-only, frozen snapshots are immutable -----------

#[tokio::test]
async fn revocation_never_rewrites_a_frozen_snapshot_and_stops_the_next_freeze() {
    let root = tempfile::tempdir().expect("tempdir");
    let profile = p_gate_support::profile("s19br-freeze", root.path());
    let models: Arc<dyn ModelService> = Arc::new(UnusedModel);
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    let store = Arc::new(V1Store::open(&profile.database_path()).expect("open store"));
    let plan = publish_approved_plan(&store, "task-frozen", vec![escalated_wire(UNIT, "target")]);
    let state = ready_state("task-frozen", &plan);
    store
        .save_task_and_events(&state, vec![])
        .await
        .expect("seed ready task");

    // Grant through the real authenticated flow, then freeze through the
    // real builder + production store-backed source.
    service
        .effect_approval_request("task-frozen", UNIT, "op-f1", "run-f1")
        .await
        .expect("request");
    service
        .approvals_decide_with_session(
            &json!({"operationId": "op-f1", "decision": "granted"}),
            "freeze-actor",
            "freeze-cmd",
            CommandSource::Local,
        )
        .await
        .expect("grant");
    let unit = kernel_unit(&plan.material().work_units[0]);
    let frozen = freeze_execution(&store, root.path(), &state, &plan, &unit)
        .await
        .expect("the granted approval expands the unit");
    store
        .save_run_snapshot(&frozen.snapshot)
        .expect("persist the frozen run");
    let snapshot_id = frozen.snapshot.id().clone();

    // Revoke through the real method: supersede + attribution journal.
    let revoked = service
        .effect_approval_revoke("task-frozen", UNIT, "revoker-1", "revoke-cmd")
        .await
        .expect("revoke");
    assert_eq!(
        revoked,
        json!({
            "taskId": "task-frozen",
            "workUnitId": UNIT,
            "approvalId": "op-f1",
            "revoked": true,
        })
    );
    let rows = store
        .effect_approvals_for_task("task-frozen")
        .expect("audit rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, EffectApprovalState::Superseded);
    assert!(rows[0].superseded_at_ms.is_some());

    // The already-frozen snapshot row is untouched (immutable content-addressed row).
    let reloaded = store
        .load_run_snapshot(snapshot_id.as_str())
        .expect("load snapshot")
        .expect("frozen row survives revocation");
    assert!(
        reloaded == frozen.snapshot,
        "revocation must never rewrite a frozen RunSnapshot"
    );

    // Only the NEXT freeze re-consults the store — and fails closed.
    let refused = match freeze_execution(&store, root.path(), &state, &plan, &unit).await {
        Ok(_) => panic!("a revoked approval must stop the next freeze"),
        Err(error) => error,
    };
    assert!(
        refused.contains("exact active effect approval"),
        "{refused}"
    );

    // Revoking again is an idempotent no-op.
    let again = service
        .effect_approval_revoke("task-frozen", UNIT, "revoker-1", "revoke-cmd-2")
        .await
        .expect("revoke again");
    assert_eq!(again["revoked"], json!(false));
    assert!(again["approvalId"].is_null());
}

// -- list projection: pending + audit, field-for-field the store record -----

#[tokio::test]
async fn the_list_projection_is_field_for_field_the_persisted_record() {
    let fixture = Fixture::new("s19br-list", vec![escalated_wire(UNIT, "target")]).await;
    assert_eq!(
        fixture.list().await,
        json!({"taskId": fixture.task_id, "pending": [], "approvals": []})
    );

    fixture.request("op-l1", UNIT).await.expect("request");
    let listed = fixture.list().await;
    assert_eq!(listed["taskId"], fixture.task_id);
    assert_eq!(listed["approvals"], json!([]));
    let pending = listed["pending"].as_array().expect("pending rows");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["operationId"], "op-l1");
    assert_eq!(
        pending[0]["summary"],
        "effect approval for unit-shell [workspace-mutation/public-internet-client]"
    );
    assert!(pending[0]["createdSeq"].is_u64());
    assert!(pending[0]["createdMs"].is_i64());
    assert!(pending[0]["ageMs"].is_u64());
    assert_eq!(pending[0]["request"], fixture.request_material(UNIT));
    assert_keys(
        &pending[0],
        &[
            "operationId",
            "summary",
            "createdSeq",
            "createdMs",
            "ageMs",
            "request",
        ],
    );

    let decided = fixture
        .decide("op-l1", "granted", "actor-l", "cmd-l")
        .await
        .expect("grant");
    let listed = fixture.list().await;
    assert_eq!(listed["pending"], json!([]), "decided ops leave the list");
    assert_eq!(listed["approvals"].as_array().expect("approvals").len(), 1);
    assert_eq!(listed["approvals"][0], decided["approval"]);
    assert_view_matches_record(&listed["approvals"][0], &fixture.rows()[0]);

    // Revocation keeps the audit row field-for-field, now superseded.
    fixture.revoke("actor-l", "cmd-l2").await.expect("revoke");
    let listed = fixture.list().await;
    assert_eq!(listed["approvals"][0]["state"], "superseded");
    assert_view_matches_record(&listed["approvals"][0], &fixture.rows()[0]);
    assert_eq!(
        listed["approvals"][0]["scope"], EFFECT_APPROVE_SCOPE,
        "every approval is minted under the fixed scope"
    );
}

// -- durability: a restarted daemon materializes only the journaled binding -

#[tokio::test]
async fn a_restarted_daemon_materializes_only_the_journaled_binding() {
    let root = tempfile::tempdir().expect("tempdir");
    let profile = p_gate_support::profile("s19br-restart", root.path());
    let models: Arc<dyn ModelService> = Arc::new(UnusedModel);
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    let store = V1Store::open(&profile.database_path()).expect("open store");
    seed_task(&store, "task-restart").await;
    let plan = publish_approved_plan(&store, "task-restart", vec![escalated_wire(UNIT, "target")]);
    service
        .effect_approval_request("task-restart", UNIT, "op-restart", "run-r")
        .await
        .expect("request");
    // "Daemon dies" before any decision: only the journal survives.
    drop(service);

    let restarted = ApplicationService::compose(
        &profile,
        Arc::new(UnusedModel) as Arc<dyn ModelService>,
        Arc::new(FakeToolService::default()) as Arc<dyn ToolService>,
    )
    .expect("recompose");
    restarted.rebuild_approvals().await;

    let listed = restarted
        .effect_approval_list("task-restart")
        .await
        .expect("list");
    let pending = listed["pending"].as_array().expect("pending rows");
    assert_eq!(pending.len(), 1, "the pending op rebuilds from the journal");
    assert_eq!(pending[0]["operationId"], "op-restart");
    let expected = EffectBinding {
        plan_revision: plan.reference().as_str().into(),
        work_unit_id: UNIT.into(),
        effect_class: "workspace-mutation".into(),
        network: "public-internet-client".into(),
        payload_hash: work_unit_payload_hash(&plan.material().work_units[0]),
    }
    .material("task-restart");
    assert_eq!(pending[0]["request"], expected);

    // The restarted daemon grants EXACTLY the journaled payload.
    let decided = restarted
        .approvals_decide_with_session(
            &json!({"operationId": "op-restart", "decision": "granted"}),
            "actor-r",
            "cmd-r",
            CommandSource::Local,
        )
        .await
        .expect("grant after restart");
    let rows = store
        .effect_approvals_for_task("task-restart")
        .expect("audit rows");
    assert_eq!(rows.len(), 1);
    assert_view_matches_record(&decided["approval"], &rows[0]);
    assert_eq!(rows[0].payload_hash, expected["payloadHash"]);
}

// -- no plugin can mint: the plugin surface never reaches the effect flow ---

fn plugin_wait_request(op_id: &str) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: "host.approvals.request".into(),
        params: Some(json!({
            "pending_operation": {"operation_id": op_id, "inputHash": "x"},
            "summary": "effect approval",
        })),
    }
}

fn raw_request(method: &str) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: method.into(),
        params: Some(json!({
            "taskId": "t1",
            "workUnitId": UNIT,
            "operationId": "op-forged",
            "decision": "granted",
        })),
    }
}

#[tokio::test]
async fn no_plugin_surface_can_reach_or_mint_effect_approvals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(V1Store::open(&dir.path().join("journal.db")).expect("open store"));
    seed_task(&store, "t1").await;
    let approvals = Arc::new(ApprovalStore::new(store.clone(), Duration::from_secs(300)));
    let router = HostRouter::new(
        RunIdentity {
            task_id: "t1".into(),
            branch_id: "branch-t1".into(),
            run_id: "run-t1-1".into(),
            attempt_id: "attempt-t1-1".into(),
            generation: 1,
        },
        RunGuard::new("run-t1-1", 1),
        vec![HostService::ApprovalsRequest],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        store.clone(),
        Arc::new(IgnoreQuestions),
    )
    .with_approvals(approvals.clone());

    // The protocol and router allow-lists exclude the whole decision and
    // effect surface: a plugin frame is refused as an unknown method.
    for method in [
        "approvals.effect.request",
        "approvals.effect.list",
        "approvals.effect.revoke",
        "approvals.decide",
        "approvals.list",
    ] {
        assert!(!is_known_method(method), "{method} is not a plugin method");
        assert!(
            service_for_method(method).is_none(),
            "{method} maps to no host service"
        );
        let error = router
            .handle_request(raw_request(method))
            .await
            .expect_err("plugin frames cannot reach the effect flow");
        assert_eq!(error.code, error_code::UNKNOWN_METHOD, "{method}");
    }

    // The only plugin approval verb is WAITING on a host-created op — and
    // the reply carries the decision only, never approval material.
    let binding = EffectBinding {
        plan_revision: format!("sha256:{}", "b".repeat(64)),
        work_unit_id: UNIT.into(),
        effect_class: "workspace-mutation".into(),
        network: "offline".into(),
        payload_hash: format!("sha256:{}", "c".repeat(64)),
    };
    approvals
        .register_effect(
            "op-eff",
            "effect approval for unit-shell",
            "run-t1-1",
            "t1",
            binding.clone(),
        )
        .await;
    let error = router
        .handle_request(plugin_wait_request("op-forged"))
        .await
        .expect_err("forged references stay refused");
    assert_eq!(error.code, error_code::PROTOCOL_VIOLATION);

    // Even a host-side store-level grant (bypassing ApplicationService)
    // journals the decision but persists NO approval: materialization lives
    // only in the authenticated application path.
    approvals
        .decide("op-eff", ApprovalDecision::Granted, "client-host")
        .await
        .expect("store-level decide");
    let reply = router
        .handle_request(plugin_wait_request("op-eff"))
        .await
        .expect("waiter observes the decision");
    assert_eq!(reply, json!({"decision": "granted"}));
    assert!(
        store
            .effect_approvals_for_task("t1")
            .expect("audit rows")
            .is_empty(),
        "the plugin-facing path can never mint an approval"
    );

    let events = store.task_events("t1");
    assert_eq!(events.len(), 2, "requested + decided, nothing else");
    let requested = events
        .iter()
        .find(|event| event.kind == "approval.requested")
        .expect("requested event");
    assert_eq!(
        requested.payload["effect"],
        serde_json::to_value(&binding).expect("binding json"),
        "the journaled binding is the canonical camelCase material"
    );
    assert!(events
        .iter()
        .all(|event| !event.kind.starts_with("effect.")));
}

// -- remote transports are refused for the new effect RPCs ------------------

#[test]
fn remote_transports_are_refused_for_the_effect_rpcs() {
    // The listener gate refuses any method without a capability mapping;
    // the P19B-R effect surface is deliberately unmapped (local-only until
    // a later wave wires remote exposure).
    for method in [
        "approvals.effect.request",
        "approvals.effect.list",
        "approvals.effect.revoke",
    ] {
        assert_eq!(
            required_capability(method),
            None,
            "{method} must stay unreachable from remote transports"
        );
    }
    // The pre-existing remote surface is unchanged.
    assert_eq!(
        required_capability("approvals.decide"),
        Some(Capability::ApprovalsDecide)
    );
    assert_eq!(
        required_capability("approvals.list"),
        Some(Capability::EventsRead)
    );
}

// -- the real daemon wire surface -------------------------------------------

fn seed_daemon_plan(name: &str, root: &Path) -> (RuntimeProfile, V1Store, PlanRevision) {
    let profile = p_gate_support::profile(name, root);
    let store = V1Store::open(&profile.database_path()).expect("open store");
    let plan = publish_approved_plan(&store, "task-rpc", vec![escalated_wire(UNIT, "target")]);
    (profile, store, plan)
}

async fn daemon_request_and_grant(
    client: &mut r_code_client::DaemonClient,
    request_command: &str,
    decide_command: &str,
) -> (serde_json::Value, serde_json::Value) {
    let requested = client
        .call_with_id(
            "approvals.effect.request",
            json!({"taskId": "task-rpc", "workUnitId": UNIT, "operationId": "op-wire"}),
            request_command,
        )
        .await
        .expect("effect request");
    let decided = client
        .call_with_id(
            "approvals.decide",
            json!({"operationId": "op-wire", "decision": "granted"}),
            decide_command,
        )
        .await
        .expect("decide granted");
    (requested, decided)
}

#[tokio::test]
async fn daemon_rpc_exposes_exact_request_material_and_authenticated_attribution() {
    let root = tempfile::tempdir().expect("tempdir");
    let (profile, store, plan) = seed_daemon_plan("s19br-daemon-decide", root.path());
    seed_task(&store, "task-rpc").await;
    let daemon = Daemon::start(&profile, None);
    let mut client = daemon.connect(&profile, "s19br-client").await;

    // operationId defaults to the command id (unique per request, stable
    // across deduped retries).
    let requested = client
        .call_with_id(
            "approvals.effect.request",
            json!({"taskId": "task-rpc", "workUnitId": UNIT}),
            "req-cmd-1",
        )
        .await
        .expect("effect request");
    assert_eq!(requested["status"], "pending");
    assert_eq!(requested["operationId"], "req-cmd-1");
    assert_eq!(requested["request"]["taskId"], "task-rpc");
    assert_eq!(
        requested["request"]["planRevision"],
        plan.reference().as_str()
    );
    assert_eq!(requested["request"]["workUnitId"], UNIT);
    assert_eq!(requested["request"]["effectClass"], "workspace-mutation");
    assert_eq!(requested["request"]["network"], "public-internet-client");
    assert_eq!(
        requested["request"]["payloadHash"],
        work_unit_payload_hash(&plan.material().work_units[0])
    );
    assert_keys(
        &requested["request"],
        &[
            "taskId",
            "planRevision",
            "workUnitId",
            "effectClass",
            "network",
            "payloadHash",
        ],
    );

    // The RA2 pending row carries the same binding for every client.
    let pending = client
        .call("approvals.list", json!({}))
        .await
        .expect("approvals.list");
    assert_eq!(pending["pending"][0]["opId"], "req-cmd-1");
    assert_eq!(
        pending["pending"][0]["effect"]["payloadHash"],
        requested["request"]["payloadHash"]
    );
    assert_keys(
        &pending["pending"][0]["effect"],
        &[
            "planRevision",
            "workUnitId",
            "effectClass",
            "network",
            "payloadHash",
        ],
    );

    // Attribution rides the AUTHENTICATED identities; params cannot inject
    // actor/session/decidedBy material.
    let decide_params = json!({
        "operationId": "req-cmd-1",
        "decision": "granted",
        "actorId": "spoofed-actor",
        "sessionId": "spoofed-session",
        "decidedBy": "spoofed-decider",
    });
    let decided = client
        .call_with_id("approvals.decide", decide_params.clone(), "decide-cmd-1")
        .await
        .expect("decide granted");
    assert_eq!(decided["decidedBy"], "s19br-client");
    assert_eq!(decided["approval"]["actorId"], "s19br-client");
    assert_eq!(decided["approval"]["sessionId"], "decide-cmd-1");
    assert_eq!(decided["approval"]["scope"], EFFECT_APPROVE_SCOPE);
    assert_eq!(decided["approval"]["approvalId"], "req-cmd-1");
    let rows = store
        .effect_approvals_for_task("task-rpc")
        .expect("audit rows");
    assert_eq!(rows.len(), 1, "the wire grant persists exactly one row");
    assert_eq!(rows[0].actor_id, "s19br-client");
    assert_eq!(rows[0].session_id, "decide-cmd-1");
    assert_view_matches_record(&decided["approval"], &rows[0]);

    // A replayed command id returns the identical receipt (daemon dedup).
    let replay = client
        .call_with_id("approvals.decide", decide_params, "decide-cmd-1")
        .await
        .expect("dedup replay");
    assert_eq!(replay, decided);

    // A fresh request while the approval is active short-circuits to it.
    let again = client
        .call(
            "approvals.effect.request",
            json!({"taskId": "task-rpc", "workUnitId": UNIT}),
        )
        .await
        .expect("request replay");
    assert_eq!(again["status"], "granted");
    assert_eq!(again["approval"], decided["approval"]);
    assert!(again.get("operationId").is_none());
}

#[tokio::test]
async fn daemon_rpc_list_and_revoke_audit_the_persisted_row() {
    let root = tempfile::tempdir().expect("tempdir");
    let (profile, store, _plan) = seed_daemon_plan("s19br-daemon-revoke", root.path());
    seed_task(&store, "task-rpc").await;
    let daemon = Daemon::start(&profile, None);
    let mut client = daemon.connect(&profile, "s19br-client").await;
    let (requested, decided) =
        daemon_request_and_grant(&mut client, "req-cmd-1", "decide-cmd-1").await;

    // The granted approval echoes the requested canonical material.
    for (key, value) in requested["request"].as_object().expect("request object") {
        assert_eq!(
            &decided["approval"][key.as_str()],
            value,
            "grant echoes {key}"
        );
    }

    let listed = client
        .call("approvals.effect.list", json!({"taskId": "task-rpc"}))
        .await
        .expect("effect list");
    assert_eq!(listed["taskId"], "task-rpc");
    assert_eq!(listed["pending"], json!([]));
    assert_eq!(listed["approvals"].as_array().expect("approvals").len(), 1);
    assert_eq!(listed["approvals"][0], decided["approval"]);
    let rows = store
        .effect_approvals_for_task("task-rpc")
        .expect("audit rows");
    assert_view_matches_record(&listed["approvals"][0], &rows[0]);

    let revoked = client
        .call_with_id(
            "approvals.effect.revoke",
            json!({"taskId": "task-rpc", "workUnitId": UNIT}),
            "revoke-cmd-1",
        )
        .await
        .expect("revoke");
    assert_eq!(
        revoked,
        json!({
            "taskId": "task-rpc",
            "workUnitId": UNIT,
            "approvalId": "op-wire",
            "revoked": true,
        })
    );
    let rows = store
        .effect_approvals_for_task("task-rpc")
        .expect("audit rows");
    assert_eq!(rows.len(), 1, "revocation supersedes, never deletes");
    assert_eq!(rows[0].state, EffectApprovalState::Superseded);
    let event = store
        .task_events("task-rpc")
        .into_iter()
        .find(|event| event.kind == "effect.approval.revoked")
        .expect("revoked attribution event");
    assert_eq!(event.payload["approvalId"], "op-wire");
    assert_eq!(event.payload["workUnitId"], UNIT);
    assert_eq!(event.payload["actorId"], "s19br-client");
    assert_eq!(event.payload["sessionId"], "revoke-cmd-1");

    // Idempotent: nothing active → revoked:false, no second supersede.
    let again = client
        .call_with_id(
            "approvals.effect.revoke",
            json!({"taskId": "task-rpc", "workUnitId": UNIT}),
            "revoke-cmd-2",
        )
        .await
        .expect("revoke again");
    assert_eq!(again["revoked"], json!(false));
    assert!(again["approvalId"].is_null());

    // The list keeps auditing the superseded row field-for-field.
    let audited = client
        .call("approvals.effect.list", json!({"taskId": "task-rpc"}))
        .await
        .expect("effect list after revoke");
    assert_eq!(audited["approvals"][0]["state"], "superseded");
    let rows = store
        .effect_approvals_for_task("task-rpc")
        .expect("audit rows");
    assert_view_matches_record(&audited["approvals"][0], &rows[0]);

    // Wire errors carry the structured contract codes.
    let error = client
        .call(
            "approvals.effect.request",
            json!({"taskId": "task-rpc", "workUnitId": "unit-nope"}),
        )
        .await
        .expect_err("unknown unit refused");
    assert!(error.to_string().contains("work_unit_not_found"), "{error}");
    let error = client
        .call(
            "approvals.effect.request",
            json!({"taskId": "task-unknown", "workUnitId": UNIT}),
        )
        .await
        .expect_err("unknown task refused");
    assert!(
        error.to_string().contains("task task-unknown not found"),
        "{error}"
    );
}
