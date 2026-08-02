//! LLM Agent Runtime -- 真实 provider 的多轮 agent runtime。
//!
//! 基于 `hermes_llm` 的真实 provider（Anthropic / OpenAI 兼容 / DeepSeek）：
//! - `create_session` 建立会话（消息历史 + 权限上下文 task_id）
//! - `start_run` spawn 多轮 agent loop（复用 `run_agent_loop_iteration` 单轮实现）
//! - 工具调用经 `SessionToolHost` → `ToolGateway::execute_with_wait`
//!   （权限审批挂起等待，abort 可中断）
//! - `steer` 在运行中注入用户消息（下一轮迭代前并入历史）
//! - `poll_events` 排空事件队列（由宿主 drain 循环持久化 + 转发 WebView）
//!
//! [doc-04 §9, §10]

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, Local};
use hermes_core::{
    CompletionRequest, InferenceOptions, LlmProvider, Message, Session, SessionMeta,
    ToolCallOutcome, ToolHost, ToolSource, ToolSpec,
};
use r_code_core::dto::{
    AgentActivityPhase, AgentEvent, AgentEventScope, AgentKind, AgentRunRuntimeKind,
    CreateSessionInput, ProjectAccessMode, SubagentAccessMode, SubagentState, TaskMode, TaskState,
};
use r_code_core::error::ProductError;
use r_code_core::security::PathGuard;
use r_code_gateway::{
    classify_shell_command, subagent_read_only_tool_allowed, PathArity, PathBinding, ToolGateway,
};
use tokio::sync::{watch, Mutex, Semaphore};
use uuid::Uuid;

use crate::agent_loop::{
    run_agent_loop_iteration_streaming_with_abort, run_agent_loop_iteration_with_abort_and_emit,
};
use crate::runtime::{AgentRuntime, SteerResult};

/// 单个 run 的最大迭代轮数（防失控兜底）。
const MAX_ITERATIONS: usize = 32;
/// 同一主运行并行执行的子代理上限。
const MAX_PARALLEL_SUBAGENTS: usize = 3;
/// 单次主运行可委派的子代理总量上限，防止模型无限排队占用资源。
const MAX_SUBAGENTS_PER_RUN: usize = 8;
/// 单个只读子代理的工具轮次上限。
const MAX_SUBAGENT_ITERATIONS: usize = 12;
/// 进入主 Agent 上下文的单个子代理摘要上限。
const MAX_SUBAGENT_SUMMARY_CHARS: usize = 3_000;

/// `delegate_task(agent="auto")` 的宿主路由策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DelegationRouterMode {
    Manual,
    #[default]
    Balanced,
    RCodeFirst,
    CodexFirst,
}

/// 完成主回复后是否启动显式、可观察的质量复核。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityLoopMode {
    Off,
    #[default]
    Auto,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityReviewer {
    #[default]
    Auto,
    RCode,
    Codex,
}

/// 运行时编排策略。路由与复核策略在重建 provider runtime 时读取；Codex 委派开关
/// 会另外复制到共享原子门，在运行中也能即时更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrchestrationPolicy {
    pub delegation_router: DelegationRouterMode,
    pub allow_cross_engine_delegation: bool,
    pub quality_loop: QualityLoopMode,
    pub quality_reviewer: QualityReviewer,
    pub max_review_rounds: u8,
}

/// 用户可编辑的 Agent 协作提示。它只补充角色分工，不替代工具权限、工作区范围或
/// 本轮显式禁用子代理等宿主硬边界。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentPromptPolicy {
    #[serde(default = "default_main_agent_prompt")]
    pub main_agent: String,
    #[serde(default = "default_subagent_prompt")]
    pub subagent: String,
}

pub const DEFAULT_MAIN_AGENT_PROMPT: &str = "You are the main agent and own the final result. \
Solve the task directly when delegation would not add clear value. Delegate only bounded, \
independent work, avoid duplicate investigations, and integrate every child result into your own \
verified final answer. An explicit user request not to use subagents or external agent CLIs takes \
priority over automatic routing.";

pub const DEFAULT_SUBAGENT_PROMPT: &str = "You are a delegated child agent. Stay within the \
assignment from the parent, do not create further agents, avoid duplicating the parent's work, and \
return a concise factual result with relevant verification evidence.";

fn default_main_agent_prompt() -> String {
    DEFAULT_MAIN_AGENT_PROMPT.to_string()
}

fn default_subagent_prompt() -> String {
    DEFAULT_SUBAGENT_PROMPT.to_string()
}

impl Default for AgentPromptPolicy {
    fn default() -> Self {
        Self {
            main_agent: default_main_agent_prompt(),
            subagent: default_subagent_prompt(),
        }
    }
}

impl Default for OrchestrationPolicy {
    fn default() -> Self {
        Self {
            delegation_router: DelegationRouterMode::Balanced,
            allow_cross_engine_delegation: true,
            quality_loop: QualityLoopMode::Auto,
            quality_reviewer: QualityReviewer::Auto,
            max_review_rounds: 1,
        }
    }
}

/// 系统提示（v1：紧凑通用；项目记忆/rules 注入后续里程碑）。
const CHAT_SYSTEM_PROMPT: &str = "You are R-Code, a helpful desktop coding assistant.\n\
No workspace is attached to this conversation, so you do not have file, terminal, or git access.\n\
Answer the user's question directly. If local context would help, ask them to attach a folder.\n\
Keep replies concise and concrete.";

const WORKSPACE_SYSTEM_PROMPT: &str = "You are R-Code, a coding agent working inside a user-approved workspace.\n\
Work on the user's goal directly with the provided workspace tools. Keep replies concise and concrete.\n\
All file paths are relative to the attached workspace; read before you write.\n\
When the goal is fully addressed, stop calling tools and summarize what you did.\n\
\n\
Tool selection matters:\n\
- To find code, use `search` (content regex) and `glob` (file names). Never shell out to \
grep, rg, find, ls or dir — the built-in tools respect .gitignore, skip binaries, and behave \
identically on Windows, macOS and Linux, while shell commands differ per platform and need approval.\n\
- To read files use `read_file` (page long files with offset/limit), not cat or type.\n\
- To change a file use `edit` with an exact literal snippet. Prefer it over `apply_patch`: \
rewriting a whole file wastes tokens and silently discards concurrent changes.\n\
- Reserve `bash` for builds, tests, linters, git and package managers. Every command needs user \
approval, and some (privilege escalation, `git push`, publishing) are refused outright — \
if you need one of those, ask the user to run it themselves.\n\
\n\
Execution order matters:\n\
- When several inspections are independent, issue independent read-only tool calls together in \
the same turn so R-Code can execute them concurrently.\n\
- R-Code only parallelizes side-effect-free reads in bounded batches. Keep writes, shell commands, \
and result-dependent work sequential.";

const LIVE_GUIDANCE_PREFIX: &str =
    "[system] Live guidance for the current run (supplemental guidance, not a replacement).";
const LIVE_GUIDANCE_MARKER: &str = "\n\nAccepted live guidance:\n";

/// `steer` is an in-flight constraint on the current run, not an ordinary next-turn user goal.
/// Keep the raw text in the host event/store for faithful UI replay, but wrap the copy injected
/// into the model history so a short side question cannot silently replace the original task.
fn format_live_guidance(text: &str) -> String {
    format!(
        "{LIVE_GUIDANCE_PREFIX}\n\
Preserve and complete the current user task. Apply this text as an added constraint or brief side \
question, then resume the original work. Only replace or cancel the current task when this guidance \
explicitly asks to do so.{LIVE_GUIDANCE_MARKER}{}",
        text.trim()
    )
}

fn parse_live_guidance(text: &str) -> Option<&str> {
    text.strip_prefix(LIVE_GUIDANCE_PREFIX)?
        .split_once(LIVE_GUIDANCE_MARKER)
        .map(|(_, guidance)| guidance.trim())
        .filter(|guidance| !guidance.is_empty())
}

/// 构建每轮请求的系统提示。客户端本地时间是纯聊天回答“今天/星期几”的可信来源，
/// 不需要为此开放终端、文件系统或外部插件。
fn build_system_prompt_at(has_workspace_tools: bool, now: DateTime<FixedOffset>) -> String {
    let base = if has_workspace_tools {
        WORKSPACE_SYSTEM_PROMPT
    } else {
        CHAT_SYSTEM_PROMPT
    };
    format!(
        "{base}\n\nCurrent local time: {} ({}). Use this local clock for date and time questions. \
Answer ordinary, non-programming questions directly when no workspace is attached.",
        now.format("%Y-%m-%dT%H:%M:%S%:z"),
        now.format("%A"),
    )
}

fn build_system_prompt(has_workspace_tools: bool) -> String {
    build_system_prompt_at(has_workspace_tools, Local::now().fixed_offset())
}

fn append_editable_prompt(mut base: String, label: &str, prompt: &str) -> String {
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        base.push_str("\n\n");
        base.push_str(label);
        base.push('\n');
        base.push_str(prompt);
    }
    base
}

fn build_main_system_prompt(has_workspace_tools: bool, prompts: &AgentPromptPolicy) -> String {
    append_editable_prompt(
        build_system_prompt(has_workspace_tools),
        "User-configured main/subagent coordination guidance:",
        &prompts.main_agent,
    )
}

fn build_subagent_system_prompt(
    has_workspace_tools: bool,
    access_mode: SubagentAccessMode,
    editable_prompt: &str,
) -> String {
    let base = if has_workspace_tools {
        WORKSPACE_SYSTEM_PROMPT
    } else {
        CHAT_SYSTEM_PROMPT
    };
    let capability = match access_mode {
        SubagentAccessMode::ReadOnly => "You are a read-only delegated subagent. Investigate the assigned question and use only the provided read-only tools. Do not edit files or run terminal commands.",
        SubagentAccessMode::FullAccess => "The parent agent explicitly delegated this task with full workspace access. You may edit files and run commands when they are necessary for the assignment, but stay inside the attached workspace and make only task-scoped changes.",
    };
    append_editable_prompt(
        format!(
            "{base}\n\n{capability} Return a concise factual summary for the parent agent. \
Do not create further subagents or expose private chain-of-thought."
        ),
        "User-configured subagent guidance:",
        editable_prompt,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelegationDirective {
    Automatic,
    Disabled,
}

fn delegation_directive_for_text(text: &str) -> DelegationDirective {
    let normalized = text
        .to_lowercase()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let disabled = [
        "不使用子代理",
        "不用子代理",
        "不要使用子代理",
        "不要再使用子代理",
        "不要调用子代理",
        "不要再调用子代理",
        "别用子代理",
        "禁止使用子代理",
        "不需要子代理",
        "不使用子智能体",
        "不用子智能体",
        "不要使用子智能体",
        "不要调用子智能体",
        "不要委派",
        "不委派",
        "不要调用codex",
        "不要再调用codex",
        "不要使用codex",
        "不使用codex",
        "不用codex",
        "不要调用claude",
        "donotusesubagents",
        "don'tusesubagents",
        "nosubagents",
        "donotdelegate",
        "don'tdelegate",
        "donotusecodex",
        "don'tusecodex",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern));
    if disabled {
        DelegationDirective::Disabled
    } else {
        DelegationDirective::Automatic
    }
}

fn delegation_directive(messages: &[Message]) -> DelegationDirective {
    messages
        .iter()
        .rev()
        .find(|message| message.role == hermes_core::Role::User)
        .map(Message::text_content)
        .map(|text| delegation_directive_for_text(&text))
        .unwrap_or(DelegationDirective::Automatic)
}

fn external_agent_executable(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '\'' | '"' | '`' | '(' | ')' | '{' | '}' | '[' | ']'
        )
    });
    let base = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .to_ascii_lowercase();
    let stem = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".cmd"))
        .or_else(|| base.strip_suffix(".bat"))
        .unwrap_or(&base);
    matches!(stem, "codex" | "claude")
}

/// 检测 shell 串是否把外部 Agent CLI 放在任一命令位置。分隔符内的普通参数（例如
/// `echo codex`）不会误判；嵌套 PowerShell/cmd 的参数仍会被扫描，以防绕开委派门禁。
fn command_invokes_external_agent(command: &str) -> bool {
    command
        .split([';', '|', '&', '\n', '\r'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .any(|segment| {
            let tokens = segment.split_whitespace().collect::<Vec<_>>();
            let Some(head) = tokens.first() else {
                return false;
            };
            if external_agent_executable(head) {
                return true;
            }
            let wrapper = head
                .trim_matches(['\'', '"'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(head)
                .to_ascii_lowercase();
            matches!(
                wrapper.as_str(),
                "pwsh"
                    | "pwsh.exe"
                    | "powershell"
                    | "powershell.exe"
                    | "cmd"
                    | "cmd.exe"
                    | "bash"
                    | "sh"
            ) && tokens
                .iter()
                .skip(1)
                .any(|token| external_agent_executable(token))
        })
}

const DELEGATION_PROMPT_HINT: &str = "\n\nFor independent investigation, you may call \
`delegate_task` to start up to three subagents in parallel. Subagents default to `access='read_only'`. \
Use `access='full_access'` only when the user conversation or your explicit parent plan delegates \
workspace edits or command execution to that child. Call `collect_subagents` before your final answer \
to obtain their concise findings.";

const CODEX_DELEGATION_PROMPT_HINT: &str = " When the user explicitly asks for Codex, call \
`delegate_task` with `agent` set to `codex`; do not substitute an internal R-Code subagent and do \
not claim Codex is unsupported before trying the tool. Codex runs through the user's installed and \
authenticated Codex CLI using the permission profile configured in Codex; setup failures are returned as tool errors.";

const CODEX_WORKSPACE_REQUIRED_PROMPT_HINT: &str = "\n\nCodex CLI delegation requires an attached \
workspace. If the user asks you to invoke Codex while no workspace is attached, tell them to attach \
a folder to this conversation first. Do not describe this as a model permission problem, and do not \
infer that Codex is unconfigured.";

/// Result returned by the host-provided Codex CLI bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexSubagentOutcome {
    Completed(String),
    Cancelled,
}

/// Safe, user-observable progress emitted by the host Codex bridge.
///
/// The callback receives child-local events; [`SubagentSupervisor`] attaches the stable run scope
/// before forwarding them to persistence and the WebView. Implementations must never emit raw
/// reasoning or unredacted command output through this channel.
pub type CodexSubagentEventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// 完整的主机委派上下文。将 task/run 归属传到主机，才能把 Codex App Server 的
/// `on-request` 权限提示准确投影回当前 R-Code 任务，而不是在非交互 CLI 中卡住。
#[derive(Clone)]
pub struct CodexSubagentRequest {
    pub workspace: PathBuf,
    pub goal: String,
    pub task_id: String,
    pub run_id: String,
    pub caller: String,
    pub access_mode: SubagentAccessMode,
    pub abort: Arc<AtomicBool>,
    pub event_sink: CodexSubagentEventSink,
}

/// Host boundary used by the provider runtime to invoke Codex without depending on Tauri.
///
/// The worker owns tool exposure, concurrency and lifecycle events. The desktop host owns CLI
/// discovery, authentication checks and process execution.
#[async_trait]
pub trait CodexSubagentRunner: Send + Sync {
    async fn run(
        &self,
        request: CodexSubagentRequest,
    ) -> Result<CodexSubagentOutcome, ProductError>;
}

/// LLM Agent Runtime -- 真实 provider 驱动。
pub struct LlmAgentRuntime {
    provider: Arc<dyn LlmProvider>,
    model: String,
    gateway: Arc<ToolGateway>,
    max_tokens: u32,
    temperature: Option<f32>,
    sessions: Arc<Mutex<HashMap<String, SessionState>>>,
    event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    event_rx: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<AgentEvent>>>,
    running: Arc<AtomicBool>,
    aborted: Arc<AtomicBool>,
    codex_subagent_runner: Option<Arc<dyn CodexSubagentRunner>>,
    cross_engine_delegation_enabled: Arc<AtomicBool>,
    orchestration: OrchestrationPolicy,
    agent_prompts: AgentPromptPolicy,
}

struct SessionState {
    /// 权限门 / 审计上下文
    task_id: String,
    /// 会话级模型覆盖（None = runtime 默认）
    model: Option<String>,
    /// 会话级模型推理参数。
    inference: InferenceOptions,
    /// 任务模式决定主运行的可用工具能力；`Ask` 在附加工作区后仍保持只读。
    mode: TaskMode,
    /// 消息历史（多轮 loop 的工作集）
    messages: Vec<Message>,
    /// 运行中注入的用户消息（下一轮迭代前并入）
    steer_queue: VecDeque<String>,
    /// 当前运行是否仍可接纳引导；和队列共用 session 锁以消除结束边界竞态。
    accepting_steer: bool,
    /// 当前 run 的中止标志
    abort: Arc<AtomicBool>,
    /// 用户在当前运行中显式关闭委派后立即锁存；工具执行阶段再次检查，避免 steer
    /// 与同一轮 provider 工具调用之间的竞态。
    delegation_disabled: Arc<AtomicBool>,
    /// 工作区作用域。None 即纯聊天；附加后始终通过 PathGuard 限制本地工具。
    workspace_scope: Option<WorkspaceScope>,
    /// 当前主运行所拥有的子代理监督器；仅在运行期间存在。
    supervisor: Option<Arc<SubagentSupervisor>>,
    /// 监督器所属的主运行 ID，防止旧运行收尾时误清理新运行状态。
    active_run_id: Option<String>,
}

/// 会话持有的本地文件能力边界。
#[derive(Clone)]
struct WorkspaceScope {
    guard: PathGuard,
    access_mode: ProjectAccessMode,
}

impl WorkspaceScope {
    fn from_binding(
        workspace_path: Option<String>,
        access_mode: ProjectAccessMode,
    ) -> Result<Option<Self>, ProductError> {
        let Some(path) = workspace_path else {
            return Ok(None);
        };
        Ok(Some(Self {
            guard: PathGuard::new(PathBuf::from(path))?,
            access_mode,
        }))
    }
}

impl LlmAgentRuntime {
    /// 创建 runtime。
    ///
    /// `max_tokens` None 时取 8192。
    pub fn new(
        provider: Box<dyn LlmProvider>,
        model: String,
        gateway: Arc<ToolGateway>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Self {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            provider: Arc::from(provider),
            model,
            gateway,
            max_tokens: max_tokens.unwrap_or(8192),
            temperature,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            running: Arc::new(AtomicBool::new(false)),
            aborted: Arc::new(AtomicBool::new(false)),
            codex_subagent_runner: None,
            cross_engine_delegation_enabled: Arc::new(AtomicBool::new(true)),
            orchestration: OrchestrationPolicy::default(),
            agent_prompts: AgentPromptPolicy::default(),
        }
    }

    /// Attach the desktop host's Codex CLI bridge.
    pub fn with_codex_subagent_runner(mut self, runner: Arc<dyn CodexSubagentRunner>) -> Self {
        self.codex_subagent_runner = Some(runner);
        self
    }

    /// 应用用户在设置页保存的编排策略。轮数在这里再次收紧，避免损坏配置失控。
    pub fn with_orchestration_policy(mut self, mut policy: OrchestrationPolicy) -> Self {
        policy.max_review_rounds = policy.max_review_rounds.clamp(1, 3);
        self.cross_engine_delegation_enabled
            .store(policy.allow_cross_engine_delegation, Ordering::SeqCst);
        self.orchestration = policy;
        self
    }

    /// 热切换新的 Codex 子代理委派。监督器共享同一原子门，因此已经启动的子代理
    /// 继续完成，而之后的显式或自动 Codex 路由会立即改用 R-Code。
    pub fn set_cross_engine_delegation_enabled(&self, enabled: bool) {
        self.cross_engine_delegation_enabled
            .store(enabled, Ordering::SeqCst);
    }

    pub fn cross_engine_delegation_enabled(&self) -> bool {
        self.cross_engine_delegation_enabled.load(Ordering::SeqCst)
    }

    /// 应用保存在用户级 AppData 中的可编辑协作提示。
    pub fn with_agent_prompts(mut self, prompts: AgentPromptPolicy) -> Self {
        self.agent_prompts = prompts;
        self
    }

    /// 是否有 run 处于活跃。
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// 是否请求了 abort。
    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl AgentRuntime for LlmAgentRuntime {
    async fn create_session(&mut self, input: CreateSessionInput) -> Result<Session, ProductError> {
        let meta = SessionMeta::new(
            input.model.clone().unwrap_or_else(|| self.model.clone()),
            self.provider.name().to_string(),
        );
        let session = Session::new(meta);
        let workspace_scope =
            WorkspaceScope::from_binding(input.workspace_path, input.workspace_access_mode)?;
        self.sessions.lock().await.insert(
            session.meta.id.clone(),
            SessionState {
                task_id: input.task_id,
                model: input.model,
                inference: input.inference,
                mode: input.mode,
                messages: Vec::new(),
                steer_queue: VecDeque::new(),
                accepting_steer: false,
                abort: Arc::new(AtomicBool::new(false)),
                delegation_disabled: Arc::new(AtomicBool::new(false)),
                workspace_scope,
                supervisor: None,
                active_run_id: None,
            },
        );
        Ok(session)
    }

    async fn start_run(&mut self, session_id: &str, goal: &str) -> Result<String, ProductError> {
        self.start_run_with_message(session_id, Message::user_text(goal))
            .await
    }

    async fn start_run_with_message(
        &mut self,
        session_id: &str,
        message: Message,
    ) -> Result<String, ProductError> {
        let run_id = Uuid::new_v4();
        let (task_id, model, inference, mode, abort, delegation_disabled, workspace_scope) = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions
                .get_mut(session_id)
                .ok_or_else(|| ProductError::Other(format!("session not found: {session_id}")))?;
            let disable_delegation = delegation_directive_for_text(&message.text_content())
                == DelegationDirective::Disabled;
            session.messages.push(message);
            session.abort.store(false, Ordering::Relaxed);
            session.delegation_disabled = Arc::new(AtomicBool::new(disable_delegation));
            session.accepting_steer = true;
            (
                session.task_id.clone(),
                session.model.clone().unwrap_or_else(|| self.model.clone()),
                session.inference.clone(),
                session.mode,
                session.abort.clone(),
                session.delegation_disabled.clone(),
                session.workspace_scope.clone(),
            )
        };
        // Ask 是带工作区的只读问答，而不是“允许写入、只是碰巧没有点确认”。
        // 外部 MCP 调用会创建 Ask 任务，因此这里形成可执行的最小权限边界。
        let policy = tool_policy_for_task_mode(mode);
        // “Ask”只决定工作区工具为只读，不能再被当成“当前运行就是子代理”。
        // 只要用户已经附加工作区，主运行就可以委派只读调查；真正的子代理仍由
        // run_child 单独构造 ToolHost，并且没有 delegation，因而不能递归委派。
        let allows_delegation = workspace_scope.is_some();
        let run_id_text = run_id.to_string();
        let supervisor = Arc::new(SubagentSupervisor::new(
            self.provider.clone(),
            self.gateway.clone(),
            self.event_tx.clone(),
            task_id.clone(),
            run_id_text.clone(),
            model.clone(),
            self.max_tokens,
            self.temperature,
            inference.clone(),
            abort.clone(),
            workspace_scope.clone(),
            self.codex_subagent_runner.clone(),
            self.cross_engine_delegation_enabled.clone(),
            self.orchestration,
            self.agent_prompts.clone(),
        ));
        {
            let mut sessions = self.sessions.lock().await;
            let session = sessions
                .get_mut(session_id)
                .ok_or_else(|| ProductError::Other(format!("session not found: {session_id}")))?;
            session.supervisor = Some(supervisor.clone());
            session.active_run_id = Some(run_id_text.clone());
        }
        self.aborted.store(false, Ordering::Relaxed);
        self.running.store(true, Ordering::Relaxed);

        tokio::spawn(run_loop(RunLoopCtx {
            sessions: self.sessions.clone(),
            event_tx: self.event_tx.clone(),
            running: self.running.clone(),
            aborted_flag: self.aborted.clone(),
            provider: self.provider.clone(),
            gateway: self.gateway.clone(),
            session_id: session_id.to_string(),
            run_id,
            task_id,
            model,
            max_tokens: self.max_tokens,
            temperature: self.temperature,
            inference,
            abort,
            delegation_disabled,
            workspace_scope,
            supervisor,
            policy,
            allows_delegation,
            orchestration: self.orchestration,
            agent_prompts: self.agent_prompts.clone(),
        }));

        Ok(run_id_text)
    }

    async fn steer(
        &mut self,
        session_id: &str,
        message: &str,
    ) -> Result<SteerResult, ProductError> {
        if !self.running.load(Ordering::Relaxed) {
            return Ok(SteerResult::RunFinished);
        }
        let disable_delegation =
            delegation_directive_for_text(message) == DelegationDirective::Disabled;
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| ProductError::Other(format!("session not found: {session_id}")))?;
        if !session.accepting_steer {
            return Ok(SteerResult::RunFinished);
        }
        session.steer_queue.push_back(message.to_string());
        if disable_delegation {
            session.delegation_disabled.store(true, Ordering::SeqCst);
        }
        let supervisor = disable_delegation
            .then(|| session.supervisor.clone())
            .flatten();
        drop(sessions);
        if let Some(supervisor) = supervisor {
            supervisor.abort_all().await;
        }
        let _ = self.event_tx.send(AgentEvent::Activity {
            phase: r_code_core::dto::AgentActivityPhase::SteerAccepted,
            detail: None,
        });
        Ok(SteerResult::Accepted)
    }

    async fn abort(&mut self, session_id: &str) -> Result<(), ProductError> {
        let was_running = self.running.load(Ordering::Relaxed);
        let mut sessions = self.sessions.lock().await;
        let supervisor = if let Some(session) = sessions.get_mut(session_id) {
            session.abort.store(true, Ordering::Relaxed);
            session.accepting_steer = false;
            session.steer_queue.clear();
            session.supervisor.clone()
        } else {
            None
        };
        drop(sessions);
        if let Some(supervisor) = supervisor {
            supervisor.abort_all().await;
        }
        self.aborted.store(true, Ordering::Relaxed);
        let _ = self.event_tx.send(AgentEvent::Activity {
            phase: r_code_core::dto::AgentActivityPhase::Finalizing,
            detail: None,
        });
        // 没有活动 run 时不存在需要等待的子树，保留旧调用方的即时中断语义。
        if !was_running {
            let _ = self.event_tx.send(AgentEvent::State {
                state: TaskState::Interrupted,
            });
        }
        Ok(())
    }

    async fn abort_subagent(
        &mut self,
        session_id: &str,
        subagent_id: &str,
    ) -> Result<bool, ProductError> {
        let supervisor = self
            .sessions
            .lock()
            .await
            .get(session_id)
            .and_then(|session| session.supervisor.clone());
        match supervisor {
            Some(supervisor) => Ok(supervisor.abort_one(subagent_id).await),
            None => Ok(false),
        }
    }

    async fn replace_history(
        &mut self,
        session_id: &str,
        messages: Vec<Message>,
    ) -> Result<(), ProductError> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| ProductError::Other(format!("session not found: {session_id}")))?;
        session.messages = messages;
        session.steer_queue.clear();
        session.accepting_steer = false;
        session.abort.store(false, Ordering::Relaxed);
        Ok(())
    }

    async fn history_snapshot(
        &mut self,
        session_id: &str,
    ) -> Result<Option<Vec<Message>>, ProductError> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| ProductError::Other(format!("session not found: {session_id}")))?;
        Ok(Some(session.messages.clone()))
    }

    async fn update_workspace_scope(
        &mut self,
        session_id: &str,
        workspace_path: Option<String>,
        access_mode: ProjectAccessMode,
    ) -> Result<(), ProductError> {
        let workspace_scope = WorkspaceScope::from_binding(workspace_path, access_mode)?;
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| ProductError::Other(format!("session not found: {session_id}")))?;
        session.workspace_scope = workspace_scope;
        Ok(())
    }

    async fn poll_events(&mut self) -> Result<Vec<AgentEvent>, ProductError> {
        let mut event_rx = self.event_rx.lock().await;
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        Ok(events)
    }
}

/// run_loop 的共享上下文。
struct RunLoopCtx {
    sessions: Arc<Mutex<HashMap<String, SessionState>>>,
    event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    running: Arc<AtomicBool>,
    aborted_flag: Arc<AtomicBool>,
    provider: Arc<dyn LlmProvider>,
    gateway: Arc<ToolGateway>,
    session_id: String,
    run_id: Uuid,
    task_id: String,
    model: String,
    max_tokens: u32,
    temperature: Option<f32>,
    inference: InferenceOptions,
    abort: Arc<AtomicBool>,
    delegation_disabled: Arc<AtomicBool>,
    workspace_scope: Option<WorkspaceScope>,
    supervisor: Arc<SubagentSupervisor>,
    policy: ToolPolicy,
    allows_delegation: bool,
    orchestration: OrchestrationPolicy,
    agent_prompts: AgentPromptPolicy,
}

/// 将常见的服务端参数错误转换为可操作的界面提示；原始错误仍会写入 tracing 日志，
/// 便于诊断，但不应把一整段 JSON 直接丢给普通用户。
fn user_facing_provider_error(detail: &str) -> String {
    if detail.contains("Invalid max_tokens") && detail.contains("393216") {
        return "模型服务拒绝了“最大输出”设置。DeepSeek V4 的 1,000,000 是上下文窗口，不是单次输出；请在设置中把最大输出改为 8,192（或不超过 393,216）。".to_string();
    }
    let normalized = detail.to_ascii_lowercase();
    if normalized.contains("invalid_request_error")
        || normalized.contains("stream_options")
        || normalized.contains("cache_control")
    {
        return "模型服务拒绝了本次请求参数。请确认设置中的接口地址与模型匹配；若使用兼容接口，请尝试更新服务配置或关闭不支持的流式/缓存能力。".to_string();
    }
    detail.to_string()
}

fn emit_activity(
    event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    phase: AgentActivityPhase,
    detail: Option<String>,
) {
    let _ = event_tx.send(AgentEvent::Activity { phase, detail });
}

/// 多轮 agent loop：工具回合受上限保护；无工具回复若在收尾临界点收到 steer，
/// 则原子地将其并入下一次模型请求，而不是提前结束 run。
async fn run_loop(ctx: RunLoopCtx) {
    let mut terminal_err: Option<String> = None;
    let mut tool_iterations = 0usize;
    let mut quality_rounds = 0u8;

    loop {
        if ctx.abort.load(Ordering::Relaxed) {
            break;
        }

        // 在 session 锁内同时取走 steer 与工作集。后续的结束判断也在同一把锁内
        // 检查队列，确保 steer 不会落在“检查为空”和“标记完成”之间而丢失。
        let (mut messages, applied_steers) = {
            let mut sessions = ctx.sessions.lock().await;
            let Some(session) = sessions.get_mut(&ctx.session_id) else {
                terminal_err = Some(format!("session lost: {}", ctx.session_id));
                break;
            };
            let mut applied_steers = 0usize;
            while let Some(text) = session.steer_queue.pop_front() {
                session
                    .messages
                    .push(Message::user_text(format_live_guidance(&text)));
                applied_steers += 1;
            }
            (session.messages.clone(), applied_steers)
        };
        if applied_steers > 0 {
            emit_activity(
                &ctx.event_tx,
                AgentActivityPhase::SteerApplied,
                Some(format!("{applied_steers} 条引导已并入下一次请求")),
            );
        }
        emit_activity(&ctx.event_tx, AgentActivityPhase::Requesting, None);

        let delegation_allowed = ctx.allows_delegation
            && !ctx.delegation_disabled.load(Ordering::SeqCst)
            && delegation_directive(&messages) != DelegationDirective::Disabled;
        let tool_host = SessionToolHost {
            gateway: ctx.gateway.clone(),
            task_id: ctx.task_id.clone(),
            run_id: ctx.run_id.to_string(),
            abort: ctx.abort.clone(),
            workspace_scope: ctx.workspace_scope.clone(),
            policy: ctx.policy,
            caller: "agent".to_string(),
            delegation: delegation_allowed.then(|| ctx.supervisor.clone()),
            delegation_disabled: ctx.delegation_disabled.clone(),
        };
        let tools = tool_host.tool_specs();
        // 这是主会话的 run loop。Ask 只收紧工具权限，不改变 Agent 身份；子代理使用
        // run_child 中的 build_subagent_system_prompt。
        let mut system_prompt =
            build_main_system_prompt(ctx.workspace_scope.is_some(), &ctx.agent_prompts);
        if delegation_allowed {
            system_prompt.push_str(DELEGATION_PROMPT_HINT);
            if ctx.supervisor.codex_available() {
                system_prompt.push_str(CODEX_DELEGATION_PROMPT_HINT);
            }
        } else if ctx.supervisor.codex_configured() {
            if ctx.workspace_scope.is_none() {
                system_prompt.push_str(CODEX_WORKSPACE_REQUIRED_PROMPT_HINT);
            } else {
                system_prompt.push_str(
                    "\n\nThe current user turn explicitly disables subagents and external agent \
CLIs. Work directly and do not delegate or invoke Codex/Claude through shell commands.",
                );
            }
        }
        let request = CompletionRequest {
            model: ctx.model.clone(),
            system: Some(system_prompt),
            messages: Vec::new(), // 由 run_agent_loop_iteration 同步
            tools: Vec::new(),    // 同上
            max_tokens: ctx.max_tokens,
            temperature: ctx.temperature,
            // 纯聊天没有长系统提示或工具定义可复用，关闭缓存可避免部分兼容接口
            // 对 cache_control 的不支持错误；工作区工具回合继续允许 provider 缓存。
            enable_caching: !tools.is_empty(),
            inference: ctx.inference.clone(),
        };

        let result = run_agent_loop_iteration_streaming_with_abort(
            ctx.provider.as_ref(),
            &tool_host,
            request,
            &mut messages,
            &tools,
            Some(ctx.abort.as_ref()),
            ctx.event_tx.clone(),
        )
        .await;

        match result {
            Ok(outcome) => {
                let reaches_tool_limit =
                    outcome.had_tool_call && tool_iterations + 1 >= MAX_ITERATIONS;
                let should_continue = {
                    let mut sessions = ctx.sessions.lock().await;
                    let Some(session) = sessions.get_mut(&ctx.session_id) else {
                        terminal_err = Some(format!("session lost: {}", ctx.session_id));
                        break;
                    };
                    // Keep the local snapshot available for the host-managed quality review
                    // below. The session owns the authoritative copy; the local clone is only
                    // used to derive the visible draft after this iteration has settled.
                    session.messages = messages.clone();

                    if ctx.abort.load(Ordering::Relaxed) {
                        session.accepting_steer = false;
                        false
                    } else if outcome.had_tool_call {
                        // 最后一轮工具调用后不再接受 steer，避免“已接纳”却没有下一次
                        // provider 请求可注入的假象；调用方会把它持久化为队列消息。
                        if reaches_tool_limit {
                            session.accepting_steer = false;
                            false
                        } else {
                            true
                        }
                    } else if !session.steer_queue.is_empty() {
                        // steer 在本轮无工具回复的收尾期间抵达：继续一轮以消费它。
                        true
                    } else {
                        session.accepting_steer = false;
                        false
                    }
                };

                if !should_continue {
                    // 子代理尚未收集时，不提前结束：等待完成后自动收集结果并
                    // 注入历史，让模型有机会在下一轮汇总。
                    if terminal_err.is_none()
                        && !ctx.abort.load(Ordering::Relaxed)
                        && delegation_allowed
                        && !ctx.delegation_disabled.load(Ordering::SeqCst)
                        && ctx.supervisor.has_children().await
                    {
                        emit_activity(
                            &ctx.event_tx,
                            AgentActivityPhase::Requesting,
                            Some("等待子代理完成...".to_string()),
                        );
                        ctx.supervisor.wait_for_all().await;
                        if let Ok(collected) = ctx.supervisor.collect(None).await {
                            let mut sessions = ctx.sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&ctx.session_id) {
                                session.messages.push(Message::user_text(format!(
                                    "[system] Delegated subagents have completed. \
Their findings are provided below; you do not need to call collect_subagents. \
Please summarize and present these results.\n\n{}",
                                    collected.content
                                )));
                                session.accepting_steer = true;
                            }
                            continue;
                        }
                    }

                    if reaches_tool_limit {
                        terminal_err = Some(format!(
                            "达到 {MAX_ITERATIONS} 轮工具调用上限，已停止继续执行。"
                        ));
                    }
                    if terminal_err.is_none()
                        && !ctx.abort.load(Ordering::Relaxed)
                        && delegation_allowed
                        && !ctx.delegation_disabled.load(Ordering::SeqCst)
                        && quality_rounds < ctx.orchestration.max_review_rounds
                        && should_run_quality_review(
                            ctx.orchestration.quality_loop,
                            ctx.workspace_scope.is_some(),
                            tool_iterations > 0,
                        )
                    {
                        let draft = final_visible_response(&messages);
                        if !draft.is_empty() {
                            let review_packet = current_run_review_packet(&messages);
                            quality_rounds += 1;
                            emit_activity(
                                &ctx.event_tx,
                                AgentActivityPhase::Reviewing,
                                Some(format!(
                                    "质量复核 {quality_rounds}/{}",
                                    ctx.orchestration.max_review_rounds
                                )),
                            );
                            match ctx
                                .supervisor
                                .quality_review(quality_rounds, &review_packet, &draft)
                                .await
                            {
                                Ok(QualityReviewResult::Passed(summary)) => {
                                    emit_activity(
                                        &ctx.event_tx,
                                        AgentActivityPhase::Reviewing,
                                        Some(format!(
                                            "质量复核已通过 · {}",
                                            short_summary(&summary, 96)
                                        )),
                                    );
                                }
                                Ok(QualityReviewResult::Revise(findings)) => {
                                    let mut sessions = ctx.sessions.lock().await;
                                    if let Some(session) = sessions.get_mut(&ctx.session_id) {
                                        session.messages.push(Message::user_text(format!(
                                            "[system] Quality review round {quality_rounds} found issues in the visible draft. \
Address the concrete findings below, re-check any relevant workspace evidence, and then provide a corrected final answer. \
Do not mention private reasoning.\n\n{}",
                                            short_summary(&findings, MAX_SUBAGENT_SUMMARY_CHARS)
                                        )));
                                        session.accepting_steer = true;
                                    }
                                    continue;
                                }
                                Err(error) => {
                                    emit_activity(
                                        &ctx.event_tx,
                                        AgentActivityPhase::Reviewing,
                                        Some(format!(
                                            "质量复核未完成，本轮保留主结果 · {}",
                                            short_summary(&error.to_string(), 96)
                                        )),
                                    );
                                }
                            }
                        }
                    }
                    break;
                }
                if outcome.had_tool_call {
                    tool_iterations += 1;
                }
            }
            Err(e) => {
                tracing::warn!(session_id = %ctx.session_id, "agent loop iteration failed: {e}");
                terminal_err = Some(user_facing_provider_error(&e.to_string()));
                break;
            }
        }
    }

    let was_aborted = ctx.abort.load(Ordering::Relaxed) || ctx.aborted_flag.load(Ordering::Relaxed);
    // 父运行正常结束时等待所有已启动的子代理完成，避免后台孤儿运行；父运行失败或
    // 被中止时则级联停止整棵树。模型应通过 collect_subagents 主动获得这些摘要。
    if was_aborted || terminal_err.is_some() {
        ctx.supervisor.abort_all().await;
        // 取消信号本身不足以保证子任务已经结束；在发布父运行终态前等待它们确认，
        // 防止 drain 循环或下一条队列消息先于最后的子代理生命周期事件关闭。
        ctx.supervisor.wait_for_all().await;
    } else {
        ctx.supervisor.wait_for_all().await;
    }

    // 所有终止路径都关闭 steer 闸门。abort 时同时丢弃尚未消费的引导，避免下一次 run
    // 意外继承旧消息；正常收尾前的引导已在上面的同锁检查中被消费或拒绝。
    let finished_run_id = ctx.run_id.to_string();
    {
        let mut sessions = ctx.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&ctx.session_id) {
            session.accepting_steer = false;
            if was_aborted {
                session.steer_queue.clear();
            }
            if session.active_run_id.as_deref() == Some(finished_run_id.as_str()) {
                session.supervisor = None;
                session.active_run_id = None;
            }
        }
    }

    // 收尾：错误以非增量消息呈现；正常完成才发出 ReviewReady。
    if let Some(err) = terminal_err {
        let _ = ctx.event_tx.send(AgentEvent::Message {
            text: format!("[error] {err}"),
            delta: false,
        });
    }
    if was_aborted {
        let _ = ctx.event_tx.send(AgentEvent::State {
            state: TaskState::Interrupted,
        });
    } else {
        emit_activity(&ctx.event_tx, AgentActivityPhase::Finalizing, None);
        let _ = ctx.event_tx.send(AgentEvent::State {
            state: TaskState::ReviewReady,
        });
    }
    ctx.running.store(false, Ordering::Relaxed);
}

/// 绑定任务上下文的 ToolHost：工具调用经 ToolGateway 权限门（审批挂起等待）。
struct SessionToolHost {
    gateway: Arc<ToolGateway>,
    task_id: String,
    run_id: String,
    abort: Arc<AtomicBool>,
    workspace_scope: Option<WorkspaceScope>,
    policy: ToolPolicy,
    caller: String,
    delegation: Option<Arc<SubagentSupervisor>>,
    delegation_disabled: Arc<AtomicBool>,
}

/// Agent 可见工具的能力边界。子代理不能再次委派；默认只读，显式提权后才开放写工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPolicy {
    Main,
    ReadOnly,
    FullAccess,
}

fn should_run_quality_review(mode: QualityLoopMode, has_workspace: bool, used_tools: bool) -> bool {
    match mode {
        QualityLoopMode::Off => false,
        QualityLoopMode::Auto => has_workspace && used_tools,
        QualityLoopMode::Always => true,
    }
}

fn final_visible_response(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| message.role == hermes_core::Role::Assistant)
        .map(Message::text_content)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_default()
}

/// Build a bounded acceptance packet for the automatic reviewer from the current run only.
/// Synthetic tool/subagent collection messages intentionally use a `[system]` prefix even though
/// the provider message role is `user`; exclude those so the reviewer sees the user's actual goal.
fn current_run_review_packet(messages: &[Message]) -> String {
    let mut current_goal = None;
    let mut live_guidance = Vec::new();

    for message in messages.iter().rev() {
        if message.role != hermes_core::Role::User {
            continue;
        }
        let text = message.text_content();
        if let Some(guidance) = parse_live_guidance(&text) {
            live_guidance.push(short_summary(guidance, 1_500));
            continue;
        }
        if text.trim_start().starts_with("[system]") {
            continue;
        }
        current_goal = Some(short_summary(&text, 4_000));
        break;
    }

    live_guidance.reverse();
    let goal = current_goal.unwrap_or_else(|| "(current user task unavailable)".to_string());
    if live_guidance.is_empty() {
        format!("Current user task:\n{goal}\n\nAccepted live guidance:\n(none)")
    } else {
        let guidance = live_guidance
            .iter()
            .enumerate()
            .map(|(index, item)| format!("{}. {item}", index + 1))
            .collect::<Vec<_>>()
            .join("\n");
        short_summary(
            &format!("Current user task:\n{goal}\n\nAccepted live guidance:\n{guidance}"),
            6_000,
        )
    }
}

fn build_quality_review_goal(review_packet: &str, draft: &str) -> String {
    let review_packet = short_summary(review_packet, 6_000);
    let draft = short_summary(draft, 6_000);
    format!(
        "Act as a fast, read-only release gate for the parent agent. Review the provisional draft \
against the supplied user task and accepted guidance. Check correctness, completeness, unsupported \
claims, missed validation, and unsafe changes. The packet is the primary evidence. Do not load skills \
or broadly rescan the repository, and do not rerun checks already reported in the draft. Use at most \
two targeted read-only workspace checks only when a specific material claim cannot be judged from the \
packet. Do not edit files. Return `PASS` on the first line when no correction is needed; otherwise \
return `REVISE` followed by concise, actionable findings. Do not expose private chain-of-thought.\n\n\
User task and accepted live guidance:\n{review_packet}\n\n\
Provisional draft (not yet delivered):\n{draft}"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubagentBackend {
    RCode,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskComplexity {
    Simple,
    Standard,
    Complex,
}

/// 将持久化任务模式映射为运行时能力边界。
///
/// `Ask` 既适用于纯聊天，也适用于「带工作区的只读调查」；后者尤其用于外部 MCP
/// 委派，不能因为附加了目录就意外拿到写工具或 shell。
fn tool_policy_for_task_mode(mode: TaskMode) -> ToolPolicy {
    if mode == TaskMode::Ask {
        ToolPolicy::ReadOnly
    } else {
        ToolPolicy::Main
    }
}

impl SessionToolHost {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut tools = match &self.workspace_scope {
            Some(_) => self
                .gateway
                .tool_specs()
                .into_iter()
                .filter(|tool| self.tool_allowed(&tool.name))
                .collect(),
            _ => Vec::new(),
        };
        if !self.delegation_disabled.load(Ordering::SeqCst) {
            if let Some(supervisor) = &self.delegation {
                tools.extend(delegation_tool_specs(supervisor.codex_available()));
            }
        }
        tools
    }

    fn tool_allowed(&self, name: &str) -> bool {
        match self.policy {
            ToolPolicy::Main | ToolPolicy::FullAccess => workspace_tool_allowed(name),
            ToolPolicy::ReadOnly => subagent_read_only_tool_allowed(name),
        }
    }

    fn scoped_input(
        &self,
        name: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, ProductError> {
        let scope = self.workspace_scope.as_ref().ok_or_else(|| {
            ProductError::Other("no workspace is attached to this conversation".to_string())
        })?;
        if !self.tool_allowed(name) {
            return Err(ProductError::Other(format!(
                "tool '{name}' is not available in a workspace-scoped conversation"
            )));
        }
        // 路径键由工具自己声明；未注册的工具沿用历史的单 `path` 语义。
        let bindings = self
            .gateway
            .path_bindings(name)
            .unwrap_or_else(|| fallback_bindings(name));
        let require_existing = self.gateway.requires_existing_path(name);
        bind_workspace_paths(name, bindings, input, &scope.guard, require_existing)
    }

    async fn call_inner(
        &self,
        call_id: Option<&str>,
        name: &str,
        args: serde_json::Value,
    ) -> hermes_error::Result<ToolCallOutcome> {
        let delegation_disabled = self.delegation_disabled.load(Ordering::SeqCst);
        if delegation_disabled && matches!(name, "delegate_task" | "collect_subagents") {
            return Err(hermes_error::Error::ToolHost(
                "本轮用户已明确关闭子代理；运行时拒绝了委派调用".to_string(),
            ));
        }
        if delegation_disabled
            && name == "bash"
            && args
                .get("command")
                .and_then(serde_json::Value::as_str)
                .is_some_and(command_invokes_external_agent)
        {
            return Err(hermes_error::Error::ToolHost(
                "本轮用户已明确关闭子代理；运行时拒绝了外部 Agent CLI 命令".to_string(),
            ));
        }
        if name == "delegate_task" {
            let supervisor = self.delegation.as_ref().ok_or_else(|| {
                hermes_error::Error::ToolHost(
                    "delegate_task is unavailable in this run".to_string(),
                )
            })?;
            let goal = args
                .get("goal")
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    hermes_error::Error::ToolHost(
                        "delegate_task requires a non-empty 'goal'".to_string(),
                    )
                })?;
            let label = args
                .get("label")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned);
            let requested_agent = args
                .get("agent")
                .and_then(|value| value.as_str())
                .unwrap_or("auto");
            let complexity = match args
                .get("complexity")
                .and_then(|value| value.as_str())
                .unwrap_or("standard")
            {
                "simple" => TaskComplexity::Simple,
                "standard" => TaskComplexity::Standard,
                "complex" => TaskComplexity::Complex,
                value => {
                    return Err(hermes_error::Error::ToolHost(format!(
                        "delegate_task received unsupported complexity '{value}'"
                    )))
                }
            };
            let (backend, routing_reason) =
                supervisor
                    .route_backend(requested_agent, complexity)
                    .map_err(|e| hermes_error::Error::ToolHost(e.to_string()))?;
            let access_mode = match args
                .get("access")
                .and_then(|value| value.as_str())
                .unwrap_or("read_only")
            {
                "read_only" => SubagentAccessMode::ReadOnly,
                "full_access" => SubagentAccessMode::FullAccess,
                value => {
                    return Err(hermes_error::Error::ToolHost(format!(
                        "delegate_task received unsupported access mode '{value}'"
                    )))
                }
            };
            return supervisor
                .spawn(
                    backend,
                    label,
                    goal.to_string(),
                    access_mode,
                    call_id.map(ToOwned::to_owned),
                    routing_reason,
                )
                .await
                .map_err(|e| hermes_error::Error::ToolHost(e.to_string()));
        }
        if name == "collect_subagents" {
            let supervisor = self.delegation.as_ref().ok_or_else(|| {
                hermes_error::Error::ToolHost(
                    "collect_subagents is unavailable in this run".to_string(),
                )
            })?;
            let ids = args
                .get("ids")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                        .collect::<Vec<_>>()
                });
            return supervisor
                .collect(ids)
                .await
                .map_err(|e| hermes_error::Error::ToolHost(e.to_string()));
        }
        let access_mode = if self.policy == ToolPolicy::FullAccess {
            ProjectAccessMode::FullAccess
        } else {
            self.workspace_scope
                .as_ref()
                .map(|scope| scope.access_mode)
                .unwrap_or(ProjectAccessMode::RequestApproval)
        };
        let args = match self.scoped_input(name, args) {
            Ok(args) => args,
            Err(ProductError::PathNotFound(msg)) => {
                return Ok(ToolCallOutcome {
                    content: format!("Error: {msg}"),
                    is_error: true,
                    metadata: None,
                });
            }
            Err(e) => return Err(hermes_error::Error::ToolHost(e.to_string())),
        };
        let summary = summarize_input(name, &args);
        match self
            .gateway
            .execute_with_wait_with_access_mode(
                &self.task_id,
                &self.run_id,
                call_id,
                name,
                args,
                Some(&self.caller),
                &summary,
                Some(self.abort.clone()),
                access_mode,
            )
            .await
        {
            Ok(outcome) => Ok(outcome),
            // 工具执行错误（IO、权限等）作为工具结果返回给模型，不终止 agent loop。
            // 模型可以据此调整策略（换路径、换工具或告知用户）。
            Err(e) => Ok(ToolCallOutcome {
                content: format!("Error: {e}"),
                is_error: true,
                metadata: None,
            }),
        }
    }
}

fn workspace_tool_allowed(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_files"
            | "search"
            | "glob"
            | "git_status"
            | "edit"
            | "apply_patch"
            | "create_file"
            | "delete_file"
            | "bash"
    )
}

/// 工具未在 Gateway 注册时的兜底绑定：单个必填 `path`（历史语义）。
const FALLBACK_PATH_BINDINGS: &[PathBinding] = &[PathBinding::required("path")];
/// `git_status` 的历史豁免：path 缺省时回落到工作区根。
const GIT_STATUS_PATH_BINDINGS: &[PathBinding] = &[PathBinding::default_root("path")];

fn fallback_bindings(tool_name: &str) -> &'static [PathBinding] {
    if tool_name == "git_status" {
        GIT_STATUS_PATH_BINDINGS
    } else {
        FALLBACK_PATH_BINDINGS
    }
}

/// 将模型输入中的路径参数绑定到当前会话工作区。即使模型传入绝对路径也要经
/// PathGuard 重新解析，以抵御 `..`、符号链接和 CWD 逃逸。
///
/// 哪些键是路径由工具自己通过 [`Tool::path_bindings`] 声明——过去这里硬编码
/// `"path"`，导致 `glob`（`pattern` + `path`）和 `bash`（`command` + `cwd`）
/// 这类多参数工具无法接入。
fn bind_workspace_paths(
    tool_name: &str,
    bindings: &[PathBinding],
    mut input: serde_json::Value,
    guard: &PathGuard,
    require_existing: bool,
) -> Result<serde_json::Value, ProductError> {
    let object = input.as_object_mut().ok_or_else(|| {
        ProductError::Other(format!("tool '{tool_name}' expects an object input"))
    })?;

    for binding in bindings {
        let provided = object
            .get(binding.key)
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);

        let raw_path = match (provided, binding.arity) {
            (Some(value), _) => value,
            (None, PathArity::DefaultRoot) => guard.root().display().to_string(),
            // 缺失即拒绝：绝不静默回落到进程 CWD（那是工作区之外）。
            (None, PathArity::Required) => {
                return Err(ProductError::Other(format!(
                    "tool '{tool_name}' is missing required path parameter '{}'",
                    binding.key
                )))
            }
            (None, PathArity::Optional) => continue,
        };

        let requested = PathBuf::from(raw_path);
        let candidate = if requested.is_absolute() {
            requested
        } else {
            guard.root().join(requested)
        };
        let resolved = if require_existing {
            guard.resolve_existing(&candidate)?
        } else {
            guard.resolve(&candidate)?
        };
        object.insert(
            binding.key.to_string(),
            serde_json::Value::String(resolved.display().to_string()),
        );
    }
    Ok(input)
}

#[async_trait]
impl ToolHost for SessionToolHost {
    async fn list_tools(&self) -> hermes_error::Result<Vec<ToolSpec>> {
        Ok(self.tool_specs())
    }

    async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> hermes_error::Result<ToolCallOutcome> {
        self.call_inner(None, name, args).await
    }

    async fn call_with_id(
        &self,
        call_id: &str,
        name: &str,
        args: serde_json::Value,
    ) -> hermes_error::Result<ToolCallOutcome> {
        self.call_inner(Some(call_id), name, args).await
    }
}

fn delegation_tool_specs(codex_available: bool) -> Vec<ToolSpec> {
    let delegate_description = if codex_available {
        "Start an independent subagent. It is read-only by default; choose access='full_access' \
only when the user conversation or the parent plan explicitly assigns workspace edits or commands. \
Use agent='auto' to apply the visible routing policy, 'r_code' for the current provider, or \
agent='codex' for the user's installed Codex CLI. Always set complexity. Use Codex when the user \
explicitly requests it. Call collect_subagents before your final answer."
    } else {
        "Start an independent R-Code subagent. It is read-only by default; choose \
access='full_access' only when the user conversation or the parent plan explicitly assigns \
workspace edits or commands. Call collect_subagents before your final answer."
    };
    vec![
        ToolSpec {
            name: "delegate_task".to_string(),
            description: delegate_description.to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "A focused task for the delegated subagent."
                    },
                    "label": {
                        "type": "string",
                        "description": "A short user-visible label for this investigation."
                    },
                    "agent": {
                        "type": "string",
                        "enum": if codex_available { vec!["auto", "r_code", "codex"] } else { vec!["auto", "r_code"] },
                        "default": "auto",
                        "description": "Execution backend. Auto applies the configured, user-visible router."
                    },
                    "complexity": {
                        "type": "string",
                        "enum": ["simple", "standard", "complex"],
                        "default": "standard",
                        "description": "Task complexity used by the configured routing policy."
                    },
                    "access": {
                        "type": "string",
                        "enum": ["read_only", "full_access"],
                        "default": "read_only",
                        "description": "Workspace capability. Keep read_only unless the conversation or parent plan explicitly delegates edits or commands."
                    }
                },
                "required": ["goal"]
            }),
            source: ToolSource::Builtin,
            requires_confirmation: false,
        },
        ToolSpec {
            name: "collect_subagents".to_string(),
            description: "Wait for delegated subagents and return their concise summaries. \
Use optional ids to collect a subset; omit ids to collect all."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional subagent run IDs."
                    }
                }
            }),
            source: ToolSource::Builtin,
            requires_confirmation: false,
        },
    ]
}

/// 生成工具输入的人类可读摘要（审批卡展示用）。
///
/// 命令类工具额外附上分级原因：用户被打断时应当立刻看懂"为什么要问我"，
/// 而不是只看到一条命令字符串。
fn summarize_input(name: &str, args: &serde_json::Value) -> String {
    if name == "bash" {
        if let Some(command) = args.get("command").and_then(|v| v.as_str()) {
            let classification = classify_shell_command(command);
            let command = truncate_summary(command);
            return if classification.reasons.is_empty() {
                format!("bash {command}")
            } else {
                format!("bash {command} — {}", classification.reasons.join("；"))
            };
        }
    }
    for key in [
        "path",
        "file_path",
        "filePath",
        "command",
        "cmd",
        "query",
        "pattern",
    ] {
        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
            return format!("{name} {}", truncate_summary(v));
        }
    }
    name.to_string()
}

/// 摘要截断到 120 个**字符**。
///
/// 原实现按字节切 (`&v[..120]`)：路径或命令里有中文时，第 120 字节大概率落在
/// 多字节序列中间，`str` 索引会直接 panic。审批摘要不该有能崩溃的路径。
fn truncate_summary(text: &str) -> String {
    const MAX: usize = 120;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX).collect();
    format!("{head}…")
}

/// 父运行内的受限子代理监督器。它只负责并发、隔离、取消和事件转发；主 Agent
/// 必须通过 `collect_subagents` 获得摘要，子代理的完整过程不会进入主会话历史。
#[derive(Clone)]
struct SubagentSupervisor {
    provider: Arc<dyn LlmProvider>,
    gateway: Arc<ToolGateway>,
    event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    task_id: String,
    parent_run_id: String,
    model: String,
    max_tokens: u32,
    temperature: Option<f32>,
    inference: InferenceOptions,
    parent_abort: Arc<AtomicBool>,
    workspace_scope: Option<WorkspaceScope>,
    codex_subagent_runner: Option<Arc<dyn CodexSubagentRunner>>,
    cross_engine_delegation_enabled: Arc<AtomicBool>,
    semaphore: Arc<Semaphore>,
    children: Arc<Mutex<HashMap<String, SubagentHandle>>>,
    orchestration: OrchestrationPolicy,
    agent_prompts: AgentPromptPolicy,
}

#[derive(Clone)]
struct SubagentHandle {
    scope: AgentEventScope,
    abort: Arc<AtomicBool>,
    result_rx: watch::Receiver<Option<SubagentResult>>,
}

#[derive(Debug, Clone)]
struct SubagentResult {
    state: SubagentState,
    summary: String,
}

enum QualityReviewResult {
    Passed(String),
    Revise(String),
}

impl SubagentSupervisor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        provider: Arc<dyn LlmProvider>,
        gateway: Arc<ToolGateway>,
        event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        task_id: String,
        parent_run_id: String,
        model: String,
        max_tokens: u32,
        temperature: Option<f32>,
        inference: InferenceOptions,
        parent_abort: Arc<AtomicBool>,
        workspace_scope: Option<WorkspaceScope>,
        codex_subagent_runner: Option<Arc<dyn CodexSubagentRunner>>,
        cross_engine_delegation_enabled: Arc<AtomicBool>,
        orchestration: OrchestrationPolicy,
        agent_prompts: AgentPromptPolicy,
    ) -> Self {
        Self {
            provider,
            gateway,
            event_tx,
            task_id,
            parent_run_id,
            model,
            max_tokens,
            temperature,
            inference,
            parent_abort,
            workspace_scope,
            codex_subagent_runner,
            cross_engine_delegation_enabled,
            semaphore: Arc::new(Semaphore::new(MAX_PARALLEL_SUBAGENTS)),
            children: Arc::new(Mutex::new(HashMap::new())),
            orchestration,
            agent_prompts,
        }
    }

    fn codex_available(&self) -> bool {
        self.cross_engine_delegation_enabled.load(Ordering::SeqCst)
            && self.codex_configured()
            && self.workspace_scope.is_some()
    }

    fn codex_configured(&self) -> bool {
        self.codex_subagent_runner.is_some()
    }

    fn route_backend(
        &self,
        requested: &str,
        complexity: TaskComplexity,
    ) -> Result<(SubagentBackend, String), ProductError> {
        match requested {
            "r_code" | "native" => Ok((
                SubagentBackend::RCode,
                "主智能体显式选择 R-Code 子智能体".to_string(),
            )),
            "codex" | "codex_cli" => {
                if !self.codex_available() {
                    let reason = if !self.cross_engine_delegation_enabled.load(Ordering::SeqCst) {
                        "Codex 子代理已在设置中关闭，本次委派已回退 R-Code"
                    } else {
                        "Codex 子代理当前不可用，本次委派已回退 R-Code"
                    };
                    return Ok((SubagentBackend::RCode, reason.to_string()));
                }
                Ok((
                    SubagentBackend::Codex,
                    "主智能体显式选择 Codex CLI 子智能体".to_string(),
                ))
            }
            "auto" => {
                let prefer_codex = match self.orchestration.delegation_router {
                    DelegationRouterMode::Manual | DelegationRouterMode::RCodeFirst => false,
                    DelegationRouterMode::Balanced => complexity == TaskComplexity::Complex,
                    DelegationRouterMode::CodexFirst => complexity != TaskComplexity::Simple,
                };
                if prefer_codex && self.codex_available() {
                    let reason = match self.orchestration.delegation_router {
                        DelegationRouterMode::Balanced => "均衡路由：复杂任务优先 Codex CLI",
                        DelegationRouterMode::CodexFirst => {
                            "Codex 优先路由：标准或复杂任务使用 Codex CLI"
                        }
                        _ => unreachable!("only Codex-preferring policies reach this branch"),
                    };
                    Ok((SubagentBackend::Codex, reason.to_string()))
                } else {
                    let reason = if prefer_codex {
                        "自动路由原计划使用 Codex，但当前不可用，已回退 R-Code"
                    } else {
                        match self.orchestration.delegation_router {
                            DelegationRouterMode::Manual => "手动路由：auto 安全回退 R-Code",
                            DelegationRouterMode::Balanced => "均衡路由：简单或标准任务使用 R-Code",
                            DelegationRouterMode::RCodeFirst => "R-Code 优先路由",
                            DelegationRouterMode::CodexFirst => {
                                "Codex 优先路由：简单任务使用 R-Code"
                            }
                        }
                    };
                    Ok((SubagentBackend::RCode, reason.to_string()))
                }
            }
            value => Err(ProductError::Other(format!(
                "delegate_task received unsupported agent '{value}'"
            ))),
        }
    }

    fn quality_backend(&self) -> (SubagentBackend, String) {
        match self.orchestration.quality_reviewer {
            QualityReviewer::RCode => (
                SubagentBackend::RCode,
                "质量循环设置指定 R-Code 复核".to_string(),
            ),
            QualityReviewer::Codex if self.codex_available() => (
                SubagentBackend::Codex,
                "质量循环设置指定 Codex CLI 复核".to_string(),
            ),
            QualityReviewer::Codex => (
                SubagentBackend::RCode,
                "质量循环原计划使用 Codex，但当前不可用，已回退 R-Code".to_string(),
            ),
            QualityReviewer::Auto if self.codex_available() => (
                SubagentBackend::Codex,
                "质量循环自动交叉选择 Codex CLI 复核 R-Code 主结果".to_string(),
            ),
            QualityReviewer::Auto => (
                SubagentBackend::RCode,
                "质量循环自动选择当前可用的 R-Code 复核器".to_string(),
            ),
        }
    }

    async fn quality_review(
        &self,
        round: u8,
        review_packet: &str,
        draft: &str,
    ) -> Result<QualityReviewResult, ProductError> {
        let (backend, routing_reason) = self.quality_backend();
        let goal = build_quality_review_goal(review_packet, draft);
        let queued = self
            .spawn(
                backend,
                Some(format!("质量复核 {round}")),
                goal,
                SubagentAccessMode::ReadOnly,
                None,
                routing_reason,
            )
            .await?;
        let payload: serde_json::Value = serde_json::from_str(&queued.content)
            .map_err(|error| ProductError::Other(format!("无法读取质量复核任务：{error}")))?;
        let id = payload
            .get("subagent_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ProductError::Other("质量复核任务缺少运行 ID".to_string()))?;
        let collected = self.collect(Some(vec![id.to_string()])).await?;
        let value: serde_json::Value = serde_json::from_str(&collected.content)
            .map_err(|error| ProductError::Other(format!("无法读取质量复核结果：{error}")))?;
        let summary = value
            .pointer("/subagents/0/summary")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("质量复核没有返回可见摘要")
            .trim()
            .to_string();
        let first_line = summary.lines().next().unwrap_or_default().trim();
        if first_line.eq_ignore_ascii_case("pass")
            || first_line.eq_ignore_ascii_case("[pass]")
            || first_line == "通过"
        {
            Ok(QualityReviewResult::Passed(summary))
        } else {
            Ok(QualityReviewResult::Revise(summary))
        }
    }

    async fn spawn(
        &self,
        backend: SubagentBackend,
        label: Option<String>,
        goal: String,
        access_mode: SubagentAccessMode,
        delegated_by_tool_call_id: Option<String>,
        routing_reason: String,
    ) -> Result<ToolCallOutcome, ProductError> {
        if self.parent_abort.load(Ordering::Relaxed) {
            return Err(ProductError::Other(
                "主运行正在停止，不能再委派子代理".to_string(),
            ));
        }
        if backend == SubagentBackend::Codex && self.codex_subagent_runner.is_none() {
            return Err(ProductError::Other(
                "当前 R-Code 宿主没有启用 Codex CLI 子代理桥".to_string(),
            ));
        }
        if backend == SubagentBackend::Codex && self.workspace_scope.is_none() {
            return Err(ProductError::Other(
                "Codex 子代理需要先为当前对话附加一个工作区".to_string(),
            ));
        }
        let run_id = Uuid::new_v4().to_string();
        let label = normalize_subagent_label(label, &goal);
        let label = match backend {
            SubagentBackend::RCode => label,
            SubagentBackend::Codex => format!("Codex CLI · {label}"),
        };
        let scope = AgentEventScope {
            run_id: run_id.clone(),
            agent_id: run_id.clone(),
            parent_run_id: Some(self.parent_run_id.clone()),
            agent_kind: AgentKind::Subagent,
            agent_label: Some(label.clone()),
            delegated_by_tool_call_id: delegated_by_tool_call_id.clone(),
            runtime_kind: match backend {
                SubagentBackend::RCode => AgentRunRuntimeKind::Native,
                SubagentBackend::Codex => AgentRunRuntimeKind::CodexExec,
            },
            model: Some(match backend {
                SubagentBackend::RCode => self.model.clone(),
                SubagentBackend::Codex => "codex-cli".to_string(),
            }),
            access_mode,
            routing_reason: Some(routing_reason.clone()),
        };
        let abort = Arc::new(AtomicBool::new(false));
        let (result_tx, result_rx) = watch::channel(None);
        {
            let mut children = self.children.lock().await;
            if children.len() >= MAX_SUBAGENTS_PER_RUN {
                return Err(ProductError::Other(format!(
                    "单次运行最多可委派 {MAX_SUBAGENTS_PER_RUN} 个子代理"
                )));
            }
            children.insert(
                run_id.clone(),
                SubagentHandle {
                    scope: scope.clone(),
                    abort: abort.clone(),
                    result_rx,
                },
            );
        }

        emit_scoped(
            &self.event_tx,
            &scope,
            AgentEvent::SubagentLifecycle {
                state: SubagentState::Queued,
                detail: Some(format!(
                    "{} · {}",
                    match backend {
                        SubagentBackend::RCode => "已加入 R-Code 子代理队列",
                        SubagentBackend::Codex => "已加入 Codex CLI 子代理队列",
                    },
                    routing_reason
                )),
            },
        );

        let supervisor = self.clone();
        tokio::spawn(async move {
            supervisor
                .run_child(
                    backend,
                    scope,
                    abort,
                    goal,
                    delegated_by_tool_call_id,
                    result_tx,
                )
                .await;
        });

        Ok(ToolCallOutcome {
            content: serde_json::json!({
                "subagent_id": run_id,
                "label": label,
                "agent": match backend {
                    SubagentBackend::RCode => "r_code",
                    SubagentBackend::Codex => "codex",
                },
                "access": access_mode.to_string(),
                "routing_reason": routing_reason,
                "status": "queued"
            })
            .to_string(),
            is_error: false,
            metadata: None,
        })
    }

    async fn collect(&self, ids: Option<Vec<String>>) -> Result<ToolCallOutcome, ProductError> {
        let (handles, collected_ids) = {
            let children = self.children.lock().await;
            let ids = ids.unwrap_or_else(|| {
                let mut all = children.keys().cloned().collect::<Vec<_>>();
                all.sort();
                all
            });
            let mut handles = Vec::with_capacity(ids.len());
            for id in &ids {
                let handle = children
                    .get(id)
                    .cloned()
                    .ok_or_else(|| ProductError::Other(format!("未知子代理：{id}")))?;
                handles.push(handle);
            }
            (handles, ids)
        };

        let mut summaries = Vec::with_capacity(handles.len());
        for handle in handles {
            let result = wait_for_subagent(&handle).await;
            summaries.push(serde_json::json!({
                "subagent_id": handle.scope.run_id,
                "label": handle.scope.agent_label,
                "access": handle.scope.access_mode.to_string(),
                "status": result.state.to_string(),
                "summary": result.summary,
            }));
        }

        // 收集完成后删除 handle，使 has_children() 只反映尚未收集的子代理。
        {
            let mut children = self.children.lock().await;
            for id in &collected_ids {
                children.remove(id);
            }
        }

        Ok(ToolCallOutcome {
            content: serde_json::json!({ "subagents": summaries }).to_string(),
            is_error: false,
            metadata: None,
        })
    }

    /// 是否有尚未收集结果的子代理。
    async fn has_children(&self) -> bool {
        !self.children.lock().await.is_empty()
    }

    async fn abort_all(&self) {
        let handles = self
            .children
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for handle in handles {
            if handle.result_rx.borrow().is_none() {
                handle.abort.store(true, Ordering::Relaxed);
                emit_scoped(
                    &self.event_tx,
                    &handle.scope,
                    AgentEvent::SubagentLifecycle {
                        state: SubagentState::Cancelled,
                        detail: Some("主运行已请求停止".to_string()),
                    },
                );
            }
        }
    }

    async fn abort_one(&self, subagent_id: &str) -> bool {
        let handle = self.children.lock().await.get(subagent_id).cloned();
        let Some(handle) = handle else {
            return false;
        };
        if handle.result_rx.borrow().is_some() {
            return false;
        }
        handle.abort.store(true, Ordering::Relaxed);
        emit_scoped(
            &self.event_tx,
            &handle.scope,
            AgentEvent::SubagentLifecycle {
                state: SubagentState::Cancelled,
                detail: Some("已请求停止此子代理".to_string()),
            },
        );
        true
    }

    async fn wait_for_all(&self) {
        let handles = self
            .children
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for handle in handles {
            let _ = wait_for_subagent(&handle).await;
        }
    }

    async fn run_child(
        self,
        backend: SubagentBackend,
        scope: AgentEventScope,
        abort: Arc<AtomicBool>,
        goal: String,
        _delegated_by_tool_call_id: Option<String>,
        result_tx: watch::Sender<Option<SubagentResult>>,
    ) {
        let permit = match self.semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                self.finish_child(
                    &scope,
                    SubagentState::Failed,
                    "子代理调度器已关闭".to_string(),
                    result_tx,
                );
                return;
            }
        };
        if self.is_child_cancelled(&abort) {
            self.finish_child(
                &scope,
                SubagentState::Cancelled,
                "子代理已在启动前取消".to_string(),
                result_tx,
            );
            drop(permit);
            return;
        }
        emit_scoped(
            &self.event_tx,
            &scope,
            AgentEvent::SubagentLifecycle {
                state: SubagentState::Running,
                detail: Some(match (backend, scope.access_mode) {
                    (SubagentBackend::RCode, SubagentAccessMode::ReadOnly) => {
                        "R-Code 子智能体正在进行只读调查".to_string()
                    }
                    (SubagentBackend::RCode, SubagentAccessMode::FullAccess) => {
                        "R-Code 子智能体已获完全访问权限".to_string()
                    }
                    (SubagentBackend::Codex, SubagentAccessMode::ReadOnly) => {
                        "Codex CLI 子智能体正在进行只读调查".to_string()
                    }
                    (SubagentBackend::Codex, SubagentAccessMode::FullAccess) => {
                        "Codex CLI 子智能体已获完全访问权限".to_string()
                    }
                }),
            },
        );

        if backend == SubagentBackend::Codex {
            let runner = self
                .codex_subagent_runner
                .clone()
                .expect("Codex runner checked before child creation");
            let Some(workspace) = self
                .workspace_scope
                .as_ref()
                .map(|scope| scope.guard.root().to_path_buf())
            else {
                drop(permit);
                self.finish_child(
                    &scope,
                    SubagentState::Failed,
                    "Codex 子代理需要已附加的工作区".to_string(),
                    result_tx,
                );
                return;
            };
            let event_tx = self.event_tx.clone();
            let event_scope = scope.clone();
            let event_sink: CodexSubagentEventSink = Arc::new(move |event| {
                emit_scoped(&event_tx, &event_scope, event);
            });
            let outcome = runner
                .run(CodexSubagentRequest {
                    workspace,
                    goal,
                    task_id: self.task_id.clone(),
                    run_id: scope.run_id.clone(),
                    caller: format!("subagent:{}", scope.agent_id),
                    access_mode: scope.access_mode,
                    abort: abort.clone(),
                    event_sink,
                })
                .await;
            drop(permit);
            if self.is_child_cancelled(&abort) {
                self.finish_child(
                    &scope,
                    SubagentState::Cancelled,
                    "Codex CLI 子代理已停止".to_string(),
                    result_tx,
                );
                return;
            }
            match outcome {
                Ok(CodexSubagentOutcome::Completed(summary)) => {
                    self.finish_child(&scope, SubagentState::Completed, summary, result_tx)
                }
                Ok(CodexSubagentOutcome::Cancelled) => self.finish_child(
                    &scope,
                    SubagentState::Cancelled,
                    "Codex CLI 子代理已停止".to_string(),
                    result_tx,
                ),
                Err(error) => {
                    let error = error.to_string();
                    emit_scoped(
                        &self.event_tx,
                        &scope,
                        AgentEvent::Message {
                            text: format!("[error] {error}"),
                            delta: false,
                        },
                    );
                    self.finish_child(&scope, SubagentState::Failed, error, result_tx);
                }
            }
            return;
        }

        let tool_host = SessionToolHost {
            gateway: self.gateway.clone(),
            task_id: self.task_id.clone(),
            run_id: scope.run_id.clone(),
            abort: abort.clone(),
            workspace_scope: self.workspace_scope.clone(),
            policy: match scope.access_mode {
                SubagentAccessMode::ReadOnly => ToolPolicy::ReadOnly,
                SubagentAccessMode::FullAccess => ToolPolicy::FullAccess,
            },
            caller: format!("subagent:{}", scope.agent_id),
            delegation: None,
            delegation_disabled: Arc::new(AtomicBool::new(true)),
        };
        let tools = tool_host.tool_specs();
        let mut messages = vec![Message::user_text(goal)];
        let mut terminal_error: Option<String> = None;

        for iteration in 0..MAX_SUBAGENT_ITERATIONS {
            if self.is_child_cancelled(&abort) {
                break;
            }
            emit_scoped(
                &self.event_tx,
                &scope,
                AgentEvent::Activity {
                    phase: AgentActivityPhase::Requesting,
                    detail: None,
                },
            );
            let request = CompletionRequest {
                model: self.model.clone(),
                system: Some(build_subagent_system_prompt(
                    !tools.is_empty(),
                    scope.access_mode,
                    &self.agent_prompts.subagent,
                )),
                messages: Vec::new(),
                tools: Vec::new(),
                max_tokens: self.max_tokens,
                temperature: self.temperature,
                enable_caching: !tools.is_empty(),
                inference: self.inference.clone(),
            };
            let event_tx = self.event_tx.clone();
            let event_scope = scope.clone();
            let outcome = run_agent_loop_iteration_with_abort_and_emit(
                self.provider.as_ref(),
                &tool_host,
                request,
                &mut messages,
                &tools,
                Some(abort.as_ref()),
                true,
                move |event| emit_scoped(&event_tx, &event_scope, event),
            )
            .await;

            match outcome {
                Ok(outcome) if !outcome.had_tool_call => break,
                Ok(_) if iteration + 1 >= MAX_SUBAGENT_ITERATIONS => {
                    terminal_error =
                        Some(format!("达到 {MAX_SUBAGENT_ITERATIONS} 轮只读工具调用上限"));
                    break;
                }
                Ok(_) => {}
                Err(error) => {
                    terminal_error = Some(user_facing_provider_error(&error.to_string()));
                    break;
                }
            }
        }

        drop(permit);
        if self.is_child_cancelled(&abort) {
            self.finish_child(
                &scope,
                SubagentState::Cancelled,
                "子代理已停止".to_string(),
                result_tx,
            );
            return;
        }
        if let Some(error) = terminal_error {
            emit_scoped(
                &self.event_tx,
                &scope,
                AgentEvent::Message {
                    text: format!("[error] {error}"),
                    delta: false,
                },
            );
            self.finish_child(&scope, SubagentState::Failed, error, result_tx);
            return;
        }
        self.finish_child(
            &scope,
            SubagentState::Completed,
            final_subagent_summary(&messages),
            result_tx,
        );
    }

    fn is_child_cancelled(&self, child_abort: &AtomicBool) -> bool {
        self.parent_abort.load(Ordering::Relaxed) || child_abort.load(Ordering::Relaxed)
    }

    fn finish_child(
        &self,
        scope: &AgentEventScope,
        state: SubagentState,
        summary: String,
        result_tx: watch::Sender<Option<SubagentResult>>,
    ) {
        let visible = short_summary(&summary, 180);
        emit_scoped(
            &self.event_tx,
            scope,
            AgentEvent::SubagentLifecycle {
                state,
                detail: Some(visible),
            },
        );
        let _ = result_tx.send(Some(SubagentResult { state, summary }));
    }
}

fn emit_scoped(
    event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    scope: &AgentEventScope,
    event: AgentEvent,
) {
    let _ = event_tx.send(AgentEvent::Scoped {
        scope: scope.clone(),
        event: Box::new(event),
    });
}

async fn wait_for_subagent(handle: &SubagentHandle) -> SubagentResult {
    let mut receiver = handle.result_rx.clone();
    loop {
        if let Some(result) = receiver.borrow().clone() {
            return result;
        }
        if receiver.changed().await.is_err() {
            return SubagentResult {
                state: SubagentState::Failed,
                summary: "子代理在返回结果前意外停止".to_string(),
            };
        }
    }
}

fn normalize_subagent_label(label: Option<String>, goal: &str) -> String {
    let raw = label
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| goal.trim().to_string());
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        "只读调查".to_string()
    } else {
        short_summary(&normalized, 72)
    }
}

fn final_subagent_summary(messages: &[Message]) -> String {
    let summary = messages
        .iter()
        .rev()
        .find(|message| message.role == hermes_core::Role::Assistant)
        .map(Message::text_content)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "子代理未产生可见摘要。".to_string());
    short_summary(&summary, MAX_SUBAGENT_SUMMARY_CHARS)
}

fn short_summary(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let clipped = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    format!("{clipped}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use futures::StreamExt;
    use hermes_core::{Capabilities, CompletionResponse, StopReason, StreamEvent};
    use hermes_error::Error as HermesError;
    use hermes_llm::MockProvider;
    use r_code_core::dto::{ProjectAccessMode, TaskMode};
    use r_code_gateway::{PermissionEngine, ToolGateway};
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;
    use tokio::sync::Notify;

    fn test_gateway() -> Arc<ToolGateway> {
        let engine = Arc::new(PermissionEngine::new());
        let mut gateway = ToolGateway::new(engine);
        gateway.register(Box::new(r_code_gateway::ReadFileTool));
        Arc::new(gateway)
    }

    struct RecordingCodexRunner {
        calls: AtomicUsize,
    }

    struct GatedQualityRunner {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl CodexSubagentRunner for RecordingCodexRunner {
        async fn run(
            &self,
            request: CodexSubagentRequest,
        ) -> Result<CodexSubagentOutcome, ProductError> {
            assert!(request.workspace.is_dir());
            assert_eq!(request.goal, "请 Codex 检查边界");
            assert_eq!(request.task_id, "task-1");
            assert!(!request.run_id.is_empty());
            assert_eq!(request.access_mode, SubagentAccessMode::ReadOnly);
            self.calls.fetch_add(1, Ordering::Relaxed);
            (request.event_sink)(AgentEvent::Activity {
                phase: AgentActivityPhase::Tool,
                detail: Some("Codex 正在读取边界文件".to_string()),
            });
            Ok(CodexSubagentOutcome::Completed(
                "Codex 返回的只读结论".to_string(),
            ))
        }
    }

    #[async_trait]
    impl CodexSubagentRunner for GatedQualityRunner {
        async fn run(
            &self,
            request: CodexSubagentRequest,
        ) -> Result<CodexSubagentOutcome, ProductError> {
            assert!(request
                .goal
                .contains("Provisional draft (not yet delivered)"));
            self.started.notify_one();
            self.release.notified().await;
            Ok(CodexSubagentOutcome::Completed(
                "PASS\n验收包与草稿一致".to_string(),
            ))
        }
    }

    fn input() -> CreateSessionInput {
        CreateSessionInput {
            workspace_path: None,
            workspace_access_mode: ProjectAccessMode::RequestApproval,
            task_id: "task-1".into(),
            goal: "do thing".into(),
            mode: TaskMode::Ask,
            model: None,
            inference: Default::default(),
            context: vec![],
        }
    }

    /// 第一轮在首个流事件前等待，用于稳定复现“无工具文本收尾时收到 steer”的边界。
    struct DelayedProvider {
        turns: StdMutex<VecDeque<(bool, Vec<StreamEvent>)>>,
        first_turn_release: Arc<Notify>,
        requests: Arc<StdMutex<Vec<CompletionRequest>>>,
    }

    impl DelayedProvider {
        fn new(
            turns: Vec<(bool, Vec<StreamEvent>)>,
            first_turn_release: Arc<Notify>,
            requests: Arc<StdMutex<Vec<CompletionRequest>>>,
        ) -> Self {
            Self {
                turns: StdMutex::new(turns.into()),
                first_turn_release,
                requests,
            }
        }
    }

    #[async_trait]
    impl LlmProvider for DelayedProvider {
        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> hermes_error::Result<CompletionResponse> {
            Err(HermesError::Internal(
                "DelayedProvider only supports stream".to_string(),
            ))
        }

        async fn stream(
            &self,
            request: CompletionRequest,
        ) -> hermes_error::Result<futures::stream::BoxStream<'static, StreamEvent>> {
            self.requests.lock().unwrap().push(request);
            let (wait_for_release, events) = self
                .turns
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| HermesError::Internal("no scripted turn".to_string()))?;
            if wait_for_release {
                let release = self.first_turn_release.clone();
                Ok(Box::pin(
                    futures::stream::once(async move {
                        release.notified().await;
                        events
                    })
                    .flat_map(futures::stream::iter),
                ))
            } else {
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                supports_streaming: true,
                supports_tool_use: true,
                supports_vision: false,
                supports_prompt_caching: false,
                max_context_tokens: 16_000,
            }
        }

        fn name(&self) -> &str {
            "delayed"
        }
    }

    /// 始终保持流打开，用于验证监督器的并发槽位与取消路径。
    struct PendingProvider {
        requests: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LlmProvider for PendingProvider {
        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> hermes_error::Result<CompletionResponse> {
            Err(HermesError::Internal(
                "PendingProvider only supports stream".to_string(),
            ))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> hermes_error::Result<futures::stream::BoxStream<'static, StreamEvent>> {
            self.requests.fetch_add(1, Ordering::Relaxed);
            Ok(Box::pin(futures::stream::pending()))
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                supports_streaming: true,
                supports_tool_use: true,
                supports_vision: false,
                supports_prompt_caching: false,
                max_context_tokens: 16_000,
            }
        }

        fn name(&self) -> &str {
            "pending"
        }
    }

    fn test_supervisor(
        provider: Arc<dyn LlmProvider>,
        event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) -> SubagentSupervisor {
        SubagentSupervisor::new(
            provider,
            test_gateway(),
            event_tx,
            "task-1".to_string(),
            "parent-run".to_string(),
            "mock-model".to_string(),
            512,
            None,
            InferenceOptions::default(),
            Arc::new(AtomicBool::new(false)),
            None,
            None,
            Arc::new(AtomicBool::new(true)),
            OrchestrationPolicy::default(),
            AgentPromptPolicy::default(),
        )
    }

    #[tokio::test]
    async fn text_turn_completes_and_emits_state() {
        let provider = MockProvider::new("mock");
        provider.push_text_turn("done!", hermes_core::Usage::default());
        let mut rt = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        );

        let session = rt.create_session(input()).await.unwrap();
        rt.start_run(&session.meta.id, "go").await.unwrap();

        // 等 loop 跑完
        for _ in 0..50 {
            if !rt.is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!rt.is_running());

        let events = rt.poll_events().await.unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Message { text, .. } if text.contains("done!")
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::State {
                state: TaskState::ReviewReady
            }
        )));
    }

    #[tokio::test]
    async fn parent_result_is_not_delivered_before_quality_gate_finishes() {
        let directory = TempDir::new().unwrap();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let provider = MockProvider::new("mock");
        provider.push_text_turn("待复核草稿", hermes_core::Usage::default());
        let mut runtime = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        )
        .with_orchestration_policy(OrchestrationPolicy {
            quality_loop: QualityLoopMode::Always,
            quality_reviewer: QualityReviewer::Codex,
            ..OrchestrationPolicy::default()
        })
        .with_codex_subagent_runner(Arc::new(GatedQualityRunner {
            started: started.clone(),
            release: release.clone(),
        }));
        let session = runtime
            .create_session(CreateSessionInput {
                workspace_path: Some(directory.path().to_string_lossy().into_owned()),
                ..input()
            })
            .await
            .unwrap();

        runtime
            .start_run(&session.meta.id, "检查项目并给出结论")
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .expect("质量复核应在草稿生成后启动");

        let before_gate = runtime.poll_events().await.unwrap();
        assert!(runtime.is_running());
        assert!(before_gate.iter().any(|event| matches!(
            event,
            AgentEvent::Message { text, .. } if text.contains("待复核草稿")
        )));
        assert!(before_gate.iter().any(|event| matches!(
            event,
            AgentEvent::Activity {
                phase: AgentActivityPhase::Reviewing,
                ..
            }
        )));
        assert!(!before_gate.iter().any(|event| matches!(
            event,
            AgentEvent::State {
                state: TaskState::ReviewReady
            }
        )));

        release.notify_one();
        for _ in 0..100 {
            if !runtime.is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(!runtime.is_running());
        let after_gate = runtime.poll_events().await.unwrap();
        assert!(after_gate.iter().any(|event| matches!(
            event,
            AgentEvent::State {
                state: TaskState::ReviewReady
            }
        )));
    }

    #[tokio::test]
    async fn pure_chat_main_run_is_not_misidentified_as_a_subagent() {
        let release = Arc::new(Notify::new());
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let provider = DelayedProvider::new(
            vec![(
                false,
                vec![
                    StreamEvent::TextDelta {
                        text: "请先附加工作区".to_string(),
                    },
                    StreamEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ],
            )],
            release,
            requests.clone(),
        );
        let mut runtime = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        )
        .with_codex_subagent_runner(Arc::new(RecordingCodexRunner {
            calls: AtomicUsize::new(0),
        }));

        let session = runtime.create_session(input()).await.unwrap();
        runtime
            .start_run(&session.meta.id, "能调用 Codex 子代理吗")
            .await
            .unwrap();
        for _ in 0..50 {
            if !runtime.is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!runtime.is_running());

        let requests = requests.lock().unwrap();
        let request = requests.first().unwrap();
        let system = request.system.as_deref().unwrap();
        assert!(!system.contains("You are a read-only delegated subagent"));
        assert!(system.contains("requires an attached workspace"));
        assert!(!request
            .tools
            .iter()
            .any(|tool| tool.name == "delegate_task"));
    }

    #[tokio::test]
    async fn ask_main_run_exposes_codex_delegation_after_workspace_is_attached() {
        let directory = TempDir::new().unwrap();
        let release = Arc::new(Notify::new());
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let provider = DelayedProvider::new(
            vec![(
                false,
                vec![
                    StreamEvent::TextDelta {
                        text: "done".to_string(),
                    },
                    StreamEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ],
            )],
            release,
            requests.clone(),
        );
        let mut runtime = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        )
        .with_codex_subagent_runner(Arc::new(RecordingCodexRunner {
            calls: AtomicUsize::new(0),
        }));

        // 复现 UI 路径：先创建纯聊天 Ask 会话，随后在 Room 顶部附加工作区。
        let session = runtime.create_session(input()).await.unwrap();
        runtime
            .update_workspace_scope(
                &session.meta.id,
                Some(directory.path().to_string_lossy().into_owned()),
                ProjectAccessMode::RequestApproval,
            )
            .await
            .unwrap();
        runtime
            .start_run(&session.meta.id, "检查代码")
            .await
            .unwrap();
        for _ in 0..50 {
            if !runtime.is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!runtime.is_running());

        let requests = requests.lock().unwrap();
        let request = requests.first().unwrap();
        let system = request.system.as_deref().unwrap();
        assert!(system.contains("coding agent working inside a user-approved workspace"));
        assert!(system.contains("When the user explicitly asks for Codex"));
        assert!(!system.contains("You are a read-only delegated subagent"));
        assert!(request.tools.iter().any(|tool| tool.name == "read_file"));
        let delegate = request
            .tools
            .iter()
            .find(|tool| tool.name == "delegate_task")
            .unwrap();
        assert!(delegate.input_schema["properties"]["agent"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "codex"));
        assert!(request
            .tools
            .iter()
            .any(|tool| tool.name == "collect_subagents"));
    }

    #[tokio::test]
    async fn explicit_opt_out_hides_delegation_tools_even_with_codex_and_workspace() {
        let directory = TempDir::new().unwrap();
        let release = Arc::new(Notify::new());
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let provider = DelayedProvider::new(
            vec![(
                false,
                vec![
                    StreamEvent::TextDelta {
                        text: "我会直接完成".to_string(),
                    },
                    StreamEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ],
            )],
            release,
            requests.clone(),
        );
        let mut runtime = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        )
        .with_codex_subagent_runner(Arc::new(RecordingCodexRunner {
            calls: AtomicUsize::new(0),
        }));

        let session = runtime.create_session(input()).await.unwrap();
        runtime
            .update_workspace_scope(
                &session.meta.id,
                Some(directory.path().to_string_lossy().into_owned()),
                ProjectAccessMode::RequestApproval,
            )
            .await
            .unwrap();
        runtime
            .start_run(
                &session.meta.id,
                "这个任务你自己完成，不使用子代理，也不要调用 Codex CLI。",
            )
            .await
            .unwrap();
        for _ in 0..50 {
            if !runtime.is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let requests = requests.lock().unwrap();
        let request = requests.first().unwrap();
        assert!(request
            .system
            .as_deref()
            .unwrap()
            .contains("explicitly disables subagents"));
        assert!(!request
            .tools
            .iter()
            .any(|tool| matches!(tool.name.as_str(), "delegate_task" | "collect_subagents")));
    }

    #[tokio::test]
    async fn stale_tool_calls_cannot_bypass_the_delegation_latch() {
        let disabled = Arc::new(AtomicBool::new(true));
        let tool_host = SessionToolHost {
            gateway: test_gateway(),
            task_id: "task-1".to_string(),
            run_id: "parent-run".to_string(),
            abort: Arc::new(AtomicBool::new(false)),
            workspace_scope: None,
            policy: ToolPolicy::Main,
            caller: "agent".to_string(),
            delegation: None,
            delegation_disabled: disabled,
        };

        let delegate_error = tool_host
            .call_inner(
                Some("late-delegate"),
                "delegate_task",
                serde_json::json!({"goal": "inspect", "agent": "codex"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(delegate_error.contains("运行时拒绝了委派调用"));

        let shell_error = tool_host
            .call_inner(
                Some("late-shell"),
                "bash",
                serde_json::json!({"command": "codex login status 2>&1"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(shell_error.contains("运行时拒绝了外部 Agent CLI 命令"));
    }

    #[tokio::test]
    async fn codex_delegation_without_workspace_fails_before_queueing_a_child() {
        let runner = Arc::new(RecordingCodexRunner {
            calls: AtomicUsize::new(0),
        });
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let supervisor = SubagentSupervisor::new(
            Arc::new(MockProvider::new("mock")),
            test_gateway(),
            event_tx,
            "task-1".to_string(),
            "parent-run".to_string(),
            "mock-model".to_string(),
            512,
            None,
            InferenceOptions::default(),
            Arc::new(AtomicBool::new(false)),
            None,
            Some(runner.clone()),
            Arc::new(AtomicBool::new(true)),
            OrchestrationPolicy::default(),
            AgentPromptPolicy::default(),
        );

        let error = supervisor
            .spawn(
                SubagentBackend::Codex,
                None,
                "检查代码".to_string(),
                SubagentAccessMode::ReadOnly,
                Some("call-codex".to_string()),
                "测试显式选择 Codex".to_string(),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("先为当前对话附加一个工作区"));
        assert_eq!(runner.calls.load(Ordering::Relaxed), 0);
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn abort_emits_interrupted_state() {
        let provider = MockProvider::new("mock");
        provider.push_text_turn("x", hermes_core::Usage::default());
        let mut rt = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        );
        let session = rt.create_session(input()).await.unwrap();
        rt.abort(&session.meta.id).await.unwrap();
        assert!(rt.aborted());
        let events = rt.poll_events().await.unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::State {
                state: TaskState::Interrupted
            }
        )));
    }

    #[tokio::test]
    async fn steer_rejects_a_session_after_its_run_has_finished() {
        let provider = MockProvider::new("mock");
        let mut rt = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        );
        let session = rt.create_session(input()).await.unwrap();

        let result = rt.steer(&session.meta.id, "late steer").await.unwrap();

        assert_eq!(result, SteerResult::RunFinished);
        assert!(rt.poll_events().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn accepted_steer_emits_observable_confirmation() {
        let provider = MockProvider::new("mock");
        let mut rt = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        );
        let session = rt.create_session(input()).await.unwrap();
        {
            let mut sessions = rt.sessions.lock().await;
            let state = sessions.get_mut(&session.meta.id).unwrap();
            state.accepting_steer = true;
        }
        rt.running.store(true, Ordering::Relaxed);

        let result = rt.steer(&session.meta.id, "改为检查测试").await.unwrap();

        assert_eq!(result, SteerResult::Accepted);
        let events = rt.poll_events().await.unwrap();
        assert!(matches!(
            events.as_slice(),
            [AgentEvent::Activity {
                phase: AgentActivityPhase::SteerAccepted,
                ..
            }]
        ));
        let sessions = rt.sessions.lock().await;
        assert_eq!(
            sessions
                .get(&session.meta.id)
                .unwrap()
                .steer_queue
                .front()
                .map(String::as_str),
            Some("改为检查测试")
        );
    }

    #[tokio::test]
    async fn steer_received_during_a_text_only_turn_forces_the_next_request() {
        let release = Arc::new(Notify::new());
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let provider = DelayedProvider::new(
            vec![
                (
                    true,
                    vec![
                        StreamEvent::TextDelta {
                            text: "第一轮".to_string(),
                        },
                        StreamEvent::Stop {
                            reason: StopReason::EndTurn,
                        },
                    ],
                ),
                (
                    false,
                    vec![
                        StreamEvent::TextDelta {
                            text: "已按引导继续".to_string(),
                        },
                        StreamEvent::Stop {
                            reason: StopReason::EndTurn,
                        },
                    ],
                ),
            ],
            release.clone(),
            requests.clone(),
        );
        let mut rt = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        );
        let session = rt.create_session(input()).await.unwrap();
        rt.start_run(&session.meta.id, "开始").await.unwrap();

        // 等待第一轮已经进入 provider 流，但仍被 gate 阻塞。
        for _ in 0..50 {
            if rt.poll_events().await.unwrap().iter().any(|event| {
                matches!(
                    event,
                    AgentEvent::Activity {
                        phase: AgentActivityPhase::Requesting,
                        ..
                    }
                )
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(
            rt.steer(&session.meta.id, "改为检查边界").await.unwrap(),
            SteerResult::Accepted
        );
        release.notify_one();

        for _ in 0..100 {
            if !rt.is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(!rt.is_running());

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let applied = requests[1]
            .messages
            .iter()
            .map(Message::text_content)
            .find(|text| text.contains("改为检查边界"))
            .expect("下一轮请求必须包含已接纳的引导");
        assert!(applied.contains("supplemental guidance"));
        assert!(applied.contains("Preserve and complete the current user task"));
        assert!(applied.contains("Only replace or cancel"));
    }

    #[test]
    fn quality_review_packet_contains_the_current_goal_and_live_guidance() {
        let messages = vec![
            Message::user_text("检查整个项目并给出架构结论"),
            Message::assistant_text("我先读取关键模块。"),
            Message::user_text(format_live_guidance("先回答一下今天星期几")),
            Message::user_text(
                "[system] Delegated subagents have completed. Internal collection payload.",
            ),
            Message::assistant_text("今天是星期日；下面继续给出架构结论。"),
        ];

        let packet = current_run_review_packet(&messages);
        assert!(packet.contains("检查整个项目并给出架构结论"));
        assert!(packet.contains("先回答一下今天星期几"));
        assert!(!packet.contains("Internal collection payload"));

        let goal = build_quality_review_goal(&packet, "最终草稿");
        assert!(goal.contains("User task and accepted live guidance"));
        assert!(goal.contains("Provisional draft (not yet delivered)"));
        assert!(goal.contains("Do not load skills or broadly rescan the repository"));
        assert!(goal.contains("最终草稿"));
    }

    #[tokio::test]
    async fn delegate_task_routes_explicit_codex_requests_through_the_host_runner() {
        let directory = TempDir::new().unwrap();
        let workspace_scope = WorkspaceScope {
            guard: PathGuard::new(directory.path().to_path_buf()).unwrap(),
            access_mode: ProjectAccessMode::RequestApproval,
        };
        let runner = Arc::new(RecordingCodexRunner {
            calls: AtomicUsize::new(0),
        });
        let provider = MockProvider::new("mock");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let supervisor = Arc::new(SubagentSupervisor::new(
            Arc::new(provider),
            test_gateway(),
            event_tx,
            "task-1".to_string(),
            "parent-run".to_string(),
            "mock-model".to_string(),
            512,
            None,
            InferenceOptions::default(),
            Arc::new(AtomicBool::new(false)),
            Some(workspace_scope.clone()),
            Some(runner.clone()),
            Arc::new(AtomicBool::new(true)),
            OrchestrationPolicy::default(),
            AgentPromptPolicy::default(),
        ));
        let tool_host = SessionToolHost {
            gateway: test_gateway(),
            task_id: "task-1".to_string(),
            run_id: "parent-run".to_string(),
            abort: Arc::new(AtomicBool::new(false)),
            workspace_scope: Some(workspace_scope),
            policy: ToolPolicy::Main,
            caller: "agent".to_string(),
            delegation: Some(supervisor),
            delegation_disabled: Arc::new(AtomicBool::new(false)),
        };

        let delegate_spec = tool_host
            .tool_specs()
            .into_iter()
            .find(|tool| tool.name == "delegate_task")
            .unwrap();
        assert!(delegate_spec.description.contains("Codex CLI"));
        assert!(delegate_spec.input_schema["properties"]["agent"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "codex"));
        assert_eq!(
            delegate_spec.input_schema["properties"]["access"]["default"],
            "read_only"
        );

        let started = tool_host
            .call_inner(
                Some("call-codex"),
                "delegate_task",
                serde_json::json!({
                    "agent": "codex",
                    "goal": "请 Codex 检查边界",
                    "label": "检查边界"
                }),
            )
            .await
            .unwrap();
        let child_id = serde_json::from_str::<serde_json::Value>(&started.content).unwrap()
            ["subagent_id"]
            .as_str()
            .unwrap()
            .to_string();
        let collected = tool_host
            .call_inner(
                Some("collect-codex"),
                "collect_subagents",
                serde_json::json!({"ids": [child_id.clone()]}),
            )
            .await
            .unwrap();

        assert_eq!(runner.calls.load(Ordering::Relaxed), 1);
        assert!(collected.content.contains("Codex 返回的只读结论"));
        let events = std::iter::from_fn(|| event_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Scoped { scope, event }
                if scope.run_id == child_id
                    && scope.runtime_kind == AgentRunRuntimeKind::CodexExec
                    && scope.model.as_deref() == Some("codex-cli")
                    && matches!(
                        event.as_ref(),
                        AgentEvent::SubagentLifecycle {
                            state: SubagentState::Completed,
                            ..
                        }
                    )
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Scoped { scope, event }
                if scope.run_id == child_id
                    && matches!(
                        event.as_ref(),
                        AgentEvent::Activity {
                            phase: AgentActivityPhase::Tool,
                            detail: Some(detail),
                        } if detail == "Codex 正在读取边界文件"
                    )
        )));
    }

    #[test]
    fn explicit_codex_request_falls_back_when_cross_engine_delegation_is_disabled() {
        let provider = MockProvider::new("mock");
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(AtomicBool::new(true));
        let supervisor = SubagentSupervisor::new(
            Arc::new(provider),
            test_gateway(),
            event_tx,
            "task-1".to_string(),
            "parent-run".to_string(),
            "mock-model".to_string(),
            512,
            None,
            InferenceOptions::default(),
            Arc::new(AtomicBool::new(false)),
            None,
            None,
            gate.clone(),
            OrchestrationPolicy::default(),
            AgentPromptPolicy::default(),
        );
        gate.store(false, Ordering::SeqCst);

        let (backend, reason) = supervisor
            .route_backend("codex", TaskComplexity::Complex)
            .expect("关闭 Codex 后应平滑回退，而不是让主代理报错");

        assert_eq!(backend, SubagentBackend::RCode);
        assert!(reason.contains("回退 R-Code"));
    }

    #[tokio::test]
    async fn delegated_subagent_emits_scoped_lifecycle_and_returns_isolated_summary() {
        let provider = MockProvider::new("mock");
        provider.push_text_turn("只读调查结论", hermes_core::Usage::default());
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let supervisor = test_supervisor(Arc::new(provider), event_tx);

        let started = supervisor
            .spawn(
                SubagentBackend::RCode,
                Some("检查现状".to_string()),
                "只读调查".to_string(),
                SubagentAccessMode::ReadOnly,
                Some("call-delegate-1".to_string()),
                "测试显式选择 R-Code".to_string(),
            )
            .await
            .unwrap();
        let child_id = serde_json::from_str::<serde_json::Value>(&started.content).unwrap()
            ["subagent_id"]
            .as_str()
            .unwrap()
            .to_string();
        let collected = supervisor
            .collect(Some(vec![child_id.clone()]))
            .await
            .unwrap();

        assert!(collected.content.contains("只读调查结论"));
        let events = std::iter::from_fn(|| event_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Scoped {
                scope,
                event,
            } if scope.run_id == child_id
                && scope.parent_run_id.as_deref() == Some("parent-run")
                && scope.delegated_by_tool_call_id.as_deref() == Some("call-delegate-1")
                && matches!(
                    event.as_ref(),
                    AgentEvent::SubagentLifecycle {
                        state: SubagentState::Completed,
                        ..
                    }
                )
        )));
        assert!(events.iter().all(|event| !matches!(
            event,
            AgentEvent::Message { text, .. } if text.contains("只读调查结论")
        )));
    }

    #[tokio::test]
    async fn subagent_supervisor_limits_parallel_runs_to_three_and_cascades_cancel() {
        let requests = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(PendingProvider {
            requests: requests.clone(),
        });
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let supervisor = test_supervisor(provider, event_tx);
        let mut ids = Vec::new();
        for index in 0..4 {
            let started = supervisor
                .spawn(
                    SubagentBackend::RCode,
                    Some(format!("调查 {index}")),
                    format!("只读任务 {index}"),
                    SubagentAccessMode::ReadOnly,
                    None,
                    "测试并发子任务".to_string(),
                )
                .await
                .unwrap();
            ids.push(
                serde_json::from_str::<serde_json::Value>(&started.content).unwrap()["subagent_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }

        for _ in 0..100 {
            if requests.load(Ordering::Relaxed) == MAX_PARALLEL_SUBAGENTS {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(requests.load(Ordering::Relaxed), MAX_PARALLEL_SUBAGENTS);

        supervisor.abort_all().await;
        let collected = supervisor.collect(Some(ids)).await.unwrap();
        assert!(collected.content.contains("\"cancelled\""));

        let events = std::iter::from_fn(|| event_rx.try_recv().ok()).collect::<Vec<_>>();
        let running_count = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AgentEvent::Scoped {
                        event,
                        ..
                    } if matches!(
                        event.as_ref(),
                        AgentEvent::SubagentLifecycle {
                            state: SubagentState::Running,
                            ..
                        }
                    )
                )
            })
            .count();
        assert_eq!(running_count, MAX_PARALLEL_SUBAGENTS);
    }

    #[tokio::test]
    async fn replace_history_rebuilds_the_session_working_set() {
        let provider = MockProvider::new("mock");
        let mut rt = LlmAgentRuntime::new(
            Box::new(provider),
            "mock-model".into(),
            test_gateway(),
            None,
            None,
        );
        let session = rt.create_session(input()).await.unwrap();
        rt.replace_history(
            &session.meta.id,
            vec![hermes_core::Message::user_text("before fork")],
        )
        .await
        .unwrap();

        let sessions = rt.sessions.lock().await;
        let restored = sessions.get(&session.meta.id).unwrap();
        assert_eq!(restored.messages.len(), 1);
        assert_eq!(restored.messages[0].text_content(), "before fork");
    }

    #[test]
    fn summarize_picks_path() {
        let s = summarize_input("read_file", &serde_json::json!({"path": "src/a.rs"}));
        assert_eq!(s, "read_file src/a.rs");
    }

    #[test]
    fn summarize_bash_explains_the_escalation() {
        // 低风险命令：只显示命令本身 + 一条"已识别"说明
        let s = summarize_input("bash", &serde_json::json!({"command": "cargo test"}));
        assert!(s.starts_with("bash cargo test"), "summary was: {s}");

        // 被拒命令：用户必须看到原因
        let s = summarize_input("bash", &serde_json::json!({"command": "sudo rm -rf /"}));
        assert!(s.contains("提权"), "summary was: {s}");
    }

    #[test]
    fn summarize_does_not_panic_on_multibyte_boundaries() {
        // 旧实现按字节切片，中文路径超过 120 字节时会 panic。
        let long = "中".repeat(200);
        let s = summarize_input("read_file", &serde_json::json!({"path": long}));
        assert!(s.ends_with('…'));
    }

    #[test]
    fn read_only_policy_never_exposes_workspace_write_tools() {
        assert!(subagent_read_only_tool_allowed("read_file"));
        assert!(subagent_read_only_tool_allowed("search"));
        // glob 只读遍历，子代理可用
        assert!(subagent_read_only_tool_allowed("glob"));
        assert!(!subagent_read_only_tool_allowed("apply_patch"));
        assert!(!subagent_read_only_tool_allowed("create_file"));
        assert!(!subagent_read_only_tool_allowed("delete_file"));
        // 有副作用的新工具绝不给子代理
        assert!(!subagent_read_only_tool_allowed("edit"));
        assert!(!subagent_read_only_tool_allowed("bash"));
    }

    #[test]
    fn ask_mode_is_enforced_as_read_only_when_a_workspace_is_attached() {
        assert_eq!(
            tool_policy_for_task_mode(TaskMode::Ask),
            ToolPolicy::ReadOnly
        );
        assert_eq!(tool_policy_for_task_mode(TaskMode::Edit), ToolPolicy::Main);
        assert_eq!(tool_policy_for_task_mode(TaskMode::Auto), ToolPolicy::Main);
    }

    #[test]
    fn workspace_policy_exposes_the_new_tools() {
        for name in ["search", "glob", "edit", "bash", "read_file"] {
            assert!(workspace_tool_allowed(name), "{name} should be allowed");
        }
        assert!(!workspace_tool_allowed("load_skill"));
        assert!(!workspace_tool_allowed("nonexistent_tool"));
    }

    #[test]
    fn bind_paths_resolves_every_declared_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let guard = PathGuard::new(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        // bash：cwd 缺省回落到工作区根，command 不受影响
        let bound = bind_workspace_paths(
            "bash",
            &[PathBinding::default_root("cwd")],
            serde_json::json!({"command": "cargo test"}),
            &guard,
            true,
        )
        .unwrap();
        assert_eq!(bound["command"], "cargo test");
        assert!(bound["cwd"].as_str().unwrap().len() > 1);

        // 相对路径被拼到工作区根下
        let bound = bind_workspace_paths(
            "bash",
            &[PathBinding::default_root("cwd")],
            serde_json::json!({"command": "ls", "cwd": "sub"}),
            &guard,
            true,
        )
        .unwrap();
        assert!(bound["cwd"].as_str().unwrap().ends_with("sub"));

        // 必填键缺失 -> 报错，绝不回落到进程 CWD
        assert!(bind_workspace_paths(
            "glob",
            &[PathBinding::required("path")],
            serde_json::json!({"pattern": "**/*.rs"}),
            &guard,
            true,
        )
        .is_err());

        // 逃逸尝试被 PathGuard 拒绝
        assert!(bind_workspace_paths(
            "bash",
            &[PathBinding::default_root("cwd")],
            serde_json::json!({"command": "ls", "cwd": "../../etc"}),
            &guard,
            true,
        )
        .is_err());
    }

    #[test]
    fn registered_git_status_defaults_to_the_attached_workspace_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let expected_root = dir.path().canonicalize().unwrap();
        let engine = Arc::new(PermissionEngine::new());
        let mut gateway = ToolGateway::new(engine);
        gateway.register(Box::new(r_code_gateway::GitStatusTool));
        let host = SessionToolHost {
            gateway: Arc::new(gateway),
            task_id: "task-1".to_string(),
            run_id: "run-1".to_string(),
            abort: Arc::new(AtomicBool::new(false)),
            workspace_scope: Some(WorkspaceScope {
                guard: PathGuard::new(dir.path().to_path_buf()).unwrap(),
                access_mode: ProjectAccessMode::RequestApproval,
            }),
            policy: ToolPolicy::Main,
            caller: "agent".to_string(),
            delegation: None,
            delegation_disabled: Arc::new(AtomicBool::new(false)),
        };

        let bound = host
            .scoped_input("git_status", serde_json::json!({}))
            .unwrap();

        assert_eq!(
            PathBuf::from(bound["path"].as_str().unwrap()),
            expected_root
        );
    }

    #[test]
    fn max_tokens_provider_error_is_actionable() {
        let message = user_facing_provider_error(
            "API error: 400 - Invalid max_tokens value, the valid range of max_tokens is [1, 393216]",
        );
        assert!(message.contains("1,000,000 是上下文窗口"));
        assert!(message.contains("8,192"));
    }

    #[test]
    fn system_prompt_includes_fixed_local_clock() {
        let zone = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        let now = zone
            .with_ymd_and_hms(2026, 7, 26, 13, 20, 0)
            .single()
            .unwrap();
        let prompt = build_system_prompt_at(false, now);
        assert!(prompt.contains("2026-07-26T13:20:00+08:00"));
        assert!(prompt.contains("Sunday"));
        assert!(prompt.contains("ordinary, non-programming questions"));
    }

    #[test]
    fn workspace_prompts_prefer_parallel_independent_reads() {
        let prompt = build_system_prompt(true);
        assert!(prompt.contains("issue independent read-only tool calls together"));
        assert!(
            prompt.contains("Keep writes, shell commands, and result-dependent work sequential")
        );

        let child = build_subagent_system_prompt(
            true,
            SubagentAccessMode::ReadOnly,
            DEFAULT_SUBAGENT_PROMPT,
        );
        assert!(child.contains("issue independent read-only tool calls together"));
    }

    #[test]
    fn explicit_user_delegation_opt_out_is_a_hard_runtime_boundary() {
        let messages = vec![Message::user_text(
            "这个任务由你自己完成，不要使用子代理，也不要调用 Codex CLI。",
        )];

        assert_eq!(
            delegation_directive(&messages),
            DelegationDirective::Disabled
        );
        assert!(command_invokes_external_agent("codex login status 2>&1"));
        assert!(command_invokes_external_agent("claude --version"));
        assert!(!command_invokes_external_agent("cargo test -p r-code-core"));
    }

    #[test]
    fn custom_agent_prompts_are_layered_without_replacing_safety_prompt() {
        let prompts = AgentPromptPolicy {
            main_agent: "MAIN CUSTOM RELATIONSHIP".to_string(),
            subagent: "CHILD CUSTOM RELATIONSHIP".to_string(),
        };

        let main = build_main_system_prompt(true, &prompts);
        assert!(main.contains("All file paths are relative to the attached workspace"));
        assert!(main.contains("MAIN CUSTOM RELATIONSHIP"));

        let child =
            build_subagent_system_prompt(true, SubagentAccessMode::ReadOnly, &prompts.subagent);
        assert!(child.contains("read-only delegated subagent"));
        assert!(child.contains("CHILD CUSTOM RELATIONSHIP"));
    }
}
