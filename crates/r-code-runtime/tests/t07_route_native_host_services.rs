//! T07 — negotiated Native host services and their task-scoped state.

use base64::Engine as _;
use r_code_harness_protocol::rpc::{error_code, RpcRequest};
use r_code_harness_protocol::services::{
    ArtifactRef, ArtifactsPutRequest, ArtifactsReadRequest, ContentBlock, ContextPage,
    ModelMessage, ModelRole, ModelStreamRequest, OutputBlock,
};
use r_code_harness_protocol::{
    canonical_input_hash, ApiVersion, HarnessId, HarnessManifest, HostService,
    NegotiatedCapabilities, PackageRef, RpcId, RunIdentity,
};
use r_code_kernel::ports::{JournalStore, ModelService, RunGuard, ToolService};
use r_code_kernel::task::{
    Attempt, PermissionSnapshotRef, PromptSnapshotMode, PromptSnapshotRef, ProviderRouteKind,
    ProviderSnapshotRef, RunSnapshot, RunSnapshotMaterial, RunSnapshotPhase, TaskContract,
    TaskExecution, TaskKind, TaskState, WorkspaceSnapshotRef,
};
use r_code_kernel::testing::{FakeModelService, FakeProcessService, FakeToolService};
use r_code_runtime::application::ApplicationService;
use r_code_runtime::plugins::router::{
    service_for_method, supported_requested_services, RouterServiceAvailability,
};
use r_code_runtime::plugins::{
    HostRouter, IgnoreQuestions, PluginSession, QuestionSink, RaisedQuestion, TransportLimits,
};
use r_code_runtime::services::artifacts::{ArtifactError, ArtifactStore};
use r_code_runtime::services::context::{ContextError, ContextRegistry, MAX_TRANSCRIPT_PAGE_LIMIT};
use r_code_runtime::services::run_snapshots::RunSnapshotBuilder;
use r_code_runtime::services::settings_store::SettingsStore;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::V1Store;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn identity(task_id: &str, run_id: &str, attempt_id: &str) -> RunIdentity {
    RunIdentity {
        task_id: task_id.into(),
        branch_id: format!("branch-{task_id}"),
        run_id: run_id.into(),
        attempt_id: attempt_id.into(),
        generation: 1,
    }
}

fn request(method: &str, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: method.into(),
        params: Some(params),
    }
}

fn package() -> PackageRef {
    PackageRef {
        id: HarnessId::new("native.r-code"),
        version: semver::Version::new(1, 0, 1),
        content_digest: "sha256:native-package".into(),
    }
}

fn snapshot(task_id: &str, phase: RunSnapshotPhase, grants: &[HostService]) -> RunSnapshot {
    let mut capabilities = grants
        .iter()
        .map(|service| service.wire_name().to_string())
        .collect::<Vec<_>>();
    capabilities.sort();
    let work_unit_id = (!matches!(&phase, RunSnapshotPhase::Planning)).then(|| "implement".into());
    RunSnapshot::new(RunSnapshotMaterial {
        task_id: task_id.into(),
        task_revision: 1,
        phase,
        work_unit_id,
        provider: ProviderSnapshotRef {
            kind: ProviderRouteKind::HostProvider,
            settings_revision: 7,
            provider_id: "deepseek".into(),
            model_id: "deepseek-chat".into(),
            base_url: Some("https://api.deepseek.com".into()),
            protocol: Some("openai-compatible".into()),
            capabilities: vec!["streaming".into(), "tools".into()],
        },
        prompt: PromptSnapshotRef {
            revision: "prompt-r7".into(),
            mode: PromptSnapshotMode::Default,
            content_sha256: "sha256:prompt-r7".into(),
            resolved_system_prompt: "system-r7".into(),
        },
        workspace: WorkspaceSnapshotRef {
            canonical_root: "D:/project/r-code".into(),
            workspace_identity: "repo:r-code".into(),
            baseline_sha256: "sha256:workspace-r7".into(),
        },
        permissions: PermissionSnapshotRef {
            revision: "sha256:permissions-r7".into(),
            profile_id: "harness-manifest-v1".into(),
            capabilities,
        },
        harness_package: package(),
        tool_catalog_sha256: "sha256:tools-r7".into(),
        inference: Some(serde_json::json!({"temperature": 0})),
    })
    .expect("snapshot")
}

async fn save_running_task(store: &V1Store, task_id: &str, kind: TaskKind, id: &RunIdentity) {
    let mut state = TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind,
        objective: "T07 fixture".into(),
        constraints: vec![],
        required_checks: vec!["check:test".into(), "check:lint".into()],
        revision: 1,
    });
    state
        .start_attempt(&Attempt {
            attempt_id: id.attempt_id.clone(),
            task_id: task_id.into(),
            branch_id: id.branch_id.clone(),
            package: package(),
            contract_revision: 1,
            config_hash: "fixture".into(),
            workspace_identity: "repo:r-code".into(),
            run_id: id.run_id.clone(),
        })
        .expect("start fixture attempt");
    store
        .save_task_and_events(&state, vec![])
        .await
        .expect("persist running task");
}

fn row_count(path: &Path, table: &str) -> i64 {
    Connection::open(path)
        .expect("open database")
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("row count")
}

fn compile_blocking_question_harness(root: &Path) -> PathBuf {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/blocking_question_harness.rs");
    let binary = root.join(if cfg!(windows) {
        "blocking-question-harness.exe"
    } else {
        "blocking-question-harness"
    });
    let status = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
        .args(["--edition=2021", "-O"])
        .arg(source)
        .arg("-o")
        .arg(&binary)
        .status()
        .expect("compile blocking-question fixture");
    assert!(status.success(), "fixture compilation failed");
    binary
}

fn stage_blocking_question_harness(root: &Path) -> PathBuf {
    let binary = compile_blocking_question_harness(root);
    let package_root = root.join("blocking-question-package");
    let bin_root = package_root.join("bin");
    std::fs::create_dir_all(&bin_root).expect("package bin");
    let file_name = binary.file_name().expect("fixture filename");
    std::fs::copy(&binary, bin_root.join(file_name)).expect("stage fixture");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    std::fs::write(
        package_root.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": "fixture.blocking-question",
            "version": "1.0.0",
            "apiMajor": 1,
            "apiMinor": 0,
            "displayName": "Blocking question fixture",
            "supportedPlatforms": [{
                "platform": platform,
                "executable": format!("bin/{}", file_name.to_string_lossy())
            }],
            "requestedHostServices": ["host.questions.ask"],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("fixture manifest");
    package_root
}

async fn wait_for_journal_kind(
    service: &ApplicationService,
    task_id: &str,
    kind: &str,
) -> Vec<r_code_harness_protocol::EventEnvelope> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = service.events_after(0, 500).await;
        if events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!(kind))
        }) {
            return events;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {kind}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn model_request(messages: Vec<ModelMessage>) -> serde_json::Value {
    serde_json::to_value(ModelStreamRequest {
        selection: Some("deepseek-chat".into()),
        messages,
        tools: vec![],
        inference: None,
        deadline_ms: None,
    })
    .expect("model request")
}

fn text(role: ModelRole, value: &str) -> ModelMessage {
    ModelMessage {
        role,
        content: vec![ContentBlock::Text { text: value.into() }],
    }
}

#[derive(Default)]
struct RecordingQuestions {
    raised: Mutex<Vec<RaisedQuestion>>,
}

impl QuestionSink for RecordingQuestions {
    fn raised(&self, question: RaisedQuestion) {
        self.raised.lock().expect("questions").push(question);
    }
}

#[test]
fn native_manifest_grants_are_the_exact_requested_supported_intersection() {
    let manifest_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/native/harness.json");
    let manifest: HarnessManifest = serde_json::from_str(
        &std::fs::read_to_string(manifest_path).expect("read native manifest"),
    )
    .expect("parse native manifest");
    let availability = RouterServiceAvailability {
        model_stream: true,
        tools: true,
        context: true,
        artifacts: true,
        plan_publish: true,
        questions: true,
        approvals: true,
        checkpoints: true,
        completion: true,
        sandbox_activated: false,
    };
    let grants = supported_requested_services(&manifest.requested_host_services, availability);
    let expected = vec![
        HostService::ModelStream,
        HostService::ToolsList,
        HostService::ToolsCall,
        HostService::ContextRead,
        HostService::ArtifactsPut,
        HostService::ArtifactsRead,
        HostService::PlanPublish,
        HostService::QuestionsAsk,
        HostService::CheckpointSave,
        HostService::CompletionPropose,
    ];
    assert_eq!(grants, expected);

    for service in HostService::ALL {
        assert_eq!(
            service_for_method(service.wire_name()),
            Some(*service),
            "every manifest capability must have an unambiguous router mapping"
        );
    }
    for unsupported in [
        HostService::ProcessOpen,
        HostService::ProcessWrite,
        HostService::ProcessClose,
        HostService::PlanUpdate,
        HostService::ChildrenSpawn,
        HostService::ChildrenWait,
        HostService::ChildrenCancel,
        HostService::VerificationRun,
    ] {
        assert!(!supported_requested_services(&[unsupported], availability).contains(&unsupported));
    }
}

#[tokio::test]
async fn snapshot_router_and_initialize_share_one_exact_grant_set() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.path().join("profile"))
            .with_ipc_name("t07-grants"),
    )
    .expect("profile");
    let settings = SettingsStore::for_profile(&profile);
    let grants = vec![
        HostService::ModelStream,
        HostService::ToolsList,
        HostService::ToolsCall,
        HostService::ContextRead,
        HostService::ArtifactsPut,
        HostService::ArtifactsRead,
        HostService::PlanPublish,
        HostService::QuestionsAsk,
        HostService::CheckpointSave,
        HostService::CompletionPropose,
    ];
    let id = identity("task-grants", "run-grants", "attempt-grants");
    let mut state = TaskState::new(TaskContract {
        task_id: id.task_id.clone(),
        kind: TaskKind::PlanDraft,
        objective: "grant snapshot".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    });
    state.preferences.system_prompt = Some("frozen prompt".into());
    let model: Arc<dyn ModelService> = Arc::new(FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let guard = RunGuard::new(&id.run_id, 1);
    let frozen = RunSnapshotBuilder::new(&settings, &model, &tools, true)
        .freeze(&state, &package(), &grants, &guard)
        .await
        .expect("freeze through production builder");
    let router = Arc::new(HostRouter::new(
        id.clone(),
        guard.clone(),
        grants.clone(),
        tools,
        model,
        Arc::new(FakeProcessService::default()),
        Arc::new(r_code_kernel::testing::MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    ));
    let negotiated = NegotiatedCapabilities {
        plugin_api: ApiVersion::new(1, 0),
        host_api: ApiVersion::new(1, 0),
        granted_services: grants.clone(),
    };
    let session = PluginSession::start(
        Path::new(env!("CARGO_BIN_EXE_harness-test-helper")),
        &["serve".into()],
        id,
        negotiated,
        guard,
        router.clone(),
        serde_json::json!({}),
        TransportLimits::default(),
    )
    .await
    .expect("initialize fixture");

    assert_eq!(router.granted, grants);
    assert_eq!(session.capabilities().granted_services, grants);
    let mut frozen_capabilities = frozen.snapshot.material().permissions.capabilities.clone();
    let mut initialized = session
        .capabilities()
        .granted_services
        .iter()
        .map(|service| service.wire_name().to_string())
        .collect::<Vec<_>>();
    frozen_capabilities.sort();
    initialized.sort();
    assert_eq!(frozen_capabilities, initialized);
    session.process().kill().await.expect("stop fixture");
}

#[tokio::test]
async fn supported_routes_are_operational_and_ungranted_routes_have_zero_effects() {
    let temp = tempfile::tempdir().expect("tempdir");
    let database = temp.path().join("tasks.sqlite3");
    let store = Arc::new(V1Store::open(&database).expect("store"));
    let id = identity("task-plan", "run-plan", "attempt-plan");
    save_running_task(&store, "task-plan", TaskKind::PlanDraft, &id).await;
    let registry = ContextRegistry::new();
    let transcript_path = temp.path().join("transcript.jsonl");
    let transcript = Arc::new(
        registry
            .open_transcript("task-plan", Some(transcript_path))
            .expect("transcript"),
    );
    let artifact_root = temp.path().join("artifacts/task-plan");
    let artifacts = Arc::new(ArtifactStore::for_task(&artifact_root, "task-plan"));
    let tools = Arc::new(FakeToolService::default());
    let models = Arc::new(FakeModelService::default());
    let processes = Arc::new(FakeProcessService::default());
    let questions = Arc::new(RecordingQuestions::default());
    let grants = vec![
        HostService::ModelStream,
        HostService::ToolsList,
        HostService::ToolsCall,
        HostService::ContextRead,
        HostService::ArtifactsPut,
        HostService::ArtifactsRead,
        HostService::PlanPublish,
        HostService::QuestionsAsk,
        HostService::CheckpointSave,
        HostService::CompletionPropose,
    ];
    let frozen = snapshot("task-plan", RunSnapshotPhase::Planning, &grants);
    let router = HostRouter::new(
        id.clone(),
        RunGuard::new(&id.run_id, 1),
        grants.clone(),
        tools.clone(),
        models.clone(),
        processes.clone(),
        store.clone(),
        questions.clone(),
    )
    .with_transcript(transcript.clone())
    .with_artifacts(artifacts)
    .with_v1_store(store.clone())
    .with_plan_publication(
        frozen.clone(),
        TaskKind::PlanDraft,
        vec!["check:lint".into(), "check:test".into()],
        true,
    );

    assert_eq!(router.granted, grants);
    let mut frozen_capabilities = frozen.material().permissions.capabilities.clone();
    let mut granted_capabilities = router
        .granted
        .iter()
        .map(|service| service.wire_name().to_string())
        .collect::<Vec<_>>();
    frozen_capabilities.sort();
    granted_capabilities.sort();
    assert_eq!(frozen_capabilities, granted_capabilities);

    router
        .handle_request(request("host.tools.list", serde_json::json!({})))
        .await
        .expect("tools list");
    router
        .handle_request(request(
            "host.tools.call",
            serde_json::json!({"tool": "read_file", "input": {}}),
        ))
        .await
        .expect("tools call");
    router
        .handle_request(request(
            "host.model.stream",
            model_request(vec![
                text(ModelRole::System, "control"),
                text(ModelRole::User, "hello"),
            ]),
        ))
        .await
        .expect("model stream");
    let page: ContextPage = serde_json::from_value(
        router
            .handle_request(request(
                "host.context.read",
                serde_json::json!({"projection": "transcript", "cursor": 0, "limit": 10}),
            ))
            .await
            .expect("context read"),
    )
    .expect("context page");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].role, ModelRole::User);

    let artifact: ArtifactRef = serde_json::from_value(
        router
            .handle_request(request(
                "host.artifacts.put",
                serde_json::json!({
                    "media_type": "text/plain",
                    "data_base64": base64::engine::general_purpose::STANDARD.encode(b"artifact")
                }),
            ))
            .await
            .expect("artifact put"),
    )
    .expect("artifact ref");
    router
        .handle_request(request(
            "host.artifacts.read",
            serde_json::json!({"artifact": artifact, "offset": 0, "length": 0}),
        ))
        .await
        .expect("artifact read");

    router
        .handle_request(request(
            "host.plan.publish",
            serde_json::json!({
                "revision": 1,
                "work_units": [{
                    "id": "implement",
                    "description": "implement the approved unit",
                    "dependencies": [],
                    "acceptance": ["check:test"]
                }]
            }),
        ))
        .await
        .expect("plan publish");
    router
        .handle_request(request(
            "host.questions.ask",
            serde_json::json!({"text": "optional detail?", "options": [], "blocking": false}),
        ))
        .await
        .expect("question");
    router
        .handle_request(request(
            "host.checkpoint.save",
            serde_json::json!({"state_base64": "c3RhdGU=", "consumed_input_seq": 1}),
        ))
        .await
        .expect("checkpoint");
    router
        .handle_request(request(
            "host.completion.propose",
            serde_json::json!({
                "kind": "plan-draft",
                "summary": "plan is ready",
                "work_unit_statuses": []
            }),
        ))
        .await
        .expect("completion proposal");

    let tool_calls = tools.calls.lock().expect("tool calls").len();
    let model_calls = models.calls.lock().expect("model calls").len();
    let process_opens = *processes.open_count.lock().expect("process opens");
    let transcript_position = transcript.position();
    let question_count = row_count(&database, "questions");
    let plan_count = row_count(&database, "plan_revisions");
    let checkpoint_count = row_count(&database, "checkpoints");
    let proposal_count = router.recorded_proposals.lock().expect("proposals").len();

    for service in [
        HostService::ProcessOpen,
        HostService::ProcessWrite,
        HostService::ProcessClose,
        HostService::PlanUpdate,
        HostService::ChildrenSpawn,
        HostService::ChildrenWait,
        HostService::ChildrenCancel,
        HostService::VerificationRun,
    ] {
        let error = router
            .handle_request(request(service.wire_name(), serde_json::json!({})))
            .await
            .expect_err("unsupported route must fail before parsing or effects");
        assert_eq!(error.code, error_code::PROTOCOL_VIOLATION);
    }
    assert_eq!(tools.calls.lock().expect("tool calls").len(), tool_calls);
    assert_eq!(models.calls.lock().expect("model calls").len(), model_calls);
    assert_eq!(
        *processes.open_count.lock().expect("process opens"),
        process_opens
    );
    assert_eq!(transcript.position(), transcript_position);
    assert_eq!(row_count(&database, "questions"), question_count);
    assert_eq!(row_count(&database, "plan_revisions"), plan_count);
    assert_eq!(row_count(&database, "checkpoints"), checkpoint_count);
    assert_eq!(
        router.recorded_proposals.lock().expect("proposals").len(),
        proposal_count
    );
}

#[tokio::test]
async fn transcript_is_dense_durable_prompt_neutral_pair_safe_and_fail_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("task-a.jsonl");
    let registry = ContextRegistry::new();
    let transcript = Arc::new(
        registry
            .open_transcript("task-a", Some(path.clone()))
            .expect("transcript"),
    );
    let id = identity("task-a", "run-a", "attempt-a");
    let router = HostRouter::new(
        id.clone(),
        RunGuard::new(&id.run_id, 1),
        vec![HostService::ModelStream, HostService::ContextRead],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(r_code_kernel::testing::MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    )
    .with_transcript(transcript.clone());

    let user = text(ModelRole::User, "real user");
    router
        .handle_request(request(
            "host.model.stream",
            model_request(vec![
                text(ModelRole::System, "system-v1"),
                text(ModelRole::Developer, "developer-v1"),
                user.clone(),
            ]),
        ))
        .await
        .expect("first run");
    let assistant_text = text(ModelRole::Assistant, "assistant prefix");
    router
        .handle_request(request(
            "host.model.stream",
            model_request(vec![
                text(ModelRole::System, "system-v2"),
                text(ModelRole::Developer, "developer-v2"),
                user.clone(),
                assistant_text.clone(),
            ]),
        ))
        .await
        .expect("new prompt does not fork conversation");

    let tool_call = ModelMessage {
        role: ModelRole::Assistant,
        content: vec![ContentBlock::ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "README.md"}),
        }],
    };
    let tool_result = ModelMessage {
        role: ModelRole::Tool,
        content: vec![ContentBlock::ToolResult {
            call_id: "call-1".into(),
            output: vec![OutputBlock::Text {
                text: "body".into(),
            }],
        }],
    };
    router
        .handle_request(request(
            "host.model.stream",
            model_request(vec![
                text(ModelRole::System, "system-v3"),
                user.clone(),
                assistant_text.clone(),
                tool_call.clone(),
                tool_result.clone(),
            ]),
        ))
        .await
        .expect("append complete tool pair");

    let page = transcript.read_page(0, 3);
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(page
        .entries
        .iter()
        .all(|entry| !matches!(entry.role, ModelRole::System | ModelRole::Developer)));
    assert_eq!(transcript.read_page(2, u32::MAX).entries.len(), 2);
    assert!(transcript.read_page(u64::MAX, 10).entries.is_empty());

    let before_forks = transcript.position();
    let user_fork = router
        .handle_request(request(
            "host.model.stream",
            model_request(vec![text(ModelRole::User, "rewritten user")]),
        ))
        .await
        .expect_err("user prefix fork");
    assert_eq!(user_fork.code, error_code::PROTOCOL_VIOLATION);
    let mut changed_result = tool_result;
    changed_result.content = vec![ContentBlock::ToolResult {
        call_id: "call-1".into(),
        output: vec![OutputBlock::Text {
            text: "forged".into(),
        }],
    }];
    let tool_fork = router
        .handle_request(request(
            "host.model.stream",
            model_request(vec![user, assistant_text, tool_call, changed_result]),
        ))
        .await
        .expect_err("tool prefix fork");
    assert_eq!(tool_fork.code, error_code::PROTOCOL_VIOLATION);
    assert_eq!(transcript.position(), before_forks);

    let rollback = transcript.position();
    transcript
        .append(
            ModelRole::User,
            vec![ContentBlock::Text {
                text: "failed run".into(),
            }],
        )
        .expect("append failed run input");
    transcript
        .truncate_to(rollback)
        .expect("rollback failed run");
    transcript
        .append(
            ModelRole::User,
            vec![ContentBlock::Text {
                text: "successful run".into(),
            }],
        )
        .expect("retain successful run");
    drop(router);
    drop(transcript);
    drop(registry);

    let restarted = ContextRegistry::new()
        .open_transcript("task-a", Some(path.clone()))
        .expect("restart transcript");
    let restarted_page = restarted.read_page(0, MAX_TRANSCRIPT_PAGE_LIMIT);
    assert_eq!(
        restarted_page
            .entries
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    assert!(format!("{:?}", restarted_page.entries.last().unwrap()).contains("successful run"));
    assert!(!format!("{restarted_page:?}").contains("failed run"));

    let foreign_id = identity("task-b", "run-b", "attempt-b");
    let foreign = HostRouter::new(
        foreign_id.clone(),
        RunGuard::new(&foreign_id.run_id, 1),
        vec![HostService::ContextRead],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(r_code_kernel::testing::MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    )
    .with_transcript(Arc::new(restarted));
    let error = foreign
        .handle_request(request(
            "host.context.read",
            serde_json::json!({"projection": "transcript"}),
        ))
        .await
        .expect_err("cross-task transcript read");
    assert_eq!(error.code, error_code::RUN_MISMATCH);

    let corrupt = temp.path().join("corrupt.jsonl");
    std::fs::write(
        &corrupt,
        serde_json::to_string(&r_code_harness_protocol::services::TranscriptEntry {
            seq: 2,
            role: ModelRole::User,
            blocks: vec![],
        })
        .unwrap(),
    )
    .expect("write corrupt transcript");
    assert!(matches!(
        ContextRegistry::new().open_transcript("corrupt", Some(corrupt)),
        Err(ContextError::InvalidTranscript)
    ));
}

#[test]
fn transcript_page_limit_is_bounded_at_the_protocol_maximum() {
    let registry = ContextRegistry::new();
    let transcript = registry
        .open_transcript("bounded", None)
        .expect("transcript");
    for index in 0..(MAX_TRANSCRIPT_PAGE_LIMIT + 44) {
        transcript
            .append(
                ModelRole::User,
                vec![ContentBlock::Text {
                    text: format!("message-{index}"),
                }],
            )
            .expect("append");
    }
    let page = transcript.read_page(0, u32::MAX);
    assert_eq!(page.entries.len(), MAX_TRANSCRIPT_PAGE_LIMIT as usize);
    assert_eq!(page.next_cursor, Some(MAX_TRANSCRIPT_PAGE_LIMIT as u64));
    let next = transcript.read_page(page.next_cursor.unwrap(), 100);
    assert_eq!(next.entries.len(), 44);
}

#[test]
fn artifacts_survive_restart_and_reject_forgery_tampering_and_cross_task_reads() {
    let temp = tempfile::tempdir().expect("tempdir");
    let task_a_root = temp.path().join("tasks/task-a");
    let task_b_root = temp.path().join("tasks/task-b");
    let store = ArtifactStore::for_task(&task_a_root, "task-a");
    let reference = store
        .put(
            &ArtifactsPutRequest {
                media_type: Some("text/plain".into()),
                data_base64: base64::engine::general_purpose::STANDARD.encode(b"0123456789"),
            },
            "task-a",
        )
        .expect("put");
    drop(store);

    let restarted = ArtifactStore::for_task(&task_a_root, "task-a");
    let full = restarted
        .read(&ArtifactsReadRequest {
            artifact: reference.clone(),
            offset: 0,
            length: 0,
        })
        .expect("restart read");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(full.data_base64)
            .unwrap(),
        b"0123456789"
    );
    let range = restarted
        .read(&ArtifactsReadRequest {
            artifact: reference.clone(),
            offset: 3,
            length: 4,
        })
        .expect("range");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(range.data_base64)
            .unwrap(),
        b"3456"
    );

    let task_b = ArtifactStore::for_task(&task_b_root, "task-b");
    assert!(matches!(
        task_b.read(&ArtifactsReadRequest {
            artifact: reference.clone(),
            offset: 0,
            length: 0,
        }),
        Err(ArtifactError::NotFound | ArtifactError::TaskMismatch)
    ));
    assert!(matches!(
        task_b.put(
            &ArtifactsPutRequest {
                media_type: None,
                data_base64: "eA==".into(),
            },
            "task-a",
        ),
        Err(ArtifactError::TaskMismatch)
    ));

    let task_b_reference = task_b
        .put(
            &ArtifactsPutRequest {
                media_type: Some("text/plain".into()),
                data_base64: base64::engine::general_purpose::STANDARD.encode(b"0123456789"),
            },
            "task-b",
        )
        .expect("same content in task b");
    assert_eq!(task_b_reference.sha256, reference.sha256);
    assert_ne!(task_a_root, task_b_root);

    let forged = [
        (
            "blob_id",
            ArtifactRef {
                blob_id: format!("blob:sha256:{}", "a".repeat(64)),
                ..reference.clone()
            },
        ),
        (
            "sha256",
            ArtifactRef {
                sha256: "a".repeat(64),
                ..reference.clone()
            },
        ),
        (
            "bytes",
            ArtifactRef {
                bytes: reference.bytes + 1,
                ..reference.clone()
            },
        ),
        (
            "media_type",
            ArtifactRef {
                media_type: Some("application/x-forged".into()),
                ..reference.clone()
            },
        ),
    ];
    let forged_fields = forged
        .into_iter()
        .filter_map(|(field, artifact)| {
            restarted
                .read(&ArtifactsReadRequest {
                    artifact,
                    offset: 0,
                    length: 0,
                })
                .is_ok()
                .then_some(field)
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        restarted.read(&ArtifactsReadRequest {
            artifact: reference.clone(),
            offset: reference.bytes + 1,
            length: 1,
        }),
        Err(ArtifactError::InvalidRange)
    ));
    assert!(matches!(
        restarted.put(
            &ArtifactsPutRequest {
                media_type: None,
                data_base64: "%%%".into(),
            },
            "task-a",
        ),
        Err(ArtifactError::Base64)
    ));
    let escape = ArtifactRef {
        schema: ArtifactRef::SCHEMA,
        blob_id: "blob:sha256:../../outside".into(),
        bytes: 0,
        sha256: "../../outside".into(),
        media_type: None,
    };
    assert!(matches!(
        restarted.read(&ArtifactsReadRequest {
            artifact: escape,
            offset: 0,
            length: 0,
        }),
        Err(ArtifactError::InvalidReference)
    ));

    let owner = task_a_root.join(format!("{}.owner", reference.sha256));
    std::fs::write(&owner, "task-b").expect("tamper sidecar");
    let owner_tamper_rejected = restarted
        .read(&ArtifactsReadRequest {
            artifact: reference.clone(),
            offset: 0,
            length: 0,
        })
        .is_err();
    std::fs::write(&owner, "task-a").expect("restore sidecar");
    let blob = task_a_root.join(format!("{}.blob", reference.sha256));
    std::fs::write(blob, b"tampered").expect("tamper blob");
    assert!(matches!(
        restarted.read(&ArtifactsReadRequest {
            artifact: reference,
            offset: 0,
            length: 0,
        }),
        Err(ArtifactError::InvalidReference)
    ));
    assert!(
        forged_fields.is_empty() && owner_tamper_rejected,
        "forged reference fields accepted: {forged_fields:?}; owner sidecar rejected: {owner_tamper_rejected}"
    );
    let task_b_read = task_b
        .read(&ArtifactsReadRequest {
            artifact: task_b_reference,
            offset: 0,
            length: 0,
        })
        .expect("task b remains isolated from task a tampering");
    assert_eq!(task_b_read.total_bytes, 10);
}

#[tokio::test]
async fn plan_publication_uses_only_snapshot_material_and_never_approves_itself() {
    let temp = tempfile::tempdir().expect("tempdir");
    let database = temp.path().join("plans.sqlite3");
    let store = Arc::new(V1Store::open(&database).expect("store"));
    let id = identity("task-plan", "run-plan", "attempt-plan");
    let grants = vec![HostService::PlanPublish];
    let frozen = snapshot("task-plan", RunSnapshotPhase::Planning, &grants);
    let router = HostRouter::new(
        id.clone(),
        RunGuard::new(&id.run_id, 1),
        grants,
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        store.clone(),
        Arc::new(IgnoreQuestions),
    )
    .with_v1_store(store.clone())
    .with_plan_publication(
        frozen.clone(),
        TaskKind::PlanDraft,
        vec![
            "check:test".into(),
            "check:lint".into(),
            "check:test".into(),
        ],
        true,
    );
    let first_request = serde_json::json!({
        "revision": 1,
        "work_units": [{
            "id": "one",
            "description": "first unit",
            "dependencies": [],
            "acceptance": ["check:test"]
        }]
    });
    let first = router
        .handle_request(request("host.plan.publish", first_request.clone()))
        .await
        .expect("publish first");
    let replay = router
        .handle_request(request("host.plan.publish", first_request))
        .await
        .expect("idempotent replay");
    assert_eq!(first, replay);
    assert_eq!(row_count(&database, "plan_revisions"), 1);

    let persisted = store
        .current_plan_revision("task-plan")
        .expect("load plan")
        .expect("plan");
    let material = persisted.material();
    assert_eq!(material.revision, 1);
    assert_eq!(material.parent_revision, None);
    assert_eq!(
        material.workspace_baseline,
        frozen.material().workspace.baseline_sha256
    );
    assert_eq!(
        material.current_base_hash,
        frozen.material().workspace.baseline_sha256
    );
    assert_eq!(
        material.prompt_digest,
        frozen.material().prompt.content_sha256
    );
    assert_eq!(
        material.permission_digest,
        frozen.material().permissions.revision
    );
    assert_eq!(material.required_checks, vec!["check:lint", "check:test"]);
    assert_eq!(material.work_units[0].id, "one");
    assert_eq!(
        material.route_digest,
        canonical_input_hash(&serde_json::to_value(&frozen.material().provider).unwrap())
    );
    assert_eq!(
        material.check_digest,
        canonical_input_hash(&serde_json::json!(["check:lint", "check:test"]))
    );

    let second = router
        .handle_request(request(
            "host.plan.publish",
            serde_json::json!({
                "revision": 2,
                "work_units": [{
                    "id": "two",
                    "description": "second unit",
                    "dependencies": [],
                    "acceptance": ["check:lint"]
                }]
            }),
        ))
        .await
        .expect("publish second");
    assert_ne!(first["revisionHash"], second["revisionHash"]);
    let current = store
        .current_plan_revision("task-plan")
        .expect("current")
        .expect("head");
    assert_eq!(current.material().revision, 2);
    assert_eq!(
        current
            .material()
            .parent_revision
            .as_ref()
            .map(|value| value.as_str()),
        first["revisionHash"].as_str()
    );

    let approval_guess = router
        .handle_request(request("host.plan.approve", serde_json::json!({})))
        .await
        .expect_err("plugin cannot approve a plan");
    assert_eq!(approval_guess.code, -32601);
    assert_eq!(row_count(&database, "plan_approvals"), 0);

    let other_id = identity(
        "task-conversation",
        "run-conversation",
        "attempt-conversation",
    );
    let non_planning = HostRouter::new(
        other_id.clone(),
        RunGuard::new(&other_id.run_id, 1),
        vec![HostService::PlanPublish],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        store.clone(),
        Arc::new(IgnoreQuestions),
    )
    .with_v1_store(store)
    .with_plan_publication(
        snapshot(
            "task-conversation",
            RunSnapshotPhase::Planning,
            &[HostService::PlanPublish],
        ),
        TaskKind::Conversation,
        vec![],
        false,
    );
    let before = row_count(&database, "plan_revisions");
    let error = non_planning
        .handle_request(request(
            "host.plan.publish",
            serde_json::json!({"revision": 1, "work_units": []}),
        ))
        .await
        .expect_err("non-planning publish");
    assert_eq!(error.code, error_code::PROTOCOL_VIOLATION);
    assert_eq!(row_count(&database, "plan_revisions"), before);
}

#[tokio::test]
async fn questions_persist_before_waiting_and_enforce_run_and_task_ownership() {
    let temp = tempfile::tempdir().expect("tempdir");
    let database = temp.path().join("questions.sqlite3");
    let store = Arc::new(V1Store::open(&database).expect("store"));
    let id = identity("task-q", "run-q", "attempt-q");
    save_running_task(&store, "task-q", TaskKind::PlanDraft, &id).await;
    let sink = Arc::new(RecordingQuestions::default());
    let router = HostRouter::new(
        id.clone(),
        RunGuard::new(&id.run_id, 1),
        vec![HostService::QuestionsAsk],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        store.clone(),
        sink.clone(),
    )
    .with_v1_store(store.clone());

    router
        .handle_request(request(
            "host.questions.ask",
            serde_json::json!({"text": "nonblocking", "options": [], "blocking": false}),
        ))
        .await
        .expect("nonblocking question");
    assert!(matches!(
        store.load_task("task-q").await.unwrap().execution,
        TaskExecution::Running { .. }
    ));

    let blocking = router
        .handle_request(request(
            "host.questions.ask",
            serde_json::json!({"text": "blocking", "options": ["yes", "no"], "blocking": true}),
        ))
        .await
        .expect("blocking question");
    let blocking_id = blocking["question_id"].as_str().expect("question id");
    assert!(matches!(
        store.load_task("task-q").await.unwrap().execution,
        TaskExecution::WaitingInput { ref question_id, .. } if question_id == blocking_id
    ));
    let events = store.task_events("task-q");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "question.raised")
            .count(),
        2
    );
    assert_eq!(row_count(&database, "questions"), 2);
    assert_eq!(sink.raised.lock().expect("sink").len(), 2);
    drop(router);
    drop(store);

    let restarted = Arc::new(V1Store::open(&database).expect("restart store"));
    assert!(matches!(
        restarted.load_task("task-q").await.unwrap().execution,
        TaskExecution::WaitingInput { ref question_id, .. } if question_id == blocking_id
    ));
    assert_eq!(row_count(&database, "questions"), 2);

    let before_foreign = row_count(&database, "questions");
    for foreign in [
        identity("task-q", "run-foreign", "attempt-foreign"),
        identity("task-foreign", "run-foreign", "attempt-foreign"),
    ] {
        let foreign_router = HostRouter::new(
            foreign.clone(),
            RunGuard::new(&foreign.run_id, 1),
            vec![HostService::QuestionsAsk],
            Arc::new(FakeToolService::default()),
            Arc::new(FakeModelService::default()),
            Arc::new(FakeProcessService::default()),
            restarted.clone(),
            Arc::new(IgnoreQuestions),
        )
        .with_v1_store(restarted.clone());
        assert!(foreign_router
            .handle_request(request(
                "host.questions.ask",
                serde_json::json!({"text": "forged", "options": [], "blocking": true}),
            ))
            .await
            .is_err());
    }
    assert_eq!(
        row_count(&database, "questions"),
        before_foreign,
        "rejected cross-run/task questions must leave no rows"
    );
}

#[tokio::test]
async fn blocking_question_survives_observation_finalize_and_daemon_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.path().join("profile"))
            .with_ipc_name("t07-blocking-question"),
    )
    .expect("profile");
    let service = ApplicationService::compose(
        &profile,
        Arc::new(FakeModelService::default()),
        Arc::new(FakeToolService::default()),
    )
    .expect("compose");
    service
        .install_package_from_directory(&stage_blocking_question_harness(temp.path()))
        .expect("install question fixture");
    service
        .create_task("task-blocking", "", TaskKind::Conversation, vec![])
        .await
        .expect("create task");
    service
        .select_harness("task-blocking", "fixture.blocking-question")
        .await
        .expect("select fixture");
    service
        .send_message("task-blocking", "ask the question")
        .await
        .expect("send");
    let events = wait_for_journal_kind(&service, "task-blocking", "run.waiting_input").await;
    assert!(events.iter().any(|event| {
        event.task_id == "task-blocking"
            && event.payload.get("journalKind") == Some(&serde_json::json!("question.raised"))
    }));
    assert!(events.iter().any(|event| {
        event.task_id == "task-blocking"
            && event.payload.get("journalKind") == Some(&serde_json::json!("harness.progress"))
    }));
    assert!(!events.iter().any(|event| {
        event.task_id == "task-blocking"
            && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
    }));

    let store = V1Store::open(&profile.database_path()).expect("inspect store");
    let waiting_question = match store
        .load_task("task-blocking")
        .await
        .expect("persisted task")
        .execution
    {
        TaskExecution::WaitingInput { question_id, .. } => question_id,
        execution => panic!("blocking question was overwritten by finalize: {execution:?}"),
    };
    assert_eq!(row_count(&profile.database_path(), "questions"), 1);
    let persisted_question: (String, String, String) = Connection::open(profile.database_path())
        .expect("question database")
        .query_row(
            "SELECT task_id, run_id, state FROM questions WHERE question_id = ?1",
            [&waiting_question],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("durable question");
    assert_eq!(persisted_question.0, "task-blocking");
    assert_eq!(persisted_question.1, "run-task-blocking-1");
    assert_eq!(persisted_question.2, "open");
    drop(store);
    drop(service);

    let restarted = ApplicationService::compose(
        &profile,
        Arc::new(FakeModelService::default()),
        Arc::new(FakeToolService::default()),
    )
    .expect("restart service");
    let restarted_store = V1Store::open(&profile.database_path()).expect("restart store");
    assert!(matches!(
        restarted_store
            .load_task("task-blocking")
            .await
            .expect("task after restart")
            .execution,
        TaskExecution::WaitingInput { ref question_id, .. } if question_id == &waiting_question
    ));
    let replayed = restarted.events_after(0, 500).await;
    assert!(replayed.iter().any(|event| {
        event.task_id == "task-blocking"
            && event.payload.get("journalKind") == Some(&serde_json::json!("run.waiting_input"))
    }));
}

#[tokio::test]
async fn question_task_save_failure_is_reported_without_false_notification_or_waiting_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let database = temp.path().join("questions-fault.sqlite3");
    let store = Arc::new(V1Store::open(&database).expect("store"));
    let id = identity("task-fault", "run-fault", "attempt-fault");
    save_running_task(&store, "task-fault", TaskKind::PlanDraft, &id).await;
    let sink = Arc::new(RecordingQuestions::default());
    let router = HostRouter::new(
        id.clone(),
        RunGuard::new(&id.run_id, 1),
        vec![HostService::QuestionsAsk],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        store.clone(),
        sink.clone(),
    )
    .with_v1_store(store.clone());
    store.debug_fail_next_save();
    let error = router
        .handle_request(request(
            "host.questions.ask",
            serde_json::json!({"text": "must fail", "options": [], "blocking": true}),
        ))
        .await
        .expect_err("aggregate save fault");
    assert_eq!(error.code, error_code::INTERNAL);
    assert!(matches!(
        store.load_task("task-fault").await.unwrap().execution,
        TaskExecution::Running { .. }
    ));
    assert!(store.task_events("task-fault").is_empty());
    assert!(sink.raised.lock().expect("sink").is_empty());
    assert_eq!(
        row_count(&database, "questions"),
        1,
        "the question is durable before the waiting-state transaction"
    );
}

#[tokio::test]
async fn question_insert_failure_returns_error_and_leaves_task_running() {
    let temp = tempfile::tempdir().expect("tempdir");
    let database = temp.path().join("questions-insert-fault.sqlite3");
    let store = Arc::new(V1Store::open(&database).expect("store"));
    let id = identity(
        "task-insert-fault",
        "run-insert-fault",
        "attempt-insert-fault",
    );
    save_running_task(&store, "task-insert-fault", TaskKind::PlanDraft, &id).await;
    Connection::open(&database)
        .expect("open trigger connection")
        .execute_batch(
            "CREATE TRIGGER reject_questions BEFORE INSERT ON questions
             BEGIN SELECT RAISE(ABORT, 'injected question failure'); END;",
        )
        .expect("install failure trigger");
    let sink = Arc::new(RecordingQuestions::default());
    let router = HostRouter::new(
        id.clone(),
        RunGuard::new(&id.run_id, 1),
        vec![HostService::QuestionsAsk],
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        store.clone(),
        sink.clone(),
    )
    .with_v1_store(store.clone());
    let error = router
        .handle_request(request(
            "host.questions.ask",
            serde_json::json!({"text": "insert fails", "options": [], "blocking": true}),
        ))
        .await
        .expect_err("question insert fault");
    assert_eq!(error.code, error_code::INTERNAL);
    assert_eq!(row_count(&database, "questions"), 0);
    assert!(store.task_events("task-insert-fault").is_empty());
    assert!(sink.raised.lock().expect("sink").is_empty());
    assert!(matches!(
        store
            .load_task("task-insert-fault")
            .await
            .unwrap()
            .execution,
        TaskExecution::Running { .. }
    ));
}
