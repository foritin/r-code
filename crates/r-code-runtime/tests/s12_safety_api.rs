//! P12 — activation predicate, regeneration seam and read-only, redacted
//! safety report diagnostics over a real daemon (s02 RPC discipline).

use r_code_client::{ClientError, DaemonClient};
use r_code_runtime::process_guard::BootIdentity;
use r_code_runtime::remote::capabilities::{required_capability, Capability};
use r_code_runtime::services::sandbox::{
    current_platform_material, evaluate_safety_activation, regenerate_safety_report,
    SafetyActivation, SafetyBinaryIdentity, SafetyProbeResult, SafetyReportMaterial, SafetyStatus,
    SAFETY_REPORT_MATERIAL_VERSION,
};
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::safety::{SafetyReportRecord, SafetyReportStatus};
use r_code_store::v1::V1Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const SERVICE: &str = env!("CARGO_BIN_EXE_r-code-service");
const BOOT_A: &str = "windows:01234567-89ab-4cde-8f01-23456789abcd";
const BOOT_B: &str = "windows:01234567-89ab-4cde-8f01-23456789abce";
const CAPABILITY: &str = "write-execution";
/// Recognizable private path planted in the helper/executable identity: the
/// wire projection must never leak it.
const LEAK_PATH: &str = "C:/private/s12-leak";

fn hex64(seed: char) -> String {
    std::iter::repeat_n(seed, 64).collect()
}

fn digest_of(value: &Value) -> String {
    r_code_harness_protocol::canonical_input_hash(value)
}

fn binary_identity(path: &str) -> SafetyBinaryIdentity {
    SafetyBinaryIdentity {
        path: path.to_string(),
        sha256: digest_of(&json!({"path": path})),
    }
}

fn probe(probe_id: &str, passed: bool) -> SafetyProbeResult {
    SafetyProbeResult {
        probe_id: probe_id.to_string(),
        passed,
        detail_digest: digest_of(&json!({"probe": probe_id})),
    }
}

fn base_material() -> SafetyReportMaterial {
    SafetyReportMaterial {
        material_version: SAFETY_REPORT_MATERIAL_VERSION,
        capability: CAPABILITY.to_string(),
        os: "windows".to_string(),
        arch: "x86_64".to_string(),
        boot_identity: BOOT_A.to_string(),
        backend: "supervisor".to_string(),
        backend_policy_digest: hex64('1'),
        helper: Some(binary_identity(&format!("{LEAK_PATH}/helper.exe"))),
        executable: Some(binary_identity(&format!("{LEAK_PATH}/writer.exe"))),
        probes: vec![probe("probe-write", true), probe("probe-job", true)],
        status: SafetyStatus::Activated,
        generated_at_ms: 1_000,
    }
}

fn coarse(status: &SafetyStatus) -> SafetyReportStatus {
    match status {
        SafetyStatus::Unsupported { .. } => SafetyReportStatus::Unsupported,
        SafetyStatus::SafeDisabled { .. } => SafetyReportStatus::SafeDisabled,
        SafetyStatus::Activated => SafetyReportStatus::Activated,
    }
}

fn record_for(material: &SafetyReportMaterial) -> SafetyReportRecord {
    SafetyReportRecord {
        report_id: material.report_id(),
        capability: material.capability.clone(),
        material_digest: material.digest(),
        status: coarse(&material.status),
        material_json: serde_json::to_string(material).unwrap(),
        created_at_ms: material.generated_at_ms,
    }
}

fn assert_not_activated(activation: &SafetyActivation, expected: &str) {
    match activation {
        SafetyActivation::NotActivated { reason, .. } => assert_eq!(*reason, expected),
        SafetyActivation::Activated { .. } => panic!("expected NotActivated({expected})"),
    }
}

#[test]
fn predicate_is_fail_closed_for_missing_tampered_foreign_and_stale_reports() {
    let current = base_material();
    let persisted = record_for(&current);

    assert_not_activated(
        &evaluate_safety_activation(None, &current),
        "report-missing",
    );

    let mut unparseable = persisted.clone();
    unparseable.material_json = "{not-json".to_string();
    assert_not_activated(
        &evaluate_safety_activation(Some(&unparseable), &current),
        "material-unparseable",
    );

    // Tampering with material_json (identity edited without the digest) is
    // caught by the content address.
    for edit in ["boot_identity", "backend"] {
        let mut tampered = persisted.clone();
        let mut edited: Value = serde_json::from_str(&persisted.material_json).unwrap();
        edited[edit] = json!(BOOT_B);
        tampered.material_json = edited.to_string();
        assert_not_activated(
            &evaluate_safety_activation(Some(&tampered), &current),
            "digest-mismatch",
        );
    }
    let mut forged_status = persisted.clone();
    let mut edited: Value = serde_json::from_str(&persisted.material_json).unwrap();
    edited["status"] = json!({"kind": "safe-disabled", "reason": "forged"});
    forged_status.material_json = edited.to_string();
    assert_not_activated(
        &evaluate_safety_activation(Some(&forged_status), &current),
        "digest-mismatch",
    );

    // Consistent digest but a forged report id (or capability column) never
    // poses as its neighbour.
    let mut wrong_id = persisted.clone();
    wrong_id.report_id = format!("safety-{}", hex64('9'));
    assert_not_activated(
        &evaluate_safety_activation(Some(&wrong_id), &current),
        "report-id-mismatch",
    );
    let mut wrong_capability = persisted.clone();
    wrong_capability.capability = "other-capability".to_string();
    assert_not_activated(
        &evaluate_safety_activation(Some(&wrong_capability), &current),
        "capability-mismatch",
    );

    // Ordered staleness: the current runtime drifting one identity input at a
    // time yields the specific reason.
    let mut drifted = current.clone();
    drifted.boot_identity = BOOT_B.to_string();
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "boot-identity-changed",
    );
    for os in ["linux", "macos"] {
        let mut drifted = current.clone();
        drifted.os = os.to_string();
        assert_not_activated(
            &evaluate_safety_activation(Some(&persisted), &drifted),
            "platform-mismatch",
        );
    }
    let mut drifted = current.clone();
    drifted.arch = "aarch64".to_string();
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "platform-mismatch",
    );
    let mut drifted = current.clone();
    drifted.backend = "other-backend".to_string();
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "backend-changed",
    );
    let mut drifted = current.clone();
    drifted.backend_policy_digest = hex64('2');
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "policy-changed",
    );
    let mut drifted = current.clone();
    drifted.helper = Some(binary_identity("C:/other/helper.exe"));
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "helper-changed",
    );
    let mut drifted = current.clone();
    drifted.helper = None;
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "helper-changed",
    );
    let mut drifted = current.clone();
    drifted.executable = Some(binary_identity("C:/other/writer.exe"));
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "executable-changed",
    );
    let mut drifted = current.clone();
    drifted.executable = None;
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "executable-changed",
    );
    let mut drifted = current.clone();
    drifted.probes = vec![probe("probe-write", true)];
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "probes-changed",
    );
    let mut drifted = current.clone();
    drifted.probes = vec![probe("probe-write", true), probe("probe-renamed", true)];
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "probes-changed",
    );

    // The reason order is stable: boot beats platform, platform beats backend.
    let mut drifted = current.clone();
    drifted.boot_identity = BOOT_B.to_string();
    drifted.os = "linux".to_string();
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "boot-identity-changed",
    );
    let mut drifted = current.clone();
    drifted.os = "linux".to_string();
    drifted.backend = "other-backend".to_string();
    assert_not_activated(
        &evaluate_safety_activation(Some(&persisted), &drifted),
        "platform-mismatch",
    );
}

#[test]
fn safe_disabled_and_unsupported_pass_portability_but_never_activate() {
    for (label, status, expected_status) in [
        (
            "safe-disabled",
            SafetyStatus::SafeDisabled {
                reason: "macos-no-kernel-tree-containment".to_string(),
            },
            "status-safe-disabled",
        ),
        (
            "unsupported",
            SafetyStatus::Unsupported {
                reason: "not-implemented".to_string(),
            },
            "status-unsupported",
        ),
    ] {
        let mut material = base_material();
        material.status = status;
        let persisted = record_for(&material);
        let activation = evaluate_safety_activation(Some(&persisted), &material);
        let SafetyActivation::NotActivated { reason, status } = activation else {
            panic!("{label} activated");
        };
        // The reason is the status arm, not a staleness arm: identity and
        // portability checks passed, activation did not (P12.3).
        assert_eq!(reason, expected_status);
        assert_eq!(
            status.as_deref(),
            Some(expected_status.trim_start_matches("status-"))
        );

        // The same disabled report on a foreign boot still fails closed with
        // the specific staleness reason.
        let mut foreign = material.clone();
        foreign.boot_identity = BOOT_B.to_string();
        assert_not_activated(
            &evaluate_safety_activation(Some(&persisted), &foreign),
            "boot-identity-changed",
        );
    }
}

#[test]
fn activated_requires_exact_material_and_complete_passing_probes() {
    let current = base_material();
    let persisted = record_for(&current);
    match evaluate_safety_activation(Some(&persisted), &current) {
        SafetyActivation::Activated { report_id } => assert_eq!(report_id, persisted.report_id),
        other => panic!("expected activation, got {other:?}"),
    }

    // generated_at_ms is excluded from the identity: a later regeneration of
    // the same material still activates against the persisted report.
    let mut later = current.clone();
    later.generated_at_ms += 60_000;
    let mut later_record = persisted.clone();
    let mut edited: Value = serde_json::from_str(&persisted.material_json).unwrap();
    edited["generated_at_ms"] = json!(later.generated_at_ms);
    later_record.material_json = edited.to_string();
    assert!(matches!(
        evaluate_safety_activation(Some(&later_record), &later),
        SafetyActivation::Activated { .. }
    ));

    // A hollow activation (no probe evidence) never activates.
    let mut hollow = base_material();
    hollow.probes = Vec::new();
    let hollow_record = record_for(&hollow);
    assert_not_activated(
        &evaluate_safety_activation(Some(&hollow_record), &hollow),
        "probes-incomplete",
    );

    // A failing probe never activates even when identity matches exactly.
    let mut failing = base_material();
    failing.probes = vec![probe("probe-write", true), probe("probe-job", false)];
    let failing_record = record_for(&failing);
    assert_not_activated(
        &evaluate_safety_activation(Some(&failing_record), &failing),
        "probes-incomplete",
    );
}

#[test]
fn material_validate_rejects_incomplete_material() {
    assert_eq!(base_material().validate(), Ok(()));

    let mut variants: Vec<(&str, SafetyReportMaterial)> = Vec::new();
    let mut empty_boot = base_material();
    empty_boot.boot_identity = " ".to_string();
    variants.push(("empty boot identity", empty_boot));
    let mut empty_backend = base_material();
    empty_backend.backend = String::new();
    variants.push(("empty backend", empty_backend));
    let mut empty_policy = base_material();
    empty_policy.backend_policy_digest = String::new();
    variants.push(("empty policy digest", empty_policy));
    let mut empty_os = base_material();
    empty_os.os = String::new();
    variants.push(("empty os", empty_os));
    let mut empty_arch = base_material();
    empty_arch.arch = " ".to_string();
    variants.push(("empty arch", empty_arch));
    let mut empty_capability = base_material();
    empty_capability.capability = String::new();
    variants.push(("empty capability", empty_capability));
    for version in [0, SAFETY_REPORT_MATERIAL_VERSION + 1] {
        let mut drifted = base_material();
        drifted.material_version = version;
        variants.push(("wrong material version", drifted));
    }
    let mut duplicate_probes = base_material();
    duplicate_probes.probes = vec![probe("probe-write", true), probe("probe-write", true)];
    variants.push(("duplicate probe ids", duplicate_probes));
    let mut empty_probe_id = base_material();
    empty_probe_id.probes = vec![probe("", true)];
    variants.push(("empty probe id", empty_probe_id));
    let mut blank_helper = base_material();
    blank_helper.helper = Some(SafetyBinaryIdentity {
        path: String::new(),
        sha256: hex64('3'),
    });
    variants.push(("blank helper path", blank_helper));
    let mut blank_executable = base_material();
    blank_executable.executable = Some(SafetyBinaryIdentity {
        path: "C:/bin/writer.exe".to_string(),
        sha256: " ".to_string(),
    });
    variants.push(("blank executable sha", blank_executable));

    for (label, variant) in &variants {
        assert!(variant.validate().is_err(), "{label} validated");
    }
}

#[test]
fn regenerate_is_idempotent_prunes_stale_rows_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();

    let mut material = base_material();
    material.status = SafetyStatus::SafeDisabled {
        reason: "windows-job-object-pending-p15".to_string(),
    };
    let first = regenerate_safety_report(&store, &material).unwrap();

    // Timestamp exclusion: the same identity regenerated later keeps the
    // report id and stays a single history row.
    let mut later = material.clone();
    later.generated_at_ms += 60_000;
    assert_eq!(later.report_id(), first);
    assert_eq!(regenerate_safety_report(&store, &later).unwrap(), first);
    assert_eq!(store.safety_report_history(CAPABILITY).unwrap().len(), 1);
    let current = store.current_safety_report(CAPABILITY).unwrap().unwrap();
    assert_eq!(current.report_id, first);
    assert_eq!(current.created_at_ms, later.generated_at_ms);

    // Identity change (boot) produces a new report id, prunes the stale row
    // and re-points the head.
    let mut changed = material.clone();
    changed.boot_identity = BOOT_B.to_string();
    let third = regenerate_safety_report(&store, &changed).unwrap();
    assert_ne!(third, first);
    assert_eq!(store.safety_report_history(CAPABILITY).unwrap().len(), 1);
    assert_eq!(
        store
            .current_safety_report(CAPABILITY)
            .unwrap()
            .unwrap()
            .report_id,
        third
    );
    let raw = Connection::open(&path).unwrap();
    let rows: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM safety_capability_reports",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(raw);
    assert_eq!(rows, 1);

    // Restart: reopening the same database keeps the current report.
    drop(store);
    let store = V1Store::open(&path).unwrap();
    assert_eq!(
        store
            .current_safety_report(CAPABILITY)
            .unwrap()
            .unwrap()
            .report_id,
        third
    );

    // The regeneration seam refuses incomplete material outright.
    let mut incomplete = material;
    incomplete.boot_identity = " ".to_string();
    assert!(regenerate_safety_report(&store, &incomplete).is_err());
}

fn profile_for(name: &str, root: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(root.join("root"))
            .with_ipc_name(name),
    )
    .unwrap()
}

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_daemon(profile: &RuntimeProfile) -> DaemonGuard {
    let child = Command::new(SERVICE)
        .arg("--profile")
        .arg("development")
        .arg("--data-root")
        .arg(profile.data_root())
        .arg("--ipc-name")
        .arg(profile.ipc_name().unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    DaemonGuard(child)
}

fn wait_for_owner(profile: &RuntimeProfile) -> r_code_client::DaemonInfo {
    for _ in 0..100 {
        if let Some(info) = r_code_client::read_owner_token(&profile.harness_v1_root()) {
            return info;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("daemon never wrote owner token")
}

async fn connect_ready(profile: &RuntimeProfile, token: &str) -> Result<DaemonClient, ClientError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            token,
            "s12-local-client",
        )
        .await
        {
            Err(ClientError::Unreachable(_)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            result => return result,
        }
    }
}

fn safety_row_counts(path: &Path) -> [i64; 2] {
    let connection = Connection::open(path).unwrap();
    let mut counts = [0; 2];
    for (index, table) in ["safety_capability_reports", "safety_report_heads"]
        .into_iter()
        .enumerate()
    {
        counts[index] = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
    }
    counts
}

fn database_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("store.db")
}

#[tokio::test]
async fn local_rpc_projects_redacted_diagnostics_strict_params_and_stays_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let ipc_name = format!("s12-safety-rpc-{}", std::process::id());
    let profile = profile_for(&ipc_name, temp.path());
    let store = V1Store::open(&profile.database_path()).unwrap();

    let current_boot = BootIdentity::current().unwrap();
    let mut material = base_material();
    material.boot_identity = current_boot.as_str().to_string();
    material.status = SafetyStatus::SafeDisabled {
        reason: "windows-job-object-pending-p15".to_string(),
    };
    regenerate_safety_report(&store, &material).unwrap();
    let mut foreign = base_material();
    foreign.capability = "network-execution".to_string();
    regenerate_safety_report(&store, &foreign).unwrap();
    drop(store);

    let _daemon = spawn_daemon(&profile);
    let owner = wait_for_owner(&profile);
    let mut client = connect_ready(&profile, &owner.token).await.unwrap();
    let before = safety_row_counts(&profile.database_path());

    let all = client
        .call("safety.report.get", json!({}))
        .await
        .expect("local diagnostic read");
    let views = all.as_array().unwrap();
    assert_eq!(views.len(), 2);
    assert_eq!(views[0]["capability"], json!("network-execution"));
    assert_eq!(views[1]["capability"], json!(CAPABILITY));
    for view in views {
        let mut keys: Vec<&str> = view
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "bootMatchesCurrent",
                "capability",
                "createdAtMs",
                "materialDigest",
                "reportRef",
                "status",
            ]
        );
    }
    // P24A: the daemon regenerates THIS boot's platform material at startup
    // (no activation), so the served head for the write-execution capability
    // is the fresh Unsupported material rather than the pre-seeded
    // SafeDisabled row; the foreign capability row below is untouched.
    let regenerated = current_platform_material(current_boot.as_str());
    let current_view = &views[1];
    assert_eq!(current_view["reportRef"], json!(regenerated.report_id()));
    assert_eq!(current_view["materialDigest"], json!(regenerated.digest()));
    assert_eq!(current_view["status"], json!("unsupported"));
    assert_eq!(current_view["bootMatchesCurrent"], json!(true));
    assert_eq!(views[0]["bootMatchesCurrent"], json!(false));

    let filtered = client
        .call("safety.report.get", json!({"capability": CAPABILITY}))
        .await
        .expect("filtered local diagnostic read");
    assert_eq!(filtered.as_array().unwrap().len(), 1);
    assert_eq!(filtered[0]["capability"], json!(CAPABILITY));

    // No raw material JSON and no identity/path leakage on the wire.
    for serialized in [all.to_string(), filtered.to_string()] {
        for secret in [
            LEAK_PATH,
            current_boot.as_str(),
            BOOT_A,
            BOOT_B,
            "material_json",
            "materialJson",
            "helper",
            "executable",
            "backend_policy_digest",
            "detail_digest",
            "boot_identity",
        ] {
            assert!(!serialized.contains(secret), "RPC leaked {secret:?}");
        }
    }

    // Strict parameter matrix: null, non-object and unknown keys are refused.
    for params in [
        Value::Null,
        json!("not-an-object"),
        json!([]),
        json!({"capability": null}),
        json!({"capability": ""}),
        json!({"capability": 42}),
        json!({"unknown": true}),
        json!({"capability": CAPABILITY, "extra": true}),
        json!({"regenerate": true}),
    ] {
        let error = client
            .call("safety.report.get", params)
            .await
            .expect_err("invalid params must be refused");
        assert!(matches!(error, ClientError::Command(_)));
    }
    // No mutating safety report method exists.
    for method in [
        "safety.report.put",
        "safety.report.regenerate",
        "safety.report.prune",
    ] {
        let error = client
            .call(method, json!({}))
            .await
            .expect_err("mutating safety method must not exist");
        assert!(matches!(error, ClientError::Command(_)));
    }
    // Strictly read-only: the row counts are unchanged after every call.
    assert_eq!(safety_row_counts(&profile.database_path()), before);

    // A forged owner token never reaches the route.
    let forged = match DaemonClient::connect(
        &profile.ipc_endpoint(),
        &profile.profile_id(),
        "forged-token",
        "s12-forged-client",
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("forged local token must fail"),
    };
    assert!(matches!(forged, ClientError::Handshake(_)));

    // Remote capability matrix: local-only, exactly like safety.quarantine.get.
    assert_eq!(required_capability("safety.report.get"), None);
    assert_eq!(
        required_capability("safety.report.get"),
        required_capability("safety.quarantine.get")
    );
    for capability in [
        Capability::EventsRead,
        Capability::TasksWrite,
        Capability::ApprovalsDecide,
    ] {
        assert_ne!(required_capability("safety.report.get"), Some(capability));
    }
}
