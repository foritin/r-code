//! macOS：daemon→native harness 链路依赖 P13 安全激活报告，本 wave 固定
//! Unsupported——按设计拒绝启动；用例由 linux/windows 腿运行，P13 落地后移除。
#![cfg(not(target_os = "macos"))]

mod p_gate_support;

use p_gate_support::{compose_with_builtin, profile, stage_native, wait_for_kind, write_workspace};
use r_code_harness_protocol::services::{ModelStreamRequest, StreamEvent, StreamPayload};
use r_code_harness_protocol::Provenance;
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::{
    Actor, EvidenceRecord, NetworkCeiling, ReviewDisposition, TaskExecution, TaskKind,
    TaskPreferences, TaskVerdict, ValidationOutcome, WorkUnit, WorkUnitEffectClass, WorkUnitStatus,
};
use r_code_kernel::testing::FakeToolService;
use r_code_runtime::application::{
    ApplicationService, ReviewActionContext, UnverifiedOverrideInput,
};
use r_code_runtime::services::artifacts::{sha256_hex, ArtifactStore};
use r_code_runtime::services::review::DurableReviewService;
use r_code_runtime::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_runtime::RuntimeProfile;
use r_code_store::v1::{LeaseRequest, MutationState, V1Store};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

struct ReviewModel {
    execution_turn: AtomicUsize,
}

struct BlockingModel {
    entered: Arc<Notify>,
}

#[async_trait::async_trait]
impl ModelService for BlockingModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        _sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.entered.notify_waiters();
        std::future::pending().await
    }
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
            reasoning: None,
        })
    }
}

struct ReviewFixture {
    _temp: tempfile::TempDir,
    profile: RuntimeProfile,
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
            profile,
            service,
            store,
            workspace,
            task_id: label.into(),
        }
    }

    async fn view(&self) -> r_code_runtime::services::review::DurableReviewView {
        self.service.review(&self.task_id).await.unwrap()
    }

    fn context(
        &self,
        view: &r_code_runtime::services::review::DurableReviewView,
        action_id: &str,
    ) -> ReviewActionContext {
        ReviewActionContext {
            action_id: action_id.into(),
            expected_task_revision: view.task_revision,
            candidate_digest: view.candidate_digest.clone(),
            actor_id: "authenticated-user".into(),
            session_id: "review-session".into(),
        }
    }

    fn workspace_key(&self) -> String {
        let canonical = std::fs::canonicalize(&self.workspace).unwrap();
        format!(
            "sha256:{}",
            sha256_hex(canonical.to_string_lossy().as_bytes())
        )
    }

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
            // E05: derived from the failed unit's per-unit record.
            candidate_digest: state.task_candidate_digest().unwrap(),
            actor_id: "authenticated-user".into(),
            session_id: "override-session".into(),
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

fn git_fingerprint(workspace: &Path) -> BTreeMap<String, (Vec<u8>, u128)> {
    fn walk(root: &Path, current: &Path, out: &mut BTreeMap<String, (Vec<u8>, u128)>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let metadata = std::fs::metadata(&path).unwrap();
                let modified = metadata
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    (std::fs::read(path).unwrap(), modified),
                );
            }
        }
    }
    let root = workspace.join(".git");
    let mut out = BTreeMap::new();
    walk(&root, &root, &mut out);
    out
}

#[tokio::test]
async fn review_projection_is_exact_net_and_host_owned() {
    let fixture = ReviewFixture::ready("m04-project").await;
    let git_before = git_fingerprint(&fixture.workspace);
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
    let changes = view
        .changes
        .iter()
        .map(|change| (change.path.as_str(), change))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(changes.len(), 4);
    assert_eq!(changes["src/created.txt"].before_sha256, None);
    assert_eq!(
        changes["src/created.txt"].after_sha256.as_deref(),
        Some(sha256_hex(b"created-after").as_str())
    );
    assert_eq!(
        changes["src/edit.txt"].before_sha256.as_deref(),
        Some(sha256_hex(b"edit-before").as_str())
    );
    assert_eq!(
        changes["src/edit.txt"].after_sha256.as_deref(),
        Some(sha256_hex(b"edit-after").as_str())
    );
    assert_eq!(
        changes["src/deleted.txt"].before_sha256.as_deref(),
        Some(sha256_hex(b"delete-before").as_str())
    );
    assert_eq!(changes["src/deleted.txt"].after_sha256, None);
    assert_eq!(
        changes["src/multi.txt"].before_sha256.as_deref(),
        Some(sha256_hex(b"multi-before").as_str())
    );
    assert_eq!(
        changes["src/multi.txt"].after_sha256.as_deref(),
        Some(sha256_hex(b"multi-after").as_str())
    );
    assert!(view
        .changes
        .iter()
        .all(|change| change.current_matches_after));
    assert!(fixture.service.review("missing-task").await.is_err());
    assert_eq!(git_fingerprint(&fixture.workspace), git_before);
}

#[tokio::test]
async fn projection_rejects_missing_attempt_event_and_plugin_evidence_never_passes() {
    let fixture = ReviewFixture::ready("m04-projection-guards").await;
    let (mut state, revision) = fixture
        .store
        .load_task_with_revision(&fixture.task_id)
        .unwrap()
        .unwrap();
    state.contract.required_checks = vec!["check:required".into()];
    state.evidence.push(EvidenceRecord {
        evidence_id: "plugin-evidence".into(),
        task_id: fixture.task_id.clone(),
        check_id: "check:required".into(),
        definition_identity: "definition".into(),
        candidate_digest: state.task_candidate_digest().clone().unwrap(),
        environment: "plugin".into(),
        environment_fingerprint: "environment".into(),
        passed: true,
        host_output: None,
        recorded_by: Provenance::Plugin {
            harness_id: "untrusted".into(),
            package_digest: "untrusted".into(),
        },
    });
    fixture
        .store
        .save_task_and_events_if_revision(&state, vec![], revision)
        .unwrap();
    let view = fixture.view().await;
    assert_eq!(view.checks.len(), 1);
    assert!(!view.checks[0].passed);

    let (mut wrong_attempt, revision) = fixture
        .store
        .load_task_with_revision(&fixture.task_id)
        .unwrap()
        .unwrap();
    wrong_attempt.execution = TaskExecution::ReviewReady {
        attempt_id: "attempt-without-review-event".into(),
    };
    fixture
        .store
        .save_task_and_events_if_revision(&wrong_attempt, vec![], revision)
        .unwrap();
    assert!(fixture.service.review(&fixture.task_id).await.is_err());
}

#[tokio::test]
async fn verified_accept_is_cas_idempotent_keeps_after_bytes_and_never_touches_git() {
    let fixture = ReviewFixture::ready("m04-accept").await;
    let git_before = git_fingerprint(&fixture.workspace);
    let view = fixture.view().await;
    let context = fixture.context(&view, "accept-1");
    let accepted = fixture
        .service
        .accept_review(&fixture.task_id, context.clone())
        .await
        .unwrap();
    assert_eq!(accepted.outcome, "verified-accepted");
    assert_eq!(
        fixture
            .service
            .accept_review(&fixture.task_id, context.clone())
            .await
            .unwrap(),
        accepted
    );
    let mut conflicting = context;
    conflicting.actor_id = "different-user".into();
    assert!(fixture
        .service
        .accept_review(&fixture.task_id, conflicting)
        .await
        .is_err());
    let state = fixture.store.load_task(&fixture.task_id).await.unwrap();
    assert_eq!(state.review, ReviewDisposition::Accepted);
    assert!(matches!(
        state.execution,
        TaskExecution::Terminal {
            verdict: TaskVerdict::VerifiedAccepted { .. }
        }
    ));
    assert!(fixture.service.review(&fixture.task_id).await.is_err());
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/edit.txt")).unwrap(),
        b"edit-after"
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/multi.txt")).unwrap(),
        b"multi-after"
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/created.txt")).unwrap(),
        b"created-after"
    );
    assert!(!fixture.workspace.join("src/deleted.txt").exists());
    assert_eq!(git_fingerprint(&fixture.workspace), git_before);
}

#[tokio::test]
async fn reject_restores_create_edit_delete_and_multi_edit_with_durable_inverse_journal() {
    let fixture = ReviewFixture::ready("m04-reject").await;
    let git_before = git_fingerprint(&fixture.workspace);
    let view = fixture.view().await;
    let context = fixture.context(&view, "reject-1");
    let result = fixture
        .service
        .reject_review(&fixture.task_id, context.clone(), "not wanted")
        .await
        .unwrap();
    assert_eq!(result.outcome, "rejected");
    assert!(!fixture.workspace.join("src/created.txt").exists());
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/edit.txt")).unwrap(),
        b"edit-before"
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/deleted.txt")).unwrap(),
        b"delete-before"
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/multi.txt")).unwrap(),
        b"multi-before"
    );
    let state = fixture.store.load_task(&fixture.task_id).await.unwrap();
    assert_eq!(state.review, ReviewDisposition::Rejected);
    assert!(matches!(
        state.execution,
        TaskExecution::RepairRequired { .. }
    ));
    let inverse = fixture
        .store
        .mutation_operations_for_owner("review:m04-reject:reject-1")
        .unwrap();
    assert_eq!(inverse.len(), 5);
    assert!(inverse
        .iter()
        .all(|operation| operation.state == MutationState::Receipted));
    assert!(fixture
        .store
        .active_leases(&fixture.workspace_key())
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .service
            .reject_review(&fixture.task_id, context.clone(), "not wanted")
            .await
            .unwrap(),
        result
    );
    assert!(fixture
        .service
        .reject_review(&fixture.task_id, context, "different reason")
        .await
        .is_err());
    assert_eq!(git_fingerprint(&fixture.workspace), git_before);
}

#[tokio::test]
async fn completed_rollback_with_released_lease_converges_when_final_event_was_lost() {
    let fixture = ReviewFixture::ready("m04-lost-final").await;
    let view = fixture.view().await;
    let context = fixture.context(&view, "reject-lost-final");
    let reason = "reject after crash";
    let request_hash = r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
        "action": "reject",
        "actionId": context.action_id.clone(),
        "expectedTaskRevision": context.expected_task_revision,
        "candidateDigest": context.candidate_digest.clone(),
        "actorId": context.actor_id.clone(),
        "sessionId": context.session_id.clone(),
        "reason": reason,
        "checks": Vec::<String>::new(),
    }));
    let (mut state, revision) = fixture
        .store
        .load_task_with_revision(&fixture.task_id)
        .unwrap()
        .unwrap();
    state
        .reject_review(
            Actor::User,
            &view.attempt_id,
            &view.candidate_digest,
            reason.into(),
        )
        .unwrap();
    fixture
        .store
        .save_task_and_events_if_revision(
            &state,
            vec![r_code_kernel::ports::JournalEvent {
                seq: 0,
                task_id: fixture.task_id.clone(),
                kind: "review.rejecting".into(),
                payload: serde_json::json!({
                    "actionId": context.action_id.clone(),
                    "requestHash": request_hash.clone(),
                }),
            }],
            revision,
        )
        .unwrap();
    let artifact_root = fixture
        .profile
        .blobs_root()
        .join("tasks")
        .join(sha256_hex(fixture.task_id.as_bytes()));
    let rollback = DurableReviewService::new(
        Arc::new(V1Store::open(&fixture.profile.database_path()).unwrap()),
        TaskWorkspaceBinding::bind_local(&fixture.task_id, &fixture.workspace, &[]).unwrap(),
        Arc::new(ArtifactStore::for_task(&artifact_root, &fixture.task_id)),
        &fixture.task_id,
        &view.attempt_id,
        &view.work_unit_id,
        &view.candidate_digest,
    )
    .unwrap();
    rollback.rollback(&context.action_id).unwrap();
    assert!(fixture
        .store
        .active_leases(&fixture.workspace_key())
        .unwrap()
        .is_empty());

    let completed = fixture
        .service
        .reject_review(&fixture.task_id, context, reason)
        .await
        .unwrap();
    assert_eq!(completed.outcome, "rejected");
    assert_eq!(
        fixture
            .store
            .task_events(&fixture.task_id)
            .iter()
            .filter(|event| event.kind == "review.rejected")
            .count(),
        1
    );
    assert!(fixture
        .store
        .active_leases(&fixture.workspace_key())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn later_user_edit_makes_projection_stale_and_reject_never_overwrites_it() {
    let fixture = ReviewFixture::ready("m04-conflict").await;
    let view = fixture.view().await;
    std::fs::write(fixture.workspace.join("src/edit.txt"), b"user-later-edit").unwrap();
    let stale = fixture.view().await;
    assert!(stale.stale && stale.conflict);
    let accept_context = fixture.context(&view, "accept-stale");
    assert!(fixture
        .service
        .accept_review(&fixture.task_id, accept_context)
        .await
        .is_err());
    let reject_context = fixture.context(&view, "reject-stale");
    let result = fixture
        .service
        .reject_review(&fixture.task_id, reject_context, "conflict")
        .await
        .unwrap();
    assert_eq!(result.outcome, "conflict");
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/edit.txt")).unwrap(),
        b"user-later-edit"
    );
    assert!(matches!(
        fixture
            .store
            .load_task(&fixture.task_id)
            .await
            .unwrap()
            .execution,
        TaskExecution::RepairRequired { .. }
    ));
    assert!(fixture
        .store
        .task_events(&fixture.task_id)
        .iter()
        .any(|event| event.kind == "review.conflict"));
}

#[tokio::test]
async fn accept_rejects_stale_revision_and_active_workspace_lease() {
    let fixture = ReviewFixture::ready("m04-accept-gates").await;
    let view = fixture.view().await;
    let mut stale = fixture.context(&view, "accept-stale-revision");
    stale.expected_task_revision += 1;
    assert!(fixture
        .service
        .accept_review(&fixture.task_id, stale)
        .await
        .is_err());

    let lease = fixture
        .store
        .acquire_lease(LeaseRequest {
            workspace_key: fixture.workspace_key(),
            operation_id: "foreign-review-writer".into(),
            owner_id: "foreign-owner".into(),
            read_paths: vec![],
            write_paths: vec!["src".into()],
            repo_exclusive: false,
        })
        .unwrap();
    assert!(fixture
        .service
        .accept_review(
            &fixture.task_id,
            fixture.context(&view, "accept-active-lease")
        )
        .await
        .is_err());
    assert!(fixture
        .store
        .release_lease(&lease.lease_id, "foreign-owner", lease.fencing_epoch)
        .unwrap());
    assert!(matches!(
        fixture
            .store
            .load_task(&fixture.task_id)
            .await
            .unwrap()
            .execution,
        TaskExecution::ReviewReady { .. }
    ));
}

#[tokio::test]
async fn concurrent_accept_and_reject_have_one_task_cas_winner() {
    let fixture = ReviewFixture::ready("m04-action-race").await;
    let view = fixture.view().await;
    let other = ApplicationService::compose(
        &fixture.profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(FakeToolService::default()),
    )
    .unwrap();
    let accept = fixture.context(&view, "race-accept");
    let reject = fixture.context(&view, "race-reject");
    let (accepted, rejected) = tokio::join!(
        fixture.service.accept_review(&fixture.task_id, accept),
        other.reject_review(&fixture.task_id, reject, "race rejection")
    );
    assert_ne!(
        accepted.is_ok(),
        rejected.is_ok(),
        "exactly one CAS action wins"
    );
    let state = fixture.store.load_task(&fixture.task_id).await.unwrap();
    assert!(matches!(
        state.execution,
        TaskExecution::Terminal {
            verdict: TaskVerdict::VerifiedAccepted { .. }
        } | TaskExecution::RepairRequired { .. }
    ));
    let terminal_events = fixture
        .store
        .task_events(&fixture.task_id)
        .into_iter()
        .filter(|event| {
            matches!(
                event.kind.as_str(),
                "review.accepted" | "review.rejected" | "review.conflict"
            )
        })
        .count();
    assert_eq!(terminal_events, 1);
}

#[tokio::test]
async fn hardlink_identity_swap_is_stale_even_when_candidate_bytes_match() {
    let fixture = ReviewFixture::ready("m04-hardlink").await;
    let original = fixture.view().await;
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("outside.txt");
    std::fs::write(&outside_file, b"edit-after").unwrap();
    let reviewed_path = fixture.workspace.join("src/edit.txt");
    std::fs::remove_file(&reviewed_path).unwrap();
    std::fs::hard_link(&outside_file, &reviewed_path).unwrap();

    let projected = fixture.service.review(&fixture.task_id).await;
    assert!(
        projected
            .as_ref()
            .map(|view| view.stale || view.conflict)
            .unwrap_or(true),
        "physical identity replacement must fail closed"
    );
    assert!(fixture
        .service
        .accept_review(
            &fixture.task_id,
            fixture.context(&original, "accept-hardlink-swap")
        )
        .await
        .is_err());
    assert!(fixture
        .service
        .reject_review(
            &fixture.task_id,
            fixture.context(&original, "reject-hardlink-swap"),
            "identity conflict"
        )
        .await
        .map(|result| result.outcome == "conflict")
        .unwrap_or(true));
    assert_eq!(std::fs::read(&outside_file).unwrap(), b"edit-after");
}

#[tokio::test]
async fn accept_recaptures_candidate_after_quiescence_wait() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("m04-accept-toctou", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let entered = Arc::new(Notify::new());
    let blocking = Arc::new(
        ApplicationService::compose(
            &profile,
            Arc::new(BlockingModel {
                entered: entered.clone(),
            }),
            Arc::new(FakeToolService::default()),
        )
        .unwrap(),
    );
    blocking.ensure_builtin(&package).unwrap();
    blocking
        .create_task("m04-accept-toctou", "chat", TaskKind::Conversation, vec![])
        .await
        .unwrap();
    blocking
        .set_task_preferences(
            "m04-accept-toctou",
            TaskPreferences {
                workspace_path: Some(workspace.display().to_string()),
                ..TaskPreferences::default()
            },
        )
        .await
        .unwrap();
    blocking
        .send_message("m04-accept-toctou", "hold quiescence")
        .await
        .unwrap();
    if tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .is_err()
    {
        let store = V1Store::open(&profile.database_path()).unwrap();
        panic!(
            "blocking run never reached model: state={:?} events={:?}",
            store.load_task("m04-accept-toctou").await,
            store.task_events("m04-accept-toctou")
        );
    }
    let store = V1Store::open(&profile.database_path()).unwrap();
    let (mut review_state, running_revision) = store
        .load_task_with_revision("m04-accept-toctou")
        .unwrap()
        .unwrap();
    let attempt_id = match &review_state.execution {
        TaskExecution::Running { attempt_id, .. } => attempt_id.clone(),
        other => panic!("expected Running, got {other:?}"),
    };
    let binding = TaskWorkspaceBinding::bind_local("m04-accept-toctou", &workspace, &[]).unwrap();
    let candidate = CandidateManifest::capture(&binding).unwrap().candidate_id;
    review_state.work_units = vec![WorkUnit {
        id: "unit-1".into(),
        description: "review".into(),
        dependencies: vec![],
        acceptance: vec![],
        read_paths: vec![],
        write_paths: vec!["src".into()],
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class: WorkUnitEffectClass::ReadOnly,
        network_ceiling: NetworkCeiling::Offline,
        status: WorkUnitStatus::Completed,
    }];
    // E05: the candidate digest and verification outcome live on the
    // unit's per-unit record; the task-level digest is derived from it.
    review_state.unit_records.insert(
        "unit-1".into(),
        r_code_kernel::task::UnitRecord {
            attempt_id: Some(attempt_id.clone()),
            candidate_digest: Some(candidate.clone()),
            verification: ValidationOutcome::Verified {
                candidate_digest: candidate.clone(),
            },
            settlement: r_code_kernel::task::UnitSettlement::Completed,
        },
    );
    review_state.review = ReviewDisposition::Pending;
    review_state.execution = TaskExecution::ReviewReady {
        attempt_id: attempt_id.clone(),
    };
    store
        .save_task_and_events_if_revision(
            &review_state,
            vec![r_code_kernel::ports::JournalEvent {
                seq: 0,
                task_id: "m04-accept-toctou".into(),
                kind: "review-ready".into(),
                payload: serde_json::json!({
                    "attemptId": attempt_id,
                    "workUnitId": "unit-1",
                    "candidateDigest": candidate,
                    "checks": []
                }),
            }],
            running_revision,
        )
        .unwrap();
    let view = blocking.review("m04-accept-toctou").await.unwrap();
    let context = ReviewActionContext {
        action_id: "accept-toctou".into(),
        expected_task_revision: view.task_revision,
        candidate_digest: view.candidate_digest,
        actor_id: "authenticated-user".into(),
        session_id: "session".into(),
    };
    let acceptor = {
        let blocking = blocking.clone();
        tokio::spawn(async move { blocking.accept_review("m04-accept-toctou", context).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(workspace.join("tracked.txt"), b"external-after-projection").unwrap();
    assert!(blocking.cancel_task("m04-accept-toctou").await.unwrap());
    assert!(acceptor.await.unwrap().is_err());
    assert_eq!(
        std::fs::read(workspace.join("tracked.txt")).unwrap(),
        b"external-after-projection"
    );
    assert!(matches!(
        store
            .load_task("m04-accept-toctou")
            .await
            .unwrap()
            .execution,
        TaskExecution::ReviewReady { .. }
    ));
}

#[tokio::test]
async fn unverified_override_requires_exact_unresolved_checks_and_is_immutable() {
    let fixture = ReviewFixture::ready("m04-override").await;
    let git_before = git_fingerprint(&fixture.workspace);
    let context = fixture.require_override().await;
    for checks in [
        vec![],
        vec!["check:a".into()],
        vec!["check:a".into(), "check:b".into(), "check:c".into()],
        vec!["unknown".into(), "check:b".into()],
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
    let mut empty_actor = context.clone();
    empty_actor.actor_id.clear();
    assert!(fixture
        .service
        .accept_unverified(
            &fixture.task_id,
            UnverifiedOverrideInput {
                context: empty_actor,
                reason: "reason".into(),
                checks: vec!["check:a".into(), "check:b".into()],
            }
        )
        .await
        .is_err());

    let lease = fixture
        .store
        .acquire_lease(LeaseRequest {
            workspace_key: fixture.workspace_key(),
            operation_id: "override-active-lease".into(),
            owner_id: "foreign-owner".into(),
            read_paths: vec![],
            write_paths: vec!["src".into()],
            repo_exclusive: false,
        })
        .unwrap();
    let exact = UnverifiedOverrideInput {
        context: context.clone(),
        reason: "explicit risk acceptance".into(),
        checks: vec!["check:b".into(), "check:a".into()],
    };
    assert!(fixture
        .service
        .accept_unverified(&fixture.task_id, exact.clone())
        .await
        .is_err());
    fixture
        .store
        .release_lease(&lease.lease_id, "foreign-owner", lease.fencing_epoch)
        .unwrap();

    let accepted = fixture
        .service
        .accept_unverified(&fixture.task_id, exact.clone())
        .await
        .unwrap();
    assert_eq!(accepted.outcome, "unverified-accepted");
    assert_eq!(
        fixture
            .service
            .accept_unverified(&fixture.task_id, exact.clone())
            .await
            .unwrap(),
        accepted
    );
    let mut different = exact;
    different.reason = "different reason".into();
    assert!(fixture
        .service
        .accept_unverified(&fixture.task_id, different)
        .await
        .is_err());
    let state = fixture.store.load_task(&fixture.task_id).await.unwrap();
    assert_eq!(state.review, ReviewDisposition::OverrideAccepted);
    assert!(matches!(
        state.execution,
        TaskExecution::Terminal {
            verdict: TaskVerdict::UnverifiedAccepted {
                ref actor_id,
                ref checks,
                ..
            }
        } if actor_id == "authenticated-user" && checks == &["check:a", "check:b"]
    ));
    assert_eq!(
        fixture
            .store
            .task_events(&fixture.task_id)
            .iter()
            .filter(|event| event.kind == "review.unverified-accepted")
            .count(),
        1
    );
    assert_eq!(git_fingerprint(&fixture.workspace), git_before);
}

#[tokio::test]
async fn override_rejects_verified_state_and_live_candidate_drift() {
    let verified = ReviewFixture::ready("m04-override-verified").await;
    let view = verified.view().await;
    assert!(verified
        .service
        .accept_unverified(
            &verified.task_id,
            UnverifiedOverrideInput {
                context: verified.context(&view, "override-verified"),
                reason: "not allowed".into(),
                checks: vec!["check:a".into()],
            }
        )
        .await
        .is_err());

    let drift = ReviewFixture::ready("m04-override-drift").await;
    let context = drift.require_override().await;
    std::fs::write(drift.workspace.join("tracked.txt"), b"user-live-drift").unwrap();
    assert!(drift
        .service
        .accept_unverified(
            &drift.task_id,
            UnverifiedOverrideInput {
                context,
                reason: "not allowed after drift".into(),
                checks: vec!["check:a".into(), "check:b".into()],
            }
        )
        .await
        .is_err());
    assert_eq!(
        std::fs::read(drift.workspace.join("tracked.txt")).unwrap(),
        b"user-live-drift"
    );
}
