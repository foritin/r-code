//! R07.A2 — daemon `/app` hosting: the built remote console is served from
//! a configured directory; without one the daemon answers an honest 404
//! naming the missing build (never a fake page).

use r_code_harness_protocol::application::ApplicationCommand;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::fanout::FanoutHub;
use r_code_runtime::remote::listener::listen;
use r_code_runtime::remote::registry::{DeviceRegistry, ListenerConfig};
use r_code_runtime::remote::tls::{ensure_identity, pinned_client_config};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct NullHandler;

#[async_trait::async_trait]
impl ApplicationHandler for NullHandler {
    async fn execute(&self, _command: ApplicationCommand) -> Result<serde_json::Value, String> {
        Err("unused".into())
    }
    async fn events_after(
        &self,
        _after_seq: u64,
        _limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        vec![]
    }
}

struct Fixture {
    identity: r_code_runtime::remote::tls::Identity,
    addr: std::net::SocketAddr,
}

impl Fixture {
    async fn new(tag: &str, app_dir: Option<std::path::PathBuf>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(tag);
        std::fs::create_dir_all(&root).expect("mkdir");
        let registry = Arc::new(DeviceRegistry::open(&root).expect("registry"));
        registry
            .set_listener(ListenerConfig {
                enabled: true,
                bind: vec![],
                port: None,
            })
            .expect("enable");
        let identity = ensure_identity(&root).expect("identity");
        registry
            .register(
                "Phone",
                "ios-pwa",
                &identity.fingerprint,
                Default::default(),
            )
            .expect("pair");
        let handle = listen(
            "127.0.0.1".parse().unwrap(),
            0,
            registry.clone(),
            identity.duplicate(),
            Arc::new(NullHandler),
            FanoutHub::new(),
            app_dir,
        )
        .await
        .expect("listen");
        let addr = handle.local_addr;
        std::mem::forget(handle);
        std::mem::forget(dir);
        Self { identity, addr }
    }
}

async fn https_get(fixture: &Fixture, path: &str) -> (String, Vec<u8>) {
    let tcp = TcpStream::connect(fixture.addr).await.expect("tcp");
    let connector = tokio_rustls::TlsConnector::from(Arc::new(
        pinned_client_config(&fixture.identity.fingerprint).expect("cfg"),
    ));
    let name = rustls::pki_types::ServerName::try_from("r-code-daemon".to_string()).unwrap();
    let mut tls = connector.connect(name, tcp).await.expect("tls");
    tls.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: r-code-daemon\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .expect("write");
    let mut response = Vec::new();
    let mut chunk = [0u8; 4096];
    // The server closes without close_notify; treat EOF as end of body.
    loop {
        match tls.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => response.extend_from_slice(&chunk[..n]),
        }
    }
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header terminator");
    let head = String::from_utf8_lossy(&response[..split]).to_string();
    (head, response[split + 4..].to_vec())
}

#[tokio::test]
async fn r07_a2_app_served_when_built_and_honest_404_when_not() {
    // Not configured: an honest 404 naming the missing build.
    let bare = Fixture::new("bare", None).await;
    let (head, body) = https_get(&bare, "/app").await;
    assert!(head.starts_with("HTTP/1.1 404"), "honest 404, got: {head}");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("remote app not built"),
        "names the fix: {text}"
    );

    // A staged build is served with proper content types.
    let dir = tempfile::tempdir().expect("tempdir");
    let app = dir.path().join("remote-app");
    std::fs::create_dir_all(&app).expect("mkdir");
    std::fs::write(app.join("remote.html"), b"<html>r-code remote</html>").expect("html");
    std::fs::write(app.join("app.js"), b"console.log('r')").expect("js");
    let built = Fixture::new("built", Some(app)).await;
    let (head, body) = https_get(&built, "/app").await;
    assert!(head.starts_with("HTTP/1.1 200"), "shell served: {head}");
    assert!(head.contains("text/html"), "content type: {head}");
    assert!(body.starts_with(b"<html>r-code remote</html>"));

    let (head, body) = https_get(&built, "/app/app.js").await;
    assert!(head.starts_with("HTTP/1.1 200"));
    assert!(head.contains("text/javascript"));
    assert_eq!(body, b"console.log('r')");

    // Path traversal is refused.
    let (head, _) = https_get(&built, "/app/../secret").await;
    assert!(
        head.starts_with("HTTP/1.1 403"),
        "traversal refused: {head}"
    );
}
