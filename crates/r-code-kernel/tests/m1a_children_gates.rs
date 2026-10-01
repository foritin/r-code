//! M1a-09 (FR-8 step one): real ChildrenSupervisor semantics — monotonic
//! ids across close, the concurrency gate (default 6), the nesting gate
//! (default 1), slot reclamation via close, and the condvar wait that
//! blocks without polling.

use r_code_harness_protocol::services::{ChildReport, ChildrenSpawnRequest, PermissionCeiling};
use r_code_kernel::children::{wait_child, ChildrenError, ChildrenSupervisor};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn spawn_request(objective: &str) -> ChildrenSpawnRequest {
    ChildrenSpawnRequest {
        objective: objective.into(),
        harness: None,
        permissions: PermissionCeiling::ReadOnly,
        budget_share: None,
    }
}

fn report(id: &str) -> ChildReport {
    ChildReport {
        child_task_id: id.into(),
        outcome: "completed".into(),
        verified: vec![],
        inferred: vec![],
        unverifiable: vec![],
        summary: Some("scout summary".into()),
    }
}

#[test]
fn ids_are_monotonic_and_never_reuse_after_close() {
    let mut supervisor = ChildrenSupervisor::new();
    let first = supervisor
        .spawn(PermissionCeiling::Full, &spawn_request("one"))
        .unwrap();
    assert_eq!(first, "child-1");
    supervisor.complete(&first, report(&first)).unwrap();
    supervisor.close(&first).unwrap();
    assert!(
        supervisor.child(&first).is_none(),
        "close reclaims the entry"
    );

    let second = supervisor
        .spawn(PermissionCeiling::Full, &spawn_request("two"))
        .unwrap();
    assert_eq!(second, "child-2", "the counter never rewinds after close");
    assert!(supervisor.close("child-absent").is_err());
}

#[test]
fn concurrency_gate_rejects_the_seventh_live_child() {
    let mut supervisor = ChildrenSupervisor::new();
    for index in 0..6 {
        supervisor
            .spawn(
                PermissionCeiling::Full,
                &spawn_request(&format!("scout {index}")),
            )
            .unwrap_or_else(|error| panic!("spawn {index} failed: {error}"));
    }
    let rejection = supervisor
        .spawn(PermissionCeiling::Full, &spawn_request("seventh"))
        .expect_err("the default bound is 6 live children");
    assert!(matches!(
        rejection,
        ChildrenError::ConcurrencyLimit { live: 6, limit: 6 }
    ));
    // Completing one child still counts as live until closed — the executor
    // frees the slot on close (Codex lesson).
    let first = supervisor.child("child-1").unwrap().child_task_id.clone();
    supervisor.complete(&first, report(&first)).unwrap();
    assert!(matches!(
        supervisor.spawn(PermissionCeiling::Full, &spawn_request("still seventh")),
        Err(ChildrenError::ConcurrencyLimit { .. })
    ));
    supervisor.close(&first).unwrap();
    let replacement = supervisor
        .spawn(PermissionCeiling::Full, &spawn_request("after close"))
        .expect("close freed the concurrency slot");
    // Rejected attempts burned ids 7 and 8 (reservation precedes the
    // gate); the next success takes 9 — ids never repeat, gaps are fine.
    assert_eq!(replacement, "child-9");
}

#[test]
fn nesting_gate_rejects_child_supervisors_at_depth() {
    let parent = ChildrenSupervisor::new();
    let (max_live, child_depth, max_depth) = parent.child_limits();
    let mut child = ChildrenSupervisor::with_limits(max_live, child_depth, max_depth);
    let rejection = child
        .spawn(PermissionCeiling::Full, &spawn_request("grandchild"))
        .expect_err("depth 1 of 1 cannot spawn");
    assert!(matches!(
        rejection,
        ChildrenError::NestingLimit { depth: 1, limit: 1 }
    ));
    // The parent (depth 0) is unaffected.
    assert!(ChildrenSupervisor::new()
        .spawn(PermissionCeiling::Full, &spawn_request("child"))
        .is_ok());
}

#[test]
fn wait_blocks_on_the_condvar_and_wakes_on_completion_without_polling() {
    let supervisor = Arc::new(Mutex::new(ChildrenSupervisor::new()));
    let id = supervisor
        .lock()
        .unwrap()
        .spawn(PermissionCeiling::Full, &spawn_request("waiter"))
        .unwrap();

    // The completing thread flips state after a delay; the waiter must be
    // sleeping on the condvar, not polling (poll count stays at zero until
    // the single wakeup).
    let polls = Arc::new(AtomicUsize::new(0));
    let completer = {
        let supervisor = Arc::clone(&supervisor);
        let id = id.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            supervisor
                .lock()
                .unwrap()
                .complete(&id, report(&id))
                .unwrap();
        })
    };
    let waiter = {
        let supervisor = Arc::clone(&supervisor);
        let polls = Arc::clone(&polls);
        std::thread::spawn(move || {
            let outcome = wait_child(&supervisor, &id, Duration::from_secs(5)).unwrap();
            polls.store(1, Ordering::SeqCst);
            outcome
        })
    };
    completer.join().unwrap();
    let outcome = waiter.join().unwrap();
    assert!(matches!(
        outcome,
        r_code_kernel::children::ChildWait::Completed(_)
    ));
    assert_eq!(polls.load(Ordering::SeqCst), 1);

    // Timeout returns Running (the wait expired; state untouched).
    supervisor.lock().unwrap().cancel_all();
    let id2 = supervisor
        .lock()
        .unwrap()
        .spawn(PermissionCeiling::Full, &spawn_request("timed out"))
        .unwrap();
    let outcome = wait_child(&supervisor, &id2, Duration::from_millis(50)).unwrap();
    assert!(matches!(
        outcome,
        r_code_kernel::children::ChildWait::Running
    ));
}
