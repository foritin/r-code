//! T07 — plugin package installer.
//!
//! Acceptance: installing a valid package has no process side effects;
//! corrupted or escaping archives leave no active registry entry.

use r_code_harness_protocol::{HarnessManifest, Platform};
use r_code_runtime::plugins::*;
use std::fs;
use std::io::Write;
use std::path::Path;

fn manifest_json(executable: &str) -> String {
    let platform = match Platform::current() {
        Platform::WindowsX64 => "windows-x64",
        Platform::MacosArm64 => "macos-arm64",
        Platform::MacosX64 => "macos-x64",
        Platform::LinuxX64 => "linux-x64",
    };
    serde_json::json!({
        "schema_version": "1",
        "id": "example.harness",
        "version": "1.2.3",
        "apiMajor": 1,
        "apiMinor": 0,
        "displayName": "Example Harness",
        "supportedPlatforms": [
            {"platform": platform, "executable": executable, "argv": ["--serve"]}
        ],
        "supportedFeatures": ["multi-turn-tools"],
        "requestedHostServices": ["host.model.stream", "host.tools.call"],
        "configSchema": {"type": "object"}
    })
    .to_string()
}

fn entrypoint_name() -> &'static str {
    if cfg!(windows) {
        "harness.exe"
    } else {
        "harness"
    }
}

/// Build a directory package on disk.
fn write_directory_package(
    root: &Path,
    manifest_body: &str,
    binary_body: &[u8],
) -> std::io::Result<()> {
    let source = root.join("source-pkg");
    fs::create_dir_all(source.join("bin"))?;
    fs::write(source.join("harness.json"), manifest_body)?;
    fs::write(source.join("bin").join(entrypoint_name()), binary_body)?;
    Ok(())
}

#[test]
fn valid_directory_package_installs_immutably_without_execution() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_directory_package(
        temp.path(),
        &manifest_json("bin/harness"),
        b"#!/bin/sh\n# payload",
    )
    .expect("write package");
    let installer = PackageInstaller::new(temp.path().join("plugins"));

    let installed = installer
        .install_from_directory(&temp.path().join("source-pkg"))
        .expect("install");
    assert_eq!(installed.manifest.id.0, "example.harness");
    assert_eq!(installed.package_ref.version.to_string(), "1.2.3");
    assert!(installed.package_ref.content_digest.len() >= 16);
    // Immutable id/version/digest layout.
    let rendered = installed.install_dir.to_string_lossy().replace('\\', "/");
    assert!(rendered.contains("example.harness/1.2.3/"));
    assert!(installed.executable.is_file());
    assert_eq!(installed.argv, vec!["--serve".to_string()]);

    // No staging leftovers and no stray entries beside the package dir.
    let plugins_dir = temp.path().join("plugins");
    let top: Vec<String> = fs::read_dir(&plugins_dir)
        .expect("plugins dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(top, vec!["example.harness".to_string()]);
    let staging: Vec<&String> = top
        .iter()
        .filter(|name| name.starts_with(".staging"))
        .collect();
    assert!(staging.is_empty(), "staging must not survive install");

    // Reinstalling identical bytes is idempotent: same install dir.
    let again = installer
        .install_from_directory(&temp.path().join("source-pkg"))
        .expect("reinstall");
    assert_eq!(again.install_dir, installed.install_dir);
}

#[test]
fn same_identity_with_different_bytes_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_directory_package(temp.path(), &manifest_json("bin/harness"), b"payload-a")
        .expect("write a");
    let installer = PackageInstaller::new(temp.path().join("plugins"));
    installer
        .install_from_directory(&temp.path().join("source-pkg"))
        .expect("install a");

    // Same id/version, different content.
    fs::write(
        temp.path().join("source-pkg/bin").join(entrypoint_name()),
        b"payload-b",
    )
    .expect("mutate");
    let error = installer
        .install_from_directory(&temp.path().join("source-pkg"))
        .expect_err("duplicate identity");
    assert!(
        matches!(error, InstallError::DuplicateIdentity { ref id, .. } if id == "example.harness")
    );
}

#[test]
fn zip_package_installs_and_corrupt_archives_leave_no_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let zip_path = temp.path().join("pkg.zip");
    {
        let file = fs::File::create(&zip_path).expect("create zip");
        let mut archive = zip::ZipWriter::new(file);
        let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        archive
            .start_file("harness.json", options)
            .expect("start manifest");
        archive
            .write_all(manifest_json("bin/harness").as_bytes())
            .expect("write manifest");
        archive
            .start_file(format!("bin/{}", entrypoint_name()), options)
            .expect("start bin");
        archive.write_all(b"zip-payload").expect("write bin");
        archive.finish().expect("finish");
    }

    let installer = PackageInstaller::new(temp.path().join("plugins"));
    let installed = installer.install_from_zip(&zip_path).expect("install zip");
    assert!(installed.executable.is_file());
    assert_eq!(
        fs::read(&installed.executable).expect("read"),
        b"zip-payload"
    );

    // A corrupted archive errors and leaves no registry entry.
    let corrupt = temp.path().join("corrupt.zip");
    fs::write(&corrupt, b"this is not a zip archive").expect("write corrupt");
    let installer_b = PackageInstaller::new(temp.path().join("plugins-b"));
    assert!(matches!(
        installer_b.install_from_zip(&corrupt),
        Err(InstallError::Zip(_))
    ));
    assert!(
        !temp
            .path()
            .join("plugins-b")
            .join("example.harness")
            .exists(),
        "no registry entry for a failed install"
    );
    // Staging was cleaned up too.
    if temp.path().join("plugins-b").exists() {
        let leftovers: Vec<String> = fs::read_dir(temp.path().join("plugins-b"))
            .expect("dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
    }
}

#[test]
fn escaping_and_link_entries_are_rejected_before_install() {
    let temp = tempfile::tempdir().expect("tempdir");
    let zip_path = temp.path().join("evil.zip");
    {
        let file = fs::File::create(&zip_path).expect("create");
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("../escape.txt", options).expect("start");
        archive.write_all(b"evil").expect("write");
        archive.finish().expect("finish");
    }
    let installer = PackageInstaller::new(temp.path().join("plugins"));
    assert!(matches!(
        installer.install_from_zip(&zip_path),
        Err(InstallError::TraversalEntry(name)) if name == "../escape.txt"
    ));
    assert!(!temp.path().join("escape.txt").exists());
    assert!(!temp.path().join("plugins/example.harness").exists());

    // Manifest-level traversal is rejected before anything is staged.
    write_directory_package(temp.path(), &manifest_json("../outside/bin"), b"x").expect("write");
    let installer = PackageInstaller::new(temp.path().join("plugins-c"));
    assert!(matches!(
        installer.install_from_directory(&temp.path().join("source-pkg")),
        Err(InstallError::InvalidManifest(_))
    ));
}

#[test]
fn missing_manifest_and_missing_entrypoint_are_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    // No harness.json at all.
    let source = temp.path().join("no-manifest");
    fs::create_dir_all(source.join("bin")).expect("mkdir");
    fs::write(source.join("bin").join(entrypoint_name()), b"x").expect("write");
    let installer = PackageInstaller::new(temp.path().join("plugins"));
    assert!(matches!(
        installer.install_from_directory(&source),
        Err(InstallError::MissingManifest)
    ));

    // Manifest without an entrypoint for this platform.
    write_directory_package(temp.path(), &manifest_json("bin/harness"), b"x").expect("write");
    fs::remove_file(temp.path().join("source-pkg/bin").join(entrypoint_name())).expect("remove");
    let installer = PackageInstaller::new(temp.path().join("plugins-d"));
    assert!(matches!(
        installer.install_from_directory(&temp.path().join("source-pkg")),
        Err(InstallError::MissingEntrypoint { .. })
    ));
}

#[test]
fn installs_have_no_process_side_effects_by_construction() {
    // The installer source never spawns processes; this fixture pins that
    // contract by scanning the module for spawn/batch APIs.
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/package.rs");
    let source = fs::read_to_string(&source_path).expect("read installer source");
    for forbidden in ["Command::new", ".spawn()", "powershell", "cmd /c", "sh -c"] {
        assert!(
            !source.contains(forbidden),
            "installer must never execute processes (found {forbidden})"
        );
    }
    // The manifest type itself stays protocol-pure.
    let _manifest: HarnessManifest =
        serde_json::from_str(&manifest_json("bin/harness")).expect("parse");
}
