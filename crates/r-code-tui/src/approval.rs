//! 内联审批（R-TUI-03 / M8-02.A1 续）：审批卡与 Gateway 审批矩阵的同源交互。
//!
//! `ApprovalDecision` 的三种决策在 TUI 侧只产生**意图**——真正落账的是
//! ToolGateway/PermissionEngine（AllowAlways 的精确目标语义 = 命令本身，
//! 经 PermissionEngine 的 standing rule 生效；拒绝经同一链路）。

use crate::interaction::ApprovalCard;

/// 用户对审批卡的决策（意图；执行经宿主 PermissionEngine）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    ApproveAlways,
    Deny,
}

/// 决策 → 卡片状态流转。
pub fn resolve(card: &ApprovalCard, decision: ApprovalDecision) -> ApprovalCard {
    match card {
        ApprovalCard::PendingCommand { .. } => ApprovalCard::Resolved {
            approved: !matches!(decision, ApprovalDecision::Deny),
            always: matches!(decision, ApprovalDecision::ApproveAlways),
        },
        resolved @ ApprovalCard::Resolved { .. } => resolved.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::approval_card_for_command;

    /// 决策流转：Approve/ApproveAlways/Deny 三态；AllowAlways 标记 always=true。
    #[test]
    fn decisions_resolve_cards() {
        let card = approval_card_for_command("cargo test");
        let approved = resolve(&card, ApprovalDecision::Approve);
        assert!(matches!(
            approved,
            ApprovalCard::Resolved {
                approved: true,
                always: false
            }
        ));
        let always = resolve(&card, ApprovalDecision::ApproveAlways);
        assert!(matches!(
            always,
            ApprovalCard::Resolved {
                approved: true,
                always: true
            }
        ));
        let denied = resolve(&card, ApprovalDecision::Deny);
        assert!(matches!(
            denied,
            ApprovalCard::Resolved {
                approved: false,
                always: false
            }
        ));
        // 已决策卡幂等。
        assert_eq!(resolve(&denied, ApprovalDecision::Approve), denied);
    }
}
