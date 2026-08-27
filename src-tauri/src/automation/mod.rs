//! Automation host boundary.
//!
//! This module intentionally contains only feature gating and pure scheduling semantics. Durable
//! repositories, scheduler threads, task dispatch, and UI commands are introduced by later gates.

mod clock;
mod semantics;

use r_code_core::UserFacingError;

use crate::feature_flags::{ProductFeature, ProductFeatureFlags};

pub use clock::{Clock, SystemClock};
pub use semantics::{
    apply_wall_clock_policy, decide_overlap, plan_catch_up, profile_dispatch_decision,
    transition_definition, transition_run, validate_run_transition, validate_schedule,
    AutomationSemanticError, CatchUpPlan, DefinitionAction, DispatchDecision,
    ExecutionProfileAvailability, OverlapDecision, WallClockResolution,
    BASE_REF_UNAVAILABLE_ERROR_CODE, MODEL_UNAVAILABLE_ERROR_CODE, OVERLAP_SKIPPED_ERROR_CODE,
    PROVIDER_UNAVAILABLE_ERROR_CODE,
};

/// Every future Automation command must cross this server-side gate before doing any work.
pub fn require_feature_enabled(flags: ProductFeatureFlags) -> Result<(), UserFacingError> {
    flags.require(ProductFeature::Automation)
}
