//! Data contracts for R-Code's local evolving-memory subsystem.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const MEMORY_REVIEW_SCHEMA_VERSION: u32 = 1;
pub const MEMORY_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const MEMORY_HASH_VERSION: &str = "blake3_utf8_v1";
pub const MEMORY_NORMALIZATION_VERSION: &str = "trim_crlf_unicode_scalar_v1";

pub const MEMORY_GLOBAL_CHAR_CAP: usize = 4_000;
pub const MEMORY_PROJECT_CHAR_CAP: usize = 8_000;
pub const MEMORY_ENTRY_CHAR_CAP: usize = 1_000;
pub const MEMORY_ACTIVE_ENTRIES_PER_SCOPE_CAP: usize = 32;
pub const MEMORY_REVIEW_PROPOSAL_CAP: usize = 8;

pub const MEMORY_TRIGGER_TURNS_MIN: u32 = 5;
pub const MEMORY_TRIGGER_TURNS_DEFAULT: u32 = 10;
pub const MEMORY_TRIGGER_TURNS_MAX: u32 = 50;

pub const MEMORY_RAW_TURN_RETENTION_DAYS: i64 = 30;
pub const MEMORY_RAW_TURNS_PER_BRANCH_CAP: usize = 50;
pub const MEMORY_PENDING_BODY_RETENTION_DAYS: i64 = 90;
pub const MEMORY_TERMINAL_METADATA_RETENTION_DAYS: i64 = 180;
pub const MEMORY_TERMINAL_METADATA_CAP: usize = 500;
pub const MEMORY_REVISION_RETENTION_DAYS: i64 = 180;
pub const MEMORY_REVISIONS_PER_ENTRY_CAP: usize = 20;

pub const MEMORY_REVIEW_ENVELOPE_CHAR_CAP: usize = 24_000;
pub const MEMORY_PROPOSAL_CONTENT_MIN_CHARS: usize = 5;
pub const MEMORY_PROPOSAL_REASON_CHAR_CAP: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryKind {
    Preference,
    Constraint,
    Convention,
    Decision,
    Pitfall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum GlobalMemoryAuthorization {
    Manual,
    ApprovedCandidate,
    /// Created directly by the coding agent through the `save_memory` tool.
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectMemoryOrigin {
    Manual,
    AutomaticReview,
    /// Created directly by the coding agent through the `save_memory` tool.
    Agent,
    Undo,
}

/// Owner and creation authority encoded together so invalid origins cannot be
/// represented.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryOwner {
    Global {
        authorization: GlobalMemoryAuthorization,
    },
    Project {
        workspace_id: String,
        origin: ProjectMemoryOrigin,
    },
}

impl MemoryOwner {
    pub const fn scope(&self) -> MemoryScope {
        match self {
            Self::Global { .. } => MemoryScope::Global,
            Self::Project { .. } => MemoryScope::Project,
        }
    }

    pub fn validate(&self) -> Result<(), MemoryContractError> {
        if matches!(self, Self::Project { workspace_id, .. } if workspace_id.trim().is_empty()) {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidIdentity,
                "persistent memory identity must not be empty",
            ));
        }
        Ok(())
    }
}

/// Immutable review identity captured before review execution starts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "context", rename_all = "snake_case", deny_unknown_fields)]
pub enum FrozenReviewSource {
    PureChat {
        run_id: String,
        task_id: String,
        branch_id: String,
    },
    Project {
        run_id: String,
        task_id: String,
        branch_id: String,
        workspace_id: String,
        workspace_memory_generation: u64,
    },
}

impl FrozenReviewSource {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        let invalid = match self {
            Self::PureChat {
                run_id,
                task_id,
                branch_id,
            } => [run_id, task_id, branch_id]
                .into_iter()
                .any(|identity| identity.trim().is_empty()),
            Self::Project {
                run_id,
                task_id,
                branch_id,
                workspace_id,
                ..
            } => [run_id, task_id, branch_id, workspace_id]
                .into_iter()
                .any(|identity| identity.trim().is_empty()),
        };
        if invalid {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidIdentity,
                "persistent memory identity must not be empty",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "owner", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemorySnapshotOwner {
    PureChat,
    Project {
        workspace_id: String,
        workspace_memory_generation: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemorySnapshotEntry {
    pub entry_id: String,
    pub version: u64,
    pub kind: MemoryKind,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryEntryRef {
    pub entry_id: String,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemorySnapshot {
    pub schema_version: u32,
    pub owner: MemorySnapshotOwner,
    pub global_generation: u64,
    pub global_entries: Vec<MemorySnapshotEntry>,
    pub project_entries: Vec<MemorySnapshotEntry>,
    pub snapshot_hash: String,
    pub global_chars: u32,
    pub project_chars: u32,
}

impl MemorySnapshot {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        if self.schema_version != MEMORY_SNAPSHOT_SCHEMA_VERSION {
            return invalid_snapshot("unsupported snapshot schema version");
        }
        let global_chars = validate_snapshot_entries(&self.global_entries)?;
        let project_chars = validate_snapshot_entries(&self.project_entries)?;
        if global_chars != self.global_chars || project_chars != self.project_chars {
            return invalid_snapshot("snapshot character totals do not match entries");
        }
        if global_chars as usize > MEMORY_GLOBAL_CHAR_CAP
            || project_chars as usize > MEMORY_PROJECT_CHAR_CAP
        {
            return invalid_snapshot("snapshot character total exceeds its scope cap");
        }
        match &self.owner {
            MemorySnapshotOwner::PureChat
                if !self.project_entries.is_empty() || self.project_chars != 0 =>
            {
                return invalid_snapshot("pure-chat snapshot contains project memory");
            }
            MemorySnapshotOwner::Project { workspace_id, .. } if workspace_id.trim().is_empty() => {
                return invalid_snapshot("project snapshot workspace_id is empty");
            }
            _ => {}
        }
        if self.snapshot_hash.trim().is_empty() {
            return invalid_snapshot("snapshot_hash is empty");
        }
        Ok(())
    }
}

fn validate_snapshot_entries(entries: &[MemorySnapshotEntry]) -> Result<u32, MemoryContractError> {
    if entries.len() > MEMORY_ACTIVE_ENTRIES_PER_SCOPE_CAP {
        return invalid_snapshot("snapshot scope exceeds the entry cap");
    }
    let mut total = 0;
    for entry in entries {
        let chars = entry.content.chars().count();
        if !(1..=MEMORY_ENTRY_CHAR_CAP).contains(&chars) {
            return invalid_snapshot("snapshot entry content must be 1..=1000 characters");
        }
        total += chars as u32;
    }
    Ok(total)
}

fn invalid_snapshot<T>(message: &'static str) -> Result<T, MemoryContractError> {
    Err(MemoryContractError::new(
        MemoryContractErrorCode::InvalidSnapshot,
        message,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryLoadDisabledReason {
    FeatureDisabled,
    WorkspaceOff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemorySnapshotLoadOutcome {
    Disabled { reason: MemoryLoadDisabledReason },
    Ready { snapshot: MemorySnapshot },
    Unavailable { error_code: MemoryMutationErrorCode },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RunContext {
    pub source: FrozenReviewSource,
    pub global_memory_generation: u64,
    pub memory: MemorySnapshotLoadOutcome,
}

impl RunContext {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        let MemorySnapshotLoadOutcome::Ready { snapshot } = &self.memory else {
            return Ok(());
        };
        snapshot.validate()?;
        if snapshot.global_generation != self.global_memory_generation {
            return invalid_snapshot("snapshot global generation does not match run context");
        }
        match (&self.source, &snapshot.owner) {
            (FrozenReviewSource::PureChat { .. }, MemorySnapshotOwner::PureChat) => Ok(()),
            (
                FrozenReviewSource::Project {
                    workspace_id,
                    workspace_memory_generation,
                    ..
                },
                MemorySnapshotOwner::Project {
                    workspace_id: snapshot_workspace_id,
                    workspace_memory_generation: snapshot_generation,
                },
            ) if workspace_id == snapshot_workspace_id
                && workspace_memory_generation == snapshot_generation =>
            {
                Ok(())
            }
            _ => invalid_snapshot("snapshot owner does not match frozen source"),
        }
    }
}

/// Child runs inherit only snapshot owner, generations, and content. This seed
/// cannot carry parent containment, decision, proof, or injection events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct FrozenChildMemorySeed {
    pub snapshot: MemorySnapshot,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectNotificationMode {
    Off,
    #[default]
    On,
    Verbose,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ReviewerSelection {
    pub provider_name: String,
    pub model: String,
}

impl ReviewerSelection {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        if self.provider_name.trim().is_empty() {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidReviewer,
                "reviewer provider_name must not be empty",
            ));
        }
        if self.model.trim().is_empty() {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidReviewer,
                "reviewer model must not be empty",
            ));
        }
        Ok(())
    }
}

/// Internal persistence record. High-water and cleanup epoch values must never
/// cross the wire, so this type intentionally has no serde implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryReviewSettings {
    pub enabled: bool,
    pub reviewer: Option<ReviewerSelection>,
    pub trigger_every_turns: u32,
    pub explicit_remember_immediate: bool,
    pub project_notification_mode: ProjectNotificationMode,
    pub version: u64,
    pub review_generation: u64,
    pub retention_time_high_watermark: DateTime<Utc>,
    pub physical_cleanup_pending: bool,
    pub physical_cleanup_epoch: u64,
    pub updated_at: DateTime<Utc>,
}

impl MemoryReviewSettings {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        validate_settings(
            self.enabled,
            self.reviewer.as_ref(),
            self.trigger_every_turns,
        )
    }

    pub fn validate_next(&self, next: &Self) -> Result<(), MemoryContractError> {
        self.validate()?;
        next.validate()?;
        let visible_config_changed = self.enabled != next.enabled
            || self.reviewer != next.reviewer
            || self.trigger_every_turns != next.trigger_every_turns
            || self.explicit_remember_immediate != next.explicit_remember_immediate
            || self.project_notification_mode != next.project_notification_mode;
        let review_selection_changed =
            self.enabled != next.enabled || self.reviewer != next.reviewer;
        let invalid = next.version < self.version
            || next.review_generation < self.review_generation
            || next.physical_cleanup_epoch < self.physical_cleanup_epoch
            || next.retention_time_high_watermark < self.retention_time_high_watermark
            || next.updated_at < self.updated_at
            || visible_config_changed && next.version <= self.version
            || review_selection_changed && next.review_generation <= self.review_generation
            || !self.physical_cleanup_pending
                && next.physical_cleanup_pending
                && next.physical_cleanup_epoch <= self.physical_cleanup_epoch
            || next.physical_cleanup_epoch > self.physical_cleanup_epoch
                && !next.physical_cleanup_pending
            || self.physical_cleanup_pending
                && !next.physical_cleanup_pending
                && next.physical_cleanup_epoch != self.physical_cleanup_epoch;
        if invalid {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidSettingsTransition,
                "invalid memory review settings transition",
            ));
        }
        Ok(())
    }

    pub fn to_view(&self) -> MemoryReviewSettingsView {
        MemoryReviewSettingsView {
            enabled: self.enabled,
            reviewer: self.reviewer.clone(),
            trigger_every_turns: self.trigger_every_turns,
            explicit_remember_immediate: self.explicit_remember_immediate,
            project_notification_mode: self.project_notification_mode,
            version: self.version,
            review_generation: self.review_generation,
            physical_cleanup_pending: self.physical_cleanup_pending,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewSettingsView {
    pub enabled: bool,
    pub reviewer: Option<ReviewerSelection>,
    pub trigger_every_turns: u32,
    pub explicit_remember_immediate: bool,
    pub project_notification_mode: ProjectNotificationMode,
    pub version: u64,
    pub review_generation: u64,
    pub physical_cleanup_pending: bool,
    pub updated_at: DateTime<Utc>,
}

impl Default for MemoryReviewSettingsView {
    fn default() -> Self {
        Self {
            enabled: false,
            reviewer: None,
            trigger_every_turns: MEMORY_TRIGGER_TURNS_DEFAULT,
            explicit_remember_immediate: true,
            project_notification_mode: ProjectNotificationMode::On,
            version: 0,
            review_generation: 0,
            physical_cleanup_pending: false,
            updated_at: DateTime::<Utc>::default(),
        }
    }
}

impl MemoryReviewSettingsView {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        validate_settings(
            self.enabled,
            self.reviewer.as_ref(),
            self.trigger_every_turns,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewSettingsUpdate {
    pub expected_version: u64,
    pub enabled: bool,
    pub reviewer: Option<ReviewerSelection>,
    pub trigger_every_turns: u32,
    pub explicit_remember_immediate: bool,
    pub project_notification_mode: ProjectNotificationMode,
}

impl MemoryReviewSettingsUpdate {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        validate_settings(
            self.enabled,
            self.reviewer.as_ref(),
            self.trigger_every_turns,
        )
    }
}

fn validate_settings(
    enabled: bool,
    reviewer: Option<&ReviewerSelection>,
    trigger_every_turns: u32,
) -> Result<(), MemoryContractError> {
    if !(MEMORY_TRIGGER_TURNS_MIN..=MEMORY_TRIGGER_TURNS_MAX).contains(&trigger_every_turns) {
        return Err(MemoryContractError::new(
            MemoryContractErrorCode::CadenceOutOfRange,
            "review cadence must be between 5 and 50 turns",
        ));
    }
    if let Some(selection) = reviewer {
        selection.validate()?;
    } else if enabled {
        return Err(MemoryContractError::new(
            MemoryContractErrorCode::ReviewerRequired,
            "enabled memory review requires a reviewer",
        ));
    }
    Ok(())
}

/// Positive database sequence encoded as a JSON string to remain exact in JS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SequenceCursor(i64);

impl SequenceCursor {
    pub fn new(value: i64) -> Result<Self, MemoryContractError> {
        if value <= 0 {
            return Err(invalid_cursor("sequence cursor must be positive"));
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> i64 {
        self.0
    }
}

impl fmt::Display for SequenceCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for SequenceCursor {
    type Err = MemoryContractError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid_cursor(
                "sequence cursor must be an unsigned decimal string",
            ));
        }
        if value.len() > 1 && value.starts_with('0') {
            return Err(invalid_cursor(
                "sequence cursor must use canonical decimal encoding without leading zeros",
            ));
        }
        let parsed = value
            .parse::<i64>()
            .map_err(|_| invalid_cursor("sequence cursor exceeds the signed 64-bit range"))?;
        Self::new(parsed)
    }
}

impl Serialize for SequenceCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SequenceCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(SequenceCursorVisitor)
    }
}

struct SequenceCursorVisitor;

impl Visitor<'_> for SequenceCursorVisitor {
    type Value = SequenceCursor;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a positive signed-64-bit decimal JSON string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        value.parse().map_err(E::custom)
    }
}

fn invalid_cursor(message: &'static str) -> MemoryContractError {
    MemoryContractError::new(MemoryContractErrorCode::InvalidCursor, message)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SequencePage<T> {
    pub items: Vec<T>,
    pub next_before_sequence: Option<SequenceCursor>,
}

/// A current, active memory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryEntry {
    pub id: String,
    pub owner: MemoryOwner,
    pub kind: MemoryKind,
    pub content: String,
    pub normalized_hash: String,
    pub version: u64,
    pub pinned: bool,
    pub source_job_id: Option<String>,
    pub source_candidate_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryRevisionAction {
    Edit,
    AutomaticReplace,
    Undo,
}

/// The complete pre-change value needed to audit or undo an entry mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryEntryRevision {
    pub sequence: SequenceCursor,
    pub id: String,
    pub entry_id: String,
    pub prior_kind: MemoryKind,
    pub prior_content: String,
    pub prior_normalized_hash: String,
    pub prior_version: u64,
    pub prior_pinned: bool,
    pub action: MemoryRevisionAction,
    pub source_job_id: Option<String>,
    pub source_candidate_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryCandidateMutation {
    Add,
    Replace {
        target_entry_id: String,
        target_version: u64,
    },
}

/// Pending candidates alone retain proposal body text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryCandidateState {
    Pending {
        proposed_content: String,
        reason: String,
    },
    Approved {
        resolved_at: DateTime<Utc>,
    },
    Rejected {
        resolved_at: DateTime<Utc>,
    },
    Expired {
        resolved_at: DateTime<Utc>,
    },
    Superseded {
        resolved_at: DateTime<Utc>,
    },
}

/// A global candidate. `source_run_id` is the last run at the frozen inclusive
/// trigger boundary, not necessarily the run that first introduced the fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryCandidate {
    pub sequence: SequenceCursor,
    pub id: String,
    pub kind: MemoryKind,
    pub mutation: MemoryCandidateMutation,
    pub source_task_id: String,
    pub source_workspace_id: Option<String>,
    pub source_run_id: String,
    pub captured_at: DateTime<Utc>,
    pub source_job_id: Option<String>,
    pub proposal_index: u32,
    pub proposal_hash: String,
    pub reason_hash: String,
    pub confidence: f64,
    pub state: MemoryCandidateState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MemoryCandidate {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        if self.proposal_index >= MEMORY_REVIEW_PROPOSAL_CAP as u32 {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidCandidateState,
                "candidate proposal_index exceeds the review proposal cap",
            ));
        }
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidConfidence,
                "candidate confidence must be finite and between 0 and 1",
            ));
        }
        if let MemoryCandidateState::Pending {
            proposed_content,
            reason,
        } = &self.state
        {
            let content_chars = proposed_content.chars().count();
            if !(MEMORY_PROPOSAL_CONTENT_MIN_CHARS..=MEMORY_ENTRY_CHAR_CAP).contains(&content_chars)
            {
                return Err(MemoryContractError::new(
                    MemoryContractErrorCode::InvalidCandidateState,
                    "pending candidate content must contain between 5 and 1000 characters",
                ));
            }
            if reason.chars().count() > MEMORY_PROPOSAL_REASON_CHAR_CAP {
                return Err(MemoryContractError::new(
                    MemoryContractErrorCode::InvalidCandidateState,
                    "pending candidate reason must not exceed 300 characters",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReviewTurnScrubReason {
    ReviewSucceeded,
    Cancelled,
    Invalidated,
    RetentionExpired,
    CapacityEvicted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReviewTurnBody {
    Available {
        user_text: String,
        assistant_text: String,
    },
    Scrubbed {
        scrubbed_at: DateTime<Utc>,
        reason: MemoryReviewTurnScrubReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewTurn {
    pub sequence: SequenceCursor,
    pub id: String,
    pub source: FrozenReviewSource,
    pub captured_at: DateTime<Utc>,
    pub global_generation: u64,
    pub body: MemoryReviewTurnBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReviewTrigger {
    Cadence,
    Manual,
    ExplicitRemember,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReviewCancellationReason {
    UserRequested,
    MemoryDisabled,
    SelectionChanged,
    SourceExpired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReviewJobState {
    Queued {
        queued_at: DateTime<Utc>,
    },
    Running {
        started_at: DateTime<Utc>,
    },
    Succeeded {
        completed_at: DateTime<Utc>,
        input_hash: String,
        turn_count: u32,
        proposal_count: u32,
    },
    Failed {
        failed_at: DateTime<Utc>,
        error_code: MemoryMutationErrorCode,
    },
    Interrupted {
        interrupted_at: DateTime<Utc>,
        error_code: MemoryMutationErrorCode,
    },
    Cancelled {
        cancelled_at: DateTime<Utc>,
        reason: MemoryReviewCancellationReason,
    },
}

impl MemoryReviewJobState {
    pub fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Queued { .. },
                Self::Running { .. } | Self::Cancelled { .. }
            ) | (
                Self::Running { .. },
                Self::Succeeded { .. }
                    | Self::Failed { .. }
                    | Self::Interrupted { .. }
                    | Self::Cancelled { .. }
            ) | (
                Self::Failed { .. } | Self::Interrupted { .. },
                Self::Queued { .. } | Self::Cancelled { .. }
            )
        )
    }

    pub fn validate_next_attempt(
        &self,
        next: &Self,
        current_attempt: u32,
        next_attempt: u32,
    ) -> Result<(), MemoryContractError> {
        if !self.can_transition_to(next) {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidTransition,
                "invalid memory review job transition",
            ));
        }
        let starts_attempt = matches!((self, next), (Self::Queued { .. }, Self::Running { .. }));
        let expected = if starts_attempt {
            current_attempt.checked_add(1).ok_or_else(|| {
                MemoryContractError::new(
                    MemoryContractErrorCode::InvalidAttempt,
                    "memory review job attempt overflow",
                )
            })?
        } else {
            current_attempt
        };
        if next_attempt != expected {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidAttempt,
                "memory review job attempt does not match its transition",
            ));
        }
        Ok(())
    }

    fn validate_next_recovery_count(
        &self,
        next: &Self,
        current_recovery_count: u32,
        next_recovery_count: u32,
    ) -> Result<(), MemoryContractError> {
        let recovers = matches!(
            (self, next),
            (
                Self::Failed { .. } | Self::Interrupted { .. },
                Self::Queued { .. }
            )
        );
        let expected = if recovers {
            current_recovery_count.checked_add(1).ok_or_else(|| {
                MemoryContractError::new(
                    MemoryContractErrorCode::InvalidAttempt,
                    "memory review job recovery count overflow",
                )
            })?
        } else {
            current_recovery_count
        };
        if next_recovery_count != expected {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidAttempt,
                "memory review job recovery count does not match its transition",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewJob {
    pub sequence: SequenceCursor,
    pub id: String,
    pub source: FrozenReviewSource,
    pub review_generation: u64,
    pub reviewer: ReviewerSelection,
    pub inclusive_boundary: SequenceCursor,
    pub attempt: u32,
    pub recovery_count: u32,
    pub suppressed_turn_count: u32,
    pub trigger: MemoryReviewTrigger,
    pub state: MemoryReviewJobState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MemoryReviewJob {
    pub fn validate_next_attempt(&self, next: &Self) -> Result<(), MemoryContractError> {
        self.state
            .validate_next_attempt(&next.state, self.attempt, next.attempt)?;
        self.state.validate_next_recovery_count(
            &next.state,
            self.recovery_count,
            next.recovery_count,
        )?;

        if self.sequence != next.sequence
            || self.id != next.id
            || self.source != next.source
            || self.review_generation != next.review_generation
            || self.reviewer != next.reviewer
            || self.inclusive_boundary != next.inclusive_boundary
            || self.trigger != next.trigger
            || self.created_at != next.created_at
        {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidJobMutation,
                "immutable memory review job fields must not change between attempts",
            ));
        }
        if next.updated_at < self.updated_at {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidJobMutation,
                "memory review job updated_at must not decrease",
            ));
        }
        if next.suppressed_turn_count < self.suppressed_turn_count {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidJobMutation,
                "memory review job suppressed_turn_count must not decrease",
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum GlobalCandidateOutcome {
    Pending,
    Duplicate,
    Rejected,
    SensitiveRejected,
    StaleTarget,
    CapacityExceeded,
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectEntryOutcome {
    Applied,
    Duplicate,
    Rejected,
    SensitiveRejected,
    StaleTarget,
    CapacityExceeded,
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SkippedOutcome {
    Rejected,
    SensitiveRejected,
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReviewOutcomeEffect {
    GlobalCandidate { result: GlobalCandidateOutcome },
    ProjectEntry { result: ProjectEntryOutcome },
    Skipped { result: SkippedOutcome },
}

/// Metadata-only review result; proposal body text never enters this record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewOutcome {
    pub sequence: SequenceCursor,
    pub job_id: String,
    pub proposal_index: u32,
    pub effect: MemoryReviewOutcomeEffect,
    pub entry_id: Option<String>,
    pub candidate_id: Option<String>,
    pub error_code: Option<MemoryMutationErrorCode>,
    pub created_at: DateTime<Utc>,
}

impl MemoryReviewOutcome {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        let empty_id = |id: Option<&str>| id.is_some_and(|value| value.trim().is_empty());
        let invalid_shape = match self.effect {
            MemoryReviewOutcomeEffect::GlobalCandidate { .. } => self.entry_id.is_some(),
            MemoryReviewOutcomeEffect::ProjectEntry { .. } => self.candidate_id.is_some(),
            MemoryReviewOutcomeEffect::Skipped { .. } => {
                self.entry_id.is_some() || self.candidate_id.is_some()
            }
        };
        if self.proposal_index >= MEMORY_REVIEW_PROPOSAL_CAP as u32
            || self.job_id.trim().is_empty()
            || empty_id(self.entry_id.as_deref())
            || empty_id(self.candidate_id.as_deref())
            || invalid_shape
        {
            return Err(MemoryContractError::new(
                MemoryContractErrorCode::InvalidReviewOutcome,
                "invalid memory review outcome",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryInjectionStatus {
    Recorded,
    AbortedBeforePublish,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryInjectionRecord {
    pub run_id: String,
    pub status: MemoryInjectionStatus,
    pub snapshot_hash: String,
    pub global_entry_refs: Vec<MemoryEntryRef>,
    pub project_entry_refs: Vec<MemoryEntryRef>,
    pub global_chars: u32,
    pub project_chars: u32,
    pub created_at: DateTime<Utc>,
}

impl MemoryInjectionRecord {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        if self.run_id.trim().is_empty() || self.snapshot_hash.trim().is_empty() {
            return invalid_injection("injection run_id and snapshot_hash must not be empty");
        }
        if self.global_entry_refs.len() > MEMORY_ACTIVE_ENTRIES_PER_SCOPE_CAP
            || self.project_entry_refs.len() > MEMORY_ACTIVE_ENTRIES_PER_SCOPE_CAP
            || self.global_chars as usize > MEMORY_GLOBAL_CHAR_CAP
            || self.project_chars as usize > MEMORY_PROJECT_CHAR_CAP
        {
            return invalid_injection("injection counts exceed their scope caps");
        }
        Ok(())
    }
}

fn invalid_injection<T>(message: &'static str) -> Result<T, MemoryContractError> {
    Err(MemoryContractError::new(
        MemoryContractErrorCode::InvalidInjection,
        message,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SensitiveMemoryDisclosure {
    MainAndReviewerProvidersV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SensitiveMemoryMutation {
    Add,
    Edit,
    ApproveCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SensitiveMemoryConfirmation {
    pub disclosure: SensitiveMemoryDisclosure,
    pub mutation: SensitiveMemoryMutation,
    pub content_hash: String,
    pub owner: MemoryOwner,
    pub target_entry_id: Option<String>,
    pub expected_version: Option<u64>,
}

impl SensitiveMemoryConfirmation {
    pub fn validate(&self) -> Result<(), MemoryContractError> {
        if self.content_hash.trim().is_empty() {
            return invalid_confirmation("sensitive-memory content_hash is empty");
        }
        let has_target = self.target_entry_id.is_some();
        let has_version = self.expected_version.is_some();
        let valid = match self.mutation {
            SensitiveMemoryMutation::Add => !has_target && !has_version,
            SensitiveMemoryMutation::Edit => has_target && has_version,
            SensitiveMemoryMutation::ApproveCandidate => {
                matches!(
                    self.owner,
                    MemoryOwner::Global {
                        authorization: GlobalMemoryAuthorization::ApprovedCandidate
                    }
                ) && has_target == has_version
            }
        };
        if !valid {
            return invalid_confirmation("sensitive-memory confirmation mismatches mutation");
        }
        Ok(())
    }
}

fn invalid_confirmation<T>(message: &'static str) -> Result<T, MemoryContractError> {
    Err(MemoryContractError::new(
        MemoryContractErrorCode::InvalidConfirmation,
        message,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryMutationErrorCode {
    ProviderUnavailable,
    ProviderRequestFailed,
    InvalidReviewOutput,
    ReviewInterrupted,
    ProposalRejected,
    SensitiveMemoryRejected,
    StaleTarget,
    CapacityExceeded,
    MemoryDisabled,
    SelectionChanged,
    StorageUnavailable,
    MemorySnapshotUnavailable,
    VersionConflict,
    NotFound,
    PhysicalCleanupPending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryMutationError {
    pub code: MemoryMutationErrorCode,
    pub message: String,
    pub current_version: Option<u64>,
}

impl fmt::Display for MemoryMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MemoryMutationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryContractErrorCode {
    InvalidReviewer,
    ReviewerRequired,
    CadenceOutOfRange,
    InvalidSettingsTransition,
    InvalidIdentity,
    InvalidCursor,
    InvalidTransition,
    InvalidAttempt,
    InvalidJobMutation,
    InvalidCandidateState,
    InvalidConfidence,
    InvalidSnapshot,
    InvalidInjection,
    InvalidConfirmation,
    InvalidReviewOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryContractError {
    pub code: MemoryContractErrorCode,
    pub message: String,
}

impl MemoryContractError {
    pub fn new(code: MemoryContractErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub const fn code(&self) -> MemoryContractErrorCode {
        self.code
    }
}

impl fmt::Display for MemoryContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MemoryContractError {}

mod provider_wire;
pub use provider_wire::*;
