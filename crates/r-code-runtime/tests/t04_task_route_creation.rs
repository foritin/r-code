//! T04 — task creation carries one validated model/harness route into v1.

use r_code_client::{DaemonClient, DaemonInfo};
use r_code_harness_protocol::{HarnessId, HostService, PackageRef};
use r_code_kernel::ports::{JournalEvent, RunGuard};
use r_code_kernel::task::{
    Attempt, ModelRoute, ProviderRouteKind, TaskContract, TaskKind, TaskPreferences, TaskState,
};
use r_code_runtime::application::{ApplicationService, CreateTaskInput};
use r_code_runtime::services::run_snapshots::RunSnapshotBuilder;
use r_code_runtime::services::settings_store::{ProviderEntry, SettingsStore, V1Settings};
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::{PluginCatalogRecord, V1Store};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const SERVICE: &str = env!("CARGO_BIN_EXE_r-code-service");

struct DaemonFixture {
    _directory: tempfile::TempDir,
    profile: RuntimeProfile,
    child: Child,
    provider_env: String,
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl DaemonFixture {
    fn new(tag: &str) -> Self {
        let directory = tempfile::tempdir().expect("fixture root");
        let profile = RuntimeProfile::resolve(
            &LaunchOptions::new(ProfileFlavor::Development)
                .with_data_root(directory.path().join("data"))
                .with_ipc_name(format!("t04-{tag}-{}", std::process::id())),
        )
        .expect("profile");
        let builtins = directory.path().join("builtins");
        stage_harness(&builtins, "native.r-code", true);
        stage_harness(&builtins, "codex.r-code", false);
        let provider_env = format!(
            "R_CODE_T04_PROVIDER_KEY_{}_{}",
            std::process::id(),
            tag.to_ascii_uppercase()
        );
        let child = Command::new(SERVICE)
            .arg("--profile")
            .arg("development")
            .arg("--data-root")
            .arg(profile.data_root())
            .arg("--ipc-name")
            .arg(profile.ipc_name().expect("IPC name"))
            .env("R_CODE_BUILTIN_PLUGINS_DIR", &builtins)
            .env(&provider_env, "test-only-secret")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon");
        let fixture = Self {
            _directory: directory,
            profile,
            child,
            provider_env,
        };
        fixture.wait_for_owner();
        fixture
    }

    fn wait_for_owner(&self) -> DaemonInfo {
        for _ in 0..150 {
            if let Some(info) = r_code_client::read_owner_token(&self.profile.harness_v1_root()) {
                return info;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon never published owner information");
    }

    async fn connect(&self, client_id: &str) -> DaemonClient {
        for _ in 0..100 {
            let owner = self.wait_for_owner();
            if let Ok(client) = DaemonClient::connect(
                &self.profile.ipc_endpoint(),
                &self.profile.profile_id(),
                &owner.token,
                client_id,
            )
            .await
            {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("connect daemon client timed out")
    }

    fn store(&self) -> V1Store {
        V1Store::open(&self.profile.database_path()).expect("open v1 store")
    }
}

fn stage_harness(resources: &Path, id: &str, host_model: bool) {
    let package = resources.join("plugins").join(id);
    let bin = package.join("bin");
    std::fs::create_dir_all(&bin).expect("package directories");
    let source = std::env::current_exe().expect("current test executable");
    let staged_name = if cfg!(windows) {
        "fixture-harness.exe"
    } else {
        "fixture-harness"
    };
    std::fs::copy(&source, bin.join(staged_name)).expect("stage fixture executable");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    let mut services = vec![
        "host.tools.list",
        "host.tools.call",
        "host.checkpoint.save",
        "host.completion.propose",
    ];
    if host_model {
        services.push("host.model.stream");
    }
    std::fs::write(
        package.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": id,
            "version": "1.0.0",
            "apiMajor": 1,
            "apiMinor": 0,
            "displayName": id,
            "supportedPlatforms": [{
                "platform": platform,
                "executable": "bin/fixture-harness"
            }],
            "requestedHostServices": services,
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("write harness manifest");
}

async fn configure_providers(fixture: &DaemonFixture, client: &mut DaemonClient) {
    let applied = client
        .call(
            "settings.apply",
            serde_json::json!({
                "expectedRevision": 0,
                "selection": "deepseek",
                "model": "deepseek-default",
                "envVar": fixture.provider_env,
            }),
        )
        .await
        .expect("configure DeepSeek");
    assert_eq!(applied["revision"], 1);
    let applied = client
        .call(
            "settings.apply",
            serde_json::json!({
                "expectedRevision": 1,
                "selection": "openai",
                "model": "gpt-missing-credential",
            }),
        )
        .await
        .expect("configure unavailable provider");
    assert_eq!(applied["revision"], 2);
}

fn assert_absent(fixture: &DaemonFixture, task_id: &str) {
    let store = fixture.store();
    assert!(
        store
            .load_task_with_revision(task_id)
            .expect("query rejected task")
            .is_none(),
        "rejected create left a task aggregate for {task_id}"
    );
    assert!(
        store.task_events(task_id).is_empty(),
        "rejected create left journal events for {task_id}"
    );
}

#[tokio::test]
async fn old_and_new_create_payloads_are_atomic_and_project_the_bound_route() {
    let fixture = DaemonFixture::new("create");
    let mut client = fixture.connect("t04-create").await;

    let legacy = client
        .call(
            "task.create",
            serde_json::json!({
                "taskId": "legacy-create",
                "objective": "old v1 payload",
                "kind": "conversation",
            }),
        )
        .await
        .expect("old task.create payload remains valid");
    assert_eq!(legacy["taskId"], "legacy-create");

    configure_providers(&fixture, &mut client).await;
    client
        .call(
            "task.create",
            serde_json::json!({
                "taskId": "configured-create",
                "title": "DeepSeek task",
                "objective": "persist the complete selection",
                "mode": "ask",
                "workspacePath": "D:/project/rust/r-code",
                "systemPrompt": "task prompt",
                "inference": {"temperature": 0.25},
                "modelRoute": {
                    "kind": "host-provider",
                    "providerId": "deepseek",
                    "modelId": "deepseek-concrete"
                },
                "harnessId": "native.r-code"
            }),
        )
        .await
        .expect("configured create");
    let detail = client
        .call(
            "task.detail",
            serde_json::json!({"taskId": "configured-create"}),
        )
        .await
        .expect("configured detail");
    assert_eq!(detail["provider"], "deepseek");
    assert_eq!(detail["model"], "deepseek-concrete");
    assert_eq!(detail["engine"], "r_code");
    assert_eq!(detail["harness_id"], "native.r-code");
    assert_eq!(
        detail["inference"],
        serde_json::json!({"temperature": 0.25})
    );
    assert_eq!(detail["mode"], "ask");
    assert_eq!(
        detail["model_route"],
        serde_json::json!({
            "kind": "host-provider",
            "providerId": "deepseek",
            "modelId": "deepseek-concrete"
        })
    );

    let store = fixture.store();
    let (state, revision) = store
        .load_task_with_revision("configured-create")
        .expect("load configured task")
        .expect("configured task exists");
    assert_eq!(revision, 1, "creation must use one aggregate commit");
    assert_eq!(state.title.as_deref(), Some("DeepSeek task"));
    assert_eq!(
        state.preferences.model_route,
        Some(ModelRoute::HostProvider {
            provider_id: "deepseek".into(),
            model_id: Some("deepseek-concrete".into()),
        })
    );
    assert_eq!(
        store
            .task_events("configured-create")
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["task.created", "harness.pinned"]
    );
}

#[tokio::test]
async fn create_maps_modes_and_rejects_invalid_routes_without_a_trace() {
    let fixture = DaemonFixture::new("validation");
    let mut client = fixture.connect("t04-validation").await;
    configure_providers(&fixture, &mut client).await;

    for (mode, expected) in [
        ("ask", TaskKind::Conversation),
        ("plan", TaskKind::PlanDraft),
        ("edit", TaskKind::Implementation),
        ("auto", TaskKind::Implementation),
    ] {
        let task_id = format!("mode-{mode}");
        client
            .call(
                "task.create",
                serde_json::json!({
                    "taskId": task_id,
                    "objective": mode,
                    "mode": mode,
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("create mode {mode}: {error}"));
        let state = fixture
            .store()
            .load_task_with_revision(&task_id)
            .expect("load mode task")
            .expect("mode task exists")
            .0;
        assert_eq!(state.contract.kind, expected, "mode={mode}");
    }

    let invalid = [
        (
            "invalid-provider",
            serde_json::json!({
                "modelRoute": {"kind": "host-provider", "providerId": "not-a-provider"},
                "harnessId": "native.r-code"
            }),
        ),
        (
            "missing-credential",
            serde_json::json!({
                "modelRoute": {"kind": "host-provider", "providerId": "openai"},
                "harnessId": "native.r-code"
            }),
        ),
        (
            "route-harness-mismatch",
            serde_json::json!({
                "modelRoute": {"kind": "harness-managed", "harnessId": "codex.r-code"},
                "harnessId": "native.r-code"
            }),
        ),
        (
            "unavailable-harness",
            serde_json::json!({
                "modelRoute": {"kind": "harness-managed", "harnessId": "missing.r-code"},
                "harnessId": "missing.r-code"
            }),
        ),
    ];
    for (task_id, route) in invalid {
        let mut payload = serde_json::json!({
            "taskId": task_id,
            "objective": "must fail atomically",
            "mode": "ask",
        });
        payload
            .as_object_mut()
            .expect("object")
            .extend(route.as_object().expect("route object").clone());
        let result = client.call("task.create", payload).await;
        assert!(result.is_err(), "{task_id} unexpectedly succeeded");
        assert_absent(&fixture, task_id);
    }
}

#[tokio::test]
async fn concurrent_preference_patches_preserve_both_fields() {
    let directory = tempfile::tempdir().expect("fixture root");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("t04-stale-preference-patches"),
    )
    .expect("profile");
    let service = ApplicationService::compose(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose service");
    service
        .create_task(
            "patch-race",
            "concurrent patches",
            TaskKind::Conversation,
            vec![],
        )
        .await
        .expect("create race task");

    // These are the two documents read by concurrent task.setPreferences handlers before either
    // applies its independent patch. Application CAS must merge or reject a stale full document;
    // silently accepting both while discarding one field is a lost update.
    let mut inference_patch = service
        .task_preferences("patch-race")
        .await
        .expect("read inference base");
    let mut prompt_patch = service
        .task_preferences("patch-race")
        .await
        .expect("read prompt base");
    inference_patch.inference = Some(serde_json::json!({"temperature": 0.25}));
    prompt_patch.system_prompt = Some("concurrent prompt".into());

    let (inference_result, prompt_result) = tokio::join!(
        service.set_task_preferences("patch-race", inference_patch),
        service.set_task_preferences("patch-race", prompt_patch),
    );
    inference_result.expect("apply inference patch");
    prompt_result.expect("apply prompt patch");
    let final_preferences = service
        .task_preferences("patch-race")
        .await
        .expect("read final preferences");
    assert_eq!(
        final_preferences.inference,
        Some(serde_json::json!({"temperature": 0.25})),
        "a stale full-document CAS silently discarded the concurrent inference field"
    );
    assert_eq!(
        final_preferences.system_prompt.as_deref(),
        Some("concurrent prompt")
    );
}

#[tokio::test]
async fn configured_task_pins_its_harness_before_the_first_run() {
    let fixture = DaemonFixture::new("selection-pin");
    let mut client = fixture.connect("t04-pin").await;
    configure_providers(&fixture, &mut client).await;
    client
        .call(
            "task.create",
            serde_json::json!({
                "taskId": "selected-native",
                "objective": "keep selected package",
                "mode": "ask",
                "modelRoute": {
                    "kind": "host-provider",
                    "providerId": "deepseek",
                    "modelId": "deepseek-concrete"
                },
                "harnessId": "native.r-code"
            }),
        )
        .await
        .expect("create configured task");
    let digest = fixture
        .store()
        .list_plugins()
        .expect("list plugin rows")
        .into_iter()
        .find(|entry| entry.id == "native.r-code")
        .expect("native package")
        .content_digest;
    let removal = client
        .call(
            "plugins.remove",
            serde_json::json!({"id": "native.r-code", "digest": digest}),
        )
        .await;
    assert!(
        removal.is_err(),
        "a task-level harness.pinned event must have a durable plugin_pins row before first run"
    );
}

fn package(id: &str, digest: &str) -> PackageRef {
    PackageRef {
        id: HarnessId::new(id),
        version: semver::Version::new(1, 0, 0),
        content_digest: digest.to_string(),
    }
}

fn register_package(store: &V1Store, package: &PackageRef) {
    store
        .register_plugin(&PluginCatalogRecord {
            id: package.id.0.clone(),
            version: package.version.to_string(),
            content_digest: package.content_digest.clone(),
            enabled: true,
            granted_services: vec![],
            config: "{}".into(),
            manifest_json: "{}".into(),
            install_dir: "fixture".into(),
        })
        .expect("register package");
}

fn task_state(task_id: &str) -> TaskState {
    TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind: TaskKind::Conversation,
        objective: "atomic selection".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
        memory: None,
    })
}

#[test]
fn selection_pin_transactions_roll_back_task_events_and_replacements_on_fault() {
    let directory = tempfile::tempdir().expect("fixture root");
    let store = V1Store::open(&directory.path().join("v1.db")).expect("open store");
    let native = package("native.r-code", "sha256:native");
    let codex = package("codex.r-code", "sha256:codex");
    register_package(&store, &native);
    register_package(&store, &codex);
    let state = task_state("atomic-pin");
    let created = JournalEvent {
        seq: 0,
        task_id: "atomic-pin".into(),
        kind: "task.created".into(),
        payload: serde_json::json!({}),
    };

    store.debug_fail_next_save();
    assert!(store
        .create_task_with_selection_pin(&state, vec![created.clone()], &native)
        .is_err());
    assert!(store
        .load_task_with_revision("atomic-pin")
        .expect("query rolled-back task")
        .is_none());
    assert!(store.task_events("atomic-pin").is_empty());
    assert!(store
        .plugin_pin_for_attempt("selection-atomic-pin")
        .expect("query rolled-back pin")
        .is_none());

    store
        .create_task_with_selection_pin(&state, vec![created], &native)
        .expect("create atomically");
    let mut updated = state.clone();
    updated.title = Some("must roll back".into());
    store.debug_fail_next_save();
    assert!(store
        .save_task_events_and_selection_pin_if_revision(
            &updated,
            vec![JournalEvent {
                seq: 0,
                task_id: "atomic-pin".into(),
                kind: "harness.pinned".into(),
                payload: serde_json::json!({"id": "codex.r-code"}),
            }],
            1,
            Some(&codex),
        )
        .is_err());
    let (persisted, revision) = store
        .load_task_with_revision("atomic-pin")
        .expect("query original task")
        .expect("original task remains");
    assert_eq!(revision, 1);
    assert_eq!(persisted.title, None);
    assert_eq!(store.task_events("atomic-pin").len(), 1);
    let pin = store
        .plugin_pin_for_attempt("selection-atomic-pin")
        .expect("query preserved pin")
        .expect("native selection remains");
    assert_eq!(pin.id, "native.r-code");
    assert_eq!(pin.content_digest, native.content_digest);
}

#[tokio::test]
async fn idle_harness_change_replaces_selection_pin_and_active_rejection_preserves_it() {
    let directory = tempfile::tempdir().expect("fixture root");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("t04-selection-replacement"),
    )
    .expect("profile");
    let resources = directory.path().join("packages");
    stage_harness(&resources, "native.r-code", true);
    stage_harness(&resources, "codex.r-code", false);
    let service = ApplicationService::compose(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose service");
    let native = service
        .install_package_from_directory(&resources.join("plugins/native.r-code"))
        .expect("install native");
    let codex = service
        .install_package_from_directory(&resources.join("plugins/codex.r-code"))
        .expect("install codex");
    service
        .create_task_configured(CreateTaskInput {
            task_id: "selection-task".into(),
            objective: "selection lifecycle".into(),
            title: None,
            kind: TaskKind::Conversation,
            required_checks: vec![],
            memory: None,
            preferences: TaskPreferences::default(),
            harness_id: Some("native.r-code".into()),
        })
        .await
        .expect("create with native");
    service
        .select_harness("selection-task", "codex.r-code")
        .await
        .expect("idle harness switch");

    let store = V1Store::open(&profile.database_path()).expect("open store");
    let selected = store
        .plugin_pin_for_attempt("selection-selection-task")
        .expect("selection pin")
        .expect("selection exists");
    assert_eq!(selected.id, "codex.r-code");
    assert_eq!(selected.content_digest, codex.package_ref.content_digest);
    assert!(
        store
            .plugin_pins_for("native.r-code", &native.package_ref.content_digest)
            .expect("native pins")
            .is_empty(),
        "idle switch must replace, not accumulate, the selection pin"
    );

    let (mut state, revision) = store
        .load_task_with_revision("selection-task")
        .expect("load task")
        .expect("task exists");
    state
        .start_attempt(&Attempt {
            attempt_id: "active-attempt".into(),
            task_id: "selection-task".into(),
            branch_id: "main".into(),
            package: codex.package_ref.clone(),
            contract_revision: state.contract.revision,
            config_hash: "legacy-test".into(),
            workspace_identity: "unbound-read-only".into(),
            run_id: "active-run".into(),
        })
        .expect("mark task active");
    store
        .save_task_and_events_if_revision(&state, vec![], revision)
        .expect("persist active state");
    let before_rejection = store
        .load_task_with_revision("selection-task")
        .expect("load active revision")
        .expect("active task");
    assert!(
        service
            .select_harness("selection-task", "native.r-code")
            .await
            .is_err(),
        "active harness changes must be rejected"
    );
    let after_rejection = store
        .load_task_with_revision("selection-task")
        .expect("load after rejection")
        .expect("task remains");
    assert_eq!(after_rejection, before_rejection);
    let selected = store
        .plugin_pin_for_attempt("selection-selection-task")
        .expect("selection pin after rejection")
        .expect("selection remains");
    assert_eq!(selected.id, "codex.r-code");
    assert_eq!(selected.content_digest, codex.package_ref.content_digest);
}

#[tokio::test]
async fn concrete_host_route_and_harness_managed_route_freeze_without_live_default_drift() {
    let directory = tempfile::tempdir().expect("fixture root");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("t04-route-snapshot"),
    )
    .expect("profile");
    let environment = format!("R_CODE_T04_SNAPSHOT_KEY_{}", std::process::id());
    std::env::set_var(&environment, "snapshot-secret");
    let settings = SettingsStore::for_profile(&profile);
    settings
        .compare_and_swap(
            0,
            V1Settings {
                revision: 0,
                providers: vec![ProviderEntry {
                    selection: "deepseek".into(),
                    model: "deepseek-default-a".into(),
                    base_url: None,
                    protocol: None,
                    env_var: Some(environment.clone()),
                }],
                default_selection: Some("deepseek".into()),
            },
        )
        .expect("configure provider");
    let models: Arc<dyn r_code_kernel::ports::ModelService> =
        Arc::new(r_code_kernel::testing::FakeModelService::default());
    let tools: Arc<dyn r_code_kernel::ports::ToolService> =
        Arc::new(r_code_kernel::testing::FakeToolService::default());
    let builder = RunSnapshotBuilder::new(&settings, &models, &tools, false);
    let native = package("native.r-code", "sha256:native-snapshot");
    let mut host_state = task_state("host-route");
    host_state.preferences.model_route = Some(ModelRoute::HostProvider {
        provider_id: "deepseek".into(),
        model_id: Some("deepseek-concrete".into()),
    });
    let guard = RunGuard::new("host-run", 1);
    let frozen = builder
        .freeze(&host_state, &native, &[HostService::ModelStream], &guard)
        .await
        .expect("freeze concrete host route");
    assert_eq!(
        frozen.snapshot.material().provider.model_id,
        "deepseek-concrete"
    );
    assert_eq!(frozen.snapshot.material().provider.settings_revision, 1);

    settings
        .compare_and_swap(
            1,
            V1Settings {
                revision: 1,
                providers: vec![ProviderEntry {
                    selection: "deepseek".into(),
                    model: "deepseek-default-b".into(),
                    base_url: None,
                    protocol: None,
                    env_var: Some(environment.clone()),
                }],
                default_selection: Some("deepseek".into()),
            },
        )
        .expect("change provider default");
    let next = builder
        .freeze(
            &host_state,
            &native,
            &[HostService::ModelStream],
            &RunGuard::new("next-host-run", 1),
        )
        .await
        .expect("freeze next concrete route");
    assert_eq!(
        frozen.snapshot.material().provider.model_id,
        "deepseek-concrete"
    );
    assert_eq!(
        next.snapshot.material().provider.model_id,
        "deepseek-concrete"
    );
    assert_eq!(next.snapshot.material().provider.settings_revision, 2);

    std::env::remove_var(&environment);
    let mut managed_state = task_state("managed-route");
    managed_state.preferences.model_route = Some(ModelRoute::HarnessManaged {
        harness_id: "codex.r-code".into(),
        model_id: Some("codex-concrete".into()),
    });
    let managed = builder
        .freeze(
            &managed_state,
            &package("codex.r-code", "sha256:codex-snapshot"),
            &[],
            &RunGuard::new("managed-run", 1),
        )
        .await
        .expect("managed route must not read host credentials");
    assert_eq!(
        managed.snapshot.material().provider.kind,
        ProviderRouteKind::HarnessManaged
    );
    assert_eq!(
        managed.snapshot.material().provider.model_id,
        "codex-concrete"
    );
    assert!(!managed
        .snapshot
        .material()
        .permissions
        .capabilities
        .iter()
        .any(|capability| capability == "host.model.stream"));
}
