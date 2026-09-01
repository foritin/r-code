//! 阶段 2 交互面（pi-alignment PRD §4.1 R-TUI-02/03/05 / M8-02）。
//!
//! 三个纯逻辑模块（无终端 IO，可单测）：
//! - [`inline_approval`]：内联审批卡——复用 `ToolGateway` 审批矩阵的风险
//!   分级同源判定（分类器 → R0-R4 → 展示/精确 AllowAlways 目标语义），
//!   **不在 TUI 层重新实现安全**；
//! - [`snapshot_authority`]：snapshot 权威 vs 事件瞬时——渲染层不累积领域
//!   状态副本，权威状态从 JSONL 重建（此处实现"从 JSONL 重建 transcript"
//!   的纯函数：与事件累积视图的一致性断言）；
//! - [`turn_window`]：长会话 turn 级窗口化（只渲染窗口内 turn，滚动完整）。

pub use crate::approval;
pub use crate::snapshot;
pub use crate::window;

use r_code_core::dto::RiskLevel;
use r_code_gateway::classify_shell_command;

/// 内联审批卡（R-TUI-03）：从 PermissionRequest 语义事件构造。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalCard {
    /// 待审批的命令（R-TUI-03 内联审批与风险分级同源）。
    PendingCommand {
        command: String,
        /// R0-R4（经 classify_shell_command 同源分类器）。
        risk: String,
        /// AllowAlways 的精确目标 = 命令本身（不是"全部 bash"）。
        allow_always_target: String,
    },
    /// 已决策。
    Resolved { approved: bool, always: bool },
}

/// 审批面唯一入口：命令 → 风险分级（与 ToolGateway classifier 同一函数）。
pub fn approval_card_for_command(command: &str) -> ApprovalCard {
    let level = classify_shell_command(command).level;
    ApprovalCard::PendingCommand {
        command: command.to_string(),
        risk: risk_label(level),
        allow_always_target: command.to_string(),
    }
}

fn risk_label(level: RiskLevel) -> String {
    match level {
        RiskLevel::R0 => "R0 无风险".to_string(),
        RiskLevel::R1 => "R1 低风险（只读）".to_string(),
        RiskLevel::R2 => "R2 中风险".to_string(),
        RiskLevel::R3 => "R3 需审批".to_string(),
        RiskLevel::R4 => "R4 危险（默认拒绝）".to_string(),
    }
}

/// snapshot 权威（R-TUI-02）：权威 transcript 从 JSONL 重建，渲染层的事件
/// 累积视图只是瞬时缓存——两者一致性由 [`snapshot::rebuild_from_jsonl`] 的
/// 调用方断言（见 snapshot 模块测试）。
pub mod snapshot_reexport {
    pub use super::snapshot::*;
}

/// turn 级窗口化（R-TUI-05）：turn = 一次 user 输入到下一次 user 输入之间
/// 的所有行；窗口只保留最近 N 个 turn，滚动不整帧重渲全部历史。
pub mod window_reexport {
    pub use super::window::*;
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::AgentEvent;

    /// M8-02.A1：内联审批 + 风险分级同源（分类器同一函数）。
    #[test]
    fn approval_card_uses_gateway_classifier() {
        let card = approval_card_for_command("cargo test");
        assert!(matches!(card, ApprovalCard::PendingCommand { risk, .. } if risk.starts_with("R")));
        // 危险命令的分级与分类器一致（同源函数，无第二实现）。
        let dangerous = approval_card_for_command("sudo rm -rf /");
        assert!(
            matches!(&dangerous, ApprovalCard::PendingCommand { risk, .. } if risk.starts_with("R4"))
        );
        // AllowAlways 精确目标 = 命令本身。
        let ApprovalCard::PendingCommand {
            allow_always_target,
            ..
        } = &dangerous
        else {
            panic!("必须是待审批卡");
        };
        assert_eq!(allow_always_target, "sudo rm -rf /");
    }

    use crate::TuiState;

    /// 事件累积视图（瞬时）在工具事件到达时产生审批卡素材。
    #[test]
    fn tool_events_carry_approval_material() {
        let mut state = TuiState::new();
        state.apply(&AgentEvent::ToolCall {
            name: "bash".into(),
            input: serde_json::json!({"command": "cargo build"}),
            call_id: "c1".into(),
        });
        assert_eq!(state.rows().len(), 1);
    }
}
