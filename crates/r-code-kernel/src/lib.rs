//! # r-code-kernel
//!
//! Durable task lifecycle, acceptance arbitration, plan, delegation and
//! recovery state machines. This crate owns domain decisions only and must
//! not depend on Tauri, SQLite, the Gateway, provider implementations or
//! harness binaries — persistence and runtime live behind ports ([`ports`],
//! T04).

pub mod budget;
pub mod cancellation;
pub mod children;
pub mod completion;
pub mod plans;
pub mod ports;
pub mod questions;
pub mod recovery;
pub mod task;
pub mod tasks;
pub mod testing;
pub mod verification;

pub use completion::*;
pub use plans::*;
pub use ports::*;
pub use questions::*;
pub use task::*;
pub use verification::*;
