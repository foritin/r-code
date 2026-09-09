//! T14b — Unix guardian and process-group containment.
//!
//! Linux/macOS real-process tests: kill the daemon, leave a grandchild and
//! force guardian failure; uncertain survivors block a new writer. This
//! suite runs only on Unix hosts (CI covers it; a Windows host skips).

#![cfg(unix)]

use r_code_runtime::process_guard::unix::{confirm_termination, spawn_guarded};
use std::time::Duration;

#[tokio::test]
async fn daemon_death_kills_the_managed_group_via_guardian_eof() {
    // The guarded wrapper leaves a grandchild (sleep) in its group; killing
    // our daemon-side write handle simulates daemon death, which must bring
    // the whole group down through the guardian's EOF → TERM → KILL path.
    let mut guarded = spawn_guarded(
        "/bin/sh".as_ref(),
        &["-c".into(), "sleep 300 & sleep 300".into()],
    )
    .await
    .expect("guarded spawn");
    let pgid = guarded.pgid;

    // Simulate daemon death: drop the write end without graceful cancel.
    // (Dropping the GuardedChild also reaps the direct child.)
    drop(guarded);

    assert!(
        confirm_termination(pgid, Duration::from_secs(15)).await,
        "guardian must terminate the group after daemon EOF"
    );
}

#[tokio::test]
async fn cancellation_terminates_the_group_gracefully() {
    let mut guarded = spawn_guarded("/bin/sh".as_ref(), &["-c".into(), "sleep 300".into()])
        .await
        .expect("guarded spawn");
    guarded.cancel_tree().await;
    assert!(confirm_termination(guarded.pgid, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn unverifiable_termination_blocks_rather_than_guesses() {
    // A group that cannot be proven dead within the deadline reports
    // unverifiable: writers must treat that as blocked, not as success.
    let mut guarded = spawn_guarded(
        "/bin/sh".as_ref(),
        &["-c".into(), "trap '' TERM; sleep 300".into()],
    )
    .await
    .expect("guarded spawn");
    // TERM is trapped: the short window cannot prove termination.
    let proven = confirm_termination(guarded.pgid, Duration::from_millis(300)).await;
    assert!(!proven, "unprovable termination must not claim success");
    // Cleanup: force kill.
    unsafe {
        libc::kill(-guarded.pgid, libc::SIGKILL);
    }
    let _ = guarded.child.wait().await;
    drop(guarded);
}
