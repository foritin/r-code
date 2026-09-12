//! R18 — desktop relay configuration: persistence, the outbound link's
//! status machine, the zero-outbound-connections invariant when
//! unconfigured (F12 opt-in), and a live registration against a real relay.

use r_code_harness_protocol::application::ApplicationCommand;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::relay_config::{
    fingerprint_of_seed, generate_registration_code, RelayConfig, RelayLink, RelayMethods,
    RelayStatus,
};
use std::sync::Arc;

fn root(tag: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(tag);
    std::fs::create_dir_all(&root).expect("mkdir");
    (dir, root)
}

#[tokio::test]
async fn r18_a1_config_persists_and_unconfigured_never_dials() {
    let (_dir, root) = root("persist");
    let link = RelayLink::open(&root).expect("open");

    // Unconfigured: the frozen invariant — zero outbound attempts.
    assert_eq!(link.status(), RelayStatus::Unconfigured);
    assert_eq!(link.connect_attempts(), 0, "no config → no outbound dials");
    // A beat to make sure nothing races a dial in the background.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(link.connect_attempts(), 0, "still zero");

    // Configure (enabled): persists and dials.
    let config = RelayConfig {
        url: "127.0.0.1:1".into(), // nothing listens here → backoff loop
        registration_code: "code-x".into(),
        enabled: true,
    };
    let status = link.configure(config.clone()).expect("configure");
    assert_eq!(status, RelayStatus::Connecting { attempt: 1 });
    assert!(link.connect_attempts() >= 1, "configured → dial starts");

    // Persistence: reopen reads the same config.
    let persisted: RelayConfig =
        serde_json::from_str(&std::fs::read_to_string(root.join("relay.json")).unwrap()).unwrap();
    assert_eq!(persisted, config);
    let reopened = RelayLink::open(&root).expect("reopen");
    assert!(matches!(reopened.status(), RelayStatus::Connecting { .. }));
    assert!(reopened.connect_attempts() >= 1, "reopen with config dials");

    // Disable: teardown, no more dials, status unconfigured.
    let disabled = RelayConfig {
        enabled: false,
        ..config
    };
    let status = reopened.configure(disabled).expect("disable");
    assert_eq!(status, RelayStatus::Unconfigured);
}

#[tokio::test]
async fn r18_a1_backoff_machine_visible_in_status() {
    let (_dir, root) = root("backoff");
    let link = RelayLink::open(&root).expect("open");
    link.configure(RelayConfig {
        url: "127.0.0.1:1".into(), // dead port: every dial fails
        registration_code: "code-x".into(),
        enabled: true,
    })
    .expect("configure");

    // The status machine walks connecting → backoff(attempt grows).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut saw_backoff = false;
    let mut attempts_seen = 0;
    while std::time::Instant::now() < deadline {
        match link.status() {
            RelayStatus::Backoff { attempt } => {
                saw_backoff = true;
                attempts_seen = attempts_seen.max(attempt);
            }
            RelayStatus::Connecting { attempt } => {
                attempts_seen = attempts_seen.max(attempt);
            }
            _ => {}
        }
        if saw_backoff && attempts_seen >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(saw_backoff, "failed dials surface as backoff");
    assert!(attempts_seen >= 2, "attempts step up: {attempts_seen}");
}

#[tokio::test]
async fn r18_live_registration_against_a_real_relay_reaches_online() {
    let (_dir, root) = root("live");
    // A real relay instance (R16 server) with a pre-imported code.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let port = listener.local_addr().unwrap().port();
    let state = r_code_relay::RelayState::new();
    let mut codes = std::collections::HashMap::new();
    codes.insert("live-code-1".to_string(), true);
    let codes = Arc::new(std::sync::Mutex::new(codes));
    tokio::spawn(r_code_relay::serve(listener, state, codes));

    let link = RelayLink::open(&root).expect("open");
    let status = link
        .configure(RelayConfig {
            url: format!("127.0.0.1:{port}"),
            registration_code: "live-code-1".into(),
            enabled: true,
        })
        .expect("configure");

    // The link dials; the relay registers the owner; status reaches online.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if link.status() == RelayStatus::Online {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never reached online; status={status:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // The one-time code is consumed (a second relay with the same code
    // list would refuse — single-use enforced in R16).
}

#[test]
fn r18_registration_code_shape_and_fingerprint_stability() {
    let code = generate_registration_code();
    assert_eq!(code.len(), 64, "32-byte code, hex");
    let seed = [7u8; 32];
    assert_eq!(fingerprint_of_seed(&seed), fingerprint_of_seed(&seed));
    assert_ne!(fingerprint_of_seed(&seed), fingerprint_of_seed(&[8u8; 32]));
}

// The daemon method surface (relay.status/configure/issueCode) mirrors the
// handler used by the bin; exercised here through the same implementation.
#[tokio::test]
async fn r18_daemon_method_surface_roundtrip() {
    let (_dir, root) = root("methods");
    let link = Arc::new(RelayLink::open(&root).expect("open"));
    let handler = RelayMethods { link: link.clone() };

    let status = handler
        .execute(ApplicationCommand {
            client_id: "console".into(),
            command_id: "c1".into(),
            method: "relay.status".into(),
            params: serde_json::json!({}),
        })
        .await
        .expect("status");
    assert_eq!(status["state"], "unconfigured");

    let code = handler
        .execute(ApplicationCommand {
            client_id: "console".into(),
            command_id: "c2".into(),
            method: "relay.issueCode".into(),
            params: serde_json::json!({}),
        })
        .await
        .expect("issueCode");
    assert_eq!(code["code"].as_str().unwrap().len(), 64);
}
