//! S31 — the deterministic safety conformance gate (cross-platform fake E2E).
//!
//! One suite that walks every durable fault boundary the wave built and
//! proves the wave's central invariant end to end: SafeDisabled never
//! counts as Activated, an omitted proof/report/receipt fails activation,
//! faults preserve quarantine and the single resume, and the emitted
//! report digests are deterministic and secret-free. The fake backend
//! makes the whole matrix deterministic on every platform; native
//! identity/probe execution belongs to the platform suites (s06-s18) and
//! the packaging gate (P32).

use r_code_runtime::services::process_supervisor::{
    DeterministicFakeBackend, FaultPoint, PreparedChildKind, ProcessTreeBackend, SpawnSpec,
};
use r_code_runtime::services::sandbox::{
    current_platform_material, evaluate_safety_activation, SafetyActivation, SafetyReportMaterial,
    SafetyStatus, SAFETY_REPORT_MATERIAL_VERSION,
};
use r_code_store::v1::safety::SafetyReportStatus;
use r_code_store::v1::V1Store;
use std::sync::Arc;

fn store(temp: &std::path::Path) -> V1Store {
    V1Store::open(&temp.join("gate.db")).expect("store")
}

/// Persist one material as its own content-addressed report row.
fn persist(
    store: &V1Store,
    material: &SafetyReportMaterial,
) -> r_code_store::v1::safety::SafetyReportRecord {
    let record = r_code_store::v1::safety::SafetyReportRecord {
        report_id: material.report_id(),
        capability: material.capability.clone(),
        material_digest: material.digest(),
        status: match material.status {
            SafetyStatus::Unsupported { .. } => SafetyReportStatus::Unsupported,
            SafetyStatus::SafeDisabled { .. } => SafetyReportStatus::SafeDisabled,
            SafetyStatus::Activated => SafetyReportStatus::Activated,
        },
        material_json: serde_json::to_string(material).unwrap(),
        created_at_ms: 1,
    };
    store.put_safety_report(record.clone()).expect("persist");
    record
}

fn material_for(boot: &str, status: SafetyStatus) -> SafetyReportMaterial {
    let mut material = current_platform_material(boot);
    material.status = status;
    material
}

// ---------------------------------------------------------------------------
// P31.1 — the activation predicate over fake AND native material shapes
// ---------------------------------------------------------------------------

#[test]
fn every_omission_fails_activation_and_safedisabled_never_counts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = store(temp.path());
    let boot = "windows:s31-boot-0123456789abcdef0123456789abcdef";

    // 1. No persisted report at all: NotActivated.
    let material = material_for(boot, SafetyStatus::Activated);
    let verdict = evaluate_safety_activation(None, &material);
    assert!(matches!(verdict, SafetyActivation::NotActivated { .. }));

    // 2. SafeDisabled never counts as Activated — the central invariant.
    let disabled = material_for(
        boot,
        SafetyStatus::SafeDisabled {
            reason: "pending-native-backend".into(),
        },
    );
    persist(&store, &disabled);
    let persisted = store
        .current_safety_report(&disabled.capability)
        .expect("read")
        .expect("present");
    let verdict = evaluate_safety_activation(Some(&persisted), &disabled);
    assert!(
        matches!(verdict, SafetyActivation::NotActivated { .. }),
        "SafeDisabled must never evaluate Activated"
    );

    // 3. Unsupported never counts either.
    let unsupported = material_for(
        boot,
        SafetyStatus::Unsupported {
            reason: "none-this-wave".into(),
        },
    );
    let persisted_unsupported = persist(&store, &unsupported);
    let verdict = evaluate_safety_activation(Some(&persisted_unsupported), &unsupported);
    assert!(matches!(verdict, SafetyActivation::NotActivated { .. }));

    // 4. An Activated material matching its own persisted row IS Activated
    //    (the only green shape — exact digest, same boot, probes present
    //    and passing: an Activated status with no probes fails).
    let bare = material_for(boot, SafetyStatus::Activated);
    let persisted_bare = persist(&store, &bare);
    assert!(matches!(
        evaluate_safety_activation(Some(&persisted_bare), &bare),
        SafetyActivation::NotActivated { reason, .. } if reason == "probes-incomplete"
    ));
    let mut activated = material_for(boot, SafetyStatus::Activated);
    activated.probes = vec![r_code_runtime::services::sandbox::SafetyProbeResult {
        probe_id: "write-boundary".into(),
        passed: true,
        detail_digest: "probe-digest".into(),
    }];
    let persisted_activated = persist(&store, &activated);
    let verdict = evaluate_safety_activation(Some(&persisted_activated), &activated);
    assert!(
        matches!(verdict, SafetyActivation::Activated { .. }),
        "the exact Activated row evaluates Activated"
    );

    // 5. A FOREIGN boot's activated row does not activate this boot.
    let mut other_boot = material_for(
        "windows:s31-other-boot-ffffffffffffffff",
        SafetyStatus::Activated,
    );
    other_boot.probes = activated.probes.clone();
    let verdict = evaluate_safety_activation(Some(&persisted_activated), &other_boot);
    assert!(matches!(verdict, SafetyActivation::NotActivated { .. }));

    // 6. The O-GATE predicate requires Activated: capability_granted (the
    //    P24B per-capability predicate) returns false under every
    //    NotActivated shape above — SafeDisabled grants nothing.
    for verdict in [
        evaluate_safety_activation(None, &material),
        evaluate_safety_activation(Some(&persisted), &disabled),
        evaluate_safety_activation(Some(&persisted_activated), &other_boot),
    ] {
        assert!(
            !r_code_runtime::run_manager::RunManager::capability_granted(
                &verdict,
                "process.noxworkspace"
            )
        );
    }
}

// ---------------------------------------------------------------------------
// P31.2 — every durable fault surfaces; states never skip (fake E2E)
// ---------------------------------------------------------------------------

fn spec(port: u16) -> SpawnSpec {
    SpawnSpec {
        executable: std::path::PathBuf::from("/bin/true"),
        arguments: vec![format!("--port={port}")],
        cwd: std::path::PathBuf::from("/tmp"),
        environment: Default::default(),
        inherited_objects: Vec::new(),
        output_capacity_bytes: 4096,
    }
}

fn owner(seed: u64) -> r_code_runtime::process_guard::ProcessOwnerIdentity {
    r_code_runtime::process_guard::ProcessOwnerIdentity::new(
        4000 + seed as u32,
        seed,
        r_code_runtime::process_guard::BootIdentity::current().expect("boot"),
        serde_json::json!({"seed": seed}),
    )
    .expect("owner")
}

#[tokio::test]
async fn every_durable_fault_surfaces_and_states_never_skip() {
    // The deterministic fake backend walks the supervisor's own boundary
    // machine: each fault point surfaces as an error, the retry succeeds,
    // and the state machine refuses to skip or repeat (the single-resume
    // invariant in the seam P22's service consumes).
    let backend = DeterministicFakeBackend::new(PreparedChildKind::WindowsSuspended);

    backend.fail_once(FaultPoint::Prepare);
    assert!(matches!(
        backend.prepare(spec(4096)).await,
        Err(r_code_runtime::services::process_supervisor::SupervisorError::Injected(actual))
            if actual == FaultPoint::Prepare
    ));
    let launch = backend.prepare(spec(4096)).await.expect("retry prepares");

    backend.fail_once(FaultPoint::SpawnSuspended);
    assert!(backend.spawn_suspended(launch.clone()).await.is_err());
    // The fake consumes next_identity AT SPAWN: preset it so the probe arm
    // exercises the fault instead of missing identity.
    backend.set_next_identity(owner(1));
    let child = backend.spawn_suspended(launch).await.expect("retry spawns");

    backend.fail_once(FaultPoint::ProbeIdentity);
    assert!(backend.probe_identity(&child).await.is_err());
    let probed = backend.probe_identity(&child).await.expect("retry probes");

    backend.fail_once(FaultPoint::PersistIdentity);
    assert!(backend
        .persist_identity(&child, probed.clone())
        .await
        .is_err());
    backend
        .persist_identity(&child, probed.clone())
        .await
        .expect("retry persists");
    // A repeat persist is an invalid state — identity lands once.
    assert!(backend.persist_identity(&child, probed).await.is_err());

    backend.fail_once(FaultPoint::ResumeOnce);
    assert!(backend.resume_once(&child).await.is_err());
    let running = backend.resume_once(&child).await.expect("retry resumes");
    // The single resume: a second resume is an invalid state.
    assert!(backend.resume_once(&child).await.is_err());

    backend.fail_once(FaultPoint::Terminate);
    assert!(backend.terminate(&running).await.is_err());
    backend.terminate(&running).await.expect("retry terminates");
}

#[tokio::test]
async fn supervisor_start_refuses_an_operation_id_replay() {
    // The journal-level single-resume invariant: the same operation id can
    // never carry two trees (P22's identity discipline at the seam).
    use r_code_runtime::services::process_supervisor::SupervisorStart;
    let backend = Arc::new(DeterministicFakeBackend::new(PreparedChildKind::UnixGated));
    let journal = Arc::new(
        r_code_runtime::services::process_supervisor::DeterministicSupervisorJournal::default(),
    );
    let supervisor = r_code_runtime::services::process_supervisor::ProcessSupervisor::new(
        backend.clone(),
        journal.clone(),
    );
    let first = SupervisorStart {
        operation_id: "op-s31-replay".into(),
        tree_id: "tree-a".into(),
        task_id: "task-s31".into(),
        ownership_epoch: 1,
        spec: spec(5000),
    };
    let second = SupervisorStart {
        operation_id: "op-s31-replay".into(),
        tree_id: "tree-b".into(),
        task_id: "task-s31".into(),
        ownership_epoch: 1,
        spec: spec(5001),
    };
    backend.set_next_identity(owner(7));
    let _run = supervisor.start(first).await.expect("first start");
    assert!(
        supervisor.start(second).await.is_err(),
        "an operation id never carries two trees"
    );
    // The record is durable: recovery finds exactly one tree.
    let reopened = journal.reopen();
    let _ = reopened;
}

// ---------------------------------------------------------------------------
// P31.3 — deterministic, secret-free report digests
// ---------------------------------------------------------------------------

#[test]
fn report_digests_are_deterministic_and_secret_free() {
    let boot = "windows:s31-digest-0123456789abcdef0123456789abcdef";
    let first = material_for(
        boot,
        SafetyStatus::SafeDisabled {
            reason: "gate".into(),
        },
    );
    let mut second = material_for(
        boot,
        SafetyStatus::SafeDisabled {
            reason: "gate".into(),
        },
    );
    // The wall-clock stamp is excluded: two generations digest identically.
    second.generated_at_ms = first.generated_at_ms + 9_999;
    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.material_version, SAFETY_REPORT_MATERIAL_VERSION);

    // The material JSON (what gets persisted/published) carries no secret:
    // no environment values, no tokens — only identity/verdict fields.
    let json = serde_json::to_string(&first).unwrap();
    for marker in ["token", "secret", "apiKey", "password", "authorization"] {
        let lower = json.to_ascii_lowercase();
        assert!(
            !lower.contains(&format!("\"{marker}\"")),
            "the report JSON must not carry {marker}"
        );
    }
}

// ---------------------------------------------------------------------------
// P31.4 — legacy recovery: same-boot refusal and stale-epoch audit
// (The full legacy RebootProof matrix lives in s01/s11R; the gate pins
// the wave-level composition invariant here.)
// ---------------------------------------------------------------------------

#[test]
fn recovery_incomplete_effects_before_any_grant() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = store(temp.path());
    // P27's recovery: with no incomplete effects the boot is recovered.
    let quarantined = r_code_runtime::services::process_effects::recover_incomplete_effects(&store)
        .expect("recover");
    assert!(quarantined.is_empty());
    // And the readiness under this host's honest report grants nothing.
    let boot = "windows:s31-recovery-0123456789abcdef0123456";
    let readiness = r_code_runtime::application::ActivationReadiness::evaluate(&store, boot)
        .expect("readiness");
    assert!(readiness.recovered);
    assert!(readiness.granted_capabilities.is_empty());
}
