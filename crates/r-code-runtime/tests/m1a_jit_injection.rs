//! M1a-07 (FR-1.1/1.4/1.5): JIT subdirectory injection through the host
//! projection layer — read-tool hits drive monotone instruction blocks that
//! are visible to the model request only (the canonical transcript was
//! synced from the unprojected request), over-allowance batches are
//! abandoned with a timeline trace, and every applied batch lands in the
//! injection ledger.

//! macOS：daemon→native harness 链路依赖 P13 安全激活报告，本 wave 固定
//! Unsupported——按设计拒绝启动；用例由 linux/windows 腿运行，P13 落地后移除。
#![cfg(not(target_os = "macos"))]

mod p_gate_support;

use p_gate_support::{compose_with_builtin, stage_native};
use r_code_harness_protocol::services::{ModelStreamRequest, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::TaskKind;
use r_code_runtime::application::CreateTaskInput;
use r_code_runtime::services::project_instructions::{InstructionSettings, JitTracker};
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn jit_tracker_applies_blocks_monotonically_and_dedups() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let sub = repo.join("crates/sub");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(repo.join("AGENTS.md"), "root rules\n").unwrap();
    std::fs::write(sub.join("AGENTS.md"), "subdir rules\n").unwrap();

    let mut tracker = JitTracker::new(repo.canonicalize().unwrap(), InstructionSettings::default());
    // The frozen bundle already injected the root file (simulated by the
    // root's own hit never re-injecting: seed via the root AGENTS.md hash).
    tracker.note_hit_dir(&repo);
    // The root dir itself was frozen-injected in production via seeding;
    // without a seed here the root AGENTS.md IS injected on first hit — pin
    // both behaviors explicitly below with a seeded variant.
    let _ = tracker.render_current();

    // Seeded variant mirrors production: frozen root, JIT subdirectory.
    let mut seeded = JitTracker::new(repo.canonicalize().unwrap(), InstructionSettings::default());
    let frozen = r_code_runtime::services::project_instructions::plan_bundle(
        &[
            r_code_runtime::services::project_instructions::CandidateFile {
                layer:
                    r_code_runtime::services::project_instructions::InstructionLayer::RepoForeign,
                path: repo.join("AGENTS.md").to_string_lossy().to_string(),
                content: "root rules\n".into(),
            },
        ],
        &[],
        &InstructionSettings::default(),
    );
    let set_ref = r_code_kernel::task::InstructionSetRef {
        digest: frozen.digest.clone(),
        rendered: String::new(),
        entries: frozen
            .entries
            .iter()
            .map(|entry| r_code_kernel::task::InstructionEntryRef {
                layer: entry.layer.label().to_string(),
                path: entry.path.clone(),
                sha256: entry.sha256.clone(),
                bytes: entry.bytes as u64,
                status: "injected".into(),
            })
            .collect(),
    };
    seeded.seed_from_frozen(&set_ref);

    seeded.note_hit_dir(&repo);
    assert!(
        seeded.render_current().is_none(),
        "the frozen root file must not re-inject through JIT"
    );
    seeded.note_hit_dir(&sub);
    let applied = seeded.render_current().expect("subdir block applied");
    assert!(applied.contains("subdir rules"));
    let again = seeded.render_current().expect("still applied");
    assert_eq!(applied, again);
    assert_eq!(applied.matches("subdir rules").count(), 1);
    seeded.note_hit_dir(&sub);
    assert_eq!(seeded.render_current().as_deref(), Some(applied.as_str()));

    let events = seeded.take_audit_events();
    assert!(
        events
            .iter()
            .any(|(kind, payload)| kind == "context.jit" && payload["applied"].as_i64() == Some(1)),
        "audit events: {events:?}"
    );
}

#[test]
fn jit_tracker_abandons_over_allowance_batches_with_a_trace() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let sub = repo.join("big");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("AGENTS.md"), "z".repeat(512)).unwrap();

    let settings = InstructionSettings {
        jit_allowance_bytes: 64,
        ..InstructionSettings::default()
    };
    let mut tracker = JitTracker::new(repo.canonicalize().unwrap(), settings);
    tracker.note_hit_dir(&sub);
    assert!(
        tracker.render_current().is_none(),
        "over-allowance batch must be abandoned entirely"
    );
    let events = tracker.take_audit_events();
    assert!(
        events.iter().any(|(kind, payload)| kind == "context.jit"
            && payload["reason"] == "jit-allowance-exceeded"),
        "abandonment trace missing: {events:?}"
    );
    assert!(tracker.render_current().is_none());
}

/// Scripted model: turn 0 asks the plugin to read a file inside the
/// subdirectory; later turns answer with plain text. Captures every model
/// request verbatim so the projection is directly observable.
#[derive(Default)]
struct ScriptedModel {
    requests: Mutex<Vec<ModelStreamRequest>>,
    turns: Mutex<u32>,
    target: Mutex<PathBuf>,
}

impl ScriptedModel {
    fn new(target: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            turns: Mutex::new(0),
            target: Mutex::new(target),
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
        self.requests.lock().unwrap().push(request);
        let turn = {
            let mut turns = self.turns.lock().unwrap();
            let current = *turns;
            *turns += 1;
            current
        };
        let stream_id = format!("scripted-{turn}");
        if turn == 0 {
            let path = self.target.lock().unwrap().display().to_string();
            let input = serde_json::json!({"path": path}).to_string();
            sink.send(StreamEvent {
                stream_id: stream_id.clone(),
                sequence: 0,
                payload: StreamPayload::ToolCallDelta {
                    id: "call-jit".into(),
                    name: "read_file".into(),
                    partial_input: input,
                },
                done: None,
            })
            .await?;
        } else {
            sink.send(StreamEvent {
                stream_id: stream_id.clone(),
                sequence: 0,
                payload: StreamPayload::TextDelta {
                    text: "done".into(),
                },
                done: None,
            })
            .await?;
        }
        sink.send(StreamEvent {
            stream_id: stream_id.clone(),
            sequence: 1,
            payload: StreamPayload::Finish {
                reason: "stop".into(),
                usage: Default::default(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id,
            finish_reason: Some("stop".into()),
            usage: Default::default(),
            reasoning: None,
        })
    }
}

#[tokio::test]
async fn jit_block_reaches_the_model_request_but_not_the_canonical_journal() {
    let directory = tempfile::tempdir().unwrap();
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("m1a-07-jit-projection"),
    )
    .unwrap();
    let package = stage_native(directory.path(), "native.r-code", "1.0.0", true);

    let repo = tempfile::tempdir().unwrap();
    let repo_root = repo.path().join("repo");
    let sub = repo_root.join("sub");
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    std::fs::create_dir_all(&sub).unwrap();
    // No root-level instruction file: everything the JIT block carries must
    // come from the subdirectory.
    std::fs::write(sub.join("AGENTS.md"), "subdir jit marker rules\n").unwrap();
    let target = sub.join("notes.txt");
    std::fs::write(&target, "note content\n").unwrap();

    let model = ScriptedModel::new(target);
    let service = compose_with_builtin(&profile, &package, model.clone());
    let canonical = repo_root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();

    service
        .create_task_legacy_default(CreateTaskInput {
            task_id: "jit-task".into(),
            objective: "read the note".into(),
            title: None,
            kind: TaskKind::Conversation,
            required_checks: vec![],
            memory: None,
            preferences: r_code_kernel::task::TaskPreferences {
                workspace_path: Some(canonical),
                ..Default::default()
            },
            harness_id: None,
        })
        .await
        .expect("create task");
    service.send_message("jit-task", "go").await.expect("send");

    let deadline = Instant::now() + Duration::from_secs(90);
    let events = loop {
        let events = service.events_after(0, 500).await;
        let settled = events.iter().any(|event| {
            event.task_id == "jit-task"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
        });
        if settled {
            break events;
        }
        assert!(
            Instant::now() < deadline,
            "run did not settle; journal: {:?}",
            events
                .iter()
                .map(|e| e.payload.get("journalKind"))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let requests = model.requests.lock().unwrap().clone();
    assert!(requests.len() >= 2, "expected a tool turn then a text turn");
    let system_text = |index: usize| -> String {
        requests[index]
            .messages
            .iter()
            .find(|m| m.role == r_code_harness_protocol::services::ModelRole::System)
            .map(|m| {
                m.content
                    .iter()
                    .filter_map(|block| match block {
                        r_code_harness_protocol::services::ContentBlock::Text { text } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<String>()
            })
            .unwrap_or_default()
    };
    assert!(
        !system_text(0).contains("subdir jit marker rules"),
        "the first request must not carry the JIT block"
    );
    assert!(
        system_text(requests.len() - 1).contains("subdir jit marker rules"),
        "the post-hit request must carry the JIT block in its system message"
    );

    // The canonical journal never carries the JIT text: only the low-noise
    // context.jit audit event (paths/bytes, no content) exists.
    let journal_text = serde_json::to_string(&events).unwrap();
    assert!(
        !journal_text.contains("subdir jit marker rules"),
        "JIT content leaked into the canonical journal"
    );
    assert!(
        events.iter().any(|event| {
            event.task_id == "jit-task"
                && event.payload.get("journalKind") == Some(&serde_json::json!("context.jit"))
        }),
        "context.jit audit event missing"
    );

    let store = r_code_store::v1::V1Store::open(&profile.database_path()).unwrap();
    let rows = store.injections_for_run("run-jit-task-1").unwrap();
    assert!(
        rows.iter().any(|row| row.kind == "jit"),
        "expected a jit ledger row, got {rows:?}"
    );
}
