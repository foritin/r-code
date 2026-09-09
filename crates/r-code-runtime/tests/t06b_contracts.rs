//! T06b — deduplicate application commands across client reconnects.
//!
//! Real clients lose replies and reconnect: effects (counter bumps) occur
//! once; a reused command_id with a different payload fails; receipts
//! survive a daemon restart; the client outbox persists pending commands
//! before first transmission.

use r_code_client::{DaemonClient, Outbox};
use r_code_harness_protocol::application::methods;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::future::Future;
use std::process::{Child, Stdio};
use std::time::Duration;

const SERVICE: &str = env!("CARGO_BIN_EXE_r-code-service");

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn profile_for(name: &str, root: &std::path::Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(root.join("root"))
            .with_ipc_name(name),
    )
    .expect("profile")
}

fn spawn_daemon(profile: &RuntimeProfile) -> Child {
    let mut command = std::process::Command::new(SERVICE);
    command
        .arg("--profile")
        .arg("development")
        .arg("--data-root")
        .arg(profile.data_root())
        .arg("--ipc-name")
        .arg(profile.ipc_name().expect("test profile sets an ipc name"));
    command
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

fn run<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

#[test]
fn effects_occur_once_across_lost_replies_and_reconnects() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("t06b-effects", temp.path());
    let guard = DaemonGuard(spawn_daemon(&profile));
    let info = wait_for_owner(&profile);

    // The outbox records the command BEFORE the first transmission.
    let mut outbox = Outbox::open(&profile.harness_v2_root(), "client-a").expect("outbox");
    outbox
        .prepare("bump-1", "counter.bump", serde_json::json!({}))
        .expect("prepare");
    assert_eq!(outbox.pending().len(), 1);

    // First transmission: the effect runs and the reply arrives.
    run(async {
        let mut client = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "client-a",
        )
        .await
        .expect("connect");
        let result = client
            .call_with_id("counter.bump", serde_json::json!({}), "bump-1")
            .await
            .expect("bump");
        assert_eq!(result["count"], 1);
        outbox.mark_acked("bump-1").expect("ack");
        assert!(outbox.pending().is_empty());
        let total = client
            .call("counter.get", serde_json::json!({}))
            .await
            .expect("get");
        assert_eq!(total["count"], 1);
    });

    // Reconnect and replay the same command id (as a client would after a
    // lost reply): the stored result replays, the counter does not move.
    run(async {
        let mut client = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "client-a",
        )
        .await
        .expect("reconnect");
        let replayed = client
            .call_with_id("counter.bump", serde_json::json!({}), "bump-1")
            .await
            .expect("replay");
        assert_eq!(replayed["count"], 1, "original result replayed");
        let total = client
            .call("counter.get", serde_json::json!({}))
            .await
            .expect("get");
        assert_eq!(total["count"], 1, "effect executed exactly once");
    });
    drop(guard);
}

#[test]
fn reused_command_id_with_different_payload_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("t06b-conflict", temp.path());
    let guard = DaemonGuard(spawn_daemon(&profile));
    let info = wait_for_owner(&profile);

    run(async {
        let mut client = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "client-b",
        )
        .await
        .expect("connect");
        let first = client
            .call_with_id(methods::ECHO, serde_json::json!({"value": 1}), "echo-1")
            .await
            .expect("echo");
        assert_eq!(first["value"], 1);

        // Same id, different payload: refused before any effect.
        let error = client
            .call_with_id(methods::ECHO, serde_json::json!({"value": 2}), "echo-1")
            .await
            .expect_err("conflict");
        assert!(error.to_string().contains("different payload"), "{error}");

        // Different id with the same payload is a fresh, valid command.
        let second = client
            .call_with_id(methods::ECHO, serde_json::json!({"value": 2}), "echo-2")
            .await
            .expect("echo-2");
        assert_eq!(second["value"], 2);
    });
    drop(guard);
}

#[test]
fn receipts_survive_a_daemon_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("t06b-restart", temp.path());

    let mut first = spawn_daemon(&profile);
    let info = wait_for_owner(&profile);
    run(async {
        let mut client = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "client-c",
        )
        .await
        .expect("connect");
        let result = client
            .call_with_id("counter.bump", serde_json::json!({}), "bump-restart")
            .await
            .expect("bump");
        assert_eq!(result["count"], 1);
    });
    first.kill().expect("kill daemon");
    first.wait().expect("reap");

    // Successor daemon: same store, receipts intact. Wait for the identity
    // to flip (the old owner.json lingers on disk).
    let successor = DaemonGuard(spawn_daemon(&profile));
    let mut successor_info = None;
    for _ in 0..80 {
        if let Some(candidate) = r_code_client::read_owner_token(&profile.harness_v2_root()) {
            if candidate.nonce != info.nonce {
                successor_info = Some(candidate);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let successor_info = successor_info.expect("successor never took over");
    run(async {
        let mut client = DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &successor_info.token,
            "client-c",
        )
        .await
        .expect("reconnect");
        let replayed = client
            .call_with_id("counter.bump", serde_json::json!({}), "bump-restart")
            .await
            .expect("replay after restart");
        assert_eq!(replayed["count"], 1, "receipt survived the restart");
        let total = client
            .call("counter.get", serde_json::json!({}))
            .await
            .expect("get");
        assert_eq!(total["count"], 1, "still exactly one effect");
    });
    drop(successor);
}

#[test]
fn hash_covers_method_and_canonical_payload() {
    use r_code_runtime::application_receipts::canonical_command_hash;
    let a = canonical_command_hash(
        "task.create",
        &serde_json::json!({"title": "x", "body": "y"}),
    );
    let b = canonical_command_hash(
        "task.create",
        &serde_json::json!({"body": "y", "title": "x"}),
    );
    assert_eq!(a, b, "member order never matters");
    let c = canonical_command_hash(
        "task.delete",
        &serde_json::json!({"title": "x", "body": "y"}),
    );
    assert_ne!(a, c, "method is part of the identity");
    let d = canonical_command_hash(
        "task.create",
        &serde_json::json!({"title": "z", "body": "y"}),
    );
    assert_ne!(a, d, "payload differences change the hash");
}
