//! Extension 生命周期事件面与工具注册桥（docs/pi-alignment PRD §4.1 R-EXT-03 / M4-03）。
//!
//! 扩展事件面从 `AgentEvent` 订阅面**派生**（不新造事件源）：宿主把已订阅的
//! agent 事件流投喂 [`ExtensionHost::dispatch`]，扩展按生命周期钩子收到语义化
//! 回调（session_start / tool_before / tool_after / agent_settled）。
//!
//! 扩展注册的自定义工具经 `ToolGateway` 同源入口注册（`register_guarded`）——
//! R3/R4 审批矩阵、PathGuard、审计记账对扩展工具与内置工具完全一致；
//! **扩展不能绕过安全边界**：注册只是把工具放进同一审批链，不经审批的
//! 直连执行在本设计中不存在。

use std::sync::Arc;

use async_trait::async_trait;
use r_code_core::dto::{AgentEvent, RiskLevel};
use r_code_core::error::ProductError;
use r_code_gateway::{PathBinding, Tool, ToolGateway};
use serde_json::Value;

/// 扩展可观察的生命周期事件（从 AgentEvent 派生的语义面）。
#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionEvent {
    /// 会话启动（首个 run 启动）。
    SessionStart { session_id: String },
    /// 工具调用前（审批通过、执行前）。
    ToolBefore { name: String, input: Value },
    /// 工具调用后（含结果错误标记）。
    ToolAfter { name: String, is_error: bool },
    /// agent 运行收敛。
    AgentSettled { session_id: String },
}

/// AgentEvent → ExtensionEvent 派生（不可派生的变体返回 None）。
///
/// 生命周期锚点（TaskEventType 的事件在 task 事件流，不在 AgentEvent）：
/// - session_start ← 首次 `State { Idle → … }` 之后的首个 `Activity`（请求开始）；
///   简化为 `Activity { Requesting }` 首达（宿主在 run 启动时总会发它）；
/// - tool_before ← `ToolCall`；
/// - tool_after ← `ToolResult`；
/// - agent_settled ← `State { Idle | ReviewReady }`（运行收敛）。
pub fn derive_extension_event(session_id: &str, event: &AgentEvent) -> Option<ExtensionEvent> {
    use r_code_core::dto::{AgentActivityPhase, TaskState};
    match event {
        AgentEvent::Activity {
            phase: AgentActivityPhase::Requesting,
            ..
        } => Some(ExtensionEvent::SessionStart {
            session_id: session_id.to_string(),
        }),
        AgentEvent::ToolCall { name, input, .. } => Some(ExtensionEvent::ToolBefore {
            name: name.clone(),
            input: input.clone(),
        }),
        AgentEvent::ToolResult { is_error, .. } => Some(ExtensionEvent::ToolAfter {
            name: String::new(),
            is_error: *is_error,
        }),
        AgentEvent::State {
            state: TaskState::Idle | TaskState::ReviewReady,
        } => Some(ExtensionEvent::AgentSettled {
            session_id: session_id.to_string(),
        }),
        _ => None,
    }
}

/// 扩展上下文（生命周期回调的注入面）。
pub struct ExtensionContext<'a> {
    pub session_id: &'a str,
}

/// 扩展生命周期回调 trait（钩子有默认空实现——扩展按需覆盖）。
pub trait Extension: Send + Sync {
    fn id(&self) -> &str;

    fn on_event(&self, _context: &ExtensionContext<'_>, _event: &ExtensionEvent) {}
}

/// 经 Gateway 同源注册的扩展工具（注册即进审批链；风险等级必须显式声明——
/// 注册面不提供"免审批"通道）。
pub struct ExtensionTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub risk_level: RiskLevel,
    pub handler: Arc<dyn Fn(Value) -> Result<String, ProductError> + Send + Sync>,
}

#[async_trait]
impl Tool for ExtensionTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn risk_level(&self) -> RiskLevel {
        self.risk_level
    }

    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }

    async fn execute(&self, input: Value) -> Result<String, ProductError> {
        (self.handler)(input)
    }
}

/// 扩展宿主：事件派生分发 + 工具经 Gateway 注册。
pub struct ExtensionHost {
    extensions: Vec<Arc<dyn Extension>>,
}

impl ExtensionHost {
    pub fn new() -> Self {
        Self {
            extensions: Vec::new(),
        }
    }

    /// 挂载扩展（仅事件面；不隐含任何工具）。
    pub fn attach(&mut self, extension: Arc<dyn Extension>) {
        self.extensions.push(extension);
    }

    /// 注册扩展工具：经 ToolGateway::register_guarded 同源入口（R0-R4 审批
    /// 矩阵对扩展工具与内置工具一致）。名字冲突返回 None（拒绝注册）；成功
    /// 返回 EffectGuard——**调用方必须持有 guard**（guard Drop 即注销，与
    /// 内置 guarded 工具同一生命周期语义），扩展卸载时 drop 即可。
    pub fn register_tool(
        gateway: &mut ToolGateway,
        tool: ExtensionTool,
    ) -> Option<r_code_gateway::EffectGuard> {
        if gateway
            .tool_specs()
            .iter()
            .any(|spec| spec.name == tool.name)
        {
            return None;
        }
        Some(gateway.register_guarded(Arc::new(tool)))
    }

    /// 事件派发（宿主 drain 循环调用；派生失败静默——不可派生变体不打扰扩展）。
    pub fn dispatch(&self, session_id: &str, event: &AgentEvent) {
        let Some(derived) = derive_extension_event(session_id, event) else {
            return;
        };
        let context = ExtensionContext { session_id };
        for extension in &self.extensions {
            extension.on_event(&context, &derived);
        }
    }

    pub fn extension_count(&self) -> usize {
        self.extensions.len()
    }
}

impl Default for ExtensionHost {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_gateway::PermissionEngine;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn recording_extension(counter: Arc<AtomicUsize>) -> Arc<dyn Extension> {
        struct Recorder(Arc<AtomicUsize>);
        impl Extension for Recorder {
            fn id(&self) -> &str {
                "recorder"
            }
            fn on_event(&self, _context: &ExtensionContext<'_>, _event: &ExtensionEvent) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        Arc::new(Recorder(counter))
    }

    /// M4-03.A1：事件面完整——四生命周期钩子从 AgentEvent 派生并分发。
    #[test]
    fn lifecycle_events_derive_and_dispatch() {
        let seen = Arc::new(AtomicUsize::new(0));
        let mut host = ExtensionHost::new();
        host.attach(recording_extension(seen.clone()));
        assert_eq!(host.extension_count(), 1);

        use r_code_core::dto::{AgentActivityPhase, TaskState};
        host.dispatch(
            "s1",
            &AgentEvent::Activity {
                phase: AgentActivityPhase::Requesting,
                detail: None,
            },
        );
        host.dispatch(
            "s1",
            &AgentEvent::ToolCall {
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
                call_id: "c1".into(),
            },
        );
        host.dispatch(
            "s1",
            &AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: serde_json::json!("ok"),
                is_error: false,
            },
        );
        // 不可派生变体不分发。
        host.dispatch(
            "s1",
            &AgentEvent::Message {
                text: "hi".into(),
                delta: false,
            },
        );
        assert_eq!(seen.load(Ordering::Relaxed), 3);
        // 收敛事件派生 agent_settled。
        host.dispatch(
            "s1",
            &AgentEvent::State {
                state: TaskState::Idle,
            },
        );
        assert_eq!(seen.load(Ordering::Relaxed), 4);
        // 派生正确性：ToolBefore 携带名与入参。
        let derived = derive_extension_event(
            "s1",
            &AgentEvent::ToolCall {
                name: "bash".into(),
                input: serde_json::json!({"command": "x"}),
                call_id: "c".into(),
            },
        )
        .unwrap();
        assert!(matches!(
            derived,
            ExtensionEvent::ToolBefore { ref name, .. } if name == "bash"
        ));
    }

    /// M4-03.A2：扩展工具经 Gateway 同源注册（同名拒绝；注册后走同一 tool_specs 面）。
    #[test]
    fn extension_tools_register_through_gateway() {
        let gateway = ToolGateway::new(Arc::new(PermissionEngine::default()));
        let tool = ExtensionTool {
            name: "ext_probe".to_string(),
            description: "test extension tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            risk_level: RiskLevel::R3,
            handler: Arc::new(|_| Ok("done".to_string())),
        };
        let mut gateway = gateway;
        let guard = ExtensionHost::register_tool(&mut gateway, tool).expect("首次注册必须成功");
        // tool_specs（模型可见目录）出现扩展工具——与内置工具同一面。
        assert!(gateway
            .tool_specs()
            .iter()
            .any(|spec| spec.name == "ext_probe"));
        // 同名冲突：拒绝（劫持/覆盖既有工具面没有通道）。
        let conflict = ExtensionTool {
            name: "ext_probe".to_string(),
            description: "hijack".to_string(),
            input_schema: serde_json::json!({}),
            risk_level: RiskLevel::R0,
            handler: Arc::new(|_| Ok(String::new())),
        };
        assert!(ExtensionHost::register_tool(&mut gateway, conflict).is_none());
        // guard Drop 即注销（扩展卸载语义）。
        drop(guard);
        assert!(!gateway
            .tool_specs()
            .iter()
            .any(|spec| spec.name == "ext_probe"));
    }

    /// M4-03.A3：R3/R4 不绕过——扩展工具的风险等级进入审批矩阵（分级执行路径）。
    #[tokio::test]
    async fn extension_tools_go_through_approval_matrix() {
        use r_code_gateway::classify_shell_command;
        let mut gateway = ToolGateway::new(Arc::new(PermissionEngine::default()));
        // 分类器红线语义不因扩展注册改变：sudo 恒 R4。
        assert_eq!(classify_shell_command("sudo rm -rf /").level, RiskLevel::R4);
        // 扩展工具声明 R3：执行必须走审批检查（未授权上下文 → 权限错误而非直接执行）。
        let tool = ExtensionTool {
            name: "ext_risky".to_string(),
            description: "risky ext tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            risk_level: RiskLevel::R3,
            handler: Arc::new(|_| Ok("should not run without approval".to_string())),
        };
        let _guard = ExtensionHost::register_tool(&mut gateway, tool).expect("注册必须成功");
        let outcome = gateway
            .execute_call("task-x", "run-x", "ext_risky", serde_json::json!({}), None)
            .await;
        assert!(outcome.is_err(), "R3 扩展工具不得绕过审批直连执行");
    }
}
