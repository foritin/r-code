use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use chrono::Utc;
use r_code_core::dto::{AgentRun, Task, TaskMode};
use r_code_core::error::ProductError;
use r_code_store::{
    AgentRunRepository, Database, EnhancedReviewTarget, FinishPlanWriteInput,
    OsPlanReviewFileSystem, PathCoordinator, PlanReviewFileSystem, PlanReviewStore, TaskRepository,
    PLAN_REVIEW_FEATURE_NOT_TERMINAL, PLAN_REVIEW_SCOPE_CONFLICT,
};
use rusqlite::params;
use tempfile::TempDir;

struct Fixture {
    db: Database,
    db_path: Option<PathBuf>,
    _temp: TempDir,
    workspace: PathBuf,
    blobs: PathBuf,
    task: Task,
    run: AgentRun,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let blobs = temp.path().join("blobs");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&blobs).unwrap();
        let db = Database::open_in_memory().unwrap();
        let task = Task::new(
            Some(workspace.to_string_lossy().into_owned()),
            "Plan review",
            "test enhanced review",
            TaskMode::Edit,
        );
        TaskRepository::new(&db).create(&task).unwrap();
        let run = AgentRun::new(&task.id, "test-model");
        AgentRunRepository::new(&db).create(&run).unwrap();
        Self {
            db,
            db_path: None,
            _temp: temp,
            workspace,
            blobs,
            task,
            run,
        }
    }

    fn new_file_backed() -> Self {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let blobs = temp.path().join("blobs");
        let db_path = temp.path().join("r-code.db");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&blobs).unwrap();
        let db = Database::open(&db_path).unwrap();
        let task = Task::new(
            Some(workspace.to_string_lossy().into_owned()),
            "Plan review",
            "test enhanced review",
            TaskMode::Edit,
        );
        TaskRepository::new(&db).create(&task).unwrap();
        let run = AgentRun::new(&task.id, "test-model");
        AgentRunRepository::new(&db).create(&run).unwrap();
        Self {
            db,
            db_path: Some(db_path),
            _temp: temp,
            workspace,
            blobs,
            task,
            run,
        }
    }

    fn store(&self) -> PlanReviewStore<'_> {
        PlanReviewStore::new(&self.db, self.blobs.clone())
    }

    fn store_with_fs(&self, fs: Arc<dyn PlanReviewFileSystem>) -> PlanReviewStore<'_> {
        PlanReviewStore::with_dependencies(
            &self.db,
            self.blobs.clone(),
            PathCoordinator::default(),
            fs,
        )
    }

    fn insert_plan(&self, plan_id: &str, state: &str, items: &[(&str, &str)]) {
        let now = Utc::now().to_rfc3339();
        let conn = self.db.conn().unwrap();
        conn.execute(
            "INSERT INTO plans (
                 id, task_id, revision, state, approved_revision, created_at, updated_at,
                 approved_at
             ) VALUES (?1, ?2, 1, ?3, 1, ?4, ?4, ?4)",
            params![plan_id, self.task.id, state, now],
        )
        .unwrap();
        for (ordinal, (item_id, item_state)) in items.iter().enumerate() {
            conn.execute(
                "INSERT INTO plan_items (
                     id, plan_id, revision, ordinal, title, description, state,
                     created_at, updated_at
                 ) VALUES (?1, ?2, 1, ?3, ?4, ?4, ?5, ?6, ?6)",
                params![
                    item_id,
                    plan_id,
                    ordinal as i64,
                    format!("Feature {item_id}"),
                    item_state,
                    now,
                ],
            )
            .unwrap();
        }
    }

    fn set_item_state(&self, plan_id: &str, item_id: &str, state: &str) {
        self.db
            .conn()
            .unwrap()
            .execute(
                "UPDATE plan_items SET state = ?1, updated_at = ?2
                 WHERE plan_id = ?3 AND revision = 1 AND id = ?4",
                params![state, Utc::now().to_rfc3339(), plan_id, item_id],
            )
            .unwrap();
    }

    fn complete_plan(&self, plan_id: &str) {
        self.db
            .conn()
            .unwrap()
            .execute(
                "UPDATE plans SET state = 'completed', updated_at = ?1 WHERE id = ?2",
                params![Utc::now().to_rfc3339(), plan_id],
            )
            .unwrap();
    }

    async fn tracked_write(
        &self,
        store: &PlanReviewStore<'_>,
        relative: &str,
        content: &str,
        tool_call_id: &str,
    ) {
        let guard = store
            .begin_feature_write(
                &self.workspace,
                &self.task.id,
                &self.run.id,
                Path::new(relative),
            )
            .await
            .unwrap();
        std::fs::write(guard.path(), content).unwrap();
        store
            .finish_feature_write(
                guard,
                FinishPlanWriteInput {
                    tool_call_id: tool_call_id.to_string(),
                },
            )
            .unwrap();
    }

    fn target(&self, plan_id: &str, item_id: &str, path: Option<&str>) -> EnhancedReviewTarget {
        EnhancedReviewTarget {
            task_id: self.task.id.clone(),
            plan_id: plan_id.to_string(),
            plan_revision: 1,
            item_id: item_id.to_string(),
            path: path.map(str::to_string),
        }
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_path_case_variants_share_one_coordinator_lock() {
    use tokio::sync::oneshot;
    use tokio::time::{timeout, Duration};

    let coordinator = PathCoordinator::default();
    let first = coordinator
        .acquire([PathBuf::from(r"C:\R-Code\Shared.txt")])
        .await
        .unwrap();
    let contender = coordinator.clone();
    let (acquired_tx, mut acquired_rx) = oneshot::channel();
    let waiter = tokio::spawn(async move {
        let _second = contender
            .acquire([PathBuf::from(r"c:\r-code\shared.TXT")])
            .await
            .unwrap();
        let _ = acquired_tx.send(());
    });

    assert!(
        timeout(Duration::from_millis(50), &mut acquired_rx)
            .await
            .is_err(),
        "case-only path variants must not acquire separate mutexes on Windows"
    );
    drop(first);
    timeout(Duration::from_secs(1), &mut acquired_rx)
        .await
        .expect("second path should acquire after the first lease drops")
        .unwrap();
    waiter.await.unwrap();
}

#[tokio::test]
async fn rejecting_interleaved_feature_lines_preserves_later_feature() {
    let fixture = Fixture::new();
    fixture.insert_plan(
        "plan",
        "executing",
        &[("feature-a", "in_progress"), ("feature-b", "pending")],
    );
    let file = fixture.workspace.join("shared.txt");
    std::fs::write(&file, "one\ntwo\nthree\nfour\n").unwrap();
    let store = fixture.store();
    fixture
        .tracked_write(
            &store,
            "shared.txt",
            "ONE-A\ntwo\nTHREE-A\nfour\n",
            "write-a",
        )
        .await;
    fixture.set_item_state("plan", "feature-a", "completed");
    fixture.set_item_state("plan", "feature-b", "in_progress");
    fixture
        .tracked_write(
            &store,
            "shared.txt",
            "ONE-A\nTWO-B\nTHREE-A\nFOUR-B\n",
            "write-b",
        )
        .await;
    fixture.set_item_state("plan", "feature-b", "completed");

    store
        .reject_file(
            &fixture.workspace,
            &fixture.target("plan", "feature-a", Some("shared.txt")),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "one\nTWO-B\nthree\nFOUR-B\n"
    );

    store
        .reject_file(
            &fixture.workspace,
            &fixture.target("plan", "feature-b", Some("shared.txt")),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        "one\ntwo\nthree\nfour\n",
        "rejecting B after A must return to baseline without reintroducing A"
    );
}

#[tokio::test]
async fn conflicting_rejection_fails_closed_without_writing() {
    let fixture = Fixture::new();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    let file = fixture.workspace.join("shared.txt");
    std::fs::write(&file, "base\n").unwrap();
    let store = fixture.store();
    fixture
        .tracked_write(&store, "shared.txt", "feature\n", "write-a")
        .await;
    fixture.set_item_state("plan", "feature-a", "completed");
    std::fs::write(&file, "external\n").unwrap();

    let error = store
        .reject_file(
            &fixture.workspace,
            &fixture.target("plan", "feature-a", Some("shared.txt")),
        )
        .await
        .expect_err("conflicting rejection must fail closed");
    assert!(matches!(error, ProductError::RollbackError(_)));
    assert_eq!(std::fs::read_to_string(file).unwrap(), "external\n");
}

#[derive(Default)]
struct FailingFileSystem {
    writes: AtomicUsize,
    fail_on: HashSet<usize>,
}

impl FailingFileSystem {
    fn new(fail_on: impl IntoIterator<Item = usize>) -> Self {
        Self {
            writes: AtomicUsize::new(0),
            fail_on: fail_on.into_iter().collect(),
        }
    }
}

impl PlanReviewFileSystem for FailingFileSystem {
    fn read_snapshot(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        OsPlanReviewFileSystem.read_snapshot(path)
    }

    fn write_snapshot(&self, path: &Path, content: Option<&[u8]>) -> io::Result<()> {
        let call = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_on.contains(&call) {
            return Err(io::Error::other(format!("injected write failure {call}")));
        }
        OsPlanReviewFileSystem.write_snapshot(path, content)
    }
}

struct BlockingReadFileSystem {
    reads: AtomicUsize,
    block_on_read: usize,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl BlockingReadFileSystem {
    fn new(block_on_read: usize, entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) -> Self {
        Self {
            reads: AtomicUsize::new(0),
            block_on_read,
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(release),
        }
    }
}

impl PlanReviewFileSystem for BlockingReadFileSystem {
    fn read_snapshot(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let call = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.block_on_read {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                entered
                    .send(())
                    .map_err(|error| io::Error::other(error.to_string()))?;
            }
            self.release
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        OsPlanReviewFileSystem.read_snapshot(path)
    }

    fn write_snapshot(&self, path: &Path, content: Option<&[u8]>) -> io::Result<()> {
        OsPlanReviewFileSystem.write_snapshot(path, content)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_feature_rejection_blocks_racing_file_accept_across_connections() {
    let fixture = Fixture::new_file_backed();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    let file = fixture.workspace.join("shared.txt");
    std::fs::write(&file, "baseline\n").unwrap();
    let capture_store = fixture.store();
    fixture
        .tracked_write(&capture_store, "shared.txt", "feature\n", "write-a")
        .await;
    fixture.set_item_state("plan", "feature-a", "completed");

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    // The second read happens after the rejection journal has acquired its durable active claim.
    let reject_store = fixture.store_with_fs(Arc::new(BlockingReadFileSystem::new(
        2, entered_tx, release_rx,
    )));
    let second_db = Database::open(fixture.db_path.as_ref().unwrap()).unwrap();
    let accept_target = fixture.target("plan", "feature-a", Some("shared.txt"));
    let blobs = fixture.blobs.clone();
    let accept = std::thread::spawn(move || {
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let result = PlanReviewStore::new(&second_db, blobs).accept_file(&accept_target);
        release_tx.send(()).unwrap();
        result
    });

    let rejected = reject_store
        .reject_feature(
            &fixture.workspace,
            &fixture.target("plan", "feature-a", None),
        )
        .await
        .unwrap();
    let accept_error = accept.join().unwrap().unwrap_err();
    assert!(accept_error
        .to_string()
        .contains(PLAN_REVIEW_SCOPE_CONFLICT));
    assert_eq!(rejected.decision.decision.as_str(), "rejected");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "baseline\n");

    let conn = fixture.db.conn().unwrap();
    let decisions: Vec<(String, String)> = conn
        .prepare(
            "SELECT scope, decision FROM plan_review_decisions
             WHERE plan_id = 'plan' AND item_id = 'feature-a' ORDER BY scope",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(decisions, vec![("feature".into(), "rejected".into())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn racing_file_accept_blocks_feature_rejection_before_it_writes() {
    let fixture = Fixture::new_file_backed();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    let file = fixture.workspace.join("shared.txt");
    std::fs::write(&file, "baseline\n").unwrap();
    let capture_store = fixture.store();
    fixture
        .tracked_write(&capture_store, "shared.txt", "feature\n", "write-a")
        .await;
    fixture.set_item_state("plan", "feature-a", "completed");

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    // The first read pauses before the rejection journal can claim the feature.
    let reject_store = fixture.store_with_fs(Arc::new(BlockingReadFileSystem::new(
        1, entered_tx, release_rx,
    )));
    let second_db = Database::open(fixture.db_path.as_ref().unwrap()).unwrap();
    let accept_target = fixture.target("plan", "feature-a", Some("shared.txt"));
    let blobs = fixture.blobs.clone();
    let accept = std::thread::spawn(move || {
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let result = PlanReviewStore::new(&second_db, blobs).accept_file(&accept_target);
        release_tx.send(()).unwrap();
        result
    });

    let reject_error = reject_store
        .reject_feature(
            &fixture.workspace,
            &fixture.target("plan", "feature-a", None),
        )
        .await
        .unwrap_err();
    let accepted = accept.join().unwrap().unwrap();
    assert_eq!(accepted.decision.as_str(), "accepted");
    assert!(reject_error
        .to_string()
        .contains(PLAN_REVIEW_SCOPE_CONFLICT));
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        "feature\n",
        "the losing rejection must fail before mutating the file"
    );

    let conn = fixture.db.conn().unwrap();
    let decisions: Vec<(String, String)> = conn
        .prepare(
            "SELECT scope, decision FROM plan_review_decisions
             WHERE plan_id = 'plan' AND item_id = 'feature-a' ORDER BY scope",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(decisions, vec![("file".into(), "accepted".into())]);
}

#[tokio::test]
async fn multi_file_failure_is_recovered_from_durable_journal() {
    let fixture = Fixture::new();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    std::fs::write(fixture.workspace.join("a.txt"), "a0\n").unwrap();
    std::fs::write(fixture.workspace.join("b.txt"), "b0\n").unwrap();
    let capture_store = fixture.store();
    fixture
        .tracked_write(&capture_store, "a.txt", "a-feature\n", "write-a")
        .await;
    fixture
        .tracked_write(&capture_store, "b.txt", "b-feature\n", "write-b")
        .await;
    fixture.set_item_state("plan", "feature-a", "completed");

    // Apply a.txt succeeds, applying b.txt fails, and the immediate a.txt rollback also fails.
    let failing = fixture.store_with_fs(Arc::new(FailingFileSystem::new([2, 3])));
    assert!(failing
        .reject_feature(
            &fixture.workspace,
            &fixture.target("plan", "feature-a", None),
        )
        .await
        .is_err());

    let recovery = fixture.store().recover_pending().await.unwrap();
    assert_eq!(recovery.recovered_operation_ids.len(), 1);
    assert!(recovery.conflicted_operation_ids.is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("a.txt")).unwrap(),
        "a-feature\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("b.txt")).unwrap(),
        "b-feature\n"
    );
}

#[tokio::test]
async fn writer_lease_blocks_reject_until_event_is_durable() {
    let fixture = Fixture::new();
    fixture.insert_plan(
        "plan",
        "executing",
        &[("feature-a", "completed"), ("feature-b", "in_progress")],
    );
    let file = fixture.workspace.join("shared.txt");
    std::fs::write(&file, "base\nsecond\n").unwrap();
    let store = fixture.store();

    // Seed feature A through the same event contract, then return feature B to in-progress.
    fixture.set_item_state("plan", "feature-a", "in_progress");
    fixture.set_item_state("plan", "feature-b", "pending");
    fixture
        .tracked_write(&store, "shared.txt", "feature-a\nsecond\n", "write-a")
        .await;
    fixture.set_item_state("plan", "feature-a", "completed");
    fixture.set_item_state("plan", "feature-b", "in_progress");

    let guard = store
        .begin_feature_write(
            &fixture.workspace,
            &fixture.task.id,
            &fixture.run.id,
            Path::new("shared.txt"),
        )
        .await
        .unwrap();
    std::fs::write(guard.path(), "feature-a\nfeature-b\n").unwrap();
    let target = fixture.target("plan", "feature-a", Some("shared.txt"));
    let mut reject = Box::pin(store.reject_file(&fixture.workspace, &target));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(40), &mut reject)
            .await
            .is_err()
    );

    store
        .finish_feature_write(
            guard,
            FinishPlanWriteInput {
                tool_call_id: "write-b".to_string(),
            },
        )
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), &mut reject)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.changed_paths, vec!["shared.txt"]);
}

#[tokio::test]
async fn ordinary_direct_write_is_serialized_with_feature_rejection() {
    use tokio::time::{timeout, Duration};

    let fixture = Fixture::new();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    let file = fixture.workspace.join("shared.txt");
    std::fs::write(&file, "baseline\n").unwrap();
    let store = fixture.store();
    fixture
        .tracked_write(&store, "shared.txt", "feature\n", "write-a")
        .await;
    fixture.set_item_state("plan", "feature-a", "completed");

    let direct = store
        .begin_coordinated_write(&fixture.workspace, &fixture.task.id, &file)
        .await
        .unwrap();
    // This models a later ordinary run. It may write the same bytes, but must retain the path
    // lease until its inner tool returns.
    std::fs::write(direct.path(), "feature\n").unwrap();
    let target = fixture.target("plan", "feature-a", Some("shared.txt"));
    let mut reject = Box::pin(store.reject_file(&fixture.workspace, &target));
    assert!(
        timeout(Duration::from_millis(50), &mut reject)
            .await
            .is_err(),
        "feature rejection must wait for a later ordinary write lease"
    );

    drop(direct);
    timeout(Duration::from_secs(1), reject)
        .await
        .expect("rejection should resume after direct write exits")
        .unwrap();
    assert_eq!(std::fs::read_to_string(file).unwrap(), "baseline\n");
}

#[tokio::test]
async fn accept_is_ledger_only_and_does_not_touch_file() {
    let fixture = Fixture::new();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    let file = fixture.workspace.join("accepted.txt");
    std::fs::write(&file, "before\n").unwrap();
    let store = fixture.store();
    fixture
        .tracked_write(&store, "accepted.txt", "after\n", "write-a")
        .await;
    fixture.set_item_state("plan", "feature-a", "completed");
    let before = std::fs::read(&file).unwrap();

    let decision = store
        .accept_file(&fixture.target("plan", "feature-a", Some("accepted.txt")))
        .unwrap();
    assert_eq!(decision.decision.as_str(), "accepted");
    assert_eq!(std::fs::read(file).unwrap(), before);
}

#[tokio::test]
async fn list_current_excludes_completed_older_plan() {
    let fixture = Fixture::new();
    let store = fixture.store();
    fixture.insert_plan("old-plan", "executing", &[("old-feature", "in_progress")]);
    std::fs::write(fixture.workspace.join("old.txt"), "before\n").unwrap();
    fixture
        .tracked_write(&store, "old.txt", "old\n", "old-write")
        .await;
    fixture.set_item_state("old-plan", "old-feature", "completed");
    fixture.complete_plan("old-plan");

    fixture.insert_plan("new-plan", "executing", &[("new-feature", "in_progress")]);
    std::fs::write(fixture.workspace.join("new.txt"), "before\n").unwrap();
    fixture
        .tracked_write(&store, "new.txt", "new\n", "new-write")
        .await;
    fixture.set_item_state("new-plan", "new-feature", "completed");

    let view = store.list_current(&fixture.task.id).unwrap().unwrap();
    assert_eq!(view.plan_id, "new-plan");
    assert_eq!(view.groups.len(), 1);
    assert_eq!(view.groups[0].item_id, "new-feature");
    assert_eq!(view.groups[0].files[0].path, "new.txt");
}

#[tokio::test]
async fn active_feature_cannot_be_decided_but_terminal_feature_can() {
    let fixture = Fixture::new();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    std::fs::write(fixture.workspace.join("file.txt"), "before\n").unwrap();
    let store = fixture.store();
    fixture
        .tracked_write(&store, "file.txt", "after\n", "write-a")
        .await;
    let target = fixture.target("plan", "feature-a", Some("file.txt"));

    let error = store.accept_file(&target).unwrap_err();
    assert!(error.to_string().contains(PLAN_REVIEW_FEATURE_NOT_TERMINAL));
    fixture.set_item_state("plan", "feature-a", "blocked");
    let error = store.accept_file(&target).unwrap_err();
    assert!(error.to_string().contains(PLAN_REVIEW_FEATURE_NOT_TERMINAL));
    fixture.set_item_state("plan", "feature-a", "completed");
    assert!(store.accept_file(&target).is_ok());
}

#[test]
fn host_helpers_validate_run_and_resolve_current_ownership() {
    let fixture = Fixture::new();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    let store = fixture.store();
    let plan = store
        .current_plan_for_task(&fixture.task.id)
        .unwrap()
        .unwrap();
    assert_eq!(plan.plan_id, "plan");
    let feature = store
        .active_feature_for_run(&fixture.task.id, &fixture.run.id)
        .unwrap()
        .unwrap();
    assert_eq!(feature.plan_id, "plan");
    assert_eq!(feature.item_id, "feature-a");
    assert!(store
        .active_feature_for_run(&fixture.task.id, "unknown-run")
        .is_err());
}

#[tokio::test]
async fn lexical_workspace_escape_is_rejected() {
    let fixture = Fixture::new();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    let result = fixture
        .store()
        .begin_feature_write(
            &fixture.workspace,
            &fixture.task.id,
            &fixture.run.id,
            Path::new("../outside.txt"),
        )
        .await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("workspace escape must fail closed"),
    };
    assert!(matches!(error, ProductError::PathEscape(_)));
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_workspace_escape_is_rejected() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    fixture.insert_plan("plan", "executing", &[("feature-a", "in_progress")]);
    let outside = fixture._temp.path().join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    symlink(&outside, fixture.workspace.join("escape.txt")).unwrap();
    let result = fixture
        .store()
        .begin_feature_write(
            &fixture.workspace,
            &fixture.task.id,
            &fixture.run.id,
            Path::new("escape.txt"),
        )
        .await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("symlink escape must fail closed"),
    };
    assert!(matches!(error, ProductError::PathEscape(_)));
}
