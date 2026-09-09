//! The headless ApplicationService: one composition point inside the
//! r-code-service daemon exposing plugin management, task lifecycle,
//! harness sessions and the durable event journal to every client
//! (GUI/TUI/MCP). Non-UI composition moved out of any CommandState; all
//! task work flows through the same lifecycle API regardless of which
//! harness (Native, Codex or third-party) a task pins.

use crate::plugins::catalog::CatalogEntry;
use crate::plugins::package::InstalledPackage;
use crate::plugins::PluginCatalog;
use crate::profile::RuntimeProfile;
use crate::run_manager::{envelope_of, RunManager};
use crate::services::settings_store::SettingsStore;
use r_code_harness_protocol::{EventEnvelope, PackageRef};
use r_code_kernel::ports::JournalStore as _;
use r_code_kernel::task::{TaskKind, TaskState};
use r_code_kernel::tasks::TaskService as KernelTaskService;
use r_code_store::v2::V2Store;
use serde::{Deserialize, Serialize};
use std::path::Path;
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

/// One task row for `task.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSummaryView {
    pub task_id: String,
    pub title: String,
    pub kind: String,
    pub state: String,
    pub running: bool,
    pub updated_at_ms: i64,
}

/// One run row inside a task detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRunView {
    pub run_id: String,
    pub outcome: String,
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
    pub model: Option<String>,
    pub inference: Option<serde_json::Value>,
    pub mode: Option<String>,
    pub runs: Vec<TaskRunView>,
    pub usage: TaskUsageView,
}

/// Aggregated usage across runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskUsageView {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The composed application surface.
pub struct ApplicationService {
    store: Arc<V2Store>,
    kernel_tasks: Arc<KernelTaskService>,
    catalog: Arc<PluginCatalog>,
    runs: Arc<RunManager>,
    settings: Arc<SettingsStore>,
    nonce: String,
}

impl ApplicationService {
    /// Compose over a resolved profile. Services are injected so tests use
    /// scripted providers and the daemon wires the real ones.
    pub fn compose(
        profile: &RuntimeProfile,
        models: Arc<dyn r_code_kernel::ports::ModelService>,
        tools: Arc<dyn r_code_kernel::ports::ToolService>,
    ) -> Result<Self, ApplicationError> {
        let store = Arc::new(
            V2Store::open(&profile.database_path())
                .map_err(|e| ApplicationError::Store(e.to_string()))?,
        );
        let kernel_tasks = Arc::new(KernelTaskService::new(store.clone()));
        let catalog = Arc::new(PluginCatalog::new(profile.plugins_root(), store.clone()));
        let settings = Arc::new(SettingsStore::new(profile.harness_v2_root()));
        let runs = RunManager::new(
            store.clone(),
            kernel_tasks.clone(),
            catalog.clone(),
            models,
            tools,
            Arc::new(r_code_kernel::testing::FakeProcessService::default()),
            settings.clone(),
        );
        Ok(Self {
            store,
            kernel_tasks,
            catalog,
            runs,
            settings,
            nonce: uuid::Uuid::new_v4().simple().to_string(),
        })
    }

    /// This daemon instance's identity.
    pub fn instance_nonce(&self) -> &str {
        &self.nonce
    }

    /// The run manager (chat engine).
    pub fn runs(&self) -> &Arc<RunManager> {
        &self.runs
    }

    /// The settings store.
    pub fn settings(&self) -> &Arc<SettingsStore> {
        &self.settings
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
        let manifest: r_code_harness_protocol::HarnessManifest =
            serde_json::from_str(&manifest_text)
                .map_err(|e| ApplicationError::Plugin(format!("builtin manifest invalid: {e}")))?;
        // Already installed with identical bytes? Idempotent no-op.
        if let Ok(entries) = self.catalog.list() {
            if entries.iter().any(|entry| {
                entry.manifest.id == manifest.id && entry.package_ref.version == manifest.version
            }) {
                return Ok(None);
            }
        }
        let installed = self
            .catalog
            .install_from_directory(package_dir)
            .map_err(|e| ApplicationError::Plugin(e.to_string()))?;
        Ok(Some(installed))
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
        self.catalog
            .pin(&format!("attempt-{task_id}-1"), task_id, &effective)
            .map_err(|e| ApplicationError::Plugin(e.to_string()))?;
        self.kernel_tasks
            .pin_harness(task_id, effective.clone())
            .await
            .map_err(|e| ApplicationError::Task(e.to_string()))?;
        Ok(effective)
    }

    /// Rename a task (UI metadata).
    pub async fn rename_task(&self, task_id: &str, title: &str) -> Result<(), ApplicationError> {
        self.kernel_tasks
            .rename(task_id, title)
            .await
            .map_err(|e| ApplicationError::Task(e.to_string()))
    }

    /// Update per-task preferences (model / inference / mode) applied to
    /// future runs.
    pub async fn set_task_preferences(
        &self,
        task_id: &str,
        preferences: r_code_kernel::task::TaskPreferences,
    ) -> Result<(), ApplicationError> {
        self.kernel_tasks
            .set_preferences(task_id, preferences)
            .await
            .map_err(|e| ApplicationError::Task(e.to_string()))
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
    pub fn task_branches(&self) -> Vec<r_code_store::v2::tasks::TaskBranch> {
        self.store.branches().unwrap_or_default()
    }

    /// Send a message: enqueue + async run (see [`RunManager::send`]).
    pub async fn send_message(
        &self,
        task_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, ApplicationError> {
        self.runs
            .clone()
            .send(task_id, text)
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
        Ok(TaskDetailView {
            task_id: task_id.to_string(),
            title: state.title.clone().unwrap_or_else(|| default_title(&state)),
            kind: kind_label(&state.contract.kind),
            objective: state.contract.objective.clone(),
            state: phase_label(&state),
            running,
            model: state.preferences.model.clone(),
            inference: state.preferences.inference.clone(),
            mode: state.preferences.mode.clone(),
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
        r_code_kernel::task::TaskExecution::ReviewReady { .. } => "review-ready".into(),
        r_code_kernel::task::TaskExecution::Terminal { verdict } => match verdict {
            r_code_kernel::task::TaskVerdict::Verified { .. } => "verified".into(),
            r_code_kernel::task::TaskVerdict::Unverified { .. } => "unverified".into(),
            r_code_kernel::task::TaskVerdict::Blocked { .. } => "blocked".into(),
            r_code_kernel::task::TaskVerdict::Failed { .. } => "failed".into(),
            r_code_kernel::task::TaskVerdict::Cancelled { .. } => "cancelled".into(),
        },
    }
}
