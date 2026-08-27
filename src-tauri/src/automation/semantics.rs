use chrono::{DateTime, Utc};
use r_code_core::{AutomationDefinitionState, RunStatus, ScheduleSpec, HOURLY_INTERVAL_MINUTES};

pub const OVERLAP_SKIPPED_ERROR_CODE: &str = "automation.overlap_skipped";
pub const PROVIDER_UNAVAILABLE_ERROR_CODE: &str = "automation.provider_unavailable";
pub const MODEL_UNAVAILABLE_ERROR_CODE: &str = "automation.model_unavailable";
pub const BASE_REF_UNAVAILABLE_ERROR_CODE: &str = "automation.base_ref_unavailable";

/// Explicit definition operations; `Completed` is reserved for the first dispatch of `once`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionAction {
    Pause,
    Resume,
    CompleteOnce,
}

/// Pure contract validation failures. These are diagnostic errors, not localized UI messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AutomationSemanticError {
    #[error("invalid automation definition transition: {from:?} via {action:?}")]
    InvalidDefinitionTransition {
        from: AutomationDefinitionState,
        action: DefinitionAction,
    },
    #[error("only a once schedule may become completed after dispatch")]
    CompletionRequiresOnceSchedule,
    #[error("invalid automation run transition: {from:?} -> {to:?}")]
    InvalidRunTransition { from: RunStatus, to: RunStatus },
    #[error("hourly automation interval must be {HOURLY_INTERVAL_MINUTES} minutes, got {actual}")]
    InvalidHourlyInterval { actual: u16 },
}

/// Validate invariants encoded by a schedule without calculating its next occurrence.
pub fn validate_schedule(schedule: &ScheduleSpec) -> Result<(), AutomationSemanticError> {
    if let ScheduleSpec::Hourly {
        interval_minutes, ..
    } = schedule
    {
        if *interval_minutes != HOURLY_INTERVAL_MINUTES {
            return Err(AutomationSemanticError::InvalidHourlyInterval {
                actual: *interval_minutes,
            });
        }
    }
    Ok(())
}

/// Apply a definition lifecycle operation without persistence or side effects.
pub fn transition_definition(
    current: AutomationDefinitionState,
    schedule: &ScheduleSpec,
    action: DefinitionAction,
) -> Result<AutomationDefinitionState, AutomationSemanticError> {
    match (current, action) {
        (AutomationDefinitionState::Active, DefinitionAction::Pause) => {
            Ok(AutomationDefinitionState::Paused)
        }
        (AutomationDefinitionState::Paused, DefinitionAction::Resume) => {
            Ok(AutomationDefinitionState::Active)
        }
        (AutomationDefinitionState::Active, DefinitionAction::CompleteOnce) => {
            if schedule.is_once() {
                Ok(AutomationDefinitionState::Completed)
            } else {
                Err(AutomationSemanticError::CompletionRequiresOnceSchedule)
            }
        }
        (from, action) => {
            Err(AutomationSemanticError::InvalidDefinitionTransition { from, action })
        }
    }
}

/// Return whether a durable run transition is legal.
pub const fn can_transition_run(from: RunStatus, to: RunStatus) -> bool {
    matches!(
        (from, to),
        (
            RunStatus::Queued,
            RunStatus::Running
                | RunStatus::WaitingApproval
                | RunStatus::Failed
                | RunStatus::Skipped
                | RunStatus::Cancelled
        ) | (
            RunStatus::Running,
            RunStatus::WaitingApproval
                | RunStatus::Succeeded
                | RunStatus::Failed
                | RunStatus::Cancelled
        ) | (
            RunStatus::WaitingApproval,
            RunStatus::Running | RunStatus::Failed | RunStatus::Cancelled
        )
    )
}

pub fn validate_run_transition(
    from: RunStatus,
    to: RunStatus,
) -> Result<(), AutomationSemanticError> {
    if can_transition_run(from, to) {
        Ok(())
    } else {
        Err(AutomationSemanticError::InvalidRunTransition { from, to })
    }
}

pub fn transition_run(
    from: RunStatus,
    to: RunStatus,
) -> Result<RunStatus, AutomationSemanticError> {
    validate_run_transition(from, to)?;
    Ok(to)
}

/// Result of enforcing the one-non-terminal-run-per-definition invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlapDecision {
    Dispatch,
    Skip,
}

impl OverlapDecision {
    pub const fn error_code(self) -> Option<&'static str> {
        match self {
            Self::Dispatch => None,
            Self::Skip => Some(OVERLAP_SKIPPED_ERROR_CODE),
        }
    }
}

pub fn decide_overlap(existing_statuses: &[RunStatus]) -> OverlapDecision {
    if existing_statuses
        .iter()
        .any(|status| status.is_non_terminal())
    {
        OverlapDecision::Skip
    } else {
        OverlapDecision::Dispatch
    }
}

/// Recovery plan: dispatch only the latest occurrence and aggregate all earlier misses once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CatchUpPlan {
    pub latest_scheduled_for: Option<DateTime<Utc>>,
    /// Value written to the single aggregated `skipped` run; excludes the latest occurrence.
    pub missed_count: u32,
}

impl CatchUpPlan {
    pub const fn should_dispatch(self) -> bool {
        self.latest_scheduled_for.is_some()
    }
}

pub fn plan_catch_up(missed_occurrences: &[DateTime<Utc>]) -> CatchUpPlan {
    let latest_scheduled_for = missed_occurrences.iter().copied().max();
    let older_count = missed_occurrences.len().saturating_sub(1);
    CatchUpPlan {
        latest_scheduled_for,
        missed_count: u32::try_from(older_count).unwrap_or(u32::MAX),
    }
}

/// Result supplied by a future IANA timezone resolver for one requested wall-clock occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallClockResolution {
    Exact(DateTime<Utc>),
    Missing {
        first_valid_after_gap_utc: DateTime<Utc>,
    },
    Repeated {
        first_occurrence_utc: DateTime<Utc>,
        second_occurrence_utc: DateTime<Utc>,
    },
}

/// Apply the frozen DST policy: gaps run at the first valid instant; folds run only the first.
pub const fn apply_wall_clock_policy(resolution: WallClockResolution) -> DateTime<Utc> {
    match resolution {
        WallClockResolution::Exact(instant) => instant,
        WallClockResolution::Missing {
            first_valid_after_gap_utc,
        } => first_valid_after_gap_utc,
        WallClockResolution::Repeated {
            first_occurrence_utc,
            ..
        } => first_occurrence_utc,
    }
}

/// Preflight result for the exact provider/model/base requested by the immutable snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionProfileAvailability {
    Available,
    ProviderUnavailable,
    ModelUnavailable,
    BaseRefUnavailable,
}

/// No failure variant carries a fallback profile; unavailable frozen inputs always fail the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchDecision {
    Ready,
    Fail { error_code: &'static str },
}

pub const fn profile_dispatch_decision(
    availability: ExecutionProfileAvailability,
) -> DispatchDecision {
    let error_code = match availability {
        ExecutionProfileAvailability::Available => return DispatchDecision::Ready,
        ExecutionProfileAvailability::ProviderUnavailable => PROVIDER_UNAVAILABLE_ERROR_CODE,
        ExecutionProfileAvailability::ModelUnavailable => MODEL_UNAVAILABLE_ERROR_CODE,
        ExecutionProfileAvailability::BaseRefUnavailable => BASE_REF_UNAVAILABLE_ERROR_CODE,
    };
    DispatchDecision::Fail { error_code }
}
