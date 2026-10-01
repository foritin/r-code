//! Harness v1 storage. Fresh schema, atomic aggregate+event transactions,
//! operation receipts, checkpoints, run leases and blob registry. The v1
//! opening path is fully isolated from legacy `Database`/`MigrationManager`.

pub mod application_commands;
pub mod attempts;
pub mod context_settings;
pub mod injections;
pub mod journal;
pub mod mutations;
pub mod operations;
pub mod plans;
pub mod plugins;
pub mod process_effects;
pub mod questions;
pub mod reviews;
pub mod runs;
pub mod safety;
pub mod schema;
pub mod tasks;
pub mod verification;

pub use application_commands::CommandReceiptState;
pub use attempts::{
    WorkUnitAttemptError, WorkUnitAttemptPhase, WorkUnitAttemptRecord, WorkUnitAttemptSeed,
};
pub use context_settings::ContextSettingsRecord;
pub use injections::{InjectionKind, InjectionRecord, InjectionRecordView};
pub use journal::{LeaseAcquisition, V1Store, V1StoreError};
pub use mutations::{
    LeaseGrant, LeaseMode, LeaseRequest, MutationError, MutationFile, MutationOperation,
    MutationState,
};
pub use plugins::{PluginCatalogRecord, PluginPinRecord};
pub use process_effects::{
    EffectArtifactRef, ProcessEffectError, ProcessEffectPrepare, ProcessEffectRecord,
    ProcessEffectState,
};
pub use reviews::{
    OverrideCommitError, UnverifiedOverrideError, UnverifiedOverrideRecord, UnverifiedOverrideSeed,
};
pub use safety::{SafetyReportRecord, SafetyReportStatus};
pub use tasks::{rebuild_queue, TaskBranch};
