//! R08 — manual pairing end to end against the *real* r-code-service
//! binary: the local console starts a pairing session over the pipe, a
//! remote client exchanges the one-time code for a device token over
//! pinned TLS, then observes real task events. Unauthenticated or
//! wrong-code connections get nothing.

use futures_util::{SinkExt, StreamExt};
use r_code_client::ws::{RemoteClient, RemoteEndpoint};
use r_code_client::DaemonClient;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const SERVICE: &str = env!("CARGO_BIN_EXE_r-code-service");

struct DaemonGuard(Child, std::path::PathBuf);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        // Kill any listener the pairing session opened (belt and braces:
        // the supervision loop also closes it, but the process may die
        // before its next tick).
        let _ = Command::new("taskkill")
            .args(["/PID", &self.0.id().to_string(), "/F", "/T"])
            .status();
        let _ = &self.1;
    }
}

fn profile_for(test: &str, root: &std::path::Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(root.join("root"))
            .with_ipc_name(test),
    )
    .expect("profile")
}

/// Stage the `serve` fixture harness as a built-in package directory.
fn stage_serve_fixture(temp: &std::path::Path) -> std::path::PathBuf {
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_harness-test-helper"));
    let resources = temp.join("builtin-plugins");
    let pkg = resources.join("plugins").join("fixture.serve");
    let bin = pkg.join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    std::fs::copy(&binary, bin.join("harness-test-helper")).expect("copy");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    std::fs::write(
        pkg.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": "fixture.serve",
            "version": "1.0.0",
            "apiMajor": 1,
            "apiMinor": 0,
            "displayName": "Serve Fixture",
            "supportedPlatforms": [{"platform": platform, "executable": "bin/harness-test-helper", "argv": ["serve"]}],
            "requestedHostServices": ["host.tools.list", "host.completion.propose"],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");
    resources
}

fn spawn_daemon(profile: &RuntimeProfile, builtin_dir: &std::path::Path) -> Child {
    let mut command = Command::new(SERVICE);
    command
        .arg("--profile")
        .arg("development")
        .arg("--data-root")
        .arg(profile.data_root())
        .env("R_CODE_BUILTIN_PLUGINS_DIR", builtin_dir);
    if let Some(name) = profile.ipc_name() {
        command.arg("--ipc-name").arg(name);
    }
    command
        .stdout(Stdio::null())
        .stderr(
            std::fs::File::create(std::env::temp_dir().join("r08-service.log")).expect("log file"),
        )
        .spawn()
        .expect("spawn service")
}

fn wait_for_owner(profile: &RuntimeProfile) -> r_code_client::DaemonInfo {
    for _ in 0..100 {
        if let Some(info) = r_code_client::read_owner_token(&profile.harness_v2_root()) {
            return info;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("daemon never wrote its owner file");
}

type Ws = WebSocketStream<TlsStream<TcpStream>>;

async fn wss_connect(port: u16, fingerprint: &str) -> Ws {
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.expect("tcp");
    let connector = tokio_rustls::TlsConnector::from(Arc::new(
        r_code_runtime::remote::pinned_client_config(fingerprint).expect("cfg"),
    ));
    let name = rustls::pki_types::ServerName::try_from("r-code-daemon".to_string()).unwrap();
    let tls = connector.connect(name, tcp).await.expect("pinned tls");
    let (ws, _) = tokio_tungstenite::client_async_with_config(
        format!("wss://r-code-daemon:{port}/remote"),
        tls,
        None,
    )
    .await
    .expect("ws");
    ws
}

async fn hello_frame(ws: &mut Ws, body: serde_json::Value) -> serde_json::Value {
    ws.send(Message::Text(body.to_string().into()))
        .await
        .expect("send");
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).expect("welcome json");
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            other => panic!("expected welcome, got {other:?}"),
        }
    }
}

/// The owner file appears before the named pipe binds; retry until the
/// endpoint actually accepts (t06a races the same window).
async fn connect_with_retry(
    profile: &RuntimeProfile,
    owner: &r_code_client::DaemonInfo,
) -> DaemonClient {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &owner.token,
            "e2e-console",
        )
        .await
        {
            Ok(client) => return client,
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            Err(error) => panic!("pipe connect: {error}"),
        }
    }
}

#[tokio::test]
async fn r08_a1_manual_pairing_read_only_loop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("r08a1", temp.path());
    let builtin = stage_serve_fixture(temp.path());
    let _guard = DaemonGuard(spawn_daemon(&profile, &builtin), temp.path().to_path_buf());
    let owner = wait_for_owner(&profile);
    let mut local = connect_with_retry(&profile, &owner).await;

    // 1) The local console starts pairing: code + listener + fingerprint.
    let started = local
        .call("remote.pairingStart", serde_json::json!({}))
        .await
        .expect("pairingStart");
    let code = started["pairingCode"].as_str().expect("code").to_string();
    let port = started["port"].as_u64().expect("port") as u16;
    let fingerprint = started["fingerprint"].as_str().expect("fp").to_string();
    assert!(code.len() >= 32, "high-entropy one-time code");
    assert!(started["qrPayload"]
        .as_str()
        .unwrap()
        .starts_with("rcode://pair"));

    // 2) The remote client exchanges the code over pinned TLS.
    let mut pairing_ws = wss_connect(port, &fingerprint).await;
    let welcome = hello_frame(
        &mut pairing_ws,
        serde_json::json!({
            "hello": "r-code-remote/1",
            "token": code,
            "device_name": "e2e-phone",
            "platform": "pwa",
        }),
    )
    .await;
    assert_eq!(welcome["ok"], true, "pairing accepted: {welcome}");
    assert_eq!(welcome["paired"], true);
    let device_token = welcome["token"].as_str().expect("minted token").to_string();
    let device_id = welcome["deviceId"].as_str().expect("device id").to_string();
    assert_eq!(welcome["capabilities"][0], "events-read");

    // 3) The same code can never pair again (F4 one-shot).
    let mut replay = wss_connect(port, &fingerprint).await;
    let refused = hello_frame(
        &mut replay,
        serde_json::json!({"hello": "r-code-remote/1", "token": code, "device_name": "x"}),
    )
    .await;
    assert_eq!(refused["ok"], false, "one-time code: {refused}");

    // 4) The paired token connects as a device (read-only).
    let endpoint = RemoteEndpoint {
        host: "127.0.0.1".into(),
        port,
        fingerprint,
        token: device_token,
        device_id,
    };
    let mut device = RemoteClient::connect(&endpoint)
        .await
        .expect("device connect");
    let tasks = device
        .call("task.list", serde_json::json!({}))
        .await
        .expect("task.list");
    assert!(tasks.as_array().is_some(), "read surface works: {tasks}");
    // Write surface refuses (read-only default).
    let refused = device
        .call(
            "task.sendMessage",
            serde_json::json!({"taskId": "t", "text": "x"}),
        )
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("permission_denied"));

    // 5) The local console drives a real run; the device observes events.
    let task = local
        .call(
            "task.create",
            serde_json::json!({"taskId": "e2e-1", "objective": "r08 e2e"}),
        )
        .await
        .expect("create");
    assert_eq!(task["taskId"], "e2e-1");
    local
        .call(
            "task.selectHarness",
            serde_json::json!({"taskId": "e2e-1", "harnessId": "fixture.serve"}),
        )
        .await
        .expect("pin fixture");
    local
        .call(
            "task.sendMessage",
            serde_json::json!({"taskId": "e2e-1", "text": "hello"}),
        )
        .await
        .expect("send");

    // Subscribe from 0: history + live both arrive through the same
    // projection as task.events (F8).
    let mut seen_kinds = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    'outer: loop {
        let batch = device.subscribe_events(0).await.expect("subscribe");
        for event in batch {
            if let Some(kind) = event.payload.get("journalKind").and_then(|v| v.as_str()) {
                if kind == "run.completed" || kind == "run.failed" {
                    seen_kinds.push(kind.to_string());
                    break 'outer;
                }
                seen_kinds.push(kind.to_string());
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run never settled: {seen_kinds:?}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(
        seen_kinds.iter().any(|kind| kind == "run.started"),
        "real run events observed: {seen_kinds:?}"
    );
}

#[tokio::test]
async fn r08_a2_unauthenticated_connections_get_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("r08a2", temp.path());
    let builtin = stage_serve_fixture(temp.path());
    let _guard = DaemonGuard(spawn_daemon(&profile, &builtin), temp.path().to_path_buf());
    let owner = wait_for_owner(&profile);
    let mut local = connect_with_retry(&profile, &owner).await;
    let started = local
        .call("remote.pairingStart", serde_json::json!({}))
        .await
        .expect("pairingStart");
    let port = started["port"].as_u64().unwrap() as u16;
    let fingerprint = started["fingerprint"].as_str().unwrap().to_string();

    // Wrong code: refused with the structured code, no data.
    let mut bad = wss_connect(port, &fingerprint).await;
    let refused = hello_frame(
        &mut bad,
        serde_json::json!({"hello": "r-code-remote/1", "token": "not-a-code"}),
    )
    .await;
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "unauthorized");

    // No hello at all: a command frame never gets an application answer.
    let mut silent = wss_connect(port, &fingerprint).await;
    silent
        .send(Message::Text(
            serde_json::json!({
                "client_id": "x", "command_id": "c1",
                "method": "task.list", "params": {}
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send");
    let answer = tokio::time::timeout(Duration::from_secs(3), silent.next()).await;
    match answer {
        // A close or the structured refusal is acceptable — never a
        // command result carrying task data.
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Err(_) => {}
        Ok(Some(Ok(Message::Text(text)))) => {
            let parsed: serde_json::Value = serde_json::from_str(&text).expect("json frame");
            assert_eq!(
                parsed.get("ok"),
                Some(&serde_json::json!(false)),
                "unauthenticated client must never receive task data: {text}"
            );
        }
        other => panic!("unexpected frame: {other:?}"),
    }
}
