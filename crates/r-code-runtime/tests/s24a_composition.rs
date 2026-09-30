//! S24A — composition owns every launch site; nothing activates here.
//!
//! Proves the P24A posture on the composition the daemon actually builds:
//! the plugin-facing process surface is the real supervised service in its
//! dormant shape (every launch refuses before a process exists, no testing
//! fake remains in the production composition), and the startup gate
//! regenerates the safety diagnostics for the boot without activating
//! anything.

use r_code_kernel::ports::RunGuard;
use r_code_kernel::ports::{ModelService, ToolService};
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::Path;
use std::sync::Arc;

fn profile_for(name: &str, temp: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.join(name))
            .with_ipc_name(name),
    )
    .expect("profile")
}

/// The daemon composition refuses every process launch from the dormant
/// surface: no fake handle, no spawn, and the refusal names the dormant
/// floor (no host-registered executable behind any profile).
#[tokio::test]
async fn composition_wires_the_dormant_supervised_process_surface() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("s24a-dormant", temp.path());
    let models: Arc<dyn ModelService> =
        Arc::new(r_code_kernel::testing::FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");

    let token = RunGuard::new("run-s24a", 1).token();
    let opened = service
        .process_service()
        .open(
            token.clone(),
            "any-profile",
            vec!["--version".to_string()],
            None,
        )
        .await;
    let error = opened.expect_err("the dormant surface never opens a process");
    let text = error.to_string();
    assert!(
        text.contains("no host-registered executable"),
        "the refusal must name the dormant floor, got: {text}"
    );

    // The rest of the surface refuses too — no fake handle ever exists to
    // read, write or close.
    let read = service
        .process_service()
        .read(
            token.clone(),
            r_code_harness_protocol::services::ProcessReadRequest {
                handle: "bogus".into(),
                cursor: 0,
                max_bytes: 64,
                wait_ms: None,
            },
        )
        .await;
    assert!(read.is_err(), "read on the dormant surface must refuse");
    let written = service
        .process_service()
        .write(token.clone(), "bogus", b"x".to_vec())
        .await;
    assert!(written.is_err(), "write on the dormant surface must refuse");
    let closed = service.process_service().close(token, "bogus").await;
    assert!(closed.is_err(), "close on the dormant surface must refuse");
}

/// Source-level pin: the production composition carries the real supervised
/// service and no testing process fake, and the daemon binary regenerates
/// the boot's safety diagnostics through the activation gate (never by
/// activating anything).
#[test]
fn composition_sources_carry_the_real_surface_and_the_startup_gate() {
    let application = include_str!("../src/application.rs");
    assert!(
        !application.contains("FakeProcessService"),
        "no testing process fake may sit in the production composition"
    );
    assert!(
        application.contains("ManagedProcessService::new"),
        "the dormant supervised service must be the composed surface"
    );

    let service_bin = include_str!("../src/bin/r-code-service.rs");
    assert!(
        service_bin.contains("platform_activation_gate"),
        "startup must regenerate the boot's safety diagnostics through the gate"
    );
    assert!(
        !service_bin.contains("ManagedProcessService"),
        "the s04 wiring pin holds: the daemon binary never constructs the service itself"
    );
}

/// The startup gate mechanism, exercised directly: regenerating the report
/// is idempotent, persists a diagnostics row for the boot, and the verdict
/// stays honestly NotActivated (Unsupported) — regeneration never activates.
#[test]
fn startup_gate_regenerates_diagnostics_without_activating() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("s24a-gate", temp.path());
    let store = r_code_store::v1::V1Store::open(&profile.database_path()).expect("store");
    let boot = r_code_runtime::process_guard::BootIdentity::current().expect("boot identity");

    let first = r_code_runtime::services::sandbox::platform_activation_gate(&store, boot.as_str());
    let second = r_code_runtime::services::sandbox::platform_activation_gate(&store, boot.as_str());
    assert_eq!(first, second, "regeneration is idempotent per boot");
    match &second {
        r_code_runtime::services::sandbox::SafetyActivation::Activated { .. } => {
            panic!("nothing may activate in this wave: {second:?}");
        }
        r_code_runtime::services::sandbox::SafetyActivation::NotActivated { status, .. } => {
            let status = status.clone().unwrap_or_default();
            assert!(
                status.to_ascii_lowercase().contains("unsupported"),
                "the honest verdict is Unsupported, got status {status:?}"
            );
        }
    }
    // The regenerated material is persisted and served: the diagnostics row
    // exists for this boot's capability and its material names THIS boot.
    let persisted = store
        .current_safety_report(r_code_runtime::services::sandbox::SAFETY_CAPABILITY_WRITE_EXECUTION)
        .expect("read current report")
        .expect("regeneration persisted the boot's report row");
    assert_eq!(
        persisted.capability,
        r_code_runtime::services::sandbox::SAFETY_CAPABILITY_WRITE_EXECUTION,
    );
    assert!(
        persisted.material_json.contains(boot.as_str()),
        "the served diagnostics material names THIS boot: {}",
        persisted.material_json
    );
}
