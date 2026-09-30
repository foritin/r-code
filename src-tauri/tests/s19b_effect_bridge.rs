//! S19B-C — the desktop bridge speaks the daemon's effect-approval contract.
//!
//! The grant path itself (decision → exactly one immutable approval,
//! deny/expiry persisting nothing, revoke future-runs-only, the frozen
//! snapshot untouched) is proven e2e against a real daemon and store by
//! `crates/r-code-runtime/tests/s19br_effect_approval_flow.rs`. Unique to this
//! layer, and silently fatal if it drifts: the bridge names methods the daemon
//! dispatches, forwards the canonical camelCase keys, cannot smuggle in its own
//! attribution, registers every handler it implements, and keeps the effect
//! surface unreachable from a remote. Checked without spawning a daemon —
//! `daemon_common`'s nested `cargo build` does not terminate reliably here, and
//! a hanging suite is worse than a narrower one.

use r_code_host::harness_v1::HarnessV1Bridge;
use r_code_host::HARNESS_V1_EFFECT_COMMANDS;
use r_code_runtime::remote::capabilities::required_capability;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};

const CANONICAL_KEYS: [&str; 6] = [
    "taskId",
    "planRevision",
    "workUnitId",
    "effectClass",
    "network",
    "payloadHash",
];

const DAEMON_SOURCE: &str = include_str!("../../crates/r-code-runtime/src/bin/r-code-service.rs");
const BRIDGE_SOURCE: &str = include_str!("../src/harness_v1.rs");
const IPC_SOURCE: &str = include_str!("../frontend/src/lib/ipc.ts");

/// A method the bridge calls but the daemon never dispatches would leave the
/// desktop failing forever behind an unexplained protocol error.
#[test]
fn every_effect_method_the_bridge_calls_is_served_by_the_daemon() {
    for (method, rpc) in [
        ("effect_approval_request", "approvals.effect.request"),
        ("effect_approval_list", "approvals.effect.list"),
        ("effect_approval_revoke", "approvals.effect.revoke"),
        ("approval_decide", "approvals.decide"),
    ] {
        let body = bridge_body(&format!("pub async fn {method}("));
        assert!(
            body.contains(&format!("\"{rpc}\"")),
            "the bridge {method} must call {rpc}"
        );
        assert!(
            DAEMON_SOURCE.contains(&format!("\"{rpc}\" =>")),
            "the bridge calls {rpc} but the daemon does not dispatch it"
        );
    }
}

/// Effect authority is minted only by a locally authenticated decision, so no
/// remote transport may reach it. A `None` capability is what fails it closed,
/// as opposed to merely hiding a button in one client.
#[test]
fn the_effect_surface_is_unreachable_from_a_remote() {
    for rpc in [
        "approvals.effect.request",
        "approvals.effect.list",
        "approvals.effect.revoke",
    ] {
        assert_eq!(required_capability(rpc), None, "{rpc} must stay local-only");
    }
}

/// Every advertised command has a handler and is wired into the invoke
/// handler, and the typed IPC layer actually calls it: an unregistered command
/// is invisible to the webview and fails the desktop with no diagnostic.
#[test]
fn every_effect_command_is_registered_implemented_and_called() {
    assert_eq!(
        HARNESS_V1_EFFECT_COMMANDS.len(),
        4,
        "request / list / revoke / decide"
    );
    let registrations = include_str!("../src/main.rs");
    for command in HARNESS_V1_EFFECT_COMMANDS {
        assert!(
            BRIDGE_SOURCE.contains(&format!("pub async fn {command}(")),
            "{command} is advertised but has no handler"
        );
        assert!(
            registrations.contains(&format!("harness_v1::{command}")),
            "{command} is not wired into the invoke handler"
        );
        assert!(
            IPC_SOURCE.contains(&format!("\"{command}\"")),
            "{command} is never called from the typed IPC layer"
        );
    }
}

/// The bridge forwards the params the daemon resolves and cannot assert its own
/// attribution — actor and session come from the authenticated connection. The
/// webview's typed material is the same six-key canonical set the daemon
/// freezes, so all three clients render one authority rather than three
/// dialects of it.
#[test]
fn the_bridge_and_ipc_agree_on_the_canonical_material_keys() {
    let body = bridge_body("pub async fn effect_approval_request(");
    for key in ["taskId", "workUnitId", "operationId", "runId"] {
        assert!(
            body.contains(&format!("\"{key}\"")),
            "the bridge must forward {key} or the daemon cannot resolve it"
        );
    }
    assert!(
        !body.contains("actorId") && !body.contains("sessionId"),
        "the bridge must not let the webview assert its own attribution"
    );

    let material = IPC_SOURCE
        .find("export interface EffectApprovalMaterial")
        .expect("typed material");
    let block = &IPC_SOURCE[material..material + 400];
    for key in CANONICAL_KEYS {
        assert!(
            block.contains(&format!("{key}: string")),
            "ipc.ts EffectApprovalMaterial is missing {key}"
        );
    }
}

/// A malformed decision is refused before any connection, so no RPC is spent
/// and no daemon state is reachable through a typo in the caller's intent.
#[tokio::test]
async fn a_malformed_decision_is_refused_before_any_rpc() {
    let root = tempfile::tempdir().expect("tempdir");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(root.path())
            .with_ipc_name(format!("s19bc-guard-{}", std::process::id())),
    )
    .expect("profile");
    let bridge = HarnessV1Bridge::new_from_profile(profile, None);
    for decision in ["maybe", "", "GRANTED", "grant"] {
        let error = bridge
            .approval_decide("op-never-registered", decision)
            .await
            .expect_err("only granted|denied are decisions");
        assert!(
            error.to_string().contains("granted or denied"),
            "decision {decision:?}: {error}"
        );
    }
}

fn bridge_body(signature: &str) -> String {
    let start = BRIDGE_SOURCE
        .find(signature)
        .unwrap_or_else(|| panic!("missing {signature}"));
    let rest = &BRIDGE_SOURCE[start..];
    let end = rest[1..]
        .find("\n    pub async fn ")
        .map(|offset| offset + 1)
        .unwrap_or_else(|| rest.find("\n}\n").expect("method terminator"));
    rest[..end].to_string()
}
