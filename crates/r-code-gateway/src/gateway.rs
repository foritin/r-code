//! Tool Gateway -- 工具注册、权限检查、审计记账。 [doc-02 §9, §1, §8]
//!
//! Tool Gateway 是 Agent 工具调用的唯一入口。所有调用经过：
//! Schema 校验 -> 权限分级 -> 执行 -> 记账。
//!
//! `ToolGateway` 实现 `hermes_core::ToolHost` trait，可无缝接入 Agent 循环。
//!
//! ## 审计策略 [doc-02 §8]
//! 所有调用（含被拒绝 / 待审批）入 `ledger`，含调用者身份。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{ToolCallOutcome, ToolHost, ToolSource, ToolSpec};
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
    /// 声明需要经 `PathGuard` 重绑定的输入键。默认单个必填 `path`。
    fn path_bindings(&self) -> &'static [PathBinding] {
        DEFAULT_PATH_BINDINGS
    }
    /// 工具是否要求路径已存在。默认 `true`（只读工具）。
    ///
    /// `create_file` 等写入工具覆写为 `false`：目标文件尚未创建，需通过
    /// `PathGuard::resolve`（而非 `resolve_existing`）解析。
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

        match tool
            .execute_with_context_and_abort_with_workspace(
                input.clone(),
                context,
                abort_flag,
                workspace_guard,
            )
            .await
        {
            Ok(result) => return Ok(result),
            Err(error) if is_transient_storage_contention(&error) && attempt < max_attempts => {
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
            Err(error) if is_transient_storage_contention(&error) && max_attempts > 1 => {
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
            Err(error) => return Err(error),
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
    matches!(
        name,
        "read_file" | "list_files" | "search" | "glob" | "git_status"
    )
}

fn is_subagent_caller(caller: Option<&str>) -> bool {
    caller.is_some_and(|value| value.starts_with("subagent:"))
}

/// Tool Gateway -- 管理工具注册、权限检查与审计账本。
///
/// 实现 `hermes_core::ToolHost`，可注册到 Agent 循环。
pub struct ToolGateway {
    tools: HashMap<String, Box<dyn Tool>>,
    permission_engine: Arc<PermissionEngine>,
    /// 审计账本 -- 所有工具调用记录（含被拒绝 / 待审批）。
    ledger: Arc<RwLock<Vec<ToolCall>>>,
    policy_guard: Option<Arc<dyn ToolPolicyGuard>>,
}

impl ToolGateway {
    /// 创建新的 Tool Gateway。
    pub fn new(permission_engine: Arc<PermissionEngine>) -> Self {
        Self {
            tools: HashMap::new(),
            permission_engine,
            ledger: Arc::new(RwLock::new(Vec::new())),
            policy_guard: None,
        }
    }

    pub fn set_policy_guard(&mut self, guard: Arc<dyn ToolPolicyGuard>) {
        self.policy_guard = Some(guard);
    }

    /// 注册一个工具。
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// 查询某工具声明的路径绑定；工具未注册时返回 `None`。
    ///
    /// 供 Agent 运行时在调用前把路径参数重绑定到会话工作区。
    pub fn path_bindings(&self, tool_name: &str) -> Option<&'static [PathBinding]> {
        self.tools.get(tool_name).map(|tool| tool.path_bindings())
    }

    /// 查询某工具是否要求路径已存在。未注册工具默认 `true`（fail-closed）。
    pub fn requires_existing_path(&self, tool_name: &str) -> bool {
        self.tools
            .get(tool_name)
            .map(|tool| tool.requires_existing_path())
            .unwrap_or(true)
    }

    /// Query whether a registered tool requires an attached workspace scope.
    /// Unknown tools default to `true` so callers cannot accidentally expose them globally.
    pub fn requires_workspace_scope(&self, tool_name: &str) -> bool {
        self.tools
            .get(tool_name)
            .map(|tool| tool.requires_workspace_scope())
            .unwrap_or(true)
    }

    /// 列出所有已注册工具的规格。
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|tool| ToolSpec {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: tool.input_schema(),
                source: ToolSource::Builtin,
                requires_confirmation: tool.risk_level().requires_confirmation(),
            })
            .collect()
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
            .tools
            .get(tool_name)
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
        };
        if let Some(guard) = &self.policy_guard {
            if let Err(error) = guard.check(&context, tool_name, risk_level) {
                audit.deny(error.to_string());
                self.ledger.write().await.push(audit);
                return Err(error);
            }
        }

        // 5. 权限检查
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
                access_mode,
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
            .tools
            .get(tool_name)
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
                target,
                access_mode,
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

/// 为 `ToolGateway` 实现 `hermes_core::ToolHost`。
///
/// - `list_tools`：返回所有已注册工具的 `ToolSpec`。
/// - `call`：委托给 `execute_call`（task_id / run_id 为空，表示直接调用）。
#[async_trait]
impl ToolHost for ToolGateway {
    async fn list_tools(&self) -> hermes_error::Result<Vec<ToolSpec>> {
        Ok(self.tool_specs())
    }

    async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> hermes_error::Result<ToolCallOutcome> {
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
            hermes_error::Error::PermissionDenied(_)
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
}
