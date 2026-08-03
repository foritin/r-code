use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use r_code_core::dto::{AgentRun, FileChangeType, ReviewState, Task, TaskMode};
use r_code_store::{
    review_line_id, AgentRunRepository, ChangeService, Database, GitService,
    NewRunWorkspaceSnapshot, ReviewDiffLineKind, ReviewGitService, RollbackResult, TaskRepository,
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
        let blobs = tempfile::tempdir().unwrap();
        save_snapshot(&db, repo.path(), &run, &task);
        Self {
            db,
            blobs,
            repo,
            task,
            run,
        }
    }

    fn service(&self) -> ReviewGitService<'_> {
        ReviewGitService::new(&self.db, self.blobs.path().to_path_buf())
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

    fn finish(&self) {
        AgentRunRepository::new(&self.db)
            .update_review_state(&self.run.id, ReviewState::Answered)
            .unwrap();
    }
}

fn save_snapshot(db: &Database, repo: &Path, run: &AgentRun, task: &Task) {
    let git = GitService::new(repo.to_path_buf());
    let entry_index = git.index_snapshot().unwrap().unwrap();
    let entry_worktree = git.entry_snapshot().unwrap().unwrap();
    let entry_head = git.head_tree().unwrap();
    ChangeService::new(db, repo.join(".test-blobs"))
        .save_run_workspace_snapshot(NewRunWorkspaceSnapshot {
            run_id: &run.id,
            task_id: &task.id,
            repo_root: repo,
            workspace_root: repo,
            entry_head_tree: entry_head.as_deref(),
            entry_index_tree: &entry_index,
            entry_worktree_tree: &entry_worktree,
        })
        .unwrap();
}

#[tokio::test]
async fn review_excludes_git_ignored_and_generated_artifacts() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.repo.path().join("ignored")).unwrap();
    std::fs::create_dir_all(fixture.repo.path().join("test/tmp")).unwrap();
    std::fs::write(fixture.repo.path().join("ignored/data.txt"), b"noise\n").unwrap();
    std::fs::write(fixture.repo.path().join("test/tmp/run.log"), b"noise\n").unwrap();
    std::fs::write(fixture.repo.path().join("visible.txt"), b"review me\n").unwrap();
    fixture
        .record(
            "ignored/data.txt",
            FileChangeType::Create,
            None,
            Some(b"noise\n"),
        )
        .await;
    fixture
        .record(
            "test/tmp/run.log",
            FileChangeType::Create,
            None,
            Some(b"noise\n"),
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
    fixture.finish();

    let status = fixture.service().status(&fixture.task.id).unwrap();
    assert_eq!(
        status
            .paths
            .iter()
            .map(|path| path.path.as_str())
            .collect::<Vec<_>>(),
        vec!["visible.txt"]
    );
}

#[tokio::test]
async fn accept_is_persistent_idempotent_and_never_touches_the_git_index() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.repo.path().join("line.txt"),
        b"one\ninserted\ntwo\n",
    )
    .unwrap();
    std::fs::write(fixture.repo.path().join("file.txt"), b"after\n").unwrap();
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
    fixture.finish();

    let index_before = GitService::new(fixture.repo.path().to_path_buf())
        .index_snapshot()
        .unwrap();
    let line_id = review_line_id(ReviewDiffLineKind::Add, None, Some(2), "inserted");
    let service = fixture.service();
    service.status(&fixture.task.id).unwrap();
    let acceptance_started = Instant::now();
    let first = service
        .accept_line(&fixture.task.id, "line.txt", &line_id)
        .unwrap();
    assert!(
        acceptance_started.elapsed() < Duration::from_secs(1),
        "a materialized ledger decision must stay off the Git subprocess path"
    );
    assert_eq!(first.remaining_count, 1);
    let duplicate = service
        .accept_line(&fixture.task.id, "line.txt", &line_id)
        .unwrap();
    assert_eq!(duplicate.remaining_count, 1);
    assert_eq!(
        GitService::new(fixture.repo.path().to_path_buf())
            .index_snapshot()
            .unwrap(),
        index_before
    );

    service.accept_file(&fixture.task.id, "file.txt").unwrap();
    let reopened = fixture.service().status(&fixture.task.id).unwrap();
    assert_eq!(reopened.accepted_count, 2);
    assert_eq!(reopened.remaining_count, 0);
    assert_eq!(
        GitService::new(fixture.repo.path().to_path_buf())
            .index_snapshot()
            .unwrap(),
        index_before,
        "review decisions must not stage files"
    );
}

#[tokio::test]
async fn a_new_run_gets_a_fresh_review_session_for_the_same_path() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.path().join("file.txt"), b"after one\n").unwrap();
    fixture
        .record(
            "file.txt",
            FileChangeType::Modify,
            Some(b"before\n"),
            Some(b"after one\n"),
        )
        .await;
    fixture.finish();
    fixture
        .service()
        .accept_file(&fixture.task.id, "file.txt")
        .unwrap();

    let second_run = AgentRun::new(&fixture.task.id, "second-model");
    AgentRunRepository::new(&fixture.db)
        .create(&second_run)
        .unwrap();
    save_snapshot(&fixture.db, fixture.repo.path(), &second_run, &fixture.task);
    std::fs::write(fixture.repo.path().join("file.txt"), b"after two\n").unwrap();
    ChangeService::new(&fixture.db, fixture.blobs.path().to_path_buf())
        .record_snapshot_change(
            &second_run.id,
            &fixture.task.id,
            "file.txt",
            FileChangeType::Modify,
            Some(b"after one\n"),
            Some(b"after two\n"),
        )
        .await
        .unwrap();
    AgentRunRepository::new(&fixture.db)
        .update_review_state(&second_run.id, ReviewState::Answered)
        .unwrap();

    let status = fixture.service().status(&fixture.task.id).unwrap();
    assert_eq!(status.accepted_count, 0);
    assert_eq!(status.remaining_count, 1);

    let snapshot = fixture
        .service()
        .file_snapshot(&fixture.task.id, "file.txt")
        .unwrap()
        .unwrap();
    let result = ChangeService::new(&fixture.db, fixture.blobs.path().to_path_buf())
        .restore_snapshot_at(
            "file.txt",
            &fixture.repo.path().join("file.txt"),
            snapshot.before_hash.as_deref(),
            snapshot.after_hash.as_deref(),
        )
        .await
        .unwrap();
    assert!(matches!(result, RollbackResult::Restored { .. }));
    fixture
        .service()
        .reject_file(&fixture.task.id, "file.txt")
        .unwrap();
    assert_eq!(
        std::fs::read(fixture.repo.path().join("file.txt")).unwrap(),
        b"after one\n",
        "rejecting the second run must restore its entry snapshot, not the task's oldest baseline"
    );
}

#[tokio::test]
async fn rejecting_a_created_file_deletes_it_and_persists_the_decision() {
    let fixture = Fixture::new();
    let created = fixture.repo.path().join("created.txt");
    std::fs::write(&created, b"created\n").unwrap();
    fixture
        .record(
            "created.txt",
            FileChangeType::Create,
            None,
            Some(b"created\n"),
        )
        .await;
    fixture.finish();
    fixture.service().status(&fixture.task.id).unwrap();

    let rollback = ChangeService::new(&fixture.db, fixture.blobs.path().to_path_buf())
        .rollback_file_at(&fixture.task.id, "created.txt", &created)
        .await
        .unwrap();
    assert!(matches!(rollback, RollbackResult::Restored { .. }));
    fixture
        .service()
        .reject_file(&fixture.task.id, "created.txt")
        .unwrap();
    assert!(!created.exists());
    let status = fixture.service().status(&fixture.task.id).unwrap();
    assert_eq!(status.rejected_count, 1);
    assert_eq!(status.remaining_count, 0);
}

#[tokio::test]
async fn git_delivery_requires_an_explicit_stage_step() {
    let fixture = Fixture::new();
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
    fixture.finish();
    let service = fixture.service();
    service.accept_all(&fixture.task.id).unwrap();

    let before_stage = service.delivery_status(&fixture.task.id).unwrap();
    assert!(before_stage.can_stage);
    assert!(before_stage.staged_task_paths.is_empty());
    assert!(!before_stage.can_commit);

    let staged = service.stage_accepted(&fixture.task.id).unwrap();
    assert_eq!(staged.staged_task_paths, vec!["file.txt"]);
    assert!(!staged.can_stage);
    assert!(staged.can_commit);
    assert!(
        !git(fixture.repo.path(), &["diff", "--cached", "--name-only"]).contains("local-only.tmp")
    );
}

#[tokio::test]
async fn stage_detects_edits_made_after_acceptance() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.path().join("file.txt"), b"after\n").unwrap();
    fixture
        .record(
            "file.txt",
            FileChangeType::Modify,
            Some(b"before\n"),
            Some(b"after\n"),
        )
        .await;
    fixture.finish();
    let service = fixture.service();
    service.accept_all(&fixture.task.id).unwrap();

    std::fs::write(fixture.repo.path().join("file.txt"), b"edited later\n").unwrap();
    assert!(service.stage_accepted(&fixture.task.id).is_err());
    let status = service.status(&fixture.task.id).unwrap();
    assert_eq!(status.conflict_count, 1);
    assert!(GitService::new(fixture.repo.path().to_path_buf())
        .staged_paths()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn delivery_refuses_to_commit_with_unrelated_staged_content() {
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
    fixture.finish();
    let service = fixture.service();
    service.accept_all(&fixture.task.id).unwrap();
    service.stage_accepted(&fixture.task.id).unwrap();
    git(fixture.repo.path(), &["add", "foreign.txt"]);

    let status = service.delivery_status(&fixture.task.id).unwrap();
    assert_eq!(status.staged_other_paths, vec!["foreign.txt"]);
    assert!(!status.can_commit);
    assert!(service
        .commit_task(&fixture.task.id, "feat: unsafe")
        .is_err());
}
