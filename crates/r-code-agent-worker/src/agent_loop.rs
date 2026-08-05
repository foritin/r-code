//! Agent Loop 实现 -- 路径 B（自行封装）。
//!
//! 在 `LlmProvider` + `ToolHost` 之上实现单次迭代：
//! `model -> (stream) -> tool -> feedback -> model`。
//!
//! 流程：
//! 1. 调用 `provider.stream(request)` 获取事件流。
//! 2. `TextDelta` -> 累积文本，emit `AgentEvent::Message { delta: true }`。
//! 3. `ToolUseStart` / `ToolUseDelta` -> 跟踪工具调用与输入 JSON 累积。
//! 4. `ToolUseComplete` -> 收集同一轮工具调用；可证明安全的只读调用按最多 4 路并发执行，
//!    其余保持串行。每个调用 emit `AgentEvent::ToolCall` + `AgentEvent::ToolResult`。
//! 5. `Stop` -> 结束本次迭代。
//! 6. 追加 assistant 消息（含 Text + ToolUse 块）；若因工具调用停止，
//!    追加 user 消息（含 ToolResult 块）。
//!
//! [doc-04 §10 路径 B]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use hermes_core::{
    CompletionRequest, ContentBlock, LlmProvider, Message, Role, StreamEvent, ToolHost, ToolSpec,
    Usage,
};
use r_code_core::dto::AgentEvent;
use r_code_core::error::ProductError;

/// Provider 请求建立连接的最大等待（vendor 层无超时，F2 兜底）。
const LLM_PROVIDER_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// 流式响应两次事件之间的最大空闲。长推理可能数分钟无输出，10 分钟是安全上限。
const LLM_PROVIDER_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// 单轮 agent 循环的控制结果。
///
/// 可见事件经回调在产生时交付；调用方只需要根据此结果决定是否进入下一次模型请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentLoopOutcome {
    /// 本轮是否发起了工具调用；为真时模型需要收到工具结果后继续下一轮。
    pub had_tool_call: bool,
}

const MAX_PARALLEL_READ_TOOL_CALLS: usize = 4;
const AGENT_ABORT_POLL_INTERVAL: Duration = Duration::from_millis(25);
#[cfg(not(test))]
const TOOL_ABORT_CLEANUP_GRACE: Duration = Duration::from_secs(5);
#[cfg(test)]
const TOOL_ABORT_CLEANUP_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
struct PendingToolCall {
    id: String,
    name: String,
    input: serde_json::Value,
}

fn is_parallel_read_tool(tools: &[ToolSpec], name: &str) -> bool {
    matches!(
        name,
        "read_file" | "list_files" | "search" | "glob" | "git_status"
    ) && tools
        .iter()
        .find(|tool| tool.name == name)
        .is_some_and(|tool| !tool.requires_confirmation)
}

async fn execute_pending_tool(
    tool_host: &dyn ToolHost,
    call: &PendingToolCall,
    abort: Option<&AtomicBool>,
) -> Result<hermes_core::ToolCallOutcome, ProductError> {
    if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Ok(cancelled_tool_outcome(call));
    }

    let call_id = call.id.clone();
    let tool_name = call.name.clone();
    let execution = tool_host.call_with_id(&call_id, &tool_name, call.input.clone());
    tokio::pin!(execution);
    let Some(abort) = abort else {
        return execution.await.map_err(map_hermes_err);
    };

    loop {
        if abort.load(Ordering::Relaxed) {
            // Built-ins, Bash and MCP use this window to perform cooperative cleanup (including
            // process-tree termination and protocol cancellation). A broken host is force-dropped
            // after the bounded grace so the agent itself can always terminate.
            let _ = tokio::time::timeout(TOOL_ABORT_CLEANUP_GRACE, &mut execution).await;
            return Ok(cancelled_tool_outcome(call));
        }
        tokio::select! {
            result = &mut execution => return result.map_err(map_hermes_err),
            _ = tokio::time::sleep(AGENT_ABORT_POLL_INTERVAL) => {}
        }
    }
}

async fn execute_pending_tools(
    tool_host: &dyn ToolHost,
    tools: &[ToolSpec],
    calls: &[PendingToolCall],
    abort: Option<&AtomicBool>,
) -> Result<Vec<hermes_core::ToolCallOutcome>, ProductError> {
    let can_run_in_parallel = calls.len() > 1
        && calls
            .iter()
            .all(|call| is_parallel_read_tool(tools, &call.name));

    if !can_run_in_parallel {
        let mut outcomes = Vec::with_capacity(calls.len());
        for call in calls {
            if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                outcomes.push(cancelled_tool_outcome(call));
                continue;
            }
            outcomes.push(execute_pending_tool(tool_host, call, abort).await?);
        }
        return Ok(outcomes);
    }

    let mut outcomes = Vec::with_capacity(calls.len());
    for batch in calls.chunks(MAX_PARALLEL_READ_TOOL_CALLS) {
        if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            outcomes.extend(batch.iter().map(cancelled_tool_outcome));
            continue;
        }
        let results = futures::future::join_all(
            batch
                .iter()
                .map(|call| execute_pending_tool(tool_host, call, abort)),
        )
        .await;
        for result in results {
            outcomes.push(result?);
        }
    }
    Ok(outcomes)
}

fn cancelled_tool_outcome(call: &PendingToolCall) -> hermes_core::ToolCallOutcome {
    hermes_core::ToolCallOutcome {
        content: serde_json::json!({
            "status": "cancelled",
            "reason": format!(
                "{} did not complete because the agent run was interrupted",
                call.name
            )
        })
        .to_string(),
        is_error: true,
        metadata: None,
    }
}

/// 修复旧版本在中断工具期间可能留下的悬空 `ToolUse`。
///
/// Provider 要求 assistant 发出的每个工具调用在下一条 user 工具消息中都有对应结果。
/// 合成的错误结果不会重新执行工具，只把“已被中断”这一事实补回协议历史；修复后的
/// 工作集会随下一次 history snapshot 持久化，从而让既有受损会话自动恢复。
fn repair_dangling_tool_uses(messages: &mut Vec<Message>) -> usize {
    let mut index = 0usize;
    let mut repaired = 0usize;

    while index < messages.len() {
        let tool_use_ids = if messages[index].role == Role::Assistant {
            messages[index]
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if tool_use_ids.is_empty() {
            index += 1;
            continue;
        }

        let recorded_results = messages
            .get(index + 1)
            .filter(|message| message.role == Role::User)
            .map(|message| {
                message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                        _ => None,
                    })
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let missing = tool_use_ids
            .into_iter()
            .filter(|id| !recorded_results.contains(id))
            .map(|tool_use_id| ContentBlock::ToolResult {
                tool_use_id,
                content: serde_json::json!({
                    "status": "cancelled",
                    "reason": "tool execution was interrupted before its result was recorded"
                })
                .to_string(),
                is_error: true,
            })
            .collect::<Vec<_>>();
        if missing.is_empty() {
            index += 1;
            continue;
        }

        repaired += missing.len();
        let next_is_tool_result_message = messages.get(index + 1).is_some_and(|message| {
            message.role == Role::User && message.content.iter().any(ContentBlock::is_tool_result)
        });
        if next_is_tool_result_message {
            messages[index + 1].content.extend(missing);
        } else {
            messages.insert(
                index + 1,
                Message {
                    role: Role::User,
                    content: missing,
                },
            );
        }
        index += 2;
    }

    repaired
}

/// 运行一次 agent 循环迭代。
///
/// `request` 提供模型 / 系统提示 / max_tokens 等标量配置；
/// `messages` 是工作消息集（会被追加 assistant 消息与可选的 tool_result 消息）；
/// `tools` 是可用工具规格。函数内部会将 `messages` 与 `tools` 同步进 request。
///
/// 返回本次迭代产生的 `AgentEvent` 列表。
///
/// 这是面向既有调用方与单元测试的聚合包装。真实 runtime 应使用
/// [`run_agent_loop_iteration_streaming_with_abort`]，以在事件产生时立即交付。
pub async fn run_agent_loop_iteration(
    provider: &dyn LlmProvider,
    tool_host: &dyn ToolHost,
    request: CompletionRequest,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
) -> Result<Vec<AgentEvent>, ProductError> {
    run_agent_loop_iteration_with_abort(provider, tool_host, request, messages, tools, None).await
}

/// 运行一次可中止的 agent 循环迭代。
///
/// `abort` 会在等待下一条流式事件及执行工具前被检查。取消时立即停止继续消费
/// HTTP 流并让调用方收尾；底层连接由 provider 流的 drop 释放。
pub async fn run_agent_loop_iteration_with_abort(
    provider: &dyn LlmProvider,
    tool_host: &dyn ToolHost,
    request: CompletionRequest,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
    abort: Option<&AtomicBool>,
) -> Result<Vec<AgentEvent>, ProductError> {
    let mut events = Vec::new();
    run_agent_loop_iteration_with_abort_and_emit(
        provider,
        tool_host,
        request,
        messages,
        tools,
        abort,
        false,
        |event| events.push(event),
    )
    .await?;
    Ok(events)
}

/// 运行一次可中止的 agent 循环迭代，并在每个用户可见事件产生时立即发送。
///
/// `event_tx` 的接收端由 runtime 的 `poll_events()` 排空。发送失败只代表 runtime
/// 已被销毁，不应让已开始的 provider 请求因此崩溃。
pub async fn run_agent_loop_iteration_streaming_with_abort(
    provider: &dyn LlmProvider,
    tool_host: &dyn ToolHost,
    request: CompletionRequest,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
    abort: Option<&AtomicBool>,
    event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
) -> Result<AgentLoopOutcome, ProductError> {
    run_agent_loop_iteration_with_abort_and_emit(
        provider,
        tool_host,
        request,
        messages,
        tools,
        abort,
        true,
        move |event| {
            let _ = event_tx.send(event);
        },
    )
    .await
}

/// 运行一次可中止迭代，并在事件产生时调用 `emit`。
///
/// 相比基于 channel 的包装，此入口让嵌套运行可为每个事件附加运行作用域，
/// 同时复用同一套文本、工具和中止语义。
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop_iteration_with_abort_and_emit<F>(
    provider: &dyn LlmProvider,
    tool_host: &dyn ToolHost,
    mut request: CompletionRequest,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
    abort: Option<&AtomicBool>,
    emit_activity: bool,
    mut emit: F,
) -> Result<AgentLoopOutcome, ProductError>
where
    F: FnMut(AgentEvent),
{
    let repaired_tool_results = repair_dangling_tool_uses(messages);
    if repaired_tool_results > 0 {
        tracing::warn!(
            repaired_tool_results,
            "repaired interrupted tool results before provider request"
        );
    }
    // 将工作集同步进 request（messages 是单一事实源）
    request.messages = messages.clone();
    request.tools = tools.to_vec();

    // Provider 建连本身也可能卡住；同时跟踪绝对 deadline 和 abort，
    // 取消时直接 drop vendor future，不再被固定 60s 超时窗口阻塞。
    let connection = provider.stream(request);
    tokio::pin!(connection);
    let connect_deadline = tokio::time::sleep(LLM_PROVIDER_CONNECT_TIMEOUT);
    tokio::pin!(connect_deadline);
    let connected = loop {
        if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Ok(AgentLoopOutcome {
                had_tool_call: false,
            });
        }
        break tokio::select! {
            result = &mut connection => result,
            _ = &mut connect_deadline => {
                return Err(map_hermes_err(hermes_error::Error::Provider(
                    "模型请求连接超时".to_string(),
                )))
            }
            _ = tokio::time::sleep(AGENT_ABORT_POLL_INTERVAL), if abort.is_some() => continue,
        };
    };
    let mut stream = connected.map_err(map_hermes_err)?;

    let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
    let mut current_text = String::new();
    // id -> (name, 累积的 input_json 片段)
    let mut pending_tools: HashMap<String, (String, String)> = HashMap::new();
    let mut tool_calls: Vec<PendingToolCall> = Vec::new();
    let mut tool_results: Vec<ContentBlock> = Vec::new();
    let mut total_usage = Usage::default();
    let mut streaming_started = false;
    let mut had_tool_call = false;

    loop {
        if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            break;
        }
        // 流式空闲超时：两次事件之间超过 LLM_PROVIDER_IDLE_TIMEOUT 视为卡死。
        let next = if abort.is_some() {
            tokio::select! {
                event = tokio::time::timeout(LLM_PROVIDER_IDLE_TIMEOUT, stream.next()) => {
                    match event {
                        Ok(Some(ev)) => Some(ev),
                        Ok(None) => None,
                        Err(_) => {
                            return Err(map_hermes_err(hermes_error::Error::Provider(
                                "模型流式响应空闲超时".to_string(),
                            )))
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => continue,
            }
        } else {
            match tokio::time::timeout(LLM_PROVIDER_IDLE_TIMEOUT, stream.next()).await {
                Ok(Some(ev)) => Some(ev),
                Ok(None) => None,
                Err(_) => {
                    return Err(map_hermes_err(hermes_error::Error::Provider(
                        "模型流式响应空闲超时".to_string(),
                    )))
                }
            }
        };
        let Some(ev) = next else {
            break;
        };
        match ev {
            StreamEvent::TextDelta { text } => {
                current_text.push_str(&text);
                if emit_activity && !streaming_started {
                    emit(AgentEvent::Activity {
                        phase: r_code_core::dto::AgentActivityPhase::Streaming,
                        detail: None,
                    });
                    streaming_started = true;
                }
                emit(AgentEvent::Message { text, delta: true });
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
                if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    break;
                }
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
                had_tool_call = true;
                if emit_activity {
                    emit(AgentEvent::Activity {
                        phase: r_code_core::dto::AgentActivityPhase::Tool,
                        // 工具名是安全的可观察信息；参数由独立 ToolCall 事件按需展示，
                        // 避免将潜在敏感路径或命令重复写入活动栏。
                        detail: Some(name.clone()),
                    });
                }
                emit(AgentEvent::ToolCall {
                    name: name.clone(),
                    input: input.clone(),
                    call_id: id.clone(),
                });
                tool_calls.push(PendingToolCall { id, name, input });
            }
            StreamEvent::HostedToolUse {
                id,
                name,
                input,
                provider_content,
            } => {
                if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    break;
                }
                flush_text(&mut current_text, &mut assistant_blocks);
                if let Some(block) = provider_content.and_then(provider_content_to_custom) {
                    assistant_blocks.push(block);
                }
                tracing::debug!(tool_id = %id, tool_name = %name, "provider-hosted tool use");
                if emit_activity {
                    emit(AgentEvent::Activity {
                        phase: r_code_core::dto::AgentActivityPhase::Tool,
                        detail: Some(name.clone()),
                    });
                }
                emit(AgentEvent::ToolCall {
                    name,
                    input,
                    call_id: id,
                });
            }
            StreamEvent::HostedToolResult {
                id,
                name: _,
                output,
                is_error,
                provider_content,
            } => {
                flush_text(&mut current_text, &mut assistant_blocks);
                if let Some(block) = provider_content.and_then(provider_content_to_custom) {
                    assistant_blocks.push(block);
                }
                emit(AgentEvent::ToolResult {
                    call_id: id,
                    output,
                    is_error,
                });
            }
            StreamEvent::Stop { reason } => {
                // Anthropic server tools normally finish inside one response. A rare
                // `pause_turn` asks the client to replay the provider blocks and continue;
                // treat that as a protocol continuation, never as a local tool execution.
                if matches!(reason, hermes_core::StopReason::Other(ref value) if value == "pause_turn")
                {
                    had_tool_call = true;
                }
                break;
            }
            StreamEvent::Usage(u) => {
                total_usage += u;
            }
        }
    }

    flush_text(&mut current_text, &mut assistant_blocks);

    if !tool_calls.is_empty() {
        let outcomes = execute_pending_tools(tool_host, tools, &tool_calls, abort).await?;
        for (call, outcome) in tool_calls.into_iter().zip(outcomes) {
            // 一旦 assistant 已经声明了一组 ToolUse，就必须为每个调用闭合协议对；
            // 即使并发期间收到中断，也会为未启动调用合成可记录的取消结果。
            let output_val: serde_json::Value = serde_json::from_str(&outcome.content)
                .unwrap_or(serde_json::Value::String(outcome.content.clone()));
            emit(AgentEvent::ToolResult {
                call_id: call.id.clone(),
                output: output_val,
                is_error: outcome.is_error,
            });
            tool_results.push(ContentBlock::ToolResult {
                tool_use_id: call.id,
                content: outcome.content,
                is_error: outcome.is_error,
            });
        }
    }

    // 追加 assistant 消息（含 Text + ToolUse 块）
    if !assistant_blocks.is_empty() {
        messages.push(Message {
            role: Role::Assistant,
            content: assistant_blocks,
        });
    }

    // 只要工具实际完成，就必须回填 ToolResult 供下一轮迭代使用。部分 OpenAI
    // 兼容流会在 ToolUseComplete 后直接关闭，不再额外发送 Stop(ToolUse)；
    // 不能因此丢掉工具结果并构造出不合法的 assistant.tool_calls 历史。
    if !tool_results.is_empty() {
        messages.push(Message {
            role: Role::User,
            content: tool_results,
        });
    }

    tracing::debug!(
        input_tokens = total_usage.input_tokens,
        output_tokens = total_usage.output_tokens,
        had_tool_call,
        "agent loop iteration complete"
    );

    Ok(AgentLoopOutcome { had_tool_call })
}

/// 将累积的文本刷出为 `Text` 内容块。
fn flush_text(current_text: &mut String, blocks: &mut Vec<ContentBlock>) {
    if !current_text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: std::mem::take(current_text),
        });
    }
}

fn provider_content_to_custom(content: serde_json::Value) -> Option<ContentBlock> {
    let mut data = content.as_object()?.clone();
    let type_name = data.remove("type")?.as_str()?.to_string();
    Some(ContentBlock::Custom {
        type_name,
        data: serde_json::Value::Object(data),
    })
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
        Capabilities, CompletionRequest, CompletionResponse, ContentBlock, LlmProvider, Message,
        Role, StopReason, StreamEvent, ToolCallOutcome, ToolHost, ToolSource, ToolSpec, Usage,
    };
    use hermes_error::{Error, Result};
    use hermes_llm::{MockProvider, RecordedTurn};
    use r_code_core::dto::AgentEvent;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;

    use super::{
        run_agent_loop_iteration, run_agent_loop_iteration_with_abort,
        run_agent_loop_iteration_with_abort_and_emit,
    };

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct PendingConnectProvider {
        started: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl LlmProvider for PendingConnectProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
            Err(Error::Internal(
                "PendingConnectProvider only supports stream".to_string(),
            ))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<futures::stream::BoxStream<'static, StreamEvent>> {
            let _drop = DropFlag(self.dropped.clone());
            self.started.store(true, Ordering::SeqCst);
            futures::future::pending::<()>().await;
            unreachable!("pending provider connection completed")
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                supports_streaming: true,
                supports_tool_use: true,
                supports_vision: false,
                supports_prompt_caching: false,
                max_context_tokens: 16_000,
            }
        }

        fn name(&self) -> &str {
            "pending-connect"
        }
    }

    /// 简单的回放 ToolHost：注册一个工具，调用时返回 `{ "echo": args }`。
    struct EchoToolHost {
        tools: Vec<ToolSpec>,
    }

    struct PendingToolHost {
        tools: Vec<ToolSpec>,
        started: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
        completed: Arc<AtomicBool>,
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

    #[async_trait]
    impl ToolHost for PendingToolHost {
        async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
            Ok(self.tools.clone())
        }

        async fn call(&self, _name: &str, _args: serde_json::Value) -> Result<ToolCallOutcome> {
            let _drop = DropFlag(self.dropped.clone());
            self.started.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(30)).await;
            self.completed.store(true, Ordering::SeqCst);
            Ok(ToolCallOutcome {
                content: "unexpected completion".to_string(),
                is_error: false,
                metadata: None,
            })
        }
    }

    /// 模拟工具在执行期间收到取消信号：工具仍然返回一个可记录的取消结果。
    struct AbortDuringToolHost {
        tools: Vec<ToolSpec>,
        abort: Arc<AtomicBool>,
        calls: AtomicUsize,
    }

    struct ConcurrencyToolHost {
        tools: Vec<ToolSpec>,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
    }

    impl ConcurrencyToolHost {
        fn new(tool_name: &str) -> Self {
            Self::with_tools(&[tool_name])
        }

        fn with_tools(tool_names: &[&str]) -> Self {
            Self {
                tools: tool_names
                    .iter()
                    .map(|tool_name| ToolSpec {
                        name: (*tool_name).to_string(),
                        description: "concurrency probe".to_string(),
                        input_schema: serde_json::json!({"type": "object"}),
                        source: ToolSource::Builtin,
                        requires_confirmation: false,
                    })
                    .collect(),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl ToolHost for ConcurrencyToolHost {
        async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
            Ok(self.tools.clone())
        }

        async fn call(&self, _name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(60)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolCallOutcome {
                content: args.to_string(),
                is_error: false,
                metadata: None,
            })
        }
    }

    #[async_trait]
    impl ToolHost for AbortDuringToolHost {
        async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
            Ok(self.tools.clone())
        }

        async fn call(&self, _name: &str, _args: serde_json::Value) -> Result<ToolCallOutcome> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.abort.store(true, Ordering::Relaxed);
            Ok(ToolCallOutcome {
                content: serde_json::json!({
                    "status": "cancelled",
                    "reason": "run interrupted"
                })
                .to_string(),
                is_error: true,
                metadata: None,
            })
        }
    }

    fn base_request() -> CompletionRequest {
        CompletionRequest {
            model: "mock".to_string(),
            system: None,
            messages: vec![Message::user_text("hi")],
            tools: vec![],
            hosted_tools: vec![],
            max_tokens: 128,
            temperature: None,
            enable_caching: false,
            inference: Default::default(),
        }
    }

    #[tokio::test]
    async fn abort_drops_a_provider_connection_future_without_waiting_for_connect_timeout() {
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let provider = PendingConnectProvider {
            started: started.clone(),
            dropped: dropped.clone(),
        };
        let tool_host = EchoToolHost::new("noop");
        let abort = Arc::new(AtomicBool::new(false));
        let cancel_abort = abort.clone();
        let cancel = tokio::spawn(async move {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            cancel_abort.store(true, Ordering::SeqCst);
        });
        let mut messages = vec![Message::user_text("hi")];

        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            run_agent_loop_iteration_with_abort_and_emit(
                &provider,
                &tool_host,
                base_request(),
                &mut messages,
                &[],
                Some(abort.as_ref()),
                false,
                |_| {},
            ),
        )
        .await
        .expect("provider connect cancellation must not wait for the 60 second timeout")
        .unwrap();
        cancel.await.unwrap();

        assert!(!outcome.had_tool_call);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn abort_force_drops_a_non_cooperative_active_tool_call() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "pending-tool".to_string(),
                name: "read_file".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "pending-tool".to_string(),
                input: serde_json::json!({"path": "README.md"}),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let tool_host = PendingToolHost {
            tools: vec![ToolSpec {
                name: "read_file".to_string(),
                description: "pending read".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                source: ToolSource::Builtin,
                requires_confirmation: false,
            }],
            started: started.clone(),
            dropped: dropped.clone(),
            completed: completed.clone(),
        };
        let abort = Arc::new(AtomicBool::new(false));
        let cancel_abort = abort.clone();
        let cancel = tokio::spawn(async move {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            cancel_abort.store(true, Ordering::SeqCst);
        });
        let mut messages = vec![Message::user_text("read")];

        let events = tokio::time::timeout(
            Duration::from_secs(1),
            run_agent_loop_iteration_with_abort(
                &provider,
                &tool_host,
                base_request(),
                &mut messages,
                &tool_host.tools,
                Some(abort.as_ref()),
            ),
        )
        .await
        .expect("active tool cancellation must be bounded")
        .unwrap();
        cancel.await.unwrap();

        assert!(dropped.load(Ordering::SeqCst));
        assert!(!completed.load(Ordering::SeqCst));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolResult {
                call_id,
                is_error: true,
                ..
            } if call_id == "pending-tool"
        )));
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
    async fn callback_streaming_emits_activity_before_first_text_delta() {
        let provider = MockProvider::new("mock");
        provider.push_text_turn("hello", Usage::default());
        let tool_host = EchoToolHost::new("noop");
        let mut messages = vec![Message::user_text("hi")];
        let mut events = Vec::new();

        let outcome = run_agent_loop_iteration_with_abort_and_emit(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &[],
            None,
            true,
            |event| events.push(event),
        )
        .await
        .unwrap();

        assert!(!outcome.had_tool_call);
        assert!(matches!(
            events.first(),
            Some(AgentEvent::Activity {
                phase: r_code_core::dto::AgentActivityPhase::Streaming,
                ..
            })
        ));
        assert!(matches!(
            events.get(1),
            Some(AgentEvent::Message { text, delta: true }) if text == "hello"
        ));
    }

    #[tokio::test]
    async fn provider_hosted_tool_is_observed_but_never_executed_locally() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::HostedToolUse {
                id: "srvtoolu_1".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query": "Rust"}),
                provider_content: Some(serde_json::json!({
                    "type": "server_tool_use",
                    "id": "srvtoolu_1",
                    "name": "web_search",
                    "input": {"query": "Rust"},
                })),
            },
            StreamEvent::HostedToolResult {
                id: "srvtoolu_1".into(),
                name: "web_search".into(),
                output: serde_json::json!({"sources": [{"url": "https://www.rust-lang.org"}]}),
                is_error: false,
                provider_content: Some(serde_json::json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_1",
                    "content": [{
                        "type": "web_search_result",
                        "url": "https://www.rust-lang.org",
                        "encrypted_content": "provider-private",
                    }],
                })),
            },
            StreamEvent::TextDelta {
                text: "Rust source summary".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]));
        let tool_host = EchoToolHost::new("web_search");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("search")];
        let mut events = Vec::new();

        let outcome = run_agent_loop_iteration_with_abort_and_emit(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &tools,
            None,
            true,
            |event| events.push(event),
        )
        .await
        .unwrap();

        assert!(!outcome.had_tool_call);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].text_content(), "Rust source summary");
        assert!(matches!(
            &messages[1].content[..],
            [
                ContentBlock::Custom { type_name: call, .. },
                ContentBlock::Custom { type_name: result, .. },
                ContentBlock::Text { .. },
            ] if call == "server_tool_use" && result == "web_search_tool_result"
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall { name, call_id, .. }
                if name == "web_search" && call_id == "srvtoolu_1"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolResult { call_id, output, .. }
                if call_id == "srvtoolu_1"
                    && output["sources"][0]["url"] == "https://www.rust-lang.org"
        )));
    }

    #[tokio::test]
    async fn provider_pause_turn_requests_continuation_without_local_execution() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::HostedToolUse {
                id: "srvtoolu_pause".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query": "Rust"}),
                provider_content: Some(serde_json::json!({
                    "type": "server_tool_use",
                    "id": "srvtoolu_pause",
                    "name": "web_search",
                    "input": {"query": "Rust"},
                })),
            },
            StreamEvent::Stop {
                reason: StopReason::Other("pause_turn".into()),
            },
        ]));
        let tool_host = EchoToolHost::new("web_search");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("search")];

        let outcome = run_agent_loop_iteration_with_abort_and_emit(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &tools,
            None,
            false,
            |_| {},
        )
        .await
        .unwrap();

        assert!(outcome.had_tool_call);
        assert!(matches!(
            &messages[1].content[..],
            [ContentBlock::Custom { type_name, .. }] if type_name == "server_tool_use"
        ));
    }

    #[tokio::test]
    async fn cancelled_iteration_does_not_consume_provider_stream() {
        let provider = MockProvider::new("mock");
        provider.push_text_turn("must not be emitted", Usage::new(10, 5));
        let tool_host = EchoToolHost::new("noop");
        let mut messages = vec![Message::user_text("hi")];
        let abort = AtomicBool::new(true);

        let events = run_agent_loop_iteration_with_abort(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &[],
            Some(&abort),
        )
        .await
        .unwrap();

        assert!(events.is_empty());
        assert_eq!(messages.len(), 1);
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
    async fn independent_read_tools_from_one_model_turn_execute_concurrently() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "read-a".to_string(),
                name: "read_file".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "read-a".to_string(),
                input: serde_json::json!({"path": "a.rs"}),
            },
            StreamEvent::ToolUseStart {
                id: "read-b".to_string(),
                name: "read_file".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "read-b".to_string(),
                input: serde_json::json!({"path": "b.rs"}),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let tool_host = ConcurrencyToolHost::new("read_file");
        let mut messages = vec![Message::user_text("inspect both")];
        let tools = tool_host.list_tools().await.unwrap();

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &tools)
                .await
                .unwrap();

        assert_eq!(tool_host.max_in_flight.load(Ordering::SeqCst), 2);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ToolResult { .. }))
                .count(),
            2,
        );
    }

    #[tokio::test]
    async fn mixed_read_and_write_tools_from_one_model_turn_remain_sequential() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "read-a".to_string(),
                name: "read_file".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "read-a".to_string(),
                input: serde_json::json!({"path": "a.rs"}),
            },
            StreamEvent::ToolUseStart {
                id: "edit-a".to_string(),
                name: "edit".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "edit-a".to_string(),
                input: serde_json::json!({"path": "a.rs", "old": "a", "new": "b"}),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let tool_host = ConcurrencyToolHost::with_tools(&["read_file", "edit"]);
        let mut messages = vec![Message::user_text("inspect then edit")];
        let tools = tool_host.list_tools().await.unwrap();

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &tools)
                .await
                .unwrap();

        assert_eq!(tool_host.max_in_flight.load(Ordering::SeqCst), 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ToolResult { .. }))
                .count(),
            2,
        );
    }

    #[tokio::test]
    async fn parallel_read_tools_are_bounded_to_four_at_a_time() {
        let provider = MockProvider::new("mock");
        let mut stream_events = Vec::new();
        for index in 0..5 {
            let id = format!("read-{index}");
            stream_events.push(StreamEvent::ToolUseStart {
                id: id.clone(),
                name: "read_file".to_string(),
            });
            stream_events.push(StreamEvent::ToolUseComplete {
                id,
                input: serde_json::json!({"path": format!("{index}.rs")}),
            });
        }
        stream_events.push(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        });
        provider.push_turn(RecordedTurn::ok(stream_events));
        let tool_host = ConcurrencyToolHost::new("read_file");
        let mut messages = vec![Message::user_text("inspect five files")];
        let tools = tool_host.list_tools().await.unwrap();

        run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &tools)
            .await
            .unwrap();

        assert_eq!(tool_host.max_in_flight.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn abort_during_tool_call_preserves_the_matching_tool_result() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "interrupted-tool".to_string(),
                name: "wait_for_children".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "interrupted-tool".to_string(),
                input: serde_json::json!({}),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let abort = Arc::new(AtomicBool::new(false));
        let tool_host = AbortDuringToolHost {
            tools: vec![ToolSpec {
                name: "wait_for_children".to_string(),
                description: "wait for delegated work".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                source: ToolSource::Builtin,
                requires_confirmation: false,
            }],
            abort: abort.clone(),
            calls: AtomicUsize::new(0),
        };
        let mut messages = vec![Message::user_text("start")];
        let tools = tool_host.list_tools().await.unwrap();

        let events = run_agent_loop_iteration_with_abort(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &tools,
            Some(abort.as_ref()),
        )
        .await
        .unwrap();

        assert!(abort.load(Ordering::Relaxed));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolResult {
                call_id,
                is_error: true,
                ..
            } if call_id == "interrupted-tool"
        )));
        assert_eq!(messages.len(), 3, "tool use must be closed before aborting");
        assert!(matches!(
            messages[2].content.as_slice(),
            [ContentBlock::ToolResult { tool_use_id, is_error: true, .. }]
                if tool_use_id == "interrupted-tool"
        ));
    }

    #[tokio::test]
    async fn abort_during_a_write_skips_later_declared_writes_but_closes_results() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "edit-a".to_string(),
                name: "edit".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "edit-a".to_string(),
                input: serde_json::json!({"path": "a.rs"}),
            },
            StreamEvent::ToolUseStart {
                id: "edit-b".to_string(),
                name: "edit".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "edit-b".to_string(),
                input: serde_json::json!({"path": "b.rs"}),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let abort = Arc::new(AtomicBool::new(false));
        let tool_host = AbortDuringToolHost {
            tools: vec![ToolSpec {
                name: "edit".to_string(),
                description: "edit a file".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                source: ToolSource::Builtin,
                requires_confirmation: false,
            }],
            abort: abort.clone(),
            calls: AtomicUsize::new(0),
        };
        let mut messages = vec![Message::user_text("edit two files")];
        let tools = tool_host.list_tools().await.unwrap();

        let events = run_agent_loop_iteration_with_abort(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &tools,
            Some(abort.as_ref()),
        )
        .await
        .unwrap();

        assert_eq!(tool_host.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ToolResult { .. }))
                .count(),
            2,
        );
        assert_eq!(messages.len(), 3, "every declared tool use must be closed");
    }

    #[tokio::test]
    async fn dangling_tool_use_from_an_interrupted_history_is_repaired_before_the_next_request() {
        let provider = MockProvider::new("mock");
        provider.push_text_turn("recovered", Usage::default());
        let tool_host = EchoToolHost::new("noop");
        let mut messages = vec![
            Message::user_text("start"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "orphaned-tool".to_string(),
                    name: "collect_subagents".to_string(),
                    input: serde_json::json!({}),
                }],
            },
            Message::user_text("continue after interrupt"),
        ];

        run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &[])
            .await
            .unwrap();

        assert_eq!(messages[2].role, Role::User);
        assert!(matches!(
            messages[2].content.as_slice(),
            [ContentBlock::ToolResult { tool_use_id, is_error: true, .. }]
                if tool_use_id == "orphaned-tool"
        ));
        assert_eq!(messages[3].text_content(), "continue after interrupt");
        assert_eq!(messages.last().unwrap().text_content(), "recovered");
    }

    #[tokio::test]
    async fn tool_result_is_preserved_when_stream_omits_tool_use_stop() {
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
            // 部分 OpenAI 兼容流在这里直接 EOF，不发送 Stop(ToolUse)。
        ]));
        let tool_host = EchoToolHost::new("echo");
        let mut messages = vec![Message::user_text("hi")];
        let tools = tool_host.list_tools().await.unwrap();

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &tools)
                .await
                .unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolResult {
                is_error: false,
                ..
            }
        )));
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].role, Role::User);
        assert!(messages[2]
            .content
            .iter()
            .any(|block| block.is_tool_result()));
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
