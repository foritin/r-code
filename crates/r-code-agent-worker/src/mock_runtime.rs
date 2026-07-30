//! Mock Agent Runtime -- 用于测试 / 开发的确定性后端。
//!
//! 使用场景引擎驱动事件脚本回放：预加载若干 `Vec<AgentEvent>` 序列，
//! 每次 `start_run` 回放下一个场景。不触及网络与真实 LLM。
//!
//! [doc-04 §2.2]

use async_trait::async_trait;
use hermes_core::{Session, SessionMeta};
use r_code_core::dto::{AgentEvent, CreateSessionInput, TaskState};
use r_code_core::error::ProductError;
use uuid::Uuid;

use crate::{AgentRuntime, SteerResult};

/// Mock Agent Runtime -- 确定性后端，用于测试 / 开发。
///
/// 使用场景引擎驱动事件脚本：预加载若干 `Vec<AgentEvent>` 序列，
/// 每次 `start_run` 回放下一个场景。
#[derive(Default)]
pub struct MockAgentRuntime {
    /// 预加载的事件序列
    scenarios: Vec<Vec<AgentEvent>>,
    /// 当前场景索引
    scenario_index: usize,
    /// 等待被 poll 的事件
    pending_events: Vec<AgentEvent>,
    /// 是否有 run 处于活跃
    is_running: bool,
    /// 是否请求了 abort
    aborted: bool,
}

impl MockAgentRuntime {
    /// 创建空的 mock runtime。
    pub fn new() -> Self {
        Self::default()
    }

    /// 推入一个场景（事件序列）以供回放。
    pub fn push_scenario(&mut self, events: Vec<AgentEvent>) {
        self.scenarios.push(events);
    }

    /// 推入一个简单文本场景。
    pub fn push_text_scenario(&mut self, text: &str) {
        self.push_scenario(vec![AgentEvent::Message {
            text: text.to_string(),
            delta: true,
        }]);
    }

    /// 推入一个工具调用场景（ToolCall + ToolResult）。
    pub fn push_tool_scenario(
        &mut self,
        tool_name: &str,
        input: serde_json::Value,
        output: serde_json::Value,
    ) {
        let call_id = Uuid::new_v4().to_string();
        self.push_scenario(vec![
            AgentEvent::ToolCall {
                name: tool_name.to_string(),
                input,
                call_id: call_id.clone(),
            },
            AgentEvent::ToolResult {
                call_id,
                output,
                is_error: false,
            },
        ]);
    }

    /// 推入一个错误场景（以非增量 Message 表达）。
    pub fn push_error_scenario(&mut self, error: &str) {
        self.push_scenario(vec![AgentEvent::Message {
            text: format!("[error] {error}"),
            delta: false,
        }]);
    }

    /// 是否有 run 处于活跃。
    pub fn is_running(&self) -> bool {
        self.is_running
    }

    /// 是否请求了 abort。
    pub fn aborted(&self) -> bool {
        self.aborted
    }

    /// 剩余未回放的场景数。
    pub fn remaining_scenarios(&self) -> usize {
        self.scenarios.len().saturating_sub(self.scenario_index)
    }
}

#[async_trait]
impl AgentRuntime for MockAgentRuntime {
    async fn create_session(&mut self, input: CreateSessionInput) -> Result<Session, ProductError> {
        let model = input.model.unwrap_or_else(|| "mock-sonnet".to_string());
        let meta = SessionMeta::new(model, "mock");
        Ok(Session::new(meta))
    }

    async fn start_run(&mut self, _session_id: &str, _goal: &str) -> Result<String, ProductError> {
        if self.scenario_index >= self.scenarios.len() {
            return Err(ProductError::Other(
                "mock runtime: no more scenarios to replay".to_string(),
            ));
        }
        let scenario = self.scenarios[self.scenario_index].clone();
        self.scenario_index += 1;
        self.pending_events.extend(scenario);
        self.is_running = true;
        self.aborted = false;
        Ok(Uuid::new_v4().to_string())
    }

    async fn steer(
        &mut self,
        _session_id: &str,
        message: &str,
    ) -> Result<SteerResult, ProductError> {
        if !self.is_running {
            return Ok(SteerResult::RunFinished);
        }
        self.pending_events.push(AgentEvent::Message {
            text: message.to_string(),
            delta: false,
        });
        Ok(SteerResult::Accepted)
    }

    async fn abort(&mut self, _session_id: &str) -> Result<(), ProductError> {
        self.aborted = true;
        self.is_running = false;
        self.pending_events.push(AgentEvent::State {
            state: TaskState::Interrupted,
        });
        Ok(())
    }

    async fn poll_events(&mut self) -> Result<Vec<AgentEvent>, ProductError> {
        Ok(std::mem::take(&mut self.pending_events))
    }
}

#[cfg(test)]
mod tests {
    use crate::AgentRuntime;
    use r_code_core::dto::{
        AgentEvent, CreateSessionInput, ProjectAccessMode, TaskMode, TaskState,
    };
    use r_code_core::error::ProductError;

    use super::MockAgentRuntime;

    fn input(model: Option<&str>) -> CreateSessionInput {
        CreateSessionInput {
            workspace_path: None,
            workspace_access_mode: ProjectAccessMode::RequestApproval,
            task_id: "task-1".to_string(),
            goal: "do thing".to_string(),
            mode: TaskMode::Ask,
            model: model.map(|s| s.to_string()),
            inference: Default::default(),
            context: vec![],
        }
    }

    #[tokio::test]
    async fn create_session_returns_mock_session() {
        let mut rt = MockAgentRuntime::new();
        let session = rt.create_session(input(Some("mock-model"))).await.unwrap();
        assert_eq!(session.meta.model, "mock-model");
        assert_eq!(session.meta.provider, "mock");
        assert!(session.messages.is_empty());
    }

    #[tokio::test]
    async fn create_session_defaults_model() {
        let mut rt = MockAgentRuntime::new();
        let session = rt.create_session(input(None)).await.unwrap();
        assert_eq!(session.meta.model, "mock-sonnet");
    }

    #[tokio::test]
    async fn text_scenario_replays() {
        let mut rt = MockAgentRuntime::new();
        rt.push_text_scenario("hello world");
        rt.start_run("s1", "g").await.unwrap();
        let events = rt.poll_events().await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::Message { text, delta } => {
                assert_eq!(text, "hello world");
                assert!(*delta);
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_scenario_replays() {
        let mut rt = MockAgentRuntime::new();
        rt.push_tool_scenario(
            "read_file",
            serde_json::json!({"path": "/a"}),
            serde_json::json!({"content": "hi"}),
        );
        rt.start_run("s1", "g").await.unwrap();
        let events = rt.poll_events().await.unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AgentEvent::ToolCall { .. }));
        match &events[1] {
            AgentEvent::ToolResult { is_error, .. } => assert!(!*is_error),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn error_scenario_replays() {
        let mut rt = MockAgentRuntime::new();
        rt.push_error_scenario("boom");
        rt.start_run("s1", "g").await.unwrap();
        let events = rt.poll_events().await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::Message { text, delta } => {
                assert!(text.contains("boom"));
                assert!(!*delta);
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn abort_sets_state_and_emits_state_event() {
        let mut rt = MockAgentRuntime::new();
        rt.push_text_scenario("x");
        rt.start_run("s1", "g").await.unwrap();
        assert!(rt.is_running());
        assert!(!rt.aborted());
        rt.abort("s1").await.unwrap();
        assert!(!rt.is_running());
        assert!(rt.aborted());
        let events = rt.poll_events().await.unwrap();
        // 1 from scenario + 1 State from abort
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[1],
            AgentEvent::State {
                state: TaskState::Interrupted
            }
        ));
    }

    #[tokio::test]
    async fn steer_injects_message() {
        let mut rt = MockAgentRuntime::new();
        rt.push_text_scenario("initial");
        rt.start_run("s1", "g").await.unwrap();
        rt.steer("s1", "wait, do this instead").await.unwrap();
        let events = rt.poll_events().await.unwrap();
        assert_eq!(events.len(), 2);
        match &events[1] {
            AgentEvent::Message { text, delta } => {
                assert_eq!(text, "wait, do this instead");
                assert!(!*delta);
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn poll_events_clears_pending() {
        let mut rt = MockAgentRuntime::new();
        rt.push_text_scenario("a");
        rt.start_run("s1", "g").await.unwrap();
        let first = rt.poll_events().await.unwrap();
        assert!(!first.is_empty());
        let second = rt.poll_events().await.unwrap();
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn start_run_without_scenario_errors() {
        let mut rt = MockAgentRuntime::new();
        let err = rt.start_run("s1", "g").await.unwrap_err();
        assert!(matches!(err, ProductError::Other(_)));
    }

    #[tokio::test]
    async fn multiple_scenarios_replay_in_order() {
        let mut rt = MockAgentRuntime::new();
        rt.push_text_scenario("first");
        rt.push_text_scenario("second");
        assert_eq!(rt.remaining_scenarios(), 2);

        rt.start_run("s1", "g").await.unwrap();
        let e1 = rt.poll_events().await.unwrap();
        assert_eq!(e1.len(), 1);
        match &e1[0] {
            AgentEvent::Message { text, .. } => assert_eq!(text, "first"),
            other => panic!("expected Message, got {other:?}"),
        }

        rt.start_run("s1", "g").await.unwrap();
        let e2 = rt.poll_events().await.unwrap();
        assert_eq!(e2.len(), 1);
        match &e2[0] {
            AgentEvent::Message { text, .. } => assert_eq!(text, "second"),
            other => panic!("expected Message, got {other:?}"),
        }
        assert_eq!(rt.remaining_scenarios(), 0);
    }

    #[tokio::test]
    async fn push_scenario_accepts_custom_events() {
        let mut rt = MockAgentRuntime::new();
        rt.push_scenario(vec![
            AgentEvent::Plan {
                steps: vec![r_code_core::dto::PlanStep {
                    description: "step 1".to_string(),
                    completed: false,
                }],
            },
            AgentEvent::State {
                state: TaskState::InProgress,
            },
        ]);
        rt.start_run("s1", "g").await.unwrap();
        let events = rt.poll_events().await.unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AgentEvent::Plan { .. }));
        assert!(matches!(
            events[1],
            AgentEvent::State {
                state: TaskState::InProgress
            }
        ));
    }
}
