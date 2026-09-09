//! # r-code-harness-protocol
//!
//! Versioned public wire contracts between the R-Code host and harness
//! plugin processes. This crate owns DTOs only: it must stay free of
//! product, platform, storage and provider implementation dependencies
//! (serde/serde_json/semver/thiserror are the entire surface).
//!
//! Layout:
//! - [`manifest`]: `harness.json` identity and capability negotiation.
//! - [`rpc`]: JSON-RPC 2.0 envelopes and framing limits.
//! - [`services`]: host-service request/reply shapes.
//! - [`events`]: plugin progress and provenance-tagged observations.

pub mod application;
pub mod events;
pub mod manifest;
pub mod operations;
pub mod process_profile;
pub mod rpc;
pub mod services;

pub use application::*;
pub use events::*;
pub use manifest::*;
pub use operations::*;
pub use rpc::*;
pub use services::*;
