//! 崩溃恢复 -- 扫描中断的 agent run。
//!
//! Worker 崩溃后，可能留下 `ended_at IS NULL` 的 agent run 记录。
//! 本模块负责扫描这些孤儿 run，供上层决定恢复 / 标记失败 / 重新调度。
//!
//! [doc-04 §3.3]

use r_code_core::dto::TaskState;

/// 一个 task 的恢复状态（worker 崩溃后重建）。
#[derive(Debug, Clone)]
pub struct RecoveryState {
    /// 所属 Task ID
    pub task_id: String,
    /// 关联的 Session ID
    pub session_id: String,
    /// 崩溃前的最后状态
    pub last_state: TaskState,
    /// 是否在执行中被中断
    pub interrupted: bool,
}

/// 扫描孤儿 / 中断的 run。
///
/// 正常实现会查询数据库中 `ended_at IS NULL` 的 agent run 记录。
/// 当前为桩实现，返回空列表（R4 阶段：存储层接入后补全）。
pub fn scan_orphaned_runs() -> Vec<RecoveryState> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use r_code_core::dto::TaskState;

    use super::{scan_orphaned_runs, RecoveryState};

    #[test]
    fn scan_orphaned_runs_stub_returns_empty() {
        let runs = scan_orphaned_runs();
        assert!(runs.is_empty());
    }

    #[test]
    fn recovery_state_constructs() {
        let rs = RecoveryState {
            task_id: "t1".to_string(),
            session_id: "s1".to_string(),
            last_state: TaskState::InProgress,
            interrupted: true,
        };
        assert_eq!(rs.task_id, "t1");
        assert_eq!(rs.session_id, "s1");
        assert!(rs.interrupted);
        assert_eq!(rs.last_state, TaskState::InProgress);
    }

    #[test]
    fn recovery_state_is_clone() {
        let rs = RecoveryState {
            task_id: "t1".to_string(),
            session_id: "s1".to_string(),
            last_state: TaskState::Exploring,
            interrupted: false,
        };
        let cloned = rs.clone();
        assert_eq!(rs.task_id, cloned.task_id);
        assert_eq!(rs.last_state, cloned.last_state);
    }
}
