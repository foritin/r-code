//! S19B-C — TUI effect approval overlay parity with the desktop renderer.
//!
//! The three clients (desktop Permissions/Canvas, remote projection, TUI
//! overlay) must show the SAME authority. This pins the TUI half: the six
//! canonical material keys in the frozen `EFFECT_MATERIAL_FIELDS` order,
//! then approvalId/scope/actorId/sessionId/state, and the three standing
//! semantics (revocation is future-runs-only, a grant id is never reused, a
//! denial or expiry persists nothing) in both the active and superseded
//! states. `from_list_row` must also fail closed on a row missing any
//! canonical key rather than defaulting it.

use r_code_tui::approval_overlay::{effect_overlay_lines, EffectAuthority, LineKind, OverlayLine};
use serde_json::json;

/// The frozen canonical order (`Permissions.EFFECT_MATERIAL_FIELDS`).
const MATERIAL_KEYS: [&str; 6] = [
    "taskId",
    "planRevision",
    "workUnitId",
    "effectClass",
    "network",
    "payloadHash",
];

fn row(state: &str) -> serde_json::Value {
    json!({
        "taskId": "task-s19bc",
        "planRevision": "rev-1",
        "workUnitId": "unit-shell",
        "effectClass": "workspace-mutation",
        "network": "public-internet-client",
        "payloadHash": "sha256:abc",
        "approvalId": "op-s19bc-1",
        "actorId": "actor-x",
        "sessionId": "cmd-x",
        "scope": "effect.approve",
        "state": state,
    })
}

fn authority(state: &str) -> EffectAuthority {
    EffectAuthority::from_list_row(&row(state)).expect("complete row projects")
}

fn texts(lines: &[OverlayLine]) -> Vec<String> {
    lines.iter().map(|line| line.text.clone()).collect()
}

/// A label line is `<key><padding><value>`; the key must appear in order.
fn keyed_lines(lines: &[OverlayLine]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.kind == LineKind::Hint)
        .map(|line| {
            line.text
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[test]
fn effect_overlay_renders_the_six_canonical_keys_in_frozen_order() {
    let lines = effect_overlay_lines(&authority("active"));
    let keys = keyed_lines(&lines);
    let authority_keys = &keys[..MATERIAL_KEYS.len()];
    assert_eq!(authority_keys, MATERIAL_KEYS, "canonical material order");
    // …followed by the persisted identity, in the desktop's own order.
    assert_eq!(
        &keys[MATERIAL_KEYS.len()..MATERIAL_KEYS.len() + 5],
        &["approvalId", "scope", "actorId", "sessionId", "state"],
        "identity keys follow the six columns"
    );
    assert_eq!(lines[0].kind, LineKind::Title, "first line is the title");
    // Every value the daemon froze is present verbatim (no derivation).
    let rendered = texts(&lines).join("\n");
    for value in [
        "task-s19bc",
        "rev-1",
        "unit-shell",
        "workspace-mutation",
        "public-internet-client",
        "sha256:abc",
        "op-s19bc-1",
        "actor-x",
        "cmd-x",
        "effect.approve",
    ] {
        assert!(rendered.contains(value), "missing value {value}");
    }
}

#[test]
fn both_states_state_the_three_standing_semantics() {
    for state in ["active", "superseded"] {
        let rendered = texts(&effect_overlay_lines(&authority(state))).join("\n");
        assert!(
            rendered.contains("撤销仅影响未来运行"),
            "{state}: future-runs-only must be stated, not just implied"
        );
        assert!(
            rendered.contains("授权编号终身不复用"),
            "{state}: id-never-reused must be stated"
        );
        assert!(
            rendered.contains("拒绝或过期不会留下任何授权记录"),
            "{state}: denial-persists-nothing must be stated"
        );
    }
}

#[test]
fn state_label_and_revoke_option_follow_the_authority_state() {
    let active = effect_overlay_lines(&authority("active"));
    let superseded = effect_overlay_lines(&authority("superseded"));
    let joined = |lines: &[OverlayLine]| texts(lines).join("\n");
    assert!(joined(&active).contains("生效中"), "active reads 生效中");
    assert!(
        joined(&superseded).contains("已撤销"),
        "superseded reads 已撤销"
    );
    // Revoking is offered only while the grant is live; a superseded row is
    // audit-only, so no control is rendered for it.
    assert!(joined(&active).contains("撤销此授权"));
    assert!(!joined(&superseded).contains("撤销此授权"));
    assert!(authority("active").is_active());
    assert!(!authority("superseded").is_active());
}

#[test]
fn from_list_row_refuses_a_row_missing_any_canonical_key() {
    const IDENTITY_KEYS: [&str; 5] = ["approvalId", "actorId", "sessionId", "scope", "state"];
    let required = MATERIAL_KEYS
        .iter()
        .copied()
        .chain(IDENTITY_KEYS)
        .collect::<Vec<&str>>();
    for key in required {
        let mut incomplete = row("active");
        incomplete.as_object_mut().expect("object").remove(key);
        assert!(
            EffectAuthority::from_list_row(&incomplete).is_none(),
            "a row missing {key} must project to nothing, never a default"
        );
    }
    // A non-string material value is equally disqualifying.
    let mut mistyped = row("active");
    mistyped["network"] = json!(42);
    assert!(EffectAuthority::from_list_row(&mistyped).is_none());
    assert!(EffectAuthority::from_list_row(&json!(null)).is_none());
    assert!(EffectAuthority::from_list_row(&json!("not-a-row")).is_none());
}

/// An unrecognized `state` must be refused, never rendered.
///
/// This pinned a real gap: `is_active()` is a two-state test, so a state
/// outside `active|superseded` projected as authority and rendered as
/// 已撤销 — masquerading as a revocation the daemon never recorded. The
/// remote projection and the desktop panel already rejected that set, so
/// the TUI was the only surface failing open; `from_list_row` now refuses it.
#[test]
fn unknown_state_must_be_refused_rather_than_rendered_as_revoked() {
    let mut unknown_state = row("active");
    unknown_state["state"] = json!("granted");
    let projected = EffectAuthority::from_list_row(&unknown_state);
    assert!(
        projected.is_none(),
        "only active|superseded are authority states; got {projected:?}"
    );
    // Refusal is the whole remedy: an unrecognized state never reaches the
    // renderer, so no overlay line can claim it was revoked.
    assert!(
        EffectAuthority::from_list_row(&unknown_state)
            .map(|authority| texts(&effect_overlay_lines(&authority)))
            .unwrap_or_default()
            .into_iter()
            .all(|line| !line.contains("已撤销")),
        "an unrecognized state must not be displayed as a revocation"
    );
    // The two legitimate states must still project, or this would be a
    // blanket rejection rather than a state restriction.
    assert!(EffectAuthority::from_list_row(&row("active")).is_some());
    assert!(EffectAuthority::from_list_row(&row("superseded")).is_some());
    let mut non_string = row("active");
    non_string["state"] = json!(true);
    assert!(EffectAuthority::from_list_row(&non_string).is_none());
}
