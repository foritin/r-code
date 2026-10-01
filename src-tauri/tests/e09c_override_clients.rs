//! E09-C — the override record projects to all four clients.
//!
//! Desktop, TUI, MCP and remote render the same canonical camelCase material
//! from `review.overrides.list`; capability-missing surfaces HIDE the control
//! rather than disabling it; malformed rows fail closed in the remote
//! projection. Checked s19b-style by source contract (no daemon spawn):
//! every method a client calls is served by the daemon, the advertised
//! command has a handler and a typed IPC call, and the canonical key order
//! is literally identical across all four clients. The remote projection's
//! fail-closed behavior is pinned for real by the sibling
//! `e09c-overrides.test.mjs` (node --test over the same TS module).

use r_code_host::HARNESS_V1_OVERRIDE_COMMANDS;

const DAEMON_SOURCE: &str = include_str!("../../crates/r-code-runtime/src/bin/r-code-service.rs");
const BRIDGE_SOURCE: &str = include_str!("../src/harness_v1.rs");
const IPC_SOURCE: &str = include_str!("../frontend/src/lib/ipc.ts");
const PERMISSIONS_SOURCE: &str = include_str!("../frontend/src/components/room/Permissions.tsx");
const TUI_SOURCE: &str = include_str!("../../crates/r-code-tui/src/harness_client.rs");
const MCP_SOURCE: &str = include_str!("../../crates/r-code-mcp/src/lib.rs");
const REMOTE_SOURCE: &str = include_str!("../frontend/src/remote/core/approvals-aggregate.ts");

const CANONICAL_KEYS: [&str; 8] = [
    "overrideId",
    "taskId",
    "candidateDigest",
    "actorId",
    "sessionId",
    "reason",
    "checks",
    "createdAtMs",
];

/// A method a client calls but the daemon never dispatches would fail that
/// client forever behind an unexplained protocol error.
#[test]
fn every_client_rpc_is_served_by_the_daemon() {
    assert!(
        DAEMON_SOURCE.contains("\"review.overrides.list\" =>"),
        "the daemon must dispatch review.overrides.list"
    );
    for (client, source) in [
        ("desktop bridge", BRIDGE_SOURCE),
        ("tui overlay", TUI_SOURCE),
        ("mcp tool", MCP_SOURCE),
    ] {
        assert!(
            source.contains("review.overrides.list"),
            "the {client} must call the canonical RPC"
        );
    }
}

/// The advertised desktop command exists as a handler and the typed IPC
/// layer actually calls it. (The one-line `main.rs` invoke-handler wiring is
/// a recorded deviation outside this task's file set — see the ledger; the
/// authoritative command list here is the contract surface.)
#[test]
fn the_desktop_command_is_advertised_implemented_and_typed() {
    assert_eq!(HARNESS_V1_OVERRIDE_COMMANDS.len(), 1, "one read surface");
    for command in HARNESS_V1_OVERRIDE_COMMANDS {
        assert!(
            BRIDGE_SOURCE.contains(&format!("pub async fn {command}(")),
            "{command} is advertised but has no handler"
        );
        assert!(
            IPC_SOURCE.contains(&format!("\"{command}\"")),
            "{command} is never called from the typed IPC layer"
        );
    }
}

/// The canonical key order is literally identical across all four clients.
/// The two TS surfaces declare the six material fields and their row
/// renderer appends `checks` then `createdAtMs`; the TUI and MCP carry the
/// full eight-key order in one declaration.
#[test]
fn all_four_clients_share_the_canonical_key_order() {
    let material: Vec<&'static str> = CANONICAL_KEYS[..6].to_vec();
    let tail: Vec<&'static str> = CANONICAL_KEYS[6..].to_vec();
    let ordered_after_anchor = |source: &str, anchor: &str| -> Vec<&'static str> {
        let start = source
            .find(anchor)
            .unwrap_or_else(|| panic!("anchor {anchor} missing"));
        let window: String = source[start..].chars().take(400).collect();
        CANONICAL_KEYS
            .iter()
            .filter(|key| window.contains(&format!("\"{key}\"")))
            .copied()
            .collect()
    };
    for (surface, source) in [
        ("desktop renderer", PERMISSIONS_SOURCE),
        ("remote projection", REMOTE_SOURCE),
    ] {
        assert_eq!(
            ordered_after_anchor(source, "OVERRIDE_MATERIAL_FIELDS"),
            material,
            "the {surface}'s material declaration must list the six keys in order"
        );
        assert_eq!(
            ordered_after_anchor(source, "overrideAuthorityRows"),
            tail,
            "the {surface}'s row renderer must append checks then createdAtMs"
        );
    }
    assert_eq!(
        ordered_after_anchor(TUI_SOURCE, "const KEY_ORDER"),
        CANONICAL_KEYS,
        "the TUI overlay must render the same key order"
    );
    assert_eq!(
        ordered_after_anchor(MCP_SOURCE, "pub const CANONICAL_FIELDS"),
        CANONICAL_KEYS,
        "the MCP tool surface must carry the same key order"
    );
}

/// The capability-missing desktop surface HIDES the override control (the
/// element is absent — `return null` — never disabled), and the remote
/// projection drops malformed rows rather than defaulting them: a row
/// missing any canonical key returns `null` and the projection filters.
#[test]
fn capability_missing_hides_and_malformed_rows_fail_closed() {
    let panel = permissions_panel_body();
    assert!(
        panel.contains("if (!enabled || rows === null) return null;"),
        "the override panel must be ABSENT when the capability is missing"
    );
    let projection = remote_projection_body();
    assert!(
        projection.contains("return null;"),
        "a row missing canonical keys must be dropped, never defaulted"
    );
    assert!(
        !projection.contains("?? \"\"") && !projection.contains("|| \"\""),
        "the remote projection must never inject default material values"
    );
}

fn source_fn<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("signature {signature} missing"));
    let end = (start + 2200).min(source.len());
    &source[start..end]
}

fn permissions_panel_body() -> &'static str {
    source_fn(PERMISSIONS_SOURCE, "export function OverrideAuditPanel(")
}

fn remote_projection_body() -> &'static str {
    source_fn(REMOTE_SOURCE, "function projectOverrideRow(")
}
