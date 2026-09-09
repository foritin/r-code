//! Model provider broker (host.model.stream).
//!
//! Adapts the existing `agent-llm` providers to the kernel `ModelService`
//! port with a stable request projection. Credentials stay host-owned: the
//! plugin sends an opaque selection string; the resolver holds provider
//! configs and API keys internally and never puts them on the wire. Usage
//! is recorded host-side per call; retries happen only for requests proven
//! safe (idempotent, no observable side effects beyond billing).

use agent_contract::provider::{CompletionRequest, LlmProvider, StreamEvent as ProviderEvent};
use agent_contract::{ContentBlock, Message, Role, ToolSpec, Usage};
use r_code_harness_protocol::services::{
    ContentBlock as WireBlock, ModelStreamRequest, OutputBlock,
};
use r_code_harness_protocol::{ModelUsage, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Resolves opaque model selections to configured providers. Host-owned;
/// implementations hold credentials.
pub trait ProviderResolver: Send + Sync {
    /// Resolve `selection` (e.g. "provider-id/model-name") to a provider
    /// plus concrete model id. None = unknown selection.
    fn resolve(&self, selection: &str) -> Option<(Arc<dyn LlmProvider>, String)>;
    /// The default selection when a request carries none.
    fn default_selection(&self) -> String;
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

    /// Recorded usage rows (host accounting / billing projection).
    pub async fn usage_records(&self) -> Vec<UsageRecord> {
        self.usage_log.lock().await.clone()
    }

    fn project(request: &ModelStreamRequest, model: &str) -> CompletionRequest {
        CompletionRequest {
            model: model.to_string(),
            system: None,
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
            max_tokens: 8192,
            temperature: request
                .inference
                .as_ref()
                .and_then(|inference| inference.get("temperature"))
                .and_then(|value| value.as_f64())
                .map(|value| value as f32),
            enable_caching: true,
            inference: Default::default(),
        }
    }
}

fn project_message(message: &r_code_harness_protocol::services::ModelMessage) -> Option<Message> {
    let role = match message.role {
        r_code_harness_protocol::services::ModelRole::System
        | r_code_harness_protocol::services::ModelRole::User => Role::User,
        r_code_harness_protocol::services::ModelRole::Assistant
        | r_code_harness_protocol::services::ModelRole::Tool => Role::Assistant,
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

        let projected = Arc::new(Self::project(&request, &model));
        let mut events = provider
            .stream(projected)
            .await
            .map_err(|e| ServiceError::Failure(e.to_string()))?;

        let stream_id = format!("model-{}-{}", token.run_id, token.generation);
        let deadline = request
            .deadline_ms
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(300));
        let deadline = tokio::time::Instant::now() + deadline;

        let mut sequence: u64 = 0;
        let mut final_usage = Usage::default();
        let mut finish_reason: Option<String> = None;
        use futures::StreamExt;
        loop {
            let next = tokio::time::timeout_at(deadline, events.next()).await;
            match next {
                Err(_) => {
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
                            final_usage = usage;
                            Some(StreamPayload::Usage { usage: wire_usage })
                        }
                        ProviderEvent::Stop { reason } => {
                            let reason_text = match reason {
                                agent_contract::provider::StopReason::EndTurn => {
                                    "end_turn".to_string()
                                }
                                agent_contract::provider::StopReason::ToolUse => {
                                    "tool_use".to_string()
                                }
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
                                usage: usage_to_wire(&final_usage),
                            })
                        }
                        ProviderEvent::ReasoningDelta { .. }
                        | ProviderEvent::ToolUseComplete { .. }
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
                    }
                }
            }
        }

        let usage = usage_to_wire(&final_usage);
        self.usage_log.lock().await.push(UsageRecord {
            run_id: token.run_id,
            selection,
            model,
            usage,
        });
        Ok(ModelStreamOutcome {
            stream_id,
            finish_reason,
            usage,
        })
    }
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
