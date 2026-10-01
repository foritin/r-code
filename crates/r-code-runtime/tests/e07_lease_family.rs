//! E07 — durable lease families: all-or-nothing acquisition, family release
//! settling the attempt record together with exactly its member leases, and
//! whole-family restart reconciliation, proven at the store seam the
//! dispatcher drives (`V1Store` over a real database, the m02 fixture's
//! idioms).
//!
//! The three contract scenarios plus the three acceptance criteria: a family
//! never partially exists; fencing derives from the workspace lease epoch the
//! members already carry (no third epoch currency); no lease outlives its
//! family's durable settle or quarantine.

use r_code_store::v1::mutations::{LeaseFamilyOutcome, LeaseFamilyState};
use r_code_store::v1::{
    LeaseRequest, MutationError, MutationFile, MutationOperation, MutationState, V1Store,
    WorkUnitAttemptPhase, WorkUnitAttemptSeed,
};
use std::path::PathBuf;
use std::sync::Arc;

const WORKSPACE: &str = "ws-e07";

fn member(attempt_id: &str, operation: &str, write_paths: &[&str]) -> LeaseRequest {
    LeaseRequest {
        workspace_key: WORKSPACE.to_string(),
        operation_id: operation.to_string(),
        owner_id: attempt_id.to_string(),
        read_paths: Vec::new(),
        write_paths: write_paths.iter().map(|path| (*path).to_string()).collect(),
        repo_exclusive: false,
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    store: Arc<V1Store>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let database_path: PathBuf = temp.path().join("store.db");
        let store = Arc::new(V1Store::open(&database_path).expect("store opens"));
        Self { _temp: temp, store }
    }

    /// One prepared attempt row — the family's key.
    fn prepared_attempt(&self, attempt_id: &str, task_id: &str, work_unit_id: &str) {
        self.store
            .prepare_work_unit_attempt(&WorkUnitAttemptSeed {
                attempt_id: attempt_id.to_string(),
                task_id: task_id.to_string(),
                plan_revision: "rev-e07".to_string(),
                work_unit_id: work_unit_id.to_string(),
                content_sha256: format!("sha256:{attempt_id}-content").replace(' ', "-"),
            })
            .expect("attempt row prepares");
    }

    fn active_member_count(&self, attempt_id: &str) -> usize {
        self.store
            .load_lease_family(attempt_id)
            .expect("family loads")
            .map(|family| family.leases.iter().filter(|lease| lease.active).count())
            .unwrap_or(0)
    }
}

/// A foreign active lease on `path` held by another owner.
fn foreign_lease(store: &V1Store, owner: &str, path: &str) -> r_code_store::v1::LeaseGrant {
    store
        .acquire_lease(LeaseRequest {
            workspace_key: WORKSPACE.to_string(),
            operation_id: format!("lease:{owner}:{path}").replace('/', "-"),
            owner_id: owner.to_string(),
            read_paths: Vec::new(),
            write_paths: vec![path.to_string()],
            repo_exclusive: false,
        })
        .expect("foreign lease acquires")
}

// ---------------------------------------------------------------------------
// Contract 1 + acceptance "a family never partially exists"
// ---------------------------------------------------------------------------

/// A family whose second member lease conflicts acquires NOTHING: no family
/// row exists, the first member lease is not held, and — once the foreign
/// holder releases — the identical acquire succeeds whole.
#[test]
fn family_acquire_is_all_or_nothing() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let attempt = "attempt-e07-aon";
    let foreign = foreign_lease(store, "attempt-foreign", "shared-zone");

    let error = store
        .acquire_lease_family(
            attempt,
            &[
                member(attempt, "op-first", &["zone-alpha"]),
                member(attempt, "op-second", &["shared-zone"]),
            ],
        )
        .expect_err("the second member's conflict refuses the whole family");
    assert!(
        matches!(error, MutationError::LeaseConflict { ref holder } if *holder == foreign.lease_id),
        "conflict must name the foreign member lease: {error:?}"
    );
    assert!(
        store.load_lease_family(attempt).unwrap().is_none(),
        "a family never partially exists"
    );
    let active = store.active_leases(WORKSPACE).unwrap();
    assert_eq!(active.len(), 1, "the first member lease is not held");
    assert_eq!(active[0].request.owner_id, "attempt-foreign");

    // Once the conflicting holder releases, the identical acquire succeeds
    // whole: both members active under one family.
    assert!(store
        .release_lease(&foreign.lease_id, "attempt-foreign", foreign.fencing_epoch)
        .unwrap());
    let family = store
        .acquire_lease_family(
            attempt,
            &[
                member(attempt, "op-first", &["zone-alpha"]),
                member(attempt, "op-second", &["shared-zone"]),
            ],
        )
        .expect("the uncontended family acquires whole");
    assert_eq!(family.state, LeaseFamilyState::Active);
    assert_eq!(family.leases.len(), 2);
    assert_eq!(fixture.active_member_count(attempt), 2);
}

// ---------------------------------------------------------------------------
// Contract 2 — disjoint families coexist; refusal is by member lease
// ---------------------------------------------------------------------------

/// Two tasks' families over disjoint paths coexist; another task's attempt
/// on a held path is refused by the member lease that holds it — never by
/// family cross-talk — and both original families stay untouched.
#[test]
fn families_coexist_and_refuse_by_member_lease() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let attempt_a = "attempt-e07-task-a";
    let attempt_b = "attempt-e07-task-b";
    let attempt_c = "attempt-e07-task-c";
    let family_a = store
        .acquire_lease_family(attempt_a, &[member(attempt_a, "op-a", &["scope-a"])])
        .expect("task A family acquires");
    store
        .acquire_lease_family(attempt_b, &[member(attempt_b, "op-b", &["scope-b"])])
        .expect("task B family acquires over disjoint paths");

    // Task C's attempt on task A's path: the refusal names A's MEMBER lease.
    let error = store
        .acquire_lease_family(attempt_c, &[member(attempt_c, "op-c", &["scope-a"])])
        .expect_err("a held path refuses the other task's attempt");
    let holder = family_a.leases[0].lease_id.clone();
    assert!(
        matches!(error, MutationError::LeaseConflict { holder: ref held } if *held == holder),
        "refusal must be by the member lease, not family cross-talk: {error:?}"
    );

    // Neither original family was touched by the refused attempt.
    assert_eq!(
        store.load_lease_family(attempt_a).unwrap().unwrap().state,
        LeaseFamilyState::Active
    );
    assert_eq!(
        store.load_lease_family(attempt_b).unwrap().unwrap().state,
        LeaseFamilyState::Active
    );
    assert_eq!(fixture.active_member_count(attempt_a), 1);
    assert_eq!(fixture.active_member_count(attempt_b), 1);
}

// ---------------------------------------------------------------------------
// Contract 3 + acceptance "no lease outlives settle or quarantine"
// ---------------------------------------------------------------------------

/// Crash between family acquire and settle: restart reconciliation resolves
/// every active family from its OWN attempt evidence — the settled attempt's
/// family releases, the incomplete ones quarantine with their members
/// fenced — and families of different tasks never fence each other.
#[test]
fn restart_reconciliation_resolves_whole_families() {
    let fixture = Fixture::new();
    let store = &fixture.store;

    // Family X: two members, attempt still prepared (crash mid-flight).
    let attempt_x = "attempt-e07-crash-x";
    fixture.prepared_attempt(attempt_x, "task-x", "unit-x");
    let family_x = store
        .acquire_lease_family(
            attempt_x,
            &[
                member(attempt_x, "op-x1", &["scope-x1"]),
                member(attempt_x, "op-x2", &["scope-x2"]),
            ],
        )
        .expect("family X acquires");
    // A journaled effect under X's first member: the fence must refuse it.
    let fenced_operation = MutationOperation {
        operation_id: format!("{attempt_x}-effect"),
        workspace_key: WORKSPACE.to_string(),
        lease_id: family_x.leases[0].lease_id.clone(),
        owner_id: attempt_x.to_string(),
        fencing_epoch: family_x.leases[0].fencing_epoch,
        input_hash: "input-hash-x".to_string(),
        state: MutationState::Prepared,
        files: vec![MutationFile {
            logical_path: "scope-x1/file.txt".to_string(),
            before_sha256: None,
            after_sha256: None,
            before_cas_ref: None,
            after_cas_ref: None,
        }],
    };
    store
        .prepare_operation(&fenced_operation)
        .expect("the effect journals while the lease is active");

    // Family Y: crashed AFTER its attempt settled but before the family
    // released — reconciliation must release it, not quarantine it.
    let attempt_y = "attempt-e07-crash-y";
    fixture.prepared_attempt(attempt_y, "task-y", "unit-y");
    store
        .acquire_lease_family(attempt_y, &[member(attempt_y, "op-y", &["scope-y"])])
        .expect("family Y acquires");
    store
        .settle_work_unit_attempt(attempt_y, true)
        .expect("Y's attempt settled before the crash");

    // Family Z: one member, attempt incomplete — quarantines like X.
    let attempt_z = "attempt-e07-crash-z";
    fixture.prepared_attempt(attempt_z, "task-z", "unit-z");
    store
        .acquire_lease_family(attempt_z, &[member(attempt_z, "op-z", &["scope-z"])])
        .expect("family Z acquires");

    let reconciled = store.reconcile_lease_families().expect("reconcile runs");
    assert_eq!(
        reconciled.len(),
        3,
        "every active family resolves: {reconciled:?}"
    );
    let outcome = |attempt: &str| {
        reconciled
            .iter()
            .find(|row| row.attempt_id == attempt)
            .map(|row| row.outcome)
            .expect("reconciliation covers the family")
    };
    assert_eq!(outcome(attempt_x), LeaseFamilyOutcome::Quarantined);
    assert_eq!(outcome(attempt_y), LeaseFamilyOutcome::Released);
    assert_eq!(outcome(attempt_z), LeaseFamilyOutcome::Quarantined);

    // Whole-family resolution: no lease of any family survives, and each
    // family's terminal state follows its own attempt — Y RELEASED even
    // though the sibling families of other tasks quarantined.
    assert!(store.active_leases(WORKSPACE).unwrap().is_empty());
    let terminal = |attempt: &str| {
        store
            .load_lease_family(attempt)
            .unwrap()
            .expect("family row survives")
            .state
    };
    assert_eq!(terminal(attempt_x), LeaseFamilyState::Quarantined);
    assert_eq!(terminal(attempt_y), LeaseFamilyState::Released);
    assert_eq!(terminal(attempt_z), LeaseFamilyState::Quarantined);
    assert!(
        terminal(attempt_y) != terminal(attempt_x),
        "different tasks' families never fence each other's outcomes"
    );

    // The fence is real: the quarantined member's late writer is refused.
    assert!(matches!(
        store.prepare_operation(&fenced_operation),
        Err(MutationError::LeaseInactive)
    ));
    // Reconciliation is idempotent: nothing active remains to resolve.
    assert!(store.reconcile_lease_families().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// E07.2 — family release settles attempt + members; stale attempts refuse
// ---------------------------------------------------------------------------

/// A family release releases exactly its member leases and settles the
/// attempt record in the same durable step; the release is exactly-once, and
/// a stale attempt can neither release nor extend.
#[test]
fn family_release_settles_attempt_and_members_exactly_once() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let attempt = "attempt-e07-release";
    fixture.prepared_attempt(attempt, "task-r", "unit-r");
    store
        .acquire_lease_family(
            attempt,
            &[
                member(attempt, "op-r1", &["scope-r1"]),
                member(attempt, "op-r2", &["scope-r2"]),
            ],
        )
        .expect("family acquires");

    // Wrong owner refuses before anything settles.
    assert!(matches!(
        store.release_lease_family(attempt, "someone-else", true),
        Err(MutationError::StaleLease)
    ));

    assert!(store.release_lease_family(attempt, attempt, true).unwrap());
    let attempt_row = store
        .load_work_unit_attempt(attempt)
        .unwrap()
        .expect("attempt row exists");
    assert_eq!(attempt_row.phase, WorkUnitAttemptPhase::SettledCompleted);
    let family = store
        .load_lease_family(attempt)
        .unwrap()
        .expect("family row survives");
    assert_eq!(family.state, LeaseFamilyState::Released);
    assert!(family.settled_at_ms.is_some());
    assert!(family.leases.iter().all(|lease| !lease.active));
    assert!(store.active_leases(WORKSPACE).unwrap().is_empty());

    // Exactly once: a second release is a no-op, and the settled attempt
    // cannot extend its family back into existence.
    assert!(!store.release_lease_family(attempt, attempt, true).unwrap());
    assert!(matches!(
        store.acquire_lease_family(attempt, &[member(attempt, "op-r1", &["scope-r1"])]),
        Err(MutationError::StaleAttempt(_))
    ));

    // A family whose attempt settled OUTSIDE the family refuses release —
    // the stale attempt vocabulary.
    let attempt_w = "attempt-e07-stale";
    fixture.prepared_attempt(attempt_w, "task-w", "unit-w");
    store
        .acquire_lease_family(attempt_w, &[member(attempt_w, "op-w", &["scope-w"])])
        .expect("family W acquires");
    store
        .settle_work_unit_attempt(attempt_w, false)
        .expect("W settles directly");
    assert!(matches!(
        store.release_lease_family(attempt_w, attempt_w, false),
        Err(MutationError::StaleAttempt(_))
    ));
}

// ---------------------------------------------------------------------------
// Acceptance — fencing reuses the workspace lease epoch
// ---------------------------------------------------------------------------

/// The family carries no epoch of its own: member epochs come from the one
/// workspace lease counter, consecutively, and the counter continues with
/// the next plain lease after the family — no third epoch currency exists.
#[test]
fn family_fencing_derives_from_the_workspace_lease_epoch() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let attempt = "attempt-e07-epoch";
    let family = store
        .acquire_lease_family(
            attempt,
            &[
                member(attempt, "op-e1", &["scope-e1"]),
                member(attempt, "op-e2", &["scope-e2"]),
            ],
        )
        .expect("family acquires");
    let epochs: Vec<u64> = family
        .leases
        .iter()
        .map(|lease| lease.fencing_epoch)
        .collect();
    assert_eq!(epochs.len(), 2);
    assert_eq!(
        epochs[1],
        epochs[0] + 1,
        "members fence with consecutive epochs from the one counter: {epochs:?}"
    );

    // The SAME counter serves the next plain lease: had the family minted
    // its own currency, this epoch would skip.
    let next = foreign_lease(store, "attempt-foreign-e", "scope-e3");
    assert_eq!(
        next.fencing_epoch,
        epochs[1] + 1,
        "the workspace lease epoch counter continues unchanged"
    );
}

// ---------------------------------------------------------------------------
// E07.1 — replay converges, divergent members refuse
// ---------------------------------------------------------------------------

/// A byte-identical family acquire converges on the existing family (same
/// member lease ids); a divergent member set under the same attempt refuses.
#[test]
fn family_replay_converges_and_divergent_members_refuse() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let attempt = "attempt-e07-replay";
    let first = store
        .acquire_lease_family(
            attempt,
            &[
                member(attempt, "op-p1", &["scope-p1"]),
                member(attempt, "op-p2", &["scope-p2"]),
            ],
        )
        .expect("family acquires");

    let replay = store
        .acquire_lease_family(
            attempt,
            &[
                member(attempt, "op-p2", &["scope-p2"]),
                member(attempt, "op-p1", &["scope-p1"]),
            ],
        )
        .expect("identical replay converges regardless of member order");
    let mut first_ids: Vec<&str> = first.leases.iter().map(|l| l.lease_id.as_str()).collect();
    first_ids.sort();
    let mut replay_ids: Vec<&str> = replay.leases.iter().map(|l| l.lease_id.as_str()).collect();
    replay_ids.sort();
    assert_eq!(first_ids, replay_ids, "replay keeps the same members");

    assert!(matches!(
        store.acquire_lease_family(
            attempt,
            &[
                member(attempt, "op-p1", &["scope-p1"]),
                member(attempt, "op-p3", &["scope-p3"]),
            ]
        ),
        Err(MutationError::OperationConflict)
    ));
    assert_eq!(fixture.active_member_count(attempt), 2);
}
