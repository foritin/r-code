use std::path::Path;
use std::process::Command;

use r_code_core::dto::{AgentRun, FileChangeType, Task, TaskMode};
use r_code_store::{
    AgentRunRepository, ChangeService, Database, GitService, GitTreeChangeKind,
    NewRunWorkspaceSnapshot, TaskRepository,
};

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git should launch");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn committed_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(
        repo.path(),
        &["config", "user.email", "review@example.test"],
    );
    git(repo.path(), &["config", "user.name", "Review Test"]);
    std::fs::write(repo.path().join("modified.txt"), b"before\n").unwrap();
    std::fs::write(repo.path().join("deleted.txt"), b"remove me\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "baseline"]);
    repo
}

#[test]
fn git_tree_snapshots_include_exact_create_modify_delete_blobs() {
    let repo = committed_repo();
    let service = GitService::new(repo.path().to_path_buf());
    let entry_index = service.index_snapshot().unwrap().unwrap();
    let entry_worktree = service.entry_snapshot().unwrap().unwrap();
    assert_eq!(entry_index, entry_worktree);

    std::fs::write(repo.path().join("modified.txt"), b"after\n").unwrap();
    std::fs::remove_file(repo.path().join("deleted.txt")).unwrap();
    std::fs::write(repo.path().join("created.txt"), b"created\n").unwrap();

    let exit = service.entry_snapshot().unwrap().unwrap();
    let changes = service.tree_changes(&entry_worktree, &exit).unwrap();
    assert_eq!(changes.len(), 3);
    assert_eq!(
        changes
            .iter()
            .find(|change| change.path == "created.txt")
            .unwrap()
            .kind,
        GitTreeChangeKind::Added
    );
    assert_eq!(
        changes
            .iter()
            .find(|change| change.path == "modified.txt")
            .unwrap()
            .kind,
        GitTreeChangeKind::Modified
    );
    assert_eq!(
        changes
            .iter()
            .find(|change| change.path == "deleted.txt")
            .unwrap()
            .kind,
        GitTreeChangeKind::Deleted
    );
    assert_eq!(
        service
            .blob_at_tree(&entry_worktree, "modified.txt")
            .unwrap(),
        Some(b"before\n".to_vec())
    );
    assert_eq!(
        service.blob_at_tree(&exit, "modified.txt").unwrap(),
        Some(b"after\n".to_vec())
    );
    assert_eq!(
        service.blob_at_tree(&exit, "created.txt").unwrap(),
        Some(b"created\n".to_vec())
    );
    assert_eq!(service.blob_at_tree(&exit, "deleted.txt").unwrap(), None);
}

#[tokio::test]
async fn snapshot_change_materialization_is_idempotent_per_run_and_path() {
    let repo = committed_repo();
    let db = Database::open_in_memory().unwrap();
    let task = Task::new(
        Some(repo.path().to_string_lossy().into_owned()),
        "snapshot test",
        "record changes",
        TaskMode::Edit,
    );
    TaskRepository::new(&db).create(&task).unwrap();
    let run = AgentRun::new(&task.id, "test-model");
    AgentRunRepository::new(&db).create(&run).unwrap();

    let blobs = tempfile::tempdir().unwrap();
    let changes = ChangeService::new(&db, blobs.path().to_path_buf());
    let git = GitService::new(repo.path().to_path_buf());
    let entry_index = git.index_snapshot().unwrap().unwrap();
    let entry_worktree = git.entry_snapshot().unwrap().unwrap();
    let entry_head = git.head_tree().unwrap();
    changes
        .save_run_workspace_snapshot(NewRunWorkspaceSnapshot {
            run_id: &run.id,
            task_id: &task.id,
            repo_root: repo.path(),
            workspace_root: repo.path(),
            entry_head_tree: entry_head.as_deref(),
            entry_index_tree: &entry_index,
            entry_worktree_tree: &entry_worktree,
        })
        .unwrap();

    let first = changes
        .record_snapshot_change(
            &run.id,
            &task.id,
            "created.txt",
            FileChangeType::Create,
            None,
            Some(b"first\n"),
        )
        .await
        .unwrap();
    let second = changes
        .record_snapshot_change(
            &run.id,
            &task.id,
            "created.txt",
            FileChangeType::Create,
            None,
            Some(b"different retry content\n"),
        )
        .await
        .unwrap();

    assert_eq!(first.id, second.id);
    let listed = changes.list_changes(&task.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].after_hash, first.after_hash);

    changes
        .finalize_run_workspace_snapshot(&run.id, &entry_worktree)
        .unwrap();
    changes
        .finalize_run_workspace_snapshot(&run.id, "ignored-second-finalize")
        .unwrap();
    let stored_snapshot = changes
        .get_run_workspace_snapshot(&run.id)
        .unwrap()
        .unwrap();
    assert_eq!(stored_snapshot.entry_head_tree, entry_head);
    assert_eq!(
        stored_snapshot.exit_worktree_tree.as_deref(),
        Some(entry_worktree.as_str())
    );
}
