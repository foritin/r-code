//! T08 — plugin catalog and version lifecycle.
//!
//! Acceptance: catalog tests prove stable run pinning, explicit unavailable
//! states and reversible enable/disable without changing another task.

use r_code_harness_protocol::Platform;
use r_code_runtime::plugins::*;
use r_code_store::v2::V2Store;
use std::fs;
use std::path::Path;

fn write_package(root: &Path, name: &str, version: &str, body: &[u8]) -> std::io::Result<()> {
    let source = root.join(format!("pkg-{name}"));
    fs::create_dir_all(source.join("bin"))?;
    let platform = match Platform::current() {
        Platform::WindowsX64 => "windows-x64",
        Platform::MacosArm64 => "macos-arm64",
        Platform::MacosX64 => "macos-x64",
        Platform::LinuxX64 => "linux-x64",
    };
    fs::write(
        source.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": "example.harness",
            "version": version,
            "apiMajor": 1,
            "apiMinor": 0,
            "displayName": "Example",
            "supportedPlatforms": [{"platform": platform, "executable": "bin/harness"}],
            "requestedHostServices": ["host.model.stream"],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )?;
    let binary = if cfg!(windows) {
        "harness.exe"
    } else {
        "harness"
    };
    fs::write(source.join("bin").join(binary), body)?;
    Ok(())
}

fn catalog(temp: &Path) -> PluginCatalog {
    let store = V2Store::open(&temp.join("harness-v2").join("tasks.sqlite3")).expect("store");
    PluginCatalog::new(temp.join("plugins"), std::sync::Arc::new(store))
}

#[test]
fn installs_register_as_available_and_disable_is_reversible() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_package(temp.path(), "v1", "1.0.0", b"body-v1").expect("write");
    let catalog = catalog(temp.path());

    let installed = catalog
        .install_from_directory(&temp.path().join("pkg-v1"))
        .expect("install");
    let digest_v1 = installed.package_ref.content_digest.clone();

    let entries = catalog.list().expect("list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].availability, Availability::Available);
    assert!(entries[0].enabled);
    assert_eq!(entries[0].manifest.requested_host_services.len(), 1);

    // Disable: explicit unavailable state.
    catalog
        .set_enabled("example.harness", &digest_v1, false)
        .expect("disable");
    let entries = catalog.list().expect("list");
    assert_eq!(
        entries[0].availability,
        Availability::Unavailable(UnavailableReason::Disabled)
    );
    // Effective selection for new runs refuses disabled packages.
    assert!(matches!(
        catalog.effective_package("example.harness"),
        Err(CatalogError::NoUsableVersion(_))
    ));

    // Re-enable: available again.
    catalog
        .set_enabled("example.harness", &digest_v1, true)
        .expect("enable");
    assert_eq!(
        catalog.list().expect("list")[0].availability,
        Availability::Available
    );
    assert_eq!(
        catalog
            .effective_package("example.harness")
            .expect("effective")
            .content_digest,
        digest_v1
    );
}

#[test]
fn run_pins_are_stable_across_upgrades() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_package(temp.path(), "v1", "1.0.0", b"body-v1").expect("write v1");
    write_package(temp.path(), "v2", "2.0.0", b"body-v2").expect("write v2");
    let catalog = catalog(temp.path());

    let v1 = catalog
        .install_from_directory(&temp.path().join("pkg-v1"))
        .expect("install v1");
    let v2 = catalog
        .install_from_directory(&temp.path().join("pkg-v2"))
        .expect("install v2");
    assert_ne!(v1.package_ref.content_digest, v2.package_ref.content_digest);

    // New runs select the newest available version.
    assert_eq!(
        catalog
            .effective_package("example.harness")
            .expect("effective")
            .version
            .to_string(),
        "2.0.0"
    );

    // Attempt A pins v1; installing v3 later changes nothing for A.
    catalog
        .pin("attempt-a", "task-1", &v1.package_ref)
        .expect("pin");
    write_package(temp.path(), "v3", "3.0.0", b"body-v3").expect("write v3");
    catalog
        .install_from_directory(&temp.path().join("pkg-v3"))
        .expect("install v3");
    let pinned = catalog
        .pinned_package("attempt-a")
        .expect("pinned")
        .expect("present");
    assert_eq!(pinned.content_digest, v1.package_ref.content_digest);
    assert_eq!(pinned.version.to_string(), "1.0.0");
    assert_eq!(
        catalog
            .effective_package("example.harness")
            .expect("effective")
            .version
            .to_string(),
        "3.0.0"
    );

    // Removing the pinned v1 is refused; removing unpinned v2 succeeds.
    assert!(matches!(
        catalog.remove("example.harness", &v1.package_ref.content_digest),
        Err(CatalogError::RemovePinned { ref pinned_by, .. }) if pinned_by == &vec!["attempt-a".to_string()]
    ));
    catalog
        .remove("example.harness", &v2.package_ref.content_digest)
        .expect("remove unpinned");
    let ids: Vec<String> = catalog
        .list()
        .expect("list")
        .into_iter()
        .map(|entry| entry.manifest.version.to_string())
        .collect();
    assert_eq!(ids, vec!["1.0.0".to_string(), "3.0.0".to_string()]);
}

#[test]
fn enable_disable_does_not_touch_other_tasks_pins() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_package(temp.path(), "v1", "1.0.0", b"body-v1").expect("write");
    let catalog = catalog(temp.path());
    let installed = catalog
        .install_from_directory(&temp.path().join("pkg-v1"))
        .expect("install");

    // Two tasks pin the same package.
    catalog
        .pin("attempt-1", "task-1", &installed.package_ref)
        .expect("pin 1");
    catalog
        .pin("attempt-2", "task-2", &installed.package_ref)
        .expect("pin 2");

    // Disabling the plugin changes neither pin nor the recorded versions.
    catalog
        .set_enabled(
            "example.harness",
            &installed.package_ref.content_digest,
            false,
        )
        .expect("disable");
    for attempt in ["attempt-1", "attempt-2"] {
        let pinned = catalog
            .pinned_package(attempt)
            .expect("pinned")
            .expect("present");
        assert_eq!(pinned.content_digest, installed.package_ref.content_digest);
        assert_eq!(pinned.version.to_string(), "1.0.0");
    }
    // Removal is still refused while pins exist.
    assert!(matches!(
        catalog.remove("example.harness", &installed.package_ref.content_digest),
        Err(CatalogError::RemovePinned { .. })
    ));
}

#[test]
fn missing_entrypoint_is_explicitly_unavailable() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_package(temp.path(), "v1", "1.0.0", b"body-v1").expect("write");
    let catalog = catalog(temp.path());
    let installed = catalog
        .install_from_directory(&temp.path().join("pkg-v1"))
        .expect("install");

    // Sabotage the installed executable.
    let binary = if cfg!(windows) {
        "harness.exe"
    } else {
        "harness"
    };
    fs::remove_file(installed.install_dir.join("bin").join(binary)).expect("remove binary");

    let entries = catalog.list().expect("list");
    match &entries[0].availability {
        Availability::Unavailable(UnavailableReason::MissingEntrypoint { executable }) => {
            assert!(executable.contains("bin/harness"));
        }
        other => panic!("expected missing-entrypoint unavailability, got {other:?}"),
    }
    assert!(matches!(
        catalog.effective_package("example.harness"),
        Err(CatalogError::NoUsableVersion(_))
    ));
}

#[test]
fn duplicate_identity_with_different_bytes_is_rejected_at_install() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_package(temp.path(), "v1", "1.0.0", b"body-original").expect("write");
    let catalog = catalog(temp.path());
    catalog
        .install_from_directory(&temp.path().join("pkg-v1"))
        .expect("install");

    // Same id+version, different bytes.
    write_package(temp.path(), "v1b", "1.0.0", b"body-tampered").expect("write tampered");
    // The second package has the same identity; only its binary differs.
    let error = catalog
        .install_from_directory(&temp.path().join("pkg-v1b"))
        .expect_err("duplicate identity");
    assert!(matches!(
        error,
        CatalogError::Install(InstallError::DuplicateIdentity { .. })
    ));
    // The catalog still holds exactly one row.
    assert_eq!(catalog.list().expect("list").len(), 1);
}
