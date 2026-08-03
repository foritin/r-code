use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use r_code_core::dto::{AgentRun, Task, TaskMode, Workspace};
use r_code_store::{
    AgentRunRepository, BlobStore, Database, LifecyclePurgeStore, PlanStore, TaskRepository,
    WorkspaceRepository, PURGE_REJECT_IN_PROGRESS,
};
use rusqlite::{params, OptionalExtension};
use tempfile::TempDir;
use uuid::Uuid;

struct Fixture {
    _directory: TempDir,
    db: Database,
    blobs: PathBuf,
    plans: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let blobs = directory.path().join("app-data/blobs");
        let plans = directory.path().join("app-data/plans");
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        Self {
            _directory: directory,
            db: Database::open_in_memory().unwrap(),
            blobs,
            plans,
            workspace,
        }
    }

    fn create_workspace(&self, path: &Path, name: &str) -> Workspace {
        let workspace = Workspace::new(path.to_string_lossy(), name);
        WorkspaceRepository::new(&self.db)
            .upsert(&workspace)
            .unwrap();
        workspace
    }

    fn create_task(&self, workspace: &Path, title: &str) -> (Task, AgentRun) {
        let task = Task::new(
            Some(workspace.to_string_lossy().into_owned()),
            title,
            "purge test",
            TaskMode::Edit,
        );
        TaskRepository::new(&self.db).create(&task).unwrap();
        let run = AgentRun::new(&task.id, "test-model");
        AgentRunRepository::new(&self.db).create(&run).unwrap();
        (task, run)
    }

    fn store_blob_with_refs(&self, content: &[u8], count: usize) -> String {
        let store = BlobStore::new(&self.db, self.blobs.clone());
        let hash = store.put(content).unwrap();
        for _ in 0..count {
            store.increment_ref(&hash).unwrap();
        }
        hash
    }

    fn purger(&self) -> LifecyclePurgeStore<'_> {
        LifecyclePurgeStore::new(&self.db, self.blobs.clone(), self.plans.clone())
    }
}

fn insert_plan(
    db: &Database,
    task: &Task,
    run: &AgentRun,
    plan_id: &str,
    item_id: &str,
    projection_path: &Path,
    state: &str,
) {
    let now = Utc::now().to_rfc3339();
    let item_state = if state == "completed" {
        "completed"
    } else {
        "in_progress"
    };
    let completed_at = (state == "completed").then_some(now.as_str());
    let connection = db.conn().unwrap();
    connection
        .execute(
            "INSERT INTO plans (
                 id, task_id, revision, state, approved_revision, projection_path,
                 created_at, updated_at, approved_at
             ) VALUES (?1, ?2, 1, ?3, 1, ?4, ?5, ?5, ?5)",
            params![
                plan_id,
                task.id,
                state,
                projection_path.to_string_lossy(),
                now
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO plan_items (
                 id, plan_id, revision, ordinal, title, description, state,
                 created_at, updated_at, started_at, completed_at
             ) VALUES (?1, ?2, 1, 0, 'Feature', 'Feature body', ?3, ?4, ?4, ?4, ?5)",
            params![item_id, plan_id, item_state, now, completed_at],
        )
        .unwrap();
    // Keep the run alive in the graph used by plan_change_events.
    assert_eq!(run.task_id, task.id);
}

fn blob_ref_count(db: &Database, hash: &str) -> Option<i64> {
    db.conn()
        .unwrap()
        .query_row(
            "SELECT ref_count FROM blobs WHERE hash = ?1",
            params![hash],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

#[test]
fn task_purge_counts_each_owned_column_and_ignores_review_copies() {
    let fixture = Fixture::new();
    fixture.create_workspace(&fixture.workspace, "Workspace");
    let (task, run) = fixture.create_task(&fixture.workspace, "Task");
    let plan_id = Uuid::new_v4().to_string();
    let item_id = "feature-1";
    let workspace_sentinel = fixture.workspace.join("must-survive.txt");
    std::fs::write(&workspace_sentinel, b"workspace-owned").unwrap();
    insert_plan(
        &fixture.db,
        &task,
        &run,
        &plan_id,
        item_id,
        &workspace_sentinel,
        "completed",
    );
    let projection = fixture.plans.join(&plan_id);
    std::fs::create_dir_all(&projection).unwrap();
    std::fs::write(projection.join("plan.md"), b"projection").unwrap();

    // file_changes(2) + baseline(1) + verification(1) + plan event(2) + reject journal(3).
    let hash = fixture.store_blob_with_refs(b"same content in every owned column", 9);
    let now = Utc::now().to_rfc3339();
    let connection = fixture.db.conn().unwrap();
    connection
        .execute(
            "INSERT INTO file_changes (
                 id, task_id, path, change_type, before_hash, after_hash, created_at
             ) VALUES ('change', ?1, 'src/a.txt', 'modify', ?2, ?2, ?3)",
            params![task.id, hash, now],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO file_baselines (
                 id, task_id, path, content_hash, blob_key, captured_at
             ) VALUES ('baseline', ?1, 'src/a.txt', ?2, ?2, ?3)",
            params![task.id, hash, now],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO verifications (
                 id, task_id, run_id, command, status, output_blob_key, started_at, ended_at
             ) VALUES ('verification', ?1, ?2, 'test', 'passed', ?3, ?4, ?4)",
            params![task.id, run.id, hash, now],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO plan_change_events (
                 id, plan_id, plan_revision, item_id, task_id, run_id, tool_call_id, path,
                 before_blob_hash, before_exists, after_blob_hash, after_exists, state,
                 created_at, finalized_at
             ) VALUES ('event', ?1, 1, ?2, ?3, ?4, 'tool', 'src/a.txt',
                       ?5, 1, ?5, 1, 'captured', ?6, ?6)",
            params![plan_id, item_id, task.id, run.id, hash, now],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO plan_reject_operations (
                 id, plan_id, plan_revision, item_id, scope, state, recovery_count,
                 created_at, updated_at, completed_at
             ) VALUES ('operation', ?1, 1, ?2, 'feature', 'committed', 0, ?3, ?3, ?3)",
            params![plan_id, item_id, now],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO plan_reject_operation_files (
                 operation_id, ordinal, path, expected_current_hash, expected_exists,
                 rollback_hash, rollback_exists, desired_hash, desired_exists,
                 state, applied_at
             ) VALUES ('operation', 0, 'src/a.txt', ?1, 1, ?1, 1, ?1, 1, 'applied', ?2)",
            params![hash, now],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO review_sessions (
                 id, run_id, task_id, state, materialized_at, created_at, updated_at
             ) VALUES ('review', ?1, ?2, 'pending', ?3, ?3, ?3)",
            params![run.id, task.id, now],
        )
        .unwrap();
    // review_files is a materialized copy and deliberately owns no extra BlobStore refs.
    connection
        .execute(
            "INSERT INTO review_files (
                 id, session_id, path, before_hash, after_hash, state, created_at, updated_at
             ) VALUES ('review-file', 'review', 'src/a.txt', ?1, ?1, 'pending', ?2, ?2)",
            params![hash, now],
        )
        .unwrap();
    drop(connection);

    let result = fixture.purger().purge_task(&task.id).unwrap();
    assert_eq!(result.removed_tasks, 1);
    assert_eq!(result.released_blob_references, 9);
    assert_eq!(result.unreferenced_blob_hashes, vec![hash.clone()]);
    assert_eq!(result.removed_plan_ids, vec![plan_id]);
    assert!(result.cleanup_warnings.is_empty());
    assert_eq!(blob_ref_count(&fixture.db, &hash), None);
    assert!(!fixture.blobs.join(&hash).exists());
    assert!(!projection.exists());
    assert_eq!(
        std::fs::read(&workspace_sentinel).unwrap(),
        b"workspace-owned"
    );
}

#[test]
fn shared_blob_survives_first_task_and_is_removed_after_last_owner() {
    let fixture = Fixture::new();
    fixture.create_workspace(&fixture.workspace, "Workspace");
    let (first, _) = fixture.create_task(&fixture.workspace, "First");
    let (second, _) = fixture.create_task(&fixture.workspace, "Second");
    let hash = fixture.store_blob_with_refs(b"shared", 2);
    let now = Utc::now().to_rfc3339();
    let connection = fixture.db.conn().unwrap();
    for (id, task_id) in [("first-change", &first.id), ("second-change", &second.id)] {
        connection
            .execute(
                "INSERT INTO file_changes (
                     id, task_id, path, change_type, after_hash, created_at
                 ) VALUES (?1, ?2, 'shared.txt', 'modify', ?3, ?4)",
                params![id, task_id, hash, now],
            )
            .unwrap();
    }
    drop(connection);

    fixture.purger().purge_task(&first.id).unwrap();
    assert_eq!(blob_ref_count(&fixture.db, &hash), Some(1));
    assert!(fixture.blobs.join(&hash).is_file());

    fixture.purger().purge_task(&second.id).unwrap();
    assert_eq!(blob_ref_count(&fixture.db, &hash), None);
    assert!(!fixture.blobs.join(&hash).exists());
}

#[test]
fn workspace_repository_uses_unified_purge_without_touching_workspace_files() {
    let fixture = Fixture::new();
    let other_workspace = fixture._directory.path().join("other-workspace");
    std::fs::create_dir_all(&other_workspace).unwrap();
    fixture.create_workspace(&fixture.workspace, "Target");
    fixture.create_workspace(&other_workspace, "Other");
    let (first, _) = fixture.create_task(&fixture.workspace, "First");
    let (second, _) = fixture.create_task(&fixture.workspace, "Second");
    let (other, _) = fixture.create_task(&other_workspace, "Other");
    let sentinel = fixture.workspace.join("source.txt");
    std::fs::write(&sentinel, b"do not delete").unwrap();

    let target_hash = fixture.store_blob_with_refs(b"target", 2);
    let other_hash = fixture.store_blob_with_refs(b"other", 1);
    let now = Utc::now().to_rfc3339();
    let connection = fixture.db.conn().unwrap();
    for (id, task_id) in [("target-1", &first.id), ("target-2", &second.id)] {
        connection
            .execute(
                "INSERT INTO file_changes (
                     id, task_id, path, change_type, after_hash, created_at
                 ) VALUES (?1, ?2, 'target.txt', 'modify', ?3, ?4)",
                params![id, task_id, target_hash, now],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO file_changes (
                 id, task_id, path, change_type, after_hash, created_at
             ) VALUES ('other', ?1, 'other.txt', 'modify', ?2, ?3)",
            params![other.id, other_hash, now],
        )
        .unwrap();
    drop(connection);

    let removed = WorkspaceRepository::new(&fixture.db)
        .remove(
            &fixture.workspace.to_string_lossy(),
            &fixture.blobs,
            &fixture.plans,
        )
        .unwrap();
    assert_eq!(removed, (true, 2));
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"do not delete");
    assert_eq!(blob_ref_count(&fixture.db, &target_hash), None);
    assert_eq!(blob_ref_count(&fixture.db, &other_hash), Some(1));
    assert!(TaskRepository::new(&fixture.db)
        .get(&other.id)
        .unwrap()
        .is_some());
}

#[test]
fn active_rejection_blocks_task_and_workspace_purge_fail_closed() {
    let fixture = Fixture::new();
    fixture.create_workspace(&fixture.workspace, "Workspace");
    let (task, run) = fixture.create_task(&fixture.workspace, "Task");
    let plan_id = Uuid::new_v4().to_string();
    insert_plan(
        &fixture.db,
        &task,
        &run,
        &plan_id,
        "feature",
        &fixture.plans.join(&plan_id).join("plan.md"),
        "executing",
    );
    let now = Utc::now().to_rfc3339();
    fixture
        .db
        .conn()
        .unwrap()
        .execute(
            "INSERT INTO plan_reject_operations (
                 id, plan_id, plan_revision, item_id, scope, state, recovery_count,
                 created_at, updated_at
             ) VALUES ('active-operation', ?1, 1, 'feature', 'feature', 'prepared', 0, ?2, ?2)",
            params![plan_id, now],
        )
        .unwrap();

    let task_error = fixture.purger().purge_task(&task.id).unwrap_err();
    assert!(task_error.to_string().contains(PURGE_REJECT_IN_PROGRESS));
    let workspace_error = fixture
        .purger()
        .purge_workspace(&fixture.workspace.to_string_lossy())
        .unwrap_err();
    assert!(workspace_error
        .to_string()
        .contains(PURGE_REJECT_IN_PROGRESS));
    assert!(TaskRepository::new(&fixture.db)
        .get(&task.id)
        .unwrap()
        .is_some());
}

#[test]
fn restart_prune_removes_only_safe_orphan_appdata_entries() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("db/r-code.db");
    let blobs = directory.path().join("blobs");
    let plans = directory.path().join("plans");
    std::fs::create_dir_all(database_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&blobs).unwrap();
    std::fs::create_dir_all(&plans).unwrap();
    let orphan_hash = blake3::hash(b"orphan").to_hex().to_string();
    let kept_hash = blake3::hash(b"kept").to_hex().to_string();
    let orphan_plan = Uuid::new_v4().to_string();
    let kept_plan = Uuid::new_v4().to_string();

    {
        let db = Database::open(&database_path).unwrap();
        let task = Task::new(None, "Task", "Goal", TaskMode::Ask);
        TaskRepository::new(&db).create(&task).unwrap();
        let now = Utc::now().to_rfc3339();
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO plans (id, task_id, revision, state, created_at, updated_at) \
                 VALUES (?1, ?2, 1, 'completed', ?3, ?3)",
                params![kept_plan, task.id, now],
            )
            .unwrap();
        let store = BlobStore::new(&db, blobs.clone());
        assert_eq!(store.put(b"kept").unwrap(), kept_hash);
        store.increment_ref(&kept_hash).unwrap();
        std::fs::write(blobs.join(&orphan_hash), b"orphan").unwrap();
        std::fs::write(blobs.join("NOT-A-HASH"), b"leave me").unwrap();
        std::fs::create_dir(blobs.join(blake3::hash(b"dir").to_hex().to_string())).unwrap();
        std::fs::create_dir_all(plans.join(&orphan_plan)).unwrap();
        std::fs::write(plans.join(&orphan_plan).join("plan.md"), b"orphan").unwrap();
        std::fs::create_dir_all(plans.join(&kept_plan)).unwrap();
        std::fs::create_dir_all(plans.join("not-a-plan-id")).unwrap();
    }

    // A fresh process/store instance performs the startup retry.
    let reopened = Arc::new(Database::open(&database_path).unwrap());
    let blob_report = BlobStore::new(reopened.as_ref(), blobs.clone())
        .prune_unreferenced_files()
        .unwrap();
    let plan_report = PlanStore::new(Arc::clone(&reopened), &plans)
        .prune_orphan_projection_directories()
        .unwrap();
    assert_eq!(blob_report.removed, 1);
    assert!(blob_report.warnings.is_empty());
    assert_eq!(plan_report.removed, 1);
    assert!(plan_report.warnings.is_empty());
    assert!(!blobs.join(&orphan_hash).exists());
    assert!(blobs.join(&kept_hash).is_file());
    assert!(blobs.join("NOT-A-HASH").is_file());
    assert!(plans.join(&kept_plan).is_dir());
    assert!(plans.join("not-a-plan-id").is_dir());
}
