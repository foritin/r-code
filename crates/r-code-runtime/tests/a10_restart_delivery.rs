//! A10 — daemon restart reseeds undelivered inputs (G12 e2e).
//! macOS：daemon→native harness 链路依赖平台安全激活报告（P13），本 wave
//! 报告后端固定 none-this-wave/Unsupported——链路在 macOS 按设计拒绝启动；
//! 端到端用例由 linux/windows 腿运行，P13 报告落地后移除此门。
#![cfg(not(target_os = "macos"))]

//!
//! A stranded `input.queued` journal row (written by a previous process that
//! died before dispatch) must dispatch after restart: the new drive loop's
//! reseed hook rebuilds the in-memory queue from the store when a poll comes
//! back empty while the store still holds pending inputs.

use r_code_harness_protocol::services::ModelStreamRequest;
use r_code_harness_protocol::{ModelUsage, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink, ToolService,
};
use r_code_kernel::task::TaskKind;
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Echo model: replies `seen=<messages>:last=<text>` so each run identifies
/// the input it served.
struct EchoModel {
    requests: Mutex<Vec<String>>,
}

fn usage() -> ModelUsage {
    ModelUsage {
        input_tokens: Some(10),
        output_tokens: Some(5),
        cost_micros: None,
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
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::to_string(&request).unwrap_or_default());
        // 双端迭代器上 last() 会全量遍历——rev().next() 直接取末个用户文本。
        let last_user = request
            .messages
            .iter()
            .filter(|message| {
                matches!(
                    message.role,
                    r_code_harness_protocol::services::ModelRole::User
                )
            })
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                r_code_harness_protocol::services::ContentBlock::Text { text } => {
                    Some(text.clone())
                }
                _ => None,
            })
            .next_back()
            .unwrap_or_default();
        let reply = format!("seen={}:last={last_user}", request.messages.len());
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
                usage: usage(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id: "scripted".into(),
            finish_reason: Some("end_turn".into()),
            usage: usage(),
            reasoning: None,
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

fn inject_stranded_input(profile: &RuntimeProfile, task_id: &str, text: &str) {
    // 直接落一条 input.queued 事件，模拟"上个进程在派发前崩溃"滞留的输入。
    let payload = serde_json::json!({
        "message_id": "stranded-1",
        "input_seq": 999,
        "kind": "user",
        "text": text,
    });
    let connection = rusqlite::Connection::open(profile.database_path()).expect("open v1 database");
    connection
        .execute(
            "INSERT INTO events(task_id, kind, payload) VALUES (?1, 'input.queued', ?2)",
            rusqlite::params![task_id, payload.to_string()],
        )
        .expect("inject stranded input");
}

#[tokio::test]
async fn a10_restart_reseeds_undelivered() {
    let temp = tempdir().expect("tempdir");
    let profile = profile_for("a10restart", temp.path());
    let models: Arc<dyn ModelService> = Arc::new(EchoModel {
        requests: Mutex::new(Vec::new()),
    });
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());

    // 第一个"进程"：正常跑完一条消息，然后注入一条滞留输入（模拟崩溃前
    // 已入队未派发）。
    let first = ApplicationService::compose(&profile, models.clone(), tools.clone())
        .expect("compose first");
    first
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    let task_id = "chat-a10";
    first
        .create_task(task_id, "", TaskKind::Conversation, vec![])
        .await
        .expect("create");
    first
        .send_message(task_id, "before crash")
        .await
        .expect("send");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let events = first.events_after(0, 500).await;
        if events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
        }) || Instant::now() >= deadline
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(first);
    inject_stranded_input(&profile, task_id, "stranded before restart");

    // "重启"：同 profile 上的新 service 实例（内存队列为空）。发一条新消息
    // 触发派发循环——重播种钩子应把滞留输入捞起派发。
    let restarted =
        ApplicationService::compose(&profile, models.clone(), tools).expect("compose restarted");
    restarted
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    restarted
        .send_message(task_id, "after restart")
        .await
        .expect("send after restart");

    let deadline = Instant::now() + Duration::from_secs(90);
    let events = loop {
        let events = restarted.events_after(0, 500).await;
        let completed = events
            .iter()
            .filter(|event| {
                event.task_id == task_id
                    && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
            })
            .count();
        if completed >= 3 || Instant::now() >= deadline {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    let completed = events
        .iter()
        .filter(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
        })
        .count();
    assert_eq!(
        completed, 3,
        "pre-crash run + after-restart run + reseeded stranded run"
    );
    assert!(
        events.iter().any(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("assistant.message"))
                && event.payload["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("stranded before restart"))
        }),
        "the stranded input must dispatch after restart (G12)"
    );
}
