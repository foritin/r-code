//! T12b — declarative process protocol constraints.
//!
//! Fixture profiles prove frame splitting cannot bypass scope/permission
//! constraints, unknown methods are denied, and the kernel contains no
//! Codex method-name switch.

use r_code_harness_protocol::process_profile::*;
use r_code_runtime::services::process_profiles::{parse_profile, FrameValidator};
use std::path::Path;

fn fixture_profile() -> ProcessProfileSchema {
    serde_json::from_value(serde_json::json!({
        "name": "fixture-app-server",
        "framing": "ndjson-rpc",
        "methods": [
            {
                "name": "initialize",
                "params": [
                    {"pointer": "/cwd", "constraint": {"kind": "bound-to", "value": "workspace-root"}},
                    {"pointer": "/approvalPolicy", "constraint": {"kind": "enum", "values": ["untrusted", "on-request"]}},
                    {"pointer": "/apiKey", "constraint": {"kind": "must-be-absent"}}
                ]
            },
            {
                "name": "sendTurn",
                "params": [
                    {"pointer": "/cwd", "constraint": {"kind": "within-workspace"}},
                    {"pointer": "/escalate", "constraint": {"kind": "must-be-absent"}}
                ]
            }
        ],
        "env": ["APP_HOME"]
    }))
    .expect("fixture profile")
}

fn bindings() -> HostBindings {
    HostBindings {
        workspace_root: "D:/work/project".into(),
        task_id: "task-1".into(),
        run_id: "run-1".into(),
        attempt_id: "attempt-1".into(),
        permission_ceiling: "approval-required".into(),
    }
}

#[test]
fn allowlisted_methods_pass_and_unknown_methods_are_denied() {
    let validator = FrameValidator::new(fixture_profile(), bindings());
    let frame = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"D:/work/project","approvalPolicy":"on-request"}}"#;
    validator
        .validate_complete_frame(frame)
        .expect("allowlisted method with valid params");

    let unknown = r#"{"jsonrpc":"2.0","id":2,"method":"eval","params":{}}"#;
    assert!(matches!(
        validator.validate_complete_frame(unknown),
        Err(ConstraintViolation::UnknownMethod(method)) if method == "eval"
    ));
}

#[test]
fn host_bound_and_forbidden_fields_are_enforced() {
    let validator = FrameValidator::new(fixture_profile(), bindings());

    // cwd bound to the workspace root: a different value is refused.
    let escaped = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"E:/evil","approvalPolicy":"on-request"}}"#;
    assert!(matches!(
        validator.validate_complete_frame(escaped),
        Err(ConstraintViolation::NotHostBound { pointer, .. }) if pointer == "/cwd"
    ));

    // Missing bound field is refused (not defaulted).
    let missing = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"approvalPolicy":"on-request"}}"#;
    assert!(matches!(
        validator.validate_complete_frame(missing),
        Err(ConstraintViolation::MissingBound { pointer }) if pointer == "/cwd"
    ));

    // Secret-bearing and permission-escalating fields must be absent.
    let smuggled_key = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"D:/work/project","approvalPolicy":"on-request","apiKey":"sk-123"}}"#;
    assert!(matches!(
        validator.validate_complete_frame(smuggled_key),
        Err(ConstraintViolation::ForbiddenField { pointer }) if pointer == "/apiKey"
    ));
    let escalation = r#"{"jsonrpc":"2.0","id":1,"method":"sendTurn","params":{"cwd":"D:/work/project","escalate":true}}"#;
    assert!(matches!(
        validator.validate_complete_frame(escalation),
        Err(ConstraintViolation::ForbiddenField { pointer }) if pointer == "/escalate"
    ));

    // Enum-constrained fields reject out-of-set values.
    let bad_policy = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"D:/work/project","approvalPolicy":"yolo"}}"#;
    assert!(matches!(
        validator.validate_complete_frame(bad_policy),
        Err(ConstraintViolation::NotEnumerated { pointer, .. }) if pointer == "/approvalPolicy"
    ));

    // within-workspace paths stay under the root.
    let outside = r#"{"jsonrpc":"2.0","id":1,"method":"sendTurn","params":{"cwd":"E:/other"}}"#;
    assert!(matches!(
        validator.validate_complete_frame(outside),
        Err(ConstraintViolation::OutsideWorkspace { pointer, found }) if pointer == "/cwd" && found == "E:/other"
    ));
}

#[test]
fn frame_splitting_cannot_bypass_constraints() {
    let validator = FrameValidator::new(fixture_profile(), bindings());

    // A malicious frame split across two write chunks: the validator only
    // ever sees complete newline-terminated frames. The reassembled line is
    // validated identically to the unsplit version.
    let malicious = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"E:/evil","approvalPolicy":"on-request"}}"#;
    let (first, second) = malicious.split_at(malicious.len() / 2);
    let mut buffer = Vec::new();
    buffer.extend_from_slice(first.as_bytes());
    // First half: no complete frame, nothing validated or forwarded.
    assert!(validator
        .drain_buffer(&mut buffer)
        .expect("no complete frames")
        .is_empty());
    buffer.extend_from_slice(second.as_bytes());
    // Still no newline: the frame must not be forwarded.
    assert!(validator
        .drain_buffer(&mut buffer)
        .expect("no complete frames")
        .is_empty());
    buffer.push(b'\n');
    // Now the complete frame is validated — and refused.
    assert!(matches!(
        validator.drain_buffer(&mut buffer),
        Err(ConstraintViolation::NotHostBound { .. })
    ));

    // Two frames coalesced into one buffer: each is validated separately.
    let good = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"D:/work/project","approvalPolicy":"untrusted"}}"#;
    let bad = r#"{"jsonrpc":"2.0","id":2,"method":"eval","params":{}}"#;
    let mut buffer = Vec::new();
    buffer.extend_from_slice(format!("{good}\n{bad}\n").as_bytes());
    assert!(matches!(
        validator.drain_buffer(&mut buffer),
        Err(ConstraintViolation::UnknownMethod(method)) if method == "eval"
    ));

    // Valid coalesced frames both pass.
    let mut buffer = Vec::new();
    buffer.extend_from_slice(format!("{good}\n{good}\n").as_bytes());
    let frames = validator.drain_buffer(&mut buffer).expect("both valid");
    assert_eq!(frames.len(), 2);
}

#[test]
fn codex_profile_is_package_data_and_kernel_has_no_codex_switch() {
    // The Codex App Server profile ships as plugin-package data.
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/codex/process-profile.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let profile = parse_profile(&bytes).expect("codex profile parses");
    assert_eq!(profile.name, "codex-app-server");
    assert_eq!(profile.framing, ProfileFraming::NdjsonRpc);
    assert!(profile.methods.iter().any(|rule| rule.name == "initialize"));
    assert!(profile.env.contains(&"CODEX_HOME".to_string()));

    // It works with the neutral interpreter unchanged.
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
    let ok = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"D:/work","approvalPolicy":"on-request","sandboxMode":"workspace-write"}}"#;
    validator
        .validate_complete_frame(ok)
        .expect("codex frame through neutral interpreter");
    let smuggled = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"cwd":"D:/work","approvalPolicy":"never","sandboxMode":"danger-full-access","credentials":"sk-1"}}"#;
    assert!(matches!(
        validator.validate_complete_frame(smuggled),
        Err(ConstraintViolation::ForbiddenField { pointer }) if pointer == "/credentials"
    ));

    // The kernel crate contains no Codex method-name switch: scan its source.
    let kernel_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../r-code-kernel/src");
    let mut offending = Vec::new();
    fn scan(dir: &Path, offending: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("kernel src readable") {
            let entry = entry.expect("entry");
            let path = entry.path();
            if path.is_dir() {
                scan(&path, offending);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                let text = std::fs::read_to_string(&path).expect("read");
                if text.to_ascii_lowercase().contains("codex") {
                    offending.push(path.display().to_string());
                }
            }
        }
    }
    scan(&kernel_dir, &mut offending);
    assert!(
        offending.is_empty(),
        "kernel references Codex in {offending:?}"
    );
}
