//! R00 — transport seam: the daemon's connection loop runs over any
//! `AppStream` byte transport, injected. The named-pipe/Unix path keeps its
//! exact behavior (t06a/t06b/t35 regressions run the same loop through the
//! real endpoint); until R04 nothing in the runtime may open a network
//! listener.

use r_code_harness_protocol::application::{ApplicationFrame, DaemonHandshake};
use r_code_runtime::daemon::{serve_app_frames, ApplicationHandler, OwnerIdentity};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct EchoHandler;

#[async_trait::async_trait]
impl ApplicationHandler for EchoHandler {
    async fn execute(
        &self,
        command: r_code_harness_protocol::application::ApplicationCommand,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "echo": command.params,
            "client": command.client_id,
        }))
    }

    async fn events_after(
        &self,
        _after_seq: u64,
        _limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        vec![]
    }
}

fn identity() -> OwnerIdentity {
    OwnerIdentity {
        pid: std::process::id(),
        nonce: "nonce-r00".into(),
        token: "token-r00".into(),
        profile_id: "profile-r00".into(),
        started_unix_ms: 0,
    }
}

async fn write_line<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, frame: &ApplicationFrame) {
    let payload = serde_json::to_vec(frame).unwrap();
    writer.write_all(&payload).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();
}

// -- R00.A1: injected transport serves the identical frame protocol --------

#[tokio::test]
async fn r00_a1_injected_stream_serves_the_frame_protocol() {
    // An in-memory duplex stands in for any transport (pipe, Unix socket,
    // future WS): the loop neither knows nor cares.
    let (client_side, server_side) = tokio::io::duplex(8 * 1024);
    let owner = identity();
    let handler: Arc<dyn ApplicationHandler> = Arc::new(EchoHandler);
    let server =
        tokio::spawn(
            async move { serve_app_frames(Box::new(server_side), &owner, &handler).await },
        );

    let (reader, mut writer) = tokio::io::split(client_side);
    let mut lines = BufReader::new(reader);

    // Handshake with the owner token → welcome.
    write_line(
        &mut writer,
        &ApplicationFrame::Handshake(DaemonHandshake {
            protocol: "r-code-service/1".into(),
            profile_id: "profile-r00".into(),
            token: "token-r00".into(),
            client_id: Some("client-1".into()),
        }),
    )
    .await;
    let mut line = String::new();
    lines.read_line(&mut line).await.unwrap();
    let welcome: ApplicationFrame = serde_json::from_str(line.trim()).unwrap();
    assert!(
        matches!(&welcome, ApplicationFrame::Welcome(w) if w.profile_id == "profile-r00"),
        "welcome after token handshake"
    );

    // A command round-trips through the same handler path.
    write_line(
        &mut writer,
        &ApplicationFrame::Command(r_code_harness_protocol::application::ApplicationCommand {
            client_id: "client-1".into(),
            command_id: "cmd-1".into(),
            method: "echo".into(),
            params: serde_json::json!({"x": 1}),
        }),
    )
    .await;
    let mut line = String::new();
    lines.read_line(&mut line).await.unwrap();
    let result: ApplicationFrame = serde_json::from_str(line.trim()).unwrap();
    match result {
        ApplicationFrame::Result(result) => {
            assert!(result.outcome.is_ok());
            assert_eq!(result.command_id, "cmd-1");
        }
        other => panic!("expected result frame, got {other:?}"),
    }

    // A bad token is refused (fail closed) — protocol bytes unchanged.
    drop(server);
    let (client_side, server_side) = tokio::io::duplex(8 * 1024);
    let owner = identity();
    let handler: Arc<dyn ApplicationHandler> = Arc::new(EchoHandler);
    tokio::spawn(async move {
        let _ = serve_app_frames(Box::new(server_side), &owner, &handler).await;
    });
    let (reader, mut writer) = tokio::io::split(client_side);
    let mut lines = BufReader::new(reader);
    write_line(
        &mut writer,
        &ApplicationFrame::Handshake(DaemonHandshake {
            protocol: "r-code-service/1".into(),
            profile_id: "profile-r00".into(),
            token: "wrong-token".into(),
            client_id: None,
        }),
    )
    .await;
    let mut line = String::new();
    lines.read_line(&mut line).await.unwrap();
    let rejected: ApplicationFrame = serde_json::from_str(line.trim()).unwrap();
    assert!(
        matches!(rejected, ApplicationFrame::Error { .. }),
        "bad token rejected"
    );
}

// -- R00.A1 (guard): network listening confined to the remote listener ----

#[test]
fn r00_a1_network_listening_is_confined_to_the_remote_listener() {
    // After R04 the *only* permitted network listener is the remote
    // listener module (F2's single, pairing-gated surface); anywhere else
    // in the runtime stays listener-free.
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let label = path.display().to_string().replace('\\', "/");
                let is_listener_module = label.ends_with("remote/listener.rs");
                if text.contains("TcpListener") && !is_listener_module {
                    offenders.push(path.display().to_string());
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "runtime network listening must stay confined to remote/listener.rs (F2 zero default surface): {offenders:?}"
    );
}
