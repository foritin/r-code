//! Work card template -- 可审计工程任务模板 [doc-19 §4]。
//!
//! No changes enter implementation without a work card. Each card captures:
//! goal, boundary, contract, failure states, required tests, evidence, rollback.
//!
//! [doc-19 §4]

use serde::{Deserialize, Serialize};

/// Work card template for auditable engineering tasks.
/// [doc-19 §4] No changes enter implementation without a work card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkCard {
    /// Card ID
    pub id: String,
    /// Phase (P0, P1, R1-R9, Final)
    pub phase: String,
    /// Goal - what the user can observe
    pub goal: String,
    /// Boundary - which workspaces, processes, secrets, files, networks are involved
    pub boundary: WorkCardBoundary,
    /// Contract - new/modified DTOs, events, migrations, RPC methods
    pub contract: WorkCardContract,
    /// Failure states - how cancel, restart, bad input, permission deny, external changes manifest
    pub failure_states: Vec<FailureState>,
    /// Tests - which unit/integration/desktop E2E/security/performance tests are required
    pub tests: Vec<RequiredTest>,
    /// Evidence - screenshots, traces, log summaries, migration fixtures, release command output
    pub evidence: Vec<EvidenceItem>,
    /// Rollback - how to undo code, data migrations, user-visible state, external side effects
    pub rollback: RollbackPlan,
}

/// Boundary -- the scope of resources a work card touches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkCardBoundary {
    /// Workspaces involved
    pub workspaces: Vec<String>,
    /// Processes involved
    pub processes: Vec<String>,
    /// Secrets involved (names only, never values)
    pub secrets: Vec<String>,
    /// Files involved
    pub files: Vec<String>,
    /// Networks involved
    pub networks: Vec<String>,
}

/// Contract -- public surface area changes introduced by this card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkCardContract {
    /// New DTOs introduced
    pub new_dtos: Vec<String>,
    /// Modified DTOs
    pub modified_dtos: Vec<String>,
    /// New events
    pub new_events: Vec<String>,
    /// New migrations
    pub new_migrations: Vec<String>,
    /// New RPC methods
    pub new_rpc_methods: Vec<String>,
}

/// A failure state scenario and its expected behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureState {
    /// Scenario description
    pub scenario: String,
    /// Expected behavior when this failure occurs
    pub expected_behavior: String,
}

/// A required test for the work card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiredTest {
    /// Test type
    pub test_type: TestType,
    /// Test description
    pub description: String,
    /// Current status
    pub status: TestStatus,
}

/// Test type classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestType {
    /// Unit test
    Unit,
    /// Integration test
    Integration,
    /// Desktop end-to-end test
    DesktopE2E,
    /// Security test
    Security,
    /// Performance test
    Performance,
}

/// Test status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    /// Required but not yet implemented
    Required,
    /// Implemented but not yet passing
    Implemented,
    /// Passing
    Passing,
}

/// An evidence item proving the work card's deliverable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceItem {
    /// Evidence kind (screenshot, trace, log summary, etc.)
    pub kind: String,
    /// Description of the evidence
    pub description: String,
    /// Location (path, command, URL)
    pub location: String,
}

/// Rollback plan -- how to undo the work card's changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackPlan {
    /// How to roll back code
    pub code_rollback: String,
    /// How to roll back data migrations
    pub data_rollback: String,
    /// How to roll back user-visible state
    pub user_state_rollback: String,
    /// How to roll back external side effects
    pub external_side_effects: String,
}

impl WorkCard {
    /// Create a new work card with the given goal and phase.
    pub fn new(id: impl Into<String>, phase: impl Into<String>, goal: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            phase: phase.into(),
            goal: goal.into(),
            boundary: WorkCardBoundary {
                workspaces: vec![],
                processes: vec![],
                secrets: vec![],
                files: vec![],
                networks: vec![],
            },
            contract: WorkCardContract {
                new_dtos: vec![],
                modified_dtos: vec![],
                new_events: vec![],
                new_migrations: vec![],
                new_rpc_methods: vec![],
            },
            failure_states: vec![],
            tests: vec![],
            evidence: vec![],
            rollback: RollbackPlan {
                code_rollback: String::new(),
                data_rollback: String::new(),
                user_state_rollback: String::new(),
                external_side_effects: String::new(),
            },
        }
    }

    /// Validate the work card is complete.
    pub fn validate(&self) -> Result<(), String> {
        if self.goal.is_empty() {
            return Err("Goal is required".to_string());
        }
        if self.failure_states.is_empty() {
            return Err("At least one failure state is required".to_string());
        }
        if self.tests.is_empty() {
            return Err("At least one required test is required".to_string());
        }
        if self.rollback.code_rollback.is_empty() {
            return Err("Code rollback plan is required".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_card_has_empty_collections() {
        let card = WorkCard::new("WC-001", "R1", "Test goal");
        assert_eq!(card.id, "WC-001");
        assert_eq!(card.phase, "R1");
        assert_eq!(card.goal, "Test goal");
        assert!(card.boundary.workspaces.is_empty());
        assert!(card.contract.new_dtos.is_empty());
        assert!(card.failure_states.is_empty());
        assert!(card.tests.is_empty());
        assert!(card.evidence.is_empty());
        assert!(card.rollback.code_rollback.is_empty());
    }

    #[test]
    fn validate_rejects_empty_goal() {
        let card = WorkCard::new("WC-001", "R1", "");
        assert!(card.validate().is_err());
    }

    #[test]
    fn validate_rejects_missing_failure_states() {
        let mut card = WorkCard::new("WC-001", "R1", "Goal");
        card.tests.push(RequiredTest {
            test_type: TestType::Unit,
            description: "test".to_string(),
            status: TestStatus::Passing,
        });
        card.rollback.code_rollback = "git revert".to_string();
        assert!(card.validate().is_err());
    }

    #[test]
    fn validate_accepts_complete_card() {
        let mut card = WorkCard::new("WC-001", "R1", "Goal");
        card.failure_states.push(FailureState {
            scenario: "cancel".to_string(),
            expected_behavior: "no side effects".to_string(),
        });
        card.tests.push(RequiredTest {
            test_type: TestType::Unit,
            description: "test".to_string(),
            status: TestStatus::Passing,
        });
        card.rollback.code_rollback = "git revert".to_string();
        assert!(card.validate().is_ok());
    }
}
