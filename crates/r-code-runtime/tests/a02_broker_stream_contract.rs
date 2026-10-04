//! A02 — broker stream completeness contract + protocol hygiene.

mod common;

use common::{chaos_broker, chaos_token, plain_request, ChaosFault, ChaosProvider, CollectingSink};
use r_code_harness_protocol::StreamPayload;
use r_code_kernel::ModelService;

fn request_with_deadline(
    deadline_ms: u64,
) -> r_code_harness_protocol::services::ModelStreamRequest {
    let mut request = plain_request("hi");
    request.deadline_ms = Some(deadline_ms);
    request
}

#[tokio::test]
async fn a02_incomplete_stream_returns_error_not_ok() {
    let provider = ChaosProvider::new(vec![ChaosFault::DropSilent]);
    let mut sink = CollectingSink { events: vec![] };
    let broker = chaos_broker(provider.clone());
    let err = broker
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect_err("silent drop must not surface as Ok");
    let message = err.to_string();
    assert!(message.contains("incomplete"), "message: {message}");
    assert!(
        message.contains("partial "),
        "partial text embedded: {message}"
    );
    // 4KB partial cap (DEC/part of A02): the 5.2KB scripted delta is truncated.
    assert!(
        message.contains("[truncated"),
        "partial must be capped: {} bytes",
        message.len()
    );
    assert!(
        message.len() < 5_600,
        "message must stay near the cap: {}",
        message.len()
    );
}

#[tokio::test]
async fn a02_idle_timeout_classified_incomplete() {
    let provider = ChaosProvider::new(vec![ChaosFault::IdleStall]);
    let mut sink = CollectingSink { events: vec![] };
    let err = chaos_broker(provider)
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect_err("idle timeout is an incomplete stream");
    assert!(
        err.to_string().contains("stream_idle_timeout"),
        "message: {err}"
    );
}

#[tokio::test]
async fn a02_api_error_stop_classified_incomplete() {
    let provider = ChaosProvider::new(vec![ChaosFault::TransportApiError]);
    let mut sink = CollectingSink { events: vec![] };
    let err = chaos_broker(provider)
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect_err("transport api_error Stop is an incomplete stream");
    assert!(
        err.to_string()
            .contains("api_error: stream transport error"),
        "message: {err}"
    );
}

#[tokio::test]
async fn a02_max_tokens_ok_flagged_truncated() {
    let provider = ChaosProvider::new(vec![ChaosFault::MaxTokens]);
    let mut sink = CollectingSink { events: vec![] };
    let outcome = chaos_broker(provider)
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect("max_tokens is a complete (truncated) turn");
    assert_eq!(outcome.finish_reason.as_deref(), Some("max_tokens"));
}

#[tokio::test]
async fn a02_unknown_reason_fail_open() {
    let provider = ChaosProvider::new(vec![ChaosFault::UnknownOther]);
    let mut sink = CollectingSink { events: vec![] };
    let outcome = chaos_broker(provider)
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect("unknown Other values fail open (DEC-3)");
    assert_eq!(outcome.finish_reason.as_deref(), Some("some future marker"));
}

#[tokio::test]
async fn a02_connect_hang_hits_deadline() {
    let provider = ChaosProvider::new(vec![ChaosFault::ConnectHang]);
    let mut sink = CollectingSink { events: vec![] };
    let started = std::time::Instant::now();
    let err = chaos_broker(provider)
        .stream(chaos_token(), request_with_deadline(1_000), &mut sink)
        .await
        .expect_err("connect black hole must hit the deadline");
    assert!(
        err.to_string().contains("connect deadline"),
        "message: {err}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn a02_deadline_zero_clamped() {
    let provider = ChaosProvider::new(vec![]);
    let mut sink = CollectingSink { events: vec![] };
    let outcome = chaos_broker(provider)
        .stream(chaos_token(), request_with_deadline(0), &mut sink)
        .await
        .expect("deadline_ms=0 clamps to 1s minimum instead of failing instantly");
    assert_eq!(outcome.finish_reason.as_deref(), Some("end_turn"));
}

#[tokio::test]
async fn a02_timeout_failed_sequence_increments() {
    let provider = ChaosProvider::new(vec![ChaosFault::MidStreamStallMs(3_000)]);
    let mut sink = CollectingSink { events: vec![] };
    let err = chaos_broker(provider)
        .stream(chaos_token(), request_with_deadline(1_000), &mut sink)
        .await
        .expect_err("mid-stream stall must hit the deadline");
    assert!(err.to_string().contains("deadline"), "message: {err}");
    let last = sink.events.last().expect("Failed event sent");
    assert_eq!(
        last.sequence, 2,
        "delta was 1; Failed must be 2 (incremented)"
    );
    assert!(matches!(last.payload, StreamPayload::Failed { .. }));
    assert_eq!(last.done, Some(true));
}

#[tokio::test]
async fn a02_done_is_last_event() {
    let provider = ChaosProvider::new(vec![ChaosFault::DoubleStop]);
    let mut sink = CollectingSink { events: vec![] };
    let outcome = chaos_broker(provider)
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect("first Stop completes the stream");
    assert_eq!(outcome.finish_reason.as_deref(), Some("end_turn"));
    let finishes = sink
        .events
        .iter()
        .filter(|event| matches!(event.payload, StreamPayload::Finish { .. }))
        .count();
    assert_eq!(
        finishes, 1,
        "double-Stop provider must yield exactly one Finish"
    );
    assert!(
        matches!(
            sink.events.last().expect("non-empty").payload,
            StreamPayload::Finish { .. }
        ),
        "done:true must be the final event"
    );
    for window in sink.events.windows(2) {
        assert!(
            window[0].sequence < window[1].sequence,
            "sequences strictly increasing"
        );
    }
}

#[tokio::test]
async fn a02_stream_id_unique_per_turn() {
    let provider = ChaosProvider::new(vec![]);
    let broker = chaos_broker(provider);
    let mut sink = CollectingSink { events: vec![] };
    let first = broker
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect("first turn");
    let mut sink = CollectingSink { events: vec![] };
    let second = broker
        .stream(chaos_token(), plain_request("again"), &mut sink)
        .await
        .expect("second turn");
    assert_ne!(first.stream_id, second.stream_id);
}

#[tokio::test]
async fn a02_usage_recorded_on_error_paths() {
    let provider = ChaosProvider::new(vec![ChaosFault::Server5xx]);
    let mut sink = CollectingSink { events: vec![] };
    let broker = chaos_broker(provider);
    let _ = broker
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect_err("5xx");
    let records = broker.usage_records().await;
    assert_eq!(
        records.len(),
        1,
        "request-error exit must still record usage"
    );

    let provider = ChaosProvider::new(vec![ChaosFault::DropSilent]);
    let mut sink = CollectingSink { events: vec![] };
    let broker = chaos_broker(provider);
    let _ = broker
        .stream(chaos_token(), plain_request("hi"), &mut sink)
        .await
        .expect_err("incomplete stream");
    assert_eq!(
        broker.usage_records().await.len(),
        1,
        "incomplete-stream exit must still record usage"
    );
}
