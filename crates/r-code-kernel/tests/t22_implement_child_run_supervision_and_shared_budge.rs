//! T22 — child-run supervision and shared budgets.
//!
//! Cross-Harness child fixtures prove permission ceilings, parent
//! cancellation, no orphan runs and budget exhaustion with usable partial
//! results.

use r_code_harness_protocol::services::{ChildReport, ChildrenSpawnRequest, PermissionCeiling};
use r_code_kernel::budget::{BudgetError, BudgetPool, BudgetReservation};
use r_code_kernel::children::*;

fn spawn_request(harness: Option<&str>, permissions: PermissionCeiling) -> ChildrenSpawnRequest {
    ChildrenSpawnRequest {
        objective: "investigate the flaky area".into(),
        harness: harness.map(str::to_string),
        permissions,
        budget_share: None,
    }
}

fn report(child: &str, verified: &[&str]) -> ChildReport {
    ChildReport {
        child_task_id: child.into(),
        outcome: "completed".into(),
        verified: verified.iter().map(|item| item.to_string()).collect(),
        inferred: vec![],
        unverifiable: vec![],
        summary: Some(format!("summary for {child}")),
    }
}

#[test]
fn children_may_use_other_harnesses_within_permission_ceilings() {
    let mut supervisor = ChildrenSupervisor::new();
    // Parent ceiling Full: any child ceiling fits.
    let native_child = supervisor
        .spawn(
            PermissionCeiling::Full,
            &spawn_request(Some("native"), PermissionCeiling::ReadOnly),
        )
        .expect("native child");
    let codex_child = supervisor
        .spawn(
            PermissionCeiling::Full,
            &spawn_request(Some("codex.harness"), PermissionCeiling::ApprovalRequired),
        )
        .expect("codex child");
    let third_party = supervisor
        .spawn(
            PermissionCeiling::Full,
            &spawn_request(Some("repair-harness.example"), PermissionCeiling::Full),
        )
        .expect("third-party child");
    assert_ne!(native_child, codex_child);
    assert_ne!(native_child, third_party);

    // Parent ceiling ApprovalRequired: a Full child is refused.
    let mut restricted = ChildrenSupervisor::new();
    let error = restricted
        .spawn(
            PermissionCeiling::ApprovalRequired,
            &spawn_request(None, PermissionCeiling::Full),
        )
        .expect_err("escalation refused");
    assert!(matches!(error, ChildrenError::PermissionExceeded { .. }));
    // But weaker ceilings fit.
    restricted
        .spawn(
            PermissionCeiling::ApprovalRequired,
            &spawn_request(None, PermissionCeiling::ReadOnly),
        )
        .expect("read-only child fits");
}

#[test]
fn parents_cannot_finalize_with_live_children_and_no_orphans_remain() {
    let mut supervisor = ChildrenSupervisor::new();
    let a = supervisor
        .spawn(
            PermissionCeiling::Full,
            &spawn_request(Some("native"), PermissionCeiling::ReadOnly),
        )
        .expect("a");
    let b = supervisor
        .spawn(
            PermissionCeiling::Full,
            &spawn_request(Some("native"), PermissionCeiling::ReadOnly),
        )
        .expect("b");

    // Live children block finalization.
    let error = supervisor.can_finalize().expect_err("blocked");
    assert!(matches!(error, ChildrenError::Uncollected(list) if list.len() == 2));

    // Complete one, cancel the other: both settle, no orphans.
    supervisor
        .complete(&a, report(&a, &["check:unit"]))
        .expect("complete a");
    supervisor.cancel_child(&b).expect("cancel b");
    supervisor.can_finalize().expect("can finalize");
    assert!(supervisor.live_children().is_empty());

    // Reports collect with verified facts distinct from prose.
    let reports = supervisor.collect_reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].verified, vec!["check:unit".to_string()]);
    assert!(reports[0].summary.as_deref().unwrap().contains("summary"));
}

#[test]
fn parent_cancellation_cascades_to_all_children() {
    let mut supervisor = ChildrenSupervisor::new();
    let a = supervisor
        .spawn(
            PermissionCeiling::Full,
            &spawn_request(None, PermissionCeiling::ReadOnly),
        )
        .expect("a");
    let b = supervisor
        .spawn(
            PermissionCeiling::Full,
            &spawn_request(None, PermissionCeiling::ReadOnly),
        )
        .expect("b");
    supervisor.complete(&a, report(&a, &[])).expect("a done");

    // Cancelling the parent cancels the live child; the settled one keeps
    // its report (usable partial results).
    supervisor.cancel_all();
    assert!(matches!(
        supervisor.child(&b).unwrap().state,
        ChildState::Cancelled
    ));
    assert!(matches!(
        supervisor.child(&a).unwrap().state,
        ChildState::Completed(_)
    ));
    supervisor.can_finalize().expect("settled after cascade");
    let reports = supervisor.collect_reports();
    assert_eq!(reports.len(), 1, "partial results preserved");
}

#[test]
fn budget_exhaustion_is_terminal_but_preserves_partial_results() {
    let mut tree = SupervisedTree::new(10);
    let a = tree
        .supervisor
        .spawn(
            PermissionCeiling::Full,
            &spawn_request(None, PermissionCeiling::ReadOnly),
        )
        .expect("a");
    let b = tree
        .supervisor
        .spawn(
            PermissionCeiling::Full,
            &spawn_request(None, PermissionCeiling::ReadOnly),
        )
        .expect("b");

    // Children draw from the shared root budget.
    tree.budget.consume(6).expect("child a work");
    tree.budget.consume(4).expect("child b work");
    assert!(tree.budget.is_exhausted());
    let error = tree.budget.consume(1).expect_err("exhausted");
    assert!(matches!(
        error,
        BudgetError::Exhausted {
            total: 10,
            consumed: 10,
            requested: 1
        }
    ));

    // Completed work remains collectable: partial results survive the
    // budget stop.
    tree.supervisor
        .complete(&a, report(&a, &["check:half"]))
        .expect("a done");
    tree.supervisor
        .cancel_child(&b)
        .expect("b cancelled by budget stop");
    tree.supervisor.can_finalize().expect("finalize after stop");
    let reports = tree.supervisor.collect_reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].verified, vec!["check:half".to_string()]);
}

#[test]
fn reservations_refund_unspent_budget() {
    let mut pool = BudgetPool::new(10);
    {
        let mut reservation = BudgetReservation::reserve(&mut pool, 6);
        reservation.spend(2).expect("spend");
        // Dropping refunds the unspent 4.
    }
    assert_eq!(pool.consumed, 2);
    assert_eq!(pool.remaining(), 8);

    // A reservation cannot overspend its own grant.
    let mut pool = BudgetPool::new(4);
    let mut reservation = BudgetReservation::reserve(&mut pool, 4);
    assert!(reservation.spend(5).is_err());
}
