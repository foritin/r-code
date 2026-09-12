//! The conversation run manager: chat-grade task execution over the plugin
//! protocol.
//!
//! One plugin process per run (the protocol's designed topology); multi-turn
//! conversations resume from the latest checkpoint with a stable attempt id,
//! so operation-key receipts and checkpoints survive across runs. Runs are
//! asynchronous — `send` enqueues input and returns a run id immediately; a
//! drive loop per task dispatches queued inputs run after run (queue
//! semantics: a message sent while a run is active dispatches when the run
//! ends). Everything the frontend needs to render — assistant turns, tool
//! calls, usage, run lifecycle — lands in the durable journal as it happens.

use crate::plugins::catalog::Availability;
use crate::plugins::transport::PluginProcess;
use crate::plugins::{ApprovalStore, HostRouter, PluginCatalog, PluginSession, TransportLimits};
use crate::services::models::ProviderResolver as _;
use crate::services::settings_store::SettingsStore;
use r_code_harness_protocol::{
    EventEnvelope, EventKind, InputKind, InputMessage, NegotiatedCapabilities, PackageRef,
    Provenance, RunIdentity,
};
use r_code_kernel::ports::{HarnessSession as _, JournalStore as _, RunGuard};
use r_code_kernel::task::{
    Actor, CompletionProposal, TaskContract, TaskExecution, TaskState, TaskVerdict,
};
use r_code_kernel::tasks::TaskService as KernelTaskService;
use r_code_store::v2::V2Store;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

/// The built-in native harness id auto-pinned for fresh tasks.
pub const DEFAULT_HARNESS_ID: &str = "native.r-code";

/// Errors surfaced to daemon callers.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RunError {
    #[error("task {0} not found")]
    UnknownTask(String),
    #[error("{0}")]
    Failure(String),
}

/// Per-task drive state: the live run handles plus the dispatch loop token.
/// All mutations hold the slot mutex, which serializes send-vs-exit races.
#[derive(Default)]
struct RunSlot {
    /// Sticky owner task id (set on first run, never changed).
    task_id: Option<String>,
    run_id: Option<String>,
    guard: Option<Arc<RunGuard>>,
    process: Option<Arc<PluginProcess>>,
    stop_pump: Option<Arc<Notify>>,
    drive: Option<tokio::task::JoinHandle<()>>,
}

/// Chat engine over the composed services.
pub struct RunManager {
    store: Arc<V2Store>,
    kernel_tasks: Arc<KernelTaskService>,
    catalog: Arc<PluginCatalog>,
    models: Arc<dyn r_code_kernel::ports::ModelService>,
    tools: Arc<dyn r_code_kernel::ports::ToolService>,
    processes: Arc<dyn r_code_kernel::ports::ProcessService>,
    settings: Arc<SettingsStore>,
    approvals: Arc<ApprovalStore>,
    slots: Mutex<HashMap<String, Arc<Mutex<RunSlot>>>>,
}

impl RunManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<V2Store>,
        kernel_tasks: Arc<KernelTaskService>,
        catalog: Arc<PluginCatalog>,
        models: Arc<dyn r_code_kernel::ports::ModelService>,
        tools: Arc<dyn r_code_kernel::ports::ToolService>,
        processes: Arc<dyn r_code_kernel::ports::ProcessService>,
        settings: Arc<SettingsStore>,
        approvals: Arc<ApprovalStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            kernel_tasks,
            catalog,
            models,
            tools,
            processes,
            settings,
            approvals,
            slots: Mutex::new(HashMap::new()),
        })
    }

    async fn slot_of(&self, task_id: &str) -> Arc<Mutex<RunSlot>> {
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
        let state = self
            .store
            .load_task(task_id)
            .await
            .ok_or_else(|| RunError::UnknownTask(task_id.to_string()))?;
        // Follow-up inputs reopen a settled conversation before queueing.
        if !matches!(state.execution, TaskExecution::Running { .. }) {
            let mut state = state;
            let before = state.execution.clone();
            state
                .reopen_for_input()
                .map_err(|_| RunError::Failure("任务运行中，请等待当前回合结束或先中止".into()))?;
            if state.execution != before {
                self.store
                    .save_task_and_events(
                        &state,
                        vec![journal_event(
                            task_id,
                            "task.reopened",
                            serde_json::json!({}),
                        )],
                    )
                    .await
                    .map_err(|e| RunError::Failure(e.to_string()))?;
            }
        }

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

    /// Dispatch queued inputs run after run until the queue drains. Exit is
    /// race-free: the final poll happens under the slot lock, and `send`
    /// only skips spawning while a loop token is present.
    async fn drive_loop(self: Arc<Self>, slot: &Arc<Mutex<RunSlot>>) {
        loop {
            let Some((task_id, input)) = self.next_input(slot).await else {
                break;
            };
            let outcome = self.run_one(&task_id, &input).await;
            // Release the run registration on BOTH paths: a failed run must
            // not leave a stale guard/process behind (is_running would stay
            // true forever and cancel would target a dead run).
            self.release_slot(slot).await;
            if let Err(error) = outcome {
                self.record_failure(&task_id, &input, &error).await;
                break;
            }
        }
    }

    /// Clear the live-run handles of one slot (keep the sticky task id and
    /// the drive token — those belong to the loop, not the run).
    async fn release_slot(&self, slot: &Arc<Mutex<RunSlot>>) {
        let mut slot_guard = slot.lock().await;
        slot_guard.run_id = None;
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
        if input.is_none() {
            slot_guard.drive = None;
            slot_guard.run_id = None;
            slot_guard.guard = None;
            slot_guard.process = None;
            slot_guard.stop_pump = None;
        }
        input.map(|input| (task_id, input))
    }

    /// Journal a failed run and release the input (delivered to an engine
    /// that then failed; retrying is a new send).
    async fn record_failure(&self, task_id: &str, input: &InputMessage, error: &str) {
        let snapshot = self.store.load_task(task_id).await.unwrap_or_else(|| {
            TaskState::new(TaskContract {
                task_id: task_id.to_string(),
                kind: r_code_kernel::task::TaskKind::Conversation,
                objective: String::new(),
                constraints: vec![],
                required_checks: vec![],
                revision: 1,
            })
        });
        let mut snapshot = snapshot;
        // A failed run is not an active run: force the task back to Pending
        // (reopen alone rejects Running — the exact state a mid-run failure
        // leaves behind).
        let _ = snapshot.fail_attempt();
        let _ = self
            .store
            .save_task_and_events(
                &snapshot,
                vec![journal_event(
                    task_id,
                    "run.failed",
                    serde_json::json!({"error": error}),
                )],
            )
            .await;
        let _ = self
            .kernel_tasks
            .acknowledge(task_id, &input.message_id)
            .await;
    }

    /// Execute exactly one run for one input.
    async fn run_one(&self, task_id: &str, input: &InputMessage) -> Result<(), String> {
        let mut state = self
            .store
            .load_task(task_id)
            .await
            .ok_or_else(|| format!("task {task_id} not found"))?;
        // A settled conversation reopens for the next queued input (the
        // send path reopens too; this covers loop-dispatched follow-ups).
        state
            .reopen_for_input()
            .map_err(|e| format!("task {task_id} is still active: {e}"))?;

        // Resolve the pinned harness, auto-pinning the native built-in for
        // fresh tasks.
        let pinned_id = match self.pinned_harness_id(task_id).await {
            Some(id) => id,
            None => {
                let effective = self
                    .catalog
                    .effective_package(DEFAULT_HARNESS_ID)
                    .map_err(|e| format!("内置 Harness 不可用：{e}"))?;
                self.kernel_tasks
                    .pin_harness(task_id, effective)
                    .await
                    .map_err(|e| e.to_string())?;
                DEFAULT_HARNESS_ID.to_string()
            }
        };
        let package: PackageRef = self
            .catalog
            .effective_package(&pinned_id)
            .map_err(|e| format!("Harness {pinned_id} 不可用：{e}"))?;
        let entry = self
            .catalog
            .list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|entry| {
                entry.package_ref.content_digest == package.content_digest
                    && entry.availability == Availability::Available
            })
            .ok_or_else(|| format!("Harness {pinned_id} 未安装或不可用"))?;

        let run_number = self.count_runs(task_id).await + 1;
        let identity = RunIdentity {
            task_id: task_id.to_string(),
            branch_id: format!("branch-{task_id}"),
            run_id: format!("run-{task_id}-{run_number}"),
            attempt_id: format!("attempt-{task_id}-1"),
            generation: 1,
        };
        let guard = RunGuard::new(&identity.run_id, 1);
        let attempt = r_code_kernel::task::Attempt {
            attempt_id: identity.attempt_id.clone(),
            task_id: task_id.to_string(),
            branch_id: identity.branch_id.clone(),
            package: package.clone(),
            contract_revision: state.contract.revision,
            config_hash: "daemon-v2".into(),
            workspace_identity: "chat".into(),
            run_id: identity.run_id.clone(),
        };

        // State transition + run.started BEFORE the plugin runs: frontends
        // see the run open in real time.
        state.start_attempt(&attempt).map_err(|e| e.to_string())?;
        self.store
            .save_task_and_events(
                &state,
                vec![journal_event(
                    task_id,
                    "run.started",
                    serde_json::json!({
                        "runId": identity.run_id,
                        "attemptId": identity.attempt_id,
                        "harness": package.id.0,
                        "packageDigest": package.content_digest,
                    }),
                )],
            )
            .await
            .map_err(|e| e.to_string())?;

        // Per-task preferences ride the harness config into the plugin.
        let preferences = state.preferences.clone();
        let harness_config = serde_json::json!({
            "modelSelection": preferences.model,
            "inference": preferences.inference,
            "taskMode": preferences.mode,
            "defaultModelSelection": self.settings.registry().default_selection(),
        });

        let grants = entry.manifest.requested_host_services.clone();
        let router = Arc::new(
            HostRouter::new(
                identity.clone(),
                guard.clone(),
                grants.clone(),
                self.tools.clone(),
                self.models.clone(),
                self.processes.clone(),
                self.store.clone(),
                Arc::new(crate::plugins::IgnoreQuestions),
            )
            .with_approvals(self.approvals.clone()),
        );
        let platform = entry
            .manifest
            .supported_platforms
            .first()
            .ok_or("manifest has no platform entry")?;
        let session = PluginSession::start(
            &entry.install_dir.join(&platform.executable),
            &platform.argv,
            identity.clone(),
            NegotiatedCapabilities {
                plugin_api: r_code_harness_protocol::ApiVersion::new(
                    entry.manifest.api_major,
                    entry.manifest.api_minor,
                ),
                host_api: crate::plugins::HOST_API,
                granted_services: grants,
            },
            guard.clone(),
            router.clone(),
            harness_config,
            TransportLimits::default(),
        )
        .await
        .map_err(|e| format!("启动 Harness 进程失败：{e}"))?;

        // Register the run for cancellation. The slot's task id is sticky
        // (one slot per task; set once, never changed).
        let slot = self.slot_of(task_id).await;
        let stop_pump = Arc::new(Notify::new());
        {
            let mut slot_guard = slot.lock().await;
            slot_guard.run_id = Some(identity.run_id.clone());
            slot_guard.guard = Some(guard.clone());
            slot_guard.process = Some(session.process().clone());
            slot_guard.stop_pump = Some(stop_pump.clone());
        }

        // Observation pump: persist host observations + plugin progress
        // notifications while the run executes.
        let shared_state = Arc::new(Mutex::new(state.clone()));
        let stop_pump_for_join = stop_pump.clone();
        let pump = {
            let router = router.clone();
            let store = self.store.clone();
            let shared_state = shared_state.clone();
            let task_id = task_id.to_string();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_millis(250));
                loop {
                    tokio::select! {
                        _ = ticker.tick() => {}
                        _ = stop_pump.notified() => {
                            // Settle drain: plugin notifications can land in
                            // the router microseconds after the run's own
                            // response resolved the caller. Keep draining
                            // until a full tick passes with nothing new —
                            // stopping on the first notification would race
                            // the reader task and drop events.
                            loop {
                                tokio::time::sleep(Duration::from_millis(250)).await;
                                let observations = drain_observations(&router);
                                if observations.is_empty() {
                                    break;
                                }
                                let snapshot = shared_state.lock().await.clone();
                                let _ = store.save_task_and_events(&snapshot, observations).await;
                            }
                            break;
                        }
                    }
                    let observations = drain_observations(&router);
                    if observations.is_empty() {
                        continue;
                    }
                    let snapshot = shared_state.lock().await.clone();
                    let _ = store.save_task_and_events(&snapshot, observations).await;
                    let _ = task_id;
                }
            })
        };

        // First run seeds the conversation from the message; later runs
        // resume from the latest checkpoint with the new input replayed.
        let checkpoint = self.store.load_latest_checkpoint(&attempt.attempt_id).await;
        let outcome = if run_number == 1 || checkpoint.is_none() {
            let mut start_contract = state.contract.clone();
            start_contract.objective = input.text.clone();
            session
                .start(&attempt, &start_contract, input)
                .await
                .map_err(|e| e.to_string())
        } else {
            let checkpoint = match checkpoint {
                Some(record) => record,
                None => unreachable!("resume branch checked checkpoint above"),
            };
            session
                .resume(&attempt, &checkpoint.artifact, std::slice::from_ref(input))
                .await
                .map_err(|e| e.to_string())
        };

        // Stop the pump and flush remaining observations.
        stop_pump_for_join.notify_waiters();
        let _ = pump.await;
        let mut state = shared_state.lock().await.clone();
        let final_observations = drain_observations(&router);
        if !final_observations.is_empty() {
            self.store
                .save_task_and_events(&state, final_observations)
                .await
                .map_err(|e| e.to_string())?;
        }

        // Terminal bookkeeping: cancelled runs settle as cancelled; normal
        // runs arbitrate the recorded completion proposal.
        let cancelled = guard.is_cancelled();
        let mut events = Vec::new();
        if cancelled {
            state
                .cancel(Actor::Host, 1, "user requested cancel")
                .map_err(|e| e.to_string())?;
            events.push(journal_event(
                task_id,
                "run.cancelled",
                serde_json::json!({
                    "runId": identity.run_id,
                    "reason": "user requested cancel",
                }),
            ));
        } else {
            outcome?;
            let proposal = router
                .recorded_proposals
                .lock()
                .expect("proposals")
                .pop()
                .map(|request| CompletionProposal {
                    actor: Actor::Plugin,
                    kind: request.kind,
                    summary: request.summary,
                    candidate_digest: request.candidate_digest,
                });
            let mut verdict = TaskVerdict::Unverified {
                reason: "no completion proposal recorded".into(),
            };
            if let Some(proposal) = proposal {
                match state.apply_proposal(1, &proposal) {
                    Ok(r_code_kernel::task::ProposalDecision::Accept { verdict: v }) => {
                        verdict = v;
                    }
                    Ok(r_code_kernel::task::ProposalDecision::Reject { reason }) => {
                        verdict = TaskVerdict::Unverified { reason };
                    }
                    Ok(r_code_kernel::task::ProposalDecision::Repair { feedback }) => {
                        verdict = TaskVerdict::Unverified { reason: feedback };
                    }
                    Err(error) => {
                        let _ = state.reopen_for_input();
                        self.store
                            .save_task_and_events(&state, Vec::new())
                            .await
                            .ok();
                        return Err(format!("completion arbitration failed: {error}"));
                    }
                }
            } else {
                let _ = state.reopen_for_input();
            }
            events.push(journal_event(
                task_id,
                "run.completed",
                serde_json::json!({
                    "runId": identity.run_id,
                    "verdict": verdict_label(&verdict),
                }),
            ));
        }
        self.store
            .save_task_and_events(&state, events)
            .await
            .map_err(|e| e.to_string())?;
        self.kernel_tasks
            .acknowledge(task_id, &input.message_id)
            .await
            .map_err(|e| e.to_string())?;

        // Per-run plugin process: always torn down after the run.
        let _ = session.process().kill().await;
        Ok(())
    }

    // -- cancel ------------------------------------------------------------

    /// Cancel the active run of a task (no-op when idle).
    pub async fn cancel(&self, task_id: &str) -> Result<bool, RunError> {
        self.store
            .load_task(task_id)
            .await
            .ok_or_else(|| RunError::UnknownTask(task_id.to_string()))?;
        let slot = {
            let slots = self.slots.lock().await;
            slots.get(task_id).cloned()
        };
        let Some(slot) = slot else {
            return Ok(false);
        };
        // The run registers its guard after spawning the plugin process;
        // a cancel racing run startup waits briefly for the registration.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (guard, process) = loop {
            let (guard, process) = {
                let slot_guard = slot.lock().await;
                (slot_guard.guard.clone(), slot_guard.process.clone())
            };
            if guard.is_some() || std::time::Instant::now() >= deadline {
                break (guard, process);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let Some(guard) = guard else {
            return Ok(false);
        };
        guard.revoke();
        if let Some(process) = process {
            let _ = process.cancel("user requested cancel").await;
        }
        Ok(true)
    }

    // -- queries -----------------------------------------------------------

    /// Whether a run is currently active for the task.
    pub async fn is_running(&self, task_id: &str) -> bool {
        let slots = self.slots.lock().await;
        match slots.get(task_id) {
            Some(slot) => slot.lock().await.run_id.is_some(),
            None => false,
        }
    }

    async fn count_runs(&self, task_id: &str) -> u64 {
        self.store
            .task_events(task_id)
            .into_iter()
            .filter(|event| event.kind == "run.started")
            .count() as u64
    }

    async fn pinned_harness_id(&self, task_id: &str) -> Option<String> {
        self.store
            .task_events(task_id)
            .into_iter()
            .rev()
            .find(|event| event.kind == "harness.pinned")
            .and_then(|event| {
                event
                    .payload
                    .get("id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
    }
}

/// Drain both observation buffers into journal events.
fn drain_observations(router: &Arc<HostRouter>) -> Vec<r_code_kernel::ports::JournalEvent> {
    let mut events = Vec::new();
    {
        let mut observations = router.host_observations.lock().expect("observations");
        for (kind, payload) in observations.drain(..) {
            events.push(journal_event(&router.identity.task_id, &kind, payload));
        }
    }
    {
        let mut notifications = router.observed_events.lock().expect("events");
        for notification in notifications.drain(..) {
            if notification.method == "harness.event" {
                if let Some(params) = &notification.params {
                    events.push(journal_event(
                        &router.identity.task_id,
                        "harness.progress",
                        params.clone(),
                    ));
                }
            }
        }
    }
    events
}

fn journal_event(
    task_id: &str,
    kind: &str,
    payload: serde_json::Value,
) -> r_code_kernel::ports::JournalEvent {
    r_code_kernel::ports::JournalEvent {
        seq: 0,
        task_id: task_id.to_string(),
        kind: kind.to_string(),
        payload,
    }
}

fn verdict_label(verdict: &TaskVerdict) -> &'static str {
    match verdict {
        TaskVerdict::Verified { .. } => "verified",
        TaskVerdict::Unverified { .. } => "unverified",
        TaskVerdict::Blocked { .. } => "blocked",
        TaskVerdict::Failed { .. } => "failed",
        TaskVerdict::Cancelled { .. } => "cancelled",
    }
}

/// Map journal rows to wire envelopes for `events_after` / `task.events`.
pub fn envelope_of(event: r_code_kernel::ports::JournalEvent) -> EventEnvelope {
    let run_id = event
        .payload
        .get("runId")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("run-{}", event.task_id));
    let kind = match event.kind.as_str() {
        "task.created" | "harness.pinned" | "task.renamed" | "task.preferences"
        | "task.reopened" | "run.started" | "run.completed" | "run.failed" | "run.cancelled"
        | "branch.created" => EventKind::RunState,
        "assistant.message" => EventKind::ModelStream,
        "tool.call" => EventKind::ToolStarted,
        "tool.result" => EventKind::ToolFinished,
        "approval.requested" | "approval.decided" => EventKind::ApprovalRaised,
        _ => EventKind::Progress,
    };
    let payload = {
        let mut payload = event.payload;
        if let Some(map) = payload.as_object_mut() {
            map.insert("journalKind".to_string(), serde_json::json!(event.kind));
        }
        payload
    };
    EventEnvelope {
        seq: event.seq,
        task_id: event.task_id,
        run_id,
        kind,
        source: Provenance::Host,
        payload,
    }
}
