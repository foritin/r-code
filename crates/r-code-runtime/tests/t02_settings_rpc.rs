use r_code_client::{ClientError, DaemonClient};
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

fn profile_for(name: &str, root: &std::path::Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(root.join("root"))
            .with_ipc_name(name),
    )
    .expect("resolve test profile")
}

fn spawn_daemon(profile: &RuntimeProfile) -> DaemonGuard {
    let mut command = Command::new(SERVICE);
    command
        .arg("--profile")
        .arg("development")
        .arg("--data-root")
        .arg(profile.data_root())
        .arg("--ipc-name")
        .arg(profile.ipc_name().expect("test profile has IPC name"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    DaemonGuard(command.spawn().expect("spawn r-code-service"))
}

fn wait_for_owner(profile: &RuntimeProfile) -> r_code_client::DaemonInfo {
    for _ in 0..100 {
        if let Some(info) = r_code_client::read_owner_token(&profile.harness_v1_root()) {
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

async fn connect(profile: &RuntimeProfile, info: &r_code_client::DaemonInfo) -> DaemonClient {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            &info.token,
            "t02-settings-qa",
        )
        .await
        {
            Ok(client) => return client,
            Err(ClientError::Unreachable(_)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => panic!("connect to settings daemon: {error}"),
        }
    }
}

fn command_error(error: ClientError) -> String {
    match error {
        ClientError::Command(message) => message,
        other => panic!("expected daemon command error, got {other:?}"),
    }
}

#[test]
fn daemon_settings_mutations_require_and_advance_exact_revision() {
    let directory = tempfile::tempdir().expect("create daemon profile root");
    let profile = profile_for("t02-settings-cas", directory.path());
    let environment = format!("R_CODE_T02_RPC_KEY_{}", std::process::id());
    std::env::set_var(&environment, "rpc-environment-secret");
    let _guard = spawn_daemon(&profile);
    let info = wait_for_owner(&profile);

    run(async {
        let mut client = connect(&profile, &info).await;
        let initial = client
            .call("settings.get", serde_json::json!({}))
            .await
            .expect("read initial settings");
        assert_eq!(initial["revision"], 0);

        let missing_secret = "missing-revision-secret";
        for (method, params) in [
            (
                "settings.apply",
                serde_json::json!({
                    "selection": "qa-t02-provider",
                    "model": "model-a",
                    "apiKey": missing_secret,
                }),
            ),
            (
                "settings.setDefault",
                serde_json::json!({"selection": "qa-t02-provider"}),
            ),
            (
                "settings.removeProvider",
                serde_json::json!({"selection": "qa-t02-provider"}),
            ),
        ] {
            let missing = client
                .call(method, params)
                .await
                .expect_err("legacy mutation without revision must fail");
            let missing = command_error(missing);
            assert_eq!(missing, "settings_revision_required", "method={method}");
            assert!(!missing.contains(missing_secret));
        }

        let applied = client
            .call(
                "settings.apply",
                serde_json::json!({
                    "expectedRevision": 0,
                    "selection": "qa-t02-provider",
                    "model": "model-a",
                    "envVar": environment,
                }),
            )
            .await
            .expect("apply at revision zero");
        assert_eq!(applied["revision"], 1);
        assert_eq!(applied["settings"]["revision"], 1);
        assert_eq!(applied["settings"]["providers"][0]["model"], "model-a");
        assert_eq!(applied["providers"][0]["selection"], "qa-t02-provider");
        assert_eq!(applied["providers"][0]["has_credential"], true);
        let serialized = applied.to_string();
        assert!(!serialized.contains("rpc-environment-secret"));
        assert!(!serialized.contains("apiKey"));

        let stale_secret = "stale-revision-secret";
        for (method, params) in [
            (
                "settings.apply",
                serde_json::json!({
                    "expectedRevision": 0,
                    "selection": "qa-t02-provider",
                    "model": "model-b",
                    "apiKey": stale_secret,
                }),
            ),
            (
                "settings.setDefault",
                serde_json::json!({
                    "expectedRevision": 0,
                    "selection": "qa-t02-provider",
                }),
            ),
            (
                "settings.removeProvider",
                serde_json::json!({
                    "expectedRevision": 0,
                    "selection": "qa-t02-provider",
                }),
            ),
        ] {
            let stale = client
                .call(method, params)
                .await
                .expect_err("stale mutation must fail");
            let stale = command_error(stale);
            assert_eq!(
                stale, "settings_stale_revision:expected=0:actual=1",
                "method={method}"
            );
            assert!(!stale.contains(stale_secret));
        }

        let unchanged = client
            .call("settings.get", serde_json::json!({}))
            .await
            .expect("read after stale mutation");
        assert_eq!(unchanged["revision"], 1);
        assert_eq!(unchanged["providers"][0]["model"], "model-a");

        let updated = client
            .call(
                "settings.setDefault",
                serde_json::json!({
                    "expectedRevision": 1,
                    "selection": "qa-t02-provider",
                }),
            )
            .await
            .expect("set default at exact revision");
        assert_eq!(updated["revision"], 2);
        assert_eq!(updated["settings"]["revision"], 2);
        assert_eq!(updated["providers"][0]["is_default"], true);
    });

    std::env::remove_var(environment);
}

#[test]
fn daemon_checked_settings_reads_and_mutations_fail_closed_without_secret_echo() {
    let directory = tempfile::tempdir().expect("create corrupt profile root");
    let profile = profile_for("t02-settings-corrupt", directory.path());
    profile.ensure_layout().expect("create profile layout");
    let corrupt = r#"{"providers":[],"apiKey":"corrupt-file-secret","broken":}"#;
    let path = profile.harness_v1_root().join("settings.json");
    std::fs::write(&path, corrupt).expect("write corrupt settings fixture");
    let _guard = spawn_daemon(&profile);
    let info = wait_for_owner(&profile);

    run(async {
        let mut client = connect(&profile, &info).await;
        for method in ["settings.get", "models.available"] {
            let error = client
                .call(method, serde_json::json!({}))
                .await
                .expect_err("corrupt settings must fail closed");
            let error = command_error(error);
            assert_eq!(error, "settings_corrupt");
            assert!(!error.contains("corrupt-file-secret"));
        }

        let request_secret = "corrupt-mutation-secret";
        let error = client
            .call(
                "settings.apply",
                serde_json::json!({
                    "expectedRevision": 0,
                    "selection": "qa-t02-provider",
                    "model": "model-a",
                    "apiKey": request_secret,
                }),
            )
            .await
            .expect_err("mutation must not replace corrupt settings");
        let error = command_error(error);
        assert_eq!(error, "settings_corrupt");
        assert!(!error.contains(request_secret));
    });

    assert_eq!(
        std::fs::read_to_string(path).expect("read preserved corrupt settings"),
        corrupt
    );
}
