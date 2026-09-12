//! RA2 — daemon approval decision surface: `approvals.list` returns the
//! pending projection; `approvals.decide` decides with the *connection's*
//! client id as auditor and refuses unknown ops structurally. Decisions
//! ride the daemon's command dedup: a replayed command id returns the
//! first result, a fresh command id with a conflicting decision is
//! refused.

use r_code_harness_protocol::application::ApplicationCommand;
use r_code_kernel::ports::ToolService;
use r_code_kernel::testing::FakeToolService;
use r_code_runtime::application::{
    approval_decision_source_gate, ApplicationService, CommandSource,
};
use r_code_runtime::application_receipts::CommandDedup;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::Path;
use std::sync::Arc;

fn profile_for(name: &str, temp: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.join(name))
            .with_ipc_name(name),
    )
    .expect("profile")
}

/// Thin facade mirroring the daemon's `approvals.*` dispatch (the same
/// ApplicationService methods the ServiceHandler branches to).
struct ApprovalFacade(Arc<ApplicationService>);

#[async_trait::async_trait]
impl ApplicationHandler for ApprovalFacade {
    async fn execute(&self, command: ApplicationCommand) -> Result<serde_json::Value, String> {
        match command.method.as_str() {
            "approvals.list" => Ok(serde_json::json!({
                "pending": self.0.approvals_list().await
            })),
            "approvals.decide" => {
                approval_decision_source_gate(CommandSource::Local)?;
                self.0
                    .approvals_decide(
                        &command.params,
                        &command.client_id,
                        r_code_runtime::application::CommandSource::Local,
                    )
                    .await
                    .map_err(|e| e.to_string())
            }
            other => Err(format!("unknown method {other}")),
        }
    }

    async fn events_after(
        &self,
        after_seq: u64,
        limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        self.0.events_after(after_seq, limit).await
    }
}

fn decide_command(client: &str, command_id: &str, op: &str, decision: &str) -> ApplicationCommand {
    ApplicationCommand {
        client_id: client.into(),
        command_id: command_id.into(),
        method: "approvals.decide".into(),
        params: serde_json::json!({
            "operationId": op,
            "decision": decision,
        }),
    }
}

// -- RA2.A1: list reflects pending, decide clears it and audits ------------

#[tokio::test]
async fn ra2_a1_list_then_decide_with_connection_auditing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("ra2a1", temp.path());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let service = ApplicationService::compose(
        &profile,
        Arc::new(EchoModel) as Arc<dyn r_code_kernel::ports::ModelService>,
        tools,
    )
    .expect("compose");
    service
        .create_task(
            "t1",
            "fixture",
            r_code_kernel::task::TaskKind::Conversation,
            vec![],
        )
        .await
        .expect("task");

    // No pending ops initially.
    assert!(service.approvals_list().await.is_empty());

    // The host registers one pending operation (plugin wiring is RA3; the
    // daemon surface is identical either way).
    service
        .approvals()
        .register("op-a1", "run tests", "run-t1-1", "t1")
        .await;

    let pending = service.approvals_list().await;
    assert_eq!(pending.len(), 1, "list shows exactly the pending op");
    assert_eq!(pending[0]["opId"], "op-a1");
    assert_eq!(pending[0]["summary"], "run tests");
    assert_eq!(pending[0]["runId"], "run-t1-1");
    assert_eq!(pending[0]["taskId"], "t1");
    assert!(pending[0]["createdSeq"].is_u64());
    assert!(pending[0]["ageMs"].is_u64());

    // Decide as the connected client; the auditor is the connection id.
    service
        .approvals_decide(
            &serde_json::json!({"operationId": "op-a1", "decision": "granted"}),
            "client-tui",
            r_code_runtime::application::CommandSource::Local,
        )
        .await
        .expect("decide");
    assert!(
        service.approvals_list().await.is_empty(),
        "decided ops leave the list"
    );

    // The journal carries the decision with the connection's identity.
    let events = service.events_after(0, 500).await;
    let decided = events
        .iter()
        .find(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("approval.decided"))
        })
        .expect("decided event on the wire");
    assert_eq!(decided.payload["decidedBy"], "client-tui");
    assert_eq!(decided.payload["decision"], "granted");
}

// -- Negative surface: unknown op / bad decision / remote gate -------------

#[tokio::test]
async fn ra2_negatives_unknown_op_bad_decision_remote_gate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("ra2neg", temp.path());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let service = ApplicationService::compose(
        &profile,
        Arc::new(EchoModel) as Arc<dyn r_code_kernel::ports::ModelService>,
        tools,
    )
    .expect("compose");

    // Unknown op: structured refusal, never implicitly created.
    let error = service
        .approvals_decide(
            &serde_json::json!({"operationId": "op-nope", "decision": "granted"}),
            "client-tui",
            r_code_runtime::application::CommandSource::Local,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("approval_unknown"),
        "structured error, got: {error}"
    );
    assert!(service.approvals_list().await.is_empty());

    // Non granted|denied decisions are invalid params.
    let error = service
        .approvals_decide(
            &serde_json::json!({"operationId": "op-x", "decision": "maybe"}),
            "client-tui",
            r_code_runtime::application::CommandSource::Local,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("granted"),
        "invalid decision refused: {error}"
    );

    // Remote decisions refuse until R04 (source placeholder gate).
    assert_eq!(
        approval_decision_source_gate(CommandSource::Remote).unwrap_err(),
        "remote_not_enabled: approvals.decide is local-only until the remote listener lands (R04)"
    );
    assert!(approval_decision_source_gate(CommandSource::Local).is_ok());
}

// -- RA2.A2: dedup — replayed id returns the first result ------------------

#[tokio::test]
async fn ra2_a2_command_dedup_replays_and_conflicts_are_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("ra2a2", temp.path());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let service = Arc::new(
        ApplicationService::compose(
            &profile,
            Arc::new(EchoModel) as Arc<dyn r_code_kernel::ports::ModelService>,
            tools,
        )
        .expect("compose"),
    );
    service
        .create_task(
            "t1",
            "fixture",
            r_code_kernel::task::TaskKind::Conversation,
            vec![],
        )
        .await
        .expect("task");
    service
        .approvals()
        .register("op-a2", "deploy", "run-t1-1", "t1")
        .await;

    let store = Arc::new(r_code_store::v2::V2Store::open(&profile.database_path()).expect("store"));
    let handler = CommandDedup::new(
        &profile.profile_id(),
        store,
        Arc::new(ApprovalFacade(service.clone())),
    );

    // First decision grants.
    let first = handler
        .execute(decide_command("client-a", "cmd-1", "op-a2", "granted"))
        .await
        .expect("first decide");
    assert_eq!(first["decision"], "granted");
    assert_eq!(first["decidedBy"], "client-a");

    // Same (client_id, command_id) with the SAME payload replays the
    // recorded result — the store is never re-executed.
    let replay = handler
        .execute(decide_command("client-a", "cmd-1", "op-a2", "granted"))
        .await
        .expect("replay resolves");
    assert_eq!(
        replay["decision"], "granted",
        "receipt replay, not re-execution"
    );

    // Reusing a command id with a DIFFERENT payload is the dedup's own
    // conflict (F7: a command id binds to one canonical request).
    let payload_swap = handler
        .execute(decide_command("client-a", "cmd-1", "op-a2", "denied"))
        .await
        .expect_err("payload swap on the same command id");
    assert!(
        payload_swap.contains("different payload"),
        "got: {payload_swap}"
    );

    // A *fresh* command id deciding the same op differently is refused by
    // the store (first decision wins).
    let conflict = handler
        .execute(decide_command("client-b", "cmd-2", "op-a2", "denied"))
        .await
        .expect_err("conflicting decision refused");
    assert!(conflict.contains("approval_conflict"), "got: {conflict}");

    // Same fresh-shape command replaying the SAME decision returns the
    // original record (idempotent, original auditor).
    let replay_same = handler
        .execute(decide_command("client-b", "cmd-3", "op-a2", "granted"))
        .await
        .expect("same decision replays");
    assert_eq!(
        replay_same["decidedBy"], "client-a",
        "original decision record"
    );
}

// -- Minimal model service for compose (no model calls happen here) --------

struct EchoModel;

#[async_trait::async_trait]
impl r_code_kernel::ports::ModelService for EchoModel {
    async fn stream(
        &self,
        _token: r_code_kernel::ports::GenerationToken,
        _request: r_code_harness_protocol::ModelStreamRequest,
        _sink: &mut dyn r_code_kernel::ports::StreamSink,
    ) -> Result<r_code_kernel::ports::ModelStreamOutcome, r_code_kernel::ports::ServiceError> {
        unreachable!("no model calls in the RA2 surface")
    }
}
