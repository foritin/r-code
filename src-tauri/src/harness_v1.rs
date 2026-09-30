//! Harness v1 desktop bridge (T33/T34).
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
pub struct HarnessV1Bridge {
    profile: RuntimeProfile,
    service_binary: Option<PathBuf>,
}

/// Errors surfaced to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HarnessV1Error {
    #[error("daemon unavailable: {0}")]
    Daemon(String),
    #[error("command failed: {0}")]
    Command(String),
    #[error("io failure: {0}")]
    Io(String),
}

impl From<ClientError> for HarnessV1Error {
    fn from(error: ClientError) -> Self {
        HarnessV1Error::Daemon(error.to_string())
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

impl HarnessV1Bridge {
    /// Resolve the bridge from explicit launch values (the GUI passes its
    /// build flavor; nothing is inferred from Tauri features here).
    pub fn new(flavor: ProfileFlavor, data_root: Option<PathBuf>) -> Result<Self, HarnessV1Error> {
        let mut options = LaunchOptions::new(flavor);
        if let Some(root) = data_root {
            options = options.with_data_root(root);
        }
        let profile = RuntimeProfile::resolve(&options)
            .map_err(|error| HarnessV1Error::Daemon(error.to_string()))?;
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
    pub async fn connect(&self) -> Result<DaemonClient, HarnessV1Error> {
        let info = r_code_client::ensure_daemon(
            &self.profile.harness_v1_root(),
            &self.profile.ipc_endpoint(),
            &self.profile.profile_id(),
            self.service_binary.as_deref(),
        )
        .await
        .map_err(HarnessV1Error::from)?;
        DaemonClient::connect(
            &self.profile.ipc_endpoint(),
            &self.profile.profile_id(),
            &info.token,
            "desktop-gui",
        )
        .await
        .map_err(HarnessV1Error::from)
    }
}

/// Shared per-app bridge state (managed by Tauri).
pub fn shared_bridge() -> Result<Arc<Mutex<HarnessV1Bridge>>, HarnessV1Error> {
    let (flavor, data_root) = crate::app_paths::AppFlavor::current().harness_launch_options();
    let flavor = match flavor {
        "production" => ProfileFlavor::Production,
        _ => ProfileFlavor::Development,
    };
    let bridge = HarnessV1Bridge::new(flavor, data_root)?;
    Ok(Arc::new(Mutex::new(bridge)))
}

// ---------------------------------------------------------------------------
// Plain-callable surface (shared by the Tauri wrappers and tests)
// ---------------------------------------------------------------------------

impl HarnessV1Bridge {
    pub async fn ping(&self) -> Result<Value, HarnessV1Error> {
        self.connect()
            .await?
            .call("ping", serde_json::json!({}))
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    pub async fn plugins_list(&self) -> Result<Value, HarnessV1Error> {
        self.connect()
            .await?
            .call("plugins.list", serde_json::json!({}))
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    pub async fn plugins_install(&self, path: &str) -> Result<Value, HarnessV1Error> {
        self.connect()
            .await?
            .call("plugins.install", serde_json::json!({"path": path}))
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    pub async fn plugins_set_enabled(
        &self,
        id: &str,
        digest: &str,
        enabled: bool,
    ) -> Result<(), HarnessV1Error> {
        self.connect()
            .await?
            .call(
                "plugins.setEnabled",
                serde_json::json!({"id": id, "digest": digest, "enabled": enabled}),
            )
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))?;
        Ok(())
    }

    pub async fn plugins_remove(&self, id: &str, digest: &str) -> Result<(), HarnessV1Error> {
        self.connect()
            .await?
            .call(
                "plugins.remove",
                serde_json::json!({"id": id, "digest": digest}),
            )
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))?;
        Ok(())
    }

    pub async fn task_create(
        &self,
        task_id: &str,
        objective: &str,
        kind: &str,
        required_checks: Vec<String>,
    ) -> Result<Value, HarnessV1Error> {
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
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    pub async fn task_select_harness(
        &self,
        task_id: &str,
        harness_id: &str,
    ) -> Result<Value, HarnessV1Error> {
        self.connect()
            .await?
            .call(
                "task.selectHarness",
                serde_json::json!({"taskId": task_id, "harnessId": harness_id}),
            )
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    pub async fn task_send_message(
        &self,
        task_id: &str,
        text: &str,
    ) -> Result<Value, HarnessV1Error> {
        self.connect()
            .await?
            .call(
                "task.sendMessage",
                serde_json::json!({"taskId": task_id, "text": text}),
            )
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    /// Explicitly stop the owning daemon (cancels/drains its work). The
    /// one supported stop path; frontends closing never stop it.
    pub async fn shutdown_daemon(&self) -> Result<(), HarnessV1Error> {
        self.connect()
            .await?
            .call("service.shutdown", serde_json::json!({}))
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))?;
        Ok(())
    }

    pub async fn task_events(&self, after_seq: u64) -> Result<Value, HarnessV1Error> {
        self.connect()
            .await?
            .call(
                "task.events",
                serde_json::json!({"afterSeq": after_seq, "limit": 200}),
            )
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    // -- P19B-C 效果授权 --------------------------------------------------
    //
    // 三个 RPC 在 daemon 侧是 local-only（远端被未知方法能力门拒绝，插件
    // router 没有 effect 面），actor 恒为连接身份、session 恒为 command id。
    // 客户端传的 actorId/sessionId/decidedBy 一律被忽略，这里也绝不转发。

    /// `approvals.effect.request`：按已批准计划的真实 WorkUnit 派生六列
    /// 权威材料并登记一个待决操作；已存在完全相同的 active 授权时回放它。
    pub async fn effect_approval_request(
        &self,
        task_id: &str,
        work_unit_id: &str,
        operation_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Value, HarnessV1Error> {
        let mut params = serde_json::json!({
            "taskId": task_id,
            "workUnitId": work_unit_id,
        });
        if let Some(operation_id) = operation_id {
            params["operationId"] = serde_json::json!(operation_id);
        }
        if let Some(run_id) = run_id {
            params["runId"] = serde_json::json!(run_id);
        }
        self.connect()
            .await?
            .call("approvals.effect.request", params)
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    /// `approvals.effect.list`：一个任务的全部落库审批（active 与
    /// superseded）+ 仍可决策的 pending 请求，两者都投影同一份材料。
    pub async fn effect_approval_list(&self, task_id: &str) -> Result<Value, HarnessV1Error> {
        self.connect()
            .await?
            .call(
                "approvals.effect.list",
                serde_json::json!({"taskId": task_id}),
            )
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    /// `approvals.effect.revoke`：把某个 WorkUnit 的 active 授权置为
    /// superseded。**仅影响未来运行**——已冻结的 RunSnapshot 是不可变行，
    /// 撤销不改写它，只有下一次冻结才重新查阅并失败关闭。幂等：没有
    /// active 授权时回 `revoked: false`。
    pub async fn effect_approval_revoke(
        &self,
        task_id: &str,
        work_unit_id: &str,
    ) -> Result<Value, HarnessV1Error> {
        self.connect()
            .await?
            .call(
                "approvals.effect.revoke",
                serde_json::json!({"taskId": task_id, "workUnitId": work_unit_id}),
            )
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }

    /// `approvals.decide`：授权决策仍走既有认证面（不重铸安全边界）。
    /// effect 授权时响应多一个 `approval` 键，正是那唯一一条物化审批；
    /// 拒绝/过期不落任何审批。
    pub async fn approval_decide(
        &self,
        operation_id: &str,
        decision: &str,
    ) -> Result<Value, HarnessV1Error> {
        if !matches!(decision, "granted" | "denied") {
            return Err(HarnessV1Error::Command(format!(
                "decision must be granted or denied, got {decision}"
            )));
        }
        self.connect()
            .await?
            .call(
                "approvals.decide",
                serde_json::json!({"operationId": operation_id, "decision": decision}),
            )
            .await
            .map_err(|e| HarnessV1Error::Command(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Tauri commands (thin wrappers; all logic rides the daemon)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cmd_harness_v1_ping(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge.ping().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v1_plugins_list(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge.plugins_list().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v1_plugins_install(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
    path: String,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .plugins_install(&path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v1_plugins_set_enabled(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
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
pub async fn cmd_harness_v1_plugins_remove(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
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
pub async fn cmd_harness_v1_task_create(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
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
pub async fn cmd_harness_v1_task_select_harness(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
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
pub async fn cmd_harness_v1_task_send(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
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
pub async fn cmd_harness_v1_task_events(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
    after_seq: u64,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .task_events(after_seq)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v1_effect_request(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
    task_id: String,
    work_unit_id: String,
    operation_id: Option<String>,
    run_id: Option<String>,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .effect_approval_request(
            &task_id,
            &work_unit_id,
            operation_id.as_deref(),
            run_id.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v1_effect_list(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
    task_id: String,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .effect_approval_list(&task_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v1_effect_revoke(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
    task_id: String,
    work_unit_id: String,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .effect_approval_revoke(&task_id, &work_unit_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_harness_v1_approval_decide(
    state: tauri::State<'_, Arc<Mutex<HarnessV1Bridge>>>,
    operation_id: String,
    decision: String,
) -> Result<Value, String> {
    let bridge = state.lock().await.clone();
    bridge
        .approval_decide(&operation_id, &decision)
        .await
        .map_err(|e| e.to_string())
}
