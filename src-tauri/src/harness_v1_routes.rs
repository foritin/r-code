//! Task creation and model/Harness route updates for the GUI v1 bridge.

use std::collections::HashSet;

use agent_contract::InferenceOptions;
use chrono::Utc;
use r_code_core::dto::{AgentEngine, Task, TaskMode, TaskState};
use r_code_core::plan::{
    Plan, PlanGoal, PlanImplementationDispatchState, PlanItem, PlanItemState, PlanState, PlanView,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::harness_v1_chat::{project_task, ChatV1Bridge};

/// FR-7: freeze the desktop-owned memory snapshot once, at task creation.
/// Fail-open by design: any error, disabled feature, or missing snapshot
/// yields no payload — memory must never block task creation.
pub fn frozen_memory_payload(
    db: &r_code_store::Database,
    workspace_path: Option<&str>,
) -> Option<Value> {
    let loaded = r_code_store::MemoryStore::new(db)
        .load_snapshot(workspace_path)
        .ok()?;
    let r_code_core::MemorySnapshotLoadOutcome::Ready { snapshot } = loaded.outcome else {
        return None;
    };
    let rendered = r_code_store::render_snapshot(&snapshot)?;
    let entry_ids: Vec<String> = snapshot
        .global_entries
        .iter()
        .chain(&snapshot.project_entries)
        .map(|entry| entry.entry_id.clone())
        .collect();
    Some(json!({
        "rendered": rendered,
        "entryIds": entry_ids,
        "snapshotHash": snapshot.snapshot_hash,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonPlanView {
    task_id: String,
    revision: u64,
    revision_hash: String,
    work_units: Vec<DaemonWorkUnit>,
    approval: Option<DaemonPlanApproval>,
    state: String,
}

#[derive(Debug, Deserialize)]
struct DaemonWorkUnit {
    id: String,
    description: String,
    #[serde(default)]
    dependencies: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DaemonPlanApproval {
    approval_id: String,
    task_id: String,
    plan_revision: String,
    actor: DaemonPlanApprovalActor,
    state: String,
}

#[derive(Debug, Deserialize)]
struct DaemonPlanApprovalActor {
    actor_id: String,
    session_id: String,
    scope: String,
}

#[derive(Debug, Deserialize)]
struct DaemonTaskGoal {
    task_id: String,
    objective: String,
}

impl ChatV1Bridge {
    /// Compatibility entrypoint for callers that rely on daemon defaults.
    pub async fn task_create(
        &self,
        title: &str,
        goal: &str,
        mode: &str,
        workspace_path: Option<&str>,
        system_prompt: &str,
        memory: Option<Value>,
    ) -> Result<Task, crate::harness_v1::HarnessV1Error> {
        self.task_create_with_route(
            title,
            goal,
            mode,
            workspace_path,
            system_prompt,
            None,
            None,
            None,
            None,
            memory.as_ref(),
        )
        .await
    }

    /// Create one daemon task with its complete future-run route.
    #[allow(clippy::too_many_arguments)]
    pub async fn task_create_with_route(
        &self,
        title: &str,
        goal: &str,
        mode: &str,
        workspace_path: Option<&str>,
        system_prompt: &str,
        provider_name: Option<&str>,
        agent_engine: Option<&str>,
        model: Option<&str>,
        inference: Option<&InferenceOptions>,
        memory: Option<&Value>,
    ) -> Result<Task, crate::harness_v1::HarnessV1Error> {
        let mode = TaskMode::try_from_str(mode.trim()).ok_or_else(|| {
            crate::harness_v1::HarnessV1Error::Command(format!("invalid task mode: {mode}"))
        })?;
        let objective = if goal.trim().is_empty() { title } else { goal };
        let engine = match agent_engine {
            Some(value) => Some(AgentEngine::try_from_str(value).ok_or_else(|| {
                crate::harness_v1::HarnessV1Error::Command(format!("invalid agent engine: {value}"))
            })?),
            None if provider_name.is_some() => Some(AgentEngine::RCode),
            None => None,
        };
        let mut params = json!({
            "objective": objective,
            "title": title,
            "mode": mode.to_string(),
            "kind": match mode {
                TaskMode::Plan => "plan-draft",
                TaskMode::Edit | TaskMode::Auto => "implementation",
                TaskMode::Ask => "conversation",
            },
            "workspacePath": workspace_path,
            "systemPrompt": system_prompt,
            "inference": inference,
        });
        if let Some(memory) = memory {
            params["memory"] = memory.clone();
        }
        if let Some(engine) = engine {
            let (harness_id, model_route) = match engine {
                AgentEngine::RCode => (
                    "native.r-code",
                    provider_name.map(|provider_id| {
                        json!({
                            "kind": "host-provider",
                            "providerId": provider_id,
                            "modelId": model,
                        })
                    }),
                ),
                AgentEngine::Codex => (
                    "codex.r-code",
                    Some(json!({
                        "kind": "harness-managed",
                        "harnessId": "codex.r-code",
                        "modelId": model,
                    })),
                ),
            };
            params["harnessId"] = json!(harness_id);
            if let Some(model_route) = model_route {
                params["modelRoute"] = model_route;
            }
        }
        let created = self.call("task.create", params).await?;
        let task_id = created["taskId"].as_str().unwrap_or_default().to_string();
        if task_id.is_empty() {
            return Err(crate::harness_v1::HarnessV1Error::Command(
                "task.create returned no taskId".into(),
            ));
        }
        let now = Utc::now();
        let projected_engine = engine.unwrap_or(AgentEngine::RCode);
        let projected_provider = (projected_engine == AgentEngine::RCode)
            .then_some(provider_name)
            .flatten();
        Ok(project_task(
            &task_id,
            title,
            objective,
            workspace_path,
            TaskState::Idle,
            mode,
            projected_provider,
            model,
            inference,
            projected_engine,
            now,
            now,
        ))
    }

    pub async fn task_set_provider(
        &self,
        task_id: &str,
        provider_name: &str,
    ) -> Result<Task, crate::harness_v1::HarnessV1Error> {
        self.call(
            "task.setPreferences",
            json!({
                "taskId": task_id,
                "harnessId": "native.r-code",
                "modelRoute": {
                    "kind": "host-provider",
                    "providerId": provider_name,
                },
            }),
        )
        .await?;
        Ok(self.task_detail(task_id).await?.task)
    }

    pub async fn task_set_model(
        &self,
        task_id: &str,
        model: Option<&str>,
    ) -> Result<Task, crate::harness_v1::HarnessV1Error> {
        let detail = self.call("task.detail", json!({"taskId": task_id})).await?;
        let engine = detail["engine"].as_str().unwrap_or("r_code");
        let (harness_id, model_route) = if engine == "codex" {
            (
                "codex.r-code",
                json!({
                    "kind": "harness-managed",
                    "harnessId": "codex.r-code",
                    "modelId": model,
                }),
            )
        } else {
            let provider = match detail["provider"].as_str() {
                Some(provider) => provider.to_string(),
                None => self.default_provider().await?,
            };
            (
                "native.r-code",
                json!({
                    "kind": "host-provider",
                    "providerId": provider,
                    "modelId": model,
                }),
            )
        };
        self.call(
            "task.setPreferences",
            json!({
                "taskId": task_id,
                "harnessId": harness_id,
                "modelRoute": model_route,
            }),
        )
        .await?;
        Ok(self.task_detail(task_id).await?.task)
    }

    pub async fn task_set_inference(
        &self,
        task_id: &str,
        inference: &InferenceOptions,
    ) -> Result<Task, crate::harness_v1::HarnessV1Error> {
        self.call(
            "task.setPreferences",
            json!({"taskId": task_id, "inference": inference}),
        )
        .await?;
        Ok(self.task_detail(task_id).await?.task)
    }

    pub async fn task_set_agent_engine(
        &self,
        task_id: &str,
        agent_engine: &str,
    ) -> Result<Task, crate::harness_v1::HarnessV1Error> {
        let engine = AgentEngine::try_from_str(agent_engine).ok_or_else(|| {
            crate::harness_v1::HarnessV1Error::Command(format!(
                "invalid agent engine: {agent_engine}"
            ))
        })?;
        let detail = self.call("task.detail", json!({"taskId": task_id})).await?;
        let (harness_id, model_route) = match engine {
            AgentEngine::Codex => (
                "codex.r-code",
                json!({
                    "kind": "harness-managed",
                    "harnessId": "codex.r-code",
                }),
            ),
            AgentEngine::RCode => {
                let provider = match detail["provider"].as_str() {
                    Some(provider) => provider.to_string(),
                    None => self.default_provider().await?,
                };
                (
                    "native.r-code",
                    json!({
                        "kind": "host-provider",
                        "providerId": provider,
                    }),
                )
            }
        };
        self.call(
            "task.setPreferences",
            json!({
                "taskId": task_id,
                "harnessId": harness_id,
                "modelRoute": model_route,
            }),
        )
        .await?;
        Ok(self.task_detail(task_id).await?.task)
    }

    /// Read the daemon-owned immutable Plan and project it into the existing
    /// desktop Plan DTO. A task without a published Plan remains `None` for
    /// compatibility with the current panel loading contract.
    pub async fn plan_get(
        &self,
        task_id: &str,
    ) -> Result<Option<PlanView>, crate::harness_v1::HarnessV1Error> {
        validate_identifier("requested task id", task_id)?;
        let payload = match self.call("plan.get", json!({"taskId": task_id})).await {
            Ok(payload) => payload,
            Err(error) if is_missing_plan_error(&error, task_id) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(self.project_plan(task_id, payload).await?))
    }

    /// Compatibility entrypoint for the old panel's explicit initialize
    /// action. Planning is daemon-driven now, so this only returns an existing
    /// immutable Plan and never creates a draft in the legacy store.
    pub async fn plan_create(
        &self,
        task_id: &str,
    ) -> Result<PlanView, crate::harness_v1::HarnessV1Error> {
        self.plan_get(task_id).await?.ok_or_else(|| {
            crate::harness_v1::HarnessV1Error::Command(format!(
                "task {task_id} has no daemon Plan; send the first message to start planning"
            ))
        })
    }

    /// Approve exactly the daemon revision displayed by the desktop. The
    /// preflight gives the existing optimistic-revision UI an immediate stale
    /// error; the daemon still enforces the revision hash atomically.
    pub async fn plan_approve(
        &self,
        task_id: &str,
        revision_hash: &str,
        expected_revision: u64,
    ) -> Result<PlanView, crate::harness_v1::HarnessV1Error> {
        validate_revision_hash(revision_hash)?;
        let current = self.plan_get(task_id).await?.ok_or_else(|| {
            crate::harness_v1::HarnessV1Error::Command(format!(
                "task {task_id} has no daemon Plan to approve"
            ))
        })?;
        if current.plan.revision != expected_revision {
            return Err(crate::harness_v1::HarnessV1Error::Command(format!(
                "stale plan revision: expected {}, got {expected_revision}",
                current.plan.revision
            )));
        }
        if current.plan.id != revision_hash {
            return Err(crate::harness_v1::HarnessV1Error::Command(format!(
                "stale plan identity: expected {}, got {revision_hash}",
                current.plan.id
            )));
        }

        let payload = self
            .call(
                "plan.approve",
                json!({"taskId": task_id, "revisionHash": revision_hash}),
            )
            .await?;
        self.project_plan(task_id, payload).await
    }

    async fn project_plan(
        &self,
        task_id: &str,
        payload: Value,
    ) -> Result<PlanView, crate::harness_v1::HarnessV1Error> {
        let task = self.call("task.detail", json!({"taskId": task_id})).await?;
        project_plan_view(task_id, payload, task)
    }

    async fn default_provider(&self) -> Result<String, crate::harness_v1::HarnessV1Error> {
        let providers = self.call("models.available", json!({})).await?;
        let rows = providers.as_array().cloned().unwrap_or_default();
        rows.iter()
            .find(|row| row["is_default"].as_bool().unwrap_or(false))
            .or_else(|| rows.first())
            .and_then(|row| row["selection"].as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                crate::harness_v1::HarnessV1Error::Command(
                    "no configured host Provider is available".to_string(),
                )
            })
    }
}

fn project_plan_view(
    requested_task_id: &str,
    plan_payload: Value,
    task_payload: Value,
) -> Result<PlanView, crate::harness_v1::HarnessV1Error> {
    let plan: DaemonPlanView = serde_json::from_value(plan_payload)
        .map_err(|error| invalid_plan_payload(format!("invalid plan shape: {error}")))?;
    let task: DaemonTaskGoal = serde_json::from_value(task_payload)
        .map_err(|error| invalid_plan_payload(format!("invalid task detail shape: {error}")))?;

    validate_identifier("plan task id", &plan.task_id)?;
    validate_identifier("task detail id", &task.task_id)?;
    validate_non_empty("task objective", &task.objective)?;
    if plan.task_id != requested_task_id || task.task_id != requested_task_id {
        return Err(invalid_plan_payload(format!(
            "task ownership mismatch: requested {requested_task_id}, plan {}, detail {}",
            plan.task_id, task.task_id
        )));
    }
    if plan.revision == 0 {
        return Err(invalid_plan_payload(
            "plan revision must be greater than zero",
        ));
    }
    validate_revision_hash(&plan.revision_hash)?;
    if plan.work_units.is_empty() {
        return Err(invalid_plan_payload(
            "plan must contain at least one work unit",
        ));
    }

    let (state, item_state, approved_revision, approved_at) = match plan.state.as_str() {
        "awaiting-plan-approval" if plan.approval.is_none() => {
            (PlanState::Ready, PlanItemState::Proposed, None, None)
        }
        "ready" if plan.approval.is_some() => (
            PlanState::Approved,
            PlanItemState::Pending,
            Some(plan.revision),
            Some(Utc::now()),
        ),
        "awaiting-plan-approval" => {
            return Err(invalid_plan_payload(
                "awaiting-plan-approval unexpectedly contains an approval",
            ));
        }
        "ready" => return Err(invalid_plan_payload("ready plan is missing its approval")),
        other => {
            return Err(invalid_plan_payload(format!(
                "unsupported daemon plan state {other}"
            )));
        }
    };

    if let Some(approval) = &plan.approval {
        validate_approval(approval, requested_task_id, &plan.revision_hash)?;
    }

    let ids = plan
        .work_units
        .iter()
        .map(|unit| {
            validate_identifier("work unit id", &unit.id)?;
            validate_non_empty("work unit description", &unit.description)?;
            Ok(unit.id.clone())
        })
        .collect::<Result<HashSet<_>, crate::harness_v1::HarnessV1Error>>()?;
    if ids.len() != plan.work_units.len() {
        return Err(invalid_plan_payload(
            "plan contains duplicate work unit ids",
        ));
    }

    let now = approved_at.unwrap_or_else(Utc::now);
    let items = plan
        .work_units
        .into_iter()
        .enumerate()
        .map(|(ordinal, unit)| {
            let mut dependencies = HashSet::new();
            for dependency in &unit.dependencies {
                validate_identifier("work unit dependency", dependency)?;
                if dependency == &unit.id {
                    return Err(invalid_plan_payload(format!(
                        "work unit {} cannot depend on itself",
                        unit.id
                    )));
                }
                if !ids.contains(dependency.as_str()) {
                    return Err(invalid_plan_payload(format!(
                        "work unit {} depends on unknown unit {dependency}",
                        unit.id
                    )));
                }
                if !dependencies.insert(dependency) {
                    return Err(invalid_plan_payload(format!(
                        "work unit {} repeats dependency {dependency}",
                        unit.id
                    )));
                }
            }
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| invalid_plan_payload("plan contains too many work units"))?;
            Ok(PlanItem {
                id: unit.id,
                plan_id: plan.revision_hash.clone(),
                revision: plan.revision,
                ordinal,
                title: unit.description.clone(),
                description: unit.description,
                section_path: Vec::new(),
                state: item_state,
                depends_on: unit.dependencies,
                created_at: now,
                updated_at: now,
                started_at: None,
                completed_at: None,
            })
        })
        .collect::<Result<Vec<_>, crate::harness_v1::HarnessV1Error>>()?;

    Ok(PlanView {
        plan: Plan {
            id: plan.revision_hash.clone(),
            task_id: plan.task_id,
            revision: plan.revision,
            state,
            approved_revision,
            projection_path: None,
            projection_revision: None,
            projection_error: None,
            created_at: now,
            updated_at: now,
            approved_at,
            implementation_dispatch_state: PlanImplementationDispatchState::NotRequested,
            implementation_dispatch_error: None,
            implementation_queue_message_id: None,
            implementation_dispatched_at: None,
            runtime_profile: None,
            catalog_phase: None,
        },
        goal: PlanGoal {
            task_id: task.task_id,
            goal: task.objective,
            updated_at: now,
        },
        items,
        pending_question_set: None,
        continuation_question_set: None,
    })
}

fn validate_approval(
    approval: &DaemonPlanApproval,
    task_id: &str,
    revision_hash: &str,
) -> Result<(), crate::harness_v1::HarnessV1Error> {
    validate_identifier("approval id", &approval.approval_id)?;
    validate_identifier("approval task id", &approval.task_id)?;
    validate_identifier("approval actor id", &approval.actor.actor_id)?;
    validate_identifier("approval session id", &approval.actor.session_id)?;
    if approval.task_id != task_id || approval.plan_revision != revision_hash {
        return Err(invalid_plan_payload(
            "approval does not belong to the projected task revision",
        ));
    }
    if approval.actor.scope != "plan.approve" || approval.state != "active" {
        return Err(invalid_plan_payload(
            "approval is not an active plan.approve authorization",
        ));
    }
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<(), crate::harness_v1::HarnessV1Error> {
    if value.is_empty() || value.trim() != value {
        return Err(invalid_plan_payload(format!(
            "{field} must be non-empty and canonical"
        )));
    }
    Ok(())
}

fn validate_non_empty(field: &str, value: &str) -> Result<(), crate::harness_v1::HarnessV1Error> {
    if value.trim().is_empty() {
        return Err(invalid_plan_payload(format!("{field} must not be empty")));
    }
    Ok(())
}

fn validate_revision_hash(value: &str) -> Result<(), crate::harness_v1::HarnessV1Error> {
    let valid = value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(invalid_plan_payload("plan revision hash is not canonical"))
    }
}

fn invalid_plan_payload(message: impl Into<String>) -> crate::harness_v1::HarnessV1Error {
    crate::harness_v1::HarnessV1Error::Command(format!(
        "daemon Plan payload rejected: {}",
        message.into()
    ))
}

fn is_missing_plan_error(error: &crate::harness_v1::HarnessV1Error, task_id: &str) -> bool {
    let crate::harness_v1::HarnessV1Error::Command(message) = error else {
        return false;
    };
    message.ends_with(&format!("task {task_id} has no plan"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn plan_payload(state: &str, approval: Option<Value>) -> Value {
        json!({
            "taskId": "task-plan",
            "revision": 7,
            "revisionHash": REVISION,
            "workUnits": [
                {
                    "id": "inspect",
                    "description": "Inspect the current checkout",
                    "dependencies": []
                },
                {
                    "id": "implement",
                    "description": "Implement the approved PRD",
                    "dependencies": ["inspect"]
                }
            ],
            "approval": approval,
            "state": state,
            "providerSecret": "SECRET_MUST_NOT_REACH_THE_DESKTOP",
            "routeDigest": "sha256:ROUTE_DIGEST_MUST_NOT_BE_PRESENTED"
        })
    }

    fn task_payload() -> Value {
        json!({
            "task_id": "task-plan",
            "objective": "Deliver the complete PRD",
            "provider_secret": "TASK_SECRET_MUST_NOT_REACH_THE_DESKTOP"
        })
    }

    #[test]
    fn daemon_plan_projection_preserves_identity_goal_order_and_dependencies_without_secrets() {
        let view = project_plan_view(
            "task-plan",
            plan_payload("awaiting-plan-approval", None),
            task_payload(),
        )
        .expect("project awaiting plan");

        assert_eq!(view.plan.id, REVISION);
        assert_eq!(view.plan.task_id, "task-plan");
        assert_eq!(view.plan.revision, 7);
        assert_eq!(view.plan.state, PlanState::Ready);
        assert_eq!(view.plan.approved_revision, None);
        assert_eq!(view.goal.task_id, "task-plan");
        assert_eq!(view.goal.goal, "Deliver the complete PRD");
        assert_eq!(
            view.items
                .iter()
                .map(|item| (item.id.as_str(), item.ordinal, item.depends_on.as_slice()))
                .collect::<Vec<_>>(),
            vec![
                ("inspect", 0, &[][..]),
                ("implement", 1, &["inspect".to_string()][..]),
            ]
        );
        assert!(view.items.iter().all(|item| item.plan_id == REVISION));
        assert!(view.items.iter().all(|item| item.revision == 7));
        assert_eq!(
            view.plan.implementation_dispatch_state,
            PlanImplementationDispatchState::NotRequested
        );
        assert!(view.plan.implementation_queue_message_id.is_none());
        assert!(view.pending_question_set.is_none());
        assert!(view.continuation_question_set.is_none());

        let rendered = serde_json::to_string(&view).expect("serialize projection");
        for forbidden in [
            "SECRET_MUST_NOT_REACH_THE_DESKTOP",
            "TASK_SECRET_MUST_NOT_REACH_THE_DESKTOP",
            "ROUTE_DIGEST_MUST_NOT_BE_PRESENTED",
        ] {
            assert!(!rendered.contains(forbidden), "leaked {forbidden}");
        }
    }

    #[test]
    fn daemon_ready_projection_requires_the_exact_active_approval() {
        let approval = json!({
            "approval_id": "approval-7",
            "task_id": "task-plan",
            "plan_revision": REVISION,
            "actor": {
                "actor_id": "desktop-user",
                "session_id": "approve-request",
                "scope": "plan.approve"
            },
            "state": "active"
        });
        let view = project_plan_view(
            "task-plan",
            plan_payload("ready", Some(approval.clone())),
            task_payload(),
        )
        .expect("project approved plan");
        assert_eq!(view.plan.state, PlanState::Approved);
        assert_eq!(view.plan.approved_revision, Some(7));
        assert!(view.plan.approved_at.is_some());
        assert!(view
            .items
            .iter()
            .all(|item| item.state == PlanItemState::Pending));

        let mut wrong_task = approval;
        wrong_task["task_id"] = json!("task-other");
        let error = project_plan_view(
            "task-plan",
            plan_payload("ready", Some(wrong_task)),
            task_payload(),
        )
        .expect_err("cross-task approval must be rejected");
        assert!(error.to_string().contains("does not belong"));
    }

    #[test]
    fn daemon_plan_projection_rejects_cross_task_or_noncanonical_identity() {
        let cross_task = project_plan_view(
            "task-other",
            plan_payload("awaiting-plan-approval", None),
            task_payload(),
        )
        .expect_err("cross-task projection must fail");
        assert!(cross_task.to_string().contains("task ownership mismatch"));

        let mut invalid_hash = plan_payload("awaiting-plan-approval", None);
        invalid_hash["revisionHash"] = json!("plan-7");
        let invalid = project_plan_view("task-plan", invalid_hash, task_payload())
            .expect_err("non-content-addressed plan identity must fail");
        assert!(invalid.to_string().contains("not canonical"));
    }
}
