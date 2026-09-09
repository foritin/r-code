//! T10 — session binding and host RPC router.
//!
//! Acceptance: cross-run handles, unsupported methods and late callbacks
//! are rejected before reaching effectful services.

use r_code_harness_protocol::rpc::{error_code, RpcRequest};
use r_code_harness_protocol::services::ToolCallRequest;
use r_code_harness_protocol::{
    ApiVersion, HostService, NegotiatedCapabilities, RpcId, RunIdentity,
};
use r_code_kernel::ports::{JournalStore, RunGuard, ToolService};
use r_code_kernel::testing::{
    FakeModelService, FakeProcessService, FakeToolService, MemoryJournal,
};
use r_code_runtime::plugins::*;
use std::sync::Arc;

fn identity() -> RunIdentity {
    RunIdentity {
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        run_id: "run-1".into(),
        attempt_id: "attempt-1".into(),
        generation: 1,
    }
}

fn capabilities(services: &[HostService]) -> NegotiatedCapabilities {
    NegotiatedCapabilities {
        plugin_api: ApiVersion::new(1, 0),
        host_api: ApiVersion::new(1, 0),
        granted_services: services.to_vec(),
    }
}

struct RecordingQuestions;

impl QuestionSink for RecordingQuestions {
    fn raised(&self, question: RaisedQuestion) {
        QUESTION_LOG.lock().unwrap().push(question);
    }
}

static QUESTION_LOG: std::sync::Mutex<Vec<RaisedQuestion>> = std::sync::Mutex::new(Vec::new());

fn request(method: &str, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: method.into(),
        params: Some(params),
    }
}

#[tokio::test]
async fn full_stack_nested_callback_through_real_transport() {
    struct ListRecorder {
        inner: FakeToolService,
        lists: std::sync::Mutex<u32>,
    }

    #[async_trait::async_trait]
    impl ToolService for ListRecorder {
        async fn list(
            &self,
            token: r_code_kernel::ports::GenerationToken,
        ) -> Result<Vec<r_code_harness_protocol::ToolDescriptor>, r_code_kernel::ports::ServiceError>
        {
            *self.lists.lock().unwrap() += 1;
            self.inner.list(token).await
        }

        async fn call(
            &self,
            token: r_code_kernel::ports::GenerationToken,
            call: ToolCallRequest,
        ) -> Result<r_code_harness_protocol::ToolCallReply, r_code_kernel::ports::ServiceError>
        {
            self.inner.call(token, call).await
        }
    }

    let guard = RunGuard::new("run-1", 1);
    let tools = Arc::new(ListRecorder {
        inner: FakeToolService::default(),
        lists: std::sync::Mutex::new(0),
    });
    let router = Arc::new(HostRouter::new(
        identity(),
        guard.clone(),
        vec![
            HostService::ToolsList,
            HostService::ToolsCall,
            HostService::ModelStream,
        ],
        tools.clone(),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    ));

    let session = PluginSession::start(
        std::path::Path::new(env!("CARGO_BIN_EXE_harness-test-helper")),
        &["serve".into()],
        identity(),
        capabilities(&[
            HostService::ToolsList,
            HostService::ToolsCall,
            HostService::ModelStream,
        ]),
        guard,
        router.clone(),
        serde_json::json!({}),
        TransportLimits::default(),
    )
    .await
    .expect("session start");

    // harness.start triggers the plugin's nested host.tools.list callback;
    // the independent reader must serve it while start is pending.
    use r_code_kernel::ports::HarnessSession;
    let attempt = r_code_kernel::task::Attempt {
        attempt_id: "attempt-1".into(),
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        package: r_code_harness_protocol::PackageRef {
            id: r_code_harness_protocol::HarnessId::new("fixture.harness"),
            version: semver::Version::new(0, 1, 0),
            content_digest: "sha".into(),
        },
        contract_revision: 1,
        config_hash: "cfg".into(),
        workspace_identity: "ws".into(),
        run_id: "run-1".into(),
    };
    let contract = r_code_kernel::task::TaskContract {
        task_id: "task-1".into(),
        kind: r_code_kernel::task::TaskKind::Conversation,
        objective: "o".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    };
    let input = r_code_harness_protocol::InputMessage {
        message_id: "m1".into(),
        input_seq: 1,
        kind: r_code_harness_protocol::InputKind::User,
        text: "go".into(),
    };
    session
        .start(&attempt, &contract, &input)
        .await
        .expect("harness start");
    assert_eq!(
        *tools.lists.lock().unwrap(),
        1,
        "nested callback reached the tool service"
    );
    // The progress notification from the plugin was observed.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let events = router.observed_events.lock().expect("events").clone();
    assert!(events.iter().any(|event| event.method == "harness.event"));
    session.cancel("done").await.expect("cancel");
}

#[tokio::test]
async fn cross_run_handles_are_rejected_before_the_process_service() {
    let guard = RunGuard::new("run-1", 1);
    let processes = Arc::new(FakeProcessService::default());
    let router = HostRouter::new(
        identity(),
        guard,
        vec![
            HostService::ProcessOpen,
            HostService::ProcessWrite,
            HostService::ProcessClose,
        ],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        processes.clone(),
        Arc::new(MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    );

    // Open a handle on run-1.
    let opened: serde_json::Value = router
        .handle_request(request(
            "host.process.open",
            serde_json::json!({"profile": "app-server", "arguments": []}),
        ))
        .await
        .expect("open");
    let handle = opened["handle"].as_str().expect("handle").to_string();
    assert!(handle.starts_with("run-1:"));
    assert_eq!(*processes.open_count.lock().unwrap(), 1);

    // Write with a handle forged for another run: rejected before the
    // process service can observe it.
    let forged = format!("run-2:{}", &handle["run-1:".len()..]);
    let error = router
        .handle_request(request(
            "host.process.write",
            serde_json::json!({"handle": forged, "data_base64": "aGVsbG8="}),
        ))
        .await
        .expect_err("cross-run handle");
    assert_eq!(error.code, error_code::RUN_MISMATCH);
    // The service saw only the open.
    assert_eq!(*processes.open_count.lock().unwrap(), 1);
}

#[tokio::test]
async fn unknown_and_ungranted_methods_fail_closed() {
    let guard = RunGuard::new("run-1", 1);
    let router = HostRouter::new(
        identity(),
        guard,
        vec![HostService::ToolsList], // tools.call NOT granted
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    );

    let unknown = router
        .handle_request(request("host.debug.spawn", serde_json::json!({})))
        .await
        .expect_err("unknown method");
    assert_eq!(unknown.code, -32601);

    let ungranted = router
        .handle_request(request(
            "host.tools.call",
            serde_json::to_value(ToolCallRequest {
                tool: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            })
            .unwrap(),
        ))
        .await
        .expect_err("service not granted");
    assert_eq!(ungranted.code, error_code::PROTOCOL_VIOLATION);
    assert!(ungranted.message.contains("not part of this run's grants"));
}

#[tokio::test]
async fn late_callbacks_from_revoked_generations_are_rejected() {
    let guard = RunGuard::new("run-1", 1);
    let tools = Arc::new(FakeToolService::default());
    let router = HostRouter::new(
        identity(),
        guard.clone(),
        vec![HostService::ToolsCall],
        tools.clone(),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    );

    // Live call works.
    router
        .handle_request(request(
            "host.tools.call",
            serde_json::json!({"tool": "read_file", "input": {}}),
        ))
        .await
        .expect("live call");

    // Cancel revokes the generation: a late callback is refused.
    guard.revoke();
    let late = router
        .handle_request(request(
            "host.tools.call",
            serde_json::json!({"tool": "read_file", "input": {}}),
        ))
        .await
        .expect_err("late callback");
    assert_eq!(late.code, error_code::GENERATION_REVOKED);
    assert_eq!(
        tools.calls.lock().unwrap().len(),
        1,
        "only the live call executed"
    );

    // A rotated generation (restart) is equally fenced.
    let guard2 = RunGuard::new("run-1", 2);
    let mut rotated_identity = identity();
    rotated_identity.generation = 1; // stale generation token
    let router2 = HostRouter::new(
        rotated_identity,
        guard2,
        vec![HostService::ToolsCall],
        tools.clone(),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    );
    let stale = router2
        .handle_request(request(
            "host.tools.call",
            serde_json::json!({"tool": "read_file", "input": {}}),
        ))
        .await
        .expect_err("stale generation");
    assert_eq!(stale.code, error_code::GENERATION_REVOKED);
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn operation_keys_deduplicate_effects_and_reject_conflicts() {
    let guard = RunGuard::new("run-1", 1);
    let tools = Arc::new(FakeToolService::default());
    let store: Arc<dyn JournalStore> = Arc::new(MemoryJournal::new());
    let router = HostRouter::new(
        identity(),
        guard,
        vec![HostService::ToolsCall],
        tools.clone(),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        store,
        Arc::new(IgnoreQuestions),
    );

    let params = serde_json::json!({
        "tool": "read_file",
        "input": {"path": "a.txt"},
        "operation_key": "write-a"
    });
    let first = router
        .handle_request(request("host.tools.call", params.clone()))
        .await
        .expect("first call");
    assert!(first["output"].is_array());
    assert_eq!(tools.calls.lock().unwrap().len(), 1);

    // Same key + same input: receipt replay, no second execution.
    let replay = router
        .handle_request(request("host.tools.call", params.clone()))
        .await
        .expect("replay");
    assert_eq!(replay, first);
    assert_eq!(tools.calls.lock().unwrap().len(), 1, "deduplicated");

    // Same key + different input: refused.
    let mut conflicting = params.clone();
    conflicting["input"]["path"] = serde_json::json!("b.txt");
    let error = router
        .handle_request(request("host.tools.call", conflicting))
        .await
        .expect_err("conflict");
    assert_eq!(error.code, error_code::PROTOCOL_VIOLATION);
    assert!(error.message.contains("different input"));
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn approvals_reference_only_host_created_pending_operations() {
    QUESTION_LOG.lock().unwrap().clear();
    let guard = RunGuard::new("run-1", 1);
    let router = HostRouter::new(
        identity(),
        guard,
        vec![HostService::QuestionsAsk, HostService::ApprovalsRequest],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(RecordingQuestions),
    );

    // Questions never grant anything; they persist and return an id.
    let asked: serde_json::Value = router
        .handle_request(request(
            "host.questions.ask",
            serde_json::json!({"text": "which db?", "options": ["sqlite"], "blocking": true}),
        ))
        .await
        .expect("ask");
    let question_id = asked["question_id"].as_str().expect("id").to_string();
    assert!(question_id.starts_with("q-run-1-"));
    assert_eq!(QUESTION_LOG.lock().unwrap().len(), 1);

    // Approval citing an unknown pending operation: denied, never created.
    let denied: serde_json::Value = router
        .handle_request(request(
            "host.approvals.request",
            serde_json::json!({
                "pending_operation": {"operation_id": "op-404", "inputHash": "x"},
                "summary": "run tests"
            }),
        ))
        .await
        .expect("handled");
    assert_eq!(denied["decision"], "denied");

    // Host-side decision for a real pending operation is honoured.
    router
        .approvals
        .set_decision("op-7", r_code_harness_protocol::ApprovalDecision::Granted);
    let granted: serde_json::Value = router
        .handle_request(request(
            "host.approvals.request",
            serde_json::json!({
                "pending_operation": {"operation_id": "op-7", "inputHash": "x"},
                "summary": "run tests"
            }),
        ))
        .await
        .expect("handled");
    assert_eq!(granted["decision"], "granted");
}
