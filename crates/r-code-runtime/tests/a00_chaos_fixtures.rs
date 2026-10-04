//! A00 — chaos fixtures: one smoke test per fault point.
//!
//! Provider-level assertions pin the FAULT SHAPE (stable across A02's broker
//! changes); broker-level assertions cover request-error faults, the clean
//! baseline and invocation counting. The silent-truncation *outcome* flip is
//! pinned by a03's regression tests, not here.

mod common;

use agent_contract::provider::{LlmProvider, StreamEvent as ProviderEvent};
use common::{chaos_broker, chaos_token, plain_request, ChaosFault, ChaosProvider, CollectingSink};
use r_code_kernel::ModelService;
use std::sync::Arc;

#[tokio::test]
async fn a00_clean_turn_shape() {
    let provider = ChaosProvider::new(vec![]);
    let mut sink = CollectingSink { events: vec![] };
    let outcome = chaos_broker(provider.clone())
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await;
    let outcome = outcome.expect("clean turn streams");
    assert_eq!(outcome.finish_reason.as_deref(), Some("end_turn"));
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn a00_drop_silent_shape() {
    let provider = ChaosProvider::new(vec![ChaosFault::DropSilent]);
    let stream = provider
        .stream(empty_completion_request())
        .await
        .expect("drop fault streams");
    let events: Vec<ProviderEvent> = futures::StreamExt::collect(stream).await;
    assert!(events
        .iter()
        .any(|e| matches!(e, ProviderEvent::TextDelta { .. })));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Stop { .. })),
        "drop-silent must carry no Stop event"
    );
}

#[tokio::test]
async fn a00_idle_stall_shape() {
    let provider = ChaosProvider::new(vec![ChaosFault::IdleStall]);
    let stream = provider
        .stream(empty_completion_request())
        .await
        .expect("idle fault streams");
    let events: Vec<ProviderEvent> = futures::StreamExt::collect(stream).await;
    let stops: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Stop {
                reason: agent_contract::provider::StopReason::Other(other),
            } => Some(other),
            _ => None,
        })
        .collect();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0], "stream_idle_timeout");
}

#[tokio::test]
async fn a00_transport_api_error_shape() {
    let provider = ChaosProvider::new(vec![ChaosFault::TransportApiError]);
    let stream = provider
        .stream(empty_completion_request())
        .await
        .expect("transport fault streams");
    let events: Vec<ProviderEvent> = futures::StreamExt::collect(stream).await;
    let stops: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Stop {
                reason: agent_contract::provider::StopReason::Other(other),
            } => Some(other),
            _ => None,
        })
        .collect();
    assert_eq!(stops.len(), 1);
    assert!(stops[0].starts_with("api_error: stream transport error"));
}

#[tokio::test]
async fn a00_max_tokens_shape() {
    let provider = ChaosProvider::new(vec![ChaosFault::MaxTokens]);
    let mut sink = CollectingSink { events: vec![] };
    let outcome = chaos_broker(provider.clone())
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect("max_tokens turn streams");
    assert_eq!(outcome.finish_reason.as_deref(), Some("max_tokens"));
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn a00_double_stop_shape() {
    let provider = ChaosProvider::new(vec![ChaosFault::DoubleStop]);
    let stream = provider
        .stream(empty_completion_request())
        .await
        .expect("double stop streams");
    let events: Vec<ProviderEvent> = futures::StreamExt::collect(stream).await;
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ProviderEvent::Stop { .. }))
            .count(),
        2,
        "double-stop fault emits exactly two Stops"
    );
}

#[tokio::test]
async fn a00_rate429_is_request_error() {
    let provider = ChaosProvider::new(vec![ChaosFault::Rate429 { retry_after: 30 }]);
    let mut sink = CollectingSink { events: vec![] };
    let err = chaos_broker(provider.clone())
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect_err("429 must surface as a request error");
    assert!(err.to_string().contains("30"), "message: {err}");
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn a00_server5xx_is_request_error() {
    let provider = ChaosProvider::new(vec![ChaosFault::Server5xx]);
    let mut sink = CollectingSink { events: vec![] };
    let err = chaos_broker(provider.clone())
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect_err("5xx must surface as a request error");
    assert!(err.to_string().contains("503"), "message: {err}");
}

#[tokio::test]
async fn a00_overflow400_is_request_error() {
    let provider = ChaosProvider::new(vec![ChaosFault::Overflow400]);
    let mut sink = CollectingSink { events: vec![] };
    let err = chaos_broker(provider.clone())
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect_err("overflow must surface as a request error");
    let message = err.to_string();
    assert!(
        message.contains("400") || message.to_lowercase().contains("context"),
        "message: {message}"
    );
}

/// An empty-but-valid completion request for provider-direct stream calls.
fn empty_completion_request() -> Arc<agent_contract::provider::CompletionRequest> {
    use agent_contract::provider::CompletionRequest;
    Arc::new(CompletionRequest {
        model: "test-model".into(),
        system: None,
        messages: vec![],
        tools: vec![],
        hosted_tools: vec![],
        max_tokens: 128,
        temperature: None,
        enable_caching: false,
        inference: Default::default(),
    })
}
