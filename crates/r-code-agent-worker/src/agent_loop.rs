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
///
/// 注意：不含 `Copy`——`usage` 字段（hermes_core::Usage）不实现 Copy。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolMetadataObservation {
    pub tool_name: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct AgentLoopOutcome {
    /// 本轮是否发起了工具调用；为真时模型需要收到工具结果后继续下一轮。
    pub had_tool_call: bool,
    /// Successful metadata envelopes emitted by tools in this iteration. The run coordinator
    /// applies only explicitly allowlisted host-owned state updates before constructing the next
    /// model request; metadata is never added to model-visible ToolResult content.
    pub tool_metadata: Vec<ToolMetadataObservation>,
    /// P2-G：本轮累计真实 usage（provider 未报告时为全零）。调用方（run_loop）
    /// 用它反推 tokPerChar 校准分层压缩的 token 估算（docs/archive/deepseek-prefix-cache.md §5 P2-G）。
    pub usage: Usage,
    /// 本轮追加到请求工作集末尾的持久化协议消息。调用方用它更新 canonical
    /// transcript，而不需要把压缩投影或动态注入反向猜测/切片出来。
    pub appended_messages: Vec<Message>,
    /// Provider-hosted tools completed but the same response contained no visible answer. The
    /// coordinator must preserve the provider blocks, then make exactly one tool-free summary
    /// request instead of treating the opaque tool blocks as a successful final response.
    pub requires_final_summary_recovery: bool,
}

const MAX_PARALLEL_READ_TOOL_CALLS: usize = 4;
const AGENT_ABORT_POLL_INTERVAL: Duration = Duration::from_millis(25);

// P1-E：vendor 层流空闲 watchdog 的可恢复终止标记（openai.rs 与其一致）。
const STREAM_IDLE_TIMEOUT_REASON: &str = "stream_idle_timeout";
/// P1-E：流中断（未产出内容）时最多重放次数（Reasonix maxStreamRecoveries=5）。
const MAX_STREAM_RECOVERIES: u32 = 5;
/// P1-E：流恢复退避基数（500ms * 2^(n-1)，对齐 Reasonix run_loop.go）。
const STREAM_RECOVERY_BASE_MS: u64 = 500;
#[cfg(not(test))]
const TOOL_ABORT_CLEANUP_GRACE: Duration = Duration::from_secs(5);
#[cfg(test)]
const TOOL_ABORT_CLEANUP_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderStreamErrorDisposition {
    Retryable,
    Fatal,
}

/// Anthropic 兼容服务会在 HTTP 200 的 SSE 流里发送 `event: error`。Provider
/// 将其编码为 `<error_type>: <message>`；这里必须区分可恢复服务端故障和配置错误，
/// 绝不能把任一类当作正常 Stop 静默完成。
fn provider_stream_error_disposition(reason: &str) -> Option<ProviderStreamErrorDisposition> {
    let error_type = reason
        .split_once(':')
        .map_or(reason, |(error_type, _)| error_type)
        .trim()
        .to_ascii_lowercase();
    match error_type.as_str() {
        "overloaded_error" | "rate_limit_error" | "api_error" => {
            Some(ProviderStreamErrorDisposition::Retryable)
        }
        value if value.ends_with("_error") => Some(ProviderStreamErrorDisposition::Fatal),
        _ => None,
    }
}

fn public_provider_stream_error(
    reason: &str,
    disposition: ProviderStreamErrorDisposition,
) -> ProductError {
    let error_type = reason
        .split_once(':')
        .map_or(reason, |(error_type, _)| error_type)
        .trim()
        .to_ascii_lowercase();
    let message = match (disposition, error_type.as_str()) {
        (_, "authentication_error" | "permission_error") => "模型服务鉴权失败，请检查访问密钥",
        (ProviderStreamErrorDisposition::Fatal, "invalid_request_error") => {
            "模型服务拒绝了请求，请检查模型与线路配置"
        }
        (ProviderStreamErrorDisposition::Retryable, _) => {
            "模型服务暂时不可用，自动重试后仍未恢复，请稍后再试"
        }
        _ => "模型服务返回错误，请查看诊断日志",
    };
    ProductError::Other(message.to_string())
}

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
/// 合成的错误结果不会重新执行工具，只把“已被中断”这一事实补回协议历史。
///
/// P1-D 修复固化契约（PRD §5）：
/// 1. **幂等且一次固化**：对已健康的历史（每个 ToolUse 都有对应 ToolResult）本函数
///    零修改返回 0——调用方得以把历史原对象直接透传（零拷贝快路径思想）。修复只
///    发生在真正悬挂的位置，插入合成 ToolResult 后该位置立即健康，因此同一份历史
///    永远不会被修复两次，也不会重复插入。
/// 2. **修复结果随工作集固化**：本函数只改写 `messages` 工作集；调用方负责把修复
///    后的工作集写回持久化历史——同 run 内由 `llm_runtime.rs` 迭代结束的
///    `session.messages = messages.clone()` 同步（后续轮直接透传健康历史），跨 run
///    由宿主收尾时的 `SessionEvent::HistorySnapshot` 落盘 JSONL（下次恢复即健康）。
/// 3. **已知缺口**：`llm_runtime.rs` 迭代 Err 路径（如 provider 连接失败）不同步
///    session，此时本函数的修复结果随工作集丢弃，下次 run 会重新修复（幂等、不
///    膨胀，但“修复一次固化”的保证降级为“成功收尾时固化”）。
pub(crate) fn repair_dangling_tool_uses(messages: &mut Vec<Message>) -> usize {
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
    // P1-D：线路侧一次性修复——只在真正悬挂时改写历史，健康历史零修改直接透传
    // （repair 返回 0 即原对象透传）。修复结果随工作集固化：同 run 后续轮由调用方
    // 从 session 取回的已是健康历史（不再重复扫描插入），跨 run 由宿主收尾的
    // HistorySnapshot 落盘。注意：迭代 Err 时调用方不同步 session，修复会随工作集
    // 丢弃并在下次 run 重做（幂等，见 repair_dangling_tool_uses 文档）。
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

    // P1-E：冻结请求供流中断重放（重试字节与首试逐字节一致，不破坏前缀缓存）。
    let frozen_request = request.clone();
    let mut stream_recoveries: u32 = 0;

    // Provider 建连本身也可能卡住；同时跟踪绝对 deadline 和 abort，
    // 取消时直接 drop vendor future，不再被固定 60s 超时窗口阻塞。
    // 连接阶段失败不在此重试（vendor 层 send_with_retry 已按 408/429/5xx
    // 与传输错误指数退避，abort 语义保持）。
    let outcome = 'attempt: loop {
        // 每轮重试重置流状态（重放语义：整轮从头再来，未产出内容才可重放）
        let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
        let mut current_text = String::new();
        // id -> (name, 累积的 input_json 片段)
        let mut pending_tools: HashMap<String, (String, String)> = HashMap::new();
        let mut tool_calls: Vec<PendingToolCall> = Vec::new();
        let mut tool_results: Vec<ContentBlock> = Vec::new();
        let mut tool_metadata: Vec<ToolMetadataObservation> = Vec::new();
        let mut total_usage = Usage::default();
        let mut streaming_started = false;
        let mut received_stream_event = false;
        let mut had_tool_call = false;
        let mut had_hosted_tool_activity = false;
        let mut provider_requested_continuation = false;

        let connection = provider.stream(frozen_request.clone());
        tokio::pin!(connection);
        let connect_deadline = tokio::time::sleep(LLM_PROVIDER_CONNECT_TIMEOUT);
        tokio::pin!(connect_deadline);
        let connected = loop {
            if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return Ok(AgentLoopOutcome {
                    had_tool_call: false,
                    tool_metadata: Vec::new(),
                    usage: total_usage.clone(),
                    appended_messages: Vec::new(),
                    requires_final_summary_recovery: false,
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
            received_stream_event = true;
            match ev {
                StreamEvent::ReasoningDelta { text } => {
                    if text.is_empty() {
                        continue;
                    }
                    if !streaming_started {
                        if emit_activity {
                            emit(AgentEvent::Activity {
                                phase: r_code_core::dto::AgentActivityPhase::Streaming,
                                detail: None,
                            });
                        }
                        streaming_started = true;
                    }
                    emit(AgentEvent::Reasoning { text, delta: true });
                }
                StreamEvent::TextDelta { text } => {
                    current_text.push_str(&text);
                    if !streaming_started {
                        if emit_activity {
                            emit(AgentEvent::Activity {
                                phase: r_code_core::dto::AgentActivityPhase::Streaming,
                                detail: None,
                            });
                        }
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
                    had_hosted_tool_activity = true;
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
                    had_hosted_tool_activity = true;
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
                    let other_reason = match &reason {
                        hermes_core::StopReason::Other(value) => Some(value.as_str()),
                        _ => None,
                    };
                    // Anthropic server tools normally finish inside one response. A rare
                    // `pause_turn` asks the client to replay the provider blocks and continue;
                    // treat that as a protocol continuation, never as a local tool execution.
                    if other_reason == Some("pause_turn") {
                        had_tool_call = true;
                        provider_requested_continuation = true;
                    }
                    // P1-E：流空闲 watchdog（vendor 层 120s 无数据）以可恢复标记终止。
                    // 仅当本轮**尚未产出任何内容**（无文本、无工具、无块）时用冻结请求
                    // 重放——已产出内容后重放会造成重复输出，宁可直接结束（Reasonix
                    // 同语义）。abort 在退避期间可立即中断。
                    let idle_timeout = other_reason == Some(STREAM_IDLE_TIMEOUT_REASON);
                    let provider_error = other_reason.and_then(provider_stream_error_disposition);
                    let has_output = streaming_started
                        || !current_text.is_empty()
                        || !assistant_blocks.is_empty()
                        || !pending_tools.is_empty()
                        || !tool_calls.is_empty();
                    let retryable_provider_error =
                        provider_error == Some(ProviderStreamErrorDisposition::Retryable);
                    if (idle_timeout || retryable_provider_error)
                        && !has_output
                        && stream_recoveries < MAX_STREAM_RECOVERIES
                    {
                        stream_recoveries += 1;
                        let delay = Duration::from_millis(
                            STREAM_RECOVERY_BASE_MS * 2u64.pow(stream_recoveries - 1),
                        );
                        tracing::warn!(
                            attempt = stream_recoveries,
                            ?delay,
                            "provider stream idle before any output, replaying frozen request"
                        );
                        let recovery_deadline = tokio::time::sleep(delay);
                        tokio::pin!(recovery_deadline);
                        loop {
                            if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                                break;
                            }
                            tokio::select! {
                                _ = &mut recovery_deadline => break,
                                _ = tokio::time::sleep(AGENT_ABORT_POLL_INTERVAL), if abort.is_some() => continue,
                            }
                        }
                        if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                            break 'attempt Ok(AgentLoopOutcome {
                                had_tool_call: false,
                                tool_metadata: Vec::new(),
                                usage: total_usage.clone(),
                                appended_messages: Vec::new(),
                                requires_final_summary_recovery: false,
                            });
                        }
                        // P1-E §8：重试计数对用户可见——每次实际重放前发出事件，
                        // attempt 为本轮内第几次重放（agent 层统计，对齐 Reasonix
                        // RequestAttemptCounter；vendor 连接层重试不在此计数）。
                        emit(AgentEvent::StreamReplay {
                            attempt: stream_recoveries,
                        });
                        continue 'attempt;
                    }
                    if let (Some(detail), Some(disposition)) = (other_reason, provider_error) {
                        tracing::warn!(
                            provider_stream_error = %detail,
                            ?disposition,
                            stream_recoveries,
                            has_output,
                            "provider SSE error terminated the model turn"
                        );
                        return Err(public_provider_stream_error(detail, disposition));
                    }
                    if idle_timeout && !has_output {
                        return Err(ProductError::Other(
                            "模型流式响应中断，自动重试后仍未恢复".to_string(),
                        ));
                    }
                    break;
                }
                StreamEvent::Usage(u) => {
                    total_usage += u;
                }
            }
        }

        let was_aborted = abort.is_some_and(|flag| flag.load(Ordering::Relaxed));
        let requires_final_summary_recovery = !was_aborted
            && had_hosted_tool_activity
            && !provider_requested_continuation
            && !has_visible_assistant_text(&current_text, &assistant_blocks);
        if !was_aborted
            && !requires_final_summary_recovery
            && !has_persistable_assistant_output(&current_text, &assistant_blocks)
        {
            tracing::warn!(
                received_stream_event,
                pending_tool_count = pending_tools.len(),
                input_tokens = total_usage.input_tokens,
                output_tokens = total_usage.output_tokens,
                "provider stream ended without persistable assistant output"
            );
            return Err(ProductError::EmptyAssistantResponse);
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
                if !outcome.is_error {
                    if let Some(metadata) = outcome.metadata.clone() {
                        tool_metadata.push(ToolMetadataObservation {
                            tool_name: call.name.clone(),
                            metadata,
                        });
                    }
                }
                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: call.id,
                    content: outcome.content,
                    is_error: outcome.is_error,
                });
            }
        }

        let mut appended_messages = Vec::with_capacity(2);

        // 追加 assistant 消息（含 Text + ToolUse 块）
        if !assistant_blocks.is_empty() {
            let message = Message {
                role: Role::Assistant,
                content: assistant_blocks,
            };
            messages.push(message.clone());
            appended_messages.push(message);
        }

        // 只要工具实际完成，就必须回填 ToolResult 供下一轮迭代使用。部分 OpenAI
        // 兼容流会在 ToolUseComplete 后直接关闭，不再额外发送 Stop(ToolUse)；
        // 不能因此丢掉工具结果并构造出不合法的 assistant.tool_calls 历史。
        if !tool_results.is_empty() {
            let message = Message {
                role: Role::User,
                content: tool_results,
            };
            messages.push(message.clone());
            appended_messages.push(message);
        }

        // P0-B：把本轮累计 usage 经事件链暴露给宿主。Codex 线路在 run 完成时写库
        // （commands.rs set_usage），原生线路复用同一事件链与同一 JSON 形状——
        // hermes Usage 的 serde 输出键（input_tokens/output_tokens/cache_read_tokens/
        // cache_write_tokens）即前端 runUsageLabel 与 usage_json 列的契约。仅当
        // provider 报告了非零用量时发出，避免无 usage 的 provider 制造噪音事件。
        if total_usage.input_tokens > 0
            || total_usage.output_tokens > 0
            || total_usage.cache_read_tokens.unwrap_or(0) > 0
            || total_usage.cache_write_tokens.unwrap_or(0) > 0
        {
            let usage_json =
                serde_json::to_string(&total_usage).unwrap_or_else(|_| "{}".to_string());
            emit(AgentEvent::Usage { usage_json });
        }

        tracing::debug!(
            input_tokens = total_usage.input_tokens,
            output_tokens = total_usage.output_tokens,
            had_tool_call,
            "agent loop iteration complete"
        );

        break 'attempt Ok(AgentLoopOutcome {
            had_tool_call,
            tool_metadata,
            usage: total_usage.clone(),
            appended_messages,
            requires_final_summary_recovery,
        });
    };
    outcome
}

/// 将累积的文本刷出为 `Text` 内容块。
fn flush_text(current_text: &mut String, blocks: &mut Vec<ContentBlock>) {
    if !current_text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: std::mem::take(current_text),
        });
    }
}

fn has_persistable_assistant_output(current_text: &str, blocks: &[ContentBlock]) -> bool {
    !current_text.trim().is_empty()
        || blocks.iter().any(|block| match block {
            ContentBlock::Text { text } => !text.trim().is_empty(),
            ContentBlock::Thinking { thinking, .. } => !thinking.trim().is_empty(),
            _ => true,
        })
}

fn has_visible_assistant_text(current_text: &str, blocks: &[ContentBlock]) -> bool {
    !current_text.trim().is_empty()
        || blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if !text.trim().is_empty()))
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
    use super::STREAM_IDLE_TIMEOUT_REASON;
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
        Arc, Mutex,
    };
    use std::time::Duration;

    use super::{
        repair_dangling_tool_uses, run_agent_loop_iteration, run_agent_loop_iteration_with_abort,
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
        metadata: Option<serde_json::Value>,
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
                metadata: None,
            }
        }

        fn with_metadata(tool_name: &str, metadata: serde_json::Value) -> Self {
            let mut host = Self::new(tool_name);
            host.metadata = Some(metadata);
            host
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
                    metadata: self.metadata.clone(),
                })
            } else {
                Err(Error::ToolNotFound(name.to_string()))
            }
        }
    }

    /// 记录每次 `stream` 收到的请求消息，再回放一段固定文本。
    ///
    /// 用于断言后续请求透传的是修复后的健康历史（字节级），而非重新修复/重复插入。
    struct RecordingProvider {
        requests: Arc<Mutex<Vec<Vec<Message>>>>,
    }

    #[async_trait]
    impl LlmProvider for RecordingProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
            Err(Error::Internal(
                "RecordingProvider only supports stream".to_string(),
            ))
        }

        async fn stream(
            &self,
            request: CompletionRequest,
        ) -> Result<futures::stream::BoxStream<'static, StreamEvent>> {
            self.requests.lock().unwrap().push(request.messages);
            Ok(Box::pin(futures::stream::iter(vec![
                StreamEvent::TextDelta {
                    text: "recovered".to_string(),
                },
                StreamEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])))
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
            "recording"
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
    async fn provider_reasoning_is_emitted_separately_from_answer_text() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ReasoningDelta {
                text: "checking".into(),
            },
            StreamEvent::TextDelta {
                text: "answer".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]));
        let tool_host = EchoToolHost::new("noop");
        let mut messages = vec![Message::user_text("hi")];
        let mut events = Vec::new();

        run_agent_loop_iteration_with_abort_and_emit(
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

        assert!(matches!(
            events.as_slice(),
            [
                AgentEvent::Activity { .. },
                AgentEvent::Reasoning { text: reasoning, delta: true },
                AgentEvent::Message { text: answer, delta: true },
                ..
            ] if reasoning == "checking" && answer == "answer"
        ));
        assert_eq!(
            messages.last().map(Message::text_content).as_deref(),
            Some("answer")
        );
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
        assert!(!outcome.requires_final_summary_recovery);
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
    async fn hosted_tool_without_visible_text_preserves_provider_blocks_for_summary_recovery() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::HostedToolUse {
                id: "srvtoolu_empty".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query": "Rust"}),
                provider_content: Some(serde_json::json!({
                    "type": "server_tool_use",
                    "id": "srvtoolu_empty",
                    "name": "web_search",
                    "input": {"query": "Rust"},
                })),
            },
            StreamEvent::HostedToolResult {
                id: "srvtoolu_empty".into(),
                name: "web_search".into(),
                output: serde_json::json!({"sources": [{"url": "https://www.rust-lang.org"}]}),
                is_error: false,
                provider_content: Some(serde_json::json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_empty",
                    "content": [{"type": "web_search_result", "url": "https://www.rust-lang.org"}],
                })),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
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

        assert!(!outcome.had_tool_call);
        assert!(outcome.requires_final_summary_recovery);
        assert_eq!(outcome.appended_messages.len(), 1);
        assert!(matches!(
            &outcome.appended_messages[0].content[..],
            [
                ContentBlock::Custom { type_name: call, .. },
                ContentBlock::Custom { type_name: result, .. },
            ] if call == "server_tool_use" && result == "web_search_tool_result"
        ));
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
        assert!(!outcome.requires_final_summary_recovery);
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
    async fn successful_tool_metadata_reaches_the_run_coordinator_out_of_band() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "plan-call".to_string(),
                name: "plan_item_update".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "plan-call".to_string(),
                input: serde_json::json!({"state": "completed"}),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let metadata = serde_json::json!({
            "directive": {"type": "require_agent_continuation"},
            "data": {"r_code_authoritative_plan_view": {"plan": {"revision": 7}}}
        });
        let tool_host = EchoToolHost::with_metadata("plan_item_update", metadata.clone());
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("finish the feature")];

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

        assert_eq!(outcome.tool_metadata.len(), 1);
        assert_eq!(outcome.tool_metadata[0].tool_name, "plan_item_update");
        assert_eq!(outcome.tool_metadata[0].metadata, metadata);
        assert!(messages[2]
            .content
            .iter()
            .any(|block| block.is_tool_result()));
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

    #[test]
    fn repair_is_idempotent_and_healthy_history_is_passed_through_unchanged() {
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

        // 首次修复：插入一条合成 ToolResult，该位置立即健康。
        assert_eq!(repair_dangling_tool_uses(&mut messages), 1);
        assert!(matches!(
            messages[2].content.as_slice(),
            [ContentBlock::ToolResult { tool_use_id, is_error: true, .. }]
                if tool_use_id == "orphaned-tool"
        ));
        let repaired_bytes = serde_json::to_string(&messages).unwrap();

        // 幂等：已健康的历史再次修复返回 0，且逐字节不变——这是“健康历史直接
        // 透传原对象”（零拷贝快路径）的前提，保证修复动作只发生一次。
        assert_eq!(repair_dangling_tool_uses(&mut messages), 0);
        assert_eq!(serde_json::to_string(&messages).unwrap(), repaired_bytes);

        // 连续调用同样零修改。
        assert_eq!(repair_dangling_tool_uses(&mut messages), 0);
        assert_eq!(serde_json::to_string(&messages).unwrap(), repaired_bytes);
    }

    #[tokio::test]
    async fn repaired_history_is_persisted_and_passed_through_on_the_next_request() {
        let requests = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
        let provider = RecordingProvider {
            requests: requests.clone(),
        };
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

        // 第一次请求：修复在发送前一次性完成（线路侧），请求体已是修复后历史。
        run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &[])
            .await
            .unwrap();
        assert!(matches!(
            messages[2].content.as_slice(),
            [ContentBlock::ToolResult { tool_use_id, is_error: true, .. }]
                if tool_use_id == "orphaned-tool"
        ));

        // 模拟 session 落盘：等价于调用方迭代结束同步 session.messages（同 run）
        // 与宿主收尾写 SessionEvent::HistorySnapshot（跨 run）——修复结果随工作集固化。
        let persisted = messages.clone();

        // 模拟下一次 run 从持久化历史恢复（SessionStore::load + replace_history），
        // 再次发起请求。
        let mut restored = persisted.clone();
        run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut restored, &[])
            .await
            .unwrap();

        // 透传断言：第二次请求体与落盘的修复后历史逐字节一致——未重新修复、
        // 未重复插入，健康历史原样透传（DeepSeek 前缀稳定）。
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 2, "exactly two provider requests");
        assert_eq!(
            serde_json::to_string(&recorded[0]).unwrap(),
            serde_json::to_string(&messages[..4]).unwrap(),
            "first request must carry the repaired history"
        );
        assert_eq!(
            serde_json::to_string(&recorded[1]).unwrap(),
            serde_json::to_string(&persisted).unwrap(),
            "second request must pass through the repaired history unchanged"
        );

        // 无重复插入：第二次迭代只追加本轮产物，历史前缀与落盘快照逐字节一致。
        assert_eq!(restored.len(), persisted.len() + 1, "append-only growth");
        assert_eq!(
            serde_json::to_string(&restored[..persisted.len()]).unwrap(),
            serde_json::to_string(&persisted).unwrap(),
            "history prefix must stay byte-identical"
        );
        let synthetic_result_count = restored
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| block.is_tool_result() && block_content_is_synthetic(block))
            .count();
        assert_eq!(
            synthetic_result_count, 1,
            "synthetic cancelled result must be inserted exactly once"
        );
    }

    #[test]
    fn repair_closes_multiple_dangling_tool_uses_in_one_pass_without_touching_healthy_pairs() {
        let mut messages = vec![
            Message::user_text("start"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "dangling-a".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({}),
                }],
            },
            Message::user_text("interrupted before result"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "healthy-1".to_string(),
                        name: "search".to_string(),
                        input: serde_json::json!({}),
                    },
                    ContentBlock::ToolUse {
                        id: "dangling-b".to_string(),
                        name: "edit".to_string(),
                        input: serde_json::json!({}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "healthy-1".to_string(),
                    content: "ok".to_string(),
                    is_error: false,
                }],
            },
        ];

        // 一次修复闭合全部悬挂：dangling-a 单独插入新消息；dangling-b 并入既有
        // 结果消息；已配对的 healthy-1 保持原样。
        assert_eq!(repair_dangling_tool_uses(&mut messages), 2);
        let synthetic_ids: Vec<&str> = messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error: true,
                    ..
                } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(synthetic_ids, vec!["dangling-a", "dangling-b"]);

        let healthy = messages
            .iter()
            .flat_map(|message| &message.content)
            .find(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { tool_use_id, .. }
                        if tool_use_id.as_str() == "healthy-1"
                )
            })
            .expect("healthy-1 result must be preserved");
        assert!(matches!(
            healthy,
            ContentBlock::ToolResult { content, is_error: false, .. }
                if content.as_str() == "ok"
        ));

        // 幂等：再次修复零修改。
        assert_eq!(repair_dangling_tool_uses(&mut messages), 0);
    }

    #[tokio::test]
    async fn recoverable_provider_sse_error_before_output_replays_frozen_request() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![StreamEvent::Stop {
            reason: StopReason::Other("overloaded_error: temporarily overloaded".into()),
        }]));
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::TextDelta {
                text: "recovered".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];
        let mut events = Vec::new();

        run_agent_loop_iteration_with_abort_and_emit(
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

        assert_eq!(messages.last().unwrap().text_content(), "recovered");
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::StreamReplay { attempt: 1 })));
    }

    #[tokio::test]
    async fn fatal_provider_sse_error_is_never_reported_as_a_successful_empty_turn() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![StreamEvent::Stop {
            reason: StopReason::Other("invalid_request_error: unsupported model".into()),
        }]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];

        let error = run_agent_loop_iteration_with_abort_and_emit(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &tools,
            None,
            true,
            |_| {},
        )
        .await
        .expect_err("SSE error must fail the iteration");

        assert!(error.to_string().contains("拒绝了请求"));
        assert_eq!(
            messages.len(),
            1,
            "failed turns must not append an empty reply"
        );
    }

    #[tokio::test]
    async fn empty_or_metadata_only_streams_fail_without_appending_an_assistant_reply() {
        let cases = vec![
            vec![],
            vec![
                StreamEvent::Usage(Usage::new(12, 0)),
                StreamEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ],
        ];

        for stream_events in cases {
            let provider = MockProvider::new("mock");
            provider.push_turn(RecordedTurn::ok(stream_events));
            let tool_host = EchoToolHost::new("");
            let tools = tool_host.list_tools().await.unwrap();
            let mut messages = vec![Message::user_text("hi")];

            let error = run_agent_loop_iteration_with_abort_and_emit(
                &provider,
                &tool_host,
                base_request(),
                &mut messages,
                &tools,
                None,
                true,
                |_| {},
            )
            .await
            .expect_err("a content-free provider stream must fail the iteration");

            assert!(error.to_string().contains("模型服务"));
            assert_eq!(
                messages.len(),
                1,
                "content-free turns must not append an empty assistant reply"
            );
        }
    }

    /// P1-E：流空闲 watchdog 标记且未产出任何内容时，用冻结请求重放并成功产出。
    #[tokio::test]
    async fn idle_stream_before_any_output_replays_the_frozen_request() {
        let provider = MockProvider::new("mock");
        // 第一轮：未产出任何内容即空闲超时（vendor watchdog 的可恢复标记）。
        provider.push_turn(RecordedTurn::ok(vec![StreamEvent::Stop {
            reason: StopReason::Other(STREAM_IDLE_TIMEOUT_REASON.to_string()),
        }]));
        // 第二轮：重放后正常结束。
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::TextDelta {
                text: "recovered".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];
        let mut events = Vec::new();

        let outcome = run_agent_loop_iteration_with_abort_and_emit(
            &provider,
            &tool_host,
            CompletionRequest {
                model: "mock".into(),
                system: None,
                messages: messages.clone(),
                tools: tools.clone(),
                hosted_tools: vec![],
                max_tokens: 128,
                temperature: None,
                enable_caching: false,
                inference: Default::default(),
            },
            &mut messages,
            &tools,
            None,
            true,
            |event| events.push(event),
        )
        .await
        .unwrap();

        assert!(!outcome.had_tool_call);
        // 重放成功：最终消息只含一轮文本，无重复产出。
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Message { text, .. } if text == "recovered")));
        let assistant_text = messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .flat_map(|m| m.content.iter())
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(assistant_text, "recovered");
    }

    /// P1-E：已产出内容后的空闲超时**不重放**（避免重复输出），按中断正常收尾。
    #[tokio::test]
    async fn idle_stream_after_output_does_not_replay() {
        let provider = MockProvider::new("mock");
        // 唯一一轮：先产出文本，然后空闲超时标记。
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::TextDelta {
                text: "partial".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::Other(STREAM_IDLE_TIMEOUT_REASON.to_string()),
            },
        ]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];
        let mut events = Vec::new();

        let outcome = run_agent_loop_iteration_with_abort_and_emit(
            &provider,
            &tool_host,
            CompletionRequest {
                model: "mock".into(),
                system: None,
                messages: messages.clone(),
                tools: tools.clone(),
                hosted_tools: vec![],
                max_tokens: 128,
                temperature: None,
                enable_caching: false,
                inference: Default::default(),
            },
            &mut messages,
            &tools,
            None,
            true,
            |event| events.push(event),
        )
        .await
        .unwrap();

        assert!(!outcome.had_tool_call);
        // 只消费了一轮（mock 无剩余轮次也不报错：重放被跳过）。
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Message { text, .. } if text == "partial")));
        let assistant_text = messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .flat_map(|m| m.content.iter())
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(assistant_text, "partial");
    }

    /// P1-E §8：每次冻结请求重放发出 StreamReplay 事件，attempt 逐次递增
    /// （对齐 Reasonix RequestAttemptCounter 的 agent 层统计）。
    #[tokio::test]
    async fn stream_replay_emits_attempt_events_with_increasing_count() {
        let provider = MockProvider::new("mock");
        // 前两轮：未产出任何内容即空闲超时（vendor watchdog 的可恢复标记）。
        for _ in 0..2 {
            provider.push_turn(RecordedTurn::ok(vec![StreamEvent::Stop {
                reason: StopReason::Other(STREAM_IDLE_TIMEOUT_REASON.to_string()),
            }]));
        }
        // 第三轮：重放后正常结束。
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::TextDelta {
                text: "recovered".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];
        let mut events = Vec::new();

        let outcome = run_agent_loop_iteration_with_abort_and_emit(
            &provider,
            &tool_host,
            CompletionRequest {
                model: "mock".into(),
                system: None,
                messages: messages.clone(),
                tools: tools.clone(),
                hosted_tools: vec![],
                max_tokens: 128,
                temperature: None,
                enable_caching: false,
                inference: Default::default(),
            },
            &mut messages,
            &tools,
            None,
            true,
            |event| events.push(event),
        )
        .await
        .unwrap();

        assert!(!outcome.had_tool_call);
        let attempts: Vec<u32> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::StreamReplay { attempt } => Some(*attempt),
                _ => None,
            })
            .collect();
        assert_eq!(attempts, vec![1, 2]);
    }

    /// P1-E §8：未发生重放时不发 StreamReplay 事件（重试计数对用户不可见）。
    #[tokio::test]
    async fn no_stream_replay_event_without_idle_timeout() {
        let provider = MockProvider::new("mock");
        // 唯一一轮：正常产出并结束，无空闲超时。
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::TextDelta { text: "ok".into() },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];
        let mut events = Vec::new();

        let outcome = run_agent_loop_iteration_with_abort_and_emit(
            &provider,
            &tool_host,
            CompletionRequest {
                model: "mock".into(),
                system: None,
                messages: messages.clone(),
                tools: tools.clone(),
                hosted_tools: vec![],
                max_tokens: 128,
                temperature: None,
                enable_caching: false,
                inference: Default::default(),
            },
            &mut messages,
            &tools,
            None,
            true,
            |event| events.push(event),
        )
        .await
        .unwrap();

        assert!(!outcome.had_tool_call);
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::StreamReplay { .. })));
    }

    /// 判断 ToolResult 是否为修复合成的 cancelled 结果（内容为
    /// `{"reason": ..., "status": "cancelled"}` JSON 文本）。
    fn block_content_is_synthetic(block: &ContentBlock) -> bool {
        match block {
            ContentBlock::ToolResult {
                content,
                is_error: true,
                ..
            } => matches!(
                serde_json::from_str::<serde_json::Value>(content)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("status")
                            .and_then(|s| s.as_str())
                            .map(str::to_string)
                    })
                    .as_deref(),
                Some("cancelled")
            ),
            _ => false,
        }
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

    #[tokio::test]
    async fn usage_event_carries_run_usage_json_with_cache_fields() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::TextDelta {
                text: "hello".to_string(),
            },
            StreamEvent::Usage(Usage {
                input_tokens: 120,
                output_tokens: 30,
                cache_read_tokens: Some(80),
                cache_write_tokens: Some(40),
            }),
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]));
        let tool_host = EchoToolHost::new("noop");
        let mut messages = vec![Message::user_text("hi")];

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &[])
                .await
                .unwrap();

        // P0-B：单轮累计 usage 以 Usage 事件暴露给宿主，JSON 键与 Codex 路径写入
        // usage_json 列的形状一致（含 cache 字段），前端 runUsageLabel 可直接解析。
        let usage_event = events.iter().find_map(|e| match e {
            AgentEvent::Usage { usage_json } => Some(usage_json.as_str()),
            _ => None,
        });
        let parsed: serde_json::Value =
            serde_json::from_str(usage_event.expect("non-zero usage must emit Usage event"))
                .unwrap();
        assert_eq!(parsed["input_tokens"], 120);
        assert_eq!(parsed["output_tokens"], 30);
        assert_eq!(parsed["cache_read_tokens"], 80);
        assert_eq!(parsed["cache_write_tokens"], 40);
    }

    #[tokio::test]
    async fn zero_usage_suppresses_usage_event() {
        let provider = MockProvider::new("mock");
        provider.push_text_turn("hello", Usage::default());
        let tool_host = EchoToolHost::new("noop");
        let mut messages = vec![Message::user_text("hi")];

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &[])
                .await
                .unwrap();

        assert!(
            !events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })),
            "provider 未报告用量时不应发出 Usage 事件"
        );
    }
}
