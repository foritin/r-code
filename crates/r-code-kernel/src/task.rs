//! Task domain model: contracts, work units, attempts, receipts, evidence and
//! the pure transition decisions that govern a task's life.
//!
//! Three concerns are deliberately orthogonal:
//! - execution lifecycle ([`TaskExecution`]),
//! - validation outcome ([`ValidationOutcome`]),
//! - user review disposition ([`ReviewDisposition`]).
//!
//! Only the kernel issues terminal verdicts ([`TaskVerdict`]); plugins
//! propose, the host decides.

use r_code_harness_protocol::{ArtifactRef, OperationKey, PackageRef, Provenance};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What kind of work a task represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    /// Q&A / conversation; may finish without code checks.
    Conversation,
    /// Code changes; completion requires host-owned evidence.
    Implementation,
    /// A plan draft; may finish without code checks.
    PlanDraft,
    /// Repair of a previously failed implementation.
    Repair,
}

impl TaskKind {
    pub fn requires_code_evidence(&self) -> bool {
        matches!(self, TaskKind::Implementation | TaskKind::Repair)
    }
}

/// The frozen agreement between user and system about what "done" means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContract {
    pub task_id: String,
    pub kind: TaskKind,
    pub objective: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Required check-definition ids. Weakening them requires a new,
    /// user-authorized revision.
    #[serde(default)]
    pub required_checks: Vec<String>,
    pub revision: u64,
}

/// One unit of planned work with acceptance mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkUnit {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Check-definition ids this unit is accepted by.
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repo_exclusive: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ephemeral_roots: Vec<String>,
    /// P19A: the effect authority class carried from the wire unit. The
    /// default is the conservative ReadOnly.
    #[serde(default, skip_serializing_if = "WorkUnitEffectClass::is_read_only")]
    pub effect_class: WorkUnitEffectClass,
    /// P19A: the network ceiling carried from the wire unit. The default
    /// Offline is the floor.
    #[serde(default, skip_serializing_if = "NetworkCeiling::is_offline")]
    pub network_ceiling: NetworkCeiling,
    pub status: WorkUnitStatus,
}

pub use r_code_harness_protocol::{NetworkCeiling, WorkUnitEffectClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkUnitStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

/// Content-addressed identity of an immutable run snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunSnapshotId(String);

impl RunSnapshotId {
    pub fn parse(value: impl Into<String>) -> Result<Self, RunSnapshotError> {
        let value = value.into();
        let Some(digest) = value.strip_prefix("sha256:") else {
            return Err(RunSnapshotError::InvalidId(value));
        };
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RunSnapshotError::InvalidId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for RunSnapshotId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for RunSnapshotId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The host-owned route kind. Provider credentials deliberately have no
/// representation in this contract and stay in the credential store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderRouteKind {
    HostProvider,
    HarnessManaged,
}

/// Provider-neutral model route selected for future runs of a task.
///
/// `HostProvider` resolves through daemon-owned settings and credentials.
/// `HarnessManaged` delegates model access to the selected harness, so the
/// host never interprets or special-cases a particular harness identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ModelRoute {
    HostProvider {
        #[serde(rename = "providerId", alias = "provider_id")]
        provider_id: String,
        #[serde(
            default,
            rename = "modelId",
            alias = "model_id",
            skip_serializing_if = "Option::is_none"
        )]
        model_id: Option<String>,
    },
    HarnessManaged {
        #[serde(rename = "harnessId", alias = "harness_id")]
        harness_id: String,
        #[serde(
            default,
            rename = "modelId",
            alias = "model_id",
            skip_serializing_if = "Option::is_none"
        )]
        model_id: Option<String>,
    },
}

impl ModelRoute {
    pub fn model_id(&self) -> Option<&str> {
        match self {
            Self::HostProvider { model_id, .. } | Self::HarnessManaged { model_id, .. } => {
                model_id.as_deref()
            }
        }
    }
}

/// Credential-free provider and model metadata frozen for one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSnapshotRef {
    pub kind: ProviderRouteKind,
    pub settings_revision: u64,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// How the editable user prompt participates in the resolved prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptSnapshotMode {
    Default,
    Append,
    Replace,
}

/// Resolved prompt content and the revision from which it was produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSnapshotRef {
    pub revision: String,
    pub mode: PromptSnapshotMode,
    pub content_sha256: String,
    pub resolved_system_prompt: String,
}

/// Identity of the current checkout used by this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshotRef {
    pub canonical_root: String,
    pub workspace_identity: String,
    pub baseline_sha256: String,
}

/// Permission profile frozen before dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSnapshotRef {
    pub revision: String,
    pub profile_id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Immutable identity of a plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlanRevisionRef(pub String);

/// Approval of one exact plan revision. Effect approvals are a separate
/// aggregate and cannot be substituted for this reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanApprovalRef {
    pub approval_id: String,
    pub plan_revision: PlanRevisionRef,
}

/// Purpose of a run. The enum shape makes it impossible to construct an
/// execution or repair snapshot without an exact plan approval reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RunSnapshotPhase {
    Planning,
    Execution { approval: PlanApprovalRef },
    Repair { approval: PlanApprovalRef },
}

impl RunSnapshotPhase {
    pub fn approval(&self) -> Option<&PlanApprovalRef> {
        match self {
            Self::Planning => None,
            Self::Execution { approval } | Self::Repair { approval } => Some(approval),
        }
    }

    pub fn plan_revision(&self) -> Option<&PlanRevisionRef> {
        self.approval().map(|approval| &approval.plan_revision)
    }
}

/// All material inputs whose identity must not drift during a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSnapshotMaterial {
    pub task_id: String,
    pub task_revision: u64,
    pub phase: RunSnapshotPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit_id: Option<String>,
    pub provider: ProviderSnapshotRef,
    pub prompt: PromptSnapshotRef,
    pub workspace: WorkspaceSnapshotRef,
    pub permissions: PermissionSnapshotRef,
    pub harness_package: PackageRef,
    pub tool_catalog_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference: Option<serde_json::Value>,
}

/// Immutable, content-addressed run configuration. The identifier is derived
/// from canonical JSON for `material`; timestamps and database row identity
/// are intentionally excluded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSnapshot {
    snapshot_id: RunSnapshotId,
    material: RunSnapshotMaterial,
}

impl RunSnapshot {
    pub fn new(mut material: RunSnapshotMaterial) -> Result<Self, RunSnapshotError> {
        validate_snapshot_material(&material)?;

        // These fields are sets semantically. Canonicalizing them prevents
        // discovery order from changing the snapshot identity.
        material.provider.capabilities.sort();
        material.provider.capabilities.dedup();
        material.permissions.capabilities.sort();
        material.permissions.capabilities.dedup();

        let snapshot_id = snapshot_id_for(&material)?;
        Ok(Self {
            snapshot_id,
            material,
        })
    }

    pub fn id(&self) -> &RunSnapshotId {
        &self.snapshot_id
    }

    pub fn material(&self) -> &RunSnapshotMaterial {
        &self.material
    }

    pub fn phase(&self) -> &RunSnapshotPhase {
        &self.material.phase
    }

    pub fn validate_identity(&self) -> Result<(), RunSnapshotError> {
        validate_snapshot_material(&self.material)?;
        let actual = snapshot_id_for(&self.material)?;
        if actual != self.snapshot_id {
            return Err(RunSnapshotError::IdentityMismatch {
                expected: self.snapshot_id.clone(),
                actual,
            });
        }
        Ok(())
    }
}

fn snapshot_id_for(material: &RunSnapshotMaterial) -> Result<RunSnapshotId, RunSnapshotError> {
    let value = serde_json::to_value(material)
        .map_err(|error| RunSnapshotError::Serialization(error.to_string()))?;
    let digest = r_code_harness_protocol::canonical_input_hash(&value);
    RunSnapshotId::parse(format!("sha256:{digest}"))
}

fn validate_snapshot_material(material: &RunSnapshotMaterial) -> Result<(), RunSnapshotError> {
    match (&material.phase, material.work_unit_id.as_deref()) {
        (RunSnapshotPhase::Planning, None) => {}
        (RunSnapshotPhase::Execution { .. } | RunSnapshotPhase::Repair { .. }, Some(id))
            if !id.trim().is_empty() => {}
        (RunSnapshotPhase::Planning, Some(_)) => return Err(RunSnapshotError::UnexpectedWorkUnit),
        _ => return Err(RunSnapshotError::MissingWorkUnit),
    }
    if let Some(approval) = material.phase.approval() {
        if approval.approval_id.trim().is_empty() {
            return Err(RunSnapshotError::InvalidPlanApproval {
                field: "approval_id",
            });
        }
        if approval.plan_revision.0.trim().is_empty() {
            return Err(RunSnapshotError::InvalidPlanApproval {
                field: "plan_revision",
            });
        }
    }
    if let Some(inference) = &material.inference {
        validate_credential_free_json(inference, "inference")?;
    }
    Ok(())
}

fn validate_credential_free_json(
    value: &serde_json::Value,
    path: &str,
) -> Result<(), RunSnapshotError> {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                let normalized = normalize_key(key);
                if is_credential_key(&normalized) {
                    return Err(RunSnapshotError::CredentialBearingInferenceKey {
                        path: format!("{path}.{key}"),
                        key: key.clone(),
                    });
                }
                validate_credential_free_json(child, &format!("{path}.{key}"))?;
            }
        }
        serde_json::Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                validate_credential_free_json(child, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn is_credential_key(normalized: &str) -> bool {
    matches!(
        normalized,
        "apikey"
            | "accesstoken"
            | "authorization"
            | "secret"
            | "password"
            | "credential"
            | "credentials"
            | "clientsecret"
            | "refreshtoken"
            | "authtoken"
            | "bearertoken"
            | "privatekey"
    )
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunSnapshotError {
    #[error("invalid run snapshot id {0}")]
    InvalidId(String),
    #[error("run snapshot serialization failed: {0}")]
    Serialization(String),
    #[error("run snapshot inference contains credential-bearing key {key} at {path}")]
    CredentialBearingInferenceKey { path: String, key: String },
    #[error("run snapshot plan approval has an empty {field}")]
    InvalidPlanApproval { field: &'static str },
    #[error("run snapshot identity mismatch: expected {expected}, actual {actual}")]
    IdentityMismatch {
        expected: RunSnapshotId,
        actual: RunSnapshotId,
    },
    #[error("execution and repair snapshots require a work unit")]
    MissingWorkUnit,
    #[error("planning snapshots cannot carry a work unit")]
    UnexpectedWorkUnit,
}

/// A pinned attempt: plugin package, contract, context and workspace identity
/// are frozen for its lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub attempt_id: String,
    pub task_id: String,
    pub branch_id: String,
    pub package: PackageRef,
    pub contract_revision: u64,
    pub config_hash: String,
    pub workspace_identity: String,
    pub run_id: String,
}

const ATTEMPT_SNAPSHOT_PREFIX: &str = "run-snapshot:";

impl Attempt {
    /// Construct a new attempt bound to a durable run snapshot. Keeping the
    /// binding in the legacy `config_hash` slot preserves source and serde
    /// compatibility with pre-snapshot attempts.
    #[allow(clippy::too_many_arguments)]
    pub fn for_run_snapshot(
        attempt_id: impl Into<String>,
        task_id: impl Into<String>,
        branch_id: impl Into<String>,
        package: PackageRef,
        contract_revision: u64,
        workspace_identity: impl Into<String>,
        run_id: impl Into<String>,
        snapshot_id: &RunSnapshotId,
    ) -> Self {
        Self {
            attempt_id: attempt_id.into(),
            task_id: task_id.into(),
            branch_id: branch_id.into(),
            package,
            contract_revision,
            config_hash: snapshot_binding(snapshot_id),
            workspace_identity: workspace_identity.into(),
            run_id: run_id.into(),
        }
    }

    /// Bind a legacy-shaped attempt before it is first persisted or started.
    pub fn with_run_snapshot(mut self, snapshot_id: &RunSnapshotId) -> Self {
        self.config_hash = snapshot_binding(snapshot_id);
        self
    }

    /// The immutable snapshot referenced by a new-style attempt. Legacy
    /// attempts return `None` and remain readable for recovery/migration.
    pub fn run_snapshot_id(&self) -> Option<RunSnapshotId> {
        self.config_hash
            .strip_prefix(ATTEMPT_SNAPSHOT_PREFIX)
            .and_then(|value| RunSnapshotId::parse(value.to_string()).ok())
    }
}

fn snapshot_binding(snapshot_id: &RunSnapshotId) -> String {
    format!("{ATTEMPT_SNAPSHOT_PREFIX}{snapshot_id}")
}

impl Serialize for Attempt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct AttemptWire<'a> {
            attempt_id: &'a str,
            task_id: &'a str,
            branch_id: &'a str,
            package: &'a PackageRef,
            contract_revision: u64,
            config_hash: &'a str,
            workspace_identity: &'a str,
            run_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            snapshot_id: Option<&'a str>,
        }

        let snapshot_id = self.config_hash.strip_prefix(ATTEMPT_SNAPSHOT_PREFIX);
        AttemptWire {
            attempt_id: &self.attempt_id,
            task_id: &self.task_id,
            branch_id: &self.branch_id,
            package: &self.package,
            contract_revision: self.contract_revision,
            config_hash: &self.config_hash,
            workspace_identity: &self.workspace_identity,
            run_id: &self.run_id,
            snapshot_id,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Attempt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct AttemptWire {
            attempt_id: String,
            task_id: String,
            branch_id: String,
            package: PackageRef,
            contract_revision: u64,
            config_hash: String,
            workspace_identity: String,
            run_id: String,
            #[serde(default)]
            snapshot_id: Option<String>,
        }

        let wire = AttemptWire::deserialize(deserializer)?;
        let config_hash = match wire.snapshot_id {
            Some(value) => {
                let snapshot_id = RunSnapshotId::parse(value).map_err(serde::de::Error::custom)?;
                let binding = snapshot_binding(&snapshot_id);
                if wire.config_hash != binding {
                    return Err(serde::de::Error::custom(
                        "attempt config_hash conflicts with snapshot_id",
                    ));
                }
                binding
            }
            None => wire.config_hash,
        };
        Ok(Self {
            attempt_id: wire.attempt_id,
            task_id: wire.task_id,
            branch_id: wire.branch_id,
            package: wire.package,
            contract_revision: wire.contract_revision,
            config_hash,
            workspace_identity: wire.workspace_identity,
            run_id: wire.run_id,
        })
    }
}

/// Durable receipt binding an idempotency key to a request hash and outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationReceipt {
    pub attempt_id: String,
    pub operation_key: OperationKey,
    /// Host method the key was used with (drives replay classification).
    pub method: String,
    pub input_hash: String,
    pub outcome: ReceiptOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum ReceiptOutcome {
    Completed { result: serde_json::Value },
    Indeterminate { reason: String },
    Rejected { reason: String },
}

/// Host-recorded evidence binding a check to candidate content and output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub evidence_id: String,
    #[serde(default)]
    pub task_id: String,
    pub check_id: String,
    #[serde(default)]
    pub definition_identity: String,
    /// Content digest of the candidate the check ran against.
    pub candidate_digest: String,
    /// Toolchain/environment identity the check ran in.
    pub environment: String,
    #[serde(default)]
    pub environment_fingerprint: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_output: Option<ArtifactRef>,
    /// Only host-provenance records count as acceptance evidence.
    pub recorded_by: Provenance,
}

impl EvidenceRecord {
    pub fn is_valid_for(&self, check_id: &str, candidate_digest: &str) -> bool {
        self.passed
            && !self.task_id.trim().is_empty()
            && !self.definition_identity.trim().is_empty()
            && !self.environment_fingerprint.trim().is_empty()
            && self.check_id == check_id
            && self.candidate_digest == candidate_digest
            && matches!(self.recorded_by, Provenance::Host)
    }

    pub fn is_valid_for_binding(
        &self,
        requirement: &EvidenceRequirement,
        task_id: &str,
        candidate_digest: &str,
    ) -> bool {
        self.task_id == task_id
            && self.definition_identity == requirement.definition_identity
            && self.environment_fingerprint == requirement.environment_fingerprint
            && self.is_valid_for(&requirement.check_id, candidate_digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRequirement {
    pub check_id: String,
    pub definition_identity: String,
    pub environment_fingerprint: String,
}

/// Execution lifecycle. Terminal states admit no further transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "kebab-case")]
pub enum TaskExecution {
    Pending,
    Running {
        attempt_id: String,
        generation: u64,
    },
    WaitingInput {
        attempt_id: String,
        generation: u64,
        question_id: String,
    },
    /// A planning attempt published one immutable revision and stopped. No
    /// implementation attempt may start until that exact revision is
    /// approved by the host.
    AwaitingPlanApproval {
        plan_revision: PlanRevisionRef,
    },
    /// The exact plan revision was approved. T08A deliberately does not
    /// activate implementation from this state; a later execution gate must
    /// consume the approval explicitly.
    Ready {
        approval: PlanApprovalRef,
    },
    /// A unit's verification is in flight. Which unit (and its attempt,
    /// candidate digest and outcome) lives in [`TaskState::unit_records`];
    /// the task-level payload names only the slot's owning attempt (E05).
    Verifying {
        attempt_id: String,
    },
    RepairRequired {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        work_unit_id: Option<String>,
        reason: String,
    },
    ReviewReady {
        attempt_id: String,
    },
    Terminal {
        verdict: TaskVerdict,
    },
}

/// Independent validation outcome; separate from execution and review.
/// Since E05 this vocabulary is carried PER UNIT (see [`UnitRecord`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ValidationOutcome {
    #[default]
    NotEvaluated,
    InProgress,
    Verified {
        candidate_digest: String,
    },
    Unverified {
        reason: String,
    },
    CheckUnavailable {
        reason: String,
    },
}

/// User review disposition; separate from execution and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewDisposition {
    NotRequired,
    Pending,
    Accepted,
    Rejected,
    OverrideAccepted,
}

/// Terminal settlement of one work unit's execution (E05).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitSettlement {
    /// Started and still executing; not a terminal per-unit outcome.
    #[default]
    InFlight,
    /// Completed with host-verified effect.
    Completed,
    /// Completed by the state machine without an attempt or a child: a
    /// read-only unit whose dependencies were all Completed (E05.3).
    CompletedWithoutEffect,
    /// Failed; the failure reason lives in the record's verification.
    Failed,
}

impl UnitSettlement {
    /// Whether this is a terminal per-unit outcome.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, UnitSettlement::InFlight)
    }
}

/// Per-unit execution record (E05): one entry per started or settled work
/// unit, keyed by unit id in [`TaskState::unit_records`]. Each in-flight
/// unit carries its own candidate digest, verification outcome and failure
/// reason; task-level verdicts aggregate over these records and never store
/// a separate single-slot copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitRecord {
    /// The execution attempt driving this unit. `None` only for read-only
    /// units the state machine completes without an attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// This unit's candidate content digest, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_digest: Option<String>,
    /// This unit's verification outcome (per-unit; failure reasons live in
    /// the `Unverified`/`CheckUnavailable` arms).
    #[serde(default)]
    pub verification: ValidationOutcome,
    /// Whether the unit reached a terminal per-unit outcome.
    #[serde(default)]
    pub settlement: UnitSettlement,
}

impl Default for UnitRecord {
    fn default() -> Self {
        Self {
            attempt_id: None,
            candidate_digest: None,
            verification: ValidationOutcome::NotEvaluated,
            settlement: UnitSettlement::InFlight,
        }
    }
}

/// The authoritative terminal verdict. Issued by the kernel only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum TaskVerdict {
    Verified {
        candidate_digest: String,
    },
    VerifiedAccepted {
        candidate_digest: String,
        actor_id: String,
    },
    Unverified {
        reason: String,
    },
    UnverifiedAccepted {
        candidate_digest: String,
        actor_id: String,
        reason: String,
        checks: Vec<String>,
    },
    Blocked {
        reason: String,
    },
    Failed {
        reason: String,
    },
    Cancelled {
        reason: String,
    },
}

impl TaskVerdict {
    pub fn is_terminal(&self) -> bool {
        true
    }
}

/// Aggregate root for one task branch.
///
/// Since E05 the verification material is per-unit: [`Self::unit_records`]
/// carries each started unit's candidate digest, verification outcome and
/// failure reason, and task-level verdicts aggregate over those records.
/// Task-level candidate digests (e.g. the harness-protocol wire's
/// `candidateDigest`) are DERIVED — see [`Self::task_candidate_digest`] —
/// and never stored here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskState {
    pub contract: TaskContract,
    #[serde(default)]
    pub work_units: Vec<WorkUnit>,
    pub execution: TaskExecution,
    /// Task-level aggregate review disposition. The user's accept / reject /
    /// override decision is one whole-task decision with one actor; whether
    /// it is OFFERED derives from the per-unit records (E05.2/E05.4).
    pub review: ReviewDisposition,
    #[serde(default)]
    pub evidence: Vec<EvidenceRecord>,
    /// Human-facing session title (UI metadata; never affects contracts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Per-task harness preferences (model selection, inference knobs, mode, prompt and workspace
    /// metadata) applied to future runs. Not part of contract identity.
    #[serde(default)]
    pub preferences: TaskPreferences,
    /// Per-unit execution records keyed by work-unit id (E05).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unit_records: BTreeMap<String, UnitRecord>,
    /// The plan approval arming the current execution wave; set by
    /// [`Self::start_execution_attempt`] and cleared when the wave's
    /// task-level aggregate settles (E05).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_approval: Option<PlanApprovalRef>,
}

/// User-set run preferences carried into harness config on each run.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TaskPreferences {
    /// Explicit route for new clients. Absence preserves the pre-route
    /// default/legacy behavior and keeps old task documents readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_route: Option<ModelRoute>,
    /// Legacy provider selection. New callers use `model_route`; retaining
    /// this field keeps existing task documents and v1 clients readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Opaque inference knobs (thinking level, effort) forwarded verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Effective main-agent prompt frozen into the next harness run. The host refreshes this from
    /// the editable global/project prompt policy before dispatching a GUI message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Workspace metadata used to preserve the prompt scope across the v1 task projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    /// High-sensitivity approvals on this task require a desktop (local)
    /// confirmation: remote `approvals.decide` answers
    /// `needs_desktop_confirm` instead of a final decision (R12; frozen
    /// protocol field, default off).
    #[serde(default)]
    pub require_desktop_confirm: bool,
}

/// Transitional decode shape for one TaskState row: accepts both the
/// per-unit form and the pre-E05 single-slot form (`validation` /
/// `candidate_digest` top-level fields plus a `Verifying` variant that
/// carried the verifying unit's id). The store's `load_task` decodes with a
/// silent `.ok()`, so without this fold a legacy row would silently vanish
/// (E05.6).
#[derive(Deserialize)]
struct TaskStateWire {
    contract: TaskContract,
    #[serde(default)]
    work_units: Vec<WorkUnit>,
    execution: LegacyExecution,
    review: ReviewDisposition,
    #[serde(default)]
    evidence: Vec<EvidenceRecord>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    preferences: TaskPreferences,
    #[serde(default)]
    unit_records: BTreeMap<String, UnitRecord>,
    #[serde(default)]
    active_approval: Option<PlanApprovalRef>,
    /// Pre-E05 single-slot verification outcome (folded into one per-unit
    /// record on load).
    #[serde(default)]
    validation: Option<ValidationOutcome>,
    /// Pre-E05 single-slot candidate digest (folded with `validation`).
    #[serde(default)]
    candidate_digest: Option<String>,
}

/// Mirror of [`TaskExecution`] that additionally accepts the legacy
/// `Verifying { work_unit_id }` payload so the fold can recover WHICH unit
/// the old single slot belonged to.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "phase", rename_all = "kebab-case")]
enum LegacyExecution {
    Pending,
    Running {
        attempt_id: String,
        generation: u64,
    },
    WaitingInput {
        attempt_id: String,
        generation: u64,
        question_id: String,
    },
    AwaitingPlanApproval {
        plan_revision: PlanRevisionRef,
    },
    Ready {
        approval: PlanApprovalRef,
    },
    Verifying {
        attempt_id: String,
        #[serde(default)]
        work_unit_id: Option<String>,
    },
    RepairRequired {
        #[serde(default)]
        attempt_id: Option<String>,
        #[serde(default)]
        work_unit_id: Option<String>,
        reason: String,
    },
    ReviewReady {
        attempt_id: String,
    },
    Terminal {
        verdict: TaskVerdict,
    },
}

impl LegacyExecution {
    /// Convert to the live enum plus the legacy verifying unit id, if the
    /// row was persisted by a pre-E05 kernel.
    fn into_parts(self) -> (TaskExecution, Option<String>) {
        let legacy_verifying_unit = match &self {
            Self::Verifying { work_unit_id, .. } => work_unit_id.clone(),
            _ => None,
        };
        let execution = match self {
            Self::Pending => TaskExecution::Pending,
            Self::Running {
                attempt_id,
                generation,
            } => TaskExecution::Running {
                attempt_id,
                generation,
            },
            Self::WaitingInput {
                attempt_id,
                generation,
                question_id,
            } => TaskExecution::WaitingInput {
                attempt_id,
                generation,
                question_id,
            },
            Self::AwaitingPlanApproval { plan_revision } => {
                TaskExecution::AwaitingPlanApproval { plan_revision }
            }
            Self::Ready { approval } => TaskExecution::Ready { approval },
            Self::Verifying { attempt_id, .. } => TaskExecution::Verifying { attempt_id },
            Self::RepairRequired {
                attempt_id,
                work_unit_id,
                reason,
            } => TaskExecution::RepairRequired {
                attempt_id,
                work_unit_id,
                reason,
            },
            Self::ReviewReady { attempt_id } => TaskExecution::ReviewReady { attempt_id },
            Self::Terminal { verdict } => TaskExecution::Terminal { verdict },
        };
        (execution, legacy_verifying_unit)
    }
}

impl<'de> Deserialize<'de> for TaskState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = TaskStateWire::deserialize(deserializer)?;
        let (execution, legacy_verifying_unit) = wire.execution.into_parts();
        let mut state = Self {
            contract: wire.contract,
            work_units: wire.work_units,
            execution,
            review: wire.review,
            evidence: wire.evidence,
            title: wire.title,
            preferences: wire.preferences,
            unit_records: wire.unit_records,
            active_approval: wire.active_approval,
        };
        state.fold_legacy_single_slot(
            legacy_verifying_unit,
            wire.validation,
            wire.candidate_digest,
        );
        Ok(state)
    }
}

impl TaskState {
    /// E05.6 legacy fold: a pre-per-unit row carried its verification state
    /// in single top-level slots. Fold them into one per-unit record for the
    /// unit they belonged to, so an old row never silently vanishes and
    /// never loses its verification state. Never overwrites new-shape
    /// records.
    fn fold_legacy_single_slot(
        &mut self,
        legacy_verifying_unit: Option<String>,
        validation: Option<ValidationOutcome>,
        candidate_digest: Option<String>,
    ) {
        let validation = validation.unwrap_or(ValidationOutcome::NotEvaluated);
        let meaningful =
            !matches!(validation, ValidationOutcome::NotEvaluated) || candidate_digest.is_some();
        if !meaningful {
            return;
        }
        // Which unit did the single slot belong to?
        let target = legacy_verifying_unit
            .or_else(|| match &self.execution {
                TaskExecution::RepairRequired { work_unit_id, .. } => work_unit_id.clone(),
                _ => None,
            })
            .or_else(|| self.single_unit_with_status(WorkUnitStatus::Completed))
            .or_else(|| self.single_unit_with_status(WorkUnitStatus::InProgress));
        let Some(unit_id) = target else {
            return;
        };
        if self.unit_records.contains_key(&unit_id) {
            return;
        }
        let attempt_id = match &self.execution {
            TaskExecution::Running { attempt_id, .. }
            | TaskExecution::WaitingInput { attempt_id, .. }
            | TaskExecution::Verifying { attempt_id }
            | TaskExecution::ReviewReady { attempt_id } => Some(attempt_id.clone()),
            TaskExecution::RepairRequired { attempt_id, .. } => attempt_id.clone(),
            _ => None,
        };
        let unit_completed = self
            .work_units
            .iter()
            .any(|unit| unit.id == unit_id && unit.status == WorkUnitStatus::Completed);
        let settlement = match &validation {
            ValidationOutcome::NotEvaluated => UnitSettlement::InFlight,
            ValidationOutcome::InProgress => UnitSettlement::InFlight,
            ValidationOutcome::Verified { .. } if unit_completed => UnitSettlement::Completed,
            ValidationOutcome::Unverified { .. } | ValidationOutcome::CheckUnavailable { .. } => {
                UnitSettlement::Failed
            }
            ValidationOutcome::Verified { .. } => UnitSettlement::InFlight,
        };
        self.unit_records.insert(
            unit_id,
            UnitRecord {
                attempt_id,
                candidate_digest,
                verification: validation,
                settlement,
            },
        );
    }

    fn single_unit_with_status(&self, status: WorkUnitStatus) -> Option<String> {
        let matching: Vec<&WorkUnit> = self
            .work_units
            .iter()
            .filter(|unit| unit.status == status)
            .collect();
        match matching.as_slice() {
            [only] => Some(only.id.clone()),
            _ => None,
        }
    }
}

/// Who is trying to perform a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Host,
    Plugin,
    User,
}

/// Errors from pure transition decisions.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TransitionError {
    #[error("task is already terminal ({verdict:?}); no further transitions apply")]
    AlreadyTerminal { verdict: TaskVerdict },
    #[error("stale revision: expected {expected}, got {provided}")]
    StaleRevision { expected: u64, provided: u64 },
    #[error("late generation: current {current}, got {provided}")]
    LateGeneration { current: u64, provided: u64 },
    #[error("plugins cannot issue terminal verdicts; they may only propose completion")]
    PluginVerdictRejected,
    #[error("invalid transition from {from:?}: {action}")]
    InvalidTransition {
        from: &'static str,
        action: &'static str,
    },
    #[error("work unit {0} not found")]
    UnknownWorkUnit(String),
    #[error("dependency {0} is not completed")]
    DependencyNotCompleted(String),
    #[error("code work units require current evidence before completion")]
    EvidenceRequired,
    #[error("question {0} is not the one being answered")]
    WrongQuestion(String),
    #[error("implementation and repair attempts require an immutable run snapshot")]
    RunSnapshotRequired,
    #[error("{action} is a host-only task transition")]
    HostTransitionRequired { action: &'static str },
    #[error("plan approval is for {provided}, but the task awaits {expected}")]
    WrongPlanRevision {
        expected: PlanRevisionRef,
        provided: PlanRevisionRef,
    },
}

impl TaskState {
    pub fn new(contract: TaskContract) -> Self {
        Self {
            contract,
            work_units: Vec::new(),
            execution: TaskExecution::Pending,
            review: ReviewDisposition::NotRequired,
            evidence: Vec::new(),
            title: None,
            preferences: TaskPreferences::default(),
            unit_records: BTreeMap::new(),
            active_approval: None,
        }
    }

    // -- E05 per-unit helpers ----------------------------------------------

    /// The per-unit record of one work unit, if the wave started it.
    pub fn unit_record(&self, work_unit_id: &str) -> Option<&UnitRecord> {
        self.unit_records.get(work_unit_id)
    }

    /// Mutable access to one unit's record for hosts seeding per-unit
    /// material (e.g. a candidate digest before dispatch).
    pub fn unit_record_mut(&mut self, work_unit_id: &str) -> Option<&mut UnitRecord> {
        self.unit_records.get_mut(work_unit_id)
    }

    /// Whether any STARTED unit of the current wave has not reached a
    /// terminal per-unit outcome (E05.2/E05.4): task-level verdicts are not
    /// computed while this holds. Units that were never started stay
    /// outside the current wave's aggregate.
    pub fn has_unsettled_units(&self) -> bool {
        self.work_units.iter().any(|unit| {
            unit.status == WorkUnitStatus::InProgress
                || self
                    .unit_records
                    .get(&unit.id)
                    .is_some_and(|record| record.settlement == UnitSettlement::InFlight)
        })
    }

    /// The task-level candidate digest, DERIVED from per-unit records and
    /// never stored (E05): anchored on the attempt the task-level phase
    /// names. Callers at the RPC boundary (review projection, wire
    /// envelopes) re-derive this; it is the aggregate the harness-protocol
    /// wire's `candidateDigest` carries.
    pub fn task_candidate_digest(&self) -> Option<String> {
        match &self.execution {
            TaskExecution::Running { attempt_id, .. }
            | TaskExecution::WaitingInput { attempt_id, .. }
            | TaskExecution::Verifying { attempt_id }
            | TaskExecution::ReviewReady { attempt_id } => {
                self.record_digest_for_attempt(attempt_id)
            }
            TaskExecution::RepairRequired { attempt_id, .. } => attempt_id
                .as_deref()
                .and_then(|attempt| self.record_digest_for_attempt(attempt))
                .or_else(|| self.failed_record_digest()),
            _ => None,
        }
    }

    /// The digest a verified acceptance may accept (E05.4): the anchored
    /// record's, only while that record's verification is `Verified`.
    pub fn review_verified_digest(&self) -> Option<String> {
        let TaskExecution::ReviewReady { attempt_id } = &self.execution else {
            return None;
        };
        match &self
            .unit_records
            .values()
            .find(|record| record.attempt_id.as_deref() == Some(attempt_id.as_str()))?
            .verification
        {
            ValidationOutcome::Verified { candidate_digest } => Some(candidate_digest.clone()),
            _ => None,
        }
    }

    /// The digest an unverified override may accept (E05.4): the anchored
    /// record's, only while that record's verification failed or its
    /// environment was unavailable.
    pub fn override_candidate_digest(&self) -> Option<String> {
        let TaskExecution::RepairRequired { attempt_id, .. } = &self.execution else {
            return None;
        };
        let record = match attempt_id.as_deref() {
            Some(attempt_id) => self
                .unit_records
                .values()
                .find(|record| record.attempt_id.as_deref() == Some(attempt_id))?,
            None => self
                .unit_records
                .values()
                .find(|record| record.settlement == UnitSettlement::Failed)?,
        };
        match &record.verification {
            ValidationOutcome::Unverified { .. } | ValidationOutcome::CheckUnavailable { .. } => {
                record.candidate_digest.clone()
            }
            _ => None,
        }
    }

    /// The work unit currently in task-level verification, if any (E05).
    pub fn verifying_work_unit_id(&self) -> Option<String> {
        if !matches!(self.execution, TaskExecution::Verifying { .. }) {
            return None;
        }
        self.unit_records
            .iter()
            .find(|(_, record)| matches!(record.verification, ValidationOutcome::InProgress))
            .map(|(unit_id, _)| unit_id.clone())
    }

    /// Unit ids whose per-unit records mark them failed, in stable order.
    fn failed_unit_ids(&self) -> Vec<String> {
        self.unit_records
            .iter()
            .filter(|(_, record)| record.settlement == UnitSettlement::Failed)
            .map(|(unit_id, _)| unit_id.clone())
            .collect()
    }

    fn record_digest_for_attempt(&self, attempt_id: &str) -> Option<String> {
        self.unit_records
            .values()
            .find(|record| record.attempt_id.as_deref() == Some(attempt_id))
            .and_then(|record| record.candidate_digest.clone())
    }

    fn failed_record_digest(&self) -> Option<String> {
        self.unit_records
            .values()
            .find(|record| record.settlement == UnitSettlement::Failed)
            .and_then(|record| record.candidate_digest.clone())
    }

    /// The attempt of any still-executing unit, preferring the task-level
    /// slot's own attempt when it is still in flight.
    fn in_flight_attempt(&self) -> Option<String> {
        if let TaskExecution::Running { attempt_id, .. }
        | TaskExecution::WaitingInput { attempt_id, .. } = &self.execution
        {
            let still_flight = self.unit_records.iter().any(|(_, record)| {
                record.settlement == UnitSettlement::InFlight
                    && record.attempt_id.as_deref() == Some(attempt_id.as_str())
            });
            if still_flight
                || self
                    .work_units
                    .iter()
                    .any(|unit| unit.status == WorkUnitStatus::InProgress)
            {
                return Some(attempt_id.clone());
            }
        }
        self.unit_records
            .iter()
            .find(|(_, record)| record.settlement == UnitSettlement::InFlight)
            .and_then(|(_, record)| record.attempt_id.clone())
    }

    /// E05.3: complete read-only units (no write scope, not repo-exclusive)
    /// whose dependencies are all Completed — no attempt, no child — with a
    /// per-unit record marking the no-effect completion. Fixpoint so
    /// read-only chains settle. Returns how many units settled.
    pub fn complete_read_only_units(&mut self) -> usize {
        let mut completed = 0;
        loop {
            let mut progressed = false;
            for index in 0..self.work_units.len() {
                let unit = &self.work_units[index];
                if unit.status != WorkUnitStatus::Pending
                    || (!unit.write_paths.is_empty() || unit.repo_exclusive)
                    || self.unit_records.contains_key(&unit.id)
                {
                    continue;
                }
                let dependencies_completed = unit.dependencies.iter().all(|dependency| {
                    self.work_units.iter().any(|other| {
                        other.id == *dependency && other.status == WorkUnitStatus::Completed
                    })
                });
                if !dependencies_completed {
                    continue;
                }
                let unit_id = unit.id.clone();
                self.work_units[index].status = WorkUnitStatus::Completed;
                self.unit_records.insert(
                    unit_id,
                    UnitRecord {
                        attempt_id: None,
                        candidate_digest: None,
                        verification: ValidationOutcome::NotEvaluated,
                        settlement: UnitSettlement::CompletedWithoutEffect,
                    },
                );
                completed += 1;
                progressed = true;
            }
            if !progressed {
                break;
            }
        }
        completed
    }

    /// Record no-effect completions for read-only units a caller's plan
    /// snapshot already marked Completed (used right after
    /// `self.work_units` is replaced by the plan units in
    /// [`Self::start_execution_attempt`]).
    fn record_swept_read_only_units(&mut self) {
        let swept: Vec<String> = self
            .work_units
            .iter()
            .filter(|unit| {
                unit.status == WorkUnitStatus::Completed
                    && unit.write_paths.is_empty()
                    && !unit.repo_exclusive
            })
            .map(|unit| unit.id.clone())
            .collect();
        for unit_id in swept {
            self.unit_records.entry(unit_id).or_default().settlement =
                UnitSettlement::CompletedWithoutEffect;
        }
    }

    /// E05.2: block never-started units whose dependencies failed (or were
    /// blocked) — a failed unit blocks exactly its dependents, not the
    /// whole task. Fixpoint over the dependency graph.
    fn block_unreachable_dependents(&mut self) {
        loop {
            let mut progressed = false;
            for index in 0..self.work_units.len() {
                if self.work_units[index].status != WorkUnitStatus::Pending {
                    continue;
                }
                let dependency_dead =
                    self.work_units[index]
                        .dependencies
                        .iter()
                        .any(|dependency| {
                            self.work_units.iter().any(|unit| {
                                unit.id == *dependency && unit.status == WorkUnitStatus::Blocked
                            }) || self
                                .unit_records
                                .get(dependency)
                                .is_some_and(|record| record.settlement == UnitSettlement::Failed)
                        });
                if dependency_dead {
                    self.work_units[index].status = WorkUnitStatus::Blocked;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
    }

    /// Mark one unit failed with its verification outcome and reason,
    /// preserving the record's attempt/candidate binding (E05.2). Unknown
    /// unit ids are ignored (callers keep their own repair naming).
    fn fail_unit(
        &mut self,
        work_unit_id: &str,
        attempt_id: Option<String>,
        verification: ValidationOutcome,
    ) {
        if !self.work_units.iter().any(|unit| unit.id == work_unit_id) {
            return;
        }
        if let Some(unit) = self
            .work_units
            .iter_mut()
            .find(|unit| unit.id == work_unit_id)
        {
            unit.status = WorkUnitStatus::Blocked;
        }
        let mut record = self.unit_records.remove(work_unit_id).unwrap_or_default();
        record.attempt_id = record.attempt_id.or(attempt_id);
        record.verification = verification;
        record.settlement = UnitSettlement::Failed;
        self.unit_records.insert(work_unit_id.to_string(), record);
    }

    fn ensure_not_terminal(&self) -> Result<(), TransitionError> {
        if let TaskExecution::Terminal { verdict } = &self.execution {
            return Err(TransitionError::AlreadyTerminal {
                verdict: verdict.clone(),
            });
        }
        Ok(())
    }

    fn current_generation(&self) -> u64 {
        match &self.execution {
            TaskExecution::Running { generation, .. }
            | TaskExecution::WaitingInput { generation, .. } => *generation,
            _ => 0,
        }
    }

    fn check_generation(&self, provided: u64) -> Result<(), TransitionError> {
        let current = self.current_generation();
        if current != 0 && provided != current {
            return Err(TransitionError::LateGeneration { current, provided });
        }
        Ok(())
    }

    /// A run failed (engine/transport/model error): the attempt is over, so
    /// every in-flight unit OF THAT ATTEMPT settles failed and blocks its
    /// dependents (E05.2); concurrent units of other attempts keep
    /// executing and the task-level verdict waits for them. With nothing in
    /// flight the task returns to Pending and the next input may start a
    /// new run. Idempotent for already-idle states.
    pub fn fail_attempt(&mut self) -> Result<(), TransitionError> {
        self.ensure_not_terminal()?;
        let (TaskExecution::Running {
            attempt_id,
            generation,
        }
        | TaskExecution::WaitingInput {
            attempt_id,
            generation,
            ..
        }) = self.execution.clone()
        else {
            return Ok(());
        };
        let reason = "execution attempt ended before verification".to_string();
        let in_flight: Vec<String> =
            self.work_units
                .iter()
                .filter(|unit| {
                    unit.status == WorkUnitStatus::InProgress
                        && match self.unit_records.get(&unit.id) {
                            None => true,
                            // A seeded record (attempt not yet bound) belongs to
                            // the active attempt.
                            Some(record) => {
                                record.attempt_id.as_deref().is_none_or(|record_attempt| {
                                    record_attempt == attempt_id.as_str()
                                }) && record.settlement == UnitSettlement::InFlight
                            }
                        }
                })
                .map(|unit| unit.id.clone())
                .collect();
        if in_flight.is_empty() {
            self.execution = TaskExecution::Pending;
            return Ok(());
        }
        for unit_id in in_flight {
            self.fail_unit(
                &unit_id,
                Some(attempt_id.clone()),
                ValidationOutcome::Unverified {
                    reason: reason.clone(),
                },
            );
        }
        self.block_unreachable_dependents();
        self.complete_read_only_units();
        if let Some(other) = self.in_flight_attempt() {
            // E05.2: siblings of other attempts keep executing.
            self.execution = TaskExecution::Running {
                attempt_id: other,
                generation,
            };
            return Ok(());
        }
        let failed = self.failed_unit_ids();
        let (work_unit_id, repair_reason) = match failed.as_slice() {
            [only] => (Some(only.clone()), reason),
            many => (
                None,
                format!("work units failed: {} ({reason})", many.join(", ")),
            ),
        };
        self.active_approval = None;
        self.execution = TaskExecution::RepairRequired {
            attempt_id: Some(attempt_id),
            work_unit_id,
            reason: repair_reason,
        };
        Ok(())
    }

    /// Reopen a settled task for a follow-up user input: ReviewReady or a
    /// non-terminal-blocked Terminal settles back to Pending. Running and
    /// WaitingInput states cannot reopen (a run is in flight).
    pub fn reopen_for_input(&mut self) -> Result<(), TransitionError> {
        match self.execution {
            TaskExecution::Pending => Ok(()),
            TaskExecution::ReviewReady { .. } | TaskExecution::Terminal { .. } => {
                self.execution = TaskExecution::Pending;
                self.active_approval = None;
                Ok(())
            }
            TaskExecution::AwaitingPlanApproval { .. }
            | TaskExecution::Ready { .. }
            | TaskExecution::Verifying { .. }
            | TaskExecution::RepairRequired { .. } => Err(TransitionError::InvalidTransition {
                from: phase_name(&self.execution),
                action: "reopen_for_input",
            }),
            TaskExecution::Running { .. } | TaskExecution::WaitingInput { .. } => {
                Err(TransitionError::InvalidTransition {
                    from: "active",
                    action: "reopen_for_input",
                })
            }
        }
    }

    /// Start an attempt: Pending -> Running with generation 1.
    pub fn start_attempt(&mut self, attempt: &Attempt) -> Result<(), TransitionError> {
        self.ensure_not_terminal()?;
        if !matches!(self.execution, TaskExecution::Pending) {
            return Err(TransitionError::InvalidTransition {
                from: "active",
                action: "start_attempt",
            });
        }
        if attempt.contract_revision != self.contract.revision {
            return Err(TransitionError::StaleRevision {
                expected: self.contract.revision,
                provided: attempt.contract_revision,
            });
        }
        if self.contract.kind.requires_code_evidence() && attempt.run_snapshot_id().is_none() {
            return Err(TransitionError::RunSnapshotRequired);
        }
        self.execution = TaskExecution::Running {
            attempt_id: attempt.attempt_id.clone(),
            generation: 1,
        };
        Ok(())
    }

    /// Consume one exact approval and start one dependency-ready WorkUnit.
    /// E05: a task may hold N concurrent InProgress units — this transition
    /// is legal from `Ready` (first unit of the wave) and from `Running`
    /// while arming the SAME approval (subsequent units); each started unit
    /// gets its own per-unit record. Read-only units in the plan settle
    /// without an attempt as soon as their dependencies are Completed
    /// (E05.3).
    pub fn start_execution_attempt(
        &mut self,
        actor: Actor,
        attempt: &Attempt,
        approval: &PlanApprovalRef,
        mut plan_units: Vec<WorkUnit>,
        work_unit_id: &str,
    ) -> Result<(), TransitionError> {
        if actor != Actor::Host {
            return Err(TransitionError::HostTransitionRequired {
                action: "start_execution_attempt",
            });
        }
        self.ensure_not_terminal()?;
        match &self.execution {
            TaskExecution::Ready { approval: current } if current == approval => {}
            TaskExecution::Running { .. } if self.active_approval.as_ref() == Some(approval) => {}
            other => {
                return Err(TransitionError::InvalidTransition {
                    from: phase_name(other),
                    action: "start_execution_attempt",
                });
            }
        }
        if !self.contract.kind.requires_code_evidence() {
            return Err(TransitionError::InvalidTransition {
                from: "ready",
                action: "start non-code execution",
            });
        }
        if attempt.contract_revision != self.contract.revision {
            return Err(TransitionError::StaleRevision {
                expected: self.contract.revision,
                provided: attempt.contract_revision,
            });
        }
        if attempt.run_snapshot_id().is_none() {
            return Err(TransitionError::RunSnapshotRequired);
        }
        for unit in &mut plan_units {
            // The task owns unit statuses (E05): a re-presented plan snapshot
            // never resets a prior unit's Completed / InProgress / Blocked
            // fact, so a concurrent second start cannot clobber the first.
            if let Some(prior) = self
                .work_units
                .iter()
                .find(|prior| prior.id == unit.id && prior.status != WorkUnitStatus::Pending)
            {
                unit.status = prior.status;
            }
        }
        // E05.3 sweep over the incoming plan snapshot: read-only units with
        // satisfied dependencies settle Completed before any selection, so
        // their dependents' readiness checks observe them.
        sweep_plan_read_only_units(&mut plan_units);
        let selected = plan_units
            .iter()
            .find(|unit| unit.id == work_unit_id)
            .ok_or_else(|| TransitionError::UnknownWorkUnit(work_unit_id.to_string()))?;
        if selected.status != WorkUnitStatus::Pending
            || (selected.write_paths.is_empty() && !selected.repo_exclusive)
        {
            return Err(TransitionError::InvalidTransition {
                from: "ready",
                action: "start non-writable or active work unit",
            });
        }
        for dependency in &selected.dependencies {
            if !plan_units
                .iter()
                .any(|unit| &unit.id == dependency && unit.status == WorkUnitStatus::Completed)
            {
                return Err(TransitionError::DependencyNotCompleted(dependency.clone()));
            }
        }
        plan_units
            .iter_mut()
            .find(|unit| unit.id == work_unit_id)
            .expect("selected unit exists")
            .status = WorkUnitStatus::InProgress;
        self.work_units = plan_units;
        self.record_swept_read_only_units();
        self.unit_records.insert(
            work_unit_id.to_string(),
            UnitRecord {
                attempt_id: Some(attempt.attempt_id.clone()),
                candidate_digest: None,
                verification: ValidationOutcome::NotEvaluated,
                settlement: UnitSettlement::InFlight,
            },
        );
        self.active_approval = Some(approval.clone());
        self.review = ReviewDisposition::NotRequired;
        self.execution = TaskExecution::Running {
            attempt_id: attempt.attempt_id.clone(),
            generation: 1,
        };
        Ok(())
    }

    /// Begin verifying one unit's candidate. The exactness is record-
    /// anchored (E05): the unit must be in progress under exactly this
    /// attempt. Other units may keep executing concurrently.
    pub fn begin_verification(
        &mut self,
        actor: Actor,
        attempt_id: &str,
        work_unit_id: &str,
        candidate_digest: String,
    ) -> Result<(), TransitionError> {
        if actor != Actor::Host {
            return Err(TransitionError::HostTransitionRequired {
                action: "begin_verification",
            });
        }
        match &self.execution {
            TaskExecution::Running { .. } => {}
            other => {
                return Err(TransitionError::InvalidTransition {
                    from: phase_name(other),
                    action: "begin_verification",
                });
            }
        }
        if !self
            .work_units
            .iter()
            .any(|unit| unit.id == work_unit_id && unit.status == WorkUnitStatus::InProgress)
        {
            return Err(TransitionError::UnknownWorkUnit(work_unit_id.to_string()));
        }
        let Some(record) = self.unit_records.get_mut(work_unit_id) else {
            return Err(TransitionError::InvalidTransition {
                from: phase_name(&self.execution),
                action: "begin_verification",
            });
        };
        if record.attempt_id.as_deref() != Some(attempt_id)
            || record.settlement != UnitSettlement::InFlight
        {
            return Err(TransitionError::InvalidTransition {
                from: phase_name(&self.execution),
                action: "begin_verification",
            });
        }
        record.candidate_digest = Some(candidate_digest);
        record.verification = ValidationOutcome::InProgress;
        self.execution = TaskExecution::Verifying {
            attempt_id: attempt_id.to_string(),
        };
        Ok(())
    }

    /// Finish one unit's verification: the exact host evidence for that
    /// unit's candidate digest settles the unit's per-unit record, and the
    /// task-level aggregate runs (E05.2) — ReviewReady only when every
    /// STARTED unit of the wave is terminal; otherwise the wave continues
    /// and read-only dependents settle on the way.
    pub fn finish_verification(
        &mut self,
        actor: Actor,
        attempt_id: &str,
        work_unit_id: &str,
        requirements: &[EvidenceRequirement],
    ) -> Result<(), TransitionError> {
        if actor != Actor::Host {
            return Err(TransitionError::HostTransitionRequired {
                action: "finish_verification",
            });
        }
        match &self.execution {
            TaskExecution::Verifying { attempt_id: active }
            | TaskExecution::Running {
                attempt_id: active, ..
            } if active == attempt_id => {}
            other => {
                return Err(TransitionError::InvalidTransition {
                    from: phase_name(other),
                    action: "finish_verification",
                });
            }
        }
        if !self
            .work_units
            .iter()
            .any(|unit| unit.id == work_unit_id && unit.status == WorkUnitStatus::InProgress)
        {
            return Err(TransitionError::UnknownWorkUnit(work_unit_id.to_string()));
        }
        let Some(record) = self.unit_records.get(work_unit_id) else {
            return Err(TransitionError::UnknownWorkUnit(work_unit_id.to_string()));
        };
        if record.attempt_id.as_deref() != Some(attempt_id)
            || !matches!(record.verification, ValidationOutcome::InProgress)
        {
            return Err(TransitionError::InvalidTransition {
                from: phase_name(&self.execution),
                action: "finish_verification",
            });
        }
        let candidate = record
            .candidate_digest
            .clone()
            .ok_or(TransitionError::EvidenceRequired)?;
        if !requirements.iter().all(|requirement| {
            self.evidence.iter().any(|record| {
                record.is_valid_for_binding(requirement, &self.contract.task_id, &candidate)
            })
        }) {
            return Err(TransitionError::EvidenceRequired);
        }
        let record = self
            .unit_records
            .get_mut(work_unit_id)
            .expect("record checked above");
        record.verification = ValidationOutcome::Verified {
            candidate_digest: candidate,
        };
        record.settlement = UnitSettlement::Completed;
        self.work_units
            .iter_mut()
            .find(|unit| unit.id == work_unit_id && unit.status == WorkUnitStatus::InProgress)
            .ok_or_else(|| TransitionError::UnknownWorkUnit(work_unit_id.to_string()))?
            .status = WorkUnitStatus::Completed;
        self.block_unreachable_dependents();
        self.complete_read_only_units();
        if self.has_unsettled_units() {
            // E05.2: the wave is still executing; ReviewReady waits.
            if let Some(in_flight) = self.in_flight_attempt() {
                let generation = self.current_generation();
                self.execution = TaskExecution::Running {
                    attempt_id: in_flight,
                    generation,
                };
            }
            return Ok(());
        }
        let failed = self.failed_unit_ids();
        if !failed.is_empty() {
            // E05.2: every started unit is terminal but the wave carries a
            // failed unit — the aggregate lands repair, naming it.
            self.review = ReviewDisposition::NotRequired;
            self.active_approval = None;
            let (work_unit_id, reason) = match failed.as_slice() {
                [only] => (
                    Some(only.clone()),
                    format!("work unit {only} failed verification"),
                ),
                many => (None, format!("work units failed: {}", many.join(", "))),
            };
            let repair_attempt = self
                .unit_records
                .get(&failed[0])
                .and_then(|record| record.attempt_id.clone())
                .unwrap_or_else(|| attempt_id.to_string());
            self.execution = TaskExecution::RepairRequired {
                attempt_id: Some(repair_attempt),
                work_unit_id,
                reason,
            };
            return Ok(());
        }
        self.review = ReviewDisposition::Pending;
        self.active_approval = None;
        self.execution = TaskExecution::ReviewReady {
            attempt_id: attempt_id.to_string(),
        };
        Ok(())
    }

    /// Require repair for one unit (or the task at large). E05.2: the named
    /// unit settles failed and blocks exactly its DEPENDENTS; concurrent
    /// started siblings keep executing and the task-level RepairRequired
    /// waits for them, naming the failed unit(s) when it lands.
    pub fn require_repair(
        &mut self,
        actor: Actor,
        attempt_id: Option<String>,
        work_unit_id: Option<String>,
        reason: String,
        unavailable: bool,
    ) -> Result<(), TransitionError> {
        if actor != Actor::Host {
            return Err(TransitionError::HostTransitionRequired {
                action: "require_repair",
            });
        }
        self.ensure_not_terminal()?;
        let verification = if unavailable {
            ValidationOutcome::CheckUnavailable {
                reason: reason.clone(),
            }
        } else {
            ValidationOutcome::Unverified {
                reason: reason.clone(),
            }
        };
        if let Some(unit_id) = &work_unit_id {
            self.fail_unit(unit_id, attempt_id.clone(), verification);
        }
        self.block_unreachable_dependents();
        self.complete_read_only_units();
        self.review = ReviewDisposition::NotRequired;
        if self.has_unsettled_units() {
            // E05.2: a failed unit blocks only its dependents; started
            // siblings keep executing and the task-level verdict waits.
            if let Some(in_flight) = self.in_flight_attempt() {
                let generation = self.current_generation();
                self.execution = TaskExecution::Running {
                    attempt_id: in_flight,
                    generation,
                };
            }
            return Ok(());
        }
        let failed = self.failed_unit_ids();
        let (repair_work_unit, repair_reason) = match failed.as_slice() {
            [] => (work_unit_id.clone(), reason),
            [only] => (Some(only.clone()), reason),
            many => (
                None,
                format!("work units failed: {} ({reason})", many.join(", ")),
            ),
        };
        self.active_approval = None;
        self.execution = TaskExecution::RepairRequired {
            attempt_id,
            work_unit_id: repair_work_unit,
            reason: repair_reason,
        };
        Ok(())
    }

    /// Accept the verified candidate. E05.4: single-actor explicitness is
    /// kept — the exact attempt/candidate pair must match the anchored
    /// per-unit record — and a task with ANY un-settled started unit
    /// refuses.
    pub fn accept_verified(
        &mut self,
        actor: Actor,
        attempt_id: &str,
        candidate_digest: &str,
        actor_id: &str,
    ) -> Result<(), TransitionError> {
        if actor == Actor::Plugin {
            return Err(TransitionError::HostTransitionRequired {
                action: "accept_verified",
            });
        }
        if actor_id.trim().is_empty() {
            return Err(TransitionError::InvalidTransition {
                from: phase_name(&self.execution),
                action: "accept_verified(empty actor)",
            });
        }
        let anchored = matches!(
            &self.execution,
            TaskExecution::ReviewReady {
                attempt_id: current,
            } if current == attempt_id
        ) && !self.has_unsettled_units()
            && self.review_verified_digest().as_deref() == Some(candidate_digest)
            && self.review == ReviewDisposition::Pending;
        if !anchored {
            return Err(TransitionError::InvalidTransition {
                from: phase_name(&self.execution),
                action: "accept_verified",
            });
        }
        self.review = ReviewDisposition::Accepted;
        self.execution = TaskExecution::Terminal {
            verdict: TaskVerdict::VerifiedAccepted {
                candidate_digest: candidate_digest.to_string(),
                actor_id: actor_id.to_string(),
            },
        };
        Ok(())
    }

    /// Reject the review: the verified unit(s) bound to the rejected
    /// candidate settle failed and enter repair (E05.2).
    pub fn reject_review(
        &mut self,
        actor: Actor,
        attempt_id: &str,
        candidate_digest: &str,
        reason: String,
    ) -> Result<(), TransitionError> {
        if actor == Actor::Plugin {
            return Err(TransitionError::HostTransitionRequired {
                action: "reject_review",
            });
        }
        let anchored = matches!(
            &self.execution,
            TaskExecution::ReviewReady {
                attempt_id: current,
            } if current == attempt_id
        ) && self.review_verified_digest().as_deref() == Some(candidate_digest);
        if !anchored {
            return Err(TransitionError::InvalidTransition {
                from: phase_name(&self.execution),
                action: "reject_review",
            });
        }
        let rejected: Vec<String> = self
            .work_units
            .iter()
            .filter(|unit| {
                unit.status == WorkUnitStatus::Completed
                    && self.unit_records.get(&unit.id).is_some_and(|record| {
                        record.candidate_digest.as_deref() == Some(candidate_digest)
                    })
            })
            .map(|unit| unit.id.clone())
            .collect();
        for unit_id in &rejected {
            self.fail_unit(
                unit_id,
                Some(attempt_id.to_string()),
                ValidationOutcome::Unverified {
                    reason: reason.clone(),
                },
            );
        }
        self.block_unreachable_dependents();
        self.review = ReviewDisposition::Rejected;
        self.active_approval = None;
        self.execution = TaskExecution::RepairRequired {
            attempt_id: Some(attempt_id.to_string()),
            work_unit_id: rejected.first().cloned(),
            reason,
        };
        Ok(())
    }

    /// Accept the unverified candidate with an explicit override. E05.4:
    /// the exact candidate must match the anchored failed record, and a
    /// task with ANY un-settled started unit refuses.
    pub fn accept_unverified(
        &mut self,
        actor: Actor,
        candidate_digest: &str,
        actor_id: &str,
        reason: &str,
        checks: &[String],
    ) -> Result<(), TransitionError> {
        if actor == Actor::Plugin {
            return Err(TransitionError::HostTransitionRequired {
                action: "accept_unverified",
            });
        }
        if actor_id.trim().is_empty()
            || reason.trim().is_empty()
            || checks.is_empty()
            || checks.iter().any(|check| check.trim().is_empty())
            || !matches!(self.execution, TaskExecution::RepairRequired { .. })
            || self.has_unsettled_units()
            || self.override_candidate_digest().as_deref() != Some(candidate_digest)
        {
            return Err(TransitionError::InvalidTransition {
                from: phase_name(&self.execution),
                action: "accept_unverified",
            });
        }
        let mut checks = checks.to_vec();
        checks.sort();
        checks.dedup();
        self.review = ReviewDisposition::OverrideAccepted;
        self.execution = TaskExecution::Terminal {
            verdict: TaskVerdict::UnverifiedAccepted {
                candidate_digest: candidate_digest.to_string(),
                actor_id: actor_id.to_string(),
                reason: reason.to_string(),
                checks,
            },
        };
        Ok(())
    }

    /// Settle the active planning attempt at one exact immutable plan head.
    /// Only the host can make this transition; a plugin can publish/propose,
    /// but cannot declare its own plan authoritative.
    pub fn await_plan_approval(
        &mut self,
        actor: Actor,
        attempt_id: &str,
        generation: u64,
        plan_revision: PlanRevisionRef,
    ) -> Result<(), TransitionError> {
        if actor != Actor::Host {
            return Err(TransitionError::HostTransitionRequired {
                action: "await_plan_approval",
            });
        }
        self.ensure_not_terminal()?;
        match &self.execution {
            TaskExecution::Running {
                attempt_id: active_attempt,
                generation: active_generation,
            } if active_attempt == attempt_id && *active_generation == generation => {
                self.execution = TaskExecution::AwaitingPlanApproval { plan_revision };
                self.review = ReviewDisposition::NotRequired;
                Ok(())
            }
            TaskExecution::Running {
                generation: active_generation,
                ..
            } if *active_generation != generation => Err(TransitionError::LateGeneration {
                current: *active_generation,
                provided: generation,
            }),
            other => Err(TransitionError::InvalidTransition {
                from: phase_name(other),
                action: "await_plan_approval",
            }),
        }
    }

    /// Approve exactly the revision this task is waiting for. The approval is
    /// a plan approval reference, never an effect/tool approval.
    pub fn mark_plan_ready(
        &mut self,
        actor: Actor,
        approval: PlanApprovalRef,
    ) -> Result<(), TransitionError> {
        if actor != Actor::Host {
            return Err(TransitionError::HostTransitionRequired {
                action: "mark_plan_ready",
            });
        }
        self.ensure_not_terminal()?;
        match &self.execution {
            TaskExecution::AwaitingPlanApproval { plan_revision }
                if plan_revision == &approval.plan_revision =>
            {
                self.execution = TaskExecution::Ready { approval };
                Ok(())
            }
            TaskExecution::AwaitingPlanApproval { plan_revision } => {
                Err(TransitionError::WrongPlanRevision {
                    expected: plan_revision.clone(),
                    provided: approval.plan_revision,
                })
            }
            other => Err(TransitionError::InvalidTransition {
                from: phase_name(other),
                action: "mark_plan_ready",
            }),
        }
    }

    /// Explicitly invalidate an awaiting/approved plan after a material input
    /// changes or the user requests a new revision. Persisting the matching
    /// approval supersession is the store's responsibility and happens in the
    /// same SQLite transaction as this aggregate state.
    pub fn invalidate_plan(&mut self, actor: Actor) -> Result<(), TransitionError> {
        if actor != Actor::Host {
            return Err(TransitionError::HostTransitionRequired {
                action: "invalidate_plan",
            });
        }
        self.ensure_not_terminal()?;
        match self.execution {
            TaskExecution::AwaitingPlanApproval { .. } | TaskExecution::Ready { .. } => {
                self.execution = TaskExecution::Pending;
                self.active_approval = None;
                self.review = ReviewDisposition::NotRequired;
                Ok(())
            }
            ref other => Err(TransitionError::InvalidTransition {
                from: phase_name(other),
                action: "invalidate_plan",
            }),
        }
    }

    /// Suspend waiting for a user answer.
    pub fn wait_for_input(
        &mut self,
        generation: u64,
        question_id: &str,
    ) -> Result<(), TransitionError> {
        self.ensure_not_terminal()?;
        self.check_generation(generation)?;
        match self.execution.clone() {
            TaskExecution::Running {
                attempt_id,
                generation,
            } => {
                self.execution = TaskExecution::WaitingInput {
                    attempt_id,
                    generation,
                    question_id: question_id.to_string(),
                };
                Ok(())
            }
            other => Err(TransitionError::InvalidTransition {
                from: phase_name(&other),
                action: "wait_for_input",
            }),
        }
    }

    /// Resume from a waiting question. Answering the same question again is a
    /// no-op (idempotent continuation).
    pub fn answer_input(
        &mut self,
        generation: u64,
        question_id: &str,
    ) -> Result<bool, TransitionError> {
        self.ensure_not_terminal()?;
        match self.execution.clone() {
            TaskExecution::WaitingInput {
                attempt_id,
                generation: g,
                question_id: waiting,
            } => {
                if g != generation {
                    return Err(TransitionError::LateGeneration {
                        current: g,
                        provided: generation,
                    });
                }
                if waiting != question_id {
                    return Err(TransitionError::WrongQuestion(waiting));
                }
                self.execution = TaskExecution::Running {
                    attempt_id,
                    generation: g,
                };
                Ok(true)
            }
            TaskExecution::Running { .. } => Ok(false),
            other => Err(TransitionError::InvalidTransition {
                from: phase_name(&other),
                action: "answer_input",
            }),
        }
    }

    /// Update a work unit with revision fencing and dependency/evidence gates.
    pub fn update_work_unit(
        &mut self,
        contract_revision: u64,
        update: &WorkUnitUpdate,
    ) -> Result<(), TransitionError> {
        self.ensure_not_terminal()?;
        if contract_revision != self.contract.revision {
            return Err(TransitionError::StaleRevision {
                expected: self.contract.revision,
                provided: contract_revision,
            });
        }
        let (dependencies, acceptance) = {
            let unit = self
                .work_units
                .iter()
                .find(|unit| unit.id == update.work_unit_id)
                .ok_or_else(|| TransitionError::UnknownWorkUnit(update.work_unit_id.clone()))?;
            (unit.dependencies.clone(), unit.acceptance.clone())
        };
        if update.status == WorkUnitStatus::Completed {
            for dep in &dependencies {
                let dep_state = self
                    .work_units
                    .iter()
                    .find(|other| &other.id == dep)
                    .map(|other| other.status)
                    .unwrap_or(WorkUnitStatus::Pending);
                if dep_state != WorkUnitStatus::Completed {
                    return Err(TransitionError::DependencyNotCompleted(dep.clone()));
                }
            }
            if !acceptance.is_empty()
                && self.contract.kind.requires_code_evidence()
                && !self.has_current_evidence_for(&update.work_unit_id, &acceptance)
            {
                return Err(TransitionError::EvidenceRequired);
            }
        }
        let unit = self
            .work_units
            .iter_mut()
            .find(|unit| unit.id == update.work_unit_id)
            .expect("existence checked above");
        unit.status = update.status;
        // An externally completed unit settles its per-unit record so the
        // aggregate never waits on a unit the wave already finished.
        if update.status == WorkUnitStatus::Completed {
            if let Some(record) = self.unit_records.get_mut(&update.work_unit_id) {
                if !record.settlement.is_terminal() {
                    record.settlement = UnitSettlement::Completed;
                }
            }
        }
        Ok(())
    }

    /// Record host-owned evidence. Plugin-provenance records are stored but
    /// never count toward acceptance.
    pub fn record_evidence(&mut self, record: EvidenceRecord) -> Result<(), TransitionError> {
        self.ensure_not_terminal()?;
        self.evidence.push(record);
        Ok(())
    }

    fn has_current_evidence_for(&self, work_unit_id: &str, check_ids: &[String]) -> bool {
        let Some(digest) = self
            .unit_records
            .get(work_unit_id)
            .and_then(|record| record.candidate_digest.as_deref())
        else {
            return false;
        };
        check_ids.iter().all(|check_id| {
            self.evidence.iter().any(|record| {
                record.task_id == self.contract.task_id && record.is_valid_for(check_id, digest)
            })
        })
    }

    /// Set one unit's candidate digest (E05: per-unit); evidence keyed to
    /// older digests stops counting (stale evidence). Hosts seed a unit's
    /// record before dispatch; the record is created in-flight when absent.
    pub fn set_unit_candidate_digest(
        &mut self,
        work_unit_id: &str,
        digest: Option<String>,
    ) -> Result<(), TransitionError> {
        self.ensure_not_terminal()?;
        let record = self
            .unit_records
            .entry(work_unit_id.to_string())
            .or_default();
        record.candidate_digest = digest;
        Ok(())
    }

    /// Handle a plugin completion proposal. The kernel — never the plugin —
    /// decides the verdict.
    pub fn apply_proposal(
        &mut self,
        generation: u64,
        proposal: &CompletionProposal,
    ) -> Result<ProposalDecision, TransitionError> {
        self.ensure_not_terminal()?;
        self.check_generation(generation)?;
        if proposal.actor != Actor::Plugin {
            return Err(TransitionError::InvalidTransition {
                from: phase_name(&self.execution),
                action: "apply_proposal(non-plugin)",
            });
        }
        if !self.contract.kind.requires_code_evidence() {
            // Replies and plan drafts may settle without code checks; the
            // kernel still records that no code verification occurred.
            let verdict = TaskVerdict::Unverified {
                reason: "no code verification required for this task kind".into(),
            };
            self.execution = TaskExecution::ReviewReady {
                attempt_id: current_attempt(&self.execution),
            };
            return Ok(ProposalDecision::Accept { verdict });
        }
        if proposal.kind != ProposalKind::Implementation {
            return Ok(ProposalDecision::Repair {
                feedback: "code tasks require an Implementation proposal".to_string(),
            });
        }
        Ok(ProposalDecision::Repair {
            feedback: "implementation proposals require host verification".to_string(),
        })
    }

    /// Issue a terminal verdict. Only Host/User actors may do this; a plugin
    /// attempting it is rejected outright.
    pub fn finalize(&mut self, actor: Actor, verdict: TaskVerdict) -> Result<(), TransitionError> {
        match actor {
            Actor::Plugin => Err(TransitionError::PluginVerdictRejected),
            Actor::Host | Actor::User => {
                self.ensure_not_terminal()?;
                self.execution = TaskExecution::Terminal { verdict };
                Ok(())
            }
        }
    }

    /// Cancel with generation fencing: the revocation happens first, then the
    /// task drains to terminal. Late work from the old generation cannot
    /// resurrect it.
    pub fn cancel(
        &mut self,
        actor: Actor,
        generation: u64,
        reason: &str,
    ) -> Result<(), TransitionError> {
        if actor == Actor::Plugin {
            return Err(TransitionError::PluginVerdictRejected);
        }
        self.ensure_not_terminal()?;
        self.check_generation(generation)?;
        self.execution = TaskExecution::Terminal {
            verdict: TaskVerdict::Cancelled {
                reason: reason.to_string(),
            },
        };
        Ok(())
    }
}

/// A completion proposal submitted through the kernel boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionProposal {
    pub actor: Actor,
    pub kind: ProposalKind,
    pub summary: String,
    pub candidate_digest: Option<String>,
}

/// Reuse of the protocol's proposal kinds without dragging wire concerns in.
pub use r_code_harness_protocol::services::ProposalKind;

/// What the kernel decided about a proposal.
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalDecision {
    /// Accepted; the verdict is decided by the kernel, ready to finalize.
    Accept { verdict: TaskVerdict },
    /// Rejected before any state change.
    Reject { reason: String },
    /// Repairable check failures; run continues with feedback.
    Repair { feedback: String },
}

/// A work-unit status update with the revision it was authored against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkUnitUpdate {
    pub work_unit_id: String,
    pub status: WorkUnitStatus,
}

fn phase_name(execution: &TaskExecution) -> &'static str {
    match execution {
        TaskExecution::Pending => "pending",
        TaskExecution::Running { .. } => "running",
        TaskExecution::WaitingInput { .. } => "waiting-input",
        TaskExecution::AwaitingPlanApproval { .. } => "awaiting-plan-approval",
        TaskExecution::Ready { .. } => "ready",
        TaskExecution::Verifying { .. } => "verifying",
        TaskExecution::RepairRequired { .. } => "repair-required",
        TaskExecution::ReviewReady { .. } => "review-ready",
        TaskExecution::Terminal { .. } => "terminal",
    }
}

/// Status-only E05.3 sweep over an incoming plan snapshot (the record side
/// happens once the snapshot is assigned to the task).
fn sweep_plan_read_only_units(plan_units: &mut [WorkUnit]) {
    loop {
        let mut progressed = false;
        for index in 0..plan_units.len() {
            let unit = &plan_units[index];
            if unit.status != WorkUnitStatus::Pending
                || (!unit.write_paths.is_empty() || unit.repo_exclusive)
            {
                continue;
            }
            let dependencies_completed = unit.dependencies.iter().all(|dependency| {
                plan_units.iter().any(|other| {
                    other.id == *dependency && other.status == WorkUnitStatus::Completed
                })
            });
            if dependencies_completed {
                plan_units[index].status = WorkUnitStatus::Completed;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
}

fn current_attempt(execution: &TaskExecution) -> String {
    match execution {
        TaskExecution::Running { attempt_id, .. }
        | TaskExecution::WaitingInput { attempt_id, .. }
        | TaskExecution::Verifying { attempt_id, .. } => attempt_id.clone(),
        _ => String::new(),
    }
}
