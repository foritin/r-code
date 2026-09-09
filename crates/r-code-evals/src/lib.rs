//! 行为级评估框架（docs/pi-alignment PRD §4.1 R-EVL-01 / M2-01）。
//!
//! [`Harness`] 是"被评估系统"的抽象：一次 `run` 吃进 [`EvalInput`]（prompt +
//! 可选 fixture 工作区），产出 [`EvalRunResult`]（输出 / usage / 计时 / 事件）。
//!
//! T42：与宿主旧聊天执行链（Mock runtime + agent_send 脚本化）耦合的
//! `RCodeHarness` 已退役；v2 时代的被评估系统经
//! `r_code_runtime::ApplicationService` 装配（见 [`harness_tasks`] /
//! [`harness_conformance`]）。Judge（M2-02）与配对统计（M2-03）见
//! [`judge`] / [`table`]。

pub mod corpus;
pub mod harness_conformance;
pub mod harness_tasks;
pub mod judge;
pub mod table;

use std::path::PathBuf;

use async_trait::async_trait;
use r_code_core::dto::AgentEvent;

/// 一次评估输入。
#[derive(Debug, Clone)]
pub struct EvalInput {
    /// 输入标识：groupKey 优先取它（M2-03 A1）。
    pub id: String,
    /// 交给 agent 的指令。
    pub prompt: String,
    /// 可选 fixture 目录：复制进隔离工作区（None = 空工作区）。
    pub fixture: Option<PathBuf>,
}

impl EvalInput {
    pub fn new(id: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            prompt: prompt.into(),
            fixture: None,
        }
    }

    pub fn with_fixture(mut self, fixture: PathBuf) -> Self {
        self.fixture = Some(fixture);
        self
    }
}

/// 计时（毫秒粒度；评估不测微秒级性能）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvalTimings {
    pub wall_ms: u64,
}

/// 运行终止原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalStopReason {
    /// 任务收敛（Idle / ReviewReady），等价 stopReason == stop。
    Settled,
    /// 未收敛（预算耗尽 / 中止）——配对统计判失败。
    NotSettled(String),
    /// harness 自身错误（隔离违例等）。
    HarnessError(String),
}

impl EvalStopReason {
    pub fn is_settled(&self) -> bool {
        matches!(self, Self::Settled)
    }
}

/// 一次 run 的完整结果。
#[derive(Debug, Clone)]
pub struct EvalRunResult {
    pub harness: String,
    pub input_id: String,
    pub output: String,
    pub usage_json: Option<String>,
    pub timings: EvalTimings,
    pub events: Vec<AgentEvent>,
    pub stop_reason: EvalStopReason,
    /// 隔离工作区路径（Judge 检查改动面用）。
    pub workspace: PathBuf,
}

/// 被评估系统的抽象（PRD R-EVL-01：name + run -> { output, usage, timings, events }）。
#[async_trait]
pub trait Harness: Send + Sync {
    fn name(&self) -> &str;

    async fn run(&self, input: &EvalInput) -> Result<EvalRunResult, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M2-01.A1：Harness 抽象签名完整（name + run -> output/usage/timings/events）。
    #[test]
    fn harness_trait_surface_is_complete() {
        struct EchoHarness;
        #[async_trait]
        impl Harness for EchoHarness {
            fn name(&self) -> &str {
                "echo"
            }
            async fn run(&self, input: &EvalInput) -> Result<EvalRunResult, String> {
                Ok(EvalRunResult {
                    harness: self.name().to_string(),
                    input_id: input.id.clone(),
                    output: input.prompt.clone(),
                    usage_json: None,
                    timings: EvalTimings { wall_ms: 1 },
                    events: Vec::new(),
                    stop_reason: EvalStopReason::Settled,
                    workspace: std::env::temp_dir(),
                })
            }
        }
        let harness: Box<dyn Harness> = Box::new(EchoHarness);
        assert_eq!(harness.name(), "echo");
        // 输入构造合同：id/prompt/fixture 三件套。
        let input = EvalInput::new("case-1", "do it").with_fixture(PathBuf::from("."));
        assert_eq!(input.id, "case-1");
        assert_eq!(input.prompt, "do it");
        assert!(input.fixture.is_some());
    }

    /// stopReason 非 stop 判失败（NotSettled 不得计入 pass——table 侧统一处理）。
    #[test]
    fn non_settled_is_not_passed() {
        let reason = EvalStopReason::NotSettled("budget".to_string());
        assert!(!reason.is_settled());
        assert!(EvalStopReason::Settled.is_settled());
    }
}
