use std::collections::BTreeMap;
use std::fmt::Debug;
use std::num::NonZeroU32;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use r_code_core::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use GlobalMemoryAuthorization as Auth;
use MemoryContractErrorCode as ContractCode;
use MemoryMutationErrorCode as MutationCode;
use MemoryProposalValidationCode as ProposalCode;
use MemoryReviewInputErrorCode as InputCode;
use MemoryReviewJobState as JobState;
use MemorySnapshotOwner as SnapshotOwner;
use SensitiveMemoryMutation as SMut;

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).expect("valid test timestamp")
}

fn cursor(value: i64) -> SequenceCursor {
    SequenceCursor::new(value).expect("positive test cursor")
}

fn project_source() -> FrozenReviewSource {
    serde_json::from_str(r#"{"context":"project","run_id":"run","task_id":"task","branch_id":"branch","workspace_id":"workspace","workspace_memory_generation":7}"#).unwrap()
}

fn roundtrip<T>(wire: &str) -> T
where
    T: DeserializeOwned + Serialize + Debug + PartialEq,
{
    let decoded: T = serde_json::from_str(wire).expect("deserialize exact wire value");
    assert_eq!(
        serde_json::to_value(&decoded).unwrap(),
        serde_json::from_str::<Value>(wire).unwrap()
    );
    decoded
}

fn rejects<T: Debug>(result: std::result::Result<T, MemoryContractError>, code: ContractCode) {
    assert_eq!(result.expect_err("contract must reject value").code(), code);
}

#[test]
fn constants_are_frozen() {
    assert_eq!(MEMORY_REVIEW_SCHEMA_VERSION, 1);
    assert_eq!(MEMORY_SNAPSHOT_SCHEMA_VERSION, 1);
    assert_eq!(MEMORY_HASH_VERSION, "blake3_utf8_v1");
    assert_eq!(MEMORY_NORMALIZATION_VERSION, "trim_crlf_unicode_scalar_v1");
    assert_eq!(MEMORY_GLOBAL_CHAR_CAP, 4_000);
    assert_eq!(MEMORY_PROJECT_CHAR_CAP, 8_000);
    assert_eq!(MEMORY_ENTRY_CHAR_CAP, 1_000);
    assert_eq!(MEMORY_ACTIVE_ENTRIES_PER_SCOPE_CAP, 32);
    assert_eq!(MEMORY_REVIEW_PROPOSAL_CAP, 8);
    assert_eq!(MEMORY_TRIGGER_TURNS_MIN, 5);
    assert_eq!(MEMORY_TRIGGER_TURNS_DEFAULT, 10);
    assert_eq!(MEMORY_TRIGGER_TURNS_MAX, 50);
    assert_eq!(MEMORY_RAW_TURN_RETENTION_DAYS, 30);
    assert_eq!(MEMORY_RAW_TURNS_PER_BRANCH_CAP, 50);
    assert_eq!(MEMORY_PENDING_BODY_RETENTION_DAYS, 90);
    assert_eq!(MEMORY_TERMINAL_METADATA_RETENTION_DAYS, 180);
    assert_eq!(MEMORY_TERMINAL_METADATA_CAP, 500);
    assert_eq!(MEMORY_REVISION_RETENTION_DAYS, 180);
    assert_eq!(MEMORY_REVISIONS_PER_ENTRY_CAP, 20);
    assert_eq!(MEMORY_REVIEW_ENVELOPE_CHAR_CAP, 24_000);
    assert_eq!(MEMORY_PROPOSAL_CONTENT_MIN_CHARS, 5);
    assert_eq!(MEMORY_PROPOSAL_REASON_CHAR_CAP, 300);
}

#[test]
fn settings_default_and_wire_are_closed() {
    let view: MemoryReviewSettingsView = serde_json::from_value(json!({})).unwrap();
    assert!(!view.enabled);
    assert_eq!(view.trigger_every_turns, 10);
    assert!(view.explicit_remember_immediate);
    assert_eq!(view.project_notification_mode, ProjectNotificationMode::On);
    assert!(serde_json::from_value::<MemoryReviewSettingsView>(json!({"extra": 1})).is_err());

    let update = json!({
        "enabled": false, "reviewer": null, "trigger_every_turns": 10,
        "explicit_remember_immediate": true, "project_notification_mode": "on"
    });
    assert!(serde_json::from_value::<MemoryReviewSettingsUpdate>(update.clone()).is_err());
    for private in ["retention_time_high_watermark", "physical_cleanup_epoch"] {
        let mut value = update.clone();
        value["expected_version"] = json!(0);
        value[private] = json!(1);
        assert!(serde_json::from_value::<MemoryReviewSettingsUpdate>(value).is_err());
    }
}

#[test]
fn settings_validation_requires_a_real_reviewer() {
    let mut view = MemoryReviewSettingsView {
        enabled: true,
        ..MemoryReviewSettingsView::default()
    };
    rejects(view.validate(), ContractCode::ReviewerRequired);
    for wire in [
        r#"{"provider_name":" ","model":"model"}"#,
        r#"{"provider_name":"provider","model":""}"#,
    ] {
        view.reviewer = Some(serde_json::from_str(wire).unwrap());
        rejects(view.validate(), ContractCode::InvalidReviewer);
    }
}

#[test]
fn internal_settings_view_does_not_leak_private_retention_fields() {
    let internal = MemoryReviewSettings {
        enabled: false,
        reviewer: None,
        trigger_every_turns: 10,
        explicit_remember_immediate: true,
        project_notification_mode: ProjectNotificationMode::On,
        version: 3,
        review_generation: 4,
        retention_time_high_watermark: at(1),
        physical_cleanup_pending: true,
        physical_cleanup_epoch: 9,
        updated_at: at(2),
    };
    let wire = serde_json::to_value(internal.to_view()).unwrap();
    assert!(wire.get("retention_time_high_watermark").is_none());
    assert!(wire.get("physical_cleanup_epoch").is_none());
    assert_eq!(wire["physical_cleanup_pending"], true);
}

#[test]
fn owner_wire_roundtrips_exactly_and_rejects_invalid_authority() {
    roundtrip::<MemoryOwner>(r#"{"scope":"global","authorization":"manual"}"#);
    roundtrip::<MemoryOwner>(r#"{"scope":"global","authorization":"approved_candidate"}"#);
    roundtrip::<MemoryOwner>(
        r#"{"scope":"project","workspace_id":"workspace","origin":"automatic_review"}"#,
    );
    assert!(serde_json::from_value::<MemoryOwner>(json!({
        "scope": "global", "authorization": "automatic_review"
    }))
    .is_err());
    assert!(serde_json::from_value::<MemoryOwner>(json!({
        "scope": "global", "authorization": "manual", "workspace_id": "surplus"
    }))
    .is_err());
    assert!(serde_json::from_value::<MemoryOwner>(json!({"scope": "team"})).is_err());
}

#[test]
fn frozen_source_wire_is_an_exact_tagged_union() {
    let pure = r#"{"context":"pure_chat","run_id":"run","task_id":"task","branch_id":"branch"}"#;
    let project = r#"{"context":"project","run_id":"run","task_id":"task","branch_id":"branch","workspace_id":"workspace","workspace_memory_generation":7}"#;
    roundtrip::<FrozenReviewSource>(pure);
    roundtrip::<FrozenReviewSource>(project);
    let mut nullable_pure: Value = serde_json::from_str(pure).unwrap();
    nullable_pure["workspace_id"] = Value::Null;
    nullable_pure["workspace_memory_generation"] = Value::Null;
    assert!(serde_json::from_value::<FrozenReviewSource>(nullable_pure).is_err());
    let mut nullable_project: Value = serde_json::from_str(project).unwrap();
    nullable_project["workspace_id"] = Value::Null;
    assert!(serde_json::from_value::<FrozenReviewSource>(nullable_project).is_err());
}

#[test]
fn sequence_cursor_is_a_canonical_decimal_json_string() {
    let value = cursor(i64::MAX);
    assert_eq!(
        serde_json::to_value(value).unwrap(),
        json!(i64::MAX.to_string())
    );
    assert_eq!(
        serde_json::from_value::<SequenceCursor>(json!("42")).unwrap(),
        cursor(42)
    );
    assert!(serde_json::from_value::<SequenceCursor>(json!(42)).is_err());
    for invalid in "0|-1|01|+1| 1|1.0|a|9223372036854775808".split('|') {
        assert!(
            SequenceCursor::from_str(invalid).is_err(),
            "accepted {invalid:?}"
        );
        assert!(serde_json::from_value::<SequenceCursor>(json!(invalid)).is_err());
    }
}

#[test]
fn sequence_page_is_closed_and_keeps_cursor_exact() {
    let page =
        roundtrip::<SequencePage<String>>(r#"{"items":["entry"],"next_before_sequence":"9"}"#);
    assert_eq!(page.next_before_sequence, Some(cursor(9)));
    assert!(serde_json::from_value::<SequencePage<String>>(json!({
        "items": [], "next_before_sequence": null, "unknown": true
    }))
    .is_err());
}

fn pending_candidate() -> MemoryCandidate {
    let mut value: MemoryCandidate = serde_json::from_str(r#"{"sequence":"1","id":"candidate","kind":"preference","mutation":{"operation":"add"},"source_task_id":"task","source_workspace_id":"workspace","source_run_id":"run","captured_at":"1970-01-01T00:00:01Z","source_job_id":"job","proposal_index":7,"proposal_hash":"proposal-hash","reason_hash":"reason-hash","confidence":1.0,"state":{"status":"pending","proposed_content":"12345","reason":""},"created_at":"1970-01-01T00:00:02Z","updated_at":"1970-01-01T00:00:02Z"}"#).unwrap();
    if let MemoryCandidateState::Pending { reason, .. } = &mut value.state {
        *reason = "r".repeat(300);
    }
    value
}

#[test]
fn pending_candidate_enforces_body_confidence_and_index_caps() {
    let candidate = pending_candidate();
    candidate.validate().unwrap();
    for content in ["1234".to_owned(), "x".repeat(1_001)] {
        let mut invalid = candidate.clone();
        invalid.state = MemoryCandidateState::Pending {
            proposed_content: content,
            reason: "ok".into(),
        };
        rejects(invalid.validate(), ContractCode::InvalidCandidateState);
    }
    let mut invalid = candidate.clone();
    invalid.state = MemoryCandidateState::Pending {
        proposed_content: "valid".into(),
        reason: "x".repeat(301),
    };
    rejects(invalid.validate(), ContractCode::InvalidCandidateState);
    for confidence in [-0.1, 1.1, f64::INFINITY, f64::NAN] {
        let mut invalid = candidate.clone();
        invalid.confidence = confidence;
        rejects(invalid.validate(), ContractCode::InvalidConfidence);
    }
    let mut invalid = candidate;
    invalid.proposal_index = 8;
    rejects(invalid.validate(), ContractCode::InvalidCandidateState);
}

#[test]
fn candidate_terminal_state_drops_body_and_candidate_has_no_scope() {
    let mut candidate = pending_candidate();
    candidate.state = MemoryCandidateState::Approved { resolved_at: at(3) };
    let mut wire = serde_json::to_value(&candidate).unwrap();
    assert!(wire.get("scope").is_none());
    assert!(wire["state"].get("proposed_content").is_none());
    assert!(wire["state"].get("reason").is_none());
    wire["state"]["proposed_content"] = json!("forbidden body");
    wire["state"]["reason"] = json!("forbidden reason");
    assert!(serde_json::from_value::<MemoryCandidate>(wire).is_err());
}

#[test]
fn replace_candidate_target_is_typed_and_closed() {
    let mutation = MemoryCandidateMutation::Replace {
        target_entry_id: "entry".into(),
        target_version: 4,
    };
    let wire = json!({"operation": "replace", "target_entry_id": "entry", "target_version": 4});
    assert_eq!(serde_json::to_value(&mutation).unwrap(), wire);
    assert_eq!(
        serde_json::from_value::<MemoryCandidateMutation>(wire).unwrap(),
        mutation
    );
    assert!(serde_json::from_value::<MemoryCandidateMutation>(json!({
        "operation": "replace", "target_entry_id": 2, "target_version": 4
    }))
    .is_err());
}

fn queued_job() -> MemoryReviewJob {
    serde_json::from_str(r#"{"sequence":"1","id":"job","source":{"context":"pure_chat","run_id":"run","task_id":"task","branch_id":"branch"},"review_generation":2,"reviewer":{"provider_name":"provider","model":"model"},"inclusive_boundary":"10","attempt":0,"recovery_count":0,"suppressed_turn_count":1,"trigger":"cadence","state":{"status":"queued","queued_at":"1970-01-01T00:00:10Z"},"created_at":"1970-01-01T00:00:10Z","updated_at":"1970-01-01T00:00:10Z"}"#).unwrap()
}

fn next_job(
    current: &MemoryReviewJob,
    state: JobState,
    attempt: u32,
    recovery_count: u32,
) -> MemoryReviewJob {
    let mut next = current.clone();
    next.state = state;
    next.attempt = attempt;
    next.recovery_count = recovery_count;
    next.updated_at = at(11);
    next
}

fn rejects_transition(current: &MemoryReviewJob, next: &MemoryReviewJob, code: ContractCode) {
    rejects(current.validate_next_attempt(next), code);
}

fn running_state() -> JobState {
    JobState::Running { started_at: at(11) }
}

fn succeeded_state() -> JobState {
    JobState::Succeeded {
        completed_at: at(12),
        input_hash: "hash".into(),
        turn_count: 3,
        proposal_count: 2,
    }
}

fn failed_state() -> JobState {
    JobState::Failed {
        failed_at: at(10),
        error_code: MutationCode::ProviderRequestFailed,
    }
}

fn interrupted_state() -> JobState {
    JobState::Interrupted {
        interrupted_at: at(10),
        error_code: MutationCode::ReviewInterrupted,
    }
}

fn requeued_state() -> JobState {
    JobState::Queued { queued_at: at(11) }
}

#[test]
fn job_attempt_and_recovery_counters_follow_transition_kind() {
    let queued = queued_job();
    let running = next_job(&queued, running_state(), 1, 0);
    queued.validate_next_attempt(&running).unwrap();
    let succeeded = next_job(&running, succeeded_state(), 1, 0);
    running.validate_next_attempt(&succeeded).unwrap();
    for state in [failed_state(), interrupted_state()] {
        let failed = next_job(&queued, state, 2, 3);
        let recovered = next_job(&failed, requeued_state(), 2, 4);
        failed.validate_next_attempt(&recovered).unwrap();
    }
}

#[test]
fn job_rejects_invalid_transitions_and_counters() {
    let queued = queued_job();
    let succeeded = next_job(&queued, succeeded_state(), 0, 0);
    rejects_transition(&queued, &succeeded, ContractCode::InvalidTransition);
    let wrong_attempt = next_job(&queued, running_state(), 0, 0);
    rejects_transition(&queued, &wrong_attempt, ContractCode::InvalidAttempt);
    let failed = next_job(&queued, failed_state(), 2, 3);
    let wrong_recovery = next_job(&failed, requeued_state(), 2, 3);
    rejects_transition(&failed, &wrong_recovery, ContractCode::InvalidAttempt);
}

#[test]
fn job_rejects_counter_overflow_and_non_monotonic_mutation() {
    let mut queued = queued_job();
    queued.attempt = u32::MAX;
    let running = next_job(&queued, running_state(), u32::MAX, 0);
    rejects_transition(&queued, &running, ContractCode::InvalidAttempt);
    let mut failed = queued_job();
    failed.state = failed_state();
    failed.attempt = 2;
    failed.recovery_count = u32::MAX;
    let recovered = next_job(&failed, requeued_state(), 2, u32::MAX);
    rejects_transition(&failed, &recovered, ContractCode::InvalidAttempt);
    let base = queued_job();
    let valid = next_job(&base, running_state(), 1, 0);
    for mutate in ["id", "updated_at", "suppressed_turn_count"] {
        let mut invalid = valid.clone();
        match mutate {
            "id" => invalid.id = "different".into(),
            "updated_at" => invalid.updated_at = at(9),
            _ => invalid.suppressed_turn_count = 0,
        }
        rejects_transition(&base, &invalid, ContractCode::InvalidJobMutation);
    }
}

#[test]
fn terminal_job_state_wire_keeps_auditable_fields_and_stable_error() {
    let wire = serde_json::to_value(succeeded_state()).unwrap();
    assert_eq!(wire["status"], "succeeded");
    assert_eq!(wire["input_hash"], "hash");
    assert_eq!(wire["turn_count"], 3);
    assert_eq!(wire["proposal_count"], 2);
    let interrupted = interrupted_state();
    assert_eq!(
        serde_json::to_value(interrupted).unwrap()["error_code"],
        "review_interrupted"
    );
}

#[test]
fn outcome_effect_prevents_global_applied_but_allows_project_applied() {
    assert!(serde_json::from_value::<MemoryReviewOutcomeEffect>(json!({
        "route": "global_candidate", "result": "applied"
    }))
    .is_err());
    roundtrip::<MemoryReviewOutcomeEffect>(r#"{"route":"project_entry","result":"applied"}"#);
    let mut wire = json!({
        "sequence": "1", "job_id": "job", "proposal_index": 0,
        "effect": {"route": "skipped", "result": "rejected"},
        "entry_id": null, "candidate_id": null, "error_code": "capacity_exceeded",
        "created_at": "1970-01-01T00:00:01Z"
    });
    let outcome: MemoryReviewOutcome = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(outcome.error_code, Some(MutationCode::CapacityExceeded));
    assert_eq!(wire["error_code"], "capacity_exceeded");
    wire["error_code"] = json!("free_form_error");
    assert!(serde_json::from_value::<MemoryReviewOutcome>(wire).is_err());
}

fn entry(id: &str, content: String) -> MemorySnapshotEntry {
    MemorySnapshotEntry {
        entry_id: id.into(),
        version: 1,
        kind: MemoryKind::Preference,
        content,
    }
}

fn project_owner() -> SnapshotOwner {
    SnapshotOwner::Project {
        workspace_id: "workspace".into(),
        workspace_memory_generation: 7,
    }
}

fn snapshot(owner: SnapshotOwner, global: Vec<String>, project: Vec<String>) -> MemorySnapshot {
    let global_chars = global
        .iter()
        .map(|value| value.chars().count() as u32)
        .sum();
    let project_chars = project
        .iter()
        .map(|value| value.chars().count() as u32)
        .sum();
    MemorySnapshot {
        schema_version: MEMORY_SNAPSHOT_SCHEMA_VERSION,
        owner,
        global_generation: 5,
        global_entries: global
            .into_iter()
            .enumerate()
            .map(|(i, value)| entry(&format!("g{i}"), value))
            .collect(),
        project_entries: project
            .into_iter()
            .enumerate()
            .map(|(i, value)| entry(&format!("p{i}"), value))
            .collect(),
        snapshot_hash: "hash".into(),
        global_chars,
        project_chars,
    }
}

fn assert_invalid_snapshot(value: &MemorySnapshot) {
    rejects(value.validate(), ContractCode::InvalidSnapshot);
}

#[test]
fn snapshot_rejects_pure_chat_project_data_and_integrity_violations() {
    let owner = SnapshotOwner::PureChat;
    let pure_with_project = snapshot(owner.clone(), vec![], vec!["project".into()]);
    assert_invalid_snapshot(&pure_with_project);
    let valid = snapshot(owner, vec!["global".into()], vec![]);
    valid.validate().unwrap();
    let mut wrong = valid.clone();
    wrong.schema_version += 1;
    assert_invalid_snapshot(&wrong);
    wrong = valid.clone();
    wrong.global_chars += 1;
    assert_invalid_snapshot(&wrong);
    wrong = valid.clone();
    wrong.snapshot_hash = " ".into();
    assert_invalid_snapshot(&wrong);
    for content in [String::new(), "x".repeat(1_001)] {
        assert_invalid_snapshot(&snapshot(SnapshotOwner::PureChat, vec![content], vec![]));
    }
}

#[test]
fn snapshot_rejects_scope_character_and_entry_count_caps() {
    let global_over = snapshot(SnapshotOwner::PureChat, vec!["x".repeat(1_000); 5], vec![]);
    assert_invalid_snapshot(&global_over);
    let project_over = snapshot(project_owner(), vec![], vec!["x".repeat(1_000); 9]);
    assert_invalid_snapshot(&project_over);
    let count_over = snapshot(SnapshotOwner::PureChat, vec!["x".into(); 33], vec![]);
    assert_invalid_snapshot(&count_over);
}

#[test]
fn project_run_requires_exact_snapshot_owner_and_generations() {
    let project = snapshot(project_owner(), vec!["g".into()], vec!["p".into()]);
    let run = RunContext {
        source: project_source(),
        global_memory_generation: 5,
        memory: MemorySnapshotLoadOutcome::Ready { snapshot: project },
    };
    run.validate().unwrap();
    for mutation in ["workspace", "project_generation", "global_generation"] {
        let mut invalid = run.clone();
        match mutation {
            "workspace" => match &mut invalid.source {
                FrozenReviewSource::Project { workspace_id, .. } => *workspace_id = "other".into(),
                FrozenReviewSource::PureChat { .. } => unreachable!(),
            },
            "project_generation" => match &mut invalid.source {
                FrozenReviewSource::Project {
                    workspace_memory_generation,
                    ..
                } => *workspace_memory_generation += 1,
                FrozenReviewSource::PureChat { .. } => unreachable!(),
            },
            _ => invalid.global_memory_generation += 1,
        }
        rejects(invalid.validate(), ContractCode::InvalidSnapshot);
    }
    assert!(serde_json::from_value::<MemorySnapshotOwner>(json!({
        "owner": "project", "workspace_id": "workspace"
    }))
    .is_err());
}

#[test]
fn snapshot_load_outcomes_are_distinct_tagged_wire_values() {
    roundtrip::<MemorySnapshotLoadOutcome>(r#"{"status":"disabled","reason":"feature_disabled"}"#);
    let ready = MemorySnapshotLoadOutcome::Ready {
        snapshot: snapshot(SnapshotOwner::PureChat, vec![], vec![]),
    };
    assert_eq!(serde_json::to_value(&ready).unwrap()["status"], "ready");
    roundtrip::<MemorySnapshotLoadOutcome>(
        r#"{"status":"unavailable","error_code":"memory_snapshot_unavailable"}"#,
    );
    assert_ne!(serde_json::to_value(ready).unwrap()["status"], "disabled");
}

#[test]
fn child_memory_seed_contains_only_the_snapshot() {
    let seed = FrozenChildMemorySeed {
        snapshot: snapshot(SnapshotOwner::PureChat, vec!["memory".into()], vec![]),
    };
    let wire = serde_json::to_value(seed).unwrap();
    assert_eq!(
        wire.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["snapshot"]
    );
    let text = serde_json::to_string(&wire).unwrap();
    for forbidden in ["containment_id", "decision_id", "proof_id", "injection_id"] {
        assert!(!text.contains(forbidden));
    }
}

fn injection(status: MemoryInjectionStatus) -> MemoryInjectionRecord {
    MemoryInjectionRecord {
        run_id: "run".into(),
        status,
        snapshot_hash: "hash".into(),
        global_entry_refs: vec![entry_ref()],
        project_entry_refs: vec![],
        global_chars: 10,
        project_chars: 0,
        created_at: at(1),
    }
}

fn entry_ref() -> MemoryEntryRef {
    MemoryEntryRef {
        entry_id: "g".into(),
        version: 1,
    }
}

#[test]
fn injection_validation_and_status_wire_are_stable() {
    for (status, wire_name) in [
        (MemoryInjectionStatus::Recorded, "recorded"),
        (
            MemoryInjectionStatus::AbortedBeforePublish,
            "aborted_before_publish",
        ),
    ] {
        let record = injection(status);
        record.validate().unwrap();
        let wire = serde_json::to_value(&record).unwrap();
        assert_eq!(wire["status"], wire_name);
        assert_eq!(
            serde_json::from_value::<MemoryInjectionRecord>(wire).unwrap(),
            record
        );
    }
    let mut invalid = injection(MemoryInjectionStatus::Recorded);
    invalid.run_id = " ".into();
    rejects(invalid.validate(), ContractCode::InvalidInjection);
    invalid = injection(MemoryInjectionStatus::Recorded);
    invalid.global_chars = 4_001;
    rejects(invalid.validate(), ContractCode::InvalidInjection);
    invalid = injection(MemoryInjectionStatus::Recorded);
    invalid.global_entry_refs = vec![entry_ref(); 33];
    rejects(invalid.validate(), ContractCode::InvalidInjection);
}

fn conf(
    mutation: SMut,
    owner: MemoryOwner,
    target: Option<&str>,
    version: Option<u64>,
) -> SensitiveMemoryConfirmation {
    SensitiveMemoryConfirmation {
        disclosure: SensitiveMemoryDisclosure::MainAndReviewerProvidersV1,
        mutation,
        content_hash: "hash".into(),
        owner,
        target_entry_id: target.map(str::to_owned),
        expected_version: version,
    }
}

fn global_owner(authorization: Auth) -> MemoryOwner {
    MemoryOwner::Global { authorization }
}

fn accepts(confirmation: SensitiveMemoryConfirmation) {
    confirmation.validate().unwrap();
}

#[test]
fn sensitive_confirmation_matches_mutation_owner_target_and_version() {
    let manual = global_owner(Auth::Manual);
    let approved = global_owner(Auth::ApprovedCandidate);
    accepts(conf(SMut::Add, manual.clone(), None, None));
    accepts(conf(SMut::Edit, manual.clone(), Some("entry"), Some(2)));
    accepts(conf(
        SMut::ApproveCandidate,
        approved.clone(),
        Some("entry"),
        Some(2),
    ));
    for invalid in [
        conf(SMut::Add, manual.clone(), Some("entry"), None),
        conf(SMut::Edit, manual.clone(), Some("entry"), None),
        conf(SMut::Edit, manual.clone(), None, Some(2)),
        conf(SMut::ApproveCandidate, manual, Some("entry"), Some(2)),
        conf(SMut::ApproveCandidate, approved, Some("entry"), None),
    ] {
        rejects(invalid.validate(), ContractCode::InvalidConfirmation);
    }
    let mut empty_hash = conf(SMut::Add, global_owner(Auth::Manual), None, None);
    empty_hash.content_hash.clear();
    rejects(empty_hash.validate(), ContractCode::InvalidConfirmation);
}

#[test]
fn review_input_wire_has_no_sensitive_confirmation() {
    let input: MemoryReviewInput = serde_json::from_str(r#"{"schema_version":1,"context":"pure_chat","turns":[],"tool_counts":[],"global_entries":[],"project_entries":[],"scope_usage":{"global_chars":0,"project_chars":0},"scope_caps":{"global_chars":4000,"project_chars":8000,"entry_chars":1000,"max_entries":32,"max_proposals":8}}"#).unwrap();
    let wire = serde_json::to_value(input).unwrap();
    assert!(!serde_json::to_string(&wire)
        .unwrap()
        .contains("confirmation"));
}

fn settings_record() -> MemoryReviewSettings {
    MemoryReviewSettings {
        enabled: true,
        reviewer: Some(ReviewerSelection {
            provider_name: "provider".into(),
            model: "model".into(),
        }),
        trigger_every_turns: 10,
        explicit_remember_immediate: true,
        project_notification_mode: ProjectNotificationMode::On,
        version: 3,
        review_generation: 4,
        retention_time_high_watermark: at(10),
        physical_cleanup_pending: false,
        physical_cleanup_epoch: 7,
        updated_at: at(10),
    }
}

fn rejects_settings(current: &MemoryReviewSettings, next: &MemoryReviewSettings) {
    rejects(
        current.validate_next(next),
        ContractCode::InvalidSettingsTransition,
    );
}

#[test]
fn settings_transition_enforces_monotonic_fields_and_generation_coupling() {
    let current = settings_record();
    current.validate_next(&current).unwrap();
    for field in ["version", "generation", "watermark", "epoch", "updated"] {
        let mut next = current.clone();
        match field {
            "version" => next.version -= 1,
            "generation" => next.review_generation -= 1,
            "watermark" => next.retention_time_high_watermark = at(9),
            "epoch" => next.physical_cleanup_epoch -= 1,
            _ => next.updated_at = at(9),
        }
        rejects_settings(&current, &next);
    }

    let mut visible_change = current.clone();
    visible_change.trigger_every_turns = 11;
    rejects_settings(&current, &visible_change);
    visible_change.version += 1;
    current.validate_next(&visible_change).unwrap();

    let mut reviewer_change = current.clone();
    reviewer_change.reviewer.as_mut().unwrap().model = "next-model".into();
    reviewer_change.version += 1;
    rejects_settings(&current, &reviewer_change);
    reviewer_change.review_generation += 1;
    current.validate_next(&reviewer_change).unwrap();
}

#[test]
fn settings_transition_enforces_cleanup_state_machine() {
    let current = settings_record();
    let mut scheduled = current.clone();
    scheduled.physical_cleanup_pending = true;
    scheduled.physical_cleanup_epoch += 1;
    current.validate_next(&scheduled).unwrap();

    for (pending, epoch) in [(true, 7), (false, 8)] {
        let mut invalid = current.clone();
        invalid.physical_cleanup_pending = pending;
        invalid.physical_cleanup_epoch = epoch;
        rejects_settings(&current, &invalid);
    }
    let mut cleared = scheduled.clone();
    cleared.physical_cleanup_pending = false;
    scheduled.validate_next(&cleared).unwrap();
    cleared.physical_cleanup_epoch += 1;
    rejects_settings(&scheduled, &cleared);
}

#[test]
fn owner_source_and_outcome_reject_empty_or_cross_route_identities() {
    global_owner(Auth::Manual).validate().unwrap();
    rejects(
        MemoryOwner::Project {
            workspace_id: " \t".into(),
            origin: ProjectMemoryOrigin::Manual,
        }
        .validate(),
        ContractCode::InvalidIdentity,
    );
    for wire in [
        r#"{"context":"pure_chat","run_id":" ","task_id":"task","branch_id":"branch"}"#,
        r#"{"context":"pure_chat","run_id":"run","task_id":"","branch_id":"branch"}"#,
        r#"{"context":"pure_chat","run_id":"run","task_id":"task","branch_id":"\n"}"#,
        r#"{"context":"project","run_id":"run","task_id":"task","branch_id":"branch","workspace_id":" ","workspace_memory_generation":1}"#,
    ] {
        let source: FrozenReviewSource = serde_json::from_str(wire).unwrap();
        rejects(source.validate(), ContractCode::InvalidIdentity);
    }

    let base = MemoryReviewOutcome {
        sequence: cursor(1),
        job_id: "job".into(),
        proposal_index: 7,
        effect: MemoryReviewOutcomeEffect::GlobalCandidate {
            result: GlobalCandidateOutcome::Pending,
        },
        entry_id: None,
        candidate_id: Some("candidate".into()),
        error_code: None,
        created_at: at(1),
    };
    base.validate().unwrap();
    let mut project = base.clone();
    project.effect = MemoryReviewOutcomeEffect::ProjectEntry {
        result: ProjectEntryOutcome::Applied,
    };
    project.entry_id = Some("entry".into());
    project.candidate_id = None;
    project.validate().unwrap();
    let mut skipped = base.clone();
    skipped.effect = MemoryReviewOutcomeEffect::Skipped {
        result: SkippedOutcome::Noop,
    };
    skipped.candidate_id = None;
    skipped.validate().unwrap();
    let mut invalid = base.clone();
    invalid.entry_id = Some("entry".into());
    rejects(invalid.validate(), ContractCode::InvalidReviewOutcome);
    invalid = project;
    invalid.candidate_id = Some("candidate".into());
    rejects(invalid.validate(), ContractCode::InvalidReviewOutcome);
    invalid = skipped;
    invalid.entry_id = Some("entry".into());
    rejects(invalid.validate(), ContractCode::InvalidReviewOutcome);
    for mutate in [
        |value: &mut MemoryReviewOutcome| value.job_id = " ".into(),
        |value: &mut MemoryReviewOutcome| value.candidate_id = Some("\t".into()),
        |value: &mut MemoryReviewOutcome| value.proposal_index = 8,
    ] {
        invalid = base.clone();
        mutate(&mut invalid);
        rejects(invalid.validate(), ContractCode::InvalidReviewOutcome);
    }
}

fn ordinal(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

fn provider_input(context: MemoryReviewContext) -> MemoryReviewInput {
    let mut input: MemoryReviewInput = serde_json::from_str(r#"{"schema_version":1,"context":"pure_chat","turns":[{"evidence_ordinal":1,"user_text":"u1","assistant_text":"a1"},{"evidence_ordinal":2,"user_text":"u2","assistant_text":"a2"}],"tool_counts":[{"tool_name":"shell","success_count":1,"failure_count":0}],"global_entries":[{"memory_ordinal":1,"kind":"preference","content":"global","version":3}],"project_entries":[],"scope_usage":{"global_chars":6,"project_chars":0},"scope_caps":{"global_chars":4000,"project_chars":8000,"entry_chars":1000,"max_entries":32,"max_proposals":8}}"#).unwrap();
    input.context = context;
    if context == MemoryReviewContext::CurrentProject {
        input.project_entries.push(MemoryReviewWireEntry {
            memory_ordinal: ordinal(2),
            kind: MemoryKind::Constraint,
            content: "project".into(),
            version: 4,
        });
        input.scope_usage.project_chars = 7;
    }
    input
}

fn rejects_input(input: &MemoryReviewInput, code: InputCode) {
    assert_eq!(input.validate().unwrap_err().code, code);
}

fn mutated_input(
    base: &MemoryReviewInput,
    code: InputCode,
    mutate: impl FnOnce(&mut MemoryReviewInput),
) {
    let mut invalid = base.clone();
    mutate(&mut invalid);
    rejects_input(&invalid, code);
}

#[test]
fn provider_input_serde_is_closed_and_contexts_are_exact() {
    for context in [
        MemoryReviewContext::PureChat,
        MemoryReviewContext::CurrentProject,
    ] {
        provider_input(context).validate().unwrap();
    }
    let base = serde_json::to_value(provider_input(MemoryReviewContext::CurrentProject)).unwrap();
    for location in ["root", "turn", "tool", "entry", "usage", "caps"] {
        let mut invalid = base.clone();
        match location {
            "root" => invalid["unknown"] = json!(true),
            "turn" => invalid["turns"][0]["unknown"] = json!(true),
            "tool" => invalid["tool_counts"][0]["unknown"] = json!(true),
            "entry" => invalid["global_entries"][0]["unknown"] = json!(true),
            "usage" => invalid["scope_usage"]["unknown"] = json!(true),
            _ => invalid["scope_caps"]["unknown"] = json!(true),
        }
        assert!(serde_json::from_value::<MemoryReviewInput>(invalid).is_err());
    }
    for (field, value) in [("context", "project"), ("kind", "unknown_kind")] {
        let mut invalid = base.clone();
        if field == "context" {
            invalid[field] = json!(value);
        } else {
            invalid["global_entries"][0][field] = json!(value);
        }
        assert!(serde_json::from_value::<MemoryReviewInput>(invalid).is_err());
    }
    let mut pure_with_project = provider_input(MemoryReviewContext::CurrentProject);
    pure_with_project.context = MemoryReviewContext::PureChat;
    rejects_input(&pure_with_project, InputCode::ProjectDataInPureChat);
}

#[test]
fn provider_input_validates_caps_usage_ordinals_names_and_capacity() {
    let base = provider_input(MemoryReviewContext::CurrentProject);
    mutated_input(&base, InputCode::UnexpectedScopeCaps, |v| {
        v.scope_caps.entry_chars -= 1
    });
    mutated_input(&base, InputCode::ScopeUsageMismatch, |v| {
        v.scope_usage.global_chars += 1
    });
    for (ordinal_value, code) in [
        (1, InputCode::DuplicateEvidenceOrdinal),
        (3, InputCode::NonContiguousEvidenceOrdinal),
    ] {
        mutated_input(&base, code, |v| {
            v.turns[1].evidence_ordinal = ordinal(ordinal_value)
        });
    }
    for (ordinal_value, code) in [
        (1, InputCode::DuplicateMemoryOrdinal),
        (3, InputCode::NonContiguousMemoryOrdinal),
    ] {
        mutated_input(&base, code, |v| {
            v.project_entries[0].memory_ordinal = ordinal(ordinal_value)
        });
    }
    for name in ["", " shell", "shell "] {
        mutated_input(&base, InputCode::InvalidToolName, |v| {
            v.tool_counts[0].tool_name = name.into()
        });
    }
    for content in [String::new(), "x".repeat(1_001)] {
        mutated_input(&base, InputCode::InvalidEntryContent, |v| {
            v.global_entries[0].content = content;
            v.scope_usage.global_chars = v.global_entries[0].content.chars().count() as u32;
        });
    }
    for count in [5_u32, 33] {
        let mut invalid = provider_input(MemoryReviewContext::PureChat);
        invalid.global_entries = (1..=count)
            .map(|i| MemoryReviewWireEntry {
                memory_ordinal: ordinal(i),
                kind: MemoryKind::Preference,
                content: if count == 5 {
                    "x".repeat(1_000)
                } else {
                    "x".into()
                },
                version: 1,
            })
            .collect();
        invalid.scope_usage.global_chars = invalid
            .global_entries
            .iter()
            .map(|entry| entry.content.chars().count() as u32)
            .sum();
        rejects_input(&invalid, InputCode::ScopeCapacityExceeded);
    }
}

fn proposal(
    scope: MemoryProposalScope,
    operation: MemoryProposalOperation,
) -> MemoryReviewProposal {
    MemoryReviewProposal {
        scope,
        kind: MemoryKind::Preference,
        operation,
        target_memory_ordinal: None,
        target_version: None,
        content: (operation == MemoryProposalOperation::Add).then(|| "valid content".into()),
        reason: "reason".into(),
        basis: MemoryProposalBasis::VerifiedResult,
        evidence_ordinals: vec![ordinal(1)],
        confidence: 0.75,
    }
}

fn proposal_code(value: MemoryReviewProposal, input: &MemoryReviewInput) -> ProposalCode {
    MemoryReviewOutput {
        proposals: vec![value],
    }
    .validate(input)
    .unwrap()[0]
        .unwrap_err()
        .code
}

fn replace_proposal(scope: MemoryProposalScope, target: u32, version: u64) -> MemoryReviewProposal {
    let mut value = proposal(scope, MemoryProposalOperation::Replace);
    value.target_memory_ordinal = Some(ordinal(target));
    value.target_version = Some(version);
    value.content = Some("replacement".into());
    value
}

fn mutated_proposal(
    input: &MemoryReviewInput,
    code: ProposalCode,
    mutate: impl FnOnce(&mut MemoryReviewProposal),
) {
    let mut value = proposal(MemoryProposalScope::Global, MemoryProposalOperation::Add);
    mutate(&mut value);
    assert_eq!(proposal_code(value, input), code);
}

#[test]
fn provider_output_accepts_add_replace_noop_and_skip() {
    let input = provider_input(MemoryReviewContext::CurrentProject);
    let add = proposal(MemoryProposalScope::Global, MemoryProposalOperation::Add);
    let replace = replace_proposal(MemoryProposalScope::Project, 2, 4);
    let noop = proposal(MemoryProposalScope::Global, MemoryProposalOperation::Noop);
    let skip = proposal(MemoryProposalScope::Skip, MemoryProposalOperation::Noop);
    let results = MemoryReviewOutput {
        proposals: vec![add, replace, noop, skip],
    }
    .validate(&input)
    .unwrap();
    assert!(results.iter().all(|result| result.is_ok()));
}

#[test]
fn provider_output_reports_batch_shape_and_field_errors() {
    let input = provider_input(MemoryReviewContext::CurrentProject);
    let add = proposal(MemoryProposalScope::Global, MemoryProposalOperation::Add);
    let batch = MemoryReviewOutput {
        proposals: vec![add.clone(); 9],
    }
    .validate(&input)
    .unwrap_err();
    assert_eq!(
        (batch.proposal_index, batch.code),
        (0, ProposalCode::TooManyProposals)
    );

    mutated_proposal(&input, ProposalCode::InvalidContent, |v| {
        v.content = Some("1234".into())
    });
    mutated_proposal(&input, ProposalCode::InvalidReason, |v| {
        v.reason = "r".repeat(301)
    });
    mutated_proposal(&input, ProposalCode::InvalidConfidence, |v| {
        v.confidence = f64::NAN
    });
    mutated_proposal(&input, ProposalCode::MissingEvidence, |v| {
        v.evidence_ordinals.clear()
    });
    mutated_proposal(&input, ProposalCode::InvalidEvidenceOrdinal, |v| {
        v.evidence_ordinals = vec![ordinal(3)]
    });
    mutated_proposal(&input, ProposalCode::DuplicateEvidenceOrdinal, |v| {
        v.evidence_ordinals.push(ordinal(1))
    });
    for (scope, target, version, code) in [
        (
            MemoryProposalScope::Global,
            3,
            1,
            ProposalCode::InvalidTargetOrdinal,
        ),
        (
            MemoryProposalScope::Project,
            1,
            3,
            ProposalCode::TargetScopeMismatch,
        ),
        (
            MemoryProposalScope::Global,
            1,
            99,
            ProposalCode::TargetVersionMismatch,
        ),
    ] {
        assert_eq!(
            proposal_code(replace_proposal(scope, target, version), &input),
            code
        );
    }
    assert_eq!(
        proposal_code(
            proposal(MemoryProposalScope::Skip, MemoryProposalOperation::Add),
            &input,
        ),
        ProposalCode::InvalidOperationShape
    );
    assert_eq!(
        proposal_code(
            proposal(MemoryProposalScope::Project, MemoryProposalOperation::Add),
            &provider_input(MemoryReviewContext::PureChat),
        ),
        ProposalCode::ProjectScopeInPureChat
    );
}

#[test]
fn provider_output_validation_is_per_item_and_errors_are_metadata_only() {
    let input = provider_input(MemoryReviewContext::CurrentProject);
    let valid = proposal(MemoryProposalScope::Global, MemoryProposalOperation::Add);
    let mut invalid = valid.clone();
    invalid.content = Some("bad".into());
    let results = MemoryReviewOutput {
        proposals: vec![valid.clone(), invalid, valid],
    }
    .validate(&input)
    .unwrap();
    assert!(results[0].is_ok());
    let error = results[1].unwrap_err();
    assert_eq!(
        (error.proposal_index, error.code),
        (1, ProposalCode::InvalidContent)
    );
    assert!(results[2].is_ok());
    assert_eq!(
        serde_json::to_value(error).unwrap(),
        json!({"proposal_index": 1, "code": "invalid_content"})
    );
}

fn project_assembly() -> HostReviewAssembly {
    let source = FrozenReviewSource::Project {
        run_id: "HOST_RUN_SENTINEL/HOST_PATH_SENTINEL".into(),
        task_id: "HOST_TASK_SENTINEL/HOST_PROVIDER_SENTINEL".into(),
        branch_id: "HOST_BRANCH_SENTINEL/HOST_PENDING_SENTINEL".into(),
        workspace_id: "HOST_WORKSPACE_SENTINEL/HOST_GENERATION_SENTINEL".into(),
        workspace_memory_generation: 77,
    };
    HostReviewAssembly {
        source,
        inclusive_boundary: cursor(20),
        evidence_ordinal_to_turn_sequence: BTreeMap::from([
            (ordinal(1), cursor(10)),
            (ordinal(2), cursor(20)),
        ]),
        memory_ordinal_to_target: BTreeMap::from([
            (
                ordinal(1),
                host_target("global", global_owner(Auth::ApprovedCandidate), 3),
            ),
            (
                ordinal(2),
                host_target(
                    "project",
                    MemoryOwner::Project {
                        workspace_id: "HOST_WORKSPACE_SENTINEL/HOST_GENERATION_SENTINEL".into(),
                        origin: ProjectMemoryOrigin::AutomaticReview,
                    },
                    4,
                ),
            ),
        ]),
        wire: provider_input(MemoryReviewContext::CurrentProject),
    }
}

fn host_target(label: &str, owner: MemoryOwner, version: u64) -> HostMemoryTarget {
    HostMemoryTarget {
        entry_id: format!("HOST_ENTRY_SENTINEL/HOST_REVISION_SENTINEL/{label}"),
        owner,
        version,
    }
}

fn rejects_assembly(assembly: &HostReviewAssembly, code: InputCode) {
    match assembly.validate_maps() {
        Err(error) => assert_eq!(error.code, code),
        Ok(()) => panic!("assembly unexpectedly validated"),
    }
}

fn mutated_assembly(code: InputCode, mutate: impl FnOnce(&mut HostReviewAssembly)) {
    let mut invalid = project_assembly();
    mutate(&mut invalid);
    rejects_assembly(&invalid, code);
}

#[test]
fn provider_wire_omits_every_host_sentinel() {
    let assembly = project_assembly();
    assembly.validate_maps().unwrap();
    let wire = serde_json::to_string(&assembly.wire).unwrap();
    for sentinel in [
        "HOST_RUN_SENTINEL",
        "HOST_TASK_SENTINEL",
        "HOST_BRANCH_SENTINEL",
        "HOST_WORKSPACE_SENTINEL",
        "HOST_ENTRY_SENTINEL",
        "HOST_PATH_SENTINEL",
        "HOST_PROVIDER_SENTINEL",
        "HOST_GENERATION_SENTINEL",
        "HOST_PENDING_SENTINEL",
        "HOST_REVISION_SENTINEL",
    ] {
        assert!(!wire.contains(sentinel), "provider wire leaked {sentinel}");
    }
}

#[test]
fn assembly_requires_exact_ordinal_maps_versions_context_and_workspace() {
    mutated_assembly(InputCode::OrdinalMapMismatch, |a| {
        a.evidence_ordinal_to_turn_sequence.remove(&ordinal(2));
    });
    mutated_assembly(InputCode::OrdinalMapMismatch, |a| {
        a.memory_ordinal_to_target.insert(
            ordinal(3),
            host_target("extra", global_owner(Auth::Manual), 1),
        );
    });
    mutated_assembly(InputCode::TargetVersionMismatch, |a| {
        a.memory_ordinal_to_target
            .get_mut(&ordinal(1))
            .unwrap()
            .version = 99;
    });
    mutated_assembly(InputCode::SourceOwnerMismatch, |a| {
        a.source = FrozenReviewSource::PureChat {
            run_id: "run".into(),
            task_id: "task".into(),
            branch_id: "branch".into(),
        };
    });
    mutated_assembly(InputCode::SourceOwnerMismatch, |a| {
        a.memory_ordinal_to_target
            .get_mut(&ordinal(2))
            .unwrap()
            .owner = MemoryOwner::Project {
            workspace_id: "other".into(),
            origin: ProjectMemoryOrigin::AutomaticReview,
        };
    });
}

#[test]
fn assembly_rejects_duplicate_or_future_sequences_and_blank_entry_ids() {
    for (sequence, code) in [
        (10, InputCode::DuplicateTurnSequence),
        (21, InputCode::TurnSequenceBeyondBoundary),
    ] {
        mutated_assembly(code, |a| {
            a.evidence_ordinal_to_turn_sequence
                .insert(ordinal(2), cursor(sequence));
        });
    }
    mutated_assembly(InputCode::SourceOwnerMismatch, |a| {
        a.memory_ordinal_to_target
            .get_mut(&ordinal(1))
            .unwrap()
            .entry_id = " \n".into();
    });
}
