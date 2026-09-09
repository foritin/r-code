//! T27 — Native Plan, steer and context recovery.
//!
//! Restart and accepted-steer scenarios run through the executable plugin:
//! a checkpointed conversation resumes after a restart, replayed inputs
//! drive new turns, and an accepted steer lands in the persisted state.

use r_code_gateway::gateway::ToolGateway;
use r_code_gateway::tools::ReadFileTool;
use r_code_harness_protocol::services::{ModelStreamRequest, StreamEvent, StreamPayload};
use r_code_harness_protocol::{
    ApiVersion, HarnessId, HostService, NegotiatedCapabilities, PackageRef, RunIdentity,
};
use r_code_kernel::ports::{
    GenerationToken, HarnessSession, JournalStore, ModelService, ModelStreamOutcome, RunGuard,
    ServiceError, StreamSink, ToolService,
};
use r_code_runtime::plugins::*;
use r_code_runtime::services::authorization::{
    AuthorizationService, EffectivePermissions, WorkspaceCapability,
};
use r_code_runtime::services::tools::GatewayToolService;
use r_code_store::v2::V2Store;
use std::sync::Arc;
use std::time::Duration;

fn native_binary() -> std::path::PathBuf {
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "r-code-harness-native"])
            .output()
            .expect("cargo build native");
    assert!(output.status.success(), "build failed");
    let exe = if cfg!(windows) {
        "r-code-harness-native.exe"
    } else {
        "r-code-harness-native"
    };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(exe);
    assert!(path.is_file());
    path
}

/// Scripted model: text-only finishes; records message counts.
struct TextModel {
    requests: std::sync::Mutex<Vec<usize>>,
}

#[async_trait::async_trait]
impl ModelService for TextModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.requests.lock().unwrap().push(request.messages.len());
        let stream_id = format!("m-{}", self.requests.lock().unwrap().len());
        sink.send(StreamEvent {
            stream_id: stream_id.clone(),
            sequence: 1,
            payload: StreamPayload::TextDelta {
                text: "text-only finish".into(),
            },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: stream_id.clone(),
            sequence: 2,
            payload: StreamPayload::Finish {
                reason: "end_turn".into(),
                usage: Default::default(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id,
            finish_reason: Some("end_turn".into()),
            usage: Default::default(),
        })
    }
}

async fn spawn_session(
    binary: &std::path::Path,
    store: Arc<V2Store>,
    model: Arc<dyn ModelService>,
) -> PluginSession {
    let identity = RunIdentity {
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        run_id: "run-1".into(),
        attempt_id: "attempt-1".into(),
        generation: 1,
    };
    let guard = RunGuard::new("run-1", 1);
    let grants = vec![
        HostService::ModelStream,
        HostService::ToolsList,
        HostService::ToolsCall,
        HostService::CheckpointSave,
        HostService::CompletionPropose,
    ];
    let mut gateway =
        ToolGateway::new(Arc::new(r_code_gateway::permission::PermissionEngine::new()));
    gateway.register(Box::new(ReadFileTool));
    let tools: Arc<dyn ToolService> = Arc::new(GatewayToolService::new(
        Arc::new(gateway),
        Arc::new(AuthorizationService::new()),
        store.clone(),
        WorkspaceCapability::Unrestricted,
        EffectivePermissions::full(),
    ));
    let router = Arc::new(HostRouter::new(
        identity.clone(),
        guard.clone(),
        grants.clone(),
        tools,
        model,
        Arc::new(r_code_kernel::testing::FakeProcessService::default()),
        store.clone(),
        Arc::new(IgnoreQuestions),
    ));
    PluginSession::start(
        binary,
        &[],
        identity,
        NegotiatedCapabilities {
            plugin_api: ApiVersion::new(1, 0),
            host_api: ApiVersion::new(1, 0),
            granted_services: grants,
        },
        guard,
        router,
        serde_json::json!({}),
        TransportLimits::default(),
    )
    .await
    .expect("session")
}

fn attempt() -> r_code_kernel::task::Attempt {
    r_code_kernel::task::Attempt {
        attempt_id: "attempt-1".into(),
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        package: PackageRef {
            id: HarnessId::new("native.r-code"),
            version: semver::Version::new(1, 0, 1),
            content_digest: "sha-native".into(),
        },
        contract_revision: 1,
        config_hash: "cfg".into(),
        workspace_identity: "ws".into(),
        run_id: "run-1".into(),
    }
}

fn contract() -> r_code_kernel::task::TaskContract {
    r_code_kernel::task::TaskContract {
        task_id: "task-1".into(),
        kind: r_code_kernel::task::TaskKind::Conversation,
        objective: "objective".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    }
}

fn input(
    seq: u64,
    kind: r_code_harness_protocol::InputKind,
    text: &str,
) -> r_code_harness_protocol::InputMessage {
    r_code_harness_protocol::InputMessage {
        message_id: format!("m{seq}"),
        input_seq: seq,
        kind,
        text: text.into(),
    }
}

#[tokio::test]
async fn restart_resumes_from_the_checkpoint_and_replays_inputs() {
    let binary = native_binary();
    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(V2Store::open(&temp.path().join("tasks.sqlite3")).expect("store"));
    let model = Arc::new(TextModel {
        requests: std::sync::Mutex::new(Vec::new()),
    });

    // First incarnation: one full run, checkpoint persisted.
    {
        let session = spawn_session(&binary, store.clone(), model.clone()).await;
        session
            .start(
                &attempt(),
                &contract(),
                &input(1, r_code_harness_protocol::InputKind::User, "first run"),
            )
            .await
            .expect("first run");
        session.cancel("restart").await.expect("stop first");
    }
    let checkpoint = store
        .load_latest_checkpoint("attempt-1")
        .await
        .expect("checkpoint");
    assert!(checkpoint.revision >= 1, "loop checkpointed");
    let state: serde_json::Value =
        serde_json::from_slice(&checkpoint.state).expect("conversation state");
    assert!(state["messages"]
        .as_array()
        .map(|m| !m.is_empty())
        .unwrap_or(false));

    // Second incarnation: resume from the checkpoint with one replay input.
    {
        let session = spawn_session(&binary, store.clone(), model.clone()).await;
        session
            .resume(
                &attempt(),
                &checkpoint.artifact,
                &[input(
                    2,
                    r_code_harness_protocol::InputKind::User,
                    "after restart",
                )],
            )
            .await
            .expect("resume");
        session.cancel("done").await.expect("stop second");
    }
    // The model saw two runs (restart + replay).
    let requests = model.requests.lock().unwrap();
    assert!(
        requests.len() >= 2,
        "replay drove a new model turn: {requests:?}"
    );
}

#[tokio::test]
async fn accepted_steer_lands_in_the_persisted_state_before_the_finish() {
    let binary = native_binary();
    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(V2Store::open(&temp.path().join("tasks.sqlite3")).expect("store"));
    let model = Arc::new(TextModel {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let session = spawn_session(&binary, store.clone(), model.clone()).await;

    session
        .start(
            &attempt(),
            &contract(),
            &input(
                1,
                r_code_harness_protocol::InputKind::User,
                "initial objective",
            ),
        )
        .await
        .expect("start");

    // An accepted steer: the notification reaches the plugin and queues as
    // the next user input (persisted in the plugin's state).
    session
        .steer(&input(
            2,
            r_code_harness_protocol::InputKind::Steer,
            "also check the tests",
        ))
        .await
        .expect("steer accepted");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let checkpoint = store
        .load_latest_checkpoint("attempt-1")
        .await
        .expect("checkpoint");
    let text = String::from_utf8_lossy(&checkpoint.state).into_owned();
    assert!(
        text.contains("also check the tests"),
        "steer text must land in the persisted state"
    );
    session.cancel("done").await.expect("stop");
}
