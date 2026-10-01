//! M1a-02 (FR-7.2): the frozen memory segment joins the daemon prompt
//! snapshot, and every run records an injection-ledger row in the daemon
//! V1Store — never in the desktop r-code.db.

mod p_gate_support;

use p_gate_support::{compose_with_builtin, stage_native};
use r_code_harness_protocol::{HostService, PackageRef};
use r_code_kernel::ports::{ModelService, RunGuard, ToolService};
use r_code_kernel::task::{
    FrozenMemoryHandoff, TaskContract, TaskKind, TaskState, WorkspaceSnapshotRef,
};
use r_code_kernel::testing::{FakeModelService, FakeToolService};
use r_code_runtime::services::run_snapshots::RunSnapshotBuilder;
use r_code_runtime::services::settings_store::SettingsStore;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::{InjectionKind, InjectionRecord, V1Store};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn handoff() -> FrozenMemoryHandoff {
    FrozenMemoryHandoff {
        rendered: "<r_code_memory_snapshot>frozen segment m1a-02</r_code_memory_snapshot>".into(),
        entry_ids: vec!["entry-1".into()],
        snapshot_hash: "hash-m1a-02".into(),
    }
}

fn package() -> PackageRef {
    PackageRef {
        id: r_code_harness_protocol::HarnessId::new("native.r-code"),
        version: "1.0.0".parse().unwrap(),
        content_digest: "sha256:package".into(),
    }
}

fn state_with_memory(task_id: &str, memory: Option<FrozenMemoryHandoff>) -> TaskState {
    TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind: TaskKind::Conversation,
        objective: "objective".into(),
        constraints: vec![],
        required_checks: vec![],
        memory,
        revision: 1,
    })
}

fn workspace() -> WorkspaceSnapshotRef {
    WorkspaceSnapshotRef {
        canonical_root: "D:/workspace".into(),
        workspace_identity: "workspace-1".into(),
        baseline_sha256: "sha256:workspace".into(),
    }
}

#[tokio::test]
async fn memory_segment_joins_the_frozen_prompt_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(temp.path());
    let models: Arc<dyn ModelService> = Arc::new(FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let builder = RunSnapshotBuilder::new(&settings, &models, &tools, true);
    let grants = [HostService::ModelStream];

    let plain = state_with_memory("plain-task", None);
    let frozen_plain = builder
        .freeze_with_workspace(
            &plain,
            &package(),
            &grants,
            &RunGuard::new("run-plain", 1),
            workspace(),
        )
        .await
        .unwrap();

    let with_memory = state_with_memory("memory-task", Some(handoff()));
    let frozen_memory = builder
        .freeze_with_workspace(
            &with_memory,
            &package(),
            &grants,
            &RunGuard::new("run-memory", 1),
            workspace(),
        )
        .await
        .unwrap();

    let plain_prompt = &frozen_plain.snapshot.material().prompt;
    let memory_prompt = &frozen_memory.snapshot.material().prompt;
    assert!(
        memory_prompt
            .resolved_system_prompt
            .contains("frozen segment m1a-02"),
        "memory segment must join the resolved prompt"
    );
    assert!(!plain_prompt
        .resolved_system_prompt
        .contains("frozen segment m1a-02"));
    assert!(memory_prompt.revision.contains("+mem:hash-m1a"));
    assert!(!plain_prompt.revision.contains("+mem:"));
    assert_ne!(
        plain_prompt.content_sha256, memory_prompt.content_sha256,
        "the combined text must change the snapshot identity"
    );
}

#[test]
fn injection_ledger_rows_are_idempotent_per_run_and_hash() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&temp.path().join("tasks.sqlite3")).unwrap();

    let record = InjectionRecord {
        run_id: "run-ledger-1".into(),
        kind: InjectionKind::Memory,
        snapshot_hash: "hash-a".into(),
        refs: vec!["entry-1".into(), "entry-2".into()],
        chars: 42,
    };
    store.record_injection(&record).unwrap();
    // Same (run, kind, hash): idempotent no-op.
    store.record_injection(&record).unwrap();

    let rows = store.injections_for_run("run-ledger-1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "memory");
    assert_eq!(rows[0].snapshot_hash, "hash-a");
    assert_eq!(
        rows[0].refs,
        vec!["entry-1".to_string(), "entry-2".to_string()]
    );
    assert_eq!(rows[0].chars, 42);

    // A different hash appends a new audit row for the same run.
    let rotated = InjectionRecord {
        snapshot_hash: "hash-b".into(),
        ..record.clone()
    };
    store.record_injection(&rotated).unwrap();
    let rows = store.injections_for_run("run-ledger-1").unwrap();
    assert_eq!(rows.len(), 2);

    assert!(store.injections_for_run("run-absent").unwrap().is_empty());
}

#[tokio::test]
async fn conversation_run_with_memory_records_the_ledger_row() {
    let directory = tempfile::tempdir().unwrap();
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("m1a-02-memory-ledger"),
    )
    .unwrap();
    let package = stage_native(directory.path(), "native.r-code", "1.0.0", true);
    let service = compose_with_builtin(&profile, &package, Arc::new(FakeModelService::default()));

    service
        .create_task_legacy_default(r_code_runtime::application::CreateTaskInput {
            task_id: "m1a02-task".into(),
            objective: "remember things".into(),
            title: None,
            kind: TaskKind::Conversation,
            required_checks: vec![],
            memory: Some(handoff()),
            preferences: Default::default(),
            harness_id: None,
        })
        .await
        .expect("create task");

    service.send_message("m1a02-task", "hello").await.unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let events = service.events_after(0, 500).await;
        let settled = events.iter().any(|event| {
            event.task_id == "m1a02-task"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
        });
        if settled {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "run did not settle; journal: {:?}",
            events
                .iter()
                .map(|event| event.payload.get("journalKind"))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let store = V1Store::open(&profile.database_path()).unwrap();
    let rows = store.injections_for_run("run-m1a02-task-1").unwrap();
    assert!(
        rows.iter()
            .any(|row| row.kind == "memory" && row.snapshot_hash == "hash-m1a-02"),
        "expected a memory ledger row for the first run, got {rows:?}"
    );
}
