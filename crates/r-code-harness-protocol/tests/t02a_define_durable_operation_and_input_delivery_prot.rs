//! T02a — durable operation and input delivery protocol.
//!
//! Fixtures cover: lost effect response, duplicate key with different input,
//! generation restart preserving dedup history, checkpoint lag replay and
//! lost external acknowledgements.

use r_code_harness_protocol::*;

fn record(method: &str, state: OperationState, generation: u64) -> OperationRecord {
    let params = serde_json::json!({"path": "src/lib.rs", "content": "fn main(){}"});
    OperationRecord {
        attempt_id: "attempt-1".into(),
        operation_key: OperationKey::new("write-lib"),
        method: method.into(),
        input_hash: canonical_input_hash(&params),
        state,
        generation_recorded: generation,
        generation_completed: None,
    }
}

fn same_input_hash(record: &OperationRecord) -> String {
    record.input_hash.clone()
}

#[test]
fn completed_effects_replay_their_receipt() {
    let stored = record(
        "host.tools.call",
        OperationState::Completed {
            result: serde_json::json!({"ok": true}),
        },
        1,
    );
    let decision = replay_decision(Some(&stored), "host.tools.call", &same_input_hash(&stored));
    assert_eq!(
        decision,
        ReplayDecision::ReplayReceipt {
            result: serde_json::json!({"ok": true})
        }
    );
}

#[test]
fn lost_effect_response_reconciles_by_replay_class() {
    let pending_file = record("host.tools.call", OperationState::Pending, 1);
    assert_eq!(
        replay_decision(
            Some(&pending_file),
            "host.tools.call",
            &same_input_hash(&pending_file)
        ),
        ReplayDecision::Reconcile {
            class: ReplayClass::FileEffect
        }
    );

    let mut pending_process = record("host.process.write", OperationState::Pending, 1);
    pending_process.input_hash = canonical_input_hash(&serde_json::json!({"handle": "h1"}));
    assert_eq!(
        replay_decision(
            Some(&pending_process),
            "host.process.write",
            &same_input_hash(&pending_process)
        ),
        ReplayDecision::Reconcile {
            class: ReplayClass::ProcessEffect
        }
    );
}

#[test]
fn duplicate_key_with_different_input_is_refused() {
    let stored = record(
        "host.tools.call",
        OperationState::Completed {
            result: serde_json::json!({}),
        },
        1,
    );
    let other_hash =
        canonical_input_hash(&serde_json::json!({"path": "src/lib.rs", "content": "evil"}));
    match replay_decision(Some(&stored), "host.tools.call", &other_hash) {
        ReplayDecision::ConflictingInput { recorded, incoming } => {
            assert_eq!(recorded, stored.input_hash);
            assert_eq!(incoming, other_hash);
        }
        other => panic!("expected ConflictingInput, got {other:?}"),
    }
}

#[test]
fn generation_restart_does_not_erase_dedup_history() {
    // Recorded and completed in generation 1; the run restarted at generation 2.
    let mut stored = record(
        "host.tools.call",
        OperationState::Completed {
            result: serde_json::json!({"ok": true}),
        },
        1,
    );
    stored.generation_completed = Some(1);
    assert_eq!(
        replay_decision(Some(&stored), "host.tools.call", &same_input_hash(&stored)),
        ReplayDecision::ReplayReceipt {
            result: serde_json::json!({"ok": true})
        }
    );
    // A conflicting retry is still refused after the restart.
    let other_hash = canonical_input_hash(&serde_json::json!({"different": true}));
    assert!(matches!(
        replay_decision(Some(&stored), "host.tools.call", &other_hash),
        ReplayDecision::ConflictingInput { .. }
    ));
}

#[test]
fn checkpoint_lag_replays_only_unconsumed_inputs() {
    let delivered: Vec<InputMessage> = (1..=5)
        .map(|seq| InputMessage {
            message_id: format!("msg-{seq}"),
            input_seq: seq,
            kind: InputKind::User,
            text: format!("input {seq}"),
        })
        .collect();

    let checkpoint = ConsumedInputCheckpoint {
        consumed_input_seq: 3,
    };
    let replay = inputs_to_redeliver(checkpoint, &delivered);
    assert_eq!(
        replay.iter().map(|m| m.input_seq).collect::<Vec<_>>(),
        vec![4, 5]
    );

    let caught_up = ConsumedInputCheckpoint {
        consumed_input_seq: 5,
    };
    assert!(inputs_to_redeliver(caught_up, &delivered).is_empty());

    // Consuming beyond what was delivered is invalid and must be rejected.
    assert!(matches!(
        validate_consumed_prefix(
            ConsumedInputCheckpoint {
                consumed_input_seq: 6
            },
            5
        ),
        Err(InputAckError::ConsumedBeyondDelivered { .. })
    ));
    assert!(validate_consumed_prefix(checkpoint, 5).is_ok());
}

#[test]
fn lost_external_acknowledgement_is_indeterminate() {
    let state = lost_ack_outcome(IndeterminateReason::LostExternalAcknowledgement);
    match &state {
        OperationState::Indeterminate { reason } => {
            assert_eq!(reason, "lost-external-acknowledgement");
        }
        other => panic!("expected Indeterminate, got {other:?}"),
    }
    // An indeterminate record is never blindly re-executed: replay asks for
    // conservative reconciliation instead of Fresh.
    let mut stored = record("host.process.close", state, 2);
    stored.input_hash = canonical_input_hash(&serde_json::json!({"handle": "h1"}));
    assert_eq!(
        replay_decision(
            Some(&stored),
            "host.process.close",
            &same_input_hash(&stored)
        ),
        ReplayDecision::Reconcile {
            class: ReplayClass::ExternalAck
        }
    );
}

#[test]
fn replay_classes_cover_all_effectful_host_methods() {
    let effectful = [
        "host.model.stream",
        "host.tools.call",
        "host.process.open",
        "host.process.write",
        "host.process.close",
        "host.children.spawn",
        "host.verification.run",
    ];
    for method in effectful {
        let class = ReplayClass::for_method(method).expect("classified");
        assert!(class.is_effectful(), "{method} must be effectful");
    }
    for query in [
        "host.tools.list",
        "host.context.read",
        "host.artifacts.read",
    ] {
        assert_eq!(
            ReplayClass::for_method(query),
            Some(ReplayClass::Idempotent)
        );
    }
    assert_eq!(ReplayClass::for_method("host.plan.publish"), None);
}
