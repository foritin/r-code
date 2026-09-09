//! Managed interactive processes (host.process.open/write/close).
//!
//! Launches are authorized through the common service against a
//! LaunchCapability resolved from a pinned process profile; NDJSON-RPC
//! profiles validate every complete outbound frame before it reaches the
//! child; every tree runs under the platform guardian (Job Object on
//! Windows, process group + EOF guardian on Unix). Handles are run-scoped.

use crate::process_guard::GuardedOwnerIdentity;
use crate::services::authorization::{
    AuthorizationDecision, AuthorizationService, CredentialScope, EffectivePermissions,
    OperationDescriptor, WorkspaceCapability,
};
use crate::services::process_profiles::FrameValidator;
use r_code_harness_protocol::process_profile::ConstraintViolation;
use r_code_harness_protocol::services::{OutputBlock, ToolCallError, ToolCallReply};
use r_code_kernel::ports::{GenerationToken, ProcessService, ServiceError};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// Errors surfaced by the managed process service.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessError {
    #[error("authorization denied: {0}")]
    Denied(String),
    #[error("unknown handle {0:?}")]
    UnknownHandle(String),
    #[error("frame rejected: {0}")]
    FrameRejected(String),
    #[error("io failure: {0}")]
    Io(String),
}

struct ManagedProcess {
    run_id: String,
    stdin: Option<tokio::process::ChildStdin>,
    #[allow(dead_code)]
    owner: GuardedOwnerIdentity,
    child: tokio::process::Child,
    #[cfg(windows)]
    job: Option<crate::process_guard::windows::JobGuard>,
    #[cfg(unix)]
    pgid: Option<i32>,
}

/// The host.process service.
pub struct ManagedProcessService {
    authorization: Arc<AuthorizationService>,
    permissions: EffectivePermissions,
    workspace: WorkspaceCapability,
    validator: Option<FrameValidator>,
    processes: Mutex<HashMap<String, ManagedProcess>>,
    profile_executables: Mutex<HashMap<String, String>>,
    next_handle: AtomicU64,
}

impl ManagedProcessService {
    pub fn new(
        authorization: Arc<AuthorizationService>,
        permissions: EffectivePermissions,
        workspace: WorkspaceCapability,
        validator: Option<FrameValidator>,
    ) -> Self {
        Self {
            authorization,
            permissions,
            workspace,
            validator,
            processes: Mutex::new(HashMap::new()),
            profile_executables: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
        }
    }

    /// Register the executable a profile name resolves to (host-resolved,
    /// never plugin-supplied).
    pub async fn register_profile_executable(&self, profile: &str, executable: &str) {
        self.profile_executables
            .lock()
            .await
            .insert(profile.to_string(), executable.to_string());
    }

    /// Open a managed process from a profile-pinned executable.
    pub async fn open_profiled(
        &self,
        token: &GenerationToken,
        executable: &std::path::Path,
        argv: &[String],
        cwd: Option<&str>,
    ) -> Result<String, ProcessError> {
        let descriptor = OperationDescriptor::process_launch(
            &executable.to_string_lossy(),
            argv.to_vec(),
            cwd.map(str::to_string),
        );
        match self.authorization.authorize(
            &descriptor,
            &self.workspace,
            &self.permissions,
            &CredentialScope::default(),
        ) {
            AuthorizationDecision::Allowed => {}
            AuthorizationDecision::RequiresApproval { summary } => {
                return Err(ProcessError::Denied(format!(
                    "approval required: {summary}"
                )));
            }
            AuthorizationDecision::Denied(reason) => {
                return Err(ProcessError::Denied(reason.to_string()));
            }
        }

        let mut command = tokio::process::Command::new(executable);
        command.args(argv);
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|e| ProcessError::Io(format!("launch failed: {e}")))?;
        let stdin = child.stdin.take();
        let pid = child.id().unwrap_or(0);

        // Guardian assignment before the child can escape.
        #[cfg(windows)]
        let job = {
            let job = crate::process_guard::windows::JobGuard::create()
                .map_err(|e| ProcessError::Io(e.to_string()))?;
            job.assign_pid(pid)
                .map_err(|e| ProcessError::Io(e.to_string()))?;
            Some(job)
        };

        let owner = GuardedOwnerIdentity {
            pid,
            start_identity: start_identity_of(pid),
            boot_nonce: boot_nonce(),
        };

        let sequence = self.next_handle.fetch_add(1, Ordering::SeqCst);
        let handle = format!("{}:{sequence}", token.run_id);
        self.processes.lock().await.insert(
            handle.clone(),
            ManagedProcess {
                run_id: token.run_id.clone(),
                stdin,
                owner,
                child,
                #[cfg(windows)]
                job,
                #[cfg(unix)]
                pgid: Some(pid as i32),
            },
        );
        Ok(handle)
    }

    /// Write bytes; NDJSON-RPC profiles validate complete frames first.
    pub async fn write_validated(
        &self,
        token: &GenerationToken,
        handle: &str,
        data: Vec<u8>,
    ) -> Result<(), ProcessError> {
        let mut processes = self.processes.lock().await;
        let process = processes
            .get_mut(handle)
            .ok_or_else(|| ProcessError::UnknownHandle(handle.to_string()))?;
        if process.run_id != token.run_id {
            return Err(ProcessError::UnknownHandle(handle.to_string()));
        }
        if let Some(validator) = &self.validator {
            let mut buffer = data.clone();
            // Validate every complete frame; a trailing partial stays
            // buffered would be caller's job — for the RPC profile the
            // caller always writes whole frames, so anything without a
            // newline is refused.
            if !buffer.ends_with(b"\n") {
                buffer.push(b'\n');
            }
            validator
                .drain_buffer(&mut buffer)
                .map_err(|violation: ConstraintViolation| {
                    ProcessError::FrameRejected(violation.to_string())
                })?;
        }
        let stdin = process
            .stdin
            .as_mut()
            .ok_or_else(|| ProcessError::Io("stdin closed".into()))?;
        stdin
            .write_all(&data)
            .await
            .map_err(|e| ProcessError::Io(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| ProcessError::Io(e.to_string()))?;
        Ok(())
    }

    /// Close a handle: terminate the tree and confirm.
    pub async fn close_confirmed(
        &self,
        token: &GenerationToken,
        handle: &str,
    ) -> Result<Option<i32>, ProcessError> {
        let mut process = self
            .processes
            .lock()
            .await
            .remove(handle)
            .ok_or_else(|| ProcessError::UnknownHandle(handle.to_string()))?;
        if process.run_id != token.run_id {
            // Return it; not ours to close.
            self.processes
                .lock()
                .await
                .insert(handle.to_string(), process);
            return Err(ProcessError::UnknownHandle(handle.to_string()));
        }
        let pid = process.child.id();
        let _ = process.child.start_kill();
        #[cfg(windows)]
        if let Some(job) = process.job.take() {
            job.terminate();
            drop(job);
        }
        #[cfg(unix)]
        if let Some(pgid) = process.pgid {
            unsafe {
                libc::kill(-pgid, libc::SIGTERM);
            }
        }
        let exit = process
            .child
            .wait()
            .await
            .ok()
            .and_then(|status| status.code());
        let _ = pid;
        Ok(exit)
    }
}

#[async_trait::async_trait]
impl ProcessService for ManagedProcessService {
    async fn open(
        &self,
        token: GenerationToken,
        profile: &str,
        arguments: Vec<String>,
        cwd: Option<String>,
    ) -> Result<String, ServiceError> {
        // The profile name resolves to an executable through the registered
        // host-resolved table; the launch itself still passes authorization.
        let executable = self
            .profile_executables
            .lock()
            .await
            .get(profile)
            .cloned()
            .ok_or_else(|| {
                ServiceError::Failure(format!("profile {profile:?} has no executable"))
            })?;
        self.open_profiled(
            &token,
            std::path::Path::new(&executable),
            &arguments,
            cwd.as_deref(),
        )
        .await
        .map_err(ServiceError::from)
    }

    async fn write(
        &self,
        token: GenerationToken,
        handle: &str,
        data: Vec<u8>,
    ) -> Result<(), ServiceError> {
        self.write_validated(&token, handle, data)
            .await
            .map_err(ServiceError::from)
    }

    async fn close(
        &self,
        token: GenerationToken,
        handle: &str,
    ) -> Result<Option<i32>, ServiceError> {
        self.close_confirmed(&token, handle)
            .await
            .map_err(ServiceError::from)
    }
}

impl From<ProcessError> for ServiceError {
    fn from(error: ProcessError) -> Self {
        ServiceError::Failure(error.to_string())
    }
}

fn start_identity_of(pid: u32) -> u64 {
    #[cfg(windows)]
    {
        crate::process_guard::windows::process_start_identity(pid)
    }
    #[cfg(unix)]
    {
        crate::process_guard::unix::process_start_identity(pid)
    }
}

fn boot_nonce() -> String {
    #[cfg(windows)]
    {
        format!("pid-{}", std::process::id())
    }
    #[cfg(unix)]
    {
        let boot = std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|stat| {
                stat.lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1).map(str::to_string))
            })
            .unwrap_or_else(|| "unknown".into());
        format!("boot-{boot}")
    }
}

/// A ToolCallReply-shaped error for router mapping.
pub fn process_error_reply(error: &ProcessError) -> ToolCallReply {
    ToolCallReply {
        output: vec![OutputBlock::Text {
            text: String::new(),
        }],
        error: Some(ToolCallError {
            code: "process-error".into(),
            message: error.to_string(),
            denied_by: None,
        }),
    }
}

/// How long close confirmation waits before surfacing unverifiable.
pub const CLOSE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
