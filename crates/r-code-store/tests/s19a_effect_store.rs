//! S19A — immutable exact-plan effect approvals in the real V1Store.

use std::path::{Path, PathBuf};

use r_code_harness_protocol::services::{
    work_unit_payload_hash, NetworkCeiling, WorkUnitEffectClass, WorkUnitWire,
};
use r_code_kernel::{
    PlanApprovalActor, PlanRevision, PlanRevisionMaterial, PlanRevisionRef, PLAN_APPROVE_SCOPE,
};
use r_code_store::v1::plans::{EffectApprovalRecord, EffectApprovalState, PlanStoreError};
use r_code_store::v1::V1Store;
use rusqlite::Connection;

const CREATED_AT_MS: i64 = 1_700_000_000_000;

fn database_path(root: &Path) -> PathBuf {
    root.join("harness-v1").join("tasks.sqlite3")
}

fn wire_unit(class: WorkUnitEffectClass, ceiling: NetworkCeiling) -> WorkUnitWire {
    WorkUnitWire {
        id: "unit-shell".into(),
        description: "run the pinned build".into(),
        dependencies: Vec::new(),
        acceptance: vec!["check:build".into()],
        read_paths: vec!["crates".into()],
        write_paths: vec!["target".into()],
        repo_exclusive: false,
        ephemeral_roots: Vec::new(),
        effect_class: class,
        network_ceiling: ceiling,
    }
}

fn plan(task_id: &str, marker: &str, unit: WorkUnitWire) -> PlanRevision {
    PlanRevision::new(PlanRevisionMaterial {
        task_id: task_id.into(),
        revision: 1,
        parent_revision: None,
        current_base_hash: format!("sha256:base-{marker}"),
        workspace_baseline: "sha256:workspace".into(),
        route_digest: "sha256:route".into(),
        prompt_digest: "sha256:prompt".into(),
        permission_digest: "sha256:permission".into(),
        check_digest: "sha256:checks".into(),
        required_checks: vec!["check:build".into()],
        work_units: vec![unit],
    })
    .expect("valid plan")
}

/// The exact identity an effect approval must carry: a real published plan
/// revision plus the canonical payload hash of its normalized wire unit.
#[derive(Debug, Clone)]
struct Granted {
    task_id: String,
    plan_revision: String,
    work_unit_id: String,
    effect_class: String,
    network: String,
    payload_hash: String,
}

fn granted_from(task_id: &str, authored: &PlanRevision, revision: &PlanRevisionRef) -> Granted {
    let unit = &authored.material().work_units[0];
    Granted {
        task_id: task_id.into(),
        plan_revision: revision.as_str().into(),
        work_unit_id: unit.id.clone(),
        effect_class: unit.effect_class.as_str().into(),
        network: unit.network_ceiling.as_str().into(),
        payload_hash: work_unit_payload_hash(unit),
    }
}

/// Publish one real plan carrying effect authority and return its identity.
fn grant(store: &V1Store, task_id: &str, marker: &str) -> Granted {
    grant_unit(
        store,
        task_id,
        marker,
        wire_unit(
            WorkUnitEffectClass::WorkspaceMutation,
            NetworkCeiling::PublicInternetClient,
        ),
    )
}

fn grant_unit(store: &V1Store, task_id: &str, marker: &str, unit: WorkUnitWire) -> Granted {
    let authored = plan(task_id, marker, unit);
    let revision = store
        .publish_plan_revision(&authored, None)
        .expect("publish plan");
    granted_from(task_id, &authored, &revision)
}

impl Granted {
    fn record(&self, approval_id: &str) -> EffectApprovalRecord {
        EffectApprovalRecord {
            approval_id: approval_id.into(),
            task_id: self.task_id.clone(),
            plan_revision: self.plan_revision.clone(),
            work_unit_id: self.work_unit_id.clone(),
            effect_class: self.effect_class.clone(),
            network: self.network.clone(),
            actor_id: "actor-user".into(),
            session_id: "session-1".into(),
            scope: "effect.approve".into(),
            payload_hash: self.payload_hash.clone(),
            state: EffectApprovalState::Active,
            created_at_ms: CREATED_AT_MS,
            superseded_at_ms: None,
        }
    }

    fn find(&self, store: &V1Store) -> Option<EffectApprovalRecord> {
        store
            .find_active_effect_approval(
                &self.task_id,
                &self.plan_revision,
                &self.work_unit_id,
                &self.effect_class,
                &self.network,
                &self.payload_hash,
            )
            .expect("lookup")
    }
}

/// One near-miss lookup: the exact identity with a single column perturbed.
struct Probe {
    task_id: String,
    plan_revision: String,
    work_unit_id: String,
    effect_class: String,
    network: String,
    payload_hash: String,
}

/// Which single identity column a probe falsifies.
#[derive(Debug, Clone, Copy)]
enum Mismatch {
    Task,
    PlanRevision,
    WorkUnit,
    WeakerClass,
    StrongerClass,
    WeakerCeiling,
    HostCeiling,
    PayloadHash,
}

impl Mismatch {
    fn name(self) -> &'static str {
        match self {
            Self::Task => "foreign task",
            Self::PlanRevision => "stale plan revision",
            Self::WorkUnit => "foreign work unit",
            Self::WeakerClass => "weaker class",
            Self::StrongerClass => "stronger class",
            Self::WeakerCeiling => "weaker ceiling",
            Self::HostCeiling => "host ceiling",
            Self::PayloadHash => "foreign payload hash",
        }
    }

    fn probe(self, granted: &Granted, foreign: &Granted) -> Probe {
        let mut probe = Probe::exact(granted);
        match self {
            Self::Task => probe.task_id = "task-other".into(),
            Self::PlanRevision => {
                probe.plan_revision = format!("sha256:{}", "a".repeat(64));
            }
            Self::WorkUnit => probe.work_unit_id = "unit-other".into(),
            Self::WeakerClass => probe.effect_class = "read-only".into(),
            Self::StrongerClass => probe.effect_class = "dependency-preparation".into(),
            Self::WeakerCeiling => probe.network = "offline".into(),
            Self::HostCeiling => probe.network = "host-network".into(),
            Self::PayloadHash => probe.payload_hash = foreign.payload_hash.clone(),
        }
        probe
    }
}

/// Which single required field a rejected record corrupts.
#[derive(Debug, Clone, Copy)]
enum InvalidField {
    ApprovalId,
    TaskId,
    PlanRevision,
    WorkUnitId,
    Actor,
    Session,
    Scope,
    PayloadHash,
    EffectClass,
    Network,
    RetiredState,
    RetiredTimestamp,
}

impl InvalidField {
    const ALL: [Self; 12] = [
        Self::ApprovalId,
        Self::TaskId,
        Self::PlanRevision,
        Self::WorkUnitId,
        Self::Actor,
        Self::Session,
        Self::Scope,
        Self::PayloadHash,
        Self::EffectClass,
        Self::Network,
        Self::RetiredState,
        Self::RetiredTimestamp,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::ApprovalId => "an empty approval id",
            Self::TaskId => "an empty task id",
            Self::PlanRevision => "an empty plan revision",
            Self::WorkUnitId => "an empty work unit id",
            Self::Actor => "an empty actor",
            Self::Session => "an empty session",
            Self::Scope => "the plan scope",
            Self::PayloadHash => "an empty payload hash",
            Self::EffectClass => "a junk effect class",
            Self::Network => "a junk network ceiling",
            Self::RetiredState => "an already retired state",
            Self::RetiredTimestamp => "a retirement timestamp",
        }
    }

    fn corrupt(self, record: &mut EffectApprovalRecord) {
        match self {
            Self::ApprovalId => record.approval_id = "  ".into(),
            Self::TaskId => record.task_id = String::new(),
            Self::PlanRevision => record.plan_revision = String::new(),
            Self::WorkUnitId => record.work_unit_id = String::new(),
            Self::Actor => record.actor_id = String::new(),
            Self::Session => record.session_id = String::new(),
            Self::Scope => record.scope = PLAN_APPROVE_SCOPE.into(),
            Self::PayloadHash => record.payload_hash = String::new(),
            Self::EffectClass => record.effect_class = "everything".into(),
            Self::Network => record.network = "internet".into(),
            Self::RetiredState => record.state = EffectApprovalState::Superseded,
            Self::RetiredTimestamp => record.superseded_at_ms = Some(CREATED_AT_MS),
        }
    }
}

impl Probe {
    fn exact(granted: &Granted) -> Self {
        Self {
            task_id: granted.task_id.clone(),
            plan_revision: granted.plan_revision.clone(),
            work_unit_id: granted.work_unit_id.clone(),
            effect_class: granted.effect_class.clone(),
            network: granted.network.clone(),
            payload_hash: granted.payload_hash.clone(),
        }
    }

    fn find(&self, store: &V1Store) -> Option<EffectApprovalRecord> {
        store
            .find_active_effect_approval(
                &self.task_id,
                &self.plan_revision,
                &self.work_unit_id,
                &self.effect_class,
                &self.network,
                &self.payload_hash,
            )
            .expect("lookup")
    }
}

/// A raw row written straight through SQLite, bypassing the repository's
/// validation so the DDL constraints are proven on their own.
struct RawApproval {
    approval_id: &'static str,
    work_unit_id: &'static str,
    effect_class: &'static str,
    network: &'static str,
    scope: &'static str,
    state: &'static str,
}

impl RawApproval {
    fn valid() -> Self {
        Self {
            approval_id: "raw-1",
            work_unit_id: "unit-shell",
            effect_class: "workspace-mutation",
            network: "public-internet-client",
            scope: "effect.approve",
            state: "active",
        }
    }

    fn insert(&self, path: &Path) -> rusqlite::Result<usize> {
        Connection::open(path).expect("open raw").execute(
            "INSERT INTO work_unit_effect_approvals(
                 approval_id, task_id, plan_revision, work_unit_id, effect_class,
                 network, actor_id, session_id, scope, payload_hash, state,
                 created_at_ms, superseded_at_ms)
             VALUES (?1, 'task-raw', 'sha256:plan', ?2, ?3, ?4, 'actor', 'session',
                     ?5, 'payload', ?6, ?7, NULL)",
            rusqlite::params![
                self.approval_id,
                self.work_unit_id,
                self.effect_class,
                self.network,
                self.scope,
                self.state,
                CREATED_AT_MS
            ],
        )
    }
}

fn row_count(path: &Path) -> i64 {
    Connection::open(path)
        .expect("open raw")
        .query_row(
            "SELECT COUNT(*) FROM work_unit_effect_approvals",
            [],
            |row| row.get(0),
        )
        .expect("count")
}

#[test]
fn exact_effect_approval_round_trips_and_survives_reopen() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let granted = grant(&store, "task-round-trip", "round-trip");
    let expected = granted.record("effect-approval-1");

    store.save_effect_approval(expected.clone()).expect("save");

    let found = granted.find(&store).expect("exact match");
    assert_eq!(found, expected);
    assert_eq!(found.state, EffectApprovalState::Active);
    assert_eq!(found.superseded_at_ms, None);
    assert_eq!(found.scope, "effect.approve");
    assert_eq!(found.actor_id, "actor-user");
    assert_eq!(found.session_id, "session-1");

    let audit = store
        .effect_approvals_for_task("task-round-trip")
        .expect("audit");
    assert_eq!(audit, vec![expected]);
    assert!(store
        .effect_approvals_for_task("task-unknown")
        .expect("audit")
        .is_empty());

    drop(store);
    let reopened = V1Store::open(&path).expect("reopen");
    assert_eq!(
        granted.find(&reopened).expect("durable"),
        granted.record("effect-approval-1")
    );
}

#[test]
fn every_identity_column_must_match_exactly() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(temp.path())).expect("open");
    let granted = grant(&store, "task-exact", "exact");
    let foreign = grant_unit(
        &store,
        "task-foreign",
        "foreign",
        WorkUnitWire {
            write_paths: vec!["other".into()],
            ..wire_unit(
                WorkUnitEffectClass::WorkspaceMutation,
                NetworkCeiling::PublicInternetClient,
            )
        },
    );
    assert_ne!(
        foreign.payload_hash, granted.payload_hash,
        "the foreign payload probe must differ or it proves nothing"
    );
    let first = granted.record("effect-approval-exact");
    store.save_effect_approval(first.clone()).expect("save");

    assert!(granted.find(&store).is_some());

    let mismatches = [
        Mismatch::Task,
        Mismatch::PlanRevision,
        Mismatch::WorkUnit,
        Mismatch::WeakerClass,
        Mismatch::StrongerClass,
        Mismatch::WeakerCeiling,
        Mismatch::HostCeiling,
        Mismatch::PayloadHash,
    ];
    for mismatch in mismatches {
        let probe = mismatch.probe(&granted, &foreign);
        assert!(
            probe.find(&store).is_none(),
            "{} must not match an exact approval",
            mismatch.name()
        );
    }
    assert_eq!(
        granted.find(&store).expect("still exact"),
        first,
        "every near miss leaves the recorded approval untouched"
    );
}

#[test]
fn a_plan_approval_never_authorizes_an_effect() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");

    let authored = plan(
        "task-plan-only",
        "plan-only",
        wire_unit(
            WorkUnitEffectClass::WorkspaceMutation,
            NetworkCeiling::PublicInternetClient,
        ),
    );
    let revision = store
        .publish_plan_revision(&authored, None)
        .expect("publish");
    let actor =
        PlanApprovalActor::new("actor-user", "session-1", PLAN_APPROVE_SCOPE).expect("plan actor");
    let approval = store
        .approve_plan_revision("task-plan-only", &revision, "plan-approval-1", actor)
        .expect("plan approval");
    assert_eq!(approval.actor.scope, PLAN_APPROVE_SCOPE);
    assert!(store
        .load_active_plan_approval("task-plan-only")
        .expect("load")
        .is_some());

    let granted = granted_from("task-plan-only", &authored, &revision);
    assert!(
        granted.find(&store).is_none(),
        "an approved plan carries no effect authority"
    );
    assert!(store
        .effect_approvals_for_task("task-plan-only")
        .expect("audit")
        .is_empty());
    assert_eq!(row_count(&path), 0);
}

#[test]
fn one_active_approval_per_work_unit_with_idempotent_resave() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let granted = grant(&store, "task-one-active", "one-active");
    let first = granted.record("effect-approval-first");

    store.save_effect_approval(first.clone()).expect("save");
    store
        .save_effect_approval(first.clone())
        .expect("an identical re-save is idempotent");
    assert_eq!(row_count(&path), 1);
    assert_eq!(granted.find(&store).expect("still active"), first);

    let mut rival = granted.record("effect-approval-rival");
    rival.payload_hash = format!("sha256:{}", "b".repeat(64));
    assert!(
        matches!(
            store.save_effect_approval(rival),
            Err(PlanStoreError::EffectActiveApprovalExists)
        ),
        "a different active approval for one work unit must conflict"
    );
    assert_eq!(row_count(&path), 1, "the conflict wrote nothing");
    assert_eq!(
        granted.find(&store).expect("first survives"),
        first,
        "a recorded approval is never overwritten"
    );

    let mut reused = granted.record("effect-approval-first");
    reused.work_unit_id = "unit-other".into();
    assert!(
        matches!(
            store.save_effect_approval(reused),
            Err(PlanStoreError::EffectApprovalIdConflict)
        ),
        "one approval id cannot identify two approvals"
    );
    assert_eq!(row_count(&path), 1);
}

#[test]
fn a_resave_by_a_different_actor_is_a_conflict_not_a_silent_replay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let granted = grant(&store, "task-actor", "actor");
    let first = granted.record("effect-approval-actor");
    store.save_effect_approval(first.clone()).expect("save");
    store
        .save_effect_approval(first.clone())
        .expect("a byte-identical replay stays idempotent");
    assert_eq!(row_count(&path), 1);

    let mut other_actor = first.clone();
    other_actor.actor_id = "actor-other".into();
    assert!(
        matches!(
            store.save_effect_approval(other_actor),
            Err(PlanStoreError::EffectApprovalIdConflict)
        ),
        "one approval id cannot carry two different actors"
    );

    let mut other_session = first.clone();
    other_session.session_id = "session-other".into();
    assert!(
        matches!(
            store.save_effect_approval(other_session),
            Err(PlanStoreError::EffectApprovalIdConflict)
        ),
        "one approval id cannot carry two different sessions"
    );

    let mut other_timestamp = first.clone();
    other_timestamp.created_at_ms = CREATED_AT_MS + 1;
    assert!(
        matches!(
            store.save_effect_approval(other_timestamp),
            Err(PlanStoreError::EffectApprovalIdConflict)
        ),
        "one approval id cannot carry two different creation times"
    );

    let mut other_scope = first.clone();
    other_scope.scope = PLAN_APPROVE_SCOPE.into();
    assert!(
        matches!(
            store.save_effect_approval(other_scope),
            Err(PlanStoreError::EffectApprovalInvalid)
        ),
        "the plan scope can never mint an effect approval"
    );

    let stored = granted.find(&store).expect("active");
    assert_eq!(stored, first, "the recorded attribution never drifts");
    assert_eq!(row_count(&path), 1);
}

#[test]
fn invalid_records_are_refused_before_any_write() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let granted = grant(&store, "task-invalid", "invalid");
    let valid = granted.record("effect-approval-valid");

    for field in InvalidField::ALL {
        let mut record = valid.clone();
        field.corrupt(&mut record);
        assert!(
            matches!(
                store.save_effect_approval(record),
                Err(PlanStoreError::EffectApprovalInvalid)
            ),
            "{} must be refused as invalid",
            field.name()
        );
    }
    assert_eq!(row_count(&path), 0, "no invalid record was written");
}

#[test]
fn sql_constraints_refuse_junk_columns_and_a_second_active_row() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    V1Store::open(&path).expect("migrate");

    for (name, raw) in [
        (
            "a junk effect class",
            RawApproval {
                effect_class: "everything",
                ..RawApproval::valid()
            },
        ),
        (
            "a junk network ceiling",
            RawApproval {
                network: "internet",
                ..RawApproval::valid()
            },
        ),
        (
            "the plan scope",
            RawApproval {
                scope: "plan.approve",
                ..RawApproval::valid()
            },
        ),
        (
            "a junk state",
            RawApproval {
                state: "revoked",
                ..RawApproval::valid()
            },
        ),
    ] {
        let error = raw
            .insert(&path)
            .err()
            .unwrap_or_else(|| panic!("{name} was accepted"));
        assert!(error.to_string().contains("CHECK"), "{name}: {error}");
    }

    RawApproval::valid()
        .insert(&path)
        .expect("a valid raw row is accepted");
    let duplicate = RawApproval {
        approval_id: "raw-2",
        ..RawApproval::valid()
    };
    let error = match duplicate.insert(&path) {
        Ok(_) => panic!("a second active row for one work unit was accepted"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("UNIQUE"),
        "one active per work unit: {error}"
    );

    let retired = RawApproval {
        approval_id: "raw-3",
        state: "superseded",
        ..RawApproval::valid()
    };
    retired
        .insert(&path)
        .expect("a superseded row may sit beside the active one");
    assert_eq!(row_count(&path), 2);
}

#[test]
fn supersede_retires_the_approval_for_future_runs_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let granted = grant(&store, "task-supersede", "supersede");
    let first = granted.record("effect-approval-first");
    store.save_effect_approval(first.clone()).expect("save");

    assert!(store
        .supersede_effect_approval("task-supersede", "unit-shell", CREATED_AT_MS + 5_000)
        .expect("supersede"));
    assert!(
        granted.find(&store).is_none(),
        "a retired approval can no longer authorize"
    );
    assert!(!store
        .supersede_effect_approval("task-supersede", "unit-shell", CREATED_AT_MS + 9_000)
        .expect("a second supersede finds nothing active"));

    let audit = store
        .effect_approvals_for_task("task-supersede")
        .expect("audit");
    assert_eq!(audit.len(), 1);
    let retired = &audit[0];
    assert_eq!(retired.state, EffectApprovalState::Superseded);
    assert_eq!(retired.superseded_at_ms, Some(CREATED_AT_MS + 5_000));
    assert_eq!(retired.created_at_ms, CREATED_AT_MS);
    assert_eq!(retired.approval_id, first.approval_id);
    assert_eq!(retired.payload_hash, first.payload_hash);
    assert_eq!(retired.actor_id, first.actor_id);

    let mut successor = granted.record("effect-approval-second");
    successor.created_at_ms = CREATED_AT_MS + 10_000;
    store
        .save_effect_approval(successor.clone())
        .expect("re-granting the same authority fits once the old row is retired");
    assert_eq!(row_count(&path), 2);
    assert_eq!(granted.find(&store).expect("successor"), successor);

    let audit = store
        .effect_approvals_for_task("task-supersede")
        .expect("audit");
    assert_eq!(
        audit
            .iter()
            .map(|record| record.approval_id.as_str())
            .collect::<Vec<_>>(),
        vec!["effect-approval-first", "effect-approval-second"],
        "the audit view keeps both records, oldest first"
    );
}

#[test]
fn the_effect_approval_migration_is_idempotent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let granted = grant(&store, "task-migration", "migration");
    store
        .save_effect_approval(granted.record("effect-approval-migration"))
        .expect("save");
    drop(store);
    drop(V1Store::open(&path).expect("second open"));

    let connection = Connection::open(&path).expect("inspect");
    let migrations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM v1_schema_migrations
             WHERE migration_id = 'work-unit-effect-approvals'",
            [],
            |row| row.get(0),
        )
        .expect("migration ledger");
    assert_eq!(migrations, 1, "the migration must not be applied twice");

    for index in [
        "idx_effect_approvals_one_active",
        "idx_effect_approvals_plan",
    ] {
        let present: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                rusqlite::params![index],
                |row| row.get(0),
            )
            .expect("index probe");
        assert_eq!(present, 1, "missing index {index}");
    }

    let reopened = V1Store::open(&path).expect("third open");
    assert_eq!(
        reopened
            .effect_approvals_for_task("task-migration")
            .expect("audit")
            .len(),
        1,
        "the approval survives every reopen"
    );
}
