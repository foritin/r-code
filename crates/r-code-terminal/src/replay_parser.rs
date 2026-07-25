//! Replay Parser - 结构化 Replay 解析 [doc-10 §5]
//!
//! 增量最佳-effort 解析器，处理外部 CLI 的 JSONL 流。
//!
//! ## 设计原则 [doc-10 §5.2]
//! - 丢弃 thinking/reasoning 块。
//! - 提取可观察动作。
//! - 不存储原始思维链。
//!
//! ## Codex 事件映射 [doc-10 §5.3]
//! | Codex 事件                        | 解析结果          |
//! |-----------------------------------|-------------------|
//! | thread.started / session_meta     | SessionStarted    |
//! | turn.started                      | TurnStarted       |
//! | turn.completed                    | TurnCompleted     |
//! | item* (command_execution)         | CommandExecution  |
//! | item* (agent_message)             | AgentMessage      |
//! | reasoning                         | Reasoning (丢弃)  |
//!
//! ## Claude 事件映射 [doc-10 §5.4]
//! | Claude 事件                       | 解析结果          |
//! |-----------------------------------|-------------------|
//! | system (init)                     | SessionStarted    |
//! | assistant/user (text)             | AgentMessage      |
//! | assistant (tool_use)              | ToolUse           |
//! | user (tool_result)                | ToolResult        |
//! | result                            | TurnCompleted     |
//! | thinking/redacted_thinking        | Reasoning (丢弃)  |

use serde::{Deserialize, Serialize};

use crate::cli_detector::ExternalCli;

/// 从外部 CLI transcript 解析出的 replay 事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplayEvent {
    /// 会话/线程已启动
    SessionStarted {
        session_id: String,
        timestamp: String,
    },
    /// 回合已启动
    TurnStarted { turn_id: String, timestamp: String },
    /// 回合已完成
    TurnCompleted {
        turn_id: String,
        exit_code: Option<i32>,
        timestamp: String,
    },
    /// 命令执行
    CommandExecution {
        command: String,
        output: String,
        exit_code: Option<i32>,
    },
    /// Agent 消息（文本输出）
    AgentMessage { text: String },
    /// 工具使用
    ToolUse {
        name: String,
        input: serde_json::Value,
        result: Option<serde_json::Value>,
    },
    /// 工具结果
    ToolResult {
        call_id: String,
        output: String,
        is_error: bool,
    },
    /// 推理/思考（丢弃，不存储）
    Reasoning,
}

/// Replay 解析器 - 增量解析外部 CLI 的 JSONL transcript。
///
/// 最佳-effort 解析。丢弃 thinking/reasoning 块。
/// **永不**存储原始思维链。
pub struct ReplayParser {
    events: Vec<ReplayEvent>,
    buffer: String,
}

impl ReplayParser {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            buffer: String::new(),
        }
    }

    /// 输入原始文本数据（可能包含不完整行）。
    ///
    /// 按 `\n` 分割行，每行尝试用 Codex 和 Claude 格式解析。
    /// 不完整的行暂存在内部缓冲区，等待后续 feed 补全。
    pub fn feed(&mut self, data: &str) {
        self.buffer.push_str(data);

        while let Some(nl_pos) = self.buffer.find('\n') {
            // 提取一行（不含换行符）
            let line: String = self.buffer[..nl_pos].to_string();
            // 保留剩余部分
            self.buffer = self.buffer[nl_pos + 1..].to_string();

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // 最佳-effort：先尝试 Codex 格式，再尝试 Claude 格式
            if let Some(event) = parse_codex_line(trimmed) {
                self.events.push(event);
            } else if let Some(event) = parse_claude_line(trimmed) {
                self.events.push(event);
            }
            // 无法解析的行静默丢弃（best-effort）
        }
    }

    /// 获取目前已解析的事件。
    pub fn events(&self) -> &[ReplayEvent] {
        &self.events
    }

    /// 清除已解析的事件。
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

impl Default for ReplayParser {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Codex 格式解析
// ============================================================================

/// 解析 Codex 格式的 JSONL 行。
///
/// Codex 事件：thread.started, turn.started, turn.completed,
/// item*(command_execution), item*(agent_message), reasoning
pub fn parse_codex_line(line: &str) -> Option<ReplayEvent> {
    let json: serde_json::Value = serde_json::from_str(line).ok()?;

    let type_val = json.get("type").and_then(|v| v.as_str())?;

    match type_val {
        // session_meta 是 Codex rollout 的首行
        "session_meta" => {
            let payload = json.get("payload")?;
            let session_id = payload
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let timestamp = payload
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ReplayEvent::SessionStarted {
                session_id,
                timestamp,
            })
        }
        "thread.started" => {
            let session_id = json
                .get("thread_id")
                .or_else(|| json.get("session_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let timestamp = json
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ReplayEvent::SessionStarted {
                session_id,
                timestamp,
            })
        }
        "turn.started" => {
            let turn_id = json
                .get("turn_id")
                .or_else(|| json.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let timestamp = json
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ReplayEvent::TurnStarted { turn_id, timestamp })
        }
        "turn.completed" => {
            let turn_id = json
                .get("turn_id")
                .or_else(|| json.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let exit_code = json
                .get("exit_code")
                .and_then(|v| v.as_i64())
                .map(|i| i as i32);
            let timestamp = json
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ReplayEvent::TurnCompleted {
                turn_id,
                exit_code,
                timestamp,
            })
        }
        // item, item.started, item.completed
        t if t.starts_with("item") => {
            let item = json.get("item")?;
            let item_type = item.get("type").and_then(|v| v.as_str())?;
            match item_type {
                "command_execution" => {
                    let command = item
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let output = item
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let exit_code = item
                        .get("exit_code")
                        .and_then(|v| v.as_i64())
                        .map(|i| i as i32);
                    Some(ReplayEvent::CommandExecution {
                        command,
                        output,
                        exit_code,
                    })
                }
                "agent_message" => {
                    let text = extract_content_text(item.get("content"));
                    Some(ReplayEvent::AgentMessage { text })
                }
                _ => None,
            }
        }
        "reasoning" => Some(ReplayEvent::Reasoning),
        _ => None,
    }
}

// ============================================================================
// Claude 格式解析
// ============================================================================

/// 解析 Claude 格式的 JSONL 行。
///
/// Claude 事件：system(init), assistant(text/tool_use), user(tool_result),
/// result, thinking/redacted_thinking
pub fn parse_claude_line(line: &str) -> Option<ReplayEvent> {
    let json: serde_json::Value = serde_json::from_str(line).ok()?;

    // 优先使用顶层 type 字段，回退到 role 字段
    let type_val = json
        .get("type")
        .and_then(|v| v.as_str())
        .or_else(|| json.get("role").and_then(|v| v.as_str()))?;

    match type_val {
        "system" => {
            // system (init) -> SessionStarted
            let subtype = json.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
            if subtype == "init" || json.get("session_id").is_some() {
                let session_id = json
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let timestamp = json
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(ReplayEvent::SessionStarted {
                    session_id,
                    timestamp,
                })
            } else {
                None
            }
        }
        "assistant" | "user" => {
            let message = json.get("message")?;
            let content = message.get("content")?;

            // content 可以是数组或字符串
            if let Some(arr) = content.as_array() {
                let mut saw_reasoning = false;
                for block in arr {
                    let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match block_type {
                        "text" => {
                            let text = block
                                .get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            return Some(ReplayEvent::AgentMessage { text });
                        }
                        "tool_use" => {
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let input = block
                                .get("input")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            return Some(ReplayEvent::ToolUse {
                                name,
                                input,
                                result: None,
                            });
                        }
                        "tool_result" => {
                            let call_id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let output = extract_content_text(block.get("content"));
                            let is_error = block
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            return Some(ReplayEvent::ToolResult {
                                call_id,
                                output,
                                is_error,
                            });
                        }
                        "thinking" | "redacted_thinking" => {
                            saw_reasoning = true;
                        }
                        _ => {}
                    }
                }
                // 所有块都是 thinking/reasoning
                if saw_reasoning {
                    Some(ReplayEvent::Reasoning)
                } else {
                    None
                }
            } else {
                // 用户文本消息
                content.as_str().map(|s| ReplayEvent::AgentMessage {
                    text: s.to_string(),
                })
            }
        }
        "result" => {
            // result -> TurnCompleted
            let is_error = json.get("is_error").and_then(|v| v.as_bool());
            let exit_code = is_error.map(|e| if e { 1 } else { 0 });
            let timestamp = json
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ReplayEvent::TurnCompleted {
                turn_id: String::new(),
                exit_code,
                timestamp,
            })
        }
        _ => None,
    }
}

// ============================================================================
// 格式检测
// ============================================================================

/// 从 transcript 文件的前几行判断 CLI 格式。
///
/// 检查首批行中的 Codex 或 Claude 特征字段。
pub fn detect_format(first_lines: &[&str]) -> Option<ExternalCli> {
    for line in first_lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };

        let type_val = json.get("type").and_then(|v| v.as_str());

        // Codex 特征类型
        if let Some(t) = type_val {
            if matches!(
                t,
                "session_meta" | "thread.started" | "turn.started" | "turn.completed" | "reasoning"
            ) || t.starts_with("item")
            {
                return Some(ExternalCli::Codex);
            }
        }

        // Claude 特征类型
        if let Some(t) = type_val {
            if matches!(t, "system" | "assistant" | "user" | "result") {
                return Some(ExternalCli::Claude);
            }
        }

        // Claude 顶层 role 字段
        if json.get("role").and_then(|v| v.as_str()).is_some() {
            return Some(ExternalCli::Claude);
        }
    }
    None
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 从 content 字段提取文本。
///
/// content 可以是：
/// - 字符串：直接返回
/// - 数组：拼接所有 text 块
/// - 缺失：返回空字符串
fn extract_content_text(content: Option<&serde_json::Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Codex 解析 ──────────────────────────────────────────────────

    #[test]
    fn parse_codex_session_meta() {
        let line = r#"{"type":"session_meta","payload":{"id":"abc-123","timestamp":"2024-01-01T00:00:00Z","cwd":"/tmp"}}"#;
        let event = parse_codex_line(line).unwrap();
        match event {
            ReplayEvent::SessionStarted {
                session_id,
                timestamp,
            } => {
                assert_eq!(session_id, "abc-123");
                assert_eq!(timestamp, "2024-01-01T00:00:00Z");
            }
            _ => panic!("expected SessionStarted, got {event:?}"),
        }
    }

    #[test]
    fn parse_codex_thread_started() {
        let line = r#"{"type":"thread.started","thread_id":"thread-1","timestamp":"2024-01-01T00:00:01Z"}"#;
        let event = parse_codex_line(line).unwrap();
        match event {
            ReplayEvent::SessionStarted {
                session_id,
                timestamp,
            } => {
                assert_eq!(session_id, "thread-1");
                assert_eq!(timestamp, "2024-01-01T00:00:01Z");
            }
            _ => panic!("expected SessionStarted, got {event:?}"),
        }
    }

    #[test]
    fn parse_codex_turn_started() {
        let line =
            r#"{"type":"turn.started","turn_id":"turn-1","timestamp":"2024-01-01T00:00:02Z"}"#;
        let event = parse_codex_line(line).unwrap();
        match event {
            ReplayEvent::TurnStarted { turn_id, timestamp } => {
                assert_eq!(turn_id, "turn-1");
                assert_eq!(timestamp, "2024-01-01T00:00:02Z");
            }
            _ => panic!("expected TurnStarted, got {event:?}"),
        }
    }

    #[test]
    fn parse_codex_turn_completed() {
        let line = r#"{"type":"turn.completed","turn_id":"turn-1","exit_code":0,"timestamp":"2024-01-01T00:00:10Z"}"#;
        let event = parse_codex_line(line).unwrap();
        match event {
            ReplayEvent::TurnCompleted {
                turn_id,
                exit_code,
                timestamp,
            } => {
                assert_eq!(turn_id, "turn-1");
                assert_eq!(exit_code, Some(0));
                assert_eq!(timestamp, "2024-01-01T00:00:10Z");
            }
            _ => panic!("expected TurnCompleted, got {event:?}"),
        }
    }

    #[test]
    fn parse_codex_command_execution() {
        let line = r#"{"type":"item.completed","item":{"type":"command_execution","command":"ls -la","exit_code":0,"output":"file.txt"}}"#;
        let event = parse_codex_line(line).unwrap();
        match event {
            ReplayEvent::CommandExecution {
                command,
                output,
                exit_code,
            } => {
                assert_eq!(command, "ls -la");
                assert_eq!(output, "file.txt");
                assert_eq!(exit_code, Some(0));
            }
            _ => panic!("expected CommandExecution, got {event:?}"),
        }
    }

    #[test]
    fn parse_codex_command_execution_no_exit_code() {
        let line =
            r#"{"type":"item.started","item":{"type":"command_execution","command":"echo hi"}}"#;
        let event = parse_codex_line(line).unwrap();
        match event {
            ReplayEvent::CommandExecution {
                command,
                output,
                exit_code,
            } => {
                assert_eq!(command, "echo hi");
                assert_eq!(output, "");
                assert_eq!(exit_code, None);
            }
            _ => panic!("expected CommandExecution, got {event:?}"),
        }
    }

    #[test]
    fn parse_codex_agent_message() {
        let line = r#"{"type":"item.completed","item":{"type":"agent_message","content":[{"type":"text","text":"Hello world"}]}}"#;
        let event = parse_codex_line(line).unwrap();
        match event {
            ReplayEvent::AgentMessage { text } => {
                assert_eq!(text, "Hello world");
            }
            _ => panic!("expected AgentMessage, got {event:?}"),
        }
    }

    #[test]
    fn parse_codex_reasoning_discarded() {
        let line = r#"{"type":"reasoning","reasoning":"let me think..."}"#;
        let event = parse_codex_line(line).unwrap();
        assert!(matches!(event, ReplayEvent::Reasoning));
    }

    #[test]
    fn parse_codex_unknown_type_returns_none() {
        let line = r#"{"type":"unknown_event","data":"foo"}"#;
        assert!(parse_codex_line(line).is_none());
    }

    #[test]
    fn parse_codex_invalid_json_returns_none() {
        assert!(parse_codex_line("not json").is_none());
        assert!(parse_codex_line("").is_none());
        assert!(parse_codex_line("{broken").is_none());
    }

    // ── Claude 解析 ─────────────────────────────────────────────────

    #[test]
    fn parse_claude_system_init() {
        let line = r#"{"type":"system","subtype":"init","session_id":"sess-1","cwd":"/tmp","timestamp":"2024-01-01T00:00:00Z"}"#;
        let event = parse_claude_line(line).unwrap();
        match event {
            ReplayEvent::SessionStarted {
                session_id,
                timestamp,
            } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(timestamp, "2024-01-01T00:00:00Z");
            }
            _ => panic!("expected SessionStarted, got {event:?}"),
        }
    }

    #[test]
    fn parse_claude_assistant_text() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"The answer is 42"}]}}"#;
        let event = parse_claude_line(line).unwrap();
        match event {
            ReplayEvent::AgentMessage { text } => {
                assert_eq!(text, "The answer is 42");
            }
            _ => panic!("expected AgentMessage, got {event:?}"),
        }
    }

    #[test]
    fn parse_claude_assistant_tool_use() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"},"id":"call-1"}]}}"#;
        let event = parse_claude_line(line).unwrap();
        match event {
            ReplayEvent::ToolUse { name, input, .. } => {
                assert_eq!(name, "Bash");
                assert_eq!(input["command"], "ls");
            }
            _ => panic!("expected ToolUse, got {event:?}"),
        }
    }

    #[test]
    fn parse_claude_user_tool_result() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"file.txt\nmain.rs"}]}}"#;
        let event = parse_claude_line(line).unwrap();
        match event {
            ReplayEvent::ToolResult {
                call_id,
                output,
                is_error,
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(output, "file.txt\nmain.rs");
                assert!(!is_error);
            }
            _ => panic!("expected ToolResult, got {event:?}"),
        }
    }

    #[test]
    fn parse_claude_user_tool_result_error() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-2","content":"command failed","is_error":true}]}}"#;
        let event = parse_claude_line(line).unwrap();
        match event {
            ReplayEvent::ToolResult { is_error, .. } => {
                assert!(is_error);
            }
            _ => panic!("expected ToolResult, got {event:?}"),
        }
    }

    #[test]
    fn parse_claude_result_turn_completed() {
        let line = r#"{"type":"result","result":"done","subtype":"success","is_error":false,"timestamp":"2024-01-01T00:00:05Z"}"#;
        let event = parse_claude_line(line).unwrap();
        match event {
            ReplayEvent::TurnCompleted { exit_code, .. } => {
                assert_eq!(exit_code, Some(0));
            }
            _ => panic!("expected TurnCompleted, got {event:?}"),
        }
    }

    #[test]
    fn parse_claude_result_error_exit_code() {
        let line =
            r#"{"type":"result","result":"error","subtype":"error_max_turns","is_error":true}"#;
        let event = parse_claude_line(line).unwrap();
        match event {
            ReplayEvent::TurnCompleted { exit_code, .. } => {
                assert_eq!(exit_code, Some(1));
            }
            _ => panic!("expected TurnCompleted, got {event:?}"),
        }
    }

    #[test]
    fn parse_claude_thinking_discarded() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me reason about this..."}]}}"#;
        let event = parse_claude_line(line).unwrap();
        assert!(matches!(event, ReplayEvent::Reasoning));
    }

    #[test]
    fn parse_claude_redacted_thinking_discarded() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"redacted_thinking","data":"redacted"}]}}"#;
        let event = parse_claude_line(line).unwrap();
        assert!(matches!(event, ReplayEvent::Reasoning));
    }

    #[test]
    fn parse_claude_thinking_then_text_returns_text() {
        // thinking 块后跟 text 块 - 应返回 AgentMessage，不返回 Reasoning
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"..."},{"type":"text","text":"The answer"}]}}"#;
        let event = parse_claude_line(line).unwrap();
        match event {
            ReplayEvent::AgentMessage { text } => {
                assert_eq!(text, "The answer");
            }
            _ => panic!("expected AgentMessage, got {event:?}"),
        }
    }

    #[test]
    fn parse_claude_user_text_message() {
        // 用户文本消息（content 是字符串而非数组）
        let line = r#"{"type":"user","message":{"role":"user","content":"what is 2+2?"}}"#;
        let event = parse_claude_line(line).unwrap();
        match event {
            ReplayEvent::AgentMessage { text } => {
                assert_eq!(text, "what is 2+2?");
            }
            _ => panic!("expected AgentMessage, got {event:?}"),
        }
    }

    #[test]
    fn parse_claude_unknown_type_returns_none() {
        let line = r#"{"type":"unknown","data":"foo"}"#;
        assert!(parse_claude_line(line).is_none());
    }

    #[test]
    fn parse_claude_invalid_json_returns_none() {
        assert!(parse_claude_line("not json").is_none());
        assert!(parse_claude_line("").is_none());
    }

    // ── ReplayParser feed ───────────────────────────────────────────

    #[test]
    fn feed_complete_codex_lines() {
        let mut parser = ReplayParser::new();
        let data = concat!(
            r#"{"type":"session_meta","payload":{"id":"s1","timestamp":"t0"}}"#,
            "\n",
            r#"{"type":"turn.started","turn_id":"t1","timestamp":"t1"}"#,
            "\n",
            r#"{"type":"turn.completed","turn_id":"t1","exit_code":0,"timestamp":"t2"}"#,
            "\n",
        );
        parser.feed(data);
        assert_eq!(parser.events().len(), 3);
        assert!(matches!(
            parser.events()[0],
            ReplayEvent::SessionStarted { .. }
        ));
        assert!(matches!(
            parser.events()[1],
            ReplayEvent::TurnStarted { .. }
        ));
        assert!(matches!(
            parser.events()[2],
            ReplayEvent::TurnCompleted { .. }
        ));
    }

    #[test]
    fn feed_partial_line_buffered() {
        let mut parser = ReplayParser::new();

        // 喂入不完整的行（无换行符）
        parser.feed(r#"{"type":"reasoning"}"#);
        assert_eq!(parser.events().len(), 0); // 还没换行，不解析

        // 补全换行
        parser.feed("\n");
        assert_eq!(parser.events().len(), 1);
        assert!(matches!(parser.events()[0], ReplayEvent::Reasoning));
    }

    #[test]
    fn feed_line_split_across_feeds() {
        let mut parser = ReplayParser::new();

        // 第一段
        parser.feed(r#"{"type":"turn.st"#);
        assert!(parser.events().is_empty());

        // 第二段补全
        parser.feed(r#"arted","turn_id":"t1","timestamp":"ts"}"#);
        assert!(parser.events().is_empty()); // 还没换行

        // 换行
        parser.feed("\n");
        assert_eq!(parser.events().len(), 1);
        match &parser.events()[0] {
            ReplayEvent::TurnStarted { turn_id, .. } => {
                assert_eq!(turn_id, "t1");
            }
            _ => panic!("expected TurnStarted"),
        }
    }

    #[test]
    fn feed_mixed_codex_and_claude_lines() {
        let mut parser = ReplayParser::new();
        let data = concat!(
            r#"{"type":"system","subtype":"init","session_id":"c1","timestamp":"t0"}"#,
            "\n",
            r#"{"type":"turn.started","turn_id":"t1","timestamp":"t1"}"#,
            "\n",
        );
        parser.feed(data);
        assert_eq!(parser.events().len(), 2);
        assert!(matches!(
            parser.events()[0],
            ReplayEvent::SessionStarted { .. }
        ));
        assert!(matches!(
            parser.events()[1],
            ReplayEvent::TurnStarted { .. }
        ));
    }

    #[test]
    fn feed_empty_lines_skipped() {
        let mut parser = ReplayParser::new();
        parser.feed("\n\n\n");
        assert!(parser.events().is_empty());
    }

    #[test]
    fn feed_unparseable_lines_skipped() {
        let mut parser = ReplayParser::new();
        let data = "not json line\n{\"type\":\"reasoning\"}\nalso not json\n";
        parser.feed(data);
        assert_eq!(parser.events().len(), 1);
        assert!(matches!(parser.events()[0], ReplayEvent::Reasoning));
    }

    #[test]
    fn feed_carriage_return_handled() {
        let mut parser = ReplayParser::new();
        parser.feed("{\"type\":\"reasoning\"}\r\n");
        assert_eq!(parser.events().len(), 1);
    }

    #[test]
    fn clear_resets_events() {
        let mut parser = ReplayParser::new();
        parser.feed("{\"type\":\"reasoning\"}\n");
        assert_eq!(parser.events().len(), 1);

        parser.clear();
        assert!(parser.events().is_empty());

        // 清除后仍可继续 feed
        parser.feed("{\"type\":\"reasoning\"}\n");
        assert_eq!(parser.events().len(), 1);
    }

    #[test]
    fn feed_multiple_lines_in_one_chunk() {
        let mut parser = ReplayParser::new();
        let data = concat!(
            r#"{"type":"item.completed","item":{"type":"agent_message","content":[{"type":"text","text":"msg1"}]}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","content":[{"type":"text","text":"msg2"}]}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","content":[{"type":"text","text":"msg3"}]}}"#,
            "\n",
        );
        parser.feed(data);
        assert_eq!(parser.events().len(), 3);
    }

    // ── detect_format ───────────────────────────────────────────────

    #[test]
    fn detect_format_codex_session_meta() {
        let lines = [r#"{"type":"session_meta","payload":{"id":"abc"}}"#];
        assert_eq!(detect_format(&lines), Some(ExternalCli::Codex));
    }

    #[test]
    fn detect_format_codex_thread_started() {
        let lines = [r#"{"type":"thread.started","thread_id":"t1"}"#];
        assert_eq!(detect_format(&lines), Some(ExternalCli::Codex));
    }

    #[test]
    fn detect_format_codex_item() {
        let lines = [r#"{"type":"item.completed","item":{"type":"command_execution"}}"#];
        assert_eq!(detect_format(&lines), Some(ExternalCli::Codex));
    }

    #[test]
    fn detect_format_claude_system() {
        let lines = [r#"{"type":"system","subtype":"init","session_id":"s1"}"#];
        assert_eq!(detect_format(&lines), Some(ExternalCli::Claude));
    }

    #[test]
    fn detect_format_claude_assistant() {
        let lines = [
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
        ];
        assert_eq!(detect_format(&lines), Some(ExternalCli::Claude));
    }

    #[test]
    fn detect_format_claude_result() {
        let lines = [r#"{"type":"result","result":"done"}"#];
        assert_eq!(detect_format(&lines), Some(ExternalCli::Claude));
    }

    #[test]
    fn detect_format_claude_role_field() {
        let lines = [r#"{"role":"assistant","content":"hi"}"#];
        assert_eq!(detect_format(&lines), Some(ExternalCli::Claude));
    }

    #[test]
    fn detect_format_none_for_empty() {
        assert_eq!(detect_format(&[]), None);
        assert_eq!(detect_format(&["", "  "]), None);
    }

    #[test]
    fn detect_format_none_for_unparseable() {
        let lines = ["not json", "also not json"];
        assert_eq!(detect_format(&lines), None);
    }

    #[test]
    fn detect_format_skips_invalid_lines() {
        let lines = ["not a json line", r#"{"type":"system","subtype":"init"}"#];
        assert_eq!(detect_format(&lines), Some(ExternalCli::Claude));
    }

    #[test]
    fn detect_format_uses_first_match() {
        // Codex 行在前
        let lines = [
            r#"{"type":"thread.started"}"#,
            r#"{"type":"assistant","message":{}}"#,
        ];
        assert_eq!(detect_format(&lines), Some(ExternalCli::Codex));
    }
}
