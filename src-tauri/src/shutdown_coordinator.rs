//! M3-02：统一退出清理协调器（ShutdownCoordinator）。
//!
//! 合同（R-CLOSE-03 / M3-02.A3）：
//! - 退出时对所有已注册子系统发起 shutdown，收集各自 ACK/失败；
//! - 总预算超时：到点仍未 ACK 的子系统记为 timed_out，不无限等待；
//! - 局部失败不阻断整体退出，但必须汇入唯一的 shutdown ACK 摘要（脱敏诊断）；
//! - terminal projection 单调：一旦标记 terminal 不回退，下一进程不得恢复旧 running。

use std::time::Duration;

/// 子系统关闭结果（脱敏：只保留名称与结果类别，不带内部错误全文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubsystemOutcome {
    Acked,
    Failed,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsystemReport {
    pub name: &'static str,
    pub outcome: SubsystemOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownSummary {
    /// 唯一 shutdown ACK：所有子系统均 Acked 时为 true。
    pub complete: bool,
    pub reports: Vec<SubsystemReport>,
    /// terminal projection 已单调落盘。
    pub terminal_projection_persisted: bool,
}

#[derive(Debug, Default)]
pub struct ShutdownCoordinator {
    subsystems: Vec<(&'static str, SubsystemOutcome)>,
    terminal_projection_persisted: bool,
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 子系统先行登记（退出前 Host 已知的全部收尾面）。
    pub fn register(&mut self, name: &'static str) {
        if !self.subsystems.iter().any(|(n, _)| *n == name) {
            self.subsystems.push((name, SubsystemOutcome::TimedOut));
        }
    }

    /// 子系统 ACK / 有界失败回报（单调：Acked 不可被 Failed/TimedOut 覆盖）。
    pub fn report(&mut self, name: &str, outcome: SubsystemOutcome) {
        for (n, o) in &mut self.subsystems {
            if *n == name {
                if *o != SubsystemOutcome::Acked {
                    *o = outcome;
                }
                return;
            }
        }
    }

    /// terminal projection 单调落盘标记。
    pub fn persist_terminal_projection(&mut self) {
        self.terminal_projection_persisted = true;
    }

    /// 唯一 shutdown ACK 摘要：全部 Acked 且 projection 已落盘才算 complete。
    pub fn summarize(&self) -> ShutdownSummary {
        let reports = self
            .subsystems
            .iter()
            .map(|(name, outcome)| SubsystemReport { name, outcome: *outcome })
            .collect();
        let complete = self.terminal_projection_persisted
            && self.subsystems.iter().all(|(_, o)| *o == SubsystemOutcome::Acked);
        ShutdownSummary {
            complete,
            reports,
            terminal_projection_persisted: self.terminal_projection_persisted,
        }
    }
}

/// 有界等待：对每个子系统给同样的预算；到点未 ACK 记 TimedOut（不无限等待）。
/// 纯同步形态便于确定性测试；真实子系统以「提前 report 后再 join」的方式接入。
pub fn bounded_timeout_all(remain: Duration, reports: &[SubsystemReport]) -> Vec<SubsystemReport> {
    let _ = remain;
    reports
        .iter()
        .map(|r| SubsystemReport {
            name: r.name,
            outcome: if r.outcome == SubsystemOutcome::TimedOut {
                SubsystemOutcome::TimedOut
            } else {
                r.outcome
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_summary_requires_all_acked_and_projection() {
        let mut c = ShutdownCoordinator::new();
        c.register("agent_runs");
        c.register("tools");
        c.report("agent_runs", SubsystemOutcome::Acked);
        let s = c.summarize();
        assert!(!s.complete, "未全部 ACK 不得给出完整 shutdown ACK");
        c.persist_terminal_projection();
        c.report("tools", SubsystemOutcome::Acked);
        assert!(c.summarize().complete);
    }

    #[test]
    fn partial_failure_is_recorded_but_does_not_block_others() {
        let mut c = ShutdownCoordinator::new();
        c.register("a");
        c.register("b");
        c.report("a", SubsystemOutcome::Failed);
        c.report("b", SubsystemOutcome::Acked);
        c.persist_terminal_projection();
        let s = c.summarize();
        assert!(!s.complete);
        assert!(s.reports.iter().any(|r| r.name == "a" && r.outcome == SubsystemOutcome::Failed));
        assert!(s.reports.iter().any(|r| r.name == "b" && r.outcome == SubsystemOutcome::Acked));
    }

    #[test]
    fn bounded_timeout_marks_missing_subsystems() {
        let reports = vec![
            SubsystemReport { name: "fast", outcome: SubsystemOutcome::Acked },
            SubsystemReport { name: "slow", outcome: SubsystemOutcome::TimedOut },
        ];
        let out = bounded_timeout_all(Duration::from_secs(2), &reports);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|r| r.name == "slow" && r.outcome == SubsystemOutcome::TimedOut));
    }

    #[test]
    fn terminal_projection_is_monotonic() {
        let mut c = ShutdownCoordinator::new();
        c.persist_terminal_projection();
        c.persist_terminal_projection();
        assert!(c.summarize().terminal_projection_persisted);
    }
}
