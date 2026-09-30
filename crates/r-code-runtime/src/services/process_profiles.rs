//! Neutral interpreter for declarative process-profile constraints.
//!
//! The host buffers complete NDJSON frames and validates each one against
//! the package-pinned [`ProcessProfileSchema`] *before* any byte is
//! forwarded to the child process: only whole, well-formed JSON-RPC frames
//! are ever validated or forwarded, so frame splitting cannot bypass the
//! checks. Harness-specific rules live in plugin-package data.
//!
//! It also answers "what may this pinned profile touch?" exactly once:
//! [`resolve_profile_effect`] maps a pinned profile and its host-pinned
//! declaration onto a [`ProcessProfileEffect`], and the ordering helper
//! there is the ceiling both the process service and the router consult.

use r_code_harness_protocol::process_profile::{
    ConstraintViolation, FieldConstraint, HostBindings, HostValue, ProcessProfileSchema,
};

/// A bound validator for one run.
#[derive(Debug, Clone)]
pub struct FrameValidator {
    profile: ProcessProfileSchema,
    bindings: HostBindings,
}

impl FrameValidator {
    pub fn new(profile: ProcessProfileSchema, bindings: HostBindings) -> Self {
        Self { profile, bindings }
    }

    pub fn profile(&self) -> &ProcessProfileSchema {
        &self.profile
    }

    /// Validate one complete buffered frame (the newline is already
    /// stripped). Partial or malformed content is refused — the caller only
    /// reaches here with a full line.
    pub fn validate_complete_frame(&self, line: &str) -> Result<(), ConstraintViolation> {
        let frame: serde_json::Value =
            serde_json::from_str(line).map_err(|_| ConstraintViolation::NotARpcRequest)?;
        self.profile.validate_outbound(&frame, &self.bindings)
    }

    /// Split a received buffer into complete frames, validating each one.
    /// Any trailing partial data is returned for re-buffering; a partial
    /// frame is never validated or forwarded.
    pub fn drain_buffer(
        &self,
        buffer: &mut Vec<u8>,
    ) -> Result<Vec<serde_json::Value>, ConstraintViolation> {
        let mut frames = Vec::new();
        while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=newline).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1])
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            let frame: serde_json::Value =
                serde_json::from_str(&text).map_err(|_| ConstraintViolation::NotARpcRequest)?;
            self.profile.validate_outbound(&frame, &self.bindings)?;
            frames.push(frame);
        }
        Ok(frames)
    }
}

/// Load a profile from package bytes (already pinned/verified).
pub fn parse_profile(bytes: &[u8]) -> Result<ProcessProfileSchema, String> {
    serde_json::from_slice(bytes).map_err(|error| format!("invalid process profile: {error}"))
}

/// What the child of one pinned profile is allowed to touch, ordered from
/// least to most authority (P22). The declaration is host-pinned: a profile
/// that reaches the workspace in its frame rules can only ever be the
/// widest effect, and the widest effect is the one P22 must keep
/// undiscoverable until P27/P24B.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProcessProfileEffect {
    /// No workspace at all: reads and writes stay outside any checkout.
    NoWorkspace,
    /// A host-owned scratch tree only, below the declared scratch root.
    ScratchOnly,
    /// Writes into the checked-out workspace: hidden in this wave.
    CurrentCheckoutWrite,
}

/// Why an effect cannot be named. Every variant is a refusal — the mapping
/// has no permissive default and no `Unknown` effect to fall back to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessProfileEffectError {
    #[error("profile declares no process effect")]
    Undeclared,
    #[error("process effect {0:?} is not a pinned declaration literal")]
    UnknownDeclaration(String),
    #[error("profile {0:?} constrains frames against the workspace but declares {1}")]
    WorkspaceConflict(String, String),
    #[error("profile {0:?} declares {1} but no frame rule can reach the workspace")]
    WorkspaceUnreachable(String, String),
}

impl ProcessProfileEffect {
    /// The highest effect the interactive Process set may be granted for
    /// (P22.3): a scratch tree, never the current checkout.
    pub const INTERACTIVE_CEILING: Self = Self::ScratchOnly;

    /// The pinned declaration literal this effect is written with.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoWorkspace => "no-workspace",
            Self::ScratchOnly => "scratch-only",
            Self::CurrentCheckoutWrite => "current-checkout-write",
        }
    }

    /// Read a declaration literal exactly: anything but the three pinned
    /// spellings is refused, never coerced into a nearby effect.
    pub fn parse(declaration: &str) -> Result<Self, ProcessProfileEffectError> {
        match declaration {
            "no-workspace" => Ok(Self::NoWorkspace),
            "scratch-only" => Ok(Self::ScratchOnly),
            "current-checkout-write" => Ok(Self::CurrentCheckoutWrite),
            other => Err(ProcessProfileEffectError::UnknownDeclaration(
                other.to_string(),
            )),
        }
    }

    /// Whether this effect writes into the checked-out workspace (P22).
    pub fn is_workspace_write(&self) -> bool {
        *self == Self::CurrentCheckoutWrite
    }

    /// Total ordering against a ceiling; the only place the comparison is
    /// spelled, so no caller can invent its own width rule.
    pub fn at_or_below(&self, ceiling: &Self) -> bool {
        self <= ceiling
    }

    /// The single predicate the service and the router share: may the
    /// complete interactive Process set be resolved and advertised for this
    /// effect at all.
    pub fn interactive_process_admitted(&self) -> bool {
        self.at_or_below(&Self::INTERACTIVE_CEILING)
    }
}

/// Map a package-pinned profile and its host-pinned effect declaration onto
/// the effect it really grants, or refuse. Absent and unknown declarations
/// are errors; so is any disagreement between the declaration and the frame
/// rules the package pinned, in both directions. A profile may never declare
/// less than its own rules allow, because the frame validator — not the
/// declaration — is what the child actually gets.
pub fn resolve_profile_effect(
    profile: &ProcessProfileSchema,
    declaration: Option<&str>,
) -> Result<ProcessProfileEffect, ProcessProfileEffectError> {
    let declaration = declaration.ok_or(ProcessProfileEffectError::Undeclared)?;
    let effect = ProcessProfileEffect::parse(declaration)?;
    let reaches_workspace = profile_reaches_workspace(profile);
    match (reaches_workspace, effect) {
        (true, ProcessProfileEffect::CurrentCheckoutWrite) => Ok(effect),
        (true, narrower) => Err(ProcessProfileEffectError::WorkspaceConflict(
            profile.name.clone(),
            narrower.as_str().to_string(),
        )),
        (false, ProcessProfileEffect::CurrentCheckoutWrite) => {
            Err(ProcessProfileEffectError::WorkspaceUnreachable(
                profile.name.clone(),
                effect.as_str().to_string(),
            ))
        }
        (false, _) => Ok(effect),
    }
}

/// The only structural evidence a pinned profile has of workspace
/// reachability: a frame rule that binds a params field to the workspace
/// root, or confines one below it.
fn profile_reaches_workspace(profile: &ProcessProfileSchema) -> bool {
    profile
        .methods
        .iter()
        .flat_map(|rule| rule.params.iter())
        .any(|field| {
            matches!(
                &field.constraint,
                FieldConstraint::WithinWorkspace
                    | FieldConstraint::BoundTo {
                        value: HostValue::WorkspaceRoot,
                    }
            )
        })
}
