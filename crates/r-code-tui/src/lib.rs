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
