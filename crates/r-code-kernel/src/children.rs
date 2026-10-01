//! Child-run supervision (host.children).
//!
//! Children may choose a different harness and inherit permission ceilings
//! bounded by the parent. The parent cannot finalize while uncollected
//! children are live. Reports keep verified / inferred / unverifiable
//! results distinct. Cancelling the parent cascades to children.
//!
//! M1a-09 (FR-8 step one): the supervisor is real semantics, not
//! bookkeeping — spawn ids come from a monotonic counter (close-safe), a
//! concurrency gate rejects over-limit spawns (the daemon executor queues
//! them), a nesting gate bounds delegation depth, `close` reclaims the
//! slot, and completion transitions publish through a condvar signal so
//! waits block without polling.

use crate::budget::BudgetPool;
use r_code_harness_protocol::services::{ChildReport, ChildrenSpawnRequest, PermissionCeiling};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

/// Default maximum live children (PRD D7: concurrency 6).
pub const DEFAULT_MAX_LIVE: usize = 6;
/// Default delegation nesting limit (PRD D7: depth 1 — children cannot
/// spawn grandchildren).
pub const DEFAULT_MAX_DEPTH: u32 = 1;

/// Errors from child supervision.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChildrenError {
    #[error("child permission ceiling {requested:?} exceeds the parent ceiling {parent:?}")]
    PermissionExceeded {
        requested: PermissionCeiling,
        parent: PermissionCeiling,
    },
    #[error("child {0} not found")]
    Unknown(String),
    #[error("cannot finalize: uncollected children {0:?}")]
    Uncollected(Vec<String>),
    #[error("child concurrency limit reached: {live} live of {limit}")]
    ConcurrencyLimit { live: usize, limit: usize },
    #[error("child nesting limit reached: depth {depth} of {limit}")]
    NestingLimit { depth: u32, limit: u32 },
}

/// The lifecycle of one child.
#[derive(Debug, Clone, PartialEq)]
pub enum ChildState {
    Running,
    /// Completed; report ready for collection.
    Completed(ChildReport),
    Cancelled,
}

/// One supervised child.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildRun {
    pub child_task_id: String,
    pub objective: String,
    pub harness: Option<String>,
    pub ceiling: PermissionCeiling,
    pub state: ChildState,
}

/// Instantaneous wait states; the blocking loop lives in the runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum ChildWait {
    Running,
    Completed(ChildReport),
    Cancelled,
}

/// The supervisor for one parent task.
pub struct ChildrenSupervisor {
    children: HashMap<String, ChildRun>,
    order: Vec<String>,
    /// Monotonic spawn sequence — ids never repeat, even after close.
    next_seq: u64,
    max_live: usize,
    depth: u32,
    max_depth: u32,
    /// Completion signal: (version, condvar). Every state transition bumps
    /// the version and notifies all waiters; waiters compare versions to
    /// distinguish spurious wakeups. Shared out via [`Self::signal`] so the
    /// supervisor mutex itself never blocks inside a wait.
    signal_state: Arc<Mutex<u64>>,
    signal: Arc<Condvar>,
}

impl Default for ChildrenSupervisor {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_LIVE, 0, DEFAULT_MAX_DEPTH)
    }
}

impl ChildrenSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// A supervisor with explicit limits. `depth` is this supervisor's own
    /// delegation depth (0 for a top-level run); children built with
    /// `child_supervisor()` get depth + 1.
    pub fn with_limits(max_live: usize, depth: u32, max_depth: u32) -> Self {
        Self {
            children: HashMap::new(),
            order: Vec::new(),
            next_seq: 0,
            max_live: max_live.max(1),
            depth,
            max_depth,
            signal_state: Arc::new(Mutex::new(0)),
            signal: Arc::new(Condvar::new()),
        }
    }

    /// The supervisor a spawned child would carry (one level deeper, same
    /// limits) — used by the executor and by nesting checks.
    pub fn child_limits(&self) -> (usize, u32, u32) {
        (self.max_live, self.depth + 1, self.max_depth)
    }

    /// The completion signal handle: waiters wait on this condvar and treat
    /// a changed version as "state moved, re-check".
    pub fn signal(&self) -> (Arc<Mutex<u64>>, Arc<Condvar>) {
        (Arc::clone(&self.signal_state), Arc::clone(&self.signal))
    }

    fn notify_transition(&self) {
        if let Ok(mut version) = self.signal_state.lock() {
            *version = version.wrapping_add(1);
        }
        self.signal.notify_all();
    }

    /// Spawn a child with the parent's permission ceiling bounding it.
    /// Over-limit spawns are rejected with [`ChildrenError::ConcurrencyLimit`]
    /// — the daemon executor queues them and retries when a slot frees.
    pub fn spawn(
        &mut self,
        parent_ceiling: PermissionCeiling,
        request: &ChildrenSpawnRequest,
    ) -> Result<String, ChildrenError> {
        let child_task_id = self.reserve_id();
        self.activate(child_task_id.clone(), parent_ceiling, request)?;
        Ok(child_task_id)
    }

    /// Reserve the next monotonic id without activating it — the daemon
    /// executor reserves at tool-call time and queues the start when the
    /// concurrency gate is full (FR-8.1: queued spawns keep their id).
    pub fn reserve_id(&mut self) -> String {
        self.next_seq += 1;
        format!("child-{}", self.next_seq)
    }

    /// How many not-yet-closed entries occupy concurrency slots.
    pub fn open_count(&self) -> usize {
        self.children.len()
    }

    /// Activate a previously reserved id as a live child. Gate and ceiling
    /// checks run here so queued starts cannot bypass them.
    pub fn activate(
        &mut self,
        child_task_id: String,
        parent_ceiling: PermissionCeiling,
        request: &ChildrenSpawnRequest,
    ) -> Result<(), ChildrenError> {
        if rank(request.permissions) > rank(parent_ceiling) {
            return Err(ChildrenError::PermissionExceeded {
                requested: request.permissions,
                parent: parent_ceiling,
            });
        }
        if self.depth >= self.max_depth {
            return Err(ChildrenError::NestingLimit {
                depth: self.depth,
                limit: self.max_depth,
            });
        }
        // The gate counts every not-yet-closed entry: completed children
        // keep occupying their slot until an explicit close reclaims it
        // (FR-8.1, the un-closed-slot lesson).
        if self.children.len() >= self.max_live {
            return Err(ChildrenError::ConcurrencyLimit {
                live: self.children.len(),
                limit: self.max_live,
            });
        }
        if self.children.contains_key(&child_task_id) {
            return Err(ChildrenError::Unknown(format!(
                "{child_task_id} already activated"
            )));
        }
        let objective = request.objective.clone();
        self.children.insert(
            child_task_id.clone(),
            ChildRun {
                child_task_id: child_task_id.clone(),
                objective,
                harness: request.harness.clone(),
                ceiling: request.permissions,
                state: ChildState::Running,
            },
        );
        self.order.push(child_task_id);
        Ok(())
    }

    /// Complete a child with its report (host-arbitrated facts only make it
    /// into `verified`).
    pub fn complete(
        &mut self,
        child_task_id: &str,
        report: ChildReport,
    ) -> Result<(), ChildrenError> {
        let child = self
            .children
            .get_mut(child_task_id)
            .ok_or_else(|| ChildrenError::Unknown(child_task_id.to_string()))?;
        child.state = ChildState::Completed(report);
        self.notify_transition();
        Ok(())
    }

    /// Cancel one child.
    pub fn cancel_child(&mut self, child_task_id: &str) -> Result<(), ChildrenError> {
        let child = self
            .children
            .get_mut(child_task_id)
            .ok_or_else(|| ChildrenError::Unknown(child_task_id.to_string()))?;
        if matches!(child.state, ChildState::Running) {
            child.state = ChildState::Cancelled;
            self.notify_transition();
        }
        Ok(())
    }

    /// Parent cancellation cascades: every live child is cancelled.
    pub fn cancel_all(&mut self) {
        let mut changed = false;
        for child in self.children.values_mut() {
            if matches!(child.state, ChildState::Running) {
                child.state = ChildState::Cancelled;
                changed = true;
            }
        }
        if changed {
            self.notify_transition();
        }
    }

    /// Explicitly reclaim a child's slot (FR-8.1: "完成后显式 close 回收
    /// 并发额度"). The entry leaves the registry; the id never returns.
    pub fn close(&mut self, child_task_id: &str) -> Result<(), ChildrenError> {
        if self.children.remove(child_task_id).is_none() {
            return Err(ChildrenError::Unknown(child_task_id.to_string()));
        }
        self.order.retain(|id| id != child_task_id);
        self.notify_transition();
        Ok(())
    }

    /// Whether the parent may finalize: no running children remain.
    pub fn can_finalize(&self) -> Result<(), ChildrenError> {
        let uncollected: Vec<String> = self
            .children
            .values()
            .filter(|child| matches!(child.state, ChildState::Running))
            .map(|child| child.child_task_id.clone())
            .collect();
        if !uncollected.is_empty() {
            return Err(ChildrenError::Uncollected(uncollected));
        }
        Ok(())
    }

    /// Collect all completed reports (verified / inferred / unverifiable
    /// stay distinct).
    pub fn collect_reports(&self) -> Vec<ChildReport> {
        self.order
            .iter()
            .filter_map(|id| self.children.get(id))
            .filter_map(|child| match &child.state {
                ChildState::Completed(report) => Some(report.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn child(&self, child_task_id: &str) -> Option<&ChildRun> {
        self.children.get(child_task_id)
    }

    pub fn live_children(&self) -> Vec<String> {
        self.children
            .values()
            .filter(|child| matches!(child.state, ChildState::Running))
            .map(|child| child.child_task_id.clone())
            .collect()
    }

    /// Instantaneous wait state for one child; the blocking wait loop
    /// (`wait_child`) combines this with the condvar signal.
    pub fn wait_state(&self, child_task_id: &str) -> Result<ChildWait, ChildrenError> {
        let child = self
            .children
            .get(child_task_id)
            .ok_or_else(|| ChildrenError::Unknown(child_task_id.to_string()))?;
        Ok(match &child.state {
            ChildState::Running => ChildWait::Running,
            ChildState::Completed(report) => ChildWait::Completed(report.clone()),
            ChildState::Cancelled => ChildWait::Cancelled,
        })
    }
}

/// Whether a requested ceiling fits under the parent's (rank order).
pub fn ceiling_allows(parent: PermissionCeiling, requested: PermissionCeiling) -> bool {
    rank(requested) <= rank(parent)
}

/// Block until the child leaves Running or the timeout elapses. Waiters
/// must NOT hold the supervisor mutex; this re-checks state on every
/// version bump (no polling — the condvar does the sleeping).
pub fn wait_child(
    supervisor: &Mutex<ChildrenSupervisor>,
    child_task_id: &str,
    timeout: std::time::Duration,
) -> Result<ChildWait, ChildrenError> {
    let (signal_state, signal) = {
        let guard = supervisor
            .lock()
            .map_err(|_| ChildrenError::Unknown("supervisor poisoned".into()))?;
        guard.signal()
    };
    let deadline = std::time::Instant::now() + timeout;
    let mut version = signal_state
        .lock()
        .map_err(|_| ChildrenError::Unknown("signal poisoned".into()))?;
    loop {
        let state = {
            let guard = supervisor
                .lock()
                .map_err(|_| ChildrenError::Unknown("supervisor poisoned".into()))?;
            guard.wait_state(child_task_id)?
        };
        match state {
            ChildWait::Running => {}
            terminal => return Ok(terminal),
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(ChildWait::Running);
        }
        let (guard, _result) = signal
            .wait_timeout(version, remaining)
            .map_err(|_| ChildrenError::Unknown("signal poisoned".into()))?;
        version = guard;
    }
}

fn rank(ceiling: PermissionCeiling) -> u8 {
    match ceiling {
        PermissionCeiling::ReadOnly => 0,
        PermissionCeiling::ApprovalRequired => 1,
        PermissionCeiling::Full => 2,
    }
}

/// A budget-aware supervisor pairing the tree with its shared pool.
pub struct SupervisedTree {
    pub supervisor: ChildrenSupervisor,
    pub budget: BudgetPool,
}

impl SupervisedTree {
    pub fn new(root_budget: u64) -> Self {
        Self {
            supervisor: ChildrenSupervisor::new(),
            budget: BudgetPool::new(root_budget),
        }
    }
}
