//! R03 — TLS pinning end to end over a real loopback listener: a wrong
//! fingerprint fails *inside the TLS handshake* (never the application
//! layer), a plaintext client cannot get any application frame through,
//! and the pinned fingerprint connects cleanly.

use r_code_runtime::remote::tls::{ensure_identity, pinned_client_config, server_config};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsConnector;

async fn free_listener() -> std::io::Result<(TcpListener, std::net::SocketAddr)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    Ok((listener, address))
}

#[tokio::test]
async fn r03_a2_wrong_fingerprint_fails_in_the_tls_handshake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let identity = ensure_identity(dir.path()).expect("identity");
    let acceptor =
        tokio_rustls::TlsAcceptor::from(Arc::new(server_config(&identity).expect("server config")));
    let (listener, address) = free_listener().await.expect("bind");

    let server = tokio::spawn(async move {
        // One accept; the handshake result is the assertion target.
        let (stream, _) = listener.accept().await.expect("accept");
        let _handshake = acceptor.accept(stream).await;
    });

    // A *different* identity's fingerprint (as if the QR was for another
    // host or the server was replaced): pin mismatch.
    let other_dir = tempfile::tempdir().expect("tempdir");
    let other = ensure_identity(other_dir.path()).expect("other identity");
    assert_ne!(other.fingerprint, identity.fingerprint);

    let wrong_client = TlsConnector::from(Arc::new(
        pinned_client_config(&other.fingerprint).expect("pinned config"),
    ));
    let tcp = tokio::net::TcpStream::connect(address).await.expect("tcp");
    let handshake = wrong_client
        .connect(
            rustls::pki_types::ServerName::try_from("r-code-daemon".to_string()).expect("name"),
            tcp,
        )
        .await;
    assert!(
        handshake.is_err(),
        "wrong fingerprint must fail the TLS handshake itself, not the app layer"
    );
    server.await.expect("server settled");

    // Sanity on the same listener: the *correct* pin connects.
    let (listener, address) = free_listener().await.expect("bind");
    let acceptor =
        tokio_rustls::TlsAcceptor::from(Arc::new(server_config(&identity).expect("server config")));
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut tls = acceptor.accept(stream).await.expect("handshake ok");
        tls.write_all(b"hello\n").await.expect("write");
    });
    let right_client = TlsConnector::from(Arc::new(
        pinned_client_config(&identity.fingerprint).expect("pinned config"),
    ));
    let tcp = tokio::net::TcpStream::connect(address).await.expect("tcp");
    let mut tls = right_client
        .connect(
            rustls::pki_types::ServerName::try_from("r-code-daemon".to_string()).unwrap(),
            tcp,
        )
        .await
        .expect("pinned handshake succeeds");
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 64];
    loop {
        let n = tls.read(&mut chunk).await.expect("read");
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.ends_with(b"\n") || n == 0 {
            break;
        }
    }
    assert_eq!(buffer, b"hello\n");
    server.await.expect("server settled");
}

#[tokio::test]
async fn r03_a3_plaintext_client_cannot_get_frames_through() {
    let dir = tempfile::tempdir().expect("tempdir");
    let identity = ensure_identity(dir.path()).expect("identity");
    let acceptor =
        tokio_rustls::TlsAcceptor::from(Arc::new(server_config(&identity).expect("server config")));
    let (listener, address) = free_listener().await.expect("bind");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        // A TLS server fed plaintext: the handshake fails and no
        // application byte is ever answered.
        let handshake = acceptor.accept(stream).await;
        assert!(handshake.is_err(), "plaintext is not TLS");
    });

    let mut plain = tokio::net::TcpStream::connect(address).await.expect("tcp");
    // Speak the application protocol in the clear.
    plain
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n")
        .await
        .expect("write plaintext");
    // Any response would be a protocol violation; the read side must see
    // EOF/close with no application frame.
    let mut buffer = [0u8; 128];
    let read = plain.read(&mut buffer).await;
    // A TLS alert record or a bare close is the correct rejection; the one
    // forbidden outcome is a JSON application frame.
    if let Ok(n) = read {
        let answered = &buffer[..n];
        assert!(
            !answered.contains(&b'{'),
            "plaintext client must never receive an application frame"
        );
    }
    server.await.expect("server settled");
}
