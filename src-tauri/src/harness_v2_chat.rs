//! GUI 聊天链路 → v2 daemon 投影层（T42 阶段 1）。
//!
//! 前端命令名与事件频道零改动：本模块把 v2 守护进程（r-code-service）的
//! `task.*` RPC 与 `task.events` 事件信封投影成旧 GUI 链路的 DTO 形状
//! （`core::dto::Task` / `commands::TaskDetail` / `SessionMessage`）与
//! `core::dto::AgentEvent`，经既有的 `agent-event` 频道广播。
//!
//! 诚实投影原则：v2 没有的概念（文件变更、权限请求、验证记录、持久排队
//! 视图、Plan 入口建议）一律返回诚实空值，不伪造数据；v2 事件信封不带
//! 时间戳，投影时间统一取读取时刻。旧实现（commands.rs 的 agent 段）
//! 原地保留，本期只挂新路径。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
// InferenceOptions 的公共路径在 agent-contract（core 的 dto.rs 私有引用）。
use agent_contract::InferenceOptions;
// SessionMessage / TaskDetail / TaskDetailBatch 是宿主层 DTO（commands.rs），
// 前端 ipc.ts 的类型即由此序列化而来；投影层原样复用，保证形状逐字节一致。
use crate::commands::{SessionMessage, TaskDetail, TaskDetailBatch};
use r_code_core::dto::{
    AgentEngine, AgentEvent, AgentKind, AgentRun, AgentRunRuntimeKind, ReviewState, SessionBranch,
    SubagentAccessMode, Task, TaskEvent, TaskEventType, TaskMode, TaskState,
};
use r_code_core::{project_task_status, TaskStatusProjectionInput};
use r_code_runtime::{ProfileFlavor, RuntimeProfile};
use serde_json::{json, Value};

/// v2 会话分支（投影层固定值）：旧 GUI 的 main 分支标识。
const PROJECTED_BRANCH_ID: &str = "main";
/// 事件泵轮询间隔（毫秒）。
const EVENT_POLL_MS: u64 = 300;
/// 单次 `task.events` 拉取条数。
const EVENT_BATCH_LIMIT: u64 = 500;
/// 启动期快进（不 emit）的最大批次数，防止超大历史拖住启动。
const EVENT_SYNC_BATCH_CAP: usize = 200;

/// GUI 聊天链路的 v2 桥。复用 [`HarnessV2Bridge`](crate::harness_v2::HarnessV2Bridge)
/// 的 daemon 发现/拉起（`R_CODE_SERVICE_BIN` env → ensure_daemon →
/// DaemonClient::connect），只在其上叠加 v2 → 旧形状的投影。
/// 全部方法 `&self`（每次调用都经 daemon 连接），可安全共享。
pub struct ChatV2Bridge {
    inner: crate::harness_v2::HarnessV2Bridge,
}

/// Tauri 管理的共享桥状态。
pub type SharedChatV2Bridge = Arc<ChatV2Bridge>;

impl ChatV2Bridge {
    /// 按应用 flavor 解析 profile 并构造桥（生产/开发互不串台）。
    pub fn new(
        flavor: ProfileFlavor,
        data_root: Option<PathBuf>,
    ) -> Result<Self, crate::harness_v2::HarnessV2Error> {
        Ok(Self {
            inner: crate::harness_v2::HarnessV2Bridge::new(flavor, data_root)?,
        })
    }

    /// 从已解析 profile 构造（测试 / 自定义布局）。
    pub fn new_from_profile(profile: RuntimeProfile, service_binary: Option<PathBuf>) -> Self {
        Self {
            inner: crate::harness_v2::HarnessV2Bridge::new_from_profile(profile, service_binary),
        }
    }

    /// 当前应用进程的共享桥（与 harness_v2::shared_bridge 同一 profile 解析）。
    pub fn shared() -> Result<SharedChatV2Bridge, crate::harness_v2::HarnessV2Error> {
        Ok(Arc::new(Self::for_current_app()?))
    }

    /// 按当前应用 flavor 构造（非共享、按值持有；MCP server 等自持场景）。
    pub fn for_current_app() -> Result<Self, crate::harness_v2::HarnessV2Error> {
        let (flavor, data_root) = crate::app_paths::AppFlavor::current().harness_launch_options();
        let flavor = match flavor {
            "production" => ProfileFlavor::Production,
            _ => ProfileFlavor::Development,
        };
        Self::new(flavor, data_root)
    }

    pub fn profile(&self) -> &RuntimeProfile {
        self.inner.profile()
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, crate::harness_v2::HarnessV2Error> {
        let mut client = self.inner.connect().await?;
        client
            .call(method, params)
            .await
            .map_err(|e| crate::harness_v2::HarnessV2Error::Command(e.to_string()))
    }

    // -- 聊天命令实现（返回旧形状 DTO） ----------------------------------

    /// `cmd_task_create`：v2 `task.create`（kind=conversation，taskId 由
    /// daemon 生成）+ `task.rename`（用户标题）+ `task.setPreferences`
    /// （mode）。provider/agent 参数诚实忽略——v2 默认 pin 内置 native
    /// harness，模型选择走 v2 settings 的默认 selection。
    pub async fn task_create(
        &self,
        title: &str,
        goal: &str,
        mode: &str,
        _provider_name: Option<&str>,
        _agent_engine: Option<&str>,
    ) -> Result<Task, crate::harness_v2::HarnessV2Error> {
        let mode = TaskMode::try_from_str(mode.trim()).ok_or_else(|| {
            crate::harness_v2::HarnessV2Error::Command(format!("invalid task mode: {mode}"))
        })?;
        let objective = if goal.trim().is_empty() { title } else { goal };
        let mut client = self.inner.connect().await?;
        let created = client
            .call(
                "task.create",
                json!({"objective": objective, "kind": "conversation"}),
            )
            .await
            .map_err(|e| crate::harness_v2::HarnessV2Error::Command(e.to_string()))?;
        let task_id = created["taskId"].as_str().unwrap_or_default().to_string();
        if task_id.is_empty() {
            return Err(crate::harness_v2::HarnessV2Error::Command(
                "task.create returned no taskId".into(),
            ));
        }
        // v2 默认标题截自 objective；用户显式标题一经给出即覆盖。
        if !title.trim().is_empty() && title != objective {
            client
                .call("task.rename", json!({"taskId": task_id, "title": title}))
                .await
                .map_err(|e| crate::harness_v2::HarnessV2Error::Command(e.to_string()))?;
        }
        client
            .call(
                "task.setPreferences",
                json!({"taskId": task_id, "mode": mode.to_string()}),
            )
            .await
            .map_err(|e| crate::harness_v2::HarnessV2Error::Command(e.to_string()))?;
        let now = Utc::now();
        Ok(project_task(
            &task_id,
            title,
            objective,
            TaskState::Idle,
            mode,
            None,
            None,
            now,
            now,
        ))
    }

    /// `cmd_task_list`：v2 `task.list` → 旧 `Vec<Task>`。
    /// v2 任务不绑定工作区、无归档概念：workspace 过滤与 include_archived
    /// 诚实忽略（返回全部任务）。
    pub async fn task_list(
        &self,
        _workspace_path: Option<&str>,
        _include_archived: bool,
    ) -> Result<Vec<Task>, crate::harness_v2::HarnessV2Error> {
        let value = self.call("task.list", json!({})).await?;
        let rows = value.as_array().cloned().unwrap_or_default();
        Ok(rows
            .iter()
            .map(|row| {
                let updated_at = row["updated_at_ms"]
                    .as_i64()
                    .and_then(DateTime::from_timestamp_millis)
                    .unwrap_or_else(Utc::now);
                let state = v2_state_to_task_state(
                    row["state"].as_str().unwrap_or("pending"),
                    row["running"].as_bool().unwrap_or(false),
                );
                project_task(
                    row["task_id"].as_str().unwrap_or_default(),
                    row["title"].as_str().unwrap_or("新会话"),
                    // 列表投影不含 objective；goal 由 task_detail 补全。
                    "",
                    state,
                    TaskMode::Ask,
                    None,
                    None,
                    updated_at,
                    updated_at,
                )
            })
            .collect())
    }

    /// `cmd_task_detail`：v2 `task.detail` + `task.events` → 11 字段旧形状。
    pub async fn task_detail(
        &self,
        task_id: &str,
    ) -> Result<TaskDetail, crate::harness_v2::HarnessV2Error> {
        let mut client = self.inner.connect().await?;
        let detail = client
            .call("task.detail", json!({"taskId": task_id}))
            .await
            .map_err(|e| crate::harness_v2::HarnessV2Error::Command(e.to_string()))?;
        let events = read_all_task_events(&mut client, EVENT_SYNC_BATCH_CAP).await?;
        Ok(project_task_detail(task_id, &detail, &events))
    }

    /// `cmd_task_detail_batch`：逐个 detail 聚合（v2 无批量端点）。
    pub async fn task_detail_batch(
        &self,
        task_ids: &[String],
    ) -> Result<TaskDetailBatch, crate::harness_v2::HarnessV2Error> {
        let mut details = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            details.push(self.task_detail(task_id).await?);
        }
        Ok(TaskDetailBatch { details })
    }

    /// `cmd_agent_send`：v2 `task.sendMessage`。v2 的 send 自带排队语义
    /// （忙碌时入队、空闲时开新 run），`queued` 响应同样视为成功。
    pub async fn agent_send(
        &self,
        task_id: &str,
        text: &str,
    ) -> Result<(), crate::harness_v2::HarnessV2Error> {
        self.call("task.sendMessage", json!({"taskId": task_id, "text": text}))
            .await?;
        Ok(())
    }

    /// `cmd_agent_abort`：v2 `task.cancel`。
    pub async fn agent_abort(
        &self,
        task_id: &str,
    ) -> Result<(), crate::harness_v2::HarnessV2Error> {
        self.call("task.cancel", json!({"taskId": task_id})).await?;
        Ok(())
    }

    /// `cmd_session_messages`：v2 事件 journal → 旧 `Vec<SessionMessage>`。
    /// `input.queued` → 用户消息；`assistant.message` → 助手消息；
    /// `tool.call` / `tool.result` → 工具条目。
    pub async fn session_messages(
        &self,
        task_id: &str,
    ) -> Result<Vec<SessionMessage>, crate::harness_v2::HarnessV2Error> {
        let mut client = self.inner.connect().await?;
        let events = read_all_task_events(&mut client, EVENT_SYNC_BATCH_CAP).await?;
        Ok(project_session_messages(task_id, &events))
    }

    /// `cmd_task_rename`：v2 `task.rename` 后回读详情，返回旧 `Task`。
    pub async fn task_rename(
        &self,
        task_id: &str,
        title: &str,
    ) -> Result<Task, crate::harness_v2::HarnessV2Error> {
        self.call("task.rename", json!({"taskId": task_id, "title": title}))
            .await?;
        Ok(self.task_detail(task_id).await?.task)
    }

    /// `task_clone`：v2 `task.clone`（source → 新 taskId），标题追加
    /// “（克隆）”。
    pub async fn task_clone(
        &self,
        task_id: &str,
    ) -> Result<Task, crate::harness_v2::HarnessV2Error> {
        let mut client = self.inner.connect().await?;
        let source = client
            .call("task.detail", json!({"taskId": task_id}))
            .await
            .map_err(|e| crate::harness_v2::HarnessV2Error::Command(e.to_string()))?;
        let new_id = format!("task-{}", uuid::Uuid::new_v4().simple());
        let title = format!("{}（克隆）", source["title"].as_str().unwrap_or("新会话"));
        client
            .call(
                "task.clone",
                json!({"sourceTaskId": task_id, "newTaskId": new_id, "title": title}),
            )
            .await
            .map_err(|e| crate::harness_v2::HarnessV2Error::Command(e.to_string()))?;
        drop(client);
        Ok(self.task_detail(&new_id).await?.task)
    }
}

// ---------------------------------------------------------------------------
// v2 → 旧形状投影（纯函数，单测钉形状）
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn project_task(
    id: &str,
    title: &str,
    goal: &str,
    state: TaskState,
    mode: TaskMode,
    model: Option<&str>,
    inference: Option<&InferenceOptions>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
) -> Task {
    Task {
        id: id.to_string(),
        // v2 任务不绑定工作区：诚实空。
        workspace_path: None,
        provider_name: None,
        model: model.map(str::to_string),
        inference: inference.cloned().unwrap_or_default(),
        agent_engine: AgentEngine::RCode,
        title: title.to_string(),
        goal: goal.to_string(),
        goal_active: false,
        mode,
        state,
        worktree_path: None,
        created_at,
        updated_at,
    }
}

/// v2 状态字符串 + running 观测 → 旧 `TaskState`。
/// pending→Idle、running/waiting-input→InProgress、review-ready→ReviewReady、
/// terminal 各 verdict→对应（failed/blocked→Interrupted，其余→Idle）。
fn v2_state_to_task_state(state: &str, running: bool) -> TaskState {
    if running {
        return TaskState::InProgress;
    }
    match state {
        "running" | "waiting-input" => TaskState::InProgress,
        "review-ready" => TaskState::ReviewReady,
        "failed" | "blocked" => TaskState::Interrupted,
        // "pending" / "verified" / "unverified" / "cancelled" 及未知值。
        _ => TaskState::Idle,
    }
}

/// v2 run 终态 verdict → 旧 `ReviewState`。
fn verdict_to_review_state(verdict: &str) -> ReviewState {
    match verdict {
        "verified" => ReviewState::AutoAccepted,
        "cancelled" => ReviewState::Aborted,
        "failed" | "blocked" => ReviewState::Failed,
        // "unverified"：对话任务无文件变更，等价 Ask 模式零变化结算。
        _ => ReviewState::Answered,
    }
}

fn new_projected_run(
    task_id: &str,
    run_id: &str,
    model: &str,
    started_at: DateTime<Utc>,
) -> AgentRun {
    AgentRun {
        id: run_id.to_string(),
        task_id: task_id.to_string(),
        branch_id: PROJECTED_BRANCH_ID.to_string(),
        parent_run_id: None,
        agent_kind: AgentKind::Main,
        agent_label: None,
        summary: None,
        delegated_by_tool_call_id: None,
        model: model.to_string(),
        runtime_kind: AgentRunRuntimeKind::Native,
        access_mode: SubagentAccessMode::ReadOnly,
        require_approval: false,
        routing_reason: None,
        external_session_id: None,
        review_state: ReviewState::Pending,
        started_at,
        ended_at: None,
        usage_json: None,
        guard_trip: None,
        checkpoint_sha: None,
        checkpoint_base_head: None,
    }
}

/// journal kind → 旧 `TaskEventType`（尽力映射；无对应概念归 System）。
fn journal_kind_to_event_type(kind: &str) -> TaskEventType {
    match kind {
        "task.created" => TaskEventType::TaskCreated,
        "run.started" => TaskEventType::RunStarted,
        "run.completed" | "run.failed" | "run.cancelled" => TaskEventType::RunEnded,
        "input.queued" => TaskEventType::UserMessageQueued,
        "input.delivered" => TaskEventType::QueueDispatched,
        "tool.call" => TaskEventType::ToolCall,
        "tool.result" => TaskEventType::ToolResult,
        "task.renamed" | "task.preferences" | "task.reopened" => TaskEventType::StateChanged,
        "branch.created" => TaskEventType::SessionBranched,
        // assistant.message / model.usage / harness.* / 未知。
        _ => TaskEventType::System,
    }
}

/// v2 工具输入/输出的预览字符串 → 旧 `AgentEvent` 的 `Value` 形状：
/// 能解析成 JSON 就还原，否则保持字符串原样。
fn preview_to_value(value: &Value) -> Value {
    match value.as_str() {
        Some(text) => {
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
        }
        None => value.clone(),
    }
}

/// 工具调用的事件内稳定 call_id：`{runId}:{name}:{seq}`（tool.call 与
/// tool.result 用同一规则互相关联）。
fn projected_call_id(run_id: &str, name: &str, seq: u64) -> String {
    format!("{run_id}:{name}:{seq}")
}

/// 把单条 v2 事件信封投影成旧 `AgentEvent`（前端 agent-event 频道载荷）。
/// 返回 `None` 表示该事件在旧形状里没有对应物（跳过）。
fn project_agent_event(envelope: &Value) -> Option<(String, AgentEvent)> {
    let task_id = envelope["task_id"].as_str()?.to_string();
    let payload = &envelope["payload"];
    let journal_kind = payload["journalKind"].as_str()?;
    let run_id = payload["runId"]
        .as_str()
        .or_else(|| envelope["run_id"].as_str())
        .unwrap_or("run");
    let seq = envelope["seq"].as_u64().unwrap_or(0);
    let event = match journal_kind {
        "assistant.message" => AgentEvent::Message {
            text: payload["text"].as_str().unwrap_or_default().to_string(),
            delta: false,
        },
        "tool.call" => {
            let name = payload["name"].as_str().unwrap_or_default().to_string();
            AgentEvent::ToolCall {
                input: preview_to_value(&payload["input"]),
                call_id: projected_call_id(run_id, &name, seq),
                name,
            }
        }
        "tool.result" => {
            let name = payload["name"].as_str().unwrap_or_default().to_string();
            AgentEvent::ToolResult {
                call_id: projected_call_id(run_id, &name, seq),
                output: preview_to_value(&payload["output"]),
                is_error: !payload["ok"].as_bool().unwrap_or(true),
            }
        }
        "run.started" => AgentEvent::State {
            state: TaskState::InProgress,
        },
        // 对话任务完成即回到 Idle（旧 GUI 的聊天语义）；review-ready 的
        // 显式阶段走 task_detail 的状态投影。
        "run.completed" | "run.cancelled" => AgentEvent::State {
            state: TaskState::Idle,
        },
        "run.failed" => AgentEvent::State {
            state: TaskState::Interrupted,
        },
        "model.usage" => AgentEvent::Usage {
            usage_json: serde_json::to_string(&payload["usage"]).unwrap_or_default(),
        },
        // input.queued / task.renamed / harness.pinned / harness.progress /
        // task.created / task.preferences / input.delivered：旧频道无对应物。
        _ => return None,
    };
    Some((task_id, event))
}

/// v2 task.detail + 全量事件 → 旧 11 字段 `TaskDetail`。
fn project_task_detail(task_id: &str, detail: &Value, events: &[Value]) -> TaskDetail {
    let now = Utc::now();
    let title = detail["title"].as_str().unwrap_or("新会话").to_string();
    let objective = detail["objective"].as_str().unwrap_or_default().to_string();
    let model = detail["model"].as_str().map(str::to_string);
    let inference: Option<InferenceOptions> = detail["inference"]
        .as_object()
        .and_then(|_| serde_json::from_value(detail["inference"].clone()).ok());
    let mode = detail["mode"]
        .as_str()
        .and_then(TaskMode::try_from_str)
        .unwrap_or(TaskMode::Ask);
    let running = detail["running"].as_bool().unwrap_or(false);
    let state = v2_state_to_task_state(detail["state"].as_str().unwrap_or("pending"), running);
    let task = project_task(
        task_id,
        &title,
        &objective,
        state,
        mode,
        model.as_deref(),
        inference.as_ref(),
        now,
        now,
    );

    // 事件回放：重建 runs（started/completed/failed）与逐 run usage 聚合。
    let mut runs: Vec<AgentRun> = Vec::new();
    let mut usage_input: Vec<u64> = Vec::new();
    let mut usage_output: Vec<u64> = Vec::new();
    for envelope in events {
        if envelope["task_id"].as_str() != Some(task_id) {
            continue;
        }
        let payload = &envelope["payload"];
        let journal_kind = payload["journalKind"].as_str().unwrap_or_default();
        let run_id = payload["runId"]
            .as_str()
            .or_else(|| envelope["run_id"].as_str())
            .unwrap_or_default()
            .to_string();
        match journal_kind {
            "run.started" if !run_id.is_empty() => {
                let run = new_projected_run(
                    task_id,
                    &run_id,
                    model.as_deref().unwrap_or("v2-harness"),
                    now,
                );
                usage_input.push(0);
                usage_output.push(0);
                runs.push(run);
            }
            "run.completed" | "run.cancelled" => {
                if let Some(run) = latest_open_run_mut(&mut runs, &run_id) {
                    run.ended_at = Some(now);
                    run.review_state = verdict_to_review_state(
                        payload["verdict"].as_str().unwrap_or("unverified"),
                    );
                }
            }
            "run.failed" => {
                let error = payload["error"].as_str().unwrap_or("unknown error");
                match latest_open_run_mut(&mut runs, &run_id) {
                    Some(run) => {
                        run.ended_at = Some(now);
                        run.review_state = ReviewState::Failed;
                        run.summary = Some(format!("[error] {error}"));
                    }
                    None => {
                        let fallback_id = if run_id.is_empty() {
                            format!("run-{task_id}")
                        } else {
                            run_id.clone()
                        };
                        let mut run = new_projected_run(
                            task_id,
                            &fallback_id,
                            model.as_deref().unwrap_or("v2-harness"),
                            now,
                        );
                        run.ended_at = Some(now);
                        run.review_state = ReviewState::Failed;
                        run.summary = Some(format!("[error] {error}"));
                        usage_input.push(0);
                        usage_output.push(0);
                        runs.push(run);
                    }
                }
            }
            "model.usage" => {
                if let Some(index) = runs
                    .iter()
                    .position(|run| run_id.is_empty() || run.id == run_id)
                {
                    usage_input[index] += payload["usage"]["input_tokens"].as_u64().unwrap_or(0);
                    usage_output[index] += payload["usage"]["output_tokens"].as_u64().unwrap_or(0);
                }
            }
            _ => {}
        }
    }
    for (index, run) in runs.iter_mut().enumerate() {
        if usage_input[index] > 0 || usage_output[index] > 0 {
            run.usage_json = Some(
                json!({
                    "input_tokens": usage_input[index],
                    "output_tokens": usage_output[index],
                })
                .to_string(),
            );
        }
    }

    // 统一展示状态由 core 的纯投影器派生（与旧链路同一 precedence 合同）。
    let status = project_task_status(TaskStatusProjectionInput {
        task: &task,
        runs: &runs,
        queued_messages: &[],
        approvals: &[],
        pending_question_count: 0,
        latest_verification: None,
        workspace_binding_invalid: false,
        unread_count: 0,
    });

    let active_branch = SessionBranch::main(task_id);
    let events = events
        .iter()
        .filter(|envelope| envelope["task_id"].as_str() == Some(task_id))
        .enumerate()
        .map(|(index, envelope)| TaskEvent {
            // v2 journal seq 全局唯一且有序，直接充当旧事件 id。
            id: envelope["seq"].as_u64().unwrap_or(index as u64) as i64,
            task_id: task_id.to_string(),
            branch_id: PROJECTED_BRANCH_ID.to_string(),
            event_type: journal_kind_to_event_type(
                envelope["payload"]["journalKind"]
                    .as_str()
                    .unwrap_or_default(),
            ),
            // v2 信封不带时间戳：读取时刻即投影时刻（诚实中性值）。
            created_at: now,
        })
        .collect();

    TaskDetail {
        task,
        status,
        // v2 单分支：main（storage_id = task_id，与旧 main 分支一致）。
        active_branch: active_branch.clone(),
        branches: vec![active_branch],
        runs,
        events,
        // v2 无文件变更 / 权限 / 验证 / 持久排队视图：诚实空。
        changes: Vec::new(),
        permissions: Vec::new(),
        verifications: Vec::new(),
        queued_messages: Vec::new(),
        pending_plan_entry_offer: None,
    }
}

fn latest_open_run_mut<'a>(runs: &'a mut [AgentRun], run_id: &str) -> Option<&'a mut AgentRun> {
    if !run_id.is_empty() {
        if let Some(index) = runs.iter().position(|run| run.id == run_id) {
            return Some(&mut runs[index]);
        }
    }
    runs.iter_mut().rev().find(|run| run.ended_at.is_none())
}

/// v2 事件 journal → 旧 `Vec<SessionMessage>`（Room 时间线形状）。
fn project_session_messages(task_id: &str, events: &[Value]) -> Vec<SessionMessage> {
    let mut messages = Vec::new();
    for envelope in events {
        if envelope["task_id"].as_str() != Some(task_id) {
            continue;
        }
        let payload = &envelope["payload"];
        let journal_kind = payload["journalKind"].as_str().unwrap_or_default();
        let run_id = payload["runId"]
            .as_str()
            .or_else(|| envelope["run_id"].as_str())
            .unwrap_or("run");
        let seq = envelope["seq"].as_u64().unwrap_or(0);
        match journal_kind {
            "input.queued" => messages.push(SessionMessage {
                id: payload["message_id"].as_str().map(str::to_string),
                branch_id: PROJECTED_BRANCH_ID.to_string(),
                kind: "message".into(),
                role: Some("user".into()),
                text: payload["text"].as_str().map(str::to_string),
                image_count: None,
                image_media_types: None,
                attachments: None,
                tool_name: None,
                call_id: None,
                input_json: None,
                output_json: None,
                is_error: None,
                // 旧形状只有 meta 行带 timestamp；消息行一律 None。
                timestamp: None,
            }),
            "assistant.message" => messages.push(SessionMessage {
                id: Some(format!("{run_id}:assistant:{seq}")),
                branch_id: PROJECTED_BRANCH_ID.to_string(),
                kind: "message".into(),
                role: Some("assistant".into()),
                text: payload["text"].as_str().map(str::to_string),
                image_count: None,
                image_media_types: None,
                attachments: None,
                tool_name: None,
                call_id: None,
                input_json: None,
                output_json: None,
                is_error: None,
                timestamp: None,
            }),
            "tool.call" => {
                let name = payload["name"].as_str().unwrap_or_default();
                messages.push(SessionMessage {
                    id: Some(projected_call_id(run_id, name, seq)),
                    branch_id: PROJECTED_BRANCH_ID.to_string(),
                    kind: "tool_call".into(),
                    role: None,
                    text: None,
                    image_count: None,
                    image_media_types: None,
                    attachments: None,
                    tool_name: Some(name.to_string()),
                    call_id: Some(projected_call_id(run_id, name, seq)),
                    input_json: payload["input"].as_str().map(str::to_string),
                    output_json: None,
                    is_error: None,
                    timestamp: None,
                });
            }
            "tool.result" => {
                let name = payload["name"].as_str().unwrap_or_default();
                messages.push(SessionMessage {
                    id: Some(projected_call_id(run_id, name, seq)),
                    branch_id: PROJECTED_BRANCH_ID.to_string(),
                    kind: "tool_result".into(),
                    role: None,
                    text: None,
                    image_count: None,
                    image_media_types: None,
                    attachments: None,
                    tool_name: Some(name.to_string()),
                    call_id: Some(projected_call_id(run_id, name, seq)),
                    input_json: None,
                    output_json: payload["output"].as_str().map(str::to_string),
                    is_error: Some(!payload["ok"].as_bool().unwrap_or(true)),
                    timestamp: None,
                });
            }
            _ => {}
        }
    }
    messages
}

/// 拉取 journal 事件信封（分页推进 cursor）。`batch_cap` 限制本轮最多的
/// 拉取批次数（事件泵启动期快进用大值，平时一两个批次即可追平）。
async fn read_all_task_events(
    client: &mut r_code_client::DaemonClient,
    batch_cap: usize,
) -> Result<Vec<Value>, crate::harness_v2::HarnessV2Error> {
    let mut all = Vec::new();
    let mut cursor = 0_u64;
    for _ in 0..batch_cap {
        let batch = client
            .call(
                "task.events",
                json!({"afterSeq": cursor, "limit": EVENT_BATCH_LIMIT}),
            )
            .await
            .map_err(|e| crate::harness_v2::HarnessV2Error::Command(e.to_string()))?;
        let rows = batch.as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        cursor = rows
            .last()
            .and_then(|row| row["seq"].as_u64())
            .unwrap_or(cursor);
        let reached_end = rows.len() < EVENT_BATCH_LIMIT as usize;
        all.extend(rows);
        if reached_end {
            break;
        }
    }
    Ok(all)
}

// ---------------------------------------------------------------------------
// 事件泵：v2 journal → 旧 agent-event 频道
// ---------------------------------------------------------------------------

/// 启动事件泵（tokio 任务）。桥构造失败仅告警不阻断应用；daemon 不可达时
/// 指数退避重连（2s 起，封顶 30s），绝不 panic。
///
/// 首轮先快进 cursor（不 emit），避免把历史 journal 的陈旧状态灌进刚启动
/// 的 WebView。payload 外层形状与旧 sink 完全一致：
/// `{"task_id": ..., "event": <AgentEvent>}`（复用 event_coalesce 的
/// `AgentEventEnvelope` 序列化）。
pub fn spawn_event_pump(app: tauri::AppHandle) {
    use tauri::Emitter;
    tauri::async_runtime::spawn(async move {
        let bridge = match ChatV2Bridge::shared() {
            Ok(bridge) => bridge,
            Err(error) => {
                tracing::warn!(%error, "v2 chat event pump disabled: bridge unavailable");
                return;
            }
        };
        let mut cursor = 0_u64;
        let mut synced = false;
        let mut backoff = Duration::from_secs(2);
        loop {
            let attempt = async {
                let mut client = bridge.inner.connect().await?;
                // 已追平后每轮只需一个批次。
                read_all_task_events(&mut client, if synced { 1 } else { EVENT_SYNC_BATCH_CAP })
                    .await
            };
            match attempt.await {
                Ok(batch) => {
                    backoff = Duration::from_secs(2);
                    if synced {
                        for envelope in &batch {
                            cursor = cursor.max(envelope["seq"].as_u64().unwrap_or(cursor));
                            if let Some((task_id, event)) = project_agent_event(envelope) {
                                let payload = crate::event_coalesce::AgentEventEnvelope {
                                    task_id: &task_id,
                                    event: &event,
                                };
                                if let Err(error) = app.emit("agent-event", &payload) {
                                    tracing::warn!(%error, "emit v2 agent-event failed");
                                }
                            }
                        }
                    } else {
                        // 快进：只推进 cursor，不向 WebView 发历史事件。
                        if let Some(last) = batch.last() {
                            cursor = cursor.max(last["seq"].as_u64().unwrap_or(cursor));
                        }
                        // 尾批不满说明已追平 journal。
                        if batch.len() < EVENT_BATCH_LIMIT as usize {
                            synced = true;
                            tracing::info!(cursor, "v2 chat event pump synced to daemon journal");
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(EVENT_POLL_MS)).await;
                }
                Err(error) => {
                    tracing::warn!(%error, "v2 chat event pump reconnecting");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// 单测：投影形状钉死（不依赖 daemon）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(seq: u64, task_id: &str, run_id: &str, kind: &str, payload: Value) -> Value {
        let mut payload = payload;
        if let Some(map) = payload.as_object_mut() {
            map.insert("journalKind".into(), json!(kind));
        }
        json!({
            "seq": seq,
            "task_id": task_id,
            "run_id": run_id,
            "kind": "progress",
            "source": {"source": "host"},
            "payload": payload,
        })
    }

    #[test]
    fn agent_event_projection_matches_old_channel_shape() {
        let task = "task-1";
        let started = envelope(
            1,
            task,
            "run-task-1-1",
            "run.started",
            json!({"runId": "run-task-1-1"}),
        );
        let message = envelope(
            2,
            task,
            "run-task-1-1",
            "assistant.message",
            json!({"runId": "run-task-1-1", "text": "你好"}),
        );
        let call = envelope(
            3,
            task,
            "run-task-1-1",
            "tool.call",
            json!({"runId": "run-task-1-1", "name": "read_file", "input": "{\"path\":\"a.rs\"}"}),
        );
        let result = envelope(
            4,
            task,
            "run-task-1-1",
            "tool.result",
            json!({"runId": "run-task-1-1", "name": "read_file", "ok": false, "output": "boom"}),
        );
        let usage = envelope(
            5,
            task,
            "run-task-1-1",
            "model.usage",
            json!({"runId": "run-task-1-1", "usage": {"input_tokens": 11, "output_tokens": 7}}),
        );

        let (emitted_task, event) = project_agent_event(&started).unwrap();
        assert_eq!(emitted_task, task);
        assert!(matches!(
            event,
            AgentEvent::State {
                state: TaskState::InProgress
            }
        ));

        let (_, event) = project_agent_event(&message).unwrap();
        match event {
            AgentEvent::Message { text, delta } => {
                assert_eq!(text, "你好");
                assert!(!delta);
            }
            other => panic!("unexpected {other:?}"),
        }

        let (_, event) = project_agent_event(&call).unwrap();
        match event {
            AgentEvent::ToolCall {
                name,
                input,
                call_id,
            } => {
                assert_eq!(name, "read_file");
                assert_eq!(input, json!({"path": "a.rs"}));
                assert_eq!(call_id, "run-task-1-1:read_file:3");
            }
            other => panic!("unexpected {other:?}"),
        }

        let (_, event) = project_agent_event(&result).unwrap();
        match event {
            AgentEvent::ToolResult {
                call_id,
                output,
                is_error,
            } => {
                assert_eq!(call_id, "run-task-1-1:read_file:4");
                assert_eq!(output, json!("boom"));
                assert!(is_error);
            }
            other => panic!("unexpected {other:?}"),
        }

        let (_, event) = project_agent_event(&usage).unwrap();
        match event {
            AgentEvent::Usage { usage_json } => {
                let parsed: Value = serde_json::from_str(&usage_json).unwrap();
                assert_eq!(parsed["input_tokens"], json!(11));
                assert_eq!(parsed["output_tokens"], json!(7));
            }
            other => panic!("unexpected {other:?}"),
        }

        // 非投影事件全部跳过。
        assert!(project_agent_event(&envelope(
            6,
            task,
            "",
            "task.renamed",
            json!({"title": "t"})
        ))
        .is_none());
        assert!(
            project_agent_event(&envelope(7, task, "", "harness.progress", json!({}))).is_none()
        );
    }

    #[test]
    fn task_detail_projection_carries_all_eleven_fields() {
        let task_id = "task-11";
        let detail = json!({
            "task_id": task_id,
            "title": "标题",
            "kind": "conversation",
            "objective": "目标",
            "state": "unverified",
            "running": false,
            "model": null,
            "inference": null,
            "mode": "ask",
            "runs": [],
            "usage": {"input_tokens": 0, "output_tokens": 0}
        });
        let events = vec![
            envelope(1, task_id, "", "task.created", json!({"taskId": task_id})),
            envelope(
                2,
                task_id,
                "run-task-11-1",
                "run.started",
                json!({"runId": "run-task-11-1"}),
            ),
            envelope(
                3,
                task_id,
                "run-task-11-1",
                "input.queued",
                json!({"message_id": "m1", "input_seq": 1, "kind": "user", "text": "hi"}),
            ),
            envelope(
                4,
                task_id,
                "run-task-11-1",
                "assistant.message",
                json!({"runId": "run-task-11-1", "text": "hello"}),
            ),
            envelope(
                5,
                task_id,
                "run-task-11-1",
                "run.failed",
                json!({"error": "no provider configured"}),
            ),
        ];
        let projected = project_task_detail(task_id, &detail, &events);
        // 11 字段齐全。
        assert_eq!(projected.task.id, task_id);
        assert_eq!(projected.task.title, "标题");
        assert_eq!(projected.task.goal, "目标");
        assert_eq!(projected.task.state, TaskState::Idle);
        assert_eq!(projected.status.task_id, task_id);
        assert_eq!(projected.active_branch.id, "main");
        assert_eq!(projected.branches.len(), 1);
        // run.failed（信封 run_id 兜底 run-{task}）标记最近的 open run 失败。
        assert_eq!(projected.runs.len(), 1);
        assert_eq!(projected.runs[0].review_state, ReviewState::Failed);
        assert!(projected.runs[0].ended_at.is_some());
        assert!(projected.runs[0]
            .summary
            .as_deref()
            .unwrap()
            .contains("no provider"));
        // 事件时间线映射。
        assert_eq!(projected.events.len(), 5);
        assert_eq!(projected.events[0].event_type, TaskEventType::TaskCreated);
        assert_eq!(
            projected.events[2].event_type,
            TaskEventType::UserMessageQueued
        );
        // 诚实空字段。
        assert!(projected.changes.is_empty());
        assert!(projected.permissions.is_empty());
        assert!(projected.verifications.is_empty());
        assert!(projected.queued_messages.is_empty());
        assert!(projected.pending_plan_entry_offer.is_none());
    }

    #[test]
    fn session_messages_projection_maps_user_assistant_and_tools() {
        let task_id = "task-12";
        let events = vec![
            envelope(
                1,
                task_id,
                "run-x",
                "input.queued",
                json!({"message_id": "m1", "input_seq": 1, "kind": "user", "text": "问题"}),
            ),
            envelope(
                2,
                task_id,
                "run-x",
                "tool.call",
                json!({"runId": "run-x", "name": "search", "input": "{\"query\":\"q\"}"}),
            ),
            envelope(
                3,
                task_id,
                "run-x",
                "tool.result",
                json!({"runId": "run-x", "name": "search", "ok": true, "output": "ok-text"}),
            ),
            envelope(
                4,
                task_id,
                "run-x",
                "assistant.message",
                json!({"runId": "run-x", "text": "回答"}),
            ),
        ];
        let messages = project_session_messages(task_id, &events);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role.as_deref(), Some("user"));
        assert_eq!(messages[0].text.as_deref(), Some("问题"));
        assert_eq!(messages[0].branch_id, "main");
        assert_eq!(messages[1].kind, "tool_call");
        assert_eq!(messages[1].tool_name.as_deref(), Some("search"));
        assert_eq!(messages[1].input_json.as_deref(), Some("{\"query\":\"q\"}"));
        assert_eq!(messages[1].call_id.as_deref(), Some("run-x:search:2"));
        assert_eq!(messages[2].kind, "tool_result");
        assert_eq!(messages[2].call_id.as_deref(), Some("run-x:search:3"));
        assert_eq!(messages[2].is_error, Some(false));
        assert_eq!(messages[3].role.as_deref(), Some("assistant"));
        assert_eq!(messages[3].text.as_deref(), Some("回答"));
    }

    #[test]
    fn v2_state_strings_map_onto_old_task_states() {
        assert_eq!(v2_state_to_task_state("pending", false), TaskState::Idle);
        assert_eq!(
            v2_state_to_task_state("pending", true),
            TaskState::InProgress
        );
        assert_eq!(
            v2_state_to_task_state("running", false),
            TaskState::InProgress
        );
        assert_eq!(
            v2_state_to_task_state("review-ready", false),
            TaskState::ReviewReady
        );
        assert_eq!(v2_state_to_task_state("verified", false), TaskState::Idle);
        assert_eq!(v2_state_to_task_state("unverified", false), TaskState::Idle);
        assert_eq!(v2_state_to_task_state("cancelled", false), TaskState::Idle);
        assert_eq!(
            v2_state_to_task_state("failed", false),
            TaskState::Interrupted
        );
        assert_eq!(
            v2_state_to_task_state("blocked", false),
            TaskState::Interrupted
        );
    }
}
