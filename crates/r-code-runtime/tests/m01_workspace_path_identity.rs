use r_code_runtime::services::workspaces::{PathIdentity, TaskWorkspaceBinding, WorkspaceError};
use std::fs;
use std::path::Path;

#[cfg(windows)]
fn create_directory_link(target: &Path, link: &Path) {
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("run mklink");
    assert!(
        output.status.success(),
        "create junction: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn bind(root: &Path) -> TaskWorkspaceBinding {
    TaskWorkspaceBinding::bind_local("task-m01", root, &[]).expect("bind temp workspace")
}

#[test]
fn existing_and_missing_paths_bind_target_and_nearest_parent_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let src = temp.path().join("src");
    fs::create_dir(&src).unwrap();
    let existing = src.join("lib.rs");
    fs::write(&existing, b"original bytes").unwrap();
    let binding = bind(temp.path());

    let existing_resolution = binding.resolve_path(r"src\lib.rs").unwrap();
    assert_eq!(existing_resolution.normalized_logical_path, "src/lib.rs");
    assert!(existing_resolution.existing_target_id.is_some());
    assert_eq!(fs::read(&existing).unwrap(), b"original bytes");

    let missing = binding.resolve_path("src/new/deep/out.rs").unwrap();
    assert_eq!(missing.normalized_logical_path, "src/new/deep/out.rs");
    assert!(missing.existing_target_id.is_none());
    assert_eq!(
        missing.canonical_nearest_existing_parent,
        fs::canonicalize(&src).unwrap()
    );
    assert!(
        !src.join("new").exists(),
        "path resolution wrote workspace bytes"
    );
    assert_eq!(fs::read(&existing).unwrap(), b"original bytes");
}

#[test]
fn create_delete_recreate_and_rename_invalidate_prior_identity() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("src")).unwrap();
    let binding = bind(temp.path());
    let target = temp.path().join("src/item.txt");

    let missing = binding.resolve_path("src/item.txt").unwrap();
    fs::write(&target, b"created").unwrap();
    assert_eq!(
        missing.revalidate(),
        Err(WorkspaceError::PathIdentityChanged)
    );

    let original = binding.resolve_path("src/item.txt").unwrap();
    let renamed = temp.path().join("src/renamed.txt");
    fs::rename(&target, &renamed).unwrap();
    assert_eq!(
        original.revalidate(),
        Err(WorkspaceError::PathIdentityChanged)
    );

    fs::hard_link(&renamed, temp.path().join("src/held-original.txt")).unwrap();
    fs::write(&target, b"replacement").unwrap();
    assert_eq!(
        original.revalidate(),
        Err(WorkspaceError::PathIdentityChanged)
    );

    let replacement = binding.resolve_path("src/item.txt").unwrap();
    fs::remove_file(&target).unwrap();
    assert_eq!(
        replacement.revalidate(),
        Err(WorkspaceError::PathIdentityChanged)
    );
    fs::write(&target, b"recreated").unwrap();
    assert_eq!(
        replacement.revalidate(),
        Err(WorkspaceError::PathIdentityChanged)
    );
}

#[test]
fn replacing_nearest_existing_parent_invalidates_missing_output() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("generated");
    fs::create_dir(&parent).unwrap();
    let binding = bind(temp.path());
    let output = binding.resolve_path("generated/nested/out.rs").unwrap();

    fs::rename(&parent, temp.path().join("generated-old")).unwrap();
    fs::create_dir(&parent).unwrap();
    assert_eq!(
        output.revalidate(),
        Err(WorkspaceError::PathIdentityChanged)
    );
}

#[test]
fn hardlinks_share_one_physical_target_identity() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("a.txt"), b"same file").unwrap();
    fs::hard_link(temp.path().join("a.txt"), temp.path().join("b.txt")).unwrap();
    let binding = bind(temp.path());
    let a = binding.resolve_path("a.txt").unwrap();
    let b = binding.resolve_path("b.txt").unwrap();
    assert_eq!(a.existing_target_id, b.existing_target_id);
}

#[cfg(unix)]
#[test]
fn unix_identity_uses_device_inode_and_symlink_escape_fails_closed() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("file.txt"), b"inside").unwrap();
    symlink(outside.path(), workspace.path().join("escape")).unwrap();
    let binding = bind(workspace.path());

    assert!(matches!(
        binding.resolve_path("file.txt").unwrap().existing_target_id,
        Some(PathIdentity::Unix { .. })
    ));
    let error = binding.resolve_path("escape/out.txt").unwrap_err();
    assert_eq!(error, WorkspaceError::PathEscapesWorkspace);
    assert!(!format!("{error:?} {error}").contains(&outside.path().display().to_string()));
}

#[cfg(windows)]
#[test]
fn windows_identity_is_case_folded_and_reparse_escape_fails_closed() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(workspace.path().join("CaseDir")).unwrap();
    fs::write(workspace.path().join("CaseDir/File.TXT"), b"inside").unwrap();
    create_directory_link(outside.path(), &workspace.path().join("escape"));
    let binding = bind(workspace.path());

    let resolved = binding.resolve_path("casedir/file.txt").unwrap();
    assert_eq!(resolved.normalized_logical_key, "casedir/file.txt");
    assert!(matches!(
        resolved.existing_target_id,
        Some(PathIdentity::Windows { .. }) | Some(PathIdentity::Fallback { .. })
    ));
    let error = binding.resolve_path("escape/out.txt").unwrap_err();
    assert_eq!(error, WorkspaceError::PathEscapesWorkspace);
    assert!(!format!("{error:?} {error}").contains(&outside.path().display().to_string()));
}

#[test]
fn swapping_a_safe_parent_for_an_external_link_is_rejected_on_revalidation() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let safe = workspace.path().join("safe");
    fs::create_dir(&safe).unwrap();
    let binding = bind(workspace.path());
    let resolved = binding.resolve_path("safe/out.txt").unwrap();
    fs::rename(&safe, workspace.path().join("safe-old")).unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &safe).unwrap();
    #[cfg(windows)]
    create_directory_link(outside.path(), &safe);

    assert_eq!(
        resolved.revalidate(),
        Err(WorkspaceError::PathEscapesWorkspace)
    );
}

#[test]
fn invalid_logical_paths_never_embed_absolute_external_input_in_errors() {
    let workspace = tempfile::tempdir().unwrap();
    let binding = bind(workspace.path());
    let external_secret = if cfg!(windows) {
        r"C:\outside\private-token"
    } else {
        "/outside/private-token"
    };
    let error = binding.resolve_path(external_secret).unwrap_err();
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains(external_secret));
    assert!(!rendered.contains("private-token"));
}
