//! P13 — sandbox profile material, the REAL deny-probe helper protocol and
//! the fail-closed activation gates. Every helper interaction spawns the
//! real `r-code-safety-probe` binary against REAL targets: a live local
//! TCP listener, a real outside-write path, a planted credential-store
//! sentinel and a planted forbidden env key. Nothing about the probe
//! helper is mocked.

use r_code_harness_protocol::rpc::{error_code, RpcId, RpcRequest};
use r_code_harness_protocol::{HostService, RunIdentity};
use r_code_kernel::ports::RunGuard;
use r_code_kernel::testing::{
    FakeModelService, FakeProcessService, FakeToolService, MemoryJournal,
};
use r_code_runtime::plugins::router::{supported_requested_services, RouterServiceAvailability};
use r_code_runtime::plugins::{HostRouter, IgnoreQuestions};
use r_code_runtime::services::sandbox::{
    current_platform_material, platform_activation_gate, status_from_probes,
    DeterministicFakeSandboxBackend, SafetyActivation, SafetyBinaryIdentity, SafetyProbeResult,
    SafetyReportMaterial, SafetyStatus, SandboxBackend, SandboxNetworkClass, SandboxProbeId,
    SandboxProfileMaterial, SAFETY_CAPABILITY_WRITE_EXECUTION,
};
use r_code_store::v1::V1Store;
use serde_json::{json, Value};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

const PROBE_BIN: &str = env!("CARGO_BIN_EXE_r-code-safety-probe");

// P13.1 — profile material ---------------------------------------------------

fn valid_profile() -> SandboxProfileMaterial {
    SandboxProfileMaterial {
        read_roots: vec!["C:/ws/src".into(), "/usr/share/doc".into()],
        write_roots: vec!["C:/ws/out".into()],
        scratch_root: "C:/ws/scratch".into(),
        toolchain_roots: vec!["C:/toolchains".into()],
        cache_roots: vec!["C:/cache".into()],
        git_hidden: true,
        inherited_fds: vec![0, 1, 2],
        environment_allowlist: vec!["PATH".into(), "HOME".into()],
        network: SandboxNetworkClass::Offline,
    }
}

#[test]
fn profile_policy_digest_is_deterministic_and_validation_is_strict() {
    let base = valid_profile();
    assert_eq!(base.validate(), Ok(()));
    // Determinism: identical material yields the identical policy digest.
    assert_eq!(base.policy_digest(), base.clone().policy_digest());

    // Any field change must move the digest — the profile and the policy it
    // describes change identity together (P13.3 seam).
    let variants: Vec<(&str, SandboxProfileMaterial)> = vec![
        (
            "read roots",
            SandboxProfileMaterial {
                read_roots: vec![
                    "C:/ws/src".into(),
                    "/usr/share/doc".into(),
                    "C:/other".into(),
                ],
                ..base.clone()
            },
        ),
        (
            "write roots",
            SandboxProfileMaterial {
                write_roots: vec!["C:/other-out".into()],
                ..base.clone()
            },
        ),
        (
            "scratch root",
            SandboxProfileMaterial {
                scratch_root: "C:/other-scratch".into(),
                ..base.clone()
            },
        ),
        (
            "toolchain roots",
            SandboxProfileMaterial {
                toolchain_roots: vec!["C:/other-tools".into()],
                ..base.clone()
            },
        ),
        (
            "cache roots",
            SandboxProfileMaterial {
                cache_roots: vec!["C:/other-cache".into()],
                ..base.clone()
            },
        ),
        (
            "git hidden",
            SandboxProfileMaterial {
                git_hidden: false,
                ..base.clone()
            },
        ),
        (
            "inherited fds",
            SandboxProfileMaterial {
                inherited_fds: vec![0, 1, 2, 3],
                ..base.clone()
            },
        ),
        (
            "environment allowlist",
            SandboxProfileMaterial {
                environment_allowlist: vec!["PATH".into(), "HOME".into(), "EXTRA".into()],
                ..base.clone()
            },
        ),
        (
            "network class",
            SandboxProfileMaterial {
                network: SandboxNetworkClass::PublicInternetClient,
                ..base.clone()
            },
        ),
    ];
    for (label, variant) in &variants {
        assert_ne!(
            variant.policy_digest(),
            base.policy_digest(),
            "{label} must change the policy digest"
        );
    }

    let invalid: Vec<(&str, SandboxProfileMaterial)> = vec![
        (
            "relative read root",
            SandboxProfileMaterial {
                read_roots: vec!["relative/root".into()],
                ..base.clone()
            },
        ),
        (
            "empty read roots",
            SandboxProfileMaterial {
                read_roots: Vec::new(),
                ..base.clone()
            },
        ),
        (
            "duplicate read roots",
            SandboxProfileMaterial {
                read_roots: vec!["C:/a".into(), "C:/a".into()],
                ..base.clone()
            },
        ),
        (
            "relative write root",
            SandboxProfileMaterial {
                write_roots: vec!["out".into()],
                ..base.clone()
            },
        ),
        (
            "empty write roots",
            SandboxProfileMaterial {
                write_roots: Vec::new(),
                ..base.clone()
            },
        ),
        (
            "duplicate write roots",
            SandboxProfileMaterial {
                write_roots: vec!["C:/a".into(), "C:/a".into()],
                ..base.clone()
            },
        ),
        (
            "empty scratch root",
            SandboxProfileMaterial {
                scratch_root: " ".into(),
                ..base.clone()
            },
        ),
        (
            "relative toolchain root",
            SandboxProfileMaterial {
                toolchain_roots: vec!["tools".into()],
                ..base.clone()
            },
        ),
        (
            "relative cache root",
            SandboxProfileMaterial {
                cache_roots: vec!["cache".into()],
                ..base.clone()
            },
        ),
        // INV-06: a profile claiming .git visibility is not a sandbox.
        (
            "git visible",
            SandboxProfileMaterial {
                git_hidden: false,
                ..base.clone()
            },
        ),
        (
            "duplicate inherited fds",
            SandboxProfileMaterial {
                inherited_fds: vec![0, 1, 2, 0],
                ..base.clone()
            },
        ),
        (
            "lowercase env key",
            SandboxProfileMaterial {
                environment_allowlist: vec!["PATH".into(), "home".into()],
                ..base.clone()
            },
        ),
        (
            "duplicate env keys",
            SandboxProfileMaterial {
                environment_allowlist: vec!["PATH".into(), "PATH".into()],
                ..base.clone()
            },
        ),
    ];
    for (label, variant) in &invalid {
        assert!(variant.validate().is_err(), "{label} validated");
    }
}

#[test]
fn network_classes_are_three_distinct_values_with_distinct_tags() {
    let classes = [
        SandboxNetworkClass::Offline,
        SandboxNetworkClass::PublicInternetClient,
        SandboxNetworkClass::HostNetwork,
    ];
    for (index, class) in classes.iter().enumerate() {
        for other in classes.iter().skip(index + 1) {
            assert_ne!(class, other, "network classes are not interchangeable");
        }
    }
    assert_eq!(serde_json::to_value(classes[0]).unwrap(), json!("offline"));
    assert_eq!(
        serde_json::to_value(classes[1]).unwrap(),
        json!("public-internet-client")
    );
    assert_eq!(
        serde_json::to_value(classes[2]).unwrap(),
        json!("host-network")
    );
}

// P13.2 — the real helper protocol --------------------------------------------

/// Spawn the real helper with one request on stdin and reap it. Returns
/// (exit code, raw stdout).
fn run_helper(request: &Value, envs: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut command = Command::new(PROBE_BIN);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn r-code-safety-probe");
    {
        let mut stdin = child.stdin.take().expect("helper stdin");
        stdin
            .write_all(request.to_string().as_bytes())
            .expect("write helper request");
    }
    let output = child.wait_with_output().expect("reap helper");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    )
}

/// All ten strict target fields the helper protocol requires.
struct ProbeTargets {
    outside_write_path: PathBuf,
    dot_git_dir: PathBuf,
    network_port: u16,
    credential_service: String,
    credential_user: &'static str,
    device_path: PathBuf,
    ipc_path: PathBuf,
    child_command: &'static str,
    forbidden_env: Vec<String>,
}

impl ProbeTargets {
    /// A minimal strict target set rooted at one temp directory.
    fn in_temp(root: &Path) -> Self {
        Self {
            outside_write_path: root.join("outside").join("x.txt"),
            dot_git_dir: root.join("repo.git"),
            network_port: 1,
            credential_service: "r-code-s13-probe-protocol".into(),
            credential_user: "sentinel",
            device_path: root.join("device-node"),
            ipc_path: root.join("ipc-endpoint"),
            child_command: "true",
            forbidden_env: vec!["FORBIDDEN_KEY".into()],
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "outsideWritePath": self.outside_write_path.to_string_lossy(),
            "dotGitDir": self.dot_git_dir.to_string_lossy(),
            "networkHost": "127.0.0.1",
            "networkPort": self.network_port,
            "credentialService": self.credential_service,
            "credentialUser": self.credential_user,
            "devicePath": self.device_path.to_string_lossy(),
            "ipcPath": self.ipc_path.to_string_lossy(),
            "childCommand": self.child_command,
            "forbiddenEnv": self.forbidden_env,
        })
    }
}

fn all_probe_ids() -> Vec<String> {
    SandboxProbeId::ALL
        .iter()
        .map(|probe| probe.as_str().to_string())
        .collect()
}

fn parse_reply(stdout: &str) -> Value {
    assert_eq!(
        stdout.lines().count(),
        1,
        "the helper writes exactly one JSON document on stdout"
    );
    serde_json::from_str(stdout).expect("helper stdout is valid JSON")
}

fn outcome_of<'a>(reply: &'a Value, probe: &str) -> (&'a str, &'a str) {
    let entry = reply["results"]
        .as_array()
        .unwrap_or_else(|| panic!("no results array in {reply}"))
        .iter()
        .find(|entry| entry["probe"] == json!(probe))
        .unwrap_or_else(|| panic!("no result for probe {probe} in {reply}"));
    (
        entry["outcome"].as_str().unwrap_or(""),
        entry["detail"].as_str().unwrap_or(""),
    )
}

#[test]
fn real_helper_reports_the_unsandboxed_polarity_against_live_targets() {
    let temp = tempfile::tempdir().unwrap();
    let outside_dir = temp.path().join("outside");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let outside_write_path = outside_dir.join("leak.txt");
    let dot_git_dir = temp.path().join("repo.git");
    std::fs::create_dir_all(&dot_git_dir).unwrap();
    std::fs::write(dot_git_dir.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    // Non-existent device and IPC endpoints: unsandboxed access still fails,
    // so these two probes must report a consistent denial.
    let device_path = temp.path().join("device-node");
    let ipc_path = temp.path().join("ipc-endpoint");

    // REAL live listener: an unsandboxed helper genuinely connects.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let network_port = listener.local_addr().unwrap().port();

    let sentinel = format!("R_CODE_S13_ENV_SENTINEL_{}", std::process::id());
    let credential_service = format!("r-code-s13-probe-{}", std::process::id());
    let credential_user = "sentinel";
    let planted = match keyring::Entry::new(&credential_service, credential_user) {
        Ok(entry) => entry.set_password("s13-secret").is_ok(),
        Err(_) => false,
    };

    let targets = ProbeTargets {
        outside_write_path: outside_write_path.clone(),
        dot_git_dir,
        network_port,
        credential_service: credential_service.clone(),
        credential_user,
        device_path,
        ipc_path,
        child_command: if cfg!(windows) { "cmd" } else { "true" },
        forbidden_env: vec![sentinel.clone()],
    };
    let request = json!({
        "version": 1,
        "probes": all_probe_ids(),
        "targets": targets.to_json(),
    });
    let (code, stdout) = run_helper(&request, &[(sentinel.as_str(), "leaked-value")]);
    assert_eq!(code, Some(0), "a valid suite run exits 0; stdout: {stdout}");
    let reply = parse_reply(&stdout);
    assert_eq!(reply["version"], json!(1));
    assert_eq!(
        reply["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["probe"].clone())
            .collect::<Vec<_>>(),
        all_probe_ids()
            .into_iter()
            .map(|probe| json!(probe))
            .collect::<Vec<_>>(),
        "results come back one per requested probe, in order"
    );

    // Unsandboxed polarity: every forbidden operation this process can
    // really perform reports allowed — meaning an unsandboxed suite run can
    // never satisfy activation.
    let (outcome, _detail) = outcome_of(&reply, "write-outside-allowlist");
    assert_eq!(outcome, "allowed");
    let written = std::fs::read_to_string(&outside_write_path)
        .expect("the forbidden write actually landed on disk");
    assert_eq!(written, "r-code-safety-probe sentinel\n");

    let (outcome, detail) = outcome_of(&reply, "environment-leakage");
    assert_eq!(outcome, "allowed");
    assert!(
        detail.contains(&sentinel),
        "leak detail names the sentinel key: {detail}"
    );

    let (outcome, _detail) = outcome_of(&reply, "network-outbound");
    assert_eq!(outcome, "allowed");
    listener
        .set_nonblocking(true)
        .expect("listener nonblocking");
    let mut accepted = false;
    for _ in 0..80 {
        if listener.accept().is_ok() {
            accepted = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(accepted, "the live listener saw the forbidden connection");

    let (outcome, _detail) = outcome_of(&reply, "child-escape");
    assert_eq!(outcome, "allowed");

    let (outcome, _detail) = outcome_of(&reply, "dot-git-access");
    assert_eq!(outcome, "allowed");

    // The planted credential-store sentinel is genuinely readable when the
    // OS credential store is available, so unsandboxed reports allowed.
    let (outcome, _detail) = outcome_of(&reply, "registry-or-keychain");
    if planted {
        assert_eq!(outcome, "allowed");
    } else {
        assert_eq!(outcome, "denied", "consistent denial without a store");
    }
    if planted {
        if let Ok(entry) = keyring::Entry::new(&credential_service, credential_user) {
            let _ = entry.delete_credential();
        }
    }

    // Endpoints that do not exist anywhere report a plain consistent denial.
    assert_eq!(outcome_of(&reply, "device-access").0, "denied");
    assert_eq!(outcome_of(&reply, "ipc-access").0, "denied");
}

#[test]
fn helper_protocol_is_versioned_strict_and_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let targets = ProbeTargets::in_temp(temp.path()).to_json();

    // Version mismatch is a protocol violation.
    let (code, stdout) = run_helper(
        &json!({"version": 2, "probes": [], "targets": targets}),
        &[],
    );
    assert_eq!(code, Some(2));
    let error = parse_reply(&stdout);
    assert_eq!(error["version"], json!(1));
    assert!(error["error"]
        .as_str()
        .is_some_and(|reason| !reason.is_empty()));

    // Unknown probe ids are refused, not guessed.
    let (code, stdout) = run_helper(
        &json!({
            "version": 1,
            "probes": ["write-outside-allowlist", "not-a-probe"],
            "targets": targets,
        }),
        &[],
    );
    assert_eq!(code, Some(2));
    let error = parse_reply(&stdout);
    assert!(error["error"]
        .as_str()
        .is_some_and(|reason| reason.contains("unknown probe id")));

    // deny_unknown_fields: an extra target field is refused.
    let mut with_extra = targets.clone();
    with_extra["surprise"] = json!(true);
    let (code, _) = run_helper(
        &json!({"version": 1, "probes": [], "targets": with_extra}),
        &[],
    );
    assert_eq!(code, Some(2));

    // A missing required target field is refused.
    let mut missing_field = targets.clone();
    missing_field.as_object_mut().unwrap().remove("ipcPath");
    let (code, _) = run_helper(
        &json!({"version": 1, "probes": [], "targets": missing_field}),
        &[],
    );
    assert_eq!(code, Some(2));

    // An empty probe list is a valid run with an empty result set.
    let (code, stdout) = run_helper(
        &json!({"version": 1, "probes": [], "targets": targets}),
        &[],
    );
    assert_eq!(code, Some(0));
    let reply = parse_reply(&stdout);
    assert_eq!(reply["version"], json!(1));
    assert_eq!(reply["results"], json!([]));
}

// P13.3 — status derivation and the fake backend seam -------------------------

fn result_for(probe: SandboxProbeId, passed: bool) -> SafetyProbeResult {
    SafetyProbeResult {
        probe_id: probe.as_str().to_string(),
        passed,
        detail_digest: format!("detail-{}", probe.as_str()),
    }
}

fn safe_disabled_reason(status: &SafetyStatus) -> &str {
    match status {
        SafetyStatus::SafeDisabled { reason } => reason,
        other => panic!("expected SafeDisabled, got {other:?}"),
    }
}

#[test]
fn status_from_probes_requires_exactly_once_passing_and_a_real_backend() {
    let perfect: Vec<SafetyProbeResult> = SandboxProbeId::ALL
        .iter()
        .map(|probe| result_for(*probe, true))
        .collect();

    // Without a backend nothing can be claimed, whatever the results say.
    match status_from_probes(false, SandboxProbeId::ALL, &perfect) {
        SafetyStatus::Unsupported { reason } => {
            assert!(reason.contains("no native sandbox backend"), "{reason}")
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }

    // All eight required probes present exactly once, all passing.
    assert_eq!(
        status_from_probes(true, SandboxProbeId::ALL, &perfect),
        SafetyStatus::Activated
    );

    // Omitted probes can never satisfy activation — silence names the id.
    for probe in SandboxProbeId::ALL {
        let omitted: Vec<SafetyProbeResult> = SandboxProbeId::ALL
            .iter()
            .filter(|candidate| **candidate != *probe)
            .map(|candidate| result_for(*candidate, true))
            .collect();
        let status = status_from_probes(true, SandboxProbeId::ALL, &omitted);
        let reason = safe_disabled_reason(&status);
        assert!(
            reason.contains(probe.as_str()),
            "omission of {} must be named: {reason}",
            probe.as_str()
        );
    }

    // Duplicated results are ambiguous and keep the capability disabled.
    let mut duplicated = perfect.clone();
    duplicated.push(result_for(SandboxProbeId::NetworkOutbound, true));
    let status = status_from_probes(true, SandboxProbeId::ALL, &duplicated);
    let reason = safe_disabled_reason(&status);
    assert!(reason.contains("network-outbound"), "{reason}");

    // A single failing probe disables with its id.
    let mut one_failed = perfect.clone();
    one_failed[SandboxProbeId::ALL.len() - 1] =
        result_for(SandboxProbeId::EnvironmentLeakage, false);
    let status = status_from_probes(true, SandboxProbeId::ALL, &one_failed);
    let reason = safe_disabled_reason(&status);
    assert!(reason.contains("environment-leakage"), "{reason}");

    // No results at all names every required probe.
    let status = status_from_probes(true, SandboxProbeId::ALL, &[]);
    let reason = safe_disabled_reason(&status);
    for probe in SandboxProbeId::ALL {
        assert!(reason.contains(probe.as_str()), "{reason}");
    }
}

#[tokio::test]
async fn fake_backend_seam_drives_activation_and_honors_the_policy_digest() {
    let profile = valid_profile();
    let helper = SafetyBinaryIdentity {
        path: "probe-helper".into(),
        sha256: "digest".into(),
    };

    let mut backend = DeterministicFakeSandboxBackend::new();
    for probe in SandboxProbeId::ALL {
        backend.set_outcome(*probe, true);
    }
    let results = backend
        .run_probes(&helper, &profile, SandboxProbeId::ALL)
        .await
        .expect("fake backend runs");
    assert_eq!(
        results
            .iter()
            .map(|result| result.probe_id.as_str())
            .collect::<Vec<_>>(),
        SandboxProbeId::ALL
            .iter()
            .map(|probe| probe.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        status_from_probes(true, SandboxProbeId::ALL, &results),
        SafetyStatus::Activated
    );

    backend.set_outcome(SandboxProbeId::NetworkOutbound, false);
    let results = backend
        .run_probes(&helper, &profile, SandboxProbeId::ALL)
        .await
        .unwrap();
    let status = status_from_probes(true, SandboxProbeId::ALL, &results);
    let reason = safe_disabled_reason(&status);
    assert!(reason.contains("network-outbound"), "{reason}");

    // The digest override binds a chosen policy into report material.
    assert_eq!(
        backend.policy_digest(&profile).unwrap(),
        profile.policy_digest()
    );
    backend.set_policy_digest("override-digest".into());
    assert_eq!(backend.policy_digest(&profile).unwrap(), "override-digest");

    // Requested order is preserved for subsets too.
    let subset = [
        SandboxProbeId::EnvironmentLeakage,
        SandboxProbeId::WriteOutsideAllowlist,
        SandboxProbeId::IpcAccess,
    ];
    let results = backend
        .run_probes(&helper, &profile, &subset)
        .await
        .unwrap();
    assert_eq!(
        results
            .iter()
            .map(|result| result.probe_id.as_str())
            .collect::<Vec<_>>(),
        [
            "environment-leakage",
            "write-outside-allowlist",
            "ipc-access"
        ]
    );
}

// P13.3 — the platform discovery gate over a real store ------------------------

#[test]
fn platform_gate_persists_the_unsupported_head_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&temp.path().join("store.db")).unwrap();
    let boot = "s13-boot-identity";

    let assert_not_activated_status_unsupported = |gate: &SafetyActivation| match gate {
        SafetyActivation::NotActivated { reason, status } => {
            assert_eq!(*reason, "status-unsupported");
            assert_eq!(status.as_deref(), Some("unsupported"));
        }
        SafetyActivation::Activated { .. } => {
            panic!("no platform can activate in this wave")
        }
    };

    assert_not_activated_status_unsupported(&platform_activation_gate(&store, boot));

    // The gate PERSISTED the head report with diagnostics intact.
    let record = store
        .current_safety_report(SAFETY_CAPABILITY_WRITE_EXECUTION)
        .unwrap()
        .expect("the gate persisted the platform report");
    let material: SafetyReportMaterial =
        serde_json::from_str(&record.material_json).expect("material round-trips");
    match &material.status {
        SafetyStatus::Unsupported { reason } => {
            assert!(reason.contains("no native sandbox backend"), "{reason}")
        }
        other => panic!("expected Unsupported head, got {other:?}"),
    }
    assert_eq!(material.capability, SAFETY_CAPABILITY_WRITE_EXECUTION);
    assert_eq!(material.boot_identity, boot);
    assert_eq!(material.os, std::env::consts::OS);
    assert_eq!(material.arch, std::env::consts::ARCH);
    assert_eq!(record.report_id, material.report_id());
    assert_eq!(record.material_digest, material.digest());
    assert_eq!(
        current_platform_material(boot).report_id(),
        record.report_id
    );
    assert_eq!(
        store
            .safety_report_history(SAFETY_CAPABILITY_WRITE_EXECUTION)
            .unwrap()
            .len(),
        1
    );

    // Idempotent: the second call re-evaluates the same report, not a new one.
    assert_not_activated_status_unsupported(&platform_activation_gate(&store, boot));
    assert_eq!(
        store
            .current_safety_report(SAFETY_CAPABILITY_WRITE_EXECUTION)
            .unwrap()
            .unwrap()
            .report_id,
        record.report_id
    );
    assert_eq!(
        store
            .safety_report_history(SAFETY_CAPABILITY_WRITE_EXECUTION)
            .unwrap()
            .len(),
        1
    );
}

// Router gate over guessed calls ------------------------------------------------

fn s13_identity() -> RunIdentity {
    RunIdentity {
        task_id: "task-s13".into(),
        branch_id: "branch-s13".into(),
        run_id: "run-s13".into(),
        attempt_id: "attempt-s13".into(),
        generation: 1,
    }
}

#[tokio::test]
async fn router_gate_hides_effect_services_until_activated_and_denies_guessed_open() {
    let requested = [
        HostService::ProcessOpen,
        HostService::ProcessRead,
        HostService::ProcessWrite,
        HostService::ProcessClose,
        HostService::VerificationRun,
        HostService::ChildrenSpawn,
        HostService::ChildrenWait,
        HostService::ChildrenCancel,
        HostService::PlanUpdate,
        HostService::ToolsList,
    ];

    let closed = RouterServiceAvailability {
        children: false,
        tools: true,
        ..Default::default()
    };
    let grants = supported_requested_services(&requested, closed);
    assert_eq!(grants, vec![HostService::ToolsList]);

    let opened = RouterServiceAvailability {
        children: false,
        tools: true,
        sandbox_activated: true,
        ..Default::default()
    };
    let open_grants = supported_requested_services(&requested, opened);
    for granted in [
        HostService::ProcessOpen,
        HostService::ProcessRead,
        HostService::ProcessWrite,
        HostService::ProcessClose,
        HostService::VerificationRun,
        HostService::ToolsList,
    ] {
        assert!(open_grants.contains(&granted), "{granted:?} not granted");
    }
    for still_hidden in [
        HostService::ChildrenSpawn,
        HostService::ChildrenWait,
        HostService::ChildrenCancel,
        HostService::PlanUpdate,
    ] {
        assert!(
            !open_grants.contains(&still_hidden),
            "{still_hidden:?} must stay filtered (O-GATE scope)"
        );
    }

    // The closed grant list feeds the REAL router dispatch: a guessed
    // host.process.open is refused at the grant gate with a protocol
    // violation, before any process service runs.
    let router = HostRouter::new(
        s13_identity(),
        RunGuard::new("run-s13", 1),
        grants,
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        Arc::new(MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    );
    let denied = router
        .handle_request(RpcRequest {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(1),
            method: "host.process.open".into(),
            params: Some(json!({"profile": "read-only", "arguments": []})),
        })
        .await
        .expect_err("a guessed host.process.open must be denied without activation");
    assert_eq!(denied.code, error_code::PROTOCOL_VIOLATION);
    assert!(
        denied.message.contains("not part of this run's grants"),
        "{}",
        denied.message
    );
}

// Source pins — honesty guarantees the code cannot silently lose ---------------

fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {dir:?}: {error}"));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn status_derives_only_from_probe_results_and_the_fake_backend_is_test_only() {
    let source = include_str!("../src/services/sandbox.rs");
    // Pin the signature: the decision consumes probe results (and only
    // probe results) — no os/arch string input exists to infer from.
    let signature = "pub fn status_from_probes(\n    backend_available: bool,\n    required: &[SandboxProbeId],\n    results: &[SafetyProbeResult],\n) -> SafetyStatus {";
    let start = source
        .find(signature)
        .expect("status_from_probes signature is pinned");
    let end = source[start..]
        .find("\n/// Sandbox backend contract")
        .map(|offset| start + offset)
        .expect("the backend contract follows status_from_probes");
    let body = &source[start..end];
    assert!(
        !body.contains("env::consts"),
        "status_from_probes must not consult the platform"
    );
    assert!(
        !body.contains("std::env"),
        "status_from_probes must not consult the environment"
    );

    // The deterministic fake is a test seam: no production source under
    // src/ constructs or even mentions it outside its own definition.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    collect_rust_sources(&src, &mut sources);
    let mut offenders = Vec::new();
    for path in &sources {
        let content = std::fs::read_to_string(path).unwrap();
        if content.contains("DeterministicFakeSandboxBackend") {
            offenders.push(
                path.strip_prefix(&src)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    assert_eq!(
        offenders,
        vec!["services/sandbox.rs".to_string()],
        "the fake backend may only exist where it is defined"
    );
}
