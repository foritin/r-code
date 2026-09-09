//! Verification profiles and evidence validity.
//!
//! [`CheckDefinition`] separates *frozen acceptance-control material*
//! (entrypoint bytes, control files — stored outside plugin write scope)
//! from *candidate material* (implementation and dependencies that may
//! change). Evidence validity is keyed by (check identity, candidate
//! digest, environment identity); candidate-redefinable script aliases
//! (npm script names) can never stand in for a frozen required check.

use r_code_harness_protocol::ArtifactRef;
use serde::{Deserialize, Serialize};

/// A task-kind-aware check definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckDefinition {
    pub check_id: String,
    /// Direct execution entry: an explicit command, never a candidate
    /// package.json script alias.
    pub entrypoint: CheckEntrypoint,
    /// Frozen acceptance-control files (path → digest) stored outside the
    /// plugin-writable workspace.
    #[serde(default)]
    pub control_files: Vec<ControlFile>,
    /// Source roots the check reads.
    #[serde(default)]
    pub source_roots: Vec<String>,
    /// Dependency manifests/lockfiles preserved from the candidate.
    #[serde(default)]
    pub dependency_locks: Vec<String>,
    /// Toolchain identity (e.g. "rustc 1.88", "node 22").
    pub toolchain: String,
    /// Declared external inputs (submodules, ignored files, outside paths).
    #[serde(default)]
    pub declared_external_inputs: Vec<String>,
    /// Content-addressed copy of the entrypoint bytes, stored host-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint_bytes: Option<ArtifactRef>,
}

/// How the check runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CheckEntrypoint {
    /// A direct command with explicit argv.
    Command { program: String, argv: Vec<String> },
}

/// A frozen control file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlFile {
    pub path: String,
    pub sha256: String,
}

impl CheckDefinition {
    /// Structural validation: a required check must have a direct
    /// entrypoint and pinned control material.
    pub fn validate(&self) -> Result<(), CheckDefinitionError> {
        if self.check_id.trim().is_empty() {
            return Err(CheckDefinitionError::EmptyId);
        }
        match &self.entrypoint {
            CheckEntrypoint::Command { program, argv } => {
                if program.trim().is_empty() {
                    return Err(CheckDefinitionError::NoEntrypoint(self.check_id.clone()));
                }
                let _ = argv;
            }
        }
        for control in &self.control_files {
            if control.sha256.len() != 64 {
                return Err(CheckDefinitionError::UnpinnedControlFile(
                    control.path.clone(),
                ));
            }
        }
        Ok(())
    }

    /// The identity of this check definition: any change to entrypoint or
    /// control material invalidates prior evidence.
    pub fn identity(&self) -> String {
        let material = serde_json::to_string(self).unwrap_or_default();
        r_code_harness_protocol::canonical_input_hash(&serde_json::Value::String(material))
    }

    /// Whether a candidate npm-script alias could impersonate this check:
    /// never, by construction — entrypoints are explicit commands.
    pub fn is_aliasable_by_candidate_scripts(&self) -> bool {
        false
    }
}

/// Errors validating check definitions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckDefinitionError {
    #[error("check id must not be empty")]
    EmptyId,
    #[error("check {0} has no direct entrypoint")]
    NoEntrypoint(String),
    #[error("control file {0} is not pinned by a sha256 digest")]
    UnpinnedControlFile(String),
}

/// Immutable acceptance-profile revision: the frozen set of checks a
/// contract requires at a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceProfile {
    pub revision: u64,
    pub check_ids: Vec<String>,
    /// Digest over the full definitions: weakening requires a new,
    /// user-authorized revision.
    pub definitions_digest: String,
}

/// Evidence validity rules. Evidence binds a check to a candidate digest
/// and environment; anything else is stale or foreign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceValidity;

impl EvidenceValidity {
    /// Whether an evidence record satisfies a check for a candidate.
    pub fn is_valid(
        record: &crate::task::EvidenceRecord,
        check: &CheckDefinition,
        candidate_digest: &str,
        environment: &str,
    ) -> bool {
        record.check_id == check.check_id
            && record.candidate_digest == candidate_digest
            && record.environment == environment
            && record.passed
            && matches!(
                record.recorded_by,
                r_code_harness_protocol::Provenance::Host
            )
    }

    /// Weakening detection: a proposed profile is weaker when it drops a
    /// required check or changes a definition (digest differs).
    pub fn is_weaker_than(proposed: &AcceptanceProfile, current: &AcceptanceProfile) -> bool {
        proposed.revision <= current.revision
            || proposed.check_ids.len() < current.check_ids.len()
            || current
                .check_ids
                .iter()
                .any(|check| !proposed.check_ids.contains(check))
            || proposed.definitions_digest != current.definitions_digest
    }
}

/// Build an acceptance profile over definitions (dense digest).
pub fn build_acceptance_profile(
    revision: u64,
    definitions: &[CheckDefinition],
) -> Result<AcceptanceProfile, CheckDefinitionError> {
    for definition in definitions {
        definition.validate()?;
    }
    let material: Vec<String> = definitions.iter().map(CheckDefinition::identity).collect();
    let digest = r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
        "checks": material,
    }));
    Ok(AcceptanceProfile {
        revision,
        check_ids: definitions
            .iter()
            .map(|check| check.check_id.clone())
            .collect(),
        definitions_digest: digest,
    })
}
