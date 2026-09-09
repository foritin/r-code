//! T15 — model providers through the broker.
//!
//! Provider mock and protocol fixtures verify streaming, images via
//! artifact refs, tool messages, cancellation and no key disclosure.

use agent_contract::provider::{CompletionRequest, LlmProvider, StreamEvent as ProviderEvent};
use agent_contract::{Capabilities, Result as ContractResult, Usage};
use futures::StreamExt;
use r_code_harness_protocol::services::{
    ContentBlock, ModelMessage, ModelRole, ModelStreamRequest, OutputBlock, ToolDescriptor,
};
use r_code_kernel::ports::{GenerationToken, ModelService, StreamSink};
use r_code_runtime::providers::ProviderRegistry;
use r_code_runtime::services::models::{ModelBroker, UsageRecord};
use std::sync::Arc;
use std::time::Duration;

/// A scripted provider: emits fixed deltas, records the request it saw.
struct ScriptedProvider {
    requests: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl LlmProvider for ScriptedProvider {
    async fn complete(
        &self,
        _request: Arc<CompletionRequest>,
    ) -> ContractResult<agent_contract::provider::CompletionResponse> {
        unreachable!("stream path only")
    }

    async fn stream(
        &self,
        request: Arc<CompletionRequest>,
    ) -> ContractResult<futures::stream::BoxStream<'static, ProviderEvent>> {
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::to_string(&*request).unwrap_or_default());
        let events = vec![
            ProviderEvent::TextDelta {
                text: "hello ".into(),
            },
            ProviderEvent::TextDelta {
                text: "world".into(),
            },
            ProviderEvent::ToolUseStart {
                id: "t1".into(),
                name: "read_file".into(),
            },
            ProviderEvent::ToolUseDelta {
                id: "t1".into(),
                input_json: "{\"path\":".into(),
            },
            ProviderEvent::Usage(Usage::new(10, 5)),
            ProviderEvent::Stop {
                reason: agent_contract::provider::StopReason::EndTurn,
            },
        ];
        Ok(futures::stream::iter(events).boxed())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            supports_streaming: true,
            supports_tool_use: true,
            supports_vision: true,
            supports_prompt_caching: true,
            max_context_tokens: 200_000,
            max_output_tokens: 8_192,
        }
    }

    fn name(&self) -> &str {
        "scripted"
    }
}

struct CollectingSink {
    events: Vec<r_code_harness_protocol::StreamEvent>,
}

#[async_trait::async_trait]
impl StreamSink for CollectingSink {
    async fn send(
        &mut self,
        event: r_code_harness_protocol::StreamEvent,
    ) -> Result<(), r_code_kernel::ports::ServiceError> {
        self.events.push(event);
        Ok(())
    }
}

fn broker(provider: Arc<ScriptedProvider>) -> ModelBroker {
    let mut registry = ProviderRegistry::new();
    registry.register("scripted/test-model", provider, "test-model");
    registry.set_default("scripted/test-model");
    ModelBroker::new(Arc::new(registry))
}

fn token() -> GenerationToken {
    GenerationToken {
        run_id: "run-1".into(),
        generation: 1,
    }
}

#[tokio::test]
async fn streams_deltas_tool_calls_usage_and_finish() {
    let provider = Arc::new(ScriptedProvider {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let broker = broker(provider.clone());
    let mut sink = CollectingSink { events: Vec::new() };

    let outcome = broker
        .stream(
            token(),
            ModelStreamRequest {
                selection: Some("scripted/test-model".into()),
                messages: vec![ModelMessage {
                    role: ModelRole::User,
                    content: vec![ContentBlock::Text { text: "hi".into() }],
                }],
                tools: vec![ToolDescriptor {
                    name: "read_file".into(),
                    description: "reads".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                }],
                inference: None,
                deadline_ms: Some(10_000),
            },
            &mut sink,
        )
        .await
        .expect("stream");

    assert!(outcome.stream_id.starts_with("model-run-1"));
    assert_eq!(outcome.finish_reason.as_deref(), Some("end_turn"));
    assert_eq!(outcome.usage.input_tokens, Some(10));
    assert_eq!(outcome.usage.output_tokens, Some(5));

    let kinds: Vec<&str> = sink
        .events
        .iter()
        .map(|event| match &event.payload {
            r_code_harness_protocol::StreamPayload::TextDelta { .. } => "text",
            r_code_harness_protocol::StreamPayload::ToolCallDelta { .. } => "tool",
            r_code_harness_protocol::StreamPayload::Usage { .. } => "usage",
            r_code_harness_protocol::StreamPayload::Finish { .. } => "finish",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["text", "text", "tool", "tool", "usage", "finish"]
    );
    assert_eq!(sink.events.last().unwrap().done, Some(true));
    // Sequences are dense from 1.
    assert_eq!(
        sink.events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6]
    );

    // Usage was recorded host-side.
    let records: Vec<UsageRecord> = broker.usage_records().await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].selection, "scripted/test-model");
    assert_eq!(records[0].model, "test-model");
}

#[tokio::test]
async fn tool_messages_and_images_project_into_the_request() {
    let provider = Arc::new(ScriptedProvider {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let broker = broker(provider.clone());
    let mut sink = CollectingSink { events: Vec::new() };

    broker
        .stream(
            token(),
            ModelStreamRequest {
                selection: None,
                messages: vec![
                    ModelMessage {
                        role: ModelRole::User,
                        content: vec![ContentBlock::Text {
                            text: "look".into(),
                        }],
                    },
                    ModelMessage {
                        role: ModelRole::Assistant,
                        content: vec![ContentBlock::ToolCall {
                            id: "t1".into(),
                            name: "read_file".into(),
                            input: serde_json::json!({"path": "a.txt"}),
                        }],
                    },
                    ModelMessage {
                        role: ModelRole::Tool,
                        content: vec![ContentBlock::ToolResult {
                            call_id: "t1".into(),
                            output: vec![OutputBlock::Text {
                                text: "file body".into(),
                            }],
                        }],
                    },
                    ModelMessage {
                        role: ModelRole::User,
                        content: vec![ContentBlock::Image {
                            artifact: r_code_harness_protocol::ArtifactRef {
                                schema: 1,
                                blob_id: "blob:sha256:img1".into(),
                                bytes: 1024,
                                sha256: "img1".into(),
                                media_type: Some("image/png".into()),
                            },
                        }],
                    },
                ],
                tools: vec![],
                inference: None,
                deadline_ms: None,
            },
            &mut sink,
        )
        .await
        .expect("stream with default selection");

    let requests = provider.requests.lock().unwrap();
    let projected: serde_json::Value = serde_json::from_str(&requests[0]).expect("request json");
    let blocks: Vec<serde_json::Value> = projected["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .collect();
    let types: Vec<&str> = blocks
        .iter()
        .map(|block| block["type"].as_str().unwrap_or("text"))
        .collect();
    assert!(
        types.contains(&"tool_use"),
        "tool call projected: {types:?}"
    );
    assert!(
        types.contains(&"tool_result"),
        "tool result projected: {types:?}"
    );
    // The image travels as an artifact reference, not inline bytes.
    let image_note = blocks.iter().any(|block| {
        block["text"]
            .as_str()
            .map(|text| text.contains("[image: blob:sha256:img1]"))
            .unwrap_or(false)
    });
    assert!(image_note, "image reference projected: {blocks:?}");
}

#[tokio::test]
async fn unknown_selections_fail_and_no_keys_cross_the_wire() {
    let provider = Arc::new(ScriptedProvider {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let broker = broker(provider);
    let mut sink = CollectingSink { events: Vec::new() };

    let error = broker
        .stream(
            token(),
            ModelStreamRequest {
                selection: Some("unknown/provider".into()),
                messages: vec![],
                tools: vec![],
                inference: None,
                deadline_ms: None,
            },
            &mut sink,
        )
        .await
        .expect_err("unknown selection");
    assert!(error.to_string().contains("unknown model selection"));

    // The wire-safe settings view exposes only opaque selections.
    let view = broker.selections_view();
    let json = serde_json::to_string(&view).expect("view json");
    assert!(!json.contains("api_key"));
    for forbidden in ["api_key", "apiKey", "Bearer", "credential"] {
        assert!(!json.contains(forbidden), "settings view leaks {forbidden}");
    }
    assert_eq!(view.default.as_deref(), Some("scripted/test-model"));
}

#[tokio::test]
async fn deadlines_cancel_long_streams() {
    /// A provider that never yields an event.
    struct HangingProvider;

    #[async_trait::async_trait]
    impl LlmProvider for HangingProvider {
        async fn complete(
            &self,
            _request: Arc<CompletionRequest>,
        ) -> ContractResult<agent_contract::provider::CompletionResponse> {
            unreachable!()
        }

        async fn stream(
            &self,
            _request: Arc<CompletionRequest>,
        ) -> ContractResult<futures::stream::BoxStream<'static, ProviderEvent>> {
            Ok(futures::stream::pending().boxed())
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                supports_streaming: true,
                supports_tool_use: false,
                supports_vision: false,
                supports_prompt_caching: false,
                max_context_tokens: 0,
                max_output_tokens: 0,
            }
        }

        fn name(&self) -> &str {
            "hanging"
        }
    }

    let mut registry = ProviderRegistry::new();
    registry.register("hanging/model", Arc::new(HangingProvider), "model");
    let broker = ModelBroker::new(Arc::new(registry));
    let mut sink = CollectingSink { events: Vec::new() };

    let started = std::time::Instant::now();
    let error = broker
        .stream(
            token(),
            ModelStreamRequest {
                selection: Some("hanging/model".into()),
                messages: vec![],
                tools: vec![],
                inference: None,
                deadline_ms: Some(300),
            },
            &mut sink,
        )
        .await
        .expect_err("deadline enforced");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(error.to_string().contains("deadline"));
    // The sink received a terminal failure event.
    let last = sink.events.last().expect("failure event");
    assert!(matches!(
        last.payload,
        r_code_harness_protocol::StreamPayload::Failed { .. }
    ));
    assert_eq!(last.done, Some(true));
}
