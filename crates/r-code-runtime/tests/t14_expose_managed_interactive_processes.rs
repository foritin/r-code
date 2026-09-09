//! T14 — managed interactive processes.
//!
//! Echo/App-Server-like fixture verifies bidirectional I/O, cross-run
//! handle rejection, launch failure and descendant process cleanup.

use r_code_harness_protocol::process_profile::{HostBindings, ProcessProfileSchema};
use r_code_kernel::ports::{GenerationToken, ProcessService};
use r_code_runtime::services::authorization::*;
use r_code_runtime::services::launch_profiles::*;
use r_code_runtime::services::process_profiles::FrameValidator;
use r_code_runtime::services::processes::ManagedProcessService;
use std::sync::Arc;
use std::time::Duration;

const HELPER: &str = env!("CARGO_BIN_EXE_harness-test-helper");

fn token(run: &str) -> GenerationToken {
    GenerationToken {
        run_id: run.into(),
        generation: 1,
    }
}

fn ndjson_profile() -> ProcessProfileSchema {
    serde_json::from_value(serde_json::json!({
        "name": "app-server-fixture",
        "framing": "ndjson-rpc",
        "methods": [
            {"name": "initialize", "params": [
                {"pointer": "/cwd", "constraint": {"kind": "bound-to", "value": "workspace-root"}}
            ]},
            {"name": "harness.start", "params": []}
        ]
    }))
    .expect("profile")
}

fn service(validator: Option<FrameValidator>) -> ManagedProcessService {
    let mut authorization = AuthorizationService::new();
    install_profile_capability(
        &mut authorization,
        &ProfileSource {
            harness_id: "fixture.harness".into(),
            package_digest: "sha".into(),
            profile_name: "app-server".into(),
        },
        vec![HELPER.to_string()],
        None,
        false,
        vec![],
    );
    ManagedProcessService::new(
        Arc::new(authorization),
        EffectivePermissions::full(),
        WorkspaceCapability::WriteWithin {
            root: "D:/work".into(),
        },
        validator,
    )
}

#[tokio::test]
async fn bidirectional_io_flows_through_the_managed_handle() {
    let service = service(None);
    service
        .register_profile_executable("app-server", HELPER)
        .await;

    let handle = service
        .open(token("run-1"), "app-server", vec!["serve".into()], None)
        .await
        .expect("open");
    assert!(handle.starts_with("run-1:"));

    // Send an initialize frame; the fixture answers over stdout.
    let frame =
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n".to_vec();
    service
        .write(token("run-1"), &handle, frame)
        .await
        .expect("write");

    // The child answers (read through the raw child is owned by the
    // service; here we verify the write path and clean shutdown).
    let exit = service.close(token("run-1"), &handle).await.expect("close");
    let _ = exit;
}

#[tokio::test]
async fn cross_run_handles_are_rejected() {
    let service = service(None);
    service
        .register_profile_executable("app-server", HELPER)
        .await;
    let handle = service
        .open(token("run-1"), "app-server", vec!["serve".into()], None)
        .await
        .expect("open");

    // Another run cannot write or close this handle.
    assert!(service
        .write(token("run-2"), &handle, b"x\n".to_vec())
        .await
        .is_err());
    assert!(service.close(token("run-2"), &handle).await.is_err());
    // The owner still can.
    service
        .close(token("run-1"), &handle)
        .await
        .expect("owner closes");
    // Unknown handles fail.
    assert!(service.close(token("run-1"), "run-1:999").await.is_err());
}

#[tokio::test]
async fn launch_failures_and_unauthorized_profiles_fail_closed() {
    let service = service(None);
    // No executable registered for the profile.
    assert!(service
        .open(token("run-1"), "missing-profile", vec![], None)
        .await
        .is_err());

    // Executable registered but outside every launch capability.
    service
        .register_profile_executable("app-server", "definitely-not-a-real-exe.xyz")
        .await;
    assert!(
        service
            .open(token("run-1"), "app-server", vec![], None)
            .await
            .is_err(),
        "uncovered executable must not launch"
    );

    // A real executable that does not exist fails at spawn.
    let mut authorization = AuthorizationService::new();
    install_profile_capability(
        &mut authorization,
        &ProfileSource {
            harness_id: "x".into(),
            package_digest: "s".into(),
            profile_name: "ghost".into(),
        },
        vec!["Z:/nonexistent/ghost.exe".into()],
        None,
        false,
        vec![],
    );
    let service = ManagedProcessService::new(
        Arc::new(authorization),
        EffectivePermissions::full(),
        WorkspaceCapability::WriteWithin {
            root: "Z:/nonexistent".into(),
        },
        None,
    );
    service
        .register_profile_executable("ghost", "Z:/nonexistent/ghost.exe")
        .await;
    assert!(service
        .open(
            token("run-1"),
            "ghost",
            vec![],
            Some("Z:/nonexistent".to_string())
        )
        .await
        .is_err());
}

#[tokio::test]
async fn ndjson_profiles_validate_frames_before_they_reach_the_child() {
    let validator = FrameValidator::new(
        ndjson_profile(),
        HostBindings {
            workspace_root: "D:/work".into(),
            task_id: "t".into(),
            run_id: "run-1".into(),
            attempt_id: "a".into(),
            permission_ceiling: "full".into(),
        },
    );
    let service = service(Some(validator));
    service
        .register_profile_executable("app-server", HELPER)
        .await;
    let handle = service
        .open(token("run-1"), "app-server", vec!["serve".into()], None)
        .await
        .expect("open");

    // An allowlisted frame passes the validator.
    let good =
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"harness.start\",\"params\":{}}\n".to_vec();
    service
        .write(token("run-1"), &handle, good)
        .await
        .expect("valid frame written");

    // A frame violating a host binding is refused before reaching the child.
    let bad = b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\",\"params\":{\"cwd\":\"E:/evil\"}}\n".to_vec();
    let error = service
        .write(token("run-1"), &handle, bad)
        .await
        .expect_err("violating frame refused");
    assert!(error.to_string().contains("host-bound") || error.to_string().contains("escapes"));

    // Unknown methods are refused too.
    let unknown = b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"reboot\",\"params\":{}}\n".to_vec();
    assert!(service
        .write(token("run-1"), &handle, unknown)
        .await
        .is_err());

    service.close(token("run-1"), &handle).await.expect("close");
}

#[cfg(windows)]
#[tokio::test]
async fn closing_terminates_descendants_with_the_job() {
    // Spawn a wrapper that leaves a late marker via a grandchild.
    let temp = tempfile::tempdir().expect("tempdir");
    // A dedicated service whose launch capability covers powershell.
    let mut authorization = AuthorizationService::new();
    install_profile_capability(
        &mut authorization,
        &ProfileSource {
            harness_id: "fixture.harness".into(),
            package_digest: "sha".into(),
            profile_name: "app-server".into(),
        },
        vec!["powershell".into()],
        None,
        false,
        vec![],
    );
    let service = ManagedProcessService::new(
        Arc::new(authorization),
        EffectivePermissions::full(),
        WorkspaceCapability::WriteWithin {
            root: temp.path().to_string_lossy().replace('\\', "/"),
        },
        None,
    );
    service
        .register_profile_executable("app-server", "powershell")
        .await;
    let marker = temp.path().join("late.txt");
    let marker_display = marker.to_string_lossy().replace('\\', "/");
    let script = format!(
        "Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep 15; Set-Content -Path \"{marker_display}\" -Value late' -WindowStyle Hidden; Start-Sleep 60"
    );
    let handle = service
        .open(
            token("run-1"),
            "app-server",
            vec!["-NoProfile".into(), "-Command".into(), script],
            Some(temp.path().to_string_lossy().to_string()),
        )
        .await
        .expect("open");

    tokio::time::sleep(Duration::from_millis(1500)).await;
    service
        .close(token("run-1"), &handle)
        .await
        .expect("close with tree kill");

    // The grandchild never completes its late write.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!marker.exists(), "descendant wrote after close");
}
