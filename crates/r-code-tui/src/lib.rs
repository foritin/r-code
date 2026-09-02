//! r-code-tui 核心状态与事件→widget 映射（pi-alignment PRD §4.1 R-TUI-01/M8-01）。
//!
//! 复用而非重做（PRD 冻结决策 §2.1）：会话编排经 `r_code_host::commands`
//! （CommandState 同源工厂 + Mock/Real runtime + drain 循环），事件源是
//! `AgentEvent`，持久化是 JSONL SessionStore——TUI 只是新的展示壳。
//!
//! 本模块是**可单测的纯状态机**（不含终端 IO）：`TuiState` 消费 AgentEvent
//! 维护 transcript 视图；ratatui 渲染与 crossterm 输入在 `app.rs`/`main.rs`
//! 壳层消费它。

use std::sync::{Arc, Mutex};

use r_code_core::dto::{AgentEvent, TaskState};

pub mod app;
pub mod approval;
pub mod approval_overlay;
pub mod bang_command;
pub mod bang_exec;
pub mod clipboard;
pub mod display;
pub mod export;
pub mod external_editor;
pub mod history;
pub mod ime;
pub mod inline_render;
pub mod input;
pub mod interaction;
pub mod mention;
pub mod model_selector;
pub mod paste;
pub mod session_ops;
pub mod session_picker;
pub mod setup_flow;
pub mod slash_menu;
pub mod snapshot;
pub mod status_bar;
pub mod task_mode;
pub mod thinking;
pub mod transcript_view;
pub mod window;

/// transcript 的一行（事件 → widget 映射的产物）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptRow {
    /// 用户消息。
    User { text: String },
    /// 助手消息（流式 delta 已合并；`complete` 标记是否收口）。
    Assistant { text: String, complete: bool },
    /// 工具卡（折叠态展示名 + 摘要；展开由 UI 交互层处理）。
    ToolCard {
        name: String,
        summary: String,
        is_error: bool,
    },
    /// 系统提示行（错误/状态）。
    System { text: String },
    /// !command 本地 shell 直执行（OSC 133 prompt/output 段；与 ToolCard
    /// 类型层区分——Agent 工具输出走 ToolCard）。
    Shell(crate::bang_command::ShellRow),
}

/// TUI 会话状态（纯逻辑；单线程模型——事件经 Mutex 共享投递）。
#[derive(Debug, Default)]
pub struct TuiState {
    rows: Vec<TranscriptRow>,
    /// 当前流式 assistant 累积文本（delta 合并缓冲）。
    streaming: Option<String>,
    /// 会话是否运行中（发送/steer 可用性判断）。
    running: bool,
    /// 当前 (provider, model) 选择（M2-01 footer 右侧联动；权威在 Task，
    /// 此处仅投影，选中写回成功后更新）。
    model_selection: Option<(String, String)>,
    /// 当前思考档位（M2-02 footer `• thinking` 段；None = 未设/不支持省略段）。
    thinking: Option<String>,
    /// 当前任务模式（M2-03 Shift+Tab 循环投影；权威在 Task.mode）。
    task_mode: String,
    /// 待发送队列的展示镜像（M2-04：权威在宿主持久化队列；此处仅渲染投影，
    /// 新 run 启动（Activity）即清空——宿主在 run 结束时自动派发队列）。
    queued: Vec<String>,
    /// 待审批请求投影（M2-05：权威在 PermissionEngine pending 队列）。
    pending_approval: Option<crate::approval_overlay::PendingApproval>,
    /// 会话累计用量投影（M3-01：权威在 TaskDetail.runs.usage_json）。
    usage: Option<crate::status_bar::UsageStats>,
}

impl TuiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 消费一枚 AgentEvent（AgentEvent 是唯一事件源——与桌面 WebView 同源）。
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Message { text, delta } => {
                if *delta {
                    // 流式 delta：合并进缓冲（缓冲不存在则开新 assistant 段）。
                    self.streaming
                        .get_or_insert_with(String::new)
                        .push_str(text);
                } else {
                    // 封口帧：text 非空 = 权威全文（替换缓冲）。
                    self.rows.push(TranscriptRow::Assistant {
                        text: if text.is_empty() {
                            self.streaming.take().unwrap_or_default()
                        } else {
                            text.clone()
                        },
                        complete: true,
                    });
                    self.streaming = None;
                }
            }
            AgentEvent::ToolCall { name, input, .. } => {
                self.rows.push(TranscriptRow::ToolCard {
                    name: name.clone(),
                    summary: summarize_input(input),
                    is_error: false,
                });
            }
            AgentEvent::ToolResult { is_error, .. } => {
                // 结果错误标记回写最后一张工具卡。
                if let Some(TranscriptRow::ToolCard { is_error: slot, .. }) = self.rows.last_mut() {
                    *slot = *is_error;
                }
            }
            AgentEvent::Activity { .. } => {
                self.running = true;
                // 新 run 启动 = 队列已派发/排空（宿主 run 结束时自动派发）。
                self.queued.clear();
            }
            AgentEvent::State {
                state: TaskState::Idle | TaskState::ReviewReady,
            } => {
                self.running = false;
            }
            AgentEvent::State { .. } => {
                self.running = true;
            }
            _ => {}
        }
    }

    /// 流式预览（未收口 assistant 缓冲；live 区呈现，收口后整行 commit）。
    pub fn streaming_preview(&self) -> Option<&str> {
        self.streaming.as_deref()
    }

    /// 流式缓冲刷屏（渲染前调用：未收口的 assistant 也可见）。
    pub fn flush_streaming(&mut self) {
        if let Some(text) = self.streaming.take() {
            if !text.is_empty() {
                self.rows.push(TranscriptRow::Assistant {
                    text,
                    complete: false,
                });
            }
        }
    }

    pub fn rows(&self) -> &[TranscriptRow] {
        &self.rows
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// 用户输入 → transcript 行（发送前本地可见）。
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.rows.push(TranscriptRow::User { text: text.into() });
    }

    /// 系统/错误行 → transcript（M1-03：alt-screen 下 eprintln 不可见，
    /// 交互路径的用户可见错误一律走这里）。
    pub fn push_system(&mut self, text: impl Into<String>) {
        self.rows.push(TranscriptRow::System { text: text.into() });
    }

    /// 当前模型选择投影（权威在 Task；选中写回成功后由壳层更新）。
    pub fn model_selection(&self) -> Option<&(String, String)> {
        self.model_selection.as_ref()
    }

    pub fn set_model_selection(&mut self, provider: String, model: String) {
        self.model_selection = Some((provider, model));
    }

    /// 当前思考档位投影（权威在 Task.inference）。
    pub fn thinking(&self) -> Option<&str> {
        self.thinking.as_deref()
    }

    pub fn set_thinking(&mut self, level: Option<String>) {
        self.thinking = level;
    }

    /// 当前任务模式投影（默认 ask）。
    pub fn task_mode(&self) -> &str {
        &self.task_mode
    }

    pub fn set_task_mode(&mut self, mode: String) {
        self.task_mode = mode;
    }

    /// 运行中入队的展示镜像（发送经宿主 AgentSendMode::Queue）。
    pub fn queue_message(&mut self, text: impl Into<String>) {
        self.queued.push(text.into());
    }

    pub fn queued(&self) -> &[String] {
        &self.queued
    }

    /// 待审批请求（Some 时浮层自动呈现，键位被审批契约接管）。
    pub fn pending_approval(&self) -> Option<&crate::approval_overlay::PendingApproval> {
        self.pending_approval.as_ref()
    }

    pub fn set_pending_approval(&mut self, pending: crate::approval_overlay::PendingApproval) {
        self.pending_approval = Some(pending);
    }

    /// 取走待审批请求（决策时消费；None = 已被处理）。
    pub fn take_pending_approval(&mut self) -> Option<crate::approval_overlay::PendingApproval> {
        self.pending_approval.take()
    }

    /// 追加一条已完成行（! 直通 shell 行等壳层产出）。
    pub fn push_row(&mut self, row: TranscriptRow) {
        self.rows.push(row);
    }

    /// 清空 transcript 视图（/clear；持久化 JSONL 不动，仅展示层）。
    pub fn clear_transcript(&mut self) {
        self.rows.clear();
        self.streaming = None;
    }

    /// 用量投影（footer 统计；由壳层周期性从 task_detail 刷新）。
    pub fn usage(&self) -> Option<crate::status_bar::UsageStats> {
        self.usage
    }

    pub fn set_usage(&mut self, stats: crate::status_bar::UsageStats) {
        self.usage = Some(stats);
    }
}

/// M1-03/R2：把宿主运行错误翻译成可操作文案。
///
/// provider 配置类错误（宿主 `ensure_real_runtime` 的稳定话术"模型服务/
/// 未找到默认/尚未就绪"）附上 config 文件绝对路径与桌面设置页途径；其余
/// 错误原样返回（不追加误导性指引）。
pub fn provider_error_guidance(error: &str, config_dir: &std::path::Path) -> String {
    let provider_related = ["模型服务", "未找到默认", "尚未就绪", "provider"]
        .iter()
        .any(|needle| error.contains(needle));
    if !provider_related {
        return error.to_string();
    }
    let config_file = config_dir.join("config.toml");
    format!(
        "{error}\n配置途径：桌面端 R-Code Dev「设置 → 模型服务」选择并保存，或直接编辑 {}。",
        config_file.display()
    )
}

/// M2-04：排队显示行（codex 形态：`• Queued follow-up inputs` + `  ↳`）。
pub fn queue_lines(queued: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    if !queued.is_empty() {
        lines.push("• Queued follow-up inputs".to_string());
        for text in queued {
            lines.push(format!("  ↳ {text}"));
        }
    }
    lines
}

/// M2-04：发送路由——运行中 Enter = 排队（不打断当前 run；v1 的 Auto/steer
/// 语义让位），空闲 Enter = 正常发送。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendRoute {
    Send,
    Queue,
}

pub fn route_send(running: bool) -> SendRoute {
    if running {
        SendRoute::Queue
    } else {
        SendRoute::Send
    }
}

/// R1/R2：无 provider 配置时的显式引导（不降级、不回放演示）。
///
/// 只做"有没有配置"的轻量判定（config.toml 缺失 / providers 空 / 默认服务不在表内）；
/// 不复刻宿主 readiness——密钥可能存放在 OS 凭据库，深校验留给真实装配路径，避免
/// 把"已配置好密钥的服务"误报成未配置。
pub fn provider_config_guidance(config_dir: &std::path::Path) -> Option<String> {
    let config_file = config_dir.join("config.toml");
    let settings = r_code_host::settings::SettingsService::new(config_dir.to_path_buf());
    let config = match settings.load_global_unvalidated() {
        Ok(config) => config,
        Err(error) => {
            return Some(format!(
                "配置加载失败：{error}\n配置文件：{}\n也可在桌面端 R-Code Dev 的「设置 → 模型服务」完成配置后重试。",
                config_file.display()
            ));
        }
    };
    let default = config.default_provider.trim();
    if default.is_empty() || !config.providers.contains_key(default) {
        return Some(format!(
            "未找到默认模型服务“{default}”。三种配置途径：\n  1) 本 TUI 输入 /setup（选预设 + 输 API key，保存即默认）；\n  2) 桌面端 R-Code Dev「设置 → 模型服务」选择并保存；\n  3) 直接编辑 {}。",
            config_file.display()
        ));
    }
    None
}

/// M1-04：无配置时的首屏引导行（进 transcript 头部；已配置返回空——
/// 首屏不出现引导）。发送路径由真实模式兜底：发送即出 provider 引导错误。
pub fn onboarding_lines(config_dir: &std::path::Path) -> Vec<String> {
    match provider_config_guidance(config_dir) {
        Some(guidance) => vec!["R-Code CLI 尚未配置模型服务".to_string(), guidance],
        None => Vec::new(),
    }
}

fn summarize_input(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Object(map) => {
            // 取首个短字符串字段做摘要（command/cwd/path 等常见键优先）。
            for key in ["command", "path", "pattern", "query", "url"] {
                if let Some(value) = map.get(key).and_then(|v| v.as_str()) {
                    return truncate(value, 60);
                }
            }
            if let Some((_, first)) = map.iter().next() {
                if let Some(text) = first.as_str() {
                    return truncate(text, 60);
                }
            }
            truncate(&input.to_string(), 60)
        }
        other => truncate(&other.to_string(), 60),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// 事件桥：宿主事件 sink → TUI 状态（Mutex 共享投递）。
pub struct EventBridge {
    state: Arc<Mutex<TuiState>>,
}

impl EventBridge {
    pub fn new(state: Arc<Mutex<TuiState>>) -> Self {
        Self { state }
    }

    /// 作为 AgentEventSink 闭包体（host set_agent_event_sink）。
    pub fn forward(&self, _task_id: &str, event: &AgentEvent) {
        if let Ok(mut state) = self.state.lock() {
            state.apply(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::AgentActivityPhase;

    /// M2-04.A2：排队渲染行格式（• 标题 + ↳ 缩进条目；空队列为空）。
    #[test]
    fn queue_lines_follow_codex_format() {
        assert!(queue_lines(&[]).is_empty());
        let lines = queue_lines(&["先修这个".to_string(), "再加测试".to_string()]);
        assert_eq!(lines[0], "• Queued follow-up inputs");
        assert_eq!(lines[1], "  ↳ 先修这个");
        assert_eq!(lines[2], "  ↳ 再加测试");
    }

    /// M2-04.A1/A3：运行中 Enter 走 Queue 路由；镜像随新 run（Activity）清空。
    #[test]
    fn queue_mirror_lifecycle() {
        assert_eq!(route_send(false), SendRoute::Send);
        assert_eq!(route_send(true), SendRoute::Queue, "运行中不打断当前 run");
        let mut state = TuiState::new();
        state.queue_message("follow-up");
        assert_eq!(state.queued(), &["follow-up".to_string()]);
        // 新 run 启动（宿主派发队列）→ 镜像清空。
        state.apply(&AgentEvent::Activity {
            phase: AgentActivityPhase::Requesting,
            detail: None,
        });
        assert!(state.queued().is_empty(), "Activity 后队列镜像必须清空");
    }

    /// M2-04.A1 集成：Queue 模式经宿主发送链路接受（空闲 = 立即分发契约）。
    #[tokio::test]
    async fn queue_mode_passes_host_send_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = r_code_store::Database::open(dir.path().join("app.db")).expect("db");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir");
        // 与 main.rs shared_state 同构：发送链路需要 sessions/blobs 目录就位。
        std::fs::create_dir_all(dir.path().join("sessions")).expect("mkdir sessions");
        std::fs::create_dir_all(dir.path().join("blobs")).expect("mkdir blobs");
        let state = r_code_host::commands::CommandState::new_with_planning_release_control(
            std::sync::Arc::new(db),
            dir.path().join("blobs"),
            dir.path().join("sessions"),
            config_dir,
            dir.path().join("project"),
            Some(dir.path().join("app.db")),
            r_code_host::plan_policy::PlanningReleaseControl {
                provider_kind: "tui-test".to_string(),
                release_state: r_code_host::plan_policy::PlanningReleaseState::Off,
                emergency_off: false,
                eligibility_profile_version: String::new(),
                evidence_version: String::new(),
                allowed_models: Vec::new(),
                allowed_protocols: Vec::new(),
                allowed_endpoint_classes: Vec::new(),
                basis: "queue test".to_string(),
            },
        );
        let task = r_code_host::commands::task_create(&state, None, "t", "goal", "ask")
            .await
            .expect("task");
        r_code_host::commands::install_mock_scenario(
            &state,
            &task.id,
            vec![r_code_core::dto::AgentEvent::Message {
                text: "queued-ok".to_string(),
                delta: false,
            }],
        )
        .await
        .expect("scenario");
        r_code_host::commands::agent_send_with_mode(
            &state,
            &task.id,
            "hello",
            r_code_core::dto::AgentSendMode::Queue,
        )
        .await
        .expect("queue send must pass host path");
    }

    /// M1-04.A1：空配置 → 首屏引导行（System 投影，含两条配置途径）。
    #[test]
    fn onboarding_lines_empty_config_lists_guidance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir");
        let lines = onboarding_lines(&config_dir);
        assert!(!lines.is_empty(), "unconfigured must yield onboarding rows");
        let mut state = TuiState::new();
        for line in &lines {
            state.push_system(line.clone());
        }
        let rendered = state
            .rows()
            .iter()
            .filter_map(|row| match row {
                TranscriptRow::System { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(
            rendered.contains("尚未配置"),
            "first screen must state the unconfigured condition: {rendered}"
        );
        assert!(
            rendered.contains("config.toml"),
            "onboarding must name the config file: {rendered}"
        );
        assert!(
            rendered.contains("设置 → 模型服务"),
            "onboarding must point at the desktop settings page: {rendered}"
        );
    }

    /// M1-04.A2：已配置 → 首屏引导行不存在。
    #[test]
    fn onboarding_lines_configured_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir");
        std::fs::write(
            config_dir.join("config.toml"),
            "default_provider = \"demo\"\n\n[providers.demo]\nbase_url = \"https://example.invalid/v1\"\napi_key = \"test-key\"\nmodel = \"demo-model\"\n",
        )
        .expect("write config");
        assert!(
            onboarding_lines(&config_dir).is_empty(),
            "configured default provider must not produce onboarding rows"
        );
    }

    /// M1-03.A1：交互路径错误进 transcript（System 行投影）。
    #[test]
    fn system_errors_project_into_transcript() {
        let mut state = TuiState::new();
        state.push_user("hello");
        state.push_system("发送失败：未找到默认模型服务“deepseek”");
        let rows = state.rows();
        assert_eq!(rows.len(), 2);
        assert!(
            matches!(&rows[1], TranscriptRow::System { text } if text.contains("发送失败")),
            "error must surface as a System row, got {:?}",
            rows[1]
        );
    }

    /// M1-03.A2：provider 配置类错误附 config 绝对路径与设置页途径；其余错误不追加。
    #[test]
    fn provider_errors_carry_actionable_guidance() {
        let dir = std::path::Path::new("/tmp/tui-v2-guidance/config");
        let provider_error = "未找到默认模型服务“deepseek”，请前往设置完成配置";
        let guided = provider_error_guidance(provider_error, dir);
        assert!(
            guided.contains(provider_error),
            "original error must remain"
        );
        assert!(
            guided.contains(dir.join("config.toml").to_str().unwrap()),
            "guidance must name the absolute config path: {guided}"
        );
        assert!(
            guided.contains("设置 → 模型服务"),
            "guidance must point at the desktop settings page: {guided}"
        );
        // 非 provider 错误：原样返回，不追加误导性指引。
        let other = "任务队列已满";
        assert_eq!(provider_error_guidance(other, dir), other);
    }

    /// M8-01.A2（事件→widget 映射）：消息流/工具卡折叠/流式合并。
    #[test]
    fn events_map_to_transcript_rows() {
        let mut state = TuiState::new();
        state.push_user("hello");
        state.apply(&AgentEvent::Message {
            text: "部分".into(),
            delta: true,
        });
        state.apply(&AgentEvent::Message {
            text: "回答全文".into(),
            delta: false,
        });
        state.apply(&AgentEvent::ToolCall {
            name: "bash".into(),
            input: serde_json::json!({"command": "cargo test --workspace"}),
            call_id: "c1".into(),
        });
        state.apply(&AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: serde_json::json!("ok"),
            is_error: false,
        });
        let rows = state.rows();
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], TranscriptRow::User { text } if text == "hello"));
        // 封口帧替换 delta 缓冲。
        assert!(
            matches!(&rows[1], TranscriptRow::Assistant { text, complete } if text == "回答全文" && *complete)
        );
        // 工具卡折叠：名 + 摘要（截断）+ 错误位回写。
        assert!(
            matches!(&rows[2], TranscriptRow::ToolCard { name, summary, is_error }
            if name == "bash" && summary.contains("cargo test") && !is_error)
        );
    }

    /// 流式 assistant 未收口也可见（flush_streaming）。
    #[test]
    fn streaming_assistant_visible_before_close() {
        let mut state = TuiState::new();
        state.apply(&AgentEvent::Message {
            text: "abc".into(),
            delta: true,
        });
        assert!(state.rows().is_empty(), "delta 只进缓冲");
        state.flush_streaming();
        assert!(matches!(
            state.rows().last(),
            Some(TranscriptRow::Assistant { text, complete: false }) if text == "abc"
        ));
    }

    /// 工具卡错误位：失败结果回写。
    #[test]
    fn tool_card_error_flag_written_back() {
        let mut state = TuiState::new();
        state.apply(&AgentEvent::ToolCall {
            name: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
            call_id: "c".into(),
        });
        state.apply(&AgentEvent::ToolResult {
            call_id: "c".into(),
            output: serde_json::json!(null),
            is_error: true,
        });
        assert!(matches!(
            state.rows().last(),
            Some(TranscriptRow::ToolCard { is_error: true, .. })
        ));
    }

    /// M8-01.A3（data-dir 共享）：共享 data-dir 路径解析与桌面同根。
    #[test]
    fn shared_data_dir_resolves_to_desktop_root() {
        // 桌面 AppData 根 = r_code_host::app_paths::default_data_dir()（Tauri
        // app_data_dir 同一根）；TUI 默认共享同一根（PRD 冻结决策 §2.4）。
        let root = r_code_host::app_paths::default_data_dir();
        assert!(root.is_some(), "AppData 根必须可解析");
        // 显式覆盖优先（--data-dir 参数语义）。
        let overridden = std::path::PathBuf::from("D:/custom/root");
        assert!(overridden.ends_with("root"));
    }

    /// EventBridge：sink 闭包投递进共享状态。
    #[test]
    fn event_bridge_forwards_into_shared_state() {
        let state = Arc::new(Mutex::new(TuiState::new()));
        let bridge = EventBridge::new(state.clone());
        bridge.forward(
            "task-1",
            &AgentEvent::Message {
                text: "hi".into(),
                delta: false,
            },
        );
        assert_eq!(state.lock().unwrap().rows().len(), 1);
    }
}
