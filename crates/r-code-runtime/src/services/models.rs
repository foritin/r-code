//! Model provider broker (host.model.stream).
//!
//! Adapts the existing `agent-llm` providers to the kernel `ModelService`
//! port with a stable request projection. Credentials stay host-owned: the
//! plugin sends an opaque selection string; the resolver holds provider
//! configs and API keys internally and never puts them on the wire. Usage
//! is recorded host-side on **every** exit path (A02). This layer performs
//! no retries: the completeness contract rejects unfinished streams, and
//! retry policy (frozen-request replay, idempotent requests only) lives in
//! the consuming loop (A03).

use agent_contract::provider::{CompletionRequest, LlmProvider, StreamEvent as ProviderEvent};
use agent_contract::{ContentBlock, InferenceOptions, Message, Role, ToolSpec, Usage};
use r_code_harness_protocol::services::{
    ContentBlock as WireBlock, ModelMessage, ModelRole, ModelStreamRequest, OutputBlock,
};
use r_code_harness_protocol::{ModelUsage, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 8_192;
const MAX_OUTPUT_TOKENS: u32 = 1_048_576;

#[derive(serde::Serialize)]
struct InstructionFrame<'a> {
    role: &'static str,
    content: Vec<&'a str>,
}

/// Resolves opaque model selections to configured providers. Host-owned;
/// implementations hold credentials.
pub trait ProviderResolver: Send + Sync {
    /// Resolve `selection` (e.g. "provider-id/model-name") to a provider
    /// plus concrete model id. None = unknown selection.
    fn resolve(&self, selection: &str) -> Option<(Arc<dyn LlmProvider>, String)>;
    /// The default selection when a request carries none.
    fn default_selection(&self) -> String;
}

/// Run-scoped resolver exposing exactly one already-constructed provider.
/// Settings and credentials are never consulted after this object is built,
/// so a daemon settings update can affect only a later run.
pub struct FrozenProviderResolver {
    selection: String,
    provider: Arc<dyn LlmProvider>,
    model: String,
}

impl FrozenProviderResolver {
    pub fn new(
        selection: impl Into<String>,
        provider: Arc<dyn LlmProvider>,
        model: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            selection: selection.into(),
            provider,
            model: model.into(),
        })
    }
}

impl ProviderResolver for FrozenProviderResolver {
    fn resolve(&self, selection: &str) -> Option<(Arc<dyn LlmProvider>, String)> {
        (selection == self.selection).then(|| (self.provider.clone(), self.model.clone()))
    }

    fn default_selection(&self) -> String {
        self.selection.clone()
    }
}

/// One recorded usage row (host-side accounting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub run_id: String,
    pub selection: String,
    pub model: String,
    pub usage: ModelUsage,
}

/// The broker.
pub struct ModelBroker {
    resolver: Arc<dyn ProviderResolver>,
    usage_log: Mutex<Vec<UsageRecord>>,
}

impl ModelBroker {
    pub fn new(resolver: Arc<dyn ProviderResolver>) -> Self {
        Self {
            resolver,
            usage_log: Mutex::new(Vec::new()),
        }
    }

    /// Construct a provider-neutral broker pinned to one immutable route.
    pub fn for_frozen_route(
        selection: impl Into<String>,
        provider: Arc<dyn LlmProvider>,
        model: impl Into<String>,
    ) -> Self {
        Self::new(FrozenProviderResolver::new(selection, provider, model))
    }

    /// Recorded usage rows (host accounting / billing projection).
    pub async fn usage_records(&self) -> Vec<UsageRecord> {
        self.usage_log.lock().await.clone()
    }

    fn project(
        request: &ModelStreamRequest,
        model: &str,
    ) -> Result<CompletionRequest, ServiceError> {
        let inference = request.inference.as_ref();
        Ok(CompletionRequest {
            model: model.to_string(),
            system: project_system(&request.messages)?,
            messages: request
                .messages
                .iter()
                .filter_map(project_message)
                .collect(),
            tools: request
                .tools
                .iter()
                .map(|tool| ToolSpec {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                    source: agent_contract::ToolSource::Custom {
                        id: "harness-plugin".into(),
                    },
                    requires_confirmation: false,
                })
                .collect(),
            hosted_tools: Vec::new(),
            max_tokens: project_max_output_tokens(inference),
            temperature: inference_value(inference, "temperature")
                .and_then(Value::as_f64)
                .filter(|value| *value >= 0.0 && *value <= f32::MAX as f64)
                .map(|value| value as f32),
            enable_caching: true,
            inference: project_inference_options(inference),
        })
    }
}

fn project_system(messages: &[ModelMessage]) -> Result<Option<String>, ServiceError> {
    let mut frames = Vec::new();
    for message in messages {
        let role = match message.role {
            ModelRole::System => "system",
            ModelRole::Developer => "developer",
            ModelRole::User | ModelRole::Assistant | ModelRole::Tool => continue,
        };
        let mut content = Vec::new();
        for block in &message.content {
            match block {
                WireBlock::Text { text } if !text.trim().is_empty() => content.push(text.as_str()),
                WireBlock::Text { .. } => {}
                _ => {
                    return Err(ServiceError::Failure(format!(
                        "{role} instruction message contains unsupported non-text content"
                    )));
                }
            }
        }
        if !content.is_empty() {
            frames.push(InstructionFrame { role, content });
        }
    }
    if frames.is_empty() {
        Ok(None)
    } else {
        serde_json::to_string(&frames)
            .map(Some)
            .map_err(|error| ServiceError::Failure(format!("instruction frame: {error}")))
    }
}

fn inference_value<'a>(inference: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    inference?.as_object()?.get(key)
}

fn inference_string(inference: Option<&Value>, key: &str) -> Option<String> {
    inference_value(inference, key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn project_inference_options(inference: Option<&Value>) -> InferenceOptions {
    InferenceOptions {
        thinking: inference_string(inference, "thinking"),
        reasoning_effort: inference_string(inference, "reasoning_effort"),
        verbosity: inference_string(inference, "verbosity"),
    }
}

fn project_max_output_tokens(inference: Option<&Value>) -> u32 {
    ["max_output_tokens", "max_tokens"]
        .into_iter()
        .filter_map(|key| inference_value(inference, key))
        .filter_map(Value::as_u64)
        .find(|value| *value > 0)
        .map(|value| value.min(u64::from(MAX_OUTPUT_TOKENS)) as u32)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
}

fn project_message(message: &ModelMessage) -> Option<Message> {
    let role = match message.role {
        ModelRole::System | ModelRole::Developer => return None,
        ModelRole::User => Role::User,
        ModelRole::Assistant => Role::Assistant,
        ModelRole::Tool => Role::User,
    };
    let mut content = Vec::new();
    for block in &message.content {
        match block {
            WireBlock::Text { text } => content.push(ContentBlock::Text { text: text.clone() }),
            WireBlock::ToolCall { id, name, input } => content.push(ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            WireBlock::ToolResult { call_id, output } => {
                let text = output
                    .iter()
                    .map(|block| match block {
                        OutputBlock::Text { text } => text.clone(),
                        OutputBlock::Json { value } => value.to_string(),
                        OutputBlock::Image { .. } => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                content.push(ContentBlock::ToolResult {
                    tool_use_id: call_id.clone(),
                    content: text,
                    is_error: false,
                })
            }
            // Images travel as artifact references; the host materializes
            // bytes at request-build time. Here the reference resolves to an
            // ImageSource via the blob store owned by the resolver side.
            WireBlock::Image { artifact } => content.push(ContentBlock::Text {
                text: format!("[image: {}]", artifact.blob_id),
            }),
        }
    }
    if content.is_empty() {
        return None;
    }
    Some(Message { role, content })
}

fn usage_to_wire(usage: &Usage) -> ModelUsage {
    ModelUsage {
        input_tokens: Some(usage.input_tokens as u64),
        output_tokens: Some(usage.output_tokens as u64),
        cost_micros: None,
    }
}

#[async_trait::async_trait]
impl ModelService for ModelBroker {
    async fn stream(
        &self,
        token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        let selection = request
            .selection
            .clone()
            .unwrap_or_else(|| self.resolver.default_selection());
        let (provider, model) = self.resolver.resolve(&selection).ok_or_else(|| {
            ServiceError::Failure(format!("unknown model selection {selection:?}"))
        })?;

        let projected = Arc::new(Self::project(&request, &model)?);

        // A02：deadline 必须覆盖**首个** provider await（连接/TLS 挂死此前
        // 无界）；deadline_ms ≤ 0 钳到 1s 下限（0 值会即刻误失败）。
        const MIN_STREAM_DEADLINE_MS: u64 = 1_000;
        let deadline_duration = request
            .deadline_ms
            .map(|ms| Duration::from_millis(ms.max(MIN_STREAM_DEADLINE_MS)))
            .unwrap_or(Duration::from_secs(300));
        let deadline = tokio::time::Instant::now() + deadline_duration;
        let mut events = match tokio::time::timeout_at(deadline, provider.stream(projected))
            .await
            .map_err(|_| ServiceError::Failure("model stream connect deadline exceeded".into()))
            .and_then(|stream| stream.map_err(|e| ServiceError::Failure(e.to_string())))
        {
            Ok(events) => events,
            Err(error) => {
                // A02：请求级失败（连接超时/5xx/429 等）同样入账零值 usage，
                // 保证"每次调用一条记录"的可审计性。
                self.usage_log.lock().await.push(UsageRecord {
                    run_id: token.run_id,
                    selection,
                    model,
                    usage: usage_to_wire(&Usage::default()),
                });
                return Err(error);
            }
        };

        // A02：同一 (run, generation) 内每轮各起一个流，id 必须带每流判别
        // 子，否则按 stream_id 关联的消费方会把不同轮的 sequence 错并。
        static STREAM_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stream_id = format!(
            "model-{}-{}-{}",
            token.run_id,
            token.generation,
            STREAM_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );

        let mut final_usage = Usage::default();
        let mut partial_text = String::new();
        let mut reasoning = String::new();
        let pump = pump_stream(
            &mut events,
            stream_id.clone(),
            deadline,
            sink,
            &mut final_usage,
            &mut partial_text,
            &mut reasoning,
        )
        .await;

        // A02：usage 在**所有**退出路径入账——deadline/sink 失败/provider
        // 错误路径的已耗 token 此前全部漏记。
        let usage = usage_to_wire(&final_usage);
        self.usage_log.lock().await.push(UsageRecord {
            run_id: token.run_id,
            selection,
            model,
            usage,
        });
        let finish_reason = pump?;

        // A02：完成性契约——已知异常形态（空闲超时 / 传输错误标记 / 从未收
        // 到 Stop）不得伪装成成功；未知 Other 值 fail-open 放行（DEC-3）。
        let incomplete = match finish_reason.as_deref() {
            None => Some("stream ended without a stop event".to_string()),
            Some(reason) if agent_llm::is_abnormal_stop(reason) => Some(reason.to_string()),
            Some(reason)
                if !matches!(
                    reason,
                    "end_turn" | "tool_use" | "max_tokens" | "stop_sequence"
                ) =>
            {
                eprintln!("unknown finish reason {reason}; failing open (DEC-3)");
                None
            }
            Some(_) => None,
        };
        if let Some(reason) = incomplete {
            const PARTIAL_CAP: usize = 4 * 1024;
            let partial = if partial_text.len() > PARTIAL_CAP {
                format!(
                    "{}…[truncated {} bytes]",
                    &partial_text[..PARTIAL_CAP],
                    partial_text.len()
                )
            } else {
                partial_text.clone()
            };
            return Err(ServiceError::Failure(format!(
                "model stream incomplete ({reason}); partial output: {partial}"
            )));
        }
        Ok(ModelStreamOutcome {
            stream_id,
            finish_reason,
            usage,
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
        })
    }
}

/// The provider event pump shared by [`ModelBroker::stream`]: forwards
/// provider events to the sink with per-stream sequencing, collects partial
/// text and usage, and returns the finish reason (`None` when no Stop was
/// ever seen). Sequencing invariants (A02): the timeout-path `Failed` event
/// increments `sequence` like every other event; emitting `done: true`
/// terminates the stream (double-Stop providers cannot emit two Finishes).
async fn pump_stream<S>(
    events: &mut S,
    stream_id: String,
    deadline: tokio::time::Instant,
    sink: &mut dyn StreamSink,
    final_usage: &mut Usage,
    partial_text: &mut String,
    reasoning: &mut String,
) -> Result<Option<String>, ServiceError>
where
    S: futures::Stream<Item = ProviderEvent> + Unpin,
{
    use futures::StreamExt;
    let mut sequence: u64 = 0;
    let mut finish_reason: Option<String> = None;
    loop {
        let next = tokio::time::timeout_at(deadline, events.next()).await;
        match next {
            Err(_) => {
                sequence += 1;
                sink.send(StreamEvent {
                    stream_id: stream_id.clone(),
                    sequence,
                    payload: StreamPayload::Failed {
                        message: "stream deadline exceeded".into(),
                    },
                    done: Some(true),
                })
                .await?;
                return Err(ServiceError::Failure(
                    "model stream deadline exceeded".into(),
                ));
            }
            Ok(None) => break,
            Ok(Some(event)) => {
                let payload = match event {
                    ProviderEvent::TextDelta { text } => {
                        partial_text.push_str(&text);
                        Some(StreamPayload::TextDelta { text })
                    }
                    ProviderEvent::ToolUseStart { id, name } => {
                        Some(StreamPayload::ToolCallDelta {
                            id,
                            name,
                            partial_input: String::new(),
                        })
                    }
                    ProviderEvent::ToolUseDelta { id, input_json } => {
                        Some(StreamPayload::ToolCallDelta {
                            id,
                            name: String::new(),
                            partial_input: input_json,
                        })
                    }
                    ProviderEvent::Usage(usage) => {
                        let wire_usage = usage_to_wire(&usage);
                        *final_usage = usage;
                        Some(StreamPayload::Usage { usage: wire_usage })
                    }
                    ProviderEvent::Stop { reason } => {
                        let reason_text = match reason {
                            agent_contract::provider::StopReason::EndTurn => "end_turn".to_string(),
                            agent_contract::provider::StopReason::ToolUse => "tool_use".to_string(),
                            agent_contract::provider::StopReason::MaxTokens => {
                                "max_tokens".to_string()
                            }
                            agent_contract::provider::StopReason::StopSequence => {
                                "stop_sequence".to_string()
                            }
                            agent_contract::provider::StopReason::Other(other) => other,
                        };
                        finish_reason = Some(reason_text.clone());
                        Some(StreamPayload::Finish {
                            reason: reason_text,
                            usage: usage_to_wire(final_usage),
                        })
                    }
                    ProviderEvent::ReasoningDelta { text } => {
                        reasoning.push_str(&text);
                        None
                    }
                    ProviderEvent::ToolUseComplete { .. }
                    | ProviderEvent::HostedToolUse { .. }
                    | ProviderEvent::HostedToolResult { .. } => None,
                };
                if let Some(payload) = payload {
                    sequence += 1;
                    let done = matches!(payload, StreamPayload::Finish { .. });
                    sink.send(StreamEvent {
                        stream_id: stream_id.clone(),
                        sequence,
                        payload,
                        done: done.then_some(true),
                    })
                    .await?;
                    if done {
                        // A02：done 即终帧——不再轮询，双 Stop 的 provider 不允许
                        // 产出第二个 Finish。
                        break;
                    }
                }
            }
        }
    }
    Ok(finish_reason)
}

/// Credential-free settings view for clients: selections only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct ModelSelectionsView {
    pub selections: Vec<String>,
    pub default: Option<String>,
}

impl ModelBroker {
    /// The wire-safe projection of provider configuration: opaque selection
    /// ids; no endpoints, keys or headers.
    pub fn selections_view(&self) -> ModelSelectionsView {
        let mut view = ModelSelectionsView::default();
        for selection in SELECTION_PROBE {
            if self.resolver.resolve(selection).is_some() {
                view.selections.push(selection.to_string());
            }
        }
        view.default = Some(self.resolver.default_selection());
        view
    }
}

const SELECTION_PROBE: &[&str] = &[
    "anthropic/claude",
    "openai/gpt",
    "deepseek/chat",
    "mock/test",
];
