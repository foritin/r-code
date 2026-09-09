//! T09 — bounded bidirectional stdio transport.
//!
//! Real fixture processes exercise nested callbacks, malformed/oversized
//! output, missing handshake, blocked writer and stderr floods.

use r_code_harness_protocol::rpc::{RpcError, RpcRequest};
use r_code_harness_protocol::services::{InitializeParams, ProtocolLimits, RunIdentity};
use r_code_harness_protocol::{ApiVersion, HostService};
use r_code_runtime::plugins::*;
use std::sync::Arc;
use std::time::Duration;

const HELPER: &str = env!("CARGO_BIN_EXE_harness-test-helper");

struct ServingCallbacks {
    calls: tokio::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl PluginCallbacks for ServingCallbacks {
    async fn handle_request(&self, request: RpcRequest) -> Result<serde_json::Value, RpcError> {
        self.calls.lock().await.push(request.method.clone());
        assert_eq!(request.method, "host.tools.list");
        Ok(serde_json::json!({"tools": [{"name": "read_file"}]}))
    }
}

fn initialize_params() -> InitializeParams {
    InitializeParams {
        protocol: "r-code-harness/1".into(),
        host_api: ApiVersion::new(1, 0),
        identity: RunIdentity {
            task_id: "task-1".into(),
            branch_id: "branch-1".into(),
            run_id: "run-1".into(),
            attempt_id: "attempt-1".into(),
            generation: 1,
        },
        granted_services: HostService::ALL.to_vec(),
        harness_config: serde_json::json!({}),
        limits: ProtocolLimits::default(),
    }
}

#[tokio::test]
async fn serve_mode_handshake_nested_callback_and_cancel() {
    let callbacks = Arc::new(ServingCallbacks {
        calls: tokio::sync::Mutex::new(Vec::new()),
    });
    let process = spawn_plugin(
        HELPER.as_ref(),
        &["serve".into()],
        callbacks.clone(),
        TransportLimits::default(),
    )
    .await
    .expect("spawn");

    let initialized = process
        .initialize(&initialize_params())
        .await
        .expect("handshake");
    assert_eq!(initialized.harness_id, "fixture.harness");

    // harness.start triggers a nested host callback the reader must serve
    // while this request is still pending.
    let started = process
        .request(
            "harness.start",
            serde_json::json!({"identity": {"runId": "run-1"}, "contract": {}}),
            Duration::from_secs(10),
        )
        .await
        .expect("start");
    assert_eq!(started["started"], true);
    let calls = callbacks.calls.lock().await;
    assert_eq!(calls.as_slice(), ["host.tools.list".to_string()]);

    // Graceful cancel: acknowledged within the grace period.
    assert!(process.cancel("user asked").await);
    assert!(!process.is_alive());
}

#[tokio::test]
async fn malformed_output_faults_the_transport() {
    let process = spawn_plugin(
        HELPER.as_ref(),
        &["malformed".into()],
        Arc::new(DenyCallbacks),
        TransportLimits::default(),
    )
    .await
    .expect("spawn");
    let error = process
        .initialize(&initialize_params())
        .await
        .expect_err("malformed frames must fault");
    assert!(matches!(error, TransportError::Fault(_)), "got {error:?}");
    process.kill().await;
}

#[tokio::test]
async fn oversized_output_is_rejected_at_the_frame_limit() {
    let process = spawn_plugin(
        HELPER.as_ref(),
        &["oversized".into()],
        Arc::new(DenyCallbacks),
        TransportLimits::default(),
    )
    .await
    .expect("spawn");
    let error = process
        .initialize(&initialize_params())
        .await
        .expect_err("oversized frame must fault");
    assert!(
        matches!(error, TransportError::Fault(ref reason) if reason.contains("limit")),
        "got {error:?}"
    );
    process.kill().await;
}

#[tokio::test]
async fn missing_handshake_times_out() {
    let limits = TransportLimits {
        initialize_timeout: Duration::from_millis(500),
        ..TransportLimits::default()
    };
    let process = spawn_plugin(
        HELPER.as_ref(),
        &["silent".into()],
        Arc::new(DenyCallbacks),
        limits,
    )
    .await
    .expect("spawn");
    let error = process
        .initialize(&initialize_params())
        .await
        .expect_err("silent plugin must time out");
    assert!(matches!(error, TransportError::Timeout(_)), "got {error:?}");
    process.kill().await;
}

#[tokio::test]
async fn blocked_writer_overflows_the_bounded_queue() {
    let limits = TransportLimits {
        initialize_timeout: Duration::from_secs(10),
        max_queue_bytes: 64 * 1024,
        ..TransportLimits::default()
    };
    let process = spawn_plugin(
        HELPER.as_ref(),
        &["blocked".into()],
        Arc::new(DenyCallbacks),
        limits,
    )
    .await
    .expect("spawn");

    // Push frames until the bounded queue refuses. The plugin never reads,
    // so pending bytes accumulate.
    let payload = "y".repeat(16 * 1024);
    let mut overflowed = false;
    for _ in 0..16 {
        let notification = r_code_harness_protocol::rpc::RpcMessage::Notification(
            r_code_harness_protocol::rpc::RpcNotification {
                jsonrpc: "2.0".into(),
                method: "harness.event".into(),
                params: Some(serde_json::json!({"blob": payload})),
            },
        );
        match process.notify("harness.event", serde_json::json!({"blob": payload})) {
            Ok(()) => {
                let _ = notification;
            }
            Err(TransportError::QueueOverflow(_)) => {
                overflowed = true;
                break;
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
    assert!(overflowed, "queue bound never enforced");
    process.kill().await;
}

#[tokio::test]
async fn stderr_flood_stays_bounded() {
    let process = spawn_plugin(
        HELPER.as_ref(),
        &["stderr-flood".into()],
        Arc::new(DenyCallbacks),
        TransportLimits::default(),
    )
    .await
    .expect("spawn");
    let initialized = process
        .initialize(&initialize_params())
        .await
        .expect("handshake survives stderr flood");
    assert_eq!(initialized.harness_id, "fixture.stderr");

    // Give the flood time to finish and the tail to settle.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let tail = process.stderr_tail().await;
    assert!(
        tail.len() <= STDERR_TAIL_BYTES,
        "stderr tail grew to {}",
        tail.len()
    );
    assert!(!tail.is_empty(), "some diagnostics should be retained");
    assert!(tail.windows(10).any(|w| w == b"noise line"));
    process.kill().await;
}

#[tokio::test]
async fn ignored_cancellation_is_killed_after_grace() {
    let limits = TransportLimits {
        cancel_grace: Duration::from_millis(750),
        ..TransportLimits::default()
    };
    let process = spawn_plugin(
        HELPER.as_ref(),
        &["ignore-cancel".into()],
        Arc::new(DenyCallbacks),
        limits,
    )
    .await
    .expect("spawn");
    process
        .initialize(&initialize_params())
        .await
        .expect("initialize");

    let started = std::time::Instant::now();
    let acknowledged = process.cancel("force").await;
    assert!(!acknowledged, "plugin never acknowledged");
    assert!(
        !process.is_alive(),
        "process must be killed after the grace period"
    );
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "cancel must not hang beyond grace + kill"
    );
}
