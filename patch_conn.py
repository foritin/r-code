import io

p = 'crates/r-code-runtime/src/remote/listener.rs'
s = io.open(p, encoding='utf8').read()

# 1) ListenerHandle carries a live-connection index for forced disconnects
old = '''/// A running listener; dropping the handle stops it.
pub struct ListenerHandle {
    shutdown: Arc<Notify>,
    pub local_addr: SocketAddr,
    /// The mDNS registration (daemon + full name + advertised facts) for
    /// teardown and same-process probing; cleared on stop.
    mdns: std::sync::Mutex<Option<MdnsRegistration>>,
}'''
new = '''/// A running listener; dropping the handle stops it.
pub struct ListenerHandle {
    shutdown: Arc<Notify>,
    pub local_addr: SocketAddr,
    /// The mDNS registration (daemon + full name + advertised facts) for
    /// teardown and same-process probing; cleared on stop.
    mdns: std::sync::Mutex<Option<MdnsRegistration>>,
    /// Live remote connections per device (R11): revocation and capability
    /// narrowing drop the sockets from under the sessions.
    connections: std::sync::Mutex<std::collections::HashMap<String, Vec<Arc<Notify>>>>,
}

/// Registered connection teardown token.
struct ConnectionLease {
    device_id: String,
    stop: Arc<Notify>,
    connections: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<Arc<Notify>>>>>,
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        if let Ok(mut map) = self.connections.lock() {
            if let Some(list) = map.get_mut(&self.device_id) {
                list.retain(|notify| !Arc::ptr_eq(notify, &self.stop));
                if list.is_empty() {
                    map.remove(&self.device_id);
                }
            }
        }
    }
}'''
assert old in s
s = s.replace(old, new, 1)

# 2) handle constructor + disconnect API
old = '''    let mdns = advertise_mdns(ip, local_addr.port(), &identity.fingerprint)
        .map(|(daemon, name, facts)| MdnsRegistration { daemon, name, facts });
    let handle = ListenerHandle {
        shutdown: shutdown.clone(),
        local_addr,
        mdns: std::sync::Mutex::new(mdns),
    };'''
new = '''    let mdns = advertise_mdns(ip, local_addr.port(), &identity.fingerprint)
        .map(|(daemon, name, facts)| MdnsRegistration { daemon, name, facts });
    let connections = Arc::new(std::sync::Mutex::new(
        std::collections::HashMap::<String, Vec<Arc<Notify>>>::new(),
    ));
    let handle = ListenerHandle {
        shutdown: shutdown.clone(),
        local_addr,
        mdns: std::sync::Mutex::new(mdns),
        connections,
    };'''
assert old in s
s = s.replace(old, new, 1)

old = '''    /// The registered mDNS advertisement's port and fingerprint TXT'''
new = '''    /// Drop every live connection of one device (R11: revoke / capability
    /// narrowing take effect immediately on the wire).
    pub fn disconnect_device(&self, device_id: &str) {
        let stops = self
            .connections
            .lock()
            .map(|mut map| map.remove(device_id).unwrap_or_default())
            .unwrap_or_default();
        for stop in stops {
            stop.notify_waiters();
        }
    }

    /// The registered mDNS advertisement's port and fingerprint TXT'''
assert old in s
s = s.replace(old, new, 1)

# 3) serve_tls must receive the shutdown/connection plumbing — extend the
# spawn to pass Arc clones into serve_remote_ws
old = '''            let hub = hub.clone();
            let app_dir = app_dir.clone();
            let pairing = pairing.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return; // TLS handshake failed: drop quietly
                };
                serve_tls(tls, peer_ip, &registry, &handler, &failures, &hub, app_dir, pairing)
                    .await;
            });'''
new = '''            let hub = hub.clone();
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
            });'''
assert old in s
s = s.replace(old, new, 1)

old = '''#[allow(clippy::too_many_arguments)]
async fn serve_tls(
    tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    peer: IpAddr,
    registry: &Arc<DeviceRegistry>,
    handler: &Arc<dyn ApplicationHandler>,
    failures: &Arc<AuthFailures>,
    hub: &Arc<crate::remote::fanout::FanoutHub>,
    app_dir: Option<std::path::PathBuf>,
    pairing: Option<Arc<crate::remote::pairing::PairingSessions>>,
) {'''
new = '''#[allow(clippy::too_many_arguments)]
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
    connections: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<Arc<Notify>>>>>,
) {'''
assert old in s
s = s.replace(old, new, 1)

old = '    serve_remote_ws(&mut ws, peer, registry, handler, failures, hub, pairing).await;\n}'
new = '    serve_remote_ws(&mut ws, peer, registry, handler, failures, hub, pairing, shutdown, connections).await;\n}'
assert old in s
s = s.replace(old, new, 1)

old = '''    pairing: Option<Arc<crate::remote::pairing::PairingSessions>>,
) {
    use tokio_tungstenite::tungstenite::Message;
    // Frame 1: the device hello.'''
new = '''    pairing: Option<Arc<crate::remote::pairing::PairingSessions>>,
    shutdown: Arc<Notify>,
    connections: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<Arc<Notify>>>>>,
) {
    use tokio_tungstenite::tungstenite::Message;
    // Frame 1: the device hello.'''
assert old in s
s = s.replace(old, new, 1)

io.open(p, 'w', encoding='utf8', newline='').write(s)
print('plumbing added')
