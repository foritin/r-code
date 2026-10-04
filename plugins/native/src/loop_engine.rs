//! The multi-turn model/tool loop.
//!
//! Pure request-shaping: project the conversation, call the model, execute
//! returned tool calls through host tools with attempt-stable operation
//! keys, checkpoint after every turn, and propose completion when the
//! model stops calling tools. Cancellation is cooperative between turns.

use crate::request_projection::{
    project_request, push_assistant, push_tool_result, push_user, ConversationState,
    ProjectionError, WireBlock,
};
use r_code_harness_protocol::services::{
    ModelStreamRequest, PlanPublishRequest, ProposalKind, ToolDescriptor, WorkUnitWire,
};
use r_code_harness_protocol::EventKind;
use r_code_harness_sdk::{ModelTurn, SdkError, SdkHandle};
use std::time::Duration;

/// Loop configuration (strategy defaults preserved from the Native
/// runtime; budgets stay host-owned). The host seeds per-task values
/// (model selection, inference knobs, task mode) through the initialize
/// handshake's `harness_config`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoopConfig {
    pub system_prompt: String,
    pub model_selection: Option<String>,
    /// Inference knobs (thinking level / effort) forwarded verbatim to the
    /// host model broker.
    #[serde(default)]
    pub inference: Option<serde_json::Value>,
    /// Task mode label ("ask" / "edit" / "auto" / "plan") selecting the
    /// system prompt posture.
    #[serde(default)]
    pub task_mode: Option<String>,
    /// Next task-owned immutable plan revision supplied by the host. It is
    /// required in plan mode and never guessed by the harness.
    #[serde(default)]
    pub plan_revision: Option<u64>,
    /// FR-1: the frozen project-instruction block from the host. Appended
    /// verbatim inside effective_system_prompt so the user prompt config
    /// stays pure; empty means nothing was injected for this run.
    #[serde(default)]
    pub instructions: String,
    pub max_turns: u32,
    /// A03：采样失败时的冻结请求重放次数（0 = 不重放）。默认 3。
    #[serde(default = "default_stream_replay_attempts")]
    pub stream_replay_attempts: u32,
    /// A13：同轮工具并发上限。None=默认 4；Some(0)=串行回退（开关）。
    #[serde(default)]
    pub parallel_tools: Option<u32>,
}

fn default_stream_replay_attempts() -> u32 {
    3
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            system_prompt: agent_config::DEFAULT_MAIN_AGENT_PROMPT.to_string(),
            model_selection: None,
            inference: None,
            task_mode: None,
            plan_revision: None,
            instructions: String::new(),
            max_turns: 25,
            stream_replay_attempts: default_stream_replay_attempts(),
            parallel_tools: None,
        }
    }
}

impl LoopConfig {
    /// Build the config from the host-provided harness config document.
    pub fn from_harness_config(harness_config: &serde_json::Value) -> Self {
        let mut config = Self::default();
        if let Some(selection) = harness_config
            .get("modelSelection")
            .and_then(|v| v.as_str())
        {
            if !selection.is_empty() {
                config.model_selection = Some(selection.to_string());
            }
        }
        if config.model_selection.is_none() {
            if let Some(default) = harness_config
                .get("defaultModelSelection")
                .and_then(|v| v.as_str())
            {
                if !default.is_empty() {
                    config.model_selection = Some(default.to_string());
                }
            }
        }
        config.inference = harness_config
            .get("inference")
            .cloned()
            .filter(|v| !v.is_null());
        config.task_mode = harness_config
            .get("taskMode")
            .and_then(|v| v.as_str())
            .filter(|mode| !mode.is_empty())
            .map(str::to_string);
        config.plan_revision = harness_config
            .get("planRevision")
            .and_then(|value| value.as_u64())
            .filter(|revision| *revision > 0);
        if let Some(system_prompt) = harness_config
            .get("systemPrompt")
            .and_then(|value| value.as_str())
        {
            config.system_prompt = system_prompt.to_string();
        }
        if let Some(instructions) = harness_config
            .get("instructions")
            .and_then(|value| value.as_str())
        {
            config.instructions = instructions.to_string();
        }
        if let Some(max_turns) = harness_config
            .get("maxTurns")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
        {
            config.max_turns = max_turns;
        }
        // A03：重放次数可经根键或 inference 子对象下发（0–10，越界回默认）。
        // A13：parallelTools 数字（0 = 串行回退开关）。
        config.parallel_tools = harness_config
            .get("parallelTools")
            .or_else(|| {
                harness_config
                    .get("inference")
                    .and_then(|value| value.get("parallelTools"))
            })
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value <= 16);
        config.stream_replay_attempts = harness_config
            .get("streamReplayAttempts")
            .or_else(|| {
                harness_config
                    .get("inference")
                    .and_then(|value| value.get("streamReplayAttempts"))
            })
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value <= 10)
            .unwrap_or(default_stream_replay_attempts());
        config
    }

    /// The effective system prompt for the configured task mode.
    pub fn effective_system_prompt(&self) -> String {
        // FR-1: the frozen instruction block appends to the base prompt in
        // every mode. The block is part of the run snapshot identity (its
        // digest rides in the harness config), so prompt snapshots and model
        // requests keep the same identity contract as before.
        let base = match self.task_mode.as_deref() {
            Some("plan") => format!(
                "{}\n\n当前为 Plan 模式：先调查再规划。产出分步实施计划（目标、步骤、\
                 涉及文件、风险），未经用户确认不要直接修改文件。",
                if self.system_prompt.is_empty() {
                    "You are a careful coding agent."
                } else {
                    &self.system_prompt
                }
            ),
            // Ask is enforced by the host's read-only tool capability.
            _ => self.system_prompt.clone(),
        };
        if self.instructions.trim().is_empty() {
            base
        } else {
            format!("{base}\n\n{}", self.instructions.trim_end())
        }
    }
}

/// Errors surfaced by the loop.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LoopError {
    #[error("sdk failure: {0}")]
    Sdk(String),
    #[error(transparent)]
    Projection(#[from] ProjectionError),
    #[error("turn limit reached ({0} turns)")]
    TurnLimit(u32),
    #[error("plan mode requires a host-provided plan revision")]
    MissingPlanRevision,
}

impl From<SdkError> for LoopError {
    fn from(error: SdkError) -> Self {
        LoopError::Sdk(error.to_string())
    }
}

/// The loop result.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoopResult {
    pub final_text: String,
    pub turns: u32,
    pub tool_calls_executed: u32,
    pub proposal_accepted: bool,
    /// A06：结局语义——end（正常）| cancelled（取消，绝不落入完成提案）|
    /// budget_reached（A11 接力信号）。旧序列化缺省为 end。
    #[serde(default = "default_stop_reason")]
    pub stop_reason: String,
}

fn default_stop_reason() -> String {
    "end".into()
}

/// Run the loop for one user input over the host services.
pub async fn run_loop(
    handle: &SdkHandle,
    config: &LoopConfig,
    // A08：共享锁短作用域——每次变更短暂持锁、投影/序列化用克隆，
    // on_steer 得以落在轮间（"steerable between turns" 兑现）。
    state: &tokio::sync::Mutex<ConversationState>,
    user_input: &str,
) -> Result<LoopResult, LoopError> {
    // A06 fail-fast：已取消的 run 不再触碰对话状态（否则留下无回复的用户
    // 消息）；plan 模式在首个模型调用前校验 revision（否则跑完预算才报错）。
    if handle.is_cancelled() {
        return Ok(LoopResult {
            final_text: String::new(),
            turns: 0,
            tool_calls_executed: 0,
            proposal_accepted: false,
            stop_reason: "cancelled".into(),
        });
    }
    if config.task_mode.as_deref() == Some("plan") && config.plan_revision.is_none() {
        return Err(LoopError::MissingPlanRevision);
    }
    push_user(&mut *state.lock().await, user_input);
    let tools: Vec<ToolDescriptor> = handle
        .tools_list()
        .await
        .map_err(|e| LoopError::Sdk(e.to_string()))?
        .tools;
    let system_prompt = config.effective_system_prompt();
    let mut turns = 0u32;
    let mut executed = 0u32;
    let mut final_text = String::new();
    // A05（经 A03 预落位）：截断续写只做一次，第二次截断拼接并加标记。
    let mut continuation_used = false;
    loop {
        if handle.is_cancelled() {
            // A06：取消绝不落入 plan_publish / 完成提案——直接以 cancelled
            // 结局返回（宿主据 guard 记 run.cancelled，不进完成仲裁）。
            return Ok(LoopResult {
                final_text,
                turns,
                tool_calls_executed: executed,
                proposal_accepted: false,
                stop_reason: "cancelled".into(),
            });
        }
        turns += 1;
        if turns > config.max_turns {
            // A11：预算接力信号——不报错；提案强制 Reply（接力中的任务未
            // 完成，PlanDraft/Implementation 提案是谎言），由宿主注入
            // Continuation 续跑。
            let summary = state
                .lock()
                .await
                .clone()
                .messages
                .iter()
                .rev()
                .find_map(|message| {
                    message.blocks.iter().rev().find_map(|block| match block {
                        WireBlock::Text { text } if !text.trim().is_empty() => Some(text.clone()),
                        _ => None,
                    })
                })
                .unwrap_or_else(|| "预算耗尽，已完成部分见时间线".into());
            let _ = handle
                .emit_event(
                    EventKind::Progress,
                    serde_json::json!({"budgetReached": true, "turns": config.max_turns}),
                )
                .await;
            let decision = handle
                .propose_completion(
                    r_code_harness_protocol::services::CompletionProposalRequest {
                        kind: ProposalKind::Reply,
                        summary: summary.clone(),
                        candidate_digest: None,
                        work_unit_statuses: vec![],
                    },
                )
                .await;
            return Ok(LoopResult {
                final_text: summary,
                turns: turns - 1,
                tool_calls_executed: executed,
                proposal_accepted: decision.map(|reply| reply.accepted).unwrap_or(false),
                stop_reason: "budget_reached".into(),
            });
        }
        let snapshot = state.lock().await.clone();
        let request = project_request(
            &snapshot,
            &system_prompt,
            &tools,
            config.model_selection.as_deref(),
            config.inference.clone(),
        )?;
        // A03：采样失败走冻结请求重放（模型请求无副作用，重发安全）；失败
        // 轮不落入 state（push_assistant 只在拿到完整 turn 后执行）。
        let turn = retry_model_stream(handle, config, request).await?;
        let text = turn.text();
        let calls = turn.tool_calls();
        let wire_calls: Vec<WireBlock> = calls
            .iter()
            .map(|(id, name, input)| WireBlock::ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            })
            .collect();
        push_assistant(&mut *state.lock().await, &text, &wire_calls);

        if calls.is_empty() {
            // A03：终止判定追加完成性前置——仅正常结束的轮次才允许"无工具
            // 调用即结束"；max_tokens 截断轮触发一次续写（A05 形态），再截则
            // 拼接标记后正常收尾。
            let truncated = turn.outcome.finish_reason.as_deref() == Some("max_tokens");
            if truncated && !continuation_used {
                continuation_used = true;
                push_user(&mut *state.lock().await, MAX_TOKENS_CONTINUATION_PROMPT);
                continue;
            }
            final_text = if truncated {
                // 双截标记同时发一条 journal 可见信号（harness.progress），
                // 供验收与 TUI 渲染截断提示。
                let _ = handle
                    .emit_event(
                        EventKind::Progress,
                        serde_json::json!({"truncatedFinal": true}),
                    )
                    .await;
                format!("{text}\n[回答因长度上限被截断]")
            } else {
                text
            };
            // Text-only finishes checkpoint too: the conversation state is
            // the crash-resume point regardless of how the turn ended.
            // A06：编码与保存失败传播（与轮内路径一致），杜绝空载荷覆盖好
            // checkpoint 的静默路径。
            let payload = serde_json::to_vec(&*state.lock().await)
                .map_err(|error| LoopError::Sdk(format!("checkpoint encode failed: {error}")))?;
            handle
                .save_checkpoint(&payload, turns as u64)
                .await
                .map_err(|e| LoopError::Sdk(e.to_string()))?;
            break;
        }
        // Execute every tool call through the host with stable keys.
        // A04：宿主 RPC 失败渲染为合成错误结果（不中止 run）。A13：同轮
        // 工具并发执行（parallelTools 默认 4，0=串行回退开关）；结果按
        // call 顺序回填，执行序与回填序解耦。安全不变量：幂等栅栏（A12）
        // 防同键重入；abort 对在飞工具经宿主侧 abort 传播。
        let cap = config.parallel_tools.unwrap_or(4);
        let mut outputs: Vec<(String, String)> = Vec::with_capacity(calls.len());
        if cap == 0 {
            for (id, name, input) in &calls {
                let operation_key = format!("turn-{turns}-{id}");
                let output = render_tool_outcome(
                    handle
                        .tools_call(name, input.clone(), Some(&operation_key))
                        .await,
                )
                .await;
                outputs.push((id.clone(), output));
            }
        } else {
            let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(cap as usize));
            let mut in_flight = Vec::with_capacity(calls.len());
            for (id, name, input) in &calls {
                let operation_key = format!("turn-{turns}-{id}");
                let semaphore = semaphore.clone();
                // 许可必须在任务体内获取：在外层循环 acquire 会让超出
                // cap 的调用阻塞在 join_all 之前，而已在飞任务因尚未
                // poll 永不释放许可——第 cap+1 个调用即自锁。
                in_flight.push(async move {
                    let _permit = semaphore
                        .acquire_owned()
                        .await
                        .expect("semaphore never closed");
                    let output = render_tool_outcome(
                        handle
                            .tools_call(name, input.clone(), Some(&operation_key))
                            .await,
                    )
                    .await;
                    (id.clone(), output)
                });
            }
            outputs = futures::future::join_all(in_flight).await;
        }
        for (id, output) in outputs {
            push_tool_result(&mut *state.lock().await, &id, &output);
            executed += 1;
        }
        // Checkpoint the conversation after every turn (crash-resume point).
        // A06：编码失败传播，不再降级空载荷。
        let payload = serde_json::to_vec(&*state.lock().await)
            .map_err(|error| LoopError::Sdk(format!("checkpoint encode failed: {error}")))?;
        handle
            .save_checkpoint(&payload, turns as u64)
            .await
            .map_err(|e| LoopError::Sdk(e.to_string()))?;
    }

    // A plan becomes a completion proposal only after the host validates and
    // durably publishes its exact revision. A publish failure exits here and
    // therefore cannot be mistaken for completion.
    let proposal_kind = match config.task_mode.as_deref() {
        Some("plan") => {
            let revision = config.plan_revision.ok_or(LoopError::MissingPlanRevision)?;
            handle
                .plan_publish(PlanPublishRequest {
                    revision,
                    work_units: plan_work_units(&final_text),
                })
                .await
                .map_err(|error| LoopError::Sdk(error.to_string()))?;
            ProposalKind::PlanDraft
        }
        Some("execution" | "edit" | "auto") => ProposalKind::Implementation,
        _ => ProposalKind::Reply,
    };

    // Propose completion for host arbitration.
    let decision = handle
        .propose_completion(
            r_code_harness_protocol::services::CompletionProposalRequest {
                kind: proposal_kind,
                summary: if final_text.is_empty() {
                    "run stopped".into()
                } else {
                    final_text.clone()
                },
                candidate_digest: None,
                work_unit_statuses: vec![],
            },
        )
        .await
        .map_err(|e| LoopError::Sdk(e.to_string()))?;
    Ok(LoopResult {
        final_text,
        turns,
        tool_calls_executed: executed,
        proposal_accepted: decision.accepted,
        stop_reason: "end".into(),
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictPlanDocument {
    work_units: Vec<StrictWorkUnit>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictWorkUnit {
    id: String,
    description: String,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    acceptance: Vec<String>,
    #[serde(default)]
    read_paths: Vec<String>,
    #[serde(default)]
    write_paths: Vec<String>,
    #[serde(default)]
    repo_exclusive: bool,
    #[serde(default)]
    ephemeral_roots: Vec<String>,
    /// P19A (API v1.2): effect authority carried verbatim from the
    /// strict plan document; absent fields stay at the conservative
    /// ReadOnly/Offline defaults.
    #[serde(default)]
    effect_class: r_code_harness_protocol::services::WorkUnitEffectClass,
    #[serde(default)]
    network_ceiling: r_code_harness_protocol::services::NetworkCeiling,
}

/// Parse the documented strict JSON shape when present. Semantic validation
/// (empty/duplicate ids and dependency integrity) stays host-owned. Free-form
/// model output becomes one stable unit so identical text produces identical
/// plan material across retries.
/// A03/A05：max_tokens 截断后的续写指令（DEC-4：普通用户侧输入形态）。
const MAX_TOKENS_CONTINUATION_PROMPT: &str = "继续，从你断开处接着写。";

/// A04/A13：宿主工具调用的统一结果渲染——RPC 失败与 reply.error 同为
/// "error: …" 文本回填，绝不中止 run。
async fn render_tool_outcome(
    result: Result<r_code_harness_protocol::services::ToolCallReply, SdkError>,
) -> String {
    match result {
        Ok(reply) => {
            let rendered = reply
                .output
                .iter()
                .map(|block| match block {
                    r_code_harness_protocol::services::OutputBlock::Text { text } => text.clone(),
                    r_code_harness_protocol::services::OutputBlock::Json { value } => {
                        value.to_string()
                    }
                    r_code_harness_protocol::services::OutputBlock::Image { .. } => String::new(),
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            match reply.error {
                Some(error) => format!("error: {}", error.message),
                None => rendered,
            }
        }
        Err(error) => format!("error: {error}"),
    }
}

/// A03：采样失败的重放决策——仅模型请求可安全重发（无副作用、无操作键），
/// 工具调用绝不在此重放。denylist 之外全部视为可重放（单一启发式）。
fn is_replayable_sampling_error(message: &str) -> bool {
    const DENYLIST: [&str; 7] = [
        "maximum context",
        "context length",
        "context window",
        "invalid api key",
        "unauthorized",
        "forbidden",
        "content policy",
    ];
    let lower = message.to_ascii_lowercase();
    !DENYLIST.iter().any(|needle| lower.contains(needle))
}

/// A05：上下文溢出特征（与重放 denylist 同源，供确定性分类与指引）。
fn is_overflow_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    ["maximum context", "context length", "context window"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// 从错误文本解析服务端 `Retry-After`（agent-llm RateLimited 形如
/// "rate limited, retry after {n}s"）；封顶 60s。
fn parse_retry_after_secs(message: &str) -> Option<Duration> {
    let lower = message.to_ascii_lowercase();
    let tail = {
        let idx = lower.find("retry after")? + "retry after".len();
        &message[idx..]
    };
    let digits: String = tail
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let seconds: u64 = digits.parse().ok()?;
    (seconds > 0).then(|| Duration::from_secs(seconds.min(60)))
}

/// 退避 1s/2s/4s；服务端 Retry-After 优先（已在解析处封顶 60s）。
fn replay_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(after) = retry_after {
        return after;
    }
    const SCHEDULE: [u64; 3] = [1, 2, 4];
    let index = (attempt as usize).saturating_sub(1).min(SCHEDULE.len() - 1);
    Duration::from_secs(SCHEDULE[index])
}

/// A03：带冻结请求重放的模型采样。每次重放经 Progress 上报（前端零重复：
/// 失败尝试从未产出 assistant 观察——宿主无逐 token 通道）。
async fn retry_model_stream(
    handle: &SdkHandle,
    config: &LoopConfig,
    request: ModelStreamRequest,
) -> Result<ModelTurn, LoopError> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match handle.model_stream(request.clone()).await {
            Ok(turn) => return Ok(turn),
            Err(error) => {
                let message = error.to_string();
                let exhausted = attempt > config.stream_replay_attempts;
                if exhausted || !is_replayable_sampling_error(&message) {
                    if is_overflow_error(&message) {
                        // A05：溢出是确定性失败，给用户可行动指引。
                        return Err(LoopError::Sdk(format!(
                            "{message}\n对话已超过模型上下文窗口；请 /compact 或新开会话"
                        )));
                    }
                    if exhausted {
                        return Err(LoopError::Sdk(format!(
                            "采样重放 {attempt}/{} 次后仍失败，最后原因: {message}",
                            config.stream_replay_attempts
                        )));
                    }
                    return Err(LoopError::Sdk(message));
                }
                let _ = handle
                    .emit_event(
                        EventKind::Progress,
                        serde_json::json!({
                            "streamRetry": format!(
                                "{}/{}",
                                attempt, config.stream_replay_attempts
                            ),
                        }),
                    )
                    .await;
                tokio::time::sleep(replay_delay(attempt, parse_retry_after_secs(&message))).await;
            }
        }
    }
}

fn plan_work_units(final_text: &str) -> Vec<WorkUnitWire> {
    if let Ok(document) = serde_json::from_str::<StrictPlanDocument>(final_text) {
        return document
            .work_units
            .into_iter()
            .map(|unit| WorkUnitWire {
                id: unit.id,
                description: unit.description,
                dependencies: unit.dependencies,
                acceptance: unit.acceptance,
                read_paths: unit.read_paths,
                write_paths: unit.write_paths,
                repo_exclusive: unit.repo_exclusive,
                ephemeral_roots: unit.ephemeral_roots,
                effect_class: unit.effect_class,
                network_ceiling: unit.network_ceiling,
            })
            .collect();
    }
    let digest = r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
        "finalText": final_text,
    }));
    vec![WorkUnitWire {
        id: format!("plan-{}", &digest[..16]),
        description: final_text.to_string(),
        dependencies: Vec::new(),
        acceptance: Vec::new(),
        read_paths: Vec::new(),
        write_paths: Vec::new(),
        repo_exclusive: false,
        ephemeral_roots: Vec::new(),
        effect_class: r_code_harness_protocol::services::WorkUnitEffectClass::ReadOnly,
        network_ceiling: r_code_harness_protocol::services::NetworkCeiling::Offline,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_the_product_owned_main_agent_prompt() {
        let config = LoopConfig::default();
        assert_eq!(
            config.system_prompt,
            agent_config::DEFAULT_MAIN_AGENT_PROMPT
        );
        assert!(config.system_prompt.contains("You are R-Code"));
    }

    #[test]
    fn harness_config_can_override_prompt_and_turn_budget() {
        let config = LoopConfig::from_harness_config(&serde_json::json!({
            "systemPrompt": "project prompt",
            "maxTurns": 41,
            "taskMode": "edit",
        }));
        assert_eq!(config.system_prompt, "project prompt");
        assert_eq!(config.max_turns, 41);
        assert_eq!(config.task_mode.as_deref(), Some("edit"));
    }
}
