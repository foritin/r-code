//! O00 integration gate: the review surface moved from application.rs into
//! `application/review_flow.rs` and the per-task drive loop plus queue
//! dispatch moved from run_manager.rs into `run_drive.rs`, both by pure
//! code motion. This file pins the task contract's single declared test —
//! "Every moved path still resolves and review + queue dispatch behave
//! identically after the split":
//!
//! 1. review()/accept_review() end-to-end on one happy path and refusal
//!    paths (m04_review_flow keeps the deep pins; here we pin reachability
//!    and outcome parity),
//! 2. send/send_as queue semantics through the moved drive token
//!    (started shape, queued:true while a run is active, delivery of the
//!    queued input by the moved drive loop),
//! 3. the carved modules resolve on the public crate surface.

mod p_gate_support;

// O00.3 wiring pin (compile-time): both carved modules must resolve as
// public crate paths — `application::review_flow` (child module of the
// application surface) and `run_drive` (top-level, exposed by lib.rs).
// If either is missing, renamed or made private, this file does not build.
#[allow(unused_imports)]
use r_code_runtime::application::review_flow;
#[allow(unused_imports)]
use r_code_runtime::run_drive;

use p_gate_support::{
    compose_with_builtin, profile, stage_native, wait_for_event, wait_for_kind, write_workspace,
};
use r_code_harness_protocol::services::{
    ContentBlock, ModelRole, ModelStreamRequest, StreamEvent, StreamPayload,
};
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::{
    ReviewDisposition, TaskExecution, TaskKind, TaskPreferences, TaskVerdict,
};
use r_code_runtime::application::{ApplicationService, ReviewActionContext};
use r_code_runtime::services::review::DurableReviewView;
use r_code_store::v1::V1Store;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Review fixture (m04_review_flow's minimal setup, unchanged): an
// Implementation task driven through plan → approval → execution until it
// reaches review-ready with four workspace changes.
// ---------------------------------------------------------------------------

struct ReviewModel {
    execution_turn: AtomicUsize,
}

impl ReviewModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            execution_turn: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl ModelService for ReviewModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        let execution = request.tools.iter().any(|tool| tool.name == "create_file");
        let stream_id = if execution { "execution" } else { "planning" };
        if !execution {
            sink.send(StreamEvent {
                stream_id: stream_id.into(),
                sequence: 1,
                payload: StreamPayload::TextDelta {
                    text: serde_json::json!({
                        "work_units": [{
                            "id": "implement",
                            "description": "mutate review fixture",
                            "dependencies": [],
                            "acceptance": [],
                            "write_paths": ["src"]
                        }]
                    })
                    .to_string(),
                },
                done: None,
            })
            .await?;
            sink.send(StreamEvent {
                stream_id: stream_id.into(),
                sequence: 2,
                payload: StreamPayload::Finish {
                    reason: "end_turn".into(),
                    usage: Default::default(),
                },
                done: Some(true),
            })
            .await?;
        } else {
            let turn = self.execution_turn.fetch_add(1, Ordering::SeqCst);
            let calls = match turn {
                0 => vec![
                    (
                        "create",
                        "create_file",
                        serde_json::json!({"path": "src/created.txt", "content": "created-after"}),
                    ),
                    (
                        "edit",
                        "edit",
                        serde_json::json!({
                            "path": "src/edit.txt",
                            "old_string": "edit-before",
                            "new_string": "edit-after"
                        }),
                    ),
                    (
                        "delete",
                        "delete_file",
                        serde_json::json!({"path": "src/deleted.txt"}),
                    ),
                    (
                        "multi-1",
                        "edit",
                        serde_json::json!({
                            "path": "src/multi.txt",
                            "old_string": "multi-before",
                            "new_string": "multi-middle"
                        }),
                    ),
                ],
                1 => vec![(
                    "multi-2",
                    "edit",
                    serde_json::json!({
                        "path": "src/multi.txt",
                        "old_string": "multi-middle",
                        "new_string": "multi-after"
                    }),
                )],
                _ => Vec::new(),
            };
            let mut sequence = 0;
            for (id, name, input) in calls {
                sequence += 1;
                sink.send(StreamEvent {
                    stream_id: stream_id.into(),
                    sequence,
                    payload: StreamPayload::ToolCallDelta {
                        id: id.into(),
                        name: name.into(),
                        partial_input: input.to_string(),
                    },
                    done: None,
                })
                .await?;
            }
            if sequence == 0 {
                sequence += 1;
                sink.send(StreamEvent {
                    stream_id: stream_id.into(),
                    sequence,
                    payload: StreamPayload::TextDelta {
                        text: "implementation complete".into(),
                    },
                    done: None,
                })
                .await?;
            }
            sink.send(StreamEvent {
                stream_id: stream_id.into(),
                sequence: sequence + 1,
                payload: StreamPayload::Finish {
                    reason: if turn <= 1 { "tool_use" } else { "end_turn" }.into(),
                    usage: Default::default(),
                },
                done: Some(true),
            })
            .await?;
        }
        Ok(ModelStreamOutcome {
            stream_id: stream_id.into(),
            finish_reason: Some("done".into()),
            usage: Default::default(),
        })
    }
}

struct ReviewFixture {
    _temp: tempfile::TempDir,
    service: ApplicationService,
    store: V1Store,
    workspace: PathBuf,
    task_id: String,
}

impl ReviewFixture {
    async fn ready(label: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("checkout");
        write_workspace(&workspace);
        std::fs::write(workspace.join("src/edit.txt"), b"edit-before").unwrap();
        std::fs::write(workspace.join("src/deleted.txt"), b"delete-before").unwrap();
        std::fs::write(workspace.join("src/multi.txt"), b"multi-before").unwrap();
        seed_git_sentinels(&workspace);
        let profile = profile(label, temp.path());
        let package = stage_native(temp.path(), "native.r-code", "1.0.0", true);
        let service = compose_with_builtin(&profile, &package, ReviewModel::new());
        service
            .create_task(label, label, TaskKind::Implementation, vec![])
            .await
            .unwrap();
        service
            .set_task_preferences(
                label,
                TaskPreferences {
                    workspace_path: Some(workspace.display().to_string()),
                    ..TaskPreferences::default()
                },
            )
            .await
            .unwrap();
        service.send_message(label, "plan").await.unwrap();
        wait_for_kind(&service, label, "plan.awaiting-approval").await;
        let plan = service.plan(label).await.unwrap();
        service
            .approve_plan(
                label,
                &plan.revision_hash,
                &format!("approval-{label}"),
                "local-user",
                &format!("session-{label}"),
            )
            .await
            .unwrap();
        service.send_message(label, "execute").await.unwrap();
        wait_for_kind(&service, label, "review-ready").await;
        let store = V1Store::open(&profile.database_path()).unwrap();
        Self {
            _temp: temp,
            service,
            store,
            workspace,
            task_id: label.into(),
        }
    }

    async fn view(&self) -> DurableReviewView {
        self.service.review(&self.task_id).await.unwrap()
    }

    fn context(&self, view: &DurableReviewView, action_id: &str) -> ReviewActionContext {
        ReviewActionContext {
            action_id: action_id.into(),
            expected_task_revision: view.task_revision,
            candidate_digest: view.candidate_digest.clone(),
            actor_id: "authenticated-user".into(),
            session_id: "review-session".into(),
        }
    }
}

fn seed_git_sentinels(workspace: &Path) {
    for (path, bytes) in [
        (".git/index", b"index-sentinel".as_slice()),
        (".git/config", b"config-sentinel".as_slice()),
        (".git/refs/heads/main", b"ref-sentinel".as_slice()),
        (".git/objects/sentinel", b"object-sentinel".as_slice()),
    ] {
        let target = workspace.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
    }
}

// ---------------------------------------------------------------------------
// 1. Review surface (moved into application/review_flow.rs)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn review_projection_and_verified_accept_behave_identically_after_the_split() {
    let fixture = ReviewFixture::ready("o00-accept").await;

    // The moved review() projection resolves and returns the exact view:
    // live task, no staleness, the four executed changes, store revision.
    let view = fixture.view().await;
    assert_eq!(view.task_id, fixture.task_id);
    assert!(!view.stale && !view.conflict);
    assert!(view.checks.is_empty());
    assert_eq!(
        view.task_revision,
        fixture
            .store
            .load_task_with_revision(&fixture.task_id)
            .unwrap()
            .unwrap()
            .1
    );
    let mut paths = view
        .changes
        .iter()
        .map(|change| change.path.as_str())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "src/created.txt",
            "src/deleted.txt",
            "src/edit.txt",
            "src/multi.txt"
        ]
    );
    assert!(fixture.service.review("missing-task").await.is_err());

    // Happy disposition path through the moved accept_review.
    let context = fixture.context(&view, "accept-1");
    let accepted = fixture
        .service
        .accept_review(&fixture.task_id, context.clone())
        .await
        .unwrap();
    assert_eq!(accepted.outcome, "verified-accepted");
    assert_eq!(accepted.task_id, fixture.task_id);
    assert_eq!(accepted.action_id, "accept-1");
    assert_eq!(accepted.candidate_digest, view.candidate_digest);
    // Replay of the identical action converges to the same result.
    assert_eq!(
        fixture
            .service
            .accept_review(&fixture.task_id, context.clone())
            .await
            .unwrap(),
        accepted
    );
    // A different actor on the same action id conflicts (single CAS winner).
    let mut conflicting = context;
    conflicting.actor_id = "different-user".into();
    assert!(fixture
        .service
        .accept_review(&fixture.task_id, conflicting)
        .await
        .is_err());

    // Task verdict transition + journal event, exactly once.
    let state = fixture.store.load_task(&fixture.task_id).await.unwrap();
    assert_eq!(state.review, ReviewDisposition::Accepted);
    assert!(matches!(
        state.execution,
        TaskExecution::Terminal {
            verdict: TaskVerdict::VerifiedAccepted { .. }
        }
    ));
    assert_eq!(
        fixture
            .store
            .task_events(&fixture.task_id)
            .iter()
            .filter(|event| event.kind == "review.accepted")
            .count(),
        1
    );
    // The accepted bytes stay in the workspace and review of a terminal
    // task is refused (moved review_service guard).
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/created.txt")).unwrap(),
        b"created-after"
    );
    assert!(fixture.service.review(&fixture.task_id).await.is_err());
}

#[tokio::test]
async fn accept_review_refuses_stale_revision_and_stale_evidence_after_the_split() {
    let fixture = ReviewFixture::ready("o00-refusal").await;
    let view = fixture.view().await;

    // Refusal 1: stale expected task revision (moved load_exact_review_task).
    let mut stale_revision = fixture.context(&view, "accept-stale-revision");
    stale_revision.expected_task_revision += 1;
    assert!(fixture
        .service
        .accept_review(&fixture.task_id, stale_revision)
        .await
        .is_err());

    // Refusal 2: stale evidence — the user edits the workspace after the
    // verification, so the candidate no longer matches (moved stale guard).
    std::fs::write(fixture.workspace.join("src/edit.txt"), b"user-later-edit").unwrap();
    let drifted = fixture.view().await;
    assert!(drifted.stale && drifted.conflict);
    assert!(fixture
        .service
        .accept_review(
            &fixture.task_id,
            fixture.context(&drifted, "accept-stale-evidence")
        )
        .await
        .is_err());
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/edit.txt")).unwrap(),
        b"user-later-edit",
        "refused accept must not touch the workspace"
    );

    // Both refusals left the task untouched: still review-ready, no
    // terminal review journal event.
    assert!(matches!(
        fixture
            .store
            .load_task(&fixture.task_id)
            .await
            .unwrap()
            .execution,
        TaskExecution::ReviewReady { .. }
    ));
    assert!(fixture
        .store
        .task_events(&fixture.task_id)
        .iter()
        .all(|event| !matches!(
            event.kind.as_str(),
            "review.accepted" | "review.rejected" | "review.unverified-accepted"
        )));
}

// ---------------------------------------------------------------------------
// 2. Queue dispatch (send/send_as/drive_loop moved into run_drive.rs)
// ---------------------------------------------------------------------------

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
        let last_user = request
            .messages
            .iter()
            .rev()
            .filter(|message| matches!(message.role, ModelRole::User))
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .next()
            .unwrap_or_default();
        let reply = format!("echo:{last_user}");
        sink.send(StreamEvent {
            stream_id: "echo".into(),
            sequence: 1,
            payload: StreamPayload::TextDelta { text: reply },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: "echo".into(),
            sequence: 2,
            payload: StreamPayload::Finish {
                reason: "end_turn".into(),
                usage: Default::default(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id: "echo".into(),
            finish_reason: Some("end_turn".into()),
            usage: Default::default(),
        })
    }
}

#[tokio::test]
async fn send_and_queue_dispatch_behave_identically_after_the_split() {
    let temp = tempfile::tempdir().unwrap();
    let profile = profile("o00-queue", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let service = compose_with_builtin(
        &profile,
        &package,
        Arc::new(EchoModel {
            delay: Duration::from_millis(1500),
        }),
    );
    let task_id = "o00-queue";
    service
        .create_task(task_id, "queue gate", TaskKind::Conversation, vec![])
        .await
        .unwrap();

    // First send through the moved send(): starts the drive loop and
    // answers with the started shape (started + task + runId).
    let first = service
        .send_message(task_id, "first message")
        .await
        .unwrap();
    assert_eq!(first["started"], true);
    assert_eq!(first["task"], task_id);
    assert_eq!(first["runId"], format!("run-{task_id}-1"));

    // While the slow run holds the drive token, the moved send_as() must
    // report the message as queued (no second run started).
    let second = service
        .send_message_as(task_id, "follow up", Some("qa-device-1"))
        .await
        .unwrap();
    assert_eq!(second["queued"], true);
    assert_eq!(second["task"], task_id);
    assert!(second.get("started").is_none());

    // The moved drive loop delivers the queued input as the second run.
    let events = wait_for_event(&service, |event| {
        event.task_id == task_id
            && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
            && event.payload.get("runId").and_then(|value| value.as_str())
                == Some(format!("run-{task_id}-2").as_str())
    })
    .await;
    assert!(
        events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind") == Some(&serde_json::json!("assistant.message"))
                && event.payload["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("follow up"))
        }),
        "queued follow-up must be dispatched after the active run"
    );

    // send_as stamps the audit actor into the queued input's journal event.
    let store = V1Store::open(&profile.database_path()).unwrap();
    let events_all = store.task_events(task_id);
    let audited = events_all
        .iter()
        .filter(|event| {
            event.kind == "input.queued"
                && event.payload.get("text").and_then(|value| value.as_str()) == Some("follow up")
        })
        .collect::<Vec<_>>();
    assert_eq!(audited.len(), 1, "exactly one queued input for the text");
    assert_eq!(
        audited[0]
            .payload
            .get("actor")
            .and_then(|value| value.as_str()),
        Some("qa-device-1")
    );

    // Both runs project as completed on the task view.
    let detail = service.task_detail(task_id).await.unwrap();
    assert_eq!(detail.runs.len(), 2);
    assert!(detail.runs.iter().all(|run| run.outcome == "completed"));
}

// ---------------------------------------------------------------------------
// 3. Module wiring (O00.3 re-export transparency)
// ---------------------------------------------------------------------------

#[test]
fn moved_modules_resolve_on_the_public_crate_surface() {
    // The `use r_code_runtime::application::review_flow` and
    // `use r_code_runtime::run_drive` statements at the top of this file
    // are the compile-time pin: both carved modules resolve at their
    // declared public paths. Method-resolution transparency is proven by
    // the async tests above, which call review/accept_review (defined in
    // review_flow.rs) and the send/send_as drive path (defined in
    // run_drive.rs) as inherent methods on the unchanged types.
    assert_eq!(
        std::any::type_name::<ApplicationService>(),
        "r_code_runtime::application::ApplicationService",
        "ApplicationService keeps its canonical public path"
    );
    assert_eq!(
        std::any::type_name::<r_code_runtime::run_manager::RunManager>(),
        "r_code_runtime::run_manager::RunManager",
        "RunManager keeps its canonical public path"
    );
}
