//! Unified command execution for tools and verification.
//!
//! One service routes every shell-shaped effect: common authorization first
//! (T12a), then spawn/collect on a selectable [`CommandExecutionBackend`].
//! The default backend is the existing five-tier shell resolution chain
//! (RTK-aware, Windows-dialect preserving); process trees are killed on
//! timeout/abort by the backend's kill-on-drop + kill-tree semantics.

use crate::services::authorization::{
    AuthorizationDecision, AuthorizationService, EffectivePermissions, OperationDescriptor,
    WorkspaceCapability,
};
use r_code_gateway::execution_backend::{
    CollectedOutput, CommandExecutionBackend, CommandSpec, LocalShellBackend,
};
use r_code_kernel::ports::ServiceError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Errors from unified execution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionError {
    #[error("authorization denied: {0}")]
    Denied(String),
    #[error("approval required: {0}")]
    ApprovalRequired(String),
    #[error("backend failure: {0}")]
    Backend(String),
}

/// The shared execution service.
pub struct ExecutionService {
    default: Arc<dyn CommandExecutionBackend>,
    selected: tokio::sync::RwLock<Option<Arc<dyn CommandExecutionBackend>>>,
    authorization: Arc<AuthorizationService>,
    abort: Arc<AtomicBool>,
}

impl ExecutionService {
    /// Service over the default local shell backend.
    pub fn local(authorization: Arc<AuthorizationService>) -> Self {
        Self {
            default: Arc::new(LocalShellBackend::new()),
            selected: tokio::sync::RwLock::new(None),
            authorization,
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Service over an explicit default backend (tests, docker).
    pub fn with_backend(
        default: Arc<dyn CommandExecutionBackend>,
        authorization: Arc<AuthorizationService>,
    ) -> Self {
        Self {
            default,
            selected: tokio::sync::RwLock::new(None),
            authorization,
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Select the backend used by subsequent runs (verification prep may
    /// pin its own); None falls back to the default.
    pub async fn select_backend(&self, backend: Option<Arc<dyn CommandExecutionBackend>>) {
        *self.selected.write().await = backend;
    }

    /// The backend a run would currently use.
    async fn effective_backend(&self) -> Arc<dyn CommandExecutionBackend> {
        match &*self.selected.read().await {
            Some(selected) => selected.clone(),
            None => self.default.clone(),
        }
    }

    /// Request abort of in-flight and future commands (cancellation).
    pub fn abort_all(&self) {
        self.abort.store(true, Ordering::SeqCst);
    }

    fn clear_abort(&self) {
        self.abort.store(false, Ordering::SeqCst);
    }

    /// Run one authorized command. Authorization happens here; backends
    /// never re-decide semantics, they only execute.
    pub async fn run_authorized(
        &self,
        descriptor: &OperationDescriptor,
        workspace: &WorkspaceCapability,
        permissions: &EffectivePermissions,
        command: &str,
        cwd: &std::path::Path,
        timeout: Duration,
    ) -> Result<CollectedOutput, ExecutionError> {
        match self
            .authorization
            .authorize(descriptor, workspace, permissions, &Default::default())
        {
            AuthorizationDecision::Allowed => {}
            AuthorizationDecision::RequiresApproval { summary } => {
                return Err(ExecutionError::ApprovalRequired(summary));
            }
            AuthorizationDecision::Denied(reason) => {
                return Err(ExecutionError::Denied(reason.to_string()));
            }
        }
        let spec = CommandSpec {
            command: command.to_string(),
            cwd: cwd.to_path_buf(),
            timeout,
        };
        let backend = self.effective_backend().await;
        self.clear_abort();
        let handle = backend
            .spawn(&spec, Some(&self.abort))
            .await
            .map_err(|e| ExecutionError::Backend(e.to_string()))?;
        backend
            .collect(handle, &spec, Some(&self.abort))
            .await
            .map_err(|e| ExecutionError::Backend(e.to_string()))
    }

    /// The bash-tool route: same authorization + backend, tool-shaped
    /// descriptor. Future verification preparation calls the same path.
    pub async fn run_bash(
        &self,
        workspace: &WorkspaceCapability,
        permissions: &EffectivePermissions,
        command: &str,
        cwd: &std::path::Path,
        timeout: Duration,
    ) -> Result<CollectedOutput, ExecutionError> {
        let descriptor = OperationDescriptor::tool_call("bash", vec![command.to_string()], None);
        self.run_authorized(&descriptor, workspace, permissions, command, cwd, timeout)
            .await
    }
}

impl From<ExecutionError> for ServiceError {
    fn from(error: ExecutionError) -> Self {
        ServiceError::Failure(error.to_string())
    }
}
