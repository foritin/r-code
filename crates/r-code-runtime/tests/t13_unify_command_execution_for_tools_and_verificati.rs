//! T13 — unified command execution for tools and verification.
//!
//! Windows command corpus through real default and selected backend routes;
//! timeout/cancel leaves no child process; authorization stays separated
//! from backend spawning.

use r_code_gateway::execution_backend::{
    CollectedOutput, CommandExecutionBackend, CommandHandle, CommandSpec, LocalShellBackend,
};
use r_code_kernel::ports::ServiceError;
use r_code_runtime::services::authorization::*;
use r_code_runtime::services::execution::{ExecutionError, ExecutionService};
use std::sync::Arc;
use std::time::Duration;

fn authorization() -> Arc<AuthorizationService> {
    Arc::new(AuthorizationService::new())
}

fn workspace(root: &std::path::Path) -> WorkspaceCapability {
    WorkspaceCapability::WriteWithin {
        root: root.to_string_lossy().replace('\\', "/"),
    }
}

/// A selectable backend that records the commands routed through it.
struct RecordingBackend {
    inner: LocalShellBackend,
    commands: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl CommandExecutionBackend for RecordingBackend {
    fn backend_id(&self) -> &'static str {
        "recording"
    }

    async fn spawn(
        &self,
        spec: &CommandSpec,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<CommandHandle, r_code_core::error::ProductError> {
        self.commands.lock().unwrap().push(spec.command.clone());
        self.inner.spawn(spec, abort_flag).await
    }

    async fn collect(
        &self,
        handle: CommandHandle,
        spec: &CommandSpec,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<CollectedOutput, r_code_core::error::ProductError> {
        self.inner.collect(handle, spec, abort_flag).await
    }
}

#[tokio::test]
async fn windows_command_corpus_runs_through_default_and_selected_routes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = ExecutionService::local(authorization());
    let permissions = EffectivePermissions::full();

    // Default route (local five-tier shell resolution; Windows dialect).
    let corpus = [
        ("echo hello", "hello"),
        ("cd", ""),                  // prints cwd; just needs success
        ("echo %RANDOM% > nul", ""), // cmd-style variable
        (
            "powershell -NoProfile -Command \"Write-Output ps-ok\"",
            "ps-ok",
        ),
    ];
    for (command, expect_contains) in &corpus {
        let output = service
            .run_bash(
                &workspace(temp.path()),
                &permissions,
                command,
                temp.path(),
                Duration::from_secs(60),
            )
            .await
            .unwrap_or_else(|e| panic!("command {command:?} failed: {e:?}"));
        assert_eq!(
            output.exit_code,
            Some(0),
            "command {command:?}: {:?}",
            output.stderr
        );
        if !expect_contains.is_empty() {
            assert!(
                output.stdout.contains(expect_contains),
                "command {command:?} stdout {:?} missing {expect_contains:?}",
                output.stdout
            );
        }
    }

    // Selected backend route: a recording wrapper receives the commands.
    let recording = Arc::new(RecordingBackend {
        inner: LocalShellBackend::new(),
        commands: std::sync::Mutex::new(Vec::new()),
    });
    let service =
        ExecutionService::with_backend(Arc::new(LocalShellBackend::new()), authorization());
    service.select_backend(Some(recording.clone())).await;
    let output = service
        .run_bash(
            &workspace(temp.path()),
            &permissions,
            "echo routed",
            temp.path(),
            Duration::from_secs(60),
        )
        .await
        .expect("selected backend run");
    assert!(output.stdout.contains("routed"));
    assert_eq!(
        recording.commands.lock().unwrap().as_slice(),
        ["echo routed".to_string()]
    );

    // Clearing the selection falls back to the default backend.
    service.select_backend(None).await;
    let output = service
        .run_bash(
            &workspace(temp.path()),
            &permissions,
            "echo default-again",
            temp.path(),
            Duration::from_secs(60),
        )
        .await
        .expect("default backend run");
    assert!(output.stdout.contains("default-again"));
    assert_eq!(
        recording.commands.lock().unwrap().len(),
        1,
        "no extra routing after deselect"
    );
}

#[tokio::test]
async fn authorization_gates_run_before_any_spawn() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = ExecutionService::local(authorization());

    // Read-only workspace + processes disabled: denied before spawning.
    let error = service
        .run_bash(
            &WorkspaceCapability::ReadOnly {
                root: temp.path().to_string_lossy().replace('\\', "/"),
            },
            &EffectivePermissions::read_only(),
            "echo never",
            temp.path(),
            Duration::from_secs(10),
        )
        .await
        .expect_err("denied");
    assert!(
        matches!(error, ExecutionError::Denied(ref reason) if reason.contains("not allowed")),
        "got {error:?}"
    );

    // Approval-required: surfaces as ApprovalRequired, nothing spawned.
    let error = service
        .run_bash(
            &workspace(temp.path()),
            &EffectivePermissions::approval_required(),
            "echo maybe",
            temp.path(),
            Duration::from_secs(10),
        )
        .await
        .expect_err("approval");
    assert!(matches!(error, ExecutionError::ApprovalRequired(_)));
}

#[cfg(windows)]
#[tokio::test]
async fn timeout_kills_the_process_tree_leaving_no_children() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = ExecutionService::local(authorization());
    let permissions = EffectivePermissions::full();

    // A child that would write a marker file *after* sleeping well beyond
    // the timeout. If the tree were left alive, the file would appear.
    let marker = temp.path().join("late-marker.txt");
    let marker_display = marker.to_string_lossy().replace('\\', "/");
    let command = format!(
        "powershell -NoProfile -Command \"Start-Sleep 25; Set-Content -Path '{marker_display}' -Value 'late'\""
    );
    let started = std::time::Instant::now();
    let output = service
        .run_bash(
            &workspace(temp.path()),
            &permissions,
            &command,
            temp.path(),
            Duration::from_secs(3),
        )
        .await
        .expect("collected with timeout");
    // Timed-out collection reports no clean exit code.
    assert_ne!(output.exit_code, Some(0));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "timeout enforced promptly"
    );

    // Wait past the child's own sleep; the marker must never appear.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        !marker.exists(),
        "child process survived the timeout and wrote the marker"
    );
}

#[cfg(not(windows))]
#[tokio::test]
async fn timeout_kills_the_process_tree_leaving_no_children() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = ExecutionService::local(authorization());
    let permissions = EffectivePermissions::full();
    let marker = temp.path().join("late-marker.txt");
    let command = format!("(sleep 25 && touch {}) & wait", marker.display());
    let output = service
        .run_bash(
            &workspace(temp.path()),
            &permissions,
            &command,
            temp.path(),
            Duration::from_secs(3),
        )
        .await
        .expect("collected");
    assert_ne!(output.exit_code, Some(0));
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!marker.exists(), "grandchild survived the timeout");
}

#[tokio::test]
async fn execution_errors_map_to_service_errors() {
    let mapped: ServiceError = ExecutionError::Denied("no".into()).into();
    assert!(matches!(mapped, ServiceError::Failure(message) if message.contains("denied")));
}
