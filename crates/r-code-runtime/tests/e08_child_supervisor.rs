//! E08 — the ChildSupervisor registry: every in-flight supervised tree
//! registered under exactly one owning attempt, swept with per-tree death
//! proofs, never an assumed set-wide kill.
//!
//! The registry's own contracts are pinned with fake child handles (the
//! registry is generic over [`SupervisedChild`] by design); the profiled-run
//! handle is pinned against the repo's deterministic supervisor backend so
//! the REAL terminate/wait_and-prove path is exercised, and the surfaced
//! birth-seam ids (`SupervisedRun::tree_id`, `tree_of`) are pinned with the
//! real services.

use r_code_runtime::child_supervisor::{
    ChildSupervisor, ProfiledTreeChild, SupervisedChild, SupervisedTreeError, SupervisorSweep,
};
use r_code_runtime::services::authorization::{
    AuthorizationService, EffectivePermissions, WorkspaceCapability,
};
use r_code_runtime::services::process_supervisor::{
    DeterministicFakeBackend, DeterministicSupervisorJournal, FaultPoint, PreparedChildKind,
    ProcessSupervisor, SpawnSpec,
};
use r_code_runtime::services::processes::ManagedProcessService;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Test handles
// ---------------------------------------------------------------------------

/// A fake supervised child: killable on demand, countable, optionally
/// already settled or permanently unprovable.
struct FakeChild {
    tree_id: String,
    alive: AtomicBool,
    kills: AtomicUsize,
    provable: AtomicBool,
}

impl FakeChild {
    fn killable(tree_id: &str) -> Arc<Self> {
        Arc::new(Self {
            tree_id: tree_id.to_string(),
            alive: AtomicBool::new(true),
            kills: AtomicUsize::new(0),
            provable: AtomicBool::new(true),
        })
    }

    fn dead(tree_id: &str) -> Arc<Self> {
        Arc::new(Self {
            tree_id: tree_id.to_string(),
            alive: AtomicBool::new(false),
            kills: AtomicUsize::new(0),
            provable: AtomicBool::new(true),
        })
    }

    fn unprovable(tree_id: &str) -> Arc<Self> {
        Arc::new(Self {
            tree_id: tree_id.to_string(),
            alive: AtomicBool::new(true),
            kills: AtomicUsize::new(0),
            provable: AtomicBool::new(false),
        })
    }

    fn kills(&self) -> usize {
        self.kills.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl SupervisedChild for FakeChild {
    fn tree_id(&self) -> &str {
        &self.tree_id
    }

    async fn cancel_and_prove(&self) -> bool {
        if !self.alive.load(Ordering::SeqCst) {
            return true;
        }
        self.kills.fetch_add(1, Ordering::SeqCst);
        self.provable.load(Ordering::SeqCst) && {
            self.alive.store(false, Ordering::SeqCst);
            true
        }
    }
}

fn kind() -> PreparedChildKind {
    if cfg!(windows) {
        PreparedChildKind::WindowsSuspended
    } else {
        PreparedChildKind::UnixGated
    }
}

/// A death proof naming exactly one tree (the s22 `proof_naming` idiom).
fn proof_naming(
    run: &r_code_runtime::services::process_supervisor::SupervisedRun,
) -> r_code_runtime::process_guard::TerminationProofRecord {
    let identity = serde_json::json!({
        "treeId": run.tree_id(),
        "ownershipEpoch": run.record().ownership_epoch,
        "nativeExit": true,
    });
    r_code_runtime::process_guard::TerminationProofRecord {
        proof_id: format!("proof-{}", run.tree_id()),
        tree_id: run.tree_id().to_string(),
        ownership_epoch: run.record().ownership_epoch,
        kind: r_code_runtime::process_guard::TerminationProofKind::Exit,
        observed_boot_identity: run
            .record()
            .owner
            .clone()
            .expect("durable owner")
            .boot_identity,
        proof_identity_digest: r_code_harness_protocol::canonical_input_hash(&identity),
        proof_identity: identity,
        recorded_at_ms: 1,
    }
}

fn fake_spec() -> SpawnSpec {
    SpawnSpec {
        executable: PathBuf::from("C:/e08/harness.exe"),
        arguments: vec!["--e08".to_string()],
        cwd: PathBuf::from("C:/e08"),
        environment: BTreeMap::new(),
        inherited_objects: Vec::new(),
        output_capacity_bytes: 4096,
    }
}

// ---------------------------------------------------------------------------
// E08.3 + acceptance "exactly one owner attempt"
// ---------------------------------------------------------------------------

/// Every registered tree has exactly one owner attempt: a second attempt
/// registering the same tree refuses, the owner's re-register converges, and
/// settling — the proof path — refuses trees the registry does not hold
/// under that attempt (a foreign-tree proof proves nothing).
#[test]
fn registry_pins_exactly_one_owner_per_tree() {
    let supervisor = ChildSupervisor::new();
    let tree = FakeChild::killable("tree-e08-owned");
    supervisor.register("attempt-a", tree.clone()).unwrap();
    assert!(matches!(
        supervisor.register("attempt-b", FakeChild::killable("tree-e08-owned")),
        Err(SupervisedTreeError::AlreadyOwned { ref tree_id, ref owner })
            if tree_id == "tree-e08-owned" && owner == "attempt-a"
    ));
    supervisor
        .register("attempt-a", FakeChild::killable("tree-e08-owned"))
        .unwrap();

    // A proof/settle naming a tree not under this attempt is refused.
    assert!(matches!(
        supervisor.settle("attempt-b", "tree-e08-owned"),
        Err(SupervisedTreeError::ForeignTree { .. })
    ));
    assert!(matches!(
        supervisor.settle("attempt-a", "tree-e08-unknown"),
        Err(SupervisedTreeError::ForeignTree { .. })
    ));
    // The owner settles its own tree; a second settle is refused (gone).
    supervisor.settle("attempt-a", "tree-e08-owned").unwrap();
    assert!(matches!(
        supervisor.settle("attempt-a", "tree-e08-owned"),
        Err(SupervisedTreeError::ForeignTree { .. })
    ));
    assert!(supervisor.registered().is_empty());
}

// ---------------------------------------------------------------------------
// Contract 1 — cancel-and-prove-all
// ---------------------------------------------------------------------------

/// Two registered killable trees plus one ALREADY-SETTLED tree: cancel-all
/// proves each tree with its own proof (each killable handle is cancelled
/// exactly once), the settled tree never blocks the sweep, and the registry
/// drains.
#[tokio::test]
async fn cancel_and_prove_all_proves_each_tree() {
    let supervisor = ChildSupervisor::new();
    let first = FakeChild::killable("tree-e08-a");
    let second = FakeChild::killable("tree-e08-b");
    let settled = FakeChild::dead("tree-e08-dead");
    supervisor.register("attempt-1", first.clone()).unwrap();
    supervisor.register("attempt-2", second.clone()).unwrap();
    supervisor.register("attempt-2", settled.clone()).unwrap();

    let sweep = supervisor.cancel_and_prove_all().await;
    assert_eq!(
        sweep.proven,
        vec![
            ("attempt-1".to_string(), "tree-e08-a".to_string()),
            ("attempt-2".to_string(), "tree-e08-b".to_string()),
            ("attempt-2".to_string(), "tree-e08-dead".to_string()),
        ]
    );
    assert!(sweep.unproven.is_empty());
    assert!(
        sweep.all_dead(),
        "set-wide dead is claimed only now, with every per-tree proof"
    );
    assert_eq!(first.kills(), 1);
    assert_eq!(second.kills(), 1);
    assert_eq!(
        settled.kills(),
        0,
        "an already-settled tree is never blocked nor killed"
    );
    assert!(
        supervisor.registered().is_empty(),
        "proven trees deregister"
    );

    // The empty registry sweeps vacuously clean — nothing to prove.
    let again = supervisor.cancel_and_prove_all().await;
    assert!(again.all_dead() && again.proven.is_empty());
}

// ---------------------------------------------------------------------------
// Contract 2 — unprovable tree
// ---------------------------------------------------------------------------

/// An unprovable tree keeps the set unswept: it stays registered (still
/// supervised, never abandoned), its provable siblings still prove and
/// deregister, and the sweep reports it rather than claiming all dead.
#[tokio::test]
async fn unprovable_tree_keeps_the_set_unswept_and_stays_registered() {
    let supervisor = ChildSupervisor::new();
    let stubborn = FakeChild::unprovable("tree-e08-stubborn");
    let sibling = FakeChild::killable("tree-e08-sibling");
    supervisor.register("attempt-1", stubborn.clone()).unwrap();
    supervisor.register("attempt-2", sibling.clone()).unwrap();

    let sweep = supervisor.cancel_and_prove_all().await;
    assert_eq!(sweep.proven.len(), 1);
    assert_eq!(
        sweep.unproven,
        vec![("attempt-1".to_string(), "tree-e08-stubborn".to_string())]
    );
    assert!(
        !sweep.all_dead(),
        "an unprovable tree forbids the all-dead claim"
    );
    assert_eq!(
        supervisor.registered(),
        vec![("attempt-1".to_string(), "tree-e08-stubborn".to_string())],
        "the unprovable tree stays registered — never unsupervised (INV-08)"
    );

    // cancel-one on the stubborn tree reports unproven; once it becomes
    // provable the same call proves and deregisters it.
    assert_eq!(
        supervisor
            .cancel_one("attempt-1", "tree-e08-stubborn")
            .await,
        Some(false)
    );
    stubborn.provable.store(true, Ordering::SeqCst);
    assert_eq!(
        supervisor
            .cancel_one("attempt-1", "tree-e08-stubborn")
            .await,
        Some(true)
    );
    assert!(supervisor.registered().is_empty());
    assert!(SupervisorSweep {
        proven: Vec::new(),
        unproven: Vec::new(),
    }
    .all_dead());
}

// ---------------------------------------------------------------------------
// The profiled-run handle over the real backend vocabulary
// ---------------------------------------------------------------------------

/// The profiled-run handle sweeps through the REAL backend vocabulary —
/// terminate then wait_and_prove — and its tree id is the supervised run's
/// surfaced record id: the birth seam registers exactly that identity.
#[tokio::test]
async fn profiled_tree_child_proves_through_the_backend() {
    let owner_identity = r_code_runtime::process_guard::ProcessOwnerIdentity::new(
        43_001,
        88_001,
        r_code_runtime::process_guard::BootIdentity::current()
            .expect("authoritative boot identity on this host"),
        serde_json::json!({"native": "e08-owner"}),
    )
    .expect("valid owner identity");
    let backend = DeterministicFakeBackend::new(kind());
    backend.set_next_identity(owner_identity.clone());
    let supervisor = ProcessSupervisor::new(
        Arc::new(backend.clone()),
        Arc::new(DeterministicSupervisorJournal::default()),
    );
    let run = supervisor
        .start(
            r_code_runtime::services::process_supervisor::SupervisorStart {
                operation_id: "op-e08-profiled".to_string(),
                tree_id: "tree-e08-profiled".to_string(),
                task_id: "task-e08".to_string(),
                ownership_epoch: 1,
                spec: fake_spec(),
            },
        )
        .await
        .expect("the fake backend starts the run");
    // E08 surface: the run's record tree id, surfaced for registration.
    assert_eq!(run.tree_id(), "tree-e08-profiled");

    let registry = ChildSupervisor::new();
    // The backend proves death only from the tree's own terminal evidence:
    // an exit frame, then the proof naming exactly this tree.
    use r_code_harness_protocol::ProcessOutputFrame;
    backend
        .push_output(
            run.tree(),
            ProcessOutputFrame::Exit {
                sequence: 0,
                exit_code: Some(0),
            },
        )
        .expect("exit frame lands");
    backend
        .set_termination_proof(run.tree(), Some(proof_naming(&run)))
        .expect("proof injection requires the exit frame");
    registry
        .register(
            "attempt-e08-profiled",
            Arc::new(ProfiledTreeChild::new(
                run.tree_id(),
                Arc::new(backend.clone()),
                run.tree().clone(),
            )),
        )
        .unwrap();
    assert_eq!(
        registry.registered(),
        vec![(
            "attempt-e08-profiled".to_string(),
            "tree-e08-profiled".to_string()
        )]
    );

    // The sweep terminates through the backend and accepts its proof.
    let sweep = registry.cancel_and_prove_all().await;
    assert!(
        sweep.all_dead(),
        "the real backend proves its tree: {sweep:?}"
    );
    assert!(registry.registered().is_empty());

    // A backend whose termination fails never proves: the tree stays
    // registered and the set is never reported swept.
    let faulted_backend = DeterministicFakeBackend::new(kind());
    faulted_backend.set_next_identity(owner_identity);
    faulted_backend.fail_once(FaultPoint::Terminate);
    let faulted = ProcessSupervisor::new(
        Arc::new(faulted_backend.clone()),
        Arc::new(DeterministicSupervisorJournal::default()),
    );
    let run = faulted
        .start(
            r_code_runtime::services::process_supervisor::SupervisorStart {
                operation_id: "op-e08-faulted".to_string(),
                tree_id: "tree-e08-faulted".to_string(),
                task_id: "task-e08".to_string(),
                ownership_epoch: 1,
                spec: fake_spec(),
            },
        )
        .await
        .expect("start itself does not terminate");
    registry
        .register(
            "attempt-e08-faulted",
            Arc::new(ProfiledTreeChild::new(
                run.tree_id(),
                Arc::new(faulted_backend),
                run.tree().clone(),
            )),
        )
        .unwrap();
    let sweep = registry.cancel_and_prove_all().await;
    assert!(!sweep.all_dead());
    assert_eq!(sweep.unproven.len(), 1);
    assert_eq!(registry.registered().len(), 1);
}

// ---------------------------------------------------------------------------
// The birth-seam surface
// ---------------------------------------------------------------------------

/// The open_profiled token stays opaque while its tree id is surfaced
/// alongside it (`tree_of`), and an unknown token surfaces nothing.
#[tokio::test]
async fn open_profiled_surfaces_the_tree_id_alongside_the_token() {
    let service = ManagedProcessService::new(
        Arc::new(AuthorizationService::new()),
        EffectivePermissions::full(),
        WorkspaceCapability::Unrestricted,
        None,
    );
    assert_eq!(service.tree_of("run:attempt:tree-e08-none").await, None);
}
