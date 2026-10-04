//! A11 — turn budget relay via Continuation inputs.
//! macOS：daemon→native harness 链路依赖平台安全激活报告（P13），本 wave
//! 报告后端固定 none-this-wave/Unsupported——链路在 macOS 按设计拒绝启动；
//! 端到端用例由 linux/windows 腿运行，P13 报告落地后移除此门。
#![cfg(not(target_os = "macos"))]
// 脚手架按模板复制，各用例只取子集，未用项不逐个删。
#![allow(dead_code)]

use r_code_harness_protocol::services::ModelStreamRequest;
use r_code_harness_protocol::{ModelUsage, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink, ToolService,
};
use r_code_kernel::task::TaskKind;
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[derive(Clone)]
enum ScriptedTurn {
    Err(String),
    Clean(String),
    MaxTokens(String),
}

struct ScriptedStreamModel {
    outcomes: Mutex<Vec<ScriptedTurn>>,
    calls: AtomicUsize,
}

impl ScriptedStreamModel {
    fn new(outcomes: Vec<ScriptedTurn>) -> Arc<Self> {
        Arc::new(Self {
            outcomes: Mutex::new(outcomes),
            calls: AtomicUsize::new(0),
        })
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn usage() -> ModelUsage {
    ModelUsage {
        input_tokens: Some(10),
        output_tokens: Some(5),
        cost_micros: None,
    }
}

async fn send_turn(
    sink: &mut dyn StreamSink,
    text: &str,
    reason: &str,
) -> Result<(), ServiceError> {
    sink.send(StreamEvent {
        stream_id: "scripted".into(),
        sequence: 1,
        payload: StreamPayload::TextDelta { text: text.into() },
        done: None,
    })
    .await?;
    sink.send(StreamEvent {
        stream_id: "scripted".into(),
        sequence: 2,
        payload: StreamPayload::Finish {
            reason: reason.into(),
            usage: usage(),
        },
        done: Some(true),
    })
    .await?;
    Ok(())
}

#[async_trait::async_trait]
impl ModelService for ScriptedStreamModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or(ScriptedTurn::Clean("fallback".into()));
        if self.outcomes.lock().unwrap().len() > 1 {
            self.outcomes.lock().unwrap().remove(0);
        }
        match outcome {
            ScriptedTurn::Err(message) => Err(ServiceError::Failure(message)),
            ScriptedTurn::Clean(text) => {
                send_turn(sink, &text, "end_turn").await?;
                Ok(ModelStreamOutcome {
                    stream_id: "scripted".into(),
                    finish_reason: Some("end_turn".into()),
                    usage: usage(),
                    reasoning: None,
                })
            }
            ScriptedTurn::MaxTokens(text) => {
                send_turn(sink, &text, "max_tokens").await?;
                Ok(ModelStreamOutcome {
                    stream_id: "scripted".into(),
                    finish_reason: Some("max_tokens".into()),
                    usage: usage(),
                    reasoning: None,
                })
            }
        }
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
            "apiMinor": 3,
            "requiresSingleProcess": true,
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

async fn wait_for_event(
    service: &ApplicationService,
    task_id: &str,
    kind: &str,
    timeout: Duration,
) -> Vec<r_code_harness_protocol::EventEnvelope> {
    let deadline = Instant::now() + timeout;
    loop {
        let events = service.events_after(0, 500).await;
        if events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!(kind))
        }) {
            return events;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {kind}; journal kinds: {:?}",
                events
                    .iter()
                    .map(|e| e.payload.get("journalKind"))
                    .collect::<Vec<_>>()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_scripted(
    name: &str,
    outcomes: Vec<ScriptedTurn>,
) -> (
    Arc<ScriptedStreamModel>,
    Vec<r_code_harness_protocol::EventEnvelope>,
    tempfile::TempDir,
) {
    let temp = tempdir().expect("tempdir");
    let profile = profile_for(name, temp.path());
    let models = ScriptedStreamModel::new(outcomes.clone());
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models.clone(), tools).expect("compose");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    let task_id = "chat-run";
    service
        .create_task(task_id, "", TaskKind::Conversation, vec![])
        .await
        .expect("create");
    service.send_message(task_id, "hello").await.expect("send");
    let events = wait_for_event(
        &service,
        task_id,
        if outcomes.iter().all(|o| matches!(o, ScriptedTurn::Err(_))) {
            "run.failed"
        } else {
            "run.completed"
        },
        Duration::from_secs(60),
    )
    .await;
    (models, events, temp)
}
/// Every turn requests one read_file call — the loop only ends via budget.
struct AlwaysToolModel {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl ModelService for AlwaysToolModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        sink.send(StreamEvent {
            stream_id: "s".into(),
            sequence: 1,
            payload: StreamPayload::ToolCallDelta {
                id: format!("t{n}"),
                name: "read_file".into(),
                partial_input: String::new(),
            },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: "s".into(),
            sequence: 2,
            payload: StreamPayload::ToolCallDelta {
                id: format!("t{n}"),
                name: String::new(),
                partial_input: "{}".into(),
            },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: "s".into(),
            sequence: 3,
            payload: StreamPayload::Finish {
                reason: "tool_use".into(),
                usage: usage(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id: "s".into(),
            finish_reason: Some("tool_use".into()),
            usage: usage(),
            reasoning: None,
        })
    }
}

#[tokio::test]
async fn a11_thirty_turn_task_relays_without_error() {
    let temp = tempdir().expect("tempdir");
    let profile = profile_for("a11relay", temp.path());
    let models = Arc::new(AlwaysToolModel {
        calls: AtomicUsize::new(0),
    });
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    let task_id = "chat-a11";
    service
        .create_task(task_id, "", TaskKind::Conversation, vec![])
        .await
        .expect("create");
    service
        .send_message(task_id, "keep working")
        .await
        .expect("send");

    let deadline = Instant::now() + Duration::from_secs(120);
    let events = loop {
        let events = service.events_after(0, 1_000).await;
        let chained = events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.chained"))
        });
        let failed = events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.failed"))
        });
        if chained || failed || Instant::now() >= deadline {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(
        events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.chained"))
        }),
        "budget exhaustion must relay, not fail; journal: {:?}",
        events
            .iter()
            .map(|e| e.payload.get("journalKind"))
            .collect::<Vec<_>>()
    );
    assert!(
        !events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.failed"))
        }),
        "no TurnLimit-style failure may surface"
    );
    let continuation = events.iter().find(|event| {
        event.payload.get("journalKind") == Some(&serde_json::json!("input.queued"))
            && event.payload.get("kind") == Some(&serde_json::json!("continuation"))
    });
    assert!(
        continuation.is_some(),
        "relay enqueues a Continuation input (kebab-case kind)"
    );
}
