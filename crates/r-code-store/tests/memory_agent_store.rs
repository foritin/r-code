use chrono::{DateTime, Duration, Utc};
use r_code_core::dto::{AgentRun, Task, TaskMode, Workspace};
use r_code_core::{
    GlobalMemoryAuthorization, MemoryEntry, MemoryKind, MemoryOwner, ProjectMemoryOrigin,
};
use r_code_store::{
    AgentMemorySaveOutcome, AgentRunRepository, Database, MemoryEntryDraft, MemoryStore,
    TaskRepository, WorkspaceRepository,
};
use rusqlite::params;
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    db: Database,
    requested_workspace: Workspace,
    other_workspace: Workspace,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let requested_path = directory.path().join("requested-workspace");
        let other_path = directory.path().join("other-workspace");
        std::fs::create_dir_all(&requested_path).unwrap();
        std::fs::create_dir_all(&other_path).unwrap();

        let db = Database::open_in_memory().unwrap();
        let requested_workspace =
            Workspace::new(requested_path.to_string_lossy(), "Requested workspace");
        let other_workspace = Workspace::new(other_path.to_string_lossy(), "Other workspace");
        let workspaces = WorkspaceRepository::new(&db);
        workspaces.upsert(&requested_workspace).unwrap();
        workspaces.upsert(&other_workspace).unwrap();

        Self {
            _directory: directory,
            db,
            requested_workspace,
            other_workspace,
        }
    }
}

fn draft(
    scope: &str,
    workspace_id: Option<&str>,
    kind: MemoryKind,
    content: &str,
) -> MemoryEntryDraft {
    MemoryEntryDraft {
        scope: scope.to_string(),
        workspace_id: workspace_id.map(str::to_string),
        kind,
        content: content.to_string(),
        pinned: false,
    }
}

fn create_run(db: &Database, label: &str) -> (Task, AgentRun) {
    let task = Task::new(
        None,
        format!("{label} task"),
        "exercise agent memory persistence",
        TaskMode::Edit,
    );
    TaskRepository::new(db).create(&task).unwrap();
    let run = AgentRun::new(&task.id, "test-model");
    AgentRunRepository::new(db).create(&run).unwrap();
    (task, run)
}

fn memory_entry_count(db: &Database) -> i64 {
    db.conn()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM memory_entries", [], |row| row.get(0))
        .unwrap()
}

fn assert_observable_entry_fields(actual: &MemoryEntry, expected: &MemoryEntry) {
    assert_eq!(actual.id.as_str(), expected.id.as_str());
    assert_eq!(actual.kind, expected.kind);
    assert_eq!(actual.content.as_str(), expected.content.as_str());
    assert_eq!(actual.version, expected.version);
    assert_eq!(&actual.owner, &expected.owner);
    assert_eq!(&actual.updated_at, &expected.updated_at);
}

fn insert_pending_candidate(db: &Database, workspace_id: &str, id: &str, content: &str) {
    let now = Utc::now().to_rfc3339();
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO memory_candidates (
                 id, kind, operation, source_workspace_id, captured_at, proposal_index,
                 proposal_content, reason, proposal_hash, reason_hash, confidence, status,
                 created_at, updated_at
             ) VALUES (?1, 'pitfall', 'add', ?2, ?3, 0, ?4, 'candidate reason',
                       'candidate-proposal-hash', 'candidate-reason-hash', 0.75, 'pending', ?3, ?3)",
            params![id, workspace_id, now, content],
        )
        .unwrap();
}

fn insert_counting_entry(
    db: &Database,
    id: &str,
    origin: &str,
    run_id: &str,
    task_id: &str,
    created_at: DateTime<Utc>,
) {
    let timestamp = created_at.to_rfc3339();
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO memory_entries (
                 id, scope, workspace_id, kind, content, normalized_hash, version, origin,
                 pinned, source_run_id, source_task_id, created_at, updated_at
             ) VALUES (?1, 'global', NULL, 'convention', ?1, ?2, 1, ?3, 0, ?4, ?5, ?6, ?6)",
            params![id, format!("hash-{id}"), origin, run_id, task_id, timestamp],
        )
        .unwrap();
}

#[test]
fn list_agent_entries_returns_global_and_only_the_requested_approved_project_entries() {
    let fixture = Fixture::new();
    let store = MemoryStore::new(&fixture.db);
    let global = store
        .add_entry(&draft(
            "global",
            None,
            MemoryKind::Preference,
            "Prefer concise release notes.",
        ))
        .unwrap();
    let requested = store
        .add_entry(&draft(
            "project",
            Some(&fixture.requested_workspace.id),
            MemoryKind::Convention,
            "Use workspace-local fixture names.",
        ))
        .unwrap();
    let other = store
        .add_entry(&draft(
            "project",
            Some(&fixture.other_workspace.id),
            MemoryKind::Decision,
            "This belongs to the other workspace.",
        ))
        .unwrap();
    let candidate_id = "pending-agent-list-candidate";
    let candidate_content = "Pending candidates are not approved memories.";
    insert_pending_candidate(
        &fixture.db,
        &fixture.requested_workspace.id,
        candidate_id,
        candidate_content,
    );

    let visible = store
        .list_agent_entries(Some(&fixture.requested_workspace.id))
        .unwrap();
    assert_eq!(visible.len(), 2);
    let listed_global = visible.iter().find(|entry| entry.id == global.id).unwrap();
    let listed_requested = visible
        .iter()
        .find(|entry| entry.id == requested.id)
        .unwrap();
    assert_observable_entry_fields(listed_global, &global);
    assert_observable_entry_fields(listed_requested, &requested);
    assert_eq!(
        listed_global.owner,
        MemoryOwner::Global {
            authorization: GlobalMemoryAuthorization::Manual,
        }
    );
    assert_eq!(
        listed_requested.owner,
        MemoryOwner::Project {
            workspace_id: fixture.requested_workspace.id.clone(),
            origin: ProjectMemoryOrigin::Manual,
        }
    );
    assert!(!visible.iter().any(|entry| entry.id == other.id));
    assert!(!visible.iter().any(|entry| entry.id == candidate_id));
    assert!(!visible
        .iter()
        .any(|entry| entry.content == candidate_content));

    let global_only = store.list_agent_entries(None).unwrap();
    assert_eq!(global_only.len(), 1);
    assert_observable_entry_fields(&global_only[0], &global);
}

#[test]
fn normalized_duplicate_returns_existing_id_without_writing_or_consuming_run_quota() {
    let fixture = Fixture::new();
    let store = MemoryStore::new(&fixture.db);
    let (task, run) = create_run(&fixture.db, "deduplication");
    let original = draft(
        "global",
        None,
        MemoryKind::Preference,
        "  Keep generated files out of git.\r\nDocument exceptions.  ",
    );
    let created = match store
        .save_agent_entry(&original, &run.id, &task.id)
        .unwrap()
    {
        AgentMemorySaveOutcome::Created(entry) => entry,
        AgentMemorySaveOutcome::Duplicate { existing_id } => {
            panic!("first save unexpectedly duplicated {existing_id}")
        }
    };
    assert_eq!(
        created.content,
        "Keep generated files out of git.\nDocument exceptions."
    );
    assert_eq!(
        created.owner,
        MemoryOwner::Global {
            authorization: GlobalMemoryAuthorization::Agent,
        }
    );
    let rows_before_duplicate = memory_entry_count(&fixture.db);
    let writes_before_duplicate = store.agent_write_count_for_run(&run.id).unwrap();

    let duplicate = draft(
        "global",
        None,
        MemoryKind::Pitfall,
        "Keep generated files out of git.\nDocument exceptions.\r\n",
    );
    match store
        .save_agent_entry(&duplicate, &run.id, &task.id)
        .unwrap()
    {
        AgentMemorySaveOutcome::Duplicate { existing_id } => {
            assert_eq!(existing_id, created.id);
        }
        AgentMemorySaveOutcome::Created(entry) => {
            panic!("normalized duplicate created a new row {}", entry.id);
        }
    }
    assert_eq!(memory_entry_count(&fixture.db), rows_before_duplicate);
    assert_eq!(
        store.agent_write_count_for_run(&run.id).unwrap(),
        writes_before_duplicate
    );

    let distinct = draft(
        "project",
        Some(&fixture.requested_workspace.id),
        MemoryKind::Constraint,
        "Never overwrite user-owned workspace files.",
    );
    let distinct_created = match store
        .save_agent_entry(&distinct, &run.id, &task.id)
        .unwrap()
    {
        AgentMemorySaveOutcome::Created(entry) => entry,
        AgentMemorySaveOutcome::Duplicate { existing_id } => {
            panic!("distinct save unexpectedly duplicated {existing_id}")
        }
    };
    assert_eq!(
        distinct_created.owner,
        MemoryOwner::Project {
            workspace_id: fixture.requested_workspace.id.clone(),
            origin: ProjectMemoryOrigin::Agent,
        }
    );
    assert_eq!(memory_entry_count(&fixture.db), rows_before_duplicate + 1);
    assert_eq!(
        store.agent_write_count_for_run(&run.id).unwrap(),
        writes_before_duplicate + 1
    );
}

#[test]
fn agent_write_counts_filter_by_run_origin_and_include_the_since_boundary() {
    let db = Database::open_in_memory().unwrap();
    let store = MemoryStore::new(&db);
    let (target_task, target_run) = create_run(&db, "target counting");
    let (other_task, other_run) = create_run(&db, "other counting");
    let boundary = DateTime::parse_from_rfc3339("2026-08-14T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    insert_counting_entry(
        &db,
        "target-before-boundary",
        "agent",
        &target_run.id,
        &target_task.id,
        boundary - Duration::seconds(1),
    );
    insert_counting_entry(
        &db,
        "target-exactly-at-boundary",
        "agent",
        &target_run.id,
        &target_task.id,
        boundary,
    );
    insert_counting_entry(
        &db,
        "other-run-inside-window",
        "agent",
        &other_run.id,
        &other_task.id,
        boundary + Duration::seconds(1),
    );
    insert_counting_entry(
        &db,
        "manual-target-inside-window",
        "manual",
        &target_run.id,
        &target_task.id,
        boundary + Duration::seconds(2),
    );

    assert_eq!(store.agent_write_count_for_run(&target_run.id).unwrap(), 2);
    assert_eq!(store.agent_write_count_for_run(&other_run.id).unwrap(), 1);
    assert_eq!(
        store
            .agent_write_count_since(&target_run.id, boundary)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .agent_write_count_since(&target_run.id, boundary + Duration::seconds(1))
            .unwrap(),
        0
    );
}
