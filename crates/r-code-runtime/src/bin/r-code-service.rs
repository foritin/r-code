//! r-code-service: the single-owner background daemon for one v2 profile.
//!
//! Boot order: explicit `--profile` (never inferred) → RuntimeProfile →
//! profile ownership lock → open the v2 store → compose the
//! ApplicationService (real surface: plugin catalog, task lifecycle, the
//! durable event journal) → bind the authenticated local endpoint → serve.
//! The daemon does not exit when frontends disconnect; it stops only via an
//! explicit `service.shutdown` command or process termination.

use r_code_harness_protocol::application::methods;
use r_code_runtime::application::{ApplicationError, ApplicationService};
use r_code_runtime::application_receipts::CommandDedup;
use r_code_runtime::daemon::{ApplicationHandler, Daemon, ProfileLock};
use r_code_runtime::services::authorization::{
    AuthorizationService, EffectivePermissions, WorkspaceCapability,
};
use r_code_runtime::services::models::ModelBroker;
use r_code_runtime::services::settings_store::{
    ProviderEntry, SettingsBackedResolver, SettingsStore,
};
use r_code_runtime::services::tools::GatewayToolService;
use r_code_runtime::{LaunchOptions, RuntimeProfile};
use r_code_store::v2::V2Store;
use std::sync::Arc;
use tokio::sync::Notify;

/// Adapter: ApplicationService over the daemon's RPC surface. Thin — every
/// method maps 1:1 onto the composed service; long work returns durable
/// operation ids via the CommandDedup wrapper.
struct ServiceHandler {
    service: Arc<ApplicationService>,
    shutdown: Arc<Notify>,
    /// Profile v2 root: hosts the persistent side-effect counter used by
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
                // Omitted task ids get a host-generated one (TUI /new).
                let task_id = params["taskId"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("task-{}", uuid::Uuid::new_v4().simple()));
                let task_id = task_id.as_str();
                let objective = params["objective"].as_str().ok_or("missing objective")?;
                let kind = match params["kind"].as_str().unwrap_or("conversation") {
                    "implementation" => r_code_kernel::task::TaskKind::Implementation,
                    "plan-draft" => r_code_kernel::task::TaskKind::PlanDraft,
                    "repair" => r_code_kernel::task::TaskKind::Repair,
                    _ => r_code_kernel::task::TaskKind::Conversation,
                };
                let required_checks: Vec<String> = params["requiredChecks"]
                    .as_array()
                    .map(|checks| {
                        checks
                            .iter()
                            .filter_map(|check| check.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let state = self
                    .service
                    .create_task(task_id, objective, kind, required_checks)
                    .await
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
                let preferences = r_code_kernel::task::TaskPreferences {
                    model: params["model"].as_str().map(str::to_string),
                    inference: params.get("inference").cloned(),
                    mode: params["mode"].as_str().map(str::to_string),
                    require_desktop_confirm: params["requireDesktopConfirm"]
                        .as_bool()
                        .unwrap_or(false),
                };
                self.service
                    .set_task_preferences(task_id, preferences)
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
                let availability = self.service.settings().availability();
                Ok(serde_json::to_value(availability).unwrap_or_default())
            }
            "settings.get" => {
                let settings = self.service.settings().load();
                Ok(serde_json::to_value(settings).unwrap_or_default())
            }
            "settings.apply" => {
                let selection = params["selection"].as_str().ok_or("missing selection")?;
                let entry = ProviderEntry {
                    selection: selection.to_string(),
                    model: params["model"].as_str().ok_or("missing model")?.to_string(),
                    base_url: params["baseUrl"].as_str().map(str::to_string),
                    protocol: params["protocol"].as_str().map(str::to_string),
                    env_var: params["envVar"].as_str().map(str::to_string),
                };
                let api_key = params["apiKey"].as_str();
                self.service
                    .settings()
                    .apply_provider(entry, api_key)
                    .map_err(|e| ApplicationError::Settings(e.to_string()).to_string())?;
                Ok(
                    serde_json::to_value(self.service.settings().availability())
                        .unwrap_or_default(),
                )
            }
            "settings.setDefault" => {
                let selection = params["selection"].as_str().ok_or("missing selection")?;
                self.service
                    .settings()
                    .set_default(selection)
                    .map_err(|e| ApplicationError::Settings(e.to_string()).to_string())?;
                Ok(serde_json::Value::Null)
            }
            "settings.removeProvider" => {
                let selection = params["selection"].as_str().ok_or("missing selection")?;
                self.service
                    .settings()
                    .remove_provider(selection)
                    .map_err(|e| ApplicationError::Settings(e.to_string()).to_string())?;
                Ok(serde_json::Value::Null)
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
                // gate — the suffix is unreachable from the wire).
                self.service
                    .approvals_decide(
                        &params,
                        &command.client_id,
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
                    .approvals_decide(
                        &params,
                        &command.client_id,
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
        let lock = match ProfileLock::acquire(&profile.harness_v2_root(), &profile.profile_id()) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("r-code-service: {error}");
                std::process::exit(3);
            }
        };
        let identity = match lock.identity(&profile.harness_v2_root()) {
            Ok(identity) => identity,
            Err(error) => {
                eprintln!("r-code-service: {error}");
                std::process::exit(3);
            }
        };
        let store = match V2Store::open(&profile.database_path()) {
            Ok(store) => Arc::new(store),
            Err(error) => {
                eprintln!("r-code-service: store failure: {error}");
                std::process::exit(5);
            }
        };
        // Real composition: gateway tools over the profile workspaces root;
        // the model broker resolves through the v2 settings store (live:
        // settings applied at runtime take effect on the next model call).
        let settings_store = SettingsStore::new(profile.harness_v2_root());
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
        let service = match ApplicationService::compose(&profile, models, tools) {
            Ok(service) => Arc::new(service),
            Err(error) => {
                eprintln!("r-code-service: composition failure: {error}");
                std::process::exit(6);
            }
        };
        // Undecided approvals from a previous daemon stay decidable: the
        // pending index rebuilds from the journal (RA1).
        service.rebuild_approvals().await;
        // Remote surface (R08): registry/pairing/identity under the profile
        // root; the console app directory comes from the bundle or env.
        let harness_root = profile.harness_v2_root();
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
                Arc::new(V2Store::open(&profile.database_path()).expect("store reopen"));
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
            harness_root: profile.harness_v2_root(),
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
