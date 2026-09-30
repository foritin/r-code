use r_code_kernel::task::ProviderRouteKind;
use r_code_runtime::profile::ProfileFlavor;
use r_code_runtime::services::models::{FrozenProviderResolver, ProviderResolver};
use r_code_runtime::services::settings_store::{
    ProviderEntry, RunProviderResolution, SettingsStore, SettingsStoreError, V1Settings,
};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};

#[cfg(not(target_os = "macos"))]
use std::collections::HashMap;
#[cfg(not(target_os = "macos"))]
use std::sync::{Mutex, Once, OnceLock};

#[cfg(not(target_os = "macos"))]
static INSTALL_MOCK_KEYRING: Once = Once::new();
#[cfg(not(target_os = "macos"))]
static KEYRING_SERVICES: OnceLock<Mutex<HashMap<std::thread::ThreadId, Vec<String>>>> =
    OnceLock::new();

#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
struct RecordingCredentialBuilder;

#[cfg(not(target_os = "macos"))]
impl keyring::credential::CredentialBuilderApi for RecordingCredentialBuilder {
    fn build(
        &self,
        target: Option<&str>,
        service: &str,
        user: &str,
    ) -> keyring::Result<Box<keyring::credential::Credential>> {
        KEYRING_SERVICES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .expect("record keyring service")
            .entry(std::thread::current().id())
            .or_default()
            .push(service.to_string());
        keyring::mock::default_credential_builder().build(target, service, user)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn persistence(&self) -> keyring::credential::CredentialPersistence {
        keyring::credential::CredentialPersistence::EntryOnly
    }
}

#[cfg(not(target_os = "macos"))]
fn install_mock_keyring() {
    INSTALL_MOCK_KEYRING.call_once(|| {
        keyring::set_default_credential_builder(Box::new(RecordingCredentialBuilder));
    });
}

#[cfg(not(target_os = "macos"))]
fn take_recorded_services() -> Vec<String> {
    KEYRING_SERVICES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("read recorded keyring services")
        .remove(&std::thread::current().id())
        .unwrap_or_default()
}

fn isolated_store() -> (tempfile::TempDir, SettingsStore) {
    #[cfg(not(target_os = "macos"))]
    install_mock_keyring();

    let directory = tempfile::tempdir().expect("create isolated settings root");
    let store = SettingsStore::new(directory.path());
    (directory, store)
}

fn provider(model: &str) -> ProviderEntry {
    ProviderEntry {
        selection: "deepseek".to_string(),
        model: model.to_string(),
        base_url: None,
        protocol: None,
        env_var: None,
    }
}

fn settings(model: &str) -> V1Settings {
    V1Settings {
        revision: 999,
        providers: vec![provider(model)],
        default_selection: Some("deepseek".to_string()),
    }
}

fn settings_path(root: &Path) -> std::path::PathBuf {
    root.join("settings.json")
}

fn temporary_artifacts(root: &Path) -> Vec<String> {
    fs::read_dir(root)
        .expect("read settings root")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(".settings.json.r-code-tmp-"))
        .collect()
}

fn expect_corrupt<T>(result: Result<T, SettingsStoreError>) -> SettingsStoreError {
    match result {
        Err(error @ SettingsStoreError::Corrupt { .. }) => error,
        Err(error) => panic!("expected corrupt settings error, got {error:?}"),
        Ok(_) => panic!("corrupt settings unexpectedly succeeded"),
    }
}

fn assert_corrupt_mutation_is_rejected(
    root: &Path,
    operation: impl FnOnce() -> Result<(), SettingsStoreError>,
) {
    const CORRUPT: &str = r#"{"providers":[{"selection":"deepseek"}],"secret":"qa-secret""#;
    let path = settings_path(root);
    fs::write(&path, CORRUPT).expect("write corrupt fixture");

    let error = expect_corrupt(operation());

    assert_eq!(
        fs::read_to_string(&path).expect("read preserved corrupt fixture"),
        CORRUPT,
        "a rejected mutation must not replace the on-disk document"
    );
    assert!(temporary_artifacts(root).is_empty());
    let rendered = format!("{error:?}\n{error}");
    assert!(!rendered.contains("qa-secret"));
}

#[test]
fn legacy_document_without_revision_loads_at_revision_zero() {
    let (directory, store) = isolated_store();
    fs::write(
        settings_path(directory.path()),
        r#"{"providers":[],"default_selection":null}"#,
    )
    .expect("write legacy settings fixture");

    let loaded = store.load_checked().expect("load legacy settings");

    assert_eq!(loaded.revision, 0);
    assert!(loaded.providers.is_empty());
}

#[test]
fn concurrent_compare_and_swap_rejects_one_stale_writer() {
    let (directory, store) = isolated_store();
    let store = Arc::new(store);
    let barrier = Arc::new(Barrier::new(3));
    let mut writers = Vec::new();

    for model in ["deepseek-v4-flash", "deepseek-v4-pro"] {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        writers.push(std::thread::spawn(move || {
            barrier.wait();
            store.compare_and_swap(0, settings(model))
        }));
    }
    barrier.wait();

    let results: Vec<_> = writers
        .into_iter()
        .map(|writer| writer.join().expect("writer thread"))
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(
                result,
                Err(SettingsStoreError::StaleRevision {
                    expected: 0,
                    actual: 1
                })
            ))
            .count(),
        1
    );

    let persisted = store.load_checked().expect("load winning revision");
    assert_eq!(persisted.revision, 1);
    assert!(temporary_artifacts(directory.path()).is_empty());
}

#[test]
fn atomic_replacement_advances_revision_without_partial_or_temp_files() {
    let (directory, store) = isolated_store();
    let first = store
        .compare_and_swap(0, settings("deepseek-v4-flash"))
        .expect("create revision one");
    assert_eq!(first.revision, 1);

    let second = store
        .compare_and_swap(1, settings("deepseek-v4-pro"))
        .expect("atomically replace an existing settings file");
    assert_eq!(second.revision, 2);
    assert!(temporary_artifacts(directory.path()).is_empty());

    let path = settings_path(directory.path());
    let before_stale_attempt = fs::read(&path).expect("read revision two");
    let persisted: V1Settings =
        serde_json::from_slice(&before_stale_attempt).expect("complete settings JSON");
    assert_eq!(persisted, second);

    let error = store
        .compare_and_swap(1, settings("stale-model"))
        .expect_err("stale revision must be rejected");
    assert!(matches!(
        error,
        SettingsStoreError::StaleRevision {
            expected: 1,
            actual: 2
        }
    ));
    assert_eq!(
        fs::read(&path).expect("read file after stale attempt"),
        before_stale_attempt,
        "a stale write must not alter the previous complete file"
    );
    assert!(temporary_artifacts(directory.path()).is_empty());
}

#[test]
fn revision_fenced_mutators_reject_stale_callers_and_return_new_documents() {
    let (directory, store) = isolated_store();
    let first = store
        .apply_provider_at_revision(0, provider("deepseek-v4-flash"), None)
        .expect("revision-fenced provider apply");
    assert_eq!(first.revision, 1);
    assert_eq!(first.providers.len(), 1);
    let revision_one = fs::read(settings_path(directory.path())).expect("read revision one");

    for error in [
        store
            .apply_provider_at_revision(0, provider("stale-model"), None)
            .expect_err("stale provider apply"),
        store
            .set_default_at_revision(0, "deepseek")
            .expect_err("stale default update"),
        store
            .remove_provider_at_revision(0, "deepseek")
            .expect_err("stale provider removal"),
    ] {
        assert!(matches!(
            error,
            SettingsStoreError::StaleRevision {
                expected: 0,
                actual: 1
            }
        ));
    }
    assert_eq!(
        fs::read(settings_path(directory.path())).expect("read after stale mutations"),
        revision_one
    );

    let second = store
        .set_default_at_revision(1, "deepseek")
        .expect("revision-fenced default update");
    assert_eq!(second.revision, 2);
    assert_eq!(second.default_selection.as_deref(), Some("deepseek"));

    let third = store
        .remove_provider_at_revision(2, "deepseek")
        .expect("revision-fenced provider removal");
    assert_eq!(third.revision, 3);
    assert!(third.providers.is_empty());
    assert_eq!(third.default_selection, None);
    assert_eq!(store.load_checked().expect("load final settings"), third);
}

#[test]
fn corrupt_document_fails_closed_for_reads_routes_and_every_mutation() {
    let (directory, store) = isolated_store();
    let root = directory.path();
    let path = settings_path(root);
    fs::write(&path, r#"{"providers":[],"secret":"qa-secret","broken":}"#)
        .expect("write corrupt fixture");

    let load_error = expect_corrupt(store.load_checked());
    let registry_error = expect_corrupt(store.registry_checked());
    let route_error = expect_corrupt(store.resolve_provider_snapshot("deepseek", None));
    for error in [load_error, registry_error, route_error] {
        let rendered = format!("{error:?}\n{error}");
        assert!(!rendered.contains("qa-secret"));
    }
    let fail_closed_registry = store.registry();
    assert!(fail_closed_registry.resolve("deepseek").is_none());
    assert!(fail_closed_registry.default_selection().is_empty());

    assert_corrupt_mutation_is_rejected(root, || {
        store
            .compare_and_swap(0, settings("replacement"))
            .map(|_| ())
    });
    assert_corrupt_mutation_is_rejected(root, || store.update(0, |_| {}).map(|_| ()));
    assert_corrupt_mutation_is_rejected(root, || store.save(&V1Settings::default()));
    assert_corrupt_mutation_is_rejected(root, || {
        store.apply_provider(provider("replacement"), None)
    });
    assert_corrupt_mutation_is_rejected(root, || store.remove_provider("deepseek"));
    assert_corrupt_mutation_is_rejected(root, || store.set_default("deepseek"));
}

#[test]
fn resolved_snapshot_freezes_revision_overrides_and_capabilities_without_secret() {
    let (directory, store) = isolated_store();
    let environment = format!("R_CODE_T02_KEY_{}", std::process::id());
    let secret = "qa-deepseek-secret-value";
    std::env::set_var(&environment, secret);

    let initial = V1Settings {
        revision: 7,
        providers: vec![ProviderEntry {
            selection: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
            base_url: Some("https://initial.example.test".to_string()),
            protocol: Some("openai_chat".to_string()),
            env_var: Some(environment.clone()),
        }],
        default_selection: Some("deepseek".to_string()),
    };
    fs::write(
        settings_path(directory.path()),
        serde_json::to_vec_pretty(&initial).expect("serialize fixture"),
    )
    .expect("write initial settings");

    let frozen = store
        .resolve_provider_snapshot("deepseek", Some("deepseek-v4-pro"))
        .expect("resolve provider snapshot");
    assert_eq!(frozen.kind, ProviderRouteKind::HostProvider);
    assert_eq!(frozen.settings_revision, 7);
    assert_eq!(frozen.provider_id, "deepseek");
    assert_eq!(frozen.model_id, "deepseek-v4-pro");
    assert_eq!(
        frozen.base_url.as_deref(),
        Some("https://initial.example.test")
    );
    assert_eq!(frozen.protocol.as_deref(), Some("openai_chat"));
    assert_eq!(
        frozen.capabilities,
        vec!["prompt-caching", "streaming", "tools"]
    );

    let changed = V1Settings {
        revision: 8,
        providers: vec![ProviderEntry {
            selection: "deepseek".to_string(),
            model: "deepseek-v4-flash-vision-exp".to_string(),
            base_url: Some("https://changed.example.test".to_string()),
            protocol: Some("openai_responses".to_string()),
            env_var: Some(environment.clone()),
        }],
        default_selection: Some("deepseek".to_string()),
    };
    fs::write(
        settings_path(directory.path()),
        serde_json::to_vec_pretty(&changed).expect("serialize changed fixture"),
    )
    .expect("change settings after resolution");
    let current = store
        .resolve_provider_snapshot("deepseek", None)
        .expect("resolve changed snapshot");

    assert_eq!(frozen.settings_revision, 7);
    assert_eq!(frozen.model_id, "deepseek-v4-pro");
    assert_eq!(
        frozen.base_url.as_deref(),
        Some("https://initial.example.test")
    );
    assert!(!frozen.capabilities.iter().any(|item| item == "vision"));
    assert_eq!(current.settings_revision, 8);
    assert_eq!(current.model_id, "deepseek-v4-flash-vision-exp");
    assert!(current.capabilities.iter().any(|item| item == "vision"));

    let serialized = serde_json::to_string(&frozen).expect("serialize route snapshot");
    for forbidden in [secret, environment.as_str(), "api_key"] {
        assert!(
            !serialized.contains(forbidden),
            "snapshot leaked forbidden credential material: {forbidden}"
        );
    }
    std::env::remove_var(&environment);
}

#[test]
fn frozen_provider_resolver_never_rereads_changed_settings_default_or_credential() {
    let (directory, store) = isolated_store();
    let deepseek_key = format!("R_CODE_T03_DS_KEY_{}", std::process::id());
    let openai_key = format!("R_CODE_T03_OAI_KEY_{}", std::process::id());
    std::env::set_var(&deepseek_key, "deepseek-key-before-freeze");
    std::env::set_var(&openai_key, "openai-key-for-next-run");
    let initial = V1Settings {
        revision: 4,
        providers: vec![
            ProviderEntry {
                selection: "deepseek".into(),
                model: "deepseek-before-freeze".into(),
                base_url: Some("https://before.example.test/v1".into()),
                protocol: Some("openai_chat".into()),
                env_var: Some(deepseek_key.clone()),
            },
            ProviderEntry {
                selection: "openai".into(),
                model: "openai-for-next-run".into(),
                base_url: Some("https://next.example.test/v1".into()),
                protocol: Some("openai_chat".into()),
                env_var: Some(openai_key.clone()),
            },
        ],
        default_selection: Some("deepseek".into()),
    };
    fs::write(
        settings_path(directory.path()),
        serde_json::to_vec_pretty(&initial).expect("serialize initial settings"),
    )
    .expect("write initial settings");

    let resolved = match store
        .resolve_provider_for_run(None, None)
        .expect("resolve first run")
    {
        RunProviderResolution::Resolved(resolved) => resolved,
        RunProviderResolution::Unconfigured { .. } => panic!("provider should be configured"),
    };
    let (snapshot, provider) = resolved.into_parts();
    let frozen = FrozenProviderResolver::new(
        snapshot.provider_id.clone(),
        provider,
        snapshot.model_id.clone(),
    );

    std::env::set_var(&deepseek_key, "deepseek-key-rotated-after-freeze");
    let changed = V1Settings {
        revision: 5,
        default_selection: Some("openai".into()),
        ..initial
    };
    fs::write(
        settings_path(directory.path()),
        serde_json::to_vec_pretty(&changed).expect("serialize changed settings"),
    )
    .expect("change settings after freeze");

    assert_eq!(frozen.default_selection(), "deepseek");
    let (first_provider, first_model) = frozen.resolve("deepseek").expect("frozen route");
    let (same_provider, same_model) = frozen.resolve("deepseek").expect("same frozen route");
    assert!(Arc::ptr_eq(&first_provider, &same_provider));
    assert_eq!(first_model, "deepseek-before-freeze");
    assert_eq!(same_model, first_model);
    assert!(frozen.resolve("openai").is_none());

    let next = match store
        .resolve_provider_for_run(None, None)
        .expect("resolve next run")
    {
        RunProviderResolution::Resolved(resolved) => resolved,
        RunProviderResolution::Unconfigured { .. } => panic!("provider should be configured"),
    };
    assert_eq!(snapshot.settings_revision, 4);
    assert_eq!(snapshot.provider_id, "deepseek");
    assert_eq!(snapshot.model_id, "deepseek-before-freeze");
    assert_eq!(next.snapshot().settings_revision, 5);
    assert_eq!(next.snapshot().provider_id, "openai");
    assert_eq!(next.snapshot().model_id, "openai-for-next-run");

    std::env::remove_var(&deepseek_key);
    std::env::remove_var(&openai_key);
}

#[test]
fn provider_errors_do_not_render_resolved_secret_material() {
    let (directory, store) = isolated_store();
    let environment = format!("R_CODE_T02_BAD_PROTOCOL_KEY_{}", std::process::id());
    let secret = "qa-secret-that-must-not-be-rendered";
    std::env::set_var(&environment, secret);
    let invalid = V1Settings {
        revision: 11,
        providers: vec![ProviderEntry {
            selection: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            base_url: None,
            protocol: Some("not-a-protocol".to_string()),
            env_var: Some(environment.clone()),
        }],
        default_selection: Some("deepseek".to_string()),
    };
    fs::write(
        settings_path(directory.path()),
        serde_json::to_vec_pretty(&invalid).expect("serialize invalid fixture"),
    )
    .expect("write invalid protocol fixture");

    let error = store
        .resolve_provider_snapshot("deepseek", None)
        .expect_err("invalid protocol must fail");
    assert!(matches!(error, SettingsStoreError::InvalidProtocol { .. }));
    let rendered = format!("{error:?}\n{error}");
    assert!(!rendered.contains(secret));
    assert!(!rendered.contains("api_key"));
    std::env::remove_var(&environment);
}

#[test]
fn development_and_production_publish_distinct_credential_namespaces() {
    assert_eq!(
        ProfileFlavor::Development.credential_service(),
        "r-code-harness-v1-dev"
    );
    assert_eq!(
        ProfileFlavor::Production.credential_service(),
        "r-code-harness-v1"
    );
    assert_ne!(
        ProfileFlavor::Development.credential_service(),
        ProfileFlavor::Production.credential_service()
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn development_daemon_composition_uses_development_credential_namespace() {
    install_mock_keyring();
    let _ = take_recorded_services();
    let directory = tempfile::tempdir().expect("create profile data root");
    let profile = r_code_runtime::profile::RuntimeProfile::resolve(
        &r_code_runtime::profile::LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path()),
    )
    .expect("resolve development profile");
    profile.ensure_layout().expect("create profile layout");
    fs::write(
        settings_path(&profile.harness_v1_root()),
        serde_json::to_vec_pretty(&V1Settings {
            revision: 3,
            providers: vec![provider("deepseek-v4-pro")],
            default_selection: Some("deepseek".to_string()),
        })
        .expect("serialize settings fixture"),
    )
    .expect("write settings fixture");
    let application = r_code_runtime::application::ApplicationService::compose(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose development daemon");

    let rows = application
        .settings()
        .availability_checked()
        .expect("read provider availability");
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].has_credential);
    let services = take_recorded_services();
    assert_eq!(
        services,
        vec!["r-code-harness-v1-dev", "r-code-harness-v2-dev"],
        "development daemon must not query production credential namespaces"
    );
}
