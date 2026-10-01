//! M1a-01 (FR-7.1): the desktop-frozen memory handoff rides task creation
//! into the durable daemon contract and is inherited unchanged by later
//! loads. Ownerless paths (the simple internal API) stay memory-free.

use r_code_kernel::task::{FrozenMemoryHandoff, TaskKind, TaskPreferences};
use r_code_runtime::application::{ApplicationService, CreateTaskInput};
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::V1Store;
use std::sync::Arc;

#[tokio::test]
async fn memory_handoff_freezes_into_the_durable_contract() {
    let directory = tempfile::tempdir().expect("fixture root");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("m1a-01-memory-handoff"),
    )
    .expect("profile");
    let service = ApplicationService::compose(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose service");

    let handoff = FrozenMemoryHandoff {
        rendered: "<r_code_memory_snapshot>frozen block</r_code_memory_snapshot>".into(),
        entry_ids: vec!["entry-1".into(), "entry-2".into()],
        snapshot_hash: "hash-m1a-01".into(),
    };
    service
        .create_task_legacy_default(CreateTaskInput {
            task_id: "memory-task".into(),
            objective: "carry memory".into(),
            title: None,
            kind: TaskKind::Conversation,
            required_checks: vec![],
            memory: Some(handoff.clone()),
            preferences: TaskPreferences::default(),
            harness_id: None,
        })
        .await
        .expect("create with memory");

    let store = V1Store::open(&profile.database_path()).expect("open store");
    let (state, _revision) = store
        .load_task_with_revision("memory-task")
        .expect("load task")
        .expect("task exists");
    let frozen = state.contract.memory.as_ref().expect("memory present");
    assert_eq!(frozen.rendered, handoff.rendered);
    assert_eq!(frozen.entry_ids, handoff.entry_ids);
    assert_eq!(frozen.snapshot_hash, handoff.snapshot_hash);

    // Reloading never recomputes or mutates the frozen payload.
    let (reloaded, _) = store
        .load_task_with_revision("memory-task")
        .expect("reload")
        .expect("task exists");
    assert_eq!(reloaded.contract.memory, state.contract.memory);

    // Ownerless/simple creation paths carry no memory.
    service
        .create_task("plain-task", "objective", TaskKind::Conversation, vec![])
        .await
        .expect("create plain");
    let (plain, _) = store
        .load_task_with_revision("plain-task")
        .expect("load plain")
        .expect("plain exists");
    assert!(plain.contract.memory.is_none());
}
