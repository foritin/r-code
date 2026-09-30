//! P00 — stable, canonical operating-system boot identity.
//!
//! These tests deliberately restart this integration-test binary for the
//! cross-process assertion. That keeps the probe on the public runtime API
//! and avoids adding a production-only helper executable.

use r_code_runtime::process_guard::{BootIdentity, BootIdentityError, BootIdentitySource};
use std::path::Path;
use std::process::Command;

const UUID_A: &str = "01234567-89ab-4cde-8f01-23456789abcd";
const UUID_B: &str = "01234567-89ab-4cde-8f01-23456789abce";
const CHILD_OUTPUT_ENV: &str = "R_CODE_S00_BOOT_IDENTITY_OUTPUT";

#[test]
fn canonical_parse_display_and_serde_round_trip_exactly() {
    let canonical = [
        format!("linux:{UUID_A}"),
        "macos:1723456789:000042".to_string(),
        format!("windows:{UUID_A}"),
    ];

    for value in canonical {
        let identity = BootIdentity::parse(value.clone()).expect("canonical identity");
        assert_eq!(identity.as_str(), value);
        assert_eq!(identity.to_string(), value);

        let encoded = serde_json::to_string(&identity).expect("serialize identity");
        assert_eq!(encoded, serde_json::to_string(&value).unwrap());
        let decoded: BootIdentity =
            serde_json::from_str(&encoded).expect("deserialize canonical identity");
        assert_eq!(decoded, identity);
        assert_eq!(BootIdentity::parse(decoded.to_string()).unwrap(), identity);
    }
}

#[test]
fn parse_and_deserialize_reject_every_noncanonical_shape() {
    let invalid = [
        "",
        "linux:",
        "linux:01234567-89AB-4CDE-8F01-23456789ABCD",
        "Linux:01234567-89ab-4cde-8f01-23456789abcd",
        "linux:00000000-0000-0000-0000-000000000000",
        "windows:00000000-0000-0000-0000-000000000000",
        "windows:01234567-89AB-4CDE-8F01-23456789ABCD",
        "windows:{01234567-89ab-4cde-8f01-23456789abcd}",
        "macos:0:000000",
        "macos:1:00000",
        "macos:1:0000000",
        "macos:1:1000000",
        "macos:01:000001",
        "macos:+1:000001",
        "macos:-1:000001",
        "macos:1:-00001",
        "macos:1:000001:extra",
        "unknown:01234567-89ab-4cde-8f01-23456789abcd",
        " linux:01234567-89ab-4cde-8f01-23456789abcd",
        "linux:01234567-89ab-4cde-8f01-23456789abcd ",
        "linux:01234567-89ab-4cde-8f01-23456789abcd\r",
        "linux:01234567-89ab-4cde-8f01-23456789abcd\n",
        "linux:01234567-89ab-4cde-8f01-23456789abcd\nextra",
    ];

    for value in invalid {
        assert!(
            BootIdentity::parse(value).is_err(),
            "parse accepted noncanonical identity {value:?}"
        );
        let encoded = serde_json::to_string(value).unwrap();
        assert!(
            serde_json::from_str::<BootIdentity>(&encoded).is_err(),
            "serde accepted noncanonical identity {value:?}"
        );
    }

    for encoded in ["null", "true", "42", "[]", "{}"] {
        assert!(serde_json::from_str::<BootIdentity>(encoded).is_err());
    }
}

#[test]
fn linux_payload_requires_lowercase_uuid_and_at_most_one_lf() {
    let expected = BootIdentity::parse(format!("linux:{UUID_A}")).unwrap();
    assert_eq!(BootIdentity::from_linux_boot_id(UUID_A).unwrap(), expected);
    assert_eq!(
        BootIdentity::from_linux_boot_id(&format!("{UUID_A}\n")).unwrap(),
        expected
    );

    let uppercase = UUID_A.to_ascii_uppercase();
    for invalid in [
        "".to_string(),
        "\n".to_string(),
        uppercase,
        "00000000-0000-0000-0000-000000000000".to_string(),
        format!("{UUID_A}\n\n"),
        format!("{UUID_A}\r\n"),
        format!("{UUID_A} "),
        format!(" {UUID_A}"),
        format!("{UUID_A}\nextra"),
    ] {
        assert!(
            BootIdentity::from_linux_boot_id(&invalid).is_err(),
            "accepted noncanonical procfs payload {invalid:?}"
        );
    }
}

#[test]
fn macos_boottime_requires_positive_seconds_and_six_digit_microseconds() {
    assert_eq!(
        BootIdentity::from_macos_boottime(1, 0).unwrap().as_str(),
        "macos:1:000000"
    );
    assert_eq!(
        BootIdentity::from_macos_boottime(1, 999_999)
            .unwrap()
            .as_str(),
        "macos:1:999999"
    );

    assert!(matches!(
        BootIdentity::from_macos_boottime(0, 0),
        Err(BootIdentityError::Zero {
            source: BootIdentitySource::MacOsBootTime
        })
    ));
    for (seconds, micros) in [(0, 1), (-1, 0), (1, -1), (1, 1_000_000)] {
        assert!(BootIdentity::from_macos_boottime(seconds, micros).is_err());
    }
}

#[test]
fn windows_guid_is_strict_canonical_and_nonzero() {
    let expected = BootIdentity::parse(format!("windows:{UUID_A}")).unwrap();
    assert_eq!(
        BootIdentity::from_windows_boot_identifier(UUID_A).unwrap(),
        expected
    );

    for invalid in [
        UUID_A.to_ascii_uppercase(),
        "00000000-0000-0000-0000-000000000000".to_string(),
        format!("{{{UUID_A}}}"),
        UUID_A.replace('-', ""),
        format!("{UUID_A}\n"),
    ] {
        assert!(
            BootIdentity::from_windows_boot_identifier(&invalid).is_err(),
            "accepted noncanonical Windows GUID {invalid:?}"
        );
    }
}

#[test]
fn injected_boot_values_remain_distinct() {
    let first = BootIdentity::parse(format!("linux:{UUID_A}")).unwrap();
    let second = BootIdentity::parse(format!("linux:{UUID_B}")).unwrap();
    assert_ne!(first, second);
    assert_ne!(first.to_string(), second.to_string());
}

#[test]
fn boot_identity_exact_child_probe() {
    let Some(output) = std::env::var_os(CHILD_OUTPUT_ENV) else {
        return;
    };
    let identity = BootIdentity::current().expect("child obtains authoritative boot identity");
    std::fs::write(output, identity.as_str()).expect("child writes identity result");
}

#[test]
fn current_identity_is_stable_in_process_and_across_restarted_processes() {
    let expected = BootIdentity::current().expect("authoritative boot identity");
    for _ in 0..8 {
        assert_eq!(BootIdentity::current().unwrap(), expected);
    }

    let temp = tempfile::tempdir().expect("tempdir");
    for name in ["child-a.txt", "child-b.txt"] {
        let output = temp.path().join(name);
        run_exact_child_probe(&output);
        let child_value = std::fs::read_to_string(&output).expect("child identity output");
        assert_eq!(BootIdentity::parse(child_value).unwrap(), expected);
    }
}

fn run_exact_child_probe(output: &Path) {
    let executable = std::env::current_exe().expect("integration-test executable");
    let result = Command::new(executable)
        .args([
            "--exact",
            "boot_identity_exact_child_probe",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_OUTPUT_ENV, output)
        .status()
        .expect("restart exact child test");
    assert!(result.success(), "child probe failed with {result}");
    assert!(output.is_file(), "child probe did not write its result");
}

#[cfg(windows)]
#[test]
fn windows_nt_query_identity_and_owner_are_real_and_repeatable() {
    use r_code_runtime::process_guard::windows::{process_start_identity, OwnerIdentity};

    let first = BootIdentity::current().expect("NtQuerySystemInformation boot identity");
    assert!(first.as_str().starts_with("windows:"));
    assert_eq!(BootIdentity::parse(first.to_string()).unwrap(), first);
    for _ in 0..8 {
        assert_eq!(BootIdentity::current().unwrap(), first);
    }

    let owner = OwnerIdentity::try_current().expect("stable owner identity");
    assert_eq!(owner.pid, std::process::id());
    assert!(process_start_identity(owner.pid) > 0);
    assert_eq!(owner.boot_nonce, first.as_str());

    let compatibility = OwnerIdentity::current();
    if compatibility.boot_nonce.is_empty() {
        assert!(BootIdentity::parse(compatibility.boot_nonce).is_err());
    } else {
        assert_eq!(
            BootIdentity::parse(compatibility.boot_nonce).unwrap(),
            first
        );
    }
}

#[test]
fn process_owners_use_authoritative_identity_before_any_spawn() {
    let windows = include_str!("../src/process_guard/windows.rs");
    assert!(windows.contains("pub fn try_current()"));
    assert!(windows.contains("BootIdentity::current()?"));
    assert!(windows.contains("boot_nonce: boot_identity.to_string()"));
    assert!(windows.contains("boot_nonce: String::new()"));
    assert!(BootIdentity::parse("").is_err());

    // P22 deleted the interactive path's direct child spawn, so the launch
    // boundary this ordering pin can name is the one the code actually uses:
    // the supervisor call that prepares, journals and resumes the tree. The
    // claim itself is unchanged — identity is proved before anything that can
    // make a child exist.
    let processes = include_str!("../src/services/processes.rs");
    let identity = processes
        .find("let boot_identity = BootIdentity::current()")
        .expect("ManagedProcess obtains stable identity");
    let launch = processes
        .find(".start_with_write_profile(")
        .expect("ManagedProcess launch boundary");
    assert!(
        identity < launch,
        "stable identity must be proved before an effectful child launch"
    );
    assert!(
        processes.contains("boot_nonce: boot_identity.to_string()"),
        "the supervised owner must carry the authoritative boot nonce"
    );
    let fence = processes
        .find("if owner.boot_identity != boot_identity")
        .expect("a supervised tree from another boot is refused");
    assert!(
        identity < fence,
        "the boot fence must compare against the identity just resolved"
    );

    // Acceptance ①: no direct launch survives on the interactive path, so the
    // spawn this ordering pin used to guard against cannot reappear quietly.
    for forbidden in [
        "Command::new(",
        ".spawn()",
        "tokio::process",
        "std::process",
        "CreateProcess",
    ] {
        assert!(
            !processes.contains(forbidden),
            "the interactive service launches a child directly with {forbidden:?}"
        );
    }
}

#[test]
fn process_sources_contain_no_elapsed_time_pid_or_clock_boot_surrogate() {
    let sources = [
        (
            "process_guard/boot.rs",
            include_str!("../src/process_guard/boot.rs"),
        ),
        (
            "process_guard/mod.rs",
            include_str!("../src/process_guard/mod.rs"),
        ),
        (
            "process_guard/unix.rs",
            include_str!("../src/process_guard/unix.rs"),
        ),
        (
            "process_guard/windows.rs",
            include_str!("../src/process_guard/windows.rs"),
        ),
        (
            "services/processes.rs",
            include_str!("../src/services/processes.rs"),
        ),
    ];
    let forbidden_everywhere = [
        "GetTickCount64",
        "/proc/stat",
        "/proc/uptime",
        "boot_nonce()",
        "format!(\"pid-{}\"",
    ];

    for (path, source) in sources {
        for needle in forbidden_everywhere {
            assert!(
                !source.contains(needle),
                "{path} contains forbidden boot surrogate {needle:?}"
            );
        }
    }

    let boot_source = include_str!("../src/process_guard/boot.rs");
    for needle in [
        "CLOCK_BOOTTIME",
        "clock_gettime",
        "SystemTime::now",
        "Instant::now",
    ] {
        assert!(
            !boot_source.contains(needle),
            "boot identity uses forbidden clock surrogate {needle:?}"
        );
    }
}
