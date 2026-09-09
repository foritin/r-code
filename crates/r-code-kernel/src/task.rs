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
    pub status: WorkUnitStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkUnitStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

/// A pinned attempt: plugin package, contract, context and workspace identity
/// are frozen for its lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub check_id: String,
    /// Content digest of the candidate the check ran against.
    pub candidate_digest: String,
    /// Toolchain/environment identity the check ran in.
    pub environment: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_output: Option<ArtifactRef>,
    /// Only host-provenance records count as acceptance evidence.
    pub recorded_by: Provenance,
}

impl EvidenceRecord {
    pub fn is_valid_for(&self, check_id: &str, candidate_digest: &str) -> bool {
        self.passed
            && self.check_id == check_id
            && self.candidate_digest == candidate_digest
            && matches!(self.recorded_by, Provenance::Host)
    }
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
    ReviewReady {
        attempt_id: String,
    },
    Terminal {
        verdict: TaskVerdict,
    },
}

/// Independent validation outcome; separate from execution and review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ValidationOutcome {
    NotEvaluated,
    InProgress,
    Verified { candidate_digest: String },
    Unverified { reason: String },
    CheckUnavailable { reason: String },
}

/// User review disposition; separate from execution and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewDisposition {
    NotRequired,
    Pending,
    Accepted,
    Rejected,
}

/// The authoritative terminal verdict. Issued by the kernel only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum TaskVerdict {
    Verified { candidate_digest: String },
    Unverified { reason: String },
    Blocked { reason: String },
    Failed { reason: String },
    Cancelled { reason: String },
}

impl TaskVerdict {
    pub fn is_terminal(&self) -> bool {
        true
    }
}

/// Aggregate root for one task branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskState {
    pub contract: TaskContract,
    #[serde(default)]
    pub work_units: Vec<WorkUnit>,
    pub execution: TaskExecution,
    pub validation: ValidationOutcome,
    pub review: ReviewDisposition,
    #[serde(default)]
    pub evidence: Vec<EvidenceRecord>,
    /// Digest of the current candidate content, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_digest: Option<String>,
    /// Human-facing session title (UI metadata; never affects contracts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Per-task harness preferences (model selection, inference knobs, mode
    /// label) applied to future runs. Not part of contract identity.
    #[serde(default)]
    pub preferences: TaskPreferences,
}

/// User-set run preferences carried into harness config on each run.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TaskPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Opaque inference knobs (thinking level, effort) forwarded verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
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
}

impl TaskState {
    pub fn new(contract: TaskContract) -> Self {
        Self {
            contract,
            work_units: Vec::new(),
            execution: TaskExecution::Pending,
            validation: ValidationOutcome::NotEvaluated,
            review: ReviewDisposition::NotRequired,
            evidence: Vec::new(),
            candidate_digest: None,
            title: None,
            preferences: TaskPreferences::default(),
        }
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

    /// A run failed (engine/transport/model error): the attempt is over and
    /// nothing is executing, so the task returns to Pending and the next
    /// input may start a new run. Idempotent for already-idle states.
    pub fn fail_attempt(&mut self) -> Result<(), TransitionError> {
        self.ensure_not_terminal()?;
        match self.execution {
            TaskExecution::Running { .. } | TaskExecution::WaitingInput { .. } => {
                self.execution = TaskExecution::Pending;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Reopen a settled task for a follow-up user input: ReviewReady or a
    /// non-terminal-blocked Terminal settles back to Pending. Running and
    /// WaitingInput states cannot reopen (a run is in flight).
    pub fn reopen_for_input(&mut self) -> Result<(), TransitionError> {
        match self.execution {
            TaskExecution::Pending => Ok(()),
            TaskExecution::ReviewReady { .. } | TaskExecution::Terminal { .. } => {
                self.execution = TaskExecution::Pending;
                Ok(())
            }
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
        self.execution = TaskExecution::Running {
            attempt_id: attempt.attempt_id.clone(),
            generation: 1,
        };
        Ok(())
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
                && !self.has_current_evidence_for(&acceptance)
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
        Ok(())
    }

    /// Record host-owned evidence. Plugin-provenance records are stored but
    /// never count toward acceptance.
    pub fn record_evidence(&mut self, record: EvidenceRecord) -> Result<(), TransitionError> {
        self.ensure_not_terminal()?;
        self.evidence.push(record);
        Ok(())
    }

    fn has_current_evidence_for(&self, check_ids: &[String]) -> bool {
        let digest = match &self.candidate_digest {
            Some(digest) => digest,
            None => return false,
        };
        check_ids.iter().all(|check_id| {
            self.evidence
                .iter()
                .any(|record| record.is_valid_for(check_id, digest))
        })
    }

    /// Set the current candidate digest; evidence keyed to older digests
    /// stops counting (stale evidence).
    pub fn set_candidate_digest(&mut self, digest: Option<String>) -> Result<(), TransitionError> {
        self.ensure_not_terminal()?;
        self.candidate_digest = digest;
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
        if !self.contract.kind.requires_code_evidence() || proposal.kind == ProposalKind::Reply {
            // Replies and plan drafts may settle without code checks; the
            // kernel still records that no code verification occurred.
            let verdict = TaskVerdict::Unverified {
                reason: "no code verification required for this task kind".into(),
            };
            self.execution = TaskExecution::ReviewReady {
                attempt_id: current_attempt(&self.execution),
            };
            self.validation = ValidationOutcome::NotEvaluated;
            return Ok(ProposalDecision::Accept { verdict });
        }
        let digest = match (&self.candidate_digest, &proposal.candidate_digest) {
            (Some(current), Some(claimed)) if current == claimed => current.clone(),
            (Some(current), None) => current.clone(),
            _ => {
                return Ok(ProposalDecision::Reject {
                    reason: "proposal does not match the current candidate content".into(),
                })
            }
        };
        let missing: Vec<String> = self
            .contract
            .required_checks
            .iter()
            .filter(|check_id| {
                !self
                    .evidence
                    .iter()
                    .any(|record| record.is_valid_for(check_id, &digest))
            })
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Ok(ProposalDecision::Accept {
                verdict: TaskVerdict::Unverified {
                    reason: format!(
                        "missing required evidence for checks: {}",
                        missing.join(", ")
                    ),
                },
            });
        }
        // All required evidence passes for the current candidate.
        self.validation = ValidationOutcome::Verified {
            candidate_digest: digest.clone(),
        };
        self.execution = TaskExecution::ReviewReady {
            attempt_id: current_attempt(&self.execution),
        };
        Ok(ProposalDecision::Accept {
            verdict: TaskVerdict::Verified {
                candidate_digest: digest,
            },
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
        TaskExecution::ReviewReady { .. } => "review-ready",
        TaskExecution::Terminal { .. } => "terminal",
    }
}

fn current_attempt(execution: &TaskExecution) -> String {
    match execution {
        TaskExecution::Running { attempt_id, .. }
        | TaskExecution::WaitingInput { attempt_id, .. } => attempt_id.clone(),
        _ => String::new(),
    }
}
