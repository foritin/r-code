//! R09.A2 — mDNS discovery: the pairing listener advertises
//! `_rcode._tcp.local` with the port and fingerprint TXT record; stopping
//! the listener unregisters the service.

use r_code_harness_protocol::application::ApplicationCommand;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::fanout::FanoutHub;
use r_code_runtime::remote::listener::listen_pairing;
use r_code_runtime::remote::pairing::PairingSessions;
use r_code_runtime::remote::registry::{DeviceRegistry, ListenerConfig};
use r_code_runtime::remote::tls::ensure_identity;
use std::sync::Arc;
use std::time::Duration;

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

#[tokio::test]
async fn r09_a2_mdns_advertises_port_and_fingerprint_until_stopped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let registry = Arc::new(DeviceRegistry::open(root).expect("registry"));
    registry
        .set_listener(ListenerConfig {
            enabled: true,
            bind: vec![],
            port: None,
        })
        .expect("enable");
    let identity = ensure_identity(root).expect("identity");
    let pairing = Arc::new(PairingSessions::new(Duration::from_secs(120)));
    pairing
        .start(
            r_code_runtime::application::CommandSource::Local,
            &identity.fingerprint,
            vec![],
        )
        .expect("open pairing window");
    let handle = listen_pairing(
        "127.0.0.1".parse().unwrap(),
        0,
        registry.clone(),
        identity.duplicate(),
        Arc::new(NullHandler),
        FanoutHub::new(),
        None,
        Some(pairing.clone()),
    )
    .await
    .expect("listen");

    // Registration truth first (in-daemon table; multicast loopback is
    // platform-dependent, so the browse below is best-effort).
    let (port, fp) = handle
        .mdns_registered_info()
        .expect("service registered with port + fp TXT");
    assert_eq!(port, handle.local_addr.port());
    assert_eq!(fp, identity.fingerprint);

    // A client on the same machine browses the service (best effort).
    let browser = mdns_sd::ServiceDaemon::new().expect("browser daemon");
    let receiver = browser.browse("_rcode._tcp.local.").expect("browse");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while let Ok(event) =
        receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
            assert_eq!(info.get_port(), handle.local_addr.port(), "port TXT");
            assert_eq!(
                info.get_properties().get_property_val_str("fp"),
                Some(identity.fingerprint.as_str()),
                "fingerprint TXT"
            );
            break;
        }
    }
    browser.shutdown().expect("browser shutdown");

    // Stopping the listener unregisters the service: a fresh browse no
    // longer resolves it.
    handle.stop();
    tokio::time::sleep(Duration::from_millis(500)).await;
    // Unregistered: the in-daemon table no longer carries the service.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        handle.mdns_registered_info().is_none(),
        "service must disappear from the registry after stop()"
    );
}
