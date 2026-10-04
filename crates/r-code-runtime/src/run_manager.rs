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
use crate::plugins::router::{supported_requested_services, RouterServiceAvailability};
use crate::plugins::{ApprovalStore, HostRouter, PluginCatalog, PluginSession, TransportLimits};
use crate::run_drive::RunSlot;
use crate::services::artifacts::{sha256_hex, ArtifactStore};
use crate::services::context::{ContextRegistry, TranscriptWriter};
use crate::services::run_snapshots::{
    planning_harness_config, resolve_workspace_snapshot, RunSnapshotBuilder,
};
use crate::services::settings_store::SettingsStore;
use crate::services::tools::{ExecutionToolService, PlanningToolService};
use crate::services::verification::{verification_dir_for, CheckStatus, VerificationRunner};
use crate::services::verification_inputs::FrozenControlStore;
use crate::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_harness_protocol::{
    EventEnvelope, EventKind, InputKind, InputMessage, NegotiatedCapabilities, PackageRef,
    Provenance, RunIdentity,
};
use r_code_kernel::ports::{HarnessSession as _, JournalStore as _, ModelService, RunGuard};
use r_code_kernel::task::{
    Actor, CompletionProposal, EvidenceRequirement, ModelRoute, PlanApprovalRef, TaskExecution,
    TaskKind, TaskState, TaskVerdict, WorkUnit, WorkUnitStatus,
};
use r_code_kernel::tasks::TaskService as KernelTaskService;
use r_code_store::v1::V1Store;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

/// E06: the bounded-concurrency WorkUnit dispatcher. Declared here (with an
/// explicit path) so the new logic lives in its own file while the module
/// tree — and therefore the file list — stays as declared.
#[path = "parallel.rs"]
mod parallel;

/// The built-in native harness id auto-pinned for fresh tasks.
pub const DEFAULT_HARNESS_ID: &str = "native.r-code";

/// Durable task-scoped service roots used by the run manager.
#[derive(Debug, Clone)]
pub struct RunServicePaths {
    transcripts_root: PathBuf,
    artifacts_root: PathBuf,
}

impl RunServicePaths {
    pub fn new(transcripts_root: PathBuf, artifacts_root: PathBuf) -> Self {
        Self {
            transcripts_root,
            artifacts_root,
        }
    }

    fn ephemeral() -> Self {
        let root = std::env::temp_dir().join(format!(
            "r-code-run-services-{}",
            uuid::Uuid::new_v4().simple()
        ));
        Self::new(root.join("transcripts"), root.join("artifacts"))
    }
}

/// Errors surfaced to daemon callers.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RunError {
    #[error("task {0} not found")]
    UnknownTask(String),
    #[error("{0}")]
    Failure(String),
}

pub(crate) struct ApprovedExecution {
    approval: PlanApprovalRef,
    plan: r_code_kernel::plans::PlanRevision,
    plan_units: Vec<WorkUnit>,
}

/// Chat engine over the composed services.
pub struct RunManager {
    // pub(crate) fields: run_drive's dispatch impl reads them (O00 code motion)
    pub(crate) store: Arc<V1Store>,
    pub(crate) kernel_tasks: Arc<KernelTaskService>,
    catalog: Arc<PluginCatalog>,
    injected_models: Arc<dyn ModelService>,
    allow_injected_model_fallback: bool,
    bootstrap_tools: Arc<dyn r_code_kernel::ports::ToolService>,
    processes: Arc<dyn r_code_kernel::ports::ProcessService>,
    settings: Arc<SettingsStore>,
    approvals: Arc<ApprovalStore>,
    service_paths: RunServicePaths,
    context_registry: ContextRegistry,
    transcripts: Mutex<HashMap<String, Arc<TranscriptWriter>>>,
    pub(crate) slots: Mutex<HashMap<String, Arc<Mutex<RunSlot>>>>,
    /// A10：重启重播种的每任务一次性护栏（防每轮 poll 空转时反复扫 journal）。
    pub(crate) reseeded: std::sync::Mutex<std::collections::HashSet<String>>,
    /// E08: the one registry owning every in-flight supervised tree; task
    /// cancel sweeps it with per-tree proofs.
    pub(crate) child_supervisor: Arc<crate::child_supervisor::ChildSupervisor>,
}

impl RunManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<V1Store>,
        kernel_tasks: Arc<KernelTaskService>,
        catalog: Arc<PluginCatalog>,
        models: Arc<dyn ModelService>,
        tools: Arc<dyn r_code_kernel::ports::ToolService>,
        processes: Arc<dyn r_code_kernel::ports::ProcessService>,
        settings: Arc<SettingsStore>,
        approvals: Arc<ApprovalStore>,
    ) -> Arc<Self> {
        Self::new_with_injected_model_fallback(
            store,
            kernel_tasks,
            catalog,
            models,
            tools,
            processes,
            settings,
            approvals,
            false,
        )
    }

    /// Compose a manager with an explicit policy for an unconfigured settings
    /// document. Development/test hosts may use the injected deterministic
    /// model; production hosts fail before `run.started`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_injected_model_fallback(
        store: Arc<V1Store>,
        kernel_tasks: Arc<KernelTaskService>,
        catalog: Arc<PluginCatalog>,
        models: Arc<dyn ModelService>,
        tools: Arc<dyn r_code_kernel::ports::ToolService>,
        processes: Arc<dyn r_code_kernel::ports::ProcessService>,
        settings: Arc<SettingsStore>,
        approvals: Arc<ApprovalStore>,
        allow_injected_model_fallback: bool,
    ) -> Arc<Self> {
        Self::new_with_injected_model_fallback_and_paths(
            store,
            kernel_tasks,
            catalog,
            models,
            tools,
            processes,
            settings,
            approvals,
            allow_injected_model_fallback,
            RunServicePaths::ephemeral(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_injected_model_fallback_and_paths(
        store: Arc<V1Store>,
        kernel_tasks: Arc<KernelTaskService>,
        catalog: Arc<PluginCatalog>,
        models: Arc<dyn ModelService>,
        tools: Arc<dyn r_code_kernel::ports::ToolService>,
        processes: Arc<dyn r_code_kernel::ports::ProcessService>,
        settings: Arc<SettingsStore>,
        approvals: Arc<ApprovalStore>,
        allow_injected_model_fallback: bool,
        service_paths: RunServicePaths,
    ) -> Arc<Self> {
        // P23: a harness never runs at a checked-out workspace, and the tree it
        // runs in belongs to the host, not to whatever component happens to own
        // this profile's data directory. A service-private directory is deleted
        // recursively when the service goes away, and a launch may not have its
        // working directory removed between freezing the plan and creating the
        // child — so the scope is named by the service root while the tree lives
        // under the host's own scratch root, one directory per service root.
        let harness_scratch_scope =
            sha256_hex(service_paths.transcripts_root.to_string_lossy().as_bytes());
        crate::plugins::transport::bind_harness_scratch_root(
            std::env::temp_dir()
                .join(crate::plugins::transport::HARNESS_SCRATCH_DIR)
                .join(&harness_scratch_scope[..16]),
        );
        Arc::new(Self {
            store,
            kernel_tasks,
            catalog,
            injected_models: models,
            allow_injected_model_fallback,
            bootstrap_tools: tools,
            processes,
            settings,
            approvals,
            service_paths,
            context_registry: ContextRegistry::new(),
            transcripts: Mutex::new(HashMap::new()),
            slots: Mutex::new(HashMap::new()),
            reseeded: std::sync::Mutex::new(std::collections::HashSet::new()),
            child_supervisor: crate::child_supervisor::ChildSupervisor::new(),
        })
    }

    /// P23: publish the platform safety verdict the run just evaluated, so the
    /// harness transport resolves the containment a launch may run under from the
    /// same gate the effect services derive from. The verdict is handed over
    /// unchanged — a NotActivated boot is never presented as an activation, and a
    /// launch that needs the report and does not have it is refused with the
    /// gate's own reason.
    fn bind_harness_activation(&self, gate: crate::services::sandbox::SafetyActivation) {
        crate::plugins::transport::bind_harness_activation(gate);
    }

    async fn transcript_for(&self, task_id: &str) -> Result<Arc<TranscriptWriter>, String> {
        let mut transcripts = self.transcripts.lock().await;
        if let Some(transcript) = transcripts.get(task_id) {
            return Ok(transcript.clone());
        }
        let key = sha256_hex(task_id.as_bytes());
        let path = self
            .service_paths
            .transcripts_root
            .join(format!("{key}.jsonl"));
        let transcript = Arc::new(
            self.context_registry
                .open_transcript(task_id, Some(path))
                .map_err(|_| "无法打开任务 transcript".to_string())?,
        );
        transcripts.insert(task_id.to_string(), transcript.clone());
        Ok(transcript)
    }

    fn artifacts_for(&self, task_id: &str) -> Arc<ArtifactStore> {
        let key = sha256_hex(task_id.as_bytes());
        Arc::new(ArtifactStore::for_task(
            self.service_paths.artifacts_root.join(key),
            task_id,
        ))
    }

    /// Build but do not dispatch the M-GATE execution capability. M03 calls
    /// this only after it has durably created an execution attempt.
    pub fn execution_tools_for(
        &self,
        task_id: &str,
        attempt_id: &str,
        unit: &WorkUnit,
        binding: TaskWorkspaceBinding,
    ) -> Result<Arc<ExecutionToolService>, RunError> {
        if unit.write_paths.is_empty() && !unit.repo_exclusive {
            return Err(RunError::Failure(
                "execution WorkUnit has no approved write scope".to_string(),
            ));
        }
        let approval = self
            .store
            .load_active_plan_approval(task_id)
            .map_err(|error| RunError::Failure(error.to_string()))?
            .ok_or_else(|| RunError::Failure("task has no active plan approval".to_string()))?;
        let plan = self
            .store
            .load_plan_revision(&approval.plan_revision)
            .map_err(|error| RunError::Failure(error.to_string()))?
            .ok_or_else(|| RunError::Failure("approved plan revision is missing".to_string()))?;
        if plan.material().task_id != task_id {
            return Err(RunError::Failure(
                "approved plan belongs to another task".to_string(),
            ));
        }
        let wire = plan
            .material()
            .work_units
            .iter()
            .find(|candidate| candidate.id == unit.id)
            .cloned()
            .ok_or_else(|| RunError::Failure("approved plan lost the WorkUnit".to_string()))?;
        let shell_surface = Self::resolve_shell_surface(
            &self.store,
            task_id,
            approval.plan_revision.as_str(),
            &wire,
        );
        let mut service = ExecutionToolService::new(
            &plan,
            unit,
            binding,
            self.store.clone(),
            self.artifacts_for(task_id),
            attempt_id,
        )
        .map_err(|error| RunError::Failure(error.to_string()))?;
        if let Some(authority) = shell_surface {
            service = service.with_shell_authority(authority);
        }
        Ok(Arc::new(service))
    }

    /// Validate the task/approval/plan triple for execution and project the
    /// plan's units with the kernel-owned statuses. E06: this no longer
    /// picks one unit — the bounded dispatcher in `parallel` computes the
    /// ready set — and there is no single-InProgress guard; it still refuses
    /// a plan with no dependency-ready writable unit at all (the pre-send
    /// gate's error vocabulary).
    /// The execution deny list: no grant may carry an effect service the
    /// current activation cannot back. Kept here (its pre-E06 home) so the
    /// safe-mode deny/assert vocabulary stays in one place.
    pub(crate) fn grants_contain_unavailable_effect_services(
        grants: &[r_code_harness_protocol::HostService],
    ) -> bool {
        grants.iter().any(|service| {
            matches!(
                service,
                r_code_harness_protocol::HostService::ProcessOpen
                    | r_code_harness_protocol::HostService::ProcessRead
                    | r_code_harness_protocol::HostService::ProcessWrite
                    | r_code_harness_protocol::HostService::ProcessClose
                    | r_code_harness_protocol::HostService::PlanPublish
                    | r_code_harness_protocol::HostService::PlanUpdate
                    | r_code_harness_protocol::HostService::ChildrenSpawn
                    | r_code_harness_protocol::HostService::ChildrenWait
                    | r_code_harness_protocol::HostService::ChildrenCancel
                    | r_code_harness_protocol::HostService::VerificationRun
            )
        })
    }

    pub(crate) fn approved_execution_selection(
        &self,
        state: &TaskState,
    ) -> Result<ApprovedExecution, RunError> {
        if !matches!(
            state.contract.kind,
            TaskKind::Implementation | TaskKind::Repair
        ) {
            return Err(RunError::Failure(
                "only Implementation/Repair tasks may trigger execution".to_string(),
            ));
        }
        let approval = match &state.execution {
            TaskExecution::Ready { approval } => approval.clone(),
            _ => {
                return Err(RunError::Failure(
                    "task is not ready for approved execution".to_string(),
                ))
            }
        };
        self.store
            .validate_active_plan_approval(&state.contract.task_id, &approval)
            .map_err(|error| RunError::Failure(error.to_string()))?;
        let plan = self
            .store
            .current_plan_revision(&state.contract.task_id)
            .map_err(|error| RunError::Failure(error.to_string()))?
            .ok_or_else(|| RunError::Failure("approved plan is missing".to_string()))?;
        if plan.reference() != &approval.plan_revision {
            return Err(RunError::Failure(
                "active approval does not match the current plan".to_string(),
            ));
        }
        let plan_units = plan
            .material()
            .work_units
            .iter()
            .map(|wire| WorkUnit {
                id: wire.id.clone(),
                description: wire.description.clone(),
                dependencies: wire.dependencies.clone(),
                acceptance: wire.acceptance.clone(),
                read_paths: wire.read_paths.clone(),
                write_paths: wire.write_paths.clone(),
                repo_exclusive: wire.repo_exclusive,
                ephemeral_roots: wire.ephemeral_roots.clone(),
                effect_class: wire.effect_class,
                network_ceiling: wire.network_ceiling,
                status: state
                    .work_units
                    .iter()
                    .find(|prior| prior.id == wire.id)
                    .map(|prior| prior.status)
                    .filter(|status| {
                        *status == WorkUnitStatus::Completed
                            || *status == WorkUnitStatus::InProgress
                            || *status == WorkUnitStatus::Blocked
                    })
                    .unwrap_or(WorkUnitStatus::Pending),
            })
            .collect::<Vec<_>>();
        if !plan_units.iter().any(|unit| {
            unit.status == WorkUnitStatus::Pending
                && (!unit.write_paths.is_empty() || unit.repo_exclusive)
                && unit.dependencies.iter().all(|dependency| {
                    plan_units.iter().any(|candidate| {
                        candidate.id == *dependency && candidate.status == WorkUnitStatus::Completed
                    })
                })
        }) {
            return Err(RunError::Failure(
                "approved plan has no dependency-ready writable WorkUnit".to_string(),
            ));
        }
        Ok(ApprovedExecution {
            approval,
            plan,
            plan_units,
        })
    }

    /// Settle only the attempt owned by this local runner. A CAS loser has no
    /// slot attempt and therefore cannot mutate or acknowledge the winner.
    /// Every branch journals the failure reason: clients (and the TUI's
    /// "unknown model selection" → /setup guidance) read `payload.error` off
    /// the run.failed event, so an empty reason degrades a real error into
    /// "unknown error" on screen.
    /// A11：接力指令固定模板（DEC-4：普通用户侧输入形态，非用户可控）。
    const RELAY_INSTRUCTION: &str = "继续，从上次停止处接着做。";

    /// A11：判定本 run 是否 budget_reached——找本 run 的 harness.progress
    /// {budgetReached: true} 信号，返回其 turns。事件本身不带 runId，
    /// 以最近的 run.completed 为界即圈定本 run 的 journal 段。
    fn budget_reached_this_run(store: &V1Store, task_id: &str) -> Option<u64> {
        store
            .task_events(task_id)
            .into_iter()
            .rev()
            .take_while(|event| {
                // 只看最近一段：遇到本任务的 run.completed 即止。
                event.kind != "run.completed"
            })
            .find_map(|event| {
                if event.kind != "harness.progress" {
                    return None;
                }
                let payload = event.payload.get("payload")?;
                let hit = payload.get("budgetReached")?.as_bool()?;
                hit.then(|| {
                    payload
                        .get("turns")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0)
                })
            })
    }

    /// A11：统计当前接力链已累计轮数——从 journal 末尾向前，累加 run.chained
    /// 的 chainedTurns 语义简化为各段 turns 之和，遇最近的用户输入排队为止
    /// （用户输入开启新链）。
    fn relay_chain_turns(store: &V1Store, task_id: &str) -> u64 {
        let mut total = 0u64;
        for event in store.task_events(task_id).into_iter().rev() {
            match event.kind.as_str() {
                "run.chained" => {
                    total += event
                        .payload
                        .get("chainedTurns")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                }
                "input.queued" => {
                    let is_user = event
                        .payload
                        .get("kind")
                        .and_then(|value| value.as_str())
                        .map(|kind| kind == "user")
                        .unwrap_or(false);
                    if is_user {
                        break;
                    }
                }
                _ => {}
            }
        }
        total
    }

    /// A05：run 失败的三态分级（随 run.failed journal 落盘，供队列恢复与
    /// TUI 失败卡片消费）。启发式关键词匹配，默认 deterministic（宁可保守
    /// 不重试）。
    fn classify_failure(error: &str) -> &'static str {
        let lower = error.to_ascii_lowercase();
        if lower.contains("cancelled") || lower.contains("canceled") {
            return "cancelled";
        }
        // 错误链外层固定带 transport failure 一类包装噪声，确定性关键词必须
        // 优先于瞬时关键词判定。
        const DETERMINISTIC: [&str; 7] = [
            "maximum context",
            "context length",
            "context window",
            "invalid api key",
            "unauthorized",
            "forbidden",
            "content policy",
        ];
        if DETERMINISTIC.iter().any(|needle| lower.contains(needle)) {
            return "deterministic";
        }
        const TRANSIENT: [&str; 8] = [
            "429",
            "rate limited",
            "503",
            "500",
            "timeout",
            "timed out",
            "connection",
            "transport",
        ];
        if TRANSIENT.iter().any(|needle| lower.contains(needle)) {
            return "transient";
        }
        "deterministic"
    }

    pub(crate) async fn record_failure(
        &self,
        task_id: &str,
        input: &InputMessage,
        error: &str,
        slot: &Arc<Mutex<RunSlot>>,
    ) {
        let local_attempt = slot.lock().await.attempt_id.clone();
        let Some(local_attempt) = local_attempt else {
            // A pre-dispatch failure (for example, an older durable lease)
            // may be audited and acknowledged only while this task is still
            // Ready. If another manager already owns Running, do nothing.
            let Ok(Some((snapshot, revision))) = self.store.load_task_with_revision(task_id) else {
                return;
            };
            if !matches!(
                snapshot.execution,
                TaskExecution::Pending | TaskExecution::Ready { .. }
            ) || self
                .store
                .save_task_and_events_if_revision(
                    &snapshot,
                    vec![journal_event(
                        task_id,
                        "run.failed",
                        serde_json::json!({"ownedAttempt": false, "error": error, "errorClass": Self::classify_failure(error)}),
                    )],
                    revision,
                )
                .is_err()
            {
                return;
            }
            let _ = self
                .kernel_tasks
                .acknowledge(task_id, &input.message_id)
                .await;
            return;
        };
        let Ok(Some((mut snapshot, revision))) = self.store.load_task_with_revision(task_id) else {
            return;
        };
        let active_owned = match &snapshot.execution {
            TaskExecution::Running { attempt_id, .. }
            | TaskExecution::WaitingInput { attempt_id, .. }
            | TaskExecution::Verifying { attempt_id, .. } => attempt_id == &local_attempt,
            _ => false,
        };
        let settled_owned = match &snapshot.execution {
            TaskExecution::RepairRequired { attempt_id, .. } => {
                attempt_id.as_deref() == Some(local_attempt.as_str())
            }
            TaskExecution::ReviewReady { attempt_id } => attempt_id == &local_attempt,
            _ => false,
        };
        if !active_owned && !settled_owned {
            return;
        }
        if active_owned {
            // E05: which unit is in verification derives from the per-unit
            // records, not a task-level payload field.
            let verifying_unit = snapshot.verifying_work_unit_id();
            let transitioned = match verifying_unit {
                Some(work_unit_id) => snapshot.require_repair(
                    Actor::Host,
                    Some(local_attempt.clone()),
                    Some(work_unit_id),
                    "owned verification attempt failed".to_string(),
                    false,
                ),
                _ => snapshot.fail_attempt(),
            };
            if transitioned.is_err()
                || self
                    .store
                    .save_task_and_events_if_revision(
                        &snapshot,
                        vec![journal_event(
                            task_id,
                            "run.failed",
                            serde_json::json!({"attemptId": local_attempt, "error": error, "errorClass": Self::classify_failure(error)}),
                        )],
                        revision,
                    )
                    .is_err()
            {
                return;
            }
        } else if self
            .store
            .save_task_and_events_if_revision(
                &snapshot,
                vec![journal_event(
                    task_id,
                    "run.failed",
                    serde_json::json!({"attemptId": local_attempt, "error": error, "errorClass": Self::classify_failure(error)}),
                )],
                revision,
            )
            .is_err()
        {
            return;
        }
        let _ = self
            .kernel_tasks
            .acknowledge(task_id, &input.message_id)
            .await;
    }

    async fn handoff_checkpoint(
        &self,
        task_id: &str,
        new_attempt_id: &str,
    ) -> Result<Option<r_code_harness_protocol::ArtifactRef>, String> {
        if let Some(existing) = self.store.load_latest_checkpoint(new_attempt_id).await {
            return Ok(Some(existing.artifact));
        }
        let previous_attempt = self
            .store
            .task_events(task_id)
            .into_iter()
            .rev()
            .find(|event| event.kind == "run.started")
            .and_then(|event| {
                event
                    .payload
                    .get("attemptId")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            });
        let Some(previous_attempt) = previous_attempt else {
            return Ok(None);
        };
        let Some(previous) = self.store.load_latest_checkpoint(&previous_attempt).await else {
            return Ok(None);
        };
        self.store
            .save_checkpoint(
                new_attempt_id,
                0,
                previous.state,
                previous.consumed_input_seq,
            )
            .await
            .map(Some)
            .map_err(|error| format!("转存上一回合 checkpoint 失败：{error}"))
    }

    /// Execute exactly one run for one input.
    pub(crate) async fn run_one(
        self: &Arc<Self>,
        task_id: &str,
        input: &InputMessage,
    ) -> Result<(), String> {
        self.prepare_for_new_input(task_id)
            .await
            .map_err(|error| error.to_string())?;
        let (mut state, _task_revision) = self
            .store
            .load_task_with_revision(task_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("task {task_id} not found"))?;
        if matches!(state.execution, TaskExecution::Ready { .. }) {
            return self.run_execution_wave(task_id, input, state).await;
        }
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
        let run_id = format!("run-{task_id}-{run_number}");
        let guard = RunGuard::new(&run_id, 1);
        let transcript = self.transcript_for(task_id).await?;
        let transcript_position = transcript.position();
        let artifacts = self.artifacts_for(task_id);
        let plan_publish_enabled = matches!(
            state.contract.kind,
            TaskKind::PlanDraft | TaskKind::Implementation | TaskKind::Repair
        );
        let next_plan_revision = if plan_publish_enabled {
            let current = self
                .store
                .current_plan_revision(task_id)
                .map_err(|error| format!("读取计划 head 失败：{error}"))?;
            Some(match current {
                Some(revision) => revision
                    .material()
                    .revision
                    .checked_add(1)
                    .ok_or("计划 revision 溢出")?,
                None => 1,
            })
        } else {
            None
        };
        let host_model_available = !matches!(
            state.preferences.model_route,
            Some(ModelRoute::HarnessManaged { .. })
        );
        let mut requested_services = entry.manifest.requested_host_services.clone();
        // Native packages installed before the P-GATE service was activated
        // may carry the earlier v1 manifest. Compatibility is granted only to
        // the exact digest persisted by the trusted built-in registration
        // path; same-id third-party packages remain strict intersections.
        let trusted_native_compat = plan_publish_enabled
            && package.id.0 == DEFAULT_HARNESS_ID
            && !requested_services.contains(&r_code_harness_protocol::HostService::PlanPublish)
            && self
                .catalog
                .is_builtin(&package)
                .map_err(|error| format!("无法验证内置 Harness 身份：{error}"))?;
        if trusted_native_compat {
            requested_services.push(r_code_harness_protocol::HostService::PlanPublish);
        }
        let sandbox_gate = {
            // P13: discovery of effect services is gated on the exact
            // current SafetyCapabilityReport. This wave no platform can
            // evaluate Activated, so the gate stays closed; a persistence
            // failure also falls through fail-closed.
            let boot = crate::process_guard::BootIdentity::current()
                .map_err(|error| format!("无法读取启动身份：{error}"))?;
            crate::services::sandbox::platform_activation_gate(&self.store, boot.as_str())
        };
        let sandbox_activated = matches!(
            sandbox_gate,
            crate::services::sandbox::SafetyActivation::Activated { .. }
        );
        self.bind_harness_activation(sandbox_gate.clone());
        let grants = supported_requested_services(
            &requested_services,
            RouterServiceAvailability {
                model_stream: host_model_available,
                tools: true,
                context: true,
                artifacts: true,
                plan_publish: plan_publish_enabled,
                questions: true,
                approvals: true,
                checkpoints: true,
                completion: true,
                // FR-8 (M1a-10): children.* live for the parent run; child
                // tasks themselves are denied structurally (no controls).
                children: !task_id.contains("-child-"),
                sandbox_activated,
            },
        );
        debug_assert!(
            !grants.iter().any(|service| matches!(
                service,
                r_code_harness_protocol::HostService::ProcessOpen
                    | r_code_harness_protocol::HostService::ProcessRead
                    | r_code_harness_protocol::HostService::ProcessWrite
                    | r_code_harness_protocol::HostService::ProcessClose
                    | r_code_harness_protocol::HostService::PlanUpdate
                    | r_code_harness_protocol::HostService::VerificationRun
            )) || sandbox_activated
        );
        if self.allow_injected_model_fallback {
            // Development composition historically injected a ToolService as
            // a freeze barrier. Observe its catalog before resolving the
            // immutable run inputs, but never expose or execute those tools;
            // the per-run PlanningToolService remains the sole capability.
            self.bootstrap_tools
                .list(guard.token())
                .await
                .map_err(|error| format!("无法读取开发工具目录：{error}"))?;
        }
        let workspace = resolve_workspace_snapshot(&state)?;
        let instruction_settings = crate::services::project_instructions::resolve_settings(
            self.store.as_ref(),
            &workspace.canonical_root,
        );
        // FR-8 (M1a-10): per-run children executor. Child tasks (id pattern
        // "{parent}-child-N") never get controls — nesting closes here.
        let child_controls = if task_id.contains("-child-") {
            None
        } else {
            Some(self.start_children_executor(
                task_id,
                state.contract.memory.clone(),
                state.preferences.workspace_path.clone(),
                r_code_harness_protocol::services::PermissionCeiling::Full,
            ))
        };
        // Child runs audit as subagent:<protocol-id> so the gateway's
        // subagent gate + allowlist apply (FR-8 acceptance b).
        let tool_caller = task_id
            .rsplit_once("-child-")
            .map(|(_, suffix)| format!("subagent:child-{suffix}"))
            .unwrap_or_else(|| "harness-plugin".to_string());
        // FR-1.5 (M1a-07): the shared JIT tracker — read tools report hit
        // directories, the model-stream projection drains instruction
        // blocks (model-visible only; unbound runs never report hits).
        let jit_tracker = Arc::new(std::sync::Mutex::new(
            crate::services::project_instructions::JitTracker::new(
                std::path::PathBuf::from(&workspace.canonical_root),
                instruction_settings.clone(),
            ),
        ));
        let planning_service = PlanningToolService::from_workspace(&workspace)
            .map_err(|error| format!("无法构造只读规划工具：{error}"))?
            .with_jit_tracker(jit_tracker.clone())
            .with_caller(tool_caller);
        let planning_service = match &child_controls {
            Some(controls) => planning_service.with_child_controls(Arc::clone(controls)),
            None => planning_service,
        };
        let run_tools: Arc<dyn r_code_kernel::ports::ToolService> = Arc::new(planning_service);
        let frozen = RunSnapshotBuilder::new(
            &self.settings,
            &self.injected_models,
            &run_tools,
            self.allow_injected_model_fallback,
        )
        .with_instruction_settings(instruction_settings)
        .with_global_instructions_path(
            crate::services::project_instructions::global_context_md_path(),
        )
        .freeze_with_workspace(&state, &package, &grants, guard.as_ref(), workspace)
        .await?;
        if let Ok(mut tracker) = jit_tracker.lock() {
            tracker.seed_from_frozen(&frozen.snapshot.material().instructions);
        }
        self.store
            .save_run_snapshot(&frozen.snapshot)
            .map_err(|error| format!("保存运行快照失败：{error}"))?;
        crate::services::run_snapshots::record_run_injections(
            self.store.as_ref(),
            &run_id,
            &state,
            &frozen.snapshot.material().instructions,
        );
        let snapshot_suffix = frozen
            .snapshot
            .id()
            .as_str()
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect::<String>();
        let identity = RunIdentity {
            task_id: task_id.to_string(),
            branch_id: format!("branch-{task_id}"),
            run_id,
            attempt_id: format!("attempt-{task_id}-{run_number}-{snapshot_suffix}"),
            generation: 1,
        };
        let attempt = r_code_kernel::task::Attempt::for_run_snapshot(
            identity.attempt_id.clone(),
            task_id,
            identity.branch_id.clone(),
            package.clone(),
            state.contract.revision,
            frozen
                .snapshot
                .material()
                .workspace
                .workspace_identity
                .clone(),
            identity.run_id.clone(),
            frozen.snapshot.id(),
        );
        self.catalog
            .pin(&attempt.attempt_id, task_id, &package)
            .map_err(|error| format!("固定 Harness 包失败：{error}"))?;
        let checkpoint = self
            .handoff_checkpoint(task_id, &attempt.attempt_id)
            .await?;

        state.start_attempt(&attempt).map_err(|e| e.to_string())?;
        let mut started_events = vec![journal_event(
            task_id,
            "run.started",
            serde_json::json!({
                "runId": identity.run_id,
                "attemptId": identity.attempt_id,
                "harness": package.id.0,
                "packageDigest": package.content_digest,
                "snapshotId": frozen.snapshot.id().as_str(),
            }),
        )];
        // FR-1.7: one low-noise session note — injected N project
        // instructions for this run (skipped/trimmed facts live in /context).
        let instructions = &frozen.snapshot.material().instructions;
        if !instructions.is_empty() {
            let injected = instructions
                .entries
                .iter()
                .filter(|entry| entry.status == "injected")
                .count();
            started_events.push(journal_event(
                task_id,
                "context.instructions",
                serde_json::json!({
                    "runId": identity.run_id,
                    "injected": injected,
                    "bytes": instructions.rendered.len(),
                    "digest": instructions.digest,
                }),
            ));
        }
        state = save_run_state(&self.store, &state, started_events)?;
        let slot = self.slot_of(task_id).await;
        {
            let mut slot_guard = slot.lock().await;
            slot_guard.run_id = Some(identity.run_id.clone());
            slot_guard.attempt_id = Some(identity.attempt_id.clone());
            slot_guard.transcript_position = Some(transcript_position);
            slot_guard.guard = Some(guard.clone());
        }

        let harness_config = planning_harness_config(
            &frozen.snapshot,
            if plan_publish_enabled { "plan" } else { "ask" },
            next_plan_revision,
        );

        let router = Arc::new(
            HostRouter::new(
                identity.clone(),
                guard.clone(),
                grants.clone(),
                run_tools,
                frozen.models,
                self.processes.clone(),
                self.store.clone(),
                Arc::new(crate::plugins::IgnoreQuestions),
            )
            .with_approvals(self.approvals.clone())
            .with_transcript(transcript.clone())
            .with_artifacts(artifacts)
            .with_v1_store(self.store.clone())
            .with_jit_tracker(jit_tracker)
            .with_children_controls(child_controls)
            .with_plan_publication(
                frozen.snapshot.clone(),
                state.contract.kind,
                state.contract.required_checks.clone(),
                plan_publish_enabled,
            ),
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
        let stop_pump = Arc::new(Notify::new());
        {
            let mut slot_guard = slot.lock().await;
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
                                save_run_state(&store, &snapshot, observations)?;
                            }
                            break;
                        }
                    }
                    let observations = drain_observations(&router);
                    if observations.is_empty() {
                        continue;
                    }
                    let snapshot = shared_state.lock().await.clone();
                    save_run_state(&store, &snapshot, observations)?;
                }
                Ok::<(), String>(())
            })
        };

        // A later run resumes only from the revision-zero handoff in its own
        // attempt namespace; receipts and checkpoints from the old attempt
        // are never queried by the live router.
        let outcome = if checkpoint.is_none() {
            let mut start_contract = state.contract.clone();
            start_contract.objective = input.text.clone();
            session
                .start(&attempt, &start_contract, input)
                .await
                .map_err(|e| e.to_string())
        } else {
            let checkpoint = match checkpoint {
                Some(artifact) => artifact,
                None => unreachable!("resume branch checked checkpoint above"),
            };
            session
                .resume(&attempt, &checkpoint, std::slice::from_ref(input))
                .await
                .map_err(|e| e.to_string())
        };

        // Stop the pump and flush remaining observations.
        stop_pump_for_join.notify_waiters();
        pump.await
            .map_err(|error| format!("observation pump join failed: {error}"))??;
        let mut state = shared_state.lock().await.clone();
        let final_observations = drain_observations(&router);
        if !final_observations.is_empty() {
            state = save_run_state(&self.store, &state, final_observations)?;
        }
        if let Some(latest) = self.store.load_task(task_id).await {
            if matches!(
                &latest.execution,
                TaskExecution::WaitingInput {
                    attempt_id,
                    generation,
                    ..
                } if attempt_id == &identity.attempt_id && *generation == identity.generation
            ) {
                state = latest;
            }
        }

        // Terminal bookkeeping: cancelled runs settle as cancelled; normal
        // runs arbitrate the recorded completion proposal.
        let cancelled = guard.is_cancelled();
        if cancelled {
            transcript
                .truncate_to(transcript_position)
                .map_err(|_| "取消运行后无法恢复任务 transcript".to_string())?;
        } else if let Err(error) = outcome {
            transcript
                .truncate_to(transcript_position)
                .map_err(|_| "失败运行后无法恢复任务 transcript".to_string())?;
            let _ = session.process().kill().await;
            return Err(error);
        }
        if matches!(state.execution, TaskExecution::WaitingInput { .. }) && !cancelled {
            save_run_state(
                &self.store,
                &state,
                vec![journal_event(
                    task_id,
                    "run.waiting_input",
                    serde_json::json!({"runId": identity.run_id}),
                )],
            )?;
            self.kernel_tasks
                .acknowledge(task_id, &input.message_id)
                .await
                .map_err(|e| e.to_string())?;
            let _ = session.process().kill().await;
            return Ok(());
        }
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
        } else if plan_publish_enabled {
            let proposals = router
                .recorded_proposals
                .lock()
                .expect("proposals")
                .drain(..)
                .collect::<Vec<_>>();
            let publications = router
                .recorded_plan_publications
                .lock()
                .expect("plan publications")
                .drain(..)
                .collect::<Vec<_>>();
            let valid_pair = proposals.len() == 1
                && proposals[0].kind == r_code_harness_protocol::services::ProposalKind::PlanDraft
                && publications.len() == 1;
            if !valid_pair {
                transcript
                    .truncate_to(transcript_position)
                    .map_err(|_| "规划发布失败后无法恢复任务 transcript".to_string())?;
                let _ = session.process().kill().await;
                return Err(
                    "planning run must publish exactly one plan before proposing PlanDraft"
                        .to_string(),
                );
            }
            let publication = &publications[0];
            let current = self
                .store
                .current_plan_revision(task_id)
                .map_err(|error| format!("读取已发布计划失败：{error}"))?
                .ok_or("planning proposal has no durable plan revision")?;
            let exact_publication = Some(publication.revision) == next_plan_revision
                && current.material().revision == publication.revision
                && current.reference().as_str() == publication.revision_hash;
            if !exact_publication {
                transcript
                    .truncate_to(transcript_position)
                    .map_err(|_| "计划 revision 不匹配后无法恢复任务 transcript".to_string())?;
                let _ = session.process().kill().await;
                return Err("planning proposal does not match the exact current plan head".into());
            }
            state
                .await_plan_approval(
                    Actor::Host,
                    &identity.attempt_id,
                    identity.generation,
                    current.reference().clone(),
                )
                .map_err(|error| format!("无法进入计划审批状态：{error}"))?;
            events.push(journal_event(
                task_id,
                "plan.awaiting-approval",
                serde_json::json!({
                    "runId": identity.run_id,
                    "revision": publication.revision,
                    "revisionHash": publication.revision_hash,
                }),
            ));
            events.push(journal_event(
                task_id,
                "run.completed",
                serde_json::json!({
                    "runId": identity.run_id,
                    "verdict": "awaiting-plan-approval",
                }),
            ));
        } else {
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
                        let _ = save_run_state(&self.store, &state, Vec::new());
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
        // A11：轮数预算接力——插件在 budget_reached 时发 harness.progress
        // {budgetReached, turns}（端口 start 返回 ()，结果 JSON 不上浮，以
        // journal 信号为准）；宿主注入 Continuation 续跑，drive loop 因队列
        // 非空自然继续，新 run 经 handoff checkpoint 恢复。
        let budget = Self::budget_reached_this_run(&self.store, task_id);
        if let Some(run_turns) = budget {
            let chained_turns = Self::relay_chain_turns(&self.store, task_id);
            // DEC-4：链总轮数护栏（翻转点：此常量→设置项）。
            const MAX_TOTAL_TURNS: u64 = 200;
            if chained_turns + run_turns >= MAX_TOTAL_TURNS {
                events.push(journal_event(
                    task_id,
                    "run.chain_stopped",
                    serde_json::json!({
                        "runId": identity.run_id,
                        "chainedTurns": chained_turns + run_turns,
                        "limit": MAX_TOTAL_TURNS,
                    }),
                ));
            } else {
                self.kernel_tasks
                    .enqueue(
                        task_id,
                        InputKind::Continuation,
                        Self::RELAY_INSTRUCTION,
                        None,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                events.push(journal_event(
                    task_id,
                    "run.chained",
                    serde_json::json!({
                        "runId": identity.run_id,
                        "chainedTurns": chained_turns + run_turns,
                        "instruction": Self::RELAY_INSTRUCTION,
                    }),
                ));
            }
        }
        save_run_state(&self.store, &state, events)?;
        self.kernel_tasks
            .acknowledge(task_id, &input.message_id)
            .await
            .map_err(|e| e.to_string())?;

        // Per-run plugin process: always torn down after the run.
        let _ = session.process().kill().await;
        Ok(())
    }

    /// P24B: the exact per-capability grant predicate. A capability is
    /// granted only when the platform report is Activated AND the
    /// capability's own effect fits: NoWorkspace and ScratchOnly never
    /// touch the checkout (activatable directly under the report);
    /// CurrentCheckoutWrite additionally requires the P27 envelope and is
    /// NOT granted by this predicate — its launch site owns that.
    pub fn capability_granted(
        activation: &crate::services::sandbox::SafetyActivation,
        capability: &str,
    ) -> bool {
        match activation {
            crate::services::sandbox::SafetyActivation::Activated { .. } => {
                matches!(capability, "process.noxworkspace" | "process.scratchonly")
            }
            // SafeDisabled/Unsupported grant nothing — never a partial set.
            crate::services::sandbox::SafetyActivation::NotActivated { .. } => false,
        }
    }

    /// P28: resolve the Shell surface for one approved WorkUnit under the
    /// final predicate — the frozen wire unit's effect class and network,
    /// plus the store's SIX-column exact effect-approval lookup. `None`
    /// (including every Denied shape) keeps the tool undiscoverable;
    /// mutable settings are never consulted.
    pub fn resolve_shell_surface(
        store: &V1Store,
        task_id: &str,
        plan_revision: &str,
        wire: &r_code_harness_protocol::services::WorkUnitWire,
    ) -> Option<crate::services::authorization::ShellAuthority> {
        use crate::services::authorization::{resolve_shell_authority, ShellAuthority};
        let network = wire.network_ceiling;
        let effect_class = wire.effect_class;
        let approval = store
            .find_active_effect_approval(
                task_id,
                plan_revision,
                &wire.id,
                effect_class.as_str(),
                network.as_str(),
                &r_code_harness_protocol::services::work_unit_payload_hash(wire),
            )
            .ok()
            .flatten()
            .map(|record| {
                (
                    effect_class_of(&record.effect_class),
                    network_of(&record.network),
                )
            });
        match resolve_shell_authority(effect_class, network, approval) {
            ShellAuthority::Denied { .. } => None,
            authority => Some(authority),
        }
    }

    async fn verify_candidate(
        &self,
        binding: &TaskWorkspaceBinding,
        candidate: &CandidateManifest,
        unit: &WorkUnit,
        attempt_id: &str,
        state: &mut TaskState,
    ) -> Result<Vec<EvidenceRequirement>, (String, bool)> {
        let required = required_checks(&state.contract.required_checks, unit);
        if required.is_empty() {
            candidate
                .verify_live(binding)
                .map_err(|_| ("candidate inputs changed".to_string(), false))?;
            return Ok(Vec::new());
        }
        // P20: required checks resolve their backend from the platform safety
        // report for this exact boot identity. On any non-Activated platform
        // the runner is sandbox-gated and spawns nothing — each check reports
        // Unavailable and the run never reaches ReviewReady. There is no
        // unsandboxed local-shell fallback on this path (INV-06/INV-07).
        let boot = crate::process_guard::BootIdentity::current()
            .map_err(|error| (format!("cannot read boot identity: {error}"), true))?;
        let runner = VerificationRunner::sandboxed(&self.store, boot.as_str());
        let controls = FrozenControlStore::new(self.service_paths.artifacts_root.join("_controls"));
        let mut requirements = Vec::new();
        for check_id in required {
            let definition = self
                .store
                .load_check_definition(&check_id)
                .map_err(|_| ("check definition is unavailable".to_string(), true))?
                .ok_or_else(|| ("check definition is unavailable".to_string(), true))?;
            let check_key = sha256_hex(check_id.as_bytes());
            let attempt_key = sha256_hex(attempt_id.as_bytes());
            let candidate_check_key =
                sha256_hex(format!("{}:{check_key}", candidate.candidate_id).as_bytes());
            let verify_dir = verification_dir_for(
                &self
                    .service_paths
                    .artifacts_root
                    .join("_verification")
                    .join(&attempt_key[..16]),
                &candidate_check_key[..32],
            );
            let outcome = runner
                .run(
                    binding,
                    candidate,
                    &controls,
                    &definition,
                    &verify_dir,
                    Duration::from_secs(600),
                )
                .await;
            if let Some(evidence) = outcome.evidence {
                requirements.push(EvidenceRequirement {
                    check_id: evidence.check_id.clone(),
                    definition_identity: evidence.definition_identity.clone(),
                    environment_fingerprint: evidence.environment_fingerprint.clone(),
                });
                self.store
                    .save_evidence(&evidence)
                    .map_err(|_| ("evidence could not be persisted".to_string(), true))?;
                state
                    .record_evidence(evidence)
                    .map_err(|_| ("evidence could not be recorded".to_string(), true))?;
            }
            match outcome.status {
                CheckStatus::Passed => {}
                CheckStatus::Failed { .. } => {
                    return Err(("required check failed".to_string(), false))
                }
                CheckStatus::Unavailable { .. } | CheckStatus::TimedOut { .. } => {
                    return Err(("required check unavailable".to_string(), true))
                }
                CheckStatus::InputsChanged => {
                    return Err(("candidate inputs changed".to_string(), false))
                }
            }
        }
        candidate
            .verify_live(binding)
            .map_err(|_| ("candidate inputs changed".to_string(), false))?;
        Ok(requirements)
    }

    // -- cancel ------------------------------------------------------------

    /// E08: sweep every registered supervised tree with per-tree proofs —
    /// the shutdown path's cancel-and-prove-all. Proven trees deregister;
    /// unprovable ones stay registered and are reported, never assumed dead.
    pub async fn sweep_supervised_children(&self) -> crate::child_supervisor::SupervisorSweep {
        self.child_supervisor.cancel_and_prove_all().await
    }

    /// Cancel the active run of a task (no-op when idle).
    pub async fn cancel(&self, task_id: &str) -> Result<bool, RunError> {
        let task = self
            .store
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
        let (guard, process, run_id, attempt_id, input_id, transcript_position) = loop {
            let snapshot = {
                let slot_guard = slot.lock().await;
                (
                    slot_guard.guard.clone(),
                    slot_guard.process.clone(),
                    slot_guard.run_id.clone(),
                    slot_guard.attempt_id.clone(),
                    slot_guard.input_message_id.clone(),
                    slot_guard.transcript_position,
                )
            };
            if snapshot.1.is_some() || std::time::Instant::now() >= deadline {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let Some(guard) = guard else {
            return Ok(false);
        };
        guard.revoke();
        // E08: task cancel routes through cancel-and-prove-all — every
        // registered supervised tree is swept with its OWN death proof. An
        // unprovable tree keeps the set unswept, exactly like the slot's own
        // unconfirmed process below: the cancelled settle never lands over
        // an unproven child (INV-08).
        let sweep = self.child_supervisor.cancel_and_prove_all().await;
        let terminated = if let Some(process) = process {
            // A busy Harness can be waiting on a host model callback and be
            // unable to service its own graceful cancel RPC. Revocation is
            // already authoritative; terminate and reap with a hard bound so
            // the owning run can persist its Cancelled transition.
            if process.is_alive() {
                process.kill_confirmed().await
            } else {
                true
            }
        } else {
            false
        };
        let terminated = terminated && sweep.all_dead();
        if terminated && task.contract.kind == TaskKind::Conversation {
            let drive = {
                let mut slot_guard = slot.lock().await;
                slot_guard.drive.take()
            };
            if let Some(drive) = drive {
                drive.abort();
                let _ = drive.await;
            }
            // No stale run-state writer remains after the join. The exact
            // attempt CAS below therefore cannot be reversed by natural
            // completion racing this cancellation.
            let settle_result = self
                .settle_cancelled_conversation(
                    task_id,
                    run_id.as_deref(),
                    attempt_id.as_deref(),
                    input_id.as_deref(),
                    transcript_position.unwrap_or(0),
                )
                .await;
            if settle_result.is_ok() {
                let mut slot_guard = slot.lock().await;
                if slot_guard.attempt_id.as_deref() == attempt_id.as_deref() {
                    slot_guard.run_id = None;
                    slot_guard.attempt_id = None;
                    slot_guard.input_message_id = None;
                    slot_guard.transcript_position = None;
                    slot_guard.guard = None;
                    slot_guard.process = None;
                    slot_guard.stop_pump = None;
                }
            }
            settle_result?;
        }
        Ok(true)
    }

    async fn settle_cancelled_conversation(
        &self,
        task_id: &str,
        run_id: Option<&str>,
        attempt_id: Option<&str>,
        input_id: Option<&str>,
        transcript_position: u64,
    ) -> Result<(), RunError> {
        let Some(attempt_id) = attempt_id else {
            return Ok(());
        };
        let mut owns_cancel = false;
        for _ in 0..TASK_CAS_RETRIES {
            let (mut state, revision) = self
                .store
                .load_task_with_revision(task_id)
                .map_err(|error| RunError::Failure(error.to_string()))?
                .ok_or_else(|| RunError::UnknownTask(task_id.to_string()))?;
            let generation = match &state.execution {
                TaskExecution::Running {
                    attempt_id: active,
                    generation,
                }
                | TaskExecution::WaitingInput {
                    attempt_id: active,
                    generation,
                    ..
                } if active == attempt_id => Some(*generation),
                TaskExecution::Terminal {
                    verdict: TaskVerdict::Cancelled { .. },
                } if self.store.task_events(task_id).iter().rev().any(|event| {
                    event.kind == "run.cancelled"
                        && event
                            .payload
                            .get("attemptId")
                            .and_then(|value| value.as_str())
                            == Some(attempt_id)
                }) =>
                {
                    owns_cancel = true;
                    None
                }
                _ => return Ok(()),
            };
            if let Some(generation) = generation {
                state
                    .cancel(Actor::Host, generation, "user requested cancel")
                    .map_err(|error| RunError::Failure(error.to_string()))?;
                match self.store.save_task_and_events_if_revision(
                    &state,
                    vec![journal_event(
                        task_id,
                        "run.cancelled",
                        serde_json::json!({
                            "runId": run_id,
                            "attemptId": attempt_id,
                            "reason": "user-requested",
                        }),
                    )],
                    revision,
                ) {
                    Ok(_) => {
                        owns_cancel = true;
                        break;
                    }
                    Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
                    Err(error) => return Err(RunError::Failure(error.to_string())),
                }
            } else {
                break;
            }
        }
        if !owns_cancel {
            return Ok(());
        }
        self.transcript_for(task_id)
            .await
            .map_err(RunError::Failure)?
            .truncate_to(transcript_position)
            .map_err(|_| RunError::Failure("unable to restore cancelled transcript".into()))?;
        if let Some(input_id) = input_id {
            let _ = self.kernel_tasks.acknowledge(task_id, input_id).await;
        }
        Ok(())
    }

    // -- queries -----------------------------------------------------------

    /// Whether a run is currently active for the task.
    pub async fn is_running(&self, task_id: &str) -> bool {
        let aggregate_active = self.store.load_task(task_id).await.is_some_and(|state| {
            matches!(
                state.execution,
                TaskExecution::Running { .. }
                    | TaskExecution::WaitingInput { .. }
                    | TaskExecution::Verifying { .. }
            )
        });
        if !aggregate_active {
            return false;
        }
        let slot = self.slots.lock().await.get(task_id).cloned();
        match slot {
            Some(slot) => slot.lock().await.run_id.is_some(),
            None => false,
        }
    }

    /// Wait for both the live run and its dispatch-loop/startup slot to
    /// drain. Material changes use this after observing a settled aggregate:
    /// `is_running` alone intentionally misses the short pre-registration and
    /// post-settlement windows.
    pub async fn await_quiescent(&self, task_id: &str, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let slot = self.slots.lock().await.get(task_id).cloned();
            let busy = match slot {
                Some(slot) => {
                    let slot = slot.lock().await;
                    slot.run_id.is_some()
                        || slot.guard.is_some()
                        || slot.process.is_some()
                        || slot
                            .drive
                            .as_ref()
                            .is_some_and(|handle| !handle.is_finished())
                }
                None => false,
            };
            if !busy {
                return true;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return false;
            }
            tokio::time::sleep(
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(25)),
            )
            .await;
        }
    }

    pub(crate) async fn count_runs(&self, task_id: &str) -> u64 {
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

fn required_checks(contract_checks: &[String], unit: &WorkUnit) -> Vec<String> {
    contract_checks
        .iter()
        .chain(
            unit.acceptance
                .iter()
                .filter(|check_id| check_id.starts_with("check:")),
        )
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) const TASK_CAS_RETRIES: usize = 8;

fn save_run_state(
    store: &V1Store,
    run_state: &TaskState,
    events: Vec<r_code_kernel::ports::JournalEvent>,
) -> Result<TaskState, String> {
    for _ in 0..TASK_CAS_RETRIES {
        let (mut latest, revision) = store
            .load_task_with_revision(&run_state.contract.task_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("task {} not found", run_state.contract.task_id))?;
        if latest.contract.revision != run_state.contract.revision {
            return Err(format!(
                "task contract changed while run was active: frozen {}, current {}",
                run_state.contract.revision, latest.contract.revision
            ));
        }
        let preserve_waiting_input = matches!(
            (&latest.execution, &run_state.execution),
            (
                TaskExecution::WaitingInput {
                    attempt_id: waiting_attempt,
                    generation: waiting_generation,
                    ..
                },
                TaskExecution::Running {
                    attempt_id: incoming_attempt,
                    generation: incoming_generation,
                }
            ) if waiting_attempt == incoming_attempt && waiting_generation == incoming_generation
        );
        if !preserve_waiting_input {
            latest.execution = run_state.execution.clone();
        }
        // E05: the run's per-unit records (each unit's candidate digest and
        // verification outcome) and the wave's active approval replace the
        // pre-E05 single-slot validation/candidate fields wholesale.
        latest.review = run_state.review;
        latest.work_units = run_state.work_units.clone();
        latest.unit_records = run_state.unit_records.clone();
        latest.active_approval = run_state.active_approval.clone();
        for evidence in &run_state.evidence {
            if !latest.evidence.contains(evidence) {
                latest.evidence.push(evidence.clone());
            }
        }
        match store.save_task_and_events_if_revision(&latest, events.clone(), revision) {
            Ok(_) => return Ok(latest),
            Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(format!(
        "task {} stayed busy for {TASK_CAS_RETRIES} aggregate retries",
        run_state.contract.task_id
    ))
}

pub(crate) fn journal_event(
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
        TaskVerdict::VerifiedAccepted { .. } => "verified-accepted",
        TaskVerdict::Unverified { .. } => "unverified",
        TaskVerdict::UnverifiedAccepted { .. } => "unverified-accepted",
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
        "task.created"
        | "harness.pinned"
        | "task.renamed"
        | "task.preferences"
        | "task.reopened"
        | "run.started"
        | "run.completed"
        | "run.failed"
        | "run.cancelled"
        | "branch.created"
        | "plan.awaiting-approval"
        | "plan.approved"
        | "plan.invalidated" => EventKind::RunState,
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

/// String forms of the frozen P19A enums as the effect-approval table
/// stores them; anything unrecognized falls to the conservative floor.
fn effect_class_of(value: &str) -> r_code_harness_protocol::services::WorkUnitEffectClass {
    match value {
        "workspace-mutation" => {
            r_code_harness_protocol::services::WorkUnitEffectClass::WorkspaceMutation
        }
        "dependency-preparation" => {
            r_code_harness_protocol::services::WorkUnitEffectClass::DependencyPreparation
        }
        _ => r_code_harness_protocol::services::WorkUnitEffectClass::ReadOnly,
    }
}

fn network_of(value: &str) -> r_code_harness_protocol::NetworkCeiling {
    match value {
        "public-internet-client" => r_code_harness_protocol::NetworkCeiling::PublicInternetClient,
        "host-network" => r_code_harness_protocol::NetworkCeiling::HostNetwork,
        _ => r_code_harness_protocol::NetworkCeiling::Offline,
    }
}
