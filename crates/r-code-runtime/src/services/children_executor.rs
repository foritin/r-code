//! FR-8 step one (M1a-10/11): the daemon children executor.
//!
//! The parent run's router and host catalog tools share one [`ChildControls`]
//! handle backed by a command channel. The executor loop owns the queue
//! (spawns beyond the kernel concurrency gate park until a slot frees),
//! launches child runs THROUGH the existing run drive (real child TaskState,
//! independent PluginSession/router/transcript, slots, journal, cancel), and
//! monitors each child to terminal, arbitrating the report back into the
//! kernel supervisor. Waits block on the supervisor condvar — never poll the
//! model loop.
//!
//! Delegation context (FR-8.4 minimal slice): the child task carries the
//! parent's frozen memory handoff (same snapshot hash — FR-7 acceptance b)
//! and the parent workspace; the child's own tool surface is the read-only
//! planning set with `caller=subagent:<id>` audit, so the gateway gate
//! confines it to the read-only allowlist. The full assembly (git snapshot,
//! fork levels, three-part return payload) is M1b.

use crate::run_manager::RunManager;
use r_code_harness_protocol::services::{ChildReport, ChildrenSpawnRequest, PermissionCeiling};
use r_code_kernel::children::{ChildWait, ChildrenSupervisor};
use r_code_kernel::task::TaskKind;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Cap for the report summary carried back from a child.
const CHILD_SUMMARY_MAX_CHARS: usize = 2_000;
/// How often child monitors re-read the journal while waiting for terminal
/// (house pump cadence; the children_wait primitive itself is condvar-based
/// and never polls).
const CHILD_MONITOR_TICK_MS: u64 = 300;

/// Commands from the tool/RPC surface to the executor loop.
#[derive(Debug)]
pub enum ChildCommand {
    /// Reserve-then-start (or park) one spawn.
    Spawn {
        child_task_id: String,
        request: ChildrenSpawnRequest,
    },
    /// A slot may have freed (close) — drain the queue.
    SlotChanged,
    /// Cancel one child by protocol id.
    Cancel { child_task_id: String },
}

/// The shared control surface for host.children RPCs and the catalog tools.
pub struct ChildControls {
    pub supervisor: Arc<Mutex<ChildrenSupervisor>>,
    pub ceiling: PermissionCeiling,
    commands: mpsc::UnboundedSender<ChildCommand>,
}

impl ChildControls {
    /// Assemble controls from parts (production wiring and tests).
    pub fn new(
        supervisor: Arc<Mutex<ChildrenSupervisor>>,
        ceiling: PermissionCeiling,
        commands: mpsc::UnboundedSender<ChildCommand>,
    ) -> Self {
        Self {
            supervisor,
            ceiling,
            commands,
        }
    }

    /// Reserve an id and hand the spawn to the executor. Returns the
    /// protocol id immediately; the start may queue behind the concurrency
    /// gate (FR-8 acceptance c).
    pub fn request_spawn(&self, request: ChildrenSpawnRequest) -> Result<String, String> {
        if !r_code_kernel::children::ceiling_allows(self.ceiling, request.permissions) {
            return Err(format!(
                "child permission ceiling {:?} exceeds the parent ceiling {:?}",
                request.permissions, self.ceiling
            ));
        }
        let mut supervisor = self
            .supervisor
            .lock()
            .map_err(|_| "children supervisor poisoned".to_string())?;
        let id = supervisor.reserve_id();
        let result = self.commands.send(ChildCommand::Spawn {
            child_task_id: id.clone(),
            request,
        });
        match result {
            Ok(()) => Ok(id),
            Err(_) => Err("children executor is not running".into()),
        }
    }

    /// Notify the executor a slot changed (after close).
    pub fn notify_slot_changed(&self) {
        let _ = self.commands.send(ChildCommand::SlotChanged);
    }

    fn send(&self, command: ChildCommand) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "children executor is not running".to_string())
    }

    /// Cancel one child: supervisor state first (waiters wake), then the
    /// run itself through the normal cancel path.
    pub fn request_cancel(&self, child_task_id: &str) -> Result<(), String> {
        {
            let mut supervisor = self
                .supervisor
                .lock()
                .map_err(|_| "children supervisor poisoned".to_string())?;
            supervisor
                .cancel_child(child_task_id)
                .map_err(|error| error.to_string())?;
        }
        self.send(ChildCommand::Cancel {
            child_task_id: child_task_id.to_string(),
        })
    }
}

impl RunManager {
    /// Start the per-run children executor. Returns the shared controls for
    /// the router and the host catalog tools.
    pub(crate) fn start_children_executor(
        self: &Arc<Self>,
        parent_task_id: &str,
        parent_memory: Option<r_code_kernel::task::FrozenMemoryHandoff>,
        parent_workspace: Option<String>,
        ceiling: PermissionCeiling,
    ) -> Arc<ChildControls> {
        let supervisor = Arc::new(Mutex::new(ChildrenSupervisor::new()));
        let (sender, receiver) = mpsc::unbounded_channel();
        let controls = Arc::new(ChildControls::new(Arc::clone(&supervisor), ceiling, sender));
        let runner = Arc::downgrade(self);
        let parent_task_id = parent_task_id.to_string();
        tokio::spawn(async move {
            children_executor_loop(
                runner,
                supervisor,
                receiver,
                parent_task_id,
                parent_memory,
                parent_workspace,
                ceiling,
            )
            .await;
        });
        controls
    }
}

struct PendingSpawn {
    child_task_id: String,
    request: ChildrenSpawnRequest,
}

async fn children_executor_loop(
    runner: std::sync::Weak<RunManager>,
    supervisor: Arc<Mutex<ChildrenSupervisor>>,
    mut commands: mpsc::UnboundedReceiver<ChildCommand>,
    parent_task_id: String,
    parent_memory: Option<r_code_kernel::task::FrozenMemoryHandoff>,
    parent_workspace: Option<String>,
    ceiling: PermissionCeiling,
) {
    let mut queue: VecDeque<PendingSpawn> = VecDeque::new();
    // protocol child id → daemon task id
    let mut task_map: HashMap<String, String> = HashMap::new();

    while let Some(command) = commands.recv().await {
        let Some(runner) = runner.upgrade() else {
            break;
        };
        match command {
            ChildCommand::Spawn {
                child_task_id,
                request,
            } => {
                queue.push_back(PendingSpawn {
                    child_task_id,
                    request,
                });
                drain_queue(
                    &runner,
                    &supervisor,
                    &mut queue,
                    &mut task_map,
                    &parent_task_id,
                    &parent_memory,
                    &parent_workspace,
                    ceiling,
                )
                .await;
            }
            ChildCommand::SlotChanged => {
                drain_queue(
                    &runner,
                    &supervisor,
                    &mut queue,
                    &mut task_map,
                    &parent_task_id,
                    &parent_memory,
                    &parent_workspace,
                    ceiling,
                )
                .await;
            }
            ChildCommand::Cancel { child_task_id } => {
                if let Some(daemon_task_id) = task_map.get(&child_task_id) {
                    let daemon_task_id = daemon_task_id.clone();
                    let _ = runner.cancel(&daemon_task_id).await;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drain_queue(
    runner: &Arc<RunManager>,
    supervisor: &Arc<Mutex<ChildrenSupervisor>>,
    queue: &mut VecDeque<PendingSpawn>,
    task_map: &mut HashMap<String, String>,
    parent_task_id: &str,
    parent_memory: &Option<r_code_kernel::task::FrozenMemoryHandoff>,
    parent_workspace: &Option<String>,
    ceiling: PermissionCeiling,
) {
    loop {
        let open = supervisor
            .lock()
            .map(|guard| guard.open_count())
            .unwrap_or(usize::MAX);
        if open >= r_code_kernel::children::DEFAULT_MAX_LIVE {
            return;
        }
        let Some(pending) = queue.pop_front() else {
            return;
        };
        if let Err(error) = start_child(
            runner,
            supervisor,
            parent_task_id,
            parent_memory,
            parent_workspace,
            ceiling,
            pending,
            task_map,
        )
        .await
        {
            eprintln!("children executor failed to start child: {error}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_child(
    runner: &Arc<RunManager>,
    supervisor: &Arc<Mutex<ChildrenSupervisor>>,
    parent_task_id: &str,
    parent_memory: &Option<r_code_kernel::task::FrozenMemoryHandoff>,
    parent_workspace: &Option<String>,
    ceiling: PermissionCeiling,
    pending: PendingSpawn,
    task_map: &mut HashMap<String, String>,
) -> Result<(), String> {
    {
        let mut guard = supervisor
            .lock()
            .map_err(|_| "children supervisor poisoned".to_string())?;
        guard
            .activate(pending.child_task_id.clone(), ceiling, &pending.request)
            .map_err(|error| error.to_string())?;
    }

    // Daemon task id: parent-scoped so parallel parents never collide.
    let daemon_task_id = format!("{}-{}", parent_task_id, pending.child_task_id);
    let contract = r_code_kernel::task::TaskContract {
        task_id: daemon_task_id.clone(),
        kind: TaskKind::Conversation,
        objective: pending.request.objective.clone(),
        constraints: vec![],
        required_checks: vec![],
        // FR-7 acceptance b: the child inherits the parent's FROZEN memory
        // (same hash; never recomputed).
        memory: parent_memory.clone(),
        revision: 1,
    };
    runner
        .kernel_tasks
        .create_task(contract)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(workspace) = parent_workspace {
        runner
            .kernel_tasks
            .set_preferences(
                &daemon_task_id,
                r_code_kernel::task::TaskPreferences {
                    workspace_path: Some(workspace.clone()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    task_map.insert(pending.child_task_id.clone(), daemon_task_id.clone());

    // Drive the child run through the standard machinery (slots, freeze,
    // PluginSession, journal, cancellation). The child task carries no
    // children controls of its own — nesting is closed structurally.
    let runner_for_send = Arc::clone(runner);
    let send_task_id = daemon_task_id.clone();
    let objective = pending.request.objective.clone();
    let send_result = runner_for_send.send(&send_task_id, &objective).await;
    if let Err(error) = send_result {
        eprintln!("children executor failed to dispatch child run: {error}");
    }

    // Monitor to terminal, then arbitrate the report.
    let supervisor_for_monitor = Arc::clone(supervisor);
    let protocol_id = pending.child_task_id;
    let monitor_task = daemon_task_id.clone();
    let store = Arc::clone(&runner.store);
    tokio::spawn(async move {
        monitor_child(store, supervisor_for_monitor, &protocol_id, &monitor_task).await;
    });
    Ok(())
}

async fn monitor_child(
    store: Arc<r_code_store::v1::V1Store>,
    supervisor: Arc<Mutex<ChildrenSupervisor>>,
    protocol_id: &str,
    daemon_task_id: &str,
) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(CHILD_MONITOR_TICK_MS));
    loop {
        ticker.tick().await;
        let events = store.task_events(daemon_task_id);
        let terminal = events.iter().rev().find_map(|event| {
            let outcome = match event.kind.as_str() {
                "run.completed" => "completed",
                "run.failed" => "failed",
                "run.cancelled" => "cancelled",
                _ => return None,
            };
            if event.payload.get("runId").and_then(|v| v.as_str())
                != Some(format!("run-{daemon_task_id}-1").as_str())
                && !event
                    .payload
                    .get("runId")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| id.starts_with(&format!("run-{daemon_task_id}-")))
            {
                return None;
            }
            Some(outcome.to_string())
        });
        let Some(outcome) = terminal else {
            continue;
        };
        let summary = last_assistant_summary(&events);
        let report = ChildReport {
            child_task_id: protocol_id.to_string(),
            outcome,
            verified: vec![],
            inferred: vec![],
            unverifiable: vec![],
            summary,
        };
        if let Ok(mut guard) = supervisor.lock() {
            let _ = guard.complete(protocol_id, report);
        }
        return;
    }
}

fn last_assistant_summary(events: &[r_code_kernel::ports::JournalEvent]) -> Option<String> {
    let text = events
        .iter()
        .rev()
        .find(|event| event.kind == "assistant.message")
        .and_then(|event| {
            event
                .payload
                .get("text")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })?;
    let summary: String = text.chars().take(CHILD_SUMMARY_MAX_CHARS).collect();
    Some(summary)
}

/// Blocking wait for one child (condvar-based; used by the router's
/// host.children.wait and the catalog tool through spawn_blocking).
pub fn wait_child_blocking(
    supervisor: &Arc<Mutex<ChildrenSupervisor>>,
    child_task_id: &str,
    timeout_ms: u64,
) -> Result<ChildWait, String> {
    r_code_kernel::children::wait_child(
        supervisor,
        child_task_id,
        std::time::Duration::from_millis(timeout_ms),
    )
    .map_err(|error| error.to_string())
}
