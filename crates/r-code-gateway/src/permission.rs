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
use std::sync::Arc;

use chrono::Utc;
use r_code_core::dto::{PermissionDecision, PermissionRequest, ProjectAccessMode, RiskLevel};
use r_code_core::error::ProductError;
use tokio::sync::RwLock;

/// Permission Engine -- 风险分级与审批流程。
pub struct PermissionEngine {
    /// Standing rules: (task_id, tool_name, target) -> Decision。
    /// R3/R4 规则不持久化（`add_standing_rule` 拒绝）。
    standing_rules: Arc<RwLock<HashMap<StandingRuleKey, PermissionDecision>>>,
    /// 等待审批的权限请求。
    pending_requests: Arc<RwLock<HashMap<String, PermissionRequest>>>,
    /// 最近的审批决策（request_id → 决策与时间），供 `wait_decision` 挂起等待。
    /// 超过 `DECISION_RETENTION` 的条目在等待时被惰性清理。
    decisions: Arc<RwLock<HashMap<String, (PermissionDecision, std::time::Instant)>>>,
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
        Self {
            standing_rules: Arc::new(RwLock::new(HashMap::new())),
            pending_requests: Arc::new(RwLock::new(HashMap::new())),
            decisions: Arc::new(RwLock::new(HashMap::new())),
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

    /// 返回已有规则可直接得出的结果。严格模式只执行显式拒绝规则，避免
    /// “总是允许”绕过“请求批准”的产品承诺。
    async fn standing_result(
        &self,
        task_id: &str,
        tool_name: &str,
        target: Option<&str>,
        honor_allow_rule: bool,
    ) -> Option<PermissionCheckResult> {
        let key = StandingRuleKey {
            task_id: task_id.to_string(),
            tool_name: tool_name.to_string(),
            target: target.map(ToOwned::to_owned),
        };
        let rules = self.standing_rules.read().await;
        match rules.get(&key) {
            Some(PermissionDecision::Deny) => Some(PermissionCheckResult::Denied(
                "denied by standing rule".to_string(),
            )),
            Some(PermissionDecision::AllowAlways | PermissionDecision::Allow)
                if honor_allow_rule =>
            {
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
            .standing_result(
                task_id,
                tool_name,
                target,
                access_mode != ProjectAccessMode::RequestApproval,
            )
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
        );
        self.pending_requests
            .write()
            .await
            .insert(request.id.clone(), request.clone());
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
        self.pending_requests
            .write()
            .await
            .insert(request.id.clone(), request.clone());
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
        // 先读出请求信息（释放读锁后再可能写 standing_rules，避免死锁）
        let request_info = {
            let requests = self.pending_requests.read().await;
            requests
                .get(request_id)
                .ok_or_else(|| {
                    ProductError::PermissionError(format!(
                        "permission request {request_id} not found"
                    ))
                })?
                .clone()
        };

        // AllowAlways 需要先成功添加 standing rule（R3/R4 被拒绝）
        if decision == PermissionDecision::AllowAlways {
            self.add_standing_rule(
                &request_info.task_id,
                &request_info.tool_name,
                None,
                request_info.risk_level,
                PermissionDecision::AllowAlways,
            )
            .await?;
        }

        // 更新请求状态并从 pending 移除；决策写入暂存（供 wait_decision 查询）
        let mut requests = self.pending_requests.write().await;
        if let Some(mut req) = requests.remove(request_id) {
            req.decision = decision;
            req.decided_at = Some(Utc::now());
            drop(requests);
            self.decisions.write().await.insert(
                request_id.to_string(),
                (decision, std::time::Instant::now()),
            );
        }
        Ok(())
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
        if risk_level == RiskLevel::R4 {
            return PermissionCheckResult::Denied(
                "risk level R4: pre-rejected by policy".to_string(),
            );
        }
        if let Some(result) = self
            .standing_result(
                task_id,
                tool_name,
                target,
                access_mode != ProjectAccessMode::RequestApproval,
            )
            .await
        {
            return result;
        }
        if !Self::requires_approval(access_mode, risk_level) {
            return PermissionCheckResult::Allowed;
        }

        let request =
            PermissionRequest::new(task_id, tool_call_id, tool_name, risk_level, input_summary)
                .with_origin(run_id, caller);
        self.pending_requests
            .write()
            .await
            .insert(request.id.clone(), request.clone());
        PermissionCheckResult::NeedsApproval(request)
    }

    /// 单次查询某个权限请求的审批决策（不等待）。
    ///
    /// 返回 `Some(decision)` 若已批复；未批复/未知请求返回 `None`。
    /// 顺带惰性清理超过 `DECISION_RETENTION` 的旧决策。
    pub async fn try_decision(&self, request_id: &str) -> Option<PermissionDecision> {
        let mut decisions = self.decisions.write().await;
        decisions.retain(|_, (_, at)| at.elapsed() < DECISION_RETENTION);
        decisions.get(request_id).map(|(d, _)| *d)
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
            {
                let mut decisions = self.decisions.write().await;
                decisions.retain(|_, (_, at)| at.elapsed() < DECISION_RETENTION);
                if let Some((decision, _)) = decisions.get(request_id) {
                    return Some(*decision);
                }
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
        self.standing_rules.write().await.insert(key, decision);
        Ok(())
    }

    /// 清除指定任务的所有 standing rules。
    pub async fn clear_task_rules(&self, task_id: &str) {
        self.standing_rules
            .write()
            .await
            .retain(|key, _| key.task_id != task_id);
    }

    /// 获取指定任务的待审批请求列表。
    pub async fn pending_for_task(&self, task_id: &str) -> Vec<PermissionRequest> {
        self.pending_requests
            .read()
            .await
            .values()
            .filter(|r| r.task_id == task_id)
            .cloned()
            .collect()
    }

    /// 按 ID 读取仍待审批的请求，不改变其状态。
    ///
    /// 审批入口需要在 `decide` 移除内存请求前记住任务归属，以便写入项目活动与
    /// 关闭同源通知；暴露只读副本不会泄露任何额外能力。
    pub async fn pending_by_id(&self, request_id: &str) -> Option<PermissionRequest> {
        self.pending_requests.read().await.get(request_id).cloned()
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
