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
use std::sync::Arc;
use std::time::Duration;

use agent_contract::{
    CompletionRequest, ContentBlock, LlmProvider, Message, Role, StreamEvent, ToolHost, ToolSpec,
    Usage,
};
use futures::StreamExt;
use r_code_core::dto::AgentEvent;
use r_code_core::error::ProductError;

use crate::run_guard::ToolObservation;

/// Provider 请求建立连接的最大等待（vendor 层无超时，F2 兜底）。
const LLM_PROVIDER_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// 流式响应两次事件之间的最大空闲。长推理可能数分钟无输出，10 分钟是安全上限。
const LLM_PROVIDER_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// 观察值中保留的输出文本上限：足以覆盖测试/构建的成功与失败标记。
const OBSERVATION_SNIPPET_CHARS: usize = 16_000;

/// 单轮 agent 循环的控制结果。
///
/// 可见事件经回调在产生时交付；调用方只需要根据此结果决定是否进入下一次模型请求。
///
/// 注意：不含 `Copy`——`usage` 字段（agent_contract::Usage）不实现 Copy。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolMetadataObservation {
    pub tool_name: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct AgentLoopOutcome {
    /// 本轮是否发起了工具调用；为真时模型需要收到工具结果后继续下一轮。
    pub had_tool_call: bool,
    /// 本轮流中断重放次数（F-obs-04 指标输入；语义见 P1-E 冻结重放）。
    pub stream_recoveries: u32,
    /// Successful metadata envelopes emitted by tools in this iteration. The run coordinator
    /// applies only explicitly allowlisted host-owned state updates before constructing the next
    /// model request; metadata is never added to model-visible ToolResult content.
    pub tool_metadata: Vec<ToolMetadataObservation>,
    /// P2-G：本轮累计真实 usage（provider 未报告时为全零）。调用方（run_loop）
    /// 用它反推 tokPerChar 校准分层压缩的 token 估算（docs/support/archive/deepseek-prefix-cache.md §5 P2-G）。
    pub usage: Usage,
    /// 本轮流式 `ReasoningDelta` 的 Unicode 字符总量。多数兼容接口尚未在 usage
    /// 中单列 reasoning tokens；runtime 以约 4 字符/token 的保守估算驱动软调控，
    /// 只改变下一轮推理档位，不截断当前输出。
    pub reasoning_chars: usize,
    /// 本轮追加到请求工作集末尾的持久化协议消息。调用方用它更新 canonical
    /// transcript，而不需要把压缩投影或动态注入反向猜测/切片出来。
    pub appended_messages: Vec<Message>,
    /// Provider-hosted tools completed but the same response contained no visible answer. The
    /// coordinator must preserve the provider blocks, then make exactly one tool-free summary
    /// request instead of treating the opaque tool blocks as a successful final response.
    pub requires_final_summary_recovery: bool,
    /// A provider-hosted web tool returned an explicit error result. The run coordinator may use
    /// this signal to make one controlled retry with the local web tools, without guessing from
    /// user-visible text or declaring hosted and local tools in the same request.
    pub hosted_web_failed: bool,
    /// 本轮每个工具调用的宿主侧观察值（名称、输入、错误码、退出码、输出片段）。
    /// `llm_runtime` 把它们喂给 `RunLoopGuard`，模型自己不能接触或伪造这些信号。
    pub tool_observations: Vec<ToolObservation>,
}

const MAX_PARALLEL_READ_TOOL_CALLS: usize = 4;
const AGENT_ABORT_POLL_INTERVAL: Duration = Duration::from_millis(25);

// P1-E：vendor 层流空闲 watchdog 的可恢复终止标记（openai.rs 与其一致）。
const STREAM_IDLE_TIMEOUT_REASON: &str = "stream_idle_timeout";
/// P1-E：流中断（未产出内容）时最多重放次数（Reasonix maxStreamRecoveries=5）。
const MAX_STREAM_RECOVERIES: u32 = 5;
/// P1-E：流恢复退避基数（500ms * 2^(n-1)，对齐 Reasonix run_loop.go）。
const STREAM_RECOVERY_BASE_MS: u64 = 500;
/// P1-E：流正常结束却未返回任何可展示内容时，重放冻结请求的次数上限。
/// 与流空闲重放同一语义：只重放**尚无任何输出**的轮次，指数退避，避免重复输出。
/// 空响应重放在运行时层由“无工具最终总结恢复”统一处理；此处若继续重放会消费
/// 后续脚本轮次，阻断该恢复路径，因此按 0 次禁用（保留常量以说明语义边界）。
const MAX_EMPTY_RESPONSE_RECOVERIES: u32 = 0;
/// 空响应恢复退避基数（500ms * 2^(n-1)）。
const EMPTY_RESPONSE_RECOVERY_BASE_MS: u64 = 500;
/// MaxTokens 终态分类上下文（docs/support/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §6.5）。
///
/// 旧的「任意空 MaxTokens 回合盲目 ×2 升档两次」策略已删除——它会把 headroom
/// 钳制成 1 的伪预算请求放大成 `1 → 2 → 4` 的无效重试序列。新规则：
/// - 请求在派发前已因上下文 headroom 被钳制 → `CONTEXT_CONSTRAINED_OUTPUT_EXHAUSTED`，不重放；
/// - 正常配置上限且无任何正文/工具调用 → `OUTPUT_BUDGET_EXHAUSTED`（记录
///   attempted/configured/provider ceiling/reasoning effort），不自动翻倍；
/// - 已有正文或工具调用 → 维持「不得整轮重放」（避免重复输出/重复执行）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputBudgetContext {
    /// 用户/自动配置的单轮输出上限（钳制前）。0 = 未知。
    pub configured: u32,
    /// Provider 声明的服务端输出上限；0 = 未声明。
    pub provider_ceiling: u32,
    /// 本次请求是否因上下文 headroom 被钳制到 configured 以下。
    pub headroom_clamped: bool,
}
#[cfg(not(test))]
const TOOL_ABORT_CLEANUP_GRACE: Duration = Duration::from_secs(5);
#[cfg(test)]
const TOOL_ABORT_CLEANUP_GRACE: Duration = Duration::from_millis(100);
/// Completion watchdog for ordinary opaque ToolHost futures. This is deliberately per call, not a
/// run deadline: after the timeout the model receives a normal error result and may retry with a
/// narrower input or choose another tool. Long-running tools with their own progress/cancellation
/// contract are excluded below.
#[cfg(not(test))]
const TOOL_NO_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
#[cfg(test)]
const TOOL_NO_COMPLETION_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderStreamErrorDisposition {
    Retryable,
    Fatal,
}

/// Anthropic 兼容服务会在 HTTP 200 的 SSE 流里发送 `event: error`。Provider
/// 将其编码为 `<error_type>: <message>`；这里必须区分可恢复服务端故障和配置错误，
/// 绝不能把任一类当作正常 Stop 静默完成。
fn provider_stream_error_disposition(reason: &str) -> Option<ProviderStreamErrorDisposition> {
    let error_type = provider_stream_error_type(reason)?;
    match error_type.as_str() {
        "overloaded_error" | "rate_limit_error" | "api_error" => {
            Some(ProviderStreamErrorDisposition::Retryable)
        }
        value if value.ends_with("_error") => Some(ProviderStreamErrorDisposition::Fatal),
        _ => None,
    }
}

fn provider_stream_error_type(reason: &str) -> Option<String> {
    let prefix = reason
        .split_once(':')
        .map_or(reason, |(error_type, _)| error_type)
        .trim()
        .to_ascii_lowercase();
    if prefix.ends_with("_error") {
        return Some(prefix);
    }
    reason
        .split(['(', ',', ')'])
        .map(str::trim)
        .find_map(|part| {
            part.strip_prefix("type=")
                .filter(|value| value.ends_with("_error"))
                .map(str::to_string)
        })
}

fn public_provider_stream_error(
    reason: &str,
    disposition: ProviderStreamErrorDisposition,
) -> ProductError {
    let error_type = provider_stream_error_type(reason).unwrap_or_default();
    let normalized = reason.to_ascii_lowercase();
    if disposition == ProviderStreamErrorDisposition::Fatal
        && error_type == "invalid_request_error"
        && (normalized.contains("web_search")
            || normalized.contains("web search")
            || normalized.contains("server tool")
            || normalized.contains("hosted tool"))
        && (normalized.contains("unsupported")
            || normalized.contains("not support")
            || normalized.contains("invalid")
            || normalized.contains("unknown"))
    {
        // Keep a stable, credential-free classification marker for the run coordinator. The raw
        // provider payload has already been logged by the transport; surfacing it here would leak
        // gateway details while discarding it entirely would make a safe one-shot fallback
        // impossible for SSE `event: error` responses.
        return ProductError::Other(
            "provider: hosted web tool is unsupported by this model route".to_string(),
        );
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EditIntent {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
    must_contain: Vec<String>,
    must_not_contain: Vec<String>,
}

fn normalized_tool_path(path: &str) -> String {
    #[cfg(windows)]
    let path = path
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .unwrap_or_else(|| path.strip_prefix(r"\\?\").unwrap_or(path).to_string())
        .replace('\\', "/")
        .to_ascii_lowercase();
    #[cfg(not(windows))]
    let path = path.to_string();

    let absolute = path.starts_with('/');
    let unc = path.starts_with("//");
    let mut components: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                let can_pop = components
                    .last()
                    .is_some_and(|previous| *previous != ".." && !previous.ends_with(':'));
                if can_pop {
                    components.pop();
                } else if !absolute {
                    components.push(component);
                }
            }
            _ => components.push(component),
        }
    }
    let joined = components.join("/");
    if unc {
        format!("//{joined}")
    } else if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

fn normalized_postcondition_literals(input: &serde_json::Value, key: &str) -> Vec<String> {
    let mut literals = input
        .get("postcondition")
        .and_then(serde_json::Value::as_object)
        .and_then(|condition| condition.get(key))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    literals.sort_unstable();
    literals.dedup();
    literals
}

fn edit_intent(input: &serde_json::Value) -> Option<EditIntent> {
    Some(EditIntent {
        path: normalized_tool_path(input.get("path")?.as_str()?),
        old_string: input.get("old_string")?.as_str()?.to_string(),
        new_string: input.get("new_string")?.as_str()?.to_string(),
        replace_all: input
            .get("replace_all")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        must_contain: normalized_postcondition_literals(input, "must_contain"),
        must_not_contain: normalized_postcondition_literals(input, "must_not_contain"),
    })
}

fn tool_result_error_code(content: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(code) = value.get("code").and_then(serde_json::Value::as_str) {
            return Some(code.to_string());
        }
    }
    if content.contains("'old_string' was not found") {
        return Some("old_string_not_found".to_string());
    }
    None
}

/// 从 bash 风格输出里解析 `exit: N`；缺失时再看结构化 metadata。
fn tool_outcome_exit_code(outcome: &agent_contract::ToolCallOutcome) -> Option<i32> {
    if let Some(metadata) = outcome.metadata.as_ref() {
        if let Some(code) = metadata
            .get("exit_code")
            .and_then(serde_json::Value::as_i64)
        {
            return i32::try_from(code).ok();
        }
    }
    let content = outcome.content.as_str();
    let rest = content.split("exit:").nth(1)?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<i32>().ok()
}

/// 把一次真实工具执行转换为宿主侧护栏观察值。错误码与退出码都由宿主从结果
/// 内容/元数据推导，模型无法在 ToolResult 之外伪造这些信号。
fn tool_observation(
    call: &PendingToolCall,
    outcome: &agent_contract::ToolCallOutcome,
) -> ToolObservation {
    let snippet: String = outcome
        .content
        .chars()
        .take(OBSERVATION_SNIPPET_CHARS)
        .collect();
    ToolObservation {
        name: call.name.clone(),
        input: call.input.clone(),
        is_error: outcome.is_error,
        error_code: tool_result_error_code(&outcome.content),
        exit_code: tool_outcome_exit_code(outcome),
        output_snippet: snippet,
    }
}

#[derive(Debug, Default)]
struct EditRetryRecord {
    stale_failures: usize,
    successful_reread_after_failure: bool,
}

/// Per-run stale-edit breaker. It is explicit state rather than a transcript scan because the
/// runtime appends dynamic user-role context messages on every provider request and may compact old
/// messages. Keeping the state beside the run also lets calls in one provider response update the
/// budget sequentially.
#[derive(Debug, Default)]
pub struct EditRetryGuard {
    records: HashMap<EditIntent, EditRetryRecord>,
}

impl EditRetryGuard {
    fn before_call(&self, call: &PendingToolCall) -> Option<agent_contract::ToolCallOutcome> {
        if call.name != "edit" {
            return None;
        }
        let intent = edit_intent(&call.input)?;
        let record = self.records.get(&intent)?;
        let (code, message) = if record.stale_failures >= 2 {
            (
                "repeated_stale_edit",
                "The same stale edit intent has already failed twice; unchanged execution is blocked.",
            )
        } else if !record.successful_reread_after_failure {
            (
                "reread_required",
                "This edit intent already failed against stale content; a successful read_file of the same path is required before another attempt.",
            )
        } else {
            return None;
        };
        Some(agent_contract::ToolCallOutcome {
            content: serde_json::json!({
                "status": "blocked",
                "tool": "edit",
                "code": code,
                "message": message,
                "details": {
                    "path": call.input.get("path").cloned().unwrap_or(serde_json::Value::Null),
                    "stale_failures": record.stale_failures,
                    "required_action": "Use read_file on this path, inspect whether the intended end state is already satisfied, and otherwise submit a materially different anchor or explicit postcondition from current content."
                }
            })
            .to_string(),
            is_error: true,
            metadata: None,
        })
    }

    fn observe(&mut self, call: &PendingToolCall, outcome: &agent_contract::ToolCallOutcome) {
        if call.name == "read_file" && !outcome.is_error {
            if let Some(path) = call
                .input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(normalized_tool_path)
            {
                for (intent, record) in &mut self.records {
                    if intent.path == path && record.stale_failures > 0 {
                        record.successful_reread_after_failure = true;
                    }
                }
            }
            return;
        }
        if call.name != "edit" {
            return;
        }
        let Some(intent) = edit_intent(&call.input) else {
            return;
        };
        if !outcome.is_error {
            self.records.remove(&intent);
            return;
        }
        if matches!(
            tool_result_error_code(&outcome.content).as_deref(),
            Some("old_string_not_found" | "stale_read" | "reread_required" | "repeated_stale_edit")
        ) {
            let record = self.records.entry(intent).or_default();
            record.stale_failures = record.stale_failures.saturating_add(1);
            record.successful_reread_after_failure = false;
        }
    }
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
) -> Result<agent_contract::ToolCallOutcome, ProductError> {
    if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Ok(cancelled_tool_outcome(call));
    }

    let call_id = call.id.clone();
    let tool_name = call.name.clone();
    let execution = tool_host.call_with_id(&call_id, &tool_name, call.input.clone());
    tokio::pin!(execution);
    let watchdog = ordinary_tool_watchdog(&tool_name);
    let Some(abort) = abort else {
        return match watchdog {
            Some(timeout) => match tokio::time::timeout(timeout, &mut execution).await {
                Ok(result) => result.map_err(map_agent_err),
                Err(_) => Ok(timed_out_tool_outcome(call, timeout)),
            },
            None => execution.await.map_err(map_agent_err),
        };
    };

    let deadline = tokio::time::sleep(watchdog.unwrap_or(Duration::from_secs(24 * 60 * 60)));
    tokio::pin!(deadline);
    loop {
        if abort.load(Ordering::Relaxed) {
            // Built-ins, Bash and MCP use this window to perform cooperative cleanup (including
            // process-tree termination and protocol cancellation). A broken host is force-dropped
            // after the bounded grace so the agent itself can always terminate.
            let _ = tokio::time::timeout(TOOL_ABORT_CLEANUP_GRACE, &mut execution).await;
            return Ok(cancelled_tool_outcome(call));
        }
        tokio::select! {
            result = &mut execution => return result.map_err(map_agent_err),
            _ = &mut deadline, if watchdog.is_some() => {
                return Ok(timed_out_tool_outcome(call, watchdog.expect("deadline exists")));
            }
            _ = tokio::time::sleep(AGENT_ABORT_POLL_INTERVAL) => {}
        }
    }
}

fn ordinary_tool_watchdog(name: &str) -> Option<Duration> {
    if matches!(
        name,
        "bash" | "delegate_task" | "collect_subagents" | "request_user_input" | "mcp_call"
    ) || name.starts_with("mcp__")
    {
        return None;
    }
    Some(TOOL_NO_COMPLETION_TIMEOUT)
}

fn timed_out_tool_outcome(
    call: &PendingToolCall,
    timeout: Duration,
) -> agent_contract::ToolCallOutcome {
    tracing::warn!(
        tool = %call.name,
        tool_call_id = %call.id,
        timeout_ms = timeout.as_millis(),
        "tool execution produced no completion before the per-call watchdog expired"
    );
    agent_contract::ToolCallOutcome {
        content: serde_json::json!({
            "status": "timeout",
            "reason": format!(
                "{} did not complete within the per-call safety window; retry with a narrower input or use another approach",
                call.name
            )
        })
        .to_string(),
        is_error: true,
        metadata: None,
    }
}

async fn execute_pending_tools(
    tool_host: &dyn ToolHost,
    tools: &[ToolSpec],
    calls: &[PendingToolCall],
    retry_guard: &mut EditRetryGuard,
    abort: Option<&AtomicBool>,
) -> Result<Vec<agent_contract::ToolCallOutcome>, ProductError> {
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
            if let Some(outcome) = retry_guard.before_call(call) {
                tracing::warn!(
                    tool_call_id = %call.id,
                    "blocked a repeated stale edit before gateway execution"
                );
                retry_guard.observe(call, &outcome);
                outcomes.push(outcome);
                continue;
            }
            let outcome = execute_pending_tool(tool_host, call, abort).await?;
            retry_guard.observe(call, &outcome);
            outcomes.push(outcome);
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
        for (call, result) in batch.iter().zip(results) {
            let outcome = result?;
            retry_guard.observe(call, &outcome);
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}

fn cancelled_tool_outcome(call: &PendingToolCall) -> agent_contract::ToolCallOutcome {
    agent_contract::ToolCallOutcome {
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
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop_iteration_streaming_with_abort(
    provider: &dyn LlmProvider,
    tool_host: &dyn ToolHost,
    request: CompletionRequest,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
    abort: Option<&AtomicBool>,
    retry_guard: &mut EditRetryGuard,
    event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    output_budget: OutputBudgetContext,
) -> Result<AgentLoopOutcome, ProductError> {
    run_agent_loop_iteration_with_abort_and_emit_with_retry_guard(
        provider,
        tool_host,
        request,
        messages,
        tools,
        abort,
        retry_guard,
        true,
        move |event| {
            let _ = event_tx.send(event);
        },
        output_budget,
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
    request: CompletionRequest,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
    abort: Option<&AtomicBool>,
    emit_activity: bool,
    emit: F,
) -> Result<AgentLoopOutcome, ProductError>
where
    F: FnMut(AgentEvent),
{
    let mut retry_guard = EditRetryGuard::default();
    run_agent_loop_iteration_with_abort_and_emit_with_retry_guard(
        provider,
        tool_host,
        request,
        messages,
        tools,
        abort,
        &mut retry_guard,
        emit_activity,
        emit,
        OutputBudgetContext::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_agent_loop_iteration_with_abort_and_emit_with_retry_guard<F>(
    provider: &dyn LlmProvider,
    tool_host: &dyn ToolHost,
    mut request: CompletionRequest,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
    abort: Option<&AtomicBool>,
    retry_guard: &mut EditRetryGuard,
    emit_activity: bool,
    mut emit: F,
    // MaxTokens 终态分类所需的输出预算上下文（docs §6.5）。不再用于升档钳制——
    // 自动 ×2 升档已删除，`1 → 2 → 4` 重试序列不得再出现。
    output_budget: OutputBudgetContext,
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
    // 重放只覆盖「流中断且尚无任何输出」的线路故障场景；MaxTokens 预算终态
    // 从不重放（docs §6.5），因此冻结请求在本次迭代内不再被改写。
    // request 在此之后不再使用：包成 Arc 冻结，重试只做 Arc 引用计数，
    // 不再整份深拷贝 messages/tools（F-perf-02）。
    let attempt_request = Arc::new(request);
    let mut stream_recoveries: u32 = 0;
    let mut empty_response_recoveries: u32 = 0;

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
        let mut tool_observations: Vec<ToolObservation> = Vec::new();
        let mut total_usage = Usage::default();
        let mut reasoning_chars = 0usize;
        // DeepSeek Responses 必须把明文 reasoning_text 回传到下一轮（否则 400）；
        // Kimi 等实测方言回传 thinking 以命中 prefix cache。是否回传由 provider
        // 的能力声明决定，其余 provider 保持不落历史。
        let preserve_plaintext_reasoning = provider.echoes_reasoning();
        let mut replay_reasoning = String::new();
        // DeepSeek V4 工具调用轮次可能返回空 reasoning_text。空内容不上屏，但仍需
        // 记住“本轮产生过 reasoning item”，下一轮才能回传空 item（否则 400）。
        let mut saw_plaintext_reasoning = false;
        let mut streaming_started = false;
        // A provider response has one forward-only visible phase: private reasoning may precede
        // the answer, but it must never resume after answer text has started. Some compatible
        // gateways deliver buffered reasoning deltas late; keep counting those deltas for the
        // soft reasoning governor, while preventing them from appearing below an answer already
        // shown to the user. A tool follow-up is a new provider request and gets a fresh gate.
        let mut answer_started = false;
        let mut received_stream_event = false;
        let mut had_tool_call = false;
        let mut had_hosted_tool_activity = false;
        let mut hosted_web_failed = false;
        let mut provider_requested_continuation = false;
        // 本轮是否以 MaxTokens 截断收尾：升档耗尽后用于区分「预算耗尽」与
        // 「服务未返回内容」两种空响应终态。
        let mut stop_reason_max_tokens = false;

        let connection = provider.stream(Arc::clone(&attempt_request));
        tokio::pin!(connection);
        let connect_deadline = tokio::time::sleep(LLM_PROVIDER_CONNECT_TIMEOUT);
        tokio::pin!(connect_deadline);
        let connected = loop {
            if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return Ok(AgentLoopOutcome {
                    had_tool_call: false,
                    stream_recoveries,
                    tool_metadata: Vec::new(),
                    usage: total_usage.clone(),
                    reasoning_chars,
                    appended_messages: Vec::new(),
                    requires_final_summary_recovery: false,
                    hosted_web_failed: false,
                    tool_observations: std::mem::take(&mut tool_observations),
                });
            }
            break tokio::select! {
                result = &mut connection => result,
                _ = &mut connect_deadline => {
                    return Err(map_agent_err(agent_error::Error::Provider(
                        "模型请求连接超时".to_string(),
                    )))
                }
                _ = tokio::time::sleep(AGENT_ABORT_POLL_INTERVAL), if abort.is_some() => continue,
            };
        };
        let mut stream = connected.map_err(map_agent_err)?;

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
                                return Err(map_agent_err(agent_error::Error::Provider(
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
                        return Err(map_agent_err(agent_error::Error::Provider(
                            "模型流式响应空闲超时".to_string(),
                        )));
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
                        if preserve_plaintext_reasoning {
                            saw_plaintext_reasoning = true;
                        }
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
                    reasoning_chars = reasoning_chars.saturating_add(text.chars().count());
                    if preserve_plaintext_reasoning {
                        replay_reasoning.push_str(&text);
                        saw_plaintext_reasoning = true;
                    }
                    if !answer_started {
                        emit(AgentEvent::Reasoning { text, delta: true });
                    }
                }
                StreamEvent::TextDelta { text } => {
                    if text.is_empty() {
                        continue;
                    }
                    answer_started = true;
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
                    name,
                    output,
                    is_error,
                    provider_content,
                } => {
                    flush_text(&mut current_text, &mut assistant_blocks);
                    had_hosted_tool_activity = true;
                    if is_error && is_hosted_web_tool_name(&name) {
                        hosted_web_failed = true;
                    }
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
                        agent_contract::StopReason::Other(value) => Some(value.as_str()),
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
                    // MaxTokens 截断（docs/support/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §6.5）：不再自动
                    // ×2 升档重放。空回合的终态分类在迭代收尾处按
                    // `output_budget.headroom_clamped` 完成——headroom 钳制后推理
                    // 仍耗尽属于上下文预算问题，重放只会再次超窗；正常上限耗尽
                    // 属于配置/任务规模问题。已有可执行产物（哪怕正文被截断）
                    // 的回合保持既有「不整轮重放」规则，避免重复输出/重复执行。
                    if matches!(reason, agent_contract::StopReason::MaxTokens) {
                        stop_reason_max_tokens = true;
                    }
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
                                stream_recoveries,
                                tool_metadata: Vec::new(),
                                usage: total_usage.clone(),
                                reasoning_chars,
                                appended_messages: Vec::new(),
                                requires_final_summary_recovery: false,
                                hosted_web_failed: false,
                                tool_observations: std::mem::take(&mut tool_observations),
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
            let has_output = streaming_started
                || !current_text.is_empty()
                || !assistant_blocks.is_empty()
                || !pending_tools.is_empty()
                || !tool_calls.is_empty();
            // 空流 + 无工具 + 无空闲超时：多为线路/代理瞬断。用冻结请求指数退避
            // 重放（仅在**完全无输出**时安全），耗尽后才把空响应提升为终态错误。
            // MAX_EMPTY_RESPONSE_RECOVERIES=0 时按策略不可达：空最终轮的恢复统一由
            // 运行时层的“无工具最终总结恢复”处理（主 run_loop 与原生子代理循环均
            // 已实现）；保留重放脚手架以便未来重新启用，故对恒假比较显式豁免。
            #[allow(clippy::absurd_extreme_comparisons)]
            if !has_output && empty_response_recoveries < MAX_EMPTY_RESPONSE_RECOVERIES {
                empty_response_recoveries += 1;
                let delay = Duration::from_millis(
                    EMPTY_RESPONSE_RECOVERY_BASE_MS * 2u64.pow(empty_response_recoveries - 1),
                );
                tracing::warn!(
                    attempt = empty_response_recoveries,
                    ?delay,
                    "provider stream ended without any output, replaying frozen request"
                );
                emit(AgentEvent::Activity {
                    phase: r_code_core::dto::AgentActivityPhase::Requesting,
                    detail: Some(format!(
                        "模型未返回内容，正在自动重试（{empty_response_recoveries}/{MAX_EMPTY_RESPONSE_RECOVERIES}）"
                    )),
                });
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
                        stream_recoveries,
                        tool_metadata: Vec::new(),
                        usage: total_usage.clone(),
                        reasoning_chars,
                        appended_messages: Vec::new(),
                        requires_final_summary_recovery: false,
                        hosted_web_failed: false,
                        tool_observations: std::mem::take(&mut tool_observations),
                    });
                }
                continue 'attempt;
            }
            tracing::warn!(
                received_stream_event,
                pending_tool_count = pending_tools.len(),
                input_tokens = total_usage.input_tokens,
                output_tokens = total_usage.output_tokens,
                empty_response_recoveries,
                max_tokens_exhausted = stop_reason_max_tokens,
                "provider stream ended without persistable assistant output"
            );
            // MaxTokens 截断导致的空回合是预算问题而非线路故障：按派发前的
            // 钳制状态分类报准确原因（docs §6.5），不自动重放、不翻倍。
            if stop_reason_max_tokens {
                if output_budget.headroom_clamped {
                    return Err(ProductError::ContextConstrainedOutputExhausted {
                        effective_output: attempt_request.max_tokens,
                    });
                }
                return Err(ProductError::OutputBudgetExhausted {
                    attempted: attempt_request.max_tokens,
                    configured: output_budget.configured,
                    provider_ceiling: output_budget.provider_ceiling,
                    reasoning_effort: attempt_request.inference.reasoning_effort.clone(),
                });
            }
            return Err(ProductError::EmptyAssistantResponse);
        }

        flush_text(&mut current_text, &mut assistant_blocks);

        // 回传方言：把本轮明文 reasoning 作为 Thinking 块插入 assistant 消息最前面，
        // 下一轮由协议适配器映射为 reasoning_content（Chat）或 thinking 块
        // （Anthropic）。EncryptedReplay 依赖 signature 块，无法从 delta 重建。
        // 空 reasoning 也要插入（DeepSeek V4 要求原样回传空 reasoning_text）；仅当
        // 本轮有可回传的产物（文本/工具等）时才携带，纯空 reasoning 轮仍按空响应处理。
        if preserve_plaintext_reasoning
            && (saw_plaintext_reasoning || !replay_reasoning.is_empty())
            && !assistant_blocks.is_empty()
        {
            assistant_blocks.insert(
                0,
                ContentBlock::Thinking {
                    thinking: std::mem::take(&mut replay_reasoning),
                    signature: None,
                },
            );
        }

        if !tool_calls.is_empty() {
            let outcomes =
                execute_pending_tools(tool_host, tools, &tool_calls, retry_guard, abort).await?;
            for (call, outcome) in tool_calls.into_iter().zip(outcomes) {
                // 一旦 assistant 已经声明了一组 ToolUse，就必须为每个调用闭合协议对；
                // 即使并发期间收到中断，也会为未启动调用合成可记录的取消结果。
                tool_observations.push(tool_observation(&call, &outcome));
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
        // 公共层（agent-contract）Usage 的 serde 输出键（input_tokens/output_tokens/cache_read_tokens/
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
            stream_recoveries,
            tool_metadata,
            usage: total_usage.clone(),
            reasoning_chars,
            appended_messages,
            requires_final_summary_recovery,
            hosted_web_failed,
            tool_observations,
        });
    };
    outcome
}

fn is_hosted_web_tool_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "web_search" | "web_fetch" | "web_extractor"
    )
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

/// 将 `agent_error::Error` 映射为 `ProductError`。
///
/// `ProductError` 与 `agent_error::Error` 之间无 `From` 互转（公共层不依赖产品层），
/// 故在此显式映射常见变体，其余归入 `ProductError::Other`。
fn map_agent_err(err: agent_error::Error) -> ProductError {
    match err {
        agent_error::Error::PermissionDenied(msg) => ProductError::PermissionError(msg),
        agent_error::Error::Provider(msg) => ProductError::Other(format!("provider: {msg}")),
        agent_error::Error::ToolHost(msg) => ProductError::Other(format!("tool host: {msg}")),
        agent_error::Error::ToolNotFound(name) => {
            ProductError::Other(format!("tool not found: {name}"))
        }
        agent_error::Error::ToolCallFailed { tool, message } => {
            ProductError::Other(format!("tool call failed ({tool}): {message}"))
        }
        other => ProductError::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::OutputBudgetContext;
    use super::STREAM_IDLE_TIMEOUT_REASON;
    use agent_contract::{
        Capabilities, CompletionRequest, CompletionResponse, ContentBlock, LlmProvider, Message,
        Role, StopReason, StreamEvent, ToolCallOutcome, ToolHost, ToolSource, ToolSpec, Usage,
    };
    use agent_error::{Error, Result};
    use agent_llm::{MockProvider, RecordedTurn};
    use async_trait::async_trait;
    use r_code_core::dto::AgentEvent;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use std::time::Duration;

    use super::{
        edit_intent, normalized_tool_path, ordinary_tool_watchdog, repair_dangling_tool_uses,
        run_agent_loop_iteration, run_agent_loop_iteration_with_abort,
        run_agent_loop_iteration_with_abort_and_emit,
        run_agent_loop_iteration_with_abort_and_emit_with_retry_guard, tool_observation,
        tool_outcome_exit_code, EditRetryGuard, PendingToolCall, OBSERVATION_SNIPPET_CHARS,
        TOOL_NO_COMPLETION_TIMEOUT,
    };

    /// 包装 `MockProvider`，声明需要回传明文 reasoning（DeepSeek Responses 语义）。
    struct ReasoningEchoProvider {
        inner: MockProvider,
    }

    impl ReasoningEchoProvider {
        fn new(name: &str) -> Self {
            Self {
                inner: MockProvider::new(name),
            }
        }

        fn push_turn(&self, turn: RecordedTurn) -> &Self {
            self.inner.push_turn(turn);
            self
        }
    }

    #[async_trait]
    impl LlmProvider for ReasoningEchoProvider {
        async fn complete(&self, request: Arc<CompletionRequest>) -> Result<CompletionResponse> {
            self.inner.complete(request).await
        }

        async fn stream(
            &self,
            request: Arc<CompletionRequest>,
        ) -> Result<futures::stream::BoxStream<'static, StreamEvent>> {
            self.inner.stream(request).await
        }

        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }

        fn echoes_reasoning(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            self.inner.name()
        }
    }

    fn edit_call(id: &str, old_string: &str) -> PendingToolCall {
        PendingToolCall {
            id: id.to_string(),
            name: "edit".to_string(),
            input: serde_json::json!({
                "path": "src/memory.rs",
                "old_string": old_string,
                "new_string": "    Undo,"
            }),
        }
    }

    fn stale_outcome(code: &str) -> ToolCallOutcome {
        ToolCallOutcome {
            content: serde_json::json!({ "status": "error", "code": code }).to_string(),
            is_error: true,
            metadata: None,
        }
    }

    fn successful_read_call(id: &str, path: &str) -> (PendingToolCall, ToolCallOutcome) {
        (
            PendingToolCall {
                id: id.to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({ "path": path }),
            },
            ToolCallOutcome {
                content: "latest contents".to_string(),
                is_error: false,
                metadata: None,
            },
        )
    }

    #[test]
    fn stale_edit_requires_a_successful_reread_before_retry() {
        let failed = edit_call("edit-1", "    Undo,\n    Undo,");
        let retry = edit_call("edit-2", "    Undo,\n    Undo,");
        let mut guard = EditRetryGuard::default();
        guard.observe(&failed, &stale_outcome("old_string_not_found"));

        let blocked = guard.before_call(&retry).expect("retry must be guarded");
        let payload: serde_json::Value = serde_json::from_str(&blocked.content).unwrap();
        assert!(blocked.is_error);
        assert_eq!(payload["code"], "reread_required");
    }

    #[test]
    fn materially_reread_stale_edit_gets_one_recovery_attempt() {
        let failed = edit_call("edit-1", "    Undo,\n    Undo,");
        let retry = edit_call("edit-2", "    Undo,\n    Undo,");
        let mut guard = EditRetryGuard::default();
        guard.observe(&failed, &stale_outcome("old_string_not_found"));
        let (read, outcome) = successful_read_call("read-1", "src/memory.rs");
        guard.observe(&read, &outcome);

        assert!(guard.before_call(&retry).is_none());
    }

    #[test]
    fn same_stale_edit_is_fused_after_two_failures_even_with_rereads() {
        let first = edit_call("edit-1", "    Undo,\n    Undo,");
        let second = edit_call("edit-2", "    Undo,\n    Undo,");
        let third = edit_call("edit-3", "    Undo,\n    Undo,");
        let mut guard = EditRetryGuard::default();
        guard.observe(&first, &stale_outcome("old_string_not_found"));
        let (read, outcome) = successful_read_call("read-1", "src/memory.rs");
        guard.observe(&read, &outcome);
        guard.observe(&second, &stale_outcome("old_string_not_found"));
        let (read, outcome) = successful_read_call("read-2", "src/memory.rs");
        guard.observe(&read, &outcome);

        let blocked = guard
            .before_call(&third)
            .expect("third attempt must be fused");
        let payload: serde_json::Value = serde_json::from_str(&blocked.content).unwrap();
        assert_eq!(payload["code"], "repeated_stale_edit");
        assert_eq!(payload["details"]["stale_failures"], 2);

        let changed_anchor = edit_call("edit-4", "    Agent,\n    Undo,");
        assert!(guard.before_call(&changed_anchor).is_none());
    }

    #[test]
    fn explicit_postcondition_is_a_materially_different_edit_intent() {
        let first = edit_call("edit-1", "    Undo,\n    Undo,");
        let mut with_postcondition = edit_call("edit-2", "    Undo,\n    Undo,");
        with_postcondition.input["postcondition"] = serde_json::json!({
            "must_contain": ["    Undo,"],
            "must_not_contain": ["    Undo,\n    Undo,"]
        });
        let mut guard = EditRetryGuard::default();
        guard.observe(&first, &stale_outcome("old_string_not_found"));
        guard.observe(&first, &stale_outcome("old_string_not_found"));

        assert_ne!(
            edit_intent(&first.input),
            edit_intent(&with_postcondition.input)
        );
        assert!(guard.before_call(&with_postcondition).is_none());
    }

    #[test]
    fn edit_guard_collapses_lexical_path_aliases() {
        assert_eq!(
            normalized_tool_path("src/memory.rs"),
            normalized_tool_path("./src/memory.rs")
        );
        assert_eq!(
            normalized_tool_path("src/memory.rs"),
            normalized_tool_path("src/sub/../memory.rs")
        );
        assert_eq!(
            normalized_tool_path("src//memory.rs"),
            normalized_tool_path("src/memory.rs")
        );
        #[cfg(windows)]
        assert_eq!(
            normalized_tool_path(r"SRC\MEMORY.rs"),
            normalized_tool_path("src/memory.rs")
        );
    }

    #[tokio::test]
    async fn stale_edit_guard_is_visible_in_both_events_and_model_history() {
        let failed = edit_call("edit-1", "    Undo,\n    Undo,");
        let retry = edit_call("edit-2", "    Undo,\n    Undo,");
        let mut retry_guard = EditRetryGuard::default();
        retry_guard.observe(&failed, &stale_outcome("old_string_not_found"));
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: retry.id.clone(),
                name: retry.name.clone(),
            },
            StreamEvent::ToolUseComplete {
                id: retry.id.clone(),
                input: retry.input.clone(),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let tool_host = EchoToolHost::new("edit");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![
            Message::user_text("remove the duplicate"),
            Message::user_text("Current local time: 2026-08-14T12:00 (+08:00)."),
            Message::user_text("Plan mode is not active."),
        ];
        let mut events = Vec::new();
        run_agent_loop_iteration_with_abort_and_emit_with_retry_guard(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &tools,
            None,
            &mut retry_guard,
            false,
            |event| events.push(event),
            OutputBudgetContext::default(),
        )
        .await
        .unwrap();

        let event_payload = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolResult {
                    call_id,
                    output,
                    is_error: true,
                } if call_id == "edit-2" => Some(output),
                _ => None,
            })
            .expect("guarded result must be emitted");
        assert_eq!(event_payload["code"], "reread_required");
        assert!(matches!(
            messages.last().unwrap().content.as_slice(),
            [ContentBlock::ToolResult { content, is_error: true, .. }]
                if serde_json::from_str::<serde_json::Value>(content).unwrap()["code"]
                    == "reread_required"
        ));
    }

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
        async fn complete(&self, _request: Arc<CompletionRequest>) -> Result<CompletionResponse> {
            Err(Error::Internal(
                "PendingConnectProvider only supports stream".to_string(),
            ))
        }

        async fn stream(
            &self,
            _request: Arc<CompletionRequest>,
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
                max_output_tokens: 0,
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

    struct CountingStaleEditHost {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ToolHost for CountingStaleEditHost {
        async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
            Ok(vec![ToolSpec {
                name: "edit".to_string(),
                description: "always stale edit".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                source: ToolSource::Builtin,
                requires_confirmation: false,
            }])
        }

        async fn call(&self, name: &str, _args: serde_json::Value) -> Result<ToolCallOutcome> {
            if name != "edit" {
                return Err(Error::ToolNotFound(name.to_string()));
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(stale_outcome("old_string_not_found"))
        }
    }

    #[tokio::test]
    async fn repeated_stale_edits_in_one_provider_turn_share_the_same_budget() {
        let provider = MockProvider::new("mock");
        let mut stream_events = Vec::new();
        for index in 1..=3 {
            let call = edit_call(&format!("edit-{index}"), "    Undo,\n    Undo,");
            stream_events.push(StreamEvent::ToolUseStart {
                id: call.id.clone(),
                name: call.name.clone(),
            });
            stream_events.push(StreamEvent::ToolUseComplete {
                id: call.id,
                input: call.input,
            });
        }
        stream_events.push(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        });
        provider.push_turn(RecordedTurn::ok(stream_events));
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_host = CountingStaleEditHost {
            calls: calls.clone(),
        };
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("remove the duplicate")];

        let events =
            run_agent_loop_iteration(&provider, &tool_host, base_request(), &mut messages, &tools)
                .await
                .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let codes = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolResult {
                    output,
                    is_error: true,
                    ..
                } => output.get("code").and_then(serde_json::Value::as_str),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            vec![
                "old_string_not_found",
                "reread_required",
                "repeated_stale_edit"
            ]
        );
        assert!(matches!(
            messages.last().unwrap().content.as_slice(),
            [
                ContentBlock::ToolResult { .. },
                ContentBlock::ToolResult { .. },
                ContentBlock::ToolResult { .. }
            ]
        ));
    }

    /// 记录每次 `stream` 收到的请求消息，再回放一段固定文本。
    ///
    /// 用于断言后续请求透传的是修复后的健康历史（字节级），而非重新修复/重复插入。
    struct RecordingProvider {
        requests: Arc<Mutex<Vec<Vec<Message>>>>,
    }

    #[async_trait]
    impl LlmProvider for RecordingProvider {
        async fn complete(&self, _request: Arc<CompletionRequest>) -> Result<CompletionResponse> {
            Err(Error::Internal(
                "RecordingProvider only supports stream".to_string(),
            ))
        }

        async fn stream(
            &self,
            request: Arc<CompletionRequest>,
        ) -> Result<futures::stream::BoxStream<'static, StreamEvent>> {
            self.requests.lock().unwrap().push(request.messages.clone());
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
                max_output_tokens: 0,
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
    async fn ordinary_tool_watchdog_returns_an_error_result_instead_of_hanging_the_run() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ToolUseStart {
                id: "stalled-tool".to_string(),
                name: "read_file".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "stalled-tool".to_string(),
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
        let mut messages = vec![Message::user_text("read")];

        let events = tokio::time::timeout(
            Duration::from_secs(1),
            run_agent_loop_iteration_with_abort(
                &provider,
                &tool_host,
                base_request(),
                &mut messages,
                &tool_host.tools,
                None,
            ),
        )
        .await
        .expect("the per-tool watchdog must settle the iteration")
        .unwrap();

        assert!(started.load(Ordering::SeqCst));
        assert!(dropped.load(Ordering::SeqCst));
        assert!(!completed.load(Ordering::SeqCst));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolResult {
                call_id,
                is_error: true,
                ..
            } if call_id == "stalled-tool"
        )));
        assert!(
            messages
                .last()
                .is_some_and(|message| message.content.iter().any(|block| matches!(
                    block,
                    ContentBlock::ToolResult { content, is_error: true, .. }
                        if content.contains("timeout")
                ))),
            "the model must receive a retryable timeout result"
        );
    }

    #[test]
    fn ordinary_tool_watchdog_exempts_tools_with_their_own_wait_contract() {
        for name in [
            "bash",
            "delegate_task",
            "collect_subagents",
            "request_user_input",
            "mcp_call",
            "mcp__obsidian__search",
        ] {
            assert_eq!(ordinary_tool_watchdog(name), None, "{name}");
        }
        assert_eq!(
            ordinary_tool_watchdog("read_file"),
            Some(TOOL_NO_COMPLETION_TIMEOUT)
        );
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
        assert_eq!(outcome.reasoning_chars, "checking".chars().count());
    }

    #[tokio::test]
    async fn provider_reasoning_after_answer_start_is_counted_but_not_emitted() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ReasoningDelta {
                text: "checking".into(),
            },
            StreamEvent::TextDelta {
                text: "answer".into(),
            },
            StreamEvent::ReasoningDelta {
                text: "late buffered thought".into(),
            },
            StreamEvent::TextDelta {
                text: " continues".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]));
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

        let visible = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Reasoning { text, .. } => Some(("reasoning", text.as_str())),
                AgentEvent::Message { text, .. } => Some(("message", text.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            visible,
            vec![
                ("reasoning", "checking"),
                ("message", "answer"),
                ("message", " continues"),
            ]
        );
        assert_eq!(
            outcome.reasoning_chars,
            "checkinglate buffered thought".chars().count()
        );
        assert_eq!(
            messages.last().map(Message::text_content).as_deref(),
            Some("answer continues")
        );
    }

    #[tokio::test]
    async fn empty_reasoning_is_still_replayed_for_a_tool_call_turn() {
        // DeepSeek V4 工具调用轮次可能返回空 reasoning_text。空内容不上屏，但
        // 必须作为空 Thinking 块写进 assistant 历史，下一轮才会回传空 reasoning
        // item，否则服务端 400（"must be passed back to the API"）。
        let provider = ReasoningEchoProvider::new("deepseek_responses");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ReasoningDelta {
                text: String::new(),
            },
            StreamEvent::ToolUseStart {
                id: "call_1".to_string(),
                name: "noop".to_string(),
            },
            StreamEvent::ToolUseComplete {
                id: "call_1".to_string(),
                input: serde_json::json!({}),
            },
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        let tool_host = EchoToolHost::new("noop");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("run noop")];
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

        assert!(outcome.had_tool_call);
        // 空 reasoning 不产生用户可见的 Reasoning 事件。
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::Reasoning { .. })));
        // assistant 历史：空 Thinking 块在最前，后面跟着本轮工具调用。
        assert_eq!(messages.len(), 3);
        assert!(matches!(
            &messages[1].content[..],
            [
                ContentBlock::Thinking {
                    thinking,
                    signature: None,
                },
                ContentBlock::ToolUse { id, .. },
            ] if thinking.is_empty() && id == "call_1"
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
        assert!(!outcome.requires_final_summary_recovery);
        assert!(!outcome.hosted_web_failed);
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
        assert!(!outcome.hosted_web_failed);
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
    async fn hosted_web_error_is_reported_to_the_run_coordinator() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::HostedToolUse {
                id: "srvtoolu_failed".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query": "Rust"}),
                provider_content: Some(serde_json::json!({
                    "type": "server_tool_use",
                    "id": "srvtoolu_failed",
                    "name": "web_search",
                    "input": {"query": "Rust"},
                })),
            },
            StreamEvent::HostedToolResult {
                id: "srvtoolu_failed".into(),
                name: "web_search".into(),
                output: serde_json::json!({"error": "unsupported server tool"}),
                is_error: true,
                provider_content: Some(serde_json::json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_failed",
                    "content": {"type": "web_search_tool_error", "error_code": "unavailable"},
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

        assert!(outcome.hosted_web_failed);
        assert!(outcome.requires_final_summary_recovery);
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
    async fn hosted_web_sse_contract_error_keeps_a_safe_fallback_marker() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![StreamEvent::Stop {
            reason: StopReason::Other(
                "invalid_request_error: unsupported server tool web_search".into(),
            ),
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
        .expect_err("hosted web contract errors must fail the iteration");

        assert!(error.to_string().contains("hosted web tool is unsupported"));
        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn responses_style_error_metadata_is_classified_as_fatal() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![StreamEvent::Stop {
            reason: StopReason::Other(
                "unsupported input item (type=invalid_request_error, code=invalid_value, param=input[37])"
                    .into(),
            ),
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
        .expect_err("Responses error metadata must fail the iteration");

        assert!(error.to_string().contains("拒绝了请求"));
        assert_eq!(messages.len(), 1);
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

    /// 记录每次 stream 请求的 max_tokens——MockProvider 丢弃请求内容，升档断言
    /// 必须看到真实发出的请求参数。
    struct MaxTokensCaptureProvider {
        inner: MockProvider,
        seen: std::sync::Arc<std::sync::Mutex<Vec<u32>>>,
    }

    impl MaxTokensCaptureProvider {
        fn new() -> (Self, std::sync::Arc<std::sync::Mutex<Vec<u32>>>) {
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                Self {
                    inner: MockProvider::new("mock"),
                    seen: seen.clone(),
                },
                seen,
            )
        }

        fn push_turn(&self, turn: RecordedTurn) -> &Self {
            self.inner.push_turn(turn);
            self
        }
    }

    #[async_trait]
    impl LlmProvider for MaxTokensCaptureProvider {
        async fn complete(&self, request: Arc<CompletionRequest>) -> Result<CompletionResponse> {
            self.inner.complete(request).await
        }

        async fn stream(
            &self,
            request: Arc<CompletionRequest>,
        ) -> Result<futures::stream::BoxStream<'static, StreamEvent>> {
            self.seen
                .lock()
                .expect("max_tokens capture lock poisoned")
                .push(request.max_tokens);
            self.inner.stream(request).await
        }

        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }

        fn name(&self) -> &str {
            self.inner.name()
        }
    }

    /// 已有正文时 MaxTokens 截断不重放也不报错（既有规则保留，docs §6.5）：
    /// 已产出的正文进入历史，恰好一次请求。
    #[tokio::test]
    async fn max_tokens_after_actionable_output_keeps_result_without_replay() {
        let (provider, seen) = MaxTokensCaptureProvider::new();
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ReasoningDelta {
                text: "思考中".into(),
            },
            StreamEvent::TextDelta {
                text: "部分结论".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::MaxTokens,
            },
        ]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];
        let mut retry_guard = EditRetryGuard::default();

        let outcome = run_agent_loop_iteration_with_abort_and_emit_with_retry_guard(
            &provider,
            &tool_host,
            CompletionRequest {
                model: "mock".into(),
                system: None,
                messages: messages.clone(),
                tools: tools.clone(),
                hosted_tools: vec![],
                max_tokens: 8_192,
                temperature: None,
                enable_caching: false,
                inference: Default::default(),
            },
            &mut messages,
            &tools,
            None,
            &mut retry_guard,
            true,
            |_event| {},
            OutputBudgetContext {
                configured: 8_192,
                provider_ceiling: 12_000,
                headroom_clamped: false,
            },
        )
        .await
        .unwrap();

        assert!(!outcome.had_tool_call);
        assert_eq!(
            *seen.lock().expect("max_tokens capture lock poisoned"),
            vec![8_192],
            "已有产物的 MaxTokens 截断回合不得整轮重放"
        );
        let assistant_text = messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .flat_map(|m| m.content.iter())
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(assistant_text, "部分结论");
    }

    /// 无产物的 MaxTokens（docs §6.5）：不再 ×2 升档重放——首轮预算耗尽后
    /// 直接进入终态分类，`1 → 2 → 4` 式放大不再出现。
    #[tokio::test]
    async fn max_tokens_exhaustion_before_output_does_not_escalate() {
        let (provider, seen) = MaxTokensCaptureProvider::new();
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ReasoningDelta {
                text: "思考耗尽了整个输出预算".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::MaxTokens,
            },
        ]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];
        let mut retry_guard = EditRetryGuard::default();

        let error = run_agent_loop_iteration_with_abort_and_emit_with_retry_guard(
            &provider,
            &tool_host,
            CompletionRequest {
                model: "mock".into(),
                system: None,
                messages: messages.clone(),
                tools: tools.clone(),
                hosted_tools: vec![],
                max_tokens: 8_192,
                temperature: None,
                enable_caching: false,
                inference: Default::default(),
            },
            &mut messages,
            &tools,
            None,
            &mut retry_guard,
            true,
            |_event| {},
            OutputBudgetContext {
                configured: 8_192,
                provider_ceiling: 12_000,
                headroom_clamped: false,
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            r_code_core::error::ProductError::OutputBudgetExhausted {
                attempted: 8_192,
                ..
            }
        ));
        assert_eq!(
            *seen.lock().expect("max_tokens capture lock poisoned"),
            vec![8_192],
            "预算耗尽的空回合必须直接终态分类，不得 ×2 升档重放"
        );
    }

    /// 正常配置上限下无产物的 MaxTokens → `OUTPUT_BUDGET_EXHAUSTED`，
    /// 恰好一次请求，不自动翻倍（docs §6.5 / §12「MaxTokens 不再产生 1,2,4」）。
    #[tokio::test]
    async fn max_tokens_exhaustion_reports_budget_error_without_escalation() {
        let (provider, seen) = MaxTokensCaptureProvider::new();
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ReasoningDelta {
                text: "继续思考".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::MaxTokens,
            },
        ]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];
        let mut retry_guard = EditRetryGuard::default();

        let error = run_agent_loop_iteration_with_abort_and_emit_with_retry_guard(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &tools,
            None,
            &mut retry_guard,
            false,
            |_event| {},
            OutputBudgetContext {
                configured: 128,
                provider_ceiling: 0,
                headroom_clamped: false,
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            r_code_core::error::ProductError::OutputBudgetExhausted { attempted: 128, .. }
        ));
        assert!(error.to_string().contains("输出预算"));
        assert!(!error.to_string().contains("线路"));
        // 恰好一次请求；[1,2,4]/[128,256,512] 式翻倍序列必须消失。
        assert_eq!(
            *seen.lock().expect("max_tokens capture lock poisoned"),
            vec![128]
        );
    }

    /// headroom 钳制后的 MaxTokens 空回合 → `CONTEXT_CONSTRAINED_OUTPUT_EXHAUSTED`，
    /// 不重放：上下文预算问题重放只会再次超窗（docs §6.5）。
    #[tokio::test]
    async fn headroom_clamped_max_tokens_reports_context_constraint() {
        let (provider, seen) = MaxTokensCaptureProvider::new();
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::ReasoningDelta {
                text: "推理耗尽".into(),
            },
            StreamEvent::Stop {
                reason: StopReason::MaxTokens,
            },
        ]));
        let tool_host = EchoToolHost::new("");
        let tools = tool_host.list_tools().await.unwrap();
        let mut messages = vec![Message::user_text("hi")];
        let mut retry_guard = EditRetryGuard::default();

        let error = run_agent_loop_iteration_with_abort_and_emit_with_retry_guard(
            &provider,
            &tool_host,
            base_request(),
            &mut messages,
            &tools,
            None,
            &mut retry_guard,
            false,
            |_event| {},
            OutputBudgetContext {
                configured: 128,
                provider_ceiling: 0,
                headroom_clamped: true,
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            r_code_core::error::ProductError::ContextConstrainedOutputExhausted {
                effective_output: 128
            }
        ));
        assert_eq!(
            *seen.lock().expect("max_tokens capture lock poisoned"),
            vec![128]
        );
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
        let tool_host = agent_contract::NullToolHost;
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

    #[test]
    fn tool_observation_derives_error_code_exit_code_and_snippet() {
        let call = PendingToolCall {
            id: "c1".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({ "command": "cargo test" }),
        };
        let outcome = ToolCallOutcome {
            content: "$ cargo test\nexit: 2（非零，命令失败）\ntests failed\n".to_string(),
            is_error: true,
            metadata: None,
        };
        let observation = tool_observation(&call, &outcome);
        assert_eq!(observation.name, "bash");
        assert!(observation.is_error);
        assert_eq!(observation.exit_code, Some(2));
        assert!(observation.output_snippet.contains("tests failed"));
        assert!(observation.error_code.is_none());
    }

    #[test]
    fn tool_observation_prefers_structured_metadata_exit_code() {
        let call = PendingToolCall {
            id: "c2".to_string(),
            name: "edit".to_string(),
            input: serde_json::json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }),
        };
        let outcome = ToolCallOutcome {
            content: serde_json::json!({ "status": "error", "code": "stale_read" }).to_string(),
            is_error: true,
            metadata: Some(serde_json::json!({ "exit_code": 7 })),
        };
        let observation = tool_observation(&call, &outcome);
        assert_eq!(observation.exit_code, Some(7));
        assert_eq!(observation.error_code.as_deref(), Some("stale_read"));
        assert_eq!(
            tool_outcome_exit_code(&outcome),
            Some(7),
            "metadata 退出码应优先于输出文本"
        );
    }

    #[test]
    fn observation_snippet_is_bounded() {
        let call = PendingToolCall {
            id: "c3".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({ "path": "big.txt" }),
        };
        let outcome = ToolCallOutcome {
            content: "x".repeat(OBSERVATION_SNIPPET_CHARS + 100),
            is_error: false,
            metadata: None,
        };
        let observation = tool_observation(&call, &outcome);
        assert_eq!(
            observation.output_snippet.chars().count(),
            OBSERVATION_SNIPPET_CHARS
        );
    }
}
