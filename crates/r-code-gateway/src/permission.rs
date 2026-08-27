//! Permission Engine -- 风险分级与审批流程。 [doc-02 §2.2, §4]
//!
//! 权限引擎维护两类状态：
//! - **Standing rules**：(task, tool, target) -> Decision 的长期规则，
//!   实现 "Always allow for this task" 语义。R3/R4 不持久化（被拒绝）。
//! - **Pending requests**：等待用户审批的高风险调用请求。
//!
//! ## 风险分级策略
//! | 级别 | 行为 |
//! |------|------|
//! | R0/R1 | 自动允许（只读 / 低风险） |
//! | R2/R3 | 查 standing rule；命中 AllowAlways 则允许，否则创建待审批请求 |
//! | R4    | 前置拒绝 |
//!
//! R3 的 standing rule 不持久化（`add_standing_rule` 返回错误），
//! 对应 `RiskLevel::can_persist_standing() == false`。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use r_code_core::dto::{PermissionDecision, PermissionRequest, ProjectAccessMode, RiskLevel};
use r_code_core::error::ProductError;
use tokio::sync::{broadcast, Mutex};

/// A synchronous cancellation probe evaluated while the permission state lock is held.
///
/// Keeping the probe inside each pending request lets cancellation sources from different
/// runtimes participate in the same atomic terminal transition as an allow/deny decision.
#[derive(Clone)]
pub struct PermissionCancellation {
    probe: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl PermissionCancellation {
    /// Build a probe backed by the run's shared abort flag.
    pub fn from_atomic(flag: Arc<AtomicBool>) -> Self {
        Self::from_probe(move || flag.load(Ordering::Acquire))
    }

    /// Build a probe from another synchronous cancellation primitive.
    pub fn from_probe(probe: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            probe: Arc::new(probe),
        }
    }

    fn is_cancelled(&self) -> bool {
        (self.probe)()
    }
}

/// Permission Engine -- 风险分级与审批流程。
pub struct PermissionEngine {
    /// Pending requests, terminal decisions and standing rules share one lock so a terminal
    /// transition is committed without an intervening `.await` or partial authorization state.
    state: Arc<Mutex<PermissionState>>,
    /// 新待批请求的轻量广播；通知等观察者不能反向改变权限状态。
    requests: broadcast::Sender<PermissionRequest>,
}

#[derive(Default)]
struct PermissionState {
    /// Standing rules: (task_id, tool_name, target) -> Decision。
    standing_rules: HashMap<StandingRuleKey, StandingRule>,
    /// 等待审批的权限请求及其运行生命周期。
    pending_requests: HashMap<String, PendingPermission>,
    /// 最近的审批决策（request_id → 决策与时间），供 `wait_decision` 挂起等待。
    decisions: HashMap<String, (PermissionDecision, std::time::Instant)>,
}

#[derive(Clone)]
struct PendingPermission {
    request: PermissionRequest,
    cancellation: Option<PermissionCancellation>,
    expires_at: Option<std::time::Instant>,
}

impl PendingPermission {
    fn new(
        request: PermissionRequest,
        cancellation: Option<PermissionCancellation>,
        timeout: Option<std::time::Duration>,
    ) -> Self {
        Self {
            request,
            cancellation,
            expires_at: timeout.and_then(|value| std::time::Instant::now().checked_add(value)),
        }
    }

    fn is_valid_at(&self, now: std::time::Instant) -> bool {
        !self
            .cancellation
            .as_ref()
            .is_some_and(PermissionCancellation::is_cancelled)
            && self.expires_at.is_none_or(|deadline| now < deadline)
    }
}

/// 决策暂存的保留时长（超时未查询的决策会被清理）。
const DECISION_RETENTION: std::time::Duration = std::time::Duration::from_secs(600);

/// Standing rule 的键 -- (task_id, tool_name, target terminal)。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StandingRuleKey {
    /// 所属任务 ID
    pub task_id: String,
    /// 工具名称
    pub tool_name: String,
    /// 目标终端 ID（终端工具用）；其他工具为 None
    pub target: Option<String>,
}

/// Standing allow rules carry the highest risk level the user actually approved.
///
/// Keeping the ceiling in the value preserves the public `(task, tool, target)` key while
/// preventing an R2 "always allow" decision from silently authorizing a later R3 invocation of
/// the same dynamic-risk tool (for example `bash`). Deny rules remain fail-closed for every risk
/// level under the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StandingRule {
    decision: PermissionDecision,
    risk_ceiling: RiskLevel,
}

fn risk_rank(level: RiskLevel) -> u8 {
    match level {
        RiskLevel::R0 => 0,
        RiskLevel::R1 => 1,
        RiskLevel::R2 => 2,
        RiskLevel::R3 => 3,
        RiskLevel::R4 => 4,
    }
}

/// 权限检查结果。
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionCheckResult {
    /// 允许（R0/R1 或 standing rule 命中）
    Allowed,
    /// 需要用户审批（携带创建的 PermissionRequest，已存入 pending 队列）
    NeedsApproval(PermissionRequest),
    /// 拒绝（R4 或策略禁止）
    Denied(String),
}

impl PermissionEngine {
    /// 创建空的权限引擎。
    pub fn new() -> Self {
        let (requests, _) = broadcast::channel(128);
        Self {
            state: Arc::new(Mutex::new(PermissionState::default())),
            requests,
        }
    }

    async fn insert_pending(
        &self,
        request: PermissionRequest,
        cancellation: Option<PermissionCancellation>,
        timeout: Option<std::time::Duration>,
    ) {
        self.state.lock().await.pending_requests.insert(
            request.id.clone(),
            PendingPermission::new(request.clone(), cancellation, timeout),
        );
        let _ = self.requests.send(request);
    }

    /// 订阅新创建的待审批请求。慢观察者只会丢通知，不会阻塞权限执行路径。
    pub fn subscribe_requests(&self) -> broadcast::Receiver<PermissionRequest> {
        self.requests.subscribe()
    }

    fn retain_recent_decisions(state: &mut PermissionState) {
        state
            .decisions
            .retain(|_, (_, at)| at.elapsed() < DECISION_RETENTION);
    }

    fn prune_invalid_requests(state: &mut PermissionState) {
        let now = std::time::Instant::now();
        let invalid: Vec<String> = state
            .pending_requests
            .iter()
            .filter_map(|(id, entry)| (!entry.is_valid_at(now)).then_some(id.clone()))
            .collect();
        for id in invalid {
            state.pending_requests.remove(&id);
            state.decisions.insert(id, (PermissionDecision::Deny, now));
        }
    }

    fn requires_approval(access_mode: ProjectAccessMode, risk_level: RiskLevel) -> bool {
        match access_mode {
            // R1 覆盖低风险外发/命令；R0 保持纯只读检索的无打扰体验。
            ProjectAccessMode::RequestApproval => {
                matches!(risk_level, RiskLevel::R1 | RiskLevel::R2 | RiskLevel::R3)
            }
            ProjectAccessMode::RiskBased => matches!(risk_level, RiskLevel::R2 | RiskLevel::R3),
            ProjectAccessMode::FullAccess => false,
        }
    }

    /// 返回已有规则可直接得出的结果。
    ///
    /// `AllowAlways` 本身就是用户在“请求批准”模式下做出的显式授权；该模式只要求
    /// 未授权调用先询问，不能反过来忽略用户刚保存的任务级规则。
    async fn standing_result(
        &self,
        task_id: &str,
        tool_name: &str,
        target: Option<&str>,
        risk_level: RiskLevel,
    ) -> Option<PermissionCheckResult> {
        let key = StandingRuleKey {
            task_id: task_id.to_string(),
            tool_name: tool_name.to_string(),
            target: target.map(ToOwned::to_owned),
        };
        let state = self.state.lock().await;
        // Decisions made from the generic approval card are stored without a target and therefore
        // act as a task/tool wildcard. An explicitly targeted rule still wins when both exist.
        let decision = state.standing_rules.get(&key).or_else(|| {
            let mut wildcard = key.clone();
            wildcard.target = None;
            state.standing_rules.get(&wildcard)
        });
        match decision {
            Some(StandingRule {
                decision: PermissionDecision::Deny,
                ..
            }) => Some(PermissionCheckResult::Denied(
                "denied by standing rule".to_string(),
            )),
            Some(StandingRule {
                decision: PermissionDecision::AllowAlways | PermissionDecision::Allow,
                risk_ceiling,
            }) if risk_rank(risk_level) <= risk_rank(*risk_ceiling) => {
                Some(PermissionCheckResult::Allowed)
            }
            _ => None,
        }
    }

    /// 检查工具调用是否需要权限审批。
    ///
    /// - R0/R1 -> `Allowed`
    /// - R4 -> `Denied`
    /// - R2/R3 -> 查 standing rule；命中 `AllowAlways`/`Allow` 则 `Allowed`，
    ///   命中 `Deny` 则 `Denied`；否则创建并存储 `PermissionRequest`，返回
    ///   `NeedsApproval(request)`。
    pub async fn check(
        &self,
        task_id: &str,
        tool_name: &str,
        risk_level: RiskLevel,
        target: Option<&str>,
    ) -> PermissionCheckResult {
        self.check_with_access_mode(
            task_id,
            tool_name,
            risk_level,
            target,
            ProjectAccessMode::RiskBased,
        )
        .await
    }

    /// 按项目级权限模式检查工具调用。
    pub async fn check_with_access_mode(
        &self,
        task_id: &str,
        tool_name: &str,
        risk_level: RiskLevel,
        target: Option<&str>,
        access_mode: ProjectAccessMode,
    ) -> PermissionCheckResult {
        if risk_level == RiskLevel::R4 {
            return PermissionCheckResult::Denied(
                "risk level R4: pre-rejected by policy".to_string(),
            );
        }
        if let Some(result) = self
            .standing_result(task_id, tool_name, target, risk_level)
            .await
        {
            return result;
        }
        if !Self::requires_approval(access_mode, risk_level) {
            return PermissionCheckResult::Allowed;
        }

        let request = PermissionRequest::new(
            task_id, "", // tool_call_id -- check 阶段未知，由调用方后续补充
            tool_name, risk_level, "", // input_summary -- check 阶段未知
        )
        .with_target(target);
        self.insert_pending(request.clone(), None, None).await;
        PermissionCheckResult::NeedsApproval(request)
    }

    /// 创建一个带完整信息的权限请求并存入 pending 队列。
    ///
    /// 当调用方已持有 `tool_call_id` 和 `input_summary` 时使用此方法，
    /// 以替代 `check` 内部创建的精简请求。
    pub async fn request_permission(
        &self,
        task_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        risk_level: RiskLevel,
        input_summary: &str,
    ) -> PermissionRequest {
        let request =
            PermissionRequest::new(task_id, tool_call_id, tool_name, risk_level, input_summary);
        self.insert_pending(request.clone(), None, None).await;
        request
    }

    /// 对待审批请求做出决定。
    ///
    /// - `Allow`：单次允许，请求从 pending 移除。
    /// - `AllowAlways`：尝试添加 standing rule（R3/R4 会被拒绝并返回错误），
    ///   成功后请求从 pending 移除。
    /// - `Deny`：拒绝，请求从 pending 移除。
    pub async fn decide(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> Result<(), ProductError> {
        if decision == PermissionDecision::Pending {
            return Err(ProductError::PermissionError(
                "pending is not a terminal permission decision".to_string(),
            ));
        }

        let mut state = self.state.lock().await;
        Self::retain_recent_decisions(&mut state);
        let now = std::time::Instant::now();
        let entry = state
            .pending_requests
            .get(request_id)
            .ok_or_else(|| {
                ProductError::PermissionError(format!("permission request {request_id} not found"))
            })?
            .clone();

        // Cancellation/deadline is part of the request state and is checked under the same lock
        // as the user decision.  Whichever transition reaches this lock first is the sole winner.
        if !entry.is_valid_at(now) {
            state.pending_requests.remove(request_id);
            state
                .decisions
                .insert(request_id.to_string(), (PermissionDecision::Deny, now));
            return Err(ProductError::PermissionError(format!(
                "permission request {request_id} expired or was cancelled"
            )));
        }

        if decision == PermissionDecision::AllowAlways {
            if !entry.request.risk_level.can_persist_standing() {
                return Err(ProductError::PermissionError(format!(
                    "risk level {} cannot be persisted as standing rule",
                    entry.request.risk_level
                )));
            }
            let key = StandingRuleKey {
                task_id: entry.request.task_id.clone(),
                tool_name: entry.request.tool_name.clone(),
                target: entry.request.target.clone(),
            };
            state.standing_rules.insert(
                key,
                StandingRule {
                    decision: PermissionDecision::AllowAlways,
                    risk_ceiling: entry.request.risk_level,
                },
            );
        }

        // No await occurs between publishing a standing rule, removing pending, and recording
        // the winning decision, so aborting this future cannot expose a half-committed state.
        state.pending_requests.remove(request_id);
        state
            .decisions
            .insert(request_id.to_string(), (decision, now));
        Ok(())
    }

    /// Cancel a still-pending request and wake any waiter with a fail-closed Deny decision.
    ///
    /// This shares the same transition lock as [`Self::decide`], so a late approval cannot
    /// create a standing rule after cancellation has won the race.
    pub async fn cancel_request(&self, request_id: &str) -> bool {
        let mut state = self.state.lock().await;
        let removed = state.pending_requests.remove(request_id).is_some();
        if removed {
            state.decisions.insert(
                request_id.to_string(),
                (PermissionDecision::Deny, std::time::Instant::now()),
            );
        }
        removed
    }

    /// 检查工具调用是否需要权限审批（完整信息版）。
    ///
    /// 与 `check` 逻辑一致，但创建的 `PermissionRequest` 携带真实的
    /// `tool_call_id`、`input_summary` 和调用归属（供审批 UI 展示）。
    #[allow(clippy::too_many_arguments)]
    pub async fn check_detailed(
        &self,
        task_id: &str,
        tool_call_id: &str,
        run_id: Option<&str>,
        caller: Option<&str>,
        tool_name: &str,
        risk_level: RiskLevel,
        input_summary: &str,
        target: Option<&str>,
    ) -> PermissionCheckResult {
        self.check_detailed_with_access_mode(
            task_id,
            tool_call_id,
            run_id,
            caller,
            tool_name,
            risk_level,
            input_summary,
            target,
            ProjectAccessMode::RiskBased,
        )
        .await
    }

    /// 完整信息版的项目权限模式检查。
    #[allow(clippy::too_many_arguments)]
    pub async fn check_detailed_with_access_mode(
        &self,
        task_id: &str,
        tool_call_id: &str,
        run_id: Option<&str>,
        caller: Option<&str>,
        tool_name: &str,
        risk_level: RiskLevel,
        input_summary: &str,
        target: Option<&str>,
        access_mode: ProjectAccessMode,
    ) -> PermissionCheckResult {
        self.check_detailed_with_access_mode_and_lifecycle(
            task_id,
            tool_call_id,
            run_id,
            caller,
            tool_name,
            risk_level,
            input_summary,
            target,
            access_mode,
            None,
            None,
        )
        .await
    }

    /// Complete permission check with a cancellation/deadline lifecycle bound atomically to the
    /// newly-created pending request.  A decision arriving after either boundary is rejected.
    #[allow(clippy::too_many_arguments)]
    pub async fn check_detailed_with_access_mode_and_lifecycle(
        &self,
        task_id: &str,
        tool_call_id: &str,
        run_id: Option<&str>,
        caller: Option<&str>,
        tool_name: &str,
        risk_level: RiskLevel,
        input_summary: &str,
        target: Option<&str>,
        access_mode: ProjectAccessMode,
        cancellation: Option<PermissionCancellation>,
        timeout: Option<std::time::Duration>,
    ) -> PermissionCheckResult {
        if risk_level == RiskLevel::R4 {
            return PermissionCheckResult::Denied(
                "risk level R4: pre-rejected by policy".to_string(),
            );
        }
        if let Some(result) = self
            .standing_result(task_id, tool_name, target, risk_level)
            .await
        {
            return result;
        }
        if !Self::requires_approval(access_mode, risk_level) {
            return PermissionCheckResult::Allowed;
        }

        let request =
            PermissionRequest::new(task_id, tool_call_id, tool_name, risk_level, input_summary)
                .with_origin(run_id, caller)
                .with_target(target);
        self.insert_pending(request.clone(), cancellation, timeout)
            .await;
        PermissionCheckResult::NeedsApproval(request)
    }

    /// 单次查询某个权限请求的审批决策（不等待）。
    ///
    /// 返回 `Some(decision)` 若已批复；未批复/未知请求返回 `None`。
    /// 顺带惰性清理超过 `DECISION_RETENTION` 的旧决策。
    pub async fn try_decision(&self, request_id: &str) -> Option<PermissionDecision> {
        let mut state = self.state.lock().await;
        Self::retain_recent_decisions(&mut state);
        Self::prune_invalid_requests(&mut state);
        state.decisions.get(request_id).map(|(d, _)| *d)
    }

    /// 挂起等待某个权限请求的审批决策（150ms 轮询）。
    ///
    /// 返回 `Some(decision)`（Allow / AllowAlways / Deny）；超时返回 `None`。
    /// 顺带惰性清理超过 `DECISION_RETENTION` 的旧决策。
    pub async fn wait_decision(
        &self,
        request_id: &str,
        timeout: std::time::Duration,
    ) -> Option<PermissionDecision> {
        let start = std::time::Instant::now();
        loop {
            if let Some(decision) = self.try_decision(request_id).await {
                return Some(decision);
            }
            if start.elapsed() >= timeout {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    }

    /// 添加 standing rule（本任务内 always allow）。
    ///
    /// R3/R4 规则不持久化（`RiskLevel::can_persist_standing() == false`），
    /// 返回 `PermissionError`。
    pub async fn add_standing_rule(
        &self,
        task_id: &str,
        tool_name: &str,
        target: Option<&str>,
        risk_level: RiskLevel,
        decision: PermissionDecision,
    ) -> Result<(), ProductError> {
        if !risk_level.can_persist_standing() {
            return Err(ProductError::PermissionError(format!(
                "risk level {risk_level} cannot be persisted as standing rule"
            )));
        }
        let key = StandingRuleKey {
            task_id: task_id.to_string(),
            tool_name: tool_name.to_string(),
            target: target.map(|s| s.to_string()),
        };
        self.state.lock().await.standing_rules.insert(
            key,
            StandingRule {
                decision,
                risk_ceiling: risk_level,
            },
        );
        Ok(())
    }

    /// 清除指定任务的所有 standing rules。
    pub async fn clear_task_rules(&self, task_id: &str) {
        self.state
            .lock()
            .await
            .standing_rules
            .retain(|key, _| key.task_id != task_id);
    }

    /// 获取指定任务的待审批请求列表。
    pub async fn pending_for_task(&self, task_id: &str) -> Vec<PermissionRequest> {
        let mut state = self.state.lock().await;
        Self::retain_recent_decisions(&mut state);
        Self::prune_invalid_requests(&mut state);
        state
            .pending_requests
            .values()
            .filter(|entry| entry.request.task_id == task_id)
            .map(|entry| entry.request.clone())
            .collect()
    }

    /// 按 ID 读取仍待审批的请求，不改变其状态。
    ///
    /// 审批入口需要在 `decide` 移除内存请求前记住任务归属，以便写入项目活动与
    /// 关闭同源通知；暴露只读副本不会泄露任何额外能力。
    pub async fn pending_by_id(&self, request_id: &str) -> Option<PermissionRequest> {
        let mut state = self.state.lock().await;
        Self::retain_recent_decisions(&mut state);
        Self::prune_invalid_requests(&mut state);
        state
            .pending_requests
            .get(request_id)
            .map(|entry| entry.request.clone())
    }
}

impl Default for PermissionEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn r0_r1_allowed() {
        let engine = PermissionEngine::new();
        assert_eq!(
            engine.check("t1", "list_files", RiskLevel::R0, None).await,
            PermissionCheckResult::Allowed
        );
        assert_eq!(
            engine.check("t1", "read_file", RiskLevel::R1, None).await,
            PermissionCheckResult::Allowed
        );
    }

    #[tokio::test]
    async fn r2_r3_needs_approval() {
        let engine = PermissionEngine::new();

        let result = engine.check("t1", "write_file", RiskLevel::R2, None).await;
        assert!(matches!(result, PermissionCheckResult::NeedsApproval(_)));
        // 请求应已存入 pending 队列
        assert_eq!(engine.pending_for_task("t1").await.len(), 1);

        let result = engine.check("t1", "kill", RiskLevel::R3, None).await;
        assert!(matches!(result, PermissionCheckResult::NeedsApproval(_)));
        assert_eq!(engine.pending_for_task("t1").await.len(), 2);
    }

    #[tokio::test]
    async fn r4_denied() {
        let engine = PermissionEngine::new();
        let result = engine.check("t1", "forbidden", RiskLevel::R4, None).await;
        assert!(matches!(result, PermissionCheckResult::Denied(_)));
    }

    #[tokio::test]
    async fn request_approval_prompts_for_r1_and_r2() {
        let engine = PermissionEngine::new();
        let r0 = engine
            .check_with_access_mode(
                "t1",
                "list_files",
                RiskLevel::R0,
                None,
                ProjectAccessMode::RequestApproval,
            )
            .await;
        assert_eq!(r0, PermissionCheckResult::Allowed);

        let r1 = engine
            .check_with_access_mode(
                "t1",
                "network_lookup",
                RiskLevel::R1,
                None,
                ProjectAccessMode::RequestApproval,
            )
            .await;
        assert!(matches!(r1, PermissionCheckResult::NeedsApproval(_)));

        let r2 = engine
            .check_with_access_mode(
                "t1",
                "write_file",
                RiskLevel::R2,
                None,
                ProjectAccessMode::RequestApproval,
            )
            .await;
        assert!(matches!(r2, PermissionCheckResult::NeedsApproval(_)));
    }

    #[tokio::test]
    async fn request_approval_honors_allow_always_only_for_the_same_target() {
        let engine = PermissionEngine::new();
        let first = engine
            .check_detailed_with_access_mode(
                "t1",
                "call-1",
                Some("run-1"),
                Some("main:run-1"),
                "web_fetch",
                RiskLevel::R1,
                "fetch public documentation",
                Some("https://milvus.io/docs/"),
                ProjectAccessMode::RequestApproval,
            )
            .await;
        let PermissionCheckResult::NeedsApproval(request) = first else {
            panic!("the first request must require approval");
        };

        engine
            .decide(&request.id, PermissionDecision::AllowAlways)
            .await
            .unwrap();

        assert_eq!(
            engine
                .check_with_access_mode(
                    "t1",
                    "web_fetch",
                    RiskLevel::R1,
                    Some("https://milvus.io/docs/"),
                    ProjectAccessMode::RequestApproval,
                )
                .await,
            PermissionCheckResult::Allowed
        );
        assert!(matches!(
            engine
                .check_with_access_mode(
                    "t1",
                    "web_fetch",
                    RiskLevel::R1,
                    Some("https://milvus.io/docs/full-text-search.md"),
                    ProjectAccessMode::RequestApproval,
                )
                .await,
            PermissionCheckResult::NeedsApproval(_)
        ));
    }

    #[tokio::test]
    async fn full_access_allows_r2_r3_but_never_r4() {
        let engine = PermissionEngine::new();
        for risk_level in [RiskLevel::R2, RiskLevel::R3] {
            let result = engine
                .check_with_access_mode(
                    "t1",
                    "write_file",
                    risk_level,
                    None,
                    ProjectAccessMode::FullAccess,
                )
                .await;
            assert_eq!(result, PermissionCheckResult::Allowed);
        }
        let denied = engine
            .check_with_access_mode(
                "t1",
                "forbidden",
                RiskLevel::R4,
                None,
                ProjectAccessMode::FullAccess,
            )
            .await;
        assert!(matches!(denied, PermissionCheckResult::Denied(_)));
    }

    #[tokio::test]
    async fn full_access_keeps_explicit_deny_rule() {
        let engine = PermissionEngine::new();
        engine
            .add_standing_rule(
                "t1",
                "write_file",
                None,
                RiskLevel::R2,
                PermissionDecision::Deny,
            )
            .await
            .unwrap();
        let result = engine
            .check_with_access_mode(
                "t1",
                "write_file",
                RiskLevel::R2,
                None,
                ProjectAccessMode::FullAccess,
            )
            .await;
        assert!(matches!(result, PermissionCheckResult::Denied(_)));
    }

    #[tokio::test]
    async fn standing_rule_allows_r2() {
        let engine = PermissionEngine::new();
        engine
            .add_standing_rule(
                "t1",
                "write_file",
                None,
                RiskLevel::R2,
                PermissionDecision::AllowAlways,
            )
            .await
            .unwrap();

        let result = engine.check("t1", "write_file", RiskLevel::R2, None).await;
        assert_eq!(result, PermissionCheckResult::Allowed);
        // 不应创建 pending 请求
        assert!(engine.pending_for_task("t1").await.is_empty());
    }

    #[tokio::test]
    async fn r2_allow_always_does_not_authorize_r3_for_same_tool() {
        let engine = PermissionEngine::new();
        let request = engine
            .request_permission("t1", "call-r2", "bash", RiskLevel::R2, "bash cargo test")
            .await;

        engine
            .decide(&request.id, PermissionDecision::AllowAlways)
            .await
            .unwrap();

        assert_eq!(
            engine.check("t1", "bash", RiskLevel::R2, None).await,
            PermissionCheckResult::Allowed
        );

        let escalated = engine
            .check_detailed(
                "t1",
                "call-r3",
                Some("run-1"),
                Some("subagent:run-1"),
                "bash",
                RiskLevel::R3,
                "bash npm install x",
                None,
            )
            .await;
        assert!(matches!(escalated, PermissionCheckResult::NeedsApproval(_)));
    }

    #[tokio::test]
    async fn allow_always_is_persisted_for_the_requested_target_only() {
        let engine = PermissionEngine::new();
        let first = engine
            .check_detailed(
                "t1",
                "call-one",
                Some("run-1"),
                Some("subagent:run-1"),
                "mcp_call",
                RiskLevel::R2,
                "server-one/tool-one",
                Some("server-one/tool-one"),
            )
            .await;
        let PermissionCheckResult::NeedsApproval(first) = first else {
            panic!("first target must require approval");
        };
        assert_eq!(first.target.as_deref(), Some("server-one/tool-one"));
        engine
            .decide(&first.id, PermissionDecision::AllowAlways)
            .await
            .unwrap();

        assert_eq!(
            engine
                .check("t1", "mcp_call", RiskLevel::R2, Some("server-one/tool-one"))
                .await,
            PermissionCheckResult::Allowed
        );
        assert!(matches!(
            engine
                .check("t1", "mcp_call", RiskLevel::R2, Some("server-two/tool-two"))
                .await,
            PermissionCheckResult::NeedsApproval(_)
        ));
    }

    #[tokio::test]
    async fn standing_rule_with_target() {
        let engine = PermissionEngine::new();
        engine
            .add_standing_rule(
                "t1",
                "terminal.send",
                Some("term-1"),
                RiskLevel::R2,
                PermissionDecision::AllowAlways,
            )
            .await
            .unwrap();

        // 命中 target
        assert_eq!(
            engine
                .check("t1", "terminal.send", RiskLevel::R2, Some("term-1"))
                .await,
            PermissionCheckResult::Allowed
        );
        // 不同 target 不命中
        let miss = engine
            .check("t1", "terminal.send", RiskLevel::R2, Some("term-2"))
            .await;
        assert!(matches!(miss, PermissionCheckResult::NeedsApproval(_)));
    }

    #[tokio::test]
    async fn standing_rule_deny() {
        let engine = PermissionEngine::new();
        engine
            .add_standing_rule(
                "t1",
                "write_file",
                None,
                RiskLevel::R2,
                PermissionDecision::Deny,
            )
            .await
            .unwrap();

        let result = engine.check("t1", "write_file", RiskLevel::R2, None).await;
        assert!(matches!(result, PermissionCheckResult::Denied(_)));
    }

    #[tokio::test]
    async fn r3_standing_rule_rejected() {
        let engine = PermissionEngine::new();
        let result = engine
            .add_standing_rule(
                "t1",
                "kill",
                None,
                RiskLevel::R3,
                PermissionDecision::AllowAlways,
            )
            .await;
        assert!(result.is_err());
        // 确认规则未写入
        let check = engine.check("t1", "kill", RiskLevel::R3, None).await;
        assert!(matches!(check, PermissionCheckResult::NeedsApproval(_)));
    }

    #[tokio::test]
    async fn r4_standing_rule_rejected() {
        let engine = PermissionEngine::new();
        let result = engine
            .add_standing_rule(
                "t1",
                "forbidden",
                None,
                RiskLevel::R4,
                PermissionDecision::AllowAlways,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn clear_task_rules() {
        let engine = PermissionEngine::new();
        engine
            .add_standing_rule(
                "t1",
                "write_file",
                None,
                RiskLevel::R2,
                PermissionDecision::AllowAlways,
            )
            .await
            .unwrap();
        engine
            .add_standing_rule(
                "t2",
                "write_file",
                None,
                RiskLevel::R2,
                PermissionDecision::AllowAlways,
            )
            .await
            .unwrap();

        engine.clear_task_rules("t1").await;

        // t1 规则已清除 -> 需审批
        assert!(matches!(
            engine.check("t1", "write_file", RiskLevel::R2, None).await,
            PermissionCheckResult::NeedsApproval(_)
        ));
        // t2 规则仍在 -> 允许
        assert_eq!(
            engine.check("t2", "write_file", RiskLevel::R2, None).await,
            PermissionCheckResult::Allowed
        );
    }

    #[tokio::test]
    async fn decide_allow() {
        let engine = PermissionEngine::new();
        let req = engine
            .request_permission("t1", "tc1", "write_file", RiskLevel::R2, "write foo.txt")
            .await;

        engine
            .decide(&req.id, PermissionDecision::Allow)
            .await
            .unwrap();

        // 请求应已从 pending 移除
        assert!(engine.pending_for_task("t1").await.is_empty());
    }

    #[tokio::test]
    async fn decide_deny() {
        let engine = PermissionEngine::new();
        let req = engine
            .request_permission("t1", "tc1", "write_file", RiskLevel::R2, "write foo.txt")
            .await;

        engine
            .decide(&req.id, PermissionDecision::Deny)
            .await
            .unwrap();

        assert!(engine.pending_for_task("t1").await.is_empty());
    }

    #[tokio::test]
    async fn decide_allow_always_adds_standing_rule() {
        let engine = PermissionEngine::new();
        let req = engine
            .request_permission("t1", "tc1", "write_file", RiskLevel::R2, "write foo.txt")
            .await;

        engine
            .decide(&req.id, PermissionDecision::AllowAlways)
            .await
            .unwrap();

        // 后续同类调用应自动允许
        assert_eq!(
            engine.check("t1", "write_file", RiskLevel::R2, None).await,
            PermissionCheckResult::Allowed
        );
    }

    #[tokio::test]
    async fn concurrent_conflicting_decisions_have_one_atomic_winner() {
        let engine = Arc::new(PermissionEngine::new());
        let request = engine
            .request_permission("t1", "tc-race", "bash", RiskLevel::R2, "cargo test")
            .await;
        let barrier = Arc::new(tokio::sync::Barrier::new(3));

        let allow_engine = engine.clone();
        let allow_barrier = barrier.clone();
        let allow_id = request.id.clone();
        let allow = tokio::spawn(async move {
            allow_barrier.wait().await;
            allow_engine
                .decide(&allow_id, PermissionDecision::AllowAlways)
                .await
        });

        let deny_engine = engine.clone();
        let deny_barrier = barrier.clone();
        let deny_id = request.id.clone();
        let deny = tokio::spawn(async move {
            deny_barrier.wait().await;
            deny_engine.decide(&deny_id, PermissionDecision::Deny).await
        });

        barrier.wait().await;
        let allow_result = allow.await.unwrap();
        let deny_result = deny.await.unwrap();
        assert_ne!(allow_result.is_ok(), deny_result.is_ok());
        assert!(engine.pending_for_task("t1").await.is_empty());

        let winner = engine.try_decision(&request.id).await.unwrap();
        let next = engine.check("t1", "bash", RiskLevel::R2, None).await;
        match winner {
            PermissionDecision::AllowAlways => {
                assert_eq!(next, PermissionCheckResult::Allowed);
            }
            PermissionDecision::Deny => {
                assert!(matches!(next, PermissionCheckResult::NeedsApproval(_)));
            }
            other => panic!("unexpected winning decision: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancellation_removes_pending_and_rejects_late_allow_always() {
        let engine = PermissionEngine::new();
        let request = engine
            .request_permission("t1", "tc-cancelled", "bash", RiskLevel::R2, "cargo test")
            .await;

        assert!(engine.cancel_request(&request.id).await);
        assert!(!engine.cancel_request(&request.id).await);
        assert!(engine.pending_for_task("t1").await.is_empty());
        assert_eq!(
            engine.try_decision(&request.id).await,
            Some(PermissionDecision::Deny)
        );
        assert!(engine
            .decide(&request.id, PermissionDecision::AllowAlways)
            .await
            .is_err());
        assert!(matches!(
            engine.check("t1", "bash", RiskLevel::R2, None).await,
            PermissionCheckResult::NeedsApproval(_)
        ));
    }

    #[tokio::test]
    async fn aborted_lifecycle_rejects_allow_always_without_leaving_a_rule() {
        let engine = PermissionEngine::new();
        let abort = Arc::new(AtomicBool::new(false));
        let result = engine
            .check_detailed_with_access_mode_and_lifecycle(
                "t-aborted",
                "tc-aborted",
                Some("run-aborted"),
                Some("agent"),
                "bash",
                RiskLevel::R2,
                "cargo test",
                None,
                ProjectAccessMode::RequestApproval,
                Some(PermissionCancellation::from_atomic(abort.clone())),
                Some(std::time::Duration::from_secs(60)),
            )
            .await;
        let PermissionCheckResult::NeedsApproval(request) = result else {
            panic!("R2 request must wait for approval");
        };

        abort.store(true, Ordering::SeqCst);
        assert!(engine
            .decide(&request.id, PermissionDecision::AllowAlways)
            .await
            .is_err());
        assert_eq!(
            engine.try_decision(&request.id).await,
            Some(PermissionDecision::Deny)
        );
        assert!(engine.pending_for_task("t-aborted").await.is_empty());
        assert!(matches!(
            engine.check("t-aborted", "bash", RiskLevel::R2, None).await,
            PermissionCheckResult::NeedsApproval(_)
        ));
    }

    #[tokio::test]
    async fn expired_lifecycle_rejects_allow_always_without_leaving_a_rule() {
        let engine = PermissionEngine::new();
        let result = engine
            .check_detailed_with_access_mode_and_lifecycle(
                "t-expired",
                "tc-expired",
                Some("run-expired"),
                Some("agent"),
                "bash",
                RiskLevel::R2,
                "cargo test",
                None,
                ProjectAccessMode::RequestApproval,
                None,
                Some(std::time::Duration::ZERO),
            )
            .await;
        let PermissionCheckResult::NeedsApproval(request) = result else {
            panic!("R2 request must wait for approval");
        };

        assert!(engine
            .decide(&request.id, PermissionDecision::AllowAlways)
            .await
            .is_err());
        assert_eq!(
            engine.try_decision(&request.id).await,
            Some(PermissionDecision::Deny)
        );
        assert!(matches!(
            engine.check("t-expired", "bash", RiskLevel::R2, None).await,
            PermissionCheckResult::NeedsApproval(_)
        ));
    }

    #[tokio::test]
    async fn dropping_a_blocked_decision_future_cannot_half_commit_allow_always() {
        let engine = Arc::new(PermissionEngine::new());
        let request = engine
            .request_permission("t-drop", "tc-drop", "bash", RiskLevel::R2, "cargo test")
            .await;
        let state_guard = engine.state.lock().await;
        let deciding_engine = engine.clone();
        let request_id = request.id.clone();
        let decision = tokio::spawn(async move {
            deciding_engine
                .decide(&request_id, PermissionDecision::AllowAlways)
                .await
        });
        tokio::task::yield_now().await;

        decision.abort();
        assert!(decision.await.unwrap_err().is_cancelled());
        drop(state_guard);

        assert_eq!(engine.pending_for_task("t-drop").await.len(), 1);
        assert!(engine.cancel_request(&request.id).await);
        assert!(matches!(
            engine.check("t-drop", "bash", RiskLevel::R2, None).await,
            PermissionCheckResult::NeedsApproval(_)
        ));
    }

    #[tokio::test]
    async fn decide_allow_always_r3_rejected() {
        let engine = PermissionEngine::new();
        let req = engine
            .request_permission("t1", "tc1", "kill", RiskLevel::R3, "kill process")
            .await;

        // R3 的 AllowAlways 应失败
        let result = engine
            .decide(&req.id, PermissionDecision::AllowAlways)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn decide_unknown_request() {
        let engine = PermissionEngine::new();
        let result = engine
            .decide("nonexistent", PermissionDecision::Allow)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn request_permission_stores_pending() {
        let engine = PermissionEngine::new();
        let req = engine
            .request_permission("t1", "tc1", "write_file", RiskLevel::R2, "summary")
            .await;

        assert_eq!(req.task_id, "t1");
        assert_eq!(req.tool_call_id, "tc1");
        assert_eq!(req.tool_name, "write_file");
        assert_eq!(req.input_summary, "summary");
        assert_eq!(req.decision, PermissionDecision::Pending);

        let pending = engine.pending_for_task("t1").await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, req.id);
    }

    #[tokio::test]
    async fn detailed_request_keeps_run_and_caller_origin() {
        let engine = PermissionEngine::new();
        let result = engine
            .check_detailed(
                "t1",
                "call-1",
                Some("child-run"),
                Some("subagent:child-run"),
                "write_file",
                RiskLevel::R2,
                "write src/lib.rs",
                None,
            )
            .await;

        let PermissionCheckResult::NeedsApproval(request) = result else {
            panic!("R2 调用应创建待审批请求");
        };
        assert_eq!(request.run_id.as_deref(), Some("child-run"));
        assert_eq!(request.caller.as_deref(), Some("subagent:child-run"));
        let pending = engine.pending_for_task("t1").await;
        assert_eq!(pending[0].run_id.as_deref(), Some("child-run"));
    }

    #[tokio::test]
    async fn pending_for_task_filters() {
        let engine = PermissionEngine::new();
        engine
            .request_permission("t1", "tc1", "write_file", RiskLevel::R2, "a")
            .await;
        engine
            .request_permission("t1", "tc2", "write_file", RiskLevel::R2, "b")
            .await;
        engine
            .request_permission("t2", "tc3", "write_file", RiskLevel::R2, "c")
            .await;

        assert_eq!(engine.pending_for_task("t1").await.len(), 2);
        assert_eq!(engine.pending_for_task("t2").await.len(), 1);
        assert!(engine.pending_for_task("t3").await.is_empty());
    }
}
