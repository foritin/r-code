use r_code_runtime::services::workspaces::{
    CandidateManifest, TaskWorkspaceBinding, WorkspaceError,
};
use std::path::Path;

fn bind(root: &Path) -> TaskWorkspaceBinding {
    TaskWorkspaceBinding::bind_local("task-candidate", root, &[]).unwrap()
}

#[cfg(windows)]
fn create_directory_link(target: &Path, link: &Path) {
    let link = link.to_string_lossy().replace('/', "\\");
    let target = target.to_string_lossy().replace('/', "\\");
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(output.status.success(), "junction fixture failed");
}

#[cfg(unix)]
fn create_directory_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn candidate_is_host_computed_and_live_drift_makes_it_stale_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::create_dir(temp.path().join(".git")).unwrap();
    std::fs::write(temp.path().join(".git/HEAD"), b"git metadata").unwrap();
    let source = temp.path().join("src/lib.rs");
    std::fs::write(&source, b"candidate-before").unwrap();
    let binding = bind(temp.path());
    let candidate = CandidateManifest::capture(&binding).unwrap();

    assert_ne!(candidate.candidate_id, "model-claimed-digest");
    assert_eq!(candidate.captured_files, 1);
    assert!(candidate.files.contains_key("src/lib.rs"));
    assert!(!candidate.files.keys().any(|path| path.starts_with(".git/")));
    assert_eq!(std::fs::read(&source).unwrap(), b"candidate-before");

    std::fs::write(&source, b"user-edited-after-capture").unwrap();
    assert!(matches!(
        candidate.verify_live(&binding),
        Err(WorkspaceError::Inconsistent { .. })
    ));
    assert_eq!(
        std::fs::read(&source).unwrap(),
        b"user-edited-after-capture"
    );
}

#[test]
fn candidate_capture_rejects_symlink_or_reparse_entries_even_when_target_is_inside() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("real")).unwrap();
    std::fs::write(workspace.path().join("real/file.txt"), b"inside").unwrap();
    create_directory_link(
        &workspace.path().join("real"),
        &workspace.path().join("linked"),
    );
    let error = CandidateManifest::capture(&bind(workspace.path())).unwrap_err();
    assert!(matches!(error, WorkspaceError::Io(_)));
    assert!(!format!("{error}").contains(&workspace.path().display().to_string()));
}

#[test]
fn candidate_capture_rejects_canonical_escape_and_preserves_external_bytes() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("sentinel.txt"), b"outside-user-bytes").unwrap();
    create_directory_link(outside.path(), &workspace.path().join("escape"));

    assert!(CandidateManifest::capture(&bind(workspace.path())).is_err());
    assert_eq!(
        std::fs::read(outside.path().join("sentinel.txt")).unwrap(),
        b"outside-user-bytes"
    );
}
