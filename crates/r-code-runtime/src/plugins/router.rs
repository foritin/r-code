//! Host RPC router: maps plugin calls onto kernel service ports.
//!
//! Every inbound plugin request is checked in this order, all *before* any
//! effectful service runs:
//! 1. the method must exist in the protocol (fail closed);
//! 2. the method's host service must be part of the negotiated grants;
//! 3. a host-pinned process effect above the interactive ceiling hides the
//!    method entirely, exactly as if the protocol had no such method;
//! 4. the run generation must still be live (late callbacks rejected);
//! 5. run-scoped handles must belong to this run;
//! 6. effectful calls deduplicate through attempt-stable operation keys.

use r_code_harness_protocol::rpc::{error_code, RpcError, RpcNotification, RpcRequest};
use r_code_harness_protocol::services::*;
use r_code_harness_protocol::{ApprovalsRequest, HostService, OperationKey, RunIdentity};
use r_code_kernel::plans::{PlanRevision, PlanRevisionMaterial};
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ProcessService, RunGuard, ServiceError,
    ToolService,
};
use r_code_kernel::task::{RunSnapshot, RunSnapshotPhase, TaskExecution, TaskKind};
use r_code_store::v1::V1Store;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::plugins::approval_store::{ApprovalStore, DEFAULT_DECISION_TIMEOUT};
use crate::services::artifacts::{ArtifactError, ArtifactStore};
use crate::services::context::{ContextError, TranscriptWriter, MAX_TRANSCRIPT_PAGE_LIMIT};
use crate::services::process_profiles::ProcessProfileEffect;

/// A persisted question raised by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaisedQuestion {
    pub question_id: String,
    pub text: String,
    pub blocking: bool,
}

/// Sink receiving persisted questions.
pub trait QuestionSink: Send + Sync {
    fn raised(&self, question: RaisedQuestion);
}

/// No-op sink for tests that don't observe questions.
#[derive(Default)]
pub struct IgnoreQuestions;

impl QuestionSink for IgnoreQuestions {
    fn raised(&self, _question: RaisedQuestion) {}
}

/// The router over the service ports.
pub struct HostRouter {
    pub identity: RunIdentity,
    pub guard: Arc<RunGuard>,
    pub granted: Vec<HostService>,
    pub tools: Arc<dyn ToolService>,
    pub models: Arc<dyn ModelService>,
    pub processes: Arc<dyn ProcessService>,
    pub store: Arc<dyn JournalStore>,
    pub questions: Arc<dyn QuestionSink>,
    pub approvals: Arc<ApprovalStore>,
    /// Observation stream for tests and the daemon event fan-out.
    pub observed_events: Mutex<Vec<RpcNotification>>,
    /// Host-side observation tap: `(kind, payload)` pairs the run manager
    /// drains and persists to the journal as host-provenance events
    /// (assistant turns, tool calls, model usage).
    pub host_observations: Mutex<Vec<(String, serde_json::Value)>>,
    /// Completion proposals recorded from the plugin (arbitrated in T20).
    pub recorded_proposals: Mutex<Vec<CompletionProposalRequest>>,
    /// Host-confirmed immutable plan publications for finalization fencing.
    pub recorded_plan_publications: Mutex<Vec<PlanPublishReply>>,
    /// FR-8 (M1a-10): the executor-backed children controls for
    /// host.children.* (supervisor + ceiling + command channel).
    pub child_controls: Option<Arc<crate::services::children_executor::ChildControls>>,
    /// FR-1.5 (M1a-07): JIT injection source for the model projection.
    jit_tracker: Option<Arc<Mutex<crate::services::project_instructions::JitTracker>>>,
    transcript: Option<Arc<TranscriptWriter>>,
    artifacts: Option<Arc<ArtifactStore>>,
    v1_store: Option<Arc<V1Store>>,
    run_snapshot: Option<RunSnapshot>,
    required_checks: Vec<String>,
    plan_publish_enabled: bool,
    task_kind: Option<TaskKind>,
    /// Host-pinned process-profile effects (P22.3). A pinned workspace-write
    /// effect keeps the whole interactive Process set unadvertised and
    /// unroutable; an unpinned profile is resolved by the service itself,
    /// which refuses it without starting anything.
    process_effects: HashMap<String, ProcessProfileEffect>,
    checkpoint_revision: std::sync::atomic::AtomicU64,
}

/// Task-scoped service availability used for both negotiation and routing.
#[derive(Debug, Clone, Copy, Default)]
pub struct RouterServiceAvailability {
    pub model_stream: bool,
    pub tools: bool,
    pub context: bool,
    pub artifacts: bool,
    pub plan_publish: bool,
    pub questions: bool,
    pub approvals: bool,
    pub checkpoints: bool,
    pub completion: bool,
    /// FR-8 (M1a-10): children.* services are live for this run.
    pub children: bool,
    /// P13 activation gate for sandboxed effect services (Process* and
    /// VerificationRun): true ONLY when an exact current
    /// SafetyCapabilityReport evaluated to Activated. Guessed calls stay
    /// denied; nothing may grant these services without this flag.
    pub sandbox_activated: bool,
}

impl RouterServiceAvailability {
    fn supports(self, service: HostService) -> bool {
        match service {
            HostService::ModelStream => self.model_stream,
            HostService::ToolsList | HostService::ToolsCall => self.tools,
            HostService::ContextRead => self.context,
            HostService::ArtifactsPut | HostService::ArtifactsRead => self.artifacts,
            HostService::PlanPublish => self.plan_publish,
            HostService::QuestionsAsk => self.questions,
            HostService::ApprovalsRequest => self.approvals,
            HostService::CheckpointSave => self.checkpoints,
            HostService::CompletionPropose => self.completion,
            HostService::ProcessOpen
            | HostService::ProcessRead
            | HostService::ProcessWrite
            | HostService::ProcessClose
            | HostService::VerificationRun => self.sandbox_activated,
            HostService::PlanUpdate => false,
            HostService::ChildrenSpawn
            | HostService::ChildrenWait
            | HostService::ChildrenCancel => self.children,
        }
    }
}

/// Intersect manifest requests with services that are actually usable for
/// this task. The returned vector is the single grant list shared by the
/// router and `NegotiatedCapabilities`.
pub fn supported_requested_services(
    requested: &[HostService],
    availability: RouterServiceAvailability,
) -> Vec<HostService> {
    requested
        .iter()
        .copied()
        .filter(|service| availability.supports(*service))
        .collect()
}

/// Which host service a wire method requires.
pub fn service_for_method(method: &str) -> Option<HostService> {
    Some(match method {
        "host.model.stream" => HostService::ModelStream,
        "host.tools.list" => HostService::ToolsList,
        "host.tools.call" => HostService::ToolsCall,
        "host.process.open" => HostService::ProcessOpen,
        "host.process.read" => HostService::ProcessRead,
        "host.process.write" => HostService::ProcessWrite,
        "host.process.close" => HostService::ProcessClose,
        "host.context.read" => HostService::ContextRead,
        "host.artifacts.put" => HostService::ArtifactsPut,
        "host.artifacts.read" => HostService::ArtifactsRead,
        "host.plan.publish" => HostService::PlanPublish,
        "host.plan.update" => HostService::PlanUpdate,
        "host.questions.ask" => HostService::QuestionsAsk,
        "host.approvals.request" => HostService::ApprovalsRequest,
        "host.children.spawn" => HostService::ChildrenSpawn,
        "host.children.wait" => HostService::ChildrenWait,
        "host.children.cancel" => HostService::ChildrenCancel,
        "host.verification.run" => HostService::VerificationRun,
        "host.checkpoint.save" => HostService::CheckpointSave,
        "host.completion.propose" => HostService::CompletionPropose,
        _ => return None,
    })
}

impl HostRouter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: RunIdentity,
        guard: Arc<RunGuard>,
        granted: Vec<HostService>,
        tools: Arc<dyn ToolService>,
        models: Arc<dyn ModelService>,
        processes: Arc<dyn ProcessService>,
        store: Arc<dyn JournalStore>,
        questions: Arc<dyn QuestionSink>,
    ) -> Self {
        Self {
            identity,
            guard,
            granted,
            tools,
            models,
            processes,
            store: store.clone(),
            questions,
            approvals: Arc::new(ApprovalStore::new(store, DEFAULT_DECISION_TIMEOUT)),
            observed_events: Mutex::new(Vec::new()),
            host_observations: Mutex::new(Vec::new()),
            recorded_proposals: Mutex::new(Vec::new()),
            recorded_plan_publications: Mutex::new(Vec::new()),
            child_controls: None,
            transcript: None,
            artifacts: None,
            v1_store: None,
            run_snapshot: None,
            jit_tracker: None,
            required_checks: Vec::new(),
            plan_publish_enabled: false,
            task_kind: None,
            process_effects: HashMap::new(),
            checkpoint_revision: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Pin the workspace effect of one process profile, host-side only.
    ///
    /// The effect decides discovery: an effect above the interactive ceiling
    /// drops every Process service from this run's grants, so the tool set is
    /// never advertised, and [`Self::process_request_is_hidden`] makes a call
    /// for it answer exactly like an unknown method (P22.3, acceptance ③).
    pub fn pin_process_profile_effect(
        mut self,
        profile: &str,
        effect: ProcessProfileEffect,
    ) -> Self {
        self.process_effects.insert(profile.to_string(), effect);
        // Grants name services, not profiles, so an above-ceiling pin cannot
        // remove the surface for a profile this run may legitimately use. The
        // surface is pruned only when the run has no admissible profile at all;
        // otherwise the request-time hide below is what keeps a workspace
        // writing profile undiscoverable.
        let any_admissible = self
            .process_effects
            .values()
            .any(|effect| effect.interactive_process_admitted());
        if !any_admissible {
            self.granted.retain(|service| {
                !matches!(
                    service,
                    HostService::ProcessOpen
                        | HostService::ProcessRead
                        | HostService::ProcessWrite
                        | HostService::ProcessClose
                )
            });
        }
        self
    }

    /// The host-pinned effect of one profile, if it has one.
    pub fn process_effect(&self, profile: &str) -> Option<ProcessProfileEffect> {
        self.process_effects.get(profile).copied()
    }

    /// Share the daemon-wide approval store (RunManager wiring); a router
    /// built standalone keeps its own store over the same journal.
    pub fn with_approvals(mut self, approvals: Arc<ApprovalStore>) -> Self {
        self.approvals = approvals;
        self
    }

    /// Attach child supervision (the daemon shares one supervisor).
    /// FR-1.5 (M1a-07): attach the JIT tracker used by the model-stream
    /// projection.
    pub fn with_jit_tracker(
        mut self,
        tracker: Arc<Mutex<crate::services::project_instructions::JitTracker>>,
    ) -> Self {
        self.jit_tracker = Some(tracker);
        self
    }

    pub fn with_children(
        mut self,
        controls: Arc<crate::services::children_executor::ChildControls>,
    ) -> Self {
        self.child_controls = Some(controls);
        self
    }

    /// FR-8 (M1a-10): Option-taking variant for run paths without children.
    pub fn with_children_controls(
        mut self,
        controls: Option<Arc<crate::services::children_executor::ChildControls>>,
    ) -> Self {
        self.child_controls = controls;
        self
    }

    pub fn with_transcript(mut self, transcript: Arc<TranscriptWriter>) -> Self {
        self.transcript = Some(transcript);
        self
    }

    pub fn with_artifacts(mut self, artifacts: Arc<ArtifactStore>) -> Self {
        self.artifacts = Some(artifacts);
        self
    }

    pub fn with_v1_store(mut self, store: Arc<V1Store>) -> Self {
        self.v1_store = Some(store);
        self
    }

    pub fn with_plan_publication(
        mut self,
        snapshot: RunSnapshot,
        task_kind: TaskKind,
        required_checks: Vec<String>,
        enabled: bool,
    ) -> Self {
        self.run_snapshot = Some(snapshot);
        self.task_kind = Some(task_kind);
        self.required_checks = required_checks;
        self.plan_publish_enabled = enabled;
        self
    }

    fn next_checkpoint_revision(&self) -> u64 {
        use std::sync::atomic::Ordering;
        self.checkpoint_revision.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn token(&self) -> GenerationToken {
        GenerationToken {
            run_id: self.identity.run_id.clone(),
            generation: self.identity.generation,
        }
    }

    fn reject(&self, error: RpcError) -> Result<(), RpcError> {
        Err(error)
    }

    /// Whether this request names a Process profile the host pinned above the
    /// interactive ceiling (P22.3). Only `host.process.open` carries a profile
    /// name; read/write/close can only ever address a tree this router opened,
    /// and an open for an unpinned profile is refused by the service, which
    /// starts nothing.
    fn process_request_is_hidden(&self, request: &RpcRequest) -> bool {
        if request.method != "host.process.open" {
            return false;
        }
        let profile = request
            .params
            .as_ref()
            .and_then(|params| params.get("profile"))
            .and_then(|profile| profile.as_str())
            .unwrap_or_default();
        matches!(
            self.process_effect(profile),
            Some(effect) if !effect.interactive_process_admitted()
        )
    }

    /// Handle one plugin request. All gates run before any effect.
    pub async fn handle_request(&self, request: RpcRequest) -> Result<serde_json::Value, RpcError> {
        // 1. Known method?
        if let Some(error) = r_code_harness_protocol::rpc::reject_unknown_method(&request.method) {
            return Err(error);
        }
        // 2. A process profile the host pinned above the interactive ceiling
        // answers exactly like a method that does not exist. This runs before
        // the grant and generation checks so that a hidden profile cannot be
        // turned into a policy message, and cannot be distinguished from a
        // method this run was never granted.
        if self.process_request_is_hidden(&request) {
            return Err(RpcError::method_not_found(&request.method));
        }
        // 3. Granted service?
        let required = match service_for_method(&request.method) {
            Some(service) => service,
            None => return Err(RpcError::method_not_found(&request.method)),
        };
        if !self.granted.contains(&required) {
            return Err(RpcError {
                code: error_code::PROTOCOL_VIOLATION,
                message: format!(
                    "service {} is not part of this run's grants",
                    required.wire_name()
                ),
                data: None,
            });
        }
        // 4. Live generation?
        let token = self.token();
        if let Err(error) = self.guard.check(&token) {
            return Err(stale_error(error));
        }

        let params = request.params.clone().unwrap_or(serde_json::Value::Null);
        match request.method.as_str() {
            "host.tools.list" => {
                let _parsed: ToolsListRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let tools = self.tools.list(token).await.map_err(service_error)?;
                Ok(serde_json::to_value(ToolsListReply {
                    tools,
                    next_cursor: None,
                })
                .unwrap_or_default())
            }
            "host.tools.call" => {
                let call: ToolCallRequest = serde_json::from_value(params.clone())
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let operation_key = call.operation_key.clone();
                self.deduplicated(operation_key, "host.tools.call", &params, || {
                    Box::pin(async move {
                        let name = call.tool.clone();
                        let input_preview = tool_input_preview(&call.tool, &call.input);
                        self.observe(
                            "tool.call",
                            serde_json::json!({
                                "runId": self.identity.run_id,
                                "name": name,
                                "input": input_preview,
                            }),
                        );
                        let reply = self.tools.call(token, call).await.map_err(service_error)?;
                        let output_preview: Vec<String> = reply
                            .output
                            .iter()
                            .map(|block| match block {
                                OutputBlock::Text { text } => text.clone(),
                                OutputBlock::Json { value } => value.to_string(),
                                OutputBlock::Image { .. } => String::new(),
                            })
                            .collect();
                        let ok = reply.error.is_none();
                        let message = reply
                            .error
                            .as_ref()
                            .map(|error| error.message.clone())
                            .unwrap_or_else(|| output_preview.join("\n"));
                        self.observe(
                            "tool.result",
                            serde_json::json!({
                                "runId": self.identity.run_id,
                                "name": name,
                                "ok": ok,
                                "output": preview(&message),
                            }),
                        );
                        Ok(serde_json::to_value(&reply).unwrap_or_default())
                    })
                })
                .await
            }
            "host.model.stream" => {
                let model_request: ModelStreamRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                // The canonical transcript syncs the ORIGINAL request; the
                // JIT projection below is model-visible only (FR-1.5).
                self.sync_transcript(&model_request)?;
                let model_request = self.project_jit_instructions(model_request);
                let mut sink = RouterStreamSink::default();
                let outcome = self
                    .models
                    .stream(token, model_request, &mut sink)
                    .await
                    .map_err(service_error)?;
                // Project the stream into the assistant turn the plugin's
                // loop consumes: text + complete tool calls.
                let assistant = sink.assistant_turn();
                // Host observation: the full assistant turn and the usage row
                // (journal-persisted by the run manager; Provenance::Host).
                let turn_text = assistant["blocks"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|block| block["type"] == "text")
                            .filter_map(|block| block["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                if !turn_text.is_empty() {
                    self.observe(
                        "assistant.message",
                        serde_json::json!({
                            "runId": self.identity.run_id,
                            "text": turn_text,
                        }),
                    );
                }
                self.observe(
                    "model.usage",
                    serde_json::json!({
                        "runId": self.identity.run_id,
                        "usage": outcome.usage,
                    }),
                );
                Ok(serde_json::json!({
                    "stream_id": outcome.stream_id,
                    "finish_reason": outcome.finish_reason,
                    "usage": outcome.usage,
                    "chunks": sink.events.len(),
                    "assistant": assistant,
                }))
            }
            "host.context.read" => {
                let read: ContextReadRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                if read.projection == "instructions" {
                    // FR-1 (5.2/5.3.1): serve the frozen instruction set
                    // from the run snapshot; the reserved CatalogSnapshot
                    // field is filled from the same material.
                    let snapshot = self.run_snapshot.as_ref().ok_or_else(|| {
                        RpcError::invalid_params(
                            "no run snapshot is configured for the instructions projection",
                        )
                    })?;
                    let instructions = &snapshot.material().instructions;
                    return Ok(serde_json::json!({
                        "catalog": crate::services::context::CatalogSnapshot::from_instructions(instructions),
                        "digest": instructions.digest,
                        "rendered": instructions.rendered,
                        "entries": instructions.entries,
                    }));
                }
                if read.projection != "transcript" {
                    return Err(RpcError::invalid_params(
                        "only the transcript and instructions context projections are available",
                    ));
                }
                let transcript = self.task_transcript()?;
                let limit = read
                    .limit
                    .unwrap_or(100)
                    .clamp(1, MAX_TRANSCRIPT_PAGE_LIMIT);
                let page = transcript.read_page(read.cursor.unwrap_or(0), limit);
                Ok(serde_json::to_value(page).unwrap_or_default())
            }
            "host.artifacts.put" => {
                let put: ArtifactsPutRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let artifacts = self.task_artifacts()?;
                let reference = artifacts
                    .put(&put, &self.identity.task_id)
                    .map_err(artifact_error)?;
                Ok(serde_json::to_value(reference).unwrap_or_default())
            }
            "host.artifacts.read" => {
                let read: ArtifactsReadRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let artifacts = self.task_artifacts()?;
                let reply = artifacts.read(&read).map_err(artifact_error)?;
                Ok(serde_json::to_value(reply).unwrap_or_default())
            }
            "host.plan.publish" => {
                let publish: PlanPublishRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                self.publish_plan(publish)
            }
            "host.process.open" => {
                let open: ProcessOpenRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let operation_key = operation_key_of(&request_params(&request));
                self.deduplicated(
                    operation_key,
                    "host.process.open",
                    &request_params(&request),
                    || {
                        Box::pin(async move {
                            let handle = self
                                .processes
                                .open(token, &open.profile, open.arguments, open.cwd)
                                .await
                                .map_err(service_error)?;
                            let scoped = format!("{}:{handle}", self.identity.run_id);
                            Ok(serde_json::to_value(ProcessOpenReply { handle: scoped })
                                .unwrap_or_default())
                        })
                    },
                )
                .await
            }
            "host.process.read" => {
                let mut read: ProcessReadRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::invalid_params(error.to_string()))?;
                if read.max_bytes == 0 || read.max_bytes > PROCESS_READ_MAX_BYTES {
                    return Err(RpcError::invalid_params(format!(
                        "maxBytes must be in 1..={PROCESS_READ_MAX_BYTES}"
                    )));
                }
                if read
                    .wait_ms
                    .is_some_and(|wait_ms| wait_ms > PROCESS_READ_MAX_WAIT_MS)
                {
                    return Err(RpcError::invalid_params(format!(
                        "waitMs exceeds {PROCESS_READ_MAX_WAIT_MS}"
                    )));
                }
                let (run, raw) = split_run_handle(&read.handle)
                    .ok_or_else(|| run_mismatch("process handle carries no run identity"))?;
                if run != self.identity.run_id {
                    self.reject(run_mismatch("process handle belongs to another run"))?;
                }
                read.handle = raw;
                let reply = self
                    .processes
                    .read(token, read.clone())
                    .await
                    .map_err(service_error)?;
                validate_process_read_page(&read, &reply).map_err(RpcError::internal)?;
                serde_json::to_value(reply).map_err(|error| RpcError::internal(error.to_string()))
            }
            "host.process.write" => {
                let write: ProcessWriteRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let (run, raw) = split_run_handle(&write.handle)
                    .ok_or_else(|| run_mismatch("process handle carries no run identity"))?;
                if run != self.identity.run_id {
                    self.reject(run_mismatch("process handle belongs to another run"))?;
                }
                let data = base64_decode(&write.data_base64)
                    .map_err(|e| RpcError::invalid_params(format!("bad base64: {e}")))?;
                self.processes
                    .write(token, &raw, data)
                    .await
                    .map_err(service_error)?;
                Ok(serde_json::Value::Null)
            }
            "host.process.close" => {
                let close: ProcessCloseRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let (run, raw) = split_run_handle(&close.handle)
                    .ok_or_else(|| run_mismatch("process handle carries no run identity"))?;
                if run != self.identity.run_id {
                    self.reject(run_mismatch("process handle belongs to another run"))?;
                }
                let exit = self
                    .processes
                    .close(token, &raw)
                    .await
                    .map_err(service_error)?;
                Ok(serde_json::to_value(ProcessCloseReply { exit_code: exit }).unwrap_or_default())
            }
            "host.questions.ask" => {
                let question: QuestionsAskRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let question_id = format!(
                    "q-{}-{}",
                    self.identity.run_id,
                    uuid::Uuid::new_v4().simple()
                );
                if let Some(store) = &self.v1_store {
                    self.validate_question_owner(store)?;
                    self.persist_question(store, &question_id, &question)?;
                }
                self.questions.raised(RaisedQuestion {
                    question_id: question_id.clone(),
                    text: question.text.clone(),
                    blocking: question.blocking,
                });
                Ok(serde_json::to_value(QuestionsAskReply { question_id }).unwrap_or_default())
            }
            "host.approvals.request" => {
                let approval: ApprovalsRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                // Only host-created pending operation references are valid;
                // an unknown reference is a protocol violation (never
                // auto-created, never journaled).
                let op_id = approval.pending_operation.operation_id.clone();
                let Some(mut waiter) = self.approvals.waiter(&op_id).await else {
                    return Err(RpcError {
                        code: error_code::PROTOCOL_VIOLATION,
                        message: format!(
                            "approvals may only reference host-created pending operations; \
                             {op_id} was never registered"
                        ),
                        data: None,
                    });
                };
                // Block until a decision (or the timeout denial — journaled
                // as a decided event like any other decision).
                let decision = self.approvals.await_decision(&op_id, &mut waiter).await;
                Ok(serde_json::to_value(ApprovalsReply { decision }).unwrap_or_default())
            }
            "host.children.spawn" => {
                let spawn_request: ChildrenSpawnRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let controls = self.child_controls.as_ref().ok_or_else(|| {
                    RpcError::internal("children supervision not configured for this run")
                })?;
                let child_task_id = controls
                    .request_spawn(spawn_request)
                    .map_err(RpcError::internal)?;
                Ok(serde_json::to_value(ChildrenSpawnReply {
                    child_task_id: child_task_id.clone(),
                    child_run_id: child_task_id,
                })
                .unwrap_or_default())
            }
            "host.children.wait" => {
                let wait: ChildrenWaitRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let controls = self.child_controls.as_ref().ok_or_else(|| {
                    RpcError::internal("children supervision not configured for this run")
                })?;
                // Minutes-scale blocking wait on the supervisor condvar —
                // no busy polling (FR-8 acceptance e).
                let timeout_ms = wait
                    .timeout_ms
                    .unwrap_or(5 * 60 * 1000)
                    .clamp(1, 30 * 60 * 1000);
                let supervisor = Arc::clone(&controls.supervisor);
                let child_id = wait.child_task_id.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    crate::services::children_executor::wait_child_blocking(
                        &supervisor,
                        &child_id,
                        timeout_ms,
                    )
                })
                .await
                .map_err(|e| RpcError::internal(format!("wait join failed: {e}")))?
                .map_err(RpcError::internal)?;
                match outcome {
                    r_code_kernel::children::ChildWait::Completed(report) => {
                        Ok(serde_json::to_value(report).unwrap_or_default())
                    }
                    r_code_kernel::children::ChildWait::Cancelled => Err(RpcError::internal(
                        format!("child {} was cancelled", wait.child_task_id),
                    )),
                    r_code_kernel::children::ChildWait::Running => {
                        Err(r_code_harness_protocol::rpc::RpcError {
                            code: error_code::PROTOCOL_VIOLATION,
                            message: format!(
                                "wait for child {} timed out without completion",
                                wait.child_task_id
                            ),
                            data: None,
                        })
                    }
                }
            }
            "host.children.cancel" => {
                let cancel: ChildrenCancelRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let controls = self.child_controls.as_ref().ok_or_else(|| {
                    RpcError::internal("children supervision not configured for this run")
                })?;
                controls
                    .request_cancel(&cancel.child_task_id)
                    .map_err(RpcError::internal)?;
                Ok(serde_json::Value::Null)
            }
            "host.checkpoint.save" => {
                let save: CheckpointSaveRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let state = base64_decode(&save.state_base64)
                    .map_err(|e| RpcError::invalid_params(format!("bad base64: {e}")))?;
                let revision = self.next_checkpoint_revision();
                let artifact = self
                    .store
                    .save_checkpoint(
                        &self.identity.attempt_id,
                        revision,
                        state,
                        save.consumed_input_seq,
                    )
                    .await
                    .map_err(service_error)?;
                Ok(serde_json::to_value(CheckpointSaveReply {
                    checkpoint: artifact,
                    revision,
                })
                .unwrap_or_default())
            }
            "host.completion.propose" => {
                let proposal: CompletionProposalRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                // The host records every proposal; authoritative arbitration
                // (T20) decides verdicts — a plugin proposal is never a fact.
                self.recorded_proposals
                    .lock()
                    .expect("proposals")
                    .push(proposal.clone());
                Ok(serde_json::to_value(CompletionProposalReply {
                    accepted: true,
                    verdict: Some("recorded".into()),
                    repair_feedback: None,
                })
                .unwrap_or_default())
            }
            other => Err(RpcError {
                code: error_code::INTERNAL,
                message: format!("service {other} is not routed by this host build yet"),
                data: None,
            }),
        }
    }

    fn task_transcript(&self) -> Result<&Arc<TranscriptWriter>, RpcError> {
        let transcript = self
            .transcript
            .as_ref()
            .ok_or_else(|| RpcError::internal("transcript service is not configured"))?;
        if transcript.task_id() != self.identity.task_id {
            return Err(run_mismatch("transcript belongs to another task"));
        }
        Ok(transcript)
    }

    /// FR-1.5 (M1a-07): append the monotone JIT instruction block to the
    /// request's first system message. Model-visible only — the canonical
    /// transcript was already synced from the unprojected request.
    fn project_jit_instructions(&self, mut request: ModelStreamRequest) -> ModelStreamRequest {
        let Some(tracker) = &self.jit_tracker else {
            return request;
        };
        let (block, audit) = {
            let Ok(mut tracker) = tracker.lock() else {
                return request;
            };
            (tracker.render_current(), tracker.take_audit_events())
        };
        for (kind, payload) in audit {
            self.observe(&kind, payload);
        }
        let Some(block) = block else {
            return request;
        };
        // Ledger the JIT injection (fail-open, same discipline as the
        // frozen-injection rows).
        if let Some(store) = &self.v1_store {
            let record = r_code_store::v1::InjectionRecord {
                run_id: self.identity.run_id.clone(),
                kind: r_code_store::v1::InjectionKind::Jit,
                snapshot_hash: crate::services::artifacts::sha256_hex(block.as_bytes()),
                refs: Vec::new(),
                chars: block.chars().count() as u64,
            };
            if let Err(error) = store.record_injection(&record) {
                eprintln!(
                    "jit injection ledger write failed for {}: {error}",
                    self.identity.run_id
                );
            }
        }
        if let Some(message) = request
            .messages
            .iter_mut()
            .find(|message| message.role == r_code_harness_protocol::services::ModelRole::System)
        {
            message
                .content
                .push(r_code_harness_protocol::services::ContentBlock::Text { text: block });
        }
        request
    }

    fn sync_transcript(&self, request: &ModelStreamRequest) -> Result<(), RpcError> {
        let Some(transcript) = &self.transcript else {
            return Ok(());
        };
        if transcript.task_id() != self.identity.task_id {
            return Err(run_mismatch("transcript belongs to another task"));
        }
        // System/developer blocks are frozen control inputs in RunSnapshot,
        // not conversation history. Excluding that leading control layer
        // lets a later approved run use a new prompt without rewriting the
        // task's canonical user/assistant/tool prefix.
        let messages = request
            .messages
            .iter()
            .filter(|message| !matches!(message.role, ModelRole::System | ModelRole::Developer))
            .cloned()
            .collect::<Vec<_>>();
        transcript.sync_messages(&messages).map_err(context_error)
    }

    fn task_artifacts(&self) -> Result<&Arc<ArtifactStore>, RpcError> {
        let artifacts = self
            .artifacts
            .as_ref()
            .ok_or_else(|| RpcError::internal("artifact service is not configured"))?;
        if artifacts
            .task_id()
            .is_some_and(|task_id| task_id != self.identity.task_id)
        {
            return Err(run_mismatch("artifact store belongs to another task"));
        }
        Ok(artifacts)
    }

    fn publish_plan(&self, request: PlanPublishRequest) -> Result<serde_json::Value, RpcError> {
        if !self.plan_publish_enabled
            || !matches!(
                self.task_kind,
                Some(TaskKind::PlanDraft | TaskKind::Implementation | TaskKind::Repair)
            )
        {
            return Err(protocol_violation(
                "plan publication is not enabled for this task",
            ));
        }
        let snapshot = self
            .run_snapshot
            .as_ref()
            .ok_or_else(|| RpcError::internal("run snapshot is not configured"))?;
        snapshot
            .validate_identity()
            .map_err(|_| protocol_violation("run snapshot failed identity validation"))?;
        if snapshot.material().task_id != self.identity.task_id {
            return Err(run_mismatch("run snapshot belongs to another task"));
        }
        if !matches!(snapshot.phase(), RunSnapshotPhase::Planning) {
            return Err(protocol_violation("only a planning run may publish a plan"));
        }
        let store = self
            .v1_store
            .as_ref()
            .ok_or_else(|| RpcError::internal("plan store is not configured"))?;
        let current = store
            .current_plan_revision(&self.identity.task_id)
            .map_err(|_| RpcError::internal("plan head could not be loaded"))?;
        let expected_next = match current.as_ref() {
            Some(revision) => revision
                .material()
                .revision
                .checked_add(1)
                .ok_or_else(|| protocol_violation("plan revision overflow"))?,
            None => 1,
        };
        let is_exact_replay = current
            .as_ref()
            .is_some_and(|revision| revision.material().revision == request.revision);
        if request.revision != expected_next && !is_exact_replay {
            return Err(protocol_violation(&format!(
                "plan revision must be the next head ({expected_next})"
            )));
        }
        if request
            .work_units
            .iter()
            .any(|unit| unit.id.trim().is_empty() || unit.description.trim().is_empty())
        {
            return Err(RpcError::invalid_params(
                "plan work units require non-empty ids and descriptions",
            ));
        }
        let expected_head = current
            .as_ref()
            .map(|revision| revision.reference().clone());
        let parent_revision = current.as_ref().and_then(|revision| {
            if revision.material().revision == request.revision {
                revision.material().parent_revision.clone()
            } else {
                Some(revision.reference().clone())
            }
        });
        let material = snapshot.material();
        let route_digest = r_code_harness_protocol::canonical_input_hash(
            &serde_json::to_value(&material.provider)
                .map_err(|_| RpcError::internal("provider snapshot could not be encoded"))?,
        );
        let mut required_checks = self.required_checks.clone();
        required_checks.sort();
        required_checks.dedup();
        let check_digest = r_code_harness_protocol::canonical_input_hash(
            &serde_json::to_value(&required_checks)
                .map_err(|_| RpcError::internal("required checks could not be encoded"))?,
        );
        let revision = PlanRevision::new(PlanRevisionMaterial {
            task_id: self.identity.task_id.clone(),
            revision: request.revision,
            parent_revision,
            current_base_hash: material.workspace.baseline_sha256.clone(),
            workspace_baseline: material.workspace.baseline_sha256.clone(),
            route_digest,
            prompt_digest: material.prompt.content_sha256.clone(),
            permission_digest: material.permissions.revision.clone(),
            check_digest,
            required_checks,
            work_units: request.work_units,
        })
        .map_err(|_| RpcError::invalid_params("plan revision is invalid"))?;
        let revision_ref = store
            .publish_plan_revision(&revision, expected_head.as_ref())
            .map_err(|_| protocol_violation("plan publication failed"))?;
        let reply = PlanPublishReply {
            revision: request.revision,
            revision_hash: revision_ref.as_str().to_string(),
        };
        self.recorded_plan_publications
            .lock()
            .expect("plan publications")
            .push(reply.clone());
        Ok(serde_json::to_value(reply).unwrap_or_default())
    }

    fn persist_question(
        &self,
        store: &V1Store,
        question_id: &str,
        question: &QuestionsAskRequest,
    ) -> Result<(), RpcError> {
        store
            .save_question(
                question_id,
                &self.identity.task_id,
                &self.identity.run_id,
                &question.text,
                &question.options,
                question.blocking,
            )
            .map_err(|_| RpcError::internal("question could not be persisted"))?;

        for _ in 0..8 {
            let (mut task, revision) = store
                .load_task_with_revision(&self.identity.task_id)
                .map_err(|_| RpcError::internal("question task could not be loaded"))?
                .ok_or_else(|| RpcError::internal("question task does not exist"))?;
            match &task.execution {
                TaskExecution::Running {
                    attempt_id,
                    generation,
                } if attempt_id == &self.identity.attempt_id
                    && *generation == self.identity.generation => {}
                _ => {
                    return Err(run_mismatch(
                        "question does not belong to the active run generation",
                    ));
                }
            }
            if question.blocking {
                task.wait_for_input(self.identity.generation, question_id)
                    .map_err(|_| protocol_violation("task cannot wait for this question"))?;
            }
            let event = r_code_kernel::ports::JournalEvent {
                seq: 0,
                task_id: self.identity.task_id.clone(),
                kind: "question.raised".to_string(),
                payload: serde_json::json!({
                    "questionId": question_id,
                    "runId": self.identity.run_id,
                    "attemptId": self.identity.attempt_id,
                    "generation": self.identity.generation,
                    "blocking": question.blocking,
                }),
            };
            match store.save_task_and_events_if_revision(&task, vec![event], revision) {
                Ok(_) => return Ok(()),
                Err(r_code_store::v1::V1StoreError::StaleTaskRevision { .. }) => continue,
                Err(_) => return Err(RpcError::internal("question event could not be persisted")),
            }
        }
        Err(RpcError::internal(
            "question task stayed busy during persistence",
        ))
    }

    fn validate_question_owner(&self, store: &V1Store) -> Result<(), RpcError> {
        let (task, _) = store
            .load_task_with_revision(&self.identity.task_id)
            .map_err(|_| RpcError::internal("question task could not be loaded"))?
            .ok_or_else(|| RpcError::internal("question task does not exist"))?;
        match task.execution {
            TaskExecution::Running {
                attempt_id,
                generation,
            } if attempt_id == self.identity.attempt_id
                && generation == self.identity.generation =>
            {
                Ok(())
            }
            _ => Err(run_mismatch(
                "question does not belong to the active run generation",
            )),
        }
    }

    /// Progress notifications from the plugin.
    pub async fn handle_notification(&self, notification: RpcNotification) {
        if notification.method == "harness.event" {
            self.observed_events
                .lock()
                .expect("events")
                .push(notification);
        }
    }

    /// Record a host-side observation for journal persistence.
    fn observe(&self, kind: &str, payload: serde_json::Value) {
        self.host_observations
            .lock()
            .expect("observations")
            .push((kind.to_string(), payload));
    }

    /// Attempt-stable operation dedup over the journal store.
    async fn deduplicated<F, Fut>(
        &self,
        operation_key: Option<OperationKey>,
        method: &str,
        params: &serde_json::Value,
        execute: F,
    ) -> Result<serde_json::Value, RpcError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<serde_json::Value, RpcError>>,
    {
        let Some(key) = operation_key else {
            // No key: execute directly (the plugin opted out of idempotency).
            return execute().await;
        };
        let hash = r_code_harness_protocol::canonical_input_hash(params);
        let existing_record = self
            .store
            .load_receipt(&self.identity.attempt_id, &key)
            .await
            .map(receipt_to_record);
        match r_code_harness_protocol::replay_decision(existing_record.as_ref(), method, &hash) {
            r_code_harness_protocol::ReplayDecision::ReplayReceipt { result } => Ok(result),
            r_code_harness_protocol::ReplayDecision::ConflictingInput { recorded, incoming } => {
                Err(RpcError {
                    code: error_code::PROTOCOL_VIOLATION,
                    message: format!(
                        "operation key {} reused with different input ({recorded} vs {incoming})",
                        key.0
                    ),
                    data: None,
                })
            }
            r_code_harness_protocol::ReplayDecision::Fresh => {
                // Persist the intent before the effect.
                self.store
                    .save_receipt(r_code_kernel::task::OperationReceipt {
                        attempt_id: self.identity.attempt_id.clone(),
                        operation_key: key.clone(),
                        method: method.to_string(),
                        input_hash: hash.clone(),
                        outcome: r_code_kernel::task::ReceiptOutcome::Indeterminate {
                            reason: "operation-in-flight".to_string(),
                        },
                    })
                    .await
                    .map_err(service_error)?;
                let result = execute().await?;
                self.store
                    .save_receipt(r_code_kernel::task::OperationReceipt {
                        attempt_id: self.identity.attempt_id.clone(),
                        operation_key: key,
                        method: method.to_string(),
                        input_hash: hash,
                        outcome: r_code_kernel::task::ReceiptOutcome::Completed {
                            result: result.clone(),
                        },
                    })
                    .await
                    .map_err(service_error)?;
                Ok(result)
            }
            r_code_harness_protocol::ReplayDecision::Reconcile { class: _ }
                if method == "host.tools.call" =>
            {
                let result = execute().await?;
                self.store
                    .save_receipt(r_code_kernel::task::OperationReceipt {
                        attempt_id: self.identity.attempt_id.clone(),
                        operation_key: key,
                        method: method.to_string(),
                        input_hash: hash,
                        outcome: r_code_kernel::task::ReceiptOutcome::Completed {
                            result: result.clone(),
                        },
                    })
                    .await
                    .map_err(service_error)?;
                Ok(result)
            }
            r_code_harness_protocol::ReplayDecision::Reconcile { class } => Err(RpcError {
                code: error_code::PROTOCOL_VIOLATION,
                message: format!(
                    "operation {} is pending/indeterminate ({class:?}); reconciliation required",
                    key.0
                ),
                data: None,
            }),
        }
    }
}

fn request_params(request: &RpcRequest) -> serde_json::Value {
    request.params.clone().unwrap_or(serde_json::Value::Null)
}

/// View a kernel receipt as a protocol operation record for replay decisions.
fn receipt_to_record(
    receipt: r_code_kernel::task::OperationReceipt,
) -> r_code_harness_protocol::OperationRecord {
    use r_code_harness_protocol::OperationState;
    let state = match receipt.outcome {
        r_code_kernel::task::ReceiptOutcome::Completed { result } => {
            OperationState::Completed { result }
        }
        r_code_kernel::task::ReceiptOutcome::Indeterminate { reason } => {
            OperationState::Indeterminate { reason }
        }
        r_code_kernel::task::ReceiptOutcome::Rejected { reason } => {
            OperationState::Rejected { reason }
        }
    };
    r_code_harness_protocol::OperationRecord {
        attempt_id: receipt.attempt_id,
        operation_key: receipt.operation_key,
        method: receipt.method,
        input_hash: receipt.input_hash,
        state,
        generation_recorded: 0,
        generation_completed: None,
    }
}

fn operation_key_of(params: &serde_json::Value) -> Option<OperationKey> {
    params
        .get("operation_key")
        .and_then(|value| value.as_str())
        .map(OperationKey::new)
}

/// Bounded text preview for journal payloads (tool inputs/outputs).
fn preview(value: &str) -> String {
    const MAX: usize = 2000;
    if value.len() <= MAX {
        value.to_string()
    } else {
        let mut cut = MAX;
        while !value.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…[truncated]", &value[..cut])
    }
}

fn tool_input_preview(tool: &str, input: &serde_json::Value) -> String {
    if matches!(tool, "create_file" | "edit" | "apply_patch" | "delete_file") {
        let path = input
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("<invalid>");
        return serde_json::json!({"path": path, "content": "<redacted>"}).to_string();
    }
    preview(&input.to_string())
}

fn split_run_handle(handle: &str) -> Option<(String, String)> {
    let (run, raw) = handle.split_once(':')?;
    Some((run.to_string(), raw.to_string()))
}

fn run_mismatch(message: &str) -> RpcError {
    RpcError {
        code: error_code::RUN_MISMATCH,
        message: message.to_string(),
        data: None,
    }
}

fn protocol_violation(message: &str) -> RpcError {
    RpcError {
        code: error_code::PROTOCOL_VIOLATION,
        message: message.to_string(),
        data: None,
    }
}

fn context_error(error: ContextError) -> RpcError {
    match error {
        ContextError::HistoryFork | ContextError::InvalidTranscript => {
            protocol_violation("canonical transcript validation failed")
        }
        ContextError::DuplicateWriter(_) | ContextError::Io(_) => {
            RpcError::internal("transcript service failed")
        }
    }
}

fn artifact_error(error: ArtifactError) -> RpcError {
    match error {
        ArtifactError::Base64
        | ArtifactError::FrameTooLarge
        | ArtifactError::InvalidReference
        | ArtifactError::InvalidRange => RpcError::invalid_params(error.to_string()),
        ArtifactError::TaskMismatch => run_mismatch("artifact store belongs to another task"),
        ArtifactError::Io(_) | ArtifactError::NotFound => {
            RpcError::internal("artifact service failed")
        }
    }
}

fn stale_error(error: ServiceError) -> RpcError {
    let code = match &error {
        ServiceError::StaleGeneration { .. } | ServiceError::Cancelled => {
            error_code::GENERATION_REVOKED
        }
        _ => error_code::INTERNAL,
    };
    RpcError {
        code,
        message: error.to_string(),
        data: None,
    }
}

fn service_error(error: ServiceError) -> RpcError {
    RpcError {
        code: error_code::INTERNAL,
        message: error.to_string(),
        data: None,
    }
}

fn base64_decode(data: &str) -> Result<Vec<u8>, String> {
    // Minimal base64 decoder sufficient for fixture traffic.
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| e.to_string())
}

/// The wire contract of one bounded process page: contiguous sequences, no
/// stream frame after its EOF, terminal metadata carried only by a final exit
/// frame, and never more bytes than the caller asked for. The managed process
/// service reuses it so a page cannot pass one caller and fail another.
pub fn validate_process_read_page(
    request: &ProcessReadRequest,
    reply: &ProcessReadReply,
) -> Result<(), String> {
    let mut expected = request.cursor;
    let mut decoded_bytes = 0usize;
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let mut saw_exit = false;
    for (index, frame) in reply.frames.iter().enumerate() {
        if frame.sequence() != expected {
            return Err("process read frame sequence has a gap".into());
        }
        expected = expected
            .checked_add(1)
            .ok_or("process read cursor overflow")?;
        match frame {
            ProcessOutputFrame::Data {
                stream,
                data_base64,
                ..
            } => {
                let after_eof = match stream {
                    ProcessOutputStream::Stdout => stdout_eof,
                    ProcessOutputStream::Stderr => stderr_eof,
                };
                if after_eof || saw_exit {
                    return Err("process read data follows a terminal frame".into());
                }
                let bytes = base64_decode(data_base64)
                    .map_err(|_| "process read data is not base64".to_string())?;
                if bytes.is_empty() {
                    return Err("process read data frame is empty".into());
                }
                decoded_bytes = decoded_bytes
                    .checked_add(bytes.len())
                    .ok_or("process read byte count overflow")?;
            }
            ProcessOutputFrame::Eof { stream, .. } => {
                let seen = match stream {
                    ProcessOutputStream::Stdout => &mut stdout_eof,
                    ProcessOutputStream::Stderr => &mut stderr_eof,
                };
                if *seen || saw_exit {
                    return Err("process read contains duplicate or late EOF".into());
                }
                *seen = true;
            }
            ProcessOutputFrame::Exit { exit_code, .. } => {
                if saw_exit || index + 1 != reply.frames.len() || !reply.terminal {
                    return Err("process read exit frame is invalid".into());
                }
                if *exit_code != reply.exit_code {
                    return Err("process read exit metadata disagrees".into());
                }
                saw_exit = true;
            }
        }
    }
    if reply.next_cursor != expected {
        return Err("process read nextCursor does not follow the page".into());
    }
    if reply.terminal && !reply.frames.is_empty() && !saw_exit {
        return Err("non-empty terminal process read lacks a final exit frame".into());
    }
    if decoded_bytes > request.max_bytes as usize {
        return Err("process read page exceeds maxBytes".into());
    }
    if !reply.terminal && reply.exit_code.is_some() {
        return Err("non-terminal process read contains exit metadata".into());
    }
    Ok(())
}

/// Collecting stream sink used by the router's model bridge.
#[derive(Default)]
pub struct RouterStreamSink {
    pub events: Vec<r_code_harness_protocol::StreamEvent>,
}

impl RouterStreamSink {
    /// Fold the stream into a wire assistant turn (text + tool calls).
    pub fn assistant_turn(&self) -> serde_json::Value {
        let mut text = String::new();
        let mut calls: Vec<(String, String, String)> = Vec::new(); // id, name, partial input
        for event in &self.events {
            match &event.payload {
                r_code_harness_protocol::StreamPayload::TextDelta { text: delta } => {
                    text.push_str(delta)
                }
                r_code_harness_protocol::StreamPayload::ToolCallDelta {
                    id,
                    name,
                    partial_input,
                } => match calls.iter_mut().find(|(existing, _, _)| existing == id) {
                    Some((_, _, input)) => input.push_str(partial_input),
                    None => calls.push((id.clone(), name.clone(), partial_input.clone())),
                },
                _ => {}
            }
        }
        let mut blocks = Vec::new();
        if !text.is_empty() {
            blocks.push(serde_json::json!({"type": "text", "text": text}));
        }
        for (id, name, input) in calls {
            let parsed: serde_json::Value =
                serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);
            blocks.push(serde_json::json!({
                "type": "tool-call", "id": id, "name": name, "input": parsed
            }));
        }
        serde_json::json!({ "role": "assistant", "blocks": blocks })
    }
}

#[async_trait::async_trait]
impl r_code_kernel::ports::StreamSink for RouterStreamSink {
    async fn send(
        &mut self,
        event: r_code_harness_protocol::StreamEvent,
    ) -> Result<(), ServiceError> {
        self.events.push(event);
        Ok(())
    }
}

/// Default per-call deadline used by the router for service calls.
pub const SERVICE_CALL_TIMEOUT: Duration = Duration::from_secs(120);
