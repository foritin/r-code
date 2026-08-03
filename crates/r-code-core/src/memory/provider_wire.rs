use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
};

use serde::{Deserialize, Serialize};

use super::{
    FrozenReviewSource, MemoryKind, MemoryOwner, MemoryScope, SequenceCursor,
    MEMORY_ACTIVE_ENTRIES_PER_SCOPE_CAP, MEMORY_ENTRY_CHAR_CAP, MEMORY_GLOBAL_CHAR_CAP,
    MEMORY_PROJECT_CHAR_CAP, MEMORY_PROPOSAL_CONTENT_MIN_CHARS, MEMORY_PROPOSAL_REASON_CHAR_CAP,
    MEMORY_REVIEW_PROPOSAL_CAP, MEMORY_REVIEW_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReviewContext {
    PureChat,
    CurrentProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewWireTurn {
    pub evidence_ordinal: NonZeroU32,
    pub user_text: String,
    pub assistant_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewToolCount {
    pub tool_name: String,
    pub success_count: u32,
    pub failure_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewWireEntry {
    pub memory_ordinal: NonZeroU32,
    pub kind: MemoryKind,
    pub content: String,
    pub version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewScopeUsage {
    pub global_chars: u32,
    pub project_chars: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewScopeCaps {
    pub global_chars: u32,
    pub project_chars: u32,
    pub entry_chars: u32,
    pub max_entries: u32,
    pub max_proposals: u32,
}

impl Default for MemoryReviewScopeCaps {
    fn default() -> Self {
        Self {
            global_chars: MEMORY_GLOBAL_CHAR_CAP as u32,
            project_chars: MEMORY_PROJECT_CHAR_CAP as u32,
            entry_chars: MEMORY_ENTRY_CHAR_CAP as u32,
            max_entries: MEMORY_ACTIVE_ENTRIES_PER_SCOPE_CAP as u32,
            max_proposals: MEMORY_REVIEW_PROPOSAL_CAP as u32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewInput {
    pub schema_version: u32,
    pub context: MemoryReviewContext,
    pub turns: Vec<MemoryReviewWireTurn>,
    pub tool_counts: Vec<MemoryReviewToolCount>,
    pub global_entries: Vec<MemoryReviewWireEntry>,
    pub project_entries: Vec<MemoryReviewWireEntry>,
    pub scope_usage: MemoryReviewScopeUsage,
    pub scope_caps: MemoryReviewScopeCaps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReviewInputErrorCode {
    UnsupportedSchemaVersion,
    ProjectDataInPureChat,
    UnexpectedScopeCaps,
    DuplicateEvidenceOrdinal,
    NonContiguousEvidenceOrdinal,
    DuplicateMemoryOrdinal,
    NonContiguousMemoryOrdinal,
    InvalidEntryContent,
    ScopeUsageMismatch,
    ScopeCapacityExceeded,
    InvalidToolName,
    OrdinalMapMismatch,
    DuplicateTurnSequence,
    TurnSequenceBeyondBoundary,
    TargetVersionMismatch,
    SourceOwnerMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewInputError {
    pub code: MemoryReviewInputErrorCode,
}

impl MemoryReviewInput {
    pub fn validate(&self) -> Result<(), MemoryReviewInputError> {
        if self.schema_version != MEMORY_REVIEW_SCHEMA_VERSION {
            return input_error(MemoryReviewInputErrorCode::UnsupportedSchemaVersion);
        }
        if self.scope_caps != MemoryReviewScopeCaps::default() {
            return input_error(MemoryReviewInputErrorCode::UnexpectedScopeCaps);
        }
        if self.context == MemoryReviewContext::PureChat
            && (!self.project_entries.is_empty() || self.scope_usage.project_chars != 0)
        {
            return input_error(MemoryReviewInputErrorCode::ProjectDataInPureChat);
        }
        validate_turn_ordinals(&self.turns)?;
        validate_memory_entries(self)?;
        if self.tool_counts.iter().any(|count| {
            count.tool_name.trim().is_empty() || count.tool_name.trim() != count.tool_name
        }) {
            return input_error(MemoryReviewInputErrorCode::InvalidToolName);
        }
        Ok(())
    }
}

fn validate_turn_ordinals(turns: &[MemoryReviewWireTurn]) -> Result<(), MemoryReviewInputError> {
    let mut ordinals = BTreeSet::new();
    for turn in turns {
        if !ordinals.insert(turn.evidence_ordinal) {
            return input_error(MemoryReviewInputErrorCode::DuplicateEvidenceOrdinal);
        }
    }
    if !ordinals_are_contiguous(&ordinals) {
        return input_error(MemoryReviewInputErrorCode::NonContiguousEvidenceOrdinal);
    }
    Ok(())
}

fn validate_memory_entries(input: &MemoryReviewInput) -> Result<(), MemoryReviewInputError> {
    let mut ordinals = BTreeSet::new();
    let mut global_chars = 0usize;
    let mut project_chars = 0usize;
    for (entries, total) in [
        (&input.global_entries, &mut global_chars),
        (&input.project_entries, &mut project_chars),
    ] {
        for entry in entries {
            if !ordinals.insert(entry.memory_ordinal) {
                return input_error(MemoryReviewInputErrorCode::DuplicateMemoryOrdinal);
            }
            let chars = entry.content.chars().count();
            if !(1..=MEMORY_ENTRY_CHAR_CAP).contains(&chars) {
                return input_error(MemoryReviewInputErrorCode::InvalidEntryContent);
            }
            *total += chars;
        }
    }
    if !ordinals_are_contiguous(&ordinals) {
        return input_error(MemoryReviewInputErrorCode::NonContiguousMemoryOrdinal);
    }
    validate_scope_totals(input, global_chars, project_chars)
}

fn validate_scope_totals(
    input: &MemoryReviewInput,
    global_chars: usize,
    project_chars: usize,
) -> Result<(), MemoryReviewInputError> {
    if global_chars != input.scope_usage.global_chars as usize
        || project_chars != input.scope_usage.project_chars as usize
    {
        return input_error(MemoryReviewInputErrorCode::ScopeUsageMismatch);
    }
    if global_chars > MEMORY_GLOBAL_CHAR_CAP
        || project_chars > MEMORY_PROJECT_CHAR_CAP
        || input.global_entries.len() > MEMORY_ACTIVE_ENTRIES_PER_SCOPE_CAP
        || input.project_entries.len() > MEMORY_ACTIVE_ENTRIES_PER_SCOPE_CAP
    {
        return input_error(MemoryReviewInputErrorCode::ScopeCapacityExceeded);
    }
    Ok(())
}

fn ordinals_are_contiguous(ordinals: &BTreeSet<NonZeroU32>) -> bool {
    ordinals
        .iter()
        .zip(1_u32..)
        .all(|(ordinal, expected)| ordinal.get() == expected)
}

fn input_error<T>(code: MemoryReviewInputErrorCode) -> Result<T, MemoryReviewInputError> {
    Err(MemoryReviewInputError { code })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryProposalScope {
    Global,
    Project,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryProposalOperation {
    Add,
    Replace,
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryProposalBasis {
    ExplicitUser,
    VerifiedResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewProposal {
    pub scope: MemoryProposalScope,
    pub kind: MemoryKind,
    pub operation: MemoryProposalOperation,
    pub target_memory_ordinal: Option<NonZeroU32>,
    pub target_version: Option<u64>,
    pub content: Option<String>,
    pub reason: String,
    pub basis: MemoryProposalBasis,
    pub evidence_ordinals: Vec<NonZeroU32>,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryReviewOutput {
    pub proposals: Vec<MemoryReviewProposal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryProposalValidationCode {
    TooManyProposals,
    ProjectScopeInPureChat,
    InvalidOperationShape,
    InvalidContent,
    InvalidReason,
    InvalidConfidence,
    MissingEvidence,
    InvalidEvidenceOrdinal,
    DuplicateEvidenceOrdinal,
    InvalidTargetOrdinal,
    TargetScopeMismatch,
    TargetVersionMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryProposalValidationError {
    pub proposal_index: u32,
    pub code: MemoryProposalValidationCode,
}

impl MemoryReviewOutput {
    pub fn validate(
        &self,
        input: &MemoryReviewInput,
    ) -> Result<Vec<Result<(), MemoryProposalValidationError>>, MemoryProposalValidationError> {
        if self.proposals.len() > MEMORY_REVIEW_PROPOSAL_CAP {
            return Err(proposal_error(
                0,
                MemoryProposalValidationCode::TooManyProposals,
            ));
        }
        Ok(self
            .proposals
            .iter()
            .enumerate()
            .map(|(index, proposal)| validate_proposal(proposal, input, index as u32))
            .collect())
    }
}

fn validate_proposal(
    proposal: &MemoryReviewProposal,
    input: &MemoryReviewInput,
    index: u32,
) -> Result<(), MemoryProposalValidationError> {
    if input.context == MemoryReviewContext::PureChat
        && proposal.scope == MemoryProposalScope::Project
    {
        return proposal_invalid(index, MemoryProposalValidationCode::ProjectScopeInPureChat);
    }
    if !operation_shape_is_valid(proposal) {
        return proposal_invalid(index, MemoryProposalValidationCode::InvalidOperationShape);
    }
    if proposal.content.as_ref().is_some_and(|content| {
        !(MEMORY_PROPOSAL_CONTENT_MIN_CHARS..=MEMORY_ENTRY_CHAR_CAP)
            .contains(&content.chars().count())
    }) {
        return proposal_invalid(index, MemoryProposalValidationCode::InvalidContent);
    }
    validate_target(proposal, input, index)?;
    if proposal.reason.chars().count() > MEMORY_PROPOSAL_REASON_CHAR_CAP {
        return proposal_invalid(index, MemoryProposalValidationCode::InvalidReason);
    }
    if !proposal.confidence.is_finite() || !(0.0..=1.0).contains(&proposal.confidence) {
        return proposal_invalid(index, MemoryProposalValidationCode::InvalidConfidence);
    }
    validate_evidence(proposal, input, index)
}

fn operation_shape_is_valid(proposal: &MemoryReviewProposal) -> bool {
    if proposal.scope == MemoryProposalScope::Skip
        && proposal.operation != MemoryProposalOperation::Noop
    {
        return false;
    }
    match proposal.operation {
        MemoryProposalOperation::Add => {
            proposal.target_memory_ordinal.is_none()
                && proposal.target_version.is_none()
                && proposal.content.is_some()
        }
        MemoryProposalOperation::Replace => {
            proposal.target_memory_ordinal.is_some()
                && proposal.target_version.is_some()
                && proposal.content.is_some()
        }
        MemoryProposalOperation::Noop => {
            proposal.target_memory_ordinal.is_none()
                && proposal.target_version.is_none()
                && proposal.content.is_none()
        }
    }
}

fn validate_target(
    proposal: &MemoryReviewProposal,
    input: &MemoryReviewInput,
    index: u32,
) -> Result<(), MemoryProposalValidationError> {
    let Some(target) = proposal.target_memory_ordinal else {
        return Ok(());
    };
    let global_entry = input
        .global_entries
        .iter()
        .find(|entry| entry.memory_ordinal == target);
    let project_entry = input
        .project_entries
        .iter()
        .find(|entry| entry.memory_ordinal == target);
    let Some(target_entry) = global_entry.or(project_entry) else {
        return proposal_invalid(index, MemoryProposalValidationCode::InvalidTargetOrdinal);
    };
    let scope_matches = matches!(proposal.scope, MemoryProposalScope::Global)
        && global_entry.is_some()
        || matches!(proposal.scope, MemoryProposalScope::Project) && project_entry.is_some();
    if !scope_matches {
        return proposal_invalid(index, MemoryProposalValidationCode::TargetScopeMismatch);
    }
    if proposal.target_version != Some(target_entry.version) {
        return proposal_invalid(index, MemoryProposalValidationCode::TargetVersionMismatch);
    }
    Ok(())
}

fn validate_evidence(
    proposal: &MemoryReviewProposal,
    input: &MemoryReviewInput,
    index: u32,
) -> Result<(), MemoryProposalValidationError> {
    if proposal.evidence_ordinals.is_empty() {
        return proposal_invalid(index, MemoryProposalValidationCode::MissingEvidence);
    }
    let mut seen = BTreeSet::new();
    for ordinal in &proposal.evidence_ordinals {
        if !seen.insert(*ordinal) {
            return proposal_invalid(
                index,
                MemoryProposalValidationCode::DuplicateEvidenceOrdinal,
            );
        }
        if !input
            .turns
            .iter()
            .any(|turn| turn.evidence_ordinal == *ordinal)
        {
            return proposal_invalid(index, MemoryProposalValidationCode::InvalidEvidenceOrdinal);
        }
    }
    Ok(())
}

const fn proposal_error(
    proposal_index: u32,
    code: MemoryProposalValidationCode,
) -> MemoryProposalValidationError {
    MemoryProposalValidationError {
        proposal_index,
        code,
    }
}

fn proposal_invalid<T>(
    proposal_index: u32,
    code: MemoryProposalValidationCode,
) -> Result<T, MemoryProposalValidationError> {
    Err(proposal_error(proposal_index, code))
}

pub struct HostMemoryTarget {
    pub entry_id: String,
    pub owner: MemoryOwner,
    pub version: u64,
}

pub struct HostReviewAssembly {
    pub source: FrozenReviewSource,
    pub inclusive_boundary: SequenceCursor,
    pub evidence_ordinal_to_turn_sequence: BTreeMap<NonZeroU32, SequenceCursor>,
    pub memory_ordinal_to_target: BTreeMap<NonZeroU32, HostMemoryTarget>,
    pub wire: MemoryReviewInput,
}

impl HostReviewAssembly {
    pub fn validate_maps(&self) -> Result<(), MemoryReviewInputError> {
        self.wire.validate()?;
        if self.source.validate().is_err()
            || !source_matches_context(&self.source, self.wire.context)
        {
            return input_error(MemoryReviewInputErrorCode::SourceOwnerMismatch);
        }
        let evidence_ordinals = self
            .wire
            .turns
            .iter()
            .map(|turn| turn.evidence_ordinal)
            .collect::<BTreeSet<_>>();
        let memory_ordinals = self
            .wire
            .global_entries
            .iter()
            .chain(&self.wire.project_entries)
            .map(|entry| entry.memory_ordinal)
            .collect::<BTreeSet<_>>();
        if self
            .evidence_ordinal_to_turn_sequence
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            != evidence_ordinals
            || self
                .memory_ordinal_to_target
                .keys()
                .copied()
                .collect::<BTreeSet<_>>()
                != memory_ordinals
        {
            return input_error(MemoryReviewInputErrorCode::OrdinalMapMismatch);
        }
        self.validate_evidence_sequences()?;
        self.validate_memory_targets()
    }

    fn validate_evidence_sequences(&self) -> Result<(), MemoryReviewInputError> {
        let sequences = self
            .evidence_ordinal_to_turn_sequence
            .values()
            .copied()
            .collect::<BTreeSet<_>>();
        if sequences.len() != self.evidence_ordinal_to_turn_sequence.len() {
            return input_error(MemoryReviewInputErrorCode::DuplicateTurnSequence);
        }
        if sequences
            .iter()
            .any(|sequence| *sequence > self.inclusive_boundary)
        {
            return input_error(MemoryReviewInputErrorCode::TurnSequenceBeyondBoundary);
        }
        Ok(())
    }

    fn validate_memory_targets(&self) -> Result<(), MemoryReviewInputError> {
        for (scope, entries) in [
            (MemoryScope::Global, &self.wire.global_entries),
            (MemoryScope::Project, &self.wire.project_entries),
        ] {
            for entry in entries {
                let target = &self.memory_ordinal_to_target[&entry.memory_ordinal];
                if target.version != entry.version {
                    return input_error(MemoryReviewInputErrorCode::TargetVersionMismatch);
                }
                if target.entry_id.trim().is_empty()
                    || target.owner.validate().is_err()
                    || !owner_matches_source(&target.owner, scope, &self.source)
                {
                    return input_error(MemoryReviewInputErrorCode::SourceOwnerMismatch);
                }
            }
        }
        Ok(())
    }
}

fn source_matches_context(source: &FrozenReviewSource, context: MemoryReviewContext) -> bool {
    matches!(
        (source, context),
        (
            FrozenReviewSource::PureChat { .. },
            MemoryReviewContext::PureChat
        ) | (
            FrozenReviewSource::Project { .. },
            MemoryReviewContext::CurrentProject
        )
    )
}

fn owner_matches_source(
    owner: &MemoryOwner,
    expected_scope: MemoryScope,
    source: &FrozenReviewSource,
) -> bool {
    match (owner, expected_scope, source) {
        (MemoryOwner::Global { .. }, MemoryScope::Global, _) => true,
        (
            MemoryOwner::Project { workspace_id, .. },
            MemoryScope::Project,
            FrozenReviewSource::Project {
                workspace_id: source_workspace,
                ..
            },
        ) => workspace_id == source_workspace,
        _ => false,
    }
}
