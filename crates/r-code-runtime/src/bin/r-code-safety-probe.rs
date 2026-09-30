//! P13 — content-addressed deny-probe helper. Reads ONE versioned JSON
//! request from stdin, ATTEMPTS every requested forbidden operation for
//! real, and writes ONE versioned JSON result to stdout. Every probe is a
//! deny-probe: `denied` means the attempt failed the way a sandbox fails
//! it (the desired outcome); `allowed` means the forbidden operation
//! succeeded, which fails the suite; the network probe additionally
//! reports `ambiguous` for non-policy failures (refused/timeout), which
//! also fails the suite — a dead port can never fake a pass. The helper
//! performs no sandboxing itself and never interprets results — the
//! runner owns pass/fail. Protocol violations exit 2 with an error
//! object; diagnostics never touch stdout.

use std::io::{Read, Write};

const PROTOCOL_VERSION: u32 = 1;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProbeRequest {
    version: u32,
    probes: Vec<String>,
    targets: ProbeTargets,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProbeTargets {
    outside_write_path: String,
    dot_git_dir: String,
    network_host: String,
    network_port: u16,
    credential_service: String,
    credential_user: String,
    device_path: String,
    ipc_path: String,
    child_command: String,
    forbidden_env: Vec<String>,
}

#[derive(serde::Serialize)]
struct ProbeOutcome<'a> {
    probe: &'a str,
    outcome: &'static str,
    detail: String,
}

const EXIT_PROTOCOL: i32 = 2;

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        fail("request stream is unreadable");
    }
    let request: ProbeRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => fail(&format!("request is not valid JSON: {error}")),
    };
    if request.version != PROTOCOL_VERSION {
        fail("unsupported probe protocol version");
    }
    let mut outcomes: Vec<ProbeOutcome<'_>> = Vec::new();
    for probe in &request.probes {
        let targets = &request.targets;
        let (outcome, detail) = match probe.as_str() {
            "write-outside-allowlist" => probe_write_outside(&targets.outside_write_path),
            "dot-git-access" => probe_dot_git(&targets.dot_git_dir),
            "network-outbound" => probe_network(&targets.network_host, targets.network_port),
            "registry-or-keychain" => {
                probe_credential(&targets.credential_service, &targets.credential_user)
            }
            "device-access" => probe_device(&targets.device_path),
            "ipc-access" => probe_ipc(&targets.ipc_path),
            "child-escape" => probe_child_spawn(&targets.child_command),
            "environment-leakage" => probe_env_leakage(&targets.forbidden_env),
            other => fail(&format!("unknown probe id: {other}")),
        };
        outcomes.push(ProbeOutcome {
            probe,
            outcome,
            detail,
        });
    }
    let reply = serde_json::json!({
        "version": PROTOCOL_VERSION,
        "results": outcomes,
    });
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{reply}");
}

fn fail(reason: &str) -> ! {
    let reply = serde_json::json!({"version": PROTOCOL_VERSION, "error": reason});
    let _ = writeln!(std::io::stdout().lock(), "{reply}");
    std::process::exit(EXIT_PROTOCOL);
}

/// Allowed iff the forbidden file could be created and written.
fn probe_write_outside(path: &str) -> (&'static str, String) {
    match std::fs::File::create(path).and_then(|mut file| {
        file.write_all(b"r-code-safety-probe sentinel\n")?;
        file.sync_all()
    }) {
        Ok(()) => ("allowed", String::new()),
        Err(error) => ("denied", error.to_string()),
    }
}

/// Allowed iff the .git directory is listable/readable.
fn probe_dot_git(dir: &str) -> (&'static str, String) {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            // Force at least one directory read to prove real visibility.
            match entries.filter_map(Result::ok).next() {
                Some(entry) => match std::fs::File::open(entry.path()) {
                    Ok(_) => ("allowed", String::new()),
                    Err(error) => ("denied", error.to_string()),
                },
                None => ("allowed", "empty but listable".into()),
            }
        }
        Err(error) => ("denied", error.to_string()),
    }
}

/// Network classification is three-way: `allowed` iff an outbound TCP
/// connection was established; `denied` ONLY for a policy rejection
/// (WSAEACCES — the sandbox refusing the connect); everything else
/// (refused, timeout, unreachable) is `ambiguous` and fails the suite, so
/// pointing the probe at a dead port can never fake a pass. The runner
/// supplies the target; no listener is required on the host side.
fn probe_network(host: &str, port: u16) -> (&'static str, String) {
    match std::net::TcpStream::connect((host, port)) {
        Ok(_) => ("allowed", String::new()),
        Err(error) => {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                ("denied", error.to_string())
            } else {
                ("ambiguous", error.to_string())
            }
        }
    }
}

/// Allowed iff the sentinel credential is readable from the OS credential
/// store (Windows credential manager / macOS keychain / Linux secret
/// service via the keyring crate). The runner plants the sentinel first,
/// so a sandbox that leaks host credentials reports `allowed`.
fn probe_credential(service: &str, user: &str) -> (&'static str, String) {
    let entry = keyring::Entry::new(service, user);
    match entry.and_then(|entry| entry.get_password()) {
        Ok(_) => ("allowed", String::new()),
        Err(error) => ("denied", error.to_string()),
    }
}

/// Allowed iff a raw host device node could be opened.
fn probe_device(path: &str) -> (&'static str, String) {
    match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(_) => ("allowed", String::new()),
        Err(error) => ("denied", error.to_string()),
    }
}

/// Allowed iff a host IPC endpoint could be reached: a Unix domain socket
/// on Unix, a named pipe on Windows.
fn probe_ipc(path: &str) -> (&'static str, String) {
    #[cfg(unix)]
    {
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => ("allowed", String::new()),
            Err(error) => ("denied", error.to_string()),
        }
    }
    #[cfg(windows)]
    {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        {
            Ok(_) => ("allowed", String::new()),
            Err(error) => ("denied", error.to_string()),
        }
    }
}

/// Allowed iff any child process could be spawned (and is immediately
/// reaped — this probe leaves nothing behind either way).
fn probe_child_spawn(command: &str) -> (&'static str, String) {
    match std::process::Command::new(command).spawn() {
        Ok(mut child) => {
            let _ = child.kill();
            let _ = child.wait();
            ("allowed", String::new())
        }
        Err(error) => ("denied", error.to_string()),
    }
}

/// Allowed iff any environment key forbidden by the profile leaked into
/// this process. Absence of every forbidden key is the only denial.
fn probe_env_leakage(forbidden: &[String]) -> (&'static str, String) {
    let leaked: Vec<&str> = forbidden
        .iter()
        .filter(|key| std::env::var(key).is_ok())
        .map(String::as_str)
        .collect();
    if leaked.is_empty() {
        ("denied", String::new())
    } else {
        ("allowed", leaked.join(","))
    }
}
