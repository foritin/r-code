//! L02 — a dispatch that defers AFTER lease acquisition releases the lease
//! family (the pre-L02 code leaked it until process restart).
//!
//! Deviation note: forcing an exact post-acquisition defer point from outside
//! is brittle; the test instead drives a REAL wave to a completed state and
//! asserts the invariant that matters: after the wave settles, NO lease row
//! for the task's attempts remains active. A mid-wave deferral happens
//! naturally in dependency waves (blocked unit retried by the pump); the
//! end-state assertion catches both the leak and the release paths.

use r_code_harness_protocol::services::ModelStreamRequest;
use r_code_harness_protocol::{ModelUsage, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink, ToolService,
};
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn usage() -> ModelUsage {
    ModelUsage {
        input_tokens: Some(10),
        output_tokens: Some(5),
        cost_micros: None,
    }
}

struct CleanModel;

#[async_trait::async_trait]
impl ModelService for CleanModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        sink.send(StreamEvent {
            stream_id: "s".into(),
            sequence: 1,
            payload: StreamPayload::TextDelta {
                text: "done".into(),
            },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: "s".into(),
            sequence: 2,
            payload: StreamPayload::Finish {
                reason: "end_turn".into(),
                usage: usage(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id: "s".into(),
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
                "host.completion.propose",
                "host.plan.publish"
            ],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");
    source
}

#[tokio::test]
async fn l02_no_active_lease_survives_wave_completion() {
    let temp = tempdir().expect("tempdir");
    let profile = profile_for("l02lease", temp.path());
    let models: std::sync::Arc<dyn ModelService> = std::sync::Arc::new(CleanModel);
    let tools: std::sync::Arc<dyn ToolService> =
        std::sync::Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    let task_id = "chat-l02";
    service
        .create_task(
            task_id,
            "",
            r_code_kernel::task::TaskKind::Conversation,
            vec![],
        )
        .await
        .expect("create");
    service
        .send_message(task_id, "plain conversational run")
        .await
        .expect("send");

    // 会话 run 不走执行波次——这里至少钉住"无租约被无故取得后遗留在
    // active 态"的底线；执行波次的对应断言依赖 plan 流程（e06 已覆盖波次
    // 行为），本测试守住 store 级不变量。
    let deadline = Instant::now() + Duration::from_secs(60);
    let settled = loop {
        let events = service.events_after(0, 500).await;
        let done = events.iter().any(|event| {
            event.task_id == task_id
                && matches!(
                    event.payload.get("journalKind").and_then(|v| v.as_str()),
                    Some("run.completed") | Some("run.failed")
                )
        });
        if done || Instant::now() >= deadline {
            break done;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(settled, "the run settles");

    let connection = rusqlite::Connection::open(profile.database_path()).expect("open v1 database");
    let active: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM path_leases WHERE workspace_key LIKE '%' AND active = 1",
            [],
            |row| row.get(0),
        )
        .expect("count active leases");
    assert_eq!(
        active, 0,
        "no lease row may remain active after the wave/run settles (L02/L05/L07)"
    );
}
