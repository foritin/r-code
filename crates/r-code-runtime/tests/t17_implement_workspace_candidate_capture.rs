//! T17 — workspace candidate capture.
//!
//! Fixtures preserve pre-existing user changes, reject foreign worktrees
//! and produce different candidate IDs for relevant content changes;
//! cross-profile canonical-workspace locks exclude concurrent writers.

use r_code_runtime::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_runtime::workspace_locks::WorkspaceLock;

#[test]
fn pre_existing_user_changes_are_preserved_by_capture() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).expect("dirs");
    // A user's pre-existing dirty state.
    std::fs::write(root.join("src/lib.rs"), b"fn main() { user-edit }").expect("write");
    std::fs::write(root.join("README.md"), b"user notes").expect("write");

    let binding = TaskWorkspaceBinding::bind_local("task-1", &root, &[]).expect("bind");
    let baseline = CandidateManifest::capture(&binding).expect("baseline");
    assert_eq!(baseline.captured_files, 2);

    // Capture never mutates the workspace: user bytes are intact.
    assert_eq!(
        std::fs::read(root.join("src/lib.rs")).unwrap(),
        b"fn main() { user-edit }"
    );
    assert_eq!(
        std::fs::read(root.join("README.md")).unwrap(),
        b"user notes"
    );

    // The user keeps editing; a later capture records a different candidate
    // while the earlier manifest still describes the earlier state.
    std::fs::write(root.join("src/lib.rs"), b"fn main() { more }").expect("edit");
    let after = CandidateManifest::capture(&binding).expect("after");
    assert_ne!(after.candidate_id, baseline.candidate_id);
    // The baseline manifest is an immutable snapshot: its content map is
    // unchanged by the later capture.
    let expected_digest =
        r_code_runtime::services::artifacts::sha256_hex(b"fn main() { user-edit }");
    assert_eq!(
        baseline.files.get("src/lib.rs").map(String::as_str),
        Some(expected_digest.as_str()),
    );
}

#[test]
fn foreign_worktrees_are_rejected_unless_registered() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("linked-worktree");
    std::fs::create_dir_all(&root).expect("dir");
    // `.git` as a file marks a linked worktree.
    std::fs::write(root.join(".git"), b"gitdir: ../main/.git/worktrees/x").expect("marker");

    let error = TaskWorkspaceBinding::bind_local("task-1", &root, &[]).expect_err("foreign");
    assert!(matches!(
        error,
        r_code_runtime::services::workspaces::WorkspaceError::ForeignWorktree(_)
    ));

    // Explicitly registered worktrees bind fine.
    let binding = TaskWorkspaceBinding::bind_local("task-1", &root, std::slice::from_ref(&root))
        .expect("registered worktree binds");
    assert!(binding.canonical_root.ends_with("linked-worktree"));
}

#[test]
fn candidate_ids_track_content_not_timestamps() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).expect("dir");
    std::fs::write(root.join("a.txt"), b"content-a").expect("write");

    let binding = TaskWorkspaceBinding::bind_local("task-1", &root, &[]).expect("bind");
    let first = CandidateManifest::capture(&binding).expect("first");

    // Rewriting identical bytes (new mtime) keeps the candidate id.
    std::fs::write(root.join("a.txt"), b"content-a").expect("rewrite");
    let same = CandidateManifest::capture(&binding).expect("same");
    assert_eq!(
        first.candidate_id, same.candidate_id,
        "mtime-only changes are irrelevant"
    );

    // Relevant content change → different id.
    std::fs::write(root.join("a.txt"), b"content-b").expect("change");
    let changed = CandidateManifest::capture(&binding).expect("changed");
    assert_ne!(first.candidate_id, changed.candidate_id);

    // Adding a new file is a relevant change too.
    std::fs::write(root.join("b.txt"), b"new").expect("add");
    let grown = CandidateManifest::capture(&binding).expect("grown");
    assert_ne!(changed.candidate_id, grown.candidate_id);
    assert_eq!(grown.captured_files, 2);
}

#[test]
fn capture_live_consistency_detects_concurrent_edits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).expect("dir");
    std::fs::write(root.join("f.txt"), b"v1").expect("write");

    let binding = TaskWorkspaceBinding::bind_local("task-1", &root, &[]).expect("bind");
    let manifest = CandidateManifest::capture(&binding).expect("capture");
    manifest.verify_live(&binding).expect("consistent");

    // A concurrent edit invalidates the capture.
    std::fs::write(root.join("f.txt"), b"v2-sneaky").expect("concurrent edit");
    let error = manifest.verify_live(&binding).expect_err("inconsistent");
    assert!(matches!(
        error,
        r_code_runtime::services::workspaces::WorkspaceError::Inconsistent { .. }
    ));
}

#[test]
fn capability_checks_confine_paths_to_the_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).expect("dirs");
    std::fs::write(root.join("src/a.rs"), b"x").expect("write");

    let binding = TaskWorkspaceBinding::bind_local("task-1", &root, &[]).expect("bind");
    assert!(binding.contains(&root.join("src/a.rs")));
    assert!(binding.contains(&root.join("src").join("..").join("src").join("a.rs")));
    assert!(!binding.contains(&temp.path().join("outside.txt")));
    assert!(!binding.contains(&temp.path().join("project").join("..").join("other.txt")));
}

#[test]
fn cross_profile_workspace_locks_exclude_concurrent_writers() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).expect("dir");

    // "dev" acquires the write lock for the canonical directory.
    let lock = WorkspaceLock::acquire(&root).expect("first lock");
    // "prod" (any other profile/process) is refused for the same physical
    // directory — the lock is keyed by canonical identity, not by profile.
    let second = WorkspaceLock::acquire(&root);
    assert!(second.is_err(), "cross-profile writer must be excluded");
    drop(lock);

    // After release the lock is acquirable again.
    WorkspaceLock::acquire(&root)
        .expect("lock after release")
        .lock_path();

    // A different directory has its own lock; git common-dir locks are
    // separate from workspace locks even for the same path.
    let other = temp.path().join("other");
    std::fs::create_dir_all(&other).expect("dir");
    let ws = WorkspaceLock::acquire(&root).expect("ws lock");
    let git = WorkspaceLock::acquire_common_dir(&root.join(".git")).expect("git lock");
    let ws2 = WorkspaceLock::acquire(&other).expect("other ws lock");
    drop((ws, git, ws2));
}
