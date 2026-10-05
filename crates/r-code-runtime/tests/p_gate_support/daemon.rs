use r_code_runtime::RuntimeProfile;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct Daemon {
    child: Child,
}

impl Daemon {
    pub fn start(profile: &RuntimeProfile, env: Option<(&str, &str)>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_r-code-service"));
        command
            .arg("--profile")
            .arg("development")
            .arg("--data-root")
            .arg(profile.data_root())
            .arg("--ipc-name")
            .arg(profile.ipc_name().expect("ipc name"))
            .stdout(Stdio::null())
            // 诊断落盘：连接超时的唯一线索是 daemon 启动 stderr。
            .stderr({
                let _ = std::fs::create_dir_all(profile.harness_v1_root());
                std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(profile.harness_v1_root().join("test-daemon.stderr"))
                    .map(Stdio::from)
                    .unwrap_or_else(|_| Stdio::null())
            });
        if let Some((name, value)) = env {
            command.env(name, value);
        }
        let daemon = Self {
            child: command.spawn().expect("spawn daemon"),
        };
        for _ in 0..150 {
            if r_code_client::read_owner_token(&profile.harness_v1_root()).is_some() {
                return daemon;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon did not publish owner token")
    }

    pub async fn connect(
        &self,
        profile: &RuntimeProfile,
        client_id: &str,
    ) -> r_code_client::DaemonClient {
        for _ in 0..400 {
            if let Some(owner) = r_code_client::read_owner_token(&profile.harness_v1_root()) {
                if let Ok(client) = r_code_client::DaemonClient::connect(
                    &profile.ipc_endpoint(),
                    &profile.profile_id(),
                    &owner.token,
                    client_id,
                )
                .await
                {
                    return client;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("daemon connection timed out")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
