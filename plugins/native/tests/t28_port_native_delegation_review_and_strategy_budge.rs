//! T28 — Native delegation, review and strategy budgets.
//!
//! Native-to-fixture child scenarios through the real host.children route:
//! evidence review, bounded retries and complete report collection; root
//! budgets stay host-owned.

use r_code_harness_protocol::rpc::{RpcId, RpcRequest};
use r_code_harness_protocol::services::{ChildReport, ChildrenSpawnRequest, PermissionCeiling};
use r_code_harness_protocol::RunIdentity;
use r_code_kernel::children::ChildrenSupervisor;
use r_code_kernel::ports::RunGuard;
use r_code_kernel::testing::{
    FakeModelService, FakeProcessService, FakeToolService, MemoryJournal,
};
use r_code_runtime::plugins::HostRouter;
use std::sync::Arc;

fn request(method: &str, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: method.into(),
        params: Some(params),
    }
}

fn spawn_request(harness: Option<&str>, permissions: PermissionCeiling) -> ChildrenSpawnRequest {
    ChildrenSpawnRequest {
        objective: "investigate flaky area".into(),
        harness: harness.map(str::to_string),
        permissions,
        budget_share: None,
    }
}

fn router_with_children(
    parent_ceiling: PermissionCeiling,
) -> (Arc<HostRouter>, Arc<std::sync::Mutex<ChildrenSupervisor>>) {
    let supervisor = Arc::new(std::sync::Mutex::new(ChildrenSupervisor::new()));
    let router = Arc::new(
        HostRouter::new(
            RunIdentity {
                task_id: "task-1".into(),
                branch_id: "branch-1".into(),
                run_id: "run-1".into(),
                attempt_id: "attempt-1".into(),
                generation: 1,
            },
            RunGuard::new("run-1", 1),
            vec![
                r_code_harness_protocol::HostService::ChildrenSpawn,
                r_code_harness_protocol::HostService::ChildrenWait,
                r_code_harness_protocol::HostService::ChildrenCancel,
            ],
            Arc::new(FakeToolService::default()),
            Arc::new(FakeModelService::default()),
            Arc::new(FakeProcessService::default()),
            Arc::new(MemoryJournal::new()),
            Arc::new(r_code_runtime::plugins::IgnoreQuestions),
        )
        .with_children(supervisor.clone(), parent_ceiling),
    );
    (router, supervisor)
}

#[tokio::test]
async fn native_delegates_to_fixture_children_and_collects_reports() {
    let (router, supervisor) = router_with_children(PermissionCeiling::Full);

    // Spawn a fixture child (the third-party harness by id).
    let spawned: serde_json::Value = router
        .handle_request(request(
            "host.children.spawn",
            serde_json::to_value(spawn_request(
                Some("repair-harness.example"),
                PermissionCeiling::ReadOnly,
            ))
            .unwrap(),
        ))
        .await
        .expect("spawn");
    let child = spawned["child_task_id"]
        .as_str()
        .expect("child id")
        .to_string();

    // Waiting before completion reports "not completed yet".
    let pending = router
        .handle_request(request(
            "host.children.wait",
            serde_json::json!({"child_task_id": child}),
        ))
        .await
        .expect_err("pending");
    assert!(pending.message.contains("not completed"), "{pending:?}");

    // The child completes with host-arbitrated facts.
    supervisor
        .lock()
        .unwrap()
        .complete(
            &child,
            ChildReport {
                child_task_id: child.clone(),
                outcome: "completed".into(),
                verified: vec!["check:flaky-fixed".into()],
                inferred: vec!["likely root cause: race in pool".into()],
                unverifiable: vec!["timing under CI load".into()],
                summary: Some("fixed".into()),
            },
        )
        .expect("complete");

    // The reviewer packet keeps the three classes distinct.
    let report: serde_json::Value = router
        .handle_request(request(
            "host.children.wait",
            serde_json::json!({"child_task_id": child}),
        ))
        .await
        .expect("wait");
    let reports = vec![serde_json::from_value::<ChildReport>(report).expect("report")];
    let packet = r_code_harness_native::orchestration::reviewer_packet(&reports);
    assert_eq!(packet["verified"], serde_json::json!(["check:flaky-fixed"]));
    assert_eq!(packet["inferred"].as_array().map(Vec::len), Some(1));
    assert_eq!(packet["unverifiable"].as_array().map(Vec::len), Some(1));

    // Per-child cancellation works and the supervisor settles.
    let second: serde_json::Value = router
        .handle_request(request(
            "host.children.spawn",
            serde_json::to_value(spawn_request(None, PermissionCeiling::ApprovalRequired)).unwrap(),
        ))
        .await
        .expect("spawn 2");
    let second_id = second["child_task_id"].as_str().unwrap().to_string();
    router
        .handle_request(request(
            "host.children.cancel",
            serde_json::json!({"child_task_id": second_id}),
        ))
        .await
        .expect("cancel");
    supervisor.lock().unwrap().can_finalize().expect("settled");
}

#[tokio::test]
async fn escalation_is_refused_and_bounded_retries_are_strategy_defaults() {
    let (router, _supervisor) = router_with_children(PermissionCeiling::ApprovalRequired);
    // A Full child under an ApprovalRequired parent: refused before spawn.
    let error = router
        .handle_request(request(
            "host.children.spawn",
            serde_json::to_value(spawn_request(None, PermissionCeiling::Full)).unwrap(),
        ))
        .await
        .expect_err("escalation");
    assert!(
        error.message.contains("exceeds the parent ceiling"),
        "{error:?}"
    );

    // Strategy defaults live in the plugin config: bounded retries.
    let default = r_code_harness_native::orchestration::DelegationRequest::default();
    assert_eq!(default.max_retries, 2);
    assert!(default.harness.is_none(), "same-harness by default");
    let config = r_code_harness_native::LoopConfig::default();
    assert_eq!(config.max_turns, 25, "native strategy defaults preserved");
}
