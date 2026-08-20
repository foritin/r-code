//! Plan 入口建议的宿主运行时接线与 IPC（docs/plan-mode-dual-track-gate.md
//! §9–§12）。
//!
//! 职责：
//! - `PlanningRuntimeState`：run 启动时的资格解析（工具+提示注册门）、run →
//!   origin request key 登记、Provider snapshot 比对与 supersede；
//! - 决定 IPC：accept/decline 的视图组装与续接派发（at-least-once + 手动重试）；
//! - 启动恢复：未确认续接标记 failed、Provider 变化的 pending 建议 superseded。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use r_code_agent_worker::{LlmAgentRuntime, PlanNativeCatalogConfig, PlanNativeCatalogPhase};
use r_code_core::dto::{AgentEngine, Task, TaskMode};
use r_code_core::plan_entry::{
    OriginRequestEnvelope, OriginRequestKind, PlanEntryDecisionInput, PlanEntryOffer,
    PlanEntryOfferState, ResolvedPlanRuntimeProfile,
};
use r_code_store::{Database, PlanEntryStore, PlanStore};
use serde::{Deserialize, Serialize};

use crate::plan_policy::{
    customer_copy_template, provider_route_snapshot, resolve_plan_entry_eligibility,
    resolve_plan_runtime_profile, resolve_release_control, ArmedPlanSuggestion, EndpointClass,
    PlanSuggestionGate, PlanningReleaseControl, ProviderRouteContext, CUSTOMER_COPY_SUFFIX,
    DECLINE_QUIET_NOTE, PROVIDER_PROFILE_VERSION, SUPERSEDED_NOTICE,
};

/// run_id → 当前 origin request key 的宿主登记。gateway 经 resolver 读取并写入
/// `ToolExecutionContext::origin_request_key`。键为唯一 UUID，绑定后过期条目按
/// 上限裁剪（活跃 run 永远在最近窗口内）。
#[derive(Default)]
pub struct RunOriginRegistry {
    inner: Mutex<RunOriginInner>,
}

#[derive(Default)]
struct RunOriginInner {
    map: HashMap<String, String>,
    order: Vec<String>,
}

const RUN_ORIGIN_RETAIN: usize = 64;

impl RunOriginRegistry {
    pub fn bind(&self, run_id: &str, request_key: &str) {
        let mut inner = self.inner.lock().expect("run origin registry poisoned");
        if !inner.map.contains_key(run_id) {
            inner.order.push(run_id.to_string());
        }
        inner
            .map
            .insert(run_id.to_string(), request_key.to_string());
        while inner.order.len() > RUN_ORIGIN_RETAIN {
            let evicted = inner.order.remove(0);
            inner.map.remove(&evicted);
        }
    }

    pub fn current(&self, run_id: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("run origin registry poisoned")
            .map
            .get(run_id)
            .cloned()
    }
}

/// Plan 建议与双轨的宿主运行时状态（挂在 `CommandState` 上，全进程共享）。
pub struct PlanningRuntimeState {
    pub db: Arc<Database>,
    pub plan_entry: Arc<PlanEntryStore>,
    pub plan_store: Arc<PlanStore>,
    pub suggestion_gate: Arc<PlanSuggestionGate>,
    pub run_origins: Arc<RunOriginRegistry>,
    pub config_dir: PathBuf,
    /// 进程内解析一次；emergency off 环境变量在启动时读取。
    pub release_control: PlanningReleaseControl,
}

impl PlanningRuntimeState {
    pub fn new(db: Arc<Database>, plan_store: Arc<PlanStore>, config_dir: PathBuf) -> Self {
        Self {
            plan_entry: Arc::new(PlanEntryStore::new(db.clone())),
            db,
            plan_store,
            suggestion_gate: Arc::new(PlanSuggestionGate::default()),
            run_origins: Arc::new(RunOriginRegistry::default()),
            config_dir,
            release_control: resolve_release_control(),
        }
    }

    /// 任务绑定的 Provider route 上下文（非秘密字段）。任务未显式绑定服务时使用
    /// 全局默认 Provider（与 ensure_real_runtime 的解析规则一致）。
    pub fn route_context_for_task(&self, task: &Task) -> Result<ProviderRouteContext, String> {
        let settings = crate::settings::SettingsService::new(self.config_dir.clone());
        let config = settings
            .load_global_unvalidated()
            .map_err(|error| error.to_string())?;
        let provider_name = task
            .provider_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| config.default_provider.clone());
        let provider = config
            .providers
            .get(&provider_name)
            .ok_or_else(|| format!("未找到模型服务“{provider_name}”"))?;
        Ok(ProviderRouteContext {
            provider_kind: provider.provider_kind.clone().unwrap_or_default(),
            model: provider.model.clone(),
            wire_protocol: crate::commands::resolve_effective_protocol(&provider_name, provider)
                .as_str()
                .to_string(),
            provider_name: provider_name.clone(),
            endpoint_class: EndpointClass::classify(&provider.base_url),
        })
    }

    /// 为即将启动的 run 解析建议资格（docs §9 的全部前置条件）。返回 None 时，
    /// 工具与提示同时缺席（worker 侧 plan_suggestion_enabled=false）。
    pub fn resolve_suggestion_for_run(
        &self,
        task: &Task,
        branch_id: &str,
        _request_key: &str,
    ) -> Result<Option<ArmedPlanSuggestion>, String> {
        // 主运行时是 R-Code；Plan 模式 / Codex / 子 Agent 都不注册建议工具。
        if task.agent_engine != AgentEngine::RCode {
            return Ok(None);
        }
        if !matches!(task.mode, TaskMode::Ask | TaskMode::Edit | TaskMode::Auto) {
            return Ok(None);
        }
        // 客户开关（docs §15.1）：关闭后不再注册 propose_plan_mode。
        let settings = crate::settings::SettingsService::new(self.config_dir.clone());
        let config = settings
            .load_global_unvalidated()
            .map_err(|error| error.to_string())?;
        if !config.planning.suggest_complex_tasks {
            return Ok(None);
        }
        // 当前任务绑定 R-Code workspace。
        if task
            .workspace_path
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            return Ok(None);
        }
        // 冻结 route 的资格：只认稳定 provider_kind 与证据 allowlist。
        let route = self.route_context_for_task(task)?;
        let eligibility = resolve_plan_entry_eligibility(&route, &self.release_control);
        if !eligibility.eligible {
            tracing::debug!(
                task_id = %task.id,
                reason = eligibility.blocked_reason.as_deref().unwrap_or("ineligible"),
                "plan suggestion not registered for run"
            );
            return Ok(None);
        }
        // branch 预算与安静状态：拒绝后同 branch 持久安静（docs §6.2）。
        let branch_state = self
            .plan_entry
            .branch_state(&task.id, branch_id)
            .map_err(|error| error.to_string())?;
        if !branch_state.can_suggest() {
            return Ok(None);
        }
        // 同 request key 至多一个 offer；同任务至多一个 pending（docs §6.2）。
        if self
            .plan_entry
            .pending_offer_for_task(&task.id)
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Ok(None);
        }
        Ok(Some(ArmedPlanSuggestion {
            profile: resolve_plan_runtime_profile(&route, &self.release_control, true),
            route,
            control: self.release_control.clone(),
        }))
    }

    /// 任务的冻结 Plan profile（EnterPlanModeTool / plan_create IPC 用）。
    pub fn resolve_profile_for_task_id(&self, task_id: &str) -> ResolvedPlanRuntimeProfile {
        let Ok(Some(task)) = TaskRepositoryBridge::get(&self.db, task_id) else {
            return ResolvedPlanRuntimeProfile::baseline();
        };
        match self.route_context_for_task(&task) {
            Ok(route) => resolve_plan_runtime_profile(
                &route,
                &self.release_control,
                task.workspace_path
                    .as_deref()
                    .is_some_and(|path| !path.trim().is_empty()),
            ),
            Err(_) => ResolvedPlanRuntimeProfile::baseline(),
        }
    }

    /// Plan 原生目录配置（权威 phase 来自 plans.catalog_phase）。
    pub fn plan_native_config_for_task(&self, task_id: &str) -> Option<PlanNativeCatalogConfig> {
        let (profile, phase) = self
            .plan_store
            .current_runtime_profile_for_task(task_id)
            .ok()
            .flatten()?;
        if !profile.enabled {
            return None;
        }
        Some(PlanNativeCatalogConfig {
            phase: match phase {
                r_code_core::plan_entry::PlanCatalogPhase::Bootstrap => {
                    PlanNativeCatalogPhase::Bootstrap
                }
                r_code_core::plan_entry::PlanCatalogPhase::Resident => {
                    PlanNativeCatalogPhase::Resident
                }
            },
        })
    }

    /// run 启动后的会话级配置注入（资格通过才注册工具；Plan 任务传入原生目录）。
    /// 返回解析出的建议武装结果，调用方复用同一结果登记 gate（一次解析两处使用）。
    pub async fn prepare_runtime_session(
        &self,
        runtime: &mut LlmAgentRuntime,
        runtime_session_id: &str,
        task: &Task,
        branch_id: &str,
        request_key: &str,
    ) -> Option<ArmedPlanSuggestion> {
        let armed = self
            .resolve_suggestion_for_run(task, branch_id, request_key)
            .unwrap_or(None);
        runtime
            .update_plan_entry_suggestion(runtime_session_id, armed.is_some())
            .await;
        if task.mode == TaskMode::Plan {
            let config = self.plan_native_config_for_task(&task.id);
            runtime
                .update_plan_native_catalog(runtime_session_id, config)
                .await;
        } else {
            runtime
                .update_plan_native_catalog(runtime_session_id, None)
                .await;
        }
        armed
    }

    /// 启动恢复与 Provider 设置保存共用的 supersede 入口：pending 且 route 不再
    /// 匹配的建议转 superseded（docs §12.3）。
    pub fn reconcile_pending_offers(&self) -> Result<Vec<PlanEntryOffer>, String> {
        let pending = self
            .plan_entry
            .list_pending_offers()
            .map_err(|error| error.to_string())?;
        let mut superseded = Vec::new();
        let mut by_task: HashMap<String, Vec<PlanEntryOffer>> = HashMap::new();
        for offer in pending {
            by_task
                .entry(offer.task_id.clone())
                .or_default()
                .push(offer);
        }
        for (task_id, offers) in by_task {
            let Some(task) = TaskRepositoryBridge::get(&self.db, &task_id).ok().flatten() else {
                continue;
            };
            let Ok(route) = self.route_context_for_task(&task) else {
                continue;
            };
            let snapshot = provider_route_snapshot(&route, PROVIDER_PROFILE_VERSION);
            if offers.iter().any(|offer| offer.provider != snapshot) {
                if let Ok(mut changed) = self.plan_entry.supersede_stale_offers(&task_id, &snapshot)
                {
                    superseded.append(&mut changed);
                }
            }
        }
        Ok(superseded)
    }

    /// 崩溃窗口恢复：未达 durable 确认的续接标记 failed（显式重试）。
    pub fn recover_interrupted_continuations(&self) -> Result<u64, String> {
        self.plan_entry
            .recover_interrupted_continuations()
            .map_err(|error| error.to_string())
    }
}

/// 只读 Task 读取小帮手（避免在本模块再开一个 repository 实例类型）。
struct TaskRepositoryBridge;

impl TaskRepositoryBridge {
    fn get(db: &Database, task_id: &str) -> Result<Option<Task>, String> {
        r_code_store::TaskRepository::new(db)
            .get(task_id)
            .map_err(|error| error.to_string())
    }
}

/// 客户可见的建议视图。绝不包含模型 reason、signal 枚举、工具/目录/profile/
/// 证据版本等内部词（docs §6.1、§18 验证矩阵「客户弹窗」行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntryCustomerCopyView {
    pub lead: String,
    pub suffix: String,
    pub quiet_note: String,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntryOfferView {
    pub id: String,
    pub task_id: String,
    pub revision: u64,
    pub state: PlanEntryOfferState,
    pub customer_copy: PlanEntryCustomerCopyView,
    /// superseded / expired 的一次性非阻断状态条（docs §4.3、§6.5）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// 续接失败时的内联重试入口（docs §6.5）。
    pub continuation_state: r_code_core::plan_entry::PlanEntryContinuationState,
}

impl PlanEntryOfferView {
    pub fn from_offer(offer: &PlanEntryOffer) -> Self {
        let template = customer_copy_template(offer.primary_signal);
        let notice = match offer.state {
            PlanEntryOfferState::SupersededProviderChanged => Some(SUPERSEDED_NOTICE.to_string()),
            _ => None,
        };
        Self {
            id: offer.id.clone(),
            task_id: offer.task_id.clone(),
            revision: offer.revision,
            state: offer.state,
            customer_copy: PlanEntryCustomerCopyView {
                lead: template.lead.to_string(),
                suffix: CUSTOMER_COPY_SUFFIX.to_string(),
                quiet_note: DECLINE_QUIET_NOTE.to_string(),
                version: offer.customer_copy_version.max(template.version),
            },
            notice,
            continuation_state: offer.continuation_state,
        }
    }
}

/// 供 settings / 诊断页消费的规划发布状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningStatusView {
    pub release_state: String,
    pub emergency_off: bool,
    pub evidence_version: String,
    pub eligibility_profile_version: String,
    /// 客户开关卡片是否可见（默认 Provider 是符合资格的 DeepSeek）。
    pub customer_card_visible: bool,
    /// 证据是否已通过（未通过时客户开关不可启用）。
    pub evidence_validated: bool,
    pub basis: String,
}

pub async fn planning_status(
    state: &crate::commands::CommandState,
) -> Result<PlanningStatusView, String> {
    let planning = state.planning.clone();
    let settings = crate::settings::SettingsService::new(state.config_dir.clone());
    let config = settings
        .load_global_unvalidated()
        .map_err(|error| error.to_string())?;
    let provider_name = config.default_provider.clone();
    let route = config
        .providers
        .get(&provider_name)
        .map(|provider| ProviderRouteContext {
            provider_name,
            provider_kind: provider.provider_kind.clone().unwrap_or_default(),
            model: provider.model.clone(),
            wire_protocol: crate::commands::resolve_effective_protocol(
                &config.default_provider,
                provider,
            )
            .as_str()
            .to_string(),
            endpoint_class: EndpointClass::classify(&provider.base_url),
        });
    let eligibility = route
        .as_ref()
        .map(|route| resolve_plan_entry_eligibility(route, &planning.release_control));
    let customer_card_visible = eligibility
        .as_ref()
        .is_some_and(|eligibility| eligibility.eligible);
    Ok(PlanningStatusView {
        release_state: planning.release_control.release_state.as_str().to_string(),
        emergency_off: planning.release_control.emergency_off,
        evidence_version: planning.release_control.evidence_version.clone(),
        eligibility_profile_version: planning.release_control.eligibility_profile_version.clone(),
        customer_card_visible,
        evidence_validated: planning.release_control.release_state
            == crate::plan_policy::PlanningReleaseState::Validated,
        basis: planning.release_control.basis.clone(),
    })
}

/// 任务详情用的同步视图组装（task_detail 调用）。
pub fn plan_entry_offer_view_for_task(
    planning: &PlanningRuntimeState,
    task_id: &str,
) -> Result<Option<PlanEntryOfferView>, String> {
    let offer = planning
        .plan_entry
        .pending_offer_for_task(task_id)
        .map_err(|error| error.to_string())?;
    Ok(offer.as_ref().map(PlanEntryOfferView::from_offer))
}

pub async fn plan_entry_offer_get(
    state: &crate::commands::CommandState,
    task_id: &str,
) -> Result<Option<PlanEntryOfferView>, String> {
    let offer = state
        .planning
        .plan_entry
        .pending_offer_for_task(task_id)
        .map_err(|error| error.to_string())?;
    Ok(offer.as_ref().map(PlanEntryOfferView::from_offer))
}

/// 决定 IPC（docs §12.1/§12.2）：CAS + 原子事务在 store 内完成；成功后刷新运行
/// 上下文并派发 durable 续接（at-least-once；失败走内联重试）。
pub async fn plan_entry_decide(
    state: &crate::commands::CommandState,
    input: &PlanEntryDecisionInput,
) -> Result<PlanEntryOfferView, String> {
    let planning = state.planning.clone();
    let offer = planning
        .plan_entry
        .get_offer(&input.offer_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("plan entry offer does not exist: {}", input.offer_id))?;
    let task = TaskRepositoryBridge::get(&state.db, &offer.task_id)?
        .ok_or_else(|| format!("task not found: {}", offer.task_id))?;
    let route = planning.route_context_for_task(&task)?;
    let snapshot = provider_route_snapshot(&route, PROVIDER_PROFILE_VERSION);
    let plan_store = planning.plan_store.clone();
    let outcome = planning
        .plan_entry
        .decide(input, &snapshot, |plan_id| {
            plan_store
                .projection_path_for_plan(plan_id)
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|error| r_code_core::error::ProductError::Other(error.to_string()))
        })
        .map_err(|error| error.to_string())?;
    let decided = outcome.offer().clone();
    match &decided.state {
        PlanEntryOfferState::Accepted => {
            // 投影是二级投影：事务提交后重建（失败可由 repair_projection 重试）。
            if let Some(plan_id) = decided.plan_id.as_deref() {
                if let Err(error) = plan_store.repair_projection(&decided.task_id, plan_id) {
                    tracing::warn!(
                        task_id = %decided.task_id,
                        plan_id,
                        "plan entry accept projection repair failed: {error}"
                    );
                }
            }
            let _ = crate::commands::refresh_runtime_task_context_if_present(state, &task).await;
        }
        PlanEntryOfferState::Declined => {
            let _ = crate::commands::refresh_runtime_task_context_if_present(state, &task).await;
        }
        _ => {}
    }
    // durable 续接：queue 行已入队（行 CAS 是唯一派发边界），触发该任务的
    // 队列派发（与 Queue 分支同路径）；失败由 offer 台账的 failed 状态承接
    //（at-least-once + 手动重试，docs §12.4 降级条款）。
    if matches!(
        decided.continuation_state,
        r_code_core::plan_entry::PlanEntryContinuationState::Queued
    ) {
        if let Err(error) = dispatch_plan_entry_continuation(state, &decided.task_id).await {
            tracing::warn!(
                offer_id = %decided.id,
                "plan entry continuation dispatch failed: {error}"
            );
        }
    }
    Ok(PlanEntryOfferView::from_offer(&decided))
}

/// 派发建议续接：复用既有队列派发路径。queue 行自身的 queued→dispatching→sent
/// CAS 保证同一条续接消息不会被重复派发。
pub async fn dispatch_plan_entry_continuation(
    state: &crate::commands::CommandState,
    task_id: &str,
) -> Result<(), String> {
    crate::commands::dispatch_queue_for_task(state, task_id).await;
    Ok(())
}

pub async fn plan_entry_retry_continuation(
    state: &crate::commands::CommandState,
    offer_id: &str,
) -> Result<PlanEntryOfferView, String> {
    let planning = state.planning.clone();
    let offer = planning
        .plan_entry
        .retry_continuation(offer_id)
        .map_err(|error| error.to_string())?;
    dispatch_plan_entry_continuation(state, &offer.task_id).await?;
    Ok(PlanEntryOfferView::from_offer(&offer))
}

/// 统一宿主信封创建（docs §10.1）：在进入 Auto/Queue/SendNow/Steer 分支前生成
/// 并持久化。steer 以持久 operation ID 作为请求身份。
pub fn new_origin_request_envelope(
    planning: &PlanningRuntimeState,
    task_id: &str,
    branch_id: &str,
    kind: OriginRequestKind,
    operation_id: Option<&str>,
    parent_request_key: Option<&str>,
) -> Result<OriginRequestEnvelope, String> {
    let operation_id = operation_id
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let request_key = match kind {
        // 运行中 Steer：使用持久 steer operation ID 作为请求身份。
        OriginRequestKind::Steer => operation_id.clone(),
        _ => uuid::Uuid::new_v4().to_string(),
    };
    let envelope = OriginRequestEnvelope {
        request_key,
        kind,
        parent_request_key: parent_request_key.map(str::to_string),
        operation_id,
        task_id: task_id.to_string(),
        branch_id: branch_id.to_string(),
        created_at: chrono::Utc::now(),
    };
    planning
        .plan_entry
        .record_origin_request(&envelope)
        .map_err(|error| error.to_string())?;
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_origin_registry_binds_and_evicts_old_runs() {
        let registry = RunOriginRegistry::default();
        registry.bind("run-1", "key-1");
        assert_eq!(registry.current("run-1").as_deref(), Some("key-1"));
        for index in 0..RUN_ORIGIN_RETAIN {
            registry.bind(&format!("run-fill-{index}"), "filler");
        }
        assert!(registry.current("run-1").is_none(), "旧 run 条目按上限裁剪");
        assert!(registry
            .current(&format!("run-fill-{}", RUN_ORIGIN_RETAIN - 1))
            .is_some());
    }
}
