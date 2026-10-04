//! Shared chaos fixtures for the agent-loop resilience worklist (A00+).
//!
//! A fault-injecting [`LlmProvider`]: each `stream` call consumes the next
//! scripted fault (empty script = a clean turn), counts invocations, and
//! records the serialized request body so replay tests can assert that the
//! same frozen request was re-sent.

// 故障词汇表为共享全集，各用例只注入子集，未用变体保留。
#![allow(dead_code)]

use agent_contract::provider::{CompletionRequest, LlmProvider, StreamEvent as ProviderEvent};
use agent_contract::{Capabilities, Error, Result as ContractResult, Usage};
use futures::StreamExt;
use r_code_harness_protocol::services::ModelMessage;
use r_code_harness_protocol::StreamEvent;
use r_code_kernel::ports::{GenerationToken, ServiceError, StreamSink};
use r_code_runtime::providers::ProviderRegistry;
use r_code_runtime::services::models::ModelBroker;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The fault injected on the next `stream` call.
#[derive(Debug, Clone, PartialEq)]
pub enum ChaosFault {
    /// Two text deltas, then the stream just ENDS — no Stop event at all.
    /// The silent-truncation shape (pre-A01 transport drop).
    DropSilent,
    /// Ends with `Stop{Other("stream_idle_timeout")}` (the watchdog marker).
    IdleStall,
    /// Ends with `Stop{Other("api_error: stream transport error: …")}`
    /// (the post-A01 aligned transport-error shape).
    TransportApiError,
    /// The request fails before any events with `RateLimited{retry_after}`.
    Rate429 { retry_after: u64 },
    /// The request fails with `ApiError{status: 503}`.
    Server5xx,
    /// The request fails with `ApiError{status: 400}` carrying a
    /// context-length-overflow message.
    Overflow400,
    /// Completes normally with `Stop{MaxTokens}`.
    MaxTokens,
    /// `Stop{EndTurn}`, Usage, then a SECOND `Stop{EndTurn}`.
    DoubleStop,
    /// The initial `stream()` future hangs (connect/TLS black hole) before
    /// any events; pairs with a short `deadline_ms` to exercise the A02
    /// connect-deadline.
    ConnectHang,
    /// Emits one delta, stalls for `ms`, then completes normally; pairs with
    /// a shorter `deadline_ms` to exercise the mid-stream timeout path.
    MidStreamStallMs(u64),
    /// Ends with `Stop{Other("some future marker")}` — an unknown value that
    /// must fail open (DEC-3).
    UnknownOther,
}

/// The partial-text payload size carried by DropSilent (big enough to exceed
/// the broker's 4KB partial cap in tests).
const BIG_DELTA_CHARS: usize = 5_200;

pub struct ChaosProvider {
    script: Mutex<Vec<ChaosFault>>,
    /// Completed `stream` invocations (counted at entry).
    pub calls: AtomicUsize,
    /// Serialized request bodies in call order.
    pub requests: Mutex<Vec<String>>,
}

impl ChaosProvider {
    pub fn new(script: Vec<ChaosFault>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// Append one more fault to the script.
    pub fn push(&self, fault: ChaosFault) {
        self.script.lock().unwrap().push(fault);
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The serialized body of the n-th request (0-based).
    pub fn request_body(&self, index: usize) -> String {
        self.requests.lock().unwrap()[index].clone()
    }
}

fn transport_error_stop() -> ProviderEvent {
    ProviderEvent::Stop {
        reason: agent_contract::provider::StopReason::Other(
            "api_error: stream transport error: connection reset by peer".into(),
        ),
    }
}

#[async_trait::async_trait]
impl LlmProvider for ChaosProvider {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::to_string(&*request).unwrap_or_default());
        let fault = self.script.lock().unwrap().first().cloned();
        if self.script.lock().unwrap().len() > 1 {
            self.script.lock().unwrap().remove(0);
        }
        if fault == Some(ChaosFault::ConnectHang) {
            // 连接黑洞：首个 await 挂住，交给 A02 的 connect deadline 切断。
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
        if let Some(ChaosFault::MidStreamStallMs(ms)) = fault {
            use futures::StreamExt;
            let stall = std::time::Duration::from_millis(ms);
            let stream = futures::stream::iter(vec![ProviderEvent::TextDelta {
                text: "partial ".into(),
            }])
            .chain(futures::stream::once(async move {
                tokio::time::sleep(stall).await;
                ProviderEvent::Stop {
                    reason: agent_contract::provider::StopReason::EndTurn,
                }
            }))
            .boxed();
            return Ok(stream);
        }
        // ConnectHang 与 MidStreamStallMs 已在上方提前返回。
        let events = match fault {
            None
            | Some(ChaosFault::DropSilent)
            | Some(ChaosFault::IdleStall)
            | Some(ChaosFault::TransportApiError)
            | Some(ChaosFault::MaxTokens)
            | Some(ChaosFault::DoubleStop)
            | Some(ChaosFault::UnknownOther)
            | Some(ChaosFault::ConnectHang)
            | Some(ChaosFault::MidStreamStallMs(_)) => {
                let mut events = vec![
                    ProviderEvent::TextDelta {
                        text: "partial ".into(),
                    },
                    ProviderEvent::TextDelta {
                        text: match fault {
                            // DropSilent 带大载荷：让 partial 截断可测。
                            Some(ChaosFault::DropSilent) => "x".repeat(BIG_DELTA_CHARS),
                            _ => "answer".into(),
                        },
                    },
                ];
                match fault {
                    Some(ChaosFault::DropSilent) => {} // ends silently: no Stop
                    Some(ChaosFault::IdleStall) => events.push(ProviderEvent::Stop {
                        reason: agent_contract::provider::StopReason::Other(
                            "stream_idle_timeout".into(),
                        ),
                    }),
                    Some(ChaosFault::TransportApiError) => {
                        events.push(transport_error_stop());
                    }
                    Some(ChaosFault::MaxTokens) => {
                        events.push(ProviderEvent::Usage(Usage::new(10, 5)));
                        events.push(ProviderEvent::Stop {
                            reason: agent_contract::provider::StopReason::MaxTokens,
                        });
                    }
                    Some(ChaosFault::UnknownOther) => {
                        events.push(ProviderEvent::Stop {
                            reason: agent_contract::provider::StopReason::Other(
                                "some future marker".into(),
                            ),
                        });
                    }
                    Some(ChaosFault::DoubleStop) => {
                        events.push(ProviderEvent::Stop {
                            reason: agent_contract::provider::StopReason::EndTurn,
                        });
                        events.push(ProviderEvent::Usage(Usage::new(10, 5)));
                        events.push(ProviderEvent::Stop {
                            reason: agent_contract::provider::StopReason::EndTurn,
                        });
                    }
                    _ => {
                        events.push(ProviderEvent::Usage(Usage::new(10, 5)));
                        events.push(ProviderEvent::Stop {
                            reason: agent_contract::provider::StopReason::EndTurn,
                        });
                    }
                }
                Ok(futures::stream::iter(events).boxed())
            }
            Some(ChaosFault::Rate429 { retry_after }) => Err(Error::RateLimited { retry_after }),
            Some(ChaosFault::Server5xx) => Err(Error::ApiError {
                status: 503,
                message: "upstream unavailable".into(),
            }),
            Some(ChaosFault::Overflow400) => Err(Error::ApiError {
                status: 400,
                message: "this model's maximum context length is 128000 tokens, however you \
                          requested 200000 tokens"
                    .into(),
            }),
        };
        events
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
        "chaos"
    }
}

/// Broker over a [`ChaosProvider`] under the selection `chaos/test-model`.
pub fn chaos_broker(provider: Arc<ChaosProvider>) -> ModelBroker {
    let mut registry = ProviderRegistry::new();
    registry.register("chaos/test-model", provider, "test-model");
    registry.set_default("chaos/test-model");
    ModelBroker::new(Arc::new(registry))
}

pub fn chaos_token() -> GenerationToken {
    GenerationToken {
        run_id: "run-1".into(),
        generation: 1,
    }
}

/// Minimal one-turn user request for broker-level smoke tests.
pub fn plain_request(text: &str) -> r_code_harness_protocol::services::ModelStreamRequest {
    use r_code_harness_protocol::services::{ContentBlock, ModelRole};
    r_code_harness_protocol::services::ModelStreamRequest {
        selection: Some("chaos/test-model".into()),
        messages: vec![ModelMessage {
            role: ModelRole::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }],
        tools: vec![],
        inference: None,
        deadline_ms: None,
    }
}

/// Collects stream events for assertions (shared version of t15's sink).
pub struct CollectingSink {
    pub events: Vec<StreamEvent>,
}

#[async_trait::async_trait]
impl StreamSink for CollectingSink {
    async fn send(&mut self, event: StreamEvent) -> Result<(), ServiceError> {
        self.events.push(event);
        Ok(())
    }
}
