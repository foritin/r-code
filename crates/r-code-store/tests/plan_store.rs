use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;

use r_code_core::dto::{AgentEngine, QueuedMessageState, SessionBranch, Task, TaskMode, TaskState};
use r_code_core::plan::{
    AnswerPlanQuestionsInput, ApprovePlanInput, CancelPlanInput, CreatePlanInput,
    PlanContinuationState, PlanImplementationDispatchState, PlanItemDraft, PlanItemState,
    PlanQuestionAnswer, PlanQuestionAnswerInput, PlanQuestionDraft, PlanQuestionOptionDraft,
    PlanQuestionSetState, PlanState, PublishPlanInput, RequestPlanQuestionsInput,
    UpdatePlanItemInput,
};
use r_code_store::{
    Database, PlanStore, QueuedMessageRepository, SessionBranchRepository, TaskRepository,
};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    db: Arc<Database>,
    store: PlanStore,
    task: Task,
}

impl Fixture {
    fn in_memory(goal: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let task = seed_task(db.as_ref(), goal);
        let store = PlanStore::new(Arc::clone(&db), directory.path().join("plans"));
        Self {
            _directory: directory,
            db,
            store,
            task,
        }
    }

    fn create_plan(&self) -> r_code_core::plan::PlanView {
        self.store
            .create_plan(&CreatePlanInput {
                task_id: self.task.id.clone(),
            })
            .unwrap()
    }
}

fn seed_task(db: &Database, goal: &str) -> Task {
    let task = Task::new(None, "Plan store test", goal, TaskMode::Plan);
    TaskRepository::new(db).create(&task).unwrap();
    task
}

fn item(id: &str, title: &str, depends_on: &[&str]) -> PlanItemDraft {
    PlanItemDraft {
        id: id.to_string(),
        title: title.to_string(),
        description: format!("Implement {title}"),
        section_path: vec![],
        depends_on: depends_on
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
    }
}

fn option(id: &str, label: &str) -> PlanQuestionOptionDraft {
    PlanQuestionOptionDraft {
        id: id.to_string(),
        label: label.to_string(),
        description: format!("Choose {label}"),
    }
}

fn question(id: &str, option_count: usize) -> PlanQuestionDraft {
    PlanQuestionDraft {
        id: id.to_string(),
        header: format!("Header {id}"),
        question: format!("What should {id} do?"),
        options: (0..option_count)
            .map(|index| option(&format!("{id}-option-{index}"), &format!("Option {index}")))
            .collect(),
    }
}

fn publish(
    store: &PlanStore,
    task_id: &str,
    plan_id: &str,
    expected_revision: u64,
    items: Vec<PlanItemDraft>,
) -> r_code_core::plan::PlanView {
    store
        .publish_plan(
            task_id,
            &PublishPlanInput {
                plan_id: plan_id.to_string(),
                expected_revision,
                items,
            },
        )
        .unwrap()
}

fn request_questions(
    store: &PlanStore,
    task_id: &str,
    plan_id: &str,
    expected_revision: u64,
    questions: Vec<PlanQuestionDraft>,
) -> r_code_core::plan::PlanView {
    store
        .request_questions(
            task_id,
            &RequestPlanQuestionsInput {
                plan_id: plan_id.to_string(),
                expected_revision,
                questions,
            },
        )
        .unwrap()
}

fn projection_path(view: &r_code_core::plan::PlanView) -> PathBuf {
    PathBuf::from(
        view.plan
            .projection_path
            .as_deref()
            .expect("Plan projection path"),
    )
}

fn assert_error_contains<T>(result: Result<T, r_code_core::error::ProductError>, needle: &str) {
    let error = match result {
        Ok(_) => panic!("expected error containing {needle:?}"),
        Err(error) => error,
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains(needle),
        "expected error containing {needle:?}, got {rendered:?}"
    );
}

#[test]
fn restart_preserves_current_plan_goal_items_and_stable_projection_path() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("r-code.db");
    let projection_root = directory.path().join("plans");

    let db = Arc::new(Database::open(&database_path).unwrap());
    let task = seed_task(db.as_ref(), "Persist this user goal");
    let store = PlanStore::new(Arc::clone(&db), &projection_root);
    let created = store
        .create_plan(&CreatePlanInput {
            task_id: task.id.clone(),
        })
        .unwrap();
    let published = publish(
        &store,
        &task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("feature-1", "Persistent feature", &[])],
    );
    let expected_path = projection_path(&published);
    assert_eq!(
        expected_path,
        projection_root.join(&published.plan.id).join("plan.md")
    );
    assert_eq!(published.plan.projection_revision, Some(2));
    drop(store);
    drop(db);

    let reopened_db = Arc::new(Database::open(&database_path).unwrap());
    let reopened = PlanStore::new(Arc::clone(&reopened_db), &projection_root);
    let current = reopened.current_for_task(&task.id).unwrap().unwrap();
    assert_eq!(current.plan.id, published.plan.id);
    assert_eq!(current.plan.revision, published.plan.revision);
    assert_eq!(current.goal.goal, "Persist this user goal");
    assert_eq!(current.items.len(), 1);
    assert_eq!(current.items[0].id, "feature-1");
    assert_eq!(projection_path(&current), expected_path);
    assert!(expected_path.is_file());
    let markdown = fs::read_to_string(&expected_path).unwrap();
    assert!(markdown.contains("**1 Persistent feature**"));
}

#[test]
fn hierarchy_path_survives_restart_and_projects_numbered_leaf_progress() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("r-code.db");
    let projection_root = directory.path().join("plans");
    let db = Arc::new(Database::open(&database_path).unwrap());
    let task = seed_task(db.as_ref(), "Track hierarchical progress");
    let store = PlanStore::new(Arc::clone(&db), &projection_root);
    let created = store
        .create_plan(&CreatePlanInput {
            task_id: task.id.clone(),
        })
        .unwrap();
    let mut protocol = item("protocol", "Add protocol methods", &[]);
    protocol.section_path = vec!["Backend".to_string(), "Vector adapter".to_string()];
    let published = publish(
        &store,
        &task.id,
        &created.plan.id,
        created.plan.revision,
        vec![protocol],
    );
    drop(store);
    drop(db);

    let reopened_db = Arc::new(Database::open(&database_path).unwrap());
    let reopened = PlanStore::new(Arc::clone(&reopened_db), &projection_root);
    let current = reopened.current_for_task(&task.id).unwrap().unwrap();
    assert_eq!(
        current.items[0].section_path,
        vec!["Backend".to_string(), "Vector adapter".to_string()]
    );
    let markdown = fs::read_to_string(projection_path(&published)).unwrap();
    assert!(markdown.contains("### 1 Backend"));
    assert!(markdown.contains("#### 1.1 Vector adapter"));
    assert!(markdown.contains("**1.1.1 Add protocol methods**"));
}

#[test]
fn noncontiguous_sections_preserve_the_execution_ordinal_in_projection() {
    let directory = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(directory.path().join("r-code.db")).unwrap());
    let task = seed_task(db.as_ref(), "Preserve section order");
    let store = PlanStore::new(Arc::clone(&db), directory.path().join("plans"));
    let created = store
        .create_plan(&CreatePlanInput {
            task_id: task.id.clone(),
        })
        .unwrap();
    let mut first = item("first", "First backend item", &[]);
    first.section_path = vec!["Backend".to_string()];
    let root = item("root", "Root item", &[]);
    let mut last = item("last", "Last backend item", &["root"]);
    last.section_path = vec!["Backend".to_string()];
    let published = publish(
        &store,
        &task.id,
        &created.plan.id,
        created.plan.revision,
        vec![first, root, last],
    );

    assert_eq!(
        published
            .items
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "root", "last"]
    );
    let markdown = fs::read_to_string(projection_path(&published)).unwrap();
    let first_position = markdown.find("First backend item").unwrap();
    let root_position = markdown.find("Root item").unwrap();
    let last_position = markdown.find("Last backend item").unwrap();
    assert!(first_position < root_position && root_position < last_position);
    assert!(markdown.contains("### 1 Backend"));
    assert!(markdown.contains("**2 Root item**"));
    assert!(markdown.contains("### 3 Backend"));
}

#[test]
fn concurrent_stale_publish_has_one_winner_and_never_overwrites_new_projection() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("r-code.db");
    let projection_root = directory.path().join("plans");
    let db = Arc::new(Database::open(&database_path).unwrap());
    let task = seed_task(db.as_ref(), "Race publication");
    let store = Arc::new(PlanStore::new(Arc::clone(&db), &projection_root));
    let created = store
        .create_plan(&CreatePlanInput {
            task_id: task.id.clone(),
        })
        .unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for title in ["Red winner", "Blue winner"] {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let task_id = task.id.clone();
        let plan_id = created.plan.id.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let result = store.publish_plan(
                &task_id,
                &PublishPlanInput {
                    plan_id,
                    expected_revision: 1,
                    items: vec![item("feature-1", title, &[])],
                },
            );
            (title, result)
        }));
    }
    barrier.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_err()).count(),
        1
    );
    let winner = results
        .iter()
        .find_map(|(title, result)| result.as_ref().ok().map(|_| *title))
        .unwrap();
    let loser_error = results
        .iter()
        .find_map(|(_, result)| result.as_ref().err())
        .unwrap()
        .to_string();
    assert!(loser_error.contains("stale Plan revision"));

    let current = store.current_for_task(&task.id).unwrap().unwrap();
    assert_eq!(current.plan.revision, 2);
    assert_eq!(current.plan.projection_revision, Some(2));
    assert_eq!(current.items[0].title, winner);
    let markdown = fs::read_to_string(projection_path(&current)).unwrap();
    assert!(markdown.contains(winner));
    let loser = if winner == "Red winner" {
        "Blue winner"
    } else {
        "Red winner"
    };
    assert!(!markdown.contains(loser));
}

#[test]
fn question_and_option_cardinality_is_enforced_at_exact_boundaries() {
    let fixture = Fixture::in_memory("Question bounds");
    let created = fixture.create_plan();
    assert_error_contains(
        fixture.store.request_questions(
            &fixture.task.id,
            &RequestPlanQuestionsInput {
                plan_id: created.plan.id.clone(),
                expected_revision: 1,
                questions: vec![],
            },
        ),
        "1-3 questions",
    );
    assert_error_contains(
        fixture.store.request_questions(
            &fixture.task.id,
            &RequestPlanQuestionsInput {
                plan_id: created.plan.id.clone(),
                expected_revision: 1,
                questions: (0..4)
                    .map(|index| question(&format!("too-many-{index}"), 2))
                    .collect(),
            },
        ),
        "1-3 questions",
    );
    assert_error_contains(
        fixture.store.request_questions(
            &fixture.task.id,
            &RequestPlanQuestionsInput {
                plan_id: created.plan.id.clone(),
                expected_revision: 1,
                questions: vec![question("too-few-options", 1)],
            },
        ),
        "2-3 options",
    );
    assert_error_contains(
        fixture.store.request_questions(
            &fixture.task.id,
            &RequestPlanQuestionsInput {
                plan_id: created.plan.id.clone(),
                expected_revision: 1,
                questions: vec![question("too-many-options", 4)],
            },
        ),
        "2-3 options",
    );

    let one_fixture = Fixture::in_memory("One question");
    let one_plan = one_fixture.create_plan();
    let one = request_questions(
        &one_fixture.store,
        &one_fixture.task.id,
        &one_plan.plan.id,
        1,
        vec![question("one", 2)],
    );
    assert_eq!(one.pending_question_set.unwrap().questions.len(), 1);

    let three_fixture = Fixture::in_memory("Three questions");
    let three_plan = three_fixture.create_plan();
    let three = request_questions(
        &three_fixture.store,
        &three_fixture.task.id,
        &three_plan.plan.id,
        1,
        (0..3)
            .map(|index| question(&format!("three-{index}"), 3))
            .collect(),
    );
    let questions = three.pending_question_set.unwrap().questions;
    assert_eq!(questions.len(), 3);
    assert!(questions.iter().all(|entry| entry.options.len() == 3));
}

#[test]
fn answers_are_all_or_nothing_and_support_option_plus_free_form() {
    let fixture = Fixture::in_memory("Atomic answers");
    let created = fixture.create_plan();
    let awaiting = request_questions(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        1,
        vec![question("q1", 2), question("q2", 2)],
    );
    let set_id = awaiting.pending_question_set.unwrap().id;

    let partial = AnswerPlanQuestionsInput {
        question_set_id: set_id.clone(),
        expected_revision: 2,
        idempotency_key: "partial-attempt".to_string(),
        skip_all: false,
        answers: vec![PlanQuestionAnswerInput::Option {
            question_id: "q1".to_string(),
            option_id: "q1-option-0".to_string(),
        }],
    };
    assert_error_contains(
        fixture.store.answer_questions(&fixture.task.id, &partial),
        "exactly one response per question",
    );

    let invalid_second = AnswerPlanQuestionsInput {
        question_set_id: set_id.clone(),
        expected_revision: 2,
        idempotency_key: "invalid-option-attempt".to_string(),
        skip_all: false,
        answers: vec![
            PlanQuestionAnswerInput::Option {
                question_id: "q1".to_string(),
                option_id: "q1-option-0".to_string(),
            },
            PlanQuestionAnswerInput::Option {
                question_id: "q2".to_string(),
                option_id: "q1-option-1".to_string(),
            },
        ],
    };
    assert_error_contains(
        fixture
            .store
            .answer_questions(&fixture.task.id, &invalid_second),
        "does not belong to question",
    );
    let still_pending = fixture
        .store
        .get_question_set(&fixture.task.id, &set_id)
        .unwrap();
    assert_eq!(still_pending.state, PlanQuestionSetState::Pending);
    assert!(still_pending
        .questions
        .iter()
        .all(|entry| entry.answer.is_none()));
    let unchanged = fixture
        .store
        .get_plan(&fixture.task.id, &created.plan.id)
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.plan.revision, 2);
    assert_eq!(unchanged.plan.state, PlanState::AwaitingInput);

    let accepted = fixture
        .store
        .answer_questions(
            &fixture.task.id,
            &AnswerPlanQuestionsInput {
                question_set_id: set_id.clone(),
                expected_revision: 2,
                idempotency_key: "valid-answer".to_string(),
                skip_all: false,
                answers: vec![
                    PlanQuestionAnswerInput::Text {
                        question_id: "q2".to_string(),
                        text: "  custom response  ".to_string(),
                    },
                    PlanQuestionAnswerInput::Option {
                        question_id: "q1".to_string(),
                        option_id: "q1-option-1".to_string(),
                    },
                ],
            },
        )
        .unwrap();
    assert_eq!(accepted.plan.revision, 3);
    assert_eq!(accepted.plan.state, PlanState::Draft);
    assert!(accepted.pending_question_set.is_none());
    let answered = fixture
        .store
        .get_question_set(&fixture.task.id, &set_id)
        .unwrap();
    assert_eq!(answered.state, PlanQuestionSetState::Answered);
    assert_eq!(
        answered.questions[0].answer,
        Some(PlanQuestionAnswer::Option {
            option_id: "q1-option-1".to_string()
        })
    );
    assert_eq!(
        answered.questions[1].answer,
        Some(PlanQuestionAnswer::FreeForm {
            text: "custom response".to_string()
        })
    );
}

#[test]
fn answer_retry_is_idempotent_but_key_or_payload_mismatch_is_rejected() {
    let fixture = Fixture::in_memory("Idempotent answer");
    let created = fixture.create_plan();
    let awaiting = request_questions(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        1,
        vec![question("idempotent-q", 2)],
    );
    let set_id = awaiting.pending_question_set.unwrap().id;
    let answer = AnswerPlanQuestionsInput {
        question_set_id: set_id.clone(),
        expected_revision: 2,
        idempotency_key: "answer-key".to_string(),
        skip_all: false,
        answers: vec![PlanQuestionAnswerInput::Option {
            question_id: "idempotent-q".to_string(),
            option_id: "idempotent-q-option-0".to_string(),
        }],
    };
    let first = fixture
        .store
        .answer_questions(&fixture.task.id, &answer)
        .unwrap();
    let retry = fixture
        .store
        .answer_questions(&fixture.task.id, &answer)
        .unwrap();
    assert_eq!(first.plan.revision, 3);
    assert_eq!(retry.plan.revision, 3);

    let changed_payload = AnswerPlanQuestionsInput {
        answers: vec![PlanQuestionAnswerInput::Option {
            question_id: "idempotent-q".to_string(),
            option_id: "idempotent-q-option-1".to_string(),
        }],
        ..answer.clone()
    };
    assert_error_contains(
        fixture
            .store
            .answer_questions(&fixture.task.id, &changed_payload),
        "another payload",
    );
    let changed_key = AnswerPlanQuestionsInput {
        idempotency_key: "different-key".to_string(),
        ..answer
    };
    assert_error_contains(
        fixture
            .store
            .answer_questions(&fixture.task.id, &changed_key),
        "another payload",
    );
}

#[test]
fn whole_set_skip_is_durable_idempotent_and_stores_no_partial_answers() {
    let fixture = Fixture::in_memory("Skip all");
    let created = fixture.create_plan();
    let awaiting = request_questions(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        1,
        vec![question("skip-q1", 2), question("skip-q2", 3)],
    );
    let set_id = awaiting.pending_question_set.unwrap().id;
    let skip = AnswerPlanQuestionsInput {
        question_set_id: set_id.clone(),
        expected_revision: 2,
        idempotency_key: "skip-key".to_string(),
        skip_all: true,
        answers: vec![],
    };
    let first = fixture
        .store
        .answer_questions(&fixture.task.id, &skip)
        .unwrap();
    let retry = fixture
        .store
        .answer_questions(&fixture.task.id, &skip)
        .unwrap();
    assert_eq!(first.plan.revision, 3);
    assert_eq!(retry.plan.revision, 3);
    let skipped = fixture
        .store
        .get_question_set(&fixture.task.id, &set_id)
        .unwrap();
    assert_eq!(skipped.state, PlanQuestionSetState::Skipped);
    assert!(skipped.questions.iter().all(|entry| entry.answer.is_none()));

    let different_key = AnswerPlanQuestionsInput {
        idempotency_key: "another-skip-key".to_string(),
        ..skip
    };
    assert_error_contains(
        fixture
            .store
            .answer_questions(&fixture.task.id, &different_key),
        "another payload",
    );
}

#[test]
fn answer_idempotency_keys_are_scoped_to_their_question_set() {
    let fixture = Fixture::in_memory("First idempotency scope");
    let second_task = seed_task(fixture.db.as_ref(), "Second idempotency scope");
    let first = fixture.create_plan();
    let second = fixture
        .store
        .create_plan(&CreatePlanInput {
            task_id: second_task.id.clone(),
        })
        .unwrap();
    let first_awaiting = request_questions(
        &fixture.store,
        &fixture.task.id,
        &first.plan.id,
        1,
        vec![question("first-q", 2)],
    );
    let second_awaiting = request_questions(
        &fixture.store,
        &second_task.id,
        &second.plan.id,
        1,
        vec![question("second-q", 2)],
    );

    fixture
        .store
        .answer_questions(
            &fixture.task.id,
            &AnswerPlanQuestionsInput {
                question_set_id: first_awaiting.pending_question_set.unwrap().id,
                expected_revision: 2,
                idempotency_key: "client-retry-1".to_string(),
                skip_all: false,
                answers: vec![PlanQuestionAnswerInput::Option {
                    question_id: "first-q".to_string(),
                    option_id: "first-q-option-0".to_string(),
                }],
            },
        )
        .unwrap();
    fixture
        .store
        .answer_questions(
            &second_task.id,
            &AnswerPlanQuestionsInput {
                question_set_id: second_awaiting.pending_question_set.unwrap().id,
                expected_revision: 2,
                idempotency_key: "client-retry-1".to_string(),
                skip_all: false,
                answers: vec![PlanQuestionAnswerInput::Option {
                    question_id: "second-q".to_string(),
                    option_id: "second-q-option-0".to_string(),
                }],
            },
        )
        .expect("an idempotency key only deduplicates retries for its own question set");
}

#[test]
fn continuation_claim_failure_retry_and_completion_are_retry_safe() {
    let fixture = Fixture::in_memory("Continuation lifecycle");
    let created = fixture.create_plan();
    let awaiting = request_questions(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        1,
        vec![question("continue-q", 2)],
    );
    let set_id = awaiting.pending_question_set.unwrap().id;
    fixture
        .store
        .answer_questions(
            &fixture.task.id,
            &AnswerPlanQuestionsInput {
                question_set_id: set_id.clone(),
                expected_revision: 2,
                idempotency_key: "continue-answer".to_string(),
                skip_all: false,
                answers: vec![PlanQuestionAnswerInput::Option {
                    question_id: "continue-q".to_string(),
                    option_id: "continue-q-option-0".to_string(),
                }],
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .get_question_set(&fixture.task.id, &set_id)
            .unwrap()
            .continuation_state,
        PlanContinuationState::Pending
    );

    let claimed = fixture
        .store
        .claim_continuation(&fixture.task.id, &set_id)
        .unwrap()
        .expect("first dispatcher claims the continuation");
    assert_eq!(
        claimed.continuation_state,
        PlanContinuationState::Dispatching
    );
    assert!(fixture
        .store
        .claim_continuation(&fixture.task.id, &set_id)
        .unwrap()
        .is_none());

    let failed = fixture
        .store
        .mark_continuation_failed(&fixture.task.id, &set_id, "network unavailable")
        .unwrap();
    assert_eq!(failed.continuation_state, PlanContinuationState::Failed);
    assert_eq!(
        failed.continuation_error.as_deref(),
        Some("network unavailable")
    );
    let retry = fixture
        .store
        .retry_continuation(&fixture.task.id, &set_id)
        .unwrap();
    assert_eq!(retry.continuation_state, PlanContinuationState::Pending);
    assert_eq!(retry.continuation_error, None);

    fixture
        .store
        .claim_continuation(&fixture.task.id, &set_id)
        .unwrap()
        .expect("retry is claimable");
    let dispatched = fixture
        .store
        .mark_continuation_dispatched(&fixture.task.id, &set_id)
        .unwrap();
    assert_eq!(
        dispatched.continuation_state,
        PlanContinuationState::Dispatched
    );
    assert!(dispatched.dispatched_at.is_some());
    let idempotent = fixture
        .store
        .mark_continuation_dispatched(&fixture.task.id, &set_id)
        .unwrap();
    assert_eq!(idempotent, dispatched);
    assert!(fixture
        .store
        .claim_continuation(&fixture.task.id, &set_id)
        .unwrap()
        .is_none());
}

#[test]
fn approval_is_retry_safe_under_race_and_dependency_cycles_remain_unapproved() {
    let directory = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(directory.path().join("r-code.db")).unwrap());
    let task = seed_task(db.as_ref(), "Approval race");
    let store = Arc::new(PlanStore::new(
        Arc::clone(&db),
        directory.path().join("plans"),
    ));
    let created = store
        .create_plan(&CreatePlanInput {
            task_id: task.id.clone(),
        })
        .unwrap();
    let ready = publish(
        store.as_ref(),
        &task.id,
        &created.plan.id,
        1,
        vec![
            item("first", "First", &[]),
            item("second", "Second", &["first"]),
        ],
    );
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let task_id = task.id.clone();
        let plan_id = ready.plan.id.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            store.approve_plan(
                &task_id,
                &ApprovePlanInput {
                    plan_id,
                    expected_revision: 2,
                },
            )
        }));
    }
    barrier.wait();
    let approved: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap())
        .collect();
    assert!(approved.iter().all(|view| view.plan.revision == 3));
    assert!(approved
        .iter()
        .all(|view| view.plan.approved_revision == Some(2)));
    let current = store.current_for_task(&task.id).unwrap().unwrap();
    assert_eq!(current.plan.state, PlanState::Executing);
    assert_eq!(
        current
            .items
            .iter()
            .filter(|entry| entry.state == PlanItemState::InProgress)
            .count(),
        1
    );
    assert_eq!(current.items[0].state, PlanItemState::InProgress);
    assert_eq!(current.items[1].state, PlanItemState::Pending);

    let cyclic = Fixture::in_memory("Cycle validation");
    let cycle_plan = cyclic.create_plan();
    let ready_cycle = publish(
        &cyclic.store,
        &cyclic.task.id,
        &cycle_plan.plan.id,
        1,
        vec![
            item("cycle-a", "Cycle A", &["cycle-b"]),
            item("cycle-b", "Cycle B", &["cycle-a"]),
        ],
    );
    assert_error_contains(
        cyclic.store.approve_plan(
            &cyclic.task.id,
            &ApprovePlanInput {
                plan_id: ready_cycle.plan.id.clone(),
                expected_revision: 2,
            },
        ),
        "contain a cycle",
    );
    let unchanged = cyclic
        .store
        .get_plan(&cyclic.task.id, &ready_cycle.plan.id)
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.plan.state, PlanState::Ready);
    assert_eq!(unchanged.plan.approved_revision, None);
    assert!(unchanged
        .items
        .iter()
        .all(|entry| entry.state == PlanItemState::Proposed));
}

#[test]
fn approved_implementation_is_staged_once_with_task_mode_in_the_same_transaction() {
    let fixture = Fixture::in_memory("Durable implementation handoff");
    let created = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("feature", "Feature", &[])],
    );
    let approved = fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: ready.plan.revision,
            },
        )
        .unwrap();
    assert_eq!(
        approved.plan.implementation_dispatch_state,
        PlanImplementationDispatchState::Pending
    );
    let branch = SessionBranchRepository::new(&fixture.db)
        .ensure_active(&fixture.task.id)
        .unwrap();
    let claimed = fixture
        .store
        .claim_implementation_dispatch(&fixture.task.id, &approved.plan.id)
        .unwrap()
        .expect("pending handoff is claimable");
    assert_eq!(
        claimed.plan.implementation_dispatch_state,
        PlanImplementationDispatchState::Dispatching
    );
    let staged = fixture
        .store
        .stage_implementation_dispatch(
            &fixture.task.id,
            &approved.plan.id,
            &branch.id,
            "Implement Feature",
        )
        .unwrap();
    assert_eq!(
        staged.plan.implementation_dispatch_state,
        PlanImplementationDispatchState::Dispatched
    );
    let queue_id = format!(
        "plan-implementation:{}:{}",
        approved.plan.id, ready.plan.revision
    );
    assert_eq!(
        staged.plan.implementation_queue_message_id.as_deref(),
        Some(queue_id.as_str())
    );
    assert_eq!(
        TaskRepository::new(&fixture.db)
            .get(&fixture.task.id)
            .unwrap()
            .unwrap()
            .mode,
        TaskMode::Auto
    );
    let pending = QueuedMessageRepository::new(&fixture.db)
        .list_pending(&fixture.task.id, &branch.id)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, queue_id);
    assert_eq!(pending[0].state, QueuedMessageState::Queued);

    let repeated = fixture
        .store
        .stage_implementation_dispatch(
            &fixture.task.id,
            &approved.plan.id,
            &branch.id,
            "Implement Feature",
        )
        .unwrap();
    assert_eq!(
        repeated.plan.implementation_queue_message_id,
        Some(queue_id)
    );
    let count: i64 = fixture
        .db
        .conn()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM queued_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn agent_plan_entry_creates_the_draft_mode_and_continuation_in_one_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open_in_memory().unwrap());
    let task = Task::new(None, "Agent", "Plan before writing", TaskMode::Edit);
    TaskRepository::new(&db).create(&task).unwrap();
    let branch = SessionBranchRepository::new(&db)
        .ensure_active(&task.id)
        .unwrap();
    let store = PlanStore::new(db.clone(), directory.path().join("plans"));

    let entered = store
        .enter_plan_mode_and_stage_continuation(
            &task.id,
            &branch.id,
            "Continue the same request in Plan mode",
        )
        .unwrap();

    assert_eq!(entered.plan.state, PlanState::Draft);
    assert_eq!(
        TaskRepository::new(&db)
            .get(&task.id)
            .unwrap()
            .unwrap()
            .mode,
        TaskMode::Plan
    );
    let pending = QueuedMessageRepository::new(&db)
        .list_pending(&task.id, &branch.id)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, QueuedMessageState::Queued);
    assert!(pending[0].id.starts_with("plan-entry:"));
}

#[test]
fn agent_plan_entry_rolls_back_plan_and_queue_when_mode_switch_fails() {
    let directory = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open_in_memory().unwrap());
    let task = Task::new(None, "Agent", "Atomic Plan entry", TaskMode::Edit);
    TaskRepository::new(&db).create(&task).unwrap();
    let branch = SessionBranchRepository::new(&db)
        .ensure_active(&task.id)
        .unwrap();
    let store = PlanStore::new(db.clone(), directory.path().join("plans"));
    db.conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_agent_plan_entry
             BEFORE UPDATE OF mode ON tasks
             WHEN NEW.mode = 'plan'
             BEGIN SELECT RAISE(ABORT, 'fault after Plan outbox writes'); END;",
        )
        .unwrap();

    assert_error_contains(
        store.enter_plan_mode_and_stage_continuation(
            &task.id,
            &branch.id,
            "Continue the same request in Plan mode",
        ),
        "fault after Plan outbox writes",
    );
    let conn = db.conn().unwrap();
    let plan_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    let queue_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM queued_messages", [], |row| row.get(0))
        .unwrap();
    let mode: String = conn
        .query_row("SELECT mode FROM tasks WHERE id = ?1", [&task.id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(plan_count, 0);
    assert_eq!(queue_count, 0);
    assert_eq!(mode, "edit");
}

#[test]
fn agent_plan_entry_rejects_invalid_task_and_branch_states() {
    let directory = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open_in_memory().unwrap());
    let tasks = TaskRepository::new(&db);
    let branches = SessionBranchRepository::new(&db);
    let store = PlanStore::new(db.clone(), directory.path().join("plans"));

    assert_error_contains(
        store.enter_plan_mode_and_stage_continuation("", "branch", "continue"),
        "cannot be blank",
    );
    assert_error_contains(
        store.enter_plan_mode_and_stage_continuation("missing", "branch", "continue"),
        "task does not exist",
    );

    let archived = Task::new(None, "Archived", "Plan safely", TaskMode::Edit);
    tasks.create(&archived).unwrap();
    let archived_branch = branches.ensure_active(&archived.id).unwrap();
    tasks
        .update_state(&archived.id, TaskState::Archived)
        .unwrap();
    assert_error_contains(
        store.enter_plan_mode_and_stage_continuation(&archived.id, &archived_branch.id, "continue"),
        "archived task",
    );

    let codex = Task::new(None, "Codex", "Plan safely", TaskMode::Edit);
    tasks.create(&codex).unwrap();
    tasks
        .set_agent_engine(&codex.id, AgentEngine::Codex)
        .unwrap();
    let codex_branch = branches.ensure_active(&codex.id).unwrap();
    assert_error_contains(
        store.enter_plan_mode_and_stage_continuation(&codex.id, &codex_branch.id, "continue"),
        "requires the R-Code",
    );

    let already_plan = Task::new(None, "Plan", "Already planning", TaskMode::Plan);
    tasks.create(&already_plan).unwrap();
    let plan_branch = branches.ensure_active(&already_plan.id).unwrap();
    assert_error_contains(
        store.enter_plan_mode_and_stage_continuation(&already_plan.id, &plan_branch.id, "continue"),
        "unavailable while task mode is plan",
    );

    let inactive_branch = Task::new(None, "Branch", "Use active branch", TaskMode::Edit);
    tasks.create(&inactive_branch).unwrap();
    branches.ensure_active(&inactive_branch.id).unwrap();
    assert_error_contains(
        store.enter_plan_mode_and_stage_continuation(
            &inactive_branch.id,
            "not-the-active-branch",
            "continue",
        ),
        "active session branch",
    );

    let existing = Task::new(None, "Existing", "Do not duplicate", TaskMode::Edit);
    tasks.create(&existing).unwrap();
    let existing_branch = branches.ensure_active(&existing.id).unwrap();
    store
        .create_plan(&CreatePlanInput {
            task_id: existing.id.clone(),
        })
        .unwrap();
    assert_error_contains(
        store.enter_plan_mode_and_stage_continuation(&existing.id, &existing_branch.id, "continue"),
        "already has an active Plan",
    );
}

#[test]
fn hierarchy_validation_rejects_unsafe_paths_without_changing_the_plan() {
    let fixture = Fixture::in_memory("Validate hierarchy");
    let created = fixture.create_plan();
    let projection = projection_path(&created);
    let original_projection = fs::read_to_string(&projection).unwrap();
    let invalid_paths = [
        vec!["1", "2", "3", "4", "5"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>(),
        vec!["   ".to_string()],
        vec!["x".repeat(121)],
        vec!["unsafe\0label".to_string()],
    ];

    for section_path in invalid_paths {
        let mut draft = item("feature", "Feature", &[]);
        draft.section_path = section_path;
        assert!(fixture
            .store
            .publish_plan(
                &fixture.task.id,
                &PublishPlanInput {
                    plan_id: created.plan.id.clone(),
                    expected_revision: created.plan.revision,
                    items: vec![draft],
                },
            )
            .is_err());
        let unchanged = fixture
            .store
            .current_for_task(&fixture.task.id)
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.plan.revision, created.plan.revision);
        assert_eq!(
            fs::read_to_string(&projection).unwrap(),
            original_projection
        );
    }
}

#[test]
fn corrupt_hierarchy_json_is_reported_instead_of_silently_flattened() {
    let fixture = Fixture::in_memory("Detect corrupt hierarchy");
    let created = fixture.create_plan();
    publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("feature", "Feature", &[])],
    );
    fixture
        .db
        .conn()
        .unwrap()
        .execute(
            "UPDATE plan_items SET section_path_json = 'not-json' WHERE plan_id = ?1",
            [&created.plan.id],
        )
        .unwrap();

    assert_error_contains(
        fixture.store.current_for_task(&fixture.task.id),
        "invalid Plan section_path_json",
    );
}

#[test]
fn implementation_stage_rolls_back_task_mode_and_queue_when_plan_ack_fails() {
    let fixture = Fixture::in_memory("Atomic implementation handoff");
    let created = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("feature", "Feature", &[])],
    );
    let approved = fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: ready.plan.revision,
            },
        )
        .unwrap();
    let branch = SessionBranchRepository::new(&fixture.db)
        .ensure_active(&fixture.task.id)
        .unwrap();
    fixture
        .store
        .claim_implementation_dispatch(&fixture.task.id, &approved.plan.id)
        .unwrap()
        .unwrap();
    fixture
        .db
        .conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_plan_dispatch_ack
             BEFORE UPDATE OF implementation_dispatch_state ON plans
             WHEN NEW.implementation_dispatch_state = 'dispatched'
             BEGIN SELECT RAISE(ABORT, 'fault after outbox writes'); END;",
        )
        .unwrap();
    assert_error_contains(
        fixture.store.stage_implementation_dispatch(
            &fixture.task.id,
            &approved.plan.id,
            &branch.id,
            "Implement Feature",
        ),
        "fault after outbox writes",
    );
    let conn = fixture.db.conn().unwrap();
    let queue_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM queued_messages", [], |row| row.get(0))
        .unwrap();
    let task_mode: String = conn
        .query_row(
            "SELECT mode FROM tasks WHERE id = ?1",
            [&fixture.task.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queue_count, 0);
    assert_eq!(task_mode, "plan");
}

#[test]
fn startup_recovery_exposes_unclaimed_and_claimed_implementation_handoffs_for_retry() {
    let fixture = Fixture::in_memory("Recover implementation handoff");
    let created = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("feature", "Feature", &[])],
    );
    let approved = fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: ready.plan.revision,
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .recover_interrupted_implementation_dispatches()
            .unwrap(),
        1
    );
    let pending_recovered = fixture
        .store
        .get_plan(&fixture.task.id, &approved.plan.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        pending_recovered.plan.implementation_dispatch_state,
        PlanImplementationDispatchState::Failed
    );
    assert_eq!(
        pending_recovered
            .plan
            .implementation_dispatch_error
            .as_deref(),
        Some(r_code_store::PLAN_IMPLEMENTATION_DISPATCH_INTERRUPTED)
    );

    fixture
        .store
        .claim_implementation_dispatch(&fixture.task.id, &approved.plan.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .store
            .recover_interrupted_implementation_dispatches()
            .unwrap(),
        1
    );
    let claimed_recovered = fixture
        .store
        .get_plan(&fixture.task.id, &approved.plan.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        claimed_recovered.plan.implementation_dispatch_state,
        PlanImplementationDispatchState::Failed
    );
}

#[test]
fn restart_preserves_one_staged_implementation_message_for_startup_drain() {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("r-code.db");
    let projection_root = directory.path().join("plans");
    let db = Arc::new(Database::open(&db_path).unwrap());
    let task = seed_task(db.as_ref(), "Restart implementation drain");
    let store = PlanStore::new(Arc::clone(&db), &projection_root);
    let created = store
        .create_plan(&CreatePlanInput {
            task_id: task.id.clone(),
        })
        .unwrap();
    let ready = publish(
        &store,
        &task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("feature", "Feature", &[])],
    );
    let approved = store
        .approve_plan(
            &task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: ready.plan.revision,
            },
        )
        .unwrap();
    let branch = SessionBranchRepository::new(&db)
        .ensure_active(&task.id)
        .unwrap();
    store
        .claim_implementation_dispatch(&task.id, &approved.plan.id)
        .unwrap()
        .unwrap();
    store
        .stage_implementation_dispatch(&task.id, &approved.plan.id, &branch.id, "Implement Feature")
        .unwrap();
    drop(store);
    drop(db);

    let reopened_db = Arc::new(Database::open(&db_path).unwrap());
    let reopened = PlanStore::new(Arc::clone(&reopened_db), &projection_root);
    assert_eq!(
        reopened
            .recover_interrupted_implementation_dispatches()
            .unwrap(),
        0
    );
    assert_eq!(
        QueuedMessageRepository::new(&reopened_db)
            .list_queued_task_ids()
            .unwrap(),
        vec![task.id.clone()]
    );
    let count: i64 = reopened_db
        .conn()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM queued_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let view = reopened
        .get_plan(&task.id, &approved.plan.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        view.plan.implementation_dispatch_state,
        PlanImplementationDispatchState::Dispatched
    );
}

#[test]
fn failed_implementation_queue_can_rebind_to_the_new_active_branch_without_duplicates() {
    let fixture = Fixture::in_memory("Retry after branch change");
    let created = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("feature", "Feature", &[])],
    );
    let approved = fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: ready.plan.revision,
            },
        )
        .unwrap();
    let branches = SessionBranchRepository::new(&fixture.db);
    let original = branches.ensure_active(&fixture.task.id).unwrap();
    fixture
        .store
        .claim_implementation_dispatch(&fixture.task.id, &approved.plan.id)
        .unwrap()
        .unwrap();
    let staged = fixture
        .store
        .stage_implementation_dispatch(
            &fixture.task.id,
            &approved.plan.id,
            &original.id,
            "Old branch message",
        )
        .unwrap();
    let queue_id = staged.plan.implementation_queue_message_id.clone().unwrap();
    QueuedMessageRepository::new(&fixture.db)
        .set_state(&queue_id, QueuedMessageState::Cancelled)
        .unwrap();
    fixture
        .store
        .mark_implementation_dispatch_failed_for_queue(
            &queue_id,
            "PLAN_IMPLEMENTATION_QUEUE_FAILED: branch changed",
        )
        .unwrap();
    let replacement = SessionBranch::fork(&fixture.task.id, &original.id, "message:1");
    branches.create_fork(&replacement).unwrap();

    fixture
        .store
        .claim_implementation_dispatch(&fixture.task.id, &approved.plan.id)
        .unwrap()
        .unwrap();
    let retried = fixture
        .store
        .stage_implementation_dispatch(
            &fixture.task.id,
            &approved.plan.id,
            &replacement.id,
            "New branch message",
        )
        .unwrap();
    assert_eq!(
        retried.plan.implementation_queue_message_id.as_deref(),
        Some(queue_id.as_str())
    );
    let conn = fixture.db.conn().unwrap();
    let row: (String, String, String) = conn
        .query_row(
            "SELECT branch_id, message, state FROM queued_messages WHERE id = ?1",
            [&queue_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (replacement.id, "New branch message".into(), "queued".into())
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM queued_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn approved_items_progress_deterministically_in_ordinal_and_dependency_order() {
    let fixture = Fixture::in_memory("Deterministic progression");
    let created = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        1,
        vec![
            item("first", "First", &[]),
            item("second", "Second", &[]),
            item("third", "Third", &["first", "second"]),
        ],
    );
    let approved = fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: 2,
            },
        )
        .unwrap();
    assert_eq!(approved.items[0].state, PlanItemState::InProgress);
    assert_eq!(approved.items[1].state, PlanItemState::Pending);
    assert_eq!(approved.items[2].state, PlanItemState::Pending);

    let after_first = fixture
        .store
        .update_plan_item(
            &fixture.task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id.clone(),
                item_id: "first".to_string(),
                expected_revision: 3,
                state: PlanItemState::Completed,
            },
        )
        .unwrap();
    assert_eq!(after_first.items[0].state, PlanItemState::Completed);
    assert_eq!(after_first.items[1].state, PlanItemState::InProgress);
    assert_eq!(after_first.items[2].state, PlanItemState::Pending);

    let after_second = fixture
        .store
        .update_plan_item(
            &fixture.task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id.clone(),
                item_id: "second".to_string(),
                expected_revision: 4,
                state: PlanItemState::Completed,
            },
        )
        .unwrap();
    assert_eq!(after_second.items[2].state, PlanItemState::InProgress);

    let completed = fixture
        .store
        .update_plan_item(
            &fixture.task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id.clone(),
                item_id: "third".to_string(),
                expected_revision: 5,
                state: PlanItemState::Completed,
            },
        )
        .unwrap();
    assert_eq!(completed.plan.revision, 6);
    assert_eq!(completed.plan.state, PlanState::Completed);
    assert!(completed
        .items
        .iter()
        .all(|entry| entry.state == PlanItemState::Completed));
}

#[test]
fn blocked_feature_resumes_then_completes_and_activates_its_dependent() {
    let fixture = Fixture::in_memory("Resume a blocked feature");
    let created = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![
            item("first", "First", &[]),
            item("second", "Second", &["first"]),
        ],
    );
    let approved = fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: ready.plan.revision,
            },
        )
        .unwrap();
    let first_started_at = approved.items[0].started_at;

    let blocked = fixture
        .store
        .update_plan_item(
            &fixture.task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id.clone(),
                item_id: "first".to_string(),
                expected_revision: approved.plan.revision,
                state: PlanItemState::Blocked,
            },
        )
        .unwrap();
    assert_eq!(blocked.items[0].state, PlanItemState::Blocked);
    assert_eq!(blocked.items[0].started_at, first_started_at);
    assert!(!blocked
        .items
        .iter()
        .any(|entry| entry.state == PlanItemState::InProgress));

    let resumed = fixture
        .store
        .update_plan_item(
            &fixture.task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id.clone(),
                item_id: "first".to_string(),
                expected_revision: blocked.plan.revision,
                state: PlanItemState::InProgress,
            },
        )
        .unwrap();
    assert_eq!(resumed.items[0].state, PlanItemState::InProgress);
    assert_eq!(resumed.items[0].started_at, first_started_at);
    assert_eq!(
        resumed
            .items
            .iter()
            .filter(|entry| entry.state == PlanItemState::InProgress)
            .count(),
        1
    );

    let advanced = fixture
        .store
        .update_plan_item(
            &fixture.task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id.clone(),
                item_id: "first".to_string(),
                expected_revision: resumed.plan.revision,
                state: PlanItemState::Completed,
            },
        )
        .unwrap();
    assert_eq!(advanced.items[0].state, PlanItemState::Completed);
    assert_eq!(advanced.items[1].state, PlanItemState::InProgress);
    assert_eq!(
        advanced
            .items
            .iter()
            .filter(|entry| entry.state == PlanItemState::InProgress)
            .count(),
        1
    );
}

#[test]
fn concurrent_blocked_feature_resume_has_one_winner_and_one_active_feature() {
    let directory = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(directory.path().join("r-code.db")).unwrap());
    let task = seed_task(db.as_ref(), "Concurrent blocked resume");
    let store = Arc::new(PlanStore::new(
        Arc::clone(&db),
        directory.path().join("plans"),
    ));
    let created = store
        .create_plan(&CreatePlanInput {
            task_id: task.id.clone(),
        })
        .unwrap();
    let ready = publish(
        store.as_ref(),
        &task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("only", "Only", &[])],
    );
    let approved = store
        .approve_plan(
            &task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: ready.plan.revision,
            },
        )
        .unwrap();
    let blocked = store
        .update_plan_item(
            &task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id.clone(),
                item_id: "only".to_string(),
                expected_revision: approved.plan.revision,
                state: PlanItemState::Blocked,
            },
        )
        .unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let task_id = task.id.clone();
        let plan_id = ready.plan.id.clone();
        let expected_revision = blocked.plan.revision;
        handles.push(thread::spawn(move || {
            barrier.wait();
            store.update_plan_item(
                &task_id,
                &UpdatePlanItemInput {
                    plan_id,
                    item_id: "only".to_string(),
                    expected_revision,
                    state: PlanItemState::InProgress,
                },
            )
        }));
    }
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

    let current = store.current_for_task(&task.id).unwrap().unwrap();
    assert_eq!(current.plan.state, PlanState::Executing);
    assert_eq!(current.items[0].state, PlanItemState::InProgress);
    assert_eq!(
        current
            .items
            .iter()
            .filter(|entry| entry.state == PlanItemState::InProgress)
            .count(),
        1
    );
}

#[test]
fn cancel_plan_terminates_pending_questions_is_idempotent_and_allows_a_new_plan() {
    let fixture = Fixture::in_memory("Cancel awaiting input");
    let created = fixture.create_plan();
    let awaiting = request_questions(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![question("scope", 2)],
    );
    let question_set_id = awaiting.pending_question_set.as_ref().unwrap().id.clone();

    let cancelled = fixture
        .store
        .cancel_plan(
            &fixture.task.id,
            &CancelPlanInput {
                plan_id: awaiting.plan.id.clone(),
                expected_revision: awaiting.plan.revision,
            },
        )
        .unwrap();
    assert_eq!(cancelled.plan.state, PlanState::Cancelled);
    assert_eq!(cancelled.plan.revision, awaiting.plan.revision + 1);
    assert!(cancelled.pending_question_set.is_none());
    assert!(cancelled.continuation_question_set.is_none());
    let resolved = fixture
        .store
        .get_question_set(&fixture.task.id, &question_set_id)
        .unwrap();
    assert_eq!(resolved.state, PlanQuestionSetState::Skipped);
    assert_eq!(
        resolved.continuation_state,
        PlanContinuationState::NotRequested
    );
    assert!(resolved
        .answer_idempotency_key
        .as_deref()
        .unwrap()
        .starts_with("plan-cancel:"));
    assert_eq!(
        TaskRepository::new(&fixture.db)
            .get(&fixture.task.id)
            .unwrap()
            .unwrap()
            .mode,
        TaskMode::Ask
    );

    TaskRepository::new(&fixture.db)
        .set_mode(&fixture.task.id, TaskMode::Auto)
        .unwrap();

    let retry = fixture
        .store
        .cancel_plan(
            &fixture.task.id,
            &CancelPlanInput {
                plan_id: awaiting.plan.id.clone(),
                expected_revision: awaiting.plan.revision,
            },
        )
        .unwrap();
    assert_eq!(retry.plan.revision, cancelled.plan.revision);
    assert_eq!(
        TaskRepository::new(&fixture.db)
            .get(&fixture.task.id)
            .unwrap()
            .unwrap()
            .mode,
        TaskMode::Ask,
        "idempotent cancellation repairs a stale task mode"
    );

    let replacement = fixture.create_plan();
    assert_ne!(replacement.plan.id, cancelled.plan.id);
    assert_eq!(replacement.plan.state, PlanState::Draft);
}

#[test]
fn cancel_is_rejected_while_enhanced_review_rollback_is_nonterminal() {
    let fixture = Fixture::in_memory("Reject cancellation during rollback");
    let created = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("feature", "Feature", &[])],
    );
    let approved = fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: ready.plan.revision,
            },
        )
        .unwrap();
    fixture
        .db
        .conn()
        .unwrap()
        .execute(
            "INSERT INTO plan_reject_operations
             (id, plan_id, plan_revision, item_id, scope, state, created_at, updated_at)
             VALUES ('reject-op', ?1, ?2, 'feature', 'feature', 'prepared',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![ready.plan.id, ready.plan.revision],
        )
        .unwrap();
    assert_error_contains(
        fixture.store.cancel_plan(
            &fixture.task.id,
            &CancelPlanInput {
                plan_id: approved.plan.id.clone(),
                expected_revision: approved.plan.revision,
            },
        ),
        "审核回滚处理中",
    );
    assert_eq!(
        fixture
            .store
            .get_plan(&fixture.task.id, &approved.plan.id)
            .unwrap()
            .unwrap()
            .plan
            .state,
        PlanState::Executing
    );
}

#[test]
fn cancel_executing_plan_preserves_completed_items_and_cancels_unfinished_items() {
    let fixture = Fixture::in_memory("Cancel partial implementation");
    let created = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![
            item("first", "First", &[]),
            item("second", "Second", &["first"]),
        ],
    );
    let approved = fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: ready.plan.revision,
            },
        )
        .unwrap();
    let advanced = fixture
        .store
        .update_plan_item(
            &fixture.task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id.clone(),
                item_id: "first".to_string(),
                expected_revision: approved.plan.revision,
                state: PlanItemState::Completed,
            },
        )
        .unwrap();

    let cancelled = fixture
        .store
        .cancel_plan(
            &fixture.task.id,
            &CancelPlanInput {
                plan_id: ready.plan.id,
                expected_revision: advanced.plan.revision,
            },
        )
        .unwrap();
    assert_eq!(cancelled.plan.state, PlanState::Cancelled);
    assert_eq!(cancelled.items[0].state, PlanItemState::Completed);
    assert_eq!(cancelled.items[1].state, PlanItemState::Cancelled);
    assert!(cancelled.items[1].completed_at.is_some());
}

#[test]
fn current_plan_falls_back_to_latest_completed_plan_when_no_active_plan_exists() {
    let fixture = Fixture::in_memory("Completed enhanced review context");
    let created = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        1,
        vec![item("only", "Only feature", &[])],
    );
    fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: 2,
            },
        )
        .unwrap();
    fixture
        .store
        .update_plan_item(
            &fixture.task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id.clone(),
                item_id: "only".to_string(),
                expected_revision: 3,
                state: PlanItemState::Completed,
            },
        )
        .unwrap();

    let current = fixture
        .store
        .current_for_task(&fixture.task.id)
        .unwrap()
        .expect("the latest completed Plan remains current for enhanced review");
    assert_eq!(current.plan.id, ready.plan.id);
    assert_eq!(current.plan.state, PlanState::Completed);
}

#[test]
fn current_plan_prefers_a_new_active_plan_over_completed_history() {
    let fixture = Fixture::in_memory("Active Plan wins");
    let first = fixture.create_plan();
    let ready = publish(
        &fixture.store,
        &fixture.task.id,
        &first.plan.id,
        1,
        vec![item("historical", "Historical feature", &[])],
    );
    fixture
        .store
        .approve_plan(
            &fixture.task.id,
            &ApprovePlanInput {
                plan_id: ready.plan.id.clone(),
                expected_revision: 2,
            },
        )
        .unwrap();
    fixture
        .store
        .update_plan_item(
            &fixture.task.id,
            &UpdatePlanItemInput {
                plan_id: ready.plan.id,
                item_id: "historical".to_string(),
                expected_revision: 3,
                state: PlanItemState::Completed,
            },
        )
        .unwrap();
    let new_plan = fixture.create_plan();

    let current = fixture
        .store
        .current_for_task(&fixture.task.id)
        .unwrap()
        .unwrap();
    assert_eq!(current.plan.id, new_plan.plan.id);
    assert_eq!(current.plan.state, PlanState::Draft);
}

#[test]
fn projection_failure_is_persisted_and_repair_reuses_the_same_target() {
    let directory = tempfile::tempdir().unwrap();
    let blocked_root = directory.path().join("blocked-root");
    fs::write(&blocked_root, b"this file blocks create_dir_all").unwrap();
    let db = Arc::new(Database::open_in_memory().unwrap());
    let task = seed_task(db.as_ref(), "Repair projection");
    let store = PlanStore::new(Arc::clone(&db), &blocked_root);
    let created = store
        .create_plan(&CreatePlanInput {
            task_id: task.id.clone(),
        })
        .unwrap();
    let target = projection_path(&created);
    assert!(created.plan.projection_error.is_some());
    assert_eq!(created.plan.projection_revision, None);
    assert!(!target.exists());

    fs::remove_file(&blocked_root).unwrap();
    fs::create_dir_all(&blocked_root).unwrap();
    let repaired = store.repair_projection(&task.id, &created.plan.id).unwrap();
    assert_eq!(projection_path(&repaired), target);
    assert_eq!(repaired.plan.projection_revision, Some(1));
    assert_eq!(repaired.plan.projection_error, None);
    assert!(target.is_file());
    assert!(fs::read_to_string(target)
        .unwrap()
        .contains("# R-Code Plan"));
}

#[test]
fn projection_atomically_overwrites_existing_target_on_every_revision() {
    let fixture = Fixture::in_memory("Overwrite projection");
    let created = fixture.create_plan();
    let target = projection_path(&created);
    assert!(target.is_file());
    fs::write(&target, "stale external projection").unwrap();

    let first = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        1,
        vec![item("feature", "First title", &[])],
    );
    assert_eq!(projection_path(&first), target);
    let first_markdown = fs::read_to_string(&target).unwrap();
    assert!(first_markdown.contains("First title"));
    assert!(!first_markdown.contains("stale external projection"));

    let second = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        2,
        vec![item("feature", "Second title", &[])],
    );
    assert_eq!(projection_path(&second), target);
    assert_eq!(second.plan.projection_revision, Some(3));
    let second_markdown = fs::read_to_string(&target).unwrap();
    assert!(second_markdown.contains("Second title"));
    assert!(!second_markdown.contains("First title"));
}

#[test]
fn model_facing_feature_ids_are_scoped_to_each_plan_not_global_database_keys() {
    let fixture = Fixture::in_memory("First Plan");
    let second_task = seed_task(fixture.db.as_ref(), "Second Plan");
    let first = fixture.create_plan();
    let second = fixture
        .store
        .create_plan(&CreatePlanInput {
            task_id: second_task.id.clone(),
        })
        .unwrap();

    let first_ready = publish(
        &fixture.store,
        &fixture.task.id,
        &first.plan.id,
        1,
        vec![item("feature-1", "First feature", &[])],
    );
    let second_ready = publish(
        &fixture.store,
        &second_task.id,
        &second.plan.id,
        1,
        vec![item("feature-1", "Second feature", &[])],
    );
    assert_eq!(first_ready.items[0].id, "feature-1");
    assert_eq!(second_ready.items[0].id, "feature-1");
}

#[test]
fn model_facing_question_ids_are_scoped_to_each_plan() {
    let fixture = Fixture::in_memory("First question Plan");
    let second_task = seed_task(fixture.db.as_ref(), "Second question Plan");
    let first = fixture.create_plan();
    let second = fixture
        .store
        .create_plan(&CreatePlanInput {
            task_id: second_task.id.clone(),
        })
        .unwrap();

    let first_question = PlanQuestionDraft {
        id: "q1".to_string(),
        header: "Decision".to_string(),
        question: "Proceed?".to_string(),
        options: vec![option("first-yes", "Yes"), option("first-no", "No")],
    };
    let second_question = PlanQuestionDraft {
        id: "q1".to_string(),
        header: "Decision".to_string(),
        question: "Proceed?".to_string(),
        options: vec![option("second-yes", "Yes"), option("second-no", "No")],
    };
    let first_awaiting = request_questions(
        &fixture.store,
        &fixture.task.id,
        &first.plan.id,
        1,
        vec![first_question],
    );
    let second_awaiting = request_questions(
        &fixture.store,
        &second_task.id,
        &second.plan.id,
        1,
        vec![second_question],
    );
    assert_ne!(
        first_awaiting.pending_question_set.as_ref().unwrap().id,
        second_awaiting.pending_question_set.as_ref().unwrap().id
    );
    assert_eq!(
        first_awaiting.pending_question_set.unwrap().questions[0].id,
        "q1"
    );
    assert_eq!(
        second_awaiting.pending_question_set.unwrap().questions[0].id,
        "q1"
    );
}

#[test]
fn model_facing_option_ids_are_scoped_to_each_question_set() {
    let fixture = Fixture::in_memory("First option Plan");
    let second_task = seed_task(fixture.db.as_ref(), "Second option Plan");
    let first = fixture.create_plan();
    let second = fixture
        .store
        .create_plan(&CreatePlanInput {
            task_id: second_task.id.clone(),
        })
        .unwrap();
    let first_question = PlanQuestionDraft {
        id: "first-question".to_string(),
        header: "Decision".to_string(),
        question: "Proceed with first?".to_string(),
        options: vec![option("yes", "Yes"), option("no", "No")],
    };
    let second_question = PlanQuestionDraft {
        id: "second-question".to_string(),
        header: "Decision".to_string(),
        question: "Proceed with second?".to_string(),
        options: vec![option("yes", "Yes"), option("no", "No")],
    };
    request_questions(
        &fixture.store,
        &fixture.task.id,
        &first.plan.id,
        1,
        vec![first_question],
    );
    let second_awaiting = request_questions(
        &fixture.store,
        &second_task.id,
        &second.plan.id,
        1,
        vec![second_question],
    );
    let options = &second_awaiting.pending_question_set.unwrap().questions[0].options;
    assert_eq!(options[0].id, "yes");
    assert_eq!(options[1].id, "no");
}

#[test]
fn plan_projection_target_never_escapes_configured_root() {
    let fixture = Fixture::in_memory("Projection containment");
    let created = fixture.create_plan();
    let root = fixture.store.projection_root().canonicalize().unwrap();
    let target = projection_path(&created).canonicalize().unwrap();
    assert!(target.starts_with(root));
    assert_eq!(
        target.file_name().and_then(|value| value.to_str()),
        Some("plan.md")
    );
    assert_eq!(
        target
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str()),
        Some(created.plan.id.as_str())
    );
}

#[test]
fn tampered_projection_path_never_writes_outside_the_configured_root() {
    let fixture = Fixture::in_memory("Projection tamper containment");
    let created = fixture.create_plan();
    let canonical_target = projection_path(&created);
    let outside = fixture._directory.path().join("outside-plan.md");
    fs::write(&outside, "outside sentinel").unwrap();
    fixture
        .db
        .conn()
        .unwrap()
        .execute(
            "UPDATE plans SET projection_path = ?1, projection_revision = NULL WHERE id = ?2",
            rusqlite::params![outside.to_string_lossy().into_owned(), created.plan.id],
        )
        .unwrap();

    let published = publish(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![item("contained", "Contained change", &[])],
    );

    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside sentinel");
    assert!(published
        .plan
        .projection_error
        .as_deref()
        .is_some_and(|error| error.contains("canonical AppData target")));
    assert!(!fs::read_to_string(&canonical_target)
        .unwrap()
        .contains("Contained change"));

    let repaired = fixture
        .store
        .repair_projection(&fixture.task.id, &created.plan.id)
        .unwrap();
    assert_eq!(projection_path(&repaired), canonical_target);
    assert_eq!(
        repaired.plan.projection_revision,
        Some(repaired.plan.revision)
    );
    assert!(fs::read_to_string(canonical_target)
        .unwrap()
        .contains("Contained change"));
    assert_eq!(fs::read_to_string(outside).unwrap(), "outside sentinel");
}

#[test]
fn interrupted_continuation_recovers_as_visible_retryable_failure() {
    let fixture = Fixture::in_memory("Recover continuation");
    let created = fixture.create_plan();
    let awaiting = request_questions(
        &fixture.store,
        &fixture.task.id,
        &created.plan.id,
        created.plan.revision,
        vec![question("recover-q", 2)],
    );
    let set_id = awaiting.pending_question_set.unwrap().id;
    fixture
        .store
        .answer_questions(
            &fixture.task.id,
            &AnswerPlanQuestionsInput {
                question_set_id: set_id.clone(),
                expected_revision: 2,
                idempotency_key: "recover-answer".to_string(),
                skip_all: false,
                answers: vec![PlanQuestionAnswerInput::Option {
                    question_id: "recover-q".to_string(),
                    option_id: "recover-q-option-0".to_string(),
                }],
            },
        )
        .unwrap();
    let pending = fixture
        .store
        .current_for_task(&fixture.task.id)
        .unwrap()
        .unwrap()
        .continuation_question_set
        .unwrap();
    assert_eq!(pending.continuation_state, PlanContinuationState::Pending);

    assert_eq!(
        fixture.store.recover_interrupted_continuations().unwrap(),
        1
    );
    let recovered = fixture
        .store
        .current_for_task(&fixture.task.id)
        .unwrap()
        .unwrap()
        .continuation_question_set
        .unwrap();
    assert_eq!(recovered.continuation_state, PlanContinuationState::Failed);
    assert_eq!(
        recovered.continuation_error.as_deref(),
        Some(r_code_store::PLAN_CONTINUATION_INTERRUPTED)
    );

    let retryable = fixture
        .store
        .retry_continuation(&fixture.task.id, &set_id)
        .unwrap();
    assert_eq!(retryable.continuation_state, PlanContinuationState::Pending);

    fixture
        .store
        .claim_continuation(&fixture.task.id, &set_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture.store.recover_interrupted_continuations().unwrap(),
        1
    );
    let claimed_recovered = fixture
        .store
        .current_for_task(&fixture.task.id)
        .unwrap()
        .unwrap()
        .continuation_question_set
        .unwrap();
    assert_eq!(
        claimed_recovered.continuation_state,
        PlanContinuationState::Failed
    );
}
