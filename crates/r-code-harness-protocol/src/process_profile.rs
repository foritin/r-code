//! Declarative process-profile constraints.
//!
//! A [`ProcessProfileSchema`] is package data pinned by a trusted plugin
//! package. The host runs a *neutral interpreter* over complete outbound
//! frames: framing, method allowlist and JSON-pointer field constraints
//! bound to host-owned values. No host code contains harness-specific method
//! switches — Codex's App Server rules live in its own package.

use serde::{Deserialize, Serialize};

/// Framing of the constrained stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileFraming {
    /// Newline-delimited JSON; every frame validated before forwarding.
    NdjsonRpc,
    /// Opaque bytes; requires non-restricted authorization.
    Raw,
}

/// A host-owned value a field can be bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostValue {
    /// The task's workspace root.
    WorkspaceRoot,
    TaskId,
    RunId,
    AttemptId,
    /// The sandbox/approval ceiling name resolved by the host.
    PermissionCeiling,
}

/// Constraint applied to a params field addressed by JSON pointer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FieldConstraint {
    /// The field must equal the resolved host value.
    BoundTo { value: HostValue },
    /// The field must be a path at or below the workspace root.
    WithinWorkspace,
    /// The field must not appear at all (e.g. permission-affecting knobs).
    MustBeAbsent,
    /// The field must be one of the enumerated literals.
    Enum { values: Vec<String> },
}

/// One constrained params field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldRule {
    /// JSON pointer into the frame's params object, e.g. "/cwd".
    pub pointer: String,
    pub constraint: FieldConstraint,
}

/// One allowed outbound method with its field rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodRule {
    pub name: String,
    #[serde(default)]
    pub params: Vec<FieldRule>,
}

/// The full profile: framing, allowlist and bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessProfileSchema {
    pub name: String,
    pub framing: ProfileFraming,
    #[serde(default)]
    pub methods: Vec<MethodRule>,
    /// Environment names the profile may see (non-secret literals only).
    #[serde(default)]
    pub env: Vec<String>,
}

/// A constraint violation found by the interpreter.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConstraintViolation {
    #[error("method {0:?} is not in the profile allowlist")]
    UnknownMethod(String),
    #[error("field {pointer} is required to be bound to a host value but is missing")]
    MissingBound { pointer: String },
    #[error("field {pointer} must not be present in outbound frames")]
    ForbiddenField { pointer: String },
    #[error("field {pointer} value {found:?} does not match the host-bound value {expected:?}")]
    NotHostBound {
        pointer: String,
        found: String,
        expected: String,
    },
    #[error("field {pointer} value {found:?} escapes the workspace root")]
    OutsideWorkspace { pointer: String, found: String },
    #[error("field {pointer} value {found:?} is not one of the allowed values")]
    NotEnumerated { pointer: String, found: String },
    #[error("raw framing cannot be validated as NDJSON-RPC")]
    RawFraming,
    #[error("frame is not a JSON-RPC request")]
    NotARpcRequest,
}

impl ProcessProfileSchema {
    /// Validate one complete outbound JSON-RPC frame against this profile.
    /// `workspace_root`, `task_id`, `run_id`, `attempt_id` and `ceiling` are
    /// the host-owned values bindings resolve to.
    pub fn validate_outbound(
        &self,
        frame: &serde_json::Value,
        host: &HostBindings,
    ) -> Result<(), ConstraintViolation> {
        if self.framing != ProfileFraming::NdjsonRpc {
            return Err(ConstraintViolation::RawFraming);
        }
        let method = frame
            .get("method")
            .and_then(|value| value.as_str())
            .ok_or(ConstraintViolation::NotARpcRequest)?;
        let rule = self
            .methods
            .iter()
            .find(|rule| rule.name == method)
            .ok_or_else(|| ConstraintViolation::UnknownMethod(method.to_string()))?;
        let empty = serde_json::Map::new();
        let params = frame
            .get("params")
            .and_then(|value| value.as_object())
            .unwrap_or(&empty);
        for field in &rule.params {
            // Resolve the JSON pointer within params.
            let resolved = resolve_pointer(params, &field.pointer);
            match &field.constraint {
                FieldConstraint::MustBeAbsent => {
                    if resolved.is_some() {
                        return Err(ConstraintViolation::ForbiddenField {
                            pointer: field.pointer.clone(),
                        });
                    }
                }
                FieldConstraint::BoundTo { value } => {
                    let expected = host.resolve(value);
                    match resolved.and_then(|value| value.as_str()) {
                        Some(found) if found == expected => {}
                        Some(found) => {
                            return Err(ConstraintViolation::NotHostBound {
                                pointer: field.pointer.clone(),
                                found: found.to_string(),
                                expected,
                            })
                        }
                        None => {
                            return Err(ConstraintViolation::MissingBound {
                                pointer: field.pointer.clone(),
                            })
                        }
                    }
                }
                FieldConstraint::WithinWorkspace => {
                    let found = resolved.and_then(|value| value.as_str()).unwrap_or("");
                    if !path_within(found, &host.workspace_root) {
                        return Err(ConstraintViolation::OutsideWorkspace {
                            pointer: field.pointer.clone(),
                            found: found.to_string(),
                        });
                    }
                }
                FieldConstraint::Enum { values } => {
                    let found = resolved.and_then(|value| value.as_str()).unwrap_or("");
                    if !values.iter().any(|allowed| allowed == found) {
                        return Err(ConstraintViolation::NotEnumerated {
                            pointer: field.pointer.clone(),
                            found: found.to_string(),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Host-owned values the interpreter binds to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostBindings {
    pub workspace_root: String,
    pub task_id: String,
    pub run_id: String,
    pub attempt_id: String,
    pub permission_ceiling: String,
}

impl HostBindings {
    pub fn resolve(&self, value: &HostValue) -> String {
        match value {
            HostValue::WorkspaceRoot => self.workspace_root.clone(),
            HostValue::TaskId => self.task_id.clone(),
            HostValue::RunId => self.run_id.clone(),
            HostValue::AttemptId => self.attempt_id.clone(),
            HostValue::PermissionCeiling => self.permission_ceiling.clone(),
        }
    }
}

fn resolve_pointer<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    pointer: &str,
) -> Option<&'a serde_json::Value> {
    let mut segments = pointer.trim_start_matches('/').split('/');
    let first = segments.next()?;
    let mut current = object.get(first)?;
    for segment in segments {
        current = current.get(segment)?;
    }
    Some(current)
}

fn path_within(path: &str, root: &str) -> bool {
    let normalize = |value: &str| value.replace('\\', "/");
    let root = normalize(root).trim_end_matches('/').to_string();
    if root.is_empty() {
        return false;
    }
    normalize(path).starts_with(&root)
}
