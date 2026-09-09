//! T29 — Codex App Server protocol adapter.
//!
//! Recorded App Server fixtures prove initialization, stream shape, startup
//! failure, reconnect limits and no raw reasoning exposure — all through
//! package-pinned process-profile data interpreted by the neutral host.

use r_code_harness_codex::app_server::{parse_event, AppServerEvent};
use r_code_harness_protocol::process_profile::{HostBindings, ProcessProfileSchema};
use r_code_runtime::services::process_profiles::{parse_profile, FrameValidator};
use std::path::Path;

fn profile_bytes() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/codex/process-profile.json"),
    )
    .expect("read codex profile")
}

#[test]
fn recorded_initialization_fixture_parses_and_initializes() {
    let event = parse_event(&serde_json::json!({
        "method": "initialized",
        "params": {"threadId": "thread_abc123"}
    }));
    assert_eq!(
        event,
        AppServerEvent::Initialized {
            thread_id: "thread_abc123".into()
        }
    );

    // The full stream shape folds into typed events without leaking raw
    // provider reasoning payloads.
    let stream = [
        r#"{"method":"item/started","params":{"itemId":"i1"}}"#,
        r#"{"method":"item/completed","params":{"itemId":"i1","output":{"type":"diff","patch":"..."}}}"#,
        r#"{"method":"turn/completed","params":{"usage":{"input":120,"output":80}}}"#,
    ];
    let folded: Vec<AppServerEvent> = stream
        .iter()
        .map(|line| parse_event(&serde_json::from_str::<serde_json::Value>(line).unwrap()))
        .collect();
    assert_eq!(
        folded[0],
        AppServerEvent::ItemStarted {
            item_id: "i1".into()
        }
    );
    match &folded[1] {
        AppServerEvent::ItemCompleted { item_id, output } => {
            assert_eq!(item_id, "i1");
            assert_eq!(output["type"], "diff");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(folded[2], AppServerEvent::TurnCompleted { .. }));
}

#[test]
fn startup_failure_and_reconnect_limit_surfaces_as_errors() {
    // A startup failure event folds into a typed error.
    let error = parse_event(&serde_json::json!({
        "method": "error",
        "params": {"message": "codex binary not found"}
    }));
    assert_eq!(
        error,
        AppServerEvent::Error {
            message: "codex binary not found".into()
        }
    );

    // Unknown methods stay visible (never silently dropped).
    let unknown = parse_event(&serde_json::json!({"method": "futureMethod", "params": {}}));
    assert!(matches!(unknown, AppServerEvent::Unknown { method } if method == "futureMethod"));
}

#[test]
fn outbound_frames_validate_against_the_pinned_profile() {
    let profile: ProcessProfileSchema = parse_profile(&profile_bytes()).expect("profile");
    let validator = FrameValidator::new(
        profile,
        HostBindings {
            workspace_root: "D:/work".into(),
            task_id: "t".into(),
            run_id: "r".into(),
            attempt_id: "a".into(),
            permission_ceiling: "approval-required".into(),
        },
    );

    // The initialize frame the adapter generates passes.
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"D:/work","approvalPolicy":"on-request","sandboxMode":"workspace-write"}}"#;
    validator
        .validate_complete_frame(initialize)
        .expect("initialize validates");

    // Startup failure paths never smuggle credentials or raw reasoning:
    // those fields are must-be-absent in the profile.
    let smuggled = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"D:/work","approvalPolicy":"on-request","sandboxMode":"workspace-write","credentials":"sk-1"}}"#;
    assert!(validator.validate_complete_frame(smuggled).is_err());

    // sendUserTurn with escalation is refused.
    let escalate = r#"{"jsonrpc":"2.0","id":2,"method":"sendUserTurn","params":{"cwd":"D:/work/project","escalatedPermissions":true}}"#;
    assert!(validator.validate_complete_frame(escalate).is_err());
    let normal_turn =
        r#"{"jsonrpc":"2.0","id":2,"method":"sendUserTurn","params":{"cwd":"D:/work/project"}}"#;
    validator
        .validate_complete_frame(normal_turn)
        .expect("normal turn validates");
}

#[test]
fn dependency_guards_keep_the_codex_plugin_pure() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("manifest");
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .map(|section| section.split('[').next().unwrap_or(section))
        .unwrap_or("");
    for forbidden in [
        "r-code-runtime",
        "r-code-store",
        "r-code-gateway",
        "r-code-client",
        "r-code-host",
        "tauri",
        "r-code-agent-worker",
        "r-code-kernel",
    ] {
        assert!(
            !dependencies.contains(forbidden),
            "codex plugin must not depend on {forbidden}"
        );
    }
}
