//! 任务状态机与转换合法性校验。
//!
//! 状态机：`Idle -> Exploring -> InProgress -> ReviewReady -> Idle (accept/rollback)`，
//! 运行可从探索或执行阶段进入 `Interrupted`，之后可空闲或直接启动排队消息。
//! 任意状态可以 `-> Archived`。
//!
//! 硬性不变量：
//! - 一个 Task 任意时刻最多一个活跃主 Agent Run；其只读子 Agent 可作为该运行的受控子节点并行执行
//! - 未完成高风险 Tool Call 不能进入 ReviewReady 或 Accepted
//! - Archived 是终态，不可逆

use crate::dto::{ReviewState, TaskState};

/// 状态转换错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateTransitionError {
    /// 非法转换
    #[error("illegal transition: {from} -> {to}")]
    IllegalTransition {
        /// 起始状态
        from: TaskState,
        /// 目标状态
        to: TaskState,
    },
    /// 已归档，不可操作
    #[error("task is archived, no further transitions allowed")]
    Archived,
    /// 存在活跃 Run，不能开始新 Run
    #[error("task already has an active run")]
    ActiveRunExists,
    /// 存在未完成高风险调用
    #[error("task has pending high-risk tool calls, cannot enter review_ready")]
    PendingToolCalls,
}

/// 检查状态转换是否合法。
///
/// 合法转换：
/// - `Idle -> Exploring` (开始探索)
/// - `Exploring -> InProgress` (开始执行)
/// - `InProgress -> ReviewReady` (完成一轮，待审查)
/// - `Exploring/InProgress -> Interrupted` (用户中止)
/// - `Interrupted -> Idle/InProgress` (中止后收尾或分发排队消息)
/// - `ReviewReady -> Idle` (接受/回滚后回到空闲)
/// - `Idle/Exploring/InProgress/ReviewReady -> Archived` (归档)
/// - `Archived -> ` (终态，不可转换)
pub fn validate_transition(from: TaskState, to: TaskState) -> Result<(), StateTransitionError> {
    if from == TaskState::Archived {
        return Err(StateTransitionError::Archived);
    }

    let valid = match (from, to) {
        // 同状态不算转换
        (TaskState::Idle, TaskState::Idle) => true,
        // 正常流程
        (TaskState::Idle, TaskState::Exploring) => true,
        (TaskState::Exploring, TaskState::InProgress) => true,
        (TaskState::InProgress, TaskState::ReviewReady) => true,
        (TaskState::ReviewReady, TaskState::Idle) => true,
        (TaskState::Exploring, TaskState::Interrupted)
        | (TaskState::InProgress, TaskState::Interrupted)
        | (TaskState::Interrupted, TaskState::Idle)
        | (TaskState::Interrupted, TaskState::InProgress)
        | (TaskState::Idle, TaskState::Interrupted) => true,
        // 回退（取消）
        (TaskState::Exploring, TaskState::Idle) => true,
        (TaskState::InProgress, TaskState::Idle) => true,
        // 归档（任意非终态 -> Archived）
        (_, TaskState::Archived) => true,
        // 其他全部非法
        _ => false,
    };

    if valid {
        Ok(())
    } else {
        Err(StateTransitionError::IllegalTransition { from, to })
    }
}

/// 执行状态转换，返回新状态或错误。
pub fn transition(from: TaskState, to: TaskState) -> Result<TaskState, StateTransitionError> {
    validate_transition(from, to)?;
    Ok(to)
}

/// 检查 Agent Run 审查状态转换是否合法。
///
/// 合法转换：
/// - `Pending -> Accepted` (用户接受)
/// - `Pending -> AutoAccepted` (验证通过自动接受)
/// - `Pending -> RolledBack` (用户回滚)
/// - `Pending -> Aborted` (用户中止)
/// - `Pending -> Answered` (Ask 模式零变化自动结算)
/// - `Pending -> Failed` (运行失败)
/// - 终态不可转换
pub fn validate_review_transition(
    from: ReviewState,
    to: ReviewState,
) -> Result<(), StateTransitionError> {
    if from.is_terminal() {
        return Err(StateTransitionError::IllegalTransition {
            from: TaskState::ReviewReady, // 近似表示
            to: TaskState::ReviewReady,
        });
    }

    let valid = matches!(
        (from, to),
        (ReviewState::Pending, ReviewState::Accepted)
            | (ReviewState::Pending, ReviewState::AutoAccepted)
            | (ReviewState::Pending, ReviewState::RolledBack)
            | (ReviewState::Pending, ReviewState::Aborted)
            | (ReviewState::Pending, ReviewState::Answered)
            | (ReviewState::Pending, ReviewState::Failed)
    );

    if valid {
        Ok(())
    } else {
        Err(StateTransitionError::IllegalTransition {
            from: TaskState::ReviewReady,
            to: TaskState::ReviewReady,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_legal_transitions() {
        assert!(validate_transition(TaskState::Idle, TaskState::Exploring).is_ok());
        assert!(validate_transition(TaskState::Exploring, TaskState::InProgress).is_ok());
        assert!(validate_transition(TaskState::InProgress, TaskState::ReviewReady).is_ok());
        assert!(validate_transition(TaskState::ReviewReady, TaskState::Idle).is_ok());
    }

    #[test]
    fn test_cancel_transitions() {
        assert!(validate_transition(TaskState::Exploring, TaskState::Idle).is_ok());
        assert!(validate_transition(TaskState::InProgress, TaskState::Idle).is_ok());
        assert!(validate_transition(TaskState::InProgress, TaskState::Interrupted).is_ok());
        assert!(validate_transition(TaskState::Interrupted, TaskState::InProgress).is_ok());
    }

    #[test]
    fn test_archive_from_any_non_terminal() {
        assert!(validate_transition(TaskState::Idle, TaskState::Archived).is_ok());
        assert!(validate_transition(TaskState::Exploring, TaskState::Archived).is_ok());
        assert!(validate_transition(TaskState::InProgress, TaskState::Archived).is_ok());
        assert!(validate_transition(TaskState::ReviewReady, TaskState::Archived).is_ok());
    }

    #[test]
    fn test_archived_is_terminal() {
        assert!(matches!(
            validate_transition(TaskState::Archived, TaskState::Idle),
            Err(StateTransitionError::Archived)
        ));
        assert!(matches!(
            validate_transition(TaskState::Archived, TaskState::Archived),
            Err(StateTransitionError::Archived)
        ));
    }

    #[test]
    fn test_illegal_transitions() {
        // 不能跳过 Exploring
        assert!(validate_transition(TaskState::Idle, TaskState::InProgress).is_err());
        // 不能从 ReviewReady 直接进入 InProgress
        assert!(validate_transition(TaskState::ReviewReady, TaskState::InProgress).is_err());
        // 不能从 Archived 恢复
        assert!(validate_transition(TaskState::Archived, TaskState::Idle).is_err());
    }

    #[test]
    fn test_review_transitions() {
        assert!(validate_review_transition(ReviewState::Pending, ReviewState::Accepted).is_ok());
        assert!(
            validate_review_transition(ReviewState::Pending, ReviewState::AutoAccepted).is_ok()
        );
        assert!(validate_review_transition(ReviewState::Pending, ReviewState::RolledBack).is_ok());
        assert!(validate_review_transition(ReviewState::Pending, ReviewState::Aborted).is_ok());
        assert!(validate_review_transition(ReviewState::Pending, ReviewState::Answered).is_ok());
    }

    #[test]
    fn test_review_terminal_cannot_transition() {
        assert!(validate_review_transition(ReviewState::Accepted, ReviewState::Pending).is_err());
        assert!(
            validate_review_transition(ReviewState::RolledBack, ReviewState::Accepted).is_err()
        );
    }

    #[test]
    fn test_transition_returns_new_state() {
        let new_state = transition(TaskState::Idle, TaskState::Exploring).unwrap();
        assert_eq!(new_state, TaskState::Exploring);
    }
}
