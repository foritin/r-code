//! Harness v2 storage. Fresh schema, atomic aggregate+event transactions,
//! operation receipts, checkpoints, run leases and blob registry. The v2
//! opening path is fully isolated from legacy `Database`/`MigrationManager`.

pub mod application_commands;
pub mod journal;
pub mod operations;
pub mod plans;
pub mod plugins;
pub mod questions;
pub mod schema;
pub mod tasks;
pub mod verification;

pub use application_commands::CommandReceiptState;
pub use journal::{LeaseAcquisition, V2Store, V2StoreError};
pub use plugins::{PluginCatalogRecord, PluginPinRecord};
pub use tasks::{rebuild_queue, TaskBranch};
