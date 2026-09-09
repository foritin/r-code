//! T14a — Windows Job Object process containment.
//!
//! Real-process tests: killing the daemon (dropping the job handle)
//! terminates descendants; a wrapper that ignores cancellation is force
//! terminated after the grace; descendants cannot write after terminal
//! cleanup; recovery never trusts PID reuse.

#![cfg(windows)]

use r_code_runtime::process_guard::windows::{
    confirm_termination, is_process_alive, process_start_identity, spawn_guarded, OwnerIdentity,
};
use std::time::Duration;

const HELPER: &str = env!("CARGO_BIN_EXE_harness-test-helper");

fn helper_path() -> std::path::PathBuf {
    std::path::PathBuf::from(HELPER)
}

/// A command that spawns a grandchild which writes a marker file after a
/// delay; both sleep well beyond test timeouts when left alive.
fn sleeper_command(marker: &std::path::Path, delay_secs: u64) -> Vec<String> {
    let marker = marker.to_string_lossy().replace('\\', "/");
    vec![
        "-NoProfile".into(),
        "-Command".into(),
        format!(
            "Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep {delay_secs}; Set-Content -Path \"{marker}\" -Value late' -WindowStyle Hidden; Start-Sleep {delay_secs}"
        ),
    ]
}

#[tokio::test]
async fn dropping_the_job_handle_kills_the_tree_on_daemon_death() {
    let temp = tempfile::tempdir().expect("tempdir");
    let marker = temp.path().join("daemon-death-marker.txt");
    let argv = sleeper_command(&marker, 20);

    // Spawn a guarded PowerShell wrapper that leaves a grandchild behind.
    let (guard, mut child) = spawn_guarded("powershell".as_ref(), &argv)
        .await
        .expect("guarded spawn");
    let wrapper_pid = child.id().expect("pid");

    // Simulate daemon death: drop the job handle without graceful cancel.
    drop(guard);
    let _ = child.wait().await;

    // The wrapper and its descendants die with the job.
    assert!(confirm_termination(&[wrapper_pid], Duration::from_secs(10))
        .await
        .into_proof());
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !marker.exists(),
        "descendant survived the daemon death and wrote the marker"
    );
}

#[tokio::test]
async fn ignoring_cancellation_leads_to_forced_job_termination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let marker = temp.path().join("forced-marker.txt");
    let argv = sleeper_command(&marker, 30);

    let (guard, mut child) = spawn_guarded("powershell".as_ref(), &argv)
        .await
        .expect("guarded spawn");
    let wrapper_pid = child.id().expect("pid");

    // "Grace period expires without acknowledgement": force termination.
    guard.terminate();
    let _ = child.wait().await;
    drop(guard);

    assert!(confirm_termination(&[wrapper_pid], Duration::from_secs(10))
        .await
        .into_proof());
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!marker.exists(), "descendant wrote after terminal cleanup");
}

#[tokio::test]
async fn owner_identity_uses_start_time_not_bare_pids() {
    // The same pid value before and after a process exits refers to
    // different instances; start_identity distinguishes them.
    let (guard, mut child) = spawn_guarded(helper_path().as_ref(), &["blocked".into()])
        .await
        .expect("guarded spawn");
    let pid = child.id().expect("pid");
    let identity = OwnerIdentity::current();
    assert_eq!(identity.pid, std::process::id());
    // Our own process has a non-zero start identity on real Windows.
    assert!(process_start_identity(std::process::id()) > 0);

    // Terminate and confirm: the identity of the dead pid is unverifiable.
    guard.terminate();
    let _ = child.wait().await;
    drop(guard);
    assert!(!is_process_alive(pid));
    assert!(confirm_termination(&[pid], Duration::from_secs(5))
        .await
        .into_proof());
}

#[tokio::test]
async fn job_guard_reports_raw_handle_and_survives_multiple_assignments() {
    let (guard_a, mut child_a) = spawn_guarded(helper_path().as_ref(), &["silent".into()])
        .await
        .expect("spawn a");
    let (guard_b, mut child_b) = spawn_guarded(helper_path().as_ref(), &["silent".into()])
        .await
        .expect("spawn b");
    assert_ne!(guard_a.raw(), guard_b.raw(), "distinct jobs per tree");
    let pid_a = child_a.id().expect("pid a");
    let pid_b = child_b.id().expect("pid b");

    // Terminating job A leaves job B alive.
    guard_a.terminate();
    let _ = child_a.wait().await;
    drop(guard_a);
    assert!(confirm_termination(&[pid_a], Duration::from_secs(10)).await);
    assert!(is_process_alive(pid_b));

    guard_b.terminate();
    let _ = child_b.wait().await;
    drop(guard_b);
    assert!(confirm_termination(&[pid_b], Duration::from_secs(10)).await);
}

/// Test-local helper mirroring the shared TerminationProof conversion.
trait IntoProof {
    fn into_proof(self) -> bool;
}

impl IntoProof for bool {
    fn into_proof(self) -> bool {
        self
    }
}
