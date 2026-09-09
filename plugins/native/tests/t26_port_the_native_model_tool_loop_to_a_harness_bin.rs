//! T26 — Native model/tool loop ported to a harness binary.
//!
//! The Native binary performs real fixture editing through public host
//! APIs; dependency guards reject Gateway/store/runtime/Tauri imports.

use r_code_gateway::gateway::ToolGateway;
use r_code_gateway::tools::{CreateFileTool, ReadFileTool};
use r_code_harness_protocol::services::{
    ContentBlock, ModelMessage, ModelRole, ModelStreamRequest, StreamEvent, StreamPayload,
};
use r_code_harness_protocol::{
    ApiVersion, HarnessId, HostService, NegotiatedCapabilities, PackageRef, RunIdentity,
};
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ModelStreamOutcome, RunGuard, ServiceError,
    StreamSink, ToolService,
};
use r_code_runtime::plugins::*;
use r_code_runtime::services::authorization::{
    AuthorizationService, EffectivePermissions, WorkspaceCapability,
};
use r_code_runtime::services::tools::GatewayToolService;
use r_code_store::v2::V2Store;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Locate (always rebuilding) the native plugin binary.
fn native_binary() -> PathBuf {
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "r-code-harness-native"])
            .output()
            .expect("cargo build native");
    assert!(
        output.status.success(),
        "building native failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exe = if cfg!(windows) {
        "r-code-harness-native.exe"
    } else {
        "r-code-harness-native"
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(exe);
    assert!(path.is_file(), "missing {}", path.display());
    path
}

/// A scripted model: first turn asks to create a file, second turn wraps up.
struct EditingModel {
    turns: std::sync::atomic::AtomicU32,
    workspace_root: String,
}

#[async_trait::async_trait]
impl ModelService for EditingModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        let turn = self.turns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let stream_id = format!("model-{turn}");
        if turn == 0 {
            sink.send(StreamEvent {
                stream_id: stream_id.clone(),
                sequence: 1,
                payload: StreamPayload::ToolCallDelta {
                    id: "call-1".into(),
                    name: "create_file".into(),
                    partial_input: serde_json::json!({
                        "path": format!("{}/fixture-answer.txt", self.workspace_root),
                        "content": "edited by the native loop"
                    })
                    .to_string(),
                },
                done: None,
            })
            .await?;
            sink.send(StreamEvent {
                stream_id: stream_id.clone(),
                sequence: 2,
                payload: StreamPayload::Finish {
                    reason: "tool_use".into(),
                    usage: Default::default(),
                },
                done: Some(true),
            })
            .await?;
        } else {
            sink.send(StreamEvent {
                stream_id: stream_id.clone(),
                sequence: 1,
                payload: StreamPayload::TextDelta {
                    text: "fixture edit complete".into(),
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
        }
        Ok(ModelStreamOutcome {
            stream_id,
            finish_reason: Some("done".into()),
            usage: Default::default(),
        })
    }
}

#[tokio::test]
async fn native_binary_edits_a_real_fixture_through_public_host_apis() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("project");
    std::fs::create_dir_all(&workspace_root).expect("workspace");

    // Real host services: gateway tools over the temp workspace.
    let mut gateway =
        ToolGateway::new(Arc::new(r_code_gateway::permission::PermissionEngine::new()));
    gateway.register(Box::new(CreateFileTool));
    gateway.register(Box::new(ReadFileTool));
    let store = Arc::new(V2Store::open(&temp.path().join("tasks.sqlite3")).expect("store"));
    let tools: Arc<dyn ToolService> = Arc::new(GatewayToolService::new(
        Arc::new(gateway),
        Arc::new(AuthorizationService::new()),
        store.clone(),
        WorkspaceCapability::WriteWithin {
            root: workspace_root.to_string_lossy().replace('\\', "/"),
        },
        EffectivePermissions::full(),
    ));

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
    let router = Arc::new(HostRouter::new(
        identity.clone(),
        guard.clone(),
        grants.clone(),
        tools,
        Arc::new(EditingModel {
            turns: std::sync::atomic::AtomicU32::new(0),
            workspace_root: workspace_root.to_string_lossy().replace('\\', "/"),
        }),
        Arc::new(r_code_kernel::testing::FakeProcessService::default()),
        store.clone(),
        Arc::new(IgnoreQuestions),
    ));

    let session = PluginSession::start(
        &native_binary(),
        &[],
        identity,
        NegotiatedCapabilities {
            plugin_api: ApiVersion::new(1, 0),
            host_api: ApiVersion::new(1, 0),
            granted_services: grants,
        },
        guard,
        router.clone(),
        serde_json::json!({}),
        TransportLimits::default(),
    )
    .await
    .expect("session");

    use r_code_kernel::ports::HarnessSession;
    let attempt = r_code_kernel::task::Attempt {
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
    };
    let contract = r_code_kernel::task::TaskContract {
        task_id: "task-1".into(),
        kind: r_code_kernel::task::TaskKind::Conversation,
        objective: "create the fixture answer file".into(),
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
        .expect("native run");

    // The fixture file was really edited through host.tools.call.
    let edited = workspace_root.join("fixture-answer.txt");
    assert!(
        edited.is_file(),
        "native loop must edit the fixture through public host APIs"
    );
    assert_eq!(
        std::fs::read_to_string(&edited).unwrap(),
        "edited by the native loop"
    );

    // The loop checkpointed its conversation state with the host.
    let checkpoint = store
        .load_latest_checkpoint("attempt-1")
        .await
        .expect("checkpoint");
    let state: serde_json::Value = serde_json::from_slice(&checkpoint.state).expect("state");
    assert!(state["messages"]
        .as_array()
        .map(|m| !m.is_empty())
        .unwrap_or(false));

    // And proposed completion for host arbitration.
    let proposals = router.recorded_proposals.lock().expect("proposals").clone();
    assert!(!proposals.is_empty(), "the loop proposed completion");

    session.cancel("done").await.expect("cancel");
    let _ = ModelRole::User;
    let _ = ContentBlock::Text {
        text: String::new(),
    };
    let _ = ModelMessage {
        role: ModelRole::User,
        content: vec![],
    };
    let _ = Duration::from_secs(1);
}

#[test]
fn native_plugin_dependency_guards_reject_host_imports() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("manifest");
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .map(|section| section.split('[').next().unwrap_or(section))
        .unwrap_or("");
    for forbidden in [
        "r-code-runtime",
        "r-code-store",
        "r-code-gateway",
        "r-code-client",
        "r-code-host",
        "tauri",
        "r-code-agent-worker",
    ] {
        assert!(
            !dependencies.contains(forbidden),
            "native plugin must not depend on {forbidden}"
        );
    }
}
