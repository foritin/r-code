//! E09-R — immutable unverified-override audit records: one durable row per
//! (task, override id) bound to the EXACT failed check ids, written in the
//! same transaction as the UnverifiedAccepted verdict, with the immutable
//! table as the single query source (the review journal event is its derived
//! projection) and review.overrides.list as the canonical camelCase read.

mod p_gate_support;

use p_gate_support::{compose_with_builtin, profile, stage_native, wait_for_kind, write_workspace};
use r_code_harness_protocol::services::{ModelStreamRequest, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::{Actor, TaskExecution, TaskKind, TaskPreferences, WorkUnitStatus};
use r_code_runtime::application::{
    ApplicationService, ReviewActionContext, UnverifiedOverrideInput,
};
use r_code_store::v1::{
    OverrideCommitError, UnverifiedOverrideError, UnverifiedOverrideSeed, V1Store,
};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// The m04 review fixture, trimmed to the override path
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
                            "description": "mutate override fixture",
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
        } else {
            let turn = self.execution_turn.fetch_add(1, Ordering::SeqCst);
            let calls = match turn {
                0 => vec![(
                    "create",
                    "create_file",
                    serde_json::json!({"path": "src/created.txt", "content": "created"}),
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
            sink.send(StreamEvent {
                stream_id: stream_id.into(),
                sequence: sequence + 1,
                payload: StreamPayload::Finish {
                    reason: "end_turn".into(),
                    usage: Default::default(),
                },
                done: Some(true),
            })
            .await?;
        }
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
        Ok(ModelStreamOutcome {
            stream_id: stream_id.into(),
            finish_reason: Some("done".into()),
            usage: Default::default(),
        })
    }
}

fn seed_git_sentinels(workspace: &Path) {
    for (path, bytes) in [
        (".git/index", b"index-sentinel".as_slice()),
        (".git/config", b"config-sentinel".as_slice()),
        (".git/refs/heads/main", b"ref-sentinel".as_slice()),
    ] {
        let target = workspace.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
    }
}

struct ReviewFixture {
    _temp: TempDir,
    service: ApplicationService,
    store: V1Store,
    task_id: String,
}

impl ReviewFixture {
    async fn ready(label: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("checkout");
        write_workspace(&workspace);
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
            task_id: label.into(),
        }
    }

    /// Drive the task into RepairRequired with unresolved check:a/check:b —
    /// the exact state an override audits.
    async fn require_override(&self) -> ReviewActionContext {
        let (mut state, revision) = self
            .store
            .load_task_with_revision(&self.task_id)
            .unwrap()
            .unwrap();
        let (attempt_id, work_unit_id) = match &state.execution {
            TaskExecution::ReviewReady { attempt_id } => (
                attempt_id.clone(),
                state
                    .work_units
                    .iter()
                    .find(|unit| unit.status == WorkUnitStatus::Completed)
                    .unwrap()
                    .id
                    .clone(),
            ),
            other => panic!("expected ReviewReady, got {other:?}"),
        };
        state.contract.required_checks = vec!["check:a".into(), "check:b".into()];
        state
            .require_repair(
                Actor::Host,
                Some(attempt_id),
                Some(work_unit_id),
                "checks unavailable".into(),
                true,
            )
            .unwrap();
        let next = self
            .store
            .save_task_and_events_if_revision(&state, vec![], revision)
            .unwrap();
        ReviewActionContext {
            action_id: "override-1".into(),
            expected_task_revision: next,
            candidate_digest: state.task_candidate_digest().unwrap(),
            actor_id: "authenticated-user".into(),
            session_id: "override-session".into(),
        }
    }
}

fn seed(task: &str, override_id: &str, actor: &str) -> UnverifiedOverrideSeed {
    UnverifiedOverrideSeed {
        override_id: override_id.into(),
        task_id: task.into(),
        candidate_digest: "sha256:e09r-candidate".into(),
        actor_id: actor.into(),
        session_id: "session-e09r".into(),
        reason: "explicit risk acceptance".into(),
        checks: vec!["check:b".into(), "check:a".into()],
    }
}

// ---------------------------------------------------------------------------
// Contract 1 — one durable row, replay writes nothing new
// ---------------------------------------------------------------------------

/// An override lands with exactly ONE durable row bound to the exact failed
/// checks; a byte-identical replay writes nothing new; a different actor on
/// the same id conflicts and a stale second verdict refuses.
#[tokio::test]
async fn override_lands_with_exactly_one_durable_row() {
    let fixture = ReviewFixture::ready("e09r-durable-row").await;
    let context = fixture.require_override().await;
    let input = UnverifiedOverrideInput {
        context: context.clone(),
        reason: "explicit risk acceptance".into(),
        checks: vec!["check:b".into(), "check:a".into()],
    };
    let accepted = fixture
        .service
        .accept_unverified(&fixture.task_id, input.clone())
        .await
        .unwrap();
    assert_eq!(accepted.outcome, "unverified-accepted");

    let rows = fixture.store.list_unverified_overrides(None).unwrap();
    assert_eq!(rows.len(), 1, "exactly one durable row: {rows:?}");
    let row = &rows[0];
    assert_eq!(row.override_id, "override-1");
    assert_eq!(row.task_id, "e09r-durable-row");
    assert_eq!(row.actor_id, "authenticated-user");
    assert_eq!(row.session_id, "override-session");
    assert_eq!(row.reason, "explicit risk acceptance");
    assert_eq!(
        row.checks,
        vec!["check:a".to_string(), "check:b".to_string()],
        "the row binds the EXACT failed check ids, sorted"
    );
    assert_eq!(row.candidate_digest, input.context.candidate_digest);

    // Replay: identical action converges, nothing new is written.
    assert_eq!(
        fixture
            .service
            .accept_unverified(&fixture.task_id, input.clone())
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        fixture.store.list_unverified_overrides(None).unwrap().len(),
        1
    );

    // A different actor on the same id conflicts at the table.
    assert!(matches!(
        fixture.store.record_unverified_override(&seed(
            "e09r-durable-row",
            "override-1",
            "someone-else"
        )),
        Err(UnverifiedOverrideError::ActorConflict { .. })
    ));
    // The scoped list sees only its own task's rows.
    assert_eq!(
        fixture
            .store
            .list_unverified_overrides(Some("another-task"))
            .unwrap()
            .len(),
        0
    );
}

// ---------------------------------------------------------------------------
// Contract 2 — the existing refusal kept green
// ---------------------------------------------------------------------------

/// A mismatched check list is refused with NO row and NO verdict: the task
/// stays RepairRequired and the table stays empty.
#[tokio::test]
async fn mismatched_checks_refuse_with_no_row_and_no_verdict() {
    let fixture = ReviewFixture::ready("e09r-mismatch").await;
    let context = fixture.require_override().await;
    for checks in [
        vec!["check:a".to_string()],
        vec![
            "check:a".to_string(),
            "check:b".to_string(),
            "check:c".to_string(),
        ],
        vec!["unknown".to_string(), "check:b".to_string()],
    ] {
        assert!(fixture
            .service
            .accept_unverified(
                &fixture.task_id,
                UnverifiedOverrideInput {
                    context: context.clone(),
                    reason: "explicit risk acceptance".into(),
                    checks,
                }
            )
            .await
            .is_err());
    }
    assert!(
        fixture
            .store
            .list_unverified_overrides(None)
            .unwrap()
            .is_empty(),
        "no row without its verdict"
    );
    let state = fixture.store.load_task(&fixture.task_id).await.unwrap();
    assert!(
        matches!(state.execution, TaskExecution::RepairRequired { .. }),
        "no verdict without its row: {:?}",
        state.execution
    );
}

// ---------------------------------------------------------------------------
// E09-R.2 — the row commits in the verdict's transaction or not at all
// ---------------------------------------------------------------------------

/// The audit row and the task CAS update are ONE transaction: a seed the
/// table rejects rolls the verdict back with it (the task revision never
/// moves and the row never exists).
#[tokio::test]
async fn the_row_commits_with_the_verdict_or_not_at_all() {
    let fixture = ReviewFixture::ready("e09r-atomic").await;
    let _context = fixture.require_override().await;
    let (state, revision) = fixture
        .store
        .load_task_with_revision(&fixture.task_id)
        .unwrap()
        .unwrap();
    let mut poisoned = seed(&fixture.task_id, "override-atomic", "authenticated-user");
    poisoned.checks.clear();
    assert!(matches!(
        fixture
            .store
            .save_task_events_and_unverified_override_if_revision(
                &state,
                Vec::new(),
                revision,
                &poisoned
            ),
        Err(OverrideCommitError::Override(
            UnverifiedOverrideError::NoChecks
        ))
    ));
    let (_, after) = fixture
        .store
        .load_task_with_revision(&fixture.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        after, revision,
        "the rolled-back verdict never moved the task"
    );
    assert!(fixture
        .store
        .load_unverified_override(&fixture.task_id, "override-atomic")
        .unwrap()
        .is_none());

    // The healthy seed commits both together: the task moves AND the row
    // exists in one step.
    let (state, revision) = fixture
        .store
        .load_task_with_revision(&fixture.task_id)
        .unwrap()
        .unwrap();
    let healthy = seed(&fixture.task_id, "override-atomic", "authenticated-user");
    let (next, record) = fixture
        .store
        .save_task_events_and_unverified_override_if_revision(
            &state,
            Vec::new(),
            revision,
            &healthy,
        )
        .unwrap();
    assert_eq!(next, revision + 1);
    assert_eq!(
        record.checks,
        vec!["check:a".to_string(), "check:b".to_string()]
    );
    assert!(fixture
        .store
        .load_unverified_override(&fixture.task_id, "override-atomic")
        .unwrap()
        .is_some());
}

// ---------------------------------------------------------------------------
// E09-R.3 — the canonical projection
// ---------------------------------------------------------------------------

/// review.overrides.list serves the canonical camelCase projection of the
/// rows: unsorted check input lands sorted, and the wire keys are exactly
/// the canonical set.
#[test]
fn the_list_projection_is_canonical_camelcase() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&temp.path().join("e09r.db")).unwrap();
    let record = store
        .record_unverified_override(&seed("e09r-projection", "override-c", "actor-1"))
        .unwrap();
    // Identical replay converges on the first row.
    assert_eq!(
        store
            .record_unverified_override(&seed("e09r-projection", "override-c", "actor-1"))
            .unwrap(),
        record
    );
    store
        .record_unverified_override(&seed("e09r-other", "override-d", "actor-2"))
        .unwrap();
    let projection = serde_json::to_value(
        store
            .list_unverified_overrides(Some("e09r-projection"))
            .unwrap(),
    )
    .unwrap();
    let row = projection.as_array().unwrap();
    assert_eq!(row.len(), 1, "the scoped list is per task");
    let mut keys: Vec<&str> = row[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "actorId",
            "candidateDigest",
            "checks",
            "createdAtMs",
            "overrideId",
            "reason",
            "sessionId",
            "taskId",
        ],
        "the canonical camelCase key set"
    );
    assert_eq!(row[0]["checks"], serde_json::json!(["check:a", "check:b"]));
}
