//! Plugin conformance and failure suite (T39).
//!
//! A deliberately broken Harness fails for false completion, stale
//! evidence, cross-run handles, duplicate side effects or hanging
//! cancellation. Deterministic fixtures make this a hard CI gate; the
//! companion binary `harness-conformance` publishes the suite.

use r_code_harness_protocol::rpc::{error_code, RpcId, RpcRequest};
use r_code_harness_protocol::services::PermissionCeiling;
use r_code_harness_protocol::{HarnessId, HostService, PackageRef, Provenance, RunIdentity};
use r_code_kernel::completion::{arbitrate, Arbitration, ArbitrationInputs};
use r_code_kernel::ports::RunGuard;
use r_code_kernel::task::{Actor, CompletionProposal};
use r_code_kernel::task::{EvidenceRecord, ProposalKind, TaskContract, TaskKind, TaskVerdict};
use r_code_kernel::testing::{
    FakeModelService, FakeProcessService, FakeToolService, MemoryJournal,
};
use r_code_kernel::verification::{CheckDefinition, CheckEntrypoint};
use r_code_runtime::plugins::HostRouter;
use std::sync::Arc;

/// One conformance check result.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ConformanceResult {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

fn request(method: &str, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: method.into(),
        params: Some(params),
    }
}

fn router(services: &[HostService]) -> Arc<HostRouter> {
    Arc::new(HostRouter::new(
        RunIdentity {
            task_id: "task-1".into(),
            branch_id: "branch-1".into(),
            run_id: "run-1".into(),
            attempt_id: "attempt-1".into(),
            generation: 1,
        },
        RunGuard::new("run-1", 1),
        services.to_vec(),
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(r_code_runtime::plugins::IgnoreQuestions),
    ))
}

fn check_definition() -> CheckDefinition {
    CheckDefinition {
        check_id: "check:required".into(),
        entrypoint: CheckEntrypoint::Command {
            program: "node".into(),
            argv: vec!["verify.js".into()],
        },
        control_files: vec![],
        source_roots: vec![],
        dependency_locks: vec![],
        toolchain: "node test".into(),
        declared_external_inputs: vec![],
        entrypoint_bytes: None,
    }
}

fn contract() -> TaskContract {
    TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Implementation,
        objective: "o".into(),
        constraints: vec![],
        required_checks: vec!["check:required".into()],
        revision: 1,
    }
}

/// The full deterministic conformance suite. Hanging-cancellation coverage
/// runs in the transport tests (t09 `ignored_cancellation_is_killed_after_
/// grace`) with real processes; here the protocol-level gates are proven.
pub async fn run_conformance_suite() -> Vec<ConformanceResult> {
    let mut results = Vec::new();

    // 1. False completion: no evidence at all.
    let definitions = [check_definition()];
    let inputs = ArbitrationInputs {
        contract: &contract(),
        candidate_digest: Some("digest-1"),
        evidence: &[],
        definitions: &definitions,
        unavailable: Vec::new(),
    };
    let proposal = CompletionProposal {
        actor: Actor::Plugin,
        kind: ProposalKind::Implementation,
        summary: "trust me".into(),
        candidate_digest: Some("digest-1".into()),
    };
    let verdict = arbitrate(&inputs, &proposal);
    results.push(ConformanceResult {
        name: "false-completion-refused",
        passed: matches!(&verdict, Arbitration::Final(TaskVerdict::Unverified { .. })),
        detail: format!("{verdict:?}"),
    });

    // 2. Stale evidence: passing record for another candidate.
    let stale = [EvidenceRecord {
        evidence_id: "ev-old".into(),
        check_id: "check:required".into(),
        candidate_digest: "digest-old".into(),
        environment: "node test".into(),
        passed: true,
        host_output: None,
        recorded_by: Provenance::Host,
    }];
    let inputs = ArbitrationInputs {
        contract: &contract(),
        candidate_digest: Some("digest-1"),
        evidence: &stale,
        definitions: &definitions,
        unavailable: Vec::new(),
    };
    let verdict = arbitrate(&inputs, &proposal);
    results.push(ConformanceResult {
        name: "stale-evidence-refused",
        passed: matches!(&verdict, Arbitration::Final(TaskVerdict::Unverified { .. })),
        detail: format!("{verdict:?}"),
    });

    // 3. Cross-run handles.
    let router = router(&[HostService::ProcessOpen, HostService::ProcessWrite]);
    let opened = router
        .handle_request(request(
            "host.process.open",
            serde_json::json!({"profile": "p", "arguments": []}),
        ))
        .await
        .expect("open");
    let handle = opened["handle"]
        .as_str()
        .unwrap()
        .replace("run-1:", "run-2:");
    let error = router
        .handle_request(request(
            "host.process.write",
            serde_json::json!({"handle": handle, "data_base64": "aGk="}),
        ))
        .await
        .expect_err("cross-run refused");
    results.push(ConformanceResult {
        name: "cross-run-handle-refused",
        passed: error.code == error_code::RUN_MISMATCH,
        detail: format!("{error:?}"),
    });

    // 4. Duplicate side effects replay instead of re-executing.
    let tools = Arc::new(FakeToolService::default());
    let router = Arc::new(HostRouter::new(
        RunIdentity {
            task_id: "task-1".into(),
            branch_id: "branch-1".into(),
            run_id: "run-1".into(),
            attempt_id: "attempt-1".into(),
            generation: 1,
        },
        RunGuard::new("run-1", 1),
        vec![HostService::ToolsCall],
        tools.clone(),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(r_code_runtime::plugins::IgnoreQuestions),
    ));
    let params = serde_json::json!({
        "tool": "read_file", "input": {"path": "a"},
        "operation_key": "dedup-1"
    });
    let first = router
        .handle_request(request("host.tools.call", params.clone()))
        .await
        .expect("first");
    let second = router
        .handle_request(request("host.tools.call", params))
        .await
        .expect("replay");
    let executed_once = tools.calls.lock().unwrap().len() == 1 && first == second;
    results.push(ConformanceResult {
        name: "duplicate-side-effect-replayed",
        passed: executed_once,
        detail: format!("calls={}", tools.calls.lock().unwrap().len()),
    });

    // 5. Hanging cancellation: the transport kill-after-grace contract is
    // pinned by the real-process suite; the protocol gate is the revoked
    // generation refusing late calls.
    let guard = RunGuard::new("run-1", 1);
    let fenced_router = Arc::new(HostRouter::new(
        RunIdentity {
            task_id: "task-1".into(),
            branch_id: "branch-1".into(),
            run_id: "run-1".into(),
            attempt_id: "attempt-1".into(),
            generation: 1,
        },
        guard.clone(),
        vec![HostService::ToolsCall],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(r_code_runtime::plugins::IgnoreQuestions),
    ));
    // Revoking before the call simulates a cancellation already in flight.
    guard.revoke();
    let late = fenced_router
        .handle_request(request(
            "host.tools.call",
            serde_json::json!({
                "tool": "read_file", "input": {}, "operation_key": "late"
            }),
        ))
        .await
        .expect_err("late refused");
    results.push(ConformanceResult {
        name: "hanging-cancellation-fenced",
        passed: late.code == error_code::GENERATION_REVOKED,
        detail: format!("{late:?}"),
    });

    results
}

/// Package reference used by report consumers.
pub fn suite_identity() -> PackageRef {
    PackageRef {
        id: HarnessId::new("conformance.r-code-evals"),
        version: semver::Version::new(1, 0, 0),
        content_digest: "conformance-suite-v1".into(),
    }
}

/// Render results as one line per check.
pub fn render(results: &[ConformanceResult]) -> String {
    results
        .iter()
        .map(|result| {
            format!(
                "{} {} — {}",
                if result.passed { "PASS" } else { "FAIL" },
                result.name,
                result.detail
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The parent-ceiling constant referenced by delegating suites.
pub const CONFORMANCE_PARENT_CEILING: PermissionCeiling = PermissionCeiling::Full;
