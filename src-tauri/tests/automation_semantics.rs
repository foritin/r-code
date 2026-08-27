use chrono::{DateTime, NaiveTime, Utc};
use r_code_core::{AutomationDefinitionState, RunStatus, ScheduleSpec};
use r_code_host::automation::{
    apply_wall_clock_policy, decide_overlap, plan_catch_up, profile_dispatch_decision,
    require_feature_enabled, transition_definition, transition_run, validate_run_transition,
    validate_schedule, AutomationSemanticError, DefinitionAction, DispatchDecision,
    ExecutionProfileAvailability, OverlapDecision, WallClockResolution,
    BASE_REF_UNAVAILABLE_ERROR_CODE, MODEL_UNAVAILABLE_ERROR_CODE, OVERLAP_SKIPPED_ERROR_CODE,
    PROVIDER_UNAVAILABLE_ERROR_CODE,
};
use r_code_host::feature_flags::ProductFeatureFlags;

fn utc(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("valid test timestamp")
        .with_timezone(&Utc)
}

fn daily_schedule() -> ScheduleSpec {
    ScheduleSpec::Daily {
        local_time: NaiveTime::from_hms_opt(9, 30, 0).expect("valid wall time"),
    }
}

#[test]
fn automation_is_server_side_disabled_by_default() {
    let error = require_feature_enabled(ProductFeatureFlags::default())
        .expect_err("unfinished Automation must be disabled by default");
    assert_eq!(error.code, "automation.feature_disabled");
    assert!(error.args.is_empty());
    assert!(error.debug_detail.is_none());

    require_feature_enabled(ProductFeatureFlags {
        browser_enabled: false,
        automation_enabled: true,
    })
    .expect("the Automation gate should open only when explicitly enabled");
}

#[test]
fn all_schedule_variants_validate_but_hourly_is_exactly_sixty_minutes() {
    let schedules = [
        ScheduleSpec::Once {
            run_at_utc: utc("2026-09-01T08:00:00Z"),
        },
        ScheduleSpec::Hourly {
            anchor_at_utc: utc("2026-08-26T00:00:00Z"),
            interval_minutes: 60,
        },
        daily_schedule(),
        ScheduleSpec::Weekdays {
            local_time: NaiveTime::from_hms_opt(10, 0, 0).expect("valid wall time"),
        },
        ScheduleSpec::Weekly {
            weekday: r_code_core::AutomationWeekday::Monday,
            local_time: NaiveTime::from_hms_opt(11, 15, 0).expect("valid wall time"),
        },
    ];
    for schedule in &schedules {
        validate_schedule(schedule).expect("frozen schedule variant should validate");
    }

    for actual in [0, 1, 59, 61, 120, u16::MAX] {
        let invalid = ScheduleSpec::Hourly {
            anchor_at_utc: utc("2026-08-26T00:00:00Z"),
            interval_minutes: actual,
        };
        assert_eq!(
            validate_schedule(&invalid),
            Err(AutomationSemanticError::InvalidHourlyInterval { actual })
        );
    }
}

#[test]
fn definition_state_machine_only_allows_pause_resume_and_once_completion() {
    let once = ScheduleSpec::Once {
        run_at_utc: utc("2026-09-01T08:00:00Z"),
    };
    assert_eq!(
        transition_definition(
            AutomationDefinitionState::Active,
            &daily_schedule(),
            DefinitionAction::Pause,
        ),
        Ok(AutomationDefinitionState::Paused)
    );
    assert_eq!(
        transition_definition(
            AutomationDefinitionState::Paused,
            &daily_schedule(),
            DefinitionAction::Resume,
        ),
        Ok(AutomationDefinitionState::Active)
    );
    assert_eq!(
        transition_definition(
            AutomationDefinitionState::Active,
            &once,
            DefinitionAction::CompleteOnce,
        ),
        Ok(AutomationDefinitionState::Completed)
    );
    assert_eq!(
        transition_definition(
            AutomationDefinitionState::Active,
            &daily_schedule(),
            DefinitionAction::CompleteOnce,
        ),
        Err(AutomationSemanticError::CompletionRequiresOnceSchedule)
    );

    for (from, action) in [
        (AutomationDefinitionState::Active, DefinitionAction::Resume),
        (AutomationDefinitionState::Paused, DefinitionAction::Pause),
        (
            AutomationDefinitionState::Paused,
            DefinitionAction::CompleteOnce,
        ),
        (
            AutomationDefinitionState::Completed,
            DefinitionAction::Pause,
        ),
        (
            AutomationDefinitionState::Completed,
            DefinitionAction::Resume,
        ),
        (
            AutomationDefinitionState::Completed,
            DefinitionAction::CompleteOnce,
        ),
    ] {
        assert!(matches!(
            transition_definition(from, &once, action),
            Err(AutomationSemanticError::InvalidDefinitionTransition { .. })
        ));
    }
}

#[test]
fn run_state_machine_matches_the_frozen_transition_matrix() {
    let statuses = [
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::WaitingApproval,
        RunStatus::Succeeded,
        RunStatus::Failed,
        RunStatus::Skipped,
        RunStatus::Cancelled,
    ];
    let allowed = [
        (RunStatus::Queued, RunStatus::Running),
        (RunStatus::Queued, RunStatus::WaitingApproval),
        (RunStatus::Queued, RunStatus::Failed),
        (RunStatus::Queued, RunStatus::Skipped),
        (RunStatus::Queued, RunStatus::Cancelled),
        (RunStatus::Running, RunStatus::WaitingApproval),
        (RunStatus::Running, RunStatus::Succeeded),
        (RunStatus::Running, RunStatus::Failed),
        (RunStatus::Running, RunStatus::Cancelled),
        (RunStatus::WaitingApproval, RunStatus::Running),
        (RunStatus::WaitingApproval, RunStatus::Failed),
        (RunStatus::WaitingApproval, RunStatus::Cancelled),
    ];

    for from in statuses {
        for to in statuses {
            let expected = allowed.contains(&(from, to));
            assert_eq!(
                validate_run_transition(from, to).is_ok(),
                expected,
                "unexpected run transition result for {from:?} -> {to:?}"
            );
            if expected {
                assert_eq!(transition_run(from, to), Ok(to));
            }
        }
    }
}

#[test]
fn any_non_terminal_run_causes_an_overlap_skip_with_a_stable_error_code() {
    assert_eq!(decide_overlap(&[]), OverlapDecision::Dispatch);
    assert_eq!(
        decide_overlap(&[
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Skipped,
            RunStatus::Cancelled,
        ]),
        OverlapDecision::Dispatch
    );
    assert_eq!(OverlapDecision::Dispatch.error_code(), None);

    for status in [
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::WaitingApproval,
    ] {
        let decision = decide_overlap(&[RunStatus::Succeeded, status]);
        assert_eq!(decision, OverlapDecision::Skip);
        assert_eq!(decision.error_code(), Some(OVERLAP_SKIPPED_ERROR_CODE));
        assert_eq!(decision.error_code(), Some("automation.overlap_skipped"));
    }
}

#[test]
fn catch_up_dispatches_only_the_latest_and_aggregates_all_older_misses() {
    let first = utc("2026-08-26T00:00:00Z");
    let second = utc("2026-08-26T01:00:00Z");
    let latest = utc("2026-08-26T02:00:00Z");

    let empty = plan_catch_up(&[]);
    assert!(!empty.should_dispatch());
    assert_eq!(empty.latest_scheduled_for, None);
    assert_eq!(empty.missed_count, 0);

    let one = plan_catch_up(&[first]);
    assert!(one.should_dispatch());
    assert_eq!(one.latest_scheduled_for, Some(first));
    assert_eq!(one.missed_count, 0);

    let unsorted = plan_catch_up(&[second, first, latest]);
    assert!(unsorted.should_dispatch());
    assert_eq!(unsorted.latest_scheduled_for, Some(latest));
    assert_eq!(unsorted.missed_count, 2);
}

#[test]
fn dst_gap_runs_at_first_valid_instant_and_fold_runs_only_the_first_occurrence() {
    let exact = utc("2026-03-08T06:30:00Z");
    let first_after_gap = utc("2026-03-08T07:00:00Z");
    let first_fold = utc("2026-11-01T05:30:00Z");
    let second_fold = utc("2026-11-01T06:30:00Z");

    assert_eq!(
        apply_wall_clock_policy(WallClockResolution::Exact(exact)),
        exact
    );
    assert_eq!(
        apply_wall_clock_policy(WallClockResolution::Missing {
            first_valid_after_gap_utc: first_after_gap,
        }),
        first_after_gap
    );
    assert_eq!(
        apply_wall_clock_policy(WallClockResolution::Repeated {
            first_occurrence_utc: first_fold,
            second_occurrence_utc: second_fold,
        }),
        first_fold
    );
}

#[test]
fn unavailable_provider_model_or_base_ref_fail_closed_without_a_fallback() {
    assert_eq!(
        profile_dispatch_decision(ExecutionProfileAvailability::Available),
        DispatchDecision::Ready
    );
    for (availability, expected_code) in [
        (
            ExecutionProfileAvailability::ProviderUnavailable,
            PROVIDER_UNAVAILABLE_ERROR_CODE,
        ),
        (
            ExecutionProfileAvailability::ModelUnavailable,
            MODEL_UNAVAILABLE_ERROR_CODE,
        ),
        (
            ExecutionProfileAvailability::BaseRefUnavailable,
            BASE_REF_UNAVAILABLE_ERROR_CODE,
        ),
    ] {
        assert_eq!(
            profile_dispatch_decision(availability),
            DispatchDecision::Fail {
                error_code: expected_code,
            }
        );
    }

    assert_eq!(
        PROVIDER_UNAVAILABLE_ERROR_CODE,
        "automation.provider_unavailable"
    );
    assert_eq!(MODEL_UNAVAILABLE_ERROR_CODE, "automation.model_unavailable");
    assert_eq!(
        BASE_REF_UNAVAILABLE_ERROR_CODE,
        "automation.base_ref_unavailable"
    );
}
