//! Plan 入口建议聚合的持久化（docs/plan-mode-dual-track-gate.md §11、§12）。
//!
//! 核心不变量：
//! - `pending` 建议不修改 `tasks.mode`、不创建 `plans` 行、不派发任何 continuation；
//! - offer 插入与 branch 建议预算消耗在同一事务完成（同 branch 至多一个阻断建议）；
//! - 决定使用 revision CAS；接受前在同一事务内重新比较 Provider snapshot；
//! - 所有状态都能从 SQLite 恢复，不依赖进程内布尔值。
//!
//! 续接合同是 at-least-once / manual-retry：continuation operation ID 确定性派生，
//! 崩溃窗口由启动恢复标记 failed 后显式重试（docs §12.4 的降级条款）。

use std::sync::Arc;

use chrono::Utc;
use r_code_core::error::ProductError;
use r_code_core::plan_entry::{
    OriginRequestEnvelope, PlanComplexitySignal, PlanEntryContinuationState,
    PlanEntryDecisionInput, PlanEntryDecisionSource, PlanEntryOffer, PlanEntryOfferState,
    PlanSuggestionBranchState, ProviderRouteSnapshot, ResolvedPlanRuntimeProfile,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::Database;

pub const PLAN_ENTRY_CONTINUATION_INTERRUPTED: &str =
    "PLAN_ENTRY_CONTINUATION_INTERRUPTED: application restarted before dispatch acknowledgement";

/// 接受后续接：与显式 enter_plan_mode 的 Plan 续接同一语义。
pub const PLAN_ENTRY_ACCEPT_CONTINUATION: &str = "[system] The user accepted the planning \
suggestion, and R-Code safely changed this task from Agent mode to Plan mode. Continue the \
user's original request as a structured Plan. Investigate read-only as needed, ask only \
blocking questions with request_user_input, then publish the complete functional Plan with \
plan_publish. Do not edit files or execute implementation before the user approves the Plan.";

/// 拒绝/关闭/Escape 后续接：恢复原 Agent 请求，并附宿主抑制上下文。
pub const PLAN_ENTRY_DECLINE_CONTINUATION: &str = "[system] The user chose to continue \
directly without a Plan. Resume the original request and complete it in Agent mode. Do not \
call propose_plan_mode or enter_plan_mode for this task again; the user already declined \
planning for it.";

/// Provider supersede 后续接：模型仍挂起，幂等恢复原 Agent 请求。
pub const PLAN_ENTRY_SUPERSEDED_CONTINUATION: &str = "[system] The planning suggestion was \
cancelled because the model service changed. Resume the original request in Agent mode and \
complete it directly. Do not call propose_plan_mode again for this task.";

fn db_err(error: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(error.to_string())
}

fn invalid(message: impl Into<String>) -> ProductError {
    ProductError::StateMachineError(message.into())
}

fn parse_ts(value: &str) -> Result<chrono::DateTime<chrono::Utc>, ProductError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|error| ProductError::DatabaseError(format!("timestamp parse error: {error}")))
}

/// 创建待决建议的输入。全部字段由宿主从可信执行上下文绑定，模型不能传入 task、
/// run、request key 或目标模式。
#[derive(Debug, Clone)]
pub struct CreatePlanEntryOfferInput {
    pub task_id: String,
    pub branch_id: String,
    pub source_run_id: String,
    pub request_key: String,
    pub original_mode: String,
    pub reason_audit: String,
    pub signals: Vec<PlanComplexitySignal>,
    pub primary_signal: PlanComplexitySignal,
    pub customer_copy_key: String,
    pub customer_copy_version: u32,
    pub provider: ProviderRouteSnapshot,
    pub eligibility_profile_version: String,
    pub evidence_version: String,
    pub resolved_plan_runtime_profile: ResolvedPlanRuntimeProfile,
}

/// 决定事务的结果。`Superseded` 表示接受时发现 Provider route 已变化，事务内已
/// CAS 为 superseded 并排入 Agent 续接；`Replay` 表示同一幂等键的重复提交。
#[derive(Debug, Clone, PartialEq)]
pub enum PlanEntryDecisionOutcome {
    Accepted(PlanEntryOffer),
    Declined(PlanEntryOffer),
    Superseded(PlanEntryOffer),
    Replay(PlanEntryOffer),
}

impl PlanEntryDecisionOutcome {
    pub fn offer(&self) -> &PlanEntryOffer {
        match self {
            Self::Accepted(offer)
            | Self::Declined(offer)
            | Self::Superseded(offer)
            | Self::Replay(offer) => offer,
        }
    }
}

/// Store-backed Plan 入口建议聚合。与 `PlanStore` 共享同一 SQLite 数据库。
#[derive(Clone)]
pub struct PlanEntryStore {
    db: Arc<Database>,
}

impl PlanEntryStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// 记录统一宿主请求信封（幂等）。所有发送分支在分流前调用。
    pub fn record_origin_request(
        &self,
        envelope: &OriginRequestEnvelope,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO origin_requests \
             (request_key, kind, parent_request_key, operation_id, task_id, branch_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                envelope.request_key,
                envelope.kind.as_str(),
                envelope.parent_request_key,
                envelope.operation_id,
                envelope.task_id,
                envelope.branch_id,
                envelope.created_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 创建待决建议并消耗 branch 建议预算（同一事务）。任一前置不满足时 fail
    /// closed：不产生 offer，预算不动。
    pub fn create_offer(
        &self,
        input: &CreatePlanEntryOfferInput,
    ) -> Result<PlanEntryOffer, ProductError> {
        let offer_id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let signals_json = serde_json::to_string(&input.signals)
            .map_err(|error| invalid(format!("serialize signals: {error}")))?;
        let profile_json = serde_json::to_string(&input.resolved_plan_runtime_profile)
            .map_err(|error| invalid(format!("serialize plan runtime profile: {error}")))?;

        {
            let mut conn = self.db.conn()?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(db_err)?;
            let task: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT state, agent_engine, mode FROM tasks WHERE id = ?1",
                    params![input.task_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(db_err)?;
            let Some((task_state, agent_engine, task_mode)) = task else {
                return Err(invalid(format!("task does not exist: {}", input.task_id)));
            };
            if task_state == "archived" {
                return Err(invalid(
                    "an archived task cannot receive a planning suggestion",
                ));
            }
            if agent_engine != "r_code" {
                return Err(invalid(
                    "planning suggestions require the R-Code main Agent",
                ));
            }
            if !matches!(task_mode.as_str(), "ask" | "edit" | "auto") {
                return Err(invalid(format!(
                    "propose_plan_mode is unavailable while task mode is {task_mode}"
                )));
            }
            let branch_is_active: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM session_branches \
                 WHERE task_id = ?1 AND id = ?2 AND is_active = 1)",
                    params![input.task_id, input.branch_id],
                    |row| row.get(0),
                )
                .map_err(db_err)?;
            if !branch_is_active {
                return Err(invalid(
                    "planning suggestions require the active session branch",
                ));
            }
            // request 级幂等边界优先于 branch 预算（docs §6.2 双层抑制各自独立）：
            // 同 request key 永远最多一个 offer；同 task 至多一个 pending。唯一索引
            // 兜底并发竞态，这里先给出可读错误。
            let duplicate: Option<String> = tx
                .query_row(
                    "SELECT id FROM plan_entry_offers \
                 WHERE task_id = ?1 AND branch_id = ?2 AND request_key = ?3",
                    params![input.task_id, input.branch_id, input.request_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_err)?;
            if duplicate.is_some() {
                return Err(invalid(
                    "this request already has a planning suggestion (request key dedup)",
                ));
            }
            let pending: Option<String> = tx
                .query_row(
                    "SELECT id FROM plan_entry_offers WHERE task_id = ?1 AND state = 'pending'",
                    params![input.task_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_err)?;
            if pending.is_some() {
                return Err(invalid(
                    "this task already has a pending planning suggestion",
                ));
            }
            // branch 预算与安静状态：任一命中都不再产生主动阻断建议。
            let branch = load_branch_state(&tx, &input.task_id, &input.branch_id)?;
            if let Some(branch) = branch {
                if branch.quiet_after_decline {
                    return Err(invalid(
                        "this task branch is quiet after the user declined a planning suggestion",
                    ));
                }
                if branch.suggestion_budget_consumed_at.is_some() {
                    return Err(invalid(
                        "this task branch already used its single planning suggestion",
                    ));
                }
            }

            tx.execute(
                "INSERT INTO plan_entry_offers \
             (id, task_id, branch_id, source_run_id, request_key, original_mode, reason_audit, \
              signals_json, primary_signal, customer_copy_key, customer_copy_version, \
              provider_kind, provider_profile_id, provider_profile_version, \
              provider_route_revision, model_id, wire_protocol, endpoint_class, \
              eligibility_profile_version, evidence_version, resolved_plan_runtime_profile_json, \
              revision, state, continuation_state, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                     ?17, ?18, ?19, ?20, ?21, 1, 'pending', 'none', ?22, ?22)",
                params![
                    offer_id,
                    input.task_id,
                    input.branch_id,
                    input.source_run_id,
                    input.request_key,
                    input.original_mode,
                    input.reason_audit,
                    signals_json,
                    input.primary_signal.as_str(),
                    input.customer_copy_key,
                    input.customer_copy_version as i64,
                    input.provider.provider_kind,
                    input.provider.provider_profile_id,
                    input.provider.provider_profile_version,
                    input.provider.provider_route_revision,
                    input.provider.model_id,
                    input.provider.wire_protocol,
                    input.provider.endpoint_class,
                    input.eligibility_profile_version,
                    input.evidence_version,
                    profile_json,
                    now,
                ],
            )
            .map_err(db_err)?;
            // 预算消耗与 offer 创建同事务：同 branch 至多一个阻断建议。
            tx.execute(
                "INSERT INTO plan_suggestion_branch_states \
             (task_id, branch_id, suggestion_budget_consumed_at, quiet_after_decline, \
              quiet_reason, revision, updated_at) \
             VALUES (?1, ?2, ?3, 0, NULL, 1, ?3) \
             ON CONFLICT(task_id, branch_id) DO UPDATE SET \
              suggestion_budget_consumed_at = excluded.suggestion_budget_consumed_at, \
              revision = revision + 1, updated_at = excluded.updated_at",
                params![input.task_id, input.branch_id, now],
            )
            .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
        }
        self.get_offer(&offer_id)?
            .ok_or_else(|| invalid("plan entry offer vanished after insert"))
    }

    pub fn get_offer(&self, offer_id: &str) -> Result<Option<PlanEntryOffer>, ProductError> {
        let conn = self.db.conn()?;
        load_offer(&conn, offer_id)
    }

    /// 每个任务至多一个 pending 建议（唯一索引保证）。
    pub fn pending_offer_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<PlanEntryOffer>, ProductError> {
        let conn = self.db.conn()?;
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM plan_entry_offers WHERE task_id = ?1 AND state = 'pending'",
                params![task_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        match id.as_deref() {
            Some(id) => load_offer(&conn, id),
            None => Ok(None),
        }
    }

    pub fn list_pending_offers(&self) -> Result<Vec<PlanEntryOffer>, ProductError> {
        let conn = self.db.conn()?;
        let mut statement = conn
            .prepare("SELECT id FROM plan_entry_offers WHERE state = 'pending'")
            .map_err(db_err)?;
        let ids: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        let mut offers = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Some(offer) = load_offer(&conn, id)? {
                offers.push(offer);
            }
        }
        Ok(offers)
    }

    pub fn branch_state(
        &self,
        task_id: &str,
        branch_id: &str,
    ) -> Result<PlanSuggestionBranchState, ProductError> {
        let conn = self.db.conn()?;
        Ok(
            load_branch_state(&conn, task_id, branch_id)?.unwrap_or(PlanSuggestionBranchState {
                task_id: task_id.to_string(),
                branch_id: branch_id.to_string(),
                suggestion_budget_consumed_at: None,
                quiet_after_decline: false,
                quiet_reason: None,
                revision: 0,
                updated_at: Utc::now(),
            }),
        )
    }

    /// 决定事务（docs §12.1/§12.2）。接受在同一 `IMMEDIATE` 事务内完成 route 比对、
    /// CAS、draft Plan 创建、模式切换与续接入队；拒绝完成 CAS、branch 持久安静与
    /// Agent 续接入队。任一步失败整体回滚。
    pub fn decide(
        &self,
        input: &PlanEntryDecisionInput,
        current_route: &ProviderRouteSnapshot,
        projection_path_for_plan: impl Fn(&str) -> Result<String, ProductError>,
    ) -> Result<PlanEntryDecisionOutcome, ProductError> {
        if input.idempotency_key.trim().is_empty() {
            return Err(invalid("decision idempotency key cannot be blank"));
        }
        let now = Utc::now().to_rfc3339();
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let offer = load_offer(&tx, &input.offer_id)?.ok_or_else(|| {
            invalid(format!(
                "plan entry offer does not exist: {}",
                input.offer_id
            ))
        })?;
        if offer.state != PlanEntryOfferState::Pending {
            if offer.decision_idempotency_key.as_deref() == Some(input.idempotency_key.trim()) {
                // 同一幂等键的重复提交：返回已生效的决定，不产生第二个效果。
                tx.commit().map_err(db_err)?;
                return Ok(PlanEntryDecisionOutcome::Replay(offer));
            }
            return Err(invalid(format!(
                "plan entry offer is no longer pending (state: {})",
                offer.state
            )));
        }
        if offer.revision != input.expected_revision {
            return Err(invalid(format!(
                "stale plan entry offer revision: expected {} but current is {}",
                input.expected_revision, offer.revision
            )));
        }

        match input.decision {
            PlanEntryDecisionSource::Accept => {
                // 接受前在同一事务内重新比较当前 route 与冻结 snapshot。
                if offer.provider != *current_route {
                    let superseded = self.supersede_inside_tx(&tx, &offer, &now)?;
                    tx.commit().map_err(db_err)?;
                    return Ok(PlanEntryDecisionOutcome::Superseded(superseded));
                }
                let accepted =
                    self.accept_inside_tx(&tx, offer, input, &now, &projection_path_for_plan)?;
                tx.commit().map_err(db_err)?;
                Ok(PlanEntryDecisionOutcome::Accepted(accepted))
            }
            source @ (PlanEntryDecisionSource::Continue
            | PlanEntryDecisionSource::Close
            | PlanEntryDecisionSource::Escape) => {
                let declined = self.decline_inside_tx(&tx, offer, input, source, &now)?;
                tx.commit().map_err(db_err)?;
                Ok(PlanEntryDecisionOutcome::Declined(declined))
            }
        }
    }

    fn accept_inside_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        offer: PlanEntryOffer,
        input: &PlanEntryDecisionInput,
        now: &str,
        projection_path_for_plan: &impl Fn(&str) -> Result<String, ProductError>,
    ) -> Result<PlanEntryOffer, ProductError> {
        let task: Option<(String, String)> = tx
            .query_row(
                "SELECT state, agent_engine FROM tasks WHERE id = ?1",
                params![offer.task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        let Some((task_state, agent_engine)) = task else {
            return Err(invalid("task no longer exists"));
        };
        if task_state == "archived" {
            return Err(invalid(
                "an archived task cannot accept a planning suggestion",
            ));
        }
        if agent_engine != "r_code" {
            return Err(invalid(
                "accepting a planning suggestion requires the R-Code main Agent",
            ));
        }
        let active_plan: Option<String> = tx
            .query_row(
                "SELECT id FROM plans WHERE task_id = ?1 AND state NOT IN ('completed', 'cancelled')",
                params![offer.task_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        if active_plan.is_some() {
            return Err(invalid("task already has an active Plan"));
        }
        let branch_is_active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM session_branches \
                 WHERE task_id = ?1 AND id = ?2 AND is_active = 1)",
                params![offer.task_id, offer.branch_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        if !branch_is_active {
            return Err(invalid("accepting requires the active session branch"));
        }

        let plan_id = uuid::Uuid::new_v4().to_string();
        let projection_path = projection_path_for_plan(&plan_id)?;
        let profile_json = serde_json::to_string(&offer.resolved_plan_runtime_profile)
            .map_err(|error| invalid(format!("serialize plan runtime profile: {error}")))?;
        let catalog_phase = match (
            offer.resolved_plan_runtime_profile.enabled,
            offer.resolved_plan_runtime_profile.catalog_profile,
        ) {
            (true, r_code_core::plan_entry::PlanCatalogProfile::PlanNativeV1) => Some("bootstrap"),
            _ => None,
        };
        tx.execute(
            "INSERT INTO plans (id, task_id, revision, state, projection_path, created_at, updated_at, \
             runtime_profile_json, catalog_phase, profile_version) \
             VALUES (?1, ?2, 1, 'draft', ?3, ?4, ?4, ?5, ?6, ?7)",
            params![
                plan_id,
                offer.task_id,
                projection_path,
                now,
                profile_json,
                catalog_phase,
                offer.resolved_plan_runtime_profile.profile_version as i64,
            ],
        )
        .map_err(db_err)?;

        let queue_id = accept_queue_id(&offer.id);
        tx.execute(
            "INSERT INTO queued_messages \
             (id, task_id, branch_id, message, state, priority, created_at, updated_at, request_key) \
             VALUES (?1, ?2, ?3, ?4, 'queued', 1000000, ?5, ?5, ?6)",
            params![
                queue_id,
                offer.task_id,
                offer.branch_id,
                PLAN_ENTRY_ACCEPT_CONTINUATION,
                now,
                offer.request_key,
            ],
        )
        .map_err(db_err)?;
        let changed = tx
            .execute(
                "UPDATE tasks SET mode = 'plan', updated_at = ?1 \
                 WHERE id = ?2 AND mode IN ('ask', 'edit', 'auto')",
                params![now, offer.task_id],
            )
            .map_err(db_err)?;
        if changed != 1 {
            return Err(invalid(
                "task mode changed while the suggestion was being accepted",
            ));
        }
        let operation_id = continuation_operation_id(&offer.id, input.decision, offer.revision);
        update_decided_offer(
            tx,
            &offer.id,
            &r_code_core::plan_entry::PlanEntryOfferState::Accepted,
            Some(input.decision),
            Some(&plan_id),
            PlanEntryContinuationState::Queued,
            &operation_id,
            input,
            now,
            "accepted",
        )?;
        load_offer(tx, &offer.id)?.ok_or_else(|| invalid("offer vanished after accept"))
    }

    fn decline_inside_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        offer: PlanEntryOffer,
        input: &PlanEntryDecisionInput,
        source: PlanEntryDecisionSource,
        now: &str,
    ) -> Result<PlanEntryOffer, ProductError> {
        // 拒绝/关闭/Escape 都持久写入 branch quiet；应用重启不能恢复建议预算。
        tx.execute(
            "INSERT INTO plan_suggestion_branch_states \
             (task_id, branch_id, suggestion_budget_consumed_at, quiet_after_decline, \
              quiet_reason, revision, updated_at) \
             VALUES (?1, ?2, ?3, 1, ?4, 1, ?5) \
             ON CONFLICT(task_id, branch_id) DO UPDATE SET \
              quiet_after_decline = 1, quiet_reason = excluded.quiet_reason, \
              revision = revision + 1, updated_at = excluded.updated_at",
            params![
                offer.task_id,
                offer.branch_id,
                offer.created_at.to_rfc3339(),
                source.as_str(),
                now,
            ],
        )
        .map_err(db_err)?;
        let queue_id = decline_queue_id(&offer.id);
        tx.execute(
            "INSERT INTO queued_messages \
             (id, task_id, branch_id, message, state, priority, created_at, updated_at, request_key) \
             VALUES (?1, ?2, ?3, ?4, 'queued', 1000000, ?5, ?5, ?6)",
            params![
                queue_id,
                offer.task_id,
                offer.branch_id,
                PLAN_ENTRY_DECLINE_CONTINUATION,
                now,
                offer.request_key,
            ],
        )
        .map_err(db_err)?;
        let operation_id = continuation_operation_id(&offer.id, input.decision, offer.revision);
        update_decided_offer(
            tx,
            &offer.id,
            &r_code_core::plan_entry::PlanEntryOfferState::Declined,
            Some(input.decision),
            None,
            PlanEntryContinuationState::Queued,
            &operation_id,
            input,
            now,
            "declined",
        )?;
        load_offer(tx, &offer.id)?.ok_or_else(|| invalid("offer vanished after decline"))
    }

    fn supersede_inside_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        offer: &PlanEntryOffer,
        now: &str,
    ) -> Result<PlanEntryOffer, ProductError> {
        // superseded：不建 Plan、不改模式；原 Run 若仍在等待则幂等续接 Agent。
        let queue_id = superseded_queue_id(&offer.id);
        tx.execute(
            "INSERT OR IGNORE INTO queued_messages \
             (id, task_id, branch_id, message, state, priority, created_at, updated_at, request_key) \
             VALUES (?1, ?2, ?3, ?4, 'queued', 1000000, ?5, ?5, ?6)",
            params![
                queue_id,
                offer.task_id,
                offer.branch_id,
                PLAN_ENTRY_SUPERSEDED_CONTINUATION,
                now,
                offer.request_key,
            ],
        )
        .map_err(db_err)?;
        let operation_id =
            continuation_operation_id(&offer.id, PlanEntryDecisionSource::Close, offer.revision);
        tx.execute(
            "UPDATE plan_entry_offers SET state = 'superseded_provider_changed', \
             continuation_state = 'queued', continuation_operation_id = ?1, error = ?2, \
             revision = revision + 1, updated_at = ?3 WHERE id = ?4 AND state = 'pending'",
            params![
                operation_id,
                "provider route changed before the user decided",
                now,
                offer.id,
            ],
        )
        .map_err(db_err)?;
        load_offer(tx, &offer.id)?.ok_or_else(|| invalid("offer vanished after supersede"))
    }

    /// Provider 设置保存、任务恢复与 accept IPC 共用的 snapshot 比对入口：pending
    /// 且 route 不再匹配的建议 CAS 为 superseded。返回受影响的 offer。
    pub fn supersede_stale_offers(
        &self,
        task_id: &str,
        current_route: &ProviderRouteSnapshot,
    ) -> Result<Vec<PlanEntryOffer>, ProductError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let mut superseded = Vec::new();
        for offer in load_pending_offers_for_task(&tx, task_id)? {
            if &offer.provider != current_route {
                superseded.push(self.supersede_inside_tx(&tx, &offer, &now)?);
            }
        }
        tx.commit().map_err(db_err)?;
        Ok(superseded)
    }

    /// 产品 TTL / 任务归档 / branch 删除路径的过期清理（无副作用，续接 Agent）。
    pub fn expire_pending_offer(
        &self,
        offer_id: &str,
        reason: &str,
    ) -> Result<Option<PlanEntryOffer>, ProductError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let Some(offer) = load_offer(&tx, offer_id)? else {
            return Ok(None);
        };
        if offer.state != PlanEntryOfferState::Pending {
            tx.commit().map_err(db_err)?;
            return Ok(Some(offer));
        }
        let queue_id = superseded_queue_id(&offer.id);
        tx.execute(
            "INSERT OR IGNORE INTO queued_messages \
             (id, task_id, branch_id, message, state, priority, created_at, updated_at, request_key) \
             VALUES (?1, ?2, ?3, ?4, 'queued', 1000000, ?5, ?5, ?6)",
            params![
                queue_id,
                offer.task_id,
                offer.branch_id,
                PLAN_ENTRY_SUPERSEDED_CONTINUATION,
                now,
                offer.request_key,
            ],
        )
        .map_err(db_err)?;
        tx.execute(
            "UPDATE plan_entry_offers SET state = 'expired', error = ?1, revision = revision + 1, \
             updated_at = ?2 WHERE id = ?3 AND state = 'pending'",
            params![reason, now, offer.id],
        )
        .map_err(db_err)?;
        let expired =
            load_offer(&tx, &offer.id)?.ok_or_else(|| invalid("offer vanished after expire"))?;
        tx.commit().map_err(db_err)?;
        Ok(Some(expired))
    }

    /// 续接 claim（queued|failed -> dispatching）。已 claim/dispatched 返回 None，
    /// 防止双派发（与 PlanStore continuation 相同合同）。
    pub fn claim_continuation(
        &self,
        offer_id: &str,
    ) -> Result<Option<PlanEntryOffer>, ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let changed = tx
            .execute(
                "UPDATE plan_entry_offers SET continuation_state = 'dispatching', \
                 updated_at = ?1 WHERE id = ?2 AND continuation_state IN ('queued', 'failed')",
                params![Utc::now().to_rfc3339(), offer_id],
            )
            .map_err(db_err)?;
        let offer = load_offer(&tx, offer_id)?;
        tx.commit().map_err(db_err)?;
        if changed == 1 {
            Ok(offer)
        } else {
            offer
                .filter(|offer| offer.continuation_state == PlanEntryContinuationState::Dispatching)
                .map(Some)
                .ok_or_else(|| invalid("plan entry offer has no continuation to claim"))
        }
    }

    pub fn mark_continuation_dispatched(&self, offer_id: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE plan_entry_offers SET continuation_state = 'sent', updated_at = ?1 \
             WHERE id = ?2 AND continuation_state = 'dispatching'",
            params![Utc::now().to_rfc3339(), offer_id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    pub fn mark_continuation_failed(
        &self,
        offer_id: &str,
        error: &str,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE plan_entry_offers SET continuation_state = 'failed', error = ?1, \
             updated_at = ?2 WHERE id = ?3 AND continuation_state = 'dispatching'",
            params![error, Utc::now().to_rfc3339(), offer_id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 显式重试：failed -> queued。重试复用同一 continuation operation ID。
    pub fn retry_continuation(&self, offer_id: &str) -> Result<PlanEntryOffer, ProductError> {
        {
            let conn = self.db.conn()?;
            conn.execute(
                "UPDATE plan_entry_offers SET continuation_state = 'queued', error = NULL, \
                 updated_at = ?1 WHERE id = ?2 AND continuation_state = 'failed'",
                params![Utc::now().to_rfc3339(), offer_id],
            )
            .map_err(db_err)?;
        }
        self.get_offer(offer_id)?
            .filter(|offer| offer.continuation_state == PlanEntryContinuationState::Queued)
            .ok_or_else(|| invalid("plan entry offer continuation is not retryable"))
    }

    /// 启动恢复：已决定但续接未达 durable 确认（queued|dispatching）的 offer 标记
    /// failed，交由显式重试；不能静默重发（at-least-once / manual-retry 合同）。
    pub fn recover_interrupted_continuations(&self) -> Result<u64, ProductError> {
        let conn = self.db.conn()?;
        let changed = conn
            .execute(
                "UPDATE plan_entry_offers SET continuation_state = 'failed', error = ?1 \
                 WHERE state != 'pending' AND continuation_state IN ('queued', 'dispatching')",
                params![PLAN_ENTRY_CONTINUATION_INTERRUPTED],
            )
            .map_err(db_err)?;
        u64::try_from(changed)
            .map_err(|_| ProductError::DatabaseError("recovery count overflow".to_string()))
    }

    /// 已决定 offer 的续接待处理清单（宿主启动/轮询派发用）。
    pub fn list_continuations_pending(&self) -> Result<Vec<PlanEntryOffer>, ProductError> {
        let conn = self.db.conn()?;
        let mut statement = conn
            .prepare(
                "SELECT id FROM plan_entry_offers \
                 WHERE state != 'pending' AND continuation_state IN ('queued', 'failed')",
            )
            .map_err(db_err)?;
        let ids: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        let mut offers = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Some(offer) = load_offer(&conn, id)? {
                offers.push(offer);
            }
        }
        Ok(offers)
    }
}

fn accept_queue_id(offer_id: &str) -> String {
    format!("plan-entry-offer:{offer_id}:accept")
}

fn decline_queue_id(offer_id: &str) -> String {
    format!("plan-entry-offer:{offer_id}:decline")
}

fn superseded_queue_id(offer_id: &str) -> String {
    format!("plan-entry-offer:{offer_id}:resume")
}

/// 确定性续接 operation ID：(offer_id, decision, revision)（docs §12.4 第 1 步）。
fn continuation_operation_id(
    offer_id: &str,
    decision: PlanEntryDecisionSource,
    revision: u64,
) -> String {
    format!("plan-entry-op:{offer_id}:{}:{revision}", decision.as_str())
}

#[allow(clippy::too_many_arguments)]
fn update_decided_offer(
    tx: &rusqlite::Transaction<'_>,
    offer_id: &str,
    state: &r_code_core::plan_entry::PlanEntryOfferState,
    decision: Option<PlanEntryDecisionSource>,
    plan_id: Option<&str>,
    continuation_state: PlanEntryContinuationState,
    operation_id: &str,
    input: &PlanEntryDecisionInput,
    now: &str,
    _label: &str,
) -> Result<(), ProductError> {
    tx.execute(
        "UPDATE plan_entry_offers SET state = ?1, decision = ?2, plan_id = ?3, \
         continuation_state = ?4, continuation_operation_id = ?5, \
         decision_idempotency_key = ?6, revision = revision + 1, updated_at = ?7, \
         decided_at = ?7 WHERE id = ?8 AND state = 'pending'",
        params![
            state.as_str(),
            decision.map(|decision| decision.as_str()),
            plan_id,
            continuation_state.as_str(),
            operation_id,
            input.idempotency_key.trim(),
            now,
            offer_id,
        ],
    )
    .map_err(db_err)?;
    Ok(())
}

fn load_branch_state(
    conn: &Connection,
    task_id: &str,
    branch_id: &str,
) -> Result<Option<PlanSuggestionBranchState>, ProductError> {
    let row = conn
        .query_row(
            "SELECT suggestion_budget_consumed_at, quiet_after_decline, quiet_reason, revision, \
             updated_at FROM plan_suggestion_branch_states WHERE task_id = ?1 AND branch_id = ?2",
            params![task_id, branch_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()
        .map_err(db_err)?;
    let Some((budget_at, quiet, quiet_reason, revision, updated_at)) = row else {
        return Ok(None);
    };
    Ok(Some(PlanSuggestionBranchState {
        task_id: task_id.to_string(),
        branch_id: branch_id.to_string(),
        suggestion_budget_consumed_at: budget_at.as_deref().map(parse_ts).transpose()?,
        quiet_after_decline: quiet != 0,
        quiet_reason: quiet_reason
            .as_deref()
            .and_then(PlanEntryDecisionSource::try_from_str),
        revision: revision as u64,
        updated_at: parse_ts(&updated_at)?,
    }))
}

fn load_pending_offers_for_task(
    conn: &Connection,
    task_id: &str,
) -> Result<Vec<PlanEntryOffer>, ProductError> {
    let mut statement = conn
        .prepare("SELECT id FROM plan_entry_offers WHERE task_id = ?1 AND state = 'pending'")
        .map_err(db_err)?;
    let ids: Vec<String> = statement
        .query_map(params![task_id], |row| row.get(0))
        .map_err(db_err)?
        .collect::<Result<_, _>>()
        .map_err(db_err)?;
    let mut offers = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(offer) = load_offer(conn, id.as_str())? {
            offers.push(offer);
        }
    }
    Ok(offers)
}

fn load_offer(conn: &Connection, offer_id: &str) -> Result<Option<PlanEntryOffer>, ProductError> {
    let row = conn
        .query_row(
            "SELECT id, task_id, branch_id, source_run_id, request_key, original_mode, \
             reason_audit, signals_json, primary_signal, customer_copy_key, customer_copy_version, \
             provider_kind, provider_profile_id, provider_profile_version, provider_route_revision, \
             model_id, wire_protocol, endpoint_class, eligibility_profile_version, evidence_version, \
             resolved_plan_runtime_profile_json, revision, state, decision, plan_id, \
             continuation_state, continuation_operation_id, error, decision_idempotency_key, \
             created_at, updated_at, decided_at \
             FROM plan_entry_offers WHERE id = ?1",
            params![offer_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, String>(20)?,
                    row.get::<_, i64>(21)?,
                    row.get::<_, String>(22)?,
                    row.get::<_, Option<String>>(23)?,
                    row.get::<_, Option<String>>(24)?,
                    row.get::<_, String>(25)?,
                    row.get::<_, Option<String>>(26)?,
                    row.get::<_, Option<String>>(27)?,
                    row.get::<_, Option<String>>(28)?,
                    row.get::<_, String>(29)?,
                    row.get::<_, String>(30)?,
                    row.get::<_, Option<String>>(31)?,
                ))
            },
        )
        .optional()
        .map_err(db_err)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let signals: Vec<PlanComplexitySignal> = serde_json::from_str(&row.7)
        .map_err(|error| ProductError::DatabaseError(format!("invalid offer signals: {error}")))?;
    let profile: ResolvedPlanRuntimeProfile = serde_json::from_str(&row.20).map_err(|error| {
        ProductError::DatabaseError(format!("invalid offer plan runtime profile: {error}"))
    })?;
    let state = PlanEntryOfferState::try_from_str(&row.22).ok_or_else(|| {
        ProductError::DatabaseError(format!("invalid plan entry offer state: {}", row.22))
    })?;
    let primary_signal = PlanComplexitySignal::try_from_str(&row.8).ok_or_else(|| {
        ProductError::DatabaseError(format!("invalid plan entry offer signal: {}", row.8))
    })?;
    let continuation_state =
        PlanEntryContinuationState::try_from_str(&row.25).ok_or_else(|| {
            ProductError::DatabaseError(format!(
                "invalid plan entry offer continuation state: {}",
                row.25
            ))
        })?;
    Ok(Some(PlanEntryOffer {
        id: row.0,
        task_id: row.1,
        branch_id: row.2,
        source_run_id: row.3,
        request_key: row.4,
        original_mode: row.5,
        reason_audit: row.6,
        signals,
        primary_signal,
        customer_copy_key: row.9,
        customer_copy_version: row.10 as u32,
        provider: ProviderRouteSnapshot {
            provider_kind: row.11,
            provider_profile_id: row.12,
            provider_profile_version: row.13,
            provider_route_revision: row.14,
            model_id: row.15,
            wire_protocol: row.16,
            endpoint_class: row.17,
        },
        eligibility_profile_version: row.18,
        evidence_version: row.19,
        resolved_plan_runtime_profile: profile,
        revision: row.21 as u64,
        state,
        decision: row
            .23
            .as_deref()
            .and_then(PlanEntryDecisionSource::try_from_str),
        plan_id: row.24,
        continuation_state,
        continuation_operation_id: row.26,
        error: row.27,
        decision_idempotency_key: row.28,
        created_at: parse_ts(&row.29)?,
        updated_at: parse_ts(&row.30)?,
        decided_at: row.31.as_deref().map(parse_ts).transpose()?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionBranchRepository, TaskRepository};
    use r_code_core::dto::{Task, TaskMode};
    use r_code_core::plan_entry::{
        OriginRequestKind, PlanEntryDecisionInput, PlanEntryDecisionSource,
    };

    fn setup() -> (tempfile::TempDir, Arc<Database>, PlanEntryStore) {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = PlanEntryStore::new(db.clone());
        (temp, db, store)
    }

    fn task_in_mode(mode: TaskMode) -> Task {
        Task::new(
            Some("C:\\workspace".to_string()),
            "Agent",
            "Refactor across subsystems",
            mode,
        )
    }

    fn prepare(db: &Arc<Database>, mode: TaskMode) -> (Task, String) {
        let task = task_in_mode(mode);
        TaskRepository::new(db).create(&task).unwrap();
        let branch = SessionBranchRepository::new(db)
            .ensure_active(&task.id)
            .unwrap();
        (task, branch.id)
    }

    fn offer_input(task_id: &str, branch_id: &str, request_key: &str) -> CreatePlanEntryOfferInput {
        CreatePlanEntryOfferInput {
            task_id: task_id.to_string(),
            branch_id: branch_id.to_string(),
            source_run_id: "run-1".to_string(),
            request_key: request_key.to_string(),
            original_mode: "edit".to_string(),
            reason_audit: "spans parser, storage and UI".to_string(),
            signals: vec![PlanComplexitySignal::MultiSubsystem],
            primary_signal: PlanComplexitySignal::MultiSubsystem,
            customer_copy_key: "multi_subsystem".to_string(),
            customer_copy_version: 1,
            provider: ProviderRouteSnapshot {
                provider_kind: "deepseek".to_string(),
                provider_profile_id: "DeepSeek".to_string(),
                provider_profile_version: "1".to_string(),
                provider_route_revision: "rev-1".to_string(),
                model_id: "deepseek-v4-flash".to_string(),
                wire_protocol: "openai_chat".to_string(),
                endpoint_class: "official_api".to_string(),
            },
            eligibility_profile_version: "deepseek-plan-v1".to_string(),
            evidence_version: "test".to_string(),
            resolved_plan_runtime_profile: ResolvedPlanRuntimeProfile {
                enabled: true,
                catalog_profile: r_code_core::plan_entry::PlanCatalogProfile::PlanNativeV1,
                context_profile: r_code_core::plan_entry::PlanContextProfile::MinimalV1,
                profile_version: 1,
                evidence_version: "test".to_string(),
                provider_kind: "deepseek".to_string(),
                model_id: "deepseek-v4-flash".to_string(),
                endpoint_class: "official_api".to_string(),
            },
        }
    }

    fn envelope_for(request_key: &str) -> OriginRequestEnvelope {
        OriginRequestEnvelope {
            request_key: request_key.to_string(),
            kind: OriginRequestKind::Direct,
            parent_request_key: None,
            operation_id: format!("op-{request_key}"),
            task_id: "t".to_string(),
            branch_id: "b".to_string(),
            created_at: Utc::now(),
        }
    }

    fn projection_stub(_plan_id: &str) -> Result<String, ProductError> {
        Ok(format!("plans/{_plan_id}/plan.md"))
    }

    fn decide_input(
        offer: &PlanEntryOffer,
        decision: PlanEntryDecisionSource,
    ) -> PlanEntryDecisionInput {
        PlanEntryDecisionInput {
            offer_id: offer.id.clone(),
            expected_revision: offer.revision,
            decision,
            idempotency_key: format!("ui-{}", offer.id),
        }
    }

    #[test]
    fn create_offer_consumes_branch_budget_and_dedups_request_key() {
        let (_temp, db, store) = setup();
        let (task, branch_id) = prepare(&db, TaskMode::Edit);
        store.record_origin_request(&envelope_for("key-1")).unwrap();

        let offer = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-1"))
            .unwrap();
        assert_eq!(offer.state, PlanEntryOfferState::Pending);
        assert_eq!(offer.revision, 1);

        // 同 request key 永远最多一个 offer。
        let err = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-1"))
            .unwrap_err();
        assert!(err.to_string().contains("request key dedup"));

        // 预算已在创建事务中消耗：新请求键也不能再创建第二个建议（pending 仍在
        // 时先被「同任务至多一个 pending」挡下；两者都是 fail closed）。
        let err = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-2"))
            .unwrap_err();
        assert!(err.to_string().contains("planning suggestion"));

        let branch_state = store.branch_state(&task.id, &branch_id).unwrap();
        assert!(branch_state.suggestion_budget_consumed_at.is_some());
        assert!(!branch_state.quiet_after_decline);
    }

    #[test]
    fn pending_offer_does_not_touch_task_mode_or_plans() {
        let (_temp, db, store) = setup();
        let (task, branch_id) = prepare(&db, TaskMode::Edit);
        store
            .create_offer(&offer_input(&task.id, &branch_id, "key-1"))
            .unwrap();
        // pending 建议不修改 tasks.mode、不创建 plans 行（docs §11）。
        assert_eq!(
            TaskRepository::new(&db)
                .get(&task.id)
                .unwrap()
                .unwrap()
                .mode,
            TaskMode::Edit
        );
        let plans: i64 = db
            .conn()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
            .unwrap();
        assert_eq!(plans, 0);
    }

    #[test]
    fn accept_creates_plan_switches_mode_and_queues_continuation_atomically() {
        let (temp, db, store) = setup();
        let (task, branch_id) = prepare(&db, TaskMode::Edit);
        let offer = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-1"))
            .unwrap();

        let outcome = store
            .decide(
                &decide_input(&offer, PlanEntryDecisionSource::Accept),
                &offer.provider,
                projection_stub,
            )
            .unwrap();
        let accepted = match outcome {
            PlanEntryDecisionOutcome::Accepted(offer) => offer,
            other => panic!("expected accept, got {other:?}"),
        };
        assert_eq!(accepted.state, PlanEntryOfferState::Accepted);
        let plan_id = accepted.plan_id.clone().unwrap();

        assert_eq!(
            TaskRepository::new(&db)
                .get(&task.id)
                .unwrap()
                .unwrap()
                .mode,
            TaskMode::Plan
        );
        // 续接队列行存在且继承原请求键。
        let queued = crate::QueuedMessageRepository::new(&db)
            .list_pending(&task.id, &branch_id)
            .unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].request_key.as_deref(), Some("key-1"));
        assert!(queued[0].id.starts_with("plan-entry-offer:"));
        assert_eq!(queued[0].message, PLAN_ENTRY_ACCEPT_CONTINUATION);

        // Plan 行携带冻结 profile 与 bootstrap phase。
        let plans = crate::PlanStore::new(db.clone(), temp.path().join("plans"));
        let view = plans.current_for_task(&task.id).unwrap().unwrap();
        assert_eq!(view.plan.id, plan_id);
        let profile = view.plan.runtime_profile.unwrap();
        assert!(profile.enabled);
        assert_eq!(
            profile.catalog_profile,
            r_code_core::plan_entry::PlanCatalogProfile::PlanNativeV1
        );
        assert_eq!(
            view.plan.catalog_phase,
            Some(r_code_core::plan_entry::PlanCatalogPhase::Bootstrap)
        );

        // 同幂等键重复提交是 Replay，不产生第二个效果。
        let replay = store
            .decide(
                &decide_input(&offer, PlanEntryDecisionSource::Accept),
                &offer.provider,
                projection_stub,
            )
            .unwrap();
        assert!(matches!(replay, PlanEntryDecisionOutcome::Replay(_)));
    }

    #[test]
    fn accept_with_changed_route_supersedes_without_plan_side_effects() {
        let (temp, db, store) = setup();
        let (task, branch_id) = prepare(&db, TaskMode::Edit);
        let offer = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-1"))
            .unwrap();
        let mut changed = offer.provider.clone();
        changed.model_id = "some-other-model".to_string();

        let outcome = store
            .decide(
                &decide_input(&offer, PlanEntryDecisionSource::Accept),
                &changed,
                projection_stub,
            )
            .unwrap();
        match outcome {
            PlanEntryDecisionOutcome::Superseded(superseded) => {
                assert_eq!(
                    superseded.state,
                    PlanEntryOfferState::SupersededProviderChanged
                );
                assert!(superseded.plan_id.is_none());
            }
            other => panic!("expected superseded, got {other:?}"),
        }
        // 零 Plan 副作用：任务模式与 plans 行都未变化，但幂等续接已排队。
        assert_eq!(
            TaskRepository::new(&db)
                .get(&task.id)
                .unwrap()
                .unwrap()
                .mode,
            TaskMode::Edit
        );
        let plans = crate::PlanStore::new(db.clone(), temp.path().join("plans"));
        assert!(plans.current_for_task(&task.id).unwrap().is_none());
        let queued = crate::QueuedMessageRepository::new(&db)
            .list_pending(&task.id, &branch_id)
            .unwrap();
        assert_eq!(queued.len(), 1);
    }

    #[test]
    fn decline_marks_branch_persistently_quiet_and_queues_agent_continuation() {
        let (_temp, db, store) = setup();
        let (task, branch_id) = prepare(&db, TaskMode::Auto);
        let offer = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-1"))
            .unwrap();

        let outcome = store
            .decide(
                &decide_input(&offer, PlanEntryDecisionSource::Escape),
                &offer.provider,
                projection_stub,
            )
            .unwrap();
        match outcome {
            PlanEntryDecisionOutcome::Declined(declined) => {
                assert_eq!(declined.state, PlanEntryOfferState::Declined);
                assert_eq!(declined.decision, Some(PlanEntryDecisionSource::Escape));
            }
            other => panic!("expected decline, got {other:?}"),
        }
        // 保持原 tasks.mode；branch 持久 quiet。
        assert_eq!(
            TaskRepository::new(&db)
                .get(&task.id)
                .unwrap()
                .unwrap()
                .mode,
            TaskMode::Auto
        );
        let branch_state = store.branch_state(&task.id, &branch_id).unwrap();
        assert!(branch_state.quiet_after_decline);
        assert_eq!(
            branch_state.quiet_reason,
            Some(PlanEntryDecisionSource::Escape)
        );
        // 拒绝后：新请求键（真实新消息）仍不能创建建议——quiet 优先。
        let err = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-2"))
            .unwrap_err();
        assert!(err.to_string().contains("quiet"));
    }

    #[test]
    fn stale_revision_decision_is_rejected_by_cas() {
        let (_temp, db, store) = setup();
        let (task, branch_id) = prepare(&db, TaskMode::Edit);
        let offer = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-1"))
            .unwrap();
        let mut stale = decide_input(&offer, PlanEntryDecisionSource::Accept);
        stale.expected_revision = 99;
        let error = store
            .decide(&stale, &offer.provider, projection_stub)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("stale plan entry offer revision"));
    }

    #[test]
    fn supersede_stale_offers_only_touches_mismatched_routes() {
        let (_temp, db, store) = setup();
        let (task, branch_id) = prepare(&db, TaskMode::Edit);
        let offer = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-1"))
            .unwrap();
        // route 未变：不 supersede。
        assert!(store
            .supersede_stale_offers(&task.id, &offer.provider)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .pending_offer_for_task(&task.id)
                .unwrap()
                .unwrap()
                .state,
            PlanEntryOfferState::Pending
        );
        // route 变化：supersede 并保留预算消耗（不会再次弹阻断建议）。
        let mut changed = offer.provider.clone();
        changed.provider_profile_id = "Relay".to_string();
        let superseded = store.supersede_stale_offers(&task.id, &changed).unwrap();
        assert_eq!(superseded.len(), 1);
        assert!(store.pending_offer_for_task(&task.id).unwrap().is_none());
        let branch_state = store.branch_state(&task.id, &branch_id).unwrap();
        assert!(branch_state.suggestion_budget_consumed_at.is_some());
    }

    #[test]
    fn interrupted_continuations_are_marked_failed_for_explicit_retry() {
        let (temp, db, store) = setup();
        let (task, branch_id) = prepare(&db, TaskMode::Edit);
        let offer = store
            .create_offer(&offer_input(&task.id, &branch_id, "key-1"))
            .unwrap();
        store
            .decide(
                &decide_input(&offer, PlanEntryDecisionSource::Accept),
                &offer.provider,
                projection_stub,
            )
            .unwrap();
        // 模拟崩溃窗口：决定已落库但续接未确认。重启恢复标记 failed。
        let recovered = store.recover_interrupted_continuations().unwrap();
        assert_eq!(recovered, 1);
        let pending_retry = store.list_continuations_pending().unwrap();
        assert_eq!(pending_retry.len(), 1);
        assert_eq!(
            pending_retry[0].continuation_state,
            PlanEntryContinuationState::Failed
        );
        // 显式重试复用同一确定性 operation ID。
        let retried = store.retry_continuation(&pending_retry[0].id).unwrap();
        assert!(retried
            .continuation_operation_id
            .as_deref()
            .unwrap()
            .contains("plan-entry-op"));
        let _ = temp.keep();
    }
}
