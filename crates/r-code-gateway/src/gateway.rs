//! Tool Gateway -- 工具注册、权限检查、审计记账。 [doc-02 §9, §1, §8]
//!
//! Tool Gateway 是 Agent 工具调用的唯一入口。所有调用经过：
//! Schema 校验 -> 权限分级 -> 执行 -> 记账。
//!
//! `ToolGateway` 实现 `agent_contract::ToolHost` trait，可无缝接入 Agent 循环。
//!
//! ## 审计策略 [doc-02 §8]
//! 所有调用（含被拒绝 / 待审批）入 `ledger`，含调用者身份。

use std::collections::HashMap;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use agent_contract::{ToolCallOutcome, ToolHost, ToolSource, ToolSpec};
use async_trait::async_trait;
use r_code_core::dto::{PermissionDecision, ProjectAccessMode, RiskLevel, ToolCall};
use r_code_core::error::ProductError;
use r_code_core::security::PathGuard;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::permission::{PermissionCancellation, PermissionCheckResult, PermissionEngine};

/// 路径参数缺失时的处理策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathArity {
    /// 必须提供；缺失即拒绝调用（fail-closed）。
    Required,
    /// 缺失时回落到工作区根目录。
    DefaultRoot,
    /// 缺失时保持缺失，不注入任何值。
    Optional,
}

/// 声明工具输入中哪些键是文件系统路径，需经 `PathGuard` 重新解析。
///
/// 运行时用它把模型给出的任意路径（含绝对路径、`..`、符号链接）重绑定到当前
/// 会话工作区内。工具自己声明键名，运行时不再硬编码 `"path"`。
#[derive(Debug, Clone, Copy)]
pub struct PathBinding {
    /// 输入对象里的键名，例如 `"path"` / `"cwd"`。
    pub key: &'static str,
    /// 键缺失时的处理策略。
    pub arity: PathArity,
}

impl PathBinding {
    /// 必填路径键。
    pub const fn required(key: &'static str) -> Self {
        Self {
            key,
            arity: PathArity::Required,
        }
    }
    /// 可选路径键，缺失时回落到工作区根。
    pub const fn default_root(key: &'static str) -> Self {
        Self {
            key,
            arity: PathArity::DefaultRoot,
        }
    }
    /// 可选路径键，缺失时不注入。
    pub const fn optional(key: &'static str) -> Self {
        Self {
            key,
            arity: PathArity::Optional,
        }
    }
}

/// 默认绑定：单个必填 `path`（与历史行为一致）。
const DEFAULT_PATH_BINDINGS: &[PathBinding] = &[PathBinding::required("path")];

/// Host-installed resolver mapping a run id to its current origin request key.
pub type RunOriginResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Host-owned identity and access data for one tool invocation.
///
/// This value is constructed only after the gateway has bound the model call to a task/run and
/// chosen the effective access mode. Tools must use this context instead of accepting ownership
/// fields from model-controlled JSON input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionContext {
    pub task_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    pub access_mode: ProjectAccessMode,
    /// Origin request key bound by the trusted host for the current run
    /// (docs/plan-mode-dual-track-gate.md §10). Host-owned identity data: tools must
    /// use this field for request-scoped dedup instead of accepting keys from
    /// model-controlled JSON input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_request_key: Option<String>,
}

/// Optional host policy evaluated before permission prompts and before any built-in or dynamic
/// external tool executes. The gateway owns the audit record; the host owns product state.
pub trait ToolPolicyGuard: Send + Sync {
    fn check(
        &self,
        context: &ToolExecutionContext,
        tool_name: &str,
        risk_level: RiskLevel,
    ) -> Result<(), ProductError>;
}

/// A typed control directive emitted by a successful tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolExecutionDirective {
    /// Stop the current agent run and wait for explicit user input.
    SuspendForUser,
    /// Keep the current agent run alive until a later tool result releases the gate.
    RequireAgentContinuation,
    /// Release a previously required continuation so the agent may finish normally.
    AllowAgentCompletion,
}

/// Stable metadata envelope used at the gateway/runtime boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutcomeMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive: Option<ToolExecutionDirective>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Result returned by a context-aware tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    pub content: String,
    pub directive: Option<ToolExecutionDirective>,
    pub metadata: Option<serde_json::Value>,
}

impl ToolExecutionResult {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            directive: None,
            metadata: None,
        }
    }

    pub fn suspend_for_user(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            directive: Some(ToolExecutionDirective::SuspendForUser),
            metadata: None,
        }
    }

    pub fn require_agent_continuation(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            directive: Some(ToolExecutionDirective::RequireAgentContinuation),
            metadata: None,
        }
    }

    pub fn allow_agent_completion(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            directive: Some(ToolExecutionDirective::AllowAgentCompletion),
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    fn into_outcome(self) -> ToolCallOutcome {
        let metadata = if self.directive.is_some() || self.metadata.is_some() {
            serde_json::to_value(ToolOutcomeMetadata {
                directive: self.directive,
                data: self.metadata,
            })
            .ok()
        } else {
            None
        };
        ToolCallOutcome {
            content: self.content,
            is_error: false,
            metadata,
        }
    }
}

impl From<String> for ToolExecutionResult {
    fn from(content: String) -> Self {
        Self::success(content)
    }
}

/// Decode a gateway-owned control directive from a tool outcome.
///
/// Unknown or malformed metadata is deliberately ignored so third-party/legacy outcome metadata
/// remains backward compatible.
pub fn tool_outcome_directive(outcome: &ToolCallOutcome) -> Option<ToolExecutionDirective> {
    outcome
        .metadata
        .clone()
        .and_then(|value| serde_json::from_value::<ToolOutcomeMetadata>(value).ok())
        .and_then(|metadata| metadata.directive)
}

/// 工具 trait -- 每个内置工具实现此接口。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 工具名称（模型可见）。
    fn name(&self) -> &str;
    /// 工具描述（注入模型提示）。
    fn description(&self) -> &str;
    /// 默认（静态）风险等级。用于 `ToolSpec.requires_confirmation` 的 UI 提示，
    /// 也是 `risk_for` 的兜底值。
    fn risk_level(&self) -> RiskLevel;
    /// 按具体输入动态定级。
    ///
    /// 命令类工具必须覆写此方法：`cargo test` 与 `sudo rm -rf /` 不该同级。
    /// 默认回落到静态 [`Tool::risk_level`]，因此既有工具无需改动。
    fn risk_for(&self, _input: &serde_json::Value) -> RiskLevel {
        self.risk_level()
    }
    /// Whether this call must pass through an explicit user-approval policy even when the project
    /// otherwise grants full access.
    ///
    /// The gateway uses `RequestApproval` only for the permission-engine decision. The trusted
    /// execution context retains the project's original access mode, and an existing standing
    /// Allow/AllowAlways rule remains authoritative.
    fn requires_explicit_approval(&self, _input: &serde_json::Value) -> bool {
        false
    }
    /// 声明需要经 `PathGuard` 重绑定的输入键。默认单个必填 `path`。
    fn path_bindings(&self) -> &'static [PathBinding] {
        DEFAULT_PATH_BINDINGS
    }
    /// 工具是否要求路径已存在。默认 `true`（只读工具）。
    ///
    /// `create_file` 等写入工具覆写为 `false`：目标文件尚未创建，需通过
    /// `PathGuard::resolve_path`（而非 `resolve_existing_path`）解析。
    fn requires_existing_path(&self) -> bool {
        true
    }
    /// Whether this tool needs an attached workspace and PathGuard scope.
    ///
    /// Existing file and command tools stay fail-closed by default. Host-owned tools that operate
    /// only on AppData/SQLite may opt out and remain available in workspace-free conversations.
    fn requires_workspace_scope(&self) -> bool {
        true
    }
    /// Whether the host may replay this tool after a clearly transient storage-contention error.
    ///
    /// This is deliberately opt-in. Command and filesystem tools can have effects before they
    /// return an error, so retrying them generically could execute an operation twice. Host-owned
    /// transactional tools may enable this when replay is safe.
    fn allows_transient_retry(&self) -> bool {
        false
    }
    /// JSON Schema 输入定义。
    fn input_schema(&self) -> serde_json::Value;
    /// 执行工具，返回输出文本。
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError>;
    /// Execute with trusted host context.
    ///
    /// The default adapter preserves every existing Tool implementation. Context-aware host tools
    /// override this method and derive task/run ownership exclusively from `context`.
    async fn execute_with_context(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        self.execute(input).await.map(ToolExecutionResult::from)
    }

    /// Execute while observing the run cancellation flag.
    ///
    /// The default adapter makes every in-process built-in cancellation-safe by dropping its
    /// execution future once the run is aborted. Process-owning tools must override this hook and
    /// finish their resource cleanup before returning; `BashTool` uses it to kill and reap the
    /// complete process tree.
    async fn execute_with_context_and_abort(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<ToolExecutionResult, ProductError> {
        let execution = self.execute_with_context(input, context);
        tokio::pin!(execution);
        loop {
            if abort_flag.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
                return Err(cancelled_tool_error(self.name()));
            }
            if abort_flag.is_none() {
                return execution.await;
            }
            tokio::select! {
                result = &mut execution => return result,
                _ = tokio::time::sleep(TOOL_ABORT_POLL_INTERVAL) => {}
            }
        }
    }

    /// Execute with the optional capability-scoped workspace handle owned by the host.
    ///
    /// The default keeps existing tools unchanged. Filesystem tools override this hook and use
    /// `workspace_guard` for the actual I/O, rather than trusting a model-provided path that was
    /// merely validated earlier in the call chain.
    async fn execute_with_context_and_abort_with_workspace(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
        _workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        self.execute_with_context_and_abort(input, context, abort_flag)
            .await
    }
}

const MAX_TRANSIENT_TOOL_ATTEMPTS: usize = 3;
const TOOL_ABORT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
const APPROVAL_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
const APPROVAL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);
#[cfg(not(test))]
const EXTERNAL_ABORT_CLEANUP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(test)]
const EXTERNAL_ABORT_CLEANUP_GRACE: std::time::Duration = std::time::Duration::from_millis(100);

fn cancelled_tool_error(tool_name: &str) -> ProductError {
    ProductError::Other(format!(
        "tool {tool_name} cancelled before execution completed"
    ))
}

/// Owns a pending approval for exactly as long as the gateway future is alive.
///
/// Normal allow/deny/timeout paths remove the request before this guard is dropped. If the
/// surrounding task is force-aborted instead, `Drop` schedules a fail-closed cancellation so an
/// orphaned approval cannot be granted later and create a standing rule.
struct PendingPermissionLease {
    engine: Arc<PermissionEngine>,
    request_id: String,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl PendingPermissionLease {
    fn new(
        engine: Arc<PermissionEngine>,
        request_id: String,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            engine,
            request_id,
            cancelled,
        }
    }
}

impl Drop for PendingPermissionLease {
    fn drop(&mut self) {
        // Invalidate synchronously before scheduling async map cleanup. A concurrent late
        // `AllowAlways` therefore fails even if it reaches the engine before the cleanup task.
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        let engine = self.engine.clone();
        let request_id = self.request_id.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                engine.cancel_request(&request_id).await;
            });
        }
    }
}

async fn execute_external_abortable<Fut>(
    tool_name: &str,
    execution: Fut,
    abort_flag: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ToolCallOutcome, ProductError>
where
    Fut: Future<Output = Result<ToolCallOutcome, ProductError>>,
{
    let execution = execution;
    tokio::pin!(execution);
    loop {
        if abort_flag.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            // MCP/Bash-style external adapters may need an async protocol/resource cleanup step.
            // Give the cooperative future a small bounded window, then force-drop it so an
            // uncooperative third-party implementation cannot outlive the cancelled run.
            let _ = tokio::time::timeout(EXTERNAL_ABORT_CLEANUP_GRACE, &mut execution).await;
            return Err(cancelled_tool_error(tool_name));
        }
        if abort_flag.is_none() {
            return execution.await;
        }
        tokio::select! {
            result = &mut execution => return result,
            _ = tokio::time::sleep(TOOL_ABORT_POLL_INTERVAL) => {}
        }
    }
}

fn is_transient_storage_contention(error: &ProductError) -> bool {
    let ProductError::DatabaseError(message) = error else {
        return false;
    };
    let message = message.to_ascii_lowercase();
    message.contains("database is locked")
        || message.contains("database table is locked")
        || message.contains("database schema is locked")
        || message.contains("database is busy")
        || message.contains("database table is busy")
        || message.contains("database schema is busy")
}

fn transient_retry_delay(attempt: usize) -> std::time::Duration {
    #[cfg(test)]
    {
        let _ = attempt;
        std::time::Duration::ZERO
    }
    #[cfg(not(test))]
    {
        // The database connection already has a busy timeout. This short bounded backoff gives a
        // competing writer time to finish without adding another long fixed wait.
        std::time::Duration::from_millis(if attempt == 1 { 150 } else { 450 })
    }
}

/// 手写版 `futures::FutureExt::catch_unwind`（进程内工具 panic 隔离，harness-migration 1.1）。
///
/// 为什么不用 `catch_unwind(AssertUnwindSafe(|| block_on(tool.call(..))))` 式同步包裹：
/// 进程内工具由 tokio 驱动、在 async 上下文里 `block_on` 会与运行时 reactor 冲突
/// （定时器 / IO 直接 panic 或死锁），所以只能在 poll 边界捕 unwind。`futures` crate
/// 不在本 crate 依赖清单内（本次改动不动 Cargo.toml），故按其相同模式本地实现。
/// 限定 `F: Unpin` 是因为 `Tool` 的 async_trait 方法返回 `Pin<Box<dyn Future>>`
/// （天然 Unpin），从而避免手写 unsafe 固定投影。
struct CatchUnwindFuture<F> {
    inner: F,
}

impl<F: Future + Unpin> Future for CatchUnwindFuture<F> {
    type Output = Result<F::Output, Box<dyn std::any::Any + Send>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // F: Unpin 时结构体自动 Unpin，get_mut 是安全重借用，无需 unsafe。
        let this = self.get_mut();
        // AssertUnwindSafe：future 内部状态未必满足 unwind 安全；捕获后本次调用
        // 立即终止且状态不再复用，接受该取舍（与 futures crate 同一模式）。
        match catch_unwind(AssertUnwindSafe(|| {
            std::pin::Pin::new(&mut this.inner).poll(cx)
        })) {
            Ok(std::task::Poll::Ready(output)) => std::task::Poll::Ready(Ok(output)),
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Err(panic) => std::task::Poll::Ready(Err(panic)),
        }
    }
}

/// 提取 panic payload 的可读文案；非 String/&str payload 降级为固定文案。
///
/// `std::panic::panic_message` 尚未 stable，这里按参考实现
/// （`.reference/rust-deepseek-harness/src/tool.rs` 的 `panic_message`）手工 downcast。
fn panic_payload_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

async fn execute_registered_tool(
    tool: &dyn Tool,
    input: serde_json::Value,
    context: &ToolExecutionContext,
    abort_flag: Option<&std::sync::atomic::AtomicBool>,
    workspace_guard: Option<&PathGuard>,
) -> Result<ToolExecutionResult, ProductError> {
    let max_attempts = if tool.allows_transient_retry() {
        MAX_TRANSIENT_TOOL_ATTEMPTS
    } else {
        1
    };

    for attempt in 1..=max_attempts {
        if abort_flag.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err(ProductError::Other(format!(
                "tool {} cancelled before execution completed",
                tool.name()
            )));
        }

        // 只隔离进程内工具：bash（kill_on_drop）与 MCP（abort 轮询）走
        // `execute_external_abortable`，已有自己的隔离，不在此重复包裹。
        // panic 经 Err 通道上抛，由调用方记账为 Error 结果（运行时侧会折成
        // is_error 的 ToolCallOutcome 返回模型），绝不逃逸成会话死亡。
        let execution = CatchUnwindFuture {
            inner: tool.execute_with_context_and_abort_with_workspace(
                input.clone(),
                context,
                abort_flag,
                workspace_guard,
            ),
        };
        match execution.await {
            Ok(Ok(result)) => return Ok(result),
            Ok(Err(error)) if is_transient_storage_contention(&error) && attempt < max_attempts => {
                tracing::warn!(
                    task_id = %context.task_id,
                    run_id = %context.run_id,
                    tool_call_id = %context.tool_call_id,
                    tool = tool.name(),
                    attempt,
                    max_attempts,
                    error = %error,
                    "transient tool execution failure; retrying"
                );
                tokio::time::sleep(transient_retry_delay(attempt)).await;
            }
            Ok(Err(error)) if is_transient_storage_contention(&error) && max_attempts > 1 => {
                tracing::error!(
                    task_id = %context.task_id,
                    run_id = %context.run_id,
                    tool_call_id = %context.tool_call_id,
                    tool = tool.name(),
                    attempts = attempt,
                    error = %error,
                    "transient tool execution failure exhausted automatic retries"
                );
                return Err(error);
            }
            Ok(Err(error)) => return Err(error),
            Err(panic) => {
                // panic 不是瞬态存储竞争，不重试：重放一个刚 panic 过的进程内
                // 工具大概率原地再炸，只会放大故障。
                let message = panic_payload_message(panic.as_ref());
                tracing::error!(
                    task_id = %context.task_id,
                    run_id = %context.run_id,
                    tool_call_id = %context.tool_call_id,
                    tool = tool.name(),
                    panic = %message,
                    "in-process tool panicked; containing as a tool error"
                );
                return Err(ProductError::Other(format!(
                    "internal error: tool panicked: {message}"
                )));
            }
        }
    }

    unreachable!("tool retry loop always returns on its final attempt")
}

/// 子代理只能经由 Gateway 使用这组可证明无副作用的工作区工具。
///
/// 运行时与 Gateway 都复用此规则，避免未来新增调用路径时绕过只读边界。
///
/// `glob` / `search` 只读遍历，可安全授予；`edit` / `bash` 有副作用，永不授予。
pub fn subagent_read_only_tool_allowed(name: &str) -> bool {
    // load_skill 是 Plan 原生 resident 目录（8 工具）成员且只读：目录与执行
    // 边界必须一致（docs §13.2 工具不在目录里不是安全边界）。
    matches!(
        name,
        "read_file" | "list_files" | "search" | "glob" | "git_status" | "load_skill"
    )
}

fn is_subagent_caller(caller: Option<&str>) -> bool {
    caller.is_some_and(|value| value.starts_with("subagent:"))
}

/// 单次注册在同名栈上的条目（harness-migration 1.2）。
///
/// `id` 标识「哪一次注册」：guard drop 时按 id retain 精确弹出本次注册，
/// 同名栈底的其他版本不受影响。
struct ToolEntry {
    id: u64,
    tool: Arc<dyn Tool>,
}

/// 可逆效果的撤销守卫：drop 时执行撤销闭包（弹出对应的栈式注册）。
///
/// 为什么是 `Option<Box<dyn FnOnce() + Send>>` 而不是携带引用：guard 的存活
/// 可能超出对 `ToolGateway` 的 `&mut` 借用（跨 await、跨任务传递），撤销逻辑
/// 只能以 `'static` 闭包捕获注册表的 Arc 快照；`Drop` 里 `take` 出闭包按值
/// 调用，保证至多撤销一次。
pub struct EffectGuard {
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl EffectGuard {
    fn new(on_drop: impl FnOnce() + Send + 'static) -> Self {
        Self {
            on_drop: Some(Box::new(on_drop)),
        }
    }
}

impl Drop for EffectGuard {
    fn drop(&mut self) {
        if let Some(on_drop) = self.on_drop.take() {
            on_drop();
        }
    }
}

/// Tool Gateway -- 管理工具注册、权限检查与审计账本。
///
/// 实现 `agent_contract::ToolHost`，可注册到 Agent 循环。
pub struct ToolGateway {
    /// 栈式注册表（harness-migration 1.2）：同名工具的多次注册按栈共存，查找取栈顶。
    ///
    /// 为什么放 `Arc<std::sync::RwLock>` 后面：`register_guarded` 的撤销闭包要在
    /// guard drop 时回到注册表，而那时已没有 `&mut self`；用 std 而非 tokio 锁是
    /// 因为 `Drop` 闭包是同步的，不能 `.await`。锁只在短临界区内持有，绝不跨
    /// `.await`（执行路径先克隆 Arc 取出工具再进入异步阶段，见 `lookup_tool`）。
    tools: Arc<std::sync::RwLock<HashMap<String, Vec<ToolEntry>>>>,
    /// 注册 id 发号器：栈式注册按 id 定位撤销目标，避免同名工具被误删。
    next_registration_id: std::sync::atomic::AtomicU64,
    permission_engine: Arc<PermissionEngine>,
    /// 审计账本 -- 所有工具调用记录（含被拒绝 / 待审批）。
    ledger: Arc<RwLock<Vec<ToolCall>>>,
    policy_guard: Option<Arc<dyn ToolPolicyGuard>>,
    /// Host-installed resolver mapping a run id to its current origin request key.
    /// Populated only by the trusted desktop host; absent resolver keeps the field
    /// `None` and changes no behavior.
    run_origin_resolver: Option<RunOriginResolver>,
}

impl ToolGateway {
    /// 创建新的 Tool Gateway。
    pub fn new(permission_engine: Arc<PermissionEngine>) -> Self {
        Self {
            tools: Arc::new(std::sync::RwLock::new(HashMap::new())),
            next_registration_id: std::sync::atomic::AtomicU64::new(0),
            permission_engine,
            ledger: Arc::new(RwLock::new(Vec::new())),
            policy_guard: None,
            run_origin_resolver: None,
        }
    }

    pub fn set_policy_guard(&mut self, guard: Arc<dyn ToolPolicyGuard>) {
        self.policy_guard = Some(guard);
    }

    /// Install the host-owned run -> origin request key resolver used to fill
    /// [`ToolExecutionContext::origin_request_key`].
    pub fn set_run_origin_resolver(&mut self, resolver: RunOriginResolver) {
        self.run_origin_resolver = Some(resolver);
    }

    fn resolve_origin_request_key(&self, run_id: &str) -> Option<String> {
        self.run_origin_resolver
            .as_ref()
            .and_then(|resolve| resolve(run_id))
    }

    /// 注册一个工具。
    ///
    /// 不可逆语义与改造前的 `HashMap::insert` 完全一致：同名后注册整体替换
    /// 先注册（清空同名栈，不留可恢复版本）。需要可逆注册用
    /// [`ToolGateway::register_guarded`]。
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.write().expect("tool registry poisoned").insert(
            name,
            vec![ToolEntry {
                id: 0,
                tool: Arc::from(tool),
            }],
        );
    }

    /// 栈式可逆注册：同名后注册覆盖先注册（查找取栈顶），返回的
    /// [`EffectGuard`] drop 时按注册 id 弹出本次注册、恢复先前版本。
    ///
    /// 用于临时挂载的工具（会话级替换、测试 mock 等）；永久安装走 `register`。
    pub fn register_guarded(&mut self, tool: Arc<dyn Tool>) -> EffectGuard {
        let id = self
            .next_registration_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let name = tool.name().to_string();
        self.tools
            .write()
            .expect("tool registry poisoned")
            .entry(name.clone())
            .or_default()
            .push(ToolEntry { id, tool });
        let tools = Arc::clone(&self.tools);
        EffectGuard::new(move || {
            let mut tools = tools.write().expect("tool registry poisoned");
            // 按 id retain 只弹本次注册；栈因此变空则连键一起删，
            // 保持 owns_tool / tool_specs 看不到空栈。
            let remove_key = match tools.get_mut(&name) {
                Some(stack) => {
                    stack.retain(|entry| entry.id != id);
                    stack.is_empty()
                }
                None => false,
            };
            if remove_key {
                tools.remove(&name);
            }
        })
    }

    /// 查找工具：取同名栈的栈顶，克隆 Arc 后立即放锁。
    ///
    /// 同步锁不能跨 `.await` 持有（guard 非 Send 且会阻塞 runtime 线程），
    /// 因此执行路径先取出工具克隆再进入异步阶段；进行中的调用持有的是
    /// 稳定的 Arc，中途的注册 / 撤销不影响本次执行。
    fn lookup_tool(&self, tool_name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .read()
            .expect("tool registry poisoned")
            .get(tool_name)
            .and_then(|stack| stack.last())
            .map(|entry| Arc::clone(&entry.tool))
    }

    /// Whether this name is owned by a registered host tool.
    ///
    /// Runtime tool multiplexers use this to keep external/MCP descriptors from shadowing a
    /// trusted built-in with the same model-facing name.
    pub fn owns_tool(&self, tool_name: &str) -> bool {
        self.tools
            .read()
            .expect("tool registry poisoned")
            .contains_key(tool_name)
    }

    /// 查询某工具声明的路径绑定；工具未注册时返回 `None`。
    ///
    /// 供 Agent 运行时在调用前把路径参数重绑定到会话工作区。
    pub fn path_bindings(&self, tool_name: &str) -> Option<&'static [PathBinding]> {
        self.tools
            .read()
            .expect("tool registry poisoned")
            .get(tool_name)
            .and_then(|stack| stack.last())
            // PathBinding 切片由工具以 'static 返回（不借用注册表），放锁后仍有效。
            .map(|entry| entry.tool.path_bindings())
    }

    /// 查询某工具是否要求路径已存在。未注册工具默认 `true`（fail-closed）。
    pub fn requires_existing_path(&self, tool_name: &str) -> bool {
        self.tools
            .read()
            .expect("tool registry poisoned")
            .get(tool_name)
            .and_then(|stack| stack.last())
            .map(|entry| entry.tool.requires_existing_path())
            .unwrap_or(true)
    }

    /// Query whether a registered tool requires an attached workspace scope.
    /// Unknown tools default to `true` so callers cannot accidentally expose them globally.
    pub fn requires_workspace_scope(&self, tool_name: &str) -> bool {
        self.tools
            .read()
            .expect("tool registry poisoned")
            .get(tool_name)
            .and_then(|stack| stack.last())
            .map(|entry| entry.tool.requires_workspace_scope())
            .unwrap_or(true)
    }

    /// 列出所有已注册工具的规格。
    ///
    /// 输出按工具名稳定排序（P1-C，PRD §3 A4）：`HashMap` 的迭代顺序进程内稳定
    /// 但应用重启后随机，排序保证 tools 数组字节跨重启一致，避免重启后首个请求
    /// 的 tools 前缀 miss DeepSeek 前缀缓存。
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = self
            .tools
            .read()
            .expect("tool registry poisoned")
            .values()
            // 栈式注册只暴露栈顶：guarded 覆盖期间，被覆盖版本对模型不可见。
            .filter_map(|stack| stack.last())
            .map(|entry| {
                let tool = &entry.tool;
                ToolSpec {
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    input_schema: tool.input_schema(),
                    source: ToolSource::Builtin,
                    requires_confirmation: tool.risk_level().requires_confirmation(),
                }
            })
            .collect::<Vec<_>>();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// 执行工具调用（含权限检查与审计记账）。
    ///
    /// 流程 [doc-02 §1]：
    /// 1. 查找工具（未找到 -> 错误）
    /// 2. 获取风险等级
    /// 3. 权限检查（附带运行与调用者归属）
    /// 4. 若 `NeedsApproval` -> 记账（Denied）并返回权限错误
    /// 5. 若 `Denied` -> 记账（Denied）并返回权限错误
    /// 6. 若 `Allowed` -> 执行工具
    /// 7. 记账（Ok / Error）并返回结果
    pub async fn execute_call(
        &self,
        task_id: &str,
        run_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        caller: Option<&str>,
    ) -> Result<ToolCallOutcome, ProductError> {
        self.execute_call_with_access_mode(
            task_id,
            run_id,
            tool_name,
            input,
            caller,
            ProjectAccessMode::RiskBased,
        )
        .await
    }

    /// 以项目权限模式执行工具调用（含权限检查与审计记账）。
    pub async fn execute_call_with_access_mode(
        &self,
        task_id: &str,
        run_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        caller: Option<&str>,
        access_mode: ProjectAccessMode,
    ) -> Result<ToolCallOutcome, ProductError> {
        self.execute_call_with_access_mode_and_workspace_guard(
            task_id,
            run_id,
            tool_name,
            input,
            caller,
            access_mode,
            None,
        )
        .await
    }

    /// Execute a call with the effective permission mode and an optional workspace capability.
    ///
    /// Only the trusted host supplies `workspace_guard`; tool inputs remain model-controlled and
    /// are never themselves a filesystem authority.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_call_with_access_mode_and_workspace_guard(
        &self,
        task_id: &str,
        run_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        caller: Option<&str>,
        access_mode: ProjectAccessMode,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolCallOutcome, ProductError> {
        // 1. 查找工具
        let tool = self
            .lookup_tool(tool_name)
            .ok_or_else(|| ProductError::PermissionError(format!("tool not found: {tool_name}")))?;

        // 2. 获取风险等级（按本次输入动态定级；非命令类工具回落到静态等级）
        let risk_level = tool.risk_for(&input);
        if is_subagent_caller(caller)
            && (!tool.requires_workspace_scope()
                || (access_mode != ProjectAccessMode::FullAccess
                    && !subagent_read_only_tool_allowed(tool_name)))
        {
            let reason = format!("subagent caller may not execute tool: {tool_name}");
            let mut audit =
                ToolCall::new(run_id, task_id, tool_name, input.to_string(), risk_level);
            audit.caller = caller.map(ToOwned::to_owned);
            audit.deny(&reason);
            self.ledger.write().await.push(audit);
            return Err(ProductError::PermissionError(reason));
        }

        // 3. 从 input 中提取 target（终端工具用）
        let target = input.get("target").and_then(|v| v.as_str());

        // 4. 先创建审计记录，使待审批请求可关联稳定的 tool_call_id。
        let mut audit = ToolCall::new(run_id, task_id, tool_name, input.to_string(), risk_level);
        audit.caller = caller.map(|s| s.to_string());
        let context = ToolExecutionContext {
            task_id: task_id.to_string(),
            run_id: run_id.to_string(),
            tool_call_id: audit.id.clone(),
            caller: caller.map(ToOwned::to_owned),
            access_mode,
            origin_request_key: self.resolve_origin_request_key(run_id),
        };
        if let Some(guard) = &self.policy_guard {
            if let Err(error) = guard.check(&context, tool_name, risk_level) {
                audit.deny(error.to_string());
                self.ledger.write().await.push(audit);
                return Err(error);
            }
        }

        // 5. 权限检查
        let permission_access_mode = if tool.requires_explicit_approval(&input) {
            ProjectAccessMode::RequestApproval
        } else {
            access_mode
        };
        let check_result = self
            .permission_engine
            .check_detailed_with_access_mode(
                task_id,
                &audit.id,
                Some(run_id),
                caller,
                tool_name,
                risk_level,
                &input.to_string(),
                target,
                permission_access_mode,
            )
            .await;

        // 6. 根据检查结果处理
        match check_result {
            PermissionCheckResult::Allowed => {
                let outcome =
                    execute_registered_tool(tool.as_ref(), input, &context, None, workspace_guard)
                        .await;
                match outcome {
                    Ok(result) => {
                        audit.succeed(&result.content);
                        self.ledger.write().await.push(audit);
                        Ok(result.into_outcome())
                    }
                    Err(err) => {
                        audit.fail(err.to_string());
                        self.ledger.write().await.push(audit);
                        Err(err)
                    }
                }
            }
            PermissionCheckResult::Denied(reason) => {
                audit.deny(&reason);
                self.ledger.write().await.push(audit);
                Err(ProductError::PermissionError(reason))
            }
            PermissionCheckResult::NeedsApproval(req) => {
                let msg = format!(
                    "tool {tool_name} requires user approval (request {})",
                    req.id
                );
                audit.deny(&msg);
                self.ledger.write().await.push(audit);
                Err(ProductError::PermissionError(msg))
            }
        }
    }

    /// 执行工具调用；`NeedsApproval` 时挂起等待用户批复（而非立即失败）。
    ///
    /// 与 `execute_call` 的差异：
    /// - 权限请求用 `check_detailed` 创建（带真实 tool_call_id 与 input_summary）
    /// - 审批中挂起等待（最长 10 分钟），`abort_flag` 置位时提前返回取消错误
    /// - 批复 Allow / AllowAlways 后执行；Deny / 超时返回权限错误
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_with_wait(
        &self,
        task_id: &str,
        run_id: &str,
        call_id: Option<&str>,
        tool_name: &str,
        input: serde_json::Value,
        caller: Option<&str>,
        input_summary: &str,
        abort_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<ToolCallOutcome, ProductError> {
        self.execute_with_wait_with_access_mode(
            task_id,
            run_id,
            call_id,
            tool_name,
            input,
            caller,
            input_summary,
            abort_flag,
            ProjectAccessMode::RiskBased,
        )
        .await
    }

    /// 使用项目权限模式执行工具调用；待批时挂起等待用户决策。
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_with_wait_with_access_mode(
        &self,
        task_id: &str,
        run_id: &str,
        call_id: Option<&str>,
        tool_name: &str,
        input: serde_json::Value,
        caller: Option<&str>,
        input_summary: &str,
        abort_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        access_mode: ProjectAccessMode,
    ) -> Result<ToolCallOutcome, ProductError> {
        self.execute_with_wait_with_access_mode_and_workspace_guard(
            task_id,
            run_id,
            call_id,
            tool_name,
            input,
            caller,
            input_summary,
            abort_flag,
            access_mode,
            None,
        )
        .await
    }

    /// Execute a call after approval, with an optional capability-scoped workspace handle.
    ///
    /// The separate method preserves the public legacy API for host integrations that have no
    /// workspace, while scoped Agent runs can give built-in file tools a stable directory handle.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_with_wait_with_access_mode_and_workspace_guard(
        &self,
        task_id: &str,
        run_id: &str,
        call_id: Option<&str>,
        tool_name: &str,
        input: serde_json::Value,
        caller: Option<&str>,
        input_summary: &str,
        abort_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        access_mode: ProjectAccessMode,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolCallOutcome, ProductError> {
        let tool = self
            .lookup_tool(tool_name)
            .ok_or_else(|| ProductError::PermissionError(format!("tool not found: {tool_name}")))?;

        let risk_level = tool.risk_for(&input);
        let target = input.get("target").and_then(|v| v.as_str());

        let mut audit = ToolCall::new(run_id, task_id, tool_name, input.to_string(), risk_level);
        if let Some(call_id) = call_id {
            audit.id = call_id.to_string();
        }
        audit.caller = caller.map(|s| s.to_string());
        // F3：RequestApproval 子代理（inherit 自非 FullAccess 父运行）的工具调用
        // 必须进入权限引擎审批，而不是在这里被硬拒绝——否则 bash/edit 从"不可见"
        // 变成"可见但永远被拒"，审批能力无法打通。ReadOnly 子代理仍被硬拒绝
        // （它们看不到写工具；scoped_input 的 tool_allowed 已先行拦截）。
        if is_subagent_caller(caller)
            && (!tool.requires_workspace_scope()
                || (access_mode != ProjectAccessMode::FullAccess
                    && access_mode != ProjectAccessMode::RequestApproval
                    && !subagent_read_only_tool_allowed(tool_name)))
        {
            let reason = format!("subagent caller may not execute tool: {tool_name}");
            audit.deny(&reason);
            self.ledger.write().await.push(audit);
            return Err(ProductError::PermissionError(reason));
        }

        let context = ToolExecutionContext {
            task_id: task_id.to_string(),
            run_id: run_id.to_string(),
            tool_call_id: audit.id.clone(),
            caller: caller.map(ToOwned::to_owned),
            access_mode,
            origin_request_key: self.resolve_origin_request_key(run_id),
        };
        if let Some(guard) = &self.policy_guard {
            if let Err(error) = guard.check(&context, tool_name, risk_level) {
                audit.deny(error.to_string());
                self.ledger.write().await.push(audit);
                return Err(error);
            }
        }

        let lease_cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lifecycle_cancelled = lease_cancelled.clone();
        let run_abort = abort_flag.clone();
        let cancellation = PermissionCancellation::from_probe(move || {
            lifecycle_cancelled.load(std::sync::atomic::Ordering::Acquire)
                || run_abort
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        });
        let permission_access_mode = if tool.requires_explicit_approval(&input) {
            ProjectAccessMode::RequestApproval
        } else {
            access_mode
        };
        let check_result = self
            .permission_engine
            .check_detailed_with_access_mode_and_lifecycle(
                task_id,
                &audit.id,
                Some(run_id),
                caller,
                tool_name,
                risk_level,
                input_summary,
                target,
                permission_access_mode,
                Some(cancellation),
                Some(APPROVAL_WAIT_TIMEOUT),
            )
            .await;

        let approved = match check_result {
            PermissionCheckResult::Allowed => true,
            PermissionCheckResult::Denied(reason) => {
                audit.deny(&reason);
                self.ledger.write().await.push(audit);
                return Err(ProductError::PermissionError(reason));
            }
            PermissionCheckResult::NeedsApproval(req) => {
                // 挂起等待批复；abort 时提前返回
                let _pending_lease = PendingPermissionLease::new(
                    self.permission_engine.clone(),
                    req.id.clone(),
                    lease_cancelled,
                );
                let start = std::time::Instant::now();
                loop {
                    if abort_flag
                        .as_ref()
                        .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
                    {
                        self.permission_engine.cancel_request(&req.id).await;
                        let msg = format!("tool {tool_name} cancelled while awaiting approval");
                        audit.fail(&msg);
                        self.ledger.write().await.push(audit);
                        return Err(ProductError::PermissionError(msg));
                    }
                    if start.elapsed() >= APPROVAL_WAIT_TIMEOUT {
                        self.permission_engine.cancel_request(&req.id).await;
                        let msg = format!("tool {tool_name} approval timed out");
                        audit.fail(&msg);
                        self.ledger.write().await.push(audit);
                        return Err(ProductError::PermissionError(msg));
                    }
                    if let Some(decision) = self.permission_engine.try_decision(&req.id).await {
                        match decision {
                            PermissionDecision::Allow | PermissionDecision::AllowAlways => {
                                break true;
                            }
                            PermissionDecision::Deny => {
                                let msg = format!("tool {tool_name} denied by user");
                                audit.deny(&msg);
                                self.ledger.write().await.push(audit);
                                return Err(ProductError::PermissionError(msg));
                            }
                            PermissionDecision::Pending => {}
                        }
                    }
                    tokio::time::sleep(APPROVAL_POLL_INTERVAL).await;
                }
            }
        };
        debug_assert!(approved);

        // 已获许可：执行并记账。所有权信息来自 gateway，而不是模型输入。
        match execute_registered_tool(
            tool.as_ref(),
            input,
            &context,
            abort_flag.as_deref(),
            workspace_guard,
        )
        .await
        {
            Ok(result) => {
                audit.succeed(&result.content);
                self.ledger.write().await.push(audit);
                Ok(result.into_outcome())
            }
            Err(err) => {
                audit.fail(err.to_string());
                self.ledger.write().await.push(audit);
                Err(err)
            }
        }
    }

    /// Authorize, execute and audit a dynamic external tool without registering one schema per
    /// remote MCP tool. The caller supplies the already-classified risk and an execution closure;
    /// the permission and ledger behavior is otherwise identical to built-in tools.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_external_with_wait<F, Fut>(
        &self,
        task_id: &str,
        run_id: &str,
        call_id: Option<&str>,
        tool_name: &str,
        input: serde_json::Value,
        caller: Option<&str>,
        input_summary: &str,
        abort_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        access_mode: ProjectAccessMode,
        risk_level: RiskLevel,
        execute: F,
    ) -> Result<ToolCallOutcome, ProductError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<ToolCallOutcome, ProductError>>,
    {
        // `mcp_call` is a multiplexed entry point. A standing rule for one server/tool pair
        // must not become a wildcard grant for every installed MCP tool.
        let permission_target = if tool_name == "mcp_call" {
            Some(
                serde_json::json!({
                    "server_id": input.get("server_id").and_then(serde_json::Value::as_str),
                    "tool": input.get("tool").and_then(serde_json::Value::as_str),
                })
                .to_string(),
            )
        } else {
            None
        };
        let mut audit = ToolCall::new(run_id, task_id, tool_name, input.to_string(), risk_level);
        if let Some(call_id) = call_id {
            audit.id = call_id.to_string();
        }
        audit.caller = caller.map(ToOwned::to_owned);

        // M5：RequestApproval 子代理（inherit 自审批类父运行）放行——后续权限引擎
        // 会对 R2+ 发起审批（与内置工具路径一致）。显式 ReadOnly 子代理的 mutating
        // external 已在 SessionToolHost 的 policy 层硬拒（external_access_mode 把
        // 两档折叠成同一 access_mode，gateway 无法区分，故此处不再重复拦截）。
        if is_subagent_caller(caller)
            && access_mode != ProjectAccessMode::FullAccess
            && access_mode != ProjectAccessMode::RequestApproval
            && !matches!(risk_level, RiskLevel::R0 | RiskLevel::R1)
        {
            let reason = format!(
                "read-only subagent may not execute state-changing external tool: {tool_name}"
            );
            audit.deny(&reason);
            self.ledger.write().await.push(audit);
            return Err(ProductError::PermissionError(reason));
        }

        let context = ToolExecutionContext {
            task_id: task_id.to_string(),
            run_id: run_id.to_string(),
            tool_call_id: audit.id.clone(),
            caller: caller.map(ToOwned::to_owned),
            access_mode,
            origin_request_key: self.resolve_origin_request_key(run_id),
        };
        if let Some(guard) = &self.policy_guard {
            if let Err(error) = guard.check(&context, tool_name, risk_level) {
                audit.deny(error.to_string());
                self.ledger.write().await.push(audit);
                return Err(error);
            }
        }

        let lease_cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lifecycle_cancelled = lease_cancelled.clone();
        let run_abort = abort_flag.clone();
        let cancellation = PermissionCancellation::from_probe(move || {
            lifecycle_cancelled.load(std::sync::atomic::Ordering::Acquire)
                || run_abort
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        });
        let check_result = self
            .permission_engine
            .check_detailed_with_access_mode_and_lifecycle(
                task_id,
                &audit.id,
                Some(run_id),
                caller,
                tool_name,
                risk_level,
                input_summary,
                permission_target.as_deref(),
                access_mode,
                Some(cancellation),
                Some(APPROVAL_WAIT_TIMEOUT),
            )
            .await;

        match check_result {
            PermissionCheckResult::Allowed => {}
            PermissionCheckResult::Denied(reason) => {
                audit.deny(&reason);
                self.ledger.write().await.push(audit);
                return Err(ProductError::PermissionError(reason));
            }
            PermissionCheckResult::NeedsApproval(request) => {
                let _pending_lease = PendingPermissionLease::new(
                    self.permission_engine.clone(),
                    request.id.clone(),
                    lease_cancelled,
                );
                let start = std::time::Instant::now();
                loop {
                    if abort_flag
                        .as_ref()
                        .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
                    {
                        self.permission_engine.cancel_request(&request.id).await;
                        let message = format!("tool {tool_name} cancelled while awaiting approval");
                        audit.fail(&message);
                        self.ledger.write().await.push(audit);
                        return Err(ProductError::PermissionError(message));
                    }
                    if start.elapsed() >= APPROVAL_WAIT_TIMEOUT {
                        self.permission_engine.cancel_request(&request.id).await;
                        let message = format!("tool {tool_name} approval timed out");
                        audit.fail(&message);
                        self.ledger.write().await.push(audit);
                        return Err(ProductError::PermissionError(message));
                    }
                    if let Some(decision) = self.permission_engine.try_decision(&request.id).await {
                        match decision {
                            PermissionDecision::Allow | PermissionDecision::AllowAlways => break,
                            PermissionDecision::Deny => {
                                let message = format!("tool {tool_name} denied by user");
                                audit.deny(&message);
                                self.ledger.write().await.push(audit);
                                return Err(ProductError::PermissionError(message));
                            }
                            PermissionDecision::Pending => {}
                        }
                    }
                    tokio::time::sleep(APPROVAL_POLL_INTERVAL).await;
                }
            }
        }

        match execute_external_abortable(tool_name, execute(), abort_flag.as_deref()).await {
            Ok(outcome) => {
                if outcome.is_error {
                    audit.fail(&outcome.content);
                } else {
                    audit.succeed(&outcome.content);
                }
                self.ledger.write().await.push(audit);
                Ok(outcome)
            }
            Err(error) => {
                audit.fail(error.to_string());
                self.ledger.write().await.push(audit);
                Err(error)
            }
        }
    }

    /// 获取审计账本（所有工具调用记录）。
    pub async fn ledger(&self) -> Vec<ToolCall> {
        self.ledger.read().await.clone()
    }

    /// 获取权限引擎引用。
    pub fn permission_engine(&self) -> &Arc<PermissionEngine> {
        &self.permission_engine
    }
}

/// 为 `ToolGateway` 实现 `agent_contract::ToolHost`。
///
/// - `list_tools`：返回所有已注册工具的 `ToolSpec`。
/// - `call`：委托给 `execute_call`（task_id / run_id 为空，表示直接调用）。
#[async_trait]
impl ToolHost for ToolGateway {
    async fn list_tools(&self) -> agent_error::Result<Vec<ToolSpec>> {
        Ok(self.tool_specs())
    }

    async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> agent_error::Result<ToolCallOutcome> {
        self.execute_call("", "", name, args, None)
            .await
            .map_err(Into::into)
    }
}

// ── 测试辅助工具 ──────────────────────────────────────────────

/// 用于测试的 R0 echo 工具。
#[cfg(test)]
struct EchoTool;

#[async_trait]
#[cfg(test)]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echo the input text"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        Ok(input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }
}

/// 用于测试的 R2 write 工具（不实际写入，仅返回成功）。
#[cfg(test)]
struct WriteTool;

#[async_trait]
#[cfg(test)]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write content to a file"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R2
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        Ok(format!("wrote to {path}"))
    }
}

/// R2 fixture whose host policy always requires an explicit approval decision, including when the
/// project itself is configured for full access.
#[cfg(test)]
struct ExplicitApprovalTool {
    seen_access_modes: Arc<std::sync::Mutex<Vec<ProjectAccessMode>>>,
}

#[async_trait]
#[cfg(test)]
impl Tool for ExplicitApprovalTool {
    fn name(&self) -> &str {
        "workspace_approval"
    }
    fn description(&self) -> &str {
        "Explicit approval fixture"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R2
    }
    fn requires_explicit_approval(&self, _input: &serde_json::Value) -> bool {
        true
    }
    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }
    fn requires_workspace_scope(&self) -> bool {
        false
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(ProductError::Other(
            "workspace_approval requires trusted execution context".to_string(),
        ))
    }
    async fn execute_with_context(
        &self,
        _input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        self.seen_access_modes
            .lock()
            .unwrap()
            .push(context.access_mode);
        Ok(ToolExecutionResult::from("approved".to_string()))
    }
}

/// 用于测试的会报错的工具。
#[cfg(test)]
struct FailTool;

#[async_trait]
#[cfg(test)]
impl Tool for FailTool {
    fn name(&self) -> &str {
        "fail"
    }
    fn description(&self) -> &str {
        "Always fails"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(ProductError::Other("intentional failure".to_string()))
    }
}

/// 用于测试的会 panic 的工具（panic 隔离 fixture，harness-migration 1.1）。
#[cfg(test)]
struct PanicTool;

#[async_trait]
#[cfg(test)]
impl Tool for PanicTool {
    fn name(&self) -> &str {
        "panic_tool"
    }
    fn description(&self) -> &str {
        "Panics on every execute"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        panic!("kaboom");
    }
}

/// 同名可替换的固定回复工具（栈式注册 fixture，harness-migration 1.2）。
#[cfg(test)]
struct NamedReplyTool {
    name: &'static str,
    reply: &'static str,
}

#[async_trait]
#[cfg(test)]
impl Tool for NamedReplyTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "Replies with a fixed string"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Ok(self.reply.to_string())
    }
}

#[cfg(test)]
struct FlakyTool {
    attempts: Arc<std::sync::atomic::AtomicUsize>,
    failures_before_success: usize,
    retry_safe: bool,
}

#[cfg(test)]
struct AbortAfterTransientFailureTool {
    attempts: Arc<std::sync::atomic::AtomicUsize>,
    abort: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
#[cfg(test)]
impl Tool for AbortAfterTransientFailureTool {
    fn name(&self) -> &str {
        "abort_after_transient_failure"
    }
    fn description(&self) -> &str {
        "Sets the run abort flag after one transient failure"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn allows_transient_retry(&self) -> bool {
        true
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.abort.store(true, std::sync::atomic::Ordering::SeqCst);
        Err(ProductError::DatabaseError(
            "database is locked".to_string(),
        ))
    }
}

#[async_trait]
#[cfg(test)]
impl Tool for FlakyTool {
    fn name(&self) -> &str {
        "flaky"
    }
    fn description(&self) -> &str {
        "Fails with transient storage contention before succeeding"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn allows_transient_retry(&self) -> bool {
        self.retry_safe
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        let attempt = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if attempt <= self.failures_before_success {
            Err(ProductError::DatabaseError(
                "database is locked".to_string(),
            ))
        } else {
            Ok("recovered".to_string())
        }
    }
}

#[cfg(test)]
struct ContextTool {
    seen: Arc<std::sync::Mutex<Option<ToolExecutionContext>>>,
}

#[cfg(test)]
struct RejectMutationsGuard;

#[cfg(test)]
impl ToolPolicyGuard for RejectMutationsGuard {
    fn check(
        &self,
        _context: &ToolExecutionContext,
        _tool_name: &str,
        risk_level: RiskLevel,
    ) -> Result<(), ProductError> {
        if matches!(risk_level, RiskLevel::R0 | RiskLevel::R1) {
            Ok(())
        } else {
            Err(ProductError::PermissionError(
                "state-changing tools are paused".to_string(),
            ))
        }
    }
}

#[async_trait]
#[cfg(test)]
impl Tool for ContextTool {
    fn name(&self) -> &str {
        "context_tool"
    }
    fn description(&self) -> &str {
        "Capture trusted execution context"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }
    fn requires_workspace_scope(&self) -> bool {
        false
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(ProductError::Other(
            "context_tool requires trusted execution".to_string(),
        ))
    }
    async fn execute_with_context(
        &self,
        _input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        *self.seen.lock().unwrap() = Some(context.clone());
        Ok(ToolExecutionResult::suspend_for_user("waiting")
            .with_metadata(serde_json::json!({"question_set_id": "set-1"})))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::{PermissionDecision, ToolCallStatus};
    use tracing::instrument::WithSubscriber;

    #[derive(Clone)]
    struct LevelSubscriber {
        levels: Arc<std::sync::Mutex<Vec<tracing::Level>>>,
    }

    impl tracing::Subscriber for LevelSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            self.levels.lock().unwrap().push(*event.metadata().level());
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    fn make_gateway() -> (Arc<PermissionEngine>, ToolGateway) {
        let engine = Arc::new(PermissionEngine::new());
        let gw = ToolGateway::new(engine.clone());
        (engine, gw)
    }

    struct DropFlag(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct PendingCancellationTool {
        started: Arc<std::sync::atomic::AtomicBool>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
        completed: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl Tool for PendingCancellationTool {
        fn name(&self) -> &str {
            "pending_cancel"
        }

        fn description(&self) -> &str {
            "pending cancellation fixture"
        }

        fn risk_level(&self) -> RiskLevel {
            RiskLevel::R0
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
            let _drop = DropFlag(self.dropped.clone());
            self.started
                .store(true, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            self.completed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("unexpected completion".to_string())
        }
    }

    #[tokio::test]
    async fn abort_drops_an_active_builtin_execution_future() {
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tool = PendingCancellationTool {
            started: started.clone(),
            dropped: dropped.clone(),
            completed: completed.clone(),
        };
        let abort = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_abort = abort.clone();
        let cancel = tokio::spawn(async move {
            while !started.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            cancel_abort.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let context = ToolExecutionContext {
            task_id: "task".to_string(),
            run_id: "run".to_string(),
            tool_call_id: "call".to_string(),
            caller: Some("agent".to_string()),
            access_mode: ProjectAccessMode::FullAccess,
            origin_request_key: None,
        };

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            execute_registered_tool(
                &tool,
                serde_json::json!({}),
                &context,
                Some(abort.as_ref()),
                None,
            ),
        )
        .await
        .expect("built-in cancellation must be prompt")
        .expect_err("cancelled built-in must not succeed");
        cancel.await.unwrap();

        assert!(error.to_string().contains("cancelled"));
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!completed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn abort_force_drops_a_non_cooperative_external_future_after_cleanup_grace() {
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let abort = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_abort = abort.clone();
        let cancel_started = started.clone();
        let cancel = tokio::spawn(async move {
            while !cancel_started.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            cancel_abort.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let future_dropped = dropped.clone();
        let future_completed = completed.clone();
        let execution = async move {
            let _drop = DropFlag(future_dropped);
            started.store(true, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            future_completed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolCallOutcome {
                content: "unexpected completion".to_string(),
                is_error: false,
                metadata: None,
            })
        };

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            execute_external_abortable("pending_external", execution, Some(abort.as_ref())),
        )
        .await
        .expect("external cancellation must be bounded")
        .expect_err("cancelled external call must not succeed");
        cancel.await.unwrap();

        assert!(error.to_string().contains("cancelled"));
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!completed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancelling_builtin_approval_wait_removes_pending_and_blocks_late_grant() {
        let (engine, mut gateway) = make_gateway();
        gateway.register(Box::new(WriteTool));
        let gateway = Arc::new(gateway);
        let abort = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let execution = {
            let gateway = gateway.clone();
            let abort = abort.clone();
            tokio::spawn(async move {
                gateway
                    .execute_with_wait_with_access_mode(
                        "task-cancel-pending",
                        "run-cancel-pending",
                        Some("call-cancel-pending"),
                        "write_file",
                        serde_json::json!({ "path": "cancelled.txt", "content": "nope" }),
                        Some("agent"),
                        "write cancelled.txt",
                        Some(abort),
                        ProjectAccessMode::RequestApproval,
                    )
                    .await
            })
        };

        let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(request) = engine
                    .pending_for_task("task-cancel-pending")
                    .await
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("approval request must become pending");

        abort.store(true, std::sync::atomic::Ordering::SeqCst);
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), execution)
            .await
            .expect("approval cancellation must be prompt")
            .unwrap()
            .expect_err("cancelled approval must not execute");

        assert!(error.to_string().contains("cancelled"));
        assert!(engine
            .pending_for_task("task-cancel-pending")
            .await
            .is_empty());
        assert!(engine
            .decide(&request.id, PermissionDecision::AllowAlways)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn cancelling_external_approval_wait_removes_pending_and_blocks_late_grant() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (engine, gateway) = make_gateway();
        let gateway = Arc::new(gateway);
        let abort = Arc::new(AtomicBool::new(false));
        let executed = Arc::new(AtomicBool::new(false));

        let execution = {
            let gateway = gateway.clone();
            let abort = abort.clone();
            let executed = executed.clone();
            tokio::spawn(async move {
                gateway
                    .execute_external_with_wait(
                        "task-cancel-external",
                        "run-cancel-external",
                        Some("call-cancel-external"),
                        "mcp_call",
                        serde_json::json!({ "server_id": "fixture", "tool": "write" }),
                        Some("agent"),
                        "fixture/write",
                        Some(abort),
                        ProjectAccessMode::RequestApproval,
                        RiskLevel::R2,
                        move || async move {
                            executed.store(true, Ordering::SeqCst);
                            Ok(ToolCallOutcome {
                                content: "unexpected execution".to_string(),
                                is_error: false,
                                metadata: None,
                            })
                        },
                    )
                    .await
            })
        };

        let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(request) = engine
                    .pending_for_task("task-cancel-external")
                    .await
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("external approval request must become pending");

        abort.store(true, Ordering::SeqCst);
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), execution)
            .await
            .expect("external approval cancellation must be prompt")
            .unwrap()
            .expect_err("cancelled external approval must not execute");

        assert!(error.to_string().contains("cancelled"));
        assert!(!executed.load(Ordering::SeqCst));
        assert!(engine
            .pending_for_task("task-cancel-external")
            .await
            .is_empty());
        assert!(engine
            .decide(&request.id, PermissionDecision::AllowAlways)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn dropping_builtin_approval_wait_cleans_pending_and_blocks_late_grant() {
        let (engine, mut gateway) = make_gateway();
        gateway.register(Box::new(WriteTool));
        let gateway = Arc::new(gateway);

        let execution = {
            let gateway = gateway.clone();
            tokio::spawn(async move {
                gateway
                    .execute_with_wait_with_access_mode(
                        "task-drop-pending",
                        "run-drop-pending",
                        Some("call-drop-pending"),
                        "write_file",
                        serde_json::json!({ "path": "dropped.txt", "content": "nope" }),
                        Some("agent"),
                        "write dropped.txt",
                        None,
                        ProjectAccessMode::RequestApproval,
                    )
                    .await
            })
        };

        let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(request) = engine
                    .pending_for_task("task-drop-pending")
                    .await
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("approval request must become pending");

        execution.abort();
        assert!(execution.await.unwrap_err().is_cancelled());
        // Drop invalidates the lifecycle synchronously, before its async pending-map cleanup.
        assert!(engine
            .decide(&request.id, PermissionDecision::AllowAlways)
            .await
            .is_err());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !engine
                .pending_for_task("task-drop-pending")
                .await
                .is_empty()
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropping the gateway future must cancel its pending approval");

        assert!(matches!(
            engine
                .check("task-drop-pending", "write_file", RiskLevel::R2, None,)
                .await,
            PermissionCheckResult::NeedsApproval(_)
        ));
    }

    #[tokio::test]
    async fn dropping_external_approval_wait_cleans_pending_and_blocks_late_grant() {
        let (engine, gateway) = make_gateway();
        let gateway = Arc::new(gateway);

        let execution = {
            let gateway = gateway.clone();
            tokio::spawn(async move {
                gateway
                    .execute_external_with_wait(
                        "task-drop-external",
                        "run-drop-external",
                        Some("call-drop-external"),
                        "mcp_call",
                        serde_json::json!({ "server_id": "fixture", "tool": "write" }),
                        Some("agent"),
                        "fixture/write",
                        None,
                        ProjectAccessMode::RequestApproval,
                        RiskLevel::R2,
                        || async {
                            Ok(ToolCallOutcome {
                                content: "unexpected execution".to_string(),
                                is_error: false,
                                metadata: None,
                            })
                        },
                    )
                    .await
            })
        };

        let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(request) = engine
                    .pending_for_task("task-drop-external")
                    .await
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("external approval request must become pending");

        execution.abort();
        assert!(execution.await.unwrap_err().is_cancelled());
        assert!(engine
            .decide(&request.id, PermissionDecision::AllowAlways)
            .await
            .is_err());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !engine
                .pending_for_task("task-drop-external")
                .await
                .is_empty()
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropping the external gateway future must cancel its pending approval");

        assert!(matches!(
            engine
                .check(
                    "task-drop-external",
                    "mcp_call",
                    RiskLevel::R2,
                    Some(
                        &serde_json::json!({
                            "server_id": "fixture",
                            "tool": "write"
                        })
                        .to_string()
                    ),
                )
                .await,
            PermissionCheckResult::NeedsApproval(_)
        ));
    }

    #[tokio::test]
    async fn register_and_list_tools() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));
        gw.register(Box::new(WriteTool));

        let specs = gw.tool_specs();
        assert_eq!(specs.len(), 2);

        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"write_file"));

        // R0 不需要确认，R2 需要确认
        let echo_spec = specs.iter().find(|s| s.name == "echo").unwrap();
        assert!(!echo_spec.requires_confirmation);
        let write_spec = specs.iter().find(|s| s.name == "write_file").unwrap();
        assert!(write_spec.requires_confirmation);

        // 来源为 Builtin
        assert!(matches!(echo_spec.source, ToolSource::Builtin));
    }

    #[tokio::test]
    async fn tool_specs_order_is_stable_across_registration_order() {
        // P1-C：HashMap 迭代顺序进程内稳定但重启后随机，不同注册顺序必须产出
        // 同一字节序列，否则重启后首个请求的 tools 前缀必 miss DeepSeek 前缀缓存
        // （PRD §3 A4）。
        let (_, mut gw_a) = make_gateway();
        gw_a.register(Box::new(WriteTool));
        gw_a.register(Box::new(EchoTool));
        let (_, mut gw_b) = make_gateway();
        gw_b.register(Box::new(EchoTool));
        gw_b.register(Box::new(WriteTool));

        let names = |gw: &ToolGateway| -> Vec<String> {
            gw.tool_specs()
                .iter()
                .map(|spec| spec.name.clone())
                .collect()
        };
        assert_eq!(names(&gw_a), names(&gw_b));
        // 名称本身按字典序稳定排序。
        assert_eq!(
            names(&gw_a),
            vec!["echo".to_string(), "write_file".to_string()]
        );
    }

    #[tokio::test]
    async fn list_tools_via_tool_host() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));

        let specs = <ToolGateway as ToolHost>::list_tools(&gw).await.unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "echo");
    }

    #[tokio::test]
    async fn execute_r0_allowed() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));

        let outcome = gw
            .execute_call(
                "t1",
                "r1",
                "echo",
                serde_json::json!({ "text": "hello" }),
                Some("caller-1"),
            )
            .await
            .unwrap();

        assert_eq!(outcome.content, "hello");
        assert!(!outcome.is_error);

        // 审计账本应记录一次成功调用
        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        let entry = &ledger[0];
        assert_eq!(entry.tool_name, "echo");
        assert_eq!(entry.status, ToolCallStatus::Ok);
        assert_eq!(entry.task_id, "t1");
        assert_eq!(entry.run_id, "r1");
        assert_eq!(entry.caller.as_deref(), Some("caller-1"));
        assert!(entry.ended_at.is_some());
    }

    #[tokio::test]
    async fn context_tool_receives_trusted_identity_and_serializes_directive() {
        let (_, mut gw) = make_gateway();
        let seen = Arc::new(std::sync::Mutex::new(None));
        gw.register(Box::new(ContextTool { seen: seen.clone() }));

        let outcome = gw
            .execute_call_with_access_mode(
                "trusted-task",
                "trusted-run",
                "context_tool",
                serde_json::json!({
                    "task_id": "spoofed-task",
                    "run_id": "spoofed-run"
                }),
                Some("agent"),
                ProjectAccessMode::FullAccess,
            )
            .await
            .unwrap();

        let context = seen.lock().unwrap().clone().unwrap();
        assert_eq!(context.task_id, "trusted-task");
        assert_eq!(context.run_id, "trusted-run");
        assert!(!context.tool_call_id.is_empty());
        assert_eq!(context.caller.as_deref(), Some("agent"));
        assert_eq!(context.access_mode, ProjectAccessMode::FullAccess);
        assert!(!gw.requires_workspace_scope("context_tool"));
        assert_eq!(
            tool_outcome_directive(&outcome),
            Some(ToolExecutionDirective::SuspendForUser)
        );
        assert_eq!(
            outcome.metadata.unwrap()["data"]["question_set_id"],
            "set-1"
        );
    }

    #[tokio::test]
    async fn subagent_cannot_invoke_host_only_tool_even_with_full_workspace_access() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(ContextTool {
            seen: Arc::new(std::sync::Mutex::new(None)),
        }));

        let result = gw
            .execute_call_with_access_mode(
                "task-1",
                "child-1",
                "context_tool",
                serde_json::json!({}),
                Some("subagent:child-1"),
                ProjectAccessMode::FullAccess,
            )
            .await;

        assert!(matches!(result, Err(ProductError::PermissionError(_))));
    }

    #[tokio::test]
    async fn policy_guard_blocks_waiting_builtin_and_external_mutations_before_execution() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (_, mut gw) = make_gateway();
        gw.register(Box::new(WriteTool));
        gw.set_policy_guard(Arc::new(RejectMutationsGuard));

        let builtin = gw
            .execute_with_wait_with_access_mode(
                "task",
                "run",
                None,
                "write_file",
                serde_json::json!({ "path": "blocked.txt", "content": "blocked" }),
                Some("agent"),
                "write blocked.txt",
                None,
                ProjectAccessMode::FullAccess,
            )
            .await;
        assert!(matches!(builtin, Err(ProductError::PermissionError(_))));

        let external_called = Arc::new(AtomicBool::new(false));
        let marker = external_called.clone();
        let external = gw
            .execute_external_with_wait(
                "task",
                "run",
                None,
                "mcp_write",
                serde_json::json!({ "value": "blocked" }),
                Some("agent"),
                "external mutation",
                None,
                ProjectAccessMode::FullAccess,
                RiskLevel::R2,
                move || async move {
                    marker.store(true, Ordering::SeqCst);
                    Ok(ToolCallOutcome {
                        content: "mutated".to_string(),
                        is_error: false,
                        metadata: None,
                    })
                },
            )
            .await;
        assert!(matches!(external, Err(ProductError::PermissionError(_))));
        assert!(!external_called.load(Ordering::SeqCst));

        let read_called = Arc::new(AtomicBool::new(false));
        let marker = read_called.clone();
        let read = gw
            .execute_external_with_wait(
                "task",
                "run",
                None,
                "mcp_read",
                serde_json::json!({}),
                Some("agent"),
                "external read",
                None,
                ProjectAccessMode::FullAccess,
                RiskLevel::R1,
                move || async move {
                    marker.store(true, Ordering::SeqCst);
                    Ok(ToolCallOutcome {
                        content: "read".to_string(),
                        is_error: false,
                        metadata: None,
                    })
                },
            )
            .await
            .unwrap();
        assert_eq!(read.content, "read");
        assert!(read_called.load(Ordering::SeqCst));

        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 3);
        assert_eq!(ledger[0].status, ToolCallStatus::Denied);
        assert_eq!(ledger[1].status, ToolCallStatus::Denied);
        assert_eq!(ledger[2].status, ToolCallStatus::Ok);
    }

    #[tokio::test]
    async fn execute_r2_needs_approval() {
        let (engine, mut gw) = make_gateway();
        gw.register(Box::new(WriteTool));

        let result = gw
            .execute_call(
                "t1",
                "r1",
                "write_file",
                serde_json::json!({ "path": "foo.txt", "content": "bar" }),
                Some("agent"),
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ProductError::PermissionError(_)));

        // 审计账本应记录一次拒绝（待审批）
        let audit_id = {
            let ledger = gw.ledger().await;
            assert_eq!(ledger.len(), 1);
            assert_eq!(ledger[0].status, ToolCallStatus::Denied);
            ledger[0].id.clone()
        };
        let pending = engine.pending_for_task("t1").await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].run_id.as_deref(), Some("r1"));
        assert_eq!(pending[0].caller.as_deref(), Some("agent"));
        assert_eq!(pending[0].tool_call_id, audit_id);
    }

    #[tokio::test]
    async fn explicit_approval_tool_needs_approval_under_full_access_but_honors_standing_allow() {
        let (engine, mut gw) = make_gateway();
        let seen_access_modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        gw.register(Box::new(ExplicitApprovalTool {
            seen_access_modes: seen_access_modes.clone(),
        }));

        let input = serde_json::json!({"change": "previewed"});
        let error = gw
            .execute_call_with_access_mode(
                "task-explicit",
                "run-explicit",
                "workspace_approval",
                input.clone(),
                None,
                ProjectAccessMode::FullAccess,
            )
            .await
            .expect_err("full access must not silently bypass this tool's approval gate");
        assert!(matches!(error, ProductError::PermissionError(_)));
        assert!(seen_access_modes.lock().unwrap().is_empty());

        let pending = engine.pending_for_task("task-explicit").await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name, "workspace_approval");
        engine
            .decide(&pending[0].id, PermissionDecision::AllowAlways)
            .await
            .unwrap();

        let outcome = gw
            .execute_call_with_access_mode(
                "task-explicit",
                "run-explicit-2",
                "workspace_approval",
                input,
                None,
                ProjectAccessMode::FullAccess,
            )
            .await
            .unwrap();
        assert_eq!(outcome.content, "approved");
        assert_eq!(
            seen_access_modes.lock().unwrap().as_slice(),
            &[ProjectAccessMode::FullAccess]
        );
    }

    #[tokio::test]
    async fn explicit_approval_tool_waits_under_full_access_and_keeps_original_context_mode() {
        let (engine, mut gateway) = make_gateway();
        let seen_access_modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        gateway.register(Box::new(ExplicitApprovalTool {
            seen_access_modes: seen_access_modes.clone(),
        }));
        let gateway = Arc::new(gateway);

        let execution = {
            let gateway = gateway.clone();
            tokio::spawn(async move {
                gateway
                    .execute_with_wait_with_access_mode(
                        "task-explicit-wait",
                        "run-explicit-wait",
                        Some("call-explicit-wait"),
                        "workspace_approval",
                        serde_json::json!({"change": "previewed"}),
                        None,
                        "apply previewed workspace change",
                        None,
                        ProjectAccessMode::FullAccess,
                    )
                    .await
            })
        };

        let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(request) = engine
                    .pending_for_task("task-explicit-wait")
                    .await
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("full-access call must enter the pending approval queue");
        assert!(!execution.is_finished());
        assert!(seen_access_modes.lock().unwrap().is_empty());

        engine
            .decide(&request.id, PermissionDecision::Allow)
            .await
            .unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), execution)
            .await
            .expect("approved call must resume promptly")
            .unwrap()
            .unwrap();
        assert_eq!(outcome.content, "approved");
        assert_eq!(
            seen_access_modes.lock().unwrap().as_slice(),
            &[ProjectAccessMode::FullAccess]
        );
    }

    #[tokio::test]
    async fn execute_r2_with_standing_rule_allowed() {
        let (engine, mut gw) = make_gateway();
        gw.register(Box::new(WriteTool));

        // 添加 standing rule
        engine
            .add_standing_rule(
                "t1",
                "write_file",
                None,
                RiskLevel::R2,
                PermissionDecision::AllowAlways,
            )
            .await
            .unwrap();

        let outcome = gw
            .execute_call(
                "t1",
                "r1",
                "write_file",
                serde_json::json!({ "path": "foo.txt", "content": "bar" }),
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.content, "wrote to foo.txt");
        assert!(!outcome.is_error);

        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].status, ToolCallStatus::Ok);
    }

    #[tokio::test]
    async fn subagent_cannot_bypass_read_only_policy_even_with_standing_rule() {
        let (engine, mut gw) = make_gateway();
        gw.register(Box::new(WriteTool));
        engine
            .add_standing_rule(
                "t1",
                "write_file",
                None,
                RiskLevel::R2,
                PermissionDecision::AllowAlways,
            )
            .await
            .unwrap();

        assert!(subagent_read_only_tool_allowed("read_file"));
        assert!(!subagent_read_only_tool_allowed("write_file"));

        let direct = gw
            .execute_call(
                "t1",
                "child-1",
                "write_file",
                serde_json::json!({ "path": "foo.txt", "content": "bar" }),
                Some("subagent:child-1"),
            )
            .await;
        assert!(matches!(direct, Err(ProductError::PermissionError(_))));

        let waiting = gw
            .execute_with_wait(
                "t1",
                "child-1",
                Some("child-write"),
                "write_file",
                serde_json::json!({ "path": "foo.txt", "content": "bar" }),
                Some("subagent:child-1"),
                "write_file foo.txt",
                None,
            )
            .await;
        assert!(matches!(waiting, Err(ProductError::PermissionError(_))));

        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 2);
        assert!(ledger
            .iter()
            .all(|entry| entry.status == ToolCallStatus::Denied));
        assert!(ledger
            .iter()
            .all(|entry| entry.caller.as_deref() == Some("subagent:child-1")));
    }

    #[tokio::test]
    async fn explicitly_elevated_subagent_can_use_workspace_write_tools() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(WriteTool));

        let outcome = gw
            .execute_call_with_access_mode(
                "t1",
                "child-full",
                "write_file",
                serde_json::json!({ "path": "foo.txt", "content": "bar" }),
                Some("subagent:child-full"),
                ProjectAccessMode::FullAccess,
            )
            .await
            .unwrap();

        assert_eq!(outcome.content, "wrote to foo.txt");
        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].status, ToolCallStatus::Ok);
        assert_eq!(ledger[0].caller.as_deref(), Some("subagent:child-full"));
    }

    #[tokio::test]
    async fn execute_tool_not_found() {
        let (_, gw) = make_gateway();

        let result = gw
            .execute_call("t1", "r1", "nonexistent", serde_json::json!({}), None)
            .await;

        assert!(result.is_err());
        // 未找到工具不应记录审计
        assert!(gw.ledger().await.is_empty());
    }

    #[tokio::test]
    async fn execute_tool_failure_recorded() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(FailTool));

        let result = gw
            .execute_call("t1", "r1", "fail", serde_json::json!({}), None)
            .await;

        assert!(result.is_err());

        // 失败仍应记录审计
        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].status, ToolCallStatus::Error);
    }

    #[tokio::test]
    async fn retry_safe_tool_absorbs_transient_failures_before_auditing_success() {
        let (_, mut gw) = make_gateway();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        gw.register(Box::new(FlakyTool {
            attempts: attempts.clone(),
            failures_before_success: 2,
            retry_safe: true,
        }));
        let levels = Arc::new(std::sync::Mutex::new(Vec::new()));

        let outcome = async {
            gw.execute_with_wait_with_access_mode(
                "t1",
                "r1",
                Some("call-1"),
                "flaky",
                serde_json::json!({}),
                Some("agent"),
                "flaky operation",
                None,
                ProjectAccessMode::RiskBased,
            )
            .await
        }
        .with_subscriber(LevelSubscriber {
            levels: levels.clone(),
        })
        .await
        .unwrap();

        assert_eq!(outcome.content, "recovered");
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            MAX_TRANSIENT_TOOL_ATTEMPTS
        );
        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].id, "call-1");
        assert_eq!(ledger[0].status, ToolCallStatus::Ok);
        assert_eq!(
            *levels.lock().unwrap(),
            vec![tracing::Level::WARN, tracing::Level::WARN]
        );
    }

    #[tokio::test]
    async fn exhausted_transient_retries_record_only_the_final_failure() {
        let (_, mut gw) = make_gateway();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        gw.register(Box::new(FlakyTool {
            attempts: attempts.clone(),
            failures_before_success: usize::MAX,
            retry_safe: true,
        }));
        let levels = Arc::new(std::sync::Mutex::new(Vec::new()));

        let result = async {
            gw.execute_call("t1", "r1", "flaky", serde_json::json!({}), None)
                .await
        }
        .with_subscriber(LevelSubscriber {
            levels: levels.clone(),
        })
        .await;

        assert!(matches!(result, Err(ProductError::DatabaseError(_))));
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            MAX_TRANSIENT_TOOL_ATTEMPTS
        );
        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].status, ToolCallStatus::Error);
        assert_eq!(
            *levels.lock().unwrap(),
            vec![
                tracing::Level::WARN,
                tracing::Level::WARN,
                tracing::Level::ERROR,
            ]
        );
    }

    #[tokio::test]
    async fn tools_without_replay_opt_in_are_never_retried() {
        let (_, mut gw) = make_gateway();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        gw.register(Box::new(FlakyTool {
            attempts: attempts.clone(),
            failures_before_success: 1,
            retry_safe: false,
        }));

        let result = gw
            .execute_call("t1", "r1", "flaky", serde_json::json!({}), None)
            .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn abort_between_transient_attempts_prevents_replay() {
        let (_, mut gateway) = make_gateway();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let abort = Arc::new(std::sync::atomic::AtomicBool::new(false));
        gateway.register(Box::new(AbortAfterTransientFailureTool {
            attempts: attempts.clone(),
            abort: abort.clone(),
        }));

        let result = gateway
            .execute_with_wait(
                "t1",
                "r1",
                Some("abort-retry-call"),
                "abort_after_transient_failure",
                serde_json::json!({}),
                Some("agent"),
                "retry-safe operation",
                Some(abort),
            )
            .await;

        assert!(
            matches!(result, Err(ProductError::Other(message)) if message.contains("cancelled"))
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
        let ledger = gateway.ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].id, "abort-retry-call");
        assert_eq!(ledger[0].status, ToolCallStatus::Error);
    }

    #[tokio::test]
    async fn call_via_tool_host_trait() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));

        // 通过 ToolHost trait 调用
        let outcome =
            <ToolGateway as ToolHost>::call(&gw, "echo", serde_json::json!({ "text": "via host" }))
                .await
                .unwrap();

        assert_eq!(outcome.content, "via host");
        assert!(!outcome.is_error);
    }

    #[tokio::test]
    async fn call_via_tool_host_unknown_tool() {
        let (_, gw) = make_gateway();

        let result = <ToolGateway as ToolHost>::call(&gw, "nope", serde_json::json!({})).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            agent_error::Error::PermissionDenied(_)
        ));
    }

    #[tokio::test]
    async fn ledger_records_multiple_calls() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));

        gw.execute_call("t1", "r1", "echo", serde_json::json!({ "text": "a" }), None)
            .await
            .unwrap();
        gw.execute_call("t1", "r1", "echo", serde_json::json!({ "text": "b" }), None)
            .await
            .unwrap();

        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger[0].tool_name, "echo");
        assert_eq!(ledger[1].tool_name, "echo");
    }

    #[tokio::test]
    async fn permission_engine_accessor() {
        let (engine, gw) = make_gateway();
        assert!(Arc::ptr_eq(&engine, gw.permission_engine()));
    }

    /// 栈语义测试的公共调用路径：调用 "stacked" 并断言返回文案。
    async fn call_stacked(gw: &ToolGateway, expected: &str) {
        let outcome = gw
            .execute_call("t1", "r1", "stacked", serde_json::json!({}), None)
            .await
            .unwrap_or_else(|error| panic!("stacked call must succeed: {error}"));
        assert_eq!(outcome.content, expected);
    }

    #[tokio::test]
    async fn tool_panic_is_contained_as_error_and_subsequent_calls_continue() {
        // harness-migration 1.1：进程内工具 panic 必须被折成受控错误结果
        // （运行时侧把 Err 转为 is_error 的 ToolCallOutcome），绝不逃逸成
        // 会话死亡；panic 后续调用 / 其他工具不受影响。
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(PanicTool));
        gw.register(Box::new(EchoTool));
        let levels = Arc::new(std::sync::Mutex::new(Vec::new()));

        let first = async {
            gw.execute_call("t1", "r1", "panic_tool", serde_json::json!({}), None)
                .await
        }
        .with_subscriber(LevelSubscriber {
            levels: levels.clone(),
        })
        .await;

        assert!(
            matches!(first, Err(ProductError::Other(ref message)) if message.contains("internal error: tool panicked")),
            "panic 必须被折成受控错误结果"
        );
        assert!(
            levels.lock().unwrap().contains(&tracing::Level::ERROR),
            "panic 必须以 error 级别记录工具名与 panic 信息"
        );

        // 循环继续正常：同一工具连续调用仍是受控错误，其他工具不受影响。
        for _ in 0..2 {
            assert!(gw
                .execute_call("t1", "r1", "panic_tool", serde_json::json!({}), None)
                .await
                .is_err());
        }
        let outcome = gw
            .execute_call(
                "t1",
                "r1",
                "echo",
                serde_json::json!({ "text": "still alive" }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(outcome.content, "still alive");
        assert!(!outcome.is_error);

        // 审计记账未因 panic 中断：3 次错误 + 1 次成功。
        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 4);
        assert_eq!(ledger[0].status, ToolCallStatus::Error);
        assert_eq!(ledger[1].status, ToolCallStatus::Error);
        assert_eq!(ledger[2].status, ToolCallStatus::Error);
        assert_eq!(ledger[3].status, ToolCallStatus::Ok);
    }

    #[tokio::test]
    async fn register_guarded_restores_previous_tool_after_guard_drop() {
        // harness-migration 1.2 栈语义：注册 A -> register_guarded A'（同名）->
        // drop A' guard -> 调用得到 A。
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(NamedReplyTool {
            name: "stacked",
            reply: "base",
        }));

        let guard = gw.register_guarded(Arc::new(NamedReplyTool {
            name: "stacked",
            reply: "override",
        }));
        // 覆盖期间：调用与 tools 列表都只看到栈顶 A'。
        call_stacked(&gw, "override").await;
        assert_eq!(
            gw.tool_specs()
                .iter()
                .filter(|spec| spec.name == "stacked")
                .count(),
            1
        );

        drop(guard);
        call_stacked(&gw, "base").await;
    }

    #[tokio::test]
    async fn guarded_registrations_unwind_in_stack_order() {
        // 多层 guarded 覆盖按栈序撤销；乱序 drop 也只弹自己那一次注册。
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(NamedReplyTool {
            name: "stacked",
            reply: "base",
        }));
        let guard_first = gw.register_guarded(Arc::new(NamedReplyTool {
            name: "stacked",
            reply: "first",
        }));
        let guard_second = gw.register_guarded(Arc::new(NamedReplyTool {
            name: "stacked",
            reply: "second",
        }));

        call_stacked(&gw, "second").await;
        // 乱序：先 drop 早注册的 first，栈顶仍是 second。
        drop(guard_first);
        call_stacked(&gw, "second").await;
        drop(guard_second);
        call_stacked(&gw, "base").await;
    }

    #[tokio::test]
    async fn dropping_last_guarded_registration_removes_the_tool() {
        let (_, mut gw) = make_gateway();
        assert!(!gw.owns_tool("stacked"));
        {
            let _guard = gw.register_guarded(Arc::new(NamedReplyTool {
                name: "stacked",
                reply: "temp",
            }));
            assert!(gw.owns_tool("stacked"));
            assert!(gw.path_bindings("stacked").is_some());
        }
        // 空栈不留残键：工具回到未注册状态。
        assert!(!gw.owns_tool("stacked"));
        assert!(gw.tool_specs().iter().all(|spec| spec.name != "stacked"));
    }

    #[tokio::test]
    async fn register_replaces_same_name_tool_without_guard_restore() {
        // 回归：register 保持旧 insert 语义——同名整体替换；之后 drop 早先的
        // guard 不会把被替换的版本恢复回来。
        let (_, mut gw) = make_gateway();
        let guard = gw.register_guarded(Arc::new(NamedReplyTool {
            name: "stacked",
            reply: "guarded",
        }));
        gw.register(Box::new(NamedReplyTool {
            name: "stacked",
            reply: "inserted",
        }));

        call_stacked(&gw, "inserted").await;
        drop(guard);
        call_stacked(&gw, "inserted").await;
    }
}
