//! Review disposition surface, extracted O00 for file-gate headroom.

use super::{
    review_error, review_request_hash, review_required_checks, review_workspace_key,
    validate_review_context, ApplicationError, ApplicationService, ReviewActionContext,
    ReviewActionResult, UnverifiedOverrideInput,
};
use crate::services::artifacts::{sha256_hex, ArtifactStore};
use crate::services::review::{DurableReviewService, DurableReviewView, ReviewError};
use crate::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_kernel::task::{Actor, TaskExecution, TaskState};
use std::path::Path;
use std::sync::Arc;

impl ApplicationService {
    pub async fn review(&self, task_id: &str) -> Result<DurableReviewView, ApplicationError> {
        let (state, revision) = self
            .store
            .load_task_with_revision(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
        self.review_service(&state, false)?
            .project(&state, revision)
            .map_err(review_error)
    }

    pub async fn accept_review(
        &self,
        task_id: &str,
        context: ReviewActionContext,
    ) -> Result<ReviewActionResult, ApplicationError> {
        validate_review_context(&context)?;
        let request_hash = review_request_hash("accept", &context, None, &[]);
        if let Some(result) = self.review_replay(task_id, &context.action_id, &request_hash)? {
            return Ok(result);
        }
        let (mut state, revision) = self.load_exact_review_task(task_id, &context)?;
        let service = self.review_service(&state, false)?;
        let projection = service.project(&state, revision).map_err(review_error)?;
        if projection.stale
            || projection.conflict
            || projection.checks.iter().any(|check| !check.passed)
            || state.review_verified_digest().is_none()
        {
            return Err(ApplicationError::Task(
                "review candidate or evidence is stale".to_string(),
            ));
        }
        if !self
            .runs
            .await_quiescent(task_id, std::time::Duration::from_secs(5))
            .await
        {
            return Err(ApplicationError::Task(
                "review still has an active run or process".to_string(),
            ));
        }
        let workspace_key = review_workspace_key(&state)?;
        if !self
            .store
            .active_leases(&workspace_key)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .is_empty()
        {
            return Err(ApplicationError::Task(
                "review still has an active workspace lease".to_string(),
            ));
        }
        let final_projection = service.project(&state, revision).map_err(review_error)?;
        if final_projection.stale
            || final_projection.conflict
            || final_projection.checks.iter().any(|check| !check.passed)
        {
            return Err(ApplicationError::Task(
                "review candidate changed while waiting for quiescence".to_string(),
            ));
        }
        let attempt_id = projection.attempt_id.clone();
        let accepted_checks = projection
            .checks
            .iter()
            .map(|check| check.check_id.clone())
            .collect::<Vec<_>>();
        state
            .accept_verified(
                Actor::User,
                &attempt_id,
                &context.candidate_digest,
                &context.actor_id,
            )
            .map_err(|error| ApplicationError::Task(error.to_string()))?;
        let result = ReviewActionResult {
            task_id: task_id.to_string(),
            action_id: context.action_id.clone(),
            outcome: "verified-accepted".to_string(),
            candidate_digest: context.candidate_digest.clone(),
            paths: projection
                .changes
                .iter()
                .map(|change| change.path.clone())
                .collect(),
        };
        self.save_review_action(
            &state,
            revision,
            "review.accepted",
            &context,
            &request_hash,
            Some("verified candidate accepted"),
            &accepted_checks,
            &result,
        )?;
        Ok(result)
    }

    async fn resume_rejection(
        &self,
        task_id: &str,
        context: ReviewActionContext,
        reason: &str,
        request_hash: String,
    ) -> Result<ReviewActionResult, ApplicationError> {
        let (mut state, revision) = self
            .store
            .load_task_with_revision(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
        if state.task_candidate_digest().as_deref() != Some(context.candidate_digest.as_str())
            || !matches!(state.execution, TaskExecution::RepairRequired { .. })
        {
            return Err(ApplicationError::Task(
                "stale rollback continuation".to_string(),
            ));
        }
        let service = self.review_service(&state, true)?;
        let rollback = service.rollback(&context.action_id);
        let (event_kind, outcome, paths) = match rollback {
            Ok(rollback) => ("review.rejected", "rejected", rollback.restored_paths),
            Err(ReviewError::Conflict(path)) => {
                let (attempt_id, work_unit_id) = match &state.execution {
                    TaskExecution::RepairRequired {
                        attempt_id,
                        work_unit_id,
                        ..
                    } => (attempt_id.clone(), work_unit_id.clone()),
                    _ => (None, None),
                };
                state
                    .require_repair(
                        Actor::Host,
                        attempt_id,
                        work_unit_id,
                        "review rollback conflict".to_string(),
                        false,
                    )
                    .map_err(|error| ApplicationError::Task(error.to_string()))?;
                ("review.conflict", "conflict", vec![path])
            }
            Err(error) => return Err(review_error(error)),
        };
        let result = ReviewActionResult {
            task_id: task_id.to_string(),
            action_id: context.action_id.clone(),
            outcome: outcome.to_string(),
            candidate_digest: context.candidate_digest.clone(),
            paths,
        };
        self.save_review_action(
            &state,
            revision,
            event_kind,
            &context,
            &request_hash,
            Some(reason),
            &[],
            &result,
        )?;
        Ok(result)
    }

    pub async fn reject_review(
        &self,
        task_id: &str,
        context: ReviewActionContext,
        reason: &str,
    ) -> Result<ReviewActionResult, ApplicationError> {
        validate_review_context(&context)?;
        if reason.trim().is_empty() {
            return Err(ApplicationError::Task("review reason is required".into()));
        }
        let request_hash = review_request_hash("reject", &context, Some(reason), &[]);
        if let Some(result) = self.review_replay(task_id, &context.action_id, &request_hash)? {
            return Ok(result);
        }
        if self.store.task_events(task_id).iter().any(|event| {
            event.kind == "review.rejecting"
                && event
                    .payload
                    .get("actionId")
                    .and_then(|value| value.as_str())
                    == Some(context.action_id.as_str())
                && event
                    .payload
                    .get("requestHash")
                    .and_then(|value| value.as_str())
                    == Some(request_hash.as_str())
        }) {
            return self
                .resume_rejection(task_id, context, reason, request_hash)
                .await;
        }
        let (mut state, revision) = self.load_exact_review_task(task_id, &context)?;
        let service = self.review_service(&state, false)?;
        let projection = service.project(&state, revision).map_err(review_error)?;
        if projection.stale || projection.conflict {
            state
                .reject_review(
                    Actor::User,
                    &projection.attempt_id,
                    &context.candidate_digest,
                    "review conflict: candidate changed after verification".to_string(),
                )
                .map_err(|error| ApplicationError::Task(error.to_string()))?;
            let result = ReviewActionResult {
                task_id: task_id.to_string(),
                action_id: context.action_id.clone(),
                outcome: "conflict".to_string(),
                candidate_digest: context.candidate_digest.clone(),
                paths: projection
                    .changes
                    .iter()
                    .filter(|change| !change.current_matches_after)
                    .map(|change| change.path.clone())
                    .collect(),
            };
            self.save_review_action(
                &state,
                revision,
                "review.conflict",
                &context,
                &request_hash,
                Some(reason),
                &[],
                &result,
            )?;
            return Ok(result);
        }
        state
            .reject_review(
                Actor::User,
                &projection.attempt_id,
                &context.candidate_digest,
                reason.to_string(),
            )
            .map_err(|error| ApplicationError::Task(error.to_string()))?;
        let rejecting_result = ReviewActionResult {
            task_id: task_id.to_string(),
            action_id: context.action_id.clone(),
            outcome: "rollback-in-progress".to_string(),
            candidate_digest: context.candidate_digest.clone(),
            paths: projection
                .changes
                .iter()
                .map(|change| change.path.clone())
                .collect(),
        };
        let next_revision = self.save_review_action(
            &state,
            revision,
            "review.rejecting",
            &context,
            &request_hash,
            Some(reason),
            &[],
            &rejecting_result,
        )?;
        let rollback = service.rollback(&context.action_id);
        let (event_kind, outcome, paths) = match rollback {
            Ok(rollback) => ("review.rejected", "rejected", rollback.restored_paths),
            Err(ReviewError::Conflict(path)) => {
                state
                    .require_repair(
                        Actor::Host,
                        Some(projection.attempt_id),
                        Some(projection.work_unit_id),
                        "review rollback conflict".to_string(),
                        false,
                    )
                    .map_err(|error| ApplicationError::Task(error.to_string()))?;
                ("review.conflict", "conflict", vec![path])
            }
            Err(error) => return Err(review_error(error)),
        };
        let result = ReviewActionResult {
            task_id: task_id.to_string(),
            action_id: context.action_id.clone(),
            outcome: outcome.to_string(),
            candidate_digest: context.candidate_digest.clone(),
            paths,
        };
        self.save_review_action(
            &state,
            next_revision,
            event_kind,
            &context,
            &request_hash,
            Some(reason),
            &[],
            &result,
        )?;
        Ok(result)
    }

    pub async fn accept_unverified(
        &self,
        task_id: &str,
        mut input: UnverifiedOverrideInput,
    ) -> Result<ReviewActionResult, ApplicationError> {
        validate_review_context(&input.context)?;
        if input.reason.trim().is_empty() || input.checks.is_empty() {
            return Err(ApplicationError::Task(
                "override requires a reason and failed/missing checks".to_string(),
            ));
        }
        input.checks.sort();
        input.checks.dedup();
        let request_hash = review_request_hash(
            "override",
            &input.context,
            Some(&input.reason),
            &input.checks,
        );
        if let Some(result) =
            self.review_replay(task_id, &input.context.action_id, &request_hash)?
        {
            return Ok(result);
        }
        let (mut state, revision) = self
            .store
            .load_task_with_revision(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
        if revision != input.context.expected_task_revision
            || state.override_candidate_digest().as_deref()
                != Some(input.context.candidate_digest.as_str())
            || !matches!(state.execution, TaskExecution::RepairRequired { .. })
        {
            return Err(ApplicationError::Task("stale override request".into()));
        }
        let known = review_required_checks(&state);
        let unresolved = known
            .iter()
            .filter(|check| {
                !state.evidence.iter().any(|record| {
                    record.task_id == state.contract.task_id
                        && record.check_id == **check
                        && record.candidate_digest == input.context.candidate_digest
                        && record.passed
                        && matches!(
                            record.recorded_by,
                            r_code_harness_protocol::Provenance::Host
                        )
                        && !record.definition_identity.is_empty()
                        && !record.environment_fingerprint.is_empty()
                })
            })
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let supplied = input
            .checks
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if supplied != unresolved || unresolved.is_empty() {
            return Err(ApplicationError::Task(
                "override check list does not match failed/missing checks".into(),
            ));
        }
        let workspace_path = state
            .preferences
            .workspace_path
            .as_deref()
            .ok_or_else(|| ApplicationError::Task("override workspace is missing".into()))?;
        let binding = TaskWorkspaceBinding::bind_local(task_id, Path::new(workspace_path), &[])
            .map_err(|error| ApplicationError::Task(error.to_string()))?;
        if !CandidateManifest::capture(&binding)
            .map(|candidate| candidate.candidate_id == input.context.candidate_digest)
            .unwrap_or(false)
        {
            return Err(ApplicationError::Task(
                "override candidate changed after verification".into(),
            ));
        }
        if !self
            .runs
            .await_quiescent(task_id, std::time::Duration::from_secs(5))
            .await
            || !self
                .store
                .active_leases(&review_workspace_key(&state)?)
                .map_err(|error| ApplicationError::Store(error.to_string()))?
                .is_empty()
        {
            return Err(ApplicationError::Task(
                "override has an active run, verifier, process or lease".into(),
            ));
        }
        if !CandidateManifest::capture(&binding)
            .map(|candidate| candidate.candidate_id == input.context.candidate_digest)
            .unwrap_or(false)
        {
            return Err(ApplicationError::Task(
                "override candidate changed while waiting for quiescence".into(),
            ));
        }
        state
            .accept_unverified(
                Actor::User,
                &input.context.candidate_digest,
                &input.context.actor_id,
                &input.reason,
                &input.checks,
            )
            .map_err(|error| ApplicationError::Task(error.to_string()))?;
        let result = ReviewActionResult {
            task_id: task_id.to_string(),
            action_id: input.context.action_id.clone(),
            outcome: "unverified-accepted".to_string(),
            candidate_digest: input.context.candidate_digest.clone(),
            paths: Vec::new(),
        };
        self.save_review_action(
            &state,
            revision,
            "review.unverified-accepted",
            &input.context,
            &request_hash,
            Some(&input.reason),
            &input.checks,
            &result,
        )?;
        Ok(result)
    }

    fn load_exact_review_task(
        &self,
        task_id: &str,
        context: &ReviewActionContext,
    ) -> Result<(TaskState, u64), ApplicationError> {
        let (state, revision) = self
            .store
            .load_task_with_revision(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
        if revision != context.expected_task_revision
            || state.task_candidate_digest().as_deref() != Some(context.candidate_digest.as_str())
            || !matches!(state.execution, TaskExecution::ReviewReady { .. })
        {
            return Err(ApplicationError::Task("stale review request".to_string()));
        }
        Ok((state, revision))
    }

    fn review_service(
        &self,
        state: &TaskState,
        allow_repair: bool,
    ) -> Result<DurableReviewService, ApplicationError> {
        let attempt_id = match &state.execution {
            TaskExecution::ReviewReady { attempt_id } => attempt_id.clone(),
            TaskExecution::RepairRequired {
                attempt_id: Some(attempt_id),
                ..
            } if allow_repair => attempt_id.clone(),
            _ => return Err(ApplicationError::Task("task is not review-ready".into())),
        };
        // E05: the task-level candidate digest is DERIVED from the
        // per-unit records anchored on the review attempt — the wire's
        // candidateDigest aggregate, never a stored single-slot copy.
        let candidate_digest = state
            .task_candidate_digest()
            .ok_or_else(|| ApplicationError::Task("review candidate is missing".into()))?;
        let work_unit_id = self
            .store
            .task_events(&state.contract.task_id)
            .into_iter()
            .rev()
            .find(|event| {
                event.kind == "review-ready"
                    && event
                        .payload
                        .get("attemptId")
                        .and_then(|value| value.as_str())
                        == Some(attempt_id.as_str())
                    && event
                        .payload
                        .get("candidateDigest")
                        .and_then(|value| value.as_str())
                        == Some(candidate_digest.as_str())
            })
            .and_then(|event| {
                event
                    .payload
                    .get("workUnitId")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| ApplicationError::Task("review WorkUnit is missing".into()))?;
        let workspace_path = state
            .preferences
            .workspace_path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| ApplicationError::Task("review workspace is missing".into()))?;
        let binding = TaskWorkspaceBinding::bind_local(
            &state.contract.task_id,
            Path::new(workspace_path),
            &[],
        )
        .map_err(|error| ApplicationError::Task(error.to_string()))?;
        let artifact_key = sha256_hex(state.contract.task_id.as_bytes());
        let artifacts = Arc::new(ArtifactStore::for_task(
            self.artifact_tasks_root.join(artifact_key),
            &state.contract.task_id,
        ));
        DurableReviewService::new(
            self.store.clone(),
            binding,
            artifacts,
            &state.contract.task_id,
            attempt_id,
            work_unit_id,
            candidate_digest,
        )
        .map_err(review_error)
    }

    fn review_replay(
        &self,
        task_id: &str,
        action_id: &str,
        request_hash: &str,
    ) -> Result<Option<ReviewActionResult>, ApplicationError> {
        for event in self.store.task_events(task_id) {
            if event
                .payload
                .get("actionId")
                .and_then(|value| value.as_str())
                != Some(action_id)
            {
                continue;
            }
            if event
                .payload
                .get("requestHash")
                .and_then(|value| value.as_str())
                != Some(request_hash)
            {
                return Err(ApplicationError::Task(
                    "review action id was reused with different input".to_string(),
                ));
            }
            if matches!(
                event.kind.as_str(),
                "review.accepted"
                    | "review.rejected"
                    | "review.conflict"
                    | "review.unverified-accepted"
            ) {
                let result =
                    event.payload.get("result").cloned().ok_or_else(|| {
                        ApplicationError::Store("review result is missing".into())
                    })?;
                return serde_json::from_value(result)
                    .map(Some)
                    .map_err(|error| ApplicationError::Store(error.to_string()));
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn save_review_action(
        &self,
        state: &TaskState,
        revision: u64,
        kind: &str,
        context: &ReviewActionContext,
        request_hash: &str,
        reason: Option<&str>,
        checks: &[String],
        result: &ReviewActionResult,
    ) -> Result<u64, ApplicationError> {
        match self.store.save_task_and_events_if_revision(
            state,
            vec![r_code_kernel::ports::JournalEvent {
                seq: 0,
                task_id: state.contract.task_id.clone(),
                kind: kind.to_string(),
                payload: serde_json::json!({
                    "actionId": context.action_id,
                    "requestHash": request_hash,
                    "actorId": context.actor_id,
                    "sessionId": context.session_id,
                    "reason": reason,
                    "candidateDigest": context.candidate_digest,
                    "checks": checks,
                    "result": result,
                }),
            }],
            revision,
        ) {
            Ok(revision) => Ok(revision),
            Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. })
                if kind != "review.rejecting" =>
            {
                if self
                    .review_replay(&state.contract.task_id, &context.action_id, request_hash)?
                    .as_ref()
                    == Some(result)
                {
                    return self
                        .store
                        .load_task_with_revision(&state.contract.task_id)
                        .map_err(|error| ApplicationError::Store(error.to_string()))?
                        .map(|(_, revision)| revision)
                        .ok_or_else(|| {
                            ApplicationError::Task("review task disappeared".to_string())
                        });
                }
                Err(ApplicationError::Task("stale review action".to_string()))
            }
            Err(error) => Err(ApplicationError::Store(error.to_string())),
        }
    }
}
