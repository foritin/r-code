//! M1a-04 (FR-7.4): desktop-side memory mode semantics for the frozen
//! handoff — feature off and workspace-off produce no payload; an enabled
//! memory with a real entry produces the rendered block the daemon will
//! freeze into the task contract.

use r_code_core::MemoryKind;
use r_code_core::{MemoryReviewSettingsUpdate, ProjectNotificationMode, ReviewerSelection};
use r_code_store::memory_store::MemoryEntryDraft;
use r_code_store::{Database, MemoryStore, WorkspaceService};

fn update(enabled: bool, version: u64) -> MemoryReviewSettingsUpdate {
    MemoryReviewSettingsUpdate {
        expected_version: version,
        enabled,
        reviewer: enabled.then(|| ReviewerSelection {
            provider_name: "deepseek".into(),
            model: "deepseek-chat".into(),
        }),
        trigger_every_turns: 10,
        explicit_remember_immediate: true,
        project_notification_mode: ProjectNotificationMode::default(),
    }
}

#[test]
fn memory_off_yields_no_handoff_payload() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(temp.path().join("r-code.db")).unwrap();
    // Default state: memory disabled — no payload even with a workspace.
    let ws = temp.path().join("checkout");
    std::fs::create_dir_all(&ws).unwrap();
    let canonical = ws.canonicalize().unwrap().to_string_lossy().to_string();

    assert!(r_code_host::frozen_memory_payload(&db, Some(&canonical)).is_none());
    // Fail-open on read errors / missing workspace rows too.
    assert!(r_code_host::frozen_memory_payload(&db, None).is_none());
}

#[test]
fn enabled_memory_with_an_entry_freezes_the_rendered_block() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(temp.path().join("r-code.db")).unwrap();

    let ws = temp.path().join("checkout");
    std::fs::create_dir_all(&ws).unwrap();
    let canonical = ws.canonicalize().unwrap().to_string_lossy().to_string();
    let workspace = WorkspaceService::new(&db)
        .open(&canonical, "checkout")
        .unwrap();

    let store = MemoryStore::new(&db);
    store.update_settings(&update(true, 0)).unwrap();
    store
        .add_entry(&MemoryEntryDraft {
            scope: "global".into(),
            workspace_id: None,
            kind: MemoryKind::Preference,
            content: "prefer rust fmt over grass style".into(),
            pinned: false,
        })
        .unwrap();

    let payload = r_code_host::frozen_memory_payload(&db, Some(&canonical))
        .expect("enabled memory must freeze a payload");
    assert!(
        payload["rendered"]
            .as_str()
            .unwrap()
            .contains("prefer rust fmt"),
        "rendered block must carry the entry content: {payload}"
    );
    let hash = payload["snapshotHash"].as_str().unwrap().to_string();
    assert!(!hash.trim().is_empty());
    assert!(!payload["entryIds"].as_array().unwrap().is_empty());

    // Workspace mode off overrides the global feature: no injection.
    WorkspaceService::new(&db)
        .set_memory_mode(
            &workspace.id,
            workspace.memory_generation,
            r_code_core::dto::WorkspaceMemoryMode::Off,
        )
        .unwrap();
    assert!(
        r_code_host::frozen_memory_payload(&db, Some(&canonical)).is_none(),
        "workspace off must suppress the handoff"
    );
}
