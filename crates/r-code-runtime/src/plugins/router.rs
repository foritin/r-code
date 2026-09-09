//! Host RPC router: maps plugin calls onto kernel service ports.
//!
//! Every inbound plugin request is checked in this order, all *before* any
//! effectful service runs:
//! 1. the method must exist in the protocol (fail closed);
//! 2. the method's host service must be part of the negotiated grants;
//! 3. the run generation must still be live (late callbacks rejected);
//! 4. run-scoped handles must belong to this run;
//! 5. effectful calls deduplicate through attempt-stable operation keys.

use r_code_harness_protocol::rpc::{error_code, RpcError, RpcNotification, RpcRequest};
use r_code_harness_protocol::services::*;
use r_code_harness_protocol::{
    ApprovalDecision, ApprovalsRequest, HostService, OperationKey, RunIdentity,
};
use r_code_kernel::children::ChildrenSupervisor;
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ProcessService, RunGuard, ServiceError,
    ToolService,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

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

/// Host-side registry of pending operations that approvals may reference.
/// Only the host creates entries; generic questions can never mint one.
#[derive(Default)]
pub struct ApprovalRegistry {
    decisions: Mutex<HashMap<String, ApprovalDecision>>,
}

impl ApprovalRegistry {
    /// Host records a pending operation and (later) its decision.
    pub fn set_decision(&self, operation_id: &str, decision: ApprovalDecision) {
        self.decisions
            .lock()
            .expect("approval registry")
            .insert(operation_id.to_string(), decision);
    }

    fn decide(&self, operation_id: &str) -> Option<ApprovalDecision> {
        self.decisions
            .lock()
            .expect("approval registry")
            .get(operation_id)
            .copied()
    }
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
    pub approvals: Arc<ApprovalRegistry>,
    /// Observation stream for tests and the daemon event fan-out.
    pub observed_events: Mutex<Vec<RpcNotification>>,
    /// Host-side observation tap: `(kind, payload)` pairs the run manager
    /// drains and persists to the journal as host-provenance events
    /// (assistant turns, tool calls, model usage).
    pub host_observations: Mutex<Vec<(String, serde_json::Value)>>,
    /// Completion proposals recorded from the plugin (arbitrated in T20).
    pub recorded_proposals: Mutex<Vec<CompletionProposalRequest>>,
    /// Child supervision for host.children (shared with the daemon).
    pub children: Option<Arc<Mutex<ChildrenSupervisor>>>,
    /// The parent's permission ceiling bounding spawned children.
    pub parent_ceiling: PermissionCeiling,
    checkpoint_revision: std::sync::atomic::AtomicU64,
}

/// Which host service a wire method requires.
pub fn service_for_method(method: &str) -> Option<HostService> {
    Some(match method {
        "host.model.stream" => HostService::ModelStream,
        "host.tools.list" => HostService::ToolsList,
        "host.tools.call" => HostService::ToolsCall,
        "host.process.open" => HostService::ProcessOpen,
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
            store,
            questions,
            approvals: Arc::new(ApprovalRegistry::default()),
            observed_events: Mutex::new(Vec::new()),
            host_observations: Mutex::new(Vec::new()),
            recorded_proposals: Mutex::new(Vec::new()),
            children: None,
            parent_ceiling: PermissionCeiling::Full,
            checkpoint_revision: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Attach child supervision (the daemon shares one supervisor).
    pub fn with_children(
        mut self,
        supervisor: Arc<Mutex<ChildrenSupervisor>>,
        parent_ceiling: PermissionCeiling,
    ) -> Self {
        self.children = Some(supervisor);
        self.parent_ceiling = parent_ceiling;
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

    /// Handle one plugin request. All gates run before any effect.
    pub async fn handle_request(&self, request: RpcRequest) -> Result<serde_json::Value, RpcError> {
        // 1. Known method?
        if let Some(error) = r_code_harness_protocol::rpc::reject_unknown_method(&request.method) {
            return Err(error);
        }
        // 2. Granted service?
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
        // 3. Live generation?
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
                let operation_key = operation_key_of(&params);
                self.deduplicated(operation_key, "host.tools.call", &params, || {
                    Box::pin(async move {
                        let name = call.tool.clone();
                        let input_preview = preview(&call.input.to_string());
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
                // Questions persist before suspension (persistence wiring is
                // T21); the id is host-generated either way.
                let question_id = format!("q-{}-{}", self.identity.run_id, question_counter_next());
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
                // an unknown reference is denied, never auto-created.
                let decision = self
                    .approvals
                    .decide(&approval.pending_operation.operation_id)
                    .unwrap_or(ApprovalDecision::Denied);
                Ok(serde_json::to_value(ApprovalsReply { decision }).unwrap_or_default())
            }
            "host.children.spawn" => {
                let spawn_request: ChildrenSpawnRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let supervisor = self.children.as_ref().ok_or_else(|| {
                    RpcError::internal("children supervision not configured for this run")
                })?;
                let child_task_id = supervisor
                    .lock()
                    .expect("children")
                    .spawn(self.parent_ceiling, &spawn_request)
                    .map_err(|e| RpcError::internal(e.to_string()))?;
                Ok(serde_json::to_value(ChildrenSpawnReply {
                    child_task_id: child_task_id.clone(),
                    child_run_id: child_task_id,
                })
                .unwrap_or_default())
            }
            "host.children.wait" => {
                let wait: ChildrenWaitRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let supervisor = self.children.as_ref().ok_or_else(|| {
                    RpcError::internal("children supervision not configured for this run")
                })?;
                let supervisor = supervisor.lock().expect("children");
                match supervisor.child(&wait.child_task_id) {
                    None => Err(RpcError::internal(format!(
                        "unknown child {}",
                        wait.child_task_id
                    ))),
                    Some(child) => match &child.state {
                        r_code_kernel::children::ChildState::Completed(report) => {
                            Ok(serde_json::to_value(report).unwrap_or_default())
                        }
                        _ => Err(r_code_harness_protocol::rpc::RpcError {
                            code: error_code::PROTOCOL_VIOLATION,
                            message: format!("child {} has not completed yet", wait.child_task_id),
                            data: None,
                        }),
                    },
                }
            }
            "host.children.cancel" => {
                let cancel: ChildrenCancelRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let supervisor = self.children.as_ref().ok_or_else(|| {
                    RpcError::internal("children supervision not configured for this run")
                })?;
                supervisor
                    .lock()
                    .expect("children")
                    .cancel_child(&cancel.child_task_id)
                    .map_err(|e| RpcError::internal(e.to_string()))?;
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
                        outcome: r_code_kernel::task::ReceiptOutcome::Completed {
                            result: serde_json::Value::Null,
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

fn question_counter_next() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::SeqCst)
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
