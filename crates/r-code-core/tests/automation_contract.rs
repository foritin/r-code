use r_code_core::{
    AutomationDefinition, AutomationDefinitionState, AutomationPermission, AutomationRun,
    AutomationWeekday, RunStatus, RunTrigger, ScheduleSpec, HOURLY_INTERVAL_MINUTES,
};
use serde::Deserialize;
use serde_json::Value;

const CONTRACT_FIXTURE: &str = include_str!("../../../fixtures/automation/public-contract-v1.json");

#[derive(Debug, Deserialize)]
struct StableNames {
    permissions: Vec<String>,
    definition_states: Vec<String>,
    run_triggers: Vec<String>,
    run_statuses: Vec<String>,
    weekdays: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ContractFixture {
    schema_version: u32,
    definition: AutomationDefinition,
    run: AutomationRun,
    schedule_specs: Vec<ScheduleSpec>,
    stable_names: StableNames,
}

fn serialized_names<T: serde::Serialize>(values: &[T]) -> Vec<String> {
    values
        .iter()
        .map(|value| {
            serde_json::to_value(value)
                .expect("serialize stable contract name")
                .as_str()
                .expect("stable name must serialize as a string")
                .to_owned()
        })
        .collect()
}

#[test]
fn public_fixture_round_trips_through_the_frozen_rust_contract() {
    let raw: Value = serde_json::from_str(CONTRACT_FIXTURE).expect("parse raw fixture JSON");
    let fixture: ContractFixture =
        serde_json::from_str(CONTRACT_FIXTURE).expect("deserialize Automation fixture");

    assert_eq!(fixture.schema_version, 1);
    assert_eq!(
        serde_json::to_value(&fixture.definition).expect("serialize definition"),
        raw["definition"]
    );
    assert_eq!(
        serde_json::to_value(&fixture.run).expect("serialize run"),
        raw["run"]
    );
    assert_eq!(
        serde_json::to_value(&fixture.schedule_specs).expect("serialize schedules"),
        raw["schedule_specs"]
    );
    assert_eq!(fixture.run.automation_id, fixture.definition.id);
    assert_eq!(
        fixture.run.definition_snapshot,
        fixture.definition.snapshot(),
        "the fixture run must contain the exact immutable definition snapshot"
    );
}

#[test]
fn fixture_names_cover_every_permission_state_trigger_status_and_weekday() {
    let fixture: ContractFixture =
        serde_json::from_str(CONTRACT_FIXTURE).expect("deserialize Automation fixture");

    assert_eq!(
        fixture.stable_names.permissions,
        serialized_names(&[
            AutomationPermission::ReadOnly,
            AutomationPermission::IsolatedWrite,
        ])
    );
    assert_eq!(
        fixture.stable_names.definition_states,
        serialized_names(&[
            AutomationDefinitionState::Active,
            AutomationDefinitionState::Paused,
            AutomationDefinitionState::Completed,
        ])
    );
    assert_eq!(
        fixture.stable_names.run_triggers,
        serialized_names(&[
            RunTrigger::Scheduled,
            RunTrigger::CatchUp,
            RunTrigger::Manual,
        ])
    );
    assert_eq!(
        fixture.stable_names.run_statuses,
        serialized_names(&[
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::WaitingApproval,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Skipped,
            RunStatus::Cancelled,
        ])
    );
    assert_eq!(
        fixture.stable_names.weekdays,
        serialized_names(&[
            AutomationWeekday::Monday,
            AutomationWeekday::Tuesday,
            AutomationWeekday::Wednesday,
            AutomationWeekday::Thursday,
            AutomationWeekday::Friday,
            AutomationWeekday::Saturday,
            AutomationWeekday::Sunday,
        ])
    );

    assert!(!AutomationPermission::ReadOnly.requires_managed_worktree());
    assert!(AutomationPermission::IsolatedWrite.requires_managed_worktree());
}

#[test]
fn fixture_contains_all_five_schedule_shapes_with_a_fixed_hourly_interval() {
    let fixture: ContractFixture =
        serde_json::from_str(CONTRACT_FIXTURE).expect("deserialize Automation fixture");

    assert_eq!(fixture.schedule_specs.len(), 5);
    assert!(matches!(
        fixture.schedule_specs[0],
        ScheduleSpec::Once { .. }
    ));
    assert!(matches!(
        fixture.schedule_specs[1],
        ScheduleSpec::Hourly {
            interval_minutes: HOURLY_INTERVAL_MINUTES,
            ..
        }
    ));
    assert!(matches!(
        fixture.schedule_specs[2],
        ScheduleSpec::Daily { .. }
    ));
    assert!(matches!(
        fixture.schedule_specs[3],
        ScheduleSpec::Weekdays { .. }
    ));
    assert!(matches!(
        fixture.schedule_specs[4],
        ScheduleSpec::Weekly { .. }
    ));

    let hourly_without_interval = serde_json::json!({
        "kind": "hourly",
        "anchor_at_utc": "2026-08-26T00:00:00Z"
    });
    let restored: ScheduleSpec = serde_json::from_value(hourly_without_interval)
        .expect("legacy/default hourly schedule should deserialize");
    assert!(matches!(
        restored,
        ScheduleSpec::Hourly {
            interval_minutes: HOURLY_INTERVAL_MINUTES,
            ..
        }
    ));
}

#[test]
fn a_snapshot_is_owned_and_cannot_change_when_the_definition_is_edited() {
    let mut fixture: ContractFixture =
        serde_json::from_str(CONTRACT_FIXTURE).expect("deserialize Automation fixture");
    let snapshot = fixture.definition.snapshot();

    fixture.definition.name = "Edited name".to_owned();
    fixture.definition.workspace_path = "D:/work/another".to_owned();
    fixture.definition.prompt = "Edited prompt".to_owned();
    fixture.definition.execution_profile.provider_name = "another-provider".to_owned();
    fixture.definition.execution_profile.model = "another-model".to_owned();
    fixture.definition.schedule = fixture.schedule_specs[0].clone();
    fixture.definition.timezone = "America/New_York".to_owned();
    fixture.definition.permission = AutomationPermission::ReadOnly;
    fixture.definition.base_ref = Some("release".to_owned());
    fixture.definition.updated_at += chrono::Duration::minutes(5);

    assert_eq!(snapshot, fixture.run.definition_snapshot);
    assert_ne!(snapshot, fixture.definition.snapshot());
}

#[test]
fn run_terminal_and_lease_semantics_are_stable() {
    let mut fixture: ContractFixture =
        serde_json::from_str(CONTRACT_FIXTURE).expect("deserialize Automation fixture");
    let now = fixture.run.scheduled_for;

    for status in [
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::WaitingApproval,
    ] {
        assert!(status.is_non_terminal());
        assert!(!status.is_terminal());
    }
    for status in [
        RunStatus::Succeeded,
        RunStatus::Failed,
        RunStatus::Skipped,
        RunStatus::Cancelled,
    ] {
        assert!(status.is_terminal());
        assert!(!status.is_non_terminal());
    }

    assert!(!fixture.run.has_live_lease_at(now));
    fixture.run.lease_owner = Some("scheduler-a".to_owned());
    fixture.run.lease_expires_at = Some(now + chrono::Duration::seconds(30));
    assert!(fixture.run.has_live_lease_at(now));
    assert!(!fixture
        .run
        .has_live_lease_at(now + chrono::Duration::seconds(30)));
    fixture.run.lease_owner = Some(String::new());
    assert!(!fixture.run.has_live_lease_at(now));
}
