//! Neutral interpreter for declarative process-profile constraints.
//!
//! The host buffers complete NDJSON frames and validates each one against
//! the package-pinned [`ProcessProfileSchema`] *before* any byte is
//! forwarded to the child process. Frame splitting cannot bypass the checks:
//! only whole, well-formed JSON-RPC frames are ever validated or forwarded.
//! Harness-specific rules (Codex App Server methods, field mappings) live in
//! plugin-package data — this module is generic.

use r_code_harness_protocol::process_profile::{
    ConstraintViolation, HostBindings, ProcessProfileSchema,
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
