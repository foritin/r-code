//! 观察项 12 冷启动测量：每 run 一个插件进程的 spawn + initialize 握手 +
//! checkpoint 恢复成本。journal 的 run.started 时间戳与 assistant.message
//! 观察的落库序可差分出"进程就绪到首答"的墙钟；本测量给出后续"进程常驻
//! 复用"决策的数据基线（不进 CI，手动运行）。
//!
//! 用法：cargo test -p r-code-runtime --test cold_start_bench -- --nocapture --ignored

use r_code_harness_protocol::services::ModelStreamRequest;
use r_code_harness_protocol::{ModelUsage, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink, ToolService,
};
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

struct InstantModel {
    calls: AtomicUsize,
}

fn usage() -> ModelUsage {
    ModelUsage {
        input_tokens: Some(1),
        output_tokens: Some(1),
        cost_micros: None,
    }
}

#[async_trait::async_trait]
impl ModelService for InstantModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        sink.send(StreamEvent {
            stream_id: "s".into(),
            sequence: 1,
            payload: StreamPayload::TextDelta { text: "ok".into() },
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
            "apiMinor": 0,
            "displayName": "Native",
            "supportedPlatforms": [{"platform": platform, "executable": "bin/r-code-harness-native"}],
            "requestedHostServices": ["host.model.stream", "host.tools.list", "host.tools.call", "host.checkpoint.save", "host.completion.propose"],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");
    source
}

#[tokio::test]
#[ignore = "manual measurement: per-run plugin process cold-start cost"]
async fn cold_start_bench() {
    let temp = tempdir().expect("tempdir");
    let profile = profile_for("coldstart", temp.path());
    let models: Arc<dyn ModelService> = Arc::new(InstantModel {
        calls: AtomicUsize::new(0),
    });
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");
    service
        .ensure_builtin(&stage_native(temp.path()))
        .expect("install native");
    let task_id = "bench";
    service
        .create_task(
            task_id,
            "",
            r_code_kernel::task::TaskKind::Conversation,
            vec![],
        )
        .await
        .expect("create");

    // 模型是零耗时脚本：send→completed 的墙钟 ≈ spawn + 握手 + checkpoint
    // 恢复 + 提案/仲裁 + 进程清理。跑 5 个 run 取均值/中位。
    let mut samples = Vec::new();
    for n in 0..5 {
        let started = Instant::now();
        service
            .send_message(task_id, &format!("turn {n}"))
            .await
            .expect("send");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let events = service.events_after(0, 1_000).await;
            let mine = events.iter().any(|event| {
                event.task_id == task_id
                    && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
                    && event
                        .payload
                        .get("runId")
                        .and_then(|value| value.as_str())
                        .is_some_and(|run_id| run_id.contains(&format!("-{}", n + 1)))
            });
            if mine {
                samples.push(started.elapsed());
                break;
            }
            if Instant::now() >= deadline {
                panic!("run {n} timed out");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    samples.sort();
    let median = samples[samples.len() / 2];
    let mean = samples.iter().sum::<Duration>() / samples.len() as u32;
    println!("cold-start samples: {:?}", samples);
    println!("median per-run wall (spawn+handshake+resume+arbitrate): {median:?}");
    println!("mean:   {mean:?}");
}
