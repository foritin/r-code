//! End-to-end encryption over the relay (R17, F10/F12): Noise XX with a
//! PSK (the one-time pairing secret) on top of the relay's opaque binary
//! frames. The relay only ever sees ciphertext and handshake bytes; a
//! malicious relay can drop or corrupt (both surface as connection death),
//! never read or forge.
//!
//! Roles (relay-interface.md §3): device = initiator, owner (daemon) =
//! responder. Both sides expose the result as a plain `AppStream` byte
//! stream so the existing command pipeline (F1) is unchanged.

use ed25519_dalek::SigningKey;
use futures_util::{Sink, SinkExt, StreamExt};
use snow::{HandshakeState, TransportState};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Noise pattern: XX with PSK mixed into the third message (matches
/// relay-interface.md §3: `→ e / ← e,ee,s,es / → s,se`).
pub const NOISE_PATTERN: &str = "Noise_XXpsk3_25519_ChaChaPoly_SHA256";

/// Max plaintext per wire message (Noise message size ceiling is 65535).
const MAX_PLAINTEXT_CHUNK: usize = 48 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum E2eeError {
    #[error("handshake failed (wrong PSK or tampering): {0}")]
    Handshake(String),
    #[error("transport failure: {0}")]
    Transport(String),
    #[error("peer closed the connection")]
    Closed,
}

/// Drive the three handshake messages over the relay's binary pipe. The
/// relay forwards these opaquely. Returns the transport state.
async fn handshake<S>(
    ws: &mut WebSocketStream<S>,
    initiator: bool,
    static_key: &[u8; 32],
    psk: &[u8; 32],
) -> Result<TransportState, E2eeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    WebSocketStream<S>: Sink<Message> + Unpin,
    <WebSocketStream<S> as Sink<Message>>::Error: std::fmt::Display,
{
    // Snow keys are copied into the handshake state; the pattern/psk live
    // for the builder expression only.
    let mut hs: HandshakeState = if initiator {
        snow::Builder::new(NOISE_PATTERN.parse().expect("pattern"))
            .local_private_key(static_key)
            .psk(3, psk)
            .build_initiator()
            .map_err(|e| E2eeError::Handshake(e.to_string()))?
    } else {
        snow::Builder::new(NOISE_PATTERN.parse().expect("pattern"))
            .local_private_key(static_key)
            .psk(3, psk)
            .build_responder()
            .map_err(|e| E2eeError::Handshake(e.to_string()))?
    };
    let mut buffer = vec![0u8; 65535];
    if initiator {
        // → e
        let len = hs
            .write_message(&[], &mut buffer)
            .map_err(|e| E2eeError::Handshake(e.to_string()))?;
        ws.send(Message::Binary(buffer[..len].to_vec().into()))
            .await
            .map_err(|e| E2eeError::Transport(e.to_string()))?;
        // ← e, ee, s, es
        let message = next_binary(ws).await?;
        let len = hs
            .read_message(&message, &mut buffer)
            .map_err(|_| E2eeError::Handshake("message 2 rejected (wrong PSK/tampered?)".into()))?;
        let _ = len;
        // → s, se (PSK mixed here)
        let len = hs
            .write_message(&[], &mut buffer)
            .map_err(|e| E2eeError::Handshake(e.to_string()))?;
        ws.send(Message::Binary(buffer[..len].to_vec().into()))
            .await
            .map_err(|e| E2eeError::Transport(e.to_string()))?;
    } else {
        // ← e (from device)
        let message = next_binary(ws).await?;
        let len = hs
            .read_message(&message, &mut buffer)
            .map_err(|e| E2eeError::Handshake(e.to_string()))?;
        let _ = len;
        // → e, ee, s, es
        let len = hs
            .write_message(&[], &mut buffer)
            .map_err(|e| E2eeError::Handshake(e.to_string()))?;
        ws.send(Message::Binary(buffer[..len].to_vec().into()))
            .await
            .map_err(|e| E2eeError::Transport(e.to_string()))?;
        // ← s, se (PSK mixed here)
        let message = next_binary(ws).await?;
        hs.read_message(&message, &mut buffer)
            .map_err(|_| E2eeError::Handshake("message 3 rejected (wrong PSK/tampered?)".into()))?;
    }
    hs.into_transport_mode()
        .map_err(|e| E2eeError::Handshake(e.to_string()))
}

async fn next_control<S>(ws: &mut WebSocketStream<S>) -> Result<serde_json::Value, E2eeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    WebSocketStream<S>: Sink<Message> + Unpin,
{
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text)
                    .map_err(|e| E2eeError::Handshake(format!("bad control frame: {e}")));
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_))) | None => return Err(E2eeError::Closed),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(E2eeError::Transport(e.to_string())),
        }
    }
}

use crate::sign_message;

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn hex_nonce() -> String {
    let key = SigningKey::generate(&mut rand_core::OsRng);
    sha256_hex(&key.to_bytes())[..32].to_string()
}

/// Control-frame types the relay may legitimately still have queued when
/// the owner adopts its socket (relay-interface.md: owner.bind.ack).
const BENIGN_CONTROL_TYPES: [&str; 1] = ["owner.bind.ack"];

/// Only whitelisted, contentless relay acks may be skipped mid-handshake.
fn is_benign_control(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    value
        .get("type")
        .and_then(|kind| kind.as_str())
        .is_some_and(|kind| BENIGN_CONTROL_TYPES.contains(&kind))
}

async fn next_binary<S>(ws: &mut WebSocketStream<S>) -> Result<Vec<u8>, E2eeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    WebSocketStream<S>: Sink<Message> + Unpin,
{
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(payload))) => return Ok(payload.to_vec()),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_))) | None => return Err(E2eeError::Closed),
            Some(Ok(Message::Text(text))) => {
                // Benign control frames may still be queued when the owner
                // adopts its socket right after owner.bind (owner.bind.ack).
                // Skip exactly those; every other text frame during a Noise
                // exchange is unexpected (relay errors like owner_offline, or
                // an injecting relay) and must fail the handshake instead of
                // being silently absorbed.
                if is_benign_control(&text) {
                    continue;
                }
                return Err(E2eeError::Handshake(format!("control frame: {text}")));
            }
            Some(Err(e)) => return Err(E2eeError::Transport(e.to_string())),
            Some(Ok(_)) => continue,
        }
    }
}

/// One end of an E2EE relay session as a byte stream. Frame boundaries are
/// internal (each wire message carries a chunk of plaintext); reads coalesce
/// and writes chunk-split, so the AppStream consumer sees a normal pipe.
pub struct E2eeStream<S> {
    ws: WebSocketStream<S>,
    transport: snow::TransportState,
    /// Decrypted plaintext not yet consumed by the reader.
    read_buffer: Vec<u8>,
    read_offset: usize,
    initiator: bool,
    /// Set when the peer or relay died; subsequent IO fails.
    dead: bool,
}

impl E2eeStream<MaybeTlsStream<TcpStream>> {
    /// Device side (initiator): the full relay admission flow — hello →
    /// challenge/answer (device key proof) → welcome — then the Noise
    /// handshake with the owner's static fingerprint pinned (TOFU, F3).
    /// A mismatch kills the connection before any application byte.
    pub async fn connect_device(
        relay_addr: &str,
        owner_id: &str,
        device_signing: &SigningKey,
        psk: &[u8; 32],
        owner_fingerprint_hex: &str,
    ) -> Result<E2eeStream<MaybeTlsStream<TcpStream>>, E2eeError> {
        let tcp = TcpStream::connect(relay_addr)
            .await
            .map_err(|e| E2eeError::Transport(e.to_string()))?;
        let (mut ws, _) = tokio_tungstenite::client_async(
            format!("ws://{relay_addr}/relay"),
            MaybeTlsStream::Plain(tcp),
        )
        .await
        .map_err(|e| E2eeError::Transport(e.to_string()))?;
        // Control flow: device.hello → challenge → answer → welcome.
        let device_pubkey_b64 = b64(device_signing.verifying_key().as_bytes());
        ws.send(Message::Text(
            serde_json::json!({
                "type": "device.hello",
                "ownerId": owner_id,
                "devicePubkey": device_pubkey_b64,
                "nonce": hex_nonce(),
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|e| E2eeError::Transport(e.to_string()))?;
        let challenge = next_control(&mut ws).await?;
        if challenge.get("type").and_then(|v| v.as_str()) != Some("device.challenge") {
            return Err(E2eeError::Handshake(format!(
                "admission refused: {}",
                challenge["error"]["code"].as_str().unwrap_or("unknown")
            )));
        }
        let challenge_value = challenge
            .get("challenge")
            .and_then(|v| v.as_str())
            .ok_or_else(|| E2eeError::Handshake("missing challenge".into()))?
            .to_string();
        ws.send(Message::Text(
            serde_json::json!({
                "type": "device.answer",
                "sig": sign_message("rcode-device-challenge", &challenge_value, device_signing),
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|e| E2eeError::Transport(e.to_string()))?;
        let welcome = next_control(&mut ws).await?;
        if welcome.get("type").and_then(|v| v.as_str()) != Some("device.welcome") {
            return Err(E2eeError::Handshake(format!(
                "admission refused: {}",
                welcome["error"]["code"].as_str().unwrap_or("unknown")
            )));
        }
        // Noise over the bridged binary plane. The initiator's static key
        // is ephemeral per session (device identity rides the control flow).
        let ephemeral = SigningKey::generate(&mut rand_core::OsRng);
        let stream = Self::finish_handshake(ws, ephemeral.to_bytes(), psk, true).await?;
        // Pin the owner's static key: hash what the handshake revealed.
        let remote = stream
            .transport
            .get_remote_static()
            .map(sha256_hex)
            .unwrap_or_default();
        if remote != owner_fingerprint_hex {
            return Err(E2eeError::Handshake(
                "owner fingerprint mismatch (TOFU)".into(),
            ));
        }
        Ok(stream)
    }
}

impl<S> E2eeStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Device side (initiator) over an *existing* WebSocket (R19 tests:
    /// duplex-piped sessions). Same handshake + pinning as
    /// [`Self::connect_device`], without the dial.
    pub async fn connect_device_stream(
        ws: WebSocketStream<S>,
        psk: &[u8; 32],
        owner_fingerprint_hex: &str,
    ) -> Result<E2eeStream<S>, E2eeError> {
        let ephemeral = SigningKey::generate(&mut rand_core::OsRng);
        let stream = Self::finish_handshake(ws, ephemeral.to_bytes(), psk, true).await?;
        let remote = stream
            .transport
            .get_remote_static()
            .map(sha256_hex)
            .unwrap_or_default();
        if remote != owner_fingerprint_hex {
            return Err(E2eeError::Handshake(
                "owner fingerprint mismatch (TOFU)".into(),
            ));
        }
        Ok(stream)
    }

    /// Owner side (responder): adopted by the relay transport after the
    /// owner control frames complete; the socket is already authenticated.
    pub async fn accept_owner(
        ws: WebSocketStream<S>,
        static_key: &[u8; 32],
        psk: &[u8; 32],
    ) -> Result<E2eeStream<S>, E2eeError> {
        // No pre-read here: a queued owner.bind.ack is skipped *inside* the
        // handshake's frame loop (see `next_binary`). Pre-reading would
        // consume whichever frame arrives first — including the Noise msg1
        // the device may already have sent — and swallowing that message
        // deadlocks both ends (every caller that dials immediately).
        Self::finish_handshake(ws, *static_key, psk, false).await
    }

    async fn finish_handshake(
        ws: WebSocketStream<S>,
        static_key: [u8; 32],
        psk: &[u8; 32],
        initiator: bool,
    ) -> Result<E2eeStream<S>, E2eeError> {
        let mut ws = ws;
        let transport = handshake(&mut ws, initiator, &static_key, psk).await?;
        Ok(Self {
            ws,
            transport,
            read_buffer: Vec::new(),
            read_offset: 0,
            initiator,
            dead: false,
        })
    }

    #[allow(dead_code)] // role introspection used by tests/diagnostics
    pub fn is_initiator(&self) -> bool {
        self.initiator
    }
}

fn sha256_hex(value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(value);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

impl<S> AsyncRead for E2eeStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.read_offset < self.read_buffer.len() {
            let take = (self.read_buffer.len() - self.read_offset).min(buf.remaining());
            buf.put_slice(&self.read_buffer[self.read_offset..self.read_offset + take]);
            self.read_offset += take;
            if self.read_offset == self.read_buffer.len() {
                self.read_buffer.clear();
                self.read_offset = 0;
            }
            return Poll::Ready(Ok(()));
        }
        // Need more ciphertext from the wire.
        match self.poll_decrypt(cx) {
            Poll::Ready(Ok(())) => {
                if self.read_offset < self.read_buffer.len() {
                    let take = (self.read_buffer.len() - self.read_offset).min(buf.remaining());
                    buf.put_slice(&self.read_buffer[self.read_offset..self.read_offset + take]);
                    self.read_offset += take;
                    if self.read_offset == self.read_buffer.len() {
                        self.read_buffer.clear();
                        self.read_offset = 0;
                    }
                    Poll::Ready(Ok(()))
                } else if self.dead {
                    Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "e2ee peer closed",
                    )))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(Err(E2eeError::Closed)) => Poll::Ready(Ok(())), // clean EOF
            Poll::Ready(Err(e)) => {
                self.dead = true;
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    e.to_string(),
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> E2eeStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_decrypt(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), E2eeError>> {
        // Drive the WS future to completion of one more message. Because
        // `next_binary` is async (not poll-based), we bridge with a small
        // state machine: delegate to a boxed future stored per poll cycle.
        use std::future::Future;
        let ws = &mut self.ws;
        let mut fut = Box::pin(next_binary(ws));
        match fut.as_mut().poll(cx) {
            Poll::Ready(Ok(message)) => {
                let mut buffer = vec![0u8; message.len() + 64];
                match self.transport.read_message(&message, &mut buffer) {
                    Ok(len) => {
                        self.read_buffer.extend_from_slice(&buffer[..len]);
                        Poll::Ready(Ok(()))
                    }
                    Err(_) => {
                        self.dead = true;
                        Poll::Ready(Err(E2eeError::Handshake("ciphertext rejected".into())))
                    }
                }
            }
            Poll::Ready(Err(E2eeError::Closed)) => {
                self.dead = true;
                Poll::Ready(Ok(())) // clean EOF: next read drains then reports
            }
            Poll::Ready(Err(e)) => {
                self.dead = true;
                Poll::Ready(Err(e))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> AsyncWrite for E2eeStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
    WebSocketStream<S>: Sink<Message> + Unpin,
    <WebSocketStream<S> as Sink<Message>>::Error: std::fmt::Display,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.dead {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "e2ee stream dead",
            )));
        }
        // Encrypt one wire message synchronously and queue it on the
        // socket's Sink (`start_send` is a synchronous enqueue; the actual
        // socket write happens in poll_flush). Oversized writes take the
        // first chunk and report that length — callers loop via write_all.
        let take = buf.len().min(MAX_PLAINTEXT_CHUNK);
        let mut buffer = vec![0u8; take + 64];
        let written = self
            .transport
            .write_message(&buf[..take], &mut buffer)
            .map_err(|e| {
                self.dead = true;
                std::io::Error::new(std::io::ErrorKind::ConnectionAborted, e.to_string())
            })?;
        use futures_util::Sink;
        match Pin::new(&mut self.ws).start_send(Message::Binary(buffer[..written].to_vec().into()))
        {
            Ok(()) => Poll::Ready(Ok(take)),
            Err(e) => {
                self.dead = true;
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    e.to_string(),
                )))
            }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        use futures_util::Sink;
        match Pin::new(&mut self.ws).poll_flush(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => {
                self.dead = true;
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    e.to_string(),
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        use futures_util::Sink;
        let this = self.get_mut();
        match Pin::new(&mut this.ws).poll_close(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => {
                this.dead = true;
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    e.to_string(),
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
