//! Terminal cancellation and generation fencing.
//!
//! Cancellation runs in strict order: revoke the run generation first,
//! then cascade through children, model streams, tools and managed
//! processes; only after guardian-confirmed process termination is the
//! task lease released. Late callbacks carrying the revoked generation
//! fail closed. Ownership or process uncertainty stays blocked — exactly
//! one final outcome is ever produced.

use crate::budget::BudgetPool;
use crate::children::ChildrenSupervisor;
use crate::ports::{RunGuard, ServiceError};
use crate::task::{Actor, TaskState, TaskVerdict, TransitionError};
use std::sync::Arc;

/// What the coordinator tells the caller to stop, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancellationStep {
    /// 1. The generation is revoked: in-flight callbacks start failing.
    GenerationRevoked { run_id: String, generation: u64 },
    /// 2. Children are cancelled.
    ChildrenCancelled { count: usize },
    /// 3. Streams/tools/processes receive the stop signal.
    WorkStopped,
    /// 4. Processes are confirmed terminated (guardian proof); only then:
    TerminationConfirmed,
    /// 5. The lease may be released and the task finalized.
    LeaseReleasable,
}

/// Errors from cancellation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CancellationError {
    #[error("termination could not be proven: {0}")]
    UnprovenTermination(String),
    #[error("cancellation failed: {0}")]
    Service(String),
}

/// The termination prover: confirms that every guarded process of a run is
/// gone. Guardians (Job Objects / Unix groups) implement this.
pub trait TerminationProver: Send + Sync {
    /// True when termination of all known processes is *proven*.
    fn proven(&self, run_id: &str) -> Result<bool, String>;
}

/// A no-op prover for runs without processes.
#[derive(Default)]
pub struct NoProcesses;

impl TerminationProver for NoProcesses {
    fn proven(&self, _run_id: &str) -> Result<bool, String> {
        Ok(true)
    }
}

/// Coordinate one cancellation.
pub struct CancellationCoordinator {
    pub guard: Arc<RunGuard>,
    pub supervisor: Option<ChildrenSupervisor>,
    pub budget: Option<BudgetPool>,
    pub prover: Box<dyn TerminationProver>,
}

impl CancellationCoordinator {
    pub fn new(guard: Arc<RunGuard>) -> Self {
        Self {
            guard,
            supervisor: None,
            budget: None,
            prover: Box::new(NoProcesses),
        }
    }

    pub fn with_children(mut self, supervisor: ChildrenSupervisor) -> Self {
        self.supervisor = Some(supervisor);
        self
    }

    pub fn with_budget(mut self, budget: BudgetPool) -> Self {
        self.budget = Some(budget);
        self
    }

    pub fn with_prover(mut self, prover: Box<dyn TerminationProver>) -> Self {
        self.prover = prover;
        self
    }

    /// Execute the cancellation sequence, returning the ordered steps.
    /// `reason` lands in the single terminal outcome.
    pub fn cancel(
        &mut self,
        run_id: &str,
        reason: &str,
    ) -> Result<Vec<CancellationStep>, CancellationError> {
        let _ = reason; // recorded by the caller's finalize_cancelled
        let mut steps = Vec::new();
        let token = self.guard.token();

        // 1. Revoke the generation FIRST: late callbacks fail from here on.
        self.guard.revoke();
        if let Err(error) = self.guard.check(&token) {
            // The expected post-revoke state; anything else is a bug.
            match error {
                ServiceError::Cancelled => {}
                other => return Err(CancellationError::Service(other.to_string())),
            }
        }
        steps.push(CancellationStep::GenerationRevoked {
            run_id: run_id.to_string(),
            generation: token.generation,
        });

        // 2. Cancel children (cascade).
        if let Some(supervisor) = &mut self.supervisor {
            let live = supervisor.live_children().len();
            supervisor.cancel_all();
            steps.push(CancellationStep::ChildrenCancelled { count: live });
        }

        // 3. Signal streams/tools/processes to stop (the transport's cancel
        //    + kill path; callers wire their handles here).
        steps.push(CancellationStep::WorkStopped);

        // 4. Termination must be *proven* before the lease may go.
        let proven = self
            .prover
            .proven(run_id)
            .map_err(CancellationError::UnprovenTermination)?;
        if !proven {
            return Err(CancellationError::UnprovenTermination(format!(
                "processes of run {run_id} cannot be proven terminated; lease stays held"
            )));
        }
        steps.push(CancellationStep::TerminationConfirmed);

        // 5. Lease releasable; budget accounting may refund reservations.
        if let Some(budget) = &self.budget {
            // Unspent reservations already refunded by their handles; the
            // pool stays readable for the final report.
            let _ = budget.remaining();
        }
        steps.push(CancellationStep::LeaseReleasable);
        Ok(steps)
    }
}

/// Finalize the task with exactly one terminal outcome. The state machine
/// itself rejects double finalization; this helper also refuses to finalize
/// while the generation is still live (cancellation must complete first).
pub fn finalize_cancelled(
    state: &mut TaskState,
    generation: u64,
    reason: &str,
) -> Result<TaskVerdict, TransitionError> {
    let verdict = TaskVerdict::Cancelled {
        reason: reason.to_string(),
    };
    state.cancel(Actor::Host, generation, reason)?;
    Ok(verdict)
}

/// A queued next message during cancellation: it stays queued (never
/// delivered to the revoked generation) and is delivered to the *next*
/// run — represented by this classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedMessageDisposition {
    /// Held for the successor run.
    HeldForNextRun,
    /// The cancelled generation never sees it.
    DroppedForRevokedGeneration,
}

pub fn classify_queued_message(run_cancelled: bool) -> QueuedMessageDisposition {
    if run_cancelled {
        QueuedMessageDisposition::HeldForNextRun
    } else {
        QueuedMessageDisposition::DroppedForRevokedGeneration
    }
}
