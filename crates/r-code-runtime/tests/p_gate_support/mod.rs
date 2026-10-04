#![allow(dead_code)]

use r_code_harness_protocol::services::{
    ContentBlock, ModelRole, ModelStreamRequest, StreamEvent, StreamPayload, ToolDescriptor,
};
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::RunSnapshotId;
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub mod daemon;

pub const STRICT_PLAN: &str = r#"{"work_units":[{"id":"inspect","description":"inspect current checkout","dependencies":[],"acceptance":["read-only"],"write_paths":["src"]},{"id":"implement","description":"implement after approval","dependencies":["inspect"],"acceptance":["check:test"],"write_paths":["src"]}]}"#;

pub struct ScriptedModel {
    calls: Vec<(String, serde_json::Value)>,
    final_text: String,
    turn: AtomicUsize,
    pub requests: Arc<Mutex<Vec<ModelStreamRequest>>>,
}

impl ScriptedModel {
    pub fn fixed(final_text: impl Into<String>) -> Arc<Self> {
        Self::with_calls(Vec::new(), final_text)
    }

    pub fn with_calls(
        calls: Vec<(String, serde_json::Value)>,
        final_text: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            calls,
            final_text: final_text.into(),
            turn: AtomicUsize::new(0),
            requests: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

#[async_trait::async_trait]
impl ModelService for ScriptedModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.requests.lock().expect("requests").push(request);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);
        let stream_id = format!("p-gate-{turn}");
        let mut sequence = 0;
        if turn == 0 && !self.calls.is_empty() {
            for (index, (name, input)) in self.calls.iter().enumerate() {
                sequence += 1;
                sink.send(StreamEvent {
                    stream_id: stream_id.clone(),
                    sequence,
                    payload: StreamPayload::ToolCallDelta {
                        id: format!("call-{index}"),
                        name: name.clone(),
                        partial_input: input.to_string(),
                    },
                    done: None,
                })
                .await?;
            }
            sequence += 1;
            sink.send(StreamEvent {
                stream_id: stream_id.clone(),
                sequence,
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
                    text: self.final_text.clone(),
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
            reasoning: None,
        })
    }
}

pub fn profile(name: &str, root: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(root.join(name))
            .with_ipc_name(format!("p-gate-{name}-{}", std::process::id())),
    )
    .expect("runtime profile")
}

pub fn write_workspace(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("workspace directories");
    std::fs::create_dir_all(root.join(".git")).expect("git directory");
    std::fs::write(root.join("tracked.txt"), "TRACKED_SENTINEL\n").expect("tracked file");
    std::fs::write(root.join("src/untracked.txt"), "UNTRACKED_SENTINEL\n").expect("untracked file");
    std::fs::write(root.join(".secret"), "WORKSPACE_SECRET_SENTINEL\n").expect("secret file");
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").expect("git head");
}

pub fn escape_link(link: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
}

pub fn system_text(request: &ModelStreamRequest) -> String {
    request
        .messages
        .iter()
        .filter(|message| message.role == ModelRole::System)
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn snapshot_id(
    events: &[r_code_harness_protocol::EventEnvelope],
    task_id: &str,
) -> RunSnapshotId {
    let value = events
        .iter()
        .find(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.started"))
        })
        .and_then(|event| event.payload.get("snapshotId"))
        .and_then(|value| value.as_str())
        .expect("run snapshot id");
    RunSnapshotId::parse(value).expect("valid snapshot id")
}

pub fn compose_with_builtin(
    profile: &RuntimeProfile,
    package: &Path,
    model: Arc<dyn ModelService>,
) -> ApplicationService {
    let service = ApplicationService::compose(
        profile,
        model,
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose application");
    service.ensure_builtin(package).expect("install built-in");
    service
}

fn native_binary() -> &'static Path {
    static NATIVE: OnceLock<PathBuf> = OnceLock::new();
    NATIVE
        .get_or_init(|| {
            let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
                .args(["build", "-p", "r-code-harness-native"])
                .output()
                .expect("build native harness");
            assert!(
                output.status.success(),
                "native build failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug")
                .join(if cfg!(windows) {
                    "r-code-harness-native.exe"
                } else {
                    "r-code-harness-native"
                })
        })
        .as_path()
}

pub fn stage_native(root: &Path, id: &str, version: &str, requests_plan_publish: bool) -> PathBuf {
    let package = root.join(format!("{}-{version}", id.replace('.', "-")));
    let bin = package.join("bin");
    std::fs::create_dir_all(&bin).expect("package bin");
    std::fs::copy(
        native_binary(),
        bin.join(if cfg!(windows) {
            "r-code-harness-native.exe"
        } else {
            "r-code-harness-native"
        }),
    )
    .expect("copy native harness");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    let mut services = vec![
        "host.model.stream",
        "host.tools.list",
        "host.tools.call",
        "host.checkpoint.save",
        "host.completion.propose",
    ];
    if requests_plan_publish {
        services.push("host.plan.publish");
    }
    std::fs::write(
        package.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": id,
            "version": version,
            "apiMajor": 1,
            "apiMinor": 0,
            "displayName": id,
            "supportedPlatforms": [{
                "platform": platform,
                "executable": "bin/r-code-harness-native"
            }],
            "requestedHostServices": services,
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("write native manifest");
    package
}

pub async fn wait_for_kind(
    service: &r_code_runtime::application::ApplicationService,
    task_id: &str,
    kind: &str,
) -> Vec<r_code_harness_protocol::EventEnvelope> {
    wait_for_event(service, |event| {
        event.task_id == task_id
            && event.payload.get("journalKind") == Some(&serde_json::json!(kind))
    })
    .await
}

pub async fn wait_for_event(
    service: &r_code_runtime::application::ApplicationService,
    predicate: impl Fn(&r_code_harness_protocol::EventEnvelope) -> bool,
) -> Vec<r_code_harness_protocol::EventEnvelope> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = service.events_after(0, 1_000).await;
        if events.iter().any(&predicate) {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for event; journal tail: {:#?}",
            events.iter().rev().take(20).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    kind: &'static str,
    bytes: u64,
    modified_ns: u128,
    digest_or_target: String,
}

pub fn tree(root: &Path) -> BTreeMap<String, TreeEntry> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, TreeEntry>) {
        let mut entries = std::fs::read_dir(dir)
            .expect("read tree")
            .map(|entry| entry.expect("tree entry"))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).expect("tree metadata");
            let relative = path
                .strip_prefix(root)
                .expect("relative tree path")
                .to_string_lossy()
                .replace('\\', "/");
            let modified_ns = metadata
                .modified()
                .expect("modified time")
                .duration_since(UNIX_EPOCH)
                .expect("post epoch")
                .as_nanos();
            let (kind, digest_or_target) = if metadata.file_type().is_symlink() {
                (
                    "symlink",
                    std::fs::read_link(&path)
                        .expect("link target")
                        .to_string_lossy()
                        .into_owned(),
                )
            } else if metadata.is_dir() {
                ("dir", String::new())
            } else {
                let bytes = std::fs::read(&path).expect("tree file");
                ("file", format!("{:x}", Sha256::digest(bytes)))
            };
            out.insert(
                relative,
                TreeEntry {
                    kind,
                    bytes: metadata.len(),
                    modified_ns,
                    digest_or_target,
                },
            );
            if metadata.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut entries = BTreeMap::new();
    walk(root, root, &mut entries);
    entries
}

pub fn tool_names(tools: &[ToolDescriptor]) -> Vec<String> {
    let mut names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names
}
