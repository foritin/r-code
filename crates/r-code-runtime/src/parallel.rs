//! E06 bounded-concurrency WorkUnit dispatcher.
//!
//! Replaces run_manager's single-unit selection: one approved plan drives a
//! WAVE over the whole WorkUnit DAG. Each tick computes the ready set
//! (Pending AND every dependency Completed AND read-set revalidation
//! passing), dispatches up to the configured bound — writing a durable
//! `work_unit_attempts` row BEFORE any spawn — and settles completed units
//! strictly from their own attempt's durable settle. Drift detection rides
//! the existing P26/P27 vocabulary (`capture_manifest` / `revalidate` /
//! `delta` plus the journaled mutation history), never a new scanner.

use crate::plugins::catalog::{Availability, CatalogEntry};
use crate::plugins::router::{supported_requested_services, RouterServiceAvailability};
use crate::plugins::{HostRouter, PluginSession, TransportLimits};
use crate::services::artifacts::ArtifactStore;
use crate::services::process_effects::{
    capture_manifest, delta, revalidate, DeltaKind, FileIdentity, ScanBounds, ScanManifest,
    ScanPolicy,
};
use crate::services::run_snapshots::{planning_harness_config, RunSnapshotBuilder};
use crate::services::tools::ExecutionToolService;
use crate::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_harness_protocol::{
    HostService, InputMessage, NegotiatedCapabilities, PackageRef, RunIdentity,
};
use r_code_kernel::ports::{HarnessSession as _, RunGuard};
use r_code_kernel::task::{
    Actor, Attempt, PlanApprovalRef, TaskExecution, TaskState, UnitRecord, ValidationOutcome,
    WorkUnit, WorkUnitStatus,
};
use r_code_store::v1::{V1Store, WorkUnitAttemptSeed};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::task::JoinSet;

use super::{
    drain_observations, journal_event, save_run_state, RunManager, DEFAULT_HARNESS_ID,
    TASK_CAS_RETRIES,
};

/// How many WorkUnits may hold a live harness run at once (E06.3). Durable
/// path leases still enforce write-scope exclusion on top of this bound.
pub(crate) const DEFAULT_DISPATCH_BOUND: usize = 2;

/// Attempt id for one exact (task, plan revision, work unit) dispatch
/// (E06.1). Deterministic, so a replayed dispatch converges on the same
/// durable row instead of minting a second attempt.
pub(crate) fn work_unit_attempt_id(
    task_id: &str,
    plan_revision: &str,
    work_unit_id: &str,
) -> String {
    let short_revision = plan_revision
        .trim_start_matches("sha256:")
        .chars()
        .take(12)
        .collect::<String>();
    format!("attempt-{task_id}-{short_revision}-{work_unit_id}")
}

/// A dependency-ready unit: still Pending, writable, and every dependency
/// Completed in the live unit list (E06.2's structural half).
fn dependency_ready(units: &[WorkUnit], unit: &WorkUnit) -> bool {
    unit.status == WorkUnitStatus::Pending
        && (!unit.write_paths.is_empty() || unit.repo_exclusive)
        && unit.dependencies.iter().all(|dependency| {
            units.iter().any(|candidate| {
                candidate.id == *dependency && candidate.status == WorkUnitStatus::Completed
            })
        })
}

/// Copy the kernel-owned unit statuses onto the wave's working copy.
fn refresh_unit_statuses(units: &mut [WorkUnit], state: &TaskState) {
    for unit in units.iter_mut() {
        if let Some(prior) = state.work_units.iter().find(|other| other.id == unit.id) {
            unit.status = prior.status;
        }
    }
}

// ---------------------------------------------------------------------------
// Per-wave drift bookkeeping (E06.2)
// ---------------------------------------------------------------------------

/// The scope manifests one wave freezes and revalidates. Detection rides the
/// P26 scanner and the journaled mutation history: a write-scope change is
/// legitimate only when the unit's own attempt journaled exactly that
/// after-state. Everything else is an external edit, and external edits
/// re-block dependents — never overwrite them (INV-10).
struct WaveDrift {
    binding: TaskWorkspaceBinding,
    /// Read-set baselines, frozen the first time a unit becomes
    /// dependency-ready. Keyed by unit id.
    read_baselines: HashMap<String, Vec<(String, ScanManifest)>>,
    /// Write-scope baselines, frozen at dispatch so only changes DURING a
    /// unit's flight land in its settle delta.
    write_baselines: HashMap<String, Vec<(String, ScanManifest)>>,
}

impl WaveDrift {
    fn new(binding: TaskWorkspaceBinding) -> Self {
        Self {
            binding,
            read_baselines: HashMap::new(),
            write_baselines: HashMap::new(),
        }
    }

    /// The scopes a unit must see unchanged: its own read paths, or — when
    /// it declares none — the write scopes of its dependencies. An empty
    /// scope string means the whole bound root (repo-exclusive units).
    fn read_scopes(units: &[WorkUnit], unit: &WorkUnit) -> Vec<String> {
        if !unit.read_paths.is_empty() {
            let mut scopes = unit.read_paths.clone();
            scopes.sort();
            scopes.dedup();
            return scopes;
        }
        let mut scopes = Vec::new();
        for dependency in &unit.dependencies {
            let Some(dep) = units.iter().find(|candidate| candidate.id == *dependency) else {
                continue;
            };
            if dep.repo_exclusive {
                scopes.push(String::new());
            } else {
                scopes.extend(dep.write_paths.iter().cloned());
            }
        }
        scopes.sort();
        scopes.dedup();
        scopes
    }

    fn write_scopes(unit: &WorkUnit) -> Vec<String> {
        if unit.repo_exclusive {
            vec![String::new()]
        } else {
            unit.write_paths.clone()
        }
    }

    fn scope_root(&self, scope: &str) -> std::path::PathBuf {
        if scope.is_empty() {
            self.binding.canonical_root.clone()
        } else {
            self.binding.canonical_root.join(scope)
        }
    }

    fn capture_scope(&self, scope: &str) -> Result<ScanManifest, String> {
        let root = self.scope_root(scope);
        let metadata = match std::fs::metadata(&root) {
            Ok(metadata) => metadata,
            // An absent scope has nothing to drift yet.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ScanManifest {
                    files: Vec::new(),
                    logical_bytes: 0,
                })
            }
            Err(error) => return Err(format!("inspect drift scope: {error}")),
        };
        let policy = ScanPolicy::for_workspace_root(root);
        if metadata.is_dir() {
            return capture_manifest(&policy, &ScanBounds::default())
                .map_err(|error| format!("capture drift scope: {error}"));
        }
        // A file scope: one identity anchored to the root path itself (the
        // empty relative path makes P26 `revalidate` re-check this file).
        let bytes = std::fs::read(&policy.root).map_err(|error| error.to_string())?;
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|moment| {
                moment
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_millis() as i64)
            })
            .unwrap_or(0);
        Ok(ScanManifest {
            files: vec![FileIdentity {
                path: String::new(),
                sha256: crate::services::artifacts::sha256_hex(&bytes),
                bytes: metadata.len(),
                modified_ms,
                is_binary: false,
            }],
            logical_bytes: metadata.len(),
        })
    }

    /// Freeze the unit's read-set baseline at its first dependency-ready
    /// moment, then revalidate it. `Err` means external drift: the unit is
    /// re-blocked, never started over the drift.
    fn revalidate_read_set(&mut self, units: &[WorkUnit], unit: &WorkUnit) -> Result<(), String> {
        if !self.read_baselines.contains_key(&unit.id) {
            let mut baselines = Vec::new();
            for scope in Self::read_scopes(units, unit) {
                baselines.push((scope.clone(), self.capture_scope(&scope)?));
            }
            self.read_baselines.insert(unit.id.clone(), baselines);
        }
        for (scope, manifest) in &self.read_baselines[&unit.id] {
            let policy = ScanPolicy::for_workspace_root(self.scope_root(scope));
            revalidate(manifest, &policy)
                .map_err(|error| format!("read-set drift in {scope}: {error}"))?;
        }
        Ok(())
    }

    /// Freeze the write-scope baseline immediately before dispatch, so the
    /// settle delta observes only the flight window.
    fn freeze_write_baseline(&mut self, unit: &WorkUnit) -> Result<(), String> {
        let mut baselines = Vec::new();
        for scope in Self::write_scopes(unit) {
            baselines.push((scope.clone(), self.capture_scope(&scope)?));
        }
        self.write_baselines.insert(unit.id.clone(), baselines);
        Ok(())
    }

    /// Explain one settled unit's write-scope delta against its OWN journaled
    /// mutations. Returns the workspace-relative paths edited externally
    /// during flight (empty = no external drift).
    fn external_write_drift(
        &self,
        store: &V1Store,
        unit: &WorkUnit,
        attempt_id: &str,
    ) -> Vec<String> {
        let Some(baselines) = self.write_baselines.get(&unit.id) else {
            return Vec::new();
        };
        // The attempt's journaled after-states, last write per path.
        let mut journaled: BTreeMap<String, Option<String>> = BTreeMap::new();
        if let Ok(operations) = store.mutation_operations_for_owner(attempt_id) {
            for operation in operations {
                for file in &operation.files {
                    journaled.insert(file.logical_path.clone(), file.after_sha256.clone());
                }
            }
        }
        let mut external = Vec::new();
        for (scope, before) in baselines {
            let Ok(after) = self.capture_scope(scope) else {
                external.push(scope.clone());
                continue;
            };
            let after_sha: BTreeMap<&str, &str> = after
                .files
                .iter()
                .map(|file| (file.path.as_str(), file.sha256.as_str()))
                .collect();
            for entry in delta(before, &after) {
                let logical = if entry.path.is_empty() {
                    scope.clone()
                } else if scope.is_empty() {
                    entry.path.clone()
                } else {
                    format!("{scope}/{}", entry.path)
                };
                let explained = match entry.kind {
                    DeltaKind::Deleted => journaled.get(&logical) == Some(&None),
                    DeltaKind::Created | DeltaKind::Edited | DeltaKind::Binary => {
                        journaled.get(&logical).is_some_and(|after| {
                            after.as_deref() == after_sha.get(entry.path.as_str()).copied()
                        })
                    }
                };
                if !explained {
                    external.push(logical);
                }
            }
        }
        external.sort();
        external.dedup();
        external
    }
}

// ---------------------------------------------------------------------------
// The wave
// ---------------------------------------------------------------------------

/// What one spawned harness run came back with. All kernel/journal work
/// stays in the dispatcher loop; this carries only the run's own outcome
/// plus the per-unit handles the settle needs.
struct UnitRunOutcome {
    unit: WorkUnit,
    attempt_id: String,
    execution_tools: Arc<ExecutionToolService>,
    router: Arc<HostRouter>,
    /// `Ok` when the harness session completed and its process termination
    /// was proven. `Err((reason, unavailable))` is a repair-class failure.
    outcome: Result<(), (String, bool)>,
    /// A tool reported a conflict/failure during the run.
    tool_failed: bool,
}

/// Why a pump tick did not start a ready unit.
enum DispatchOutcome {
    /// The unit was started; its run is in the JoinSet.
    Started,
    /// The unit started durably but failed before producing a run (for
    /// example the harness process could not be spawned); it has already
    /// settled failed and the wave continues its siblings.
    FailedUnit(String),
    /// Transient (typically a write-scope lease held elsewhere): retry on a
    /// later tick.
    Deferred(String),
    /// Read-set drift: the unit is re-blocked for the rest of the wave.
    ReBlocked(String),
    /// Contract-level failure that must end the wave loudly.
    Fatal(String),
}

/// How one serialized settle ended.
enum SettleFlow {
    /// The unit settled; the wave continues.
    Continue,
    /// Cancellation was observed; the whole wave settles cancelled.
    Cancelled,
    /// The unit settled repair-class — the wave surfaces `run.failed`.
    Failed(String),
    /// Unrecoverable bookkeeping failure; the wave aborts loudly.
    Fatal(String),
}

/// Shared, immutable-per-wave context.
struct WaveCtx<'a> {
    task_id: &'a str,
    plan: &'a r_code_kernel::plans::PlanRevision,
    approval: &'a PlanApprovalRef,
    binding: &'a TaskWorkspaceBinding,
    package: &'a PackageRef,
    entry: &'a CatalogEntry,
    guard: &'a Arc<RunGuard>,
    grants: &'a [HostService],
    store: &'a V1Store,
    artifacts: &'a Arc<ArtifactStore>,
    transcript: &'a Arc<crate::services::context::TranscriptWriter>,
    transcript_position: u64,
}

impl RunManager {
    /// Drive one approved plan's whole WorkUnit DAG (E06): dispatch
    /// dependency-ready units up to [`DEFAULT_DISPATCH_BOUND`] concurrently,
    /// settle each strictly from its own attempt's durable record, and end
    /// only at the kernel's task-level aggregate or a terminal cancellation.
    pub(crate) async fn run_execution_wave(
        &self,
        task_id: &str,
        input: &InputMessage,
        state: TaskState,
    ) -> Result<(), String> {
        let selected = self
            .approved_execution_selection(&state)
            .map_err(|error| error.to_string())?;
        let pinned_id = self
            .pinned_harness_id(task_id)
            .await
            .unwrap_or_else(|| DEFAULT_HARNESS_ID.to_string());
        let package = self
            .catalog
            .effective_package(&pinned_id)
            .map_err(|error| format!("Harness {pinned_id} 不可用：{error}"))?;
        let entry = self
            .catalog
            .list()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|entry| {
                entry.package_ref.content_digest == package.content_digest
                    && entry.availability == Availability::Available
            })
            .ok_or_else(|| format!("Harness {pinned_id} 未安装或不可用"))?;
        let workspace_path = state
            .preferences
            .workspace_path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
            .ok_or("execution requires the current checkout")?;
        let binding =
            TaskWorkspaceBinding::bind_local(task_id, std::path::Path::new(workspace_path), &[])
                .map_err(|error| format!("无法绑定执行工作区：{error}"))?;
        let sandbox_gate = {
            let boot = crate::process_guard::BootIdentity::current()
                .map_err(|error| format!("无法读取启动身份：{error}"))?;
            crate::services::sandbox::platform_activation_gate(&self.store, boot.as_str())
        };
        self.bind_harness_activation(sandbox_gate.clone());
        let sandbox_activated = matches!(
            sandbox_gate,
            crate::services::sandbox::SafetyActivation::Activated { .. }
        );
        let grants = supported_requested_services(
            &entry.manifest.requested_host_services,
            RouterServiceAvailability {
                model_stream: !matches!(
                    state.preferences.model_route,
                    Some(r_code_kernel::task::ModelRoute::HarnessManaged { .. })
                ),
                tools: true,
                context: true,
                artifacts: true,
                plan_publish: false,
                questions: true,
                approvals: true,
                checkpoints: true,
                completion: true,
                sandbox_activated,
            },
        );
        if RunManager::grants_contain_unavailable_effect_services(&grants) {
            return Err("execution grants contain an unavailable effect service".to_string());
        }

        let guard = RunGuard::new(
            format!("run-{task_id}-{}", self.count_runs(task_id).await + 1),
            1,
        );
        let transcript = self.transcript_for(task_id).await?;
        let artifacts = self.artifacts_for(task_id);
        let ctx = WaveCtx {
            task_id,
            plan: &selected.plan,
            approval: &selected.approval,
            binding: &binding,
            package: &package,
            entry: &entry,
            guard: &guard,
            grants: &grants,
            store: &self.store,
            artifacts: &artifacts,
            transcript: &transcript,
            transcript_position: transcript.position(),
        };

        let mut units = selected.plan_units.clone();
        let mut live = state;
        let mut drift = WaveDrift::new(binding.clone());
        let mut runs: JoinSet<UnitRunOutcome> = JoinSet::new();
        // Unit id -> its still-held write lease (released at settle, or on
        // abort/cancel paths).
        let mut held_tools: HashMap<String, Arc<ExecutionToolService>> = HashMap::new();
        let mut awaiting: VecDeque<UnitRunOutcome> = VecDeque::new();
        let mut deferred: HashMap<String, String> = HashMap::new();
        // Unit ids whose records this wave seeded to hold the aggregate open
        // (frontier hold); only these may be cleaned up at wave end.
        let mut seeded: Vec<String> = Vec::new();
        let mut dispatched_any = false;
        let mut run_failure: Option<String> = None;
        let mut fatal: Option<String> = None;
        let mut next_run = self.count_runs(task_id).await + 1;

        loop {
            // 1. Pump the ready set while capacity remains. This runs
            //    BEFORE settling, so the kernel aggregate can never fire
            //    while more units of the DAG can still start.
            while fatal.is_none()
                && runs.len() + awaiting.len() < DEFAULT_DISPATCH_BOUND
                && matches!(
                    live.execution,
                    TaskExecution::Ready { .. } | TaskExecution::Running { .. }
                )
            {
                let Some(unit) = units
                    .iter()
                    .find(|unit| dependency_ready(&units, unit) && !deferred.contains_key(&unit.id))
                    .cloned()
                else {
                    break;
                };
                let run_id = format!("run-{task_id}-{next_run}");
                next_run += 1;
                match self
                    .dispatch_unit(
                        &ctx,
                        input,
                        &mut live,
                        &mut drift,
                        &mut units,
                        &mut runs,
                        &mut held_tools,
                        &unit,
                        run_id,
                    )
                    .await
                {
                    DispatchOutcome::Started => dispatched_any = true,
                    DispatchOutcome::FailedUnit(reason) => {
                        // The unit started and durably settled failed (its
                        // repair bookkeeping is done); the wave continues.
                        dispatched_any = true;
                        run_failure = Some(reason);
                        refresh_unit_statuses(&mut units, &live);
                    }
                    DispatchOutcome::Deferred(reason) => {
                        deferred.insert(unit.id.clone(), reason);
                    }
                    DispatchOutcome::ReBlocked(reason) => {
                        match self.reblock_unit(&live, &unit.id, &reason) {
                            Ok(state) => {
                                live = state;
                                refresh_unit_statuses(&mut units, &live);
                            }
                            Err(error) => fatal = Some(error),
                        }
                    }
                    DispatchOutcome::Fatal(reason) => fatal = Some(reason),
                }
            }

            // 2. Fatal: reap the spawned runs (each kills its own harness
            //    process at its end), release every held lease, then fail
            //    the wave. Attempt rows stay in flight for restart recovery.
            if let Some(fatal) = fatal {
                while runs.join_next().await.is_some() {}
                for tools in held_tools.values() {
                    let _ = tools.release();
                }
                for pending in &awaiting {
                    let _ = pending.execution_tools.release();
                }
                return Err(fatal);
            }

            // 3. Settle one completed run (serialized kernel transitions).
            //    The kernel's aggregate fires when every STARTED unit is
            //    terminal, so before settling the LAST unsettled unit the
            //    wave seeds the records of the units that settle would make
            //    ready (their deterministic attempt ids, still InFlight) —
            //    the aggregate stays open and the next pump starts them for
            //    real through `start_execution_attempt`.
            if let Some(next) = awaiting.pop_front() {
                if runs.is_empty() && awaiting.is_empty() {
                    let frontier: Vec<String> = units
                        .iter()
                        .filter(|unit| {
                            unit.status == WorkUnitStatus::Pending
                                && (!unit.write_paths.is_empty() || unit.repo_exclusive)
                                && unit.dependencies.iter().all(|dependency| {
                                    dependency == &next.unit.id
                                        || units.iter().any(|candidate| {
                                            candidate.id == *dependency
                                                && candidate.status == WorkUnitStatus::Completed
                                        })
                                })
                        })
                        .map(|unit| unit.id.clone())
                        .collect();
                    let mut seeded_any = false;
                    for unit_id in frontier {
                        let attempt =
                            work_unit_attempt_id(task_id, ctx.plan.reference().as_str(), &unit_id);
                        if !live.unit_records.contains_key(&unit_id) {
                            live.unit_records.insert(
                                unit_id.clone(),
                                UnitRecord {
                                    attempt_id: Some(attempt),
                                    ..UnitRecord::default()
                                },
                            );
                            seeded.push(unit_id);
                            seeded_any = true;
                        }
                    }
                    if seeded_any {
                        match save_run_state(&self.store, &live, Vec::new()) {
                            Ok(state) => live = state,
                            Err(error) => {
                                fatal = Some(error);
                                continue;
                            }
                        }
                    }
                }
                match self.settle_unit(&ctx, &mut live, &mut drift, &next).await {
                    SettleFlow::Continue => {
                        deferred.clear();
                        refresh_unit_statuses(&mut units, &live);
                    }
                    SettleFlow::Cancelled => {
                        awaiting.push_front(next);
                        return self
                            .settle_wave_cancelled(
                                task_id,
                                input,
                                &mut live,
                                &ctx,
                                &mut held_tools,
                                &mut awaiting,
                            )
                            .await;
                    }
                    SettleFlow::Failed(reason) => {
                        deferred.clear();
                        run_failure = Some(reason);
                        refresh_unit_statuses(&mut units, &live);
                    }
                    SettleFlow::Fatal(reason) => fatal = Some(reason),
                }
                continue;
            }

            // 4. Nothing to settle and nothing executing: the wave is over
            //    (the last settle already fired the task-level aggregate).
            if runs.is_empty() {
                break;
            }

            // 5. Wait for the next run to complete.
            let Some(joined) = runs.join_next().await else {
                continue;
            };
            let joined = match joined {
                Ok(done) => done,
                Err(error) => {
                    fatal = Some(format!("unit run join failed: {error}"));
                    continue;
                }
            };
            let mut done = joined;
            // A completion frees both a bound slot and (after its settle) a
            // write lease: deferred units get another chance.
            deferred.clear();
            held_tools.remove(&done.unit.id);
            let observations = drain_observations(&done.router);
            done.tool_failed = observations.iter().any(|event| {
                event.kind == "tool.result"
                    && event.payload.get("ok").and_then(|value| value.as_bool()) == Some(false)
            });
            if !observations.is_empty() {
                match save_run_state(&self.store, &live, observations) {
                    Ok(state) => live = state,
                    Err(error) => {
                        fatal = Some(error);
                        continue;
                    }
                }
            }
            awaiting.push_back(done);
        }

        // A seed that never became a real start (blocked by a failed
        // dependency, or deferred past the wave's end) must not hold the
        // aggregate open in the durable state. Only records this wave seeded
        // and that were never replaced by a real start or failure are
        // removed: the record is still the untouched seed (settlement
        // InFlight, attempt bound, no candidate) and the journal carries no
        // `execution.started` row for that seeded attempt id. The unit's
        // status — Pending or Blocked — is irrelevant to that question: a
        // dependency that fails after the seed landed leaves the dependent
        // Blocked with the seed still marking it unsettled.
        let stale_seeds: Vec<String> = if seeded.is_empty() {
            Vec::new()
        } else {
            let journal = self.store.task_events(task_id);
            let started_attempts: HashSet<&str> = journal
                .iter()
                .filter(|event| event.kind == "execution.started")
                .filter_map(|event| {
                    event
                        .payload
                        .get("attemptId")
                        .and_then(|value| value.as_str())
                })
                .collect();
            seeded
                .iter()
                .filter(|unit_id| {
                    live.unit_records.get(*unit_id).is_some_and(|record| {
                        record.settlement == r_code_kernel::task::UnitSettlement::InFlight
                            && record.attempt_id.is_some()
                            && record.candidate_digest.is_none()
                            && !record
                                .attempt_id
                                .as_deref()
                                .is_some_and(|attempt| started_attempts.contains(attempt))
                    })
                })
                .cloned()
                .collect()
        };
        if !stale_seeds.is_empty() {
            for unit_id in &stale_seeds {
                live.unit_records.remove(unit_id);
            }
            // A retracted seed may have been the only thing holding the
            // kernel aggregate open: the phase still names the seed's
            // attempt in Running while nothing is genuinely in flight.
            // Re-derive the aggregate exactly like the settle that
            // stranded the seed would have — re-issuing repair for the
            // units that genuinely failed is idempotent (each record keeps
            // its own attempt and reason).
            if matches!(live.execution, TaskExecution::Running { .. })
                && !live.has_unsettled_units()
            {
                let failed: Vec<(String, Option<String>, String, bool)> = live
                    .unit_records
                    .iter()
                    .filter(|(_, record)| {
                        record.settlement == r_code_kernel::task::UnitSettlement::Failed
                    })
                    .filter_map(|(unit_id, record)| {
                        let (reason, unavailable) = match &record.verification {
                            ValidationOutcome::Unverified { reason } => (reason.clone(), false),
                            ValidationOutcome::CheckUnavailable { reason } => {
                                (reason.clone(), true)
                            }
                            _ => return None,
                        };
                        Some((
                            unit_id.clone(),
                            record.attempt_id.clone(),
                            reason,
                            unavailable,
                        ))
                    })
                    .collect();
                for (unit_id, attempt_id, reason, unavailable) in failed {
                    live.require_repair(
                        Actor::Host,
                        attempt_id,
                        Some(unit_id),
                        reason,
                        unavailable,
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
            live = save_run_state(&self.store, &live, Vec::new())?;
        }

        if !dispatched_any {
            let reason = deferred
                .values()
                .next()
                .cloned()
                .unwrap_or_else(|| "approved plan has no dispatchable work unit".to_string());
            return Err(reason);
        }
        match &live.execution {
            TaskExecution::ReviewReady { .. } => {
                self.kernel_tasks
                    .acknowledge(task_id, &input.message_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(())
            }
            TaskExecution::RepairRequired { attempt_id, .. } => {
                if let Some(reason) = run_failure {
                    if let Some(attempt_id) = attempt_id.clone() {
                        let slot = self.slot_of(task_id).await;
                        let mut slot_guard = slot.lock().await;
                        slot_guard.attempt_id = Some(attempt_id);
                    }
                    return Err(reason);
                }
                self.kernel_tasks
                    .acknowledge(task_id, &input.message_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(())
            }
            other => Err(format!(
                "execution wave ended in unexpected state {other:?}"
            )),
        }
    }

    /// Dispatch one dependency-ready unit (E06.1/E06.3): read-set
    /// revalidation, write lease, frozen per-unit RunSnapshot, DURABLE
    /// attempt row, kernel start — and only then the harness spawn.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_unit(
        &self,
        ctx: &WaveCtx<'_>,
        input: &InputMessage,
        live: &mut TaskState,
        drift: &mut WaveDrift,
        units: &mut [WorkUnit],
        runs: &mut JoinSet<UnitRunOutcome>,
        held_tools: &mut HashMap<String, Arc<ExecutionToolService>>,
        unit: &WorkUnit,
        run_id: String,
    ) -> DispatchOutcome {
        // E06.2: read-set revalidation gates the start.
        if let Err(reason) = drift.revalidate_read_set(units, unit) {
            return DispatchOutcome::ReBlocked(reason);
        }
        let task_id = ctx.task_id;
        let attempt_id = work_unit_attempt_id(task_id, ctx.plan.reference().as_str(), &unit.id);
        let execution_tools =
            match self.execution_tools_for(task_id, &attempt_id, unit, ctx.binding.clone()) {
                Ok(tools) => tools,
                Err(error) => return DispatchOutcome::Deferred(error.to_string()),
            };
        if let Err(reason) = drift.freeze_write_baseline(unit) {
            return DispatchOutcome::Deferred(reason);
        }
        // E05 kernel: execution snapshots freeze against the exact Ready
        // approval; the wave's later units freeze the SAME approval through
        // a Ready-shaped view of the live state.
        let mut freeze_view = live.clone();
        freeze_view.execution = TaskExecution::Ready {
            approval: ctx.approval.clone(),
        };
        let run_tools: Arc<dyn r_code_kernel::ports::ToolService> = execution_tools.clone();
        let workspace = match ctx.binding.snapshot_ref() {
            Ok(workspace) => workspace,
            Err(error) => return DispatchOutcome::Deferred(format!("无法冻结执行工作区：{error}")),
        };
        let effect_approvals = crate::application::StoreEffectApprovals::new(self.store.clone());
        let frozen = match RunSnapshotBuilder::new(
            &self.settings,
            &self.injected_models,
            &run_tools,
            self.allow_injected_model_fallback,
        )
        .with_effect_approvals(&effect_approvals)
        .freeze_execution_with_workspace(
            &freeze_view,
            ctx.package,
            ctx.grants,
            ctx.guard.as_ref(),
            workspace,
            ctx.plan,
            unit,
        )
        .await
        {
            Ok(frozen) => frozen,
            Err(error) => return DispatchOutcome::Deferred(error),
        };
        if let Err(error) = ctx.store.save_run_snapshot(&frozen.snapshot) {
            return DispatchOutcome::Deferred(format!("保存执行快照失败：{error}"));
        }
        // E06.1: the durable attempt row exists BEFORE any spawn; its
        // content digest pins the frozen snapshot this dispatch runs with.
        let seed = WorkUnitAttemptSeed {
            attempt_id: attempt_id.clone(),
            task_id: task_id.to_string(),
            plan_revision: ctx.plan.reference().as_str().to_string(),
            work_unit_id: unit.id.clone(),
            content_sha256: frozen.snapshot.id().as_str().to_string(),
        };
        let attempt_row = match ctx.store.prepare_work_unit_attempt(&seed) {
            Ok(row) => row,
            Err(error) => return DispatchOutcome::Fatal(error.to_string()),
        };
        if attempt_row.phase.is_terminal() {
            return DispatchOutcome::Fatal(format!(
                "attempt {attempt_id} is already settled; refusing to redispatch"
            ));
        }
        let identity = RunIdentity {
            task_id: task_id.to_string(),
            branch_id: format!("branch-{task_id}"),
            run_id: run_id.clone(),
            attempt_id: attempt_id.clone(),
            generation: 1,
        };
        let attempt = Attempt::for_run_snapshot(
            attempt_id.clone(),
            task_id,
            identity.branch_id.clone(),
            ctx.package.clone(),
            live.contract.revision,
            frozen
                .snapshot
                .material()
                .workspace
                .workspace_identity
                .clone(),
            run_id.clone(),
            frozen.snapshot.id(),
        );
        if let Err(error) = self.catalog.pin(&attempt_id, task_id, ctx.package) {
            return DispatchOutcome::Deferred(format!("固定 Harness 包失败：{error}"));
        }
        let checkpoint = match self.handoff_checkpoint(task_id, &attempt_id).await {
            Ok(checkpoint) => checkpoint,
            Err(error) => return DispatchOutcome::Deferred(error),
        };
        // Kernel start under the task CAS: the loser of a genuine race
        // backs off loudly instead of double-starting a unit.
        let mut started = None;
        for _ in 0..TASK_CAS_RETRIES {
            let Ok(Some((mut candidate, revision))) = ctx.store.load_task_with_revision(task_id)
            else {
                return DispatchOutcome::Fatal(format!("task {task_id} not found"));
            };
            let mut view_units = units.to_vec();
            refresh_unit_statuses(&mut view_units, &candidate);
            match candidate.start_execution_attempt(
                Actor::Host,
                &attempt,
                ctx.approval,
                view_units,
                &unit.id,
            ) {
                Ok(()) => {}
                // A competing manager may own this unit already (its start
                // landed first); the deterministic attempt identity means
                // the shared write lease belongs to that same attempt —
                // back off WITHOUT touching it.
                Err(r_code_kernel::task::TransitionError::DependencyNotCompleted(_)) => {
                    return DispatchOutcome::Deferred(
                        "dependency is not completed under concurrency".to_string(),
                    )
                }
                Err(r_code_kernel::task::TransitionError::InvalidTransition { action, .. })
                    if action.contains("start non-writable or active work unit") =>
                {
                    return DispatchOutcome::Deferred(
                        "another manager already owns this work unit".to_string(),
                    )
                }
                Err(error) => return DispatchOutcome::Fatal(error.to_string()),
            }
            match ctx.store.save_task_and_events_if_revision(
                &candidate,
                vec![journal_event(
                    task_id,
                    "execution.started",
                    serde_json::json!({
                        "runId": run_id,
                        "attemptId": attempt_id,
                        "workUnitId": unit.id,
                        "snapshotId": frozen.snapshot.id().as_str(),
                        "planRevision": ctx.plan.reference().as_str(),
                    }),
                )],
                revision,
            ) {
                Ok(_) => {
                    started = Some(candidate);
                    break;
                }
                Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
                Err(error) => {
                    return DispatchOutcome::Fatal(format!(
                        "execution start lost task CAS: {error}"
                    ))
                }
            }
        }
        let Some(started_state) = started else {
            return DispatchOutcome::Fatal(
                "execution start lost task CAS after retries".to_string(),
            );
        };
        *live = started_state;
        refresh_unit_statuses(units, live);
        if let Err(error) = ctx.store.mark_work_unit_attempt_dispatched(&attempt_id) {
            return DispatchOutcome::Fatal(error.to_string());
        }
        {
            let slot = self.slot_of(task_id).await;
            let mut slot_guard = slot.lock().await;
            slot_guard.run_id = Some(run_id.clone());
            slot_guard.attempt_id = Some(attempt_id.clone());
            slot_guard.transcript_position = Some(ctx.transcript_position);
            slot_guard.guard = Some(ctx.guard.clone());
        }

        // The per-unit harness session: its own router and config, bound to
        // this unit's own frozen snapshot and attempt (E06.3).
        let mut harness_config = planning_harness_config(&frozen.snapshot, "execution", None);
        harness_config["workUnitId"] = serde_json::json!(unit.id);
        let router = Arc::new(
            HostRouter::new(
                identity.clone(),
                ctx.guard.clone(),
                ctx.grants.to_vec(),
                run_tools,
                frozen.models,
                self.processes.clone(),
                self.store.clone(),
                Arc::new(crate::plugins::IgnoreQuestions),
            )
            .with_approvals(self.approvals.clone())
            .with_transcript(ctx.transcript.clone())
            .with_artifacts(ctx.artifacts.clone())
            .with_v1_store(self.store.clone())
            .with_plan_publication(
                frozen.snapshot.clone(),
                live.contract.kind,
                live.contract.required_checks.clone(),
                false,
            ),
        );
        let Some(platform) = ctx.entry.manifest.supported_platforms.first() else {
            return DispatchOutcome::Fatal("manifest has no platform entry".to_string());
        };
        let session = match PluginSession::start(
            &ctx.entry.install_dir.join(&platform.executable),
            &platform.argv,
            identity,
            NegotiatedCapabilities {
                plugin_api: r_code_harness_protocol::ApiVersion::new(
                    ctx.entry.manifest.api_major,
                    ctx.entry.manifest.api_minor,
                ),
                host_api: crate::plugins::HOST_API,
                granted_services: ctx.grants.to_vec(),
            },
            ctx.guard.clone(),
            router.clone(),
            harness_config,
            TransportLimits::default(),
        )
        .await
        {
            Ok(session) => session,
            Err(error) => {
                // The start is durably attributed; the unit settles failed
                // without a run (same repair vocabulary as pre-E06).
                let reason = format!("启动执行 Harness 失败：{error}");
                let _ = ctx.store.settle_work_unit_attempt(&attempt_id, false);
                if let Err(error) = live.require_repair(
                    Actor::Host,
                    Some(attempt_id.clone()),
                    Some(unit.id.clone()),
                    "execution harness failed".to_string(),
                    false,
                ) {
                    return DispatchOutcome::Fatal(error.to_string());
                }
                *live = match save_run_state(
                    &self.store,
                    live,
                    vec![journal_event(
                        task_id,
                        "repair-required",
                        serde_json::json!({
                            "attemptId": attempt_id,
                            "workUnitId": unit.id,
                            "reason": "execution-harness-failed",
                        }),
                    )],
                ) {
                    Ok(state) => state,
                    Err(error) => return DispatchOutcome::Fatal(error),
                };
                refresh_unit_statuses(units, live);
                let _ = execution_tools.release();
                return DispatchOutcome::FailedUnit(reason);
            }
        };
        held_tools.insert(unit.id.clone(), execution_tools);
        {
            let slot = self.slot_of(task_id).await;
            let mut slot_guard = slot.lock().await;
            slot_guard.process = Some(session.process().clone());
        }

        // E06.3: each attempt gets its OWN execution run. The spawned task
        // drives only the session and always reaps its own process; the
        // write lease and kernel transitions stay with the dispatcher.
        let execution_input = input.clone();
        let mut execution_contract = live.contract.clone();
        execution_contract.objective = unit.description.clone();
        let attempt_for_run = attempt.clone();
        let unit_for_run = unit.clone();
        let tools_for_run = held_tools[&unit.id].clone();
        let router_for_run = router.clone();
        runs.spawn(async move {
            let outcome = match checkpoint {
                Some(checkpoint) => session
                    .resume(
                        &attempt_for_run,
                        &checkpoint,
                        std::slice::from_ref(&execution_input),
                    )
                    .await
                    .map_err(|error| (error.to_string(), false)),
                None => session
                    .start(&attempt_for_run, &execution_contract, &execution_input)
                    .await
                    .map_err(|error| (error.to_string(), false)),
            };
            let process_terminated = session.process().kill_confirmed().await;
            let outcome = if process_terminated {
                outcome
            } else {
                Err((
                    "main Harness process termination is unconfirmed".to_string(),
                    true,
                ))
            };
            UnitRunOutcome {
                unit: unit_for_run,
                attempt_id: attempt_for_run.attempt_id,
                execution_tools: tools_for_run,
                router: router_for_run,
                outcome,
                tool_failed: false,
            }
        });
        DispatchOutcome::Started
    }

    /// Settle one completed run (E06.4): the attempt's DURABLE settle first,
    /// then the kernel transition it alone authorizes. External write drift
    /// observed here re-blocks dependents instead of overwriting them.
    async fn settle_unit(
        &self,
        ctx: &WaveCtx<'_>,
        live: &mut TaskState,
        drift: &mut WaveDrift,
        done: &UnitRunOutcome,
    ) -> SettleFlow {
        let task_id = ctx.task_id;
        let unit = done.unit.clone();
        let attempt_id = done.attempt_id.clone();

        // Cancellation outranks run outcomes (pre-E06 ordering).
        if ctx.guard.is_cancelled() {
            return SettleFlow::Cancelled;
        }
        // Repair-class run failure: durable settle, then kernel repair.
        if let Err((reason, unavailable)) = &done.outcome {
            if let Err(error) = ctx.store.settle_work_unit_attempt(&attempt_id, false) {
                return SettleFlow::Fatal(error.to_string());
            }
            let flow = self.fail_unit(
                live,
                task_id,
                &unit,
                &attempt_id,
                reason,
                *unavailable,
                done,
            );
            return self.finish_settle(live, ctx, drift, unit, attempt_id, done, flow);
        }
        if done.tool_failed {
            if let Err(error) = ctx.store.settle_work_unit_attempt(&attempt_id, false) {
                return SettleFlow::Fatal(error.to_string());
            }
            let flow = self.fail_unit(
                live,
                task_id,
                &unit,
                &attempt_id,
                "execution tool reported a conflict or failure",
                false,
                done,
            );
            return self.finish_settle(live, ctx, drift, unit, attempt_id, done, flow);
        }
        let proposals = done
            .router
            .recorded_proposals
            .lock()
            .expect("proposals")
            .clone();
        if proposals.len() != 1
            || proposals[0].kind != r_code_harness_protocol::services::ProposalKind::Implementation
            || proposals[0].candidate_digest.is_some()
        {
            if let Err(error) = ctx.store.settle_work_unit_attempt(&attempt_id, false) {
                return SettleFlow::Fatal(error.to_string());
            }
            let flow = self.fail_unit(
                live,
                task_id,
                &unit,
                &attempt_id,
                "execution did not produce one host-verifiable proposal",
                false,
                done,
            );
            return self.finish_settle(live, ctx, drift, unit, attempt_id, done, flow);
        }
        let candidate = match CandidateManifest::capture(ctx.binding).and_then(|candidate| {
            candidate.verify_live(ctx.binding)?;
            Ok(candidate)
        }) {
            Ok(candidate) => candidate,
            Err(error) => {
                if let Err(error) = ctx.store.settle_work_unit_attempt(&attempt_id, false) {
                    return SettleFlow::Fatal(error.to_string());
                }
                let flow = self.fail_unit(
                    live,
                    task_id,
                    &unit,
                    &attempt_id,
                    &format!("无法捕获稳定候选：{error}"),
                    false,
                    done,
                );
                return self.finish_settle(live, ctx, drift, unit, attempt_id, done, flow);
            }
        };
        if let Err(error) = live.begin_verification(
            Actor::Host,
            &attempt_id,
            &unit.id,
            candidate.candidate_id.clone(),
        ) {
            return SettleFlow::Fatal(error.to_string());
        }
        *live = match save_run_state(
            &self.store,
            live,
            vec![journal_event(
                task_id,
                "verifying",
                serde_json::json!({
                    "attemptId": attempt_id,
                    "workUnitId": unit.id,
                    "candidateDigest": candidate.candidate_id,
                }),
            )],
        ) {
            Ok(state) => state,
            Err(error) => return SettleFlow::Fatal(error),
        };
        let verification = self
            .verify_candidate(ctx.binding, &candidate, &unit, &attempt_id, live)
            .await;
        if ctx.guard.is_cancelled() {
            return SettleFlow::Cancelled;
        }
        match verification {
            Err((reason, unavailable)) => {
                // E06.4: the unit completes only from its own attempt's
                // durable settle — settled-failed first, then the kernel.
                if let Err(error) = ctx.store.settle_work_unit_attempt(&attempt_id, false) {
                    return SettleFlow::Fatal(error.to_string());
                }
                if let Err(error) = live.require_repair(
                    Actor::Host,
                    Some(attempt_id.clone()),
                    Some(unit.id.clone()),
                    reason,
                    unavailable,
                ) {
                    return SettleFlow::Fatal(error.to_string());
                }
                *live = match save_run_state(
                    &self.store,
                    live,
                    vec![journal_event(
                        task_id,
                        "repair-required",
                        serde_json::json!({
                            "attemptId": attempt_id,
                            "workUnitId": unit.id,
                            "candidateDigest": candidate.candidate_id,
                        }),
                    )],
                ) {
                    Ok(state) => state,
                    Err(error) => return SettleFlow::Fatal(error),
                };
                self.finish_settle(
                    live,
                    ctx,
                    drift,
                    unit,
                    attempt_id,
                    done,
                    SettleFlow::Continue,
                )
            }
            Ok(requirements) => {
                let check_ids = requirements
                    .iter()
                    .map(|requirement| requirement.check_id.clone())
                    .collect::<Vec<_>>();
                if let Err(error) = ctx.store.settle_work_unit_attempt(&attempt_id, true) {
                    return SettleFlow::Fatal(error.to_string());
                }
                if let Err(error) =
                    live.finish_verification(Actor::Host, &attempt_id, &unit.id, &requirements)
                {
                    return SettleFlow::Fatal(error.to_string());
                }
                // `review-ready` stays a TASK-level signal: a per-unit
                // settle that leaves the wave open journals a unit event.
                let event_kind = if matches!(live.execution, TaskExecution::ReviewReady { .. }) {
                    "review-ready"
                } else {
                    "unit.completed"
                };
                *live = match save_run_state(
                    &self.store,
                    live,
                    vec![journal_event(
                        task_id,
                        event_kind,
                        serde_json::json!({
                            "attemptId": attempt_id,
                            "workUnitId": unit.id,
                            "candidateDigest": candidate.candidate_id,
                            "checks": check_ids,
                        }),
                    )],
                ) {
                    Ok(state) => state,
                    Err(error) => return SettleFlow::Fatal(error),
                };
                self.finish_settle(
                    live,
                    ctx,
                    drift,
                    unit,
                    attempt_id,
                    done,
                    SettleFlow::Continue,
                )
            }
        }
    }

    /// The shared tail of every settle: re-block dependents on external
    /// write drift (E06.2), then release the unit's write lease — an
    /// unreleasable lease forces repair exactly like the pre-E06 path.
    #[allow(clippy::too_many_arguments)]
    fn finish_settle(
        &self,
        live: &mut TaskState,
        ctx: &WaveCtx<'_>,
        drift: &mut WaveDrift,
        unit: WorkUnit,
        attempt_id: String,
        done: &UnitRunOutcome,
        flow: SettleFlow,
    ) -> SettleFlow {
        // E06.2: external edits to the unit's write scope during flight
        // re-block its dependents — never overwrite (INV-10).
        let external = drift.external_write_drift(ctx.store, &unit, &attempt_id);
        if !external.is_empty() {
            let reason = format!(
                "dependency {} write scope drifted externally: {}",
                unit.id,
                external.join(", ")
            );
            for dependent in dependents_of(live, &unit) {
                match self.reblock_unit(live, &dependent, &reason) {
                    Ok(state) => *live = state,
                    Err(error) => return SettleFlow::Fatal(error),
                }
            }
        }
        let release = done.execution_tools.release();
        if !matches!(release, Ok(true)) {
            if let Err(error) = live.require_repair(
                Actor::Host,
                Some(attempt_id.clone()),
                Some(unit.id.clone()),
                "execution lease could not be released".to_string(),
                true,
            ) {
                return SettleFlow::Fatal(error.to_string());
            }
            *live = match save_run_state(
                &self.store,
                live,
                vec![journal_event(
                    ctx.task_id,
                    "repair-required",
                    serde_json::json!({
                        "attemptId": attempt_id,
                        "workUnitId": unit.id,
                        "reason": "execution lease could not be released",
                    }),
                )],
            ) {
                Ok(state) => state,
                Err(error) => return SettleFlow::Fatal(error),
            };
            return SettleFlow::Failed("execution lease could not be released".to_string());
        }
        flow
    }

    /// Mark one unit failed (the attempt row is already durably settled).
    #[allow(clippy::too_many_arguments)]
    fn fail_unit(
        &self,
        live: &mut TaskState,
        task_id: &str,
        unit: &WorkUnit,
        attempt_id: &str,
        reason: &str,
        unavailable: bool,
        done: &UnitRunOutcome,
    ) -> SettleFlow {
        if let Err(error) = live.require_repair(
            Actor::Host,
            Some(attempt_id.to_string()),
            Some(unit.id.clone()),
            reason.to_string(),
            unavailable,
        ) {
            return SettleFlow::Fatal(error.to_string());
        }
        let event_reason = if unavailable {
            "harness-termination-unconfirmed"
        } else if done.tool_failed {
            "tool-failed"
        } else if reason.starts_with("无法捕获稳定候选") {
            "execution attempt failed"
        } else {
            "execution-harness-failed"
        };
        let event_reason = if reason == "execution did not produce one host-verifiable proposal" {
            "invalid-implementation-proposal"
        } else {
            event_reason
        };
        *live = match save_run_state(
            &self.store,
            live,
            vec![journal_event(
                task_id,
                "repair-required",
                serde_json::json!({
                    "attemptId": attempt_id,
                    "workUnitId": unit.id,
                    "reason": event_reason,
                }),
            )],
        ) {
            Ok(state) => state,
            Err(error) => return SettleFlow::Fatal(error),
        };
        SettleFlow::Failed(reason.to_string())
    }

    /// Re-block one never-started unit (read-set drift, or a dependency's
    /// write scope drifted externally): it settles failed and blocks exactly
    /// its dependents — the kernel's existing failure vocabulary.
    fn reblock_unit(
        &self,
        live: &TaskState,
        unit_id: &str,
        reason: &str,
    ) -> Result<TaskState, String> {
        let mut next = live.clone();
        next.require_repair(
            Actor::Host,
            None,
            Some(unit_id.to_string()),
            reason.to_string(),
            false,
        )
        .map_err(|error| error.to_string())?;
        let task_id = live.contract.task_id.clone();
        save_run_state(
            &self.store,
            &next,
            vec![journal_event(
                &task_id,
                "repair-required",
                serde_json::json!({
                    "workUnitId": unit_id,
                    "reason": reason,
                }),
            )],
        )
    }

    /// Settle the whole wave as cancelled: terminal verdict, one
    /// run.cancelled journal event, every held write lease released.
    async fn settle_wave_cancelled(
        &self,
        task_id: &str,
        input: &InputMessage,
        live: &mut TaskState,
        ctx: &WaveCtx<'_>,
        held_tools: &mut HashMap<String, Arc<ExecutionToolService>>,
        awaiting: &mut VecDeque<UnitRunOutcome>,
    ) -> Result<(), String> {
        // Infallible cleanup first: whatever the kernel transition does,
        // no write lease survives a cancelled wave.
        for tools in held_tools.values() {
            let _ = tools.release();
        }
        for pending in awaiting.drain(..) {
            let _ = ctx
                .store
                .settle_work_unit_attempt(&pending.attempt_id, false);
            let _ = pending.execution_tools.release();
        }
        ctx.transcript
            .truncate_to(ctx.transcript_position)
            .map_err(|_| "取消执行后无法恢复 transcript".to_string())?;
        live.cancel(Actor::Host, 1, "user requested cancel")
            .map_err(|error| error.to_string())?;
        let slot = self.slot_of(task_id).await;
        let (run_id, attempt_id) = {
            let slot_guard = slot.lock().await;
            (slot_guard.run_id.clone(), slot_guard.attempt_id.clone())
        };
        *live = save_run_state(
            &self.store,
            live,
            vec![journal_event(
                task_id,
                "run.cancelled",
                serde_json::json!({
                    "runId": run_id,
                    "attemptId": attempt_id,
                    "reason": "user requested cancel",
                }),
            )],
        )?;
        self.kernel_tasks
            .acknowledge(task_id, &input.message_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

/// Pending units that depend on `unit`.
fn dependents_of(live: &TaskState, unit: &WorkUnit) -> Vec<String> {
    live.work_units
        .iter()
        .filter(|other| {
            other.status == WorkUnitStatus::Pending && other.dependencies.contains(&unit.id)
        })
        .map(|other| other.id.clone())
        .collect()
}
