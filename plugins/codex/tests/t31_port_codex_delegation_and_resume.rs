//! T31 — Codex delegation and resume.
//!
//! Codex-to-Native delegation, per-child cancel, parent restart and
//! missing external-thread tests pass through generic plugin interfaces.

use r_code_harness_codex::delegation::{
    decide_resume, CodexCheckpoint, ResumeDecision, ThreadResumeRef,
};
use r_code_harness_protocol::rpc::{RpcId, RpcRequest};
use r_code_harness_protocol::services::{ChildReport, PermissionCeiling};
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

#[tokio::test]
async fn codex_delegates_to_native_children_through_generic_interfaces() {
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
        .with_children(supervisor.clone(), PermissionCeiling::Full),
    );

    // Codex → Native delegation.
    let spawned: serde_json::Value = router
        .handle_request(request(
            "host.children.spawn",
            serde_json::json!({
                "objective": "port the helper to async",
                "harness": "native.r-code",
                "permissions": "read-only"
            }),
        ))
        .await
        .expect("spawn");
    let child = spawned["child_task_id"].as_str().unwrap().to_string();

    // Per-child cancel works without touching the parent.
    router
        .handle_request(request(
            "host.children.cancel",
            serde_json::json!({"child_task_id": child}),
        ))
        .await
        .expect("cancel");
    supervisor.lock().unwrap().can_finalize().expect("settled");

    // Report collection still sees completed children only.
    let other = supervisor
        .lock()
        .unwrap()
        .spawn(
            PermissionCeiling::Full,
            &r_code_harness_protocol::services::ChildrenSpawnRequest {
                objective: "second".into(),
                harness: None,
                permissions: PermissionCeiling::ReadOnly,
                budget_share: None,
            },
        )
        .expect("second");
    supervisor
        .lock()
        .unwrap()
        .complete(
            &other,
            ChildReport {
                child_task_id: other.clone(),
                outcome: "completed".into(),
                verified: vec![],
                inferred: vec!["partial analysis".into()],
                unverifiable: vec![],
                summary: None,
            },
        )
        .expect("complete");
    let reports = supervisor.lock().unwrap().collect_reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].inferred, vec!["partial analysis".to_string()]);
}

#[test]
fn parent_restart_requires_the_same_package_and_config_identity() {
    let checkpoint = CodexCheckpoint {
        resume: Some(ThreadResumeRef {
            harness_id: "codex.r-code".into(),
            package_digest: "sha-pkg-1".into(),
            config_hash: "cfg-1".into(),
            thread_id: "thread_abc".into(),
        }),
        consumed_input_seq: 4,
    };

    // Same identity + available thread → resume.
    assert_eq!(
        decide_resume(&checkpoint, "codex.r-code", "sha-pkg-1", "cfg-1", true),
        ResumeDecision::Resume {
            thread_id: "thread_abc".into()
        }
    );

    // Upgraded package bytes: restart (checkpoints are identity-bound).
    assert!(matches!(
        decide_resume(&checkpoint, "codex.r-code", "sha-pkg-2", "cfg-1", true),
        ResumeDecision::RestartRequired { .. }
    ));
    // Config change: restart.
    assert!(matches!(
        decide_resume(&checkpoint, "codex.r-code", "sha-pkg-1", "cfg-2", true),
        ResumeDecision::RestartRequired { .. }
    ));
    // Different harness id: restart.
    assert!(matches!(
        decide_resume(&checkpoint, "native.r-code", "sha-pkg-1", "cfg-1", true),
        ResumeDecision::RestartRequired { .. }
    ));
    // Missing external thread: a *visible* restart-required outcome.
    match decide_resume(&checkpoint, "codex.r-code", "sha-pkg-1", "cfg-1", false) {
        ResumeDecision::RestartRequired { reason } => {
            assert!(reason.contains("thread_abc unavailable"), "{reason}");
        }
        other => panic!("expected restart, got {other:?}"),
    }
    // No thread reference at all: restart with a clear reason.
    let empty = CodexCheckpoint {
        resume: None,
        consumed_input_seq: 0,
    };
    match decide_resume(&empty, "codex.r-code", "sha-pkg-1", "cfg-1", true) {
        ResumeDecision::RestartRequired { reason } => {
            assert!(reason.contains("no thread reference"), "{reason}");
        }
        other => panic!("expected restart, got {other:?}"),
    }
}
