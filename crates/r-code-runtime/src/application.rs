//! The headless ApplicationService: one composition point inside the
//! r-code-service daemon exposing plugin management, task lifecycle,
//! harness sessions and the durable event journal to every client
//! (GUI/TUI/MCP). Non-UI composition moved out of any CommandState; all
//! task work flows through the same lifecycle API regardless of which
//! harness (Native, Codex or third-party) a task pins.

pub mod context_view;
pub mod effect_approvals;
pub mod review_flow;

pub use effect_approvals::{EffectApprovalView, StoreEffectApprovals, EFFECT_APPROVE_SCOPE};

use crate::plugins::catalog::CatalogEntry;
use crate::plugins::package::InstalledPackage;
use crate::plugins::{ApprovalStore, PluginCatalog, DEFAULT_DECISION_TIMEOUT};
use crate::process_guard::BootIdentity;
use crate::profile::RuntimeProfile;
use crate::run_manager::{envelope_of, RunManager, RunServicePaths};
use crate::services::artifacts::sha256_hex;
use crate::services::authorization::{
    AuthorizationService, EffectivePermissions, WorkspaceCapability,
};
use crate::services::processes::ManagedProcessService;
use crate::services::review::ReviewError;
use crate::services::settings_store::{RunProviderResolution, SettingsStore};
use r_code_harness_protocol::services::WorkUnitWire;
use r_code_harness_protocol::{EventEnvelope, HostService, PackageRef};
use r_code_kernel::plans::{PlanApproval, PlanApprovalActor, PLAN_APPROVE_SCOPE};
use r_code_kernel::ports::JournalStore as _;
use r_code_kernel::task::{
    Actor, ModelRoute, PlanRevisionRef, TaskExecution, TaskKind, TaskPreferences, TaskState,
};
use r_code_kernel::tasks::TaskService as KernelTaskService;
use r_code_store::v1::operations::{QuarantineProofStatus, QuarantineTreeState};
use r_code_store::v1::V1Store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Errors from the application service.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplicationError {
    #[error("io/store failure: {0}")]
    Store(String),
    #[error("plugin failure: {0}")]
    Plugin(String),
    #[error("task failure: {0}")]
    Task(String),
    #[error("session failure: {0}")]
    Session(String),
    #[error("settings failure: {0}")]
    Settings(String),
}

/// Whether an empty Provider settings document may use the injected model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionPolicy {
    StrictDaemon,
    AllowInjectedModelFallback,
}

impl CompositionPolicy {
    fn allows_injected_model_fallback(self) -> bool {
        matches!(self, Self::AllowInjectedModelFallback)
    }
}

/// One task row for `task.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSummaryView {
    pub task_id: String,
    pub title: String,
    pub kind: String,
    pub state: String,
    pub running: bool,
    pub workspace_path: Option<String>,
    pub updated_at_ms: i64,
}

/// One run row inside a task detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRunView {
    pub run_id: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
}

/// The `task.detail` projection frontends render from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDetailView {
    pub task_id: String,
    pub title: String,
    pub kind: String,
    pub objective: String,
    pub state: String,
    pub running: bool,
    pub model_route: Option<ModelRoute>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub engine: String,
    pub harness_id: String,
    pub inference: Option<serde_json::Value>,
    pub mode: Option<String>,
    pub workspace_path: Option<String>,
    /// FR-7: the task-frozen memory handoff projection — same hash the
    /// daemon PromptSnapshot merged, so desktop Codex delegations reuse the
    /// frozen snapshot instead of recomputing one.
    pub memory: Option<TaskMemoryView>,
    pub runs: Vec<TaskRunView>,
    pub usage: TaskUsageView,
}

/// Credential-free runtime-v1 plan projection. Provider, prompt and
/// permission material stays represented by immutable digests in storage and
/// is intentionally not expanded here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanView {
    pub task_id: String,
    pub revision: u64,
    pub revision_hash: String,
    pub work_units: Vec<WorkUnitWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<PlanApproval>,
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuarantineRemediation {
    AwaitTerminationProof,
    RebootRequired,
    AwaitingProofRetry,
    ManualRecoveryRequired,
}

/// Credential-free, handle-free projection of one blocked writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineView {
    pub tree_ref: String,
    pub workspace_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_identity: Option<String>,
    pub profile_ref: String,
    pub state: QuarantineTreeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership_epoch: Option<u64>,
    pub owner_fingerprint: String,
    pub reason_code: String,
    pub reason_digest: String,
    pub proof_status: QuarantineProofStatus,
    pub legacy_reboot_required: bool,
    pub current_boot_changed: bool,
    pub corrupt: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<i64>,
    pub remediation: QuarantineRemediation,
}

/// Credential-free, path-free projection of one persisted safety report
/// (P12). Raw material JSON is never expanded: helper/executable paths and
/// boot identity stay represented by the report reference and digests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyReportView {
    pub capability: String,
    pub report_ref: String,
    pub status: String,
    pub material_digest: String,
    /// Whether the report's boot identity equals the daemon's current one.
    pub boot_matches_current: bool,
    pub created_at_ms: i64,
}

/// Redacted outcome of one quarantine retry (P11R): the tree reference
/// and proof reference are digests; refusal reasons are stable strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineRetryView {
    pub tree_ref: String,
    pub cleared: bool,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof_ref: Option<String>,
}

fn refusal_reason(reason: r_code_store::v1::operations::QuarantineRetryRefusal) -> &'static str {
    use r_code_store::v1::operations::QuarantineRetryRefusal;
    match reason {
        QuarantineRetryRefusal::NotQuarantined => "not-quarantined",
        QuarantineRetryRefusal::SameBootLegacy => "same-boot-legacy",
        QuarantineRetryRefusal::LegacyCorrupt => "legacy-corrupt",
        QuarantineRetryRefusal::UnverifiableNoProof => "unverifiable-no-proof",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewActionContext {
    pub action_id: String,
    pub expected_task_revision: u64,
    pub candidate_digest: String,
    pub actor_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewActionResult {
    pub task_id: String,
    pub action_id: String,
    pub outcome: String,
    pub candidate_digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnverifiedOverrideInput {
    pub context: ReviewActionContext,
    pub reason: String,
    pub checks: Vec<String>,
}

/// Fully specified task creation used by the daemon's additive v1 API.
pub struct CreateTaskInput {
    pub task_id: String,
    pub objective: String,
    pub title: Option<String>,
    pub kind: TaskKind,
    pub required_checks: Vec<String>,
    /// Desktop-frozen memory snapshot (FR-7); None on ownerless paths (TUI).
    pub memory: Option<r_code_kernel::task::FrozenMemoryHandoff>,
    pub preferences: TaskPreferences,
    pub harness_id: Option<String>,
}

/// Tri-state field used by daemon preference patches. `Unchanged` is
/// distinct from an explicit JSON `null`, which clears an optional value.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum PatchField<T> {
    #[default]
    Unchanged,
    Clear,
    Set(T),
}

impl<T: Clone> PatchField<T> {
    fn apply_to(&self, target: &mut Option<T>) {
        match self {
            Self::Unchanged => {}
            Self::Clear => *target = None,
            Self::Set(value) => *target = Some(value.clone()),
        }
    }
}

/// Partial update applied to the latest task aggregate on every CAS retry.
/// This prevents independent preference edits from overwriting one another.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TaskPreferencesPatch {
    pub model_route: PatchField<ModelRoute>,
    pub legacy_model: PatchField<String>,
    pub inference: PatchField<serde_json::Value>,
    pub mode: PatchField<String>,
    pub system_prompt: PatchField<String>,
    pub workspace_path: PatchField<String>,
    pub require_desktop_confirm: Option<bool>,
    pub harness_id: PatchField<String>,
}

impl TaskPreferencesPatch {
    /// Compatibility adapter for the legacy typed setter, whose callers
    /// historically constructed sparse preference documents. Only present
    /// values participate so concurrent independent updates can merge.
    pub fn merge_present(preferences: TaskPreferences) -> Self {
        Self {
            model_route: patch_if_some(preferences.model_route),
            legacy_model: patch_if_some(preferences.model),
            inference: patch_if_some(preferences.inference),
            mode: patch_if_some(preferences.mode),
            system_prompt: patch_if_some(preferences.system_prompt),
            workspace_path: patch_if_some(preferences.workspace_path),
            require_desktop_confirm: preferences.require_desktop_confirm.then_some(true),
            harness_id: PatchField::Unchanged,
        }
    }

    pub fn apply_to(&self, preferences: &mut TaskPreferences) {
        self.model_route.apply_to(&mut preferences.model_route);
        if !matches!(self.model_route, PatchField::Unchanged)
            && matches!(self.legacy_model, PatchField::Unchanged)
        {
            preferences.model = None;
        }
        self.legacy_model.apply_to(&mut preferences.model);
        if !matches!(self.legacy_model, PatchField::Unchanged)
            && matches!(self.model_route, PatchField::Unchanged)
        {
            preferences.model_route = None;
        }
        self.inference.apply_to(&mut preferences.inference);
        self.mode.apply_to(&mut preferences.mode);
        self.system_prompt.apply_to(&mut preferences.system_prompt);
        self.workspace_path
            .apply_to(&mut preferences.workspace_path);
        if let Some(required) = self.require_desktop_confirm {
            preferences.require_desktop_confirm = required;
        }
    }

    fn requested_harness(&self) -> Option<&str> {
        match &self.harness_id {
            PatchField::Unchanged => None,
            PatchField::Clear => Some(crate::run_manager::DEFAULT_HARNESS_ID),
            PatchField::Set(harness_id) => Some(harness_id),
        }
    }
}

fn patch_if_some<T>(value: Option<T>) -> PatchField<T> {
    match value {
        Some(value) => PatchField::Set(value),
        None => PatchField::Unchanged,
    }
}

/// Aggregated usage across runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskUsageView {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// FR-7: frozen memory snapshot projection on task.detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskMemoryView {
    pub rendered: String,
    pub snapshot_hash: String,
    pub entry_ids: Vec<String>,
}

/// P30 wire projections (camelCase) for the RPC git reads.
#[derive(serde::Serialize)]
pub struct GitStatusProjection {
    pub entries: Vec<GitEntryProjection>,
    pub truncated: bool,
}

#[derive(serde::Serialize)]
pub struct GitEntryProjection {
    pub path: String,
    pub change: String,
    pub stage: String,
}

#[derive(serde::Serialize)]
pub struct GitLogProjection {
    pub commits: Vec<String>,
    pub truncated: bool,
}

/// P24B: the activation readiness of one composed service. Recovery is a
/// PRECONDITION — an incomplete process effect anywhere in the store means
/// the service is not ready and grants nothing; the platform report is the
/// OTHER precondition, and on a host whose report is not Activated the
/// granted set is empty (SafeDisabled grants nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationReadiness {
    /// Recovery completed: no incomplete process-effect operations remain.
    pub recovered: bool,
    /// The platform gate's verdict for this boot.
    pub activation: crate::services::sandbox::SafetyActivation,
    /// The capabilities this service may grant. Empty unless BOTH
    /// preconditions hold — on this wave's honest Unsupported reports it
    /// is empty on every host, and that is the truthful result.
    pub granted_capabilities: Vec<&'static str>,
    /// E10: in-flight execution attempts the startup pass reconciled
    /// (resume-once plus quarantine) — published before any new dispatch.
    pub reconciled_attempts: usize,
}

impl ActivationReadiness {
    /// P24B: compute the readiness AFTER recovery, under the exact report.
    /// Nothing about it reads mutable settings.
    pub fn evaluate(
        store: &V1Store,
        boot_identity: &str,
    ) -> Result<Self, crate::services::process_effects::EnvelopeError> {
        // Recovery first: incomplete effects quarantine (never rerun) and
        // their ids prove whether this boot arrived clean.
        let quarantined = crate::services::process_effects::recover_incomplete_effects(store)?;
        let activation = crate::services::sandbox::platform_activation_gate(store, boot_identity);
        let granted = match &activation {
            crate::services::sandbox::SafetyActivation::Activated { .. } => {
                // The exact per-capability predicates live with the grant
                // sites (run manager); the readiness only publishes that
                // the platform proved the sandbox the capabilities need.
                vec!["process.noxworkspace", "process.scratchonly"]
            }
            crate::services::sandbox::SafetyActivation::NotActivated { .. } => Vec::new(),
        };
        Ok(Self {
            recovered: quarantined.is_empty(),
            activation,
            granted_capabilities: granted,
            reconciled_attempts: 0,
        })
    }
}

/// The composed application surface.
pub struct ApplicationService {
    store: Arc<V1Store>,
    kernel_tasks: Arc<KernelTaskService>,
    catalog: Arc<PluginCatalog>,
    runs: Arc<RunManager>,
    settings: Arc<SettingsStore>,
    approvals: Arc<ApprovalStore>,
    processes: Arc<dyn r_code_kernel::ports::ProcessService>,
    artifact_tasks_root: PathBuf,
    boot_identity: BootIdentity,
    nonce: String,
    /// P24B: the activation readiness evaluated AFTER recovery, before the
    /// service was handed to any ingress.
    readiness: ActivationReadiness,
}

impl ApplicationService {
    /// Compatibility composition for tests and embedded callers that
    /// deliberately inject a model when Provider settings are empty. The
    /// service daemon must use [`Self::compose_with_policy`] with
    /// [`CompositionPolicy::StrictDaemon`].
    pub fn compose(
        profile: &RuntimeProfile,
        models: Arc<dyn r_code_kernel::ports::ModelService>,
        tools: Arc<dyn r_code_kernel::ports::ToolService>,
    ) -> Result<Self, ApplicationError> {
        Self::compose_with_policy(
            profile,
            models,
            tools,
            CompositionPolicy::AllowInjectedModelFallback,
        )
    }

    /// Compose the real daemon or an explicitly injected test host.
    pub fn compose_with_policy(
        profile: &RuntimeProfile,
        models: Arc<dyn r_code_kernel::ports::ModelService>,
        tools: Arc<dyn r_code_kernel::ports::ToolService>,
        policy: CompositionPolicy,
    ) -> Result<Self, ApplicationError> {
        Self::compose_with_policy_and_approval_timeout(
            profile,
            models,
            tools,
            DEFAULT_DECISION_TIMEOUT,
            policy,
        )
    }

    /// [`Self::compose`] with an explicit approval decision timeout
    /// (undecided requests deny after this long; tests use short values).
    pub fn compose_with_approval_timeout(
        profile: &RuntimeProfile,
        models: Arc<dyn r_code_kernel::ports::ModelService>,
        tools: Arc<dyn r_code_kernel::ports::ToolService>,
        approval_timeout: std::time::Duration,
    ) -> Result<Self, ApplicationError> {
        Self::compose_with_policy_and_approval_timeout(
            profile,
            models,
            tools,
            approval_timeout,
            CompositionPolicy::AllowInjectedModelFallback,
        )
    }

    pub fn compose_with_policy_and_approval_timeout(
        profile: &RuntimeProfile,
        models: Arc<dyn r_code_kernel::ports::ModelService>,
        tools: Arc<dyn r_code_kernel::ports::ToolService>,
        approval_timeout: std::time::Duration,
        policy: CompositionPolicy,
    ) -> Result<Self, ApplicationError> {
        let store = V1Store::open(&profile.database_path())
            .map_err(|error| ApplicationError::Store(error.to_string()))?;
        let boot_identity = BootIdentity::current()
            .map_err(|error| ApplicationError::Store(format!("boot identity: {error}")))?;
        store
            .migrate_legacy_writer_barriers(boot_identity.as_str())
            .map_err(|error| {
                ApplicationError::Store(format!("legacy writer quarantine migration: {error}"))
            })?;
        let store = Arc::new(store);
        // P27: recover every process-effect operation a crash left
        // incomplete BEFORE any write ingress — quarantined, never
        // re-executed (the frozen row is the only input).
        // P24B/P27: recovery is the FIRST write-ingress precondition, and
        // the activation readiness is evaluated right after it — under the
        // exact platform report, before the service is constructed. On a
        // host whose report is not Activated the granted set is empty.
        let mut readiness = ActivationReadiness::evaluate(&store, boot_identity.as_str())
            .map_err(|error| ApplicationError::Store(format!("effect recovery: {error}")))?;
        let kernel_tasks = Arc::new(KernelTaskService::new(store.clone()));
        let catalog = Arc::new(PluginCatalog::new(profile.plugins_root(), store.clone()));
        let settings = Arc::new(SettingsStore::for_profile(profile));
        let approvals = Arc::new(ApprovalStore::new(store.clone(), approval_timeout));
        let artifact_tasks_root = profile.blobs_root().join("tasks");
        // P24A: the daemon's plugin-facing process surface is the REAL
        // supervised service in its dormant shape, not a testing fake. No
        // supervisor binding is attached, so every launch refuses before a
        // process exists, and permissions sit at the read-only floor. The
        // platform binding and per-run permissions arrive with P24B's
        // recovery-gated activation; this task opens no capability.
        let processes: Arc<dyn r_code_kernel::ports::ProcessService> =
            Arc::new(ManagedProcessService::new(
                Arc::new(AuthorizationService::new()),
                EffectivePermissions::read_only(),
                WorkspaceCapability::ReadOnly {
                    root: profile
                        .workspaces_root()
                        .to_string_lossy()
                        .replace('\\', "/"),
                },
                None,
            ));
        let runs = RunManager::new_with_injected_model_fallback_and_paths(
            store.clone(),
            kernel_tasks.clone(),
            catalog.clone(),
            models,
            tools,
            processes.clone(),
            settings.clone(),
            approvals.clone(),
            policy.allows_injected_model_fallback(),
            RunServicePaths::new(
                profile.harness_v1_root().join("transcripts"),
                artifact_tasks_root.clone(),
            ),
        );
        // E10: the whole chain reconciles right after effect recovery and
        // before this service accepts any write ingress — every in-flight
        // attempt resolves to exactly one of resume-once or quarantine,
        // families follow their attempts, and the readiness publishes the
        // reconciled count ahead of any new dispatch.
        let (resume_once, quarantined_chains) = runs
            .reconcile_in_flight_attempts()
            .map_err(|error| ApplicationError::Store(format!("chain reconciliation: {error}")))?;
        readiness.reconciled_attempts = resume_once + quarantined_chains;
        Ok(Self {
            store,
            kernel_tasks,
            catalog,
            runs,
            settings,
            approvals,
            processes,
            artifact_tasks_root,
            boot_identity,
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            readiness,
        })
    }

    /// The daemon-wide process surface (P24A): the real supervised service in
    /// its dormant, refuse-everything shape. Read-only accessor for
    /// diagnostics and composition tests.
    pub fn process_service(&self) -> &Arc<dyn r_code_kernel::ports::ProcessService> {
        &self.processes
    }

    /// This daemon instance's identity.
    pub fn instance_nonce(&self) -> &str {
        &self.nonce
    }

    /// P30: bounded read-only git projections for RPC clients — status,
    /// log and diff over the restricted P29 reader / pure bounded diff.
    /// Only these three are public; there is no generic git surface.
    pub fn git_read_projection(
        &self,
        projection: &str,
        git_dir: &std::path::Path,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        let reader = crate::services::git_read::open_read_only(git_dir)
            .map_err(|error| error.to_string())?;
        match projection {
            "status" => {
                let report = reader
                    .status(limit.min(crate::services::git_read::MAX_STATUS_ENTRIES))
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(GitStatusProjection {
                    entries: report
                        .entries
                        .into_iter()
                        .map(|entry| GitEntryProjection {
                            path: entry.path,
                            change: format!("{:?}", entry.change),
                            stage: format!("{:?}", entry.stage),
                        })
                        .collect(),
                    truncated: report.truncated,
                })
                .map_err(|error| error.to_string())
            }
            "log" => {
                let report = reader
                    .log(limit.min(crate::services::git_read::MAX_LOG_COMMITS))
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(GitLogProjection {
                    commits: report.commits,
                    truncated: report.truncated,
                })
                .map_err(|error| error.to_string())
            }
            "diff" => Err(
                "git diff is a pure tool computation (git_diff): supply contents, not a repository"
                    .to_string(),
            ),
            other => Err(format!("unknown git projection: {other}")),
        }
    }

    /// P24B: the activation readiness computed before ingress. On an
    /// unactivated host `granted_capabilities` is empty — SafeDisabled
    /// grants nothing — and `recovered` proves the recovery precondition.
    pub fn activation_readiness(&self) -> &ActivationReadiness {
        &self.readiness
    }

    /// The run manager (chat engine).
    pub fn runs(&self) -> &Arc<RunManager> {
        &self.runs
    }

    /// The settings store.
    pub fn settings(&self) -> &Arc<SettingsStore> {
        &self.settings
    }

    /// FR-1 (M1a-06): stored instruction settings for a workspace root.
    pub fn context_settings(
        &self,
        canonical_root: &str,
    ) -> Option<r_code_store::v1::ContextSettingsRecord> {
        self.store
            .context_settings(
                &crate::services::project_instructions::workspace_settings_key(canonical_root),
            )
            .ok()
            .flatten()
    }

    /// FR-1 (M1a-06): validate and persist instruction settings for a
    /// workspace root (the desktop settings surface calls this over RPC).
    pub fn update_context_settings(
        &self,
        canonical_root: &str,
        record: r_code_store::v1::ContextSettingsRecord,
    ) -> Result<(), ApplicationError> {
        let settings = crate::services::project_instructions::InstructionSettings {
            injection_enabled: record.injection_enabled,
            total_budget_bytes: record.total_budget_bytes as usize,
            jit_allowance_bytes: record.jit_allowance_bytes as usize,
            fallback_names: if record.fallback_names.is_empty() {
                crate::services::project_instructions::InstructionSettings::default().fallback_names
            } else {
                record.fallback_names.clone()
            },
        };
        settings.validate().map_err(|error| {
            ApplicationError::Task(format!("invalid context settings: {error}"))
        })?;
        self.store
            .save_context_settings(
                &crate::services::project_instructions::workspace_settings_key(canonical_root),
                &record,
            )
            .map_err(|error| ApplicationError::Store(error.to_string()))
    }

    /// The shared approval store (pending-op index over the journal).
    pub fn approvals(&self) -> &Arc<ApprovalStore> {
        &self.approvals
    }

    /// Rebuild pending approvals from the journal (daemon restart): an
    /// undecided operation stays decidable, its state projected from the
    /// `approval.requested`/`approval.decided` events.
    pub async fn rebuild_approvals(&self) {
        let events =
            r_code_kernel::ports::JournalStore::read_events(&*self.store, 0, u32::MAX).await;
        self.approvals.rebuild_from_events(&events).await;
    }

    // -- approvals (RA2) ---------------------------------------------------

    /// `approvals.list`: undecided operations, `createdSeq` ascending.
    /// P19B-R: effect-bound rows carry their exact `effect` binding so one
    /// canonical payload reaches every client.
    pub async fn approvals_list(&self) -> Vec<serde_json::Value> {
        let now = now_ms();
        self.approvals
            .pending()
            .await
            .into_iter()
            .map(|row| {
                let age_ms = if row.created_ms > 0 {
                    now - row.created_ms
                } else {
                    0
                };
                let mut value = serde_json::json!({
                    "opId": row.op_id,
                    "summary": row.summary,
                    "runId": row.run_id,
                    "taskId": row.task_id,
                    "createdSeq": row.created_seq,
                    "createdMs": row.created_ms,
                    "ageMs": age_ms.max(0),
                });
                if let Some(binding) = &row.effect {
                    value["effect"] = serde_json::to_value(binding).unwrap_or_default();
                }
                value
            })
            .collect()
    }

    /// `approvals.decide`: decide a pending operation with the connection
    /// identity as both actor and session. See
    /// [`Self::approvals_decide_with_session`] for the full contract.
    pub async fn approvals_decide(
        &self,
        params: &serde_json::Value,
        decided_by: &str,
        source: CommandSource,
    ) -> Result<serde_json::Value, ApplicationError> {
        self.approvals_decide_with_session(params, decided_by, decided_by, source)
            .await
    }

    /// `approvals.decide` with the authenticated session identity kept
    /// distinct from the actor (the daemon passes the command id, matching
    /// `plan.approve` attribution). `decided_by` is the *connection
    /// identity*, never a param — audit integrity. A remote decision (R12)
    /// on a task flagged `require_desktop_confirm` is refused with
    /// `needs_desktop_confirm`. Codes: `approval_unknown` (never implicitly
    /// created), `approval_conflict` (first decision wins). P19B-R: a
    /// GRANTED effect-bound decision re-validates the binding against the
    /// approved plan BEFORE journaling — a stale pending request can neither
    /// consume itself nor mint an approval — then materializes exactly one
    /// immutable approval; denied/expired never persist one.
    pub async fn approvals_decide_with_session(
        &self,
        params: &serde_json::Value,
        decided_by: &str,
        session_id: &str,
        source: CommandSource,
    ) -> Result<serde_json::Value, ApplicationError> {
        let operation_id = params["operationId"]
            .as_str()
            .ok_or_else(|| ApplicationError::Session("missing operationId".into()))?;
        let decision = match params["decision"].as_str() {
            Some("granted") => r_code_harness_protocol::ApprovalDecision::Granted,
            Some("denied") => r_code_harness_protocol::ApprovalDecision::Denied,
            other => {
                return Err(ApplicationError::Session(format!(
                    "decision must be \"granted\"|\"denied\", got {other:?}"
                )))
            }
        };
        if source == CommandSource::Remote {
            // High-sensitivity tasks keep the final say on the desktop.
            let op = self
                .approvals
                .pending()
                .await
                .into_iter()
                .find(|row| row.op_id == operation_id);
            if let Some(row) = op {
                if let Some(task) = self.store.load_task(&row.task_id).await {
                    if task.preferences.require_desktop_confirm {
                        return Err(ApplicationError::Session(
                            "needs_desktop_confirm: this task requires a desktop approval".into(),
                        ));
                    }
                }
            }
        }
        // P19B-R: resolve the exact effect authority this operation commits
        // to (host-bound at request time; a plugin can never forge one).
        let effect = if decision == r_code_harness_protocol::ApprovalDecision::Granted {
            let context = self.approvals.effect_context(operation_id).await;
            if let Some(context) = &context {
                // An already-materialized exact approval answers replays
                // idempotently even when the plan legitimately moved on
                // after the grant; otherwise the binding must still match
                // the currently-approved plan head.
                if self
                    .find_active_effect(&context.task_id, &context.binding)?
                    .is_none()
                {
                    self.validate_effect_binding_current(&context.task_id, &context.binding)?;
                }
            }
            context
        } else {
            None
        };
        match self
            .approvals
            .decide(operation_id, decision, decided_by)
            .await
        {
            Ok(record) => {
                let mut response = serde_json::json!({
                    "operationId": operation_id,
                    "decision": match record.decision {
                        r_code_harness_protocol::ApprovalDecision::Granted => "granted",
                        _ => "denied",
                    },
                    "decidedBy": record.decided_by,
                    "decidedSeq": record.decided_seq,
                });
                if let Some(context) = &effect {
                    let approval = self.materialize_effect_approval(
                        operation_id,
                        context,
                        decided_by,
                        session_id,
                    )?;
                    response["approval"] = serde_json::to_value(&approval).unwrap_or_default();
                }
                Ok(response)
            }
            Err(crate::plugins::approval_store::DecideError::Unknown(op)) => {
                Err(ApplicationError::Session(format!("approval_unknown: {op}")))
            }
            Err(crate::plugins::approval_store::DecideError::Conflict(op)) => Err(
                ApplicationError::Session(format!("approval_conflict: {op}")),
            ),
            Err(error) => Err(ApplicationError::Session(error.to_string())),
        }
    }

    // -- plugin management ------------------------------------------------

    /// Install a plugin package from a local directory/zip staging path.
    pub fn install_package_from_directory(
        &self,
        source: &Path,
    ) -> Result<InstalledPackage, ApplicationError> {
        self.catalog
            .install_from_directory(source)
            .map_err(|e| ApplicationError::Plugin(e.to_string()))
    }

    /// The plugin catalog with derived availability.
    pub fn list_plugins(&self) -> Result<Vec<CatalogEntry>, ApplicationError> {
        self.catalog
            .list()
            .map_err(|e| ApplicationError::Plugin(e.to_string()))
    }

    /// Register a *built-in* harness package through the normal immutable
    /// registry (T38): no source-code engine switch — the staged package
    /// directory (manifest + entry binary) installs exactly like a
    /// third-party one. Idempotent: an already-installed matching digest is
    /// a no-op; a changed digest registers alongside (upgrades apply to new
    /// runs only).
    pub fn ensure_builtin(
        &self,
        package_dir: &Path,
    ) -> Result<Option<InstalledPackage>, ApplicationError> {
        // Read the staged manifest to learn the identity first.
        let manifest_path = package_dir.join("harness.json");
        let manifest_text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| ApplicationError::Plugin(format!("builtin manifest unreadable: {e}")))?;
        let _manifest: r_code_harness_protocol::HarnessManifest =
            serde_json::from_str(&manifest_text)
                .map_err(|e| ApplicationError::Plugin(format!("builtin manifest invalid: {e}")))?;
        let existing = self.catalog.list().unwrap_or_default();
        let installed = self
            .catalog
            .install_from_directory(package_dir)
            .map_err(|e| ApplicationError::Plugin(e.to_string()))?;
        self.catalog
            .mark_builtin(&installed.package_ref)
            .map_err(|error| ApplicationError::Plugin(error.to_string()))?;
        if existing
            .iter()
            .any(|entry| entry.package_ref == installed.package_ref)
        {
            Ok(None)
        } else {
            Ok(Some(installed))
        }
    }

    /// Discover and register built-in plugin packages from a bundle's
    /// resources directory (`plugins/<id>/`), best-effort with explicit
    /// diagnostics; a missing directory (dev runs) is not an error.
    pub fn ensure_builtins_from(
        &self,
        resources_dir: &Path,
    ) -> Vec<Result<Option<InstalledPackage>, ApplicationError>> {
        let plugins_root = resources_dir.join("plugins");
        if !plugins_root.is_dir() {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(&plugins_root) else {
            return Vec::new();
        };
        let mut dirs: Vec<_> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && path.join("harness.json").is_file())
            .collect();
        dirs.sort();
        let mut results = Vec::new();
        for dir in dirs {
            results.push(self.ensure_builtin(&dir));
        }
        results
    }

    /// Remove an installed package unless active/recoverable runs pin it.
    pub fn remove_package(&self, id: &str, digest: &str) -> Result<(), ApplicationError> {
        self.catalog
            .remove(id, digest)
            .map_err(|e| ApplicationError::Plugin(e.to_string()))
    }

    /// Reversibly enable/disable an installed package.
    pub fn set_plugin_enabled(
        &self,
        id: &str,
        digest: &str,
        enabled: bool,
    ) -> Result<(), ApplicationError> {
        self.catalog
            .set_enabled(id, digest, enabled)
            .map_err(|e| ApplicationError::Plugin(e.to_string()))
    }

    // -- task lifecycle ---------------------------------------------------

    /// Create a task with a contract, auto-pinning the native built-in
    /// harness when it is installed (the default engine for chat tasks).
    pub async fn create_task(
        &self,
        task_id: &str,
        objective: &str,
        kind: TaskKind,
        required_checks: Vec<String>,
    ) -> Result<TaskState, ApplicationError> {
        let contract = r_code_kernel::task::TaskContract {
            task_id: task_id.to_string(),
            kind,
            objective: objective.to_string(),
            constraints: vec![],
            required_checks,
            revision: 1,
            memory: None,
        };
        let state = self
            .kernel_tasks
            .create_task(contract)
            .await
            .map_err(|e| ApplicationError::Task(e.to_string()))?;
        // Default engine: pin the native built-in when present so the first
        // send works without an explicit selection step.
        if let Ok(effective) = self
            .catalog
            .effective_package(crate::run_manager::DEFAULT_HARNESS_ID)
        {
            let _ = self
                .kernel_tasks
                .pin_harness(task_id, effective)
                .await
                .map_err(|e| ApplicationError::Task(e.to_string()));
        }
        Ok(state)
    }

    /// Create a task from one validated selection and persist its aggregate,
    /// preferences, title and harness pin in one journal transaction.
    pub async fn create_task_configured(
        &self,
        mut input: CreateTaskInput,
    ) -> Result<TaskState, ApplicationError> {
        if self.store.load_task(&input.task_id).await.is_some() {
            return Err(ApplicationError::Task(format!(
                "task {} already exists",
                input.task_id
            )));
        }

        let mut preferences = std::mem::take(&mut input.preferences);
        let entry = self.resolve_route_and_harness(
            &mut preferences,
            input.harness_id.as_deref(),
            None,
            true,
        )?;
        let package = entry.package_ref;
        let state = create_task_state(input, preferences);
        let events = vec![
            task_created_event(&state),
            harness_pinned_event(&state.contract.task_id, &package),
        ];
        self.store
            .create_task_with_selection_pin(&state, events, &package)
            .map_err(|error| ApplicationError::Store(error.to_string()))?;
        Ok(state)
    }

    /// Create an additive/legacy task without requiring a default Harness.
    /// If the native Harness is currently available it is pinned only after
    /// the atomic task creation; a failed best-effort pin leaves a valid,
    /// honestly unpinned task for a later explicit `task.selectHarness`.
    pub async fn create_task_legacy_default(
        &self,
        mut input: CreateTaskInput,
    ) -> Result<TaskState, ApplicationError> {
        if self.store.load_task(&input.task_id).await.is_some() {
            return Err(ApplicationError::Task(format!(
                "task {} already exists",
                input.task_id
            )));
        }
        let preferences = std::mem::take(&mut input.preferences);
        let state = create_task_state(input, preferences);
        self.store
            .create_task_without_selection_pin(&state, vec![task_created_event(&state)])
            .map_err(|error| ApplicationError::Store(error.to_string()))?;

        if let Ok(package) = self
            .catalog
            .effective_package(crate::run_manager::DEFAULT_HARNESS_ID)
        {
            let event = harness_pinned_event(&state.contract.task_id, &package);
            let _ = self.store.save_task_events_and_selection_pin_if_revision(
                &state,
                vec![event],
                1,
                Some(&package),
            );
        }
        Ok(state)
    }

    /// Pin a harness for a task (the effective newest available version).
    /// The pin persists in the catalog's plugin_pins so removal of live
    /// pinned packages is refused across daemon restarts.
    pub async fn select_harness(
        &self,
        task_id: &str,
        harness_id: &str,
    ) -> Result<PackageRef, ApplicationError> {
        let effective = self
            .catalog
            .effective_package(harness_id)
            .map_err(|e| ApplicationError::Plugin(e.to_string()))?;
        for _ in 0..8 {
            let (mut state, revision) = self
                .store
                .load_task_with_revision(task_id)
                .map_err(|error| ApplicationError::Store(error.to_string()))?
                .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
            let selected = self
                .catalog
                .pinned_package(&format!("selection-{task_id}"))
                .map_err(|error| ApplicationError::Plugin(error.to_string()))?;
            let material_changed = selected.as_ref() != Some(&effective);
            if material_changed {
                self.ensure_material_change_quiescent(task_id, &state.execution)
                    .await?;
            }
            if !material_changed {
                return Ok(effective);
            }
            let mut events = vec![harness_pinned_event(task_id, &effective)];
            let invalidate = task_waits_on_plan(&state.execution);
            if invalidate {
                state
                    .invalidate_plan(Actor::Host)
                    .map_err(|error| ApplicationError::Store(error.to_string()))?;
                events.push(plan_invalidated_event(task_id, "harness-changed"));
            }
            let saved = if invalidate {
                self.store.save_task_and_invalidate_plan_if_revision(
                    &state,
                    events,
                    revision,
                    Some(&effective),
                )
            } else {
                self.store.save_task_events_and_selection_pin_if_revision(
                    &state,
                    events,
                    revision,
                    Some(&effective),
                )
            };
            match saved {
                Ok(_) => return Ok(effective),
                Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
                Err(error) => return Err(ApplicationError::Store(error.to_string())),
            }
        }
        Err(ApplicationError::Task(format!(
            "task {task_id} stayed busy while selecting a harness"
        )))
    }

    /// Rename a task (UI metadata).
    pub async fn rename_task(&self, task_id: &str, title: &str) -> Result<(), ApplicationError> {
        self.update_task_metadata(
            task_id,
            "task.renamed",
            serde_json::json!({"title": title}),
            |state| state.title = Some(title.to_string()),
        )
    }

    /// Update per-task preferences applied to future runs.
    pub async fn set_task_preferences(
        &self,
        task_id: &str,
        preferences: TaskPreferences,
    ) -> Result<(), ApplicationError> {
        self.set_task_preferences_patch(task_id, TaskPreferencesPatch::merge_present(preferences))
            .await
    }

    /// Apply a partial preference update to the latest aggregate on every
    /// retry. Independent fields therefore compose instead of replaying a
    /// stale read-modify-write document from the RPC layer.
    pub async fn set_task_preferences_patch(
        &self,
        task_id: &str,
        patch: TaskPreferencesPatch,
    ) -> Result<(), ApplicationError> {
        for _ in 0..8 {
            let (mut state, revision) = self
                .store
                .load_task_with_revision(task_id)
                .map_err(|error| ApplicationError::Store(error.to_string()))?
                .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
            let mut preferences = state.preferences.clone();
            patch.apply_to(&mut preferences);
            let current_harness = self.pinned_harness_id(task_id);
            let route_changed = state.preferences.model_route != preferences.model_route
                || state.preferences.model != preferences.model;
            let requested_harness = patch.requested_harness();
            let harness_patch_present = !matches!(patch.harness_id, PatchField::Unchanged);
            let harness_entry = if route_changed || harness_patch_present {
                Some(self.resolve_route_and_harness(
                    &mut preferences,
                    requested_harness,
                    current_harness.as_deref(),
                    requested_harness.is_some(),
                )?)
            } else {
                None
            };
            let current_package = self
                .catalog
                .pinned_package(&format!("selection-{task_id}"))
                .map_err(|error| ApplicationError::Plugin(error.to_string()))?;
            let harness_changed = harness_entry
                .as_ref()
                .is_some_and(|entry| current_package.as_ref() != Some(&entry.package_ref));
            let preferences_changed = state.preferences != preferences;
            let material_changed = preferences_changed || harness_changed;
            if material_changed {
                self.ensure_material_change_quiescent(task_id, &state.execution)
                    .await?;
            }

            state.preferences = preferences.clone();
            let mut events = vec![r_code_kernel::ports::JournalEvent {
                seq: 0,
                task_id: task_id.to_string(),
                kind: "task.preferences".to_string(),
                payload: serde_json::to_value(&state.preferences).unwrap_or_default(),
            }];
            if let Some(entry) = &harness_entry {
                events.push(harness_pinned_event(task_id, &entry.package_ref));
            }
            let invalidate = material_changed && task_waits_on_plan(&state.execution);
            if invalidate {
                state
                    .invalidate_plan(Actor::Host)
                    .map_err(|error| ApplicationError::Task(error.to_string()))?;
                events.push(plan_invalidated_event(task_id, "run-material-changed"));
            }
            let package = harness_entry.as_ref().map(|entry| &entry.package_ref);
            let saved = if invalidate {
                self.store
                    .save_task_and_invalidate_plan_if_revision(&state, events, revision, package)
            } else {
                self.store.save_task_events_and_selection_pin_if_revision(
                    &state, events, revision, package,
                )
            };
            match saved {
                Ok(_) => return Ok(()),
                Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
                Err(error) => return Err(ApplicationError::Store(error.to_string())),
            }
        }
        Err(ApplicationError::Task(format!(
            "task {task_id} stayed busy while updating preferences"
        )))
    }

    async fn ensure_material_change_quiescent(
        &self,
        task_id: &str,
        execution: &TaskExecution,
    ) -> Result<(), ApplicationError> {
        if task_execution_is_active(execution) {
            return Err(ApplicationError::Task(
                "run material cannot change while the task is running or waiting for input"
                    .to_string(),
            ));
        }
        if self
            .runs
            .await_quiescent(task_id, std::time::Duration::from_secs(5))
            .await
        {
            return Ok(());
        }
        Err(ApplicationError::Task(
            "run material is busy while a task run is starting or draining; retry after it settles"
                .to_string(),
        ))
    }

    /// Read the current preference document so RPC patches can preserve fields omitted by the
    /// caller. Model, inference, mode and prompt controls are independently editable surfaces.
    pub async fn task_preferences(
        &self,
        task_id: &str,
    ) -> Result<TaskPreferences, ApplicationError> {
        self.store
            .load_task(task_id)
            .await
            .map(|state| state.preferences)
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))
    }

    /// Read the current immutable plan head and its independent approval.
    pub async fn plan(&self, task_id: &str) -> Result<PlanView, ApplicationError> {
        let state = self
            .store
            .load_task(task_id)
            .await
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
        let revision = self
            .store
            .current_plan_revision(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} has no plan")))?;
        let approval = self
            .store
            .load_active_plan_approval(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?;
        Ok(PlanView {
            task_id: task_id.to_string(),
            revision: revision.material().revision,
            revision_hash: revision.reference().as_str().to_string(),
            work_units: revision.material().work_units.clone(),
            approval,
            state: phase_label(&state),
        })
    }

    /// Approve one exact plan head. Approval persistence, task Ready state
    /// and `plan.approved` audit event commit in one store transaction; this
    /// method never starts an implementation run.
    pub async fn approve_plan(
        &self,
        task_id: &str,
        revision_hash: &str,
        approval_id: &str,
        actor_id: &str,
        session_id: &str,
    ) -> Result<PlanView, ApplicationError> {
        let revision_ref = PlanRevisionRef::parse(revision_hash.to_string())
            .map_err(|error| ApplicationError::Task(error.to_string()))?;
        let actor = PlanApprovalActor::new(actor_id, session_id, PLAN_APPROVE_SCOPE)
            .map_err(|error| ApplicationError::Task(error.to_string()))?;
        let (_, task_revision) = self
            .store
            .load_task_with_revision(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
        self.store
            .approve_plan_revision_and_mark_ready(
                task_id,
                &revision_ref,
                approval_id,
                actor,
                task_revision,
            )
            .map_err(|error| ApplicationError::Task(error.to_string()))?;
        self.plan(task_id).await
    }

    /// Explicit user revision request. Unlike ordinary `task.sendMessage` on
    /// Ready, this invalidates the active approval and returns the task to
    /// Pending atomically; the caller may then send the revised objective.
    pub async fn revise_plan(
        &self,
        task_id: &str,
        reason: &str,
    ) -> Result<serde_json::Value, ApplicationError> {
        for _ in 0..8 {
            let (mut state, revision) = self
                .store
                .load_task_with_revision(task_id)
                .map_err(|error| ApplicationError::Store(error.to_string()))?
                .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
            state
                .invalidate_plan(Actor::Host)
                .map_err(|error| ApplicationError::Task(error.to_string()))?;
            match self.store.save_task_and_invalidate_plan_if_revision(
                &state,
                vec![plan_invalidated_event(task_id, reason)],
                revision,
                None,
            ) {
                Ok(next_revision) => {
                    return Ok(serde_json::json!({
                        "taskId": task_id,
                        "state": "pending",
                        "taskRevision": next_revision,
                    }));
                }
                Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
                Err(error) => return Err(ApplicationError::Store(error.to_string())),
            }
        }
        Err(ApplicationError::Task(format!(
            "task {task_id} stayed busy while revising its plan"
        )))
    }

    fn resolve_route_and_harness(
        &self,
        preferences: &mut TaskPreferences,
        requested_harness: Option<&str>,
        current_harness: Option<&str>,
        infer_managed_route: bool,
    ) -> Result<CatalogEntry, ApplicationError> {
        let route_harness = match preferences.model_route.as_ref() {
            Some(ModelRoute::HarnessManaged { harness_id, .. }) => Some(harness_id.as_str()),
            _ => None,
        };
        if let (Some(route_harness), Some(requested_harness)) = (route_harness, requested_harness) {
            if route_harness != requested_harness {
                return Err(ApplicationError::Task(format!(
                    "harness-managed route {route_harness} does not match requested harness {requested_harness}"
                )));
            }
        }
        let harness_id = route_harness
            .or(requested_harness)
            .or(current_harness)
            .unwrap_or(crate::run_manager::DEFAULT_HARNESS_ID);
        let package = self
            .catalog
            .effective_package(harness_id)
            .map_err(|error| ApplicationError::Plugin(error.to_string()))?;
        let entry = self
            .catalog
            .list()
            .map_err(|error| ApplicationError::Plugin(error.to_string()))?
            .into_iter()
            .find(|entry| entry.package_ref == package)
            .ok_or_else(|| {
                ApplicationError::Plugin(format!("Harness {harness_id} is not available"))
            })?;
        let exposes_host_models = entry
            .manifest
            .requested_host_services
            .contains(&HostService::ModelStream);
        if preferences.model_route.is_none() && infer_managed_route && !exposes_host_models {
            preferences.model_route = Some(ModelRoute::HarnessManaged {
                harness_id: harness_id.to_string(),
                model_id: None,
            });
        }
        self.validate_model_route(preferences, &entry)?;
        Ok(entry)
    }

    fn validate_model_route(
        &self,
        preferences: &TaskPreferences,
        entry: &CatalogEntry,
    ) -> Result<(), ApplicationError> {
        let exposes_host_models = entry
            .manifest
            .requested_host_services
            .contains(&HostService::ModelStream);
        match preferences.model_route.as_ref() {
            Some(ModelRoute::HostProvider {
                provider_id,
                model_id,
            }) => {
                validate_route_part("providerId", provider_id)?;
                if let Some(model_id) = model_id {
                    validate_route_part("modelId", model_id)?;
                }
                if !exposes_host_models {
                    return Err(ApplicationError::Task(format!(
                        "Harness {} does not expose host.model.stream",
                        entry.manifest.id.0
                    )));
                }
                match self
                    .settings
                    .resolve_provider_for_run(Some(provider_id), model_id.as_deref())
                    .map_err(|error| ApplicationError::Settings(error.to_string()))?
                {
                    RunProviderResolution::Resolved(_) => Ok(()),
                    RunProviderResolution::Unconfigured { .. } => Err(ApplicationError::Settings(
                        format!("Provider {provider_id} is not configured"),
                    )),
                }
            }
            Some(ModelRoute::HarnessManaged {
                harness_id,
                model_id,
            }) => {
                validate_route_part("harnessId", harness_id)?;
                if let Some(model_id) = model_id {
                    validate_route_part("modelId", model_id)?;
                }
                if entry.manifest.id.0 != *harness_id {
                    return Err(ApplicationError::Task(format!(
                        "Harness-managed route {harness_id} does not match selected harness {}",
                        entry.manifest.id.0
                    )));
                }
                if exposes_host_models {
                    return Err(ApplicationError::Task(format!(
                        "Harness-managed route {harness_id} must not expose host.model.stream"
                    )));
                }
                Ok(())
            }
            None => {
                if let Some(selection) = preferences.model.as_deref() {
                    validate_route_part("model", selection)?;
                    if !exposes_host_models {
                        return Err(ApplicationError::Task(format!(
                            "legacy provider route cannot use Harness {}",
                            entry.manifest.id.0
                        )));
                    }
                    self.settings
                        .resolve_provider_for_run(Some(selection), None)
                        .map_err(|error| ApplicationError::Settings(error.to_string()))?;
                }
                Ok(())
            }
        }
    }

    fn pinned_harness_id(&self, task_id: &str) -> Option<String> {
        self.store
            .task_events(task_id)
            .into_iter()
            .rev()
            .find(|event| event.kind == "harness.pinned")
            .and_then(|event| event.payload["id"].as_str().map(str::to_string))
    }

    fn update_task_metadata<F>(
        &self,
        task_id: &str,
        kind: &str,
        payload: serde_json::Value,
        update: F,
    ) -> Result<(), ApplicationError>
    where
        F: Fn(&mut TaskState),
    {
        for _ in 0..8 {
            let (mut state, revision) = self
                .store
                .load_task_with_revision(task_id)
                .map_err(|error| ApplicationError::Store(error.to_string()))?
                .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
            update(&mut state);
            let event = r_code_kernel::ports::JournalEvent {
                seq: 0,
                task_id: task_id.to_string(),
                kind: kind.to_string(),
                payload: payload.clone(),
            };
            match self
                .store
                .save_task_and_events_if_revision(&state, vec![event], revision)
            {
                Ok(_) => return Ok(()),
                Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
                Err(error) => return Err(ApplicationError::Store(error.to_string())),
            }
        }
        Err(ApplicationError::Task(format!(
            "task {task_id} stayed busy while updating metadata"
        )))
    }

    /// Create a branch task inheriting the source contract (a fresh
    /// conversation by design — plugin-private state never copies across).
    /// The branch keeps the source's engine pin for a like-for-like run.
    pub async fn clone_task(
        &self,
        source_task_id: &str,
        new_task_id: &str,
        title: Option<&str>,
    ) -> Result<TaskState, ApplicationError> {
        // Source must be idle (kernel rule); the engine pin rides the
        // source's journal so the clone runs the same harness.
        let source_state =
            self.store.load_task(source_task_id).await.ok_or_else(|| {
                ApplicationError::Task(format!("task {source_task_id} not found"))
            })?;
        let branch = self
            .kernel_tasks
            .create_branch(source_task_id, new_task_id)
            .await
            .map_err(|e| ApplicationError::Task(e.to_string()))?;
        let _ = self.store.record_branch(new_task_id, Some(source_task_id));
        // Carry the engine pin from the source journal (harness.pinned id).
        let pinned = self
            .store
            .task_events(source_task_id)
            .into_iter()
            .rev()
            .find(|event| event.kind == "harness.pinned")
            .and_then(|event| {
                event
                    .payload
                    .get("id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            });
        let harness_id = pinned.unwrap_or_else(|| {
            let _ = &source_state;
            crate::run_manager::DEFAULT_HARNESS_ID.to_string()
        });
        if let Ok(effective) = self.catalog.effective_package(&harness_id) {
            let _ = self.kernel_tasks.pin_harness(new_task_id, effective).await;
        }
        if let Some(title) = title {
            self.rename_task(new_task_id, title).await?;
        }
        Ok(branch)
    }

    /// Branch lineage rows (for the TUI session tree).
    pub fn task_branches(&self) -> Vec<r_code_store::v1::tasks::TaskBranch> {
        self.store.branches().unwrap_or_default()
    }

    /// Redacted, read-only view of writers that still quarantine a workspace.
    pub fn quarantine_diagnostics(
        &self,
        workspace_key: Option<&str>,
    ) -> Result<Vec<QuarantineView>, ApplicationError> {
        let records = self
            .store
            .quarantine_diagnostics(workspace_key)
            .map_err(|error| ApplicationError::Store(error.to_string()))?;
        let mut views = records
            .into_iter()
            .map(|record| {
                let boot_changed = record.current_boot_changed(self.boot_identity.as_str());
                let legacy_reboot_required =
                    record.legacy && !record.legacy_corrupt && boot_changed == Some(false);
                let remediation = if record.corrupt || record.legacy_corrupt {
                    QuarantineRemediation::ManualRecoveryRequired
                } else if record.legacy {
                    match boot_changed {
                        Some(false) => QuarantineRemediation::RebootRequired,
                        Some(true) => QuarantineRemediation::AwaitingProofRetry,
                        None => QuarantineRemediation::ManualRecoveryRequired,
                    }
                } else {
                    QuarantineRemediation::AwaitTerminationProof
                };
                QuarantineView {
                    tree_ref: record.tree_ref,
                    workspace_ref: record.workspace_ref,
                    workspace_identity: record.workspace_identity,
                    profile_ref: record.profile_ref,
                    state: record.state,
                    ownership_epoch: record.ownership_epoch,
                    owner_fingerprint: record.owner_fingerprint,
                    reason_code: record.reason_code,
                    reason_digest: record.reason_digest,
                    proof_status: record.proof_status,
                    legacy_reboot_required,
                    current_boot_changed: boot_changed.unwrap_or(false),
                    corrupt: record.corrupt,
                    created_at_ms: record.created_at_ms,
                    updated_at_ms: record.updated_at_ms,
                    remediation,
                }
            })
            .collect::<Vec<_>>();
        views.sort_by(|left, right| {
            (&left.workspace_ref, &left.tree_ref).cmp(&(&right.workspace_ref, &right.tree_ref))
        });
        Ok(views)
    }

    /// P11R: retry ONE quarantined tree by its persisted identity with
    /// the exact platform proof rules. The current stable BootIdentity
    /// decides the legacy path (a reboot is the only proof); ordinary
    /// quarantined trees clear only through a complete persisted proof.
    /// There is no force-clear — every refusal names its reason.
    pub fn retry_quarantine(
        &self,
        tree_id: &str,
        actor: &str,
        session: &str,
    ) -> Result<QuarantineRetryView, ApplicationError> {
        if tree_id.trim().is_empty() {
            return Err(ApplicationError::Store("treeId must not be empty".into()));
        }
        if actor.trim().is_empty() || session.trim().is_empty() {
            return Err(ApplicationError::Store(
                "actor and session must not be empty".into(),
            ));
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0);
        let outcome = self
            .store
            .retry_quarantine(tree_id, self.boot_identity.as_str(), actor, session, now_ms)
            .map_err(|error| ApplicationError::Store(error.to_string()))?;
        // The response never echoes raw internal proof ids: the proof
        // reference is a digest, matching the P02 redaction discipline.
        // The `sha256:` prefix matches every sibling reference surface
        // (quarantine diagnostics, mutations, review, run snapshots).
        let redact = |value: &str| {
            format!(
                "sha256:{}",
                r_code_harness_protocol::canonical_input_hash(&serde_json::Value::String(
                    value.to_string(),
                ))
            )
        };
        Ok(match outcome {
            r_code_store::v1::operations::QuarantineRetryOutcome::Cleared { proof_id } => {
                QuarantineRetryView {
                    tree_ref: redact(tree_id),
                    cleared: true,
                    reason: "cleared".into(),
                    proof_ref: Some(redact(&proof_id)),
                }
            }
            r_code_store::v1::operations::QuarantineRetryOutcome::Refused { reason } => {
                QuarantineRetryView {
                    tree_ref: redact(tree_id),
                    cleared: false,
                    reason: refusal_reason(reason).into(),
                    proof_ref: None,
                }
            }
        })
    }

    /// Read-only, redacted view of persisted safety capability reports
    /// (P12). Material JSON is never expanded — only report references,
    /// coarse status and the boot-match flag. Read-only by construction:
    /// this path performs no store mutation and never activates anything.
    pub fn safety_report_diagnostics(
        &self,
        capability: Option<&str>,
    ) -> Result<Vec<SafetyReportView>, ApplicationError> {
        let heads = self
            .store
            .safety_report_capabilities()
            .map_err(|error| ApplicationError::Store(error.to_string()))?;
        let mut views = Vec::new();
        for head_capability in heads {
            if capability.is_some_and(|filter| filter != head_capability) {
                continue;
            }
            let Some(record) = self
                .store
                .current_safety_report(&head_capability)
                .map_err(|error| ApplicationError::Store(error.to_string()))?
            else {
                continue;
            };
            let boot_matches_current =
                serde_json::from_str::<serde_json::Value>(&record.material_json)
                    .ok()
                    .and_then(|material| {
                        material
                            .get("boot_identity")
                            .and_then(serde_json::Value::as_str)
                            .map(|boot| boot == self.boot_identity.as_str())
                    })
                    .unwrap_or(false);
            views.push(SafetyReportView {
                capability: record.capability,
                report_ref: record.report_id,
                status: record.status.as_str().to_string(),
                material_digest: record.material_digest,
                boot_matches_current,
                created_at_ms: record.created_at_ms,
            });
        }
        views.sort_by(|left, right| left.capability.cmp(&right.capability));
        Ok(views)
    }

    /// Send a message: enqueue + async run (see [`RunManager::send`]).
    pub async fn send_message(
        &self,
        task_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, ApplicationError> {
        self.send_message_as(task_id, text, None).await
    }

    /// [`Self::send_message`] with an audit actor (R10): remote sends carry
    /// the authenticated device id; local sends the client id.
    pub async fn send_message_as(
        &self,
        task_id: &str,
        text: &str,
        actor: Option<&str>,
    ) -> Result<serde_json::Value, ApplicationError> {
        self.runs
            .clone()
            .send_as(task_id, text, actor)
            .await
            .map_err(|e| ApplicationError::Session(e.to_string()))
    }

    /// Cancel the active run of a task.
    pub async fn cancel_task(&self, task_id: &str) -> Result<bool, ApplicationError> {
        self.runs
            .cancel(task_id)
            .await
            .map_err(|e| ApplicationError::Session(e.to_string()))
    }

    // -- views -------------------------------------------------------------

    /// All tasks, most recently updated first.
    pub async fn list_tasks(&self) -> Vec<TaskSummaryView> {
        let mut rows = Vec::new();
        for (state, updated) in self.store.list_tasks() {
            let task_id = state.contract.task_id.clone();
            let running = self.runs.is_running(&task_id).await;
            rows.push(TaskSummaryView {
                title: state.title.clone().unwrap_or_else(|| default_title(&state)),
                kind: kind_label(&state.contract.kind),
                state: phase_label(&state),
                running,
                workspace_path: state.preferences.workspace_path.clone(),
                task_id,
                updated_at_ms: updated,
            });
        }
        rows
    }

    /// The task detail projection.
    pub async fn task_detail(&self, task_id: &str) -> Result<TaskDetailView, ApplicationError> {
        let state = self
            .store
            .load_task(task_id)
            .await
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;
        let events = self.store.task_events(task_id);
        let mut runs = Vec::new();
        let mut usage = TaskUsageView::default();
        for event in &events {
            match event.kind.as_str() {
                "run.started" => {
                    if let Some(run_id) = event.payload.get("runId").and_then(|v| v.as_str()) {
                        runs.push(TaskRunView {
                            run_id: run_id.to_string(),
                            outcome: "running".into(),
                            snapshot_id: event
                                .payload
                                .get("snapshotId")
                                .and_then(|value| value.as_str())
                                .map(str::to_string),
                        });
                    }
                }
                "run.completed" | "run.failed" | "run.cancelled" => {
                    let outcome = match event.kind.as_str() {
                        "run.completed" => "completed",
                        "run.failed" => "failed",
                        _ => "cancelled",
                    };
                    if let Some(run_id) = event.payload.get("runId").and_then(|v| v.as_str()) {
                        if let Some(row) = runs.iter_mut().find(|run| run.run_id == run_id) {
                            row.outcome = outcome.to_string();
                        }
                    }
                }
                "model.usage" => {
                    if let Some(breakdown) = event.payload.get("usage") {
                        usage.input_tokens += breakdown
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        usage.output_tokens += breakdown
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                    }
                }
                _ => {}
            }
        }
        let running = self.runs.is_running(task_id).await;
        let model_route = state.preferences.model_route.clone();
        let provider = match model_route.as_ref() {
            Some(ModelRoute::HostProvider { provider_id, .. }) => Some(provider_id.clone()),
            Some(ModelRoute::HarnessManaged { .. }) => None,
            None => state.preferences.model.clone(),
        };
        let model = match model_route.as_ref() {
            Some(ModelRoute::HostProvider {
                provider_id,
                model_id,
            }) => model_id.clone().or_else(|| {
                self.settings
                    .availability()
                    .into_iter()
                    .find(|provider| provider.selection == *provider_id)
                    .map(|provider| provider.model)
            }),
            Some(ModelRoute::HarnessManaged { model_id, .. }) => model_id.clone(),
            None => state.preferences.model.clone(),
        };
        let harness_id = self
            .pinned_harness_id(task_id)
            .unwrap_or_else(|| crate::run_manager::DEFAULT_HARNESS_ID.to_string());
        let engine = engine_label(&harness_id).to_string();
        Ok(TaskDetailView {
            task_id: task_id.to_string(),
            title: state.title.clone().unwrap_or_else(|| default_title(&state)),
            kind: kind_label(&state.contract.kind),
            objective: state.contract.objective.clone(),
            state: phase_label(&state),
            running,
            model_route,
            provider,
            model,
            engine,
            harness_id,
            inference: state.preferences.inference.clone(),
            mode: state.preferences.mode.clone(),
            workspace_path: state.preferences.workspace_path.clone(),
            memory: state.contract.memory.as_ref().map(|memory| TaskMemoryView {
                rendered: memory.rendered.clone(),
                snapshot_hash: memory.snapshot_hash.clone(),
                entry_ids: memory.entry_ids.clone(),
            }),
            runs,
            usage,
        })
    }

    // -- events ------------------------------------------------------------

    /// The durable event journal, projected as host envelopes.
    pub async fn events_after(&self, after_seq: u64, limit: u32) -> Vec<EventEnvelope> {
        self.store
            .read_events(after_seq, limit)
            .await
            .into_iter()
            .map(envelope_of)
            .collect()
    }
}

fn validate_route_part(field: &'static str, value: &str) -> Result<(), ApplicationError> {
    if value.trim().is_empty() || value.contains('\0') || value.chars().count() > 512 {
        return Err(ApplicationError::Task(format!(
            "{field} must be non-empty, contain no NUL, and be at most 512 characters"
        )));
    }
    Ok(())
}

fn create_task_state(input: CreateTaskInput, preferences: TaskPreferences) -> TaskState {
    let contract = r_code_kernel::task::TaskContract {
        task_id: input.task_id,
        kind: input.kind,
        objective: input.objective,
        constraints: vec![],
        required_checks: input.required_checks,
        memory: input.memory,
        revision: 1,
    };
    let mut state = TaskState::new(contract);
    state.title = input.title;
    state.preferences = preferences;
    state
}

fn task_created_event(state: &TaskState) -> r_code_kernel::ports::JournalEvent {
    r_code_kernel::ports::JournalEvent {
        seq: 0,
        task_id: state.contract.task_id.clone(),
        kind: "task.created".to_string(),
        payload: serde_json::json!({
            "kind": state.contract.kind,
            "title": state.title,
            "preferences": state.preferences,
        }),
    }
}

fn harness_pinned_event(task_id: &str, package: &PackageRef) -> r_code_kernel::ports::JournalEvent {
    r_code_kernel::ports::JournalEvent {
        seq: 0,
        task_id: task_id.to_string(),
        kind: "harness.pinned".to_string(),
        payload: serde_json::json!({
            "id": package.id.0,
            "version": package.version.to_string(),
            "contentDigest": package.content_digest,
        }),
    }
}

fn task_execution_is_active(execution: &TaskExecution) -> bool {
    matches!(
        execution,
        TaskExecution::Running { .. } | TaskExecution::WaitingInput { .. }
    )
}

fn task_waits_on_plan(execution: &TaskExecution) -> bool {
    matches!(
        execution,
        TaskExecution::AwaitingPlanApproval { .. } | TaskExecution::Ready { .. }
    )
}

fn plan_invalidated_event(task_id: &str, reason: &str) -> r_code_kernel::ports::JournalEvent {
    r_code_kernel::ports::JournalEvent {
        seq: 0,
        task_id: task_id.to_string(),
        kind: "plan.invalidated".to_string(),
        payload: serde_json::json!({"reason": reason}),
    }
}

fn validate_review_context(context: &ReviewActionContext) -> Result<(), ApplicationError> {
    if context.action_id.trim().is_empty()
        || context.candidate_digest.trim().is_empty()
        || context.actor_id.trim().is_empty()
        || context.session_id.trim().is_empty()
    {
        return Err(ApplicationError::Task(
            "review action, candidate, actor and session are required".to_string(),
        ));
    }
    Ok(())
}

fn review_request_hash(
    action: &str,
    context: &ReviewActionContext,
    reason: Option<&str>,
    checks: &[String],
) -> String {
    r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
        "action": action,
        "actionId": context.action_id,
        "expectedTaskRevision": context.expected_task_revision,
        "candidateDigest": context.candidate_digest,
        "actorId": context.actor_id,
        "sessionId": context.session_id,
        "reason": reason,
        "checks": checks,
    }))
}

fn review_error(error: ReviewError) -> ApplicationError {
    match error {
        ReviewError::Conflict(_) | ReviewError::Stale | ReviewError::InvalidState => {
            ApplicationError::Task(error.to_string())
        }
        _ => ApplicationError::Store(error.to_string()),
    }
}

fn review_workspace_key(state: &TaskState) -> Result<String, ApplicationError> {
    let root = state
        .preferences
        .workspace_path
        .as_deref()
        .ok_or_else(|| ApplicationError::Task("review workspace is missing".into()))?;
    let canonical = std::fs::canonicalize(root)
        .map_err(|_| ApplicationError::Task("review workspace is unavailable".into()))?;
    Ok(format!(
        "sha256:{}",
        sha256_hex(canonical.to_string_lossy().as_bytes())
    ))
}

fn review_required_checks(state: &TaskState) -> std::collections::BTreeSet<String> {
    let active_work_unit = match &state.execution {
        TaskExecution::RepairRequired { work_unit_id, .. } => work_unit_id.as_deref(),
        _ => None,
    };
    let acceptance = active_work_unit
        .and_then(|work_unit_id| state.work_units.iter().find(|unit| unit.id == work_unit_id))
        .map(|unit| unit.acceptance.as_slice())
        .unwrap_or_default();
    state
        .contract
        .required_checks
        .iter()
        .chain(
            acceptance
                .iter()
                .filter(|check| check.starts_with("check:")),
        )
        .cloned()
        .collect()
}

fn engine_label(harness_id: &str) -> &str {
    match harness_id {
        "native.r-code" => "r_code",
        "codex.r-code" => "codex",
        other => other,
    }
}

/// Wall-clock milliseconds since the epoch (best-effort; used for ages).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Where an application command arrived from. RA2 placeholder: every
/// transport is local until R04 lands the remote listener; the gate keeps
/// the future remote decision path explicit instead of implicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    Local,
    Remote,
}

/// Remote approval decisions stay disabled until R04 wires capability
/// enforcement (`approvals:decide` is default-off even for devices).
pub const REMOTE_APPROVAL_DECISIONS_ENABLED: bool = false;

/// Source gate for `approvals.decide` (RA2): remote callers get a
/// structured refusal until R04 enables the capability path.
pub fn approval_decision_source_gate(source: CommandSource) -> Result<(), String> {
    if source == CommandSource::Remote && !REMOTE_APPROVAL_DECISIONS_ENABLED {
        return Err(
            "remote_not_enabled: approvals.decide is local-only until the remote listener lands (R04)"
                .into(),
        );
    }
    Ok(())
}

fn default_title(state: &TaskState) -> String {
    let objective = state.contract.objective.trim();
    if objective.is_empty() {
        "新会话".to_string()
    } else {
        let mut chars = objective.chars();
        let head: String = chars.by_ref().take(24).collect();
        if chars.next().is_some() {
            format!("{head}…")
        } else {
            head
        }
    }
}

fn kind_label(kind: &TaskKind) -> String {
    match kind {
        TaskKind::Conversation => "conversation".into(),
        TaskKind::Implementation => "implementation".into(),
        TaskKind::PlanDraft => "plan-draft".into(),
        TaskKind::Repair => "repair".into(),
    }
}

fn phase_label(state: &TaskState) -> String {
    match &state.execution {
        r_code_kernel::task::TaskExecution::Pending => "pending".into(),
        r_code_kernel::task::TaskExecution::Running { .. } => "running".into(),
        r_code_kernel::task::TaskExecution::WaitingInput { .. } => "waiting-input".into(),
        r_code_kernel::task::TaskExecution::AwaitingPlanApproval { .. } => {
            "awaiting-plan-approval".into()
        }
        r_code_kernel::task::TaskExecution::Ready { .. } => "ready".into(),
        r_code_kernel::task::TaskExecution::Verifying { .. } => "verifying".into(),
        r_code_kernel::task::TaskExecution::RepairRequired { .. } => "repair-required".into(),
        r_code_kernel::task::TaskExecution::ReviewReady { .. } => "review-ready".into(),
        r_code_kernel::task::TaskExecution::Terminal { verdict } => match verdict {
            r_code_kernel::task::TaskVerdict::Verified { .. } => "verified".into(),
            r_code_kernel::task::TaskVerdict::VerifiedAccepted { .. } => "verified-accepted".into(),
            r_code_kernel::task::TaskVerdict::Unverified { .. } => "unverified".into(),
            r_code_kernel::task::TaskVerdict::UnverifiedAccepted { .. } => {
                "unverified-accepted".into()
            }
            r_code_kernel::task::TaskVerdict::Blocked { .. } => "blocked".into(),
            r_code_kernel::task::TaskVerdict::Failed { .. } => "failed".into(),
            r_code_kernel::task::TaskVerdict::Cancelled { .. } => "cancelled".into(),
        },
    }
}
