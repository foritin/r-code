//! Plan ownership/revision/dependency transitions (host service).
//!
//! Plans are task-owned: publishing creates a revision, updates carry the
//! revision they were authored against (stale updates are refused),
//! dependencies must complete in order, and code WorkUnits complete only
//! with current evidence. No-op duplicate updates replay their receipt
//! instead of double-applying.

use crate::task::PlanRevisionRef;
use crate::task::{WorkUnit, WorkUnitStatus, WorkUnitUpdate};
use r_code_harness_protocol::services::{
    normalize_workspace_relative_path, NetworkCeiling, WorkUnitWire, WorkspacePathError,
};
use r_code_harness_protocol::OperationKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// The only scope that can authorize an immutable plan revision.
///
/// Effect approvals use their own aggregate and scope; they cannot be
/// converted into a [`PlanApproval`].
pub const PLAN_APPROVE_SCOPE: &str = "plan.approve";

/// Credential-free inputs that define one immutable plan revision.
///
/// Provider configuration, prompts and permissions are represented only by
/// their content digests. Credentials therefore have no field through which
/// they can enter a plan revision or its canonical JSON representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRevisionMaterial {
    pub task_id: String,
    /// Monotonically increasing within `task_id`.
    pub revision: u64,
    /// Exact predecessor, or `None` for the first published plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_revision: Option<PlanRevisionRef>,
    /// Hash of the task/repository base against which this plan was authored.
    pub current_base_hash: String,
    pub workspace_baseline: String,
    pub route_digest: String,
    pub prompt_digest: String,
    pub permission_digest: String,
    pub check_digest: String,
    #[serde(default)]
    pub required_checks: Vec<String>,
    pub work_units: Vec<WorkUnitWire>,
}

/// Content-addressed immutable plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRevision {
    revision_ref: PlanRevisionRef,
    material: PlanRevisionMaterial,
}

impl PlanRevision {
    /// Normalize set-shaped fields, validate the WorkUnit DAG and derive the
    /// revision reference from canonical JSON.
    pub fn new(mut material: PlanRevisionMaterial) -> Result<Self, PlanRevisionError> {
        normalize_plan_material(&mut material)?;
        validate_plan_material(&material)?;
        let revision_ref = plan_revision_ref_for(&material)?;
        Ok(Self {
            revision_ref,
            material,
        })
    }

    pub fn reference(&self) -> &PlanRevisionRef {
        &self.revision_ref
    }

    pub fn material(&self) -> &PlanRevisionMaterial {
        &self.material
    }

    /// Deterministic JSON used by durable stores and audit records.
    pub fn canonical_json(&self) -> Result<String, PlanRevisionError> {
        self.validate_identity()?;
        serde_json::to_string(self)
            .map_err(|error| PlanRevisionError::Serialization(error.to_string()))
    }

    /// Fail closed when deserialized data does not match its content address.
    pub fn validate_identity(&self) -> Result<(), PlanRevisionError> {
        let mut normalized = self.material.clone();
        normalize_plan_material(&mut normalized)?;
        if normalized != self.material {
            return Err(PlanRevisionError::NonCanonical);
        }
        validate_plan_material(&self.material)?;
        let expected = plan_revision_ref_for(&self.material)?;
        if self.revision_ref != expected {
            return Err(PlanRevisionError::IdentityMismatch {
                expected,
                actual: self.revision_ref.clone(),
            });
        }
        Ok(())
    }
}

impl PlanRevisionRef {
    /// Parse a canonical plan revision reference.
    pub fn parse(value: impl Into<String>) -> Result<Self, PlanRevisionError> {
        let value = value.into();
        if is_sha256_ref(&value) {
            Ok(Self(value))
        } else {
            Err(PlanRevisionError::InvalidRevisionRef(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for PlanRevisionRef {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for PlanRevisionRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Errors raised before an invalid plan can become durable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanRevisionError {
    #[error("plan field {0} must not be empty")]
    EmptyField(&'static str),
    #[error("plan revision number must be greater than zero")]
    ZeroRevision,
    #[error("invalid plan revision reference {0}")]
    InvalidRevisionRef(String),
    #[error("plan must contain at least one work unit")]
    EmptyWorkUnits,
    #[error("work unit id must not be empty")]
    EmptyWorkUnitId,
    #[error("duplicate work unit {0}")]
    DuplicateWorkUnit(String),
    #[error("work unit {unit_id} depends on unknown unit {dependency}")]
    UnknownDependency { unit_id: String, dependency: String },
    #[error("work unit {0} cannot depend on itself")]
    SelfDependency(String),
    #[error("work unit dependency graph contains a cycle")]
    DependencyCycle,
    #[error("work unit {unit_id} has an invalid {field} path: {reason}")]
    InvalidEffectPath {
        unit_id: String,
        field: &'static str,
        reason: WorkspacePathError,
    },
    #[error("work unit {unit_id} declares overlapping read and write paths")]
    ConflictingEffectPath { unit_id: String },
    #[error("work unit {unit_id} declares an ephemeral root outside its write scope")]
    EphemeralOutsideWriteScope { unit_id: String },
    #[error(
        "work unit {0} requires the host-network ceiling, which is platform-incompatible in v1"
    )]
    HostNetworkUnsupported(String),
    #[error("plan revision serialization failed: {0}")]
    Serialization(String),
    #[error("plan revision material is not canonically normalized")]
    NonCanonical,
    #[error("plan revision identity mismatch: expected {expected}, actual {actual}")]
    IdentityMismatch {
        expected: PlanRevisionRef,
        actual: PlanRevisionRef,
    },
}

/// Authenticated actor that approved one exact plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanApprovalActor {
    pub actor_id: String,
    pub session_id: String,
    pub scope: String,
}

impl PlanApprovalActor {
    pub fn new(
        actor_id: impl Into<String>,
        session_id: impl Into<String>,
        scope: impl Into<String>,
    ) -> Result<Self, PlanApprovalError> {
        let actor = Self {
            actor_id: actor_id.into(),
            session_id: session_id.into(),
            scope: scope.into(),
        };
        actor.validate()?;
        Ok(actor)
    }

    pub fn validate(&self) -> Result<(), PlanApprovalError> {
        if self.actor_id.trim().is_empty() {
            return Err(PlanApprovalError::EmptyField("actor_id"));
        }
        if self.session_id.trim().is_empty() {
            return Err(PlanApprovalError::EmptyField("session_id"));
        }
        if self.scope != PLAN_APPROVE_SCOPE {
            return Err(PlanApprovalError::InvalidScope(self.scope.clone()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanApprovalState {
    Active,
    Superseded,
}

/// Durable approval of one exact task-owned plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanApproval {
    pub approval_id: String,
    pub task_id: String,
    pub plan_revision: PlanRevisionRef,
    pub actor: PlanApprovalActor,
    pub state: PlanApprovalState,
}

impl PlanApproval {
    pub fn new(
        approval_id: impl Into<String>,
        task_id: impl Into<String>,
        plan_revision: PlanRevisionRef,
        actor: PlanApprovalActor,
    ) -> Result<Self, PlanApprovalError> {
        let approval = Self {
            approval_id: approval_id.into(),
            task_id: task_id.into(),
            plan_revision,
            actor,
            state: PlanApprovalState::Active,
        };
        approval.validate()?;
        Ok(approval)
    }

    pub fn validate(&self) -> Result<(), PlanApprovalError> {
        if self.approval_id.trim().is_empty() {
            return Err(PlanApprovalError::EmptyField("approval_id"));
        }
        if self.task_id.trim().is_empty() {
            return Err(PlanApprovalError::EmptyField("task_id"));
        }
        PlanRevisionRef::parse(self.plan_revision.0.clone())?;
        self.actor.validate()?;
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<String, PlanApprovalError> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|error| PlanApprovalError::Serialization(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanApprovalError {
    #[error("plan approval field {0} must not be empty")]
    EmptyField(&'static str),
    #[error("plan approval scope must be {PLAN_APPROVE_SCOPE}, got {0}")]
    InvalidScope(String),
    #[error(transparent)]
    InvalidRevision(#[from] PlanRevisionError),
    #[error("plan approval serialization failed: {0}")]
    Serialization(String),
}

fn normalize_plan_material(material: &mut PlanRevisionMaterial) -> Result<(), PlanRevisionError> {
    material.required_checks.sort();
    material.required_checks.dedup();
    for unit in &mut material.work_units {
        unit.dependencies.sort();
        unit.dependencies.dedup();
        unit.acceptance.sort();
        unit.acceptance.dedup();
        normalize_effect_paths(&unit.id, "read_paths", &mut unit.read_paths)?;
        normalize_effect_paths(&unit.id, "write_paths", &mut unit.write_paths)?;
        normalize_effect_paths(&unit.id, "ephemeral_roots", &mut unit.ephemeral_roots)?;
        if unit.read_paths.iter().any(|read_path| {
            unit.write_paths.iter().any(|write_path| {
                logical_path_contains(read_path, write_path)
                    || logical_path_contains(write_path, read_path)
            })
        }) {
            return Err(PlanRevisionError::ConflictingEffectPath {
                unit_id: unit.id.clone(),
            });
        }
        if unit.ephemeral_roots.iter().any(|root| {
            !unit
                .write_paths
                .iter()
                .any(|scope| logical_path_contains(scope, root))
        }) {
            return Err(PlanRevisionError::EphemeralOutsideWriteScope {
                unit_id: unit.id.clone(),
            });
        }
    }
    material
        .work_units
        .sort_by(|left, right| left.id.cmp(&right.id));
    Ok(())
}

fn normalize_effect_paths(
    unit_id: &str,
    field: &'static str,
    paths: &mut Vec<String>,
) -> Result<(), PlanRevisionError> {
    for path in paths.iter_mut() {
        *path = normalize_workspace_relative_path(path).map_err(|reason| {
            PlanRevisionError::InvalidEffectPath {
                unit_id: unit_id.to_string(),
                field,
                reason,
            }
        })?;
        #[cfg(windows)]
        {
            *path = path.to_lowercase();
        }
    }
    paths.sort();
    paths.dedup();
    Ok(())
}

fn logical_path_contains(scope: &str, path: &str) -> bool {
    scope == path
        || path
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn validate_plan_material(material: &PlanRevisionMaterial) -> Result<(), PlanRevisionError> {
    if material.task_id.trim().is_empty() {
        return Err(PlanRevisionError::EmptyField("task_id"));
    }
    if material.revision == 0 {
        return Err(PlanRevisionError::ZeroRevision);
    }
    if let Some(parent) = &material.parent_revision {
        PlanRevisionRef::parse(parent.0.clone())?;
    }
    for (name, value) in [
        ("current_base_hash", &material.current_base_hash),
        ("workspace_baseline", &material.workspace_baseline),
        ("route_digest", &material.route_digest),
        ("prompt_digest", &material.prompt_digest),
        ("permission_digest", &material.permission_digest),
        ("check_digest", &material.check_digest),
    ] {
        if value.trim().is_empty() {
            return Err(PlanRevisionError::EmptyField(name));
        }
    }
    if material.work_units.is_empty() {
        return Err(PlanRevisionError::EmptyWorkUnits);
    }

    let mut ids = BTreeSet::new();
    for unit in &material.work_units {
        if unit.id.trim().is_empty() {
            return Err(PlanRevisionError::EmptyWorkUnitId);
        }
        if !ids.insert(unit.id.clone()) {
            return Err(PlanRevisionError::DuplicateWorkUnit(unit.id.clone()));
        }
        // P19A: HostNetwork is the full host stack — no v1 platform can
        // honor it, so a plan carrying it fails validation outright.
        if unit.network_ceiling == NetworkCeiling::HostNetwork {
            return Err(PlanRevisionError::HostNetworkUnsupported(unit.id.clone()));
        }
    }

    let mut incoming = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for unit in &material.work_units {
        incoming.insert(unit.id.as_str(), unit.dependencies.len());
        for dependency in &unit.dependencies {
            if dependency == &unit.id {
                return Err(PlanRevisionError::SelfDependency(unit.id.clone()));
            }
            if !ids.contains(dependency) {
                return Err(PlanRevisionError::UnknownDependency {
                    unit_id: unit.id.clone(),
                    dependency: dependency.clone(),
                });
            }
            dependents
                .entry(dependency.as_str())
                .or_default()
                .push(unit.id.as_str());
        }
    }

    let mut ready = BTreeSet::new();
    for (id, count) in &incoming {
        if *count == 0 {
            ready.insert(*id);
        }
    }
    let mut visited = 0usize;
    while let Some(id) = ready.pop_first() {
        visited += 1;
        if let Some(children) = dependents.get(id) {
            for child in children {
                let count = incoming
                    .get_mut(child)
                    .expect("validated work unit dependency");
                *count -= 1;
                if *count == 0 {
                    ready.insert(child);
                }
            }
        }
    }
    if visited != material.work_units.len() {
        return Err(PlanRevisionError::DependencyCycle);
    }
    Ok(())
}

fn plan_revision_ref_for(
    material: &PlanRevisionMaterial,
) -> Result<PlanRevisionRef, PlanRevisionError> {
    let value = serde_json::to_value(material)
        .map_err(|error| PlanRevisionError::Serialization(error.to_string()))?;
    let digest = r_code_harness_protocol::canonical_input_hash(&value);
    PlanRevisionRef::parse(format!("sha256:{digest}"))
}

fn is_sha256_ref(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

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
                read_paths: unit.read_paths,
                write_paths: unit.write_paths,
                repo_exclusive: unit.repo_exclusive,
                ephemeral_roots: unit.ephemeral_roots,
                effect_class: unit.effect_class,
                network_ceiling: unit.network_ceiling,
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
