//! Conversation engine over the real native plugin process: multi-turn
//! resume-from-checkpoint, queue dispatch, cancellation and the client
//! views (`task.list` / `task.detail`) — the daemon-side core the TUI/GUI
//! chat switch (T35/T42) stands on.

use r_code_harness_protocol::services::{ModelStreamRequest, StreamPayload};
use r_code_harness_protocol::StreamEvent;
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink, ToolService,
};
use r_code_kernel::task::TaskKind;
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Scripted model: replies `seen=<message-count>:<last-user-text>` so tests
/// can prove conversation history survived the run boundary (resume) and
/// identify which input produced which turn. The reply streams through the
/// sink (the router folds sink events into the assistant turn).
struct EchoModel {
    delay: Duration,
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

fn profile_for(name: &str, temp: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
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

#[tokio::test]
async fn multi_turn_conversation_resumes_across_runs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("multiturn", temp.path());
    let models: Arc<dyn ModelService> = Arc::new(EchoModel {
        delay: Duration::ZERO,
    });
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    service
        .install_package_from_directory(&stage_native(temp.path()))
        .expect("install native");

    service
        .create_task("chat-1", "", TaskKind::Conversation, vec![])
        .await
        .expect("create");

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
    assert_eq!(detail.runs.len(), 2);
    assert!(detail.runs.iter().all(|run| run.outcome == "completed"));
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
async fn queued_message_dispatches_after_active_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("queue", temp.path());
    let models: Arc<dyn ModelService> = Arc::new(EchoModel {
        delay: Duration::from_millis(1200),
    });
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    service
        .install_package_from_directory(&stage_native(temp.path()))
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
    });
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    service
        .install_package_from_directory(&stage_native(temp.path()))
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
}
