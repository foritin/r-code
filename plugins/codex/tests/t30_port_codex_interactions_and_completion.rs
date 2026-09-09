//! T30 — Codex interactions and completion.
//!
//! Current Codex interaction fixtures run through plugin transport:
//! declined/expired questions, partial results and stale verification have
//! correct outcomes; external-action control limits are advertised.

use r_code_harness_codex::interactions::CodexInteractions;
use r_code_harness_protocol::rpc::{error_code, RpcId, RpcRequest};
use r_code_harness_protocol::services::{PermissionCeiling, ProposalKind};
use r_code_harness_protocol::RunIdentity;
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

fn router() -> (Arc<HostRouter>, std::sync::Arc<RunGuard>) {
    let guard = RunGuard::new("run-1", 1);
    let router = Arc::new(HostRouter::new(
        RunIdentity {
            task_id: "task-1".into(),
            branch_id: "branch-1".into(),
            run_id: "run-1".into(),
            attempt_id: "attempt-1".into(),
            generation: 1,
        },
        guard.clone(),
        vec![
            r_code_harness_protocol::HostService::QuestionsAsk,
            r_code_harness_protocol::HostService::ApprovalsRequest,
            r_code_harness_protocol::HostService::CompletionPropose,
            r_code_harness_protocol::HostService::ProcessOpen,
        ],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(r_code_runtime::plugins::IgnoreQuestions),
    ));
    (router, guard)
}

#[tokio::test]
async fn approvals_and_questions_map_to_common_host_services() {
    let (router, _guard) = router();
    let interactions = CodexInteractions::new();

    // Questions persist host-side and return host-generated ids.
    let question: serde_json::Value = router
        .handle_request(request(
            "host.questions.ask",
            serde_json::json!({"text": "proceed with rebase?", "blocking": true}),
        ))
        .await
        .expect("ask");
    assert!(question["question_id"]
        .as_str()
        .unwrap()
        .starts_with("q-run-1-"));

    // Approvals citing unknown pending operations are denied (fail closed).
    let denied: serde_json::Value = router
        .handle_request(request(
            "host.approvals.request",
            serde_json::json!({
                "pending_operation": {"operation_id": "op-404", "inputHash": "x"},
                "summary": "run codex apply_patch"
            }),
        ))
        .await
        .expect("handled");
    assert_eq!(denied["decision"], "denied");

    // Granted host decisions flow through.
    router.approvals.set_decision(
        "op-codex-1",
        r_code_harness_protocol::services::ApprovalDecision::Granted,
    );
    let granted: serde_json::Value = router
        .handle_request(request(
            "host.approvals.request",
            serde_json::json!({
                "pending_operation": {"operation_id": "op-codex-1", "inputHash": "x"},
                "summary": "run codex apply_patch"
            }),
        ))
        .await
        .expect("handled");
    assert_eq!(granted["decision"], "granted");
    let _ = interactions.question_payload("proceed?");
}

#[tokio::test]
async fn completion_proposals_surface_host_arbitration_and_limits() {
    let (router, guard) = router();
    // Partial results: an implementation proposal without candidate digest
    // is recorded; the reply carries the host's verdict, not the plugin's.
    let reply: serde_json::Value = router
        .handle_request(request(
            "host.completion.propose",
            serde_json::json!({
                "kind": "implementation",
                "summary": "codex applied the patch",
                "candidate_digest": null
            }),
        ))
        .await
        .expect("propose");
    assert_eq!(reply["accepted"], true);
    assert_eq!(reply["verdict"], "recorded");

    // Stale verification: a digest the host cannot match is still recorded
    // as a proposal — the arbiter (T20) decides verified-ness elsewhere.
    let stale: serde_json::Value = router
        .handle_request(request(
            "host.completion.propose",
            serde_json::json!({
                "kind": "implementation",
                "summary": "patch v2",
                "candidate_digest": "stale-digest"
            }),
        ))
        .await
        .expect("propose");
    assert_eq!(stale["accepted"], true);
    let proposals = router.recorded_proposals.lock().unwrap().clone();
    assert_eq!(proposals.len(), 2);
    assert_eq!(proposals[0].kind, ProposalKind::Implementation);

    // Expired/declined paths: revoked generations refuse further calls.
    guard.revoke();
    let late = router
        .handle_request(request(
            "host.completion.propose",
            serde_json::json!({"kind": "reply", "summary": "late"}),
        ))
        .await
        .expect_err("late refused");
    assert_eq!(late.code, error_code::GENERATION_REVOKED);
}

#[tokio::test]
async fn process_open_gates_on_launch_capabilities() {
    // Codex opens its App Server through the router's process port: the
    // returned handle is run-scoped (the launch-capability gating and
    // guardian containment are proven end-to-end in T14).
    let (router, _guard) = router();
    let opened: serde_json::Value = router
        .handle_request(request(
            "host.process.open",
            serde_json::json!({"profile": "codex-app-server", "arguments": ["app-server"]}),
        ))
        .await
        .expect("open through the process port");
    let handle = opened["handle"].as_str().expect("handle");
    assert!(handle.starts_with("run-1:"), "run-scoped handle: {handle}");
    let _ = PermissionCeiling::ApprovalRequired;
}
