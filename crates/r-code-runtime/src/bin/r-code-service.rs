//! r-code-service: the single-owner background daemon for one v1 profile.
//!
//! Boot order: explicit `--profile` (never inferred) → RuntimeProfile →
//! profile ownership lock → open the v1 store → compose the
//! ApplicationService (real surface: plugin catalog, task lifecycle, the
//! durable event journal) → bind the authenticated local endpoint → serve.
//! The daemon does not exit when frontends disconnect; it stops only via an
//! explicit `service.shutdown` command or process termination.

use r_code_harness_protocol::application::methods;
use r_code_runtime::application::{
    ApplicationError, ApplicationService, CompositionPolicy, CreateTaskInput, PatchField,
    ReviewActionContext, TaskPreferencesPatch, UnverifiedOverrideInput,
};
use r_code_runtime::application_receipts::CommandDedup;
use r_code_runtime::daemon::{ApplicationHandler, Daemon, ProfileLock};
use r_code_runtime::services::authorization::{
    AuthorizationService, EffectivePermissions, WorkspaceCapability,
};
use r_code_runtime::services::models::ModelBroker;
use r_code_runtime::services::settings_store::{
    ProviderEntry, SettingsBackedResolver, SettingsStore, SettingsStoreError, V1Settings,
};
use r_code_runtime::services::tools::GatewayToolService;
use r_code_runtime::{LaunchOptions, RuntimeProfile};
use r_code_store::v1::V1Store;
use std::sync::Arc;
use tokio::sync::Notify;

/// Adapter: ApplicationService over the daemon's RPC surface. Thin — every
/// method maps 1:1 onto the composed service; long work returns durable
/// operation ids via the CommandDedup wrapper.
struct ServiceHandler {
    service: Arc<ApplicationService>,
    shutdown: Arc<Notify>,
    /// Profile v1 root: hosts the persistent side-effect counter used by
    /// dedup contract tests.
    harness_root: std::path::PathBuf,
    /// The remote-control surface (R08 wiring): device registry, pairing
    /// sessions, the TLS identity and the pairing-gated listener.
    remote: RemoteSurface,
}

/// Everything the remote-control surface needs, owned by the daemon.
/// Management logic lives in [`r_code_runtime::remote::RemoteManager`] so
/// the console methods here are one-liners (R11 shares it with tests).
struct RemoteSurface {
    manager: Arc<r_code_runtime::remote::RemoteManager>,
}

impl std::ops::Deref for RemoteSurface {
    type Target = r_code_runtime::remote::RemoteManager;
    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

impl RemoteSurface {
    /// `remote.pairingStart` payload shape for the console.
    async fn pairing_start(&self) -> Result<serde_json::Value, String> {
        let reply = self
            .manager
            .pairing_start()
            .await
            .map_err(|e| e.to_string())?;
        let port = self.manager.listening_port().await.unwrap_or_default();
        Ok(serde_json::json!({
            "pairingCode": reply.pairing_code,
            "qrPayload": r_code_runtime::remote::pairing::qr_payload_v1(
                &self.manager.bind_ip.to_string(),
                port,
                &reply.pairing_code,
                &self.identity_fingerprint(),
            ),
            "lanEndpoints": reply.lan_endpoints,
            "expiresAtMs": reply.expires_at_ms,
            "port": port,
            "fingerprint": self.identity_fingerprint(),
        }))
    }

    fn identity_fingerprint(&self) -> String {
        self.manager.identity.fingerprint.clone()
    }
}

fn method_error(error: ApplicationError) -> String {
    error.to_string()
}

/// Stable daemon-boundary settings failures. These strings deliberately omit
/// filesystem/credential backend details so API keys can never be reflected
/// through an RPC error.
fn settings_error(error: SettingsStoreError) -> String {
    match error {
        SettingsStoreError::Corrupt { .. } => "settings_corrupt".to_string(),
        SettingsStoreError::StaleRevision { expected, actual } => {
            format!("settings_stale_revision:expected={expected}:actual={actual}")
        }
        SettingsStoreError::MissingCredential { selection } => {
            format!("settings_missing_credential:provider={selection}")
        }
        SettingsStoreError::UnknownProvider { selection } => {
            format!("settings_unknown_provider:provider={selection}")
        }
        SettingsStoreError::ProviderNotConfigured { selection } => {
            format!("settings_provider_not_configured:provider={selection}")
        }
        SettingsStoreError::InvalidProtocol {
            selection,
            protocol,
        } => format!("settings_invalid_protocol:provider={selection}:protocol={protocol}"),
        SettingsStoreError::RevisionOverflow(_) => "settings_revision_overflow".to_string(),
        SettingsStoreError::Credential {
            operation,
            selection,
        } => format!("settings_credential_error:operation={operation}:provider={selection}"),
        SettingsStoreError::ProviderUnavailable { selection } => {
            format!("settings_provider_unavailable:provider={selection}")
        }
        SettingsStoreError::Read { .. }
        | SettingsStoreError::Serialize(_)
        | SettingsStoreError::Persist { .. }
        | SettingsStoreError::LockPoisoned => "settings_io_error".to_string(),
    }
}

fn expected_settings_revision(params: &serde_json::Value) -> Result<u64, String> {
    let Some(value) = params.get("expectedRevision") else {
        return Err("settings_revision_required".to_string());
    };
    value
        .as_u64()
        .ok_or_else(|| "settings_revision_invalid".to_string())
}

fn settings_mutation_response(
    service: &ApplicationService,
    settings: V1Settings,
) -> Result<serde_json::Value, String> {
    let providers = service
        .settings()
        .availability_checked()
        .map_err(settings_error)?;
    Ok(serde_json::json!({
        "revision": settings.revision,
        "settings": settings,
        "providers": providers,
    }))
}

fn quarantine_workspace_filter(params: &serde_json::Value) -> Result<Option<&str>, String> {
    let object = params
        .as_object()
        .ok_or_else(|| "safety.quarantine.get params must be an object".to_string())?;
    if object.keys().any(|key| key != "workspaceKey") {
        return Err("unknown safety.quarantine.get parameter".to_string());
    }
    match object.get("workspaceKey") {
        None => Ok(None),
        Some(value) => {
            let workspace_key = value
                .as_str()
                .ok_or_else(|| "workspaceKey must be a string".to_string())?;
            if workspace_key.is_empty() {
                return Err("workspaceKey must not be empty".to_string());
            }
            Ok(Some(workspace_key))
        }
    }
}

/// P12: the safety report diagnostic accepts only an optional capability
/// filter. Strictly read-only — no parameter can mutate or prune reports.
fn safety_report_capability_filter(params: &serde_json::Value) -> Result<Option<&str>, String> {
    let object = params
        .as_object()
        .ok_or_else(|| "safety.report.get params must be an object".to_string())?;
    if object.keys().any(|key| key != "capability") {
        return Err("unknown safety.report.get parameter".to_string());
    }
    match object.get("capability") {
        None => Ok(None),
        Some(value) => {
            let capability = value
                .as_str()
                .ok_or_else(|| "capability must be a string".to_string())?;
            if capability.is_empty() {
                return Err("capability must not be empty".to_string());
            }
            Ok(Some(capability))
        }
    }
}

fn optional_string(value: &serde_json::Value, field: &str) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(|text| Some(text.to_string()))
        .ok_or_else(|| format!("{field} must be a string or null"))
}

fn string_patch(params: &serde_json::Value, field: &str) -> Result<PatchField<String>, String> {
    match params.get(field) {
        None => Ok(PatchField::Unchanged),
        Some(value) if value.is_null() => Ok(PatchField::Clear),
        Some(value) => value
            .as_str()
            .map(|value| PatchField::Set(value.to_string()))
            .ok_or_else(|| format!("{field} must be a string or null")),
    }
}

fn parse_task_preferences_patch(
    params: &serde_json::Value,
) -> Result<TaskPreferencesPatch, String> {
    let model_route = match params.get("modelRoute") {
        None => PatchField::Unchanged,
        Some(value) if value.is_null() => PatchField::Clear,
        Some(value) => PatchField::Set(
            serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid modelRoute: {error}"))?,
        ),
    };
    let inference = match params.get("inference") {
        None => PatchField::Unchanged,
        Some(value) if value.is_null() => PatchField::Clear,
        Some(value) => PatchField::Set(value.clone()),
    };
    let system_prompt = string_patch(params, "systemPrompt")?;
    if let PatchField::Set(prompt) = &system_prompt {
        if prompt.contains('\0') || prompt.chars().count() > 20_000 {
            return Err("systemPrompt must contain no NUL and be at most 20000 characters".into());
        }
    }
    let workspace_path = string_patch(params, "workspacePath")?;
    if let PatchField::Set(path) = &workspace_path {
        if path.trim().is_empty() || path.contains('\0') || path.chars().count() > 4_096 {
            return Err(
                "workspacePath must be non-empty, contain no NUL, and be at most 4096 characters"
                    .into(),
            );
        }
    }
    let require_desktop_confirm = params
        .get("requireDesktopConfirm")
        .map(|value| {
            value
                .as_bool()
                .ok_or("requireDesktopConfirm must be a boolean")
        })
        .transpose()?;
    Ok(TaskPreferencesPatch {
        model_route,
        legacy_model: string_patch(params, "model")?,
        inference,
        mode: string_patch(params, "mode")?,
        system_prompt,
        workspace_path,
        require_desktop_confirm,
        harness_id: string_patch(params, "harnessId")?,
    })
}

/// FR-7: desktop-frozen memory handoff params — `memory: {rendered,
/// entryIds, snapshotHash}`; absent/null means no memory (ownerless paths
/// like the TUI). Validation caps live on the type.
fn parse_memory_handoff(
    params: &serde_json::Value,
) -> Result<Option<r_code_kernel::task::FrozenMemoryHandoff>, String> {
    let Some(value) = params.get("memory") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let field = |camel: &str, snake: &str| {
        value
            .get(camel)
            .or_else(|| value.get(snake))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let rendered = field("rendered", "rendered").ok_or("memory.rendered must be a string")?;
    let snapshot_hash =
        field("snapshotHash", "snapshot_hash").ok_or("memory.snapshotHash must be a string")?;
    let entry_ids = value
        .get("entryIds")
        .or_else(|| value.get("entry_ids"))
        .and_then(|v| v.as_array())
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let handoff = r_code_kernel::task::FrozenMemoryHandoff {
        rendered,
        entry_ids,
        snapshot_hash,
    };
    handoff
        .validate()
        .map_err(|error| format!("invalid memory handoff: {error}"))?;
    Ok(Some(handoff))
}

/// FR-1 (M1a-06): context.settings params — partial update semantics with
/// the stored record (or defaults) as the base, validated before persist.
fn parse_context_settings(
    params: &serde_json::Value,
    base: r_code_store::v1::ContextSettingsRecord,
) -> Result<r_code_store::v1::ContextSettingsRecord, String> {
    let mut record = base;
    if let Some(value) = params.get("injectionEnabled") {
        record.injection_enabled = value
            .as_bool()
            .ok_or("injectionEnabled must be a boolean")?;
    }
    if let Some(value) = params.get("totalBudgetBytes") {
        record.total_budget_bytes = value
            .as_u64()
            .ok_or("totalBudgetBytes must be an integer")?;
    }
    if let Some(value) = params.get("jitAllowanceBytes") {
        record.jit_allowance_bytes = value
            .as_u64()
            .ok_or("jitAllowanceBytes must be an integer")?;
    }
    if let Some(value) = params.get("fallbackNames") {
        let names = value
            .as_array()
            .ok_or("fallbackNames must be an array of strings")?;
        record.fallback_names = names
            .iter()
            .map(|name| {
                name.as_str()
                    .filter(|n| !n.trim().is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| "fallbackNames entries must be non-empty strings".to_string())
            })
            .collect::<Result<Vec<String>, String>>()?;
    }
    Ok(record)
}

/// Canonicalize a workspacePath param into the settings key basis.
fn canonical_workspace_param(params: &serde_json::Value) -> Result<String, String> {
    let raw = params["workspacePath"]
        .as_str()
        .ok_or("missing workspacePath")?;
    if raw.trim().is_empty() || raw.contains(' ') {
        return Err("workspacePath must be non-empty".into());
    }
    let canonical = std::fs::canonicalize(raw)
        .map_err(|error| format!("workspacePath cannot be resolved: {error}"))?;
    if !canonical.is_dir() {
        return Err("workspacePath must be an existing directory".into());
    }
    Ok(canonical.to_string_lossy().to_string())
}

fn task_kind_from_params(
    params: &serde_json::Value,
) -> Result<r_code_kernel::task::TaskKind, String> {
    if let Some(mode) = params.get("mode").and_then(|value| value.as_str()) {
        return match mode {
            "ask" => Ok(r_code_kernel::task::TaskKind::Conversation),
            "edit" | "auto" => Ok(r_code_kernel::task::TaskKind::Implementation),
            "plan" => Ok(r_code_kernel::task::TaskKind::PlanDraft),
            other => Err(format!("invalid task mode {other:?}")),
        };
    }
    Ok(match params["kind"].as_str().unwrap_or("conversation") {
        "implementation" => r_code_kernel::task::TaskKind::Implementation,
        "plan-draft" => r_code_kernel::task::TaskKind::PlanDraft,
        "repair" => r_code_kernel::task::TaskKind::Repair,
        _ => r_code_kernel::task::TaskKind::Conversation,
    })
}

fn review_context(
    params: &serde_json::Value,
    actor_id: &str,
) -> Result<ReviewActionContext, String> {
    Ok(ReviewActionContext {
        action_id: params["actionId"]
            .as_str()
            .ok_or("missing actionId")?
            .to_string(),
        expected_task_revision: params["expectedTaskRevision"]
            .as_u64()
            .ok_or("missing expectedTaskRevision")?,
        candidate_digest: params["candidateDigest"]
            .as_str()
            .ok_or("missing candidateDigest")?
            .to_string(),
        actor_id: actor_id.to_string(),
        session_id: params["sessionId"]
            .as_str()
            .ok_or("missing sessionId")?
            .to_string(),
    })
}

#[async_trait::async_trait]
impl ApplicationHandler for ServiceHandler {
    async fn execute(
        &self,
        command: r_code_harness_protocol::application::ApplicationCommand,
    ) -> Result<serde_json::Value, String> {
        let params = command.params;
        match command.method.as_str() {
            methods::PING => Ok(serde_json::json!({"pong": true})),
            // Diagnostics used by client/daemon contract tests: echo
            // replays the payload (receipt dedup proves exactly-once) and
            // slow-echo simulates long work surviving frontend exit.
            "echo" => Ok(params),
            // Persistent side-effect counter for dedup contract tests:
            // bumping increments a file-backed counter, proving effects
            // execute exactly once across reconnects and restarts.
            "counter.bump" => {
                let path = self.harness_root.join("counter.dat");
                let current: u64 = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok().map(u64::from_le_bytes))
                    .unwrap_or(0);
                let next = current + 1;
                std::fs::write(&path, next.to_le_bytes())
                    .map_err(|error| format!("counter write: {error}"))?;
                Ok(serde_json::json!({"count": next}))
            }
            "counter.get" => {
                let path = self.harness_root.join("counter.dat");
                let current: u64 = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok().map(u64::from_le_bytes))
                    .unwrap_or(0);
                Ok(serde_json::json!({"count": current}))
            }
            "slow-echo" => {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                Ok(params)
            }
            "plugins.list" => {
                let entries = self.service.list_plugins().map_err(method_error)?;
                Ok(serde_json::to_value(entries).unwrap_or_default())
            }
            "plugins.install" => {
                let path = params["path"].as_str().ok_or("missing path")?;
                let installed = self
                    .service
                    .install_package_from_directory(std::path::Path::new(path))
                    .map_err(method_error)?;
                Ok(serde_json::json!({
                    "id": installed.manifest.id.0,
                    "version": installed.manifest.version.to_string(),
                    "contentDigest": installed.package_ref.content_digest,
                }))
            }
            "plugins.setEnabled" => {
                let id = params["id"].as_str().ok_or("missing id")?;
                let digest = params["digest"].as_str().ok_or("missing digest")?;
                let enabled = params["enabled"].as_bool().unwrap_or(false);
                self.service
                    .set_plugin_enabled(id, digest, enabled)
                    .map_err(method_error)?;
                Ok(serde_json::Value::Null)
            }
            "plugins.remove" => {
                let id = params["id"].as_str().ok_or("missing id")?;
                let digest = params["digest"].as_str().ok_or("missing digest")?;
                self.service
                    .remove_package(id, digest)
                    .map_err(method_error)?;
                Ok(serde_json::Value::Null)
            }
            "task.create" => {
                let has_explicit_route =
                    params.get("modelRoute").is_some() || params.get("harnessId").is_some();
                // Omitted task ids get a host-generated one (TUI /new).
                let task_id = params["taskId"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("task-{}", uuid::Uuid::new_v4().simple()));
                let title = params
                    .get("title")
                    .map(|value| optional_string(value, "title"))
                    .transpose()?
                    .flatten();
                let objective = params["objective"]
                    .as_str()
                    .or(title.as_deref())
                    .ok_or("missing objective")?
                    .to_string();
                let kind = task_kind_from_params(&params)?;
                let required_checks: Vec<String> = params["requiredChecks"]
                    .as_array()
                    .map(|checks| {
                        checks
                            .iter()
                            .filter_map(|check| check.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let patch = parse_task_preferences_patch(&params)?;
                let mut preferences = r_code_kernel::task::TaskPreferences::default();
                patch.apply_to(&mut preferences);
                let harness_id = match &patch.harness_id {
                    PatchField::Set(harness_id) => Some(harness_id.clone()),
                    PatchField::Unchanged | PatchField::Clear => None,
                };
                let input = CreateTaskInput {
                    task_id,
                    objective,
                    title,
                    kind,
                    required_checks,
                    memory: parse_memory_handoff(&params)?,
                    preferences,
                    harness_id,
                };
                let state = if has_explicit_route {
                    self.service.create_task_configured(input).await
                } else {
                    self.service.create_task_legacy_default(input).await
                }
                .map_err(method_error)?;
                Ok(
                    serde_json::json!({"taskId": state.contract.task_id, "revision": state.contract.revision}),
                )
            }
            "task.selectHarness" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let harness_id = params["harnessId"].as_str().ok_or("missing harnessId")?;
                let package = self
                    .service
                    .select_harness(task_id, harness_id)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::json!({
                    "id": package.id.0,
                    "version": package.version.to_string(),
                    "contentDigest": package.content_digest,
                }))
            }
            "task.sendMessage" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let text = params["text"].as_str().ok_or("missing text")?;
                // Audit actor: the connection identity (device id on remote
                // transports — the listener overwrites client_id before the
                // handler sees it).
                self.service
                    .send_message_as(task_id, text, Some(&command.client_id))
                    .await
                    .map_err(method_error)
            }
            "plan.get" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let plan = self.service.plan(task_id).await.map_err(method_error)?;
                Ok(serde_json::to_value(plan).unwrap_or_default())
            }
            "plan.approve" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let revision_hash = params["revisionHash"]
                    .as_str()
                    .or_else(|| params["revision_hash"].as_str())
                    .ok_or("missing revisionHash")?;
                let plan = self
                    .service
                    .approve_plan(
                        task_id,
                        revision_hash,
                        &command.command_id,
                        &command.client_id,
                        &command.command_id,
                    )
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::to_value(plan).unwrap_or_default())
            }
            "plan.revise" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let reason = params["reason"]
                    .as_str()
                    .unwrap_or("user-requested-revision");
                self.service
                    .revise_plan(task_id, reason)
                    .await
                    .map_err(method_error)
            }
            "review.get" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let review = self.service.review(task_id).await.map_err(method_error)?;
                Ok(serde_json::to_value(review).unwrap_or_default())
            }
            "review.accept" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let context = review_context(&params, &command.client_id)?;
                let result = self
                    .service
                    .accept_review(task_id, context)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::to_value(result).unwrap_or_default())
            }
            "review.reject" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let reason = params["reason"].as_str().ok_or("missing reason")?;
                let context = review_context(&params, &command.client_id)?;
                let result = self
                    .service
                    .reject_review(task_id, context, reason)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::to_value(result).unwrap_or_default())
            }
            "review.acceptUnverified" | "review.override" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let reason = params["reason"]
                    .as_str()
                    .ok_or("missing reason")?
                    .to_string();
                let checks = params["checks"]
                    .as_array()
                    .ok_or("missing checks")?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_string)
                            .ok_or("checks must contain strings")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let context = review_context(&params, &command.client_id)?;
                let result = self
                    .service
                    .accept_unverified(
                        task_id,
                        UnverifiedOverrideInput {
                            context,
                            reason,
                            checks,
                        },
                    )
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::to_value(result).unwrap_or_default())
            }
            "review.overrides.list" => {
                // E09-R: the immutable table is the single query source;
                // the journal rows are its derived projection.
                let task_id = params["taskId"].as_str();
                let overrides = self
                    .service
                    .list_unverified_overrides(task_id)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::to_value(overrides).unwrap_or_default())
            }
            "task.cancel" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let cancelled = self
                    .service
                    .cancel_task(task_id)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::json!({"cancelled": cancelled}))
            }
            "task.list" => {
                let tasks = self.service.list_tasks().await;
                Ok(serde_json::to_value(tasks).unwrap_or_default())
            }
            "task.detail" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let detail = self
                    .service
                    .task_detail(task_id)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::to_value(detail).unwrap_or_default())
            }
            "git.read" => {
                // P30: the only git RPC family — status/log over the
                // restricted reader; diff is the pure git_diff tool.
                let Some(params) = params.as_object() else {
                    return Err("git.read params must be an object".into());
                };
                let projection = params
                    .get("projection")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("status");
                let git_dir = params
                    .get("gitDir")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("git.read requires a canonical gitDir")?;
                let limit = params
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(100) as usize;
                let value = self
                    .service
                    .git_read_projection(projection, std::path::Path::new(git_dir), limit)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(value).map_err(|error| error.to_string())
            }
            "safety.quarantine.get" => {
                let workspace_key = quarantine_workspace_filter(&params)?;
                let diagnostics = self
                    .service
                    .quarantine_diagnostics(workspace_key)
                    .map_err(method_error)?;
                serde_json::to_value(diagnostics).map_err(|error| error.to_string())
            }
            "safety.report.get" => {
                let capability = safety_report_capability_filter(&params)?;
                let diagnostics = self
                    .service
                    .safety_report_diagnostics(capability)
                    .map_err(method_error)?;
                serde_json::to_value(diagnostics).map_err(|error| error.to_string())
            }
            "safety.quarantine.retry" => {
                // P11R: an authenticated local retry with the exact
                // platform proof rules. Remote transports cannot reach
                // this method (the remote gate refuses unknown methods).
                let object = params.as_object().ok_or_else(|| {
                    "safety.quarantine.retry params must be an object".to_string()
                })?;
                if !object
                    .keys()
                    .all(|key| matches!(key.as_str(), "treeId" | "actor" | "session"))
                    || object.is_empty()
                {
                    return Err(
                        "safety.quarantine.retry accepts only treeId, actor and session".into(),
                    );
                }
                let tree_id = object
                    .get("treeId")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or("treeId must be a non-empty string")?;
                let actor = object
                    .get("actor")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or("actor must be a non-empty string")?;
                let session = object
                    .get("session")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or("session must be a non-empty string")?;
                let view = self
                    .service
                    .retry_quarantine(tree_id, actor, session)
                    .map_err(method_error)?;
                serde_json::to_value(view).map_err(|error| error.to_string())
            }
            "task.rename" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let title = params["title"].as_str().ok_or("missing title")?;
                self.service
                    .rename_task(task_id, title)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::Value::Null)
            }
            "task.setPreferences" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let patch = parse_task_preferences_patch(&params)?;
                self.service
                    .set_task_preferences_patch(task_id, patch)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::Value::Null)
            }
            "task.clone" => {
                let source = params["sourceTaskId"]
                    .as_str()
                    .ok_or("missing sourceTaskId")?;
                let new_id = params["newTaskId"].as_str().ok_or("missing newTaskId")?;
                let title = params["title"].as_str();
                let branch = self
                    .service
                    .clone_task(source, new_id, title)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::json!({
                    "taskId": branch.contract.task_id,
                    "title": branch.title,
                }))
            }
            "task.branches" => {
                let branches = self.service.task_branches();
                Ok(serde_json::to_value(branches).unwrap_or_default())
            }
            "models.available" => {
                let availability = self
                    .service
                    .settings()
                    .availability_checked()
                    .map_err(settings_error)?;
                Ok(serde_json::to_value(availability).unwrap_or_default())
            }
            "settings.get" => {
                let settings = self
                    .service
                    .settings()
                    .load_checked()
                    .map_err(settings_error)?;
                Ok(serde_json::to_value(settings).unwrap_or_default())
            }
            "context.current" => {
                let task_id = params["taskId"]
                    .as_str()
                    .ok_or("missing taskId")?
                    .to_string();
                let view = self
                    .service
                    .context_current(&task_id)
                    .await
                    .map_err(method_error)?;
                Ok(serde_json::to_value(view).unwrap_or_default())
            }
            "context.settings.update" => {
                let canonical = canonical_workspace_param(&params)?;
                let base = self
                    .service
                    .context_settings(&canonical)
                    .unwrap_or_else(r_code_store::v1::ContextSettingsRecord::defaults);
                let record = parse_context_settings(&params, base)?;
                self.service
                    .update_context_settings(&canonical, record)
                    .map_err(method_error)?;
                Ok(serde_json::json!({"ok": true}))
            }
            "context.settings.get" => {
                let canonical = canonical_workspace_param(&params)?;
                let stored = self.service.context_settings(&canonical);
                let source = if stored.is_some() {
                    "stored"
                } else {
                    "default"
                };
                let record =
                    stored.unwrap_or_else(r_code_store::v1::ContextSettingsRecord::defaults);
                Ok(serde_json::json!({
                    "source": source,
                    "injectionEnabled": record.injection_enabled,
                    "totalBudgetBytes": record.total_budget_bytes,
                    "jitAllowanceBytes": record.jit_allowance_bytes,
                    "fallbackNames": record.fallback_names,
                }))
            }
            "settings.apply" => {
                let expected_revision = expected_settings_revision(&params)?;
                let selection = params["selection"].as_str().ok_or("missing selection")?;
                let entry = ProviderEntry {
                    selection: selection.to_string(),
                    model: params["model"].as_str().ok_or("missing model")?.to_string(),
                    base_url: params["baseUrl"].as_str().map(str::to_string),
                    protocol: params["protocol"].as_str().map(str::to_string),
                    env_var: params["envVar"].as_str().map(str::to_string),
                };
                let api_key = params["apiKey"].as_str();
                let settings = self
                    .service
                    .settings()
                    .apply_provider_at_revision(expected_revision, entry, api_key)
                    .map_err(settings_error)?;
                settings_mutation_response(&self.service, settings)
            }
            "settings.setDefault" => {
                let expected_revision = expected_settings_revision(&params)?;
                let selection = params["selection"].as_str().ok_or("missing selection")?;
                let settings = self
                    .service
                    .settings()
                    .set_default_at_revision(expected_revision, selection)
                    .map_err(settings_error)?;
                settings_mutation_response(&self.service, settings)
            }
            "settings.removeProvider" => {
                let expected_revision = expected_settings_revision(&params)?;
                let selection = params["selection"].as_str().ok_or("missing selection")?;
                let settings = self
                    .service
                    .settings()
                    .remove_provider_at_revision(expected_revision, selection)
                    .map_err(settings_error)?;
                settings_mutation_response(&self.service, settings)
            }
            "codex.status" => r_code_runtime::services::codex_cli::codex_integration_status().await,
            "codex.startLogin" => {
                let mode = params["mode"].as_str().unwrap_or("browser");
                if mode == "device" {
                    r_code_runtime::services::codex_cli::codex_start_device_login()
                        .await
                        .map(|_| serde_json::json!({"started": true}))
                } else {
                    r_code_runtime::services::codex_cli::codex_start_login()
                        .await
                        .map(|_| serde_json::json!({"started": true}))
                }
            }
            "task.events" => {
                let after_seq = params["afterSeq"].as_u64().unwrap_or(0);
                let limit = params["limit"].as_u64().unwrap_or(200).min(u32::MAX as u64) as u32;
                let events = self.service.events_after(after_seq, limit).await;
                Ok(serde_json::to_value(events).unwrap_or_default())
            }
            "approvals.list" => {
                let pending = self.service.approvals_list().await;
                Ok(serde_json::json!({"pending": pending}))
            }
            // P19B-R scoped effect approvals: host-owned request/list/revoke
            // over the exact approved-plan material. Decisions reuse the
            // ordinary authenticated approvals.decide path below. These
            // methods are local-only for now (the remote capability gate
            // refuses unknown methods); plugins never see them — the plugin
            // router exposes no effect surface at all.
            "approvals.effect.request" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let work_unit_id = params["workUnitId"].as_str().ok_or("missing workUnitId")?;
                // Default operation id = command id (the plan.approve
                // precedent): unique per request, stable across deduped
                // retries. An explicit operationId lets a client pin its
                // own idempotency key.
                let operation_id = params["operationId"]
                    .as_str()
                    .unwrap_or(&command.command_id);
                let run_id = params["runId"].as_str().unwrap_or("");
                self.service
                    .effect_approval_request(task_id, work_unit_id, operation_id, run_id)
                    .await
                    .map_err(method_error)
            }
            "approvals.effect.list" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                self.service
                    .effect_approval_list(task_id)
                    .await
                    .map_err(method_error)
            }
            "approvals.effect.revoke" => {
                let task_id = params["taskId"].as_str().ok_or("missing taskId")?;
                let work_unit_id = params["workUnitId"].as_str().ok_or("missing workUnitId")?;
                // Attribution mirrors plan.approve: the authenticated
                // connection is the actor, the command id the session.
                self.service
                    .effect_approval_revoke(
                        task_id,
                        work_unit_id,
                        &command.client_id,
                        &command.command_id,
                    )
                    .await
                    .map_err(method_error)
            }
            "device.list" => Ok(serde_json::json!({
                "devices": self.remote.manager.list_devices()
            })),
            "device.revoke" => {
                let device_id = params["deviceId"].as_str().ok_or("missing deviceId")?;
                self.remote
                    .manager
                    .revoke(device_id)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::Value::Null)
            }
            "device.updateCapabilities" => {
                let device_id = params["deviceId"].as_str().ok_or("missing deviceId")?;
                let labels: Vec<&str> = params["capabilities"]
                    .as_array()
                    .map(|values| values.iter().filter_map(|value| value.as_str()).collect())
                    .unwrap_or_default();
                let applied = self
                    .remote
                    .manager
                    .update_capabilities(device_id, &labels)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({"capabilities": applied}))
            }
            "device.setListener" => {
                let enabled = params["enabled"].as_bool().unwrap_or(false);
                self.remote
                    .manager
                    .set_listener(enabled)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({"enabled": enabled}))
            }
            "remote.pairingStart" => {
                // F4/F5: the pairing console is local-only; the remote gate
                // refuses this method outright (FORBIDDEN_REMOTE_METHODS).
                self.remote.pairing_start().await
            }
            "remote.listenerStatus" => {
                let port = self.remote.manager.listening_port().await;
                Ok(serde_json::json!({
                    "listening": port.is_some(),
                    "port": port,
                    "devices": self.remote.registry.list().len(),
                }))
            }
            "approvals.decide" => {
                // Local console decision (the remote transport arrives as
                // "approvals.decide$remote", injected by the listener's
                // gate — the suffix is unreachable from the wire). The
                // command id is the audit session (P19B-R effect grants
                // persist actor/session/scope).
                self.service
                    .approvals_decide_with_session(
                        &params,
                        &command.client_id,
                        &command.command_id,
                        r_code_runtime::application::CommandSource::Local,
                    )
                    .await
                    .map_err(method_error)
            }
            "approvals.decide$remote" => {
                // R12: a remote device with the approvals:decide capability
                // (enforced by the listener gate before this runs). The
                // audit identity is the authenticated device id.
                self.service
                    .approvals_decide_with_session(
                        &params,
                        &command.client_id,
                        &command.command_id,
                        r_code_runtime::application::CommandSource::Remote,
                    )
                    .await
                    .map_err(method_error)
            }
            methods::SHUTDOWN => {
                self.shutdown.notify_waiters();
                Ok(serde_json::json!({"stopping": true}))
            }
            other => Err(format!("unknown method {other}")),
        }
    }

    async fn events_after(
        &self,
        after_seq: u64,
        limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        self.service.events_after(after_seq, limit).await
    }
}

/// The bundle's resource directory holding `plugins/<id>/` packages. In
/// packaged layouts the daemon sits beside the resources root; in dev
/// layouts this simply doesn't exist (explicit env override above).
fn builtin_resources_dir(_profile: &RuntimeProfile) -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        // macOS bundle: .../R-Code.app/Contents/MacOS/r-code-service + ../Resources
        let mac_resources = exe
            .parent()
            .and_then(|dir| dir.parent())
            .map(|dir| dir.join("Resources"));
        if let Some(dir) = mac_resources {
            if dir.join("plugins").is_dir() {
                return dir;
            }
        }
        // Windows/Linux install dir: resources staged beside the binary.
        if let Some(dir) = exe.parent() {
            let beside = dir.join("resources");
            if beside.join("plugins").is_dir() {
                return beside;
            }
            if dir.join("plugins").is_dir() {
                return dir.to_path_buf();
            }
        }
    }
    std::path::PathBuf::from(".")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match LaunchOptions::parse_args(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("r-code-service: {error}");
            std::process::exit(2);
        }
    };
    let profile = match RuntimeProfile::resolve(&options) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("r-code-service: {error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = profile.ensure_layout() {
        eprintln!("r-code-service: layout failure: {error}");
        std::process::exit(1);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async move {
        let lock = match ProfileLock::acquire(&profile.harness_v1_root(), &profile.profile_id()) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("r-code-service: {error}");
                std::process::exit(3);
            }
        };
        let identity = match lock.identity(&profile.harness_v1_root()) {
            Ok(identity) => identity,
            Err(error) => {
                eprintln!("r-code-service: {error}");
                std::process::exit(3);
            }
        };
        let store = match V1Store::open(&profile.database_path()) {
            Ok(store) => Arc::new(store),
            Err(error) => {
                eprintln!("r-code-service: store failure: {error}");
                std::process::exit(5);
            }
        };
        // Real composition: gateway tools over the profile workspaces root;
        // the model broker resolves through the v1 settings store (live:
        // settings applied at runtime take effect on the next model call).
        let settings_store = SettingsStore::for_profile(&profile);
        let models: Arc<dyn r_code_kernel::ports::ModelService> = Arc::new(ModelBroker::new(
            SettingsBackedResolver::new(settings_store),
        ));
        let tools: Arc<dyn r_code_kernel::ports::ToolService> =
            Arc::new(GatewayToolService::with_core_tools(
                Arc::new(AuthorizationService::new()),
                store.clone(),
                WorkspaceCapability::WriteWithin {
                    root: profile
                        .workspaces_root()
                        .to_string_lossy()
                        .replace('\\', "/"),
                },
                EffectivePermissions::full(),
            ));
        let service = match ApplicationService::compose_with_policy(
            &profile,
            models,
            tools,
            CompositionPolicy::StrictDaemon,
        ) {
            Ok(service) => Arc::new(service),
            Err(error) => {
                eprintln!("r-code-service: composition failure: {error}");
                std::process::exit(6);
            }
        };
        // P24B: publish the activation readiness BEFORE any ingress is
        // bound — recovery already ran inside composition (the readiness
        // was evaluated after it), and the granted set states exactly what
        // this boot may advertise. An unactivated host publishes an empty
        // set: SafeDisabled grants nothing.
        let readiness = service.activation_readiness();
        eprintln!(
            "r-code-service: activation readiness: recovered={} granted={:?} verdict={:?}",
            readiness.recovered, readiness.granted_capabilities, readiness.activation
        );
        // Undecided approvals from a previous daemon stay decidable: the
        // pending index rebuilds from the journal (RA1).
        service.rebuild_approvals().await;
        // P24A: regenerate this boot's safety diagnostics WITHOUT activating
        // anything. The gate re-derives the current platform report
        // (idempotent persistence) and evaluates the honest verdict —
        // Unsupported this wave — which safety.report.get serves and every
        // process/check/harness launch consults. Nothing opens here.
        match r_code_runtime::process_guard::BootIdentity::current() {
            Ok(boot) => {
                let sandbox_gate = r_code_runtime::services::sandbox::platform_activation_gate(
                    store.as_ref(),
                    boot.as_str(),
                );
                eprintln!("r-code-service: sandbox gate: {sandbox_gate:?}");
            }
            Err(error) => {
                eprintln!("r-code-service: boot identity failure: {error}");
                std::process::exit(3);
            }
        }
        // Remote surface (R08): registry/pairing/identity under the profile
        // root; the console app directory comes from the bundle or env.
        let harness_root = profile.harness_v1_root();
        let registry = match r_code_runtime::remote::DeviceRegistry::open(&harness_root) {
            Ok(registry) => Arc::new(registry),
            Err(error) => {
                eprintln!("r-code-service: device registry failure: {error}");
                std::process::exit(7);
            }
        };
        let tls_identity = match r_code_runtime::remote::ensure_identity(&harness_root) {
            Ok(identity) => identity,
            Err(error) => {
                eprintln!("r-code-service: TLS identity failure: {error}");
                std::process::exit(7);
            }
        };
        let app_dir = std::env::var("R_CODE_REMOTE_APP_DIR")
            .map(std::path::PathBuf::from)
            .ok()
            .or_else(|| Some(harness_root.join("remote-app")));
        let remote = RemoteSurface {
            manager: Arc::new(r_code_runtime::remote::RemoteManager::new(
                registry,
                Arc::new(r_code_runtime::remote::PairingSessions::new(
                    r_code_runtime::remote::PAIRING_TTL,
                )),
                tls_identity,
                r_code_runtime::remote::FanoutHub::new(),
                app_dir,
                "127.0.0.1".parse().expect("loopback"),
            )),
        };
        // The pairing listener keeps the journal-cursor publisher fed.
        {
            let store_for_events =
                Arc::new(V1Store::open(&profile.database_path()).expect("store reopen"));
            let hub = remote.manager.hub.clone();
            tokio::spawn(async move {
                let publisher = r_code_runtime::remote::CursorPublisher::new(store_for_events, hub);
                publisher.run(std::time::Duration::from_millis(250)).await;
            });
        }
        // Built-in harnesses register through the normal immutable registry
        // from the bundle's staged plugin resources (T38). Dev layouts can
        // point at one explicitly via R_CODE_BUILTIN_PLUGINS_DIR.
        let builtin_dir = std::env::var("R_CODE_BUILTIN_PLUGINS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| builtin_resources_dir(&profile));
        for outcome in service.ensure_builtins_from(&builtin_dir) {
            match outcome {
                Ok(None) => {}
                Ok(Some(installed)) => eprintln!(
                    "r-code-service: built-in registered: {} v{}",
                    installed.manifest.id.0, installed.manifest.version
                ),
                Err(error) => eprintln!("r-code-service: built-in registration failed: {error}"),
            }
        }
        let handler = Arc::new(ServiceHandler {
            service,
            shutdown: Arc::new(Notify::new()),
            harness_root: profile.harness_v1_root(),
            remote,
        });
        let shutdown = handler.shutdown.clone();
        let dedup = Arc::new(CommandDedup::new(
            &profile.profile_id(),
            store,
            handler.clone(),
        ));
        // The remote listener serves remote connections through the same
        // dedup-wrapped handler as the local pipe (F1).
        handler.remote.manager.wire_handler(dedup.clone()).await;
        let daemon = match Daemon::start(&profile.ipc_endpoint(), identity, dedup) {
            Ok(daemon) => daemon,
            Err(error) => {
                eprintln!("r-code-service: {error}");
                std::process::exit(4);
            }
        };
        eprintln!(
            "r-code-service: owning {} at {:?}",
            profile.profile_id(),
            profile.ipc_endpoint()
        );
        let serving = tokio::spawn(async move {
            let _ = daemon.serve().await;
        });
        shutdown.notified().await;
        serving.abort();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_handoff_params_accept_absent_null_and_both_key_styles() {
        let absent = parse_memory_handoff(&serde_json::json!({})).expect("absent ok");
        assert!(absent.is_none());
        let null = parse_memory_handoff(&serde_json::json!({"memory": null})).expect("null ok");
        assert!(null.is_none());

        let camel = parse_memory_handoff(&serde_json::json!({
            "memory": {
                "rendered": "<r_code_memory_snapshot>block</r_code_memory_snapshot>",
                "entryIds": ["e1", "e2"],
                "snapshotHash": "hash-1",
            }
        }))
        .expect("camel ok")
        .expect("some handoff");
        assert_eq!(camel.entry_ids, vec!["e1".to_string(), "e2".to_string()]);
        assert_eq!(camel.snapshot_hash, "hash-1");

        let snake = parse_memory_handoff(&serde_json::json!({
            "memory": {
                "rendered": "block",
                "entry_ids": [],
                "snapshot_hash": "h",
            }
        }))
        .expect("snake ok")
        .expect("some handoff");
        assert!(snake.entry_ids.is_empty());
    }

    #[test]
    fn memory_handoff_params_reject_invalid_shapes() {
        let missing_rendered = parse_memory_handoff(&serde_json::json!({
            "memory": {"snapshotHash": "h"}
        }));
        assert!(missing_rendered.is_err());

        let oversized = parse_memory_handoff(&serde_json::json!({
            "memory": {
                "rendered": "x".repeat(32_769),
                "entryIds": [],
                "snapshotHash": "h",
            }
        }));
        assert!(oversized.is_err());

        let blank_hash = parse_memory_handoff(&serde_json::json!({
            "memory": {"rendered": "r", "snapshotHash": " "}
        }));
        assert!(blank_hash.is_err());
    }

    #[test]
    fn task_preference_patch_preserves_omitted_fields_and_sets_prompt_scope() {
        let mut preferences = r_code_kernel::task::TaskPreferences {
            model_route: None,
            model: Some("configured-model".into()),
            inference: Some(serde_json::json!({"reasoning_effort": "high"})),
            mode: Some("ask".into()),
            system_prompt: None,
            workspace_path: None,
            require_desktop_confirm: true,
        };

        let patch = parse_task_preferences_patch(&serde_json::json!({
            "mode": "edit",
            "systemPrompt": "project prompt",
            "workspacePath": "D:/workspace/project",
        }))
        .expect("parse preferences patch");
        patch.apply_to(&mut preferences);

        assert_eq!(preferences.model.as_deref(), Some("configured-model"));
        assert_eq!(
            preferences.inference,
            Some(serde_json::json!({"reasoning_effort": "high"}))
        );
        assert_eq!(preferences.mode.as_deref(), Some("edit"));
        assert_eq!(preferences.system_prompt.as_deref(), Some("project prompt"));
        assert_eq!(
            preferences.workspace_path.as_deref(),
            Some("D:/workspace/project")
        );
        assert!(preferences.require_desktop_confirm);
    }

    #[test]
    fn task_preference_patch_rejects_an_invalid_prompt() {
        let error =
            parse_task_preferences_patch(&serde_json::json!({"systemPrompt": "bad\u{0}prompt"}))
                .expect_err("NUL prompt must be rejected");
        assert!(error.contains("NUL"));
    }
}
