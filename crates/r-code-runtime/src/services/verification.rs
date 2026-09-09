//! Host verification runner.
//!
//! Runs the prepared private candidate/check workspace through the shared
//! [`ExecutionService`] with authorization and typed deadlines; collects
//! exit/log/toolchain evidence; detects changed inputs after the run;
//! distinguishes failed checks from unavailable environments; returns
//! structured repair feedback.

use crate::services::artifacts::sha256_hex;
use crate::services::execution::ExecutionService;
use crate::services::verification_inputs::{materialize, FrozenControlStore, MaterializeError};
use crate::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_harness_protocol::ArtifactRef;
use r_code_kernel::task::EvidenceRecord;
use r_code_kernel::verification::{CheckDefinition, CheckEntrypoint};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The result of one check run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStatus {
    /// The frozen check executed and passed.
    Passed,
    /// The check executed and failed; repair feedback is attached.
    Failed { repair_feedback: String },
    /// The environment could not provide the check (missing dependency,
    /// tampered definition, missing tool): never counts as a pass or a
    /// candidate failure.
    Unavailable { reason: String },
    /// The check exceeded its deadline and was killed.
    TimedOut { after_ms: u64 },
    /// The candidate changed while checking: evidence invalid.
    InputsChanged,
}

/// One executed check's outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckOutcome {
    pub check_id: String,
    pub status: CheckStatus,
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub environment: String,
    /// Host-generated evidence when the check is usable at all.
    pub evidence: Option<EvidenceRecord>,
}

/// The runner.
pub struct VerificationRunner {
    execution: ExecutionService,
    /// Output tails are capped for evidence storage.
    pub tail_limit: usize,
}

impl Default for VerificationRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl VerificationRunner {
    pub fn new() -> Self {
        // Host-run frozen entrypoints are authorized by the frozen contract
        // itself; the capability below lists the standard toolchain
        // executables verification may invoke. Unknown executables fail
        // closed as unavailable.
        let mut authorization = crate::services::authorization::AuthorizationService::new();
        crate::services::launch_profiles::install_profile_capability(
            &mut authorization,
            &crate::services::launch_profiles::ProfileSource {
                harness_id: "host".into(),
                package_digest: "builtin".into(),
                profile_name: "verification".into(),
            },
            HOST_VERIFICATION_EXECUTABLES
                .iter()
                .map(|tool| tool.to_string())
                .collect(),
            None,
            false,
            vec![],
        );
        Self {
            execution: ExecutionService::with_backend(
                std::sync::Arc::new(r_code_gateway::execution_backend::LocalShellBackend::new()),
                std::sync::Arc::new(authorization),
            ),
            tail_limit: 8 * 1024,
        }
    }

    /// Run one check for a candidate. `verify_dir` receives the private
    /// materialized workspace; it should be empty/new per candidate+check.
    pub async fn run(
        &self,
        binding: &TaskWorkspaceBinding,
        manifest: &CandidateManifest,
        control_store: &FrozenControlStore,
        definition: &CheckDefinition,
        verify_dir: &Path,
        timeout: Duration,
    ) -> CheckOutcome {
        let environment = definition.toolchain.clone();
        // 1. Materialize; unavailable environments surface here.
        let prepared = match materialize(binding, manifest, control_store, definition, verify_dir) {
            Ok(prepared) => prepared,
            Err(MaterializeError::StaleCapture(reason)) => {
                return outcome(
                    definition,
                    CheckStatus::InputsChanged,
                    environment,
                    None,
                    "",
                    &format!("stale capture: {reason}"),
                );
            }
            Err(error) => {
                return outcome(
                    definition,
                    CheckStatus::Unavailable {
                        reason: error.to_string(),
                    },
                    environment,
                    None,
                    "",
                    &error.to_string(),
                );
            }
        };

        // 2. Execute the frozen entrypoint in the private directory.
        let (program, argv) = match &definition.entrypoint {
            CheckEntrypoint::Command { program, argv } => (program.clone(), argv.clone()),
        };
        let descriptor =
            crate::services::authorization::OperationDescriptor::verification_preparation(
                &program,
                argv.clone(),
                Some(prepared.dir.to_string_lossy().to_string()),
            );
        let workspace = crate::services::authorization::WorkspaceCapability::WriteWithin {
            root: prepared.dir.to_string_lossy().replace('\\', "/"),
        };
        let permissions = crate::services::authorization::EffectivePermissions::full();
        let command_line = format!("{} {}", program, argv.join(" "));
        let started = std::time::Instant::now();
        let executed = self
            .execution
            .run_authorized(
                &descriptor,
                &workspace,
                &permissions,
                &command_line,
                &prepared.dir,
                timeout,
            )
            .await;

        let output = match executed {
            Ok(output) => output,
            Err(error) => {
                let message = error.to_string();
                if message.contains("timeout") || message.contains("deadline") {
                    return outcome(
                        definition,
                        CheckStatus::TimedOut {
                            after_ms: started.elapsed().as_millis() as u64,
                        },
                        environment,
                        None,
                        "",
                        &message,
                    );
                }
                return outcome(
                    definition,
                    CheckStatus::Unavailable {
                        reason: message.clone(),
                    },
                    environment,
                    None,
                    "",
                    &message,
                );
            }
        };

        // 3. Inputs changed during checking? Re-verify the live candidate.
        if let Err(reason) = manifest.verify_live(binding) {
            return outcome(
                definition,
                CheckStatus::InputsChanged,
                environment,
                output.exit_code,
                &output.stdout,
                &reason.to_string(),
            );
        }

        // 4. Classify. A killed-on-timeout tree may still report a nonzero
        //    exit, so elapsed time decides timeout classification.
        let stdout_tail = tail(&output.stdout, self.tail_limit);
        let stderr_tail = tail(&output.stderr, self.tail_limit);
        let elapsed = started.elapsed();
        let status = if elapsed >= timeout {
            CheckStatus::TimedOut {
                after_ms: elapsed.as_millis() as u64,
            }
        } else {
            match output.exit_code {
                Some(0) => CheckStatus::Passed,
                Some(code) => CheckStatus::Failed {
                    repair_feedback: format!(
                        "check {} exited with {code}. stderr: {}",
                        definition.check_id,
                        stderr_tail.trim()
                    ),
                },
                None => CheckStatus::TimedOut {
                    after_ms: elapsed.as_millis() as u64,
                },
            }
        };

        let digest = manifest.candidate_id.clone();
        let evidence = EvidenceRecord {
            evidence_id: format!(
                "ev-{}-{}",
                definition.check_id,
                &digest[..16.min(digest.len())]
            ),
            check_id: definition.check_id.clone(),
            candidate_digest: digest,
            environment: environment.clone(),
            passed: status == CheckStatus::Passed,
            host_output: Some(ArtifactRef {
                schema: ArtifactRef::SCHEMA,
                blob_id: format!("blob:sha256:{}", sha256_hex(output.stdout.as_bytes())),
                bytes: output.stdout.len() as u64,
                sha256: sha256_hex(output.stdout.as_bytes()),
                media_type: Some("text/plain".into()),
            }),
            recorded_by: r_code_harness_protocol::Provenance::Host,
        };
        CheckOutcome {
            check_id: definition.check_id.clone(),
            status,
            exit_code: output.exit_code,
            stdout_tail,
            stderr_tail,
            environment,
            evidence: Some(evidence),
        }
    }
}

/// Standard toolchain executables frozen verification entrypoints may run.
pub const HOST_VERIFICATION_EXECUTABLES: &[&str] = &[
    "node",
    "npm",
    "npx",
    "cargo",
    "rustc",
    "python",
    "python3",
    "pwsh",
    "powershell",
    "cmd",
    "sh",
    "bash",
    "git",
    "make",
];

fn outcome(
    definition: &CheckDefinition,
    status: CheckStatus,
    environment: String,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> CheckOutcome {
    CheckOutcome {
        check_id: definition.check_id.clone(),
        status,
        exit_code,
        stdout_tail: stdout.to_string(),
        stderr_tail: stderr.to_string(),
        environment,
        evidence: None,
    }
}

fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        text.to_string()
    } else {
        let start = text.len() - limit;
        text[start..].to_string()
    }
}

/// Where private verification directories live for a profile.
pub fn verification_dir_for(root: &Path, candidate_id: &str) -> PathBuf {
    root.join(format!("verify-{candidate_id}"))
}
