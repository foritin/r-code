//! Agent Loop 实现 -- 路径 B（自行封装）。
//!
//! 在 `LlmProvider` + `ToolHost` 之上实现单次迭代：
//! `model -> (stream) -> tool -> feedback -> model`。
//!
//! 流程：
//! 1. 调用 `provider.stream(request)` 获取事件流。
//! 2. `TextDelta` -> 累积文本，emit `AgentEvent::Message { delta: true }`。
//! 3. `ToolUseStart` / `ToolUseDelta` -> 跟踪工具调用与输入 JSON 累积。
//! 4. `ToolUseComplete` -> 调用 `tool_host.call()`，emit `AgentEvent::ToolCall` +
//!    `AgentEvent::ToolResult`，并将 `ToolResult` 块收集起来。
//! 5. `Stop` -> 结束本次迭代。
//! 6. 追加 assistant 消息（含 Text + ToolUse 块）；若因工具调用停止，
//!    追加 user 消息（含 ToolResult 块）。
//!
//! [doc-04 §10 路径 B]

use std::collections::HashMap;

use futures::StreamExt;
use hermes_core::{
    CompletionRequest, ContentBlock, LlmProvider, Message, Role, StopReason, StreamEvent, ToolHost,
    ToolSpec, Usage,
};
use r_code_core::dto::AgentEvent;
use r_code_core::error::ProductError;

/// 运行一次 agent 循环迭代。
///
/// `request` 提供模型 / 系统提示 / max_tokens 等标量配置；
/// `messages` 是工作消息集（会被追加 assistant 消息与可选的 tool_result 消息）；
/// `tools` 是可用工具规格。函数内部会将 `messages` 与 `tools` 同步进 request。
///
/// 返回本次迭代产生的 `AgentEvent` 列表。
pub async fn run_agent_loop_iteration(
    provider: &dyn LlmProvider,
    tool_host: &dyn ToolHost,
    mut request: CompletionRequest,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
) -> Result<Vec<AgentEvent>, ProductError> {
    // 将工作集同步进 request（messages 是单一事实源）
    request.messages = messages.clone();
    request.tools = tools.to_vec();

    let mut stream = provider.stream(request).await.map_err(map_hermes_err)?;

    let mut events: Vec<AgentEvent> = Vec::new();
    let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
    let mut current_text = String::new();
    let mut stop_reason = StopReason::EndTurn;
    // id -> (name, 累积的 input_json 片段)
    let mut pending_tools: HashMap<String, (String, String)> = HashMap::new();
    let mut tool_results: Vec<ContentBlock> = Vec::new();
    let mut total_usage = Usage::default();

    while let Some(ev) = stream.next().await {
        match ev {
            StreamEvent::TextDelta { text } => {
                current_text.push_str(&text);
                events.push(AgentEvent::Message { text, delta: true });
            }
            StreamEvent::ToolUseStart { id, name } => {
                tracing::debug!(tool_id = %id, tool_name = %name, "tool use start");
                flush_text(&mut current_text, &mut assistant_blocks);
                pending_tools.insert(id, (name, String::new()));
            }
            StreamEvent::ToolUseDelta { id, input_json } => {
                if let Some((_, acc)) = pending_tools.get_mut(&id) {
                    acc.push_str(&input_json);
                }
            }
            StreamEvent::ToolUseComplete { id, input } => {
                flush_text(&mut current_text, &mut assistant_blocks);
                let name = pending_tools
                    .remove(&id)
                    .map(|(n, _)| n)
                    .unwrap_or_default();
                // 追加 ToolUse 块到 assistant 消息
                assistant_blocks.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                // emit ToolCall 事件
                events.push(AgentEvent::ToolCall {
                    name: name.clone(),
                    input: input.clone(),
                    call_id: id.clone(),
                });
                // 调用工具主机
                let outcome = tool_host.call(&name, input).await.map_err(map_hermes_err)?;
                let output_val: serde_json::Value = serde_json::from_str(&outcome.content)
                    .unwrap_or(serde_json::Value::String(outcome.content.clone()));
                events.push(AgentEvent::ToolResult {
                    call_id: id.clone(),
                    output: output_val,
                    is_error: outcome.is_error,
                });
                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: outcome.content,
                    is_error: outcome.is_error,
                });
            }
            StreamEvent::Stop { reason } => {
                stop_reason = reason;
                break;
            }
            StreamEvent::Usage(u) => {
                total_usage += u;
            }
        }
    }

    flush_text(&mut current_text, &mut assistant_blocks);

    // 追加 assistant 消息（含 Text + ToolUse 块）
    if !assistant_blocks.is_empty() {
        messages.push(Message {
            role: Role::Assistant,
            content: assistant_blocks,
        });
    }

    // 若因工具调用停止，追加 user 消息（含 ToolResult 块），供下一轮迭代
    if stop_reason == StopReason::ToolUse && !tool_results.is_empty() {
        messages.push(Message {
            role: Role::User,
            content: tool_results,
        });
    }

    tracing::debug!(
        input_tokens = total_usage.input_tokens,
        output_tokens = total_usage.output_tokens,
        events = events.len(),
        "agent loop iteration complete"
    );

    Ok(events)
}

/// 将累积的文本刷出为 `Text` 内容块。
fn flush_text(current_text: &mut String, blocks: &mut Vec<ContentBlock>) {
    if !current_text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: std::mem::take(current_text),
        });
    }
}

/// 将 `hermes_error::Error` 映射为 `ProductError`。
///
/// `ProductError` 与 `hermes_error::Error` 之间无 `From` 互转（公共层不依赖产品层），
/// 故在此显式映射常见变体，其余归入 `ProductError::Other`。
fn map_hermes_err(err: hermes_error::Error) -> ProductError {
    match err {
        hermes_error::Error::PermissionDenied(msg) => ProductError::PermissionError(msg),
        hermes_error::Error::Provider(msg) => ProductError::Other(format!("provider: {msg}")),
        hermes_error::Error::ToolHost(msg) => ProductError::Other(format!("tool host: {msg}")),
        hermes_error::Error::ToolNotFound(name) => {
            ProductError::Other(format!("tool not found: {name}"))
        }
        hermes_error::Error::ToolCallFailed { tool, message } => {
            ProductError::Other(format!("tool call failed ({tool}): {message}"))
        }
        other => ProductError::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use hermes_core::{
        CompletionRequest, Message, Role, StopReason, StreamEvent, ToolCallOutcome, ToolHost,
        ToolSource, ToolSpec, Usage,
    };
    use hermes_error::{Error, Result};
    use hermes_llm::{MockProvider, RecordedTurn};
    use r_code_core::dto::AgentEvent;

    use super::run_agent_loop_iteration;

    /// 简单的回放 ToolHost：注册一个工具，调用时返回 `{ "echo": args }`。
    struct EchoToolHost {
        tools: Vec<ToolSpec>,
    }

    impl EchoToolHost {
        fn new(tool_name: &str) -> Self {
            Self {
                tools: vec![ToolSpec {
                    name: tool_name.to_string(),
                    description: "echo tool for tests".to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                    source: ToolSource::Builtin,
                    requires_confirmation: false,
                }],
            }
        }
    }

    #[async_trait]
    impl ToolHost for EchoToolHost {
        async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
            Ok(self.tools.clone())
        }

        async fn call(&self, name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
            if name == self.tools[0].name {
                Ok(ToolCallOutcome {
                    content: serde_json::json!({ "echo": args }).to_string(),
                    is_error: false,
                    metadata: None,
                })
            } else {
                Err(Error::ToolNotFound(name.to_string()))
            }
        }
    }

    fn base_request() -> CompletionRequest {
        CompletionRequest {
            model: "mock".to_string(),
            system: None,
            messages: vec![Message::user_text("hi")],
            tools: vec![],
            max_tokens: 128,
            temperature: None,
            enable_caching: false,
        }
    }

    #[tokio::test]
    async fn text_delta_produces_message_events() {
        let provider = MockProvider::new("mock");
        provider.push_text_turn("hello", Usage::new(10, 5));
        let tool_host = EchoToolHost::new("noop");
        let mut messages = vec![Message::user_text("hi")];

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &[])
                .await
                .unwrap();

        // 至少一条增量 Message
        let msg_count = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Message { delta: true, .. }))
            .count();
        assert!(msg_count >= 1);

        // assistant 消息已追加，文本完整
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].text_content(), "hello");
    }

    #[tokio::test]
    async fn tool_use_produces_call_and_result_events() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "t1".to_string(),
                name: "echo".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "t1".to_string(),
                input: serde_json::json!({"x": 1}),
            },
            StreamEvent::Usage(Usage::new(5, 5)),
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let tool_host = EchoToolHost::new("echo");
        let mut messages = vec![Message::user_text("hi")];
        let tools = tool_host.list_tools().await.unwrap();

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &tools)
                .await
                .unwrap();

        // ToolCall + ToolResult 事件
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCall { name, .. } if name == "echo"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResult {
                is_error: false,
                ..
            }
        )));

        // user + assistant(tool_use) + user(tool_result)
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].role, Role::Assistant);
        assert!(messages[1].content.iter().any(|b| b.is_tool_use()));
        assert_eq!(messages[2].role, Role::User);
        assert!(messages[2].content.iter().any(|b| b.is_tool_result()));
    }

    #[tokio::test]
    async fn mixed_text_and_tool_use_preserves_order() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::TextDelta {
                text: "Let me check ".to_string(),
            },
            StreamEvent::TextDelta {
                text: "that".to_string(),
            },
            StreamEvent::ToolUseStart {
                id: "t1".to_string(),
                name: "echo".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "t1".to_string(),
                input: serde_json::json!({"q": 1}),
            },
            StreamEvent::Usage(Usage::new(5, 5)),
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let tool_host = EchoToolHost::new("echo");
        let mut messages = vec![Message::user_text("hi")];
        let tools = tool_host.list_tools().await.unwrap();

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &tools)
                .await
                .unwrap();

        // 两个增量文本事件按序到达
        let text_deltas: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Message { text, delta: true } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text_deltas, vec!["Let me check ", "that"]);

        // assistant 消息文本完整拼接，且含 ToolUse 块
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].text_content(), "Let me check that");
        assert!(messages[1].content.iter().any(|b| b.is_tool_use()));
    }

    #[tokio::test]
    async fn provider_error_propagates() {
        let provider = MockProvider::new("mock");
        provider.push_error_turn(Error::AuthFailed("bad key".to_string()));
        let tool_host = EchoToolHost::new("noop");
        let mut messages = vec![Message::user_text("hi")];

        let err =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &[])
                .await
                .unwrap_err();

        assert!(matches!(err, r_code_core::error::ProductError::Other(_)));
        assert!(err.to_string().contains("bad key"));
        // 未追加任何消息
        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn end_turn_does_not_append_tool_result_message() {
        let provider = MockProvider::new("mock");
        provider.push_text_turn("done", Usage::new(1, 1));
        let tool_host = EchoToolHost::new("noop");
        let mut messages = vec![Message::user_text("hi")];

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &[])
                .await
                .unwrap();

        assert!(!events.is_empty());
        // 仅 user + assistant，无额外 tool_result 消息
        assert_eq!(messages.len(), 2);
    }

    #[tokio::test]
    async fn tool_use_delta_accumulates_input() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "t1".to_string(),
                name: "echo".to_string(),
            },
            StreamEvent::ToolUseDelta {
                id: "t1".to_string(),
                input_json: "{\"a\":".to_string(),
            },
            StreamEvent::ToolUseDelta {
                id: "t1".to_string(),
                input_json: "1}".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "t1".to_string(),
                input: serde_json::json!({"a": 1}),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let tool_host = EchoToolHost::new("echo");
        let mut messages = vec![Message::user_text("hi")];
        let tools = tool_host.list_tools().await.unwrap();

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &tools)
                .await
                .unwrap();

        // ToolCall 携带完整 input
        let call = events.iter().find_map(|e| match e {
            AgentEvent::ToolCall { input, .. } => Some(input.clone()),
            _ => None,
        });
        assert_eq!(call, Some(serde_json::json!({"a": 1})));
    }

    #[tokio::test]
    async fn null_tool_host_yields_error_result_event() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "t1".to_string(),
                name: "missing".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "t1".to_string(),
                input: serde_json::json!({}),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        // NullToolHost 对任何调用返回 Error::ToolHost
        let tool_host = hermes_core::NullToolHost;
        let mut messages = vec![Message::user_text("hi")];

        let err =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &[])
                .await
                .unwrap_err();

        assert!(err.to_string().contains("tool host"));
    }
}
