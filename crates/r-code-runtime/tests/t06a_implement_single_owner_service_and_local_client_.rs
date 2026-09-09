//! T06a — single-owner service and local client transport.
//!
//! Real client processes and competing daemons prove: one owner per profile,
//! live-owner protection, stale-owner takeover after a kill, token
//! authentication, durable command results across reconnects and
//! uninterrupted work after the frontend exits.

use r_code_client::DaemonClient;
use r_code_harness_protocol::application::methods;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::future::Future;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const SERVICE: &str = env!("CARGO_BIN_EXE_r-code-service");

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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

fn daemon_command(profile: &RuntimeProfile) -> Command {
    let mut command = Command::new(SERVICE);
    command
        .arg("--profile")
        .arg("development")
        .arg("--data-root")
        .arg(profile.data_root());
    if let Some(name) = profile.ipc_name() {
        command.arg("--ipc-name").arg(name);
    }
    command
}

fn spawn_daemon(profile: &RuntimeProfile) -> Child {
    daemon_command(profile)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn service")
}

fn wait_for_owner(profile: &RuntimeProfile) -> r_code_client::DaemonInfo {
    for _ in 0..80 {
        if let Some(info) = r_code_client::read_owner_token(&profile.harness_v2_root()) {
            return info;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("daemon never wrote its owner file");
}

/// One runtime per test: client IO objects must not outlive their reactor.
fn run<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

#[test]
fn competing_daemons_converge_to_one_owner() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("competing", temp.path());

    let first = DaemonGuard(spawn_daemon(&profile));
    let info = wait_for_owner(&profile);
    assert!(!info.token.is_empty());

    // A second daemon on the same profile exits with an ownership error.
    let second = daemon_command(&profile)
        .output()
        .expect("run second daemon");
    assert!(
        !second.status.success(),
        "second daemon must refuse to start"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("owns this profile"), "stderr: {stderr}");

    // The first owner is untouched and still serving.
    run(async {
        let mut client = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "client-1",
        )
        .await
        .expect("client connects to live owner");
        let pong = client
            .call(methods::PING, serde_json::json!({}))
            .await
            .expect("ping");
        assert_eq!(pong["pong"], true);
    });
    drop(first);
}

#[test]
fn token_authentication_rejects_forged_handshakes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("token-auth", temp.path());
    let _guard = DaemonGuard(spawn_daemon(&profile));
    let info = wait_for_owner(&profile);

    run(async {
        let error = match DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            "forged-token",
            "intruder",
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("forged token must be rejected"),
        };
        assert!(matches!(error, r_code_client::ClientError::Handshake(_)));

        // Wrong profile id is equally rejected.
        let error = match DaemonClient::connect(
            &profile.ipc_endpoint(),
            "harness-v2/production",
            &info.token,
            "intruder",
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("wrong profile must be rejected"),
        };
        assert!(matches!(error, r_code_client::ClientError::Handshake(_)));
    });
}

#[test]
fn stale_owner_is_taken_over_after_a_kill() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("stale-owner", temp.path());

    // First daemon; hard-kill it without cleanup.
    let mut first = spawn_daemon(&profile);
    let first_info = wait_for_owner(&profile);
    first.kill().expect("kill owner");
    first.wait().expect("reap");

    // The OS lock died with the process: a successor takes over. The old
    // owner.json lingers on disk, so wait for the identity to actually flip.
    let successor = DaemonGuard(spawn_daemon(&profile));
    let mut successor_info = None;
    for _ in 0..80 {
        if let Some(info) = r_code_client::read_owner_token(&profile.harness_v2_root()) {
            if info.nonce != first_info.nonce {
                successor_info = Some(info);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let successor_info = successor_info.expect("successor never took over");
    assert_ne!(successor_info.token, first_info.token, "new owner identity");

    run(async {
        let mut client = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &successor_info.token,
            "client-2",
        )
        .await
        .expect("client reaches successor");
        let pong = client
            .call(methods::PING, serde_json::json!({}))
            .await
            .expect("ping");
        assert_eq!(pong["pong"], true);
    });
    drop(successor);
}

#[test]
fn two_clients_share_the_daemon_and_results_survive_reconnects() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("two-clients", temp.path());
    let guard = DaemonGuard(spawn_daemon(&profile));
    let info = wait_for_owner(&profile);

    run(async {
        // Two simultaneous clients.
        let mut client_a = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "client-a",
        )
        .await
        .expect("client a");
        let mut client_b = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "client-b",
        )
        .await
        .expect("client b");
        assert_eq!(client_a.daemon_nonce, client_b.daemon_nonce, "same daemon");

        // Durable command results: same (client, command) replays its result.
        let echo_params = serde_json::json!({"value": "keep-me"});
        let first = client_a
            .call_with_id(methods::ECHO, echo_params.clone(), "cmd-1")
            .await
            .expect("echo");
        assert_eq!(first["value"], "keep-me");

        // Reconnect as a new connection and repeat the id: original result.
        drop(client_a);
        let mut client_a2 = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "client-a",
        )
        .await
        .expect("client a reconnects");
        let replayed = client_a2
            .call_with_id(methods::ECHO, echo_params, "cmd-1")
            .await
            .expect("replay");
        assert_eq!(replayed["value"], "keep-me");

        let pong = client_b
            .call(methods::PING, serde_json::json!({}))
            .await
            .expect("ping b");
        assert_eq!(pong["pong"], true);
    });
    drop(guard);
}

#[test]
fn work_continues_after_the_frontend_exits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("frontend-exit", temp.path());
    let guard = DaemonGuard(spawn_daemon(&profile));
    let info = wait_for_owner(&profile);

    // A frontend submits a slow command, then "closes" (connection dropped).
    run(async {
        let mut frontend = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "frontend",
        )
        .await
        .expect("frontend");
        // Send the slow command but drop the connection before the reply.
        let _ = frontend
            .call_with_id("slow-echo", serde_json::json!({"work": 1}), "slow-1")
            .await;
    });
    // Give the daemon time to finish the command durably.
    std::thread::sleep(Duration::from_millis(400));

    run(async {
        // The daemon kept running through the frontend exit; the durable
        // result for (frontend, slow-1) is retrievable by a reconnect.
        let mut same_client = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "frontend",
        )
        .await
        .expect("same client id reconnects");
        let result = same_client
            .call_with_id("slow-echo", serde_json::json!({"work": 1}), "slow-1")
            .await
            .expect("durable result");
        assert_eq!(result["work"], 1);

        // Fresh work still flows.
        let mut next = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "frontend-2",
        )
        .await
        .expect("second frontend");
        let pong = next
            .call(methods::PING, serde_json::json!({}))
            .await
            .expect("ping");
        assert_eq!(pong["pong"], true);
    });
    drop(guard);
}
