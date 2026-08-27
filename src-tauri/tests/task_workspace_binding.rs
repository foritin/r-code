use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use r_code_core::dto::{ProjectAccessMode, Task, TaskMode};
use r_code_core::error::ProductError;
use r_code_host::task_workspace_binding::{resolve_task_workspace_binding, TaskWorkspaceBinding};
use r_code_store::{Database, WorkspaceService};
use tempfile::TempDir;

struct GitFixture {
    _container: TempDir,
    repo: PathBuf,
}

impl GitFixture {
    fn new() -> Self {
        let container = TempDir::new().expect("create git fixture container");
        let repo = container.path().join("repo");
        fs::create_dir(&repo).expect("create repository directory");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@r-code.dev"]);
        git(&repo, &["config", "user.name", "R-Code Test"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["config", "core.autocrlf", "false"]);
        fs::write(repo.join("tracked.txt"), "initial\n").expect("write fixture file");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "initial"]);
        Self {
            _container: container,
            repo,
        }
    }

    fn linked_worktree_at(&self, path: &Path, branch_suffix: &str) -> PathBuf {
        let branch = format!("r-code/{branch_suffix}");
        let path_arg = path.to_string_lossy().into_owned();
        git(
            &self.repo,
            &["worktree", "add", "-b", &branch, &path_arg, "HEAD"],
        );
        path.to_path_buf()
    }

    fn linked_worktree(&self, name: &str) -> PathBuf {
        let path = self._container.path().join(name);
        self.linked_worktree_at(&path, name)
    }
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()))
}

fn register_workspace(db: &Database, path: &Path, access_mode: ProjectAccessMode) -> String {
    let canonical_path = canonical(path).display().to_string();
    let service = WorkspaceService::new(db);
    service
        .open(&canonical_path, "Binding fixture")
        .expect("register workspace");
    service
        .set_access_mode(&canonical_path, access_mode)
        .expect("set workspace access mode");
    canonical_path
}

fn task_for(workspace_path: String, worktree_path: Option<&Path>) -> Task {
    let mut task = Task::new(Some(workspace_path), "binding", "binding", TaskMode::Edit);
    task.id = "task-workspace-binding-test".to_string();
    task.worktree_path = worktree_path.map(|path| path.display().to_string());
    task
}

fn expect_invalid_worktree(db: &Database, task: &Task) -> String {
    match resolve_task_workspace_binding(db, task) {
        Err(ProductError::UserFacing(error)) => {
            assert_eq!(error.code, "worktree.binding_invalid");
            assert_eq!(
                error
                    .args
                    .get("task_id")
                    .and_then(serde_json::Value::as_str),
                Some(task.id.as_str())
            );
            error
                .debug_detail
                .expect("invalid worktree retains technical detail outside the visible message")
        }
        Err(error) => panic!("expected structured worktree binding error, got {error:?}"),
        Ok(binding) => panic!("invalid worktree resolved instead of failing closed: {binding:?}"),
    }
}

#[test]
fn local_workspace_uses_registered_root_and_access_mode() {
    let workspace = TempDir::new().expect("create local workspace");
    let db = Database::open_in_memory().expect("open database");
    let workspace_path = register_workspace(&db, workspace.path(), ProjectAccessMode::FullAccess);
    let task = task_for(workspace_path, None);

    let binding = resolve_task_workspace_binding(&db, &task).expect("resolve local workspace");
    match binding {
        TaskWorkspaceBinding::Local(local) => {
            assert_eq!(local.root, canonical(workspace.path()));
            assert_eq!(local.registered_workspace_root, Some(local.root.clone()));
            assert_eq!(local.access_mode, ProjectAccessMode::FullAccess);
        }
        TaskWorkspaceBinding::ManagedWorktree(worktree) => {
            panic!("local task unexpectedly resolved as worktree: {worktree:?}")
        }
    }
}

#[test]
fn linked_worktree_resolves_only_after_git_topology_validation() {
    let fixture = GitFixture::new();
    let worktree = fixture.linked_worktree("valid-worktree");
    let db = Database::open_in_memory().expect("open database");
    let workspace_path = register_workspace(&db, &fixture.repo, ProjectAccessMode::RiskBased);
    let task = task_for(workspace_path, Some(&worktree));

    let binding = resolve_task_workspace_binding(&db, &task).expect("resolve linked worktree");
    match binding {
        TaskWorkspaceBinding::ManagedWorktree(managed) => {
            assert_eq!(managed.root, canonical(&worktree));
            assert_eq!(managed.registered_workspace_root, canonical(&fixture.repo));
            assert_eq!(managed.repo_root, canonical(&fixture.repo));
            assert_eq!(managed.common_dir, canonical(&fixture.repo.join(".git")));
            assert_eq!(managed.access_mode, ProjectAccessMode::RiskBased);
        }
        TaskWorkspaceBinding::Local(local) => {
            panic!("worktree task silently fell back to local workspace: {local:?}")
        }
    }
}

#[test]
fn missing_worktree_path_fails_closed_without_local_fallback() {
    let fixture = GitFixture::new();
    let db = Database::open_in_memory().expect("open database");
    let workspace_path = register_workspace(&db, &fixture.repo, ProjectAccessMode::RequestApproval);
    let missing = fixture._container.path().join("missing-worktree");
    let task = task_for(workspace_path, Some(&missing));

    let detail = expect_invalid_worktree(&db, &task);
    assert!(
        detail.contains("does not exist"),
        "unexpected detail: {detail}"
    );
}

#[test]
fn registered_repository_root_cannot_masquerade_as_linked_worktree() {
    let fixture = GitFixture::new();
    let db = Database::open_in_memory().expect("open database");
    let workspace_path = register_workspace(&db, &fixture.repo, ProjectAccessMode::RequestApproval);
    let task = task_for(workspace_path, Some(&fixture.repo));

    let detail = expect_invalid_worktree(&db, &task);
    assert!(
        detail.contains("linked Git worktree") || detail.contains("registered repository root"),
        "unexpected detail: {detail}"
    );
}

#[test]
fn linked_worktree_from_another_repository_is_rejected() {
    let registered = GitFixture::new();
    let foreign = GitFixture::new();
    let foreign_worktree = foreign.linked_worktree("foreign-worktree");
    let db = Database::open_in_memory().expect("open database");
    let workspace_path =
        register_workspace(&db, &registered.repo, ProjectAccessMode::RequestApproval);
    let task = task_for(workspace_path, Some(&foreign_worktree));

    let detail = expect_invalid_worktree(&db, &task);
    assert!(
        detail.contains("not registered") || detail.contains("different Git repository"),
        "unexpected detail: {detail}"
    );
}

#[test]
fn deleted_linked_worktree_fails_closed_even_if_git_registration_remains() {
    let fixture = GitFixture::new();
    let worktree = fixture.linked_worktree("deleted-worktree");
    let db = Database::open_in_memory().expect("open database");
    let workspace_path = register_workspace(&db, &fixture.repo, ProjectAccessMode::RequestApproval);
    let task = task_for(workspace_path, Some(&worktree));
    fs::remove_dir_all(&worktree).expect("delete linked worktree directory");

    let detail = expect_invalid_worktree(&db, &task);
    assert!(
        detail.contains("does not exist"),
        "unexpected detail: {detail}"
    );
}

#[test]
fn replacing_registered_worktree_with_foreign_worktree_fails_common_dir_check() {
    let registered = GitFixture::new();
    let foreign = GitFixture::new();
    let replacement_path = registered._container.path().join("replace-me");
    registered.linked_worktree_at(&replacement_path, "original");

    let db = Database::open_in_memory().expect("open database");
    let workspace_path =
        register_workspace(&db, &registered.repo, ProjectAccessMode::RequestApproval);
    let task = task_for(workspace_path, Some(&replacement_path));

    fs::remove_dir_all(&replacement_path).expect("remove original worktree directory");
    foreign.linked_worktree_at(&replacement_path, "replacement");

    let detail = expect_invalid_worktree(&db, &task);
    assert!(
        detail.contains("different Git repository"),
        "replacement should reach the common-dir mismatch guard: {detail}"
    );
}
