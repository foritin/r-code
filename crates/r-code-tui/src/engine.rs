//! V2ChatClient：TUI 聊天/会话引擎经共享 r-code-service 守护进程（T35）。
//!
//! 与桌面 GUI 同一套 v2 Harness 协议（`r_code_client::DaemonClient`）：
//! task.create/sendMessage/cancel/list/detail/rename/setPreferences/clone/
//! branches、models.available、settings.*、codex.*。连接方式与
//! [`crate::harness_client::HarnessTuiClient`] 相同（ensure_daemon 自举 +
//! token 握手），每个操作一条短连接——守护进程是唯一有状态方。
//!
//! 事件源是 `task.events`（EventEnvelope，`payload.journalKind` 判别）。
//! 事件→TuiState 投影见 [`project_event`]：用户行由发送方本地追加
//! （`input.queued` 不投），assistant 整轮文本落 Assistant 行，工具观测落
//! ToolCard，run 生命周期驱动 running 态与失败引导。

use r_code_client::DaemonClient;
use r_code_harness_protocol::EventEnvelope;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

/// task.list 的一行（守护进程 TaskSummaryView 的本地视图）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task_id: String,
    pub title: String,
    pub kind: String,
    pub state: String,
    pub running: bool,
    pub updated_at_ms: i64,
}

/// task.detail 中的一次 run。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRun {
    pub run_id: String,
    pub outcome: String,
}

/// task.detail 的聚合用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TaskUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// task.detail 的本地视图（TaskDetailView）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub task_id: String,
    pub title: String,
    pub kind: String,
    pub objective: String,
    pub state: String,
    pub running: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub inference: Option<serde_json::Value>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub runs: Vec<TaskRun>,
    #[serde(default)]
    pub usage: TaskUsage,
}

/// models.available 的一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRow {
    pub selection: String,
    pub model: String,
    pub has_credential: bool,
    pub is_default: bool,
}

/// task.branches 的一行（分支谱系：子 → 父）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchRow {
    pub task_id: String,
    #[serde(default)]
    pub parent_task_id: Option<String>,
}

/// 一次发送的结果（started = 新 run；queued = 运行中排队）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Started { run_id: String },
    Queued,
}

/// v2 客户端桥（TUI 进程一个；克隆共享 profile/游标）。
#[derive(Clone)]
pub struct V2ChatClient {
    profile: RuntimeProfile,
    service_binary: Option<PathBuf>,
    /// task.events 轮询游标（只有事件泵推进；print 模式复用）。
    cursor: std::sync::Arc<Mutex<u64>>,
}

impl V2ChatClient {
    /// 按 TUI 的 --profile/--data-dir/--ipc-name 解析（显式 flavor，绝不推断）。
    pub fn from_args(
        flavor: ProfileFlavor,
        data_root: Option<PathBuf>,
        ipc_name: Option<String>,
    ) -> Result<Self, String> {
        let mut options = LaunchOptions::new(flavor);
        if let Some(root) = data_root {
            options = options.with_data_root(root);
        }
        if let Some(name) = ipc_name {
            options = options.with_ipc_name(name);
        }
        let profile = RuntimeProfile::resolve(&options).map_err(|e| e.to_string())?;
        Ok(Self::from_profile(profile, None))
    }

    /// 测试/装配用：显式 profile + service 二进制。
    pub fn from_profile(profile: RuntimeProfile, service_binary: Option<PathBuf>) -> Self {
        Self {
            profile,
            service_binary: service_binary.or_else(crate::harness_client::default_service_binary),
            cursor: std::sync::Arc::new(Mutex::new(0)),
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

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut client = self.connect().await?;
        client.call(method, params).await.map_err(|e| e.to_string())
    }

    /// RA3：daemon 审批决策（`approvals.decide`；审计身份=本连接 client
    /// id，服务端强制）。approve=false 落 denied。
    pub async fn decide_approval(&self, op_id: &str, approve: bool) -> Result<(), String> {
        self.call(
            "approvals.decide",
            serde_json::json!({
                "operationId": op_id,
                "decision": if approve { "granted" } else { "denied" },
            }),
        )
        .await
        .map(|_| ())
    }

    pub fn profile(&self) -> &RuntimeProfile {
        &self.profile
    }

    // -- 会话生命周期 -------------------------------------------------------

    /// 新建会话任务（conversation）并命名；返回 task id。
    pub async fn create_session(&self, title: &str) -> Result<String, String> {
        let value = self
            .call(
                "task.create",
                serde_json::json!({"objective": "", "kind": "conversation"}),
            )
            .await?;
        let task_id = value["taskId"].as_str().ok_or("task.create 缺 taskId")?;
        let task_id = task_id.to_string();
        self.rename_task(&task_id, title).await?;
        Ok(task_id)
    }

    /// 启动会话（`ensure_session("tui")` 语义）：建任务即返回，发送走 send。
    pub async fn ensure_session(&self, title: &str) -> Result<String, String> {
        self.create_session(title).await
    }

    pub async fn rename_task(&self, task_id: &str, title: &str) -> Result<(), String> {
        self.call(
            "task.rename",
            serde_json::json!({"taskId": task_id, "title": title}),
        )
        .await
        .map(|_| ())
    }

    /// 发送用户消息：空闲 = 启动新 run；运行中 = 排队（run 结束自动派发）。
    pub async fn send(&self, task_id: &str, text: &str) -> Result<SendOutcome, String> {
        let value = self
            .call(
                "task.sendMessage",
                serde_json::json!({"taskId": task_id, "text": text}),
            )
            .await?;
        if value["queued"].as_bool() == Some(true) {
            return Ok(SendOutcome::Queued);
        }
        let run_id = value["runId"].as_str().unwrap_or_default().to_string();
        Ok(SendOutcome::Started { run_id })
    }

    /// 中止当前 run（空闲时守护进程 no-op）。
    pub async fn abort(&self, task_id: &str) -> Result<bool, String> {
        let value = self
            .call("task.cancel", serde_json::json!({"taskId": task_id}))
            .await?;
        Ok(value["cancelled"].as_bool().unwrap_or(false))
    }

    pub async fn list_sessions(&self) -> Result<Vec<TaskSummary>, String> {
        let value = self.call("task.list", serde_json::json!({})).await?;
        serde_json::from_value(value).map_err(|e| format!("task.list 解析失败：{e}"))
    }

    pub async fn task_detail(&self, task_id: &str) -> Result<TaskDetail, String> {
        let value = self
            .call("task.detail", serde_json::json!({"taskId": task_id}))
            .await?;
        serde_json::from_value(value).map_err(|e| format!("task.detail 解析失败：{e}"))
    }

    /// 克隆会话（v2 分支任务：契约与引擎固定继承，对话状态按新分支重开）。
    pub async fn clone_task(&self, source_task_id: &str, title: &str) -> Result<String, String> {
        let new_task_id = format!("task-{}", uuid_v4_simple());
        let value = self
            .call(
                "task.clone",
                serde_json::json!({
                    "sourceTaskId": source_task_id,
                    "newTaskId": new_task_id,
                    "title": title,
                }),
            )
            .await?;
        Ok(value["taskId"].as_str().unwrap_or(&new_task_id).to_string())
    }

    pub async fn branches(&self) -> Result<Vec<BranchRow>, String> {
        let value = self.call("task.branches", serde_json::json!({})).await?;
        serde_json::from_value(value).map_err(|e| format!("task.branches 解析失败：{e}"))
    }

    // -- 偏好（模型/思考/模式；影响下一次 run）------------------------------

    pub async fn set_preferences(
        &self,
        task_id: &str,
        model: Option<&str>,
        inference: Option<serde_json::Value>,
        mode: Option<&str>,
    ) -> Result<(), String> {
        let mut params = serde_json::json!({"taskId": task_id});
        if let Some(model) = model {
            params["model"] = serde_json::json!(model);
        }
        if let Some(inference) = inference {
            params["inference"] = inference;
        }
        if let Some(mode) = mode {
            params["mode"] = serde_json::json!(mode);
        }
        self.call("task.setPreferences", params).await.map(|_| ())
    }

    pub async fn models_available(&self) -> Result<Vec<ModelRow>, String> {
        let value = self.call("models.available", serde_json::json!({})).await?;
        serde_json::from_value(value).map_err(|e| format!("models.available 解析失败：{e}"))
    }

    pub async fn set_default_model(&self, selection: &str) -> Result<(), String> {
        self.call(
            "settings.setDefault",
            serde_json::json!({"selection": selection}),
        )
        .await
        .map(|_| ())
    }

    /// /setup 保存：settings.apply（apiKey 或 envVar 二选一；后者零凭据落盘）。
    pub async fn settings_apply(
        &self,
        selection: &str,
        model: &str,
        api_key: Option<&str>,
        env_var: Option<&str>,
    ) -> Result<Vec<ModelRow>, String> {
        let mut params = serde_json::json!({"selection": selection, "model": model});
        if let Some(key) = api_key {
            params["apiKey"] = serde_json::json!(key);
        }
        if let Some(var) = env_var {
            params["envVar"] = serde_json::json!(var);
        }
        let value = self.call("settings.apply", params).await?;
        serde_json::from_value(value).map_err(|e| format!("settings.apply 解析失败：{e}"))
    }

    // -- 登录（Codex 委托）---------------------------------------------------

    pub async fn codex_status(&self) -> Result<serde_json::Value, String> {
        self.call("codex.status", serde_json::json!({})).await
    }

    pub async fn codex_start_login(&self, mode: &str) -> Result<(), String> {
        self.call(
            "codex.startLogin",
            serde_json::json!({"mode": if mode == "device" { "device" } else { "browser" }}),
        )
        .await
        .map(|_| ())
    }

    // -- 事件 ----------------------------------------------------------------

    /// 拉取游标之后的新事件（游标推进到本次见到的最大 seq）。
    pub async fn poll_events(&self) -> Result<Vec<EventEnvelope>, String> {
        let after = *self.cursor.lock().expect("event cursor");
        let value = self
            .call(
                "task.events",
                serde_json::json!({"afterSeq": after, "limit": 500}),
            )
            .await?;
        let events: Vec<EventEnvelope> =
            serde_json::from_value(value).map_err(|e| format!("task.events 解析失败：{e}"))?;
        if let Some(max) = events.iter().map(|event| event.seq).max() {
            *self.cursor.lock().expect("event cursor") = max;
        }
        Ok(events)
    }

    /// 重置游标并取全量事件（/resume 重建 transcript 用）。
    pub async fn events_from_start(&self) -> Result<Vec<EventEnvelope>, String> {
        *self.cursor.lock().expect("event cursor") = 0;
        let mut all = Vec::new();
        loop {
            let batch = self.poll_events().await?;
            let taken = batch.len();
            all.extend(batch);
            if taken < 500 {
                break;
            }
        }
        Ok(all)
    }

    /// 显式停掉守护进程（测试清理用；正常退出留给 daemon 常驻）。
    pub async fn shutdown_service(&self) -> Result<(), String> {
        let mut client = self.connect().await?;
        client
            .call("service.shutdown", serde_json::json!({}))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// 轻量 uuid（不引入 uuid 直接依赖：借 r_code_client 侧同一格式）。
fn uuid_v4_simple() -> String {
    // Daemon 在 task.create 省略 taskId 时自动生成；这里仅为 clone 的
    // newTaskId 提供唯一名——用时间戳 + 进程内计数足够（不跨进程碰撞：
    // 同一 TUI 进程串行 clone）。
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}-{count:x}")
}

/// EventEnvelope → TranscriptEvent 投影（/resume 重建与 /fork 消息枚举共用）。
///
/// journalKind 判别：input.queued → User（message_id 取 envelope 载荷），
/// assistant.message → Assistant，tool.call → ToolCall，tool.result →
/// ToolResult；run 生命周期事件不产 transcript 行（由 pump 直接驱动 TuiState
/// 运行态），其余（model.usage/harness.progress/...）忽略。
pub fn transcript_events_of(
    events: &[EventEnvelope],
    task_id: &str,
) -> Vec<crate::TranscriptEvent> {
    events
        .iter()
        .filter(|event| event.task_id == task_id)
        .filter_map(|event| {
            let kind = event.payload.get("journalKind")?.as_str()?;
            match kind {
                "input.queued" => Some(crate::TranscriptEvent::User {
                    message_id: event
                        .payload
                        .get("message_id")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    text: event
                        .payload
                        .get("text")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                }),
                "assistant.message" => Some(crate::TranscriptEvent::Assistant {
                    run_id: event.run_id.clone(),
                    text: event
                        .payload
                        .get("text")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                }),
                "tool.call" => Some(crate::TranscriptEvent::ToolCall {
                    run_id: event.run_id.clone(),
                    name: event
                        .payload
                        .get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    input: event.payload.get("input").cloned().unwrap_or_default(),
                }),
                "tool.result" => Some(crate::TranscriptEvent::ToolResult {
                    run_id: event.run_id.clone(),
                    name: event
                        .payload
                        .get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    ok: event
                        .payload
                        .get("ok")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(true),
                    output: event.payload.get("output").cloned().unwrap_or_default(),
                }),
                _ => None,
            }
        })
        .collect()
}

/// 把一批新事件投进 TuiState（事件泵路径：250ms 轮询后调用）。
///
/// - 按当前任务过滤（旧任务尾随事件不得混入切换后的 transcript）；
/// - input.queued 不投（用户行由发送方本地加）；assistant/tool 走
///   `apply_transcript_event`；
/// - run.started → 运行态 + 清队列镜像；run.completed/cancelled → 收敛；
/// - run.failed → 收敛 + "发送失败"系统行（未配置模型类错误映射 /setup 引导）。
pub fn project_events(state: &mut crate::TuiState, events: &[EventEnvelope]) {
    let current = state.task_id().to_string();
    for event in events {
        if !current.is_empty() && event.task_id != current {
            continue;
        }
        let Some(kind) = event.payload.get("journalKind").and_then(|v| v.as_str()) else {
            continue;
        };
        match kind {
            "input.queued" => {}
            "assistant.message" => {
                let text = event
                    .payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                state.apply_transcript_event(&crate::TranscriptEvent::Assistant {
                    run_id: event.run_id.clone(),
                    text: text.to_string(),
                });
            }
            "tool.call" => {
                state.apply_transcript_event(&crate::TranscriptEvent::ToolCall {
                    run_id: event.run_id.clone(),
                    name: event
                        .payload
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    input: event.payload.get("input").cloned().unwrap_or_default(),
                });
            }
            "tool.result" => {
                let ok = event
                    .payload
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                state.apply_transcript_event(&crate::TranscriptEvent::ToolResult {
                    run_id: event.run_id.clone(),
                    name: event
                        .payload
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    ok,
                    output: event.payload.get("output").cloned().unwrap_or_default(),
                });
            }
            "run.started" => {
                state.mark_running(true);
            }
            "approval.requested" => {
                // RA3：daemon 审批（RA1 持久化通道）驱动浮层——本地
                // PermissionEngine 流不产生该事件，两条路径不冲突。
                state.set_pending_approval(
                    crate::approval_overlay::PendingApproval::from_requested_event(&event.payload),
                );
            }
            "approval.decided" => {
                // 决策（或超时拒绝）落地：浮层消失，超时给用户可见注记。
                if state.take_pending_approval().is_some()
                    && event.payload.get("decidedBy").and_then(|v| v.as_str()) == Some("<timeout>")
                {
                    state.push_system("审批已超时自动拒绝".to_string());
                }
            }
            "run.completed" | "run.cancelled" => {
                state.mark_running(false);
            }
            "run.failed" => {
                state.mark_running(false);
                let error = event
                    .payload
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知错误");
                state.push_system(format!("发送失败：{}", failure_guidance(error)));
            }
            _ => {}
        }
    }
}

/// run.failed 错误文案 → 用户可操作引导（无 provider 是首版最常见的失败）。
pub fn failure_guidance(error: &str) -> String {
    if error.contains("unknown model selection") || error.contains("registry") {
        "未配置模型服务——输入 /setup 完成配置（Tab 可切环境变量鉴权）".to_string()
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(seq: u64, task: &str, _kind: &str, payload: serde_json::Value) -> EventEnvelope {
        EventEnvelope {
            seq,
            task_id: task.to_string(),
            run_id: format!("run-{task}"),
            kind: r_code_harness_protocol::EventKind::Progress,
            source: r_code_harness_protocol::Provenance::Host,
            payload,
        }
    }

    fn with_kind(kind: &str, mut payload: serde_json::Value) -> serde_json::Value {
        payload["journalKind"] = serde_json::json!(kind);
        payload
    }

    /// 投影：user 行不投、assistant 落行、tool 卡与错误回写、run 态收敛。
    #[test]
    fn project_events_maps_journal_kinds() {
        let mut state = crate::TuiState::new();
        state.set_task_id("t1");
        let events = vec![
            envelope(
                1,
                "t1",
                "input.queued",
                with_kind(
                    "input.queued",
                    serde_json::json!({"message_id": "m1", "text": "你好"}),
                ),
            ),
            envelope(
                2,
                "t1",
                "run.started",
                with_kind("run.started", serde_json::json!({"runId": "r1"})),
            ),
            envelope(
                3,
                "t1",
                "assistant.message",
                with_kind(
                    "assistant.message",
                    serde_json::json!({"runId": "r1", "text": "回答全文"}),
                ),
            ),
            envelope(
                4,
                "t1",
                "tool.call",
                with_kind(
                    "tool.call",
                    serde_json::json!({"runId": "r1", "name": "bash", "input": "cargo test"}),
                ),
            ),
            envelope(
                5,
                "t1",
                "tool.result",
                with_kind(
                    "tool.result",
                    serde_json::json!({"runId": "r1", "name": "bash", "ok": false, "output": "boom"}),
                ),
            ),
            envelope(
                6,
                "t1",
                "run.completed",
                with_kind(
                    "run.completed",
                    serde_json::json!({"runId": "r1", "verdict": "unverified"}),
                ),
            ),
            // 旧任务事件：过滤。
            envelope(
                7,
                "old",
                "assistant.message",
                with_kind(
                    "assistant.message",
                    serde_json::json!({"text": "旧任务残留"}),
                ),
            ),
        ];
        project_events(&mut state, &events);
        assert!(!state.is_running(), "run.completed 后收敛");
        let rows = state.rows();
        assert_eq!(rows.len(), 2, "user 不投 + assistant + tool：{rows:?}");
        assert!(
            matches!(&rows[0], crate::TranscriptRow::Assistant { text, complete }
            if text == "回答全文" && *complete)
        );
        assert!(
            matches!(&rows[1], crate::TranscriptRow::ToolCard { name, is_error, .. }
                if name == "bash" && *is_error),
            "tool_result 错误位回写：{rows:?}"
        );
    }

    /// run.failed → 发送失败系统行 + /setup 引导（未配置 provider）。
    #[test]
    fn project_events_maps_run_failure_to_setup_guidance() {
        let mut state = crate::TuiState::new();
        state.set_task_id("t");
        state.mark_running(true);
        let events = vec![envelope(
            2,
            "t",
            "run.failed",
            with_kind(
                "run.failed",
                serde_json::json!({"error": "model failure: unknown model selection \"\""}),
            ),
        )];
        project_events(&mut state, &events);
        assert!(!state.is_running());
        let rendered = state
            .rows()
            .iter()
            .filter_map(|row| match row {
                crate::TranscriptRow::System { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(rendered.contains("发送失败"), "{rendered}");
        assert!(
            rendered.contains("/setup"),
            "未配置模型映射引导：{rendered}"
        );
    }

    /// transcript_events_of：/resume 重建投影（含 User）+ 任务过滤。
    #[test]
    fn transcript_events_of_projects_user_rows_for_rebuild() {
        let events = vec![
            envelope(
                1,
                "t1",
                "input.queued",
                with_kind(
                    "input.queued",
                    serde_json::json!({"message_id": "m1", "text": "第一条"}),
                ),
            ),
            envelope(
                2,
                "t1",
                "assistant.message",
                with_kind("assistant.message", serde_json::json!({"text": "答"})),
            ),
            envelope(
                3,
                "other",
                "input.queued",
                with_kind(
                    "input.queued",
                    serde_json::json!({"message_id": "m2", "text": "别家"}),
                ),
            ),
        ];
        let projected = transcript_events_of(&events, "t1");
        assert_eq!(projected.len(), 2);
        assert!(
            matches!(&projected[0], crate::TranscriptEvent::User { message_id, text }
            if message_id == "m1" && text == "第一条")
        );
        assert!(
            matches!(&projected[1], crate::TranscriptEvent::Assistant { text, .. } if text == "答")
        );
    }

    /// RA3：daemon 审批事件投影——requested 立浮层，decided 收起，
    /// 超时拒绝给用户可见注记；决策来源标记 daemon（op_id）。
    #[test]
    fn daemon_approval_events_drive_the_overlay() {
        let mut state = crate::TuiState::new();
        state.set_task_id("t1");
        assert!(state.pending_approval().is_none());

        project_events(
            &mut state,
            &[envelope(
                5,
                "t1",
                "approval.requested",
                with_kind(
                    "approval.requested",
                    serde_json::json!({
                        "opId": "op-9",
                        "summary": "run tests",
                        "runId": "run-t1-1",
                    }),
                ),
            )],
        );
        let pending = state.pending_approval().expect("overlay raised").clone();
        assert!(pending.is_daemon());
        assert_eq!(pending.op_id.as_deref(), Some("op-9"));
        assert_eq!(pending.command, "run tests");

        // Client decision: overlay collapses without a timeout note.
        project_events(
            &mut state,
            &[envelope(
                6,
                "t1",
                "approval.decided",
                with_kind(
                    "approval.decided",
                    serde_json::json!({
                        "opId": "op-9",
                        "decision": "granted",
                        "decidedBy": "r-code-tui",
                    }),
                ),
            )],
        );
        assert!(state.pending_approval().is_none());

        // Timeout denial surfaces the auto-deny note.
        project_events(
            &mut state,
            &[envelope(
                7,
                "t1",
                "approval.requested",
                with_kind(
                    "approval.requested",
                    serde_json::json!({"opId": "op-10", "summary": "deploy"}),
                ),
            )],
        );
        assert!(state.pending_approval().is_some());
        project_events(
            &mut state,
            &[envelope(
                8,
                "t1",
                "approval.decided",
                with_kind(
                    "approval.decided",
                    serde_json::json!({
                        "opId": "op-10",
                        "decision": "denied",
                        "decidedBy": "<timeout>",
                    }),
                ),
            )],
        );
        assert!(state.pending_approval().is_none());
        assert!(
            state
                .rows()
                .iter()
                .any(|row| matches!(row, crate::TranscriptRow::System { text } if text.contains("审批已超时自动拒绝"))),
            "timeout denial is visible to the user"
        );
    }
}
