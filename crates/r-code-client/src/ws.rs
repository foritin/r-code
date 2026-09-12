//! Remote (WebSocket over TLS) application client (R06, F3/F13).
//!
//! Same command semantics as the pipe/unix [`DaemonClient`] — durable
//! `(client_id, command_id)` identity, receipt replay on reconnect — over
//! a device-token-authenticated TLS connection pinned to the paired
//! fingerprint. No tauri, no runtime dependency: pure tokio + rustls.

use futures_util::{SinkExt, StreamExt};
use r_code_harness_protocol::application::{
    ApplicationCommand, ApplicationFrame, ApplicationResult, EventsSubscribeRequest,
};
use r_code_harness_protocol::EventEnvelope;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::ClientError;

/// Where and how to reach the daemon remotely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEndpoint {
    pub host: String,
    pub port: u16,
    /// SHA-256 (lowercase hex) of the daemon certificate — the pin from
    /// pairing. Any other certificate fails the TLS handshake (F3).
    pub fingerprint: String,
    /// Device token (one-time secret from pairing; F4).
    pub token: String,
    pub device_id: String,
}

/// One authenticated remote connection.
pub struct RemoteClient {
    ws: WebSocketStream<TlsStream<TcpStream>>,
    client_id: String,
}

fn fingerprint_of(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The client-side pin verifier: accepts exactly one fingerprint.
#[derive(Debug)]
struct PinnedVerifier {
    fingerprint: String,
}

impl tokio_rustls::rustls::client::danger::ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &tokio_rustls::rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[tokio_rustls::rustls::pki_types::CertificateDer<'_>],
        _server_name: &tokio_rustls::rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: tokio_rustls::rustls::pki_types::UnixTime,
    ) -> Result<tokio_rustls::rustls::client::danger::ServerCertVerified, tokio_rustls::rustls::Error>
    {
        if fingerprint_of(end_entity.as_ref()) == self.fingerprint {
            Ok(tokio_rustls::rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(tokio_rustls::rustls::Error::General(
                "certificate fingerprint mismatch".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &tokio_rustls::rustls::pki_types::CertificateDer<'_>,
        dss: &tokio_rustls::rustls::DigitallySignedStruct,
    ) -> Result<
        tokio_rustls::rustls::client::danger::HandshakeSignatureValid,
        tokio_rustls::rustls::Error,
    > {
        tokio_rustls::rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &tokio_rustls::rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &tokio_rustls::rustls::pki_types::CertificateDer<'_>,
        dss: &tokio_rustls::rustls::DigitallySignedStruct,
    ) -> Result<
        tokio_rustls::rustls::client::danger::HandshakeSignatureValid,
        tokio_rustls::rustls::Error,
    > {
        tokio_rustls::rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &tokio_rustls::rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<tokio_rustls::rustls::SignatureScheme> {
        tokio_rustls::rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl RemoteClient {
    /// Connect, verify the pinned fingerprint, authenticate the device
    /// token (hello frame, architecture §4). `Unreachable` = transport
    /// down; `Handshake` = the daemon refused the token.
    pub async fn connect(endpoint: &RemoteEndpoint) -> Result<Self, ClientError> {
        let tcp = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
            .await
            .map_err(|e| ClientError::Unreachable(e.to_string()))?;
        let config = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| ClientError::Io(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
            fingerprint: endpoint.fingerprint.clone(),
        }))
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let name =
            tokio_rustls::rustls::pki_types::ServerName::try_from("r-code-daemon".to_string())
                .map_err(|e| ClientError::Io(e.to_string()))?;
        let tls = connector
            .connect(name, tcp)
            .await
            .map_err(|e| ClientError::Unreachable(format!("TLS (pin mismatch?): {e}")))?;
        let (mut ws, _response) = tokio_tungstenite::client_async_with_config(
            format!("wss://r-code-daemon:{}/remote", endpoint.port),
            tls,
            None,
        )
        .await
        .map_err(|e| ClientError::Unreachable(format!("websocket: {e}")))?;

        // Device hello (the token authenticates; the daemon answers ok/err).
        let hello = serde_json::json!({
            "hello": "r-code-remote/1",
            "device_id": endpoint.device_id,
            "token": endpoint.token,
            "client_id": endpoint.device_id,
        });
        ws.send(Message::Text(hello.to_string().into()))
            .await
            .map_err(|e| ClientError::Io(e.to_string()))?;
        // The welcome is the remote handshake shape ({"ok":true,...} or
        // {"ok":false,"error":{"code":...}}), not an ApplicationFrame.
        let welcome: serde_json::Value = {
            let timeout = Duration::from_secs(30);
            let message = tokio::time::timeout(timeout, ws.next())
                .await
                .map_err(|_| ClientError::Io("read timeout".into()))?
                .ok_or_else(|| ClientError::Io("connection closed".into()))?
                .map_err(|e| ClientError::Io(e.to_string()))?;
            match message {
                Message::Text(text) => {
                    serde_json::from_str(&text).map_err(|e| ClientError::Protocol(e.to_string()))?
                }
                _ => return Err(ClientError::Protocol("expected welcome".into())),
            }
        };
        match welcome.get("ok").and_then(|ok| ok.as_bool()) {
            Some(true) => Ok(Self {
                ws,
                client_id: endpoint.device_id.clone(),
            }),
            Some(false) => {
                let code = welcome["error"]["code"].as_str().unwrap_or("unauthorized");
                Err(ClientError::Handshake(code.to_string()))
            }
            _ => Err(ClientError::Protocol("malformed welcome".into())),
        }
    }

    async fn next_frame(
        ws: &mut WebSocketStream<TlsStream<TcpStream>>,
    ) -> Result<ApplicationFrame, ClientError> {
        let timeout = Duration::from_secs(30);
        let message = tokio::time::timeout(timeout, ws.next())
            .await
            .map_err(|_| ClientError::Io("read timeout".into()))?
            .ok_or_else(|| ClientError::Io("connection closed".into()))?
            .map_err(|e| ClientError::Io(e.to_string()))?;
        match message {
            Message::Text(text) => serde_json::from_str(&text)
                .map_err(|e| ClientError::Protocol(format!("bad frame: {e}"))),
            Message::Ping(_) | Message::Pong(_) => {
                // Recurse via a boxed future (ping/pong frames are rare).
                Box::pin(Self::next_frame(ws)).await
            }
            Message::Close(frame) => Err(ClientError::Io(format!(
                "closed: {:?}",
                frame.map(|f| f.reason)
            ))),
            _ => Err(ClientError::Protocol("binary frames unsupported".into())),
        }
    }

    /// Execute one command with a fresh uuid command id (same shape as
    /// [`crate::DaemonClient::call`]).
    pub async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let command_id = format!("cmd-{}", uuid::Uuid::new_v4().simple());
        self.call_with_id(method, params, &command_id).await
    }

    /// Execute with an explicit command id — reconnecting with the same id
    /// replays the original result (daemon-side receipts; F7).
    pub async fn call_with_id(
        &mut self,
        method: &str,
        params: serde_json::Value,
        command_id: &str,
    ) -> Result<serde_json::Value, ClientError> {
        let command = ApplicationCommand {
            client_id: self.client_id.clone(),
            command_id: command_id.to_string(),
            method: method.to_string(),
            params,
        };
        self.ws
            .send(Message::Text(
                serde_json::to_string(&ApplicationFrame::Command(command))
                    .map_err(|e| ClientError::Io(e.to_string()))?
                    .into(),
            ))
            .await
            .map_err(|e| ClientError::Io(e.to_string()))?;
        match Self::next_frame(&mut self.ws).await? {
            ApplicationFrame::Result(ApplicationResult { outcome, .. }) => {
                outcome.map_err(ClientError::Command)
            }
            ApplicationFrame::Error { message } => Err(ClientError::Protocol(message)),
            _ => Err(ClientError::Protocol("expected result".into())),
        }
    }

    /// Subscribe to the live event stream from a cursor: the first answer
    /// carries the history replay, later frames are live pushes (F8).
    pub async fn subscribe_events(
        &mut self,
        after_seq: u64,
    ) -> Result<Vec<EventEnvelope>, ClientError> {
        self.ws
            .send(Message::Text(
                serde_json::to_string(&ApplicationFrame::EventsSubscribe(EventsSubscribeRequest {
                    after_seq,
                }))
                .map_err(|e| ClientError::Io(e.to_string()))?
                .into(),
            ))
            .await
            .map_err(|e| ClientError::Io(e.to_string()))?;
        self.next_events().await
    }

    /// Next raw wire message (None on close). Management surfaces (R11)
    /// prove forced disconnects by watching the socket go quiet.
    pub async fn next_wire_message(&mut self) -> Option<tokio_tungstenite::tungstenite::Message> {
        loop {
            let message = futures_util::StreamExt::next(&mut self.ws).await?;
            let message = match message {
                Ok(message) => message,
                Err(_) => return None,
            };
            match message {
                tokio_tungstenite::tungstenite::Message::Close(frame) => {
                    return Some(tokio_tungstenite::tungstenite::Message::Close(frame))
                }
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    return Some(tokio_tungstenite::tungstenite::Message::Text(text))
                }
                _ => continue,
            }
        }
    }

    /// The next (non-empty) live events frame.
    pub async fn next_events(&mut self) -> Result<Vec<EventEnvelope>, ClientError> {
        loop {
            match Self::next_frame(&mut self.ws).await? {
                ApplicationFrame::Events(events) if !events.is_empty() => return Ok(events),
                ApplicationFrame::Events(_) => continue,
                _ => return Err(ClientError::Protocol("expected events".into())),
            }
        }
    }
}
