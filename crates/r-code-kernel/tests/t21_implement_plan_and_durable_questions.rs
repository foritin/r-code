//! T21 — Plan and durable questions.
//!
//! Contract tests cover dependency order, stale updates, crash-after-answer
//! (resume once) and missing acceptance evidence, both in-memory and
//! through the v2 store.

use r_code_harness_protocol::services::{QuestionsAskRequest, WorkUnitWire};
use r_code_harness_protocol::OperationKey;
use r_code_kernel::plans::*;
use r_code_kernel::questions::*;
use r_code_kernel::task::WorkUnitUpdate;

fn units() -> Vec<WorkUnitWire> {
    vec![
        WorkUnitWire {
            id: "u1".into(),
            description: "first".into(),
            dependencies: vec![],
            acceptance: vec!["check:a".into()],
        },
        WorkUnitWire {
            id: "u2".into(),
            description: "second".into(),
            dependencies: vec!["u1".into()],
            acceptance: vec![],
        },
    ]
}

fn complete(id: &str) -> WorkUnitUpdate {
    WorkUnitUpdate {
        work_unit_id: id.into(),
        status: r_code_kernel::task::WorkUnitStatus::Completed,
    }
}

#[test]
fn dependency_order_and_evidence_gate_completions() {
    let mut plan = PlanState::new();
    let revision = plan.publish(units());
    assert_eq!(revision, 1);

    // u2 first: dependency not completed.
    assert!(matches!(
        plan.update(revision, complete("u2"), None, |_| true, true),
        Err(PlanError::DependencyNotCompleted(dep)) if dep == "u1"
    ));

    // u1 carries acceptance: no evidence → refused.
    assert!(matches!(
        plan.update(revision, complete("u1"), None, |_| false, true),
        Err(PlanError::EvidenceRequired(id)) if id == "u1"
    ));

    // With evidence, u1 completes; then u2.
    plan.update(revision, complete("u1"), None, |_| true, true)
        .expect("u1");
    plan.update(revision, complete("u2"), None, |_| true, true)
        .expect("u2");
    assert!(plan.all_completed());

    // Unknown units fail.
    assert!(matches!(
        plan.update(revision, complete("ghost"), None, |_| true, true),
        Err(PlanError::UnknownWorkUnit(_))
    ));
}

#[test]
fn stale_updates_are_refused_and_no_op_duplicates_replay() {
    let mut plan = PlanState::new();
    let revision = plan.publish(units());

    // A revision from the future (or past) is stale.
    assert!(matches!(
        plan.update(revision + 1, complete("u1"), None, |_| true, true),
        Err(PlanError::StaleRevision {
            expected: 1,
            provided: 2
        })
    ));

    // Same operation key + same update: replays without re-applying.
    let key = OperationKey::new("complete-u1");
    assert_eq!(
        plan.update(revision, complete("u1"), Some(key.clone()), |_| true, true)
            .unwrap(),
        UpdateOutcome::Applied
    );
    // Flip the status back manually to prove the replay changes nothing.
    if let Some(unit) = plan.work_units.iter_mut().find(|unit| unit.id == "u1") {
        unit.status = r_code_kernel::task::WorkUnitStatus::Pending;
    }
    assert_eq!(
        plan.update(revision, complete("u1"), Some(key), |_| true, true)
            .unwrap(),
        UpdateOutcome::Replayed
    );
    if let Some(unit) = plan.work_units.iter().find(|unit| unit.id == "u1") {
        assert_eq!(
            unit.status,
            r_code_kernel::task::WorkUnitStatus::Pending,
            "replay did not re-apply"
        );
    }

    // Same key, different update: refused.
    let mut plan = PlanState::new();
    let revision = plan.publish(units());
    let key = OperationKey::new("mixed");
    plan.update(revision, complete("u1"), Some(key.clone()), |_| true, true)
        .unwrap();
    let conflicting = WorkUnitUpdate {
        work_unit_id: "u2".into(),
        status: r_code_kernel::task::WorkUnitStatus::Completed,
    };
    assert_eq!(
        plan.update(revision, conflicting, Some(key), |_| true, true)
            .unwrap(),
        UpdateOutcome::ConflictingKey
    );
}

#[test]
fn questions_persist_answer_once_and_replay_continuations() {
    let mut board = QuestionBoard::new();
    let reply = board.ask(
        "task-1",
        "run-1",
        &QuestionsAskRequest {
            text: "which database?".into(),
            options: vec!["sqlite".into(), "postgres".into()],
            blocking: true,
        },
    );
    let question_id = reply.question_id;
    assert!(board.open_blocking("task-1").is_some());

    // Answering resumes exactly once; a repeated answer (same op key) is a
    // replay even after a simulated crash-restart of the answer flow.
    let key = OperationKey::new("answer-1");
    assert_eq!(
        board
            .answer(&question_id, "sqlite", Some(key.clone()))
            .unwrap(),
        AnswerOutcome::Resumed
    );
    assert_eq!(
        board.answer(&question_id, "sqlite", Some(key)).unwrap(),
        AnswerOutcome::Replayed
    );
    // Without a key, an answered question refuses re-answering.
    assert!(matches!(
        board.answer(&question_id, "postgres", None),
        Err(QuestionError::AlreadyAnswered(_))
    ));
    assert_eq!(
        board.get(&question_id).unwrap().answer.as_deref(),
        Some("sqlite")
    );

    // Expired questions cannot be answered.
    let other = board.ask(
        "task-1",
        "run-1",
        &QuestionsAskRequest {
            text: "another?".into(),
            options: vec![],
            blocking: false,
        },
    );
    board.expire(&other.question_id).unwrap();
    assert!(matches!(
        board.answer(&other.question_id, "yes", None),
        Err(QuestionError::Expired(_))
    ));
    // Unknown questions fail closed.
    assert!(matches!(
        board.answer("q-404", "x", None),
        Err(QuestionError::Unknown(_))
    ));
}

#[test]
fn plans_and_questions_round_trip_through_the_v2_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = r_code_store::v2::V2Store::open(&temp.path().join("tasks.sqlite3")).expect("open");

    // Plan snapshot persistence.
    store
        .save_plan("task-1", 3, "{\"units\":2}")
        .expect("save plan");
    let (revision, json) = store
        .load_plan("task-1")
        .expect("load plan")
        .expect("present");
    assert_eq!(revision, 3);
    assert!(json.contains("\"units\":2"));
    assert!(store.load_plan("task-2").expect("empty").is_none());

    // Question persistence: open → answered exactly once.
    store
        .save_question(
            "q-1",
            "task-1",
            "run-1",
            "which db?",
            &["sqlite".to_string()],
            true,
        )
        .expect("save question");
    assert_eq!(
        store
            .open_blocking_question("task-1")
            .expect("open")
            .as_deref(),
        Some("q-1")
    );
    assert!(store.answer_question("q-1", "sqlite").expect("answer"));
    // The second answer does not re-answer.
    assert!(!store.answer_question("q-1", "postgres").expect("second"));
    assert!(store
        .open_blocking_question("task-1")
        .expect("none open")
        .is_none());

    // Expiry path.
    store
        .save_question(
            "q-2",
            "task-1",
            "run-1",
            "another?",
            &Vec::<String>::new(),
            false,
        )
        .expect("q2");
    assert!(store.expire_question("q-2").expect("expire"));
    assert!(!store.expire_question("q-2").expect("already expired"));
}
