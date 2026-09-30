//! Host verification runner.
//!
//! Runs the prepared private candidate/check workspace through the shared
//! [`ExecutionService`] with authorization and typed deadlines; collects
//! exit/log/toolchain evidence; detects changed inputs after the run;
//! distinguishes failed checks from unavailable environments; returns
//! structured repair feedback.

use crate::services::execution::{CheckSpawnSpec, ExecutionService, SandboxedCheckBackend};
use crate::services::verification_inputs::{materialize, FrozenControlStore, MaterializeError};
use crate::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_harness_protocol::services::{NetworkCeiling, PermissionCeiling};
use r_code_kernel::task::EvidenceRecord;
use r_code_kernel::verification::{CheckDefinition, CheckEntrypoint};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    /// The P12/P13 platform report identity digest the evidence binds. A
    /// changed boot/platform identity yields a different value, so recorded
    /// evidence stales (acceptance ③). Empty on the injected real-execution
    /// path used only by legacy fixtures.
    evidence_identity: String,
}

/// The network ceiling every required check runs under. Checks are always
/// `Offline` (INV-06): neither task settings nor a wider profile can grant
/// them network authority.
pub const CHECK_NETWORK_CEILING: NetworkCeiling = NetworkCeiling::Offline;

/// The prefix carried by every sandbox-disabled Unavailable reason on the
/// required-checks path, so callers and tests can pin the refusal text.
pub const SANDBOX_DISABLED_REASON: &str = "sandbox-disabled";

impl Default for VerificationRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl VerificationRunner {
    /// Real-execution runner over the local shell backend. Retained for the
    /// T19 host fixtures only; the production required-checks path never uses
    /// it (see [`VerificationRunner::sandboxed`], wired at the run-manager
    /// call site), so no required check spawns outside the sandbox gate.
    pub fn new() -> Self {
        Self::with_backend(Arc::new(
            r_code_gateway::execution_backend::LocalShellBackend::new(),
        ))
    }

    /// Runner over an explicitly injected backend (the test seam for a
    /// passing fake/native required check). The check is still pinned
    /// `Offline` and still mints sandbox-bound evidence.
    pub fn with_backend(
        backend: Arc<dyn r_code_gateway::execution_backend::CommandExecutionBackend>,
    ) -> Self {
        Self::with_backend_identity(backend, String::new())
    }

    /// Inject a backend together with the platform report identity the minted
    /// evidence binds. This is the seam for proving evidence staleness: the
    /// same candidate/check run under two different `evidence_identity`
    /// values yields two different `environment_fingerprint`s, so prior
    /// evidence no longer satisfies the requirement (acceptance ③).
    pub fn with_backend_identity(
        backend: Arc<dyn r_code_gateway::execution_backend::CommandExecutionBackend>,
        evidence_identity: impl Into<String>,
    ) -> Self {
        Self::from_backend(backend, evidence_identity.into())
    }

    /// The production required-checks runner: it resolves the platform
    /// safety report for this exact boot identity and refuses to spawn on
    /// anything but an Activated report. On every non-Activated platform the
    /// backend id is `sandbox-gated` and each check reports
    /// `Unavailable { reason }` carrying [`SANDBOX_DISABLED_REASON`], with no
    /// local process ever started.
    pub fn sandboxed(store: &r_code_store::v1::V1Store, boot_identity: &str) -> Self {
        use crate::services::execution::CheckSandboxGate;
        use crate::services::sandbox::SafetyActivation;
        let gate = crate::services::sandbox::platform_activation_gate(store, boot_identity);
        let check_gate = match &gate {
            SafetyActivation::Activated { .. } => CheckSandboxGate::Activated,
            SafetyActivation::NotActivated { reason, status } => CheckSandboxGate::Closed {
                reason: sandbox_disabled_reason(reason, status.as_deref()),
            },
        };
        // There is no native sandbox backend bound this wave, so even an
        // Activated verdict stays fail-closed; evidence still binds the
        // current platform identity so it stales when that identity changes.
        let evidence_identity =
            crate::services::sandbox::current_platform_material(boot_identity).digest();
        let backend = Arc::new(SandboxedCheckBackend::new(check_gate, None));
        Self::from_backend(backend, evidence_identity)
    }

    fn from_backend(
        backend: Arc<dyn r_code_gateway::execution_backend::CommandExecutionBackend>,
        evidence_identity: String,
    ) -> Self {
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
            execution: ExecutionService::with_backend(backend, Arc::new(authorization)),
            tail_limit: 8 * 1024,
            evidence_identity,
        }
    }

    /// The id of the backend a check would run on. The production
    /// required-checks path returns `sandbox-gated`/`sandbox-native`, never
    /// `local` — acceptance ①.
    pub fn backend_id(&self) -> &'static str {
        self.execution.default_backend_id()
    }

    /// The network ceiling a check runs under — always
    /// [`CHECK_NETWORK_CEILING`] (`Offline`), regardless of settings.
    pub fn network_ceiling(&self) -> NetworkCeiling {
        CHECK_NETWORK_CEILING
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
        if definition.validate().is_err() {
            return outcome(
                definition,
                CheckStatus::Unavailable {
                    reason: "invalid host check definition".to_string(),
                },
                environment,
                None,
                "",
                "invalid host check definition",
            );
        }
        if verify_dir.exists()
            && std::fs::read_dir(verify_dir)
                .ok()
                .and_then(|mut entries| entries.next())
                .is_some()
        {
            return outcome(
                definition,
                CheckStatus::Unavailable {
                    reason: "verification directory is not private and empty".to_string(),
                },
                environment,
                None,
                "",
                "verification directory is not private and empty",
            );
        }
        // 1. Materialize; unavailable environments surface here.
        let prepared = match materialize(binding, manifest, control_store, definition, verify_dir) {
            Ok(prepared) => prepared,
            Err(MaterializeError::StaleCapture(reason)) => {
                let reason = sanitized_tail(&reason, self.tail_limit);
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
                let message = sanitized_tail(&error.to_string(), self.tail_limit);
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

        // 2. Freeze the exact spawn material, then execute it in the private
        //    directory. Authorization, the backend and the evidence all read
        //    this one spec, so nothing re-parses the caller's intent (P20.1).
        let (program, argv) = match &definition.entrypoint {
            CheckEntrypoint::Command { program, argv } => (program.clone(), argv.clone()),
        };
        let spec = match CheckSpawnSpec::build(
            &program,
            &argv,
            &prepared.dir,
            &definition.toolchain,
            &definition.source_roots,
        ) {
            Ok(spec) => spec,
            Err(error) => {
                let reason = format!("check material is not spawnable: {error}");
                return outcome(
                    definition,
                    CheckStatus::Unavailable {
                        reason: reason.clone(),
                    },
                    environment,
                    None,
                    "",
                    &reason,
                );
            }
        };
        let program = spec.executable.to_string_lossy().to_string();
        let argv = spec.arguments.clone();
        // P21.3: a promoted dependency cache that exists for this candidate is
        // mounted read-only into the check's own material. A cold identity adds
        // nothing, and an overlay never does — bytes the host has not validated
        // cannot become visible to a check (acceptance ③).
        let spec = if prepared.promoted_cache.is_dir() {
            match spec.with_promoted_cache(&prepared.promoted_cache, &prepared.overlay) {
                Ok(spec) => spec,
                Err(error) => {
                    let reason = format!("promoted cache cannot be mounted: {error}");
                    return outcome(
                        definition,
                        CheckStatus::Unavailable {
                            reason: reason.clone(),
                        },
                        environment,
                        None,
                        "",
                        &reason,
                    );
                }
            }
        } else {
            spec
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
        // Required checks are pinned Offline: the frozen contract authorizes
        // the toolchain process, but network authority is withheld no matter
        // what the task settings would otherwise grant (acceptance ②).
        let permissions = check_permissions();
        let started = std::time::Instant::now();
        let executed = self
            .execution
            .run_check(&spec, &descriptor, &workspace, &permissions, timeout)
            .await;

        let output = match executed {
            Ok(output) => output,
            Err(error) => {
                let raw_message = error.to_string();
                let message = sanitized_tail(&raw_message, self.tail_limit);
                if raw_message.contains("timeout") || raw_message.contains("deadline") {
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
                let message = informative_unavailable_reason(message);
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
        let stdout_tail = sanitized_tail(&output.stdout, self.tail_limit);
        let stderr_tail = sanitized_tail(&output.stderr, self.tail_limit);
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
        let environment_fingerprint = environment_fingerprint(
            definition,
            manifest,
            &spec,
            timeout,
            &self.evidence_identity,
        );
        let definition_identity = definition.identity();
        let evidence = EvidenceRecord {
            evidence_id: format!(
                "ev-{}-{}-{}",
                definition.check_id,
                &digest[..16.min(digest.len())],
                &environment_fingerprint[..16]
            ),
            task_id: binding.task_id.clone(),
            check_id: definition.check_id.clone(),
            definition_identity,
            candidate_digest: digest,
            environment: environment.clone(),
            environment_fingerprint,
            passed: status == CheckStatus::Passed,
            host_output: None,
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

fn sanitized_tail(text: &str, limit: usize) -> String {
    let redacted = text
        .lines()
        .map(|line| {
            let normalized = line.to_ascii_lowercase();
            if [
                "api_key",
                "apikey",
                "token",
                "password",
                "authorization",
                "secret",
            ]
            .iter()
            .any(|needle| normalized.contains(needle))
            {
                "<redacted>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    tail(&redacted, limit)
}

fn informative_unavailable_reason(message: String) -> String {
    let diagnostic = message
        .lines()
        .any(|line| !line.trim().is_empty() && line.trim() != "<redacted>");
    if diagnostic {
        message
    } else {
        "program unavailable or denied by host policy".to_string()
    }
}

/// The fingerprint committed into evidence: the complete frozen material that
/// was actually spawnable, plus the platform report identity that authorized
/// it. Changing the executable, an argument, the working directory, the
/// toolchain, a read root, the candidate bytes or the boot identity all yield
/// a different value, so prior evidence no longer satisfies the requirement
/// (P20.3, acceptance ③).
fn environment_fingerprint(
    definition: &CheckDefinition,
    manifest: &CandidateManifest,
    spec: &CheckSpawnSpec,
    timeout: Duration,
    identity: &str,
) -> String {
    let dependency_digests = definition
        .dependency_locks
        .iter()
        .map(|path| (path, manifest.files.get(path)))
        .collect::<Vec<_>>();
    let control_digests = definition
        .control_files
        .iter()
        .map(|control| (&control.path, &control.sha256))
        .collect::<Vec<_>>();
    r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "toolchain": definition.toolchain,
        "spawnSpec": spec.digest(),
        "timeoutMs": timeout.as_millis(),
        "candidate": manifest.candidate_id,
        "dependencies": dependency_digests,
        "controls": control_digests,
        // P12/P13 platform report identity digest: a changed boot/platform
        // identity flips this and stales prior evidence (acceptance ③).
        "reportIdentity": identity,
    }))
}

/// The permissions a required check always runs under: the frozen toolchain
/// process is allowed, network is never (INV-06). Constructed here so no
/// caller can widen it from task settings.
fn check_permissions() -> crate::services::authorization::EffectivePermissions {
    crate::services::authorization::EffectivePermissions {
        ceiling: PermissionCeiling::Full,
        allow_processes: true,
        allow_network: false,
    }
}

/// The fail-closed reason a sandboxed check backend carries when the platform
/// report is not Activated; surfaced verbatim as the Unavailable reason.
fn sandbox_disabled_reason(gate_reason: &str, status: Option<&str>) -> String {
    format!(
        "{SANDBOX_DISABLED_REASON}: required checks are offline until the platform safety \
         capability is activated (activation-gate: {gate_reason}; report-status: {})",
        status.unwrap_or("none")
    )
}

/// Where private verification directories live for a profile.
pub fn verification_dir_for(root: &Path, candidate_id: &str) -> PathBuf {
    root.join(format!("verify-{candidate_id}"))
}
