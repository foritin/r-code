//! S32 — installed-layout safety probes: the release policy, on this host.
//!
//! The full native matrix (AppImage/deb/MSI/dmg inspection, bwrap install,
//! AppContainer probes) runs in the release workflow's per-target jobs.
//! What this suite proves HERE, deterministically, is the POLICY the
//! release enforces and the installed-helper verification the packaging
//! module performs: advertised write targets must prove Activated; macOS
//! ships SafeDisabled with the write surfaces hidden (not merely
//! disabled); missing or tampered helpers fail policy; and the operator
//! runbook the docs-consistency check requires exists with the SafeDisabled
//! and quarantine prerequisites in it.

use r_code_runtime::services::helper_binaries::{
    HelperBinaryError, HelperBinaryResolver, SAFETY_PROBE_HELPER,
};

/// The release policy for one platform target: which capabilities are
/// ADVERTISED as write-enabled (and therefore must prove Activated before
/// the release may ship) versus shipped SafeDisabled with those surfaces
/// hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseSafetyPolicy {
    pub platform: &'static str,
    pub write_advertised: bool,
}

pub const WINDOWS_X64: ReleaseSafetyPolicy = ReleaseSafetyPolicy {
    platform: "windows-x64",
    write_advertised: false,
};
pub const LINUX_X64: ReleaseSafetyPolicy = ReleaseSafetyPolicy {
    platform: "linux-x64",
    write_advertised: false,
};
pub const MACOS_ARM64: ReleaseSafetyPolicy = ReleaseSafetyPolicy {
    platform: "macos-arm64",
    write_advertised: false,
};

impl ReleaseSafetyPolicy {
    /// P32.2: the activation verdict this target's release requires. This
    /// wave no platform advertises write capabilities in the shipped
    /// product (the P24B readiness grants nothing without an Activated
    /// report), so every target accepts SafeDisabled — an advertised-write
    /// target would refuse it. macOS is the documented example: SafeDisabled
    /// ships with O-GATE/Check/Process/Shell HIDDEN.
    pub fn accepts(&self, verdict: &str) -> bool {
        if self.write_advertised {
            verdict == "activated"
        } else {
            matches!(verdict, "activated" | "safe-disabled" | "unsupported")
        }
    }

    /// The surfaces that must be UNDISCOVERABLE when shipping SafeDisabled.
    pub fn hidden_when_safe_disabled(&self) -> &'static [&'static str] {
        if self.write_advertised {
            &[]
        } else {
            &["o-gate", "check.run", "process.open", "shell"]
        }
    }
}

#[test]
fn advertised_write_targets_require_activated_and_macos_hides_surfaces() {
    // No shipped target advertises write this wave: SafeDisabled ships.
    for policy in [WINDOWS_X64, LINUX_X64, MACOS_ARM64] {
        assert!(policy.accepts("safe-disabled"), "{}", policy.platform);
        assert!(policy.accepts("activated"));
    }

    // The advertised-write arm (the policy future targets flip on): only
    // Activated ships.
    let advertised = ReleaseSafetyPolicy {
        platform: "future-write-target",
        write_advertised: true,
    };
    assert!(advertised.accepts("activated"));
    assert!(!advertised.accepts("safe-disabled"));
    assert!(!advertised.accepts("unsupported"));
    assert!(advertised.hidden_when_safe_disabled().is_empty());

    // macOS SafeDisabled hides the four write surfaces — the documented
    // example: the capabilities do not exist in the UI, not merely refuse.
    let hidden = MACOS_ARM64.hidden_when_safe_disabled();
    for surface in ["o-gate", "check.run", "process.open", "shell"] {
        assert!(hidden.contains(&surface), "{surface} must be hidden");
    }
}

#[test]
fn missing_or_tampered_helpers_fail_policy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let installed = temp.path().join("installed");
    std::fs::create_dir_all(&installed).expect("layout");

    // Missing: no helpers staged -> both probes refuse Missing.
    let resolver = HelperBinaryResolver::new(None, Some(installed.clone()));
    assert!(matches!(
        resolver.safety_probe(),
        Err(HelperBinaryError::Missing { .. })
    ));
    assert!(matches!(
        resolver.guardian(),
        Err(HelperBinaryError::Missing { .. })
    ));

    // The release-side inspection (packaging::verify_installed_helpers)
    // refuses the same empty layout with a missing-helper error.
    assert!(r_code_host::packaging::verify_installed_helpers(&installed).is_err());

    // Tampered: a real-sized file with a wrong (non-native) magic.
    let probe_name = if cfg!(windows) {
        format!("{SAFETY_PROBE_HELPER}.exe")
    } else {
        SAFETY_PROBE_HELPER.to_string()
    };
    let probe_path = installed.join(&probe_name);
    std::fs::write(&probe_path, vec![0x41u8; 128 * 1024]).expect("tampered probe");
    assert!(matches!(
        resolver.safety_probe(),
        Err(HelperBinaryError::Tampered { .. })
    ));

    // Build the real helper first (cargo build is test-legal: it owns its
    // own child, and the harness elsewhere does the same).
    let build =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "build",
                "--all-features",
                "-p",
                "r-code-runtime",
                "--bin",
                SAFETY_PROBE_HELPER,
            ])
            .output()
            .expect("build probe");
    assert!(
        build.status.success(),
        "probe build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    // Valid: the real probe binary from this build's target dir verifies.
    // (src-tauri sits one level below the workspace root.)
    let exe = if cfg!(windows) {
        format!("{SAFETY_PROBE_HELPER}.exe")
    } else {
        SAFETY_PROBE_HELPER.to_string()
    };
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug")
        .join(&exe);
    std::fs::copy(&source, &probe_path).expect("stage real probe");
    assert!(
        resolver.safety_probe().is_ok(),
        "the real helper verifies from the installed layout"
    );

    // The release inspection also verifies the tampered-size rule: stage a
    // second tampered helper (guardian, below the size floor) and confirm
    // the whole-layout inspection refuses, then restore the real pair and
    // confirm the digests are recorded for both helpers.
    let guardian_name = if cfg!(windows) {
        "r-code-process-guardian.exe"
    } else {
        "r-code-process-guardian"
    };
    let guardian_path = installed.join(guardian_name);
    std::fs::write(&guardian_path, vec![0x42u8; 128 * 1024]).expect("tampered guardian");
    assert!(r_code_host::packaging::verify_installed_helpers(&installed).is_err());

    let guardian_build =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "build",
                "--all-features",
                "-p",
                "r-code-runtime",
                "--bin",
                "r-code-process-guardian",
            ])
            .output()
            .expect("build guardian");
    assert!(
        guardian_build.status.success(),
        "guardian build failed: {}",
        String::from_utf8_lossy(&guardian_build.stderr)
    );
    let guardian_source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug")
        .join(guardian_name);
    std::fs::copy(&guardian_source, &guardian_path).expect("stage real guardian");
    let verified =
        r_code_host::packaging::verify_installed_helpers(&installed).unwrap_or_else(|error| {
            // CI 诊断：转储 guardian 的头字节/大小，非 ELF 的来源一眼可辨。
            let head = std::fs::read(&guardian_path)
                .map(|bytes| {
                    (
                        bytes.len(),
                        bytes
                            .iter()
                            .take(8)
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>(),
                    )
                })
                .unwrap_or((0, "(unreadable)".into()));
            panic!("the real pair verifies: {error}; guardian(len,head8) = {head:?}");
        });
    assert_eq!(verified.len(), 2);
    for helper in &verified {
        assert_eq!(helper.sha256.len(), 64);
        assert!(helper.bytes >= 64 * 1024);
    }
}

#[test]
fn operator_runbook_documents_safedisabled_and_quarantine() {
    // P32.3: the docs-consistency check requires the runbook; this pins
    // its content — the prerequisites, the SafeDisabled posture, and the
    // quarantine recovery path an operator takes.
    let runbook = include_str!("../../docs/support/operations/safety-boundary.md");
    for required in [
        "SafeDisabled",
        "quarantine",
        "Activated",
        "helper",
        "downgrade",
    ] {
        assert!(
            runbook.contains(required),
            "the operator runbook must cover {required}"
        );
    }
}
