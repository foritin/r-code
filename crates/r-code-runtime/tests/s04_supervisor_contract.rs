//! P04 — concrete supervisor/backend contract and deterministic fake.

use base64::Engine as _;
use r_code_harness_protocol::{ProcessOutputFrame, ProcessOutputStream};
use r_code_runtime::process_guard::{
    BootIdentity, ProcessOwnerIdentity, TerminationProofKind, TerminationProofRecord,
};
use r_code_runtime::services::process_supervisor::{
    DeterministicFakeBackend, FaultPoint, InheritedObject, PreparedChildKind, ProcessTreeBackend,
    SpawnSpec, SupervisorError, MAX_OUTPUT_BUFFER_BYTES,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

const BOOT: &str = "windows:01234567-89ab-4cde-8f01-23456789abcd";

fn inherited(value: usize) -> InheritedObject {
    if cfg!(windows) {
        InheritedObject::WindowsHandle(value)
    } else {
        InheritedObject::UnixFd(i32::try_from(value).unwrap())
    }
}

fn spec(capacity: usize) -> SpawnSpec {
    SpawnSpec {
        executable: PathBuf::from("bin/fixture-child"),
        arguments: vec!["--serve".into()],
        cwd: PathBuf::from("workspace"),
        environment: BTreeMap::from([
            ("LANG".into(), "C.UTF-8".into()),
            ("PATH".into(), "bin".into()),
        ]),
        inherited_objects: vec![inherited(3), inherited(7)],
        output_capacity_bytes: capacity,
    }
}

fn owner(seed: u32) -> ProcessOwnerIdentity {
    ProcessOwnerIdentity::new(
        10_000 + seed,
        20_000 + u64::from(seed),
        BootIdentity::parse(BOOT).unwrap(),
        json!({"native": format!("owner-{seed}")}),
    )
    .unwrap()
}

fn proof(tree_id: &str, epoch: u64) -> TerminationProofRecord {
    let proof_identity = json!({
        "treeId": tree_id,
        "ownershipEpoch": epoch,
        "nativeExit": true,
    });
    TerminationProofRecord {
        proof_id: format!("proof-{tree_id}"),
        tree_id: tree_id.into(),
        ownership_epoch: epoch,
        kind: TerminationProofKind::Exit,
        observed_boot_identity: BootIdentity::parse(BOOT).unwrap(),
        proof_identity_digest: r_code_harness_protocol::canonical_input_hash(&proof_identity),
        proof_identity,
        recorded_at_ms: 123,
    }
}

fn data(sequence: u64, bytes: &[u8]) -> ProcessOutputFrame {
    ProcessOutputFrame::Data {
        sequence,
        stream: ProcessOutputStream::Stdout,
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

#[test]
fn spawn_spec_is_explicit_clean_bounded_and_has_no_ambient_inheritance() {
    let valid = spec(1024);
    valid.validate().unwrap();

    let mut empty_executable = valid.clone();
    empty_executable.executable = PathBuf::new();
    assert!(matches!(
        empty_executable.validate(),
        Err(SupervisorError::InvalidSpec(_))
    ));
    let mut empty_cwd = valid.clone();
    empty_cwd.cwd = PathBuf::new();
    assert!(matches!(
        empty_cwd.validate(),
        Err(SupervisorError::InvalidSpec(_))
    ));
    for capacity in [0, MAX_OUTPUT_BUFFER_BYTES + 1] {
        let mut invalid = valid.clone();
        invalid.output_capacity_bytes = capacity;
        assert!(matches!(
            invalid.validate(),
            Err(SupervisorError::InvalidSpec(_))
        ));
    }

    let allowlist = [
        "COMSPEC",
        "HOME",
        "LANG",
        "LC_ALL",
        "PATH",
        "PATHEXT",
        "SYSTEMROOT",
        "TEMP",
        "TERM",
        "TMP",
        "TMPDIR",
        "TZ",
        "USERPROFILE",
        "WINDIR",
    ];
    for key in allowlist {
        let mut candidate = valid.clone();
        candidate.environment = BTreeMap::from([(key.to_string(), "safe".into())]);
        candidate
            .validate()
            .unwrap_or_else(|error| panic!("{key}: {error}"));
    }
    for forbidden in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "DEEPSEEK_API_KEY",
        "CODEX_HOME",
        "R_CODE_DAEMON_SOCKET",
        "R_CODE_CONTROL_PIPE",
        "SSH_AUTH_SOCK",
    ] {
        let mut candidate = valid.clone();
        candidate.environment = BTreeMap::from([(forbidden.to_string(), "secret".into())]);
        assert!(candidate.validate().is_err(), "accepted {forbidden}");
    }
    let mut case_alias = valid.clone();
    case_alias.environment =
        BTreeMap::from([("PATH".into(), "one".into()), ("Path".into(), "two".into())]);
    assert!(case_alias.validate().is_err());

    let mut duplicate = valid.clone();
    duplicate.inherited_objects = vec![inherited(3), inherited(3)];
    assert!(duplicate.validate().is_err());
    let mut invalid_object = valid.clone();
    invalid_object.inherited_objects = if cfg!(windows) {
        vec![InheritedObject::WindowsHandle(0)]
    } else {
        vec![InheritedObject::UnixFd(-1)]
    };
    assert!(invalid_object.validate().is_err());

    let mut wrong_platform = valid.clone();
    wrong_platform.inherited_objects = if cfg!(windows) {
        vec![InheritedObject::UnixFd(3)]
    } else {
        vec![InheritedObject::WindowsHandle(3)]
    };
    assert!(
        wrong_platform.validate().is_err(),
        "a spawn spec must not mix the other platform's inheritance type"
    );
}

#[tokio::test]
async fn kinds_are_explicit_live_handles_are_opaque_and_durable_records_serialize() {
    for kind in [
        PreparedChildKind::WindowsSuspended,
        PreparedChildKind::UnixGated,
    ] {
        let backend = DeterministicFakeBackend::new(kind);
        let launch = backend.prepare(spec(1024)).await.unwrap();
        let child = backend.spawn_suspended(launch).await.unwrap();
        assert_eq!(child.kind(), kind);
        backend.persist_identity(&child, owner(1)).await.unwrap();
        let running = backend.resume_once(&child).await.unwrap();
        assert_eq!(running.kind(), kind);
        let subscription = backend.subscribe_output(&running).await.unwrap();

        for debug in [
            format!("{child:?}"),
            format!("{running:?}"),
            format!("{subscription:?}"),
        ] {
            assert!(
                !debug.contains("id") && !debug.contains('1'),
                "opaque live handle leaked its numeric identity: {debug}"
            );
        }

        let durable_owner = owner(9);
        let durable_proof = proof("tree-serializable", 7);
        assert!(serde_json::to_string(&durable_owner)
            .unwrap()
            .contains("boot_identity"));
        assert!(serde_json::to_string(&durable_proof)
            .unwrap()
            .contains("proof_id"));
    }

    let source = include_str!("../src/services/process_supervisor.rs");
    assert!(!source.contains("tokio::process::Child"));
    assert!(!source.contains("std::process::Child"));
    for live_type in ["PreparedChild", "RunningTree", "OutputSubscription"] {
        let declaration = source
            .split(&format!("pub struct {live_type}"))
            .next()
            .unwrap();
        let derive = declaration.rsplit("#[derive(").next().unwrap_or_default();
        assert!(
            !derive.contains("Serialize") && !derive.contains("Deserialize"),
            "{live_type} must not be serializable"
        );
    }
}

async fn assert_injected<T>(result: Result<T, SupervisorError>, point: FaultPoint) {
    assert!(matches!(result, Err(SupervisorError::Injected(actual)) if actual == point));
}

#[tokio::test]
async fn every_boundary_fails_once_then_retries_without_skipping_state() {
    let backend = DeterministicFakeBackend::new(PreparedChildKind::UnixGated);

    backend.fail_once(FaultPoint::Prepare);
    assert_injected(backend.prepare(spec(4096)).await, FaultPoint::Prepare).await;
    let launch = backend.prepare(spec(4096)).await.unwrap();

    backend.fail_once(FaultPoint::SpawnSuspended);
    assert_injected(
        backend.spawn_suspended(launch.clone()).await,
        FaultPoint::SpawnSuspended,
    )
    .await;
    let child = backend.spawn_suspended(launch).await.unwrap();

    assert!(matches!(
        backend.resume_once(&child).await,
        Err(SupervisorError::InvalidState(_))
    ));
    backend.fail_once(FaultPoint::PersistIdentity);
    assert_injected(
        backend.persist_identity(&child, owner(2)).await,
        FaultPoint::PersistIdentity,
    )
    .await;
    backend.persist_identity(&child, owner(2)).await.unwrap();
    assert!(matches!(
        backend.persist_identity(&child, owner(2)).await,
        Err(SupervisorError::InvalidState(_))
    ));

    backend.fail_once(FaultPoint::ResumeOnce);
    assert_injected(backend.resume_once(&child).await, FaultPoint::ResumeOnce).await;
    let running = backend.resume_once(&child).await.unwrap();
    assert!(matches!(
        backend.resume_once(&child).await,
        Err(SupervisorError::InvalidState(_))
    ));

    backend.fail_once(FaultPoint::SubscribeOutput);
    assert_injected(
        backend.subscribe_output(&running).await,
        FaultPoint::SubscribeOutput,
    )
    .await;
    let subscription = backend.subscribe_output(&running).await.unwrap();

    backend.fail_once(FaultPoint::EmitOutput);
    assert_injected(
        backend.push_output(&running, data(0, b"abc")),
        FaultPoint::EmitOutput,
    )
    .await;
    backend.push_output(&running, data(0, b"abc")).unwrap();
    backend
        .push_output(
            &running,
            ProcessOutputFrame::Eof {
                sequence: 1,
                stream: ProcessOutputStream::Stdout,
            },
        )
        .unwrap();
    backend
        .push_output(
            &running,
            ProcessOutputFrame::Exit {
                sequence: 2,
                exit_code: Some(0),
            },
        )
        .unwrap();

    backend.fail_once(FaultPoint::Drain);
    assert_injected(
        backend.drain(&subscription, 0, 64, Duration::ZERO).await,
        FaultPoint::Drain,
    )
    .await;
    let page = backend
        .drain(&subscription, 0, 64, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(page.next_cursor, 3);
    assert!(page.terminal);
    assert_eq!(page.exit_code, Some(0));

    backend.fail_once(FaultPoint::Terminate);
    assert_injected(backend.terminate(&running).await, FaultPoint::Terminate).await;
    backend.terminate(&running).await.unwrap();
    backend
        .set_termination_proof(&running, Some(proof("tree-faults", 1)))
        .unwrap();
    backend.fail_once(FaultPoint::WaitAndProve);
    assert_injected(
        backend
            .wait_and_prove(&running, Duration::from_secs(1))
            .await,
        FaultPoint::WaitAndProve,
    )
    .await;
    assert_eq!(
        backend
            .wait_and_prove(&running, Duration::from_secs(1))
            .await
            .unwrap(),
        proof("tree-faults", 1)
    );
}

#[tokio::test]
async fn foreign_handles_and_prepared_launches_never_cross_backend_instances() {
    let left = DeterministicFakeBackend::new(PreparedChildKind::WindowsSuspended);
    let right = DeterministicFakeBackend::new(PreparedChildKind::UnixGated);

    let left_launch = left.prepare(spec(1024)).await.unwrap();
    let transfer_attempt = right.spawn_suspended(left_launch.clone()).await;
    assert!(
        matches!(transfer_attempt, Err(SupervisorError::UnknownHandle)),
        "a PreparedLaunch is backend-owned and must not transfer: {transfer_attempt:?}"
    );

    let left_child = left.spawn_suspended(left_launch).await.unwrap();
    let right_child = right
        .spawn_suspended(right.prepare(spec(1024)).await.unwrap())
        .await
        .unwrap();
    assert_eq!(left_child.kind(), PreparedChildKind::WindowsSuspended);
    assert_eq!(right_child.kind(), PreparedChildKind::UnixGated);

    assert!(matches!(
        right.persist_identity(&left_child, owner(3)).await,
        Err(SupervisorError::UnknownHandle)
    ));
    left.persist_identity(&left_child, owner(3)).await.unwrap();
    right
        .persist_identity(&right_child, owner(4))
        .await
        .unwrap();
    let left_running = left.resume_once(&left_child).await.unwrap();
    let right_running = right.resume_once(&right_child).await.unwrap();

    assert!(matches!(
        right.terminate(&left_running).await,
        Err(SupervisorError::UnknownHandle)
    ));
    assert!(matches!(
        right.subscribe_output(&left_running).await,
        Err(SupervisorError::UnknownHandle)
    ));
    let left_subscription = left.subscribe_output(&left_running).await.unwrap();
    assert!(matches!(
        right.drain(&left_subscription, 0, 1, Duration::ZERO).await,
        Err(SupervisorError::UnknownHandle)
    ));

    right.terminate(&right_running).await.unwrap();
}

#[tokio::test]
async fn foreign_fencing_precedes_fault_consumption_on_every_live_boundary() {
    let left = DeterministicFakeBackend::new(PreparedChildKind::WindowsSuspended);
    let right = DeterministicFakeBackend::new(PreparedChildKind::UnixGated);
    let left_launch = left.prepare(spec(1024)).await.unwrap();
    let right_launch = right.prepare(spec(1024)).await.unwrap();

    right.fail_once(FaultPoint::SpawnSuspended);
    assert_eq!(
        right.spawn_suspended(left_launch.clone()).await,
        Err(SupervisorError::UnknownHandle)
    );
    assert_injected(
        right.spawn_suspended(right_launch.clone()).await,
        FaultPoint::SpawnSuspended,
    )
    .await;
    let left_child = left.spawn_suspended(left_launch).await.unwrap();
    let right_child = right.spawn_suspended(right_launch).await.unwrap();

    right.fail_once(FaultPoint::PersistIdentity);
    assert_eq!(
        right.persist_identity(&left_child, owner(30)).await,
        Err(SupervisorError::UnknownHandle)
    );
    assert_injected(
        right.persist_identity(&right_child, owner(31)).await,
        FaultPoint::PersistIdentity,
    )
    .await;
    left.persist_identity(&left_child, owner(30)).await.unwrap();
    right
        .persist_identity(&right_child, owner(31))
        .await
        .unwrap();

    right.fail_once(FaultPoint::ResumeOnce);
    assert_eq!(
        right.resume_once(&left_child).await,
        Err(SupervisorError::UnknownHandle)
    );
    assert_injected(
        right.resume_once(&right_child).await,
        FaultPoint::ResumeOnce,
    )
    .await;
    let left_tree = left.resume_once(&left_child).await.unwrap();
    let right_tree = right.resume_once(&right_child).await.unwrap();

    right.fail_once(FaultPoint::SubscribeOutput);
    assert_eq!(
        right.subscribe_output(&left_tree).await,
        Err(SupervisorError::UnknownHandle)
    );
    assert_injected(
        right.subscribe_output(&right_tree).await,
        FaultPoint::SubscribeOutput,
    )
    .await;
    let left_subscription = left.subscribe_output(&left_tree).await.unwrap();
    let right_subscription = right.subscribe_output(&right_tree).await.unwrap();

    right.fail_once(FaultPoint::EmitOutput);
    assert_eq!(
        right.push_output(&left_tree, data(0, b"foreign")),
        Err(SupervisorError::UnknownHandle)
    );
    assert_injected(
        right.push_output(&right_tree, data(0, b"right")),
        FaultPoint::EmitOutput,
    )
    .await;
    right.push_output(&right_tree, data(0, b"right")).unwrap();

    right.fail_once(FaultPoint::Drain);
    assert_eq!(
        right.drain(&left_subscription, 0, 1, Duration::ZERO).await,
        Err(SupervisorError::UnknownHandle)
    );
    assert_injected(
        right
            .drain(&right_subscription, 0, 16, Duration::ZERO)
            .await,
        FaultPoint::Drain,
    )
    .await;
    right
        .drain(&right_subscription, 0, 16, Duration::ZERO)
        .await
        .unwrap();

    right
        .push_output(
            &right_tree,
            ProcessOutputFrame::Exit {
                sequence: 1,
                exit_code: Some(0),
            },
        )
        .unwrap();
    right.fail_once(FaultPoint::Terminate);
    assert_eq!(
        right.terminate(&left_tree).await,
        Err(SupervisorError::UnknownHandle)
    );
    assert_injected(right.terminate(&right_tree).await, FaultPoint::Terminate).await;
    right.terminate(&right_tree).await.unwrap();

    assert_eq!(
        right.set_termination_proof(&left_tree, Some(proof("foreign", 1))),
        Err(SupervisorError::UnknownHandle)
    );
    let right_proof = proof("right", 1);
    right
        .set_termination_proof(&right_tree, Some(right_proof.clone()))
        .unwrap();
    right.fail_once(FaultPoint::WaitAndProve);
    assert_eq!(
        right.wait_and_prove(&left_tree, Duration::ZERO).await,
        Err(SupervisorError::UnknownHandle)
    );
    assert_injected(
        right.wait_and_prove(&right_tree, Duration::ZERO).await,
        FaultPoint::WaitAndProve,
    )
    .await;
    assert_eq!(
        right
            .wait_and_prove(&right_tree, Duration::ZERO)
            .await
            .unwrap(),
        right_proof
    );
}

#[tokio::test]
async fn output_log_replays_pages_without_gaps_and_enforces_all_bounds() {
    let backend = DeterministicFakeBackend::new(PreparedChildKind::UnixGated);
    let child = backend
        .spawn_suspended(backend.prepare(spec(32)).await.unwrap())
        .await
        .unwrap();
    backend.persist_identity(&child, owner(5)).await.unwrap();
    let tree = backend.resume_once(&child).await.unwrap();
    let subscription = backend.subscribe_output(&tree).await.unwrap();

    backend.push_output(&tree, data(0, b"abc")).unwrap();
    backend.push_output(&tree, data(1, b"de")).unwrap();
    backend
        .push_output(
            &tree,
            ProcessOutputFrame::Eof {
                sequence: 2,
                stream: ProcessOutputStream::Stdout,
            },
        )
        .unwrap();
    backend
        .push_output(
            &tree,
            ProcessOutputFrame::Exit {
                sequence: 3,
                exit_code: None,
            },
        )
        .unwrap();

    let first = backend
        .drain(&subscription, 0, 3, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(first.frames, [data(0, b"abc")]);
    assert_eq!(first.next_cursor, 1);
    assert!(!first.terminal);
    assert_eq!(
        backend
            .drain(&subscription, 0, 3, Duration::ZERO)
            .await
            .unwrap(),
        first,
        "same cursor must replay the same page"
    );
    let second = backend
        .drain(&subscription, 1, 2, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(second.frames[0].sequence(), 1);
    assert_eq!(second.next_cursor, 4);
    assert!(second.terminal);
    assert_eq!(second.exit_code, None);
    let terminal_empty = backend
        .drain(&subscription, 4, 1, Duration::ZERO)
        .await
        .unwrap();
    assert!(terminal_empty.frames.is_empty());
    assert_eq!(terminal_empty.next_cursor, 4);
    assert!(terminal_empty.terminal);
    assert_eq!(terminal_empty.exit_code, None);

    assert_eq!(
        backend.drain(&subscription, 0, 2, Duration::ZERO).await,
        Err(SupervisorError::PageTooSmall)
    );
    assert_eq!(
        backend.drain(&subscription, 5, 1, Duration::ZERO).await,
        Err(SupervisorError::StaleCursor)
    );
    for (max_bytes, wait) in [
        (0, Duration::ZERO),
        (
            r_code_harness_protocol::PROCESS_READ_MAX_BYTES + 1,
            Duration::ZERO,
        ),
        (
            1,
            Duration::from_millis(u64::from(
                r_code_harness_protocol::PROCESS_READ_MAX_WAIT_MS + 1,
            )),
        ),
    ] {
        assert!(matches!(
            backend.drain(&subscription, 0, max_bytes, wait).await,
            Err(SupervisorError::InvalidSpec(_))
        ));
    }
}

#[tokio::test]
async fn output_state_rejects_sequence_eof_and_terminal_corruption_and_backpressure_is_sticky() {
    let backend = DeterministicFakeBackend::new(PreparedChildKind::UnixGated);
    let child = backend
        .spawn_suspended(backend.prepare(spec(3)).await.unwrap())
        .await
        .unwrap();
    backend.persist_identity(&child, owner(6)).await.unwrap();
    let tree = backend.resume_once(&child).await.unwrap();

    assert!(matches!(
        backend.push_output(&tree, data(1, b"gap")),
        Err(SupervisorError::InvalidState(_))
    ));
    assert!(matches!(
        backend.push_output(
            &tree,
            ProcessOutputFrame::Data {
                sequence: 0,
                stream: ProcessOutputStream::Stdout,
                data_base64: "***".into(),
            }
        ),
        Err(SupervisorError::InvalidState(_))
    ));
    backend.push_output(&tree, data(0, b"abc")).unwrap();
    assert_eq!(
        backend.push_output(&tree, data(1, b"d")),
        Err(SupervisorError::Backpressure)
    );
    assert_eq!(
        backend.push_output(&tree, data(1, b"d")),
        Err(SupervisorError::Backpressure),
        "retry must not mutate or escape the bound"
    );

    let eof_backend = DeterministicFakeBackend::new(PreparedChildKind::UnixGated);
    let child = eof_backend
        .spawn_suspended(eof_backend.prepare(spec(32)).await.unwrap())
        .await
        .unwrap();
    eof_backend
        .persist_identity(&child, owner(7))
        .await
        .unwrap();
    let tree = eof_backend.resume_once(&child).await.unwrap();
    eof_backend
        .push_output(
            &tree,
            ProcessOutputFrame::Eof {
                sequence: 0,
                stream: ProcessOutputStream::Stdout,
            },
        )
        .unwrap();
    assert!(matches!(
        eof_backend.push_output(
            &tree,
            ProcessOutputFrame::Eof {
                sequence: 1,
                stream: ProcessOutputStream::Stdout,
            }
        ),
        Err(SupervisorError::InvalidState(_))
    ));
    assert!(matches!(
        eof_backend.push_output(&tree, data(1, b"late")),
        Err(SupervisorError::InvalidState(_))
    ));
    eof_backend
        .push_output(
            &tree,
            ProcessOutputFrame::Exit {
                sequence: 1,
                exit_code: Some(0),
            },
        )
        .unwrap();
    assert!(matches!(
        eof_backend.push_output(
            &tree,
            ProcessOutputFrame::Eof {
                sequence: 2,
                stream: ProcessOutputStream::Stderr,
            }
        ),
        Err(SupervisorError::InvalidState(_))
    ));
}

#[tokio::test]
async fn proof_configuration_requires_a_coherent_exit_and_wait_distinguishes_unverifiable() {
    let backend = DeterministicFakeBackend::new(PreparedChildKind::UnixGated);
    let child = backend
        .spawn_suspended(backend.prepare(spec(1024)).await.unwrap())
        .await
        .unwrap();
    backend.persist_identity(&child, owner(8)).await.unwrap();
    let tree = backend.resume_once(&child).await.unwrap();

    assert!(matches!(
        backend.set_termination_proof(&tree, Some(proof("tree-no-exit", 1))),
        Err(SupervisorError::InvalidState(_))
    ));
    backend
        .push_output(
            &tree,
            ProcessOutputFrame::Exit {
                sequence: 0,
                exit_code: Some(9),
            },
        )
        .unwrap();
    backend.terminate(&tree).await.unwrap();
    backend.set_termination_proof(&tree, None).unwrap();
    assert_eq!(
        backend
            .wait_and_prove(&tree, Duration::from_millis(10))
            .await,
        Err(SupervisorError::Unverifiable)
    );
    let subscription = backend.subscribe_output(&tree).await.unwrap();
    let page = backend
        .drain(&subscription, 0, 1, Duration::ZERO)
        .await
        .unwrap();
    assert!(page.terminal);
    assert_eq!(page.exit_code, Some(9));
}

#[test]
fn contract_wiring_lives_on_the_interactive_path_and_not_in_the_host_shell() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let supervisor = std::fs::read_to_string(root.join("services/process_supervisor.rs")).unwrap();
    for forbidden in [
        "tokio::process::Command",
        "std::process::Command",
        "CreateProcess",
        "Command::new",
        "host.process.supervisor",
    ] {
        assert!(
            !supervisor.contains(forbidden),
            "P04 must remain contract/fake only: {forbidden}"
        );
    }
    let module = std::fs::read_to_string(root.join("services/mod.rs")).unwrap();
    assert!(module.contains("pub mod process_supervisor;"));

    // P22 retired the premature-wiring guard for exactly two files, because
    // that is where it moved the interactive Process service: the service
    // resolves its material into a SpawnSpec and runs on the supervisor, and
    // the router answers on the host-pinned effect. Anything weaker here would
    // let a later wave re-own the launch outside the supervisor.
    let processes = std::fs::read_to_string(root.join("services/processes.rs")).unwrap();
    assert!(
        processes.contains("use crate::services::process_supervisor::{"),
        "the interactive service must resolve through the supervisor module"
    );
    assert!(
        processes.contains("ProcessSupervisor::new(")
            && processes.contains(".start_with_write_profile("),
        "the only launch route is supervisor start over the injected backend"
    );
    let router = std::fs::read_to_string(root.join("plugins/router.rs")).unwrap();
    assert!(
        router.contains("use crate::services::process_profiles::ProcessProfileEffect;")
            && router.contains("interactive_process_admitted()"),
        "the router must gate discovery on the one shared effect predicate"
    );

    // Production composition of the interactive service is outside P22, so the
    // host shell stays unwired: no supervisor types and no service handle.
    for path in [
        root.join("run_manager.rs"),
        root.join("bin/r-code-service.rs"),
    ] {
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(
            !source.contains("process_supervisor"),
            "supervisor wiring must stay out of the host shell: {}",
            path.display()
        );
        assert!(
            !source.contains("ManagedProcessService"),
            "the interactive process service must stay unwired: {}",
            path.display()
        );
    }

    let trait_source = supervisor
        .split("pub trait ProcessTreeBackend")
        .nth(1)
        .unwrap()
        .split("#[derive(Clone)]")
        .next()
        .unwrap();
    for boundary in [
        "prepare",
        "spawn_suspended",
        "persist_identity",
        "resume_once",
        "terminate",
        "wait_and_prove",
        "subscribe_output",
        "drain",
    ] {
        assert!(
            trait_source.contains(&format!("fn {boundary}")),
            "missing backend boundary {boundary}"
        );
    }
}

/// P22 acceptance ①: the interactive Process path has no launch route of its
/// own. Every file it may pass through is scanned, and the single supervisor
/// boundary is counted so a second, unsupervised route cannot be added.
#[test]
fn interactive_path_never_spawns_a_child_directly() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let interactive_path = [
        "services/processes.rs",
        "services/process_profiles.rs",
        "plugins/router.rs",
        "run_manager.rs",
        "bin/r-code-service.rs",
    ];
    for relative in interactive_path {
        let source = std::fs::read_to_string(root.join(relative)).unwrap();
        for forbidden in [
            "Command::new(",
            ".spawn()",
            "tokio::process",
            "std::process::Command",
            "CreateProcess",
        ] {
            assert!(
                !source.contains(forbidden),
                "acceptance ①: {relative} launches a child directly with {forbidden:?}"
            );
        }
    }

    let processes = std::fs::read_to_string(root.join("services/processes.rs")).unwrap();
    assert_eq!(
        processes.matches(".start_with_write_profile(").count(),
        1,
        "exactly one launch boundary may reach a supervised tree"
    );
    assert_eq!(
        processes.matches(".resume_once(").count(),
        0,
        "the service may not resume a tree outside the supervisor"
    );
}
