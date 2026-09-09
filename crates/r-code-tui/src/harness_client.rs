//! TUI Harness v2 客户端（/plugins 命令组，T36）。
//!
//! 与 GUI 走同一套 ApplicationService 操作（经 r-code-client 连接共享
//! r-code-service 守护进程），不依赖 r-code-host/Tauri。命令输出为
//! 结构化的 [`HarnessCommandOutcome`]，交互模式渲染为字符网格行，
//! print/脚本模式消费机器可读的 JSON 行。

use r_code_client::DaemonClient;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 一条目录项（宿主 envelope 的本地视图）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginEntry {
    pub id: String,
    pub version: String,
    pub display_name: String,
    pub digest: String,
    pub enabled: bool,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

/// 命令结果：机器可读（print/测试）+ 人类行（交互）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessCommandOutcome {
    pub ok: bool,
    pub command: String,
    /// 渲染好的状态行（每行一条）。
    pub lines: Vec<String>,
    /// 机器可读细节（op 结果 / 错误消息）。
    pub detail: serde_json::Value,
}

impl HarnessCommandOutcome {
    fn ok(command: &str, lines: Vec<String>, detail: serde_json::Value) -> Self {
        Self {
            ok: true,
            command: command.to_string(),
            lines,
            detail,
        }
    }

    fn err(command: &str, message: String) -> Self {
        Self {
            ok: false,
            command: command.to_string(),
            lines: vec![message.clone()],
            detail: serde_json::json!({"error": message}),
        }
    }
}

/// v2 客户端桥（每个 TUI 进程一个；克隆共享配置）。
#[derive(Clone)]
pub struct HarnessTuiClient {
    profile: RuntimeProfile,
    service_binary: Option<PathBuf>,
}

impl HarnessTuiClient {
    /// 按 TUI 的 --profile/--ipc-name 解析（显式，绝不推断）。
    pub fn from_args(
        profile: ProfileFlavor,
        data_root: Option<PathBuf>,
        ipc_name: Option<String>,
    ) -> Result<Self, String> {
        let mut options = LaunchOptions::new(profile);
        if let Some(root) = data_root {
            options = options.with_data_root(root);
        }
        if let Some(name) = ipc_name {
            options = options.with_ipc_name(name);
        }
        let profile = RuntimeProfile::resolve(&options).map_err(|e| e.to_string())?;
        Ok(Self {
            profile,
            service_binary: default_service_binary(),
        })
    }

    /// 测试用：显式 profile + service 二进制。
    pub fn from_profile(profile: RuntimeProfile, service_binary: Option<PathBuf>) -> Self {
        Self {
            profile,
            service_binary: service_binary.or_else(default_service_binary),
        }
    }

    async fn connect(&self) -> Result<DaemonClient, String> {
        let info = r_code_client::ensure_daemon(
            &self.profile.harness_v2_root(),
            &self.profile.ipc_endpoint(),
            &self.profile.profile_id(),
            self.service_binary.as_deref(),
        )
        .await
        .map_err(|e| format!("共享后台服务不可用：{e}"))?;
        DaemonClient::connect(
            &self.profile.ipc_endpoint(),
            &self.profile.profile_id(),
            &info.token,
            "r-code-tui",
        )
        .await
        .map_err(|e| format!("连接共享后台服务失败：{e}"))
    }

    /// 执行一条 /plugins 子命令。
    ///
    /// - `/plugins` 或 `/plugins list`
    /// - `/plugins install <目录>`
    /// - `/plugins enable <id> <digest>`
    /// - `/plugins disable <id> <digest>`
    /// - `/plugins remove <id> <digest>`
    /// - `/plugins use <task-id> <harness-id>`（空闲任务切换）
    /// - `/plugins help`
    pub async fn execute(&self, input: &str) -> HarnessCommandOutcome {
        let trimmed = input.trim();
        let mut parts = trimmed.split_whitespace();
        let sub = parts.next().unwrap_or("list");
        match sub {
            "list" | "" => self.list().await,
            "install" => match parts.next() {
                Some(path) => self.install(trimmed, path).await,
                None => HarnessCommandOutcome::err(
                    trimmed,
                    "用法：/plugins install <本地包目录>".into(),
                ),
            },
            "enable" | "disable" => {
                let (Some(id), Some(digest)) = (parts.next(), parts.next()) else {
                    return HarnessCommandOutcome::err(
                        trimmed,
                        format!("用法：/plugins {sub} <id> <digest>"),
                    );
                };
                self.set_enabled(trimmed, id, digest, sub == "enable").await
            }
            "remove" => match (parts.next(), parts.next()) {
                (Some(id), Some(digest)) => self.remove(trimmed, id, digest).await,
                _ => HarnessCommandOutcome::err(
                    trimmed,
                    "用法：/plugins remove <id> <digest>".into(),
                ),
            },
            "use" => match (parts.next(), parts.next()) {
                (Some(task), Some(harness)) => self.select(trimmed, task, harness).await,
                _ => HarnessCommandOutcome::err(
                    trimmed,
                    "用法：/plugins use <task-id> <harness-id>".into(),
                ),
            },
            _ => HarnessCommandOutcome::ok(
                trimmed,
                vec![
                    "/plugins [list]        列出已安装的 Harness 插件".into(),
                    "/plugins install <目录> 安装本地插件包（目录含 harness.json）".into(),
                    "/plugins enable|disable <id> <digest>  启用/停用（可逆）".into(),
                    "/plugins remove <id> <digest>          移除（被运行中任务固定的版本会拒绝）"
                        .into(),
                    "/plugins use <task-id> <harness-id>   为空闲任务选择 Harness".into(),
                ],
                serde_json::json!({"help": true}),
            ),
        }
    }

    async fn list(&self) -> HarnessCommandOutcome {
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => return HarnessCommandOutcome::err("list", error),
        };
        let value = match client.call("plugins.list", serde_json::json!({})).await {
            Ok(value) => value,
            Err(error) => return HarnessCommandOutcome::err("list", error.to_string()),
        };
        let entries = parse_entries(&value);
        if entries.is_empty() {
            return HarnessCommandOutcome::ok(
                "list",
                vec!["尚未安装任何 Harness 插件（/plugins install <目录>）".into()],
                serde_json::json!(entries),
            );
        }
        let mut lines = vec!["已安装的 Harness 插件：".to_string()];
        for entry in &entries {
            lines.push(render_entry(entry));
        }
        HarnessCommandOutcome::ok("list", lines, serde_json::json!(entries))
    }

    async fn install(&self, command: &str, path: &str) -> HarnessCommandOutcome {
        if !Path::new(path).is_dir() {
            return HarnessCommandOutcome::err(command, format!("目录不存在：{path}"));
        }
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => return HarnessCommandOutcome::err(command, error),
        };
        match client
            .call("plugins.install", serde_json::json!({"path": path}))
            .await
        {
            Ok(value) => HarnessCommandOutcome::ok(
                command,
                vec![format!(
                    "已安装 {} v{}（{}）",
                    value["id"].as_str().unwrap_or("?"),
                    value["version"].as_str().unwrap_or("?"),
                    value["contentDigest"].as_str().unwrap_or("?")
                )],
                value,
            ),
            Err(error) => HarnessCommandOutcome::err(command, error.to_string()),
        }
    }

    async fn set_enabled(
        &self,
        command: &str,
        id: &str,
        digest: &str,
        enabled: bool,
    ) -> HarnessCommandOutcome {
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => return HarnessCommandOutcome::err(command, error),
        };
        match client
            .call(
                "plugins.setEnabled",
                serde_json::json!({"id": id, "digest": digest, "enabled": enabled}),
            )
            .await
        {
            Ok(_) => HarnessCommandOutcome::ok(
                command,
                vec![format!("已{} {id}", if enabled { "启用" } else { "停用" })],
                serde_json::json!({"id": id, "enabled": enabled}),
            ),
            Err(error) => HarnessCommandOutcome::err(command, error.to_string()),
        }
    }

    async fn remove(&self, command: &str, id: &str, digest: &str) -> HarnessCommandOutcome {
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => return HarnessCommandOutcome::err(command, error),
        };
        match client
            .call(
                "plugins.remove",
                serde_json::json!({"id": id, "digest": digest}),
            )
            .await
        {
            Ok(_) => HarnessCommandOutcome::ok(
                command,
                vec![format!("已移除 {id}")],
                serde_json::json!({"removed": id}),
            ),
            Err(error) => HarnessCommandOutcome::err(command, error.to_string()),
        }
    }

    async fn select(
        &self,
        command: &str,
        task_id: &str,
        harness_id: &str,
    ) -> HarnessCommandOutcome {
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => return HarnessCommandOutcome::err(command, error),
        };
        match client
            .call(
                "task.selectHarness",
                serde_json::json!({"taskId": task_id, "harnessId": harness_id}),
            )
            .await
        {
            Ok(value) => HarnessCommandOutcome::ok(
                command,
                vec![format!(
                    "任务 {task_id} 已固定 {} v{}（仅新 Run 生效）",
                    value["id"].as_str().unwrap_or(harness_id),
                    value["version"].as_str().unwrap_or("?")
                )],
                value,
            ),
            Err(error) => HarnessCommandOutcome::err(command, error.to_string()),
        }
    }

    pub fn profile(&self) -> &RuntimeProfile {
        &self.profile
    }
}

/// 渲染一条目录项（字符网格一行；对齐列宽按内容计算）。
pub fn render_entry(entry: &PluginEntry) -> String {
    let state = if entry.available && entry.enabled {
        "可用".to_string()
    } else if entry.available {
        "已停用".to_string()
    } else {
        match &entry.unavailable_reason {
            Some(reason) => format!("不可用({reason})"),
            None => "不可用".to_string(),
        }
    };
    format!(
        "  {} v{}  [{}]  {}",
        entry.display_name, entry.version, state, entry.digest
    )
}

/// 解析宿主 plugins.list 返回的 envelope 数组（声明式数据）。
pub fn parse_entries(value: &serde_json::Value) -> Vec<PluginEntry> {
    value
        .as_array()
        .map(|array| {
            array
                .iter()
                .filter_map(|item| {
                    let manifest = &item["manifest"];
                    let package = &item["packageRef"];
                    let (available, reason) = match &item["availability"] {
                        serde_json::Value::String(state) => (state == "Available", None),
                        serde_json::Value::Object(map) => (false, map.get("Unavailable").cloned()),
                        _ => (false, None),
                    };
                    Some(PluginEntry {
                        id: manifest["id"].as_str()?.to_string(),
                        version: manifest["version"].as_str().unwrap_or("?").to_string(),
                        display_name: manifest["displayName"].as_str().unwrap_or("?").to_string(),
                        digest: package["contentDigest"].as_str().unwrap_or("?").to_string(),
                        enabled: item["enabled"].as_bool().unwrap_or(false),
                        available,
                        unavailable_reason: reason.map(|value| match value {
                            serde_json::Value::String(reason) => short_reason(&reason),
                            other => other.to_string(),
                        }),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn short_reason(reason: &str) -> String {
    // Rust 枚举变体名（Disabled / IncompatibleApi{..} / MissingEntrypoint{..}）
    // 渲染为紧凑中文短语；未知原因保留原文。
    match reason {
        "Disabled" => "已停用".into(),
        other if other.starts_with("IncompatibleApi") => "协议版本不兼容".into(),
        other if other.starts_with("MissingEntrypoint") => "缺少平台入口".into(),
        other => other.to_string(),
    }
}

/// r-code-service 二进制定位：R_CODE_SERVICE_BIN 显式优先，其次 exe 旁
///（打包布局）。T35 起 engine.rs 的 V2ChatClient 复用同一解析。
pub fn default_service_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("R_CODE_SERVICE_BIN") {
        return Some(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        let exe_name = if cfg!(windows) {
            "r-code-service.exe"
        } else {
            "r-code-service"
        };
        let beside = exe.parent()?.join(exe_name);
        if beside.is_file() {
            return Some(beside);
        }
    }
    None
}
