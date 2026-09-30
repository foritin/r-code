//! Conversation engine over the real native plugin process: multi-turn
//! resume-from-checkpoint, queue dispatch, cancellation and the client
//! views (`task.list` / `task.detail`) — the daemon-side core the TUI/GUI
//! chat switch (T35/T42) stands on.

use r_code_harness_protocol::services::{
    ModelStreamRequest, StreamPayload, ToolCallReply, ToolCallRequest, ToolDescriptor,
};
use r_code_harness_protocol::{OperationKey, StreamEvent};
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
    ToolService,
};
use r_code_kernel::task::{
    OperationReceipt, ReceiptOutcome, RunSnapshotId, TaskKind, TaskPreferences,
};
use r_code_runtime::application::{ApplicationService, CompositionPolicy};
use r_code_runtime::services::settings_store::SettingsStore;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

#[cfg(not(target_os = "macos"))]
use std::sync::Once;

#[cfg(not(target_os = "macos"))]
static INSTALL_MOCK_KEYRING: Once = Once::new();

#[cfg(not(target_os = "macos"))]
fn install_mock_keyring() {
    INSTALL_MOCK_KEYRING.call_once(|| {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
    });
}

#[cfg(target_os = "macos")]
fn install_mock_keyring() {}

/// Scripted model: replies `seen=<message-count>:<last-user-text>` so tests
/// can prove conversation history survived the run boundary (resume) and
/// identify which input produced which turn. The reply streams through the
/// sink (the router folds sink events into the assistant turn).
struct EchoModel {
    delay: Duration,
    captured_system_prompts: Option<Arc<std::sync::Mutex<Vec<String>>>>,
}

struct FailingModel;

#[async_trait::async_trait]
impl ModelService for FailingModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        _sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        Err(ServiceError::Failure("injected model failure".into()))
    }
}

#[async_trait::async_trait]
impl ModelService for EchoModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        if self.delay > Duration::ZERO {
            tokio::time::sleep(self.delay).await;
        }
        if let Some(captured) = &self.captured_system_prompts {
            let prompts = request
                .messages
                .iter()
                .filter(|message| {
                    matches!(
                        message.role,
                        r_code_harness_protocol::services::ModelRole::System
                    )
                })
                .flat_map(|message| message.content.iter())
                .filter_map(|block| match block {
                    r_code_harness_protocol::services::ContentBlock::Text { text } => {
                        Some(text.clone())
                    }
                    _ => None,
                });
            captured
                .lock()
                .expect("capture system prompt")
                .extend(prompts);
        }
        let user_texts: Vec<String> = request
            .messages
            .iter()
            .filter(|message| {
                matches!(
                    message.role,
                    r_code_harness_protocol::services::ModelRole::User
                )
            })
            .flat_map(|message| {
                message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        r_code_harness_protocol::services::ContentBlock::Text { text } => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let reply = format!(
            "seen={}:last={}",
            request.messages.len(),
            user_texts.last().cloned().unwrap_or_default()
        );
        sink.send(StreamEvent {
            stream_id: "scripted".into(),
            sequence: 1,
            payload: StreamPayload::TextDelta { text: reply },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: "scripted".into(),
            sequence: 2,
            payload: StreamPayload::Finish {
                reason: "end_turn".into(),
                usage: r_code_harness_protocol::ModelUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    cost_micros: None,
                },
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id: "scripted".into(),
            finish_reason: Some("end_turn".into()),
            usage: r_code_harness_protocol::ModelUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cost_micros: None,
            },
        })
    }
}

/// Records the exact host.model.stream request produced from the frozen
/// harness config. Unlike the production broker this model is deliberately
/// test-only and therefore eligible for the development fallback.
#[derive(Default)]
struct RecordingModel {
    requests: Mutex<Vec<ModelStreamRequest>>,
}

#[async_trait::async_trait]
impl ModelService for RecordingModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.requests
            .lock()
            .expect("record model request")
            .push(request);
        sink.send(StreamEvent {
            stream_id: "recording".into(),
            sequence: 1,
            payload: StreamPayload::TextDelta { text: "ok".into() },
            done: None,
        })
        .await?;
        let usage = r_code_harness_protocol::ModelUsage {
            input_tokens: Some(1),
            output_tokens: Some(1),
            cost_micros: None,
        };
        sink.send(StreamEvent {
            stream_id: "recording".into(),
            sequence: 2,
            payload: StreamPayload::Finish {
                reason: "end_turn".into(),
                usage,
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id: "recording".into(),
            finish_reason: Some("end_turn".into()),
            usage,
        })
    }
}

/// Holds only the first tool-catalog read so a test can mutate task
/// preferences after the run loaded its state but before it persists the
/// immutable snapshot. Subsequent plugin tool-list calls pass immediately.
struct FirstListGate {
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
    first: AtomicBool,
}

impl FirstListGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
            first: AtomicBool::new(true),
        })
    }

    async fn wait_until_frozen_inputs_are_in_flight(&self) {
        let permit = tokio::time::timeout(Duration::from_secs(20), self.entered.acquire())
            .await
            .expect("run never reached tool-catalog freeze")
            .expect("gate closed");
        permit.forget();
    }

    fn release(&self) {
        self.release.add_permits(1);
    }
}

#[async_trait::async_trait]
impl ToolService for FirstListGate {
    async fn list(&self, _token: GenerationToken) -> Result<Vec<ToolDescriptor>, ServiceError> {
        if self.first.swap(false, Ordering::SeqCst) {
            self.entered.add_permits(1);
            let permit = self
                .release
                .acquire()
                .await
                .map_err(|_| ServiceError::Failure("test gate closed".into()))?;
            permit.forget();
        }
        Ok(vec![ToolDescriptor {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }])
    }

    async fn call(
        &self,
        _token: GenerationToken,
        _call: ToolCallRequest,
    ) -> Result<ToolCallReply, ServiceError> {
        Err(ServiceError::Failure(
            "recording fixture does not call tools".into(),
        ))
    }
}

fn profile_for(name: &str, temp: &Path) -> RuntimeProfile {
    profile_for_flavor(name, temp, ProfileFlavor::Development)
}

fn profile_for_flavor(name: &str, temp: &Path, flavor: ProfileFlavor) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(flavor)
            .with_data_root(temp.join(name))
            .with_ipc_name(name),
    )
    .expect("profile")
}

fn native_binary() -> PathBuf {
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "r-code-harness-native"])
            .output()
            .expect("build native");
    assert!(output.status.success(), "native build failed");
    let exe = if cfg!(windows) {
        "r-code-harness-native.exe"
    } else {
        "r-code-harness-native"
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(exe)
}

/// Stage the native binary as an installable package.
fn stage_native(temp: &Path) -> PathBuf {
    let binary = native_binary();
    let source = temp.join("pkg-native");
    let bin_dir = source.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("dirs");
    std::fs::copy(&binary, bin_dir.join(binary.file_name().unwrap())).expect("copy");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    std::fs::write(
        source.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": "native.r-code",
            "version": "1.0.0",
            "apiMajor": 1,
            "apiMinor": 0,
            "displayName": "Native",
            "supportedPlatforms": [{"platform": platform, "executable": "bin/r-code-harness-native"}],
            "requestedHostServices": [
                "host.model.stream",
                "host.tools.list",
                "host.tools.call",
                "host.checkpoint.save",
                "host.completion.propose"
            ],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");
    source
}

/// Poll the journal until an event matching `pred` appears (or timeout).
async fn wait_for_event(
    service: &ApplicationService,
    pred: impl Fn(&r_code_harness_protocol::EventEnvelope) -> bool,
    timeout: Duration,
) -> Vec<r_code_harness_protocol::EventEnvelope> {
    let deadline = Instant::now() + timeout;
    loop {
        let events = service.events_after(0, 500).await;
        if events.iter().any(&pred) {
            return events;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for event; journal: {:?}",
                events
                    .iter()
                    .map(|e| e.payload.get("journalKind"))
                    .collect::<Vec<_>>()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn assert_failed_before_dispatch(
    service: &ApplicationService,
    profile: &RuntimeProfile,
    package: &Path,
    task_id: &str,
    kind: TaskKind,
    preferences: Option<TaskPreferences>,
) {
    service.ensure_builtin(package).expect("install native");
    service
        .create_task(task_id, "", kind, vec![])
        .await
        .expect("create task");
    if let Some(preferences) = preferences {
        service
            .set_task_preferences(task_id, preferences)
            .await
            .expect("set task preferences");
    }

    service
        .send_message(task_id, "must fail before dispatch")
        .await
        .expect("enqueue input");
    let events = wait_for_event(
        service,
        |event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.failed"))
        },
        Duration::from_secs(30),
    )
    .await;
    assert!(
        !events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.started"))
        }),
        "pre-dispatch failure must not publish run.started"
    );
    assert!(
        service
            .task_detail(task_id)
            .await
            .expect("task detail")
            .runs
            .is_empty(),
        "a failed pre-dispatch run must not project as a started run"
    );

    let connection = rusqlite::Connection::open(profile.database_path()).expect("open v1 database");
    let snapshot_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM run_snapshots WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .expect("count snapshots");
    assert_eq!(
        snapshot_count, 0,
        "invalid material must fail before snapshot persistence"
    );
    let pin_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM plugin_pins WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .expect("count plugin pins");
    assert_eq!(
        pin_count, 0,
        "invalid material must fail before pre-spawn package pinning"
    );
}

fn request_system_texts(request: &ModelStreamRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|message| {
            matches!(
                message.role,
                r_code_harness_protocol::services::ModelRole::System
            )
        })
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            r_code_harness_protocol::services::ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn multi_turn_conversation_resumes_across_runs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("multiturn", temp.path());
    let captured_system_prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let models: Arc<dyn ModelService> = Arc::new(EchoModel {
        delay: Duration::ZERO,
        captured_system_prompts: Some(captured_system_prompts.clone()),
    });
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");

    service
        .create_task("chat-1", "", TaskKind::Conversation, vec![])
        .await
        .expect("create");
    service
        .set_task_preferences(
            "chat-1",
            r_code_kernel::task::TaskPreferences {
                model_route: None,
                model: None,
                inference: None,
                mode: Some("edit".into()),
                system_prompt: Some("custom project prompt".into()),
                workspace_path: Some("D:/workspace/project".into()),
                require_desktop_confirm: false,
            },
        )
        .await
        .expect("prompt preferences");

    // Turn 1: seeds the conversation.
    let first = service
        .send_message("chat-1", "hello first")
        .await
        .expect("send 1");
    assert_eq!(first["started"], true);
    let events = wait_for_event(
        &service,
        |event| event.payload.get("journalKind") == Some(&serde_json::json!("run.completed")),
        Duration::from_secs(30),
    )
    .await;
    assert!(
        events.iter().any(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("assistant.message"))
                && event.payload["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("hello first"))
        }),
        "assistant turn journaled: {:?}",
        events
            .iter()
            .map(|event| event.payload.get("journalKind"))
            .collect::<Vec<_>>()
    );
    assert!(
        captured_system_prompts
            .lock()
            .expect("captured prompt")
            .iter()
            .any(|prompt| prompt == "custom project prompt"),
        "task prompt must reach the native model request"
    );

    // Turn 2 on the same task: a fresh plugin process resumes from the
    // checkpoint — the model request must carry the first turn's history.
    service
        .send_message("chat-1", "second question")
        .await
        .expect("send 2");
    let events = wait_for_event(
        &service,
        |event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
                && event
                    .payload
                    .get("runId")
                    .and_then(|value| value.as_str())
                    .is_some_and(|run_id| run_id.ends_with("-2"))
        },
        Duration::from_secs(30),
    )
    .await;
    let turn_two = events
        .iter()
        .filter(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("assistant.message"))
        })
        .map(|event| event.payload["text"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert!(
        turn_two.iter().any(|text| text.contains("second question")),
        "second turn journaled: {turn_two:?}"
    );
    // History carried: the second request saw the first turn's messages
    // (resume worked) — run 2 projects system + user1 + assistant1 + user2.
    let history_proven = turn_two.iter().any(|text| {
        text.split(':')
            .next()
            .and_then(|head| head.trim_start_matches("seen=").parse::<usize>().ok())
            .is_some_and(|count| count >= 4)
    });
    assert!(
        history_proven,
        "resumed conversation carries history: {turn_two:?}"
    );

    // Views: two completed runs, usage aggregated, assistant text counted.
    let detail = service.task_detail("chat-1").await.expect("detail");
    assert_eq!(
        detail.workspace_path.as_deref(),
        Some("D:/workspace/project")
    );
    assert_eq!(detail.runs.len(), 2);
    assert!(detail.runs.iter().all(|run| run.outcome == "completed"));
    let persisted = r_code_store::v1::V1Store::open(&profile.database_path())
        .expect("open snapshot repository");
    for run in &detail.runs {
        let projected = run.snapshot_id.as_deref().expect("projected snapshot id");
        let snapshot_id = RunSnapshotId::parse(projected).expect("valid snapshot id");
        let snapshot = persisted
            .load_run_snapshot(&snapshot_id)
            .expect("load snapshot")
            .expect("snapshot exists before run.started is visible");
        assert_eq!(snapshot.id(), &snapshot_id);
        let started = events.iter().find(|event| {
            event.task_id == "chat-1"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.started"))
                && event.payload.get("runId").and_then(|value| value.as_str())
                    == Some(run.run_id.as_str())
        });
        assert_eq!(
            started
                .and_then(|event| event.payload.get("snapshotId"))
                .and_then(|value| value.as_str()),
            Some(projected),
            "run.started, TaskRunView and the durable repository must name one snapshot"
        );
    }
    assert!(detail.usage.input_tokens >= 20);
    let list = service.list_tasks().await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].task_id, "chat-1");
    assert!(!list[0].running);

    // Rename reflects in views.
    service
        .rename_task("chat-1", "重命名会话")
        .await
        .expect("rename");
    let detail = service.task_detail("chat-1").await.expect("detail");
    assert_eq!(detail.title, "重命名会话");
}

#[tokio::test]
async fn production_provider_errors_fail_before_snapshot_or_run_started() {
    install_mock_keyring();
    let temp = tempfile::tempdir().expect("tempdir");
    let package = stage_native(temp.path());

    let missing = profile_for_flavor("provider-missing", temp.path(), ProfileFlavor::Production);
    let service = ApplicationService::compose_with_policy(
        &missing,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
        CompositionPolicy::StrictDaemon,
    )
    .expect("compose missing-provider service");
    assert_failed_before_dispatch(
        &service,
        &missing,
        &package,
        "missing-provider",
        TaskKind::Conversation,
        None,
    )
    .await;

    let corrupt = profile_for_flavor("provider-corrupt", temp.path(), ProfileFlavor::Production);
    std::fs::create_dir_all(corrupt.harness_v1_root()).expect("settings root");
    std::fs::write(
        corrupt.harness_v1_root().join("settings.json"),
        br#"{"providers":[}"#,
    )
    .expect("corrupt settings fixture");
    let service = ApplicationService::compose_with_policy(
        &corrupt,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
        CompositionPolicy::StrictDaemon,
    )
    .expect("compose corrupt-provider service");
    assert_failed_before_dispatch(
        &service,
        &corrupt,
        &package,
        "corrupt-provider",
        TaskKind::Conversation,
        None,
    )
    .await;

    let credential = profile_for_flavor(
        "provider-credential-missing",
        temp.path(),
        ProfileFlavor::Production,
    );
    let missing_env = format!("R_CODE_T03_MISSING_KEY_{}", uuid::Uuid::new_v4().simple());
    std::env::remove_var(&missing_env);
    SettingsStore::for_profile(&credential)
        .compare_and_swap(
            0,
            r_code_runtime::services::settings_store::V1Settings {
                revision: 0,
                providers: vec![r_code_runtime::services::settings_store::ProviderEntry {
                    selection: "deepseek".into(),
                    model: "deepseek-v4-flash".into(),
                    base_url: None,
                    protocol: None,
                    env_var: Some(missing_env),
                }],
                default_selection: Some("deepseek".into()),
            },
        )
        .expect("persist missing-credential provider");
    let service = ApplicationService::compose_with_policy(
        &credential,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
        CompositionPolicy::StrictDaemon,
    )
    .expect("compose missing-credential service");
    assert_failed_before_dispatch(
        &service,
        &credential,
        &package,
        "missing-credential",
        TaskKind::Conversation,
        None,
    )
    .await;
}

#[tokio::test]
async fn development_profile_is_fail_closed_under_real_daemon_policy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("development-strict-daemon", temp.path());
    let service = ApplicationService::compose_with_policy(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
        CompositionPolicy::StrictDaemon,
    )
    .expect("compose strict development daemon");
    assert_failed_before_dispatch(
        &service,
        &profile,
        &stage_native(temp.path()),
        "development-without-provider",
        TaskKind::Conversation,
        None,
    )
    .await;
}

#[tokio::test]
async fn code_tasks_reject_missing_invalid_and_foreign_workspaces_before_dispatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let package = stage_native(temp.path());

    let missing = profile_for("workspace-missing", temp.path());
    let service = ApplicationService::compose(
        &missing,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose missing-workspace service");
    assert_failed_before_dispatch(
        &service,
        &missing,
        &package,
        "implementation-unbound",
        TaskKind::Implementation,
        None,
    )
    .await;

    let invalid = profile_for("workspace-invalid", temp.path());
    let service = ApplicationService::compose(
        &invalid,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose invalid-workspace service");
    assert_failed_before_dispatch(
        &service,
        &invalid,
        &package,
        "repair-invalid",
        TaskKind::Repair,
        Some(TaskPreferences {
            workspace_path: Some(temp.path().join("does-not-exist").display().to_string()),
            ..TaskPreferences::default()
        }),
    )
    .await;

    let foreign_root = temp.path().join("foreign-linked-worktree");
    std::fs::create_dir_all(&foreign_root).expect("foreign root");
    std::fs::write(
        foreign_root.join(".git"),
        "gitdir: ../main/.git/worktrees/foreign",
    )
    .expect("linked-worktree marker");
    let foreign = profile_for("workspace-foreign", temp.path());
    let service = ApplicationService::compose(
        &foreign,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose foreign-workspace service");
    assert_failed_before_dispatch(
        &service,
        &foreign,
        &package,
        "implementation-foreign",
        TaskKind::Implementation,
        Some(TaskPreferences {
            workspace_path: Some(foreign_root.display().to_string()),
            ..TaskPreferences::default()
        }),
    )
    .await;
}

#[tokio::test]
async fn conversation_and_plan_draft_persist_explicit_unbound_read_only_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("workspace-unbound-read-only", temp.path());
    let service = ApplicationService::compose(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose service");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");

    for (task_id, kind) in [
        ("unbound-conversation", TaskKind::Conversation),
        ("unbound-plan", TaskKind::PlanDraft),
    ] {
        service
            .create_task(task_id, "", kind, vec![])
            .await
            .expect("create read-only task");
        service
            .send_message(task_id, "read only")
            .await
            .expect("send read-only task");
        let events = wait_for_event(
            &service,
            |event| {
                event.task_id == task_id
                    && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
            },
            Duration::from_secs(30),
        )
        .await;
        let snapshot_id = events
            .iter()
            .find(|event| {
                event.task_id == task_id
                    && event.payload.get("journalKind") == Some(&serde_json::json!("run.started"))
            })
            .and_then(|event| event.payload.get("snapshotId"))
            .and_then(|value| value.as_str())
            .expect("started snapshot id");
        let store =
            r_code_store::v1::V1Store::open(&profile.database_path()).expect("open snapshot store");
        let snapshot = store
            .load_run_snapshot(RunSnapshotId::parse(snapshot_id).expect("snapshot id"))
            .expect("load snapshot")
            .expect("snapshot row");
        assert_eq!(
            snapshot.material().workspace.canonical_root,
            "unbound://read-only"
        );
        assert_eq!(
            snapshot.material().workspace.workspace_identity,
            "unbound-read-only"
        );
    }
}

#[tokio::test]
async fn active_run_uses_frozen_preferences_and_changed_snapshot_has_safe_attempt_namespace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("frozen-preferences", temp.path());
    let model = Arc::new(RecordingModel::default());
    let gate = FirstListGate::new();
    let service = ApplicationService::compose(&profile, model.clone(), gate.clone())
        .expect("compose frozen-preferences service");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    service
        .create_task("frozen-prefs", "", TaskKind::Conversation, vec![])
        .await
        .expect("create task");

    let initial = TaskPreferences {
        inference: Some(serde_json::json!({"temperature": 0.1})),
        system_prompt: Some("prompt-before-run".into()),
        ..TaskPreferences::default()
    };
    service
        .set_task_preferences("frozen-prefs", initial)
        .await
        .expect("initial preferences");
    service
        .send_message("frozen-prefs", "first")
        .await
        .expect("start first run");
    gate.wait_until_frozen_inputs_are_in_flight().await;

    let changed = TaskPreferences {
        inference: Some(serde_json::json!({"temperature": 0.9})),
        system_prompt: Some("prompt-for-next-run".into()),
        ..TaskPreferences::default()
    };
    let busy = service
        .set_task_preferences("frozen-prefs", changed.clone())
        .await
        .expect_err("material preferences must not change while dispatch is starting");
    assert!(
        busy.to_string().contains("busy") || busy.to_string().contains("active"),
        "unexpected busy error: {busy}"
    );
    gate.release();
    wait_for_event(
        &service,
        |event| {
            event.task_id == "frozen-prefs"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
                && event.payload.get("runId").and_then(|value| value.as_str())
                    == Some("run-frozen-prefs-1")
        },
        Duration::from_secs(30),
    )
    .await;

    service
        .set_task_preferences("frozen-prefs", changed.clone())
        .await
        .expect("update preferences after the active run settles");

    let first_request = model
        .requests
        .lock()
        .expect("recorded requests")
        .first()
        .cloned()
        .expect("first model request");
    assert_eq!(
        first_request.inference,
        Some(serde_json::json!({"temperature": 0.1}))
    );
    assert!(request_system_texts(&first_request)
        .iter()
        .any(|text| text == "prompt-before-run"));
    assert!(!request_system_texts(&first_request)
        .iter()
        .any(|text| text == "prompt-for-next-run"));
    assert_eq!(
        service
            .task_preferences("frozen-prefs")
            .await
            .expect("persisted preferences"),
        changed,
        "a stale run aggregate must not overwrite preferences changed while it was active"
    );
}

#[tokio::test]
async fn different_snapshots_do_not_share_an_unrecoverable_attempt_namespace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("snapshot-attempt-namespace", temp.path());
    let service = ApplicationService::compose(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose service");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    service
        .create_task("snapshot-attempt", "", TaskKind::Conversation, vec![])
        .await
        .expect("create task");
    service
        .set_task_preferences(
            "snapshot-attempt",
            TaskPreferences {
                system_prompt: Some("snapshot-one".into()),
                ..TaskPreferences::default()
            },
        )
        .await
        .expect("first preferences");
    service
        .send_message("snapshot-attempt", "first")
        .await
        .expect("first run");
    let first_events = wait_for_event(
        &service,
        |event| {
            event.task_id == "snapshot-attempt"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
                && event.payload.get("runId").and_then(|value| value.as_str())
                    == Some("run-snapshot-attempt-1")
        },
        Duration::from_secs(30),
    )
    .await;
    let first_started = first_events
        .iter()
        .find(|event| {
            event.task_id == "snapshot-attempt"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.started"))
        })
        .expect("first run.started");
    let first_attempt = first_started.payload["attemptId"]
        .as_str()
        .expect("first attempt id")
        .to_string();
    let store =
        r_code_store::v1::V1Store::open(&profile.database_path()).expect("open durable run store");
    let first_checkpoint = store
        .load_latest_checkpoint(&first_attempt)
        .await
        .expect("first attempt checkpoint");
    let sentinel_key = OperationKey::new("qa-snapshot-isolation");
    store
        .save_receipt(OperationReceipt {
            attempt_id: first_attempt.clone(),
            operation_key: sentinel_key.clone(),
            method: "qa.snapshot-isolation".into(),
            input_hash: "sha256:qa-sentinel".into(),
            outcome: ReceiptOutcome::Completed {
                result: serde_json::json!({"owner": "first-attempt"}),
            },
        })
        .await
        .expect("seed first-attempt receipt");

    service
        .set_task_preferences(
            "snapshot-attempt",
            TaskPreferences {
                system_prompt: Some("snapshot-two".into()),
                ..TaskPreferences::default()
            },
        )
        .await
        .expect("second preferences");
    service
        .send_message("snapshot-attempt", "second")
        .await
        .expect("second run");
    let events = wait_for_event(
        &service,
        |event| {
            event.task_id == "snapshot-attempt"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
                && event.payload.get("runId").and_then(|value| value.as_str())
                    == Some("run-snapshot-attempt-2")
        },
        Duration::from_secs(30),
    )
    .await;
    let started = events
        .iter()
        .filter(|event| {
            event.task_id == "snapshot-attempt"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.started"))
        })
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 2);
    let snapshots = started
        .iter()
        .map(|event| event.payload["snapshotId"].as_str().expect("snapshot id"))
        .collect::<Vec<_>>();
    assert_ne!(snapshots[0], snapshots[1]);
    let attempts = started
        .iter()
        .map(|event| event.payload["attemptId"].as_str().expect("attempt id"))
        .collect::<Vec<_>>();
    assert_eq!(attempts[0], first_attempt);
    assert_ne!(
        attempts[0], attempts[1],
        "each run must have a distinct attempt namespace"
    );
    for (attempt, snapshot) in attempts.iter().zip(&snapshots) {
        let suffix = snapshot
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect::<String>();
        assert!(
            attempt.ends_with(&suffix),
            "attempt identity must visibly bind its frozen snapshot: {attempt} / {snapshot}"
        );
    }

    let second_attempt = attempts[1];
    assert!(
        store
            .load_receipt(&first_attempt, &sentinel_key)
            .await
            .is_some(),
        "the original receipt remains in its original namespace"
    );
    assert!(
        store
            .load_receipt(second_attempt, &sentinel_key)
            .await
            .is_none(),
        "a new run must not replay receipts from the previous snapshot attempt"
    );

    let connection = rusqlite::Connection::open(profile.database_path()).expect("open v1 database");
    let (handoff_state, handoff_input_seq, handoff_blob): (Vec<u8>, i64, String) = connection
        .query_row(
            "SELECT state, consumed_input_seq, blob_id FROM checkpoints
             WHERE attempt_id = ?1 AND revision = 0",
            [second_attempt],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("revision-zero checkpoint handoff");
    assert_eq!(handoff_state, first_checkpoint.state);
    assert_eq!(
        handoff_input_seq as u64,
        first_checkpoint.consumed_input_seq
    );
    assert!(handoff_blob.contains(second_attempt));
    assert!(
        store.load_latest_checkpoint(second_attempt).await.is_some(),
        "the resumed run must persist a checkpoint in only its new attempt namespace"
    );
    let pin_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM plugin_pins WHERE attempt_id IN (?1, ?2)",
            [attempts[0], attempts[1]],
            |row| row.get(0),
        )
        .expect("count attempt package pins");
    assert_eq!(pin_count, 2, "each run attempt pins the exact package");
}

#[tokio::test]
async fn queued_message_dispatches_after_active_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("queue", temp.path());
    let models: Arc<dyn ModelService> = Arc::new(EchoModel {
        delay: Duration::from_millis(1200),
        captured_system_prompts: None,
    });
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    service
        .create_task("chat-q", "", TaskKind::Conversation, vec![])
        .await
        .expect("create");

    let first = service
        .send_message("chat-q", "slow one")
        .await
        .expect("send 1");
    assert_eq!(first["started"], true);
    // While the slow run is active, the second message queues.
    let second = service
        .send_message("chat-q", "follow up")
        .await
        .expect("send 2");
    assert_eq!(second["queued"], true);

    let events = wait_for_event(
        &service,
        |event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
                && event
                    .payload
                    .get("runId")
                    .and_then(|value| value.as_str())
                    .is_some_and(|run_id| run_id.ends_with("-2"))
        },
        Duration::from_secs(30),
    )
    .await;
    assert!(
        events.iter().any(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("assistant.message"))
                && event.payload["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("follow up"))
        }),
        "queued follow-up ran after the active run"
    );
    let detail = service.task_detail("chat-q").await.expect("detail");
    assert_eq!(detail.runs.len(), 2);
}

#[tokio::test]
async fn cancel_settles_the_run_as_cancelled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("cancel", temp.path());
    let models: Arc<dyn ModelService> = Arc::new(EchoModel {
        delay: Duration::from_secs(8),
        captured_system_prompts: None,
    });
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    service
        .create_task("chat-c", "", TaskKind::Conversation, vec![])
        .await
        .expect("create");

    service
        .send_message("chat-c", "long running")
        .await
        .expect("send");
    wait_for_event(
        &service,
        |event| event.payload.get("journalKind") == Some(&serde_json::json!("run.started")),
        Duration::from_secs(20),
    )
    .await;
    let cancelled = service.cancel_task("chat-c").await.expect("cancel");
    assert!(cancelled, "an active run was cancelled");
    let events = wait_for_event(
        &service,
        |event| event.payload.get("journalKind") == Some(&serde_json::json!("run.cancelled")),
        Duration::from_secs(20),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|event| event.payload.get("journalKind")
                == Some(&serde_json::json!("run.cancelled")))
    );
    let transcript_path = profile.harness_v1_root().join("transcripts").join(format!(
        "{}.jsonl",
        r_code_runtime::services::artifacts::sha256_hex(b"chat-c")
    ));
    assert_eq!(
        std::fs::read_to_string(&transcript_path).expect("cancelled transcript"),
        "",
        "a cancelled run must roll back only the transcript prefix it appended"
    );

    // The task reopens for the next input (chat semantics).
    service
        .send_message("chat-c", "after cancel")
        .await
        .expect("send after cancel");
    wait_for_event(
        &service,
        |event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
                && event
                    .payload
                    .get("runId")
                    .and_then(|value| value.as_str())
                    .is_some_and(|run_id| run_id.ends_with("-2"))
        },
        Duration::from_secs(30),
    )
    .await;
    let transcript = std::fs::read_to_string(transcript_path).expect("successful transcript");
    assert!(transcript.contains("after cancel"));
    assert!(!transcript.contains("long running"));
}

#[tokio::test]
async fn failed_run_rolls_back_its_transcript_prefix_and_restart_keeps_later_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("failed-transcript", temp.path());
    let service = ApplicationService::compose(
        &profile,
        Arc::new(FailingModel),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose failing service");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    service
        .create_task("chat-failed-transcript", "", TaskKind::Conversation, vec![])
        .await
        .expect("create task");
    service
        .send_message("chat-failed-transcript", "must roll back")
        .await
        .expect("enqueue failing run");
    wait_for_event(
        &service,
        |event| event.payload.get("journalKind") == Some(&serde_json::json!("run.failed")),
        Duration::from_secs(30),
    )
    .await;

    let transcript_path = profile.harness_v1_root().join("transcripts").join(format!(
        "{}.jsonl",
        r_code_runtime::services::artifacts::sha256_hex(b"chat-failed-transcript")
    ));
    assert_eq!(
        std::fs::read_to_string(&transcript_path).expect("failed transcript"),
        ""
    );
    drop(service);

    let restarted = ApplicationService::compose(
        &profile,
        Arc::new(EchoModel {
            delay: Duration::ZERO,
            captured_system_prompts: None,
        }),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("restart service");
    restarted
        .send_message("chat-failed-transcript", "kept after restart")
        .await
        .expect("send successful retry");
    wait_for_event(
        &restarted,
        |event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
                && event.task_id == "chat-failed-transcript"
        },
        Duration::from_secs(30),
    )
    .await;
    let transcript = std::fs::read_to_string(transcript_path).expect("restarted transcript");
    assert!(transcript.contains("kept after restart"));
    assert!(!transcript.contains("must roll back"));
}
