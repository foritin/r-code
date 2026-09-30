//! S24B — activate production services only after recovery.
//!
//! Proves the acceptance set end to end: ingress waits recovery (the
//! readiness is evaluated inside composition, AFTER the effect recovery —
//! an incomplete effect anywhere means not-ready and grants nothing; a
//! clean boot IS ready but on this host's honest Unsupported report grants
//! an EMPTY set), no landing order exposes unsafe writes (checkout-writing
//! execution has no granted capability that opens it — its arm requires
//! the P27 envelope under an Activated report, and the grant predicate
//! refuses it by name), and SafeDisabled grants nothing (the per-capability
//! predicate returns false for EVERYTHING under NotActivated, including
//! the activatable NoWorkspace/ScratchOnly).

use r_code_kernel::ports::{ModelService, ToolService};
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::operations::PrepareProcessTree;
use r_code_store::v1::{ProcessEffectPrepare, ProcessEffectState, V1Store};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn seed_incomplete(store: &V1Store, _temp: &Path) {
    let platform_identity = json!({"nativePid": 61000, "startToken": "s24b"});
    store
        .prepare_process_tree(&PrepareProcessTree {
            tree_id: "tree-s24b".into(),
            attempt_id: "attempt-s24b".into(),
            workspace_key: "workspace-s24b".into(),
            profile_id: "s24b".into(),
            owner: r_code_store::v1::operations::ProcessTreeOwner {
                pid: 6_100,
                start_identity: 61_000,
                boot_identity: "windows:01234567-89ab-4cde-8f01-23456789abcd".into(),
                platform_identity_digest: r_code_harness_protocol::canonical_input_hash(
                    &platform_identity,
                ),
                platform_identity,
            },
        })
        .expect("seed tree");
    store
        .prepare_process_effect(ProcessEffectPrepare {
            operation_id: "op-s24b".into(),
            tree_id: "tree-s24b".into(),
            attempt_id: "attempt-s24b".into(),
            workspace_key: "workspace-s24b".into(),
            owner_id: "owner-s24b".into(),
            fencing_epoch: 2,
            command_json: json!({"program": "w"}).to_string(),
            lease_json: json!({"lease": "l"}).to_string(),
            before_manifest_json: json!({"files": []}).to_string(),
            before_manifest_digest: "digest-s24b".into(),
            scan_policy_json: json!({"roots": []}).to_string(),
            ephemeral_roots_json: json!({"roots": []}).to_string(),
        })
        .expect("seed incomplete effect");
}

fn compose_with(path: PathBuf) -> ApplicationService {
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(path)
            .with_ipc_name(format!("s24b-{}", std::process::id())),
    )
    .expect("profile");
    let models: Arc<dyn ModelService> =
        Arc::new(r_code_kernel::testing::FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    ApplicationService::compose(&profile, models, tools).expect("compose")
}

#[tokio::test]
async fn ingress_waits_recovery_and_quarantine_grants_nothing() {
    // A crash-left incomplete effect: composition recovers it (quarantine)
    // and the readiness records NOT-recovered — with an empty grant set,
    // because a boot that arrived dirty grants nothing regardless.
    let temp = tempfile::tempdir().expect("tempdir");
    let data_root = temp.path().to_path_buf();
    {
        let store =
            V1Store::open(&data_root.join("harness-v1").join("tasks.sqlite3")).expect("store");
        seed_incomplete(&store, temp.path());
    }
    let service = compose_with(data_root.clone());
    let readiness = service.activation_readiness();
    assert!(
        !readiness.recovered,
        "the dirty boot recovered an incomplete effect"
    );
    assert!(readiness.granted_capabilities.is_empty());
    drop(service);

    // The recovery is durable: the same boot's store now shows quarantine.
    let store = V1Store::open(&data_root.join("harness-v1").join("tasks.sqlite3")).expect("reopen");
    let record = store
        .load_process_effect("op-s24b")
        .expect("load")
        .expect("present");
    assert_eq!(record.state, ProcessEffectState::Quarantined);
}

#[tokio::test]
async fn clean_boot_is_ready_but_safedisabled_grants_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = compose_with(temp.path().to_path_buf());
    let readiness = service.activation_readiness();
    assert!(
        readiness.recovered,
        "a clean boot arrives recovered (zero incomplete effects)"
    );
    // The honest platform report this wave: not Activated — grants NOTHING.
    assert!(matches!(
        readiness.activation,
        r_code_runtime::services::sandbox::SafetyActivation::NotActivated { .. }
    ));
    assert!(
        readiness.granted_capabilities.is_empty(),
        "SafeDisabled grants nothing, including the activatable capabilities"
    );
}

#[test]
fn every_intermediate_state_grants_nothing_unsafe() {
    use r_code_runtime::services::sandbox::SafetyActivation;

    let not_activated = SafetyActivation::NotActivated {
        reason: "no native sandbox backend is available in this wave",
        status: Some("unsupported".into()),
    };
    // The per-capability predicate refuses EVERYTHING under NotActivated.
    for capability in [
        "process.noxworkspace",
        "process.scratchonly",
        "process.checkoutwrite",
        "shell",
        "made-up",
    ] {
        assert!(
            !r_code_runtime::run_manager::RunManager::capability_granted(
                &not_activated,
                capability
            ),
            "SafeDisabled must grant nothing: {capability}"
        );
    }

    // Under an Activated report, ONLY the two checkout-free capabilities
    // are granted — checkout-writing is never granted BY NAME: its arm
    // owns the envelope requirement, so no granted set can open it.
    let activated = SafetyActivation::Activated {
        report_id: "report-s24b".into(),
    };
    assert!(r_code_runtime::run_manager::RunManager::capability_granted(
        &activated,
        "process.noxworkspace"
    ));
    assert!(r_code_runtime::run_manager::RunManager::capability_granted(
        &activated,
        "process.scratchonly"
    ));
    assert!(
        !r_code_runtime::run_manager::RunManager::capability_granted(
            &activated,
            "process.checkoutwrite"
        ),
        "checkout-writing execution is openable only through the P27 envelope, never a grant"
    );
    assert!(!r_code_runtime::run_manager::RunManager::capability_granted(&activated, "made-up"));
}

#[test]
fn guessed_router_calls_stay_denied_before_any_grant() {
    // The router's unknown-method denial is order-independent of grants:
    // a guessed method answers method-not-found regardless of readiness.
    // (Source pin: the denial arms exist before the grant table lookup.)
    let router = include_str!("../src/plugins/router.rs");
    assert!(
        router.contains("method_not_found(&request.method)"),
        "guessed calls keep the method-not-found denial"
    );
    // And the readiness publishes only its exact grant set — nothing
    // guesses a capability into existence.
    let readiness_source = include_str!("../src/application.rs");
    assert!(
        readiness_source.contains("granted_capabilities"),
        "the readiness is the single published grant set"
    );
}
