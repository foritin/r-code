//! Plan ownership/revision/dependency transitions (host service).
//!
//! Plans are task-owned: publishing creates a revision, updates carry the
//! revision they were authored against (stale updates are refused),
//! dependencies must complete in order, and code WorkUnits complete only
//! with current evidence. No-op duplicate updates replay their receipt
//! instead of double-applying.

use crate::task::{WorkUnit, WorkUnitStatus, WorkUnitUpdate};
use r_code_harness_protocol::services::WorkUnitWire;
use r_code_harness_protocol::OperationKey;
use std::collections::HashMap;

/// Errors from plan operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("stale plan revision: expected {expected}, got {provided}")]
    StaleRevision { expected: u64, provided: u64 },
    #[error("plan has no work unit {0}")]
    UnknownWorkUnit(String),
    #[error("dependency {0} is not completed")]
    DependencyNotCompleted(String),
    #[error("code work unit {0} requires current evidence before completion")]
    EvidenceRequired(String),
    #[error("plan is immutable once the task is terminal")]
    Terminal,
}

/// The plan state for one task.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanState {
    /// Published revision (0 = none).
    pub revision: u64,
    pub work_units: Vec<WorkUnit>,
    /// Receipts for updates, keyed by operation key.
    receipts: HashMap<String, WorkUnitUpdate>,
}

impl PlanState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a fresh plan (ownership: the task; the harness only proposes
    /// through host services). Returns the new revision.
    pub fn publish(&mut self, units: Vec<WorkUnitWire>) -> u64 {
        self.revision += 1;
        self.work_units = units
            .into_iter()
            .map(|unit| WorkUnit {
                id: unit.id,
                description: unit.description,
                dependencies: unit.dependencies,
                acceptance: unit.acceptance,
                status: WorkUnitStatus::Pending,
            })
            .collect();
        self.receipts.clear();
        self.revision
    }

    /// Apply an update authored against `provided_revision`. A no-op
    /// duplicate (same key, same update) replays the receipt.
    pub fn update(
        &mut self,
        provided_revision: u64,
        update: WorkUnitUpdate,
        operation_key: Option<OperationKey>,
        has_current_evidence: impl Fn(&[String]) -> bool,
        code_task: bool,
    ) -> Result<UpdateOutcome, PlanError> {
        if provided_revision != self.revision {
            return Err(PlanError::StaleRevision {
                expected: self.revision,
                provided: provided_revision,
            });
        }
        let key = operation_key.map(|key| key.0).unwrap_or_default();
        if !key.is_empty() {
            if let Some(prior) = self.receipts.get(&key) {
                if *prior == update {
                    return Ok(UpdateOutcome::Replayed);
                }
                return Ok(UpdateOutcome::ConflictingKey);
            }
        }
        let index = self
            .work_units
            .iter()
            .position(|unit| unit.id == update.work_unit_id)
            .ok_or_else(|| PlanError::UnknownWorkUnit(update.work_unit_id.clone()))?;
        if update.status == WorkUnitStatus::Completed {
            let unit = &self.work_units[index];
            for dependency in &unit.dependencies {
                let state = self
                    .work_units
                    .iter()
                    .find(|other| &other.id == dependency)
                    .map(|other| other.status)
                    .unwrap_or(WorkUnitStatus::Pending);
                if state != WorkUnitStatus::Completed {
                    return Err(PlanError::DependencyNotCompleted(dependency.clone()));
                }
            }
            if code_task && !unit.acceptance.is_empty() && !has_current_evidence(&unit.acceptance) {
                return Err(PlanError::EvidenceRequired(unit.id.clone()));
            }
        }
        self.work_units[index].status = update.status;
        if !key.is_empty() {
            self.receipts.insert(key, update);
        }
        Ok(UpdateOutcome::Applied)
    }

    /// All units completed?
    pub fn all_completed(&self) -> bool {
        self.work_units
            .iter()
            .all(|unit| unit.status == WorkUnitStatus::Completed)
    }
}

/// Result of an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOutcome {
    Applied,
    /// Same key + same update: replayed, nothing changed.
    Replayed,
    /// Same key, different update: refused.
    ConflictingKey,
}
