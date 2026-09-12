//! Remote listener (R04, F2/F5/F6): WebSocket over TLS on a private
//! network address — only while paired devices exist and the listener is
//! enabled. Connections authenticate with a device token in the first
//! frame; commands then flow through the *same* handler as the local pipe
//! (F1), gated per-device on capabilities enforced daemon-side.

use crate::daemon::ApplicationHandler;
use crate::remote::capabilities::required_capability;
use crate::remote::registry::DeviceRegistry;
use crate::remote::tls::{server_config, Identity};
use futures_util::{SinkExt, StreamExt};
use r_code_harness_protocol::application::{
    ApplicationCommand, ApplicationFrame, ApplicationResult,
};
use r_code_harness_protocol::EventEnvelope;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::sync::{mpsc, watch};
use tokio_rustls::TlsAcceptor;

/// Hello frame (frozen contract, architecture §4). During pairing the
/// `token` carries the one-time pairing code and `device_id` is absent;
/// success answers with the minted device token in the welcome.
#[derive(serde::Deserialize)]
struct RemoteHello {
    hello: String,
    #[serde(default)]
    device_id: String,
    token: String,
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    platform: String,
}

/// A running listener; dropping the handle stops it.
pub struct ListenerHandle {
    shutdown: Arc<Notify>,
    pub local_addr: SocketAddr,
    /// The mDNS registration (daemon + full name + advertised facts) for
    /// teardown and same-process probing; cleared on stop.
    mdns: std::sync::Mutex<Option<MdnsRegistration>>,
    /// Live remote connections per device (R11): revocation and capability
    /// narrowing drop the sockets from under the sessions. The value type
    /// is a watch flag (not Notify) so a stop signal sent before the
    /// connection loop's first poll is still observed (no lost wakeup).
    connections: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<watch::Sender<bool>>>>>,
}

/// Registered connection teardown token.
struct ConnectionLease {
    device_id: String,
    /// Held for liveness semantics only (the flag rides the sender side);
    /// dropping the receiver would close the channel.
    #[allow(dead_code)]
    stop_rx: watch::Receiver<bool>,
    connections: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<watch::Sender<bool>>>>>,
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        if let Ok(mut map) = self.connections.lock() {
            if let Some(list) = map.get_mut(&self.device_id) {
                list.retain(|sender| !sender.is_closed());
                if list.is_empty() {
                    map.remove(&self.device_id);
                }
            }
        }
    }
}

struct MdnsRegistration {
    daemon: mdns_sd::ServiceDaemon,
    name: String,
    facts: (u16, String),
}

impl ListenerHandle {
    /// Stop listening, unregister mDNS, drop every accepted connection's
    /// listener side.
    pub fn stop(&self) {
        self.shutdown.notify_waiters();
        if let Ok(mut guard) = self.mdns.lock() {
            if let Some(registration) = guard.take() {
                let _ = registration.daemon.unregister(&registration.name);
            }
        }
    }

    /// Drop every live connection of one device (R11: revoke / capability
    /// narrowing take effect immediately on the wire).
    pub fn disconnect_device(&self, device_id: &str) {
        let stops = self
            .connections
            .lock()
            .map(|mut map| map.remove(device_id).unwrap_or_default())
            .unwrap_or_default();
        for stop in stops {
            let _ = stop.send(true);
        }
    }

    /// The registered mDNS advertisement's port and fingerprint TXT
    /// (R09.A2). `None` once unregistered. Same-process truth: multicast
    /// loopback is not dependable on every platform, so network-layer
    /// discovery assertions belong to the per-platform CI matrix (R14).
    pub fn mdns_registered_info(&self) -> Option<(u16, String)> {
        self.mdns
            .lock()
            .ok()?
            .as_ref()
            .map(|registration| registration.facts.clone())
    }
}

/// Advertise `_rcode._tcp.local` with the port and certificate
/// fingerprint TXT record (R09). Failures are soft: mDNS is discovery
/// sugar — pairing works without it (manual host entry).
fn advertise_mdns(
    ip: IpAddr,
    port: u16,
    fingerprint: &str,
) -> Option<(mdns_sd::ServiceDaemon, String, (u16, String))> {
    let daemon = mdns_sd::ServiceDaemon::new().ok()?;
    let mut properties = std::collections::HashMap::new();
    properties.insert("fp".to_string(), fingerprint.to_string());
    let info = mdns_sd::ServiceInfo::new(
        "_rcode._tcp.local.",
        "r-code-daemon",
        "r-code-daemon.local.",
        ip,
        port,
        properties,
    )
    .ok()?;
    let full_name = info.get_fullname().to_string();
    daemon.register(info).ok()?;
    Some((daemon, full_name, (port, fingerprint.to_string())))
}

/// Whether an address may ever be bound by the remote listener: loopback
/// (test stand-in), RFC1918, link-local or ULA only. `0.0.0.0` and public
/// addresses are rejected outright (F2: never the world-facing interface).
pub fn validate_bind_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private() // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local() // 169.254/16
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local
        }
    }
}

/// In-memory auth-failure counters per peer (rate visibility; reset on
/// restart by design).
#[derive(Default)]
struct AuthFailures {
    counts: Mutex<HashMap<IpAddr, u32>>,
}

/// Errors starting the listener.
#[derive(Debug, thiserror::Error)]
pub enum ListenerError {
    #[error("bind address {0} is not a private network address")]
    PublicBind(IpAddr),
    #[error("io failure: {0}")]
    Io(String),
}

/// Bind and serve. The loop exits (closing the port) when the shutdown
/// handle fires, the listener is disabled, or no live device remains —
/// F2's "no devices → no listener" invariant.
pub async fn listen(
    ip: IpAddr,
    port: u16,
    registry: Arc<DeviceRegistry>,
    identity: Identity,
    handler: Arc<dyn ApplicationHandler>,
    hub: Arc<crate::remote::fanout::FanoutHub>,
    app_dir: Option<std::path::PathBuf>,
) -> Result<ListenerHandle, ListenerError> {
    listen_pairing(ip, port, registry, identity, handler, hub, app_dir, None).await
}

/// [`listen`] plus an active pairing surface: a hello whose token is a
/// one-time pairing code (not a device token) pairs the device and receives
/// the minted token in the welcome (R02/R08). The listener starts even
/// with no paired device — the supervision loop still closes it once the
/// pairing window lapses and no device exists.
#[allow(clippy::too_many_arguments)]
pub async fn listen_pairing(
    ip: IpAddr,
    port: u16,
    registry: Arc<DeviceRegistry>,
    identity: Identity,
    handler: Arc<dyn ApplicationHandler>,
    hub: Arc<crate::remote::fanout::FanoutHub>,
    app_dir: Option<std::path::PathBuf>,
    pairing: Option<Arc<crate::remote::pairing::PairingSessions>>,
) -> Result<ListenerHandle, ListenerError> {
    if !validate_bind_address(ip) {
        return Err(ListenerError::PublicBind(ip));
    }
    if pairing.is_none() && (!registry.has_live_devices() || !registry.listener().enabled) {
        return Err(ListenerError::Io(
            "listener refused: no live device or listener disabled".into(),
        ));
    }
    if pairing.is_some() && !registry.listener().enabled {
        return Err(ListenerError::Io(
            "listener refused: listener disabled".into(),
        ));
    }
    let listener = TcpListener::bind(SocketAddr::new(ip, port))
        .await
        .map_err(|e| ListenerError::Io(e.to_string()))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| ListenerError::Io(e.to_string()))?;
    let shutdown = Arc::new(Notify::new());
    let mdns = advertise_mdns(ip, local_addr.port(), &identity.fingerprint).map(
        |(daemon, name, facts)| MdnsRegistration {
            daemon,
            name,
            facts,
        },
    );
    let connections = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        String,
        Vec<watch::Sender<bool>>,
    >::new()));
    let handle = ListenerHandle {
        shutdown: shutdown.clone(),
        local_addr,
        mdns: std::sync::Mutex::new(mdns),
        connections: connections.clone(),
    };
    let acceptor = TlsAcceptor::from(Arc::new(
        server_config(&identity).map_err(ListenerError::Io)?,
    ));
    let failures = Arc::new(AuthFailures::default());
    let supervision = tokio::spawn({
        let shutdown = shutdown.clone();
        let registry = registry.clone();
        let pairing = pairing.clone();
        async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
            ticker.tick().await; // consume the immediate first tick
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        // Alive while: a live device exists, or a pairing
                        // window is open (the pairing listener starts with
                        // zero devices by design). Once the window lapses
                        // with no device the port closes (F2).
                        let pairing_live = pairing
                            .as_ref()
                            .is_some_and(|sessions| sessions.is_active());
                        if (!registry.has_live_devices() && !pairing_live)
                            || !registry.listener().enabled
                        {
                            shutdown.notify_waiters();
                            break;
                        }
                    }
                    _ = shutdown.notified() => break,
                }
            }
        }
    });
    tokio::spawn(async move {
        let _supervision = supervision;
        loop {
            let accepted = tokio::select! {
                _ = shutdown.notified() => break,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, peer)) = accepted else {
                continue;
            };
            let peer_ip = peer.ip();
            let acceptor = acceptor.clone();
            let registry = registry.clone();
            let handler = handler.clone();
            let failures = failures.clone();
            let hub = hub.clone();
            let app_dir = app_dir.clone();
            let pairing = pairing.clone();
            let shutdown = shutdown.clone();
            let connections = connections.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return; // TLS handshake failed: drop quietly
                };
                serve_tls(
                    tls,
                    peer_ip,
                    &registry,
                    &handler,
                    &failures,
                    &hub,
                    app_dir,
                    pairing,
                    shutdown,
                    connections,
                )
                .await;
            });
        }
    });
    Ok(handle)
}

/// Route one TLS connection by its first HTTP request: `/app/*` serves the
/// built remote console (static; honest 404 when not built), everything
/// else upgrades to the remote WebSocket.
#[allow(clippy::too_many_arguments)]
async fn serve_tls(
    tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    peer: IpAddr,
    registry: &Arc<DeviceRegistry>,
    handler: &Arc<dyn ApplicationHandler>,
    failures: &Arc<AuthFailures>,
    hub: &Arc<crate::remote::fanout::FanoutHub>,
    app_dir: Option<std::path::PathBuf>,
    pairing: Option<Arc<crate::remote::pairing::PairingSessions>>,
    shutdown: Arc<Notify>,
    connections: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<watch::Sender<bool>>>>>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut tls_rd, mut tls_wr) = tokio::io::split(tls);
    // Read the HTTP request head (bounded; a non-HTTP peer gives up here).
    let mut head = Vec::with_capacity(2048);
    let mut byte = [0u8; 1];
    loop {
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tls_rd.read_exact(&mut byte),
        )
        .await;
        match read {
            Ok(Ok(_)) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") || head.len() > 8192 {
                    break;
                }
            }
            _ => return,
        }
    }
    let head_text = String::from_utf8_lossy(&head);
    let mut lines = head_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let header = |name: &str| -> Option<&str> {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };

    if target.starts_with("/app") {
        serve_static_app(&mut tls_wr, &target, app_dir.as_deref()).await;
        return;
    }

    // WebSocket upgrade (the remote protocol surface).
    let (Some(sec_websocket_key), Some(upgrade)) = (header("sec-websocket-key"), header("upgrade"))
    else {
        let _ = tls_wr
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await;
        return;
    };
    if !upgrade.eq_ignore_ascii_case("websocket") {
        let _ = tls_wr
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await;
        return;
    }
    let accept_key =
        tokio_tungstenite::tungstenite::handshake::derive_accept_key(sec_websocket_key.as_bytes());
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept_key}\r\n\r\n"
    );
    if tls_wr.write_all(response.as_bytes()).await.is_err() {
        return;
    }
    let tls = tls_rd.unsplit(tls_wr);
    let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
        tls,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    serve_remote_ws(
        &mut ws,
        peer,
        registry,
        handler,
        failures,
        hub,
        pairing,
        shutdown,
        connections,
    )
    .await;
}

async fn write_http<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    status: &str,
    content_type: &str,
    body: &[u8],
) {
    use tokio::io::AsyncWriteExt;
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = writer.write_all(header.as_bytes()).await;
    let _ = writer.write_all(body).await;
}

/// Static serving for the built remote console (`/app`). No directory is
/// configured → an explicit 404 naming the missing build (never a fake
/// page). Path traversal is refused by prefix check after normalization.
async fn serve_static_app<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    target: &str,
    app_dir: Option<&std::path::Path>,
) {
    let Some(root) = app_dir.filter(|dir| dir.is_dir()) else {
        write_http(
            writer,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"remote app not built: set R_CODE_REMOTE_APP_DIR or stage the build under <harness-v2>/remote-app\n",
        )
        .await;
        return;
    };
    // /app 或 /app/ → 壳页面；/app/<asset> → 构建产物。
    let relative = target
        .strip_prefix("/app")
        .unwrap_or("")
        .trim_start_matches('/');
    let relative = if relative.is_empty() {
        "remote.html"
    } else {
        relative
    };
    if relative.contains("..") {
        write_http(
            writer,
            "403 Forbidden",
            "text/plain",
            b"path traversal refused\n",
        )
        .await;
        return;
    }
    let path = root.join(relative);
    match tokio::fs::read(&path).await {
        Ok(body) => {
            let content_type = match path.extension().and_then(|ext| ext.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("js") => "text/javascript",
                Some("css") => "text/css",
                Some("json") | Some("webmanifest") => "application/json",
                Some("png") => "image/png",
                Some("svg") => "image/svg+xml",
                Some("webp") => "image/webp",
                _ => "application/octet-stream",
            };
            write_http(writer, "200 OK", content_type, &body).await;
        }
        Err(_) => {
            write_http(
                writer,
                "404 Not Found",
                "text/plain",
                b"not found
",
            )
            .await;
        }
    }
}

/// Serve one authenticated remote WebSocket: hello → capability-gated
/// command/event frames until EOF or refusal.
#[allow(clippy::too_many_arguments)]
async fn serve_remote_ws(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    >,
    peer: IpAddr,
    registry: &Arc<DeviceRegistry>,
    handler: &Arc<dyn ApplicationHandler>,
    failures: &Arc<AuthFailures>,
    hub: &Arc<crate::remote::fanout::FanoutHub>,
    pairing: Option<Arc<crate::remote::pairing::PairingSessions>>,
    shutdown: Arc<Notify>,
    connections: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<watch::Sender<bool>>>>>,
) {
    use tokio_tungstenite::tungstenite::Message;
    // Frame 1: the device hello.
    let Some(Ok(Message::Text(text))) = ws.next().await else {
        return;
    };
    let hello: RemoteHello = match serde_json::from_str::<RemoteHello>(&text) {
        Ok(hello) if hello.hello == "r-code-remote/1" => hello,
        _ => {
            let _ = ws
                .send(Message::Text(
                    r#"{"ok":false,"error":{"code":"unauthorized"}}"#.into(),
                ))
                .await;
            let _ = ws.close(None).await;
            return;
        }
    };
    // Device token first; a miss falls back to the pairing surface when
    // one is attached (the token then carries the one-time pairing code).
    enum Auth {
        Device(crate::remote::registry::DeviceRecord),
        Paired {
            record: crate::remote::registry::DeviceRecord,
            token: String,
            fingerprint: String,
        },
    }
    let auth = match registry.get_by_token(&hello.token) {
        Some(record) if hello.device_id.is_empty() || record.id == hello.device_id => {
            Auth::Device(record)
        }
        _ => {
            // A known-but-revoked device (by claimed id or token hash) is
            // distinguishable from an unknown one — both are refused, but
            // the code tells the client whether re-pairing can help.
            let claimed_revoked = registry.list().iter().any(|device| {
                device.revoked
                    && (device.id == hello.device_id
                        || device.token_sha256
                            == crate::remote::pairing::token_sha256_hex(&hello.token))
            });
            if claimed_revoked {
                *failures
                    .counts
                    .lock()
                    .expect("auth failures")
                    .entry(peer)
                    .or_insert(0) += 1;
                refuse(ws, "revoked").await;
                return;
            }
            if let Some(pairing) = pairing.as_ref() {
                let name = if hello.device_name.is_empty() {
                    "remote device".to_string()
                } else {
                    hello.device_name.clone()
                };
                let platform = if hello.platform.is_empty() {
                    "pwa".to_string()
                } else {
                    hello.platform.clone()
                };
                match pairing.pair(
                    registry,
                    crate::remote::pairing::DevicePairRequest {
                        pair_secret: hello.token.clone(),
                        device_name: name,
                        platform,
                    },
                ) {
                    Ok(reply) => match registry.get_by_token(&reply.token) {
                        Some(record) => Auth::Paired {
                            record,
                            token: reply.token,
                            fingerprint: reply.server_fingerprint,
                        },
                        None => {
                            refuse(ws, "unauthorized").await;
                            return;
                        }
                    },
                    Err(_) => {
                        *failures
                            .counts
                            .lock()
                            .expect("auth failures")
                            .entry(peer)
                            .or_insert(0) += 1;
                        refuse(ws, "unauthorized").await;
                        return;
                    }
                }
            } else {
                let revoked = registry
                    .list()
                    .iter()
                    .any(|device| device.id == hello.device_id && device.revoked);
                let code = if revoked { "revoked" } else { "unauthorized" };
                *failures
                    .counts
                    .lock()
                    .expect("auth failures")
                    .entry(peer)
                    .or_insert(0) += 1;
                refuse(ws, code).await;
                return;
            }
        }
    };
    let (device_id, capabilities, paired_token) = match &auth {
        Auth::Device(record) => (record.id.clone(), record.capabilities.clone(), None),
        Auth::Paired { record, token, .. } => (
            record.id.clone(),
            record.capabilities.clone(),
            Some((token.clone(), "1".to_string())),
        ),
    };
    let paired_fingerprint = match &auth {
        Auth::Paired { fingerprint, .. } => Some(fingerprint.clone()),
        Auth::Device(_) => None,
    };
    // Welcome: the standard ack, extended with the one-time minted token
    // when this connection just paired (delivered only inside the pinned
    // TLS channel; never persisted or logged).
    let mut welcome = serde_json::json!({
        "ok": true,
        "capabilities": capabilities.labels(),
        "profile": "development",
        "server": {"version": env!("CARGO_PKG_VERSION")},
    });
    if let (Some((token, _)), Some(fingerprint)) = (paired_token, paired_fingerprint) {
        welcome["paired"] = serde_json::json!(true);
        welcome["deviceId"] = serde_json::json!(device_id);
        welcome["token"] = serde_json::json!(token);
        welcome["serverFingerprint"] = serde_json::json!(fingerprint);
    }
    let _ = ws.send(Message::Text(welcome.to_string().into())).await;
    registry.note_seen(&device_id);

    // Connection lease: revoke/capability-narrow drops this socket
    // immediately; listener shutdown drops every connection.
    let (stop_tx, mut connection_stop) = watch::channel(false);
    let lease = ConnectionLease {
        device_id: device_id.clone(),
        stop_rx: connection_stop.clone(),
        connections: connections.clone(),
    };
    if let Ok(mut map) = connections.lock() {
        map.entry(device_id.clone()).or_default().push(stop_tx);
    }

    // Command/event loop: same frames as the local pipe, gated per
    // method, multiplexed with the live event subscription and a
    // heartbeat ping (30s; a dead peer fails the next write).
    let mut subscription: Option<(u64, mpsc::Receiver<EventEnvelope>)> = None;
    let mut last_sent_seq: u64 = 0;
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await; // consume the immediate first tick
    loop {
        let next_event = async {
            match subscription.as_mut() {
                Some((_, rx)) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            message = ws.next() => {
                let Some(Ok(message)) = message else { break };
                let Message::Text(text) = message else { continue };
                let Ok(frame) = serde_json::from_str::<ApplicationFrame>(&text) else {
                    let _ = ws
                        .send(Message::Text(
                            serde_json::json!({"kind": "error", "message": "unparseable frame"}).to_string().into(),
                        ))
                        .await;
                    continue;
                };
                match frame {
                    ApplicationFrame::Command(mut command) => {
                        // The connection identity is authoritative: client_id
                        // is the authenticated device, never the wire value.
                        command.client_id = device_id.clone();
                        let command_id = command.command_id.clone();
                        let outcome = gate_and_execute(&capabilities, command, handler).await;
                        let result = ApplicationResult {
                            client_id: device_id.clone(),
                            command_id,
                            outcome,
                        };
                        let payload = serde_json::to_string(&ApplicationFrame::Result(result))
                            .unwrap_or_default();
                        if ws.send(Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    ApplicationFrame::ReadEvents(request) => {
                        let events = handler
                            .events_after(request.after_seq, request.limit)
                            .await;
                        let payload = serde_json::to_string(&ApplicationFrame::Events(events))
                            .unwrap_or_default();
                        if ws.send(Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    ApplicationFrame::EventsSubscribe(request) => {
                        // Subscribe first, replay history second, then rely
                        // on seq-monotonic de-duplication at the receiver:
                        // no loss, no duplication across the seam (F8).
                        if let Some((id, _)) = subscription.take() {
                            hub.unsubscribe(id);
                        }
                        last_sent_seq = request.after_seq;
                        let (id, rx) = hub.subscribe();
                        subscription = Some((id, rx));
                        let history = handler.events_after(request.after_seq, 500).await;
                        if let Some(last) = history.last() {
                            last_sent_seq = last.seq;
                        }
                        let payload =
                            serde_json::to_string(&ApplicationFrame::Events(history))
                                .unwrap_or_default();
                        if ws.send(Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = ws
                            .send(Message::Text(
                                serde_json::json!({"kind": "error", "message": "unexpected frame"}).to_string().into(),
                            ))
                            .await;
                    }
                }
            }
            event = next_event => {
                let Some(event) = event else { break };
                if event.seq <= last_sent_seq {
                    continue; // already replayed in the subscribe history
                }
                last_sent_seq = event.seq;
                let payload = serde_json::to_string(&ApplicationFrame::Events(vec![event]))
                    .unwrap_or_default();
                if ws.send(Message::Text(payload.into())).await.is_err() {
                    break;
                }
            }
            _ = heartbeat.tick() => {
                if ws.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            _ = connection_stop.changed() => {
                // Device revoked or capabilities narrowed (R11): drop now.
                // watch keeps the flag, so a signal sent before this poll
                // is still observed here.
                let _ = ws.close(None).await;
                break;
            }
            _ = shutdown.notified() => {
                // Listener stopped (setListener(false) / last revoke): all
                // connections drop; device records persist.
                let _ = ws.close(None).await;
                break;
            }
        }
    }
    if let Some((id, _)) = subscription {
        hub.unsubscribe(id);
    }
    drop(lease);
}

/// Capability gate (F5/F6): forbidden methods refused outright, permitted
/// methods require the mapped capability, everything else fail-closed for
/// remotes; then the same handler the local pipe uses (F1).
async fn gate_and_execute(
    capabilities: &crate::remote::capabilities::CapabilitySet,
    mut command: ApplicationCommand,
    handler: &Arc<dyn ApplicationHandler>,
) -> Result<serde_json::Value, String> {
    // Remote approval decisions carry an internal source marker so the
    // daemon applies the desktop-confirm rule (R12); the suffix is injected
    // here and unreachable from a raw wire frame (unknown method → forbidden).
    if command.method == "approvals.decide" {
        command.method = "approvals.decide$remote".into();
    }
    match required_capability(&command.method) {
        None => Err(format!(
            "forbidden_remote_method: {} is not callable from a remote transport",
            command.method
        )),
        Some(required) if !capabilities.has(required) => Err(format!(
            "permission_denied: {} requires {}",
            command.method,
            required.as_str()
        )),
        Some(_) => handler.execute(command).await,
    }
}

/// Refuse a hello with a structured code and close.
async fn refuse(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    >,
    code: &str,
) {
    use tokio_tungstenite::tungstenite::Message;
    let _ = ws
        .send(Message::Text(
            serde_json::json!({"ok": false, "error": {"code": code}})
                .to_string()
                .into(),
        ))
        .await;
    let _ = ws.close(None).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_and_wildcard_binds_are_refused() {
        assert!(!validate_bind_address("0.0.0.0".parse().unwrap()));
        assert!(!validate_bind_address("8.8.8.8".parse().unwrap()));
        assert!(validate_bind_address("127.0.0.1".parse().unwrap()));
        assert!(validate_bind_address("192.168.1.20".parse().unwrap()));
        assert!(validate_bind_address("10.0.0.5".parse().unwrap()));
        assert!(validate_bind_address("169.254.3.4".parse().unwrap()));
        assert!(validate_bind_address("::1".parse().unwrap()));
        assert!(validate_bind_address("fd00::1".parse().unwrap()));
        assert!(!validate_bind_address("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn gate_semantics() {
        use crate::remote::capabilities::CapabilitySet;
        let read_only = CapabilitySet::read_only();
        let full = CapabilitySet::read_only()
            .with_tasks_write()
            .with_approvals_decide();
        // Capability refusal carries the required capability.
        assert_eq!(
            gate_string(&read_only, "task.sendMessage"),
            "permission_denied: task.sendMessage requires tasks-write"
        );
        // Forbidden regardless of capabilities.
        assert_eq!(
            gate_string(&full, "settings.apply"),
            "forbidden_remote_method: settings.apply is not callable from a remote transport"
        );
        assert_eq!(
            gate_string(&full, "service.shutdown"),
            "forbidden_remote_method: service.shutdown is not callable from a remote transport"
        );
        // Unknown methods fail closed for remotes.
        assert!(gate_string(&full, "made.up.method").starts_with("forbidden_remote_method"));
    }

    fn gate_string(
        capabilities: &crate::remote::capabilities::CapabilitySet,
        method: &str,
    ) -> String {
        match required_capability(method) {
            None => {
                format!("forbidden_remote_method: {method} is not callable from a remote transport")
            }
            Some(required) if !capabilities.has(required) => {
                format!("permission_denied: {method} requires {}", required.as_str())
            }
            Some(_) => "allowed".to_string(),
        }
    }
}
