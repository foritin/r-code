//! Built-in Codex harness plugin (library).
//!
//! The Codex App Server protocol and its sequencing live *here*, as plugin
//! data + code — the host only runs the neutral process-profile
//! interpreter. Inner CLI actions stay external observations; provider
//! credentials never reach this process.

pub mod app_server;
pub mod delegation;
pub mod interactions;

pub use app_server::{AppServerClient, AppServerEvent};
pub use delegation::{decide_resume, CodexCheckpoint, ResumeDecision, ThreadResumeRef};
pub use interactions::CodexInteractions;
