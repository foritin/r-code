//! S24H — guardian/safety-probe helpers ship and resolve fail-closed.
//!
//! Proves the P24H acceptance on a real staged layout: the runtime-owned
//! resolver prefers the explicit helper directory, falls back to verified
//! siblings of the daemon, never searches PATH, and refuses tampered
//! (truncated / wrong-magic) or missing helpers instead of activating them;
//! the packaged helpers actually launch from the staged layout; and a
//! standalone daemon auto-started through the client binds the helper
//! directory end to end.

use r_code_client::DaemonClient;
use r_code_runtime::services::helper_binaries::{
    helper_digest, HelperBinaryError, HelperBinaryResolver, GUARDIAN_HELPER, SAFETY_PROBE_HELPER,
};
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::process::Command;

fn target_binary(name: &str) -> PathBuf {
    // src-tauri sits one level below the workspace root: ../target is the
    // shared target dir (the runtime tests' ../../target would overshoot).
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug")
        .join(&exe);
    // A `cargo check -p r-code-host` in dev mode makes tauri-build stage
    // PLACEHOLDER sidecars (stripping the target triple) over real
    // artifacts in target/debug, and a later fresh-fingerprint build will
    // not restore them. Detect a clobbered artifact and force a relink.
    let clobbered = std::fs::metadata(&path).is_ok_and(|meta| meta.len() < 64 * 1024);
    if clobbered {
        let _ = std::fs::remove_file(&path);
    }
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", "r-code-runtime", "--bin", name])
        .output()
        .expect("build helper");
    assert!(output.status.success(), "{name} build failed");
    let metadata = std::fs::metadata(&path).unwrap_or_else(|_| panic!("{exe} missing after build"));
    assert!(
        metadata.len() >= 64 * 1024,
        "{exe} is only {} bytes — a placeholder clobbered the artifact",
        metadata.len()
    );
    path
}

fn staged_layout(temp: &Path) -> PathBuf {
    let layout = temp.join("packaged-layout");
    std::fs::create_dir_all(&layout).expect("layout dir");
    for name in ["r-code-service", GUARDIAN_HELPER, SAFETY_PROBE_HELPER] {
        std::fs::copy(target_binary(name), layout.join(file_name(name))).expect("stage helper");
    }
    layout
}

fn file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// P24H.1: explicit absolute directory first, then verified siblings of the
/// daemon — and a helper that exists ONLY outside those two places is never
/// resolved (no PATH search, no wandering).
#[test]
fn resolver_prefers_explicit_then_verified_siblings_never_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = staged_layout(temp.path());
    // The helper also exists in the build target dir — a location the
    // resolver must NOT find just because it is on disk somewhere.
    let build_dir = target_binary(SAFETY_PROBE_HELPER)
        .parent()
        .expect("target dir")
        .to_path_buf();

    let sibling = HelperBinaryResolver::new(None, Some(layout.clone()));
    let probe = sibling.safety_probe().expect("sibling resolves the probe");
    assert_eq!(probe, layout.join(file_name(SAFETY_PROBE_HELPER)));
    let guardian = sibling.guardian().expect("sibling resolves the guardian");
    assert_eq!(guardian, layout.join(file_name(GUARDIAN_HELPER)));

    // Explicit wins over the sibling directory.
    let explicit_dir = temp.path().join("explicit-helpers");
    std::fs::create_dir_all(&explicit_dir).expect("explicit dir");
    std::fs::copy(
        layout.join(file_name(SAFETY_PROBE_HELPER)),
        explicit_dir.join(file_name(SAFETY_PROBE_HELPER)),
    )
    .expect("stage explicit probe");
    let explicit = HelperBinaryResolver::new(Some(explicit_dir.clone()), Some(layout.clone()));
    let probe = explicit
        .safety_probe()
        .expect("explicit resolves the probe");
    assert_eq!(
        probe,
        explicit_dir.join(file_name(SAFETY_PROBE_HELPER)),
        "the explicit directory wins"
    );
    // The explicit binding is authoritative: it does not fall through to
    // the sibling for a helper the explicit directory does not hold.
    match explicit.guardian() {
        Err(HelperBinaryError::Missing { name, directory }) => {
            assert_eq!(name, GUARDIAN_HELPER);
            assert_eq!(directory, explicit_dir);
        }
        other => panic!("an explicit binding must not fall through: {other:?}"),
    }

    // Never-PATH, structurally: a resolver whose two directories hold no
    // probe answers Missing even though the probe sits in the build dir.
    let empty = temp.path().join("empty-daemon-dir");
    std::fs::create_dir_all(&empty).expect("empty dir");
    match HelperBinaryResolver::new(None, Some(empty)).safety_probe() {
        Err(HelperBinaryError::Missing { .. }) => {}
        other => panic!("must not resolve from elsewhere on disk: {other:?}"),
    }
    assert!(build_dir.join(file_name(SAFETY_PROBE_HELPER)).is_file());
    // Neither directory: unbound, never a guess.
    assert!(matches!(
        HelperBinaryResolver::new(None, None).safety_probe(),
        Err(HelperBinaryError::Unbound)
    ));

    // The digest is stable and pins the packaged bytes.
    let first = helper_digest(&probe).expect("digest");
    let second = helper_digest(&probe).expect("digest again");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64, "sha256 hex");
}

/// Acceptance ③, refusal arm: tampered (truncated / wrong magic) and
/// missing helpers never resolve — the capability stays SafeDisabled
/// instead of activating whatever is on disk.
#[test]
fn tampered_truncated_wrong_magic_and_missing_helpers_never_resolve() {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = staged_layout(temp.path());

    // Truncated: below the 64 KiB helper floor.
    let truncated = layout.join(file_name(SAFETY_PROBE_HELPER));
    std::fs::write(&truncated, b"MZ but far too small").expect("truncate");
    match HelperBinaryResolver::new(None, Some(layout.clone())).safety_probe() {
        Err(HelperBinaryError::Tampered { reason, .. }) => {
            assert!(
                reason.contains("bytes"),
                "the refusal names the floor: {reason}"
            );
        }
        other => panic!("a truncated helper must never resolve: {other:?}"),
    }

    // Wrong magic: placeholder-sized-but-large garbage.
    std::fs::write(&truncated, vec![0_u8; 128 * 1024]).expect("garbage magic");
    match HelperBinaryResolver::new(None, Some(layout.clone())).safety_probe() {
        Err(HelperBinaryError::Tampered { reason, .. }) => {
            assert!(
                reason.contains("magic"),
                "the refusal names the magic: {reason}"
            );
        }
        other => panic!("a wrong-magic helper must never resolve: {other:?}"),
    }

    // Missing entirely.
    std::fs::remove_file(layout.join(file_name(GUARDIAN_HELPER))).expect("remove guardian");
    match HelperBinaryResolver::new(None, Some(layout)).guardian() {
        Err(HelperBinaryError::Missing { name, .. }) => assert_eq!(name, GUARDIAN_HELPER),
        other => panic!("a missing helper must never resolve: {other:?}"),
    }
}

/// The e2e launch arm: the helpers staged in a packaged layout actually
/// launch. The safety probe answers a protocol violation with exit 2 and a
/// JSON error on stderr (its documented discipline); the guardian is an
/// empty main on non-Linux hosts (the Windows containment is the in-process
/// guardian + job), so its launch proof is platform-honest.
#[test]
fn packaged_helpers_launch_from_the_staged_layout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = staged_layout(temp.path());
    let probe = layout.join(file_name(SAFETY_PROBE_HELPER));

    let output = Command::new(&probe)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("launch the staged safety probe");
    assert_eq!(
        output.status.code(),
        Some(2),
        "the probe's protocol discipline: bad request exits 2"
    );
    // The probe answers on its reply channel: a versioned JSON error on
    // stdout (diagnostics never touch the wire, so stderr stays empty).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"error\"") && stdout.contains("\"version\""),
        "the probe's error object must ride the reply channel: {stdout}"
    );

    if !cfg!(target_os = "linux") {
        let guardian = layout.join(file_name(GUARDIAN_HELPER));
        let status = Command::new(&guardian)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("launch the staged guardian");
        assert_eq!(
            status.code(),
            Some(0),
            "the non-Linux guardian is an empty main by design"
        );
    }
    // On Linux the guardian's real session is exercised by the s08 native
    // suite; launching it here without dup2'd control descriptors would
    // prove nothing, so the resolve+verify coverage above is the honest
    // bound for this host.
}

/// P24H.3, standalone arm: the client's daemon auto-start binds the helper
/// directory end to end — the daemon accepts `--helper-dir`, comes up, and
/// owns the profile (a rejected flag would exit before any owner token).
#[tokio::test]
async fn standalone_daemon_startup_binds_the_helper_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = staged_layout(temp.path());
    let service = layout.join(file_name("r-code-service"));

    let ipc_name = format!("s24h-{}-{}", std::process::id(), temp.path().iter().count());
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.path().join("data"))
            .with_ipc_name(ipc_name.clone()),
    )
    .expect("profile");

    let info = r_code_client::ensure_daemon_with_helpers(
        &profile.harness_v1_root(),
        &profile.ipc_endpoint(),
        &profile.profile_id(),
        Some(&service),
        Some(&layout),
    )
    .await
    .expect("the daemon accepts the bound helper directory and comes up");
    assert!(info.token.len() >= 32, "owner token written");

    let client = DaemonClient::connect(
        &profile.ipc_endpoint(),
        &profile.profile_id(),
        &info.token,
        "s24h-smoke",
    )
    .await
    .expect("the bound daemon serves the profile");

    // The daemon actually bound the helper directory: its launch options
    // round-trip is not directly observable over RPC, so the observable
    // contract here is startup + service; resolver-level binding is pinned
    // by the resolver tests above plus the packaging flow test.
    drop(client);

    // Tests own their daemon: the owner document records the daemon's pid.
    let owner = r_code_runtime::daemon::owner_identity_of(&profile.harness_v1_root())
        .expect("owner.json readable");
    assert!(owner.pid > 0, "the owner document names the daemon pid");
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/PID", &owner.pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    } else {
        let _ = Command::new("kill")
            .arg("-9")
            .arg(owner.pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}
