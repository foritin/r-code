use std::path::Path;
use std::process::Command;

use r_code_core::dto::{AgentRun, FileChangeType, Task, TaskMode};
use r_code_store::{
    review_line_id, AgentRunRepository, ChangeService, Database, GitService,
    NewRunWorkspaceSnapshot, ReviewDiffLineKind, ReviewGitService, TaskRepository,
};

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn committed_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(
        repo.path(),
        &["config", "user.email", "review@example.test"],
    );
    git(repo.path(), &["config", "user.name", "Review Test"]);
    git(repo.path(), &["config", "core.autocrlf", "false"]);
    std::fs::write(repo.path().join("line.txt"), b"one\ntwo\n").unwrap();
    std::fs::write(repo.path().join("file.txt"), b"before\n").unwrap();
    std::fs::write(repo.path().join("unrelated.txt"), b"untouched\n").unwrap();
    std::fs::write(repo.path().join(".gitignore"), b"ignored/\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "baseline"]);
    repo
}

struct Fixture {
    db: Database,
    blobs: tempfile::TempDir,
    repo: tempfile::TempDir,
    task: Task,
    run: AgentRun,
}

impl Fixture {
    fn new() -> Self {
        let repo = committed_repo();
        let db = Database::open_in_memory().unwrap();
        let task = Task::new(
            Some(repo.path().to_string_lossy().into_owned()),
            "review",
            "accept task changes",
            TaskMode::Edit,
        );
        TaskRepository::new(&db).create(&task).unwrap();
        let run = AgentRun::new(&task.id, "test-model");
        AgentRunRepository::new(&db).create(&run).unwrap();
        let git = GitService::new(repo.path().to_path_buf());
        let entry_index = git.index_snapshot().unwrap().unwrap();
        let entry_worktree = git.entry_snapshot().unwrap().unwrap();
        let entry_head = git.head_tree().unwrap();
        let blobs = tempfile::tempdir().unwrap();
        ChangeService::new(&db, blobs.path().to_path_buf())
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
        Self {
            db,
            blobs,
            repo,
            task,
            run,
        }
    }

    async fn record(
        &self,
        path: &str,
        kind: FileChangeType,
        before: Option<&[u8]>,
        after: Option<&[u8]>,
    ) {
        ChangeService::new(&self.db, self.blobs.path().to_path_buf())
            .record_snapshot_change(&self.run.id, &self.task.id, path, kind, before, after)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn excludes_git_ignored_and_no_longer_changed_paths_from_review() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.repo.path().join("ignored")).unwrap();
    std::fs::write(
        fixture.repo.path().join("ignored/generated.log"),
        b"runtime noise\n",
    )
    .unwrap();
    std::fs::write(fixture.repo.path().join("visible.txt"), b"review me\n").unwrap();
    fixture
        .record(
            "ignored/generated.log",
            FileChangeType::Create,
            None,
            Some(b"runtime noise\n"),
        )
        .await;
    fixture
        .record(
            "visible.txt",
            FileChangeType::Create,
            None,
            Some(b"review me\n"),
        )
        .await;
    fixture
        .record(
            "restored.txt",
            FileChangeType::Create,
            None,
            Some(b"already gone\n"),
        )
        .await;

    let service = ReviewGitService::new(&fixture.db);
    let status = service.status(&fixture.task.id).unwrap();
    assert_eq!(
        status
            .paths
            .iter()
            .map(|path| path.path.as_str())
            .collect::<Vec<_>>(),
        vec!["visible.txt"],
    );

    service.accept_all(&fixture.task.id).unwrap();
    let staged = git(fixture.repo.path(), &["diff", "--cached", "--name-only"]);
    assert!(staged.contains("visible.txt"));
    assert!(!staged.contains("ignored/generated.log"));
    assert!(!staged.contains("restored.txt"));
}

#[tokio::test]
async fn accepts_one_line_one_file_and_all_without_touching_unrelated_paths() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.repo.path().join("line.txt"),
        b"one\ninserted\ntwo\n",
    )
    .unwrap();
    std::fs::write(fixture.repo.path().join("file.txt"), b"after\n").unwrap();
    std::fs::write(fixture.repo.path().join("created.txt"), b"created\n").unwrap();
    std::fs::write(
        fixture.repo.path().join("unrelated.tmp"),
        b"not this task\n",
    )
    .unwrap();
    fixture
        .record(
            "line.txt",
            FileChangeType::Modify,
            Some(b"one\ntwo\n"),
            Some(b"one\ninserted\ntwo\n"),
        )
        .await;
    fixture
        .record(
            "file.txt",
            FileChangeType::Modify,
            Some(b"before\n"),
            Some(b"after\n"),
        )
        .await;
    fixture
        .record(
            "created.txt",
            FileChangeType::Create,
            None,
            Some(b"created\n"),
        )
        .await;

    let service = ReviewGitService::new(&fixture.db);
    let line = service
        .diff_lines(&fixture.task.id, "line.txt")
        .unwrap()
        .unwrap()
        .into_iter()
        .find(|line| line.kind == ReviewDiffLineKind::Add)
        .unwrap();
    let result = service
        .accept_line(&fixture.task.id, "line.txt", &line.line_id)
        .unwrap();
    assert_eq!(result.remaining_count, 2);
    let cached = git(fixture.repo.path(), &["diff", "--cached", "--", "line.txt"]);
    assert!(cached.contains("+inserted"));

    service.accept_file(&fixture.task.id, "file.txt").unwrap();
    let result = service.accept_all(&fixture.task.id).unwrap();
    assert!(result.fully_accepted);
    let names = git(fixture.repo.path(), &["diff", "--cached", "--name-only"]);
    assert!(names.contains("line.txt"));
    assert!(names.contains("file.txt"));
    assert!(names.contains("created.txt"));
    assert!(!names.contains("unrelated.tmp"));
    assert!(fixture.repo.path().join("unrelated.tmp").exists());
}

#[tokio::test]
async fn accepts_a_line_from_an_untracked_task_file() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.path().join("new.txt"), b"first\nsecond\n").unwrap();
    fixture
        .record(
            "new.txt",
            FileChangeType::Create,
            None,
            Some(b"first\nsecond\n"),
        )
        .await;
    let service = ReviewGitService::new(&fixture.db);
    let first = review_line_id(ReviewDiffLineKind::Add, None, Some(1), "first");
    service
        .accept_line(&fixture.task.id, "new.txt", &first)
        .unwrap();
    let cached = git(fixture.repo.path(), &["show", ":new.txt"]);
    assert_eq!(cached, "first\n");
    assert!(service.status(&fixture.task.id).unwrap().remaining_count > 0);
}

#[tokio::test]
async fn accepts_one_deleted_line_into_the_index() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.path().join("line.txt"), b"one\n").unwrap();
    fixture
        .record(
            "line.txt",
            FileChangeType::Modify,
            Some(b"one\ntwo\n"),
            Some(b"one\n"),
        )
        .await;

    let service = ReviewGitService::new(&fixture.db);
    let deleted = service
        .diff_lines(&fixture.task.id, "line.txt")
        .unwrap()
        .unwrap()
        .into_iter()
        .find(|line| line.kind == ReviewDiffLineKind::Del)
        .unwrap();
    let result = service
        .accept_line(&fixture.task.id, "line.txt", &deleted.line_id)
        .unwrap();
    assert!(result.fully_accepted);
    assert!(git(fixture.repo.path(), &["diff", "--cached", "--", "line.txt"]).contains("-two"));
}

#[tokio::test]
async fn refuses_a_path_that_was_dirty_before_the_run_and_preserves_the_index() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.path().join("line.txt"), b"user edit\n").unwrap();
    let git_service = GitService::new(fixture.repo.path().to_path_buf());
    let dirty_entry = git_service.entry_snapshot().unwrap().unwrap();
    fixture
        .db
        .conn()
        .unwrap()
        .execute(
            "UPDATE run_workspace_snapshots SET entry_worktree_tree = ?1 WHERE run_id = ?2",
            rusqlite::params![dirty_entry, fixture.run.id],
        )
        .unwrap();
    std::fs::write(fixture.repo.path().join("line.txt"), b"agent edit\n").unwrap();
    fixture
        .record(
            "line.txt",
            FileChangeType::Modify,
            Some(b"user edit\n"),
            Some(b"agent edit\n"),
        )
        .await;

    let index_before = git_service.index_snapshot().unwrap().unwrap();
    let service = ReviewGitService::new(&fixture.db);
    let status = service.status(&fixture.task.id).unwrap();
    assert!(status.paths[0].preexisting_dirty);
    assert!(service.accept_file(&fixture.task.id, "line.txt").is_err());
    assert_eq!(git_service.index_snapshot().unwrap().unwrap(), index_before);
}

#[tokio::test]
async fn uses_the_first_snapshot_for_each_path_in_a_multi_run_task() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.path().join("file.txt"), b"user between runs\n").unwrap();

    let second_run = AgentRun::new(&fixture.task.id, "second-model");
    AgentRunRepository::new(&fixture.db)
        .create(&second_run)
        .unwrap();
    let git_service = GitService::new(fixture.repo.path().to_path_buf());
    let entry_head = git_service.head_tree().unwrap();
    let entry_index = git_service.index_snapshot().unwrap().unwrap();
    let entry_worktree = git_service.entry_snapshot().unwrap().unwrap();
    let changes = ChangeService::new(&fixture.db, fixture.blobs.path().to_path_buf());
    changes
        .save_run_workspace_snapshot(NewRunWorkspaceSnapshot {
            run_id: &second_run.id,
            task_id: &fixture.task.id,
            repo_root: fixture.repo.path(),
            workspace_root: fixture.repo.path(),
            entry_head_tree: entry_head.as_deref(),
            entry_index_tree: &entry_index,
            entry_worktree_tree: &entry_worktree,
        })
        .unwrap();

    std::fs::write(fixture.repo.path().join("file.txt"), b"agent after user\n").unwrap();
    changes
        .record_snapshot_change(
            &second_run.id,
            &fixture.task.id,
            "file.txt",
            FileChangeType::Modify,
            Some(b"user between runs\n"),
            Some(b"agent after user\n"),
        )
        .await
        .unwrap();

    let status = ReviewGitService::new(&fixture.db)
        .status(&fixture.task.id)
        .unwrap();
    let file = status
        .paths
        .iter()
        .find(|path| path.path == "file.txt")
        .unwrap();
    assert!(file.preexisting_dirty);
    assert!(!file.safe_to_accept);
}

#[tokio::test]
async fn commits_only_task_paths_and_pushes_only_to_an_existing_upstream() {
    let fixture = Fixture::new();
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare", "-q"]);
    git(
        fixture.repo.path(),
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    git(fixture.repo.path(), &["push", "-q", "-u", "origin", "HEAD"]);

    std::fs::write(fixture.repo.path().join("file.txt"), b"after\n").unwrap();
    std::fs::write(fixture.repo.path().join("local-only.tmp"), b"preserve\n").unwrap();
    fixture
        .record(
            "file.txt",
            FileChangeType::Modify,
            Some(b"before\n"),
            Some(b"after\n"),
        )
        .await;

    let service = ReviewGitService::new(&fixture.db);
    service.accept_file(&fixture.task.id, "file.txt").unwrap();
    let suggested = service.suggest_commit_message(&fixture.task.id).unwrap();
    assert_eq!(suggested, "docs: update file");
    let commit = service
        .commit_task(&fixture.task.id, "feat: update reviewed file")
        .unwrap();
    assert_eq!(
        git(fixture.repo.path(), &["rev-parse", "HEAD"]).trim(),
        commit.sha
    );
    let post_commit_review = service.status(&fixture.task.id).unwrap();
    assert_eq!(
        post_commit_review.conflict_count, 0,
        "moving HEAD through the reviewed commit must not be mistaken for pre-run dirtiness"
    );
    assert!(fixture.repo.path().join("local-only.tmp").exists());
    git(fixture.repo.path(), &["add", "local-only.tmp"]);
    assert!(service.delivery_status(&fixture.task.id).unwrap().can_push);
    let pushed = service.push_task(&fixture.task.id).unwrap();
    assert_eq!(pushed.sha, commit.sha);
    assert_eq!(
        git(
            remote.path(),
            &["rev-parse", &format!("refs/heads/{}", pushed.branch)],
        )
        .trim(),
        commit.sha
    );
    assert!(
        git(fixture.repo.path(), &["diff", "--cached", "--name-only"]).contains("local-only.tmp"),
        "pushing an existing commit must not modify unrelated staged work"
    );
}

#[tokio::test]
async fn refuses_commit_when_the_index_contains_a_non_task_path() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.path().join("file.txt"), b"after\n").unwrap();
    std::fs::write(fixture.repo.path().join("foreign.txt"), b"foreign\n").unwrap();
    fixture
        .record(
            "file.txt",
            FileChangeType::Modify,
            Some(b"before\n"),
            Some(b"after\n"),
        )
        .await;
    let service = ReviewGitService::new(&fixture.db);
    service.accept_file(&fixture.task.id, "file.txt").unwrap();
    git(fixture.repo.path(), &["add", "foreign.txt"]);

    let status = service.delivery_status(&fixture.task.id).unwrap();
    assert_eq!(status.staged_other_paths, vec!["foreign.txt"]);
    assert!(!status.can_commit);
    assert!(service
        .commit_task(&fixture.task.id, "feat: unsafe")
        .is_err());
}

#[tokio::test]
async fn refuses_commit_on_detached_head_and_push_without_an_upstream() {
    let detached = Fixture::new();
    git(detached.repo.path(), &["checkout", "--detach", "-q"]);
    std::fs::write(detached.repo.path().join("file.txt"), b"after\n").unwrap();
    detached
        .record(
            "file.txt",
            FileChangeType::Modify,
            Some(b"before\n"),
            Some(b"after\n"),
        )
        .await;
    let detached_service = ReviewGitService::new(&detached.db);
    detached_service
        .accept_file(&detached.task.id, "file.txt")
        .unwrap();
    let detached_status = detached_service.delivery_status(&detached.task.id).unwrap();
    assert!(detached_status.branch.is_none());
    assert!(!detached_status.can_commit);
    assert!(detached_service
        .commit_task(&detached.task.id, "feat: detached")
        .is_err());

    let no_upstream = Fixture::new();
    std::fs::write(no_upstream.repo.path().join("file.txt"), b"after\n").unwrap();
    no_upstream
        .record(
            "file.txt",
            FileChangeType::Modify,
            Some(b"before\n"),
            Some(b"after\n"),
        )
        .await;
    let no_upstream_service = ReviewGitService::new(&no_upstream.db);
    no_upstream_service
        .accept_file(&no_upstream.task.id, "file.txt")
        .unwrap();
    no_upstream_service
        .commit_task(&no_upstream.task.id, "feat: no upstream")
        .unwrap();
    let status = no_upstream_service
        .delivery_status(&no_upstream.task.id)
        .unwrap();
    assert!(status.upstream.is_none());
    assert!(!status.can_push);
    assert!(no_upstream_service.push_task(&no_upstream.task.id).is_err());
}
