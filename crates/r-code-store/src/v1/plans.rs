//! Durable immutable plan revisions and exact-revision approvals.

use crate::v1::journal::{now_ms as journal_now_ms, update_task_and_events_if_revision};
use crate::v1::V1Store;
use r_code_harness_protocol::services::WorkUnitWire;
use r_code_kernel::plans::{
    PlanApproval, PlanApprovalActor, PlanApprovalError, PlanApprovalState, PlanRevision,
    PlanRevisionError, PlanRevisionMaterial,
};
use r_code_kernel::ports::JournalEvent;
use r_code_kernel::task::{Actor, PlanApprovalRef, PlanRevisionRef, TaskState};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};

/// Typed failures from the plan repository. Payloads and actor sessions are
/// intentionally absent from messages so malformed input cannot leak secrets.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanStoreError {
    #[error("plan storage failed: {0}")]
    Sqlite(String),
    #[error(transparent)]
    InvalidRevision(#[from] PlanRevisionError),
    #[error(transparent)]
    InvalidApproval(#[from] PlanApprovalError),
    #[error("stale plan head for task {task_id}: expected {expected:?}, actual {actual:?}")]
    StaleHead {
        task_id: String,
        expected: Option<PlanRevisionRef>,
        actual: Option<PlanRevisionRef>,
    },
    #[error("plan revision {provided} is not the next revision after {current:?}")]
    NonMonotonicRevision { current: Option<u64>, provided: u64 },
    #[error("plan revision parent does not match the current task head")]
    ParentMismatch,
    #[error("plan revision already exists with conflicting content")]
    RevisionConflict,
    #[error("plan approval {0} was not found")]
    ApprovalNotFound(String),
    #[error("plan approval id already identifies a different approval")]
    ApprovalIdConflict,
    #[error("task already has a different active plan approval")]
    ActiveApprovalExists,
    #[error("effect approval fields are invalid")]
    EffectApprovalInvalid,
    #[error("effect approval id already identifies a different approval")]
    EffectApprovalIdConflict,
    #[error("task already has a different active effect approval for this work unit")]
    EffectActiveApprovalExists,
    #[error("plan approval belongs to a different task")]
    ApprovalTaskMismatch,
    #[error("plan approval refers to a different revision")]
    ApprovalRevisionMismatch,
    #[error("plan approval is superseded")]
    ApprovalSuperseded,
    #[error("plan approval revision is not the current task head")]
    ApprovalNotCurrent,
    #[error("stored plan data failed integrity validation")]
    CorruptRecord,
    #[error("task {0} was not found while updating plan state")]
    TaskNotFound(String),
    #[error("stale task revision for {task_id}: expected {expected}, actual {actual}")]
    StaleTaskRevision {
        task_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("plan task transition was rejected: {0}")]
    TaskTransition(String),
    #[error("plan task transaction failed: {0}")]
    TaskStore(String),
}

impl From<rusqlite::Error> for PlanStoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

impl V1Store {
    /// Publish an immutable revision when `expected_head` still identifies the
    /// task's current plan. Updating the head and superseding its approval are
    /// one SQLite transaction.
    pub fn publish_plan_revision(
        &self,
        revision: &PlanRevision,
        expected_head: Option<&PlanRevisionRef>,
    ) -> Result<PlanRevisionRef, PlanStoreError> {
        let payload = revision.canonical_json()?;
        self.publish_plan_revision_with_payload(revision, expected_head, &payload, false)
    }

    /// Load the immutable revision currently selected for `task_id`.
    pub fn current_plan_revision(
        &self,
        task_id: &str,
    ) -> Result<Option<PlanRevision>, PlanStoreError> {
        let connection = self.connection();
        let Some((head, head_number)) = load_head(&connection, task_id)? else {
            return Ok(None);
        };
        let row =
            load_plan_revision_row(&connection, &head)?.ok_or(PlanStoreError::CorruptRecord)?;
        let revision = decode_plan_revision(&row, Some(task_id))?;
        if revision.material().revision != head_number {
            return Err(PlanStoreError::CorruptRecord);
        }
        Ok(Some(revision))
    }

    /// Resolve one revision by its content address.
    pub fn load_plan_revision(
        &self,
        revision: &PlanRevisionRef,
    ) -> Result<Option<PlanRevision>, PlanStoreError> {
        let connection = self.connection();
        load_plan_revision_row(&connection, revision)?
            .map(|row| decode_plan_revision(&row, None))
            .transpose()
    }

    /// Return only the current content address, suitable for a publish CAS.
    pub fn current_plan_head(
        &self,
        task_id: &str,
    ) -> Result<Option<PlanRevisionRef>, PlanStoreError> {
        load_head(&self.connection(), task_id).map(|head| head.map(|(revision, _)| revision))
    }

    /// Approve exactly the current revision. Replaying the same approval id
    /// with identical content is idempotent; every other reuse is explicit.
    pub fn approve_plan_revision(
        &self,
        task_id: &str,
        expected_revision: &PlanRevisionRef,
        approval_id: &str,
        actor: PlanApprovalActor,
    ) -> Result<PlanApproval, PlanStoreError> {
        let approval = PlanApproval::new(approval_id, task_id, expected_revision.clone(), actor)?;
        self.persist_plan_approval(&approval)
    }

    /// Approve the exact current plan and move its task from
    /// AwaitingPlanApproval to Ready in one immediate SQLite transaction.
    /// The task CAS, approval row and audit event either all commit or all
    /// roll back.
    pub fn approve_plan_revision_and_mark_ready(
        &self,
        task_id: &str,
        expected_revision: &PlanRevisionRef,
        approval_id: &str,
        actor: PlanApprovalActor,
        expected_task_revision: u64,
    ) -> Result<(PlanApproval, u64), PlanStoreError> {
        let approval = PlanApproval::new(approval_id, task_id, expected_revision.clone(), actor)?;
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let head = load_head(&transaction, task_id)?;
        if head.as_ref().map(|(revision, _)| revision) != Some(expected_revision) {
            return Err(PlanStoreError::ApprovalNotCurrent);
        }

        if let Some(existing) = load_approval(&transaction, approval_id)? {
            if existing == approval {
                let revision = transaction
                    .query_row(
                        "SELECT revision FROM tasks WHERE task_id = ?1",
                        params![task_id],
                        |row| row.get::<_, u64>(0),
                    )
                    .map_err(|_| PlanStoreError::TaskNotFound(task_id.to_string()))?;
                transaction.commit()?;
                return Ok((existing, revision));
            }
            return Err(PlanStoreError::ApprovalIdConflict);
        }
        let active_id: Option<String> = transaction
            .query_row(
                "SELECT approval_id FROM plan_approvals
                 WHERE task_id = ?1 AND state = 'active'",
                params![task_id],
                |row| row.get(0),
            )
            .optional()?;
        if active_id.is_some() {
            return Err(PlanStoreError::ActiveApprovalExists);
        }

        let task_row: Option<(String, u64)> = transaction
            .query_row(
                "SELECT state_json, revision FROM tasks WHERE task_id = ?1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (state_json, actual_task_revision) =
            task_row.ok_or_else(|| PlanStoreError::TaskNotFound(task_id.to_string()))?;
        if actual_task_revision != expected_task_revision {
            return Err(PlanStoreError::StaleTaskRevision {
                task_id: task_id.to_string(),
                expected: expected_task_revision,
                actual: actual_task_revision,
            });
        }
        let mut state: TaskState =
            serde_json::from_str(&state_json).map_err(|_| PlanStoreError::CorruptRecord)?;
        state
            .mark_plan_ready(
                Actor::Host,
                PlanApprovalRef {
                    approval_id: approval.approval_id.clone(),
                    plan_revision: approval.plan_revision.clone(),
                },
            )
            .map_err(|error| PlanStoreError::TaskTransition(error.to_string()))?;

        transaction.execute(
            "INSERT INTO plan_approvals(
                approval_id, task_id, plan_revision, actor_id, session_id,
                scope, state, approval_json, created_at_ms, superseded_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, NULL)",
            params![
                approval.approval_id,
                approval.task_id,
                approval.plan_revision.as_str(),
                approval.actor.actor_id,
                approval.actor.session_id,
                approval.actor.scope,
                approval.canonical_json()?,
                journal_now_ms(),
            ],
        )?;
        let next_task_revision = update_task_and_events_if_revision(
            &transaction,
            &state,
            vec![JournalEvent {
                seq: 0,
                task_id: task_id.to_string(),
                kind: "plan.approved".to_string(),
                payload: serde_json::json!({
                    "approvalId": approval.approval_id,
                    "revisionHash": approval.plan_revision.as_str(),
                    "actorId": approval.actor.actor_id,
                    "sessionId": approval.actor.session_id,
                    "scope": approval.actor.scope,
                }),
            }],
            expected_task_revision,
        )
        .map_err(|error| PlanStoreError::TaskStore(error.to_string()))?;
        if self.take_debug_fail_next_save() {
            return Err(PlanStoreError::TaskStore(
                "injected fault before commit".to_string(),
            ));
        }
        transaction.commit()?;
        Ok((approval, next_task_revision))
    }

    /// Load an approval without treating a superseded record as active.
    pub fn load_plan_approval(
        &self,
        approval_id: &str,
    ) -> Result<Option<PlanApproval>, PlanStoreError> {
        load_approval(&self.connection(), approval_id)
    }

    /// Load the active approval for a task, if it has one.
    pub fn load_active_plan_approval(
        &self,
        task_id: &str,
    ) -> Result<Option<PlanApproval>, PlanStoreError> {
        let connection = self.connection();
        let approval_id: Option<String> = connection
            .query_row(
                "SELECT approval_id FROM plan_approvals
                 WHERE task_id = ?1 AND state = 'active'",
                params![task_id],
                |row| row.get(0),
            )
            .optional()?;
        approval_id
            .map(|approval_id| {
                load_approval(&connection, &approval_id)?.ok_or(PlanStoreError::CorruptRecord)
            })
            .transpose()
    }

    /// Validate the exact reference embedded in an execution/repair run.
    pub fn validate_active_plan_approval(
        &self,
        task_id: &str,
        approval: &PlanApprovalRef,
    ) -> Result<PlanApproval, PlanStoreError> {
        let connection = self.connection();
        validate_active_approval(&connection, task_id, approval)
    }

    fn persist_plan_approval(
        &self,
        approval: &PlanApproval,
    ) -> Result<PlanApproval, PlanStoreError> {
        approval.validate()?;
        if approval.state != PlanApprovalState::Active {
            return Err(PlanStoreError::ApprovalSuperseded);
        }
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let head = load_head(&transaction, &approval.task_id)?;
        if head.as_ref().map(|(revision, _)| revision) != Some(&approval.plan_revision) {
            return Err(PlanStoreError::ApprovalNotCurrent);
        }

        if let Some(existing) = load_approval(&transaction, &approval.approval_id)? {
            if existing == *approval {
                transaction.commit()?;
                return Ok(existing);
            }
            return Err(PlanStoreError::ApprovalIdConflict);
        }

        let active_id: Option<String> = transaction
            .query_row(
                "SELECT approval_id FROM plan_approvals
                 WHERE task_id = ?1 AND state = 'active'",
                params![approval.task_id],
                |row| row.get(0),
            )
            .optional()?;
        if active_id.is_some() {
            return Err(PlanStoreError::ActiveApprovalExists);
        }

        transaction.execute(
            "INSERT INTO plan_approvals(
                approval_id, task_id, plan_revision, actor_id, session_id,
                scope, state, approval_json, created_at_ms, superseded_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, NULL)",
            params![
                approval.approval_id,
                approval.task_id,
                approval.plan_revision.as_str(),
                approval.actor.actor_id,
                approval.actor.session_id,
                approval.actor.scope,
                approval.canonical_json()?,
                now_ms(),
            ],
        )?;
        transaction.commit()?;
        Ok(approval.clone())
    }

    fn publish_plan_revision_with_payload(
        &self,
        revision: &PlanRevision,
        expected_head: Option<&PlanRevisionRef>,
        payload_json: &str,
        allow_historical_first: bool,
    ) -> Result<PlanRevisionRef, PlanStoreError> {
        revision.validate_identity()?;
        let material = revision.material();
        let canonical_json = revision.canonical_json()?;
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load_head(&transaction, &material.task_id)?;
        let existing = load_plan_revision_row(&transaction, revision.reference())?;
        if let Some(row) = &existing {
            let stored = decode_plan_revision(row, Some(&material.task_id))?;
            if stored != *revision
                || row.material_json != canonical_json
                || row.payload_json != payload_json
            {
                return Err(PlanStoreError::RevisionConflict);
            }
            if current.as_ref().map(|(head, _)| head) == Some(revision.reference()) {
                if current.as_ref().map(|(_, number)| *number) != Some(material.revision) {
                    return Err(PlanStoreError::CorruptRecord);
                }
                transaction.commit()?;
                return Ok(revision.reference().clone());
            }
        }
        let actual_head = current.as_ref().map(|(head, _)| head.clone());
        if actual_head.as_ref() != expected_head {
            return Err(PlanStoreError::StaleHead {
                task_id: material.task_id.clone(),
                expected: expected_head.cloned(),
                actual: actual_head,
            });
        }

        validate_publish_lineage(material, current.as_ref(), allow_historical_first)?;
        if existing.is_none() {
            transaction.execute(
                "INSERT INTO plan_revisions(
                    plan_revision, task_id, revision_number, parent_revision,
                    current_base_hash, content_sha256, material_json,
                    payload_json, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    revision.reference().as_str(),
                    material.task_id,
                    material.revision,
                    material
                        .parent_revision
                        .as_ref()
                        .map(PlanRevisionRef::as_str),
                    material.current_base_hash,
                    revision.reference().as_str().trim_start_matches("sha256:"),
                    canonical_json,
                    payload_json,
                    now_ms(),
                ],
            )?;
        }

        match current {
            Some((head, _)) => {
                let changed = transaction.execute(
                    "UPDATE task_plan_heads
                     SET plan_revision = ?1, revision_number = ?2, updated_at_ms = ?3
                     WHERE task_id = ?4 AND plan_revision = ?5",
                    params![
                        revision.reference().as_str(),
                        material.revision,
                        now_ms(),
                        material.task_id,
                        head.as_str(),
                    ],
                )?;
                if changed != 1 {
                    return Err(PlanStoreError::StaleHead {
                        task_id: material.task_id.clone(),
                        expected: Some(head),
                        actual: load_head(&transaction, &material.task_id)?
                            .map(|(revision, _)| revision),
                    });
                }
            }
            None => {
                transaction.execute(
                    "INSERT INTO task_plan_heads(
                        task_id, plan_revision, revision_number, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        material.task_id,
                        revision.reference().as_str(),
                        material.revision,
                        now_ms(),
                    ],
                )?;
            }
        }
        supersede_active_approval(&transaction, &material.task_id)?;
        transaction.execute(
            "DELETE FROM reviews WHERE task_id = ?1",
            params![format!("plan:{}", material.task_id)],
        )?;
        transaction.commit()?;
        Ok(revision.reference().clone())
    }

    /// Legacy opaque-plan adapter. It preserves arbitrary bytes and maps them
    /// into an immutable synthetic revision; it never creates an approval.
    pub fn save_plan(
        &self,
        task_id: &str,
        revision: u64,
        plan_json: &str,
    ) -> Result<(), rusqlite::Error> {
        let current = self
            .current_plan_revision(task_id)
            .map_err(legacy_sql_error)?;
        if let Some(current) = &current {
            if current.material().revision == revision {
                let stored = self
                    .current_plan_payload(task_id)
                    .map_err(legacy_sql_error)?;
                if stored.as_deref() == Some(plan_json) {
                    return Ok(());
                }
                return Err(legacy_sql_error(PlanStoreError::RevisionConflict));
            }
        }

        let parent_revision = current.as_ref().map(|plan| plan.reference().clone());
        let payload_digest = digest_string(plan_json);
        let unavailable = digest_string("legacy-plan-field-unavailable");
        let synthetic = PlanRevision::new(PlanRevisionMaterial {
            task_id: task_id.to_string(),
            revision,
            parent_revision: parent_revision.clone(),
            current_base_hash: payload_digest,
            workspace_baseline: unavailable.clone(),
            route_digest: unavailable.clone(),
            prompt_digest: unavailable.clone(),
            permission_digest: unavailable.clone(),
            check_digest: unavailable,
            required_checks: Vec::new(),
            work_units: vec![WorkUnitWire {
                id: "legacy-plan".to_string(),
                description: "Opaque legacy plan payload".to_string(),
                dependencies: Vec::new(),
                acceptance: Vec::new(),
                read_paths: Vec::new(),
                write_paths: Vec::new(),
                repo_exclusive: false,
                ephemeral_roots: Vec::new(),
                effect_class: r_code_harness_protocol::services::WorkUnitEffectClass::ReadOnly,
                network_ceiling: r_code_harness_protocol::services::NetworkCeiling::Offline,
            }],
        })
        .map_err(PlanStoreError::from)
        .map_err(legacy_sql_error)?;
        self.publish_plan_revision_with_payload(
            &synthetic,
            parent_revision.as_ref(),
            plan_json,
            true,
        )
        .map_err(legacy_sql_error)?;
        Ok(())
    }

    /// Legacy opaque-plan read adapter. A pre-T06 review-backed row is lazily
    /// migrated once, after which reads come exclusively from the new tables.
    pub fn load_plan(&self, task_id: &str) -> Result<Option<(u64, String)>, rusqlite::Error> {
        if let Some(plan) = self
            .load_current_plan_payload(task_id)
            .map_err(legacy_sql_error)?
        {
            return Ok(Some(plan));
        }

        let legacy: Option<(String, Option<String>)> = self
            .connection()
            .query_row(
                "SELECT disposition, notes FROM reviews WHERE task_id = ?1",
                params![format!("plan:{task_id}")],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((disposition, payload)) = legacy else {
            return Ok(None);
        };
        let Some(revision) = disposition
            .strip_prefix("revision:")
            .and_then(|value| value.parse::<u64>().ok())
        else {
            return Ok(None);
        };
        let payload = payload.unwrap_or_default();
        self.save_plan(task_id, revision, &payload)?;
        Ok(Some((revision, payload)))
    }

    fn current_plan_payload(&self, task_id: &str) -> Result<Option<String>, PlanStoreError> {
        self.load_current_plan_payload(task_id)
            .map(|record| record.map(|(_, payload)| payload))
    }

    fn load_current_plan_payload(
        &self,
        task_id: &str,
    ) -> Result<Option<(u64, String)>, PlanStoreError> {
        let connection = self.connection();
        let Some((head, head_number)) = load_head(&connection, task_id)? else {
            return Ok(None);
        };
        let row =
            load_plan_revision_row(&connection, &head)?.ok_or(PlanStoreError::CorruptRecord)?;
        let revision = decode_plan_revision(&row, Some(task_id))?;
        if revision.material().revision != head_number {
            return Err(PlanStoreError::CorruptRecord);
        }
        Ok(Some((head_number, row.payload_json)))
    }
}

pub(crate) fn validate_active_approval(
    connection: &rusqlite::Connection,
    task_id: &str,
    approval_ref: &PlanApprovalRef,
) -> Result<PlanApproval, PlanStoreError> {
    let approval = load_approval(connection, &approval_ref.approval_id)?
        .ok_or_else(|| PlanStoreError::ApprovalNotFound(approval_ref.approval_id.clone()))?;
    if approval.task_id != task_id {
        return Err(PlanStoreError::ApprovalTaskMismatch);
    }
    if approval.plan_revision != approval_ref.plan_revision {
        return Err(PlanStoreError::ApprovalRevisionMismatch);
    }
    if approval.state != PlanApprovalState::Active {
        return Err(PlanStoreError::ApprovalSuperseded);
    }
    let head = load_head(connection, task_id)?;
    if head.as_ref().map(|(revision, _)| revision) != Some(&approval.plan_revision) {
        return Err(PlanStoreError::ApprovalNotCurrent);
    }
    let row = load_plan_revision_row(connection, &approval.plan_revision)?
        .ok_or(PlanStoreError::CorruptRecord)?;
    let revision = decode_plan_revision(&row, Some(task_id))?;
    if head.as_ref().map(|(_, number)| *number) != Some(revision.material().revision) {
        return Err(PlanStoreError::CorruptRecord);
    }
    Ok(approval)
}

fn validate_publish_lineage(
    material: &PlanRevisionMaterial,
    current: Option<&(PlanRevisionRef, u64)>,
    allow_historical_first: bool,
) -> Result<(), PlanStoreError> {
    match current {
        Some((head, current_number)) => {
            let next =
                current_number
                    .checked_add(1)
                    .ok_or(PlanStoreError::NonMonotonicRevision {
                        current: Some(*current_number),
                        provided: material.revision,
                    })?;
            if material.revision != next {
                return Err(PlanStoreError::NonMonotonicRevision {
                    current: Some(*current_number),
                    provided: material.revision,
                });
            }
            if material.parent_revision.as_ref() != Some(head) {
                return Err(PlanStoreError::ParentMismatch);
            }
        }
        None => {
            if material.parent_revision.is_some() {
                return Err(PlanStoreError::ParentMismatch);
            }
            if !allow_historical_first && material.revision != 1 {
                return Err(PlanStoreError::NonMonotonicRevision {
                    current: None,
                    provided: material.revision,
                });
            }
        }
    }
    Ok(())
}

fn load_head(
    connection: &rusqlite::Connection,
    task_id: &str,
) -> Result<Option<(PlanRevisionRef, u64)>, PlanStoreError> {
    let row: Option<(String, u64)> = connection
        .query_row(
            "SELECT plan_revision, revision_number FROM task_plan_heads WHERE task_id = ?1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(revision, number)| {
        PlanRevisionRef::parse(revision)
            .map(|revision| (revision, number))
            .map_err(PlanStoreError::from)
    })
    .transpose()
}

fn load_approval(
    connection: &rusqlite::Connection,
    approval_id: &str,
) -> Result<Option<PlanApproval>, PlanStoreError> {
    let row: Option<(String, String, String, String, String, String, String)> = connection
        .query_row(
            "SELECT task_id, plan_revision, actor_id, session_id, scope, state, approval_json
             FROM plan_approvals WHERE approval_id = ?1",
            params![approval_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((task_id, plan_revision, actor_id, session_id, scope, state, approval_json)) = row
    else {
        return Ok(None);
    };
    let approval: PlanApproval =
        serde_json::from_str(&approval_json).map_err(|_| PlanStoreError::CorruptRecord)?;
    approval
        .validate()
        .map_err(|_| PlanStoreError::CorruptRecord)?;
    if approval.approval_id != approval_id
        || approval.task_id != task_id
        || approval.plan_revision.as_str() != plan_revision
        || approval.actor.actor_id != actor_id
        || approval.actor.session_id != session_id
        || approval.actor.scope != scope
        || approval_state_name(approval.state) != state
        || approval
            .canonical_json()
            .map_err(|_| PlanStoreError::CorruptRecord)?
            != approval_json
    {
        return Err(PlanStoreError::CorruptRecord);
    }
    Ok(Some(approval))
}

fn approval_state_name(state: PlanApprovalState) -> &'static str {
    match state {
        PlanApprovalState::Active => "active",
        PlanApprovalState::Superseded => "superseded",
    }
}

fn supersede_active_approval(
    transaction: &Transaction<'_>,
    task_id: &str,
) -> Result<(), PlanStoreError> {
    let approval_id: Option<String> = transaction
        .query_row(
            "SELECT approval_id FROM plan_approvals
             WHERE task_id = ?1 AND state = 'active'",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(approval_id) = approval_id else {
        return Ok(());
    };
    let mut approval =
        load_approval(transaction, &approval_id)?.ok_or(PlanStoreError::CorruptRecord)?;
    approval.state = PlanApprovalState::Superseded;
    let changed = transaction.execute(
        "UPDATE plan_approvals
         SET state = 'superseded', approval_json = ?1, superseded_at_ms = ?2
         WHERE approval_id = ?3 AND state = 'active'",
        params![approval.canonical_json()?, now_ms(), approval_id],
    )?;
    if changed != 1 {
        return Err(PlanStoreError::CorruptRecord);
    }
    Ok(())
}

struct StoredPlanRevision {
    plan_revision: String,
    task_id: String,
    revision_number: u64,
    parent_revision: Option<String>,
    current_base_hash: String,
    content_sha256: String,
    material_json: String,
    payload_json: String,
}

fn load_plan_revision_row(
    connection: &rusqlite::Connection,
    revision: &PlanRevisionRef,
) -> Result<Option<StoredPlanRevision>, PlanStoreError> {
    connection
        .query_row(
            "SELECT plan_revision, task_id, revision_number, parent_revision,
                    current_base_hash, content_sha256, material_json, payload_json
             FROM plan_revisions WHERE plan_revision = ?1",
            params![revision.as_str()],
            |row| {
                Ok(StoredPlanRevision {
                    plan_revision: row.get(0)?,
                    task_id: row.get(1)?,
                    revision_number: row.get(2)?,
                    parent_revision: row.get(3)?,
                    current_base_hash: row.get(4)?,
                    content_sha256: row.get(5)?,
                    material_json: row.get(6)?,
                    payload_json: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(PlanStoreError::from)
}

fn decode_plan_revision(
    row: &StoredPlanRevision,
    expected_task: Option<&str>,
) -> Result<PlanRevision, PlanStoreError> {
    let revision: PlanRevision =
        serde_json::from_str(&row.material_json).map_err(|_| PlanStoreError::CorruptRecord)?;
    revision
        .validate_identity()
        .map_err(|_| PlanStoreError::CorruptRecord)?;
    let material = revision.material();
    if revision.reference().as_str() != row.plan_revision
        || material.task_id != row.task_id
        || material.revision != row.revision_number
        || material
            .parent_revision
            .as_ref()
            .map(PlanRevisionRef::as_str)
            != row.parent_revision.as_deref()
        || material.current_base_hash != row.current_base_hash
        || revision.reference().as_str().strip_prefix("sha256:")
            != Some(row.content_sha256.as_str())
        || expected_task.is_some_and(|task_id| revision.material().task_id != task_id)
        || revision
            .canonical_json()
            .map_err(|_| PlanStoreError::CorruptRecord)?
            != row.material_json
    {
        return Err(PlanStoreError::CorruptRecord);
    }
    Ok(revision)
}

fn digest_string(value: &str) -> String {
    let digest = r_code_harness_protocol::canonical_input_hash(&serde_json::Value::String(
        value.to_string(),
    ));
    format!("sha256:{digest}")
}

fn legacy_sql_error(error: PlanStoreError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

// P19A — immutable exact-plan effect approvals ---------------------------------

/// State of one effect approval; superseding is the only retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectApprovalState {
    Active,
    Superseded,
}

impl EffectApprovalState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
        }
    }
}

/// One persisted exact effect approval (P19A.3): task/plan-revision/
/// work-unit identity plus the approved class/network ceiling and the
/// authenticated actor/session/scope. Mutable settings never enter this
/// record; a change on ANY identity column makes it non-matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectApprovalRecord {
    pub approval_id: String,
    pub task_id: String,
    pub plan_revision: String,
    pub work_unit_id: String,
    pub effect_class: String,
    pub network: String,
    pub actor_id: String,
    pub session_id: String,
    pub scope: String,
    pub payload_hash: String,
    pub state: EffectApprovalState,
    pub created_at_ms: i64,
    pub superseded_at_ms: Option<i64>,
}

impl V1Store {
    fn validate_effect_approval(record: &EffectApprovalRecord) -> Result<(), PlanStoreError> {
        let valid_class = matches!(
            record.effect_class.as_str(),
            "read-only" | "workspace-mutation" | "dependency-preparation"
        );
        let valid_network = matches!(
            record.network.as_str(),
            "offline" | "public-internet-client" | "host-network"
        );
        if record.approval_id.trim().is_empty()
            || record.task_id.trim().is_empty()
            || record.plan_revision.trim().is_empty()
            || record.work_unit_id.trim().is_empty()
            || record.actor_id.trim().is_empty()
            || record.session_id.trim().is_empty()
            || record.scope != "effect.approve"
            || record.payload_hash.trim().is_empty()
            || record.state != EffectApprovalState::Active
            || record.superseded_at_ms.is_some()
            || !valid_class
            || !valid_network
        {
            return Err(PlanStoreError::EffectApprovalInvalid);
        }
        Ok(())
    }

    /// Persist one immutable effect approval (P19A.3). Exactly one ACTIVE
    /// approval may exist per (task, work unit): a different active
    /// approval is a conflict, never a silent overwrite. Replaying one
    /// approval id is idempotent only while EVERY persisted column still
    /// matches, so a record's actor attribution can never drift.
    pub fn save_effect_approval(&self, record: EffectApprovalRecord) -> Result<(), PlanStoreError> {
        Self::validate_effect_approval(&record)?;
        let mut connection = self.connection();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(existing) = load_effect_approval(&transaction, &record.approval_id)? {
            if existing == record {
                return Ok(());
            }
            return Err(PlanStoreError::EffectApprovalIdConflict);
        }
        let active: Option<String> = transaction
            .query_row(
                "SELECT approval_id FROM work_unit_effect_approvals
                 WHERE task_id = ?1 AND work_unit_id = ?2 AND state = 'active'",
                rusqlite::params![record.task_id, record.work_unit_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(PlanStoreError::from)?;
        if active.is_some() {
            return Err(PlanStoreError::EffectActiveApprovalExists);
        }
        transaction.execute(
            "INSERT INTO work_unit_effect_approvals(
                 approval_id, task_id, plan_revision, work_unit_id,
                 effect_class, network, actor_id, session_id, scope,
                 payload_hash, state, created_at_ms, superseded_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL)",
            rusqlite::params![
                record.approval_id,
                record.task_id,
                record.plan_revision,
                record.work_unit_id,
                record.effect_class,
                record.network,
                record.actor_id,
                record.session_id,
                record.scope,
                record.payload_hash,
                record.state.as_str(),
                record.created_at_ms
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Find the ACTIVE approval whose identity columns EXACTLY match the
    /// request (P19A acceptance: stale, foreign, weaker or otherwise
    /// different approvals never match). The class comparison is exact:
    /// an approval recorded for read-only never authorizes a mutation.
    pub fn find_active_effect_approval(
        &self,
        task_id: &str,
        plan_revision: &str,
        work_unit_id: &str,
        effect_class: &str,
        network: &str,
        payload_hash: &str,
    ) -> Result<Option<EffectApprovalRecord>, PlanStoreError> {
        let connection = self.connection();
        connection
            .query_row(
                "SELECT approval_id, task_id, plan_revision, work_unit_id,
                        effect_class, network, actor_id, session_id, scope,
                        payload_hash, state, created_at_ms, superseded_at_ms
                 FROM work_unit_effect_approvals
                 WHERE task_id = ?1 AND plan_revision = ?2 AND work_unit_id = ?3
                   AND effect_class = ?4 AND network = ?5 AND payload_hash = ?6
                   AND state = 'active'",
                rusqlite::params![
                    task_id,
                    plan_revision,
                    work_unit_id,
                    effect_class,
                    network,
                    payload_hash
                ],
                parse_effect_approval_row,
            )
            .optional()
            .map_err(PlanStoreError::from)
    }

    /// List the approvals for one task (audit view), oldest first.
    pub fn effect_approvals_for_task(
        &self,
        task_id: &str,
    ) -> Result<Vec<EffectApprovalRecord>, PlanStoreError> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT approval_id, task_id, plan_revision, work_unit_id,
                        effect_class, network, actor_id, session_id, scope,
                        payload_hash, state, created_at_ms, superseded_at_ms
                 FROM work_unit_effect_approvals WHERE task_id = ?1
                 ORDER BY created_at_ms, approval_id",
            )
            .map_err(|error| PlanStoreError::Sqlite(error.to_string()))?;
        let rows = statement
            .query_map(rusqlite::params![task_id], parse_effect_approval_row)
            .map_err(|error| PlanStoreError::Sqlite(error.to_string()))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|error| PlanStoreError::Sqlite(error.to_string()))?);
        }
        Ok(records)
    }

    /// Supersede the active approval for one work unit (revocation is
    /// future-runs-only: frozen RunSnapshots keep what they froze).
    pub fn supersede_effect_approval(
        &self,
        task_id: &str,
        work_unit_id: &str,
        now_ms: i64,
    ) -> Result<bool, PlanStoreError> {
        let connection = self.connection();
        let changed = connection
            .execute(
                "UPDATE work_unit_effect_approvals
                 SET state = 'superseded', superseded_at_ms = ?3
                 WHERE task_id = ?1 AND work_unit_id = ?2 AND state = 'active'",
                rusqlite::params![task_id, work_unit_id, now_ms],
            )
            .map_err(|error| PlanStoreError::Sqlite(error.to_string()))?;
        Ok(changed > 0)
    }
}

fn load_effect_approval(
    connection: &rusqlite::Connection,
    approval_id: &str,
) -> Result<Option<EffectApprovalRecord>, PlanStoreError> {
    connection
        .query_row(
            "SELECT approval_id, task_id, plan_revision, work_unit_id,
                    effect_class, network, actor_id, session_id, scope,
                    payload_hash, state, created_at_ms, superseded_at_ms
             FROM work_unit_effect_approvals WHERE approval_id = ?1",
            rusqlite::params![approval_id],
            parse_effect_approval_row,
        )
        .optional()
        .map_err(PlanStoreError::from)
}

fn parse_effect_approval_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EffectApprovalRecord> {
    Ok(EffectApprovalRecord {
        approval_id: row.get(0)?,
        task_id: row.get(1)?,
        plan_revision: row.get(2)?,
        work_unit_id: row.get(3)?,
        effect_class: row.get(4)?,
        network: row.get(5)?,
        actor_id: row.get(6)?,
        session_id: row.get(7)?,
        scope: row.get(8)?,
        payload_hash: row.get(9)?,
        state: match row.get::<_, String>(10)?.as_str() {
            "active" => EffectApprovalState::Active,
            _ => EffectApprovalState::Superseded,
        },
        created_at_ms: row.get(11)?,
        superseded_at_ms: row.get(12)?,
    })
}
