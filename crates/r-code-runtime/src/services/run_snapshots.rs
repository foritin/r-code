//! Run-scoped resolution of immutable provider, prompt, workspace and grant inputs.

use crate::services::artifacts::sha256_hex;
use crate::services::models::ModelBroker;
use crate::services::settings_store::{RunProviderResolution, SettingsStore};
use crate::services::workspaces::TaskWorkspaceBinding;
use r_code_harness_protocol::{HostService, PackageRef};
use r_code_kernel::plans::PlanRevision;
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, RunGuard, ServiceError, StreamSink,
    ToolService,
};
use r_code_kernel::task::{
    ModelRoute, PermissionSnapshotRef, PromptSnapshotMode, PromptSnapshotRef, ProviderRouteKind,
    ProviderSnapshotRef, RunSnapshot, RunSnapshotMaterial, RunSnapshotPhase, TaskExecution,
    TaskKind, TaskState, WorkUnit, WorkspaceSnapshotRef,
};
use std::sync::Arc;

struct HarnessManagedModelService;

#[async_trait::async_trait]
impl ModelService for HarnessManagedModelService {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: r_code_harness_protocol::ModelStreamRequest,
        _sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        Err(ServiceError::Failure(
            "harness-managed routes cannot use the host model service".to_string(),
        ))
    }
}

pub struct FrozenRun {
    pub snapshot: RunSnapshot,
    pub models: Arc<dyn ModelService>,
}

pub struct RunSnapshotBuilder<'a> {
    settings: &'a SettingsStore,
    injected_models: &'a Arc<dyn ModelService>,
    tools: &'a Arc<dyn ToolService>,
    allow_injected_model_fallback: bool,
    /// FR-1: per-workspace instruction settings; defaults enable injection.
    instruction_settings: crate::services::project_instructions::InstructionSettings,
    /// FR-1: the personal global context.md path. None means no global
    /// layer (the runtime passes the real home path; tests pin temp dirs).
    global_instructions_path: Option<std::path::PathBuf>,
    /// P19A: the exact effect-approval source consulted when freezing a
    /// WorkUnit carrying effect authority. None is fail-closed: only
    /// ReadOnly/Offline units may freeze (the P19B runtime wires the
    /// real store-backed source).
    effect_approvals: Option<&'a dyn EffectApprovalSource>,
}

/// P19A: read-only lookup of exact persisted effect approvals. The
/// runtime (P19B-R) implements this over the V1Store; snapshot expansion
/// requires an EXACT match on every identity column.
pub trait EffectApprovalSource: Send + Sync {
    fn has_exact_approval(
        &self,
        task_id: &str,
        plan_revision: &str,
        work_unit_id: &str,
        effect_class: &str,
        network: &str,
        payload_hash: &str,
    ) -> bool;
}

impl<'a> RunSnapshotBuilder<'a> {
    pub fn new(
        settings: &'a SettingsStore,
        injected_models: &'a Arc<dyn ModelService>,
        tools: &'a Arc<dyn ToolService>,
        allow_injected_model_fallback: bool,
    ) -> Self {
        Self {
            settings,
            injected_models,
            tools,
            allow_injected_model_fallback,
            instruction_settings: Default::default(),
            global_instructions_path: None,
            effect_approvals: None,
        }
    }

    /// FR-1: attach per-workspace injection settings (the runtime resolves
    /// the persisted context settings, not the builder).
    pub fn with_instruction_settings(
        mut self,
        settings: crate::services::project_instructions::InstructionSettings,
    ) -> Self {
        self.instruction_settings = settings;
        self
    }

    /// FR-1: attach the personal global context.md path.
    pub fn with_global_instructions_path(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.global_instructions_path = path;
        self
    }

    /// P19A: attach the exact effect-approval source for snapshot
    /// expansion. Without it only ReadOnly/Offline units can freeze.
    pub fn with_effect_approvals(mut self, source: &'a dyn EffectApprovalSource) -> Self {
        self.effect_approvals = Some(source);
        self
    }

    pub async fn freeze(
        &self,
        state: &TaskState,
        package: &PackageRef,
        grants: &[HostService],
        guard: &RunGuard,
    ) -> Result<FrozenRun, String> {
        let workspace = resolve_workspace_snapshot(state)?;
        self.freeze_with_workspace(state, package, grants, guard, workspace)
            .await
    }

    /// Freeze using the exact workspace identity that was already used to
    /// construct this run's planning ToolService. This prevents the tool
    /// capability and snapshot root from being resolved independently.
    pub async fn freeze_with_workspace(
        &self,
        state: &TaskState,
        package: &PackageRef,
        grants: &[HostService],
        guard: &RunGuard,
        workspace: WorkspaceSnapshotRef,
    ) -> Result<FrozenRun, String> {
        self.freeze_material(
            state,
            package,
            grants,
            guard,
            workspace,
            RunSnapshotPhase::Planning,
            None,
        )
        .await
    }

    /// Freeze one exact approved WorkUnit. The caller may persist this only
    /// while the same approval remains active; V1Store revalidates it again.
    #[allow(clippy::too_many_arguments)]
    pub async fn freeze_execution_with_workspace(
        &self,
        state: &TaskState,
        package: &PackageRef,
        grants: &[HostService],
        guard: &RunGuard,
        workspace: WorkspaceSnapshotRef,
        plan: &PlanRevision,
        unit: &WorkUnit,
    ) -> Result<FrozenRun, String> {
        let approval = match &state.execution {
            TaskExecution::Ready { approval } => approval.clone(),
            _ => return Err("execution snapshot requires a Ready task".to_string()),
        };
        if plan.reference() != &approval.plan_revision
            || plan.material().task_id != state.contract.task_id
            || !plan.material().work_units.iter().any(|wire| {
                wire.id == unit.id
                    && wire.description == unit.description
                    && wire.dependencies == unit.dependencies
                    && wire.acceptance == unit.acceptance
                    && wire.read_paths == unit.read_paths
                    && wire.write_paths == unit.write_paths
                    && wire.repo_exclusive == unit.repo_exclusive
                    && wire.ephemeral_roots == unit.ephemeral_roots
                    && wire.effect_class == unit.effect_class
                    && wire.network_ceiling == unit.network_ceiling
            })
        {
            return Err("execution snapshot does not match the approved WorkUnit".to_string());
        }
        // P19A: a WorkUnit carrying effect authority (anything beyond the
        // conservative ReadOnly/Offline floor) expands ONLY behind an
        // exact persisted effect approval. With no approval source
        // attached this is fail-closed — no approval, no snapshot.
        if !unit.effect_class.is_read_only() || !unit.network_ceiling.is_offline() {
            let wire = plan
                .material()
                .work_units
                .iter()
                .find(|wire| wire.id == unit.id)
                .expect("the unit was just matched above");
            let payload_hash = r_code_harness_protocol::services::work_unit_payload_hash(wire);
            let approved = self.effect_approvals.is_some_and(|source| {
                source.has_exact_approval(
                    &state.contract.task_id,
                    plan.reference().as_str(),
                    &unit.id,
                    unit.effect_class.as_str(),
                    unit.network_ceiling.as_str(),
                    &payload_hash,
                )
            });
            if !approved {
                return Err(
                    "WorkUnit effect authority requires an exact active effect approval"
                        .to_string(),
                );
            }
        }
        let phase = match state.contract.kind {
            TaskKind::Implementation => RunSnapshotPhase::Execution { approval },
            TaskKind::Repair => RunSnapshotPhase::Repair { approval },
            _ => return Err("only implementation or repair tasks can execute".to_string()),
        };
        self.freeze_material(
            state,
            package,
            grants,
            guard,
            workspace,
            phase,
            Some(unit.id.clone()),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn freeze_material(
        &self,
        state: &TaskState,
        package: &PackageRef,
        grants: &[HostService],
        guard: &RunGuard,
        workspace: WorkspaceSnapshotRef,
        phase: RunSnapshotPhase,
        work_unit_id: Option<String>,
    ) -> Result<FrozenRun, String> {
        let (provider, models) = self.resolve_models(state, package, grants)?;
        let prompt = prompt_snapshot(state);
        let permissions = permission_snapshot(grants);
        let tool_catalog_sha256 = self.tool_catalog_digest(guard).await?;
        let instructions = self.instruction_set(&workspace);
        let snapshot = RunSnapshot::new(RunSnapshotMaterial {
            task_id: state.contract.task_id.clone(),
            task_revision: state.contract.revision,
            phase,
            work_unit_id,
            provider,
            prompt,
            workspace,
            permissions,
            harness_package: package.clone(),
            tool_catalog_sha256,
            instructions,
            inference: state.preferences.inference.clone(),
        })
        .map_err(|error| format!("无法构造运行快照：{error}"))?;
        Ok(FrozenRun { snapshot, models })
    }

    /// FR-1: plan the frozen instruction set for a bound workspace.
    /// Unbound read-only runs, missing roots, and disabled injection all
    /// yield the empty set — which the snapshot identity skips entirely, so
    /// pre-FR-1 and injection-off snapshots keep byte-stable ids.
    fn instruction_set(
        &self,
        workspace: &WorkspaceSnapshotRef,
    ) -> r_code_kernel::task::InstructionSetRef {
        use crate::services::project_instructions as engine;
        if self.instruction_settings.injection_enabled
            && !workspace.canonical_root.starts_with("unbound://")
        {
            let root = std::path::PathBuf::from(&workspace.canonical_root);
            if root.is_dir() {
                let discovery = engine::discover_repo_roots(&root);
                let (candidates, skipped) = engine::collect_frozen_candidates(
                    &discovery,
                    self.global_instructions_path.as_deref(),
                    &self.instruction_settings,
                );
                let bundle = engine::plan_bundle(&candidates, &skipped, &self.instruction_settings);
                // Identity discipline: an empty injected set (digest empty)
                // must normalize to the fully-default ref — the snapshot
                // field skips empty sets in canonical JSON, so any
                // non-default residue (preamble, skip facts) would break
                // save idempotency across a serialize round-trip.
                if bundle.digest.is_empty() {
                    return r_code_kernel::task::InstructionSetRef::default();
                }
                return r_code_kernel::task::InstructionSetRef {
                    digest: bundle.digest,
                    rendered: bundle.rendered,
                    entries: bundle
                        .entries
                        .iter()
                        .map(|entry| r_code_kernel::task::InstructionEntryRef {
                            layer: entry.layer.label().to_string(),
                            path: entry.path.clone(),
                            sha256: entry.sha256.clone(),
                            bytes: entry.bytes as u64,
                            status: format!("{:?}", entry.status).to_lowercase(),
                        })
                        .collect(),
                };
            }
        }
        r_code_kernel::task::InstructionSetRef::default()
    }

    fn resolve_models(
        &self,
        state: &TaskState,
        package: &PackageRef,
        grants: &[HostService],
    ) -> Result<(ProviderSnapshotRef, Arc<dyn ModelService>), String> {
        if let Some(ModelRoute::HarnessManaged {
            harness_id,
            model_id,
        }) = &state.preferences.model_route
        {
            if package.id.0 != *harness_id {
                return Err(format!(
                    "Harness-managed route requires {harness_id}, but {} is pinned",
                    package.id.0
                ));
            }
            if grants.contains(&HostService::ModelStream) {
                return Err(format!(
                    "Harness-managed route {harness_id} must not request host.model.stream"
                ));
            }
            return Ok((
                ProviderSnapshotRef {
                    kind: ProviderRouteKind::HarnessManaged,
                    settings_revision: 0,
                    provider_id: harness_id.clone(),
                    model_id: model_id.clone().unwrap_or_default(),
                    base_url: None,
                    protocol: Some("harness-managed".to_string()),
                    capabilities: Vec::new(),
                },
                Arc::new(HarnessManagedModelService),
            ));
        }

        let (selection, model_override) = match &state.preferences.model_route {
            Some(ModelRoute::HostProvider {
                provider_id,
                model_id,
            }) => (Some(provider_id.as_str()), model_id.as_deref()),
            Some(ModelRoute::HarnessManaged { .. }) => unreachable!("handled above"),
            None => (state.preferences.model.as_deref(), None),
        };
        match self
            .settings
            .resolve_provider_for_run(selection, model_override)
            .map_err(|error| format!("无法冻结模型路由：{error}"))?
        {
            RunProviderResolution::Resolved(resolved) => {
                let (provider, implementation) = resolved.into_parts();
                let models = Arc::new(ModelBroker::for_frozen_route(
                    provider.provider_id.clone(),
                    implementation,
                    provider.model_id.clone(),
                ));
                Ok((provider, models))
            }
            RunProviderResolution::Unconfigured { settings_revision }
                if self.allow_injected_model_fallback =>
            {
                Ok((
                    ProviderSnapshotRef {
                        kind: ProviderRouteKind::HostProvider,
                        settings_revision,
                        provider_id: "injected.test".to_string(),
                        model_id: "injected-test-model".to_string(),
                        base_url: None,
                        protocol: Some("injected".to_string()),
                        capabilities: Vec::new(),
                    },
                    self.injected_models.clone(),
                ))
            }
            RunProviderResolution::Unconfigured { .. } => {
                Err("没有已配置的默认 Provider；运行尚未启动".to_string())
            }
        }
    }

    async fn tool_catalog_digest(&self, guard: &RunGuard) -> Result<String, String> {
        let mut catalog = self
            .tools
            .list(guard.token())
            .await
            .map_err(|error| format!("无法冻结工具目录：{error}"))?;
        catalog.sort_by_key(|tool| serde_json::to_string(tool).unwrap_or_default());
        let catalog = serde_json::to_value(catalog)
            .map_err(|error| format!("无法序列化工具目录：{error}"))?;
        Ok(r_code_harness_protocol::canonical_input_hash(&catalog))
    }
}

pub fn harness_config(snapshot: &RunSnapshot) -> serde_json::Value {
    let material = snapshot.material();
    let mut config = serde_json::json!({
        "modelSelection": material.provider.provider_id,
        "defaultModelSelection": material.provider.provider_id,
        "model": material.provider.model_id,
        "inference": material.inference,
        "systemPrompt": material.prompt.resolved_system_prompt,
    });
    // FR-1: the frozen instruction block rides beside the system prompt;
    // the native plugin appends it inside effective_system_prompt so the
    // user prompt config itself stays pure (5.2 mount table).
    if !material.instructions.is_empty() {
        config["instructions"] = serde_json::json!(material.instructions.rendered);
        config["instructionsDigest"] = serde_json::json!(material.instructions.digest);
    }
    config
}

/// Native planning posture is host-selected rather than a user prompt hint.
/// `planRevision` is the only revision the Native harness may publish for
/// this run; Conversation runs use `ask` and receive no publish revision.
pub fn planning_harness_config(
    snapshot: &RunSnapshot,
    task_mode: &str,
    plan_revision: Option<u64>,
) -> serde_json::Value {
    let mut config = harness_config(snapshot);
    config["taskMode"] = serde_json::json!(task_mode);
    if let Some(revision) = plan_revision {
        config["planRevision"] = serde_json::json!(revision);
    }
    config
}

fn prompt_snapshot(state: &TaskState) -> PromptSnapshotRef {
    let mut snapshot = match state.preferences.system_prompt.as_ref() {
        Some(content) => PromptSnapshotRef {
            revision: format!("task-preferences:{}", state.contract.revision),
            mode: PromptSnapshotMode::Replace,
            content_sha256: sha256_hex(content.as_bytes()),
            resolved_system_prompt: content.clone(),
        },
        None => {
            let content = agent_config::DEFAULT_MAIN_AGENT_PROMPT.to_string();
            PromptSnapshotRef {
                revision: "builtin-default-v1".to_string(),
                mode: PromptSnapshotMode::Default,
                content_sha256: sha256_hex(content.as_bytes()),
                resolved_system_prompt: content,
            }
        }
    };
    // FR-7.2: the desktop-frozen memory segment joins the prompt snapshot
    // here — merged once per freeze and inherited by every attempt of the
    // task; the revision marker and content hash cover the combined text.
    if let Some(memory) = &state.contract.memory {
        snapshot.resolved_system_prompt =
            format!("{}\n\n{}", snapshot.resolved_system_prompt, memory.rendered);
        let hash_len = memory.snapshot_hash.len().min(8);
        snapshot.revision = format!(
            "{}+mem:{}",
            snapshot.revision,
            &memory.snapshot_hash[..hash_len]
        );
        snapshot.content_sha256 = sha256_hex(snapshot.resolved_system_prompt.as_bytes());
    }
    snapshot
}

/// Record the FR-7/FR-1 injection ledger rows for a frozen run: the
/// memory segment (when the contract carries one) and the frozen
/// instruction set (when non-empty). The run identity is whatever is in
/// scope at freeze time (conversation run id or the deterministic WorkUnit
/// attempt id). Fail-open: accounting must never break run dispatch (house
/// precedent: effect-approval revoke path).
pub fn record_run_injections(
    store: &r_code_store::v1::V1Store,
    run_id: &str,
    state: &TaskState,
    instructions: &r_code_kernel::task::InstructionSetRef,
) {
    let mut records = Vec::new();
    if let Some(memory) = &state.contract.memory {
        records.push(r_code_store::v1::InjectionRecord {
            run_id: run_id.to_string(),
            kind: r_code_store::v1::InjectionKind::Memory,
            snapshot_hash: memory.snapshot_hash.clone(),
            refs: memory.entry_ids.clone(),
            chars: memory.rendered.chars().count() as u64,
        });
    }
    if !instructions.is_empty() {
        records.push(r_code_store::v1::InjectionRecord {
            run_id: run_id.to_string(),
            kind: r_code_store::v1::InjectionKind::Instruction,
            snapshot_hash: instructions.digest.clone(),
            refs: instructions
                .entries
                .iter()
                .map(|entry| format!("{}|{}|{}", entry.layer, entry.status, entry.path))
                .collect(),
            chars: instructions.rendered.chars().count() as u64,
        });
    }
    for record in records {
        if let Err(error) = store.record_injection(&record) {
            eprintln!("injection ledger write failed for {run_id}: {error}");
        }
    }
}

pub fn resolve_workspace_snapshot(state: &TaskState) -> Result<WorkspaceSnapshotRef, String> {
    let requires_workspace = matches!(
        state.contract.kind,
        TaskKind::Implementation | TaskKind::Repair
    );
    let configured_path = state
        .preferences
        .workspace_path
        .as_deref()
        .filter(|path| !path.trim().is_empty());
    if let Some(path) = configured_path {
        let frozen = TaskWorkspaceBinding::bind_local(
            &state.contract.task_id,
            std::path::Path::new(path),
            &[],
        )
        .and_then(|binding| binding.snapshot_ref());
        match frozen {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) if requires_workspace => {
                return Err(format!("无法冻结任务工作区 {path:?}：{error}"));
            }
            Err(_) => {}
        }
    } else if requires_workspace {
        return Err("Implementation/Repair 任务必须绑定当前 checkout，运行尚未启动".to_string());
    }
    Ok(TaskWorkspaceBinding::unbound_read_only_snapshot())
}

fn permission_snapshot(grants: &[HostService]) -> PermissionSnapshotRef {
    let mut capabilities = grants
        .iter()
        .map(|service| service.wire_name().to_string())
        .collect::<Vec<_>>();
    capabilities.sort();
    capabilities.dedup();
    let revision = r_code_harness_protocol::canonical_input_hash(
        &serde_json::json!({"capabilities": capabilities}),
    );
    PermissionSnapshotRef {
        revision: format!("sha256:{revision}"),
        profile_id: "harness-manifest-v1".to_string(),
        capabilities,
    }
}
