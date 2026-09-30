//! S26 — the bounded, stable before/delta scanner.
//!
//! Proves the acceptance set: every captured file carries content+physical
//! identity; an unbounded scan (entries, logical bytes or deadline) is
//! `Unavailable` and can never receipt; foreign bytes (pre-existing
//! ignored paths, `.git`, ephemeral roots) are never read-modified and
//! survive every cycle; links are rejected; deltas are deterministic with
//! a binary distinction; the CAS pass journals every form through the
//! P26A effect-artifact store; a concurrent initial-scan edit aborts
//! before any user code; and revalidation/post-exit double-capture close
//! the launch-release and drift doors.

use r_code_runtime::services::artifacts::{ArtifactStore, EffectArtifactPut};
use r_code_runtime::services::process_effects::{
    capture_manifest, cas_manifest, delta, post_exit_drift, revalidate, DeltaKind, ScanBounds,
    ScanError, ScanPolicy,
};
use r_code_store::v1::operations::PrepareProcessTree;
use r_code_store::v1::{ProcessEffectPrepare, V1Store};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn write(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, bytes).expect("write");
}

fn fixture(root: &Path) {
    write(root, "a.txt", b"alpha\n");
    write(root, "sub/b.txt", b"beta\n");
    write(root, "gone.txt", b"temporary\n");
    write(root, "bin.dat", b"BIN\x00ARY\n");
    // Identical content elsewhere: the CAS pass must dedupe these.
    write(root, "dup-one.txt", b"same bytes\n");
    write(root, "dup-two.txt", b"same bytes\n");
}

#[test]
fn manifests_are_deterministic_and_deltas_cover_every_kind() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    let policy = ScanPolicy::for_workspace_root(&root);

    let first = capture_manifest(&policy, &ScanBounds::default()).expect("capture");
    let second = capture_manifest(&policy, &ScanBounds::default()).expect("capture again");
    assert_eq!(first, second, "captures of an unchanged tree are identical");

    // Every file carries content + physical identity.
    for file in &first.files {
        assert_eq!(file.sha256.len(), 64);
        assert!(file.bytes > 0);
        assert!(file.modified_ms > 0);
    }
    assert!(first.files.iter().any(|file| file.is_binary));
    assert_eq!(first.files.iter().filter(|file| file.is_binary).count(), 1);

    // Mutate one file of every delta kind.
    write(&root, "a.txt", b"alpha-edited\n");
    std::fs::remove_file(root.join("gone.txt")).expect("delete");
    write(&root, "new.txt", b"created\n");
    write(&root, "bin.dat", b"BIN\x00ARY-EDITED\n");

    let after = capture_manifest(&policy, &ScanBounds::default()).expect("after");
    let entries = delta(&first, &after);
    assert_eq!(
        entries,
        vec![
            r_code_runtime::services::process_effects::DeltaEntry {
                path: "a.txt".into(),
                kind: DeltaKind::Edited
            },
            r_code_runtime::services::process_effects::DeltaEntry {
                path: "bin.dat".into(),
                kind: DeltaKind::Binary
            },
            r_code_runtime::services::process_effects::DeltaEntry {
                path: "gone.txt".into(),
                kind: DeltaKind::Deleted
            },
            r_code_runtime::services::process_effects::DeltaEntry {
                path: "new.txt".into(),
                kind: DeltaKind::Created
            },
        ],
        "path-sorted create/edit/delete/binary, duplicates untouched"
    );
}

#[test]
fn unbounded_scans_are_unavailable_and_cannot_receipt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    let policy = ScanPolicy::for_workspace_root(&root);

    // Entry bound: seven files exist; a bound below that is unavailable.
    let bounds = ScanBounds {
        max_entries: 3,
        ..ScanBounds::default()
    };
    match capture_manifest(&policy, &bounds).unwrap_err() {
        ScanError::Unavailable(reason) => assert!(reason.contains("entry"), "{reason}"),
        other => panic!("expected the entry-bound refusal, got {other:?}"),
    }

    // Logical-byte bound.
    let bounds = ScanBounds {
        max_logical_bytes: 4,
        ..ScanBounds::default()
    };
    match capture_manifest(&policy, &bounds).unwrap_err() {
        ScanError::Unavailable(reason) => assert!(reason.contains("byte"), "{reason}"),
        other => panic!("expected the byte-bound refusal, got {other:?}"),
    }

    // Deadline: an already-expired budget refuses deterministically.
    let bounds = ScanBounds {
        max_duration: Duration::ZERO,
        ..ScanBounds::default()
    };
    match capture_manifest(&policy, &bounds).unwrap_err() {
        ScanError::Unavailable(reason) => assert!(reason.contains("deadline"), "{reason}"),
        other => panic!("expected the deadline refusal, got {other:?}"),
    }
}

#[test]
fn links_rejected_ignored_ephemeral_excluded_foreign_bytes_preserved() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    // Foreign bytes the scanner must preserve verbatim.
    write(&root, ".git/HEAD", b"ref: refs/heads/main\n");
    write(&root, "vendor/generated.log", b"ignored payload\n");
    write(&root, "tmp-run/scratch.txt", b"ephemeral payload\n");

    // The workspace binding's default policy pins the same primitives.
    let binding = r_code_runtime::services::workspaces::TaskWorkspaceBinding::bind_local(
        "task-s26",
        &root,
        &[],
    )
    .expect("bind");
    let policy = binding.scan_policy();
    let mut policy = ScanPolicy {
        ignored: {
            let mut ignored = policy.ignored.clone();
            ignored.push("vendor".to_string());
            ignored
        },
        ephemeral: vec!["tmp-run".to_string()],
        ..policy
    };
    policy.root = binding.canonical_root.clone();

    let before = capture_manifest(&policy, &ScanBounds::default()).expect("capture");
    let paths: Vec<&str> = before.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "a.txt",
            "bin.dat",
            "dup-one.txt",
            "dup-two.txt",
            "gone.txt",
            "sub/b.txt",
        ],
        "ignored, ephemeral and .git paths never enter the manifest"
    );

    // Mutate everything in scope and back: the foreign bytes are identical.
    write(&root, "a.txt", b"alpha-edited\n");
    let after = capture_manifest(&policy, &ScanBounds::default()).expect("after");
    assert_eq!(delta(&before, &after).len(), 1);
    assert_eq!(
        std::fs::read(root.join(".git/HEAD")).expect("git"),
        b"ref: refs/heads/main\n",
        "foreign bytes preserved verbatim"
    );
    assert_eq!(
        std::fs::read(root.join("vendor/generated.log")).expect("vendor"),
        b"ignored payload\n"
    );
    assert_eq!(
        std::fs::read(root.join("tmp-run/scratch.txt")).expect("ephemeral"),
        b"ephemeral payload\n"
    );

    // A symlink anywhere in scope rejects the whole scan. On Windows the
    // test host lacks the symlink privilege, so the link is a directory
    // JUNCTION (mklink /J needs none) — a reparse point the scanner's
    // is_symlink check detects identically.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("a.txt"), root.join("link.txt")).expect("symlink");
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd")
            .args([
                "/c",
                "mklink",
                "/J",
                &root.join("link-dir").to_string_lossy(),
                &root.join("sub").to_string_lossy(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("mklink junction");
        assert!(status.success(), "junction creation failed");
    }
    assert!(matches!(
        capture_manifest(&policy, &ScanBounds::default()),
        Err(ScanError::Link(path)) if (cfg!(windows) && path == "link-dir")
            || (!cfg!(windows) && path == "link.txt")
    ));
}

fn database_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("store.db")
}

fn seed_operation(store: &V1Store, operation_id: &str) -> String {
    let tree_id = format!("tree-{operation_id}");
    let platform_identity = json!({"nativePid": 41000, "startToken": "s26"});
    store
        .prepare_process_tree(&PrepareProcessTree {
            tree_id: tree_id.clone(),
            attempt_id: format!("attempt-{operation_id}"),
            workspace_key: "workspace-s26".to_string(),
            profile_id: "s26".to_string(),
            owner: r_code_store::v1::operations::ProcessTreeOwner {
                pid: 4_100,
                start_identity: 41_000,
                boot_identity: "windows:01234567-89ab-4cde-8f01-23456789abcd".to_string(),
                platform_identity_digest: r_code_harness_protocol::canonical_input_hash(
                    &platform_identity,
                ),
                platform_identity,
            },
        })
        .expect("seed tree");
    store
        .prepare_process_effect(ProcessEffectPrepare {
            operation_id: operation_id.to_string(),
            tree_id,
            attempt_id: format!("attempt-{operation_id}"),
            workspace_key: "workspace-s26".to_string(),
            owner_id: format!("owner-{operation_id}"),
            fencing_epoch: 3,
            command_json: json!({"program": "tool"}).to_string(),
            lease_json: json!({"lease": "l"}).to_string(),
            before_manifest_json: json!({"files": []}).to_string(),
            before_manifest_digest: format!("digest-{operation_id}"),
            scan_policy_json: json!({"roots": []}).to_string(),
            ephemeral_roots_json: json!({"roots": []}).to_string(),
        })
        .expect("prepare operation");
    format!("owner-{operation_id}")
}

#[test]
fn cas_pass_journals_every_form_and_refuses_stale_content() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    let policy = ScanPolicy::for_workspace_root(&root);
    let store = V1Store::open(&database_path(&temp)).expect("store");
    let blobs = temp.path().join("blobs");
    let artifacts = ArtifactStore::for_task(&blobs, "task-s26");
    let owner = seed_operation(&store, "op-cas");
    let operation = EffectArtifactPut {
        operation_id: "op-cas",
        owner_id: &owner,
        fencing_epoch: 3,
        kind: "before-blob",
    };

    let manifest = capture_manifest(&policy, &ScanBounds::default()).expect("capture");
    cas_manifest(&store, &artifacts, operation, &policy, &manifest).expect("cas");

    // Every form is journaled: the manifest JSON + five distinct file
    // digests (the duplicate pair shares one digest: 6 files, 5 digests).
    let mut blob_count = 0;
    for entry in std::fs::read_dir(&blobs).expect("blobs") {
        if entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .ends_with(".blob")
        {
            blob_count += 1;
        }
    }
    assert_eq!(blob_count, 6, "manifest + five distinct file digests");
    for file in &manifest.files {
        assert!(
            store.effect_artifact_is_live(&file.sha256).expect("live"),
            "{} is refcounted",
            file.path
        );
    }

    // Stale content refuses: editing a file after the capture makes the
    // CAS pass abort instead of journaling bytes the manifest never saw.
    write(&root, "a.txt", b"alpha-mutated\n");
    assert!(matches!(
        cas_manifest(&store, &artifacts, operation, &policy, &manifest),
        Err(ScanError::ConcurrentEdit(path)) if path == "a.txt"
    ));
}

#[test]
fn concurrent_initial_scan_edit_aborts_before_user_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    // A big file widens the read window the detector brackets, and the
    // racer alternates SIZES too, so a mid-flight write flips either the
    // length or the mtime between the bracketing stats.
    let hot_path = root.join("hot.bin");
    std::fs::write(&hot_path, vec![7_u8; 4 * 1024 * 1024]).expect("hot");
    let policy = ScanPolicy::for_workspace_root(&root);

    let stop = Arc::new(AtomicBool::new(false));
    let mutator = {
        let stop = Arc::clone(&stop);
        let path = hot_path.clone();
        std::thread::spawn(move || {
            let mut flip = true;
            while !stop.load(Ordering::SeqCst) {
                flip = !flip;
                let payload = if flip {
                    vec![7_u8; 4 * 1024 * 1024]
                } else {
                    vec![9_u8; 6 * 1024 * 1024]
                };
                let _ = std::fs::write(&path, payload);
            }
        })
    };

    // The abort MUST be observed: an in-flight edit during the initial
    // scan surfaces as ConcurrentEdit, never as a manifest user code
    // could act on. A capture that completes is revalidation-consistent.
    let mut aborted = 0;
    let mut completed = 0;
    for _ in 0..120 {
        match capture_manifest(&policy, &ScanBounds::default()) {
            Err(ScanError::ConcurrentEdit(_)) => {
                aborted += 1;
                break;
            }
            Err(other) => panic!("unexpected scan error: {other:?}"),
            Ok(manifest) => {
                if revalidate(&manifest, &policy).is_ok() {
                    completed += 1;
                }
            }
        }
    }
    stop.store(true, Ordering::SeqCst);
    mutator.join().expect("mutator");
    assert!(
        aborted > 0,
        "the in-flight edit must abort the initial scan before user code (completed captures: {completed})"
    );
}

#[test]
fn revalidate_and_post_exit_double_capture_close_the_doors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    let policy = ScanPolicy::for_workspace_root(&root);

    let manifest = capture_manifest(&policy, &ScanBounds::default()).expect("capture");
    revalidate(&manifest, &policy).expect("unchanged tree revalidates");

    // A single foreign edit invalidates the manifest for launch release.
    write(&root, "sub/b.txt", b"beta-drifted\n");
    assert!(matches!(
        revalidate(&manifest, &policy),
        Err(ScanError::ConcurrentEdit(path)) if path == "sub/b.txt"
    ));

    // Post-exit drift: two captures over a drifting tree disagree; over a
    // stable tree they do not.
    let second = capture_manifest(&policy, &ScanBounds::default()).expect("second");
    let drift = post_exit_drift(&manifest, &second).expect("drift detected");
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].path, "sub/b.txt");

    let stable_a = capture_manifest(&policy, &ScanBounds::default()).expect("stable a");
    let stable_b = capture_manifest(&policy, &ScanBounds::default()).expect("stable b");
    assert_eq!(post_exit_drift(&stable_a, &stable_b), None);
}
