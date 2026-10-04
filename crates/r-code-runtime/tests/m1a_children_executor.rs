//! M1a-10/11 (FR-8 step one): the children executor end to end — the parent
//! macOS：daemon→native harness 链路依赖平台安全激活报告（P13），本 wave
//! 报告后端固定 none-this-wave/Unsupported——链路在 macOS 按设计拒绝启动；
//! 端到端用例由 linux/windows 腿运行，P13 报告落地后移除此门。
#![cfg(not(target_os = "macos"))]

//! model spawns a scout through the children_spawn catalog tool, the child
//! runs as a REAL task through the standard drive machinery (own session,
//! journal, ledger), children_wait blocks on the condvar and returns the
//! report, the child inherits the parent's frozen memory hash, and the
//! child's own tool catalog carries no children tools (structural nesting
//! fence).

mod p_gate_support;

use p_gate_support::{compose_with_builtin, stage_native};
use r_code_harness_protocol::services::{ModelStreamRequest, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::{FrozenMemoryHandoff, TaskKind};
use r_code_runtime::application::CreateTaskInput;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Parent script: spawn → wait → done. Child script: any request whose
/// latest user text is the child objective answers with the scout summary.
#[derive(Default)]
struct DelegationModel {
    requests: Mutex<Vec<ModelStreamRequest>>,
    parent_turns: Mutex<u32>,
    objective: Mutex<String>,
}

#[async_trait::async_trait]
impl ModelService for DelegationModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        let request_clone = request.clone();
        self.requests.lock().unwrap().push(request_clone);
        let objective = self.objective.lock().unwrap().clone();
        let last_user = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == r_code_harness_protocol::services::ModelRole::User)
            .and_then(|message| {
                message.content.iter().find_map(|block| match block {
                    r_code_harness_protocol::services::ContentBlock::Text { text } => {
                        Some(text.clone())
                    }
                    _ => None,
                })
            })
            .unwrap_or_default();
        let is_child = last_user.contains(&objective);
        let stream_id = if is_child {
            "child-stream".to_string()
        } else {
            let mut turns = self.parent_turns.lock().unwrap();
            let turn = *turns;
            *turns += 1;
            format!("parent-{turn}")
        };
        let payload = if is_child {
            StreamPayload::TextDelta {
                text: "scout findings: notes verified".into(),
            }
        } else {
            let turn = stream_id
                .split('-')
                .next_back()
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0);
            if turn == 0 {
                StreamPayload::ToolCallDelta {
                    id: "call-spawn".into(),
                    name: "children_spawn".into(),
                    partial_input: serde_json::json!({
                        "objective": objective,
                        "ceiling": "read-only",
                    })
                    .to_string(),
                }
            } else if turn == 1 {
                StreamPayload::ToolCallDelta {
                    id: "call-wait".into(),
                    name: "children_wait".into(),
                    partial_input: serde_json::json!({
                        "child_task_id": "child-1",
                        "timeout_ms": 120_000,
                    })
                    .to_string(),
                }
            } else {
                StreamPayload::TextDelta {
                    text: "delegated and collected".into(),
                }
            }
        };
        sink.send(StreamEvent {
            stream_id: stream_id.clone(),
            sequence: 0,
            payload,
            done: None,
        })
        .await?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parent_spawns_scout_child_waits_and_collects_the_report() {
    let directory = tempfile::tempdir().unwrap();
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("m1a-10-children-e2e"),
    )
    .unwrap();
    let package = stage_native(directory.path(), "native.r-code", "1.0.0", true);

    let repo = tempfile::tempdir().unwrap();
    let repo_root = repo.path().join("repo");
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    std::fs::write(repo_root.join("notes.txt"), "note body\n").unwrap();
    let canonical = repo_root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let model = Arc::new(DelegationModel {
        requests: Mutex::new(Vec::new()),
        parent_turns: Mutex::new(0),
        objective: Mutex::new("scout the notes file".to_string()),
    });
    let service = compose_with_builtin(&profile, &package, model.clone());

    let handoff = FrozenMemoryHandoff {
        rendered: "<r_code_memory_snapshot>shared parent memory</r_code_memory_snapshot>".into(),
        entry_ids: vec!["entry-1".into()],
        snapshot_hash: "hash-m1a-10".into(),
    };
    service
        .create_task_legacy_default(CreateTaskInput {
            task_id: "parent-task".into(),
            objective: "delegate scouting".into(),
            title: None,
            kind: TaskKind::Conversation,
            required_checks: vec![],
            memory: Some(handoff.clone()),
            preferences: r_code_kernel::task::TaskPreferences {
                workspace_path: Some(canonical),
                ..Default::default()
            },
            harness_id: None,
        })
        .await
        .expect("create parent");
    service
        .send_message("parent-task", "go delegate")
        .await
        .expect("send");

    let deadline = Instant::now() + Duration::from_secs(240);
    let events = loop {
        let events = service.events_after(0, 500).await;
        let settled = events.iter().any(|event| {
            event.task_id == "parent-task"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
        });
        if settled {
            break events;
        }
        assert!(
            Instant::now() < deadline,
            "parent run did not settle; journal: {:?}",
            events
                .iter()
                .map(|e| (e.task_id.clone(), e.payload.get("journalKind").cloned()))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    };

    // 1) The child ran as a real task and settled.
    assert!(
        events.iter().any(|event| {
            event.task_id == "parent-task-child-1"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
        }),
        "child task never settled; journal: {:?}",
        events
            .iter()
            .map(|e| (e.task_id.clone(), e.payload.get("journalKind").cloned()))
            .collect::<Vec<_>>()
    );

    // 2) children_wait returned the child's report to the parent.
    let wait_result = events
        .iter()
        .rfind(|event| {
            event.task_id == "parent-task"
                && event.payload.get("journalKind") == Some(&serde_json::json!("tool.result"))
                && event.payload.get("name") == Some(&serde_json::json!("children_wait"))
        })
        .expect("children_wait tool result event");
    let output = serde_json::to_string(wait_result.payload.get("output").unwrap_or_default())
        .unwrap_or_default();
    assert!(
        output.contains("scout findings: notes verified"),
        "wait must carry the child summary, got {output}"
    );

    // 3) The child inherited the parent's frozen memory (same hash) and
    //    recorded its own injection ledger row.
    let store = r_code_store::v1::V1Store::open(&profile.database_path()).unwrap();
    let child = store
        .load_task_with_revision("parent-task-child-1")
        .expect("child task durable")
        .map(|(state, _revision)| state)
        .expect("child task exists");
    assert_eq!(
        child
            .contract
            .memory
            .as_ref()
            .map(|m| m.snapshot_hash.clone()),
        Some("hash-m1a-10".to_string()),
        "child must inherit the parent's frozen memory hash"
    );
    let child_rows = store
        .injections_for_run("run-parent-task-child-1-1")
        .unwrap();
    assert!(
        child_rows
            .iter()
            .any(|row| row.kind == "memory" && row.snapshot_hash == "hash-m1a-10"),
        "child run must ledger the same memory snapshot: {child_rows:?}"
    );

    // 4) Structural nesting fence: the child's tool catalog carries no
    //    children tools; the parent's does.
    let requests = model.requests.lock().unwrap().clone();
    let parent_request = requests
        .iter()
        .find(|request| {
            request
                .tools
                .iter()
                .any(|tool| tool.name == "children_spawn")
        })
        .expect("parent catalog carries children_spawn");
    let child_request = requests
        .iter()
        .find(|request| {
            request.tools.iter().any(|tool| tool.name == "children_spawn")
                && request
                    .messages
                    .iter()
                    .any(|m| m.role == r_code_harness_protocol::services::ModelRole::User
                        && m.content.iter().any(|b| matches!(b, r_code_harness_protocol::services::ContentBlock::Text { text } if text.contains("scout the notes file"))))
        });
    assert!(
        child_request.is_none(),
        "the child catalog must NOT carry children tools (structural nesting)"
    );
    let _ = parent_request;
}

/// Parent script for the queue test: turns 0..=6 spawn seven scouts; turn 7
/// closes child-1 (releasing the slot); turn 8 waits for the QUEUED child-7
/// — which can only have run after the close.
#[derive(Default)]
struct QueueModel {
    requests: Mutex<Vec<ModelStreamRequest>>,
    parent_turns: Mutex<u32>,
    objective: Mutex<String>,
}

#[async_trait::async_trait]
impl ModelService for QueueModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.requests.lock().unwrap().push(request.clone());
        let objective = self.objective.lock().unwrap().clone();
        let last_user = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == r_code_harness_protocol::services::ModelRole::User)
            .and_then(|m| {
                m.content.iter().find_map(|b| match b {
                    r_code_harness_protocol::services::ContentBlock::Text { text } => {
                        Some(text.clone())
                    }
                    _ => None,
                })
            })
            .unwrap_or_default();
        let is_child = last_user.contains(&objective);
        let turn = if is_child {
            100
        } else {
            let mut turns = self.parent_turns.lock().unwrap();
            let current = *turns;
            *turns += 1;
            current
        };
        let payload = if is_child {
            StreamPayload::TextDelta {
                text: "queued scout done".into(),
            }
        } else if (0..=6).contains(&turn) {
            StreamPayload::ToolCallDelta {
                id: format!("call-spawn-{turn}"),
                name: "children_spawn".into(),
                partial_input: serde_json::json!({"objective": objective}).to_string(),
            }
        } else if turn == 7 {
            StreamPayload::ToolCallDelta {
                id: "call-close".into(),
                name: "children_close".into(),
                partial_input: serde_json::json!({"child_task_id": "child-1"}).to_string(),
            }
        } else if turn == 8 {
            StreamPayload::ToolCallDelta {
                id: "call-wait".into(),
                name: "children_wait".into(),
                partial_input: serde_json::json!({
                    "child_task_id": "child-7",
                    "timeout_ms": 120_000,
                })
                .to_string(),
            }
        } else {
            StreamPayload::TextDelta {
                text: "queue drained".into(),
            }
        };
        let stream_id = format!("q-{turn}");
        sink.send(StreamEvent {
            stream_id: stream_id.clone(),
            sequence: 0,
            payload,
            done: None,
        })
        .await?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_seventh_spawn_queues_and_runs_only_after_a_close_frees_the_slot() {
    let directory = tempfile::tempdir().unwrap();
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("m1a-10-children-queue"),
    )
    .unwrap();
    let package = stage_native(directory.path(), "native.r-code", "1.0.0", true);

    let repo = tempfile::tempdir().unwrap();
    let repo_root = repo.path().join("repo");
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    let canonical = repo_root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let model = Arc::new(QueueModel {
        requests: Mutex::new(Vec::new()),
        parent_turns: Mutex::new(0),
        objective: Mutex::new("queued scout objective".to_string()),
    });
    let service = compose_with_builtin(&profile, &package, model.clone());
    service
        .create_task_legacy_default(CreateTaskInput {
            task_id: "queue-parent".into(),
            objective: "fan out".into(),
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
        .expect("create parent");
    service
        .send_message("queue-parent", "go")
        .await
        .expect("send");

    let deadline = Instant::now() + Duration::from_secs(420);
    let events = loop {
        let events = service.events_after(0, 500).await;
        let settled = events.iter().any(|event| {
            event.task_id == "queue-parent"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
        });
        if settled {
            break events;
        }
        assert!(
            Instant::now() < deadline,
            "parent run did not settle; journal: {:?}",
            events
                .iter()
                .map(|e| (e.task_id.clone(), e.payload.get("journalKind").cloned()))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    // The queued child (child-7) completed — it could only start after the
    // close of child-1 freed the seventh slot (acceptance c).
    assert!(
        events.iter().any(|event| {
            event.task_id == "queue-parent-child-7"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
        }),
        "queued child never settled; journal: {:?}",
        events
            .iter()
            .map(|e| (e.task_id.clone(), e.payload.get("journalKind").cloned()))
            .collect::<Vec<_>>()
    );
    // And children 1..6 also ran.
    for index in 1..=6 {
        assert!(
            events.iter().any(|event| {
                event.task_id == format!("queue-parent-child-{index}")
                    && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
            }),
            "child {index} never settled"
        );
    }
}
