//! Durable Plan aggregate persistence and its human-readable Markdown projection.
//!
//! SQLite is authoritative. Mutations use short optimistic transactions, release the database
//! connection, and only then update the stable projection under a process-wide per-Plan lock.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use chrono::{DateTime, Utc};
use r_code_core::error::ProductError;
use r_code_core::plan::{
    AnswerPlanQuestionsInput, ApprovePlanInput, CancelPlanInput, CreatePlanInput, Plan,
    PlanContinuationState, PlanGoal, PlanImplementationDispatchState, PlanItem, PlanItemDraft,
    PlanItemState, PlanQuestion, PlanQuestionAnswer, PlanQuestionAnswerInput, PlanQuestionOption,
    PlanQuestionSet, PlanQuestionSetState, PlanState, PlanView, PublishPlanInput,
    RequestPlanQuestionsInput, UpdatePlanItemInput,
};
use rusqlite::{params, Connection, OptionalExtension};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::Database;

const MAX_PLAN_ITEMS: usize = 100;
const MAX_TEXT_ANSWER_CHARS: usize = 4_000;
pub const PLAN_CONTINUATION_INTERRUPTED: &str =
    "PLAN_CONTINUATION_INTERRUPTED: application restarted before dispatch acknowledgement";
pub const PLAN_IMPLEMENTATION_DISPATCH_INTERRUPTED: &str =
    "PLAN_IMPLEMENTATION_DISPATCH_INTERRUPTED: application restarted before durable queue handoff";

type PlanMutex = Mutex<()>;

static PLAN_LOCKS: OnceLock<Mutex<HashMap<String, Weak<PlanMutex>>>> = OnceLock::new();

fn db_err(error: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(error.to_string())
}

fn invalid(message: impl Into<String>) -> ProductError {
    ProductError::StateMachineError(message.into())
}

fn parse_ts(value: &str) -> Result<DateTime<Utc>, ProductError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| ProductError::DatabaseError(format!("timestamp parse error: {error}")))
}

fn sql_revision(revision: u64) -> Result<i64, ProductError> {
    i64::try_from(revision).map_err(|_| invalid("Plan revision exceeds SQLite INTEGER range"))
}

fn plan_lock(plan_id: &str) -> Result<Arc<PlanMutex>, ProductError> {
    let registry = PLAN_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = registry
        .lock()
        .map_err(|_| ProductError::Other("Plan lock registry is poisoned".to_string()))?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(plan_id).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(plan_id.to_string(), Arc::downgrade(&lock));
    Ok(lock)
}

/// Store-backed Plan workflow. Clones share the database and the process-wide Plan coordinator.
#[derive(Clone)]
pub struct PlanStore {
    db: Arc<Database>,
    projection_root: Arc<PathBuf>,
}

impl PlanStore {
    pub fn new(db: Arc<Database>, projection_root: impl Into<PathBuf>) -> Self {
        Self {
            db,
            projection_root: Arc::new(projection_root.into()),
        }
    }

    pub fn projection_root(&self) -> &Path {
        self.projection_root.as_path()
    }

    fn canonical_projection_path(&self, plan_id: &str) -> Result<PathBuf, ProductError> {
        let parsed = Uuid::parse_str(plan_id)
            .map_err(|_| invalid(format!("Plan id is not a valid UUID: {plan_id}")))?;
        let canonical_id = parsed.to_string();
        if canonical_id != plan_id {
            return Err(invalid(format!(
                "Plan id is not in canonical UUID form: {plan_id}"
            )));
        }
        Ok(self.projection_root.join(canonical_id).join("plan.md"))
    }

    /// Retry deletion of canonical UUID projection directories whose authoritative Plan row no
    /// longer exists. Unknown filenames and every path outside `projection_root` are ignored.
    pub fn prune_orphan_projection_directories(
        &self,
    ) -> Result<crate::lifecycle_purge::AppDataPruneReport, ProductError> {
        crate::lifecycle_purge::prune_orphan_plan_directories(
            self.db.as_ref(),
            self.projection_root(),
        )
    }

    /// Recover a resolved continuation that had not reached a durable dispatch acknowledgement
    /// before the authoritative desktop process exited.
    ///
    /// We cannot prove whether an external provider observed a message across that crash window,
    /// so recovery is explicit and retryable instead of silently redispatching or leaving the
    /// question set permanently stuck in `dispatching`.
    pub fn recover_interrupted_continuations(&self) -> Result<u64, ProductError> {
        let conn = self.db.conn()?;
        let changed = conn
            .execute(
                "UPDATE plan_question_sets SET continuation_state = 'failed', \
                 continuation_error = ?1 WHERE state IN ('answered', 'skipped') \
                 AND continuation_state IN ('pending', 'dispatching')",
                params![PLAN_CONTINUATION_INTERRUPTED],
            )
            .map_err(db_err)?;
        u64::try_from(changed)
            .map_err(|_| ProductError::DatabaseError("recovery count overflow".to_string()))
    }

    /// Creates a new current Plan. A task may have only one non-terminal Plan.
    pub fn create_plan(&self, input: &CreatePlanInput) -> Result<PlanView, ProductError> {
        if input.task_id.trim().is_empty() {
            return Err(invalid("task_id cannot be blank"));
        }
        let plan_id = Uuid::new_v4().to_string();
        let lock = plan_lock(&plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;
        let projection_path = self
            .canonical_projection_path(&plan_id)?
            .to_string_lossy()
            .into_owned();
        let now = Utc::now().to_rfc3339();

        {
            let mut conn = self.db.conn()?;
            let tx = conn.transaction().map_err(db_err)?;
            let task_exists = tx
                .query_row(
                    "SELECT 1 FROM tasks WHERE id = ?1",
                    params![input.task_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(db_err)?
                .is_some();
            if !task_exists {
                return Err(invalid(format!("task does not exist: {}", input.task_id)));
            }
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM plans WHERE task_id = ?1 AND state NOT IN ('completed', 'cancelled') LIMIT 1",
                    params![input.task_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_err)?;
            if let Some(existing) = existing {
                return Err(invalid(format!(
                    "task already has an active Plan: {existing}"
                )));
            }
            tx.execute(
                "INSERT INTO plans (id, task_id, revision, state, projection_path, created_at, updated_at) \
                 VALUES (?1, ?2, 1, 'draft', ?3, ?4, ?4)",
                params![plan_id, input.task_id, projection_path, now],
            )
            .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
        }

        self.sync_projection_locked(&plan_id)?;
        self.require_view(&input.task_id, &plan_id)
    }

    pub fn current_for_task(&self, task_id: &str) -> Result<Option<PlanView>, ProductError> {
        let conn = self.db.conn()?;
        let plan_id: Option<String> = conn
            .query_row(
                "SELECT id FROM plans WHERE task_id = ?1 \
                 ORDER BY CASE WHEN state NOT IN ('completed', 'cancelled') THEN 0 ELSE 1 END, \
                 updated_at DESC LIMIT 1",
                params![task_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        plan_id
            .as_deref()
            .map(|plan_id| self.load_view(&conn, task_id, plan_id))
            .transpose()
    }

    pub fn get_plan(&self, task_id: &str, plan_id: &str) -> Result<Option<PlanView>, ProductError> {
        let conn = self.db.conn()?;
        if !plan_belongs_to_task(&conn, task_id, plan_id)? {
            return Ok(None);
        }
        self.load_view(&conn, task_id, plan_id).map(Some)
    }

    /// Publishes a complete replacement of the draft feature list at a new revision.
    pub fn publish_plan(
        &self,
        task_id: &str,
        input: &PublishPlanInput,
    ) -> Result<PlanView, ProductError> {
        validate_item_drafts(&input.items)?;
        let lock = plan_lock(&input.plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;
        let next_revision = input
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| invalid("Plan revision overflow"))?;
        let now = Utc::now().to_rfc3339();

        {
            let mut conn = self.db.conn()?;
            let tx = conn.transaction().map_err(db_err)?;
            let plan = require_owned_plan(&tx, task_id, &input.plan_id)?;
            if plan.revision != input.expected_revision {
                return Err(stale_revision(input.expected_revision, plan.revision));
            }
            if !matches!(plan.state, PlanState::Draft | PlanState::Ready) {
                return Err(invalid(format!(
                    "cannot publish Plan while it is {}",
                    plan.state
                )));
            }
            let changed = tx
                .execute(
                    "UPDATE plans SET revision = ?1, state = 'ready', updated_at = ?2, \
                     projection_error = NULL WHERE id = ?3 AND task_id = ?4 AND revision = ?5 \
                     AND state IN ('draft', 'ready')",
                    params![
                        sql_revision(next_revision)?,
                        now,
                        input.plan_id,
                        task_id,
                        sql_revision(input.expected_revision)?,
                    ],
                )
                .map_err(db_err)?;
            if changed != 1 {
                return Err(invalid("Plan changed while it was being published"));
            }

            // Draft items have no write events yet. Replacing their rows preserves the stable IDs
            // supplied by the publication while avoiding ambiguous cross-revision dependencies.
            tx.execute(
                "DELETE FROM plan_items WHERE plan_id = ?1 AND state = 'proposed'",
                params![input.plan_id],
            )
            .map_err(db_err)?;
            for (ordinal, item) in input.items.iter().enumerate() {
                let section_path = serde_json::to_string(
                    &item
                        .section_path
                        .iter()
                        .map(|segment| segment.trim())
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| invalid(format!("serialize Plan section path: {error}")))?;
                tx.execute(
                    "INSERT INTO plan_items \
                     (id, plan_id, revision, ordinal, title, description, section_path_json, state, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'proposed', ?8, ?8)",
                    params![
                        item.id,
                        input.plan_id,
                        sql_revision(next_revision)?,
                        ordinal as i64,
                        item.title.trim(),
                        item.description.trim(),
                        section_path,
                        now,
                    ],
                )
                .map_err(db_err)?;
            }
            for item in &input.items {
                for dependency in &item.depends_on {
                    tx.execute(
                        "INSERT INTO plan_item_dependencies \
                         (plan_id, revision, item_id, depends_on_item_id) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            input.plan_id,
                            sql_revision(next_revision)?,
                            item.id,
                            dependency,
                        ],
                    )
                    .map_err(db_err)?;
                }
            }
            tx.commit().map_err(db_err)?;
        }

        self.sync_projection_locked(&input.plan_id)?;
        self.require_view(task_id, &input.plan_id)
    }

    /// Persists one pending 1-3 question set and moves the Plan into awaiting-input state.
    pub fn request_questions(
        &self,
        task_id: &str,
        input: &RequestPlanQuestionsInput,
    ) -> Result<PlanView, ProductError> {
        validate_question_drafts(input)?;
        let lock = plan_lock(&input.plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;
        let question_set_id = Uuid::new_v4().to_string();
        let next_revision = input
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| invalid("Plan revision overflow"))?;
        let now = Utc::now().to_rfc3339();

        {
            let mut conn = self.db.conn()?;
            let tx = conn.transaction().map_err(db_err)?;
            let plan = require_owned_plan(&tx, task_id, &input.plan_id)?;
            if plan.revision != input.expected_revision {
                return Err(stale_revision(input.expected_revision, plan.revision));
            }
            if !matches!(plan.state, PlanState::Draft | PlanState::Ready) {
                return Err(invalid(format!(
                    "cannot request input while Plan is {}",
                    plan.state
                )));
            }
            let pending: Option<String> = tx
                .query_row(
                    "SELECT id FROM plan_question_sets WHERE plan_id = ?1 AND state = 'pending' LIMIT 1",
                    params![input.plan_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_err)?;
            if let Some(pending) = pending {
                return Err(invalid(format!(
                    "Plan already awaits question set {pending}"
                )));
            }
            let changed = tx
                .execute(
                    "UPDATE plans SET revision = ?1, state = 'awaiting_input', updated_at = ?2, \
                     projection_error = NULL WHERE id = ?3 AND task_id = ?4 AND revision = ?5 \
                     AND state IN ('draft', 'ready')",
                    params![
                        sql_revision(next_revision)?,
                        now,
                        input.plan_id,
                        task_id,
                        sql_revision(input.expected_revision)?,
                    ],
                )
                .map_err(db_err)?;
            if changed != 1 {
                return Err(invalid("Plan changed while questions were being created"));
            }
            tx.execute(
                "INSERT INTO plan_question_sets \
                 (id, plan_id, revision, state, continuation_state, created_at) \
                 VALUES (?1, ?2, ?3, 'pending', 'not_requested', ?4)",
                params![
                    question_set_id,
                    input.plan_id,
                    sql_revision(next_revision)?,
                    now,
                ],
            )
            .map_err(db_err)?;
            for (ordinal, question) in input.questions.iter().enumerate() {
                tx.execute(
                    "INSERT INTO plan_questions \
                     (id, question_set_id, ordinal, header, question) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        question.id,
                        question_set_id,
                        ordinal as i64,
                        question.header.trim(),
                        question.question.trim(),
                    ],
                )
                .map_err(db_err)?;
                for (option_ordinal, option) in question.options.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO plan_question_options \
                         (id, question_id, question_set_id, ordinal, label, description) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            option.id,
                            question.id,
                            question_set_id,
                            option_ordinal as i64,
                            option.label.trim(),
                            option.description.trim(),
                        ],
                    )
                    .map_err(db_err)?;
                }
            }
            tx.commit().map_err(db_err)?;
        }

        self.sync_projection_locked(&input.plan_id)?;
        self.require_view(task_id, &input.plan_id)
    }

    /// Resolves a whole question set exactly once. Repeating the same idempotency key and payload
    /// returns the current aggregate, while key or payload reuse is rejected.
    pub fn answer_questions(
        &self,
        task_id: &str,
        input: &AnswerPlanQuestionsInput,
    ) -> Result<PlanView, ProductError> {
        validate_answer_shape(input)?;
        let plan_id = self.plan_id_for_question_set(task_id, &input.question_set_id)?;
        let lock = plan_lock(&plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;

        let mut changed_plan = false;
        {
            let mut conn = self.db.conn()?;
            let tx = conn.transaction().map_err(db_err)?;
            let set = require_question_set(&tx, task_id, &input.question_set_id)?;
            if set.state != PlanQuestionSetState::Pending {
                if set.answer_idempotency_key.as_deref() == Some(input.idempotency_key.as_str())
                    && answer_payload_matches(&set, input)
                {
                    tx.commit().map_err(db_err)?;
                } else {
                    return Err(invalid(
                        "question set was already resolved with another payload",
                    ));
                }
            } else {
                let plan = require_owned_plan(&tx, task_id, &set.plan_id)?;
                if plan.revision != input.expected_revision
                    || set.revision != input.expected_revision
                {
                    return Err(stale_revision(input.expected_revision, plan.revision));
                }
                if plan.state != PlanState::AwaitingInput {
                    return Err(invalid(format!(
                        "cannot answer questions while Plan is {}",
                        plan.state
                    )));
                }
                validate_answers_against_set(&set, input)?;
                let now = Utc::now().to_rfc3339();
                let next_revision = plan
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("Plan revision overflow"))?;
                if !input.skip_all {
                    for answer in &input.answers {
                        match answer {
                            PlanQuestionAnswerInput::Option {
                                question_id,
                                option_id,
                            } => {
                                tx.execute(
                                    "UPDATE plan_questions SET answer_kind = 'option', answer_value = ?1, \
                                     answered_at = ?2 WHERE id = ?3 AND question_set_id = ?4",
                                    params![option_id, now, question_id, input.question_set_id],
                                )
                                .map_err(db_err)?;
                            }
                            PlanQuestionAnswerInput::Text { question_id, text } => {
                                tx.execute(
                                    "UPDATE plan_questions SET answer_kind = 'free_form', answer_value = ?1, \
                                     answered_at = ?2 WHERE id = ?3 AND question_set_id = ?4",
                                    params![text.trim(), now, question_id, input.question_set_id],
                                )
                                .map_err(db_err)?;
                            }
                        }
                    }
                }
                let resolved_state = if input.skip_all {
                    "skipped"
                } else {
                    "answered"
                };
                tx.execute(
                    "UPDATE plan_question_sets SET state = ?1, answer_idempotency_key = ?2, \
                     continuation_state = 'pending', continuation_error = NULL, resolved_at = ?3 \
                     WHERE id = ?4 AND state = 'pending'",
                    params![
                        resolved_state,
                        input.idempotency_key,
                        now,
                        input.question_set_id
                    ],
                )
                .map_err(db_err)?;
                let updated = tx
                    .execute(
                        "UPDATE plans SET revision = ?1, state = 'draft', updated_at = ?2, \
                         projection_error = NULL WHERE id = ?3 AND task_id = ?4 AND revision = ?5 \
                         AND state = 'awaiting_input'",
                        params![
                            sql_revision(next_revision)?,
                            now,
                            set.plan_id,
                            task_id,
                            sql_revision(input.expected_revision)?,
                        ],
                    )
                    .map_err(db_err)?;
                if updated != 1 {
                    return Err(invalid("Plan changed while the question set was answered"));
                }
                tx.commit().map_err(db_err)?;
                changed_plan = true;
            }
        }

        if changed_plan {
            self.sync_projection_locked(&plan_id)?;
        }
        self.require_view(task_id, &plan_id)
    }

    /// CAS-pins a ready revision, validates its dependency DAG, and activates the first feature.
    pub fn approve_plan(
        &self,
        task_id: &str,
        input: &ApprovePlanInput,
    ) -> Result<PlanView, ProductError> {
        self.approve_plan_with_outcome(task_id, input)
            .map(|(view, _newly_approved)| view)
    }

    /// Approve a Plan and report whether this caller won the approval CAS. Retry callers receive
    /// the same aggregate with `false`, allowing the host to avoid dispatching implementation
    /// twice when the user double-clicks or two windows race.
    pub fn approve_plan_with_outcome(
        &self,
        task_id: &str,
        input: &ApprovePlanInput,
    ) -> Result<(PlanView, bool), ProductError> {
        let lock = plan_lock(&input.plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;
        let mut changed_plan = false;

        {
            let mut conn = self.db.conn()?;
            let tx = conn.transaction().map_err(db_err)?;
            let plan = require_owned_plan(&tx, task_id, &input.plan_id)?;
            if plan.approved_revision == Some(input.expected_revision)
                && matches!(
                    plan.state,
                    PlanState::Approved | PlanState::Executing | PlanState::Completed
                )
            {
                tx.commit().map_err(db_err)?;
            } else {
                if plan.revision != input.expected_revision {
                    return Err(stale_revision(input.expected_revision, plan.revision));
                }
                if plan.state != PlanState::Ready {
                    return Err(invalid(format!(
                        "cannot approve Plan while it is {}",
                        plan.state
                    )));
                }
                let items = load_items(&tx, &input.plan_id, input.expected_revision)?;
                if items.is_empty() {
                    return Err(invalid("cannot approve a Plan without feature items"));
                }
                validate_dependency_dag(&items)?;
                let active_id = items
                    .iter()
                    .find(|item| item.depends_on.is_empty())
                    .map(|item| item.id.clone())
                    .ok_or_else(|| invalid("Plan has no dependency-ready feature"))?;
                let now = Utc::now().to_rfc3339();
                let next_revision = plan
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("Plan revision overflow"))?;
                tx.execute(
                    "UPDATE plan_items SET state = 'pending', updated_at = ?1 \
                     WHERE plan_id = ?2 AND revision = ?3 AND state = 'proposed'",
                    params![now, input.plan_id, sql_revision(input.expected_revision)?],
                )
                .map_err(db_err)?;
                tx.execute(
                    "UPDATE plan_items SET state = 'in_progress', started_at = ?1, updated_at = ?1 \
                     WHERE id = ?2 AND plan_id = ?3 AND revision = ?4 AND state = 'pending'",
                    params![
                        now,
                        active_id,
                        input.plan_id,
                        sql_revision(input.expected_revision)?,
                    ],
                )
                .map_err(db_err)?;
                let updated = tx
                    .execute(
                        "UPDATE plans SET revision = ?1, approved_revision = ?2, state = 'executing', \
                         approved_at = ?3, updated_at = ?3, projection_error = NULL, \
                         implementation_dispatch_state = 'pending', \
                         implementation_dispatch_error = NULL, \
                         implementation_queue_message_id = NULL, \
                         implementation_dispatched_at = NULL \
                         WHERE id = ?4 AND task_id = ?5 AND revision = ?2 AND state = 'ready' \
                         AND approved_revision IS NULL",
                        params![
                            sql_revision(next_revision)?,
                            sql_revision(input.expected_revision)?,
                            now,
                            input.plan_id,
                            task_id,
                        ],
                    )
                    .map_err(db_err)?;
                if updated != 1 {
                    return Err(invalid("Plan changed while it was being approved"));
                }
                tx.commit().map_err(db_err)?;
                changed_plan = true;
            }
        }

        if changed_plan {
            self.sync_projection_locked(&input.plan_id)?;
        }
        self.require_view(task_id, &input.plan_id)
            .map(|view| (view, changed_plan))
    }

    /// Cancels a non-terminal Plan without reverting workspace files or deleting review history.
    /// Repeating cancellation is idempotent and returns the same terminal aggregate.
    pub fn cancel_plan(
        &self,
        task_id: &str,
        input: &CancelPlanInput,
    ) -> Result<PlanView, ProductError> {
        let lock = plan_lock(&input.plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;
        let mut changed_plan = false;

        {
            let mut conn = self.db.conn()?;
            let tx = conn.transaction().map_err(db_err)?;
            let plan = require_owned_plan(&tx, task_id, &input.plan_id)?;
            if plan.state == PlanState::Cancelled {
                let now = Utc::now().to_rfc3339();
                let task_updated = tx.execute(
                    "UPDATE tasks SET mode = CASE WHEN workspace_path IS NULL THEN 'ask' ELSE 'edit' END, \
                     updated_at = ?1 WHERE id = ?2",
                    params![now, task_id],
                )
                .map_err(db_err)?;
                if task_updated != 1 {
                    return Err(invalid(
                        "task disappeared while cancelled Plan was reconciled",
                    ));
                }
                tx.commit().map_err(db_err)?;
            } else {
                let rejection_in_progress: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM plan_reject_operations \
                         WHERE plan_id = ?1 AND state IN ('prepared', 'applying', 'rolling_back'))",
                        params![input.plan_id],
                        |row| row.get(0),
                    )
                    .map_err(db_err)?;
                if rejection_in_progress {
                    return Err(invalid("审核回滚处理中，请稍后"));
                }
                if plan.state == PlanState::Completed {
                    return Err(invalid("a completed Plan cannot be cancelled"));
                }
                if plan.revision != input.expected_revision {
                    return Err(stale_revision(input.expected_revision, plan.revision));
                }
                let next_revision = plan
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("Plan revision overflow"))?;
                let now = Utc::now().to_rfc3339();

                // `skipped` is the existing terminal question-set state. The host-specific
                // idempotency key satisfies the durable constraint while `not_requested`
                // guarantees cancellation never schedules a continuation.
                tx.execute(
                    "UPDATE plan_question_sets SET state = 'skipped', \
                     answer_idempotency_key = 'plan-cancel:' || id, \
                     continuation_state = 'not_requested', continuation_error = NULL, \
                     resolved_at = ?1, dispatched_at = NULL \
                     WHERE plan_id = ?2 AND state = 'pending'",
                    params![now, input.plan_id],
                )
                .map_err(db_err)?;
                tx.execute(
                    "UPDATE plan_question_sets SET continuation_state = 'not_requested', \
                     continuation_error = NULL, dispatched_at = NULL \
                     WHERE plan_id = ?1 AND continuation_state IN ('pending', 'dispatching', 'failed')",
                    params![input.plan_id],
                )
                .map_err(db_err)?;
                tx.execute(
                    "UPDATE plan_items SET state = 'cancelled', \
                     completed_at = COALESCE(completed_at, ?1), updated_at = ?1 \
                     WHERE plan_id = ?2 AND state IN ('proposed', 'pending', 'in_progress', 'blocked')",
                    params![now, input.plan_id],
                )
                .map_err(db_err)?;
                if let Some(queue_id) = plan.implementation_queue_message_id.as_deref() {
                    tx.execute(
                        "UPDATE queued_messages SET state = 'cancelled', updated_at = ?1 \
                         WHERE id = ?2 AND state IN ('queued', 'dispatching', 'failed')",
                        params![now, queue_id],
                    )
                    .map_err(db_err)?;
                }
                let updated = tx
                    .execute(
                        "UPDATE plans SET revision = ?1, state = 'cancelled', updated_at = ?2, \
                         projection_error = NULL, implementation_dispatch_state = 'not_requested', \
                         implementation_dispatch_error = NULL, implementation_dispatched_at = NULL \
                         WHERE id = ?3 AND task_id = ?4 \
                         AND revision = ?5 AND state NOT IN ('completed', 'cancelled')",
                        params![
                            sql_revision(next_revision)?,
                            now,
                            input.plan_id,
                            task_id,
                            sql_revision(input.expected_revision)?,
                        ],
                    )
                    .map_err(db_err)?;
                if updated != 1 {
                    return Err(invalid("Plan changed while it was being cancelled"));
                }
                let task_updated = tx
                    .execute(
                        "UPDATE tasks SET mode = CASE WHEN workspace_path IS NULL THEN 'ask' ELSE 'edit' END, \
                         updated_at = ?1 WHERE id = ?2",
                        params![now, task_id],
                    )
                    .map_err(db_err)?;
                if task_updated != 1 {
                    return Err(invalid("task disappeared while Plan was cancelled"));
                }
                tx.commit().map_err(db_err)?;
                changed_plan = true;
            }
        }

        if changed_plan {
            self.sync_projection_locked(&input.plan_id)?;
        }
        self.require_view(task_id, &input.plan_id)
    }

    /// Claims a pending or failed implementation handoff. `None` means it is already claimed or
    /// durably queued. A desktop restart converts an orphaned claim to `failed` for explicit retry.
    pub fn claim_implementation_dispatch(
        &self,
        task_id: &str,
        plan_id: &str,
    ) -> Result<Option<PlanView>, ProductError> {
        let lock = plan_lock(plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let plan = require_owned_plan(&tx, task_id, plan_id)?;
        if plan.state != PlanState::Executing {
            return Err(invalid(format!(
                "cannot dispatch implementation while Plan is {}",
                plan.state
            )));
        }
        match plan.implementation_dispatch_state {
            PlanImplementationDispatchState::Pending | PlanImplementationDispatchState::Failed => {
                let changed = tx
                    .execute(
                        "UPDATE plans SET implementation_dispatch_state = 'dispatching', \
                         implementation_dispatch_error = NULL, updated_at = ?1 \
                         WHERE id = ?2 AND task_id = ?3 \
                         AND implementation_dispatch_state IN ('pending', 'failed')",
                        params![Utc::now().to_rfc3339(), plan_id, task_id],
                    )
                    .map_err(db_err)?;
                if changed != 1 {
                    return Err(invalid(
                        "implementation dispatch changed while it was claimed",
                    ));
                }
                tx.commit().map_err(db_err)?;
                drop(conn);
                self.require_view(task_id, plan_id).map(Some)
            }
            PlanImplementationDispatchState::Dispatching
            | PlanImplementationDispatchState::Dispatched => {
                tx.commit().map_err(db_err)?;
                Ok(None)
            }
            PlanImplementationDispatchState::NotRequested => {
                Err(invalid("Plan implementation dispatch was not requested"))
            }
        }
    }

    /// Atomically switches the task to Auto, inserts exactly one deterministic queue row, and
    /// acknowledges the durable implementation handoff.
    pub fn stage_implementation_dispatch(
        &self,
        task_id: &str,
        plan_id: &str,
        branch_id: &str,
        message: &str,
    ) -> Result<PlanView, ProductError> {
        let lock = plan_lock(plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let plan = require_owned_plan(&tx, task_id, plan_id)?;
        if plan.state != PlanState::Executing {
            return Err(invalid(format!(
                "cannot stage implementation while Plan is {}",
                plan.state
            )));
        }
        if plan.implementation_dispatch_state == PlanImplementationDispatchState::Dispatched {
            tx.commit().map_err(db_err)?;
            drop(conn);
            return self.require_view(task_id, plan_id);
        }
        if plan.implementation_dispatch_state != PlanImplementationDispatchState::Dispatching {
            return Err(invalid(
                "implementation dispatch must be claimed before staging",
            ));
        }
        let approved_revision = plan
            .approved_revision
            .ok_or_else(|| invalid("executing Plan has no approved revision"))?;
        let queue_id = plan
            .implementation_queue_message_id
            .clone()
            .unwrap_or_else(|| format!("plan-implementation:{plan_id}:{approved_revision}"));
        let branch_exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM session_branches \
                 WHERE task_id = ?1 AND id = ?2 AND is_active = 1)",
                params![task_id, branch_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        if !branch_exists {
            return Err(invalid(
                "implementation dispatch requires the active session branch",
            ));
        }
        let now = Utc::now().to_rfc3339();
        let existing: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT task_id, branch_id, message, state FROM queued_messages WHERE id = ?1",
                params![queue_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(db_err)?;
        if let Some((existing_task, existing_branch, existing_message, state)) = existing {
            if existing_task != task_id {
                return Err(invalid(
                    "implementation queue identity collides with another task",
                ));
            }
            let retryable = matches!(state.as_str(), "failed" | "cancelled");
            if existing_branch != branch_id || existing_message != message {
                if !retryable {
                    return Err(invalid(
                        "implementation queue payload cannot change after delivery was claimed",
                    ));
                }
                tx.execute(
                    "UPDATE queued_messages SET branch_id = ?1, message = ?2, state = 'queued', \
                     priority = 1000000, updated_at = ?3 \
                     WHERE id = ?4 AND task_id = ?5 AND state IN ('failed', 'cancelled')",
                    params![branch_id, message, now, queue_id, task_id],
                )
                .map_err(db_err)?;
            } else if retryable {
                tx.execute(
                    "UPDATE queued_messages SET state = 'queued', priority = 1000000, \
                     updated_at = ?1 WHERE id = ?2 AND state IN ('failed', 'cancelled')",
                    params![now, queue_id],
                )
                .map_err(db_err)?;
            }
        } else {
            tx.execute(
                "INSERT INTO queued_messages \
                 (id, task_id, branch_id, message, state, priority, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, 'queued', 1000000, ?5, ?5)",
                params![queue_id, task_id, branch_id, message, now],
            )
            .map_err(db_err)?;
        }
        let task_updated = tx
            .execute(
                "UPDATE tasks SET mode = 'auto', updated_at = ?1 WHERE id = ?2",
                params![now, task_id],
            )
            .map_err(db_err)?;
        if task_updated != 1 {
            return Err(invalid("task disappeared while implementation was staged"));
        }
        let plan_updated = tx
            .execute(
                "UPDATE plans SET implementation_dispatch_state = 'dispatched', \
                 implementation_dispatch_error = NULL, implementation_queue_message_id = ?1, \
                 implementation_dispatched_at = ?2, updated_at = ?2 \
                 WHERE id = ?3 AND task_id = ?4 \
                 AND implementation_dispatch_state = 'dispatching'",
                params![queue_id, now, plan_id, task_id],
            )
            .map_err(db_err)?;
        if plan_updated != 1 {
            return Err(invalid(
                "implementation dispatch changed while it was staged",
            ));
        }
        tx.commit().map_err(db_err)?;
        drop(conn);
        self.require_view(task_id, plan_id)
    }

    pub fn mark_implementation_dispatch_failed_for_queue(
        &self,
        queue_id: &str,
        error: &str,
    ) -> Result<u64, ProductError> {
        let conn = self.db.conn()?;
        let changed = conn
            .execute(
                "UPDATE plans SET implementation_dispatch_state = 'failed', \
                 implementation_dispatch_error = ?1, implementation_dispatched_at = NULL, \
                 updated_at = ?2 WHERE implementation_queue_message_id = ?3 \
                 AND state = 'executing' AND implementation_dispatch_state = 'dispatched'",
                params![error.trim(), Utc::now().to_rfc3339(), queue_id],
            )
            .map_err(db_err)?;
        u64::try_from(changed)
            .map_err(|_| ProductError::DatabaseError("dispatch failure count overflow".to_string()))
    }

    pub fn mark_implementation_dispatch_failed(
        &self,
        task_id: &str,
        plan_id: &str,
        error: &str,
    ) -> Result<PlanView, ProductError> {
        let conn = self.db.conn()?;
        let changed = conn
            .execute(
                "UPDATE plans SET implementation_dispatch_state = 'failed', \
                 implementation_dispatch_error = ?1, implementation_dispatched_at = NULL, \
                 updated_at = ?2 WHERE id = ?3 AND task_id = ?4 AND state = 'executing' \
                 AND implementation_dispatch_state = 'dispatching'",
                params![error.trim(), Utc::now().to_rfc3339(), plan_id, task_id],
            )
            .map_err(db_err)?;
        if changed != 1 {
            return Err(invalid("only a dispatching implementation can fail"));
        }
        drop(conn);
        self.require_view(task_id, plan_id)
    }

    /// Desktop-authoritative startup recovery. General queue rows claimed by the dead process are
    /// made retryable; Plan pending/dispatching handoffs become visible failures. Safely queued
    /// rows remain queued and are drained separately after startup.
    pub fn recover_interrupted_implementation_dispatches(&self) -> Result<u64, ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "UPDATE queued_messages SET state = 'failed', updated_at = ?1 \
             WHERE state = 'dispatching'",
            params![Utc::now().to_rfc3339()],
        )
        .map_err(db_err)?;
        let interrupted = tx
            .execute(
                "UPDATE plans SET implementation_dispatch_state = 'failed', \
                 implementation_dispatch_error = ?1, implementation_dispatched_at = NULL, \
                 updated_at = ?2 WHERE state = 'executing' \
                 AND implementation_dispatch_state IN ('pending', 'dispatching')",
                params![
                    PLAN_IMPLEMENTATION_DISPATCH_INTERRUPTED,
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(db_err)?;
        let failed_queue = tx
            .execute(
                "UPDATE plans SET implementation_dispatch_state = 'failed', \
                 implementation_dispatch_error = 'PLAN_IMPLEMENTATION_QUEUE_FAILED: durable queue dispatch failed', \
                 implementation_dispatched_at = NULL, updated_at = ?1 \
                 WHERE state = 'executing' AND implementation_dispatch_state = 'dispatched' \
                 AND implementation_queue_message_id IN (SELECT id FROM queued_messages WHERE state = 'failed')",
                params![Utc::now().to_rfc3339()],
            )
            .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        u64::try_from(interrupted + failed_queue).map_err(|_| {
            ProductError::DatabaseError("dispatch recovery count overflow".to_string())
        })
    }

    /// Advances one executing feature and activates the next dependency-ready feature on success.
    pub fn update_plan_item(
        &self,
        task_id: &str,
        input: &UpdatePlanItemInput,
    ) -> Result<PlanView, ProductError> {
        let lock = plan_lock(&input.plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;
        let now = Utc::now().to_rfc3339();

        {
            let mut conn = self.db.conn()?;
            let tx = conn.transaction().map_err(db_err)?;
            let plan = require_owned_plan(&tx, task_id, &input.plan_id)?;
            if plan.revision != input.expected_revision {
                return Err(stale_revision(input.expected_revision, plan.revision));
            }
            if plan.state != PlanState::Executing {
                return Err(invalid(format!(
                    "cannot update a feature while Plan is {}",
                    plan.state
                )));
            }
            let approved_revision = plan
                .approved_revision
                .ok_or_else(|| invalid("executing Plan has no approved revision"))?;
            let item = load_item(&tx, &input.plan_id, approved_revision, &input.item_id)?
                .ok_or_else(|| invalid(format!("unknown Plan feature: {}", input.item_id)))?;
            let next_revision = plan
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("Plan revision overflow"))?;

            match (item.state, input.state) {
                (PlanItemState::Pending, PlanItemState::InProgress) => {
                    let active: i64 = tx
                        .query_row(
                            "SELECT COUNT(*) FROM plan_items WHERE plan_id = ?1 AND revision = ?2 \
                             AND state = 'in_progress'",
                            params![input.plan_id, sql_revision(approved_revision)?],
                            |row| row.get(0),
                        )
                        .map_err(db_err)?;
                    if active != 0 {
                        return Err(invalid("another Plan feature is already in progress"));
                    }
                    ensure_dependencies_completed(&tx, &item)?;
                    tx.execute(
                        "UPDATE plan_items SET state = 'in_progress', started_at = ?1, updated_at = ?1 \
                         WHERE id = ?2 AND plan_id = ?3 AND revision = ?4 AND state = 'pending'",
                        params![
                            now,
                            input.item_id,
                            input.plan_id,
                            sql_revision(approved_revision)?,
                        ],
                    )
                    .map_err(db_err)?;
                }
                (PlanItemState::InProgress, PlanItemState::Completed) => {
                    tx.execute(
                        "UPDATE plan_items SET state = 'completed', completed_at = ?1, updated_at = ?1 \
                         WHERE id = ?2 AND plan_id = ?3 AND revision = ?4 AND state = 'in_progress'",
                        params![
                            now,
                            input.item_id,
                            input.plan_id,
                            sql_revision(approved_revision)?,
                        ],
                    )
                    .map_err(db_err)?;
                    if let Some(next_id) = next_ready_item(&tx, &input.plan_id, approved_revision)?
                    {
                        tx.execute(
                            "UPDATE plan_items SET state = 'in_progress', started_at = ?1, updated_at = ?1 \
                             WHERE id = ?2 AND plan_id = ?3 AND revision = ?4 AND state = 'pending'",
                            params![
                                now,
                                next_id,
                                input.plan_id,
                                sql_revision(approved_revision)?,
                            ],
                        )
                        .map_err(db_err)?;
                    }
                }
                (PlanItemState::InProgress, PlanItemState::Blocked) => {
                    tx.execute(
                        "UPDATE plan_items SET state = 'blocked', updated_at = ?1 \
                         WHERE id = ?2 AND plan_id = ?3 AND revision = ?4 AND state = 'in_progress'",
                        params![
                            now,
                            input.item_id,
                            input.plan_id,
                            sql_revision(approved_revision)?,
                        ],
                    )
                    .map_err(db_err)?;
                }
                (PlanItemState::Blocked, PlanItemState::InProgress) => {
                    let active: i64 = tx
                        .query_row(
                            "SELECT COUNT(*) FROM plan_items WHERE plan_id = ?1 AND revision = ?2 \
                             AND state = 'in_progress'",
                            params![input.plan_id, sql_revision(approved_revision)?],
                            |row| row.get(0),
                        )
                        .map_err(db_err)?;
                    if active != 0 {
                        return Err(invalid("another Plan feature is already in progress"));
                    }
                    ensure_dependencies_completed(&tx, &item)?;
                    let updated = tx
                        .execute(
                            "UPDATE plan_items SET state = 'in_progress', \
                             started_at = COALESCE(started_at, ?1), updated_at = ?1 \
                             WHERE id = ?2 AND plan_id = ?3 AND revision = ?4 \
                             AND state = 'blocked'",
                            params![
                                now,
                                input.item_id,
                                input.plan_id,
                                sql_revision(approved_revision)?,
                            ],
                        )
                        .map_err(db_err)?;
                    if updated != 1 {
                        return Err(invalid("blocked Plan feature changed while it was resumed"));
                    }
                }
                (current, requested) => {
                    return Err(invalid(format!(
                        "invalid Plan feature transition: {current} -> {requested}"
                    )));
                }
            }

            let incomplete: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM plan_items WHERE plan_id = ?1 AND revision = ?2 \
                     AND state <> 'completed'",
                    params![input.plan_id, sql_revision(approved_revision)?],
                    |row| row.get(0),
                )
                .map_err(db_err)?;
            let state = if incomplete == 0 {
                "completed"
            } else {
                "executing"
            };
            let updated = tx
                .execute(
                    "UPDATE plans SET revision = ?1, state = ?2, updated_at = ?3, \
                     projection_error = NULL WHERE id = ?4 AND task_id = ?5 AND revision = ?6 \
                     AND state = 'executing'",
                    params![
                        sql_revision(next_revision)?,
                        state,
                        now,
                        input.plan_id,
                        task_id,
                        sql_revision(input.expected_revision)?,
                    ],
                )
                .map_err(db_err)?;
            if updated != 1 {
                return Err(invalid("Plan changed while its feature was being updated"));
            }
            tx.commit().map_err(db_err)?;
        }

        self.sync_projection_locked(&input.plan_id)?;
        self.require_view(task_id, &input.plan_id)
    }

    /// Atomically claims a continuation. `None` means another caller already claimed/dispatched it.
    pub fn claim_continuation(
        &self,
        task_id: &str,
        question_set_id: &str,
    ) -> Result<Option<PlanQuestionSet>, ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let set = require_question_set(&tx, task_id, question_set_id)?;
        match set.continuation_state {
            PlanContinuationState::Dispatching | PlanContinuationState::Dispatched => {
                tx.commit().map_err(db_err)?;
                Ok(None)
            }
            PlanContinuationState::Pending | PlanContinuationState::Failed => {
                tx.execute(
                    "UPDATE plan_question_sets SET continuation_state = 'dispatching', \
                     continuation_error = NULL WHERE id = ?1 AND continuation_state IN ('pending', 'failed')",
                    params![question_set_id],
                )
                .map_err(db_err)?;
                tx.commit().map_err(db_err)?;
                drop(conn);
                self.get_question_set(task_id, question_set_id).map(Some)
            }
            PlanContinuationState::NotRequested => {
                Err(invalid("question set has not been resolved"))
            }
        }
    }

    pub fn mark_continuation_dispatched(
        &self,
        task_id: &str,
        question_set_id: &str,
    ) -> Result<PlanQuestionSet, ProductError> {
        let conn = self.db.conn()?;
        if !question_set_belongs_to_task(&conn, task_id, question_set_id)? {
            return Err(invalid("question set does not belong to task"));
        }
        let current = require_question_set(&conn, task_id, question_set_id)?;
        if current.continuation_state == PlanContinuationState::Dispatched {
            return Ok(current);
        }
        let now = Utc::now().to_rfc3339();
        let changed = conn.execute(
            "UPDATE plan_question_sets SET continuation_state = 'dispatched', dispatched_at = ?1, \
             continuation_error = NULL WHERE id = ?2 AND continuation_state = 'dispatching'",
            params![now, question_set_id],
        )
        .map_err(db_err)?;
        if changed != 1 {
            return Err(invalid("only a dispatching continuation can complete"));
        }
        drop(conn);
        self.get_question_set(task_id, question_set_id)
    }

    pub fn mark_continuation_failed(
        &self,
        task_id: &str,
        question_set_id: &str,
        error: &str,
    ) -> Result<PlanQuestionSet, ProductError> {
        let conn = self.db.conn()?;
        if !question_set_belongs_to_task(&conn, task_id, question_set_id)? {
            return Err(invalid("question set does not belong to task"));
        }
        let changed = conn
            .execute(
                "UPDATE plan_question_sets SET continuation_state = 'failed', continuation_error = ?1 \
                 WHERE id = ?2 AND continuation_state = 'dispatching'",
                params![error.trim(), question_set_id],
            )
            .map_err(db_err)?;
        if changed != 1 {
            return Err(invalid("only a dispatching continuation can fail"));
        }
        drop(conn);
        self.get_question_set(task_id, question_set_id)
    }

    pub fn retry_continuation(
        &self,
        task_id: &str,
        question_set_id: &str,
    ) -> Result<PlanQuestionSet, ProductError> {
        let conn = self.db.conn()?;
        if !question_set_belongs_to_task(&conn, task_id, question_set_id)? {
            return Err(invalid("question set does not belong to task"));
        }
        let changed = conn
            .execute(
                "UPDATE plan_question_sets SET continuation_state = 'pending', continuation_error = NULL \
                 WHERE id = ?1 AND continuation_state = 'failed'",
                params![question_set_id],
            )
            .map_err(db_err)?;
        if changed != 1 {
            return Err(invalid("only a failed continuation can be retried"));
        }
        drop(conn);
        self.get_question_set(task_id, question_set_id)
    }

    pub fn get_question_set(
        &self,
        task_id: &str,
        question_set_id: &str,
    ) -> Result<PlanQuestionSet, ProductError> {
        let conn = self.db.conn()?;
        require_question_set(&conn, task_id, question_set_id)
    }

    /// Rebuilds the stable projection from SQLite. Filesystem failures are recorded on the Plan.
    pub fn repair_projection(
        &self,
        task_id: &str,
        plan_id: &str,
    ) -> Result<PlanView, ProductError> {
        let lock = plan_lock(plan_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| ProductError::Other("Plan lock is poisoned".to_string()))?;
        let expected = self.canonical_projection_path(plan_id)?;
        let conn = self.db.conn()?;
        if !plan_belongs_to_task(&conn, task_id, plan_id)? {
            return Err(invalid("Plan does not belong to task"));
        }
        conn.execute(
            "UPDATE plans SET projection_path = ?1, projection_error = NULL WHERE id = ?2",
            params![expected.to_string_lossy().into_owned(), plan_id],
        )
        .map_err(db_err)?;
        drop(conn);
        self.sync_projection_locked(plan_id)?;
        self.require_view(task_id, plan_id)
    }

    fn plan_id_for_question_set(
        &self,
        task_id: &str,
        question_set_id: &str,
    ) -> Result<String, ProductError> {
        let conn = self.db.conn()?;
        conn.query_row(
            "SELECT q.plan_id FROM plan_question_sets q JOIN plans p ON p.id = q.plan_id \
             WHERE q.id = ?1 AND p.task_id = ?2",
            params![question_set_id, task_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| invalid("question set does not belong to task"))
    }

    fn require_view(&self, task_id: &str, plan_id: &str) -> Result<PlanView, ProductError> {
        self.get_plan(task_id, plan_id)?
            .ok_or_else(|| invalid(format!("Plan does not belong to task: {plan_id}")))
    }

    fn load_view(
        &self,
        conn: &Connection,
        task_id: &str,
        plan_id: &str,
    ) -> Result<PlanView, ProductError> {
        let plan = require_owned_plan(conn, task_id, plan_id)?;
        let goal = load_goal(conn, task_id)?;
        let item_revision = match plan.approved_revision {
            Some(revision) => Some(revision),
            None => conn
                .query_row(
                    "SELECT MAX(revision) FROM plan_items WHERE plan_id = ?1",
                    params![plan_id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .map_err(db_err)?
                .map(|revision| revision as u64),
        };
        let items = item_revision
            .map(|revision| load_items(conn, plan_id, revision))
            .transpose()?
            .unwrap_or_default();
        let pending_id: Option<String> = conn
            .query_row(
                "SELECT id FROM plan_question_sets WHERE plan_id = ?1 AND state = 'pending' \
                 ORDER BY created_at DESC LIMIT 1",
                params![plan_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let pending_question_set = pending_id
            .as_deref()
            .map(|id| load_question_set(conn, id))
            .transpose()?;
        let continuation_id: Option<String> = conn
            .query_row(
                "SELECT id FROM plan_question_sets WHERE plan_id = ?1 \
                 AND state IN ('answered', 'skipped') \
                 AND continuation_state IN ('pending', 'dispatching', 'failed') \
                 ORDER BY resolved_at DESC, created_at DESC LIMIT 1",
                params![plan_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let continuation_question_set = continuation_id
            .as_deref()
            .map(|id| load_question_set(conn, id))
            .transpose()?;
        Ok(PlanView {
            plan,
            goal,
            items,
            pending_question_set,
            continuation_question_set,
        })
    }

    /// Caller holds the per-Plan mutex. No database connection survives into filesystem I/O.
    fn sync_projection_locked(&self, plan_id: &str) -> Result<(), ProductError> {
        let expected_target = match self.canonical_projection_path(plan_id) {
            Ok(path) => path,
            Err(error) => {
                self.record_projection_error(plan_id, &error.to_string())?;
                return Ok(());
            }
        };
        for _ in 0..3 {
            let view = {
                let conn = self.db.conn()?;
                let task_id: String = conn
                    .query_row(
                        "SELECT task_id FROM plans WHERE id = ?1",
                        params![plan_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(db_err)?
                    .ok_or_else(|| invalid(format!("unknown Plan: {plan_id}")))?;
                self.load_view(&conn, &task_id, plan_id)?
            };
            let revision = view.plan.revision;
            let Some(path) = view.plan.projection_path.as_deref() else {
                self.record_projection_error(plan_id, "Plan has no projection path")?;
                return Ok(());
            };
            if Path::new(path) != expected_target.as_path() {
                self.record_projection_error(
                    plan_id,
                    "Plan projection path does not match the canonical AppData target",
                )?;
                return Ok(());
            }
            let rendered = render_plan_markdown(&view);
            let persisted = match write_projection_temp_and_persist(
                &self.db,
                plan_id,
                revision,
                &expected_target,
                rendered.as_bytes(),
            ) {
                Ok(persisted) => persisted,
                Err(error) => {
                    self.record_projection_error(plan_id, &error.to_string())?;
                    return Ok(());
                }
            };
            if !persisted {
                continue;
            }

            let conn = self.db.conn()?;
            let changed = conn
                .execute(
                    "UPDATE plans SET projection_revision = ?1, projection_error = NULL \
                     WHERE id = ?2 AND revision = ?1",
                    params![sql_revision(revision)?, plan_id],
                )
                .map_err(db_err)?;
            if changed == 1 {
                return Ok(());
            }
        }
        self.record_projection_error(plan_id, "Plan changed repeatedly during projection")?;
        Ok(())
    }

    fn record_projection_error(&self, plan_id: &str, error: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE plans SET projection_error = ?1 WHERE id = ?2",
            params![error, plan_id],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

fn stale_revision(expected: u64, actual: u64) -> ProductError {
    invalid(format!(
        "stale Plan revision: expected {expected}, current {actual}"
    ))
}

fn validate_identifier(value: &str, label: &str) -> Result<(), ProductError> {
    if value.trim().is_empty() || value.len() > 256 || value.contains('\0') {
        return Err(invalid(format!("{label} must be 1-256 safe characters")));
    }
    Ok(())
}

fn validate_item_drafts(items: &[PlanItemDraft]) -> Result<(), ProductError> {
    if items.is_empty() || items.len() > MAX_PLAN_ITEMS {
        return Err(invalid(format!(
            "a Plan must contain 1-{MAX_PLAN_ITEMS} feature items"
        )));
    }
    let mut ids = HashSet::with_capacity(items.len());
    for item in items {
        validate_identifier(&item.id, "feature id")?;
        if !ids.insert(item.id.as_str()) {
            return Err(invalid(format!("duplicate feature id: {}", item.id)));
        }
        if item.title.trim().is_empty() || item.title.chars().count() > 200 {
            return Err(invalid("feature title must be 1-200 characters"));
        }
        if item.description.chars().count() > 20_000 {
            return Err(invalid("feature description exceeds 20000 characters"));
        }
        if item.section_path.len() > 4 {
            return Err(invalid("feature section_path supports at most 4 levels"));
        }
        for segment in &item.section_path {
            let length = segment.trim().chars().count();
            if !(1..=120).contains(&length) || segment.contains('\0') {
                return Err(invalid(
                    "feature section_path labels must be 1-120 safe characters",
                ));
            }
        }
        let mut dependencies = HashSet::new();
        for dependency in &item.depends_on {
            validate_identifier(dependency, "dependency id")?;
            if dependency == &item.id {
                return Err(invalid(format!(
                    "feature {} cannot depend on itself",
                    item.id
                )));
            }
            if !dependencies.insert(dependency) {
                return Err(invalid(format!(
                    "feature {} repeats dependency {dependency}",
                    item.id
                )));
            }
        }
    }
    for item in items {
        for dependency in &item.depends_on {
            if !ids.contains(dependency.as_str()) {
                return Err(invalid(format!(
                    "feature {} depends on unknown feature {dependency}",
                    item.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_question_drafts(input: &RequestPlanQuestionsInput) -> Result<(), ProductError> {
    if !(1..=3).contains(&input.questions.len()) {
        return Err(invalid("a question set must contain 1-3 questions"));
    }
    let mut identities = HashSet::new();
    for question in &input.questions {
        validate_identifier(&question.id, "question id")?;
        if !identities.insert(question.id.as_str()) {
            return Err(invalid(format!("duplicate question id: {}", question.id)));
        }
        let header_length = question.header.trim().chars().count();
        if !(1..=64).contains(&header_length) {
            return Err(invalid("question header must be 1-64 characters"));
        }
        if question.question.trim().is_empty() || question.question.chars().count() > 2_000 {
            return Err(invalid("question prompt must be 1-2000 characters"));
        }
        if !(2..=3).contains(&question.options.len()) {
            return Err(invalid("each question must contain 2-3 options"));
        }
        for option in &question.options {
            validate_identifier(&option.id, "option id")?;
            if !identities.insert(option.id.as_str()) {
                return Err(invalid(format!(
                    "duplicate question/option id: {}",
                    option.id
                )));
            }
            if option.label.trim().is_empty() || option.label.chars().count() > 120 {
                return Err(invalid("option label must be 1-120 characters"));
            }
            if option.description.chars().count() > 1_000 {
                return Err(invalid("option description exceeds 1000 characters"));
            }
        }
    }
    Ok(())
}

fn validate_answer_shape(input: &AnswerPlanQuestionsInput) -> Result<(), ProductError> {
    validate_identifier(&input.question_set_id, "question set id")?;
    validate_identifier(&input.idempotency_key, "idempotency key")?;
    if input.skip_all && !input.answers.is_empty() {
        return Err(invalid("skip_all requires an empty answers list"));
    }
    if !input.skip_all && input.answers.is_empty() {
        return Err(invalid("answers cannot be empty unless skip_all is true"));
    }
    let mut ids = HashSet::new();
    for answer in &input.answers {
        validate_identifier(answer.question_id(), "answer question id")?;
        if !ids.insert(answer.question_id()) {
            return Err(invalid(format!(
                "duplicate answer for question {}",
                answer.question_id()
            )));
        }
        match answer {
            PlanQuestionAnswerInput::Option { option_id, .. } => {
                validate_identifier(option_id, "answer option id")?;
            }
            PlanQuestionAnswerInput::Text { text, .. } => {
                let text = text.trim();
                if text.is_empty() || text.chars().count() > MAX_TEXT_ANSWER_CHARS {
                    return Err(invalid(format!(
                        "free-form answer must be 1-{MAX_TEXT_ANSWER_CHARS} characters"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_answers_against_set(
    set: &PlanQuestionSet,
    input: &AnswerPlanQuestionsInput,
) -> Result<(), ProductError> {
    if input.skip_all {
        return Ok(());
    }
    if input.answers.len() != set.questions.len() {
        return Err(invalid(
            "answers must contain exactly one response per question",
        ));
    }
    let answers: HashMap<&str, &PlanQuestionAnswerInput> = input
        .answers
        .iter()
        .map(|answer| (answer.question_id(), answer))
        .collect();
    for question in &set.questions {
        let answer = answers
            .get(question.id.as_str())
            .ok_or_else(|| invalid(format!("missing answer for question {}", question.id)))?;
        if let PlanQuestionAnswerInput::Option { option_id, .. } = answer {
            if !question
                .options
                .iter()
                .any(|option| option.id == *option_id)
            {
                return Err(invalid(format!(
                    "option {option_id} does not belong to question {}",
                    question.id
                )));
            }
        }
    }
    Ok(())
}

fn answer_payload_matches(set: &PlanQuestionSet, input: &AnswerPlanQuestionsInput) -> bool {
    match set.state {
        PlanQuestionSetState::Skipped => input.skip_all && input.answers.is_empty(),
        PlanQuestionSetState::Answered if !input.skip_all => {
            if input.answers.len() != set.questions.len() {
                return false;
            }
            let answers: HashMap<&str, &PlanQuestionAnswerInput> = input
                .answers
                .iter()
                .map(|answer| (answer.question_id(), answer))
                .collect();
            set.questions.iter().all(|question| {
                matches!(
                    (question.answer.as_ref(), answers.get(question.id.as_str())),
                    (
                        Some(PlanQuestionAnswer::Option { option_id: stored }),
                        Some(PlanQuestionAnswerInput::Option { option_id, .. })
                    ) if stored == option_id
                ) || matches!(
                    (question.answer.as_ref(), answers.get(question.id.as_str())),
                    (
                        Some(PlanQuestionAnswer::FreeForm { text: stored }),
                        Some(PlanQuestionAnswerInput::Text { text, .. })
                    ) if stored == text.trim()
                )
            })
        }
        _ => false,
    }
}

fn plan_belongs_to_task(
    conn: &Connection,
    task_id: &str,
    plan_id: &str,
) -> Result<bool, ProductError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM plans WHERE id = ?1 AND task_id = ?2",
            params![plan_id, task_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(db_err)?
        .is_some())
}

fn question_set_belongs_to_task(
    conn: &Connection,
    task_id: &str,
    question_set_id: &str,
) -> Result<bool, ProductError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM plan_question_sets q JOIN plans p ON p.id = q.plan_id \
             WHERE q.id = ?1 AND p.task_id = ?2",
            params![question_set_id, task_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(db_err)?
        .is_some())
}

fn require_owned_plan(
    conn: &Connection,
    task_id: &str,
    plan_id: &str,
) -> Result<Plan, ProductError> {
    load_plan(conn, plan_id)?
        .filter(|plan| plan.task_id == task_id)
        .ok_or_else(|| invalid(format!("Plan does not belong to task: {plan_id}")))
}

fn load_plan(conn: &Connection, plan_id: &str) -> Result<Option<Plan>, ProductError> {
    let mut statement = conn
        .prepare(
            "SELECT id, task_id, revision, state, approved_revision, projection_path, \
             projection_revision, projection_error, created_at, updated_at, approved_at, \
             implementation_dispatch_state, implementation_dispatch_error, \
             implementation_queue_message_id, implementation_dispatched_at \
             FROM plans WHERE id = ?1",
        )
        .map_err(db_err)?;
    let mut rows = statement.query(params![plan_id]).map_err(db_err)?;
    let Some(row) = rows.next().map_err(db_err)? else {
        return Ok(None);
    };
    let revision: i64 = row.get(2).map_err(db_err)?;
    let state: String = row.get(3).map_err(db_err)?;
    let approved_revision: Option<i64> = row.get(4).map_err(db_err)?;
    let projection_revision: Option<i64> = row.get(6).map_err(db_err)?;
    let created_at: String = row.get(8).map_err(db_err)?;
    let updated_at: String = row.get(9).map_err(db_err)?;
    let approved_at: Option<String> = row.get(10).map_err(db_err)?;
    let implementation_dispatch_state: String = row.get(11).map_err(db_err)?;
    let implementation_dispatched_at: Option<String> = row.get(14).map_err(db_err)?;
    Ok(Some(Plan {
        id: row.get(0).map_err(db_err)?,
        task_id: row.get(1).map_err(db_err)?,
        revision: revision as u64,
        state: PlanState::try_from_str(&state)
            .ok_or_else(|| ProductError::DatabaseError(format!("invalid Plan state: {state}")))?,
        approved_revision: approved_revision.map(|revision| revision as u64),
        projection_path: row.get(5).map_err(db_err)?,
        projection_revision: projection_revision.map(|revision| revision as u64),
        projection_error: row.get(7).map_err(db_err)?,
        created_at: parse_ts(&created_at)?,
        updated_at: parse_ts(&updated_at)?,
        approved_at: approved_at.as_deref().map(parse_ts).transpose()?,
        implementation_dispatch_state: PlanImplementationDispatchState::try_from_str(
            &implementation_dispatch_state,
        )
        .ok_or_else(|| {
            ProductError::DatabaseError(format!(
                "invalid Plan implementation dispatch state: {implementation_dispatch_state}"
            ))
        })?,
        implementation_dispatch_error: row.get(12).map_err(db_err)?,
        implementation_queue_message_id: row.get(13).map_err(db_err)?,
        implementation_dispatched_at: implementation_dispatched_at
            .as_deref()
            .map(parse_ts)
            .transpose()?,
    }))
}

fn load_goal(conn: &Connection, task_id: &str) -> Result<PlanGoal, ProductError> {
    let (goal, updated_at): (String, String) = conn
        .query_row(
            "SELECT goal, updated_at FROM tasks WHERE id = ?1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| invalid(format!("task does not exist: {task_id}")))?;
    Ok(PlanGoal {
        task_id: task_id.to_string(),
        goal,
        updated_at: parse_ts(&updated_at)?,
    })
}

fn load_items(
    conn: &Connection,
    plan_id: &str,
    revision: u64,
) -> Result<Vec<PlanItem>, ProductError> {
    let mut statement = conn
        .prepare(
            "SELECT id, ordinal, title, description, state, created_at, updated_at, started_at, completed_at, section_path_json \
             FROM plan_items WHERE plan_id = ?1 AND revision = ?2 ORDER BY ordinal ASC",
        )
        .map_err(db_err)?;
    let mut rows = statement
        .query(params![plan_id, sql_revision(revision)?])
        .map_err(db_err)?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().map_err(db_err)? {
        let id: String = row.get(0).map_err(db_err)?;
        let ordinal: i64 = row.get(1).map_err(db_err)?;
        let state: String = row.get(4).map_err(db_err)?;
        let created_at: String = row.get(5).map_err(db_err)?;
        let updated_at: String = row.get(6).map_err(db_err)?;
        let started_at: Option<String> = row.get(7).map_err(db_err)?;
        let completed_at: Option<String> = row.get(8).map_err(db_err)?;
        let section_path_json: String = row.get(9).map_err(db_err)?;
        let section_path =
            serde_json::from_str::<Vec<String>>(&section_path_json).map_err(|error| {
                ProductError::DatabaseError(format!("invalid Plan section_path_json: {error}"))
            })?;
        items.push(PlanItem {
            id: id.clone(),
            plan_id: plan_id.to_string(),
            revision,
            ordinal: ordinal as u32,
            title: row.get(2).map_err(db_err)?,
            description: row.get(3).map_err(db_err)?,
            section_path,
            state: PlanItemState::try_from_str(&state).ok_or_else(|| {
                ProductError::DatabaseError(format!("invalid Plan item state: {state}"))
            })?,
            depends_on: load_dependencies(conn, plan_id, revision, &id)?,
            created_at: parse_ts(&created_at)?,
            updated_at: parse_ts(&updated_at)?,
            started_at: started_at.as_deref().map(parse_ts).transpose()?,
            completed_at: completed_at.as_deref().map(parse_ts).transpose()?,
        });
    }
    Ok(items)
}

fn load_item(
    conn: &Connection,
    plan_id: &str,
    revision: u64,
    item_id: &str,
) -> Result<Option<PlanItem>, ProductError> {
    Ok(load_items(conn, plan_id, revision)?
        .into_iter()
        .find(|item| item.id == item_id))
}

fn load_dependencies(
    conn: &Connection,
    plan_id: &str,
    revision: u64,
    item_id: &str,
) -> Result<Vec<String>, ProductError> {
    let mut statement = conn
        .prepare(
            "SELECT d.depends_on_item_id FROM plan_item_dependencies d \
             JOIN plan_items i ON i.id = d.depends_on_item_id \
             WHERE d.plan_id = ?1 AND d.revision = ?2 AND d.item_id = ?3 \
             ORDER BY i.ordinal ASC",
        )
        .map_err(db_err)?;
    let values = statement
        .query_map(params![plan_id, sql_revision(revision)?, item_id], |row| {
            row.get(0)
        })
        .map_err(db_err)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(db_err)?;
    Ok(values)
}

fn require_question_set(
    conn: &Connection,
    task_id: &str,
    question_set_id: &str,
) -> Result<PlanQuestionSet, ProductError> {
    if !question_set_belongs_to_task(conn, task_id, question_set_id)? {
        return Err(invalid("question set does not belong to task"));
    }
    load_question_set(conn, question_set_id)
}

fn load_question_set(
    conn: &Connection,
    question_set_id: &str,
) -> Result<PlanQuestionSet, ProductError> {
    let mut statement = conn
        .prepare(
            "SELECT id, plan_id, revision, state, answer_idempotency_key, continuation_state, \
             continuation_error, created_at, resolved_at, dispatched_at \
             FROM plan_question_sets WHERE id = ?1",
        )
        .map_err(db_err)?;
    let mut rows = statement.query(params![question_set_id]).map_err(db_err)?;
    let row = rows
        .next()
        .map_err(db_err)?
        .ok_or_else(|| invalid(format!("unknown question set: {question_set_id}")))?;
    let revision: i64 = row.get(2).map_err(db_err)?;
    let state: String = row.get(3).map_err(db_err)?;
    let continuation_state: String = row.get(5).map_err(db_err)?;
    let created_at: String = row.get(7).map_err(db_err)?;
    let resolved_at: Option<String> = row.get(8).map_err(db_err)?;
    let dispatched_at: Option<String> = row.get(9).map_err(db_err)?;
    Ok(PlanQuestionSet {
        id: row.get(0).map_err(db_err)?,
        plan_id: row.get(1).map_err(db_err)?,
        revision: revision as u64,
        state: PlanQuestionSetState::try_from_str(&state).ok_or_else(|| {
            ProductError::DatabaseError(format!("invalid question set state: {state}"))
        })?,
        answer_idempotency_key: row.get(4).map_err(db_err)?,
        continuation_state: PlanContinuationState::try_from_str(&continuation_state).ok_or_else(
            || {
                ProductError::DatabaseError(format!(
                    "invalid continuation state: {continuation_state}"
                ))
            },
        )?,
        continuation_error: row.get(6).map_err(db_err)?,
        questions: load_questions(conn, question_set_id)?,
        created_at: parse_ts(&created_at)?,
        resolved_at: resolved_at.as_deref().map(parse_ts).transpose()?,
        dispatched_at: dispatched_at.as_deref().map(parse_ts).transpose()?,
    })
}

fn load_questions(
    conn: &Connection,
    question_set_id: &str,
) -> Result<Vec<PlanQuestion>, ProductError> {
    let mut statement = conn
        .prepare(
            "SELECT id, ordinal, header, question, answer_kind, answer_value, answered_at \
             FROM plan_questions WHERE question_set_id = ?1 ORDER BY ordinal ASC",
        )
        .map_err(db_err)?;
    let mut rows = statement.query(params![question_set_id]).map_err(db_err)?;
    let mut questions = Vec::new();
    while let Some(row) = rows.next().map_err(db_err)? {
        let id: String = row.get(0).map_err(db_err)?;
        let ordinal: i64 = row.get(1).map_err(db_err)?;
        let answer_kind: Option<String> = row.get(4).map_err(db_err)?;
        let answer_value: Option<String> = row.get(5).map_err(db_err)?;
        let answered_at: Option<String> = row.get(6).map_err(db_err)?;
        let answer = match (answer_kind.as_deref(), answer_value) {
            (None, None) => None,
            (Some("option"), Some(option_id)) => Some(PlanQuestionAnswer::Option { option_id }),
            (Some("free_form"), Some(text)) => Some(PlanQuestionAnswer::FreeForm { text }),
            (kind, _) => {
                return Err(ProductError::DatabaseError(format!(
                    "invalid stored question answer kind: {kind:?}"
                )));
            }
        };
        questions.push(PlanQuestion {
            id: id.clone(),
            question_set_id: question_set_id.to_string(),
            ordinal: ordinal as u32,
            header: row.get(2).map_err(db_err)?,
            question: row.get(3).map_err(db_err)?,
            options: load_question_options(conn, question_set_id, &id)?,
            answer,
            answered_at: answered_at.as_deref().map(parse_ts).transpose()?,
        });
    }
    Ok(questions)
}

fn load_question_options(
    conn: &Connection,
    question_set_id: &str,
    question_id: &str,
) -> Result<Vec<PlanQuestionOption>, ProductError> {
    let mut statement = conn
        .prepare(
            "SELECT id, label, description FROM plan_question_options \
             WHERE question_set_id = ?1 AND question_id = ?2 ORDER BY ordinal ASC",
        )
        .map_err(db_err)?;
    let options = statement
        .query_map(params![question_set_id, question_id], |row| {
            Ok(PlanQuestionOption {
                id: row.get(0)?,
                label: row.get(1)?,
                description: row.get(2)?,
            })
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    Ok(options)
}

fn validate_dependency_dag(items: &[PlanItem]) -> Result<(), ProductError> {
    let mut indegree: HashMap<&str, usize> = items
        .iter()
        .map(|item| (item.id.as_str(), item.depends_on.len()))
        .collect();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for item in items {
        for dependency in &item.depends_on {
            if !indegree.contains_key(dependency.as_str()) {
                return Err(invalid(format!(
                    "feature {} depends on unknown feature {dependency}",
                    item.id
                )));
            }
            dependents
                .entry(dependency.as_str())
                .or_default()
                .push(item.id.as_str());
        }
    }
    let mut ready: VecDeque<&str> = items
        .iter()
        .filter(|item| item.depends_on.is_empty())
        .map(|item| item.id.as_str())
        .collect();
    let mut visited = 0;
    while let Some(id) = ready.pop_front() {
        visited += 1;
        if let Some(children) = dependents.get(id) {
            for child in children {
                let degree = indegree
                    .get_mut(child)
                    .ok_or_else(|| invalid("dependency graph is inconsistent"))?;
                *degree -= 1;
                if *degree == 0 {
                    ready.push_back(child);
                }
            }
        }
    }
    if visited != items.len() {
        return Err(invalid("Plan feature dependencies contain a cycle"));
    }
    Ok(())
}

fn ensure_dependencies_completed(conn: &Connection, item: &PlanItem) -> Result<(), ProductError> {
    for dependency in &item.depends_on {
        let state: String = conn
            .query_row(
                "SELECT state FROM plan_items WHERE id = ?1 AND plan_id = ?2 AND revision = ?3",
                params![dependency, item.plan_id, sql_revision(item.revision)?],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        if state != "completed" {
            return Err(invalid(format!(
                "feature {} is waiting for dependency {dependency}",
                item.id
            )));
        }
    }
    Ok(())
}

fn next_ready_item(
    conn: &Connection,
    plan_id: &str,
    revision: u64,
) -> Result<Option<String>, ProductError> {
    conn.query_row(
        "SELECT candidate.id FROM plan_items candidate \
         WHERE candidate.plan_id = ?1 AND candidate.revision = ?2 AND candidate.state = 'pending' \
         AND NOT EXISTS ( \
             SELECT 1 FROM plan_item_dependencies dependency \
             JOIN plan_items required ON required.id = dependency.depends_on_item_id \
             WHERE dependency.plan_id = candidate.plan_id \
             AND dependency.revision = candidate.revision \
             AND dependency.item_id = candidate.id AND required.state <> 'completed' \
         ) ORDER BY candidate.ordinal ASC LIMIT 1",
        params![plan_id, sql_revision(revision)?],
        |row| row.get(0),
    )
    .optional()
    .map_err(db_err)
}

fn render_plan_markdown(view: &PlanView) -> String {
    let mut output = String::new();
    output.push_str("# R-Code Plan\n\n");
    output.push_str(&format!("- Plan: `{}`\n", view.plan.id));
    output.push_str(&format!("- Task: `{}`\n", view.plan.task_id));
    output.push_str(&format!("- Revision: {}\n", view.plan.revision));
    output.push_str(&format!("- State: `{}`\n", view.plan.state));
    if let Some(revision) = view.plan.approved_revision {
        output.push_str(&format!("- Approved content revision: {revision}\n"));
    }
    output.push_str("\n## Goal\n\n");
    if view.goal.goal.trim().is_empty() {
        output.push_str("_No goal set._\n");
    } else {
        output.push_str(view.goal.goal.trim());
        output.push('\n');
    }
    if let Some(set) = &view.pending_question_set {
        output.push_str("\n## Waiting for user input\n\n");
        for question in &set.questions {
            output.push_str(&format!(
                "### {}\n\n{}\n\n",
                question.header, question.question
            ));
            for option in &question.options {
                output.push_str(&format!(
                    "- **{}** — {}\n",
                    option.label, option.description
                ));
            }
        }
    }
    output.push_str("\n## Feature plan\n\n");
    if view.items.is_empty() {
        output.push_str("_No feature items published._\n");
    } else {
        let mut outline = Vec::new();
        for item in &view.items {
            insert_outline_item(&mut outline, &item.section_path, item);
        }
        render_outline_nodes(&mut output, &outline, &[]);
    }
    output
}

enum PlanOutlineNode<'a> {
    Section {
        title: String,
        children: Vec<PlanOutlineNode<'a>>,
    },
    Item(&'a PlanItem),
}

fn insert_outline_item<'a>(
    nodes: &mut Vec<PlanOutlineNode<'a>>,
    section_path: &[String],
    item: &'a PlanItem,
) {
    let Some((section, remainder)) = section_path.split_first() else {
        nodes.push(PlanOutlineNode::Item(item));
        return;
    };
    let index = nodes
        .iter()
        .position(|node| matches!(node, PlanOutlineNode::Section { title, .. } if title == section))
        .unwrap_or_else(|| {
            nodes.push(PlanOutlineNode::Section {
                title: section.clone(),
                children: Vec::new(),
            });
            nodes.len() - 1
        });
    if let PlanOutlineNode::Section { children, .. } = &mut nodes[index] {
        insert_outline_item(children, remainder, item);
    }
}

fn render_outline_nodes(output: &mut String, nodes: &[PlanOutlineNode<'_>], prefix: &[usize]) {
    for (index, node) in nodes.iter().enumerate() {
        let mut number_path = prefix.to_vec();
        number_path.push(index + 1);
        let number = number_path
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(".");
        match node {
            PlanOutlineNode::Section { title, children } => {
                let heading_level = (3 + prefix.len()).min(6);
                output.push_str(&format!(
                    "\n{} {number} {}\n\n",
                    "#".repeat(heading_level),
                    title.trim()
                ));
                render_outline_nodes(output, children, &number_path);
            }
            PlanOutlineNode::Item(item) => {
                let marker = if item.state == PlanItemState::Completed {
                    "x"
                } else {
                    " "
                };
                output.push_str(&format!(
                    "- [{marker}] **{number} {}** (`{}`) — `{}`\n",
                    item.title, item.id, item.state
                ));
                if !item.description.trim().is_empty() {
                    output.push_str(&format!("  {}\n", item.description.trim()));
                }
                if !item.depends_on.is_empty() {
                    output.push_str(&format!("  Depends on: {}\n", item.depends_on.join(", ")));
                }
            }
        }
    }
}

/// Builds and syncs the temporary file, drops every database connection, rechecks the committed
/// revision, then atomically replaces the target. `false` asks the caller to render the newer row.
fn write_projection_temp_and_persist(
    db: &Database,
    plan_id: &str,
    revision: u64,
    target: &Path,
    bytes: &[u8],
) -> Result<bool, ProductError> {
    let parent = target
        .parent()
        .ok_or_else(|| ProductError::Other("projection path has no parent".to_string()))?;
    fs::create_dir_all(parent).map_err(|error| {
        ProductError::Other(format!("create projection directory failed: {error}"))
    })?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| ProductError::Other(format!("create projection temp failed: {error}")))?;
    temporary
        .write_all(bytes)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| ProductError::Other(format!("write projection temp failed: {error}")))?;

    let latest = {
        let conn = db.conn()?;
        conn.query_row(
            "SELECT revision FROM plans WHERE id = ?1",
            params![plan_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| invalid(format!("unknown Plan: {plan_id}")))? as u64
    };
    if latest != revision {
        return Ok(false);
    }
    temporary.persist(target).map_err(|error| {
        ProductError::Other(format!("replace projection failed: {}", error.error))
    })?;
    Ok(true)
}
