//! Durable path leases and mutation journal for workspace side effects.

use crate::v1::{journal::now_ms, operations::workspace_has_unproved_process_tree, V1Store};
use r_code_harness_protocol::services::normalize_workspace_relative_path;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeaseMode {
    Read,
    Write,
    RepoExclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRequest {
    pub workspace_key: String,
    pub operation_id: String,
    pub owner_id: String,
    #[serde(default)]
    pub read_paths: Vec<String>,
    #[serde(default)]
    pub write_paths: Vec<String>,
    #[serde(default)]
    pub repo_exclusive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseGrant {
    pub lease_id: String,
    pub request: LeaseRequest,
    pub fencing_epoch: u64,
    pub active: bool,
}

/// Lifecycle state of one lease family (E07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseFamilyState {
    Active,
    Released,
    Quarantined,
}

impl LeaseFamilyState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Released => "released",
            Self::Quarantined => "quarantined",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "released" => Some(Self::Released),
            "quarantined" => Some(Self::Quarantined),
            _ => None,
        }
    }
}

/// One durable lease family: the attempt it belongs to, its terminal state,
/// and its member grants (E07). The family carries no epoch of its own —
/// fencing is the workspace lease epoch the members already hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseFamilyGrant {
    pub attempt_id: String,
    pub workspace_key: String,
    pub owner_id: String,
    pub state: LeaseFamilyState,
    pub created_at_ms: i64,
    pub settled_at_ms: Option<i64>,
    pub leases: Vec<LeaseGrant>,
}

/// What restart reconciliation resolved one family to (E07.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseFamilyReconciliation {
    pub attempt_id: String,
    pub outcome: LeaseFamilyOutcome,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseFamilyOutcome {
    /// The family's attempt had settled; the family released its members.
    Released,
    /// The family's attempt was incomplete; the family quarantined with its
    /// members fenced.
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MutationState {
    Prepared,
    Applied,
    Receipted,
    Conflict,
}

impl MutationState {
    fn parse(value: &str) -> Result<Self, MutationError> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "applied" => Ok(Self::Applied),
            "receipted" => Ok(Self::Receipted),
            "conflict" => Ok(Self::Conflict),
            _ => Err(MutationError::Serialization(
                "invalid mutation state".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationFile {
    pub logical_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_cas_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_cas_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationOperation {
    pub operation_id: String,
    pub workspace_key: String,
    pub lease_id: String,
    pub owner_id: String,
    pub fencing_epoch: u64,
    pub input_hash: String,
    pub state: MutationState,
    pub files: Vec<MutationFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MutationError {
    #[error("required mutation field {0} is empty")]
    EmptyField(&'static str),
    #[error("lease request contains no paths")]
    EmptyLease,
    #[error("invalid logical path: {0}")]
    InvalidPath(String),
    #[error("lease request contains contradictory read/write scope")]
    ConflictingScope,
    #[error("lease conflicts with active lease {holder}")]
    LeaseConflict { holder: String },
    #[error("lease was not found")]
    LeaseNotFound,
    #[error("lease is no longer active")]
    LeaseInactive,
    #[error("lease owner or fencing epoch is stale")]
    StaleLease,
    #[error("attempt {0} is already settled; a stale attempt cannot release or extend its family")]
    StaleAttempt(String),
    #[error("lease family members must share one workspace and be owned by their attempt")]
    FamilyScopeMismatch,
    #[error("lease family {0} was not found")]
    FamilyNotFound(String),
    #[error("workspace {workspace_key} is quarantined by an unproved writer")]
    WorkspaceQuarantined { workspace_key: String },
    #[error("operation was not found")]
    OperationNotFound,
    #[error("operation id was reused with different input")]
    OperationConflict,
    #[error("mutation file is outside the granted write scope")]
    FileNotCovered,
    #[error("mutation file content identity is invalid")]
    InvalidFileIdentity,
    #[error("expected mutation state {expected:?}, got {actual:?}")]
    InvalidTransition {
        expected: MutationState,
        actual: MutationState,
    },
    #[error("sqlite failure: {0}")]
    Sqlite(String),
    #[error("serialization failure: {0}")]
    Serialization(String),
}

impl From<rusqlite::Error> for MutationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

impl V1Store {
    pub fn acquire_lease(&self, request: LeaseRequest) -> Result<LeaseGrant, MutationError> {
        let request = normalize_request(request)?;
        let request_json = serde_json::to_string(&request)
            .map_err(|error| MutationError::Serialization(error.to_string()))?;
        let request_hash = r_code_harness_protocol::canonical_input_hash(
            &serde_json::to_value(&request)
                .map_err(|error| MutationError::Serialization(error.to_string()))?,
        );
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if request.repo_exclusive || !request.write_paths.is_empty() {
            let legacy_barrier: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM writer_barriers WHERE workspace_key = ?1
                )",
                params![request.workspace_key],
                |row| row.get(0),
            )?;
            let unproved_tree =
                workspace_has_unproved_process_tree(&transaction, &request.workspace_key)
                    .map_err(|error| MutationError::Sqlite(error.to_string()))?;
            if legacy_barrier || unproved_tree {
                return Err(MutationError::WorkspaceQuarantined {
                    workspace_key: request.workspace_key.clone(),
                });
            }
        }
        if let Some(grant) =
            grant_for_operation(&transaction, &request.workspace_key, &request.operation_id)?
        {
            if grant.request == request {
                transaction.commit()?;
                return Ok(grant);
            }
            return Err(MutationError::OperationConflict);
        }
        for active in active_grants(&transaction, &request.workspace_key)? {
            if scopes_conflict(&request, &active.request) {
                return Err(MutationError::LeaseConflict {
                    holder: active.lease_id,
                });
            }
        }
        let epoch = next_epoch(&transaction, &request.workspace_key)?;
        let lease_id = format!("lease:{}", uuid::Uuid::new_v4());
        transaction.execute(
            "INSERT INTO path_leases(lease_id, workspace_key, operation_id, owner_id,
             fencing_epoch, request_hash, request_json, active, acquired_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
            params![
                lease_id,
                request.workspace_key,
                request.operation_id,
                request.owner_id,
                epoch,
                request_hash,
                request_json,
                now_ms()
            ],
        )?;
        transaction.commit()?;
        Ok(LeaseGrant {
            lease_id,
            request,
            fencing_epoch: epoch,
            active: true,
        })
    }

    pub fn release_lease(
        &self,
        lease_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
    ) -> Result<bool, MutationError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let grant = grant_by_id(&transaction, lease_id)?.ok_or(MutationError::LeaseNotFound)?;
        validate_grant_owner(&grant, owner_id, fencing_epoch)?;
        if !grant.active {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "UPDATE path_leases SET active = 0, released_at_ms = ?1 WHERE lease_id = ?2",
            params![now_ms(), lease_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn active_leases(&self, workspace_key: &str) -> Result<Vec<LeaseGrant>, MutationError> {
        active_grants(&self.connection(), workspace_key)
    }

    pub fn prepare_operation(
        &self,
        operation: &MutationOperation,
    ) -> Result<MutationOperation, MutationError> {
        let operation = normalize_prepared_operation(operation)?;
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let grant = require_active_grant(
            &transaction,
            &operation.lease_id,
            &operation.owner_id,
            operation.fencing_epoch,
        )?;
        validate_operation_scope(&operation, &grant)?;
        if let Some(existing) = load_operation_from(&transaction, &operation.operation_id)? {
            if same_prepared_input(&existing, &operation) {
                transaction.commit()?;
                return Ok(existing);
            }
            return Err(MutationError::OperationConflict);
        }
        transaction.execute(
            "INSERT INTO mutation_operations(operation_id, workspace_key, lease_id, owner_id,
             fencing_epoch, input_hash, state, prepared_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'prepared', ?7)",
            params![
                operation.operation_id,
                operation.workspace_key,
                operation.lease_id,
                operation.owner_id,
                operation.fencing_epoch,
                operation.input_hash,
                now_ms()
            ],
        )?;
        for file in &operation.files {
            transaction.execute(
                "INSERT INTO mutation_files(operation_id, logical_path, before_sha256,
                 after_sha256, before_cas_ref, after_cas_ref)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    operation.operation_id,
                    file.logical_path,
                    file.before_sha256,
                    file.after_sha256,
                    file.before_cas_ref,
                    file.after_cas_ref
                ],
            )?;
        }
        transaction.commit()?;
        Ok(operation)
    }

    /// Strict production entry point: intended before/after identities are
    /// durable before any workspace effect may run.
    pub fn prepare_effect_operation(
        &self,
        operation: &MutationOperation,
    ) -> Result<MutationOperation, MutationError> {
        if operation.files.iter().all(valid_identity_pairs) {
            self.prepare_operation(operation)
        } else {
            Err(MutationError::InvalidFileIdentity)
        }
    }

    pub fn mark_applied(
        &self,
        operation_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
        files: Vec<MutationFile>,
    ) -> Result<MutationOperation, MutationError> {
        let files = normalize_applied_files(files)?;
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut operation = load_operation_from(&transaction, operation_id)?
            .ok_or(MutationError::OperationNotFound)?;
        require_active_grant(&transaction, &operation.lease_id, owner_id, fencing_epoch)?;
        validate_operation_owner(&operation, owner_id, fencing_epoch)?;
        if operation.state != MutationState::Prepared {
            if matches!(
                operation.state,
                MutationState::Applied | MutationState::Receipted
            ) && operation.files == files
            {
                transaction.commit()?;
                return Ok(operation);
            }
            return Err(MutationError::InvalidTransition {
                expected: MutationState::Prepared,
                actual: operation.state,
            });
        }
        if file_paths(&operation.files) != file_paths(&files) {
            return Err(MutationError::OperationConflict);
        }
        if operation.files.iter().any(has_any_identity) {
            if operation.files != files {
                return Err(MutationError::OperationConflict);
            }
        } else {
            // Compatibility for pre-M02 callers which prepared only paths.
            for file in &files {
                transaction.execute(
                    "UPDATE mutation_files SET before_sha256 = ?1, after_sha256 = ?2,
                     before_cas_ref = ?3, after_cas_ref = ?4
                     WHERE operation_id = ?5 AND logical_path = ?6",
                    params![
                        file.before_sha256,
                        file.after_sha256,
                        file.before_cas_ref,
                        file.after_cas_ref,
                        operation_id,
                        file.logical_path
                    ],
                )?;
            }
        }
        transaction.execute(
            "UPDATE mutation_operations SET state = 'applied', applied_at_ms = ?1
             WHERE operation_id = ?2 AND state = 'prepared'",
            params![now_ms(), operation_id],
        )?;
        transaction.commit()?;
        operation.state = MutationState::Applied;
        operation.files = files;
        Ok(operation)
    }

    pub fn mark_receipted(
        &self,
        operation_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
    ) -> Result<MutationOperation, MutationError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut operation = load_operation_from(&transaction, operation_id)?
            .ok_or(MutationError::OperationNotFound)?;
        require_active_grant(&transaction, &operation.lease_id, owner_id, fencing_epoch)?;
        validate_operation_owner(&operation, owner_id, fencing_epoch)?;
        if operation.state == MutationState::Receipted {
            transaction.commit()?;
            return Ok(operation);
        }
        if operation.state != MutationState::Applied {
            return Err(MutationError::InvalidTransition {
                expected: MutationState::Applied,
                actual: operation.state,
            });
        }
        transaction.execute(
            "UPDATE mutation_operations SET state = 'receipted', receipted_at_ms = ?1
             WHERE operation_id = ?2 AND state = 'applied'",
            params![now_ms(), operation_id],
        )?;
        transaction.commit()?;
        operation.state = MutationState::Receipted;
        Ok(operation)
    }

    pub fn mark_conflict(
        &self,
        operation_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
    ) -> Result<MutationOperation, MutationError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut operation = load_operation_from(&transaction, operation_id)?
            .ok_or(MutationError::OperationNotFound)?;
        require_active_grant(&transaction, &operation.lease_id, owner_id, fencing_epoch)?;
        validate_operation_owner(&operation, owner_id, fencing_epoch)?;
        if operation.state == MutationState::Conflict {
            transaction.commit()?;
            return Ok(operation);
        }
        if operation.state == MutationState::Receipted {
            return Err(MutationError::InvalidTransition {
                expected: MutationState::Applied,
                actual: operation.state,
            });
        }
        transaction.execute(
            "UPDATE mutation_operations SET state = 'conflict' WHERE operation_id = ?1",
            params![operation_id],
        )?;
        transaction.commit()?;
        operation.state = MutationState::Conflict;
        Ok(operation)
    }

    pub fn load_mutation_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<MutationOperation>, MutationError> {
        load_operation_from(&self.connection(), operation_id)
    }

    /// Full mutation history owned by one exact attempt. SQLite rowid is the
    /// durable insertion sequence; operation id is a deterministic tie-break.
    pub fn mutation_operations_for_owner(
        &self,
        owner_id: &str,
    ) -> Result<Vec<MutationOperation>, MutationError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT operation_id FROM mutation_operations WHERE owner_id = ?1
             ORDER BY prepared_at_ms, rowid, operation_id",
        )?;
        let operation_ids = statement
            .query_map(params![owner_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        operation_ids
            .into_iter()
            .map(|operation_id| {
                load_operation_from(&connection, &operation_id)?
                    .ok_or(MutationError::OperationNotFound)
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// E07 — durable lease families. One family per in-flight attempt, keyed by
// that attempt's identity: acquisition is all-or-nothing over the member
// leases, release settles the attempt record together with exactly its
// members, and restart reconciliation resolves every active family whole —
// released when its attempt settled, quarantined with its members fenced
// when it did not. Fencing reuses the workspace lease epoch the members
// already carry; the family never mints a third epoch currency.
// ---------------------------------------------------------------------------

impl V1Store {
    /// Acquire one attempt's family: every member lease lands in a single
    /// all-or-nothing transaction (E07.1), so a partially-acquirable set
    /// acquires nothing. A byte-identical replay converges on the existing
    /// active family; a terminal family refuses — replay never resurrects,
    /// and a settled attempt cannot extend its family.
    ///
    /// Members are exempt from conflicting with each other (one attempt owns
    /// them all) while each still conflicts normally against every other
    /// active lease: an attempt on a held path is refused by that member
    /// lease, never by family cross-talk.
    pub fn acquire_lease_family(
        &self,
        attempt_id: &str,
        requests: &[LeaseRequest],
    ) -> Result<LeaseFamilyGrant, MutationError> {
        let attempt_id = attempt_id.trim();
        if attempt_id.is_empty() {
            return Err(MutationError::EmptyField("attempt_id"));
        }
        let mut members: Vec<LeaseRequest> = Vec::with_capacity(requests.len());
        for request in requests {
            let request = normalize_request(request.clone())?;
            if request.owner_id != attempt_id {
                return Err(MutationError::FamilyScopeMismatch);
            }
            if let Some(first) = members.first() {
                if request.workspace_key != first.workspace_key {
                    return Err(MutationError::FamilyScopeMismatch);
                }
            }
            members.push(request);
        }
        if members.is_empty() {
            return Err(MutationError::EmptyLease);
        }
        let mut operation_ids = BTreeSet::new();
        for request in &members {
            if !operation_ids.insert(request.operation_id.clone()) {
                return Err(MutationError::OperationConflict);
            }
        }
        members.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
        let workspace_key = members[0].workspace_key.clone();

        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if members
            .iter()
            .any(|request| request.repo_exclusive || !request.write_paths.is_empty())
        {
            let legacy_barrier: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM writer_barriers WHERE workspace_key = ?1)",
                params![workspace_key],
                |row| row.get(0),
            )?;
            let unproved_tree = workspace_has_unproved_process_tree(&transaction, &workspace_key)
                .map_err(|error| MutationError::Sqlite(error.to_string()))?;
            if legacy_barrier || unproved_tree {
                return Err(MutationError::WorkspaceQuarantined { workspace_key });
            }
        }
        if let Some(family) = load_family(&transaction, attempt_id)? {
            if family.state != LeaseFamilyState::Active {
                return Err(MutationError::StaleAttempt(attempt_id.to_string()));
            }
            if family_member_requests(&family) == members {
                transaction.commit()?;
                return Ok(family);
            }
            return Err(MutationError::OperationConflict);
        }
        // A lease already holding a member's operation id is adopted — the
        // legacy upgrade path for attempts that predate families — provided
        // it is the identical request and still active.
        let mut adopted: Vec<LeaseGrant> = Vec::new();
        for request in &members {
            if let Some(grant) =
                grant_for_operation(&transaction, &request.workspace_key, &request.operation_id)?
            {
                if grant.request != *request {
                    return Err(MutationError::OperationConflict);
                }
                if !grant.active {
                    return Err(MutationError::StaleLease);
                }
                adopted.push(grant);
            }
        }
        let adopted_ids: BTreeSet<&str> = adopted
            .iter()
            .map(|grant| grant.lease_id.as_str())
            .collect();
        for active in active_grants(&transaction, &workspace_key)? {
            if adopted_ids.contains(active.lease_id.as_str()) {
                continue;
            }
            if members
                .iter()
                .any(|request| scopes_conflict(request, &active.request))
            {
                return Err(MutationError::LeaseConflict {
                    holder: active.lease_id,
                });
            }
        }
        let now = now_ms();
        transaction.execute(
            "INSERT INTO lease_families(attempt_id, workspace_key, owner_id, state, created_at_ms)
             VALUES (?1, ?2, ?3, 'active', ?4)",
            params![attempt_id, workspace_key, attempt_id, now],
        )?;
        let mut grants = Vec::with_capacity(members.len());
        for request in members {
            if let Some(grant) = adopted
                .iter()
                .find(|grant| grant.request == request)
                .cloned()
            {
                transaction.execute(
                    "INSERT INTO lease_family_members(attempt_id, lease_id) VALUES (?1, ?2)",
                    params![attempt_id, grant.lease_id],
                )?;
                grants.push(grant);
                continue;
            }
            let epoch = next_epoch(&transaction, &request.workspace_key)?;
            let lease_id = format!("lease:{}", uuid::Uuid::new_v4());
            let request_json = serde_json::to_string(&request)
                .map_err(|error| MutationError::Serialization(error.to_string()))?;
            let request_hash = r_code_harness_protocol::canonical_input_hash(
                &serde_json::to_value(&request)
                    .map_err(|error| MutationError::Serialization(error.to_string()))?,
            );
            transaction.execute(
                "INSERT INTO path_leases(lease_id, workspace_key, operation_id, owner_id,
                 fencing_epoch, request_hash, request_json, active, acquired_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
                params![
                    lease_id,
                    request.workspace_key,
                    request.operation_id,
                    request.owner_id,
                    epoch,
                    request_hash,
                    request_json,
                    now
                ],
            )?;
            transaction.execute(
                "INSERT INTO lease_family_members(attempt_id, lease_id) VALUES (?1, ?2)",
                params![attempt_id, lease_id],
            )?;
            grants.push(LeaseGrant {
                lease_id,
                request,
                fencing_epoch: epoch,
                active: true,
            });
        }
        transaction.commit()?;
        Ok(LeaseFamilyGrant {
            attempt_id: attempt_id.to_string(),
            workspace_key,
            owner_id: attempt_id.to_string(),
            state: LeaseFamilyState::Active,
            created_at_ms: now,
            settled_at_ms: None,
            leases: grants,
        })
    }

    /// Release one attempt's family (E07.2): exactly its member leases
    /// deactivate and its attempt record settles in the same durable step.
    /// A family whose attempt already settled refuses — a stale attempt can
    /// neither release nor extend. Releasing a terminal family is a no-op
    /// returning `Ok(false)`: no lease of the attempt remains either way.
    pub fn release_lease_family(
        &self,
        attempt_id: &str,
        owner_id: &str,
        completed: bool,
    ) -> Result<bool, MutationError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let family = load_family(&transaction, attempt_id)?
            .ok_or_else(|| MutationError::FamilyNotFound(attempt_id.to_string()))?;
        if family.state != LeaseFamilyState::Active {
            transaction.commit()?;
            return Ok(false);
        }
        if family.owner_id != owner_id {
            return Err(MutationError::StaleLease);
        }
        settle_family_attempt(&transaction, attempt_id, completed)?;
        let released = deactivate_family_members(&transaction, &family)?;
        transaction.execute(
            "UPDATE lease_families SET state = 'released', settled_at_ms = ?2
             WHERE attempt_id = ?1 AND state = 'active'",
            params![attempt_id, now_ms()],
        )?;
        transaction.commit()?;
        Ok(released)
    }

    /// Release ONE member lease while leaving the family open (E07): the
    /// dispatcher's fatal and cancel paths keep the attempt record in flight
    /// for restart recovery, so they release the member alone. A terminal
    /// family reports already-done (`Ok(true)`) — no lease of the attempt
    /// remains, whatever released it.
    pub fn release_lease_family_member(
        &self,
        attempt_id: &str,
        lease_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
    ) -> Result<bool, MutationError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: Option<String> = transaction
            .query_row(
                "SELECT state FROM lease_families WHERE attempt_id = ?1",
                params![attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        if state.as_deref() != Some(LeaseFamilyState::Active.as_str()) && state.is_some() {
            transaction.commit()?;
            return Ok(true);
        }
        let grant = grant_by_id(&transaction, lease_id)?.ok_or(MutationError::LeaseNotFound)?;
        validate_grant_owner(&grant, owner_id, fencing_epoch)?;
        if !grant.active {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "UPDATE path_leases SET active = 0, released_at_ms = ?1 WHERE lease_id = ?2",
            params![now_ms(), lease_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// Restart reconciliation for every active family (E07.3): a family whose
    /// attempt settled releases; one whose attempt is incomplete — or whose
    /// attempt row never landed — quarantines with its members fenced. Each
    /// family resolves from its own durable evidence alone, so families of
    /// different attempts never fence each other's members.
    pub fn reconcile_lease_families(
        &self,
    ) -> Result<Vec<LeaseFamilyReconciliation>, MutationError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = active_family_ids(&transaction)?;
        let mut reconciled = Vec::with_capacity(ids.len());
        for attempt_id in ids {
            let family = load_family(&transaction, &attempt_id)?
                .ok_or_else(|| MutationError::FamilyNotFound(attempt_id.clone()))?;
            let phase: Option<String> = transaction
                .query_row(
                    "SELECT phase FROM work_unit_attempts WHERE attempt_id = ?1",
                    params![attempt_id],
                    |row| row.get(0),
                )
                .optional()?;
            let settled = matches!(
                phase.as_deref(),
                Some("settled-completed") | Some("settled-failed")
            );
            deactivate_family_members(&transaction, &family)?;
            let (state, outcome) = if settled {
                ("released", LeaseFamilyOutcome::Released)
            } else {
                ("quarantined", LeaseFamilyOutcome::Quarantined)
            };
            let detail = match phase.as_deref() {
                Some(phase) => format!("attempt phase {phase}"),
                None => "attempt row never landed".to_string(),
            };
            transaction.execute(
                "UPDATE lease_families SET state = ?2, settled_at_ms = ?3
                 WHERE attempt_id = ?1 AND state = 'active'",
                params![attempt_id, state, now_ms()],
            )?;
            reconciled.push(LeaseFamilyReconciliation {
                attempt_id,
                outcome,
                detail,
            });
        }
        transaction.commit()?;
        Ok(reconciled)
    }

    /// One family by attempt id.
    pub fn load_lease_family(
        &self,
        attempt_id: &str,
    ) -> Result<Option<LeaseFamilyGrant>, MutationError> {
        load_family(&self.connection(), attempt_id)
    }
}

fn load_family(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<LeaseFamilyGrant>, MutationError> {
    let row: Option<(String, String, String, i64, Option<i64>)> = connection
        .query_row(
            "SELECT workspace_key, owner_id, state, created_at_ms, settled_at_ms
             FROM lease_families WHERE attempt_id = ?1",
            params![attempt_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((workspace_key, owner_id, state, created_at_ms, settled_at_ms)) = row else {
        return Ok(None);
    };
    let state = LeaseFamilyState::from_str(&state).ok_or_else(|| {
        MutationError::Serialization(format!("unknown lease family state: {state}"))
    })?;
    let mut statement =
        connection.prepare("SELECT lease_id FROM lease_family_members WHERE attempt_id = ?1")?;
    let lease_ids = statement
        .query_map(params![attempt_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut leases = Vec::with_capacity(lease_ids.len());
    for lease_id in lease_ids {
        leases.push(grant_by_id(connection, &lease_id)?.ok_or(MutationError::LeaseNotFound)?);
    }
    Ok(Some(LeaseFamilyGrant {
        attempt_id: attempt_id.to_string(),
        workspace_key,
        owner_id,
        state,
        created_at_ms,
        settled_at_ms,
        leases,
    }))
}

fn active_family_ids(connection: &Connection) -> Result<Vec<String>, MutationError> {
    let mut statement = connection.prepare(
        "SELECT attempt_id FROM lease_families WHERE state = 'active'
         ORDER BY created_at_ms, attempt_id",
    )?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

/// The family's member requests in the replay-comparison order.
fn family_member_requests(family: &LeaseFamilyGrant) -> Vec<LeaseRequest> {
    let mut requests: Vec<LeaseRequest> = family
        .leases
        .iter()
        .map(|grant| grant.request.clone())
        .collect();
    requests.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
    requests
}

/// Settle the family's attempt record, or refuse when the attempt already
/// settled (E07.2: a stale attempt cannot release). A family whose attempt
/// row never landed has no record to settle.
fn settle_family_attempt(
    transaction: &Connection,
    attempt_id: &str,
    completed: bool,
) -> Result<(), MutationError> {
    let phase: Option<String> = transaction
        .query_row(
            "SELECT phase FROM work_unit_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(phase) = phase else {
        return Ok(());
    };
    if phase.starts_with("settled-") {
        return Err(MutationError::StaleAttempt(attempt_id.to_string()));
    }
    let next = if completed {
        "settled-completed"
    } else {
        "settled-failed"
    };
    let now = now_ms();
    let changed = transaction.execute(
        "UPDATE work_unit_attempts
         SET phase = ?2, updated_at_ms = ?3, settled_at_ms = ?3
         WHERE attempt_id = ?1 AND phase IN ('prepared', 'dispatched')",
        params![attempt_id, next, now],
    )?;
    if changed != 1 {
        return Err(MutationError::OperationConflict);
    }
    Ok(())
}

/// Deactivate exactly this family's active member leases.
fn deactivate_family_members(
    transaction: &Connection,
    family: &LeaseFamilyGrant,
) -> Result<bool, MutationError> {
    let now = now_ms();
    let mut released_any = false;
    for grant in &family.leases {
        if !grant.active {
            continue;
        }
        transaction.execute(
            "UPDATE path_leases SET active = 0, released_at_ms = ?1
             WHERE lease_id = ?2 AND active = 1",
            params![now, grant.lease_id],
        )?;
        released_any = true;
    }
    Ok(released_any)
}

fn normalize_request(mut request: LeaseRequest) -> Result<LeaseRequest, MutationError> {
    require_fields(&[
        ("workspace_key", &request.workspace_key),
        ("operation_id", &request.operation_id),
        ("owner_id", &request.owner_id),
    ])?;
    normalize_paths(&mut request.read_paths)?;
    normalize_paths(&mut request.write_paths)?;
    if !request.repo_exclusive && request.read_paths.is_empty() && request.write_paths.is_empty() {
        return Err(MutationError::EmptyLease);
    }
    if request.read_paths.iter().any(|read_path| {
        request
            .write_paths
            .iter()
            .any(|write_path| paths_overlap(read_path, write_path))
    }) {
        return Err(MutationError::ConflictingScope);
    }
    Ok(request)
}

fn normalize_paths(paths: &mut Vec<String>) -> Result<(), MutationError> {
    for path in paths.iter_mut() {
        *path = normalize_workspace_relative_path(path)
            .map_err(|error| MutationError::InvalidPath(error.to_string()))?;
        #[cfg(windows)]
        {
            *path = path.to_lowercase();
        }
    }
    paths.sort();
    paths.dedup();
    Ok(())
}

fn scopes_conflict(left: &LeaseRequest, right: &LeaseRequest) -> bool {
    if left.repo_exclusive || right.repo_exclusive {
        return true;
    }
    left.write_paths.iter().any(|left_path| {
        right
            .read_paths
            .iter()
            .chain(&right.write_paths)
            .any(|right_path| paths_overlap(left_path, right_path))
    }) || right.write_paths.iter().any(|right_path| {
        left.read_paths
            .iter()
            .any(|left_path| paths_overlap(left_path, right_path))
    })
}

fn paths_overlap(left: &str, right: &str) -> bool {
    path_contains(left, right) || path_contains(right, left)
}

fn path_contains(scope: &str, path: &str) -> bool {
    scope == path
        || path
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn next_epoch(connection: &Connection, workspace_key: &str) -> Result<u64, MutationError> {
    let current: Option<i64> = connection
        .query_row(
            "SELECT last_epoch FROM lease_epochs WHERE workspace_key = ?1",
            params![workspace_key],
            |row| row.get(0),
        )
        .optional()?;
    let next = current
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| MutationError::Serialization("workspace fencing epoch overflow".into()))?;
    connection.execute(
        "INSERT INTO lease_epochs(workspace_key, last_epoch) VALUES (?1, ?2)
         ON CONFLICT(workspace_key) DO UPDATE SET last_epoch = excluded.last_epoch",
        params![workspace_key, next],
    )?;
    u64::try_from(next).map_err(|error| MutationError::Serialization(error.to_string()))
}

fn grant_for_operation(
    connection: &Connection,
    workspace_key: &str,
    operation_id: &str,
) -> Result<Option<LeaseGrant>, MutationError> {
    let lease_id: Option<String> = connection
        .query_row(
            "SELECT lease_id FROM path_leases WHERE workspace_key = ?1 AND operation_id = ?2",
            params![workspace_key, operation_id],
            |row| row.get(0),
        )
        .optional()?;
    lease_id
        .map(|id| grant_by_id(connection, &id))
        .transpose()
        .map(Option::flatten)
}

fn grant_by_id(
    connection: &Connection,
    lease_id: &str,
) -> Result<Option<LeaseGrant>, MutationError> {
    let row: Option<(String, String, i64, i64)> = connection
        .query_row(
            "SELECT request_json, request_hash, fencing_epoch, active
             FROM path_leases WHERE lease_id = ?1",
            params![lease_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((json, stored_hash, epoch, active)) = row else {
        return Ok(None);
    };
    let request: LeaseRequest = serde_json::from_str(&json)
        .map_err(|error| MutationError::Serialization(error.to_string()))?;
    if normalize_request(request.clone())? != request {
        return Err(MutationError::Serialization(
            "non-canonical lease request".into(),
        ));
    }
    let actual_hash = r_code_harness_protocol::canonical_input_hash(
        &serde_json::to_value(&request)
            .map_err(|error| MutationError::Serialization(error.to_string()))?,
    );
    if actual_hash != stored_hash {
        return Err(MutationError::Serialization(
            "lease request identity mismatch".into(),
        ));
    }
    Ok(Some(LeaseGrant {
        lease_id: lease_id.to_string(),
        request,
        fencing_epoch: u64::try_from(epoch)
            .map_err(|error| MutationError::Serialization(error.to_string()))?,
        active: active == 1,
    }))
}

fn active_grants(
    connection: &Connection,
    workspace_key: &str,
) -> Result<Vec<LeaseGrant>, MutationError> {
    let mut statement = connection.prepare(
        "SELECT lease_id FROM path_leases WHERE workspace_key = ?1 AND active = 1
         ORDER BY fencing_epoch, lease_id",
    )?;
    let ids = statement
        .query_map(params![workspace_key], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    ids.into_iter()
        .map(|id| grant_by_id(connection, &id)?.ok_or(MutationError::LeaseNotFound))
        .collect()
}

fn validate_grant_owner(grant: &LeaseGrant, owner: &str, epoch: u64) -> Result<(), MutationError> {
    if grant.request.owner_id == owner && grant.fencing_epoch == epoch {
        Ok(())
    } else {
        Err(MutationError::StaleLease)
    }
}

fn require_active_grant(
    connection: &Connection,
    lease_id: &str,
    owner: &str,
    epoch: u64,
) -> Result<LeaseGrant, MutationError> {
    let grant = grant_by_id(connection, lease_id)?.ok_or(MutationError::LeaseNotFound)?;
    validate_grant_owner(&grant, owner, epoch)?;
    if grant.active {
        Ok(grant)
    } else {
        Err(MutationError::LeaseInactive)
    }
}

fn normalize_prepared_operation(
    operation: &MutationOperation,
) -> Result<MutationOperation, MutationError> {
    require_fields(&[
        ("operation_id", &operation.operation_id),
        ("workspace_key", &operation.workspace_key),
        ("lease_id", &operation.lease_id),
        ("owner_id", &operation.owner_id),
        ("input_hash", &operation.input_hash),
    ])?;
    if operation.fencing_epoch == 0 {
        return Err(MutationError::StaleLease);
    }
    if operation.state != MutationState::Prepared {
        return Err(MutationError::InvalidTransition {
            expected: MutationState::Prepared,
            actual: operation.state,
        });
    }
    let mut normalized = operation.clone();
    let mut seen = BTreeSet::new();
    for file in &mut normalized.files {
        file.logical_path = normalize_workspace_relative_path(&file.logical_path)
            .map_err(|error| MutationError::InvalidPath(error.to_string()))?;
        #[cfg(windows)]
        {
            file.logical_path = file.logical_path.to_lowercase();
        }
        if !seen.insert(file.logical_path.clone()) {
            return Err(MutationError::InvalidFileIdentity);
        }
    }
    if normalized.files.is_empty() {
        return Err(MutationError::EmptyField("files"));
    }
    normalized
        .files
        .sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    let identities = normalized
        .files
        .iter()
        .map(has_any_identity)
        .collect::<Vec<_>>();
    if identities.iter().any(|present| *present)
        && (!identities.iter().all(|present| *present)
            || !normalized.files.iter().all(valid_identity_pairs))
    {
        return Err(MutationError::InvalidFileIdentity);
    }
    Ok(normalized)
}

fn normalize_applied_files(
    mut files: Vec<MutationFile>,
) -> Result<Vec<MutationFile>, MutationError> {
    let mut seen = BTreeSet::new();
    for file in &mut files {
        file.logical_path = normalize_workspace_relative_path(&file.logical_path)
            .map_err(|error| MutationError::InvalidPath(error.to_string()))?;
        #[cfg(windows)]
        {
            file.logical_path = file.logical_path.to_lowercase();
        }
        if !seen.insert(file.logical_path.clone()) || !valid_identity_pairs(file) {
            return Err(MutationError::InvalidFileIdentity);
        }
    }
    files.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    Ok(files)
}

fn validate_operation_scope(
    operation: &MutationOperation,
    grant: &LeaseGrant,
) -> Result<(), MutationError> {
    if operation.workspace_key != grant.request.workspace_key {
        return Err(MutationError::OperationConflict);
    }
    if grant.request.repo_exclusive
        || operation.files.iter().all(|file| {
            grant
                .request
                .write_paths
                .iter()
                .any(|scope| path_contains(scope, &file.logical_path))
        })
    {
        Ok(())
    } else {
        Err(MutationError::FileNotCovered)
    }
}

fn validate_operation_owner(
    operation: &MutationOperation,
    owner: &str,
    epoch: u64,
) -> Result<(), MutationError> {
    if operation.owner_id == owner && operation.fencing_epoch == epoch {
        Ok(())
    } else {
        Err(MutationError::StaleLease)
    }
}

fn same_prepared_input(left: &MutationOperation, right: &MutationOperation) -> bool {
    let same_files = match (
        left.files.iter().any(has_any_identity),
        right.files.iter().any(has_any_identity),
    ) {
        (true, true) => left.files == right.files,
        (false, false) => file_paths(&left.files) == file_paths(&right.files),
        _ => false,
    };
    left.operation_id == right.operation_id
        && left.workspace_key == right.workspace_key
        && left.lease_id == right.lease_id
        && left.owner_id == right.owner_id
        && left.fencing_epoch == right.fencing_epoch
        && left.input_hash == right.input_hash
        && same_files
}

fn load_operation_from(
    connection: &Connection,
    operation_id: &str,
) -> Result<Option<MutationOperation>, MutationError> {
    let row: Option<(String, String, String, i64, String, String)> = connection
        .query_row(
            "SELECT workspace_key, lease_id, owner_id, fencing_epoch, input_hash, state
             FROM mutation_operations WHERE operation_id = ?1",
            params![operation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((workspace_key, lease_id, owner_id, epoch, input_hash, state)) = row else {
        return Ok(None);
    };
    let mut statement = connection.prepare(
        "SELECT logical_path, before_sha256, after_sha256, before_cas_ref, after_cas_ref
         FROM mutation_files WHERE operation_id = ?1 ORDER BY logical_path",
    )?;
    let files = statement
        .query_map(params![operation_id], |row| {
            Ok(MutationFile {
                logical_path: row.get(0)?,
                before_sha256: row.get(1)?,
                after_sha256: row.get(2)?,
                before_cas_ref: row.get(3)?,
                after_cas_ref: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(MutationOperation {
        operation_id: operation_id.to_string(),
        workspace_key,
        lease_id,
        owner_id,
        fencing_epoch: u64::try_from(epoch)
            .map_err(|error| MutationError::Serialization(error.to_string()))?,
        input_hash,
        state: MutationState::parse(&state)?,
        files,
    }))
}

fn require_fields(fields: &[(&'static str, &str)]) -> Result<(), MutationError> {
    if let Some((name, _)) = fields.iter().find(|(_, value)| value.trim().is_empty()) {
        Err(MutationError::EmptyField(name))
    } else {
        Ok(())
    }
}

fn file_paths(files: &[MutationFile]) -> Vec<&str> {
    files
        .iter()
        .map(|file| file.logical_path.as_str())
        .collect()
}

fn has_any_identity(file: &MutationFile) -> bool {
    file.before_sha256.is_some()
        || file.after_sha256.is_some()
        || file.before_cas_ref.is_some()
        || file.after_cas_ref.is_some()
}

fn valid_identity_pairs(file: &MutationFile) -> bool {
    let before = valid_identity_pair(&file.before_sha256, &file.before_cas_ref);
    let after = valid_identity_pair(&file.after_sha256, &file.after_cas_ref);
    before && after && (file.before_sha256.is_some() || file.after_sha256.is_some())
}

fn valid_identity_pair(hash: &Option<String>, cas_ref: &Option<String>) -> bool {
    match (hash, cas_ref) {
        (None, None) => true,
        (Some(hash), Some(reference)) => is_sha256(hash) && !reference.trim().is_empty(),
        _ => false,
    }
}

fn is_sha256(value: &str) -> bool {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
