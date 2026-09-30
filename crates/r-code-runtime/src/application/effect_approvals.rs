use super::{now_ms, ApplicationError, ApplicationService};
use crate::plugins::approval_store::{EffectBinding, EffectContext};
use crate::services::run_snapshots::EffectApprovalSource;
use r_code_harness_protocol::services::work_unit_payload_hash;
use r_code_kernel::ports::JournalStore as _;
use r_code_store::v1::plans::{EffectApprovalRecord, EffectApprovalState, PlanStoreError};
use r_code_store::v1::V1Store;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// P19B-R: the fixed scope every effect approval is minted under. The store
/// validates this exact literal, so it is the single source of truth here.
pub const EFFECT_APPROVE_SCOPE: &str = "effect.approve";

/// Credential-free projection of one persisted effect approval. Field-for-
/// field identical to the store record so P19B-C clients render the same
/// canonical material the runtime froze against; nothing is expanded or
/// inferred. `state` is `"active"` or `"superseded"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectApprovalView {
    pub approval_id: String,
    pub task_id: String,
    pub plan_revision: String,
    pub work_unit_id: String,
    pub effect_class: String,
    pub network: String,
    pub actor_id: String,
    pub session_id: String,
    pub scope: String,
    pub payload_hash: String,
    pub state: String,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at_ms: Option<i64>,
}

impl EffectApprovalView {
    fn from_record(record: &EffectApprovalRecord) -> Self {
        Self {
            approval_id: record.approval_id.clone(),
            task_id: record.task_id.clone(),
            plan_revision: record.plan_revision.clone(),
            work_unit_id: record.work_unit_id.clone(),
            effect_class: record.effect_class.clone(),
            network: record.network.clone(),
            actor_id: record.actor_id.clone(),
            session_id: record.session_id.clone(),
            scope: record.scope.clone(),
            payload_hash: record.payload_hash.clone(),
            state: match record.state {
                EffectApprovalState::Active => "active",
                EffectApprovalState::Superseded => "superseded",
            }
            .to_string(),
            created_at_ms: record.created_at_ms,
            superseded_at_ms: record.superseded_at_ms,
        }
    }
}

/// P19B-R: the production [`EffectApprovalSource`] the run manager wires
/// into the snapshot freeze gate. It answers ONLY an exact six-column match
/// against the durable store and fails closed (no authority) on any lookup
/// error, so a granted-then-revoked or foreign approval never expands a
/// WorkUnit. This is the real counterpart of the test-side adapter in
/// `tests/s19a_effect_snapshot_gate.rs`.
pub struct StoreEffectApprovals {
    store: Arc<V1Store>,
}

impl StoreEffectApprovals {
    pub fn new(store: Arc<V1Store>) -> Self {
        Self { store }
    }
}

impl EffectApprovalSource for StoreEffectApprovals {
    fn has_exact_approval(
        &self,
        task_id: &str,
        plan_revision: &str,
        work_unit_id: &str,
        effect_class: &str,
        network: &str,
        payload_hash: &str,
    ) -> bool {
        self.store
            .find_active_effect_approval(
                task_id,
                plan_revision,
                work_unit_id,
                effect_class,
                network,
                payload_hash,
            )
            .map(|found| found.is_some())
            .unwrap_or(false)
    }
}

impl ApplicationService {
    // -- effect approvals (P19B-R) ------------------------------------------

    /// `approvals.effect.request`: create (or replay) a host-owned pending
    /// request for one WorkUnit of the CURRENTLY-approved plan revision. The
    /// class/network/payload-hash binding is derived from the approved plan
    /// material — never from client input or mutable settings — so a pending
    /// request can never claim authority the approved plan does not carry,
    /// and a request for an unpublished or superseded revision is refused.
    /// The decision itself arrives through the ordinary authenticated
    /// `approvals.decide` surface.
    pub async fn effect_approval_request(
        &self,
        task_id: &str,
        work_unit_id: &str,
        operation_id: &str,
        run_id: &str,
    ) -> Result<serde_json::Value, ApplicationError> {
        if task_id.trim().is_empty()
            || work_unit_id.trim().is_empty()
            || operation_id.trim().is_empty()
        {
            return Err(ApplicationError::Task(
                "taskId, workUnitId and operationId are required".to_string(),
            ));
        }
        if self.store.load_task(task_id).await.is_none() {
            return Err(ApplicationError::Task(format!("task {task_id} not found")));
        }
        let binding = self.current_effect_binding(task_id, work_unit_id)?;
        if let Some(record) = self.find_active_effect(task_id, &binding)? {
            // The exact authority is already granted: replay it instead of
            // queueing a second question nobody needs to answer.
            return Ok(effect_request_response(
                "granted",
                None,
                task_id,
                &binding,
                Some(&record),
            ));
        }
        if self
            .store
            .effect_approvals_for_task(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .iter()
            .any(|record| {
                record.state == EffectApprovalState::Active && record.work_unit_id == work_unit_id
            })
        {
            // One active approval per (task, work unit): a stale active row
            // for an earlier payload must be revoked explicitly first.
            return Err(ApplicationError::Task(format!(
                "effect_approval_active_exists: work unit {work_unit_id} already has an active effect approval for a different payload; revoke it first"
            )));
        }
        let summary = format!(
            "effect approval for {work_unit_id} [{}/{}]",
            binding.effect_class, binding.network
        );
        self.approvals
            .register_effect(operation_id, &summary, run_id, task_id, binding.clone())
            .await;
        Ok(effect_request_response(
            "pending",
            Some(operation_id),
            task_id,
            &binding,
            None,
        ))
    }

    /// `approvals.effect.list`: the audit surface for one task — every
    /// persisted approval (active and superseded, oldest first) plus the
    /// still-decidable pending requests. Both project the same canonical
    /// six-column material P19B-C renders.
    pub async fn effect_approval_list(
        &self,
        task_id: &str,
    ) -> Result<serde_json::Value, ApplicationError> {
        if task_id.trim().is_empty() {
            return Err(ApplicationError::Task("taskId is required".to_string()));
        }
        let approvals = self
            .store
            .effect_approvals_for_task(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .iter()
            .map(EffectApprovalView::from_record)
            .collect::<Vec<_>>();
        let now = now_ms();
        let pending = self
            .approvals
            .pending()
            .await
            .into_iter()
            .filter(|row| row.task_id == task_id && row.effect.is_some())
            .map(|row| {
                let binding = row.effect.clone().expect("filtered above");
                let age_ms = if row.created_ms > 0 {
                    now - row.created_ms
                } else {
                    0
                };
                serde_json::json!({
                    "operationId": row.op_id,
                    "summary": row.summary,
                    "createdSeq": row.created_seq,
                    "createdMs": row.created_ms,
                    "ageMs": age_ms.max(0),
                    "request": binding.material(&row.task_id),
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "taskId": task_id,
            "pending": pending,
            "approvals": approvals,
        }))
    }

    /// `approvals.effect.revoke`: supersede the active approval for one work
    /// unit. Revocation is future-runs-only by construction — frozen
    /// RunSnapshots are immutable rows nothing here rewrites; only the NEXT
    /// freeze re-consults the store and fails closed. The store row
    /// (state=superseded, supersededAtMs) is the durable audit; the revoker
    /// attribution rides a best-effort `effect.approval.revoked` journal
    /// event. Idempotent: revoking with nothing active reports
    /// `revoked: false`.
    pub async fn effect_approval_revoke(
        &self,
        task_id: &str,
        work_unit_id: &str,
        actor_id: &str,
        session_id: &str,
    ) -> Result<serde_json::Value, ApplicationError> {
        if task_id.trim().is_empty() || work_unit_id.trim().is_empty() {
            return Err(ApplicationError::Task(
                "taskId and workUnitId are required".to_string(),
            ));
        }
        if actor_id.trim().is_empty() || session_id.trim().is_empty() {
            return Err(ApplicationError::Task(
                "actor and session are required".to_string(),
            ));
        }
        let approval_id = self
            .store
            .effect_approvals_for_task(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .into_iter()
            .find(|record| {
                record.state == EffectApprovalState::Active && record.work_unit_id == work_unit_id
            })
            .map(|record| record.approval_id);
        let superseded_at_ms = now_ms();
        let revoked = self
            .store
            .supersede_effect_approval(task_id, work_unit_id, superseded_at_ms)
            .map_err(|error| ApplicationError::Store(error.to_string()))?;
        if revoked {
            // Best-effort attribution: the superseded store row already
            // committed, so a journal failure must not fail the revocation.
            if let Some(task) = self.store.load_task(task_id).await {
                let event = r_code_kernel::ports::JournalEvent {
                    seq: 0,
                    task_id: task_id.to_string(),
                    kind: "effect.approval.revoked".to_string(),
                    payload: serde_json::json!({
                        "approvalId": approval_id,
                        "workUnitId": work_unit_id,
                        "actorId": actor_id,
                        "sessionId": session_id,
                        "supersededAtMs": superseded_at_ms,
                    }),
                };
                if let Err(error) = r_code_kernel::ports::JournalStore::save_task_and_events(
                    &*self.store,
                    &task,
                    vec![event],
                )
                .await
                {
                    eprintln!("effect-approval revoke: journal append failed: {error}");
                }
            }
        }
        Ok(serde_json::json!({
            "taskId": task_id,
            "workUnitId": work_unit_id,
            "approvalId": approval_id,
            "revoked": revoked,
        }))
    }

    /// Derive the exact effect binding from the CURRENTLY-approved plan
    /// revision. Closes the P19A observation that the store alone would
    /// persist an approval for any revision string: a binding only ever
    /// exists for the real head covered by an active plan approval, and the
    /// payload hash comes from `work_unit_payload_hash` over the approved
    /// material.
    fn current_effect_binding(
        &self,
        task_id: &str,
        work_unit_id: &str,
    ) -> Result<EffectBinding, ApplicationError> {
        let plan = self
            .store
            .current_plan_revision(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} has no plan")))?;
        let approval = self
            .store
            .load_active_plan_approval(task_id)
            .map_err(|error| ApplicationError::Store(error.to_string()))?
            .ok_or_else(|| {
                ApplicationError::Task(format!(
                    "plan_not_approved: task {task_id} has no active plan approval"
                ))
            })?;
        if approval.plan_revision.as_str() != plan.reference().as_str() {
            return Err(ApplicationError::Task(
                "effect_approval_stale: the active plan approval does not cover the current plan head"
                    .to_string(),
            ));
        }
        let wire = plan
            .material()
            .work_units
            .iter()
            .find(|wire| wire.id == work_unit_id)
            .ok_or_else(|| {
                ApplicationError::Task(format!(
                    "work_unit_not_found: {work_unit_id} is not in the approved plan"
                ))
            })?;
        Ok(EffectBinding {
            plan_revision: plan.reference().as_str().to_string(),
            work_unit_id: work_unit_id.to_string(),
            effect_class: wire.effect_class.as_str().to_string(),
            network: wire.network_ceiling.as_str().to_string(),
            payload_hash: work_unit_payload_hash(wire),
        })
    }

    /// Refuse a grant whose pending binding no longer matches the approved
    /// plan head (re-published or invalidated after the request): a stale
    /// pending operation can never mint an approval.
    pub(super) fn validate_effect_binding_current(
        &self,
        task_id: &str,
        binding: &EffectBinding,
    ) -> Result<(), ApplicationError> {
        let current = self.current_effect_binding(task_id, &binding.work_unit_id)?;
        if current != *binding {
            return Err(ApplicationError::Task(
                "effect_approval_stale: the pending request no longer matches the approved plan payload"
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn find_active_effect(
        &self,
        task_id: &str,
        binding: &EffectBinding,
    ) -> Result<Option<EffectApprovalRecord>, ApplicationError> {
        self.store
            .find_active_effect_approval(
                task_id,
                &binding.plan_revision,
                &binding.work_unit_id,
                &binding.effect_class,
                &binding.network,
                &binding.payload_hash,
            )
            .map_err(|error| ApplicationError::Store(error.to_string()))
    }

    /// Persist the one immutable approval a granted decision commits. The
    /// operation id IS the approval id (one pending generation ↔ one
    /// approval lifetime). Retries are idempotent: an identical replay is a
    /// store no-op, and a concurrent or later replay observes the active row
    /// instead of re-minting. A superseded id is never re-minted — the
    /// conflict surfaces instead.
    pub(super) fn materialize_effect_approval(
        &self,
        operation_id: &str,
        context: &EffectContext,
        actor_id: &str,
        session_id: &str,
    ) -> Result<EffectApprovalView, ApplicationError> {
        let binding = &context.binding;
        let record = EffectApprovalRecord {
            approval_id: operation_id.to_string(),
            task_id: context.task_id.clone(),
            plan_revision: binding.plan_revision.clone(),
            work_unit_id: binding.work_unit_id.clone(),
            effect_class: binding.effect_class.clone(),
            network: binding.network.clone(),
            actor_id: actor_id.to_string(),
            session_id: session_id.to_string(),
            scope: EFFECT_APPROVE_SCOPE.to_string(),
            payload_hash: binding.payload_hash.clone(),
            state: EffectApprovalState::Active,
            created_at_ms: now_ms(),
            superseded_at_ms: None,
        };
        match self.store.save_effect_approval(record.clone()) {
            Ok(()) => Ok(EffectApprovalView::from_record(&record)),
            Err(PlanStoreError::EffectApprovalIdConflict)
            | Err(PlanStoreError::EffectActiveApprovalExists) => {
                // A first grant already persisted (its attribution wins and
                // can never drift): the exact active row is the answer. With
                // no matching active row the conflict is real (e.g. a
                // granted-then-revoked id) and stays an error.
                match self.find_active_effect(&context.task_id, binding)? {
                    Some(existing) => Ok(EffectApprovalView::from_record(&existing)),
                    None => Err(ApplicationError::Session(format!(
                        "effect_approval_conflict: {operation_id}"
                    ))),
                }
            }
            Err(error) => Err(ApplicationError::Store(error.to_string())),
        }
    }
}

/// One canonical `approvals.effect.request` answer: the exact six-column
/// request material plus either the pending `operationId` or the already
/// active `approval` record.
fn effect_request_response(
    status: &str,
    operation_id: Option<&str>,
    task_id: &str,
    binding: &EffectBinding,
    approval: Option<&EffectApprovalRecord>,
) -> serde_json::Value {
    let mut response = serde_json::json!({
        "status": status,
        "request": binding.material(task_id),
    });
    if let Some(operation_id) = operation_id {
        response["operationId"] = serde_json::json!(operation_id);
    }
    if let Some(record) = approval {
        response["approval"] =
            serde_json::to_value(EffectApprovalView::from_record(record)).unwrap_or_default();
    }
    response
}
