//! Per-task drive loop and queued-input dispatch, extracted O00.

use crate::plugins::transport::PluginProcess;
use crate::run_manager::{journal_event, RunError, RunManager, TASK_CAS_RETRIES};
use r_code_harness_protocol::{InputKind, InputMessage};
use r_code_kernel::ports::{JournalStore as _, RunGuard};
use r_code_kernel::task::{Actor, TaskExecution};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// Per-task drive state: the live run handles plus the dispatch loop token.
/// All mutations hold the slot mutex, which serializes send-vs-exit races.
#[derive(Default)]
pub(crate) struct RunSlot {
    /// Sticky owner task id (set on first run, never changed).
    pub(crate) task_id: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) attempt_id: Option<String>,
    pub(crate) input_message_id: Option<String>,
    pub(crate) transcript_position: Option<u64>,
    pub(crate) guard: Option<Arc<RunGuard>>,
    pub(crate) process: Option<Arc<PluginProcess>>,
    pub(crate) stop_pump: Option<Arc<Notify>>,
    pub(crate) drive: Option<tokio::task::JoinHandle<()>>,
}

impl RunManager {
    pub(crate) async fn slot_of(&self, task_id: &str) -> Arc<Mutex<RunSlot>> {
        let mut slots = self.slots.lock().await;
        slots
            .entry(task_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(RunSlot::default())))
            .clone()
    }

    // -- send / queue ------------------------------------------------------

    /// Enqueue a user message and (re)start the drive loop. Returns the run
    /// id that will carry the message, or `queued: true` when a run is
    /// already active for the task (the loop dispatches it next).
    pub async fn send(
        self: Arc<Self>,
        task_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, RunError> {
        self.send_as(task_id, text, None).await
    }

    /// [`Self::send`] with an audit actor (device id / client id) stamped
    /// into the `input.queued` journal event (R10).
    pub async fn send_as(
        self: Arc<Self>,
        task_id: &str,
        text: &str,
        actor: Option<&str>,
    ) -> Result<serde_json::Value, RunError> {
        self.prepare_for_new_input(task_id).await?;

        self.kernel_tasks
            .enqueue_as(task_id, InputKind::User, text, None, actor)
            .await
            .map_err(|e| RunError::Failure(e.to_string()))?;

        let slot = self.slot_of(task_id).await;
        let mut slot_guard = slot.lock().await;
        slot_guard.task_id = Some(task_id.to_string());
        // Reap a finished loop token so a fresh send restarts dispatch.
        if slot_guard
            .drive
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            slot_guard.drive = None;
        }
        if slot_guard.drive.is_some() {
            return Ok(serde_json::json!({"queued": true, "task": task_id}));
        }
        let next_run = self.count_runs(task_id).await + 1;
        let manager = self.clone();
        let slot_for_loop = slot.clone();
        slot_guard.drive = Some(tokio::spawn(async move {
            manager.drive_loop(&slot_for_loop).await;
        }));
        Ok(serde_json::json!({
            "started": true,
            "task": task_id,
            "runId": format!("run-{task_id}-{next_run}"),
        }))
    }

    pub(crate) async fn prepare_for_new_input(&self, task_id: &str) -> Result<(), RunError> {
        // E10 (adjudicated placement): chain reconciliation runs ONCE at
        // startup — after effect recovery, before this service accepts any
        // write ingress — so every drive loop this manager spawns dispatches
        // over already-reconciled state. Re-running it per input here would
        // quarantine a LIVE wave's in-flight attempts (the m03 competing
        // managers race pins exactly that), so the loop consumes the
        // outcome instead of recomputing it.
        for _ in 0..TASK_CAS_RETRIES {
            let (mut state, revision) = self
                .store
                .load_task_with_revision(task_id)
                .map_err(|error| RunError::Failure(error.to_string()))?
                .ok_or_else(|| RunError::UnknownTask(task_id.to_string()))?;
            match &state.execution {
                TaskExecution::Running { .. } => return Ok(()),
                TaskExecution::Ready { .. } => {
                    self.approved_execution_selection(&state)?;
                    return Ok(());
                }
                TaskExecution::AwaitingPlanApproval { .. } => {
                    state
                        .invalidate_plan(Actor::Host)
                        .map_err(|error| RunError::Failure(error.to_string()))?;
                    let result = self.store.save_task_and_invalidate_plan_if_revision(
                        &state,
                        vec![journal_event(
                            task_id,
                            "plan.invalidated",
                            serde_json::json!({"reason": "user-requested-revision"}),
                        )],
                        revision,
                        None,
                    );
                    match result {
                        Ok(_) => return Ok(()),
                        Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
                        Err(error) => return Err(RunError::Failure(error.to_string())),
                    }
                }
                TaskExecution::ReviewReady { .. }
                    if state.contract.kind.requires_code_evidence() =>
                {
                    return Err(RunError::Failure(
                        "review decision is required before more input".to_string(),
                    ));
                }
                TaskExecution::Verifying { .. } | TaskExecution::RepairRequired { .. } => {
                    return Err(RunError::Failure(
                        "task is waiting for verification or repair resolution".to_string(),
                    ));
                }
                _ => {}
            }
            let before = state.execution.clone();
            state
                .reopen_for_input()
                .map_err(|_| RunError::Failure("任务运行中，请等待当前回合结束或先中止".into()))?;
            if state.execution == before {
                return Ok(());
            }
            match self.store.save_task_and_events_if_revision(
                &state,
                vec![journal_event(
                    task_id,
                    "task.reopened",
                    serde_json::json!({}),
                )],
                revision,
            ) {
                Ok(_) => return Ok(()),
                Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
                Err(error) => return Err(RunError::Failure(error.to_string())),
            }
        }
        Err(RunError::Failure(format!(
            "task {task_id} stayed busy while preparing new input"
        )))
    }

    /// Dispatch queued inputs run after run until the queue drains. Exit is
    /// race-free: the final poll happens under the slot lock, and `send`
    /// only skips spawning while a loop token is present.
    async fn drive_loop(self: Arc<Self>, slot: &Arc<Mutex<RunSlot>>) {
        loop {
            let Some((task_id, input)) = self.next_input(slot).await else {
                break;
            };
            slot.lock().await.input_message_id = Some(input.message_id.clone());
            let outcome = self.run_one(&task_id, &input).await;
            if let Err(error) = outcome {
                self.record_failure(&task_id, &input, &error, slot).await;
                self.release_slot(slot).await;
                // A09：失败后走既有 settled 判定继续派发队列剩余输入（失败
                // 输入已被 record_failure acknowledge 消费）；任何提前退出都
                // 必须在锁内清 drive token——send 在 run_one 已返回而任务尚未
                // 标记 finished 的窗口内会误判 loop 存活而滞留队列。
                if self.pause_dispatch_if_settled(&task_id, slot).await {
                    break;
                }
                continue;
            }
            // Release the run registration on both paths. Failure records
            // must inspect ownership before this local identity is cleared.
            self.release_slot(slot).await;
            if self.pause_dispatch_if_settled(&task_id, slot).await {
                break;
            }
        }
    }

    async fn pause_dispatch_if_settled(&self, task_id: &str, slot: &Arc<Mutex<RunSlot>>) -> bool {
        // Serialize the final durable queue check with `send`'s drive-handle
        // check so either the old drive continues or the sender spawns one.
        let mut slot_guard = slot.lock().await;
        let Some(state) = self.store.load_task(task_id).await else {
            slot_guard.drive = None;
            return true;
        };
        let has_pending = !r_code_store::v1::rebuild_queue(&self.store, task_id)
            .1
            .is_empty();
        let pause = match state.execution {
            TaskExecution::AwaitingPlanApproval { .. } | TaskExecution::Ready { .. } => {
                !has_pending
            }
            TaskExecution::Verifying { .. } | TaskExecution::RepairRequired { .. } => true,
            TaskExecution::ReviewReady { .. } => {
                state.contract.kind.requires_code_evidence() || !has_pending
            }
            TaskExecution::Terminal { .. } => !has_pending,
            TaskExecution::Pending
            | TaskExecution::Running { .. }
            | TaskExecution::WaitingInput { .. } => false,
        };
        if pause {
            slot_guard.drive = None;
        }
        pause
    }

    /// Clear the live-run handles of one slot (keep the sticky task id and
    /// the drive token — those belong to the loop, not the run).
    async fn release_slot(&self, slot: &Arc<Mutex<RunSlot>>) {
        let mut slot_guard = slot.lock().await;
        slot_guard.run_id = None;
        slot_guard.attempt_id = None;
        slot_guard.input_message_id = None;
        slot_guard.transcript_position = None;
        slot_guard.guard = None;
        slot_guard.process = None;
        slot_guard.stop_pump = None;
    }

    /// Poll the next input for the task that owns this slot. `None` (under
    /// the slot lock) also clears the loop token — the exit decision is
    /// serialized against `send`'s spawn check.
    async fn next_input(&self, slot: &Arc<Mutex<RunSlot>>) -> Option<(String, InputMessage)> {
        let mut slot_guard = slot.lock().await;
        let task_id = slot_guard.task_id.clone()?;
        let input = self.kernel_tasks.poll(&task_id).await;
        // A10：poll 空且本进程尚未重播种过——把 store 侧重建的未投递输入灌回
        // （G12：daemon 重启后内存队列起空，崩溃时滞留的输入此前永不派发）。
        if input.is_none() {
            let needs_reseed = !self.reseeded.lock().expect("reseeded").contains(&task_id);
            if needs_reseed {
                self.reseeded
                    .lock()
                    .expect("reseeded")
                    .insert(task_id.clone());
                let undelivered = r_code_store::v1::rebuild_queue(&self.store, &task_id).1;
                if !undelivered.is_empty() {
                    self.kernel_tasks.reseed(&task_id, undelivered).await;
                    drop(slot_guard);
                    let slot_guard = slot.lock().await;
                    let task_id = slot_guard.task_id.clone()?;
                    return self
                        .kernel_tasks
                        .poll(&task_id)
                        .await
                        .map(|input| (task_id, input));
                }
            }
        }
        if input.is_none() {
            slot_guard.drive = None;
            slot_guard.run_id = None;
            slot_guard.attempt_id = None;
            slot_guard.input_message_id = None;
            slot_guard.transcript_position = None;
            slot_guard.guard = None;
            slot_guard.process = None;
            slot_guard.stop_pump = None;
        }
        input.map(|input| (task_id, input))
    }
}
