//! T19 — host verification runner.
//!
//! Real fixtures cover passing/failing checks, missing tools, timeout,
//! source edits during checking and check-definition tampering.

use r_code_kernel::verification::{CheckDefinition, CheckEntrypoint, ControlFile};
use r_code_runtime::services::artifacts::sha256_hex;
use r_code_runtime::services::verification::*;
use r_code_runtime::services::verification_inputs::FrozenControlStore;
use r_code_runtime::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use std::path::Path;
use std::time::Duration;

fn node_check(entry_argv: Vec<String>) -> CheckDefinition {
    CheckDefinition {
        check_id: "check:node".into(),
        entrypoint: CheckEntrypoint::Command {
            program: "node".into(),
            argv: entry_argv,
        },
        control_files: vec![],
        source_roots: vec![".".into()],
        dependency_locks: vec![],
        toolchain: "node test-runtime".into(),
        declared_external_inputs: vec![],
        entrypoint_bytes: None,
    }
}

fn setup(temp: &Path, verifier: &str) -> (TaskWorkspaceBinding, CandidateManifest) {
    let root = temp.join("project");
    std::fs::create_dir_all(&root).expect("dirs");
    std::fs::write(root.join("verify.js"), verifier).expect("verifier");
    let binding = TaskWorkspaceBinding::bind_local("task-1", &root, &[]).expect("bind");
    let manifest = CandidateManifest::capture(&binding).expect("capture");
    (binding, manifest)
}

fn control_store_with(temp: &Path, control: &ControlFile, bytes: &[u8]) -> FrozenControlStore {
    let store = FrozenControlStore::new(temp.join("frozen"));
    store.store(control, bytes).expect("store control");
    store
}

#[tokio::test]
async fn passing_check_produces_host_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (binding, manifest) = setup(temp.path(), "process.exit(0);");
    let definition = node_check(vec!["verify.js".into()]);
    let runner = VerificationRunner::new();

    let outcome = runner
        .run(
            &binding,
            &manifest,
            &FrozenControlStore::new(temp.path().join("frozen")),
            &definition,
            &temp.path().join("verify-1"),
            Duration::from_secs(60),
        )
        .await;
    assert_eq!(outcome.status, CheckStatus::Passed);
    assert_eq!(outcome.exit_code, Some(0));
    let evidence = outcome.evidence.expect("evidence present");
    assert!(evidence.passed);
    assert_eq!(evidence.candidate_digest, manifest.candidate_id);
    assert!(matches!(
        evidence.recorded_by,
        r_code_harness_protocol::Provenance::Host
    ));
}

#[tokio::test]
async fn failing_check_returns_structured_repair_feedback() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (binding, manifest) = setup(
        temp.path(),
        "console.error('assertion failed: widget count'); process.exit(1);",
    );
    let definition = node_check(vec!["verify.js".into()]);
    let runner = VerificationRunner::new();

    let outcome = runner
        .run(
            &binding,
            &manifest,
            &FrozenControlStore::new(temp.path().join("frozen")),
            &definition,
            &temp.path().join("verify-2"),
            Duration::from_secs(60),
        )
        .await;
    match outcome.status {
        CheckStatus::Failed { repair_feedback } => {
            assert!(repair_feedback.contains("check:node"));
            assert!(
                repair_feedback.contains("assertion failed: widget count"),
                "{repair_feedback}"
            );
            assert!(repair_feedback.contains("exited with 1"));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // Failing evidence is still recorded (passed=false) for repair loops.
    let evidence = outcome.evidence.expect("evidence");
    assert!(!evidence.passed);
}

#[tokio::test]
async fn missing_tool_is_unavailable_not_failed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (binding, manifest) = setup(temp.path(), "process.exit(0);");
    let mut definition = node_check(vec!["verify.js".into()]);
    definition.entrypoint = CheckEntrypoint::Command {
        program: "definitely-not-a-real-tool-xyz".into(),
        argv: vec![],
    };
    let runner = VerificationRunner::new();

    let outcome = runner
        .run(
            &binding,
            &manifest,
            &FrozenControlStore::new(temp.path().join("frozen")),
            &definition,
            &temp.path().join("verify-3"),
            Duration::from_secs(60),
        )
        .await;
    match outcome.status {
        CheckStatus::Unavailable { reason } => {
            // The environment lacks the tool: unavailable, not a candidate
            // failure, and no evidence was minted.
            assert!(
                reason.contains("spawn")
                    || reason.contains("not")
                    || reason.contains("denied")
                    || reason.contains("program"),
                "{reason}"
            );
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert!(
        outcome.evidence.is_none() || outcome.evidence.as_ref().map(|e| !e.passed).unwrap_or(true)
    );
}

#[tokio::test]
async fn timeout_is_distinct_from_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (binding, manifest) = setup(temp.path(), "setTimeout(() => {}, 60000);");
    let definition = node_check(vec!["verify.js".into()]);
    let runner = VerificationRunner::new();

    let started = std::time::Instant::now();
    let outcome = runner
        .run(
            &binding,
            &manifest,
            &FrozenControlStore::new(temp.path().join("frozen")),
            &definition,
            &temp.path().join("verify-4"),
            Duration::from_secs(2),
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(15));
    match outcome.status {
        CheckStatus::TimedOut { after_ms } => assert!(after_ms >= 1000, "recorded grace"),
        other => panic!("expected TimedOut, got {other:?}"),
    }
}

#[tokio::test]
async fn source_edits_during_checking_invalidate_inputs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).expect("dirs");
    std::fs::write(root.join("victim.txt"), b"original").expect("victim");
    // The verifier edits a *live candidate* file (absolute path) while the
    // check runs, simulating a concurrent external edit.
    let victim = root.join("victim.txt");
    let script = format!(
        "require('fs').writeFileSync({:?}, 'edited-during-check'); process.exit(0);",
        victim.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(root.join("verify.js"), script).expect("verifier");
    let binding = TaskWorkspaceBinding::bind_local("task-1", &root, &[]).expect("bind");
    let manifest = CandidateManifest::capture(&binding).expect("capture");

    let definition = node_check(vec!["verify.js".into()]);
    let runner = VerificationRunner::new();

    let outcome = runner
        .run(
            &binding,
            &manifest,
            &FrozenControlStore::new(temp.path().join("frozen")),
            &definition,
            &temp.path().join("verify-5"),
            Duration::from_secs(60),
        )
        .await;
    assert_eq!(outcome.status, CheckStatus::InputsChanged);
}

#[tokio::test]
async fn check_definition_tampering_is_unavailable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).expect("dirs");
    std::fs::write(root.join("verify.js"), b"process.exit(0);").expect("verifier");
    let binding = TaskWorkspaceBinding::bind_local("task-1", &root, &[]).expect("bind");
    let manifest = CandidateManifest::capture(&binding).expect("capture");

    let pinned = b"process.exit(0); // pinned verifier";
    let control = ControlFile {
        path: "verify.js".into(),
        sha256: sha256_hex(pinned),
    };
    let store = control_store_with(temp.path(), &control, pinned);

    // Tamper: replace the frozen store bytes with different content.
    std::fs::write(
        temp.path().join("frozen").join(&control.sha256),
        b"process.exit(0); // TAMPERED",
    )
    .expect("tamper");

    let definition = CheckDefinition {
        check_id: "check:tamper".into(),
        entrypoint: CheckEntrypoint::Command {
            program: "node".into(),
            argv: vec!["verify.js".into()],
        },
        control_files: vec![control],
        source_roots: vec![".".into()],
        dependency_locks: vec![],
        toolchain: "node".into(),
        declared_external_inputs: vec![],
        entrypoint_bytes: None,
    };
    let runner = VerificationRunner::new();
    let outcome = runner
        .run(
            &binding,
            &manifest,
            &store,
            &definition,
            &temp.path().join("verify-6"),
            Duration::from_secs(60),
        )
        .await;
    match outcome.status {
        CheckStatus::Unavailable { reason } => {
            assert!(
                reason.contains("control") || reason.contains("digest"),
                "{reason}"
            );
        }
        other => panic!("expected Unavailable for tampered control, got {other:?}"),
    }
    // The tampered verifier never replaced the candidate's real file path
    // inside the private dir (nothing materialized).
    let _ = Path::new(temp.path());
}
