//! Frozen cross-layer contracts for Automation definitions and runs.
//!
//! This module contains data only. Persistence, scheduling, workspace creation, and execution
//! belong to downstream crates so every consumer shares the same serialized representation.

use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dto::AgentEngine;

/// Hourly Automations always advance on a UTC sixty-minute cadence.
pub const HOURLY_INTERVAL_MINUTES: u16 = 60;

const fn default_hourly_interval_minutes() -> u16 {
    HOURLY_INTERVAL_MINUTES
}

/// Provider and model selection captured by every dispatched run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionProfile {
    pub agent_engine: AgentEngine,
    pub provider_name: String,
    pub model: String,
    /// `None` means the selected provider's configured default; dispatch must never substitute
    /// a different provider, model, or effort when the requested profile is unavailable.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

/// The maximum capability envelope an Automation run may receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationPermission {
    ReadOnly,
    IsolatedWrite,
}

impl AutomationPermission {
    pub const fn requires_managed_worktree(self) -> bool {
        matches!(self, Self::IsolatedWrite)
    }
}

/// Lifecycle state of an Automation definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationDefinitionState {
    Active,
    Paused,
    Completed,
}

/// Stable weekday names used by weekly wall-clock schedules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationWeekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

/// A frozen schedule specification.
///
/// `hourly` advances from `anchor_at_utc` in UTC. The other recurring forms use `timezone` from
/// [`AutomationDefinition`] and interpret `local_time` as a wall-clock time in that IANA zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleSpec {
    Once {
        run_at_utc: DateTime<Utc>,
    },
    Hourly {
        anchor_at_utc: DateTime<Utc>,
        #[serde(default = "default_hourly_interval_minutes")]
        interval_minutes: u16,
    },
    Daily {
        local_time: NaiveTime,
    },
    Weekdays {
        local_time: NaiveTime,
    },
    Weekly {
        weekday: AutomationWeekday,
        local_time: NaiveTime,
    },
}

impl ScheduleSpec {
    pub const fn is_once(&self) -> bool {
        matches!(self, Self::Once { .. })
    }
}

/// User-authored Automation configuration. Edits only affect runs created after the edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationDefinition {
    pub id: String,
    pub name: String,
    pub workspace_path: String,
    pub prompt: String,
    pub execution_profile: ExecutionProfile,
    pub schedule: ScheduleSpec,
    /// IANA time-zone identifier such as `Asia/Shanghai`; never an OS display name or UTC offset.
    pub timezone: String,
    pub permission: AutomationPermission,
    #[serde(default)]
    pub base_ref: Option<String>,
    pub state: AutomationDefinitionState,
    /// `None` is valid for paused/completed definitions with no pending occurrence.
    #[serde(default)]
    pub next_run_at_utc: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AutomationDefinition {
    pub fn snapshot(&self) -> AutomationDefinitionSnapshot {
        AutomationDefinitionSnapshot::from(self)
    }
}

/// Immutable execution-relevant copy embedded in an [`AutomationRun`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationDefinitionSnapshot {
    pub definition_id: String,
    pub name: String,
    pub workspace_path: String,
    pub prompt: String,
    pub execution_profile: ExecutionProfile,
    pub schedule: ScheduleSpec,
    pub timezone: String,
    pub permission: AutomationPermission,
    #[serde(default)]
    pub base_ref: Option<String>,
    pub definition_updated_at: DateTime<Utc>,
}

impl From<&AutomationDefinition> for AutomationDefinitionSnapshot {
    fn from(definition: &AutomationDefinition) -> Self {
        Self {
            definition_id: definition.id.clone(),
            name: definition.name.clone(),
            workspace_path: definition.workspace_path.clone(),
            prompt: definition.prompt.clone(),
            execution_profile: definition.execution_profile.clone(),
            schedule: definition.schedule.clone(),
            timezone: definition.timezone.clone(),
            permission: definition.permission,
            base_ref: definition.base_ref.clone(),
            definition_updated_at: definition.updated_at,
        }
    }
}

/// Source that caused a durable run to be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    Scheduled,
    CatchUp,
    Manual,
}

/// Durable Automation run state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    WaitingApproval,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

impl RunStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }

    pub const fn is_non_terminal(self) -> bool {
        !self.is_terminal()
    }
}

/// One immutable, idempotently dispatched Automation occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationRun {
    pub id: String,
    pub automation_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub trigger: RunTrigger,
    pub scheduled_for: DateTime<Utc>,
    pub definition_snapshot: AutomationDefinitionSnapshot,
    pub status: RunStatus,
    pub idempotency_key: String,
    #[serde(default)]
    pub lease_owner: Option<String>,
    #[serde(default)]
    pub lease_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub missed_count: u32,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub error_code: Option<String>,
}

impl AutomationRun {
    pub fn has_live_lease_at(&self, now: DateTime<Utc>) -> bool {
        self.lease_owner
            .as_ref()
            .is_some_and(|owner| !owner.is_empty())
            && self
                .lease_expires_at
                .is_some_and(|expires_at| expires_at > now)
    }
}
