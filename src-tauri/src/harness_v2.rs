//! Harness v2 desktop bridge (T33/T34).
//!
//! Thin Tauri command layer delegating to the shared r-code-service daemon
//! through `r-code-client`: the desktop owns UI/system concerns only —
//! task/plugin state and the event journal live daemon-side. The bridge
//! resolves the profile from the app flavor (explicit, never inferred from
//! Tauri features), discovers or spawns the daemon, and forwards typed
//! calls. Long operations return durable results deduplicated by the
//! daemon (T06b).

use r_code_client::{ClientError, DaemonClient};
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Bridge configuration: where the profile lives and which service binary
/// to spawn when no daemon owns the profile yet.
#[derive(Debug, Clone)]
pub struct HarnessV2Bridge {
    profile: RuntimeProfile,
    service_binary: Option<PathBuf>,
}

/// Errors surfaced to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HarnessV2Error {
    #[error("daemon unavailable: {0}")]
    Daemon(String),
    #[error("command failed: {0}")]
    Command(String),
    #[error("io failure: {0}")]
    Io(String),
}

impl From<ClientError> for HarnessV2Error {
    fn from(error: ClientError) -> Self {
        HarnessV2Error::Daemon(error.to_string())
    }
}

fn default_service_binary() -> Option<PathBuf> {
    // Env override first (tests, portable layouts).
    if let Ok(path) = std::env::var("R_CODE_SERVICE_BIN") {
        return Some(PathBuf::from(path));
    }
    // Packaged layout: the service ships beside the app executable
    // (T38 packaging); dev layout: the workspace target dir.
    if let Ok(exe) = std::env::current_exe() {
        #[cfg(windows)]
        let beside = exe.parent()?.join("r-code-service.exe");
        #[cfg(not(windows))]
        let beside = exe.parent()?.join("r-code-service");
        if beside.is_file() {
            return Some(beside);
        }
        let dev = exe
            .parent()
            .and_then(|dir| dir.parent())
            .map(|dir| dir.join("r-code-service"))
            .map(|path| {
                #[cfg(windows)]
                let path = path.with_extension("exe");
                path
            });
        if let Some(dev) = dev {
            if dev.is_file() {
                return Some(dev);
            }
        }
    }
    None
}

impl HarnessV2Bridge {
    /// Resolve the bridge from explicit launch values (the GUI passes its
    /// build flavor; nothing is inferred from Tauri features here).
    pub fn new(flavor: ProfileFlavor, data_root: Option<PathBuf>) -> Result<Self, HarnessV2Error> {
        let mut options = LaunchOptions::new(flavor);
        if let Some(root) = data_root {
            options = options.with_data_root(root);
        }
        let profile = RuntimeProfile::resolve(&options)
            .map_err(|error| HarnessV2Error::Daemon(error.to_string()))?;
        Ok(Self {
            profile,
            service_binary: default_service_binary(),
        })
    }

    /// Construct from an already-resolved profile (tests, custom layouts).
    pub fn new_from_profile(profile: RuntimeProfile, service_binary: Option<PathBuf>) -> Self {
        Self {
            profile,
            service_binary: service_binary.or_else(default_service_binary),
        }
    }

    /// Override the service binary (tests, custom installs).
    pub fn with_service_binary(mut self, binary: Option<PathBuf>) -> Self {
        self.service_binary = binary;
        self
    }

    pub fn profile(&self) -> &RuntimeProfile {
        &self.profile
    }

    /// Reach the owning daemon, spawning one when nothing owns the profile
    /// (races converge: losing spawns exit on the ownership lock).
    pub async fn connect(&self) -> Result<DaemonClient, HarnessV2Error> {
        let info = r_code_client::ensure_daemon(
            &self.profile.harness_v2_root(),
            &self.profile.ipc_endpoint(),
            &self.profile.profile_id(),
            self.service_binary.as_deref(),
        )
        .await
        .map_err(HarnessV2Error::from)?;
        DaemonClient::connect(
            &self.profile.ipc_endpoint(),
            &self.profile.profile_id(),
            &info.token,
            "desktop-gui",
        )
        .await
        .map_err(HarnessV2Error::from)
    }
}

/// Shared per-app bridge state (managed by Tauri).
pub fn shared_bridge() -> Result<Arc<Mutex<HarnessV2Bridge>>, HarnessV2Error> {
    let (flavor, data_root) = crate::app_paths::AppFlavor::current().harness_launch_options();
    let flavor = match flavor {
        "production" => ProfileFlavor::Production,
        _ => ProfileFlavor::Development,
    };
    let bridge = HarnessV2Bridge::new(flavor, data_root)?;
    Ok(Arc::new(Mutex::new(bridge)))
}

// ---------------------------------------------------------------------------
// Plain-callable surface (shared by the Tauri wrappers and tests)
// ---------------------------------------------------------------------------

impl HarnessV2Bridge {
    pub async fn ping(&self) -> Result<Value, HarnessV2Error> {
        self.connect()
            .await?
            .call("ping", serde_json::json!({}))
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))
    }

    pub async fn plugins_list(&self) -> Result<Value, HarnessV2Error> {
        self.connect()
            .await?
            .call("plugins.list", serde_json::json!({}))
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))
    }

    pub async fn plugins_install(&self, path: &str) -> Result<Value, HarnessV2Error> {
        self.connect()
            .await?
            .call("plugins.install", serde_json::json!({"path": path}))
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))
    }

    pub async fn plugins_set_enabled(
        &self,
        id: &str,
        digest: &str,
        enabled: bool,
    ) -> Result<(), HarnessV2Error> {
        self.connect()
            .await?
            .call(
                "plugins.setEnabled",
                serde_json::json!({"id": id, "digest": digest, "enabled": enabled}),
            )
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))?;
        Ok(())
    }

    pub async fn plugins_remove(&self, id: &str, digest: &str) -> Result<(), HarnessV2Error> {
        self.connect()
            .await?
            .call(
                "plugins.remove",
                serde_json::json!({"id": id, "digest": digest}),
            )
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))?;
        Ok(())
    }

    pub async fn task_create(
        &self,
        task_id: &str,
        objective: &str,
        kind: &str,
        required_checks: Vec<String>,
    ) -> Result<Value, HarnessV2Error> {
        self.connect()
            .await?
            .call(
                "task.create",
                serde_json::json!({
                    "taskId": task_id,
                    "objective": objective,
                    "kind": kind,
                    "requiredChecks": required_checks,
                }),
            )
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))
    }

    pub async fn task_select_harness(
        &self,
        task_id: &str,
        harness_id: &str,
    ) -> Result<Value, HarnessV2Error> {
        self.connect()
            .await?
            .call(
                "task.selectHarness",
                serde_json::json!({"taskId": task_id, "harnessId": harness_id}),
            )
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))
    }

    pub async fn task_send_message(
        &self,
        task_id: &str,
        text: &str,
    ) -> Result<Value, HarnessV2Error> {
        self.connect()
            .await?
            .call(
                "task.sendMessage",
                serde_json::json!({"taskId": task_id, "text": text}),
            )
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))
    }

    /// Explicitly stop the owning daemon (cancels/drains its work). The
    /// one supported stop path; frontends closing never stop it.
    pub async fn shutdown_daemon(&self) -> Result<(), HarnessV2Error> {
        self.connect()
            .await?
            .call("service.shutdown", serde_json::json!({}))
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))?;
        Ok(())
    }

    pub async fn task_events(&self, after_seq: u64) -> Result<Value, HarnessV2Error> {
        self.connect()
            .await?
            .call(
                "task.events",
                serde_json::json!({"afterSeq": after_seq, "limit": 200}),
            )
            .await
            .map_err(|e| HarnessV2Error::Command(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Tauri commands (thin wrappers; all logic rides the daemon)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cmd_harness_v2_ping(
    state: tauri::State<'_, Arc<Mutex<HarnessV2Bridge>>>,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge.ping().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v2_plugins_list(
    state: tauri::State<'_, Arc<Mutex<HarnessV2Bridge>>>,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge.plugins_list().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v2_plugins_install(
    state: tauri::State<'_, Arc<Mutex<HarnessV2Bridge>>>,
    path: String,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .plugins_install(&path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v2_plugins_set_enabled(
    state: tauri::State<'_, Arc<Mutex<HarnessV2Bridge>>>,
    id: String,
    digest: String,
    enabled: bool,
) -> Result<(), String> {
    let bridge = state.lock().await.clone();
    bridge
        .plugins_set_enabled(&id, &digest, enabled)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v2_plugins_remove(
    state: tauri::State<'_, Arc<Mutex<HarnessV2Bridge>>>,
    id: String,
    digest: String,
) -> Result<(), String> {
    let bridge = state.lock().await.clone();
    bridge
        .plugins_remove(&id, &digest)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v2_task_create(
    state: tauri::State<'_, Arc<Mutex<HarnessV2Bridge>>>,
    task_id: String,
    objective: String,
    kind: String,
    required_checks: Vec<String>,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .task_create(&task_id, &objective, &kind, required_checks)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v2_task_select_harness(
    state: tauri::State<'_, Arc<Mutex<HarnessV2Bridge>>>,
    task_id: String,
    harness_id: String,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .task_select_harness(&task_id, &harness_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v2_task_send(
    state: tauri::State<'_, Arc<Mutex<HarnessV2Bridge>>>,
    task_id: String,
    text: String,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .task_send_message(&task_id, &text)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v2_task_events(
    state: tauri::State<'_, Arc<Mutex<HarnessV2Bridge>>>,
    after_seq: u64,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .task_events(after_seq)
        .await
        .map_err(|e| e.to_string())
}
