//! Task, branch, queue and harness-selection services.
//!
//! The service owns: task creation, host-assigned message identity and
//! ordering, exactly-once queue acceptance keyed by durable operation
//! receipts, steer acceptance boundaries, harness pinning and idle-only
//! harness switching / branch creation. All mutations append journal events
//! through the [`JournalStore`] port so state survives restarts.

use crate::ports::{HarnessSession, JournalStore, ServiceError};
use crate::task::{Attempt, TaskContract, TaskExecution, TaskState, TransitionError};
use r_code_harness_protocol::{InputKind, InputMessage, OperationKey, PackageRef};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Errors from the task service.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TaskServiceError {
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error("task {0} not found")]
    UnknownTask(String),
    #[error("steer rejected: task {0} has no active run")]
    SteerWithoutRun(String),
    #[error("harness switch rejected: task {0} is not idle")]
    TaskNotIdle(String),
    #[error("attempt pins harness {pinned} but {provided} was given")]
    HarnessMismatch { pinned: String, provided: String },
    #[error("operation key {key} already used with different input")]
    ConflictingOperationKey { key: String },
    #[error("message {0} is not in flight")]
    NotInFlight(String),
}

/// In-memory delivery index for one task's input queue.
#[derive(Default)]
struct TaskQueue {
    last_seq: u64,
    pending: VecDeque<InputMessage>,
    in_flight: Option<InputMessage>,
    delivered: HashSet<String>,
}

impl TaskQueue {
    fn rebuild_from(events: &[crate::ports::JournalEvent]) -> Self {
        let mut queue = Self::default();
        for event in events {
            match event.kind.as_str() {
                "input.queued" => {
                    if let Ok(message) =
                        serde_json::from_value::<InputMessage>(event.payload.clone())
                    {
                        queue.last_seq = queue.last_seq.max(message.input_seq);
                        queue.pending.push_back(message);
                    }
                }
                "input.delivered" => {
                    if let Some(id) = event.payload.get("message_id").and_then(|v| v.as_str()) {
                        queue.delivered.insert(id.to_string());
                    }
                }
                _ => {}
            }
        }
        queue
            .pending
            .retain(|message| !queue.delivered.contains(&message.message_id));
        queue
    }
}

/// Coordinates tasks, queues, pins and branches over a journal store.
pub struct TaskService {
    store: Arc<dyn JournalStore + 'static>,
    index: Mutex<HashMap<String, TaskQueue>>,
    pins: Mutex<HashMap<String, PackageRef>>,
    parents: Mutex<HashMap<String, String>>,
}

impl TaskService {
    pub fn new(store: Arc<dyn JournalStore + 'static>) -> Self {
        Self {
            store,
            index: Mutex::new(HashMap::new()),
            pins: Mutex::new(HashMap::new()),
            parents: Mutex::new(HashMap::new()),
        }
    }

    fn event(task_id: &str, kind: &str, payload: serde_json::Value) -> crate::ports::JournalEvent {
        crate::ports::JournalEvent {
            seq: 0,
            task_id: task_id.to_string(),
            kind: kind.into(),
            payload,
        }
    }

    async fn save(
        &self,
        state: &TaskState,
        events: Vec<crate::ports::JournalEvent>,
    ) -> Result<(), ServiceError> {
        self.store.save_task_and_events(state, events).await
    }

    async fn load(&self, task_id: &str) -> Result<TaskState, TaskServiceError> {
        self.store
            .load_task(task_id)
            .await
            .ok_or_else(|| TaskServiceError::UnknownTask(task_id.to_string()))
    }

    /// Create a task and persist it with its first event.
    pub async fn create_task(&self, contract: TaskContract) -> Result<TaskState, TaskServiceError> {
        let task_id = contract.task_id.clone();
        let state = TaskState::new(contract);
        self.save(
            &state,
            vec![Self::event(
                &task_id,
                "task.created",
                serde_json::json!({"kind": state.contract.kind}),
            )],
        )
        .await?;
        Ok(state)
    }

    /// Pin the harness package used by the next run of this task.
    pub async fn pin_harness(
        &self,
        task_id: &str,
        package: PackageRef,
    ) -> Result<(), TaskServiceError> {
        let state = self.load(task_id).await?;
        self.pins
            .lock()
            .expect("pins")
            .insert(task_id.to_string(), package.clone());
        self.save(
            &state,
            vec![Self::event(
                task_id,
                "harness.pinned",
                serde_json::json!({
                    "id": package.id.0,
                    "version": package.version.to_string(),
                    "contentDigest": package.content_digest,
                }),
            )],
        )
        .await?;
        Ok(())
    }

    /// Switch the harness for future runs. Allowed only while the task is
    /// idle (pending or terminal): never mid-run.
    pub async fn switch_harness(
        &self,
        task_id: &str,
        package: PackageRef,
    ) -> Result<(), TaskServiceError> {
        let state = self.load(task_id).await?;
        match state.execution {
            TaskExecution::Pending | TaskExecution::Terminal { .. } => {}
            _ => return Err(TaskServiceError::TaskNotIdle(task_id.to_string())),
        }
        self.pin_harness(task_id, package).await
    }

    /// Create a new branch inheriting the canonical conversation (transcript
    /// cursor), contract and work units — never plugin-private state.
    pub async fn create_branch(
        &self,
        from_task_id: &str,
        new_task_id: &str,
    ) -> Result<TaskState, TaskServiceError> {
        let source = self.load(from_task_id).await?;
        if !matches!(
            source.execution,
            TaskExecution::Pending | TaskExecution::Terminal { .. }
        ) {
            return Err(TaskServiceError::TaskNotIdle(from_task_id.to_string()));
        }
        let mut contract = source.contract.clone();
        contract.task_id = new_task_id.to_string();
        let mut state = TaskState::new(contract);
        state
            .set_candidate_digest(source.candidate_digest.clone())
            .map_err(TaskServiceError::Transition)?;
        let mut branch = state;
        branch.work_units = source.work_units.clone();
        self.parents
            .lock()
            .expect("parents")
            .insert(new_task_id.to_string(), from_task_id.to_string());
        self.save(
            &branch,
            vec![Self::event(
                new_task_id,
                "branch.created",
                serde_json::json!({
                    "parent": from_task_id,
                    "inheritedWorkUnits": branch.work_units.len(),
                }),
            )],
        )
        .await?;
        Ok(branch)
    }

    /// Enqueue an input with exactly-once acceptance. Passing an operation
    /// key deduplicates retries: the same key with the same payload replays
    /// the original message; a different payload is refused.
    pub async fn enqueue(
        &self,
        task_id: &str,
        kind: InputKind,
        text: &str,
        operation_key: Option<OperationKey>,
    ) -> Result<InputMessage, TaskServiceError> {
        self.enqueue_as(task_id, kind, text, operation_key, None)
            .await
    }

    /// [`Self::enqueue`] with an audit actor: the input's `input.queued`
    /// journal event carries `actor` (device id for remote sends, the
    /// client id locally). Unknown payload fields are ignored by existing
    /// consumers (forward compatible).
    pub async fn enqueue_as(
        &self,
        task_id: &str,
        kind: InputKind,
        text: &str,
        operation_key: Option<OperationKey>,
        actor: Option<&str>,
    ) -> Result<InputMessage, TaskServiceError> {
        let state = self.load(task_id).await?;
        if let TaskExecution::Terminal { .. } = state.execution {
            return Err(TaskServiceError::Transition(
                TransitionError::AlreadyTerminal {
                    verdict: match &state.execution {
                        TaskExecution::Terminal { verdict } => verdict.clone(),
                        _ => unreachable!(),
                    },
                },
            ));
        }
        let scope = format!("queue:{task_id}");
        if let Some(key) = &operation_key {
            let payload = serde_json::json!({"kind": kind, "text": text});
            let hash = r_code_harness_protocol::canonical_input_hash(&payload);
            if let Some(receipt) = self.store.load_receipt(&scope, key).await {
                if receipt.input_hash != hash {
                    return Err(TaskServiceError::ConflictingOperationKey { key: key.0.clone() });
                }
                if let crate::task::ReceiptOutcome::Completed { result } = receipt.outcome {
                    if let Ok(message) = serde_json::from_value::<InputMessage>(result) {
                        return Ok(message);
                    }
                }
            }
            let message = self.allocate(task_id, kind, text).await?;
            let _ = actor;
            self.store
                .save_receipt(crate::task::OperationReceipt {
                    attempt_id: scope,
                    operation_key: key.clone(),
                    method: "queue.enqueue".into(),
                    input_hash: hash,
                    outcome: crate::task::ReceiptOutcome::Completed {
                        result: serde_json::to_value(&message).unwrap_or_default(),
                    },
                })
                .await?;
            self.persist_queued(&state, &message, actor).await?;
            return Ok(message);
        }
        let message = self.allocate(task_id, kind, text).await?;
        self.persist_queued(&state, &message, actor).await?;
        Ok(message)
    }

    async fn allocate(
        &self,
        task_id: &str,
        kind: InputKind,
        text: &str,
    ) -> Result<InputMessage, ServiceError> {
        let mut index = self.index.lock().expect("index");
        let queue = index.entry(task_id.to_string()).or_default();
        queue.last_seq += 1;
        Ok(InputMessage {
            message_id: format!("msg-{task_id}-{}", queue.last_seq),
            input_seq: queue.last_seq,
            kind,
            text: text.to_string(),
        })
    }

    async fn persist_queued(
        &self,
        state: &TaskState,
        message: &InputMessage,
        actor: Option<&str>,
    ) -> Result<(), TaskServiceError> {
        let mut payload = serde_json::to_value(message).unwrap_or_default();
        if let (Some(actor), Some(map)) = (actor, payload.as_object_mut()) {
            map.insert("actor".to_string(), serde_json::json!(actor));
        }
        self.save(
            state,
            vec![Self::event(
                &state.contract.task_id,
                "input.queued",
                payload,
            )],
        )
        .await?;
        self.index
            .lock()
            .expect("index")
            .entry(state.contract.task_id.clone())
            .or_default()
            .pending
            .push_back(message.clone());
        Ok(())
    }

    /// Steer acceptance boundaries: a steer requires an active run and is
    /// forwarded to the session immediately (bypassing queue order, but
    /// still persisted and marked delivered exactly once).
    pub async fn submit_steer(
        &self,
        task_id: &str,
        text: &str,
        session: &dyn HarnessSession,
    ) -> Result<InputMessage, TaskServiceError> {
        let state = self.load(task_id).await?;
        match state.execution {
            TaskExecution::Running { .. } | TaskExecution::WaitingInput { .. } => {}
            _ => return Err(TaskServiceError::SteerWithoutRun(task_id.to_string())),
        }
        let message = self.enqueue(task_id, InputKind::Steer, text, None).await?;
        // Pull this exact message out of the pending queue: a steer is not a
        // turn input and must not be re-delivered later.
        {
            let mut index = self.index.lock().expect("index");
            let queue = index.entry(task_id.to_string()).or_default();
            queue
                .pending
                .retain(|pending| pending.message_id != message.message_id);
            queue.delivered.insert(message.message_id.clone());
        }
        session.steer(&message).await?;
        self.save(
            &state,
            vec![Self::event(
                task_id,
                "input.delivered",
                serde_json::json!({"message_id": message.message_id}),
            )],
        )
        .await?;
        Ok(message)
    }

    /// Single-consumer delivery: returns the next undelivered message and
    /// marks it in flight. A second poll before acknowledgement gets nothing.
    pub async fn poll(&self, task_id: &str) -> Option<InputMessage> {
        let mut index = self.index.lock().expect("index");
        let queue = index.entry(task_id.to_string()).or_default();
        if queue.in_flight.is_some() {
            return None;
        }
        let next = queue.pending.pop_front()?;
        queue.in_flight = Some(next.clone());
        Some(next)
    }

    /// Acknowledge delivery of an in-flight message (durable).
    pub async fn acknowledge(
        &self,
        task_id: &str,
        message_id: &str,
    ) -> Result<(), TaskServiceError> {
        {
            let mut index = self.index.lock().expect("index");
            let queue = index.entry(task_id.to_string()).or_default();
            let matches = queue
                .in_flight
                .as_ref()
                .map(|message| message.message_id == message_id)
                .unwrap_or(false);
            if !matches {
                return Err(TaskServiceError::NotInFlight(message_id.to_string()));
            }
            queue.in_flight = None;
            queue.delivered.insert(message_id.to_string());
        }
        let state = self.load(task_id).await?;
        self.save(
            &state,
            vec![Self::event(
                task_id,
                "input.delivered",
                serde_json::json!({"message_id": message_id}),
            )],
        )
        .await?;
        Ok(())
    }

    /// Start a run: validates the attempt pins the selected harness and the
    /// current contract revision, then hands the first input to the session.
    pub async fn start_run(
        &self,
        attempt: &Attempt,
        first_input: &InputMessage,
        session: &dyn HarnessSession,
    ) -> Result<TaskState, TaskServiceError> {
        let mut state = self.load(&attempt.task_id).await?;
        let pinned = self
            .pins
            .lock()
            .expect("pins")
            .get(&attempt.task_id)
            .cloned()
            .ok_or_else(|| TaskServiceError::HarnessMismatch {
                pinned: "<none>".into(),
                provided: attempt.package.id.0.clone(),
            })?;
        if pinned.id != attempt.package.id
            || pinned.content_digest != attempt.package.content_digest
        {
            return Err(TaskServiceError::HarnessMismatch {
                pinned: pinned.id.0.clone(),
                provided: attempt.package.id.0.clone(),
            });
        }
        state.start_attempt(attempt)?;
        session.start(attempt, &state.contract, first_input).await?;
        self.save(
            &state,
            vec![Self::event(
                &attempt.task_id,
                "run.started",
                serde_json::json!({
                    "attempt": attempt.attempt_id,
                    "harness": attempt.package.id.0,
                    "packageDigest": attempt.package.content_digest,
                }),
            )],
        )
        .await?;
        Ok(state)
    }

    /// Rebuild the in-memory queue for a task after a restart.
    pub async fn reload(&self, task_id: &str) -> Result<usize, TaskServiceError> {
        self.load(task_id).await?;
        let mut all = Vec::new();
        let mut cursor = 0u64;
        loop {
            let batch = self.store.read_events(cursor, 500).await;
            if batch.is_empty() {
                break;
            }
            cursor = batch.last().expect("non-empty").seq;
            all.extend(batch.into_iter().filter(|event| event.task_id == task_id));
        }
        let queue = TaskQueue::rebuild_from(&all);
        let pending = queue.pending.len();
        self.index
            .lock()
            .expect("index")
            .insert(task_id.to_string(), queue);
        Ok(pending)
    }

    /// Rename a task's human-facing title (UI metadata only).
    pub async fn rename(&self, task_id: &str, title: &str) -> Result<(), TaskServiceError> {
        let mut state = self.load(task_id).await?;
        state.title = Some(title.to_string());
        self.save(
            &state,
            vec![Self::event(
                task_id,
                "task.renamed",
                serde_json::json!({"title": title}),
            )],
        )
        .await?;
        Ok(())
    }

    /// Update per-task harness preferences applied to future runs.
    pub async fn set_preferences(
        &self,
        task_id: &str,
        preferences: crate::task::TaskPreferences,
    ) -> Result<(), TaskServiceError> {
        let mut state = self.load(task_id).await?;
        state.preferences = preferences;
        self.save(
            &state,
            vec![Self::event(
                task_id,
                "task.preferences",
                serde_json::to_value(&state.preferences).unwrap_or_default(),
            )],
        )
        .await?;
        Ok(())
    }

    /// Parent linkage recorded by [`Self::create_branch`].
    pub fn branch_parent(&self, task_id: &str) -> Option<String> {
        self.parents.lock().expect("parents").get(task_id).cloned()
    }
}
