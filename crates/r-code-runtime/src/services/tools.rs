//! Gateway tools adapted to the host `ToolService` port.
//!
//! Every call flows: common authorization (T12a) → operation-intent
//! persistence → Gateway execution (its own schema checks, risk-based
//! permission engine and audit ledger). Read-only workspaces deny write
//! tools before the Gateway sees them; cancellations propagate by refusing
//! calls whose generation is no longer live.

use crate::services::artifacts::ArtifactStore;
use crate::services::authorization::{
    AuthorizationDecision, AuthorizationService, EffectivePermissions, OperationDescriptor,
    ShellAuthority, WorkspaceCapability,
};
use crate::services::mutations::{MutationExecutionError, MutationExecutor, MutationFaultHook};
use crate::services::workspaces::TaskWorkspaceBinding;
use r_code_core::security::PathGuard;
use r_code_gateway::gateway::{ToolExecutionDirective, ToolGateway};
use r_code_gateway::tools::{
    CreateFileTool, DeleteFileTool, EditTool, ListFilesTool, ReadFileTool,
};
use r_code_gateway::tools_search::{GlobTool, SearchTool};
use r_code_harness_protocol::services::ToolCallRequest;
use r_code_harness_protocol::services::{
    OutputBlock, ToolCallError, ToolCallReply, ToolDescriptor,
};
use r_code_kernel::plans::PlanRevision;
use r_code_kernel::ports::{GenerationToken, JournalStore, ServiceError, ToolService};
use r_code_kernel::task::{WorkUnit, WorkspaceSnapshotRef};
use r_code_store::v1::V1Store;
use std::path::PathBuf;
use std::sync::Arc;

/// Write-effecting tools denied outright for read-only workspaces.
const WRITE_TOOLS: &[&str] = &["create_file", "delete_file", "edit", "apply_patch", "bash"];

/// The complete discoverable and executable tool surface before plan
/// approval. Keeping one allowlist for both gates prevents guessed calls from
/// reaching a write-capable gateway registration.
const PLANNING_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "search",
    "glob",
    "git_status",
    "git_log",
    "git_diff",
];
const MEDIATED_WRITE_TOOLS: &[&str] = &["create_file", "edit", "apply_patch", "delete_file"];
/// P28: the exact-approved sandboxed Shell tool. Not a write tool: it
/// spawns nothing this wave — the surface exists only for the frozen
/// effect authority, and every decision is durable.
pub const SHELL_TOOL: &str = "shell";

/// Run-scoped read-only tools rooted at the exact canonical checkout frozen
/// into the run snapshot. An unbound conversation exposes an empty catalog and
/// denies every call.
/// P30: the read-only git projections — status/log/diff over the P29
/// restricted gix reader. There is NO generic git tool: each projection is
/// its own descriptor with its own bounds, and none of them can write.
/// FR-8 (M1a-11, D9): the delegation discipline travels in the tool
/// descriptions (four-element contract placeholder; full polish is M1b).
const CHILDREN_SPAWN_DESCRIPTION: &str = "Delegate one self-contained task to a child agent that runs with its own session and a read-only tool surface. Provide the objective with (1) what to produce, (2) the output format, (3) which tools/sources to use, (4) the task boundary (what NOT to do). Discipline: never spawn unless the user asked or the plan calls for it; keep critical-path work local; child tasks must be self-contained with disjoint write sets; after delegating, do not redo the child's work and do not reflexively wait — continue your own steps.";

fn text_reply(_tool: &str, text: &str) -> ToolCallReply {
    ToolCallReply {
        output: vec![r_code_harness_protocol::services::OutputBlock::Text {
            text: text.to_string(),
        }],
        error: None,
    }
}

fn children_tool_descriptors() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            name: "children_spawn".into(),
            description: CHILDREN_SPAWN_DESCRIPTION.into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "objective": {"type": "string", "description": "The self-contained task for the child (four-element delegation contract)."},
                    "ceiling": {"type": "string", "enum": ["read-only", "approval-required", "full"], "description": "Requested permission ceiling; the parent bounds it."},
                    "harness": {"type": "string", "description": "Optional harness id; defaults to the parent's."},
                    "budget_share": {"type": "integer", "description": "Optional budget share in tokens."}
                },
                "required": ["objective"]
            }),
        },
        ToolDescriptor {
            name: "children_wait".into(),
            description: "Block until one child completes (minutes-scale timeout). Returns the child's report (outcome + summary). Do not wait reflexively — continue your own work while children run.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "child_task_id": {"type": "string"},
                    "timeout_ms": {"type": "integer", "description": "Optional timeout in milliseconds (default 300000, max 1800000)."}
                },
                "required": ["child_task_id"]
            }),
        },
        ToolDescriptor {
            name: "children_close".into(),
            description: "Close one child after collecting its report, reclaiming its concurrency slot. Children keep occupying a slot until closed.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "child_task_id": {"type": "string"}
                },
                "required": ["child_task_id"]
            }),
        },
    ]
}
fn git_read_descriptors() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            name: "git_status".into(),
            description: "Bounded read-only git status (porcelain-normalized entries) over \
                          the daemon's restricted gix reader; zero metadata changes."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            }),
        },
        ToolDescriptor {
            name: "git_log".into(),
            description: "Bounded read-only commit log from HEAD (hex ids, walk order) \
                          over the restricted gix reader."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            }),
        },
        ToolDescriptor {
            name: "git_diff".into(),
            description: "Bounded text diff of two ascii/base64 contents; binary content \
                          returns metadata only. Pure computation, no repository access."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "old_base64": {"type": "string"},
                    "new_base64": {"type": "string"}
                },
                "required": ["old_base64", "new_base64"],
                "additionalProperties": false
            }),
        },
    ]
}

pub struct PlanningToolService {
    gateway: Option<Arc<ToolGateway>>,
    workspace_guard: Option<PathGuard>,
    /// P30: the canonical git directory of the bound workspace (None when
    /// unbound): the ONLY input the read-only git projections receive.
    git_dir: Option<PathBuf>,
    /// FR-1.5 (M1a-07): read-tool hit reporting feeds the JIT injection
    /// projection. None disables JIT for this surface.
    jit_tracker: Option<Arc<std::sync::Mutex<crate::services::project_instructions::JitTracker>>>,
    /// Audit caller identity: "harness-plugin" for main runs,
    /// "subagent:child-N" for spawned children (the gateway gate keys on
    /// the subagent: prefix).
    caller: String,
    /// FR-8 (M1a-11): host catalog children tools (D9). None on surfaces
    /// without children (child runs themselves — the structural nesting
    /// fence — and WorkUnit sub-runs in the first step).
    child_controls: Option<Arc<crate::services::children_executor::ChildControls>>,
}

impl PlanningToolService {
    pub fn from_workspace(workspace: &WorkspaceSnapshotRef) -> Result<Self, ServiceError> {
        if workspace.workspace_identity == "unbound-read-only" {
            return Ok(Self {
                gateway: None,
                workspace_guard: None,
                git_dir: None,
                jit_tracker: None,
                caller: "harness-plugin".into(),
                child_controls: None,
            });
        }
        let guard = PathGuard::new(PathBuf::from(&workspace.canonical_root))
            .map_err(|error| ServiceError::Failure(format!("workspace guard: {error}")))?;
        let mut gateway =
            ToolGateway::new(Arc::new(r_code_gateway::permission::PermissionEngine::new()));
        gateway.register(Box::new(ReadFileTool));
        gateway.register(Box::new(ListFilesTool));
        gateway.register(Box::new(SearchTool));
        gateway.register(Box::new(GlobTool));
        Ok(Self {
            git_dir: Some(PathBuf::from(&workspace.canonical_root).join(".git")),
            gateway: Some(Arc::new(gateway)),
            workspace_guard: Some(guard),
            jit_tracker: None,
            caller: "harness-plugin".into(),
            child_controls: None,
        })
    }

    /// Audit caller for gateway executions ("harness-plugin" default).
    pub fn with_caller(mut self, caller: String) -> Self {
        self.caller = caller;
        self
    }

    /// FR-8 (M1a-11): attach the children controls so the catalog exposes
    /// children_spawn/wait/close (D9 host catalog tools).
    pub fn with_child_controls(
        mut self,
        controls: Arc<crate::services::children_executor::ChildControls>,
    ) -> Self {
        self.child_controls = Some(controls);
        self
    }

    /// FR-8 (M1a-11): dispatch the children catalog tools onto the shared
    /// controls. These are host-run controls; outputs stay bounded JSON.
    fn children_tool_call(&self, call: &ToolCallRequest) -> ToolCallReply {
        let Some(controls) = &self.child_controls else {
            return denied(
                &call.tool,
                "children tools are not available on this surface",
            );
        };
        match call.tool.as_str() {
            "children_spawn" => {
                let objective = call
                    .input
                    .get("objective")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if objective.is_empty() {
                    return denied(&call.tool, "children_spawn requires a non-empty objective");
                }
                let permissions = match call.input.get("ceiling").and_then(|value| value.as_str()) {
                    None | Some("read-only") | Some("readonly") => {
                        r_code_harness_protocol::services::PermissionCeiling::ReadOnly
                    }
                    Some("approval-required") => {
                        r_code_harness_protocol::services::PermissionCeiling::ApprovalRequired
                    }
                    Some("full") => r_code_harness_protocol::services::PermissionCeiling::Full,
                    Some(other) => {
                        return denied(&call.tool, &format!("unknown ceiling {other:?}"))
                    }
                };
                let request = r_code_harness_protocol::services::ChildrenSpawnRequest {
                    objective,
                    harness: call
                        .input
                        .get("harness")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    permissions,
                    budget_share: call.input.get("budget_share").cloned(),
                };
                match controls.request_spawn(request) {
                    Ok(child_task_id) => text_reply(
                        &call.tool,
                        &format!("{{\"childTaskId\": \"{child_task_id}\", \"queued\": true}}"),
                    ),
                    Err(error) => denied(&call.tool, &error),
                }
            }
            "children_wait" => {
                let Some(child_task_id) = call
                    .input
                    .get("child_task_id")
                    .and_then(|value| value.as_str())
                else {
                    return denied(&call.tool, "children_wait requires child_task_id");
                };
                let timeout_ms = call
                    .input
                    .get("timeout_ms")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(5 * 60 * 1000)
                    .clamp(1, 30 * 60 * 1000);
                let supervisor = Arc::clone(&controls.supervisor);
                let child_id = child_task_id.to_string();
                // Blocking on the condvar inside the tool call is the
                // documented primitive (FR-8.1); the ToolService::call
                // future runs on the router's runtime without holding any
                // supervisor lock (wait_child manages its own locking).
                let outcome = tokio::task::block_in_place(|| {
                    crate::services::children_executor::wait_child_blocking(
                        &supervisor,
                        &child_id,
                        timeout_ms,
                    )
                });
                match outcome {
                    Ok(r_code_kernel::children::ChildWait::Completed(report)) => {
                        let payload = serde_json::json!({
                            "childTaskId": report.child_task_id,
                            "outcome": report.outcome,
                            "summary": report.summary.clone().unwrap_or_default(),
                        });
                        text_reply(&call.tool, &payload.to_string())
                    }
                    Ok(r_code_kernel::children::ChildWait::Cancelled) => {
                        denied(&call.tool, &format!("child {child_task_id} was cancelled"))
                    }
                    Ok(r_code_kernel::children::ChildWait::Running) => denied(
                        &call.tool,
                        &format!("wait for child {child_task_id} timed out without completion"),
                    ),
                    Err(error) => denied(&call.tool, &error),
                }
            }
            "children_close" => {
                let Some(child_task_id) = call
                    .input
                    .get("child_task_id")
                    .and_then(|value| value.as_str())
                else {
                    return denied(&call.tool, "children_close requires child_task_id");
                };
                let close = controls
                    .supervisor
                    .lock()
                    .map_err(|_| "children supervisor poisoned".to_string())
                    .and_then(|mut guard| guard.close(child_task_id).map_err(|e| e.to_string()));
                match close {
                    Ok(()) => {
                        controls.notify_slot_changed();
                        text_reply(
                            &call.tool,
                            &format!("{{\"childTaskId\": \"{child_task_id}\", \"closed\": true}}"),
                        )
                    }
                    Err(error) => denied(&call.tool, &error),
                }
            }
            _ => denied(&call.tool, "unknown children tool"),
        }
    }
    /// FR-1.5 (M1a-07): attach the JIT hit tracker.
    pub fn with_jit_tracker(
        mut self,
        tracker: Arc<std::sync::Mutex<crate::services::project_instructions::JitTracker>>,
    ) -> Self {
        self.jit_tracker = Some(tracker);
        self
    }
}

#[async_trait::async_trait]
impl ToolService for PlanningToolService {
    async fn list(&self, _token: GenerationToken) -> Result<Vec<ToolDescriptor>, ServiceError> {
        let Some(gateway) = &self.gateway else {
            return Ok(Vec::new());
        };
        let mut descriptors: Vec<ToolDescriptor> = gateway
            .tool_specs()
            .into_iter()
            .filter(|spec| PLANNING_TOOLS.contains(&spec.name.as_str()))
            .map(|spec| ToolDescriptor {
                name: spec.name,
                description: spec.description,
                input_schema: spec.input_schema,
            })
            .collect();
        // P30: the read-only git projections ride the planning surface —
        // listed only when a git dir is bound.
        if self.git_dir.is_some() {
            descriptors.extend(git_read_descriptors());
        }
        // FR-8 (M1a-11, D9): children host catalog tools — they join the
        // tool catalog (and therefore its frozen digest) only on surfaces
        // that own children.
        if self.child_controls.is_some() {
            descriptors.extend(children_tool_descriptors());
        }
        Ok(descriptors)
    }

    async fn call(
        &self,
        token: GenerationToken,
        call: ToolCallRequest,
    ) -> Result<ToolCallReply, ServiceError> {
        // FR-8 (M1a-11): children tools bypass the planning allowlist —
        // they are host-run controls, not workspace reads.
        if matches!(
            call.tool.as_str(),
            "children_spawn" | "children_wait" | "children_close"
        ) {
            return Ok(self.children_tool_call(&call));
        }
        if !PLANNING_TOOLS.contains(&call.tool.as_str()) {
            return Ok(denied(
                &call.tool,
                "tool is unavailable before plan approval",
            ));
        }
        // P30: the read-only git projections are served here — never by the
        // gateway, never with a write surface.
        if matches!(call.tool.as_str(), "git_status" | "git_log" | "git_diff") {
            return Ok(self.git_projection(&call));
        }
        let (Some(gateway), Some(workspace_guard)) = (&self.gateway, &self.workspace_guard) else {
            return Ok(denied(&call.tool, "task has no bound workspace"));
        };
        // FR-1.5 (M1a-07): report the directory this read tool touched to
        // the JIT projection layer (the canonical transcript is unaffected).
        if let Some(tracker) = &self.jit_tracker {
            if let Some(path) = call.input.get("path").and_then(|value| value.as_str()) {
                if !path.trim().is_empty() {
                    let candidate = std::path::Path::new(path);
                    let hit = if call.tool == "read_file" {
                        candidate
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_default()
                    } else {
                        candidate.to_path_buf()
                    };
                    if !hit.as_os_str().is_empty() {
                        if let Ok(mut tracker) = tracker.lock() {
                            tracker.note_hit_dir(&hit);
                        }
                    }
                }
            }
        }
        let outcome = match gateway
            .execute_call_with_access_mode_and_workspace_guard(
                &format!("task:{}", token.run_id),
                &token.run_id,
                &call.tool,
                call.input,
                Some(self.caller.as_str()),
                r_code_core::dto::ProjectAccessMode::RiskBased,
                Some(workspace_guard),
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(r_code_core::error::ProductError::PermissionError(reason)) => {
                return Ok(denied(&call.tool, &reason));
            }
            Err(error) => {
                return Ok(ToolCallReply {
                    output: Vec::new(),
                    error: Some(ToolCallError {
                        code: "tool-error".to_string(),
                        message: error.to_string(),
                        denied_by: None,
                    }),
                });
            }
        };
        if outcome.is_error {
            Ok(ToolCallReply {
                output: Vec::new(),
                error: Some(ToolCallError {
                    code: "tool-error".to_string(),
                    message: outcome.content,
                    denied_by: None,
                }),
            })
        } else {
            Ok(ToolCallReply {
                output: vec![OutputBlock::Text {
                    text: outcome.content,
                }],
                error: None,
            })
        }
    }
}

impl PlanningToolService {
    /// P30: serve one bounded read-only git projection. git_diff is pure
    /// computation over supplied contents; git_status/git_log read through
    /// the restricted P29 reader (canonical dir only, isolated
    /// permissions, no trusted filter sections, zero metadata changes).
    fn git_projection(&self, call: &ToolCallRequest) -> ToolCallReply {
        use crate::services::git_read as git;
        match call.tool.as_str() {
            "git_diff" => {
                use base64::Engine as _;
                let engine = base64::engine::general_purpose::STANDARD;
                let (Ok(old), Ok(new)) = (
                    engine.decode(call.input["old_base64"].as_str().unwrap_or_default()),
                    engine.decode(call.input["new_base64"].as_str().unwrap_or_default()),
                ) else {
                    return denied(&call.tool, "git_diff requires two base64 contents");
                };
                match git::diff_blobs(&old, &new, &git::DiffBounds::default()) {
                    Ok(report) => ToolCallReply {
                        output: vec![OutputBlock::Json {
                            value: serde_json::to_value(DiffReportJson::from(report))
                                .unwrap_or_default(),
                        }],
                        error: None,
                    },
                    Err(error) => denied(&call.tool, &error.to_string()),
                }
            }
            "git_status" | "git_log" => {
                let Some(git_dir) = &self.git_dir else {
                    return denied(&call.tool, "task has no bound git repository");
                };
                let limit = call.input["limit"].as_u64().unwrap_or(100) as usize;
                let Ok(reader) = git::open_read_only(git_dir) else {
                    return denied(&call.tool, "the bound git directory cannot be opened");
                };
                let outcome = if call.tool == "git_status" {
                    reader
                        .status(limit.min(git::MAX_STATUS_ENTRIES))
                        .map(|report| {
                            serde_json::to_value(StatusJson::from(report)).unwrap_or_default()
                        })
                } else {
                    reader.log(limit.min(git::MAX_LOG_COMMITS)).map(|report| {
                        serde_json::to_value(LogJson::from(report)).unwrap_or_default()
                    })
                };
                match outcome {
                    Ok(value) => ToolCallReply {
                        output: vec![OutputBlock::Json { value }],
                        error: None,
                    },
                    Err(error) => denied(&call.tool, &error.to_string()),
                }
            }
            _ => denied(&call.tool, "unknown git projection"),
        }
    }
}

/// JSON mirrors of the bounded reports (camelCase wire shapes).
#[derive(serde::Serialize)]
struct DiffReportJson {
    hunks: Vec<DiffHunkJson>,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    binary: Option<[u64; 2]>,
}

#[derive(serde::Serialize)]
struct DiffHunkJson {
    old_start: u32,
    new_start: u32,
    lines: Vec<DiffLineJson>,
}

#[derive(serde::Serialize)]
struct DiffLineJson {
    kind: &'static str,
    text: String,
}

#[derive(serde::Serialize)]
struct StatusJson {
    entries: Vec<StatusEntryJson>,
    truncated: bool,
}

#[derive(serde::Serialize)]
struct StatusEntryJson {
    path: String,
    change: &'static str,
    stage: &'static str,
}

#[derive(serde::Serialize)]
struct LogJson {
    commits: Vec<String>,
    truncated: bool,
}

impl From<crate::services::git_read::DiffReport> for DiffReportJson {
    fn from(report: crate::services::git_read::DiffReport) -> Self {
        Self {
            hunks: report
                .hunks
                .into_iter()
                .map(|hunk| DiffHunkJson {
                    old_start: hunk.old_start,
                    new_start: hunk.new_start,
                    lines: hunk
                        .lines
                        .into_iter()
                        .map(|line| DiffLineJson {
                            kind: match line.kind {
                                crate::services::git_read::DiffLineKind::Context => "context",
                                crate::services::git_read::DiffLineKind::Added => "added",
                                crate::services::git_read::DiffLineKind::Removed => "removed",
                            },
                            text: line.text,
                        })
                        .collect(),
                })
                .collect(),
            truncated: report.truncated,
            binary: report.binary.map(|(old, new)| [old, new]),
        }
    }
}

impl From<crate::services::git_read::StatusReport> for StatusJson {
    fn from(report: crate::services::git_read::StatusReport) -> Self {
        Self {
            entries: report
                .entries
                .into_iter()
                .map(|entry| StatusEntryJson {
                    path: entry.path,
                    change: match entry.change {
                        crate::services::git_read::ChangeKind::Added => "added",
                        crate::services::git_read::ChangeKind::Modified => "modified",
                        crate::services::git_read::ChangeKind::Deleted => "deleted",
                        crate::services::git_read::ChangeKind::Renamed => "renamed",
                        crate::services::git_read::ChangeKind::Untracked => "untracked",
                    },
                    stage: match entry.stage {
                        crate::services::git_read::Stage::HeadToIndex => "head-to-index",
                        crate::services::git_read::Stage::IndexToWorktree => "index-to-worktree",
                    },
                })
                .collect(),
            truncated: report.truncated,
        }
    }
}

impl From<crate::services::git_read::LogReport> for LogJson {
    fn from(report: crate::services::git_read::LogReport) -> Self {
        Self {
            commits: report.commits,
            truncated: report.truncated,
        }
    }
}

/// Execution catalog bound to one exact approved WorkUnit. Read tools keep
/// the planning capability; every discoverable write goes through the
/// durable [`MutationExecutor`].
pub struct ExecutionToolService {
    reads: PlanningToolService,
    mutations: Option<MutationExecutor>,
    attempt_id: String,
    /// P28: the frozen Shell authority. `None` (or `Denied` never injected)
    /// keeps the tool undiscoverable and every call refused.
    shell: Option<ShellAuthority>,
    store: Option<Arc<V1Store>>,
}

impl ExecutionToolService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plan: &PlanRevision,
        unit: &WorkUnit,
        binding: TaskWorkspaceBinding,
        store: Arc<V1Store>,
        artifacts: Arc<ArtifactStore>,
        attempt_id: impl Into<String>,
    ) -> Result<Self, ServiceError> {
        let canonical_root = binding.canonical_root.to_string_lossy().into_owned();
        let reads = PlanningToolService::from_workspace(&WorkspaceSnapshotRef {
            workspace_identity: format!(
                "sha256:{}",
                crate::services::artifacts::sha256_hex(canonical_root.as_bytes())
            ),
            canonical_root,
            baseline_sha256: plan.material().workspace_baseline.clone(),
        })?;
        let has_lease_scope =
            unit.repo_exclusive || !unit.read_paths.is_empty() || !unit.write_paths.is_empty();
        let attempt_owned: String = attempt_id.into();
        let mutations = has_lease_scope
            .then(|| {
                MutationExecutor::new(
                    plan,
                    unit,
                    binding,
                    store.clone(),
                    artifacts,
                    attempt_owned.clone(),
                )
            })
            .transpose()
            .map_err(mutation_service_error)?;
        Ok(Self {
            reads,
            mutations,
            attempt_id: attempt_owned,
            shell: None,
            store: Some(store),
        })
    }

    /// P28: freeze the Shell authority under the final predicate (run
    /// manager injects it only when the exact resolution is not Denied).
    /// Callers that never inject keep the surface dormant.
    pub fn with_shell_authority(mut self, authority: ShellAuthority) -> Self {
        self.shell = Some(authority);
        self
    }

    pub fn with_fault_hook(mut self, hook: Arc<dyn MutationFaultHook>) -> Self {
        self.mutations = self
            .mutations
            .take()
            .map(|executor| executor.with_fault_hook(hook));
        self
    }

    /// Explicit release for RunManager. Drop deliberately does not release a
    /// durable lease because process termination may still be unproven.
    pub fn release(&self) -> Result<bool, ServiceError> {
        self.mutations
            .as_ref()
            .map(MutationExecutor::release)
            .transpose()
            .map(|released| released.unwrap_or(false))
            .map_err(mutation_service_error)
    }

    pub fn mutation_executor(&self) -> Option<&MutationExecutor> {
        self.mutations.as_ref()
    }
}

#[async_trait::async_trait]
impl ToolService for ExecutionToolService {
    async fn list(&self, token: GenerationToken) -> Result<Vec<ToolDescriptor>, ServiceError> {
        let mut tools = self.reads.list(token).await?;
        if self
            .mutations
            .as_ref()
            .is_some_and(MutationExecutor::supports_writes)
        {
            tools.extend(mediated_write_descriptors());
        }
        if self.shell.is_some() {
            tools.push(shell_descriptor());
        }
        Ok(tools)
    }

    async fn call(
        &self,
        token: GenerationToken,
        call: ToolCallRequest,
    ) -> Result<ToolCallReply, ServiceError> {
        if PLANNING_TOOLS.contains(&call.tool.as_str()) {
            return self.reads.call(token, call).await;
        }
        if call.tool == SHELL_TOOL {
            return Ok(self.shell_call(call).await);
        }
        if !MEDIATED_WRITE_TOOLS.contains(&call.tool.as_str()) {
            return Ok(denied(&call.tool, "tool is not approved for this WorkUnit"));
        }
        let Some(executor) = self.mutations.as_ref() else {
            return Ok(denied(&call.tool, "WorkUnit has no approved write scope"));
        };
        if !executor.supports_writes() {
            return Ok(denied(&call.tool, "WorkUnit has no approved write scope"));
        }
        let Some(operation_key) = call.operation_key.as_ref() else {
            return Ok(denied(&call.tool, "write tool requires an operation key"));
        };
        match executor.execute(&call.tool, &call.input, operation_key) {
            Ok(reply) => Ok(ToolCallReply {
                output: vec![OutputBlock::Json {
                    value: serde_json::to_value(reply).unwrap_or_default(),
                }],
                error: None,
            }),
            Err(error) => Ok(mutation_error_reply(&call.tool, error)),
        }
    }
}

fn mediated_write_descriptors() -> Vec<ToolDescriptor> {
    vec![
        write_descriptor(
            "create_file",
            "Create one approved file; fails if it exists.",
            &["path", "content"],
        ),
        write_descriptor(
            "edit",
            "Replace one exact unique string, or all exact matches.",
            &["path", "old_string", "new_string"],
        ),
        write_descriptor(
            "apply_patch",
            "Atomically replace one approved file with full content.",
            &["path", "content"],
        ),
        write_descriptor(
            "delete_file",
            "Delete one approved existing file.",
            &["path"],
        ),
    ]
}

fn write_descriptor(name: &str, description: &str, required: &[&str]) -> ToolDescriptor {
    let mut properties = serde_json::Map::new();
    properties.insert("path".into(), serde_json::json!({"type": "string"}));
    match name {
        "edit" => {
            properties.insert("old_string".into(), serde_json::json!({"type": "string"}));
            properties.insert("new_string".into(), serde_json::json!({"type": "string"}));
            properties.insert("replace_all".into(), serde_json::json!({"type": "boolean"}));
            properties.insert(
                "expected_revision".into(),
                serde_json::json!({"type": "string"}),
            );
        }
        "create_file" | "apply_patch" => {
            properties.insert("content".into(), serde_json::json!({"type": "string"}));
        }
        _ => {}
    }
    ToolDescriptor {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        }),
    }
}

impl ExecutionToolService {
    /// P28: the exact-approved Shell call. The spec is hashed (P28.1), the
    /// ceiling came frozen from the exact resolution (P28.2), and the
    /// decision — including the honest spawn refusal — is durable in the
    /// operation receipts (P28.3). No sandbox backend is activated this
    /// wave, so the execution arm refuses SafeDisabled and NEVER spawns:
    /// the surface exists so the authority, hash and audit path are real.
    async fn shell_call(&self, call: ToolCallRequest) -> ToolCallReply {
        let Some(authority) = self.shell else {
            return denied(&call.tool, "shell is not approved for this WorkUnit");
        };
        let Some(operation_key) = call.operation_key.clone() else {
            return denied(&call.tool, "shell requires an operation key");
        };
        let ceiling = match authority {
            ShellAuthority::OfflineExact => r_code_harness_protocol::NetworkCeiling::Offline,
            ShellAuthority::ApprovedExact { ceiling } => ceiling,
            ShellAuthority::Denied { reason } => {
                return denied(&call.tool, reason);
            }
        };
        let spec = ShellSpec::from_input(&call.input, ceiling);
        let input_hash = spec.hash();
        let key = r_code_harness_protocol::OperationKey(operation_key.0);
        // Idempotence at the durable layer: a replay of the same
        // operation key with the same spec returns the recorded outcome
        // and never re-decides; a different spec under the same key is a
        // guessed/stale call and refused.
        if let Some(store) = self.store.as_ref() {
            if let Some(receipt) =
                r_code_kernel::ports::JournalStore::load_receipt(&**store, &self.attempt_id, &key)
                    .await
            {
                if receipt.input_hash != input_hash {
                    return denied(
                        &call.tool,
                        "operation key already used for a different spec",
                    );
                }
                return ToolCallReply {
                    output: vec![OutputBlock::Json {
                        value: serde_json::json!({
                            "replayed": true,
                            "inputHash": input_hash,
                            "outcome": receipt.outcome,
                        }),
                    }],
                    error: None,
                };
            }
        }
        let outcome = r_code_kernel::task::ReceiptOutcome::Rejected {
            reason: "sandbox backend is SafeDisabled on this platform: the exact-approved \
                     shell surface refuses to spawn"
                .to_string(),
        };
        if let Some(store) = self.store.as_ref() {
            let receipt = r_code_kernel::task::OperationReceipt {
                attempt_id: self.attempt_id.clone(),
                operation_key: key,
                method: SHELL_TOOL.to_string(),
                input_hash: input_hash.clone(),
                outcome: outcome.clone(),
            };
            let _ = r_code_kernel::ports::JournalStore::save_receipt(&**store, receipt).await;
        }
        ToolCallReply {
            output: vec![OutputBlock::Json {
                value: serde_json::json!({
                    "inputHash": input_hash,
                    "ceiling": format!("{ceiling:?}").to_kebab(),
                    "durable": true,
                }),
            }],
            error: Some(ToolCallError {
                code: "safe-disabled".to_string(),
                message: "no sandbox backend is activated on this platform; the shell surface                           is exact-approved but refuses to spawn"
                    .to_string(),
                denied_by: None,
            }),
        }
    }
}

/// P28.1: the complete frozen Shell material — command, argv, cwd, env
/// allowlist, profile and the exact ceiling — hashed canonically so one
/// operation key can never carry two different launches.
struct ShellSpec {
    command: String,
    argv: Vec<String>,
    cwd: String,
    env: Vec<String>,
    profile: &'static str,
    ceiling: r_code_harness_protocol::NetworkCeiling,
}

impl ShellSpec {
    fn from_input(
        input: &serde_json::Value,
        ceiling: r_code_harness_protocol::NetworkCeiling,
    ) -> Self {
        let command = input["command"].as_str().unwrap_or_default().to_string();
        let argv = input["argv"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let cwd = input["cwd"].as_str().unwrap_or_default().to_string();
        let env = input["env"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            command,
            argv,
            cwd,
            env,
            profile: "sandboxed-shell-v1",
            ceiling,
        }
    }

    fn hash(&self) -> String {
        crate::services::artifacts::sha256_hex(
            serde_json::json!({
                "command": self.command,
                "argv": self.argv,
                "cwd": self.cwd,
                "env": self.env,
                "profile": self.profile,
                "ceiling": format!("{:?}", self.ceiling),
            })
            .to_string()
            .as_bytes(),
        )
    }
}

/// Kebab-case the debug ceiling names for the audit reply.
trait ToKebab {
    fn to_kebab(&self) -> String;
}

impl ToKebab for str {
    fn to_kebab(&self) -> String {
        self.to_ascii_lowercase().replace(' ', "-")
    }
}

fn shell_descriptor() -> ToolDescriptor {
    let mut properties = serde_json::Map::new();
    properties.insert("command".into(), serde_json::json!({"type": "string"}));
    properties.insert(
        "argv".into(),
        serde_json::json!({"type": "array", "items": {"type": "string"}}),
    );
    properties.insert("cwd".into(), serde_json::json!({"type": "string"}));
    properties.insert(
        "env".into(),
        serde_json::json!({"type": "array", "items": {"type": "string"}}),
    );
    ToolDescriptor {
        name: SHELL_TOOL.to_string(),
        description: "Exact-approved sandboxed shell: offline by default, networked only                       under an exact active effect approval; every launch is hashed and                       auditable."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": ["command"],
            "additionalProperties": false,
        }),
    }
}

fn mutation_error_reply(tool: &str, error: MutationExecutionError) -> ToolCallReply {
    let code = match error {
        MutationExecutionError::Denied | MutationExecutionError::PlanMismatch => "denied",
        MutationExecutionError::InvalidInput(_) => "invalid-input",
        MutationExecutionError::Conflict => "conflict",
        MutationExecutionError::Lease => "stale-lease",
        _ => "tool-error",
    };
    ToolCallReply {
        output: Vec::new(),
        error: Some(ToolCallError {
            code: code.to_string(),
            message: error.to_string(),
            denied_by: Some(format!("host/mutation-executor/{tool}")),
        }),
    }
}

fn mutation_service_error(error: MutationExecutionError) -> ServiceError {
    ServiceError::Failure(error.to_string())
}

/// The host tool service over the existing Gateway.
pub struct GatewayToolService {
    gateway: Arc<ToolGateway>,
    authorization: Arc<AuthorizationService>,
    store: Arc<dyn JournalStore>,
    workspace: WorkspaceCapability,
    permissions: EffectivePermissions,
}

impl GatewayToolService {
    /// Compose with an already-configured gateway (tools registered,
    /// permission engine installed).
    pub fn new(
        gateway: Arc<ToolGateway>,
        authorization: Arc<AuthorizationService>,
        store: Arc<dyn JournalStore>,
        workspace: WorkspaceCapability,
        permissions: EffectivePermissions,
    ) -> Self {
        Self {
            gateway,
            authorization,
            store,
            workspace,
            permissions,
        }
    }

    /// A gateway pre-registered with the core file tools.
    pub fn with_core_tools(
        authorization: Arc<AuthorizationService>,
        store: Arc<dyn JournalStore>,
        workspace: WorkspaceCapability,
        permissions: EffectivePermissions,
    ) -> Self {
        let mut gateway =
            ToolGateway::new(Arc::new(r_code_gateway::permission::PermissionEngine::new()));
        gateway.register(Box::new(ReadFileTool));
        gateway.register(Box::new(CreateFileTool));
        gateway.register(Box::new(EditTool));
        gateway.register(Box::new(DeleteFileTool));
        Self::new(
            Arc::new(gateway),
            authorization,
            store,
            workspace,
            permissions,
        )
    }

    fn access_mode(&self) -> r_code_core::dto::ProjectAccessMode {
        use r_code_core::dto::ProjectAccessMode;
        use r_code_harness_protocol::services::PermissionCeiling;
        match self.permissions.ceiling {
            // Read-only workspaces already deny write tools above; reads
            // flow through risk-based checks without approval friction.
            PermissionCeiling::ReadOnly => ProjectAccessMode::RiskBased,
            PermissionCeiling::ApprovalRequired => ProjectAccessMode::RequestApproval,
            PermissionCeiling::Full => ProjectAccessMode::FullAccess,
        }
    }

    async fn persist_intent(
        &self,
        token: &GenerationToken,
        tool: &str,
        input: &serde_json::Value,
    ) -> Result<(), ServiceError> {
        // Persist an operation intent before the effect runs; the RPC router
        // (T10) keys dedup on the plugin-provided operation_key, this intent
        // makes the in-flight effect itself durable.
        let hash = r_code_harness_protocol::canonical_input_hash(input);
        self.store
            .save_receipt(r_code_kernel::task::OperationReceipt {
                attempt_id: format!("tool:{}", token.run_id),
                operation_key: r_code_harness_protocol::OperationKey(format!(
                    "intent:{tool}:{hash}"
                )),
                method: format!("host.tools.call:{tool}"),
                input_hash: hash,
                outcome: r_code_kernel::task::ReceiptOutcome::Completed {
                    result: serde_json::json!({"intent": true}),
                },
            })
            .await
    }
}

#[async_trait::async_trait]
impl ToolService for GatewayToolService {
    async fn list(&self, _token: GenerationToken) -> Result<Vec<ToolDescriptor>, ServiceError> {
        Ok(self
            .gateway
            .tool_specs()
            .into_iter()
            .map(|spec| ToolDescriptor {
                name: spec.name,
                description: spec.description,
                input_schema: spec.input_schema,
            })
            .collect())
    }

    async fn call(
        &self,
        token: GenerationToken,
        call: ToolCallRequest,
    ) -> Result<ToolCallReply, ServiceError> {
        // 1. Read-only workspaces deny write tools before the Gateway.
        if matches!(self.workspace, WorkspaceCapability::ReadOnly { .. })
            && WRITE_TOOLS.contains(&call.tool.as_str())
        {
            return Ok(denied(&call.tool, "read-only workspace"));
        }

        // 2. Common authorization: descriptor from tool name + argv-ish
        // string arguments, so shell tools cannot slip past.
        let argv = call
            .input
            .as_object()
            .map(|object| {
                object
                    .values()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let cwd = call
            .input
            .get("path")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let descriptor = OperationDescriptor::tool_call(&call.tool, argv, cwd);
        let decision = self.authorization.authorize(
            &descriptor,
            &self.workspace,
            &self.permissions,
            &Default::default(),
        );
        match decision {
            AuthorizationDecision::Allowed => {}
            AuthorizationDecision::RequiresApproval { summary } => {
                return Ok(denied(&call.tool, &format!("approval required: {summary}")));
            }
            AuthorizationDecision::Denied(reason) => {
                return Ok(denied(&call.tool, &reason.to_string()));
            }
        }

        // 3. Persist the intent before execution.
        self.persist_intent(&token, &call.tool, &call.input).await?;

        // 4. Execute through the Gateway (schema checks, risk-based
        //    permissions, audit ledger). Permission refusals come back as
        //    structured denials, not transport failures.
        let outcome = match self
            .gateway
            .execute_call_with_access_mode(
                &format!("task:{}", token.run_id),
                &token.run_id,
                &call.tool,
                call.input.clone(),
                Some("harness-plugin"),
                self.access_mode(),
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(r_code_core::error::ProductError::PermissionError(reason)) => {
                return Ok(denied(&call.tool, &reason));
            }
            Err(error) => {
                return Ok(ToolCallReply {
                    output: vec![],
                    error: Some(ToolCallError {
                        code: "tool-error".into(),
                        message: error.to_string(),
                        denied_by: None,
                    }),
                });
            }
        };

        let text = outcome.content;
        let reply = if outcome.is_error {
            ToolCallReply {
                output: vec![],
                error: Some(ToolCallError {
                    code: "tool-error".into(),
                    message: text,
                    denied_by: None,
                }),
            }
        } else {
            ToolCallReply {
                output: vec![OutputBlock::Text { text }],
                error: None,
            }
        };
        let _ = ToolExecutionDirective::AllowAgentCompletion;
        Ok(reply)
    }
}

fn denied(tool: &str, reason: &str) -> ToolCallReply {
    ToolCallReply {
        output: vec![],
        error: Some(ToolCallError {
            code: "denied".into(),
            message: reason.to_string(),
            denied_by: Some(format!("host/tool-service/{tool}")),
        }),
    }
}
