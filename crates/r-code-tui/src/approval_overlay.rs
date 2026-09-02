//! 审批浮层 codex 化（M2-05 / R-APPR-01）。
//!
//! 内联于底部面板（带面语义），y/a/esc 三键契约（MC-5）：
//! `y`=Allow（单次）、`a`=AllowAlways（本任务级 standing rule，R3/R4 被宿主拒绝）、
//! `esc`=Deny。决策只产生意图，落账经宿主 PermissionEngine（不绕过安全边界）。

use crate::approval::ApprovalDecision;
use r_code_core::dto::{PermissionDecision, PermissionRequest, RiskLevel};

/// 一条待审批请求的展示投影（权威在 PermissionEngine pending 队列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool_name: String,
    pub command: String,
    pub risk: RiskLevel,
}

impl PendingApproval {
    pub fn from_request(request: &PermissionRequest) -> Self {
        Self {
            request_id: request.id.clone(),
            tool_name: request.tool_name.clone(),
            command: request.input_summary.clone(),
            risk: request.risk_level,
        }
    }
}

/// 浮层行的渲染语义（app.rs 映射样式：Title=bold、Dim=辅助、Command=`$` 命令）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayLine {
    pub text: String,
    pub kind: LineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Title,
    Command,
    Option,
    Hint,
}

/// codex 形态浮层行：bold 标题 + `$ 命令` + 编号选项 + hints。
pub fn overlay_lines(pending: &PendingApproval) -> Vec<OverlayLine> {
    vec![
        OverlayLine {
            text: "是否允许执行以下命令？".to_string(),
            kind: LineKind::Title,
        },
        OverlayLine {
            text: format!("（{} · 风险 {}）", pending.tool_name, pending.risk),
            kind: LineKind::Hint,
        },
        OverlayLine {
            text: format!("$ {}", pending.command),
            kind: LineKind::Command,
        },
        OverlayLine {
            text: "  1. 允许 (y)".to_string(),
            kind: LineKind::Option,
        },
        OverlayLine {
            text: "  2. 允许，且本任务内不再询问该命令 (a)".to_string(),
            kind: LineKind::Option,
        },
        OverlayLine {
            text: "  3. 拒绝，并告诉 R-Code 换一种做法 (esc)".to_string(),
            kind: LineKind::Option,
        },
        OverlayLine {
            text: "  y 允许 · a 任务级放行 · esc 拒绝".to_string(),
            kind: LineKind::Hint,
        },
    ]
}

/// 决策后的 transcript 注记（a 放行出 standing rule 注记文案）。
pub fn resolution_note(pending: &PendingApproval, decision: ApprovalDecision) -> String {
    match decision {
        ApprovalDecision::Approve => format!("已允许：{}", pending.command),
        ApprovalDecision::ApproveAlways => {
            format!("已放行（本任务内不再询问）：{}", pending.command)
        }
        ApprovalDecision::Deny => format!("已拒绝：{}", pending.command),
    }
}

/// 字符键 → 决策意图（y/Y=允许、a/A=任务级放行；其余无决策）。
pub fn map_decision(ch: char) -> Option<ApprovalDecision> {
    match ch.to_ascii_lowercase() {
        'y' => Some(ApprovalDecision::Approve),
        'a' => Some(ApprovalDecision::ApproveAlways),
        _ => None,
    }
}

/// 决策意图 → 宿主 PermissionDecision。
pub fn to_host_decision(decision: ApprovalDecision) -> PermissionDecision {
    match decision {
        ApprovalDecision::Approve => PermissionDecision::Allow,
        ApprovalDecision::ApproveAlways => PermissionDecision::AllowAlways,
        ApprovalDecision::Deny => PermissionDecision::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_gateway::permission::PermissionEngine;

    fn request(risk: RiskLevel) -> PermissionRequest {
        PermissionRequest {
            id: "req-1".to_string(),
            task_id: "t1".to_string(),
            tool_call_id: "c1".to_string(),
            run_id: None,
            caller: None,
            tool_name: "bash".to_string(),
            risk_level: risk,
            input_summary: "cargo test".to_string(),
            target: None,
            decision: PermissionDecision::Pending,
            created_at: chrono::Utc::now(),
            decided_at: None,
        }
    }

    /// M2-05.A1：浮层行快照——bold 标题、`$ 命令`、编号选项 1/2/3、a 前缀放行文案。
    #[test]
    fn overlay_lines_match_codex_shape() {
        let pending = PendingApproval::from_request(&request(RiskLevel::R2));
        let lines = overlay_lines(&pending);
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(lines[0].kind, LineKind::Title, "首行必须是标题：{texts:?}");
        assert!(
            texts.iter().any(|t| t.contains("是否允许执行以下命令？")),
            "codex 标题措辞：{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.starts_with("$ cargo test")),
            "命令行 `$ ` 前缀：{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("1. 允许 (y)")),
            "选项从 1 编号：{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("2. 允许，且本任务内不再询问该命令 (a)")),
            "a = 任务级放行注记措辞：{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("3. 拒绝") && t.contains("(esc)")),
            "esc = 拒绝：{texts:?}"
        );
    }

    /// M2-05.A2：y/a/esc → 三态映射（含宿主决策枚举对齐）。
    #[test]
    fn decision_keys_map_to_three_states() {
        assert_eq!(map_decision('y'), Some(ApprovalDecision::Approve));
        assert_eq!(map_decision('Y'), Some(ApprovalDecision::Approve));
        assert_eq!(map_decision('a'), Some(ApprovalDecision::ApproveAlways));
        assert_eq!(map_decision('A'), Some(ApprovalDecision::ApproveAlways));
        assert_eq!(map_decision('n'), None, "n 不是决策键");
        assert_eq!(
            to_host_decision(ApprovalDecision::Approve),
            PermissionDecision::Allow
        );
        assert_eq!(
            to_host_decision(ApprovalDecision::ApproveAlways),
            PermissionDecision::AllowAlways
        );
        assert_eq!(
            to_host_decision(ApprovalDecision::Deny),
            PermissionDecision::Deny
        );
    }

    /// M2-05.A3：a 放行 → 宿主 standing rule 生效（同任务同工具复检直接 Allowed）。
    #[tokio::test]
    async fn approve_always_creates_standing_rule() {
        let engine = PermissionEngine::new();
        let req = engine
            .request_permission("t1", "c1", "bash", RiskLevel::R2, "cargo test")
            .await;
        engine
            .decide(&req.id, PermissionDecision::AllowAlways)
            .await
            .expect("AllowAlways decide");
        assert!(
            engine.pending_for_task("t1").await.is_empty(),
            "决策后 pending 必须清空"
        );
        let recheck = engine
            .check_detailed(
                "t1",
                "c2",
                None,
                None,
                "bash",
                RiskLevel::R2,
                "cargo test",
                None,
            )
            .await;
        assert!(
            matches!(
                recheck,
                r_code_gateway::permission::PermissionCheckResult::Allowed
            ),
            "standing rule 命中必须直接 Allowed（命中前是 NeedsApproval），got {recheck:?}"
        );
    }

    /// M2-05.A4：esc 拒绝 → pending 清空、复检回到 NeedsApproval（会话可继续）。
    #[tokio::test]
    async fn deny_clears_pending_and_session_continues() {
        let engine = PermissionEngine::new();
        let req = engine
            .request_permission("t1", "c1", "bash", RiskLevel::R2, "cargo test")
            .await;
        engine
            .decide(&req.id, PermissionDecision::Deny)
            .await
            .expect("deny decide");
        assert!(engine.pending_for_task("t1").await.is_empty());
        // 拒绝不留 standing rule：同请求再次进入仍走审批（会话未被卡死）。
        let recheck = engine
            .check_detailed(
                "t1",
                "c2",
                None,
                None,
                "bash",
                RiskLevel::R2,
                "cargo test",
                None,
            )
            .await;
        assert!(
            matches!(
                recheck,
                r_code_gateway::permission::PermissionCheckResult::NeedsApproval(_)
            ),
            "拒绝后新请求仍可正常进入审批流：{recheck:?}"
        );
    }
}
