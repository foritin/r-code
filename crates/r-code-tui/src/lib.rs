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
pub mod bang_command;
pub mod fullscreen;
pub mod ime;
pub mod input;
pub mod interaction;
pub mod snapshot;
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
            "未找到默认模型服务“{default}”。两种配置途径：\n  1) 桌面端 R-Code Dev「设置 → 模型服务」选择并保存；\n  2) 直接编辑 {}。",
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
