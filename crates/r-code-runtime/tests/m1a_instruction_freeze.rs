//! M1a-06 (FR-1.5/1.6): frozen instruction wiring — the bundle joins the
//! run snapshot (identity-stable, backward compatible), rides the harness
//! config, records a ledger row, and emits the low-noise context.instructions
//! journal event; per-workspace settings persist and honor the toggle.

mod p_gate_support;

use p_gate_support::{compose_with_builtin, stage_native};
use r_code_harness_protocol::{HostService, PackageRef};
use r_code_kernel::ports::{ModelService, RunGuard, ToolService};
use r_code_kernel::task::{TaskContract, TaskKind, TaskState, WorkspaceSnapshotRef};
use r_code_kernel::testing::{FakeModelService, FakeToolService};
use r_code_runtime::application::CreateTaskInput;
use r_code_runtime::services::project_instructions::{self, InstructionSettings};
use r_code_runtime::services::run_snapshots::{harness_config, RunSnapshotBuilder};
use r_code_runtime::services::settings_store::SettingsStore;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::{ContextSettingsRecord, InjectionKind, V1Store};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn package() -> PackageRef {
    PackageRef {
        id: r_code_harness_protocol::HarnessId::new("native.r-code"),
        version: "1.0.0".parse().unwrap(),
        content_digest: "sha256:package".into(),
    }
}

fn state(task_id: &str) -> TaskState {
    TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind: TaskKind::Conversation,
        objective: "objective".into(),
        constraints: vec![],
        required_checks: vec![],
        memory: None,
        revision: 1,
    })
}

fn workspace_ref(canonical_root: &str) -> WorkspaceSnapshotRef {
    WorkspaceSnapshotRef {
        canonical_root: canonical_root.into(),
        workspace_identity: "workspace-1".into(),
        baseline_sha256: "sha256:workspace".into(),
    }
}

/// A temp repo with foreign + own instruction files and a pinned global
/// context.md; canonicalized so the engine can walk it.
fn repo_fixture() -> (tempfile::TempDir, String) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join(".r-code")).unwrap();
    std::fs::write(repo.join("AGENTS.md"), "foreign rules\n").unwrap();
    std::fs::write(repo.join(".r-code").join("context.md"), "own rules\n").unwrap();
    let global = temp.path().join("global-context.md");
    std::fs::write(&global, "global personal rules\n").unwrap();
    // The global path is pinned explicitly, so tests never touch the home dir.
    let _ = global;
    let canonical = repo.canonicalize().unwrap().to_string_lossy().to_string();
    (temp, canonical)
}

#[tokio::test]
async fn instructions_freeze_into_the_snapshot_and_ride_the_config() {
    let temp = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(temp.path());
    let models: Arc<dyn ModelService> = Arc::new(FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let (fixture, canonical) = repo_fixture();
    let global = fixture.path().join("global-context.md");

    let builder = RunSnapshotBuilder::new(&settings, &models, &tools, true)
        .with_global_instructions_path(Some(global));
    let frozen = builder
        .freeze_with_workspace(
            &state("instructed"),
            &package(),
            &[HostService::ModelStream],
            &RunGuard::new("run-instructed", 1),
            workspace_ref(&canonical),
        )
        .await
        .unwrap();

    let instructions = &frozen.snapshot.material().instructions;
    assert!(!instructions.is_empty());
    let layers: Vec<&str> = instructions
        .entries
        .iter()
        .map(|e| e.layer.as_str())
        .collect();
    assert!(layers.contains(&"global"));
    assert!(layers.contains(&"repo-foreign"));
    assert!(layers.contains(&"repo-own"));
    assert!(instructions.rendered.contains("foreign rules"));
    assert!(instructions.rendered.contains("own rules"));

    let config = harness_config(&frozen.snapshot);
    assert_eq!(
        config["instructions"].as_str(),
        Some(instructions.rendered.as_str())
    );
    assert_eq!(
        config["instructionsDigest"].as_str(),
        Some(instructions.digest.as_str())
    );
}

#[tokio::test]
async fn empty_sets_keep_snapshot_identity_byte_stable() {
    let temp = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(temp.path());
    let models: Arc<dyn ModelService> = Arc::new(FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    // An empty temp workspace (no .git, no instruction files).
    let bare = tempfile::tempdir().unwrap();
    let canonical = bare
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let plain = RunSnapshotBuilder::new(&settings, &models, &tools, true)
        .freeze_with_workspace(
            &state("bare"),
            &package(),
            &[HostService::ModelStream],
            &RunGuard::new("run-bare", 1),
            workspace_ref(&canonical),
        )
        .await
        .unwrap();

    let disabled = InstructionSettings {
        injection_enabled: false,
        ..InstructionSettings::default()
    };
    let toggled = RunSnapshotBuilder::new(&settings, &models, &tools, true)
        .with_instruction_settings(disabled)
        .freeze_with_workspace(
            &state("bare"),
            &package(),
            &[HostService::ModelStream],
            &RunGuard::new("run-bare", 1),
            workspace_ref(&canonical),
        )
        .await
        .unwrap();

    assert!(plain.snapshot.material().instructions.is_empty());
    assert_eq!(
        plain.snapshot.id(),
        toggled.snapshot.id(),
        "disabled injection and absent files must both produce the byte-stable pre-FR-1 identity"
    );

    // A pre-FR-1 serialized material (instructions key removed) keeps the
    // same identity after rehydration.
    let mut legacy_value = serde_json::to_value(plain.snapshot.material()).unwrap();
    legacy_value.as_object_mut().unwrap().remove("instructions");
    let material: r_code_kernel::task::RunSnapshotMaterial =
        serde_json::from_value(legacy_value).unwrap();
    let legacy = r_code_kernel::task::RunSnapshot::new(material).unwrap();
    assert_eq!(legacy.id(), plain.snapshot.id());
}

#[tokio::test]
async fn instructed_run_records_ledger_row_and_journal_event() {
    let directory = tempfile::tempdir().unwrap();
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("m1a-06-instructed-run"),
    )
    .unwrap();
    let package = stage_native(directory.path(), "native.r-code", "1.0.0", true);
    let service = compose_with_builtin(&profile, &package, Arc::new(FakeModelService::default()));

    let repo = tempfile::tempdir().unwrap();
    let repo_root = repo.path().join("repo");
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    std::fs::write(repo_root.join("AGENTS.md"), "repo rules for the run\n").unwrap();
    let canonical = repo_root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();

    service
        .create_task_legacy_default(CreateTaskInput {
            task_id: "instructed-task".into(),
            objective: "follow the rules".into(),
            title: None,
            kind: TaskKind::Conversation,
            required_checks: vec![],
            memory: None,
            preferences: r_code_kernel::task::TaskPreferences {
                workspace_path: Some(canonical),
                ..Default::default()
            },
            harness_id: None,
        })
        .await
        .expect("create task");
    service
        .send_message("instructed-task", "hello")
        .await
        .expect("send");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let events = service.events_after(0, 500).await;
        let settled = events.iter().any(|event| {
            event.task_id == "instructed-task"
                && event.payload.get("journalKind") == Some(&serde_json::json!("run.completed"))
        });
        let has_context_event = events.iter().any(|event| {
            event.task_id == "instructed-task"
                && event.payload.get("journalKind")
                    == Some(&serde_json::json!("context.instructions"))
        });
        if settled {
            assert!(
                has_context_event,
                "context.instructions journal event missing; journal: {:?}",
                events
                    .iter()
                    .map(|e| e.payload.get("journalKind"))
                    .collect::<Vec<_>>()
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "run did not settle; journal: {:?}",
            events
                .iter()
                .map(|e| e.payload.get("journalKind"))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let store = V1Store::open(&profile.database_path()).unwrap();
    let rows = store.injections_for_run("run-instructed-task-1").unwrap();
    assert!(
        rows.iter().any(|row| {
            row.kind == "instruction"
                && row
                    .refs
                    .iter()
                    .any(|r| r.contains("repo-foreign") && r.contains("AGENTS.md"))
        }),
        "expected an instruction ledger row, got {rows:?}"
    );
}

#[test]
fn context_settings_persist_and_the_toggle_reaches_the_engine() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&temp.path().join("tasks.sqlite3")).unwrap();

    let canonical = temp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let key = project_instructions::workspace_settings_key(&canonical);
    assert!(
        store.context_settings(&key).unwrap().is_none(),
        "no stored settings initially"
    );
    assert!(
        project_instructions::resolve_settings(&store, &canonical).injection_enabled,
        "defaults enable injection"
    );

    let disabled = ContextSettingsRecord {
        injection_enabled: false,
        ..ContextSettingsRecord::defaults()
    };
    store.save_context_settings(&key, &disabled).unwrap();
    let loaded = store.context_settings(&key).unwrap().unwrap();
    assert!(!loaded.injection_enabled);
    assert!(
        !project_instructions::resolve_settings(&store, &canonical).injection_enabled,
        "stored toggle must flow into the engine settings"
    );

    // Ledger kinds stay a closed set (sanity for the CHECK constraint).
    let record = r_code_store::v1::InjectionRecord {
        run_id: "run-settings".into(),
        kind: InjectionKind::Instruction,
        snapshot_hash: "digest".into(),
        refs: vec![],
        chars: 0,
    };
    store.record_injection(&record).unwrap();
}
