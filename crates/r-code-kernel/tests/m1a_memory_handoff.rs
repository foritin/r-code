//! M1a-01 (FR-7.1): the frozen memory handoff contract.
//!
//! Pins: wire round-trip, legacy-payload compatibility (pre-FR-7 rows
//! deserialize with `memory = None`), and the create-path validation caps
//! that keep a hostile payload from ballooning the frozen prompt.

use r_code_kernel::task::{FrozenMemoryHandoff, TaskContract, TaskKind};

fn base_handoff() -> FrozenMemoryHandoff {
    FrozenMemoryHandoff {
        rendered: "<r_code_memory_snapshot>frozen</r_code_memory_snapshot>".into(),
        entry_ids: vec!["entry-1".into(), "entry-2".into()],
        snapshot_hash: "hash-1".into(),
    }
}

#[test]
fn handoff_roundtrips_exactly_on_the_wire() {
    let contract = TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Conversation,
        objective: "objective".into(),
        constraints: vec![],
        required_checks: vec![],
        memory: Some(base_handoff()),
        revision: 1,
    };
    let json = serde_json::to_string(&contract).expect("serialize");
    assert!(json.contains("\"memory\""), "payload: {json}");
    let back: TaskContract = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, contract);
}

#[test]
fn legacy_payloads_default_memory_to_none() {
    let legacy = r#"{"task_id":"task-1","kind":"conversation","objective":"o","revision":1}"#;
    let old: TaskContract = serde_json::from_str(legacy).expect("legacy deserialize");
    assert!(old.memory.is_none());
    assert!(old.constraints.is_empty());
    assert!(old.required_checks.is_empty());
}

#[test]
fn handoff_validation_pins_the_wire_caps() {
    assert!(base_handoff().validate().is_ok());

    let nul = FrozenMemoryHandoff {
        rendered: "bad\u{0}block".into(),
        ..base_handoff()
    };
    assert!(nul.validate().is_err());

    let oversize = FrozenMemoryHandoff {
        rendered: "x".repeat(32_769),
        ..base_handoff()
    };
    assert!(oversize.validate().is_err());

    let blank_hash = FrozenMemoryHandoff {
        snapshot_hash: "  ".into(),
        ..base_handoff()
    };
    assert!(blank_hash.validate().is_err());

    let too_many_ids = FrozenMemoryHandoff {
        entry_ids: (0..65).map(|i| format!("entry-{i}")).collect(),
        ..base_handoff()
    };
    assert!(too_many_ids.validate().is_err());

    let blank_id = FrozenMemoryHandoff {
        entry_ids: vec![" ".into()],
        ..base_handoff()
    };
    assert!(blank_id.validate().is_err());
}
