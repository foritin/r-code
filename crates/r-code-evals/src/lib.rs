//! 行为级评估框架（docs/pi-alignment PRD §4.1 R-EVL-01 / M2-01）。
//!
//! [`Harness`] 是"被评估系统"的抽象：一次 `run` 吃进 [`EvalInput`]（prompt +
//! 可选 fixture 工作区），产出 [`EvalRunResult`]（输出 / usage / 计时 / 事件）。
//! [`create_r_code_harness`] 返回与生产同一套装配的 R-Code harness：
//!
//! - **同源工厂**：`CommandState::new_with_planning_release_control`（与桌面进程、
//!   plan_eval 相同的装配入口），隔离临时 env（config/db/sessions/blobs/project
//!   全部位于操作系统临时目录）；
//! - **thinkingLevel 固定 off**：每次 run 前经 `task_set_inference` 钉死
//!   `thinking = disabled`（评估不测推理档位，防止默认值漂移污染配对差值）；
//! - **隔离检查硬断言**：run 开始前校验隔离环境（无 MCP server 加载、目录全部
//!   在临时 env 内、config 零 provider）；违例直接 [`Err`]（硬断言，不降级为
//!   扣分）；
//! - **可复现性**：默认 Mock runtime + 脚本化场景（评估管线的确定性等价物，
//!   仍走完整 host 管线：task_create → agent_send → drain → settle → detail）。
//!   真实模型运行属外部放行（§11.3），不在实现验收面内。
//!
//! Judge（M2-02）与配对统计（M2-03）见 [`judge`] / [`table`]。

pub mod corpus;
pub mod judge;
pub mod table;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use r_code_core::dto::{AgentEvent, TaskState};
use r_code_host::commands::{
    agent_send, install_mock_scenario, task_create, task_detail, task_set_inference, CommandState,
};
use r_code_host::plan_policy::{PlanningReleaseControl, PlanningReleaseState};

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
    /// 隔离工作区路径（Judge 检查改动面用）。env 由 harness 保留到 Drop。
    pub workspace: PathBuf,
}

/// 被评估系统的抽象（PRD R-EVL-01：name + run -> { output, usage, timings, events }）。
#[async_trait]
pub trait Harness: Send + Sync {
    fn name(&self) -> &str;

    async fn run(&self, input: &EvalInput) -> Result<EvalRunResult, String>;
}

/// 隔离检查硬断言失败（无意外扩展加载即抛错——不降级、不记分）。
#[derive(Debug)]
pub struct IsolationViolation(pub String);

impl std::fmt::Display for IsolationViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "eval isolation violation: {}", self.0)
    }
}

/// 脚本化场景源：按输入产出 Mock runtime 的事件脚本。
pub type ScenarioSource = Arc<dyn Fn(&EvalInput) -> Vec<AgentEvent> + Send + Sync>;

/// 与生产同一套装配的 R-Code harness（Mock runtime + 脚本化场景）。
pub struct RCodeHarness {
    scenario: ScenarioSource,
    /// run 预算（秒）；超时判 NotSettled 而非永久等待。
    settle_budget_secs: u64,
    /// 隔离 env（保留到 harness Drop：Judge 可能还要读工作区）。
    env: Mutex<Vec<tempfile::TempDir>>,
}

pub fn create_r_code_harness(scenario: ScenarioSource) -> RCodeHarness {
    RCodeHarness {
        scenario,
        settle_budget_secs: 300,
        env: Mutex::new(Vec::new()),
    }
}

/// 便捷场景源：恒定回复（不依赖输入）。
pub fn constant_reply_scenario(text: impl Into<String>) -> ScenarioSource {
    let text = Arc::new(text.into());
    Arc::new(move |_| {
        vec![AgentEvent::Message {
            text: (*text).clone(),
            delta: false,
        }]
    })
}

/// 评估 control：全部关闭（评估不启用 Plan 双轨，不读桌面生产状态）。
fn eval_release_control() -> PlanningReleaseControl {
    PlanningReleaseControl {
        provider_kind: "eval".to_string(),
        release_state: PlanningReleaseState::Off,
        emergency_off: false,
        eligibility_profile_version: String::new(),
        evidence_version: String::new(),
        allowed_models: Vec::new(),
        allowed_protocols: Vec::new(),
        allowed_endpoint_classes: Vec::new(),
        basis: "behavioral eval harness bootstrap".to_string(),
    }
}

/// 评估 config：零 provider、MCP 显式清空——mock runtime 不需要 provider 配置，
/// 隔离检查据此判定"无意外加载"。内置 research server 会被 McpSettingsService
/// 的 reconcile_builtin 无条件补回（禁用态），因此显式写 disabled 覆盖。
fn write_eval_config(config_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(config_dir)
        .map_err(|error| format!("create eval config dir: {error}"))?;
    std::fs::write(
        config_dir.join("config.toml"),
        "# eval harness: intentionally empty (mock runtime, no providers/mcp)\n",
    )
    .map_err(|error| format!("write eval config: {error}"))?;
    // reconcile_builtin 对缺失的内置 server 会补 enabled=false 的规范条目；
    // 先写 enabled=false，load 后状态稳定且不会拉起任何进程。
    std::fs::write(
        config_dir.join("mcp-servers.toml"),
        "[[servers]]\nid = \"r-code-research\"\nenabled = false\n",
    )
    .map_err(|error| format!("write eval mcp settings: {error}"))?;
    Ok(())
}

fn isolated_state(env_root: &Path) -> Result<CommandState, String> {
    let config_dir = env_root.join("config");
    write_eval_config(&config_dir)?;
    // CommandState 不负责目录预创建；plan_eval 同样先建目录再装配。
    for dir in ["blobs", "sessions", "project"] {
        std::fs::create_dir_all(env_root.join(dir))
            .map_err(|error| format!("create eval {dir}: {error}"))?;
    }
    let db = r_code_store::Database::open(env_root.join("app.db"))
        .map_err(|error| format!("open eval db: {error}"))?;
    Ok(CommandState::new_with_planning_release_control(
        Arc::new(db),
        env_root.join("blobs"),
        env_root.join("sessions"),
        config_dir,
        env_root.join("project"),
        Some(env_root.join("app.db")),
        eval_release_control(),
    ))
}

/// 隔离检查硬断言（R-EVL-01：无意外扩展加载即抛错）。
async fn assert_isolation(state: &CommandState, env_root: &Path) -> Result<(), IsolationViolation> {
    let fail = |message: String| Err(IsolationViolation(message));
    // 1) MCP：评估环境不得加载（enabled）任何 server。内置 r-code-research
    //    会被 McpSettingsService 无条件补回（禁用态、不拉进程），禁用态允许；
    //    其余 server 一律视为意外加载。
    let snapshot = state.mcp_manager.snapshot().await;
    let enabled: Vec<&str> = snapshot
        .servers
        .iter()
        .filter(|server| server.enabled)
        .map(|server| server.id.as_str())
        .collect();
    if !enabled.is_empty() {
        return fail(format!("mcp servers loaded in eval env: {enabled:?}"));
    }
    // 2) 目录隔离：config/sessions 必须位于本次临时 env 内。
    for (label, path) in [
        ("config_dir", &state.config_dir),
        ("sessions_dir", &state.sessions_dir),
    ] {
        if !path.starts_with(env_root) {
            return fail(format!("{label} escapes eval env root: {}", path.display()));
        }
    }
    // 3) config 零 provider（真实 provider 配置泄漏 = 隔离破坏）。
    let config_text = std::fs::read_to_string(state.config_dir.join("config.toml"))
        .map_err(|error| IsolationViolation(format!("read eval config: {error}")))?;
    if config_text.contains("[providers") {
        return fail("[providers] table leaked into eval config".to_string());
    }
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|error| format!("mkdir: {error}"))?;
    for entry in std::fs::read_dir(source).map_err(|error| format!("readdir: {error}"))? {
        let entry = entry.map_err(|error| format!("readdir entry: {error}"))?;
        let destination = target.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            copy_dir_recursive(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), &destination)
                .map_err(|error| format!("copy {}: {error}", entry.path().display()))?;
        }
    }
    Ok(())
}

#[async_trait]
impl Harness for RCodeHarness {
    fn name(&self) -> &str {
        "r-code"
    }

    async fn run(&self, input: &EvalInput) -> Result<EvalRunResult, String> {
        let env = tempfile::Builder::new()
            .prefix("r-code-eval-")
            .tempdir()
            .map_err(|error| format!("create eval env: {error}"))?;
        let env_root = env.path().to_path_buf();
        let workspace = env_root.join("workspace");
        if let Some(fixture) = &input.fixture {
            copy_dir_recursive(fixture, &workspace)?;
        } else {
            std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        }

        let state = isolated_state(&env_root)?;
        // 注意：不调用 enable_real_mode —— 评估默认 Mock runtime（脚本化、可复现）。

        // 隔离硬断言（违例即 Err，不产生结果）。
        assert_isolation(&state, &env_root)
            .await
            .map_err(|violation| violation.to_string())?;

        // 事件捕获：进程内 sink 转存事件（emit_agent_event 广播面）。
        let captured: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_target = captured.clone();
        state.set_agent_event_sink(Arc::new(move |_task_id, event| {
            sink_target.lock().unwrap().push(event.clone());
        }));

        let started = Instant::now();
        let task = task_create(
            &state,
            None,
            &format!("eval {}", input.id),
            &input.prompt,
            "ask",
        )
        .await
        .map_err(|error| format!("task_create: {error}"))?;
        // thinkingLevel 固定 off（R-EVL-01：评估不测推理档位）。
        task_set_inference(
            &state,
            &task.id,
            agent_contract::InferenceOptions {
                thinking: Some("disabled".to_string()),
                reasoning_effort: None,
                verbosity: None,
            },
        )
        .await
        .map_err(|error| format!("task_set_inference(thinking off): {error}"))?;

        // 脚本化场景注入（host 侧 Mock runtime；真实线路拒绝脚本化）。
        let scenario = (self.scenario)(input);
        if !install_mock_scenario(&state, &task.id, scenario).await? {
            return Err(format!(
                "install_mock_scenario: task {} runtime is not mock (real runtime must not be scripted)",
                task.id
            ));
        }

        agent_send(&state, &task.id, &input.prompt)
            .await
            .map_err(|error| format!("agent_send: {error}"))?;

        // 等待收敛（秒级轮询；预算内未收敛 = NotSettled）。
        let mut settled = false;
        for _ in 0..self.settle_budget_secs {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let detail = task_detail(&state, &task.id)
                .await
                .map_err(|error| format!("task_detail: {error}"))?;
            if matches!(detail.task.state, TaskState::Idle | TaskState::ReviewReady) {
                settled = true;
                break;
            }
        }
        let wall_ms = started.elapsed().as_millis() as u64;

        let detail = task_detail(&state, &task.id)
            .await
            .map_err(|error| format!("task_detail: {error}"))?;
        let usage_json = detail.runs.iter().find_map(|run| run.usage_json.clone());
        let events = captured.lock().unwrap().clone();
        // 输出 = 最后一条完整（delta=false）assistant 文本事件。
        let output = events
            .iter()
            .rev()
            .find_map(|event| match event {
                AgentEvent::Message { text, delta: false } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let stop_reason = if settled {
            EvalStopReason::Settled
        } else {
            EvalStopReason::NotSettled("settle budget exhausted".to_string())
        };

        // env 移交给 harness 持有（Judge 事后可读工作区；harness Drop 时统一清理）。
        self.env.lock().unwrap().push(env);
        Ok(EvalRunResult {
            harness: self.name().to_string(),
            input_id: input.id.clone(),
            output,
            usage_json,
            timings: EvalTimings { wall_ms },
            events,
            stop_reason,
            workspace,
        })
    }
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

    /// M2-01.A2/A3：隔离检查硬断言——目录逃逸 / provider 泄漏即抛错（fail-closed）。
    #[tokio::test]
    async fn isolation_checks_fail_closed() {
        let env = tempfile::tempdir().unwrap();
        let state = isolated_state(env.path()).unwrap();
        // 干净环境：通过。
        assert!(assert_isolation(&state, env.path()).await.is_ok());
        // provider 泄漏：config 里出现 [providers] 即违例。
        std::fs::write(
            state.config_dir.join("config.toml"),
            "[providers.deepseek]\n",
        )
        .unwrap();
        assert!(assert_isolation(&state, env.path()).await.is_err());
        // 目录逃逸：另一份 state 的 config 在 env 外即违例。
        let outside = tempfile::tempdir().unwrap();
        let escaped = isolated_state(outside.path()).unwrap();
        assert!(assert_isolation(&escaped, env.path()).await.is_err());
    }

    /// M2-01.A2：隔离 config 写入语义——config.toml 无 provider 表；
    /// mcp-servers.toml 只含禁用的内置条目。
    #[test]
    fn eval_config_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("config");
        write_eval_config(&config_dir).unwrap();
        let text = std::fs::read_to_string(config_dir.join("config.toml")).unwrap();
        assert!(!text.contains("[providers"));
        let mcp = std::fs::read_to_string(config_dir.join("mcp-servers.toml")).unwrap();
        assert!(mcp.contains("enabled = false"));
        assert_eq!(mcp.matches("[[servers]]").count(), 1);
    }

    /// stopReason 非 stop 判失败（NotSettled 不得计入 pass——table 侧统一处理）。
    #[test]
    fn non_settled_is_not_passed() {
        let reason = EvalStopReason::NotSettled("budget".to_string());
        assert!(!reason.is_settled());
        assert!(EvalStopReason::Settled.is_settled());
    }

    /// M2-01.A2/A3 端到端：隔离环境下完整跑一轮 mock 场景——同源工厂、
    /// thinking off、事件/输出/usage 收集全部就位，任务收敛。
    #[tokio::test]
    async fn end_to_end_mock_run_settles_with_events_and_thinking_off() {
        let harness = create_r_code_harness(constant_reply_scenario("done: ok"));
        let input = EvalInput::new("smoke", "say ok");
        let result = harness.run(&input).await.expect("mock run must succeed");
        assert_eq!(result.harness, "r-code");
        assert_eq!(result.output, "done: ok");
        assert!(
            result.stop_reason.is_settled(),
            "got {:?}",
            result.stop_reason
        );
        assert!(
            result
                .events
                .iter()
                .any(|event| matches!(event, AgentEvent::Message { .. })),
            "captured events must include the scripted message"
        );
        assert!(result.workspace.exists());
    }
}
