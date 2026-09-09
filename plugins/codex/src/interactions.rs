//! Codex interactions: approvals, questions, steer, progress and
//! completion mapping (T30).

use r_code_harness_protocol::services::{ProposalKind, QuestionsAskRequest};
use r_code_harness_protocol::EventKind;
use r_code_harness_sdk::{SdkError, SdkHandle};
use std::sync::Arc;
use tokio::sync::Mutex;

/// The interaction surface of one Codex run.
pub struct CodexInteractions {
    thread_id: Mutex<Option<String>>,
}

impl CodexInteractions {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            thread_id: Mutex::new(None),
        })
    }

    /// Record the App Server thread id (external thread reference kept in
    /// checkpoints for resume).
    pub async fn set_thread(&self, thread_id: String) {
        *self.thread_id.lock().await = Some(thread_id);
    }

    pub async fn thread_id(&self) -> Option<String> {
        self.thread_id.lock().await.clone()
    }

    /// Map an App Server approval request to a host approval citation.
    /// Only host-created pending operations are approvable — an unknown
    /// reference surfaces as denied (fail closed).
    pub async fn request_approval(
        &self,
        handle: &SdkHandle,
        operation_id: &str,
        input_hash: &str,
        summary: &str,
    ) -> Result<r_code_harness_protocol::services::ApprovalDecision, SdkError> {
        handle
            .request_approval(
                r_code_harness_protocol::PendingOperationRef {
                    operation_id: operation_id.into(),
                    input_hash: input_hash.into(),
                },
                summary,
            )
            .await
    }

    /// Map an App Server question to a durable host question.
    pub async fn ask(&self, handle: &SdkHandle, text: &str) -> Result<String, SdkError> {
        handle
            .ask_question(text, true)
            .await
            .map_err(|error| SdkError::Rpc {
                code: -32000,
                message: error.to_string(),
            })
    }

    /// Propose completion for host verification; unsupported external
    /// capabilities are advertised as unavailable, never claimed.
    pub async fn propose(
        &self,
        handle: &SdkHandle,
        summary: &str,
        candidate_digest: Option<String>,
    ) -> Result<r_code_harness_protocol::services::CompletionProposalReply, SdkError> {
        let reply = handle
            .propose_completion(
                r_code_harness_protocol::services::CompletionProposalRequest {
                    kind: ProposalKind::Implementation,
                    summary: summary.into(),
                    candidate_digest,
                    work_unit_statuses: vec![],
                },
            )
            .await?;
        handle
            .emit_event(
                EventKind::CompletionProposed,
                serde_json::json!({"accepted": reply.accepted, "externalControlLimits": true}),
            )
            .await?;
        Ok(reply)
    }

    /// Persist a question-like elicit payload through the durable path.
    pub fn question_payload(&self, text: &str) -> QuestionsAskRequest {
        QuestionsAskRequest {
            text: text.into(),
            options: vec![],
            blocking: true,
        }
    }
}
