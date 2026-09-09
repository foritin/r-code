//! Child-run supervision (host.children).
//!
//! Children may choose a different harness and inherit permission ceilings
//! bounded by the parent. The parent cannot finalize while uncollected
//! children are live. Reports keep verified / inferred / unverifiable
//! results distinct. Cancelling the parent cascades to children.

use crate::budget::BudgetPool;
use r_code_harness_protocol::services::{ChildReport, ChildrenSpawnRequest, PermissionCeiling};
use std::collections::HashMap;

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

/// The supervisor for one parent task.
#[derive(Default)]
pub struct ChildrenSupervisor {
    children: HashMap<String, ChildRun>,
    order: Vec<String>,
}

impl ChildrenSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a child with the parent's permission ceiling bounding it.
    pub fn spawn(
        &mut self,
        parent_ceiling: PermissionCeiling,
        request: &ChildrenSpawnRequest,
    ) -> Result<String, ChildrenError> {
        if rank(request.permissions) > rank(parent_ceiling) {
            return Err(ChildrenError::PermissionExceeded {
                requested: request.permissions,
                parent: parent_ceiling,
            });
        }
        let sequence = self.children.len() + 1;
        let child_task_id = format!("child-{sequence}");
        self.children.insert(
            child_task_id.clone(),
            ChildRun {
                child_task_id: child_task_id.clone(),
                objective: request.objective.clone(),
                harness: request.harness.clone(),
                ceiling: request.permissions,
                state: ChildState::Running,
            },
        );
        self.order.push(child_task_id.clone());
        Ok(child_task_id)
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
        }
        Ok(())
    }

    /// Parent cancellation cascades: every live child is cancelled.
    pub fn cancel_all(&mut self) {
        for child in self.children.values_mut() {
            if matches!(child.state, ChildState::Running) {
                child.state = ChildState::Cancelled;
            }
        }
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
