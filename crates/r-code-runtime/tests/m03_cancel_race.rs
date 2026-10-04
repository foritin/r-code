mod p_gate_support;

use p_gate_support::{compose_with_builtin, profile, stage_native};
use r_code_harness_protocol::services::{ModelStreamRequest, StreamEvent, StreamPayload};
use r_code_kernel::ports::{
    GenerationToken, JournalStore, ModelService, ModelStreamOutcome, ServiceError, StreamSink,
};
use r_code_kernel::task::{ReviewDisposition, TaskExecution, TaskKind};
use r_code_runtime::services::artifacts::sha256_hex;
use r_code_store::v1::V1Store;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

struct BlockingModel {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl ModelService for BlockingModel {
    async fn stream(
        &self,
        _token: GenerationToken,
        _request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.entered.notify_waiters();
        self.release.notified().await;
        sink.send(StreamEvent {
            stream_id: "cancel-race".into(),
            sequence: 1,
            payload: StreamPayload::TextDelta {
                text: "completed".into(),
            },
            done: None,
        })
        .await?;
        sink.send(StreamEvent {
            stream_id: "cancel-race".into(),
            sequence: 2,
            payload: StreamPayload::Finish {
                reason: "end_turn".into(),
                usage: Default::default(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id: "cancel-race".into(),
            finish_reason: Some("done".into()),
            usage: Default::default(),
            reasoning: None,
        })
    }
}

#[tokio::test]
async fn cancel_losing_to_settled_same_attempt_never_truncates_transcript() {
    let temp = tempfile::tempdir().unwrap();
    let profile = profile("m03-cancel-race", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let service = compose_with_builtin(
        &profile,
        &package,
        Arc::new(BlockingModel {
            entered: entered.clone(),
            release: release.clone(),
        }),
    );
    service
        .create_task("cancel-race", "chat", TaskKind::Conversation, vec![])
        .await
        .unwrap();
    service
        .send_message("cancel-race", "must remain canonical")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .expect("model entered");

    let store = V1Store::open(&profile.database_path()).unwrap();
    let (mut settled, revision) = store
        .load_task_with_revision("cancel-race")
        .unwrap()
        .unwrap();
    let attempt_id = match &settled.execution {
        TaskExecution::Running { attempt_id, .. } => attempt_id.clone(),
        other => panic!("expected active attempt, got {other:?}"),
    };
    settled.execution = TaskExecution::ReviewReady { attempt_id };
    settled.review = ReviewDisposition::Pending;
    store
        .save_task_and_events_if_revision(&settled, vec![], revision)
        .unwrap();

    let transcript_path = profile
        .harness_v1_root()
        .join("transcripts")
        .join(format!("{}.jsonl", sha256_hex(b"cancel-race")));
    let before = std::fs::read(&transcript_path).unwrap();
    assert!(!before.is_empty(), "canonical user input was persisted");
    assert!(service.cancel_task("cancel-race").await.unwrap());
    let after = std::fs::read(&transcript_path).unwrap();
    assert_eq!(
        after, before,
        "losing cancel must not truncate settled history"
    );
    assert!(matches!(
        store.load_task("cancel-race").await.unwrap().execution,
        TaskExecution::ReviewReady { .. }
    ));
    assert!(!store
        .task_events("cancel-race")
        .iter()
        .any(|event| event.kind == "run.cancelled"));
    release.notify_waiters();
}

#[tokio::test]
async fn failed_transcript_truncate_preserves_cancel_ownership_for_retry() {
    let temp = tempfile::tempdir().unwrap();
    let profile = profile("m03-cancel-retry", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let service = compose_with_builtin(
        &profile,
        &package,
        Arc::new(BlockingModel {
            entered: entered.clone(),
            release: release.clone(),
        }),
    );
    service
        .create_task("cancel-retry", "chat", TaskKind::Conversation, vec![])
        .await
        .unwrap();
    service
        .send_message("cancel-retry", "retryable transcript")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .expect("model entered");

    let store = V1Store::open(&profile.database_path()).unwrap();
    let transcript_root = profile.harness_v1_root().join("transcripts");
    let saved_root = profile.harness_v1_root().join("transcripts-saved");
    let transcript_name = format!("{}.jsonl", sha256_hex(b"cancel-retry"));
    let original_bytes = std::fs::read(transcript_root.join(&transcript_name)).unwrap();
    std::fs::rename(&transcript_root, &saved_root).unwrap();
    std::fs::write(&transcript_root, b"blocks transcript directory").unwrap();

    assert!(service.cancel_task("cancel-retry").await.is_err());
    assert!(matches!(
        store.load_task("cancel-retry").await.unwrap().execution,
        TaskExecution::Terminal { .. }
    ));
    assert_eq!(
        std::fs::read(saved_root.join(&transcript_name)).unwrap(),
        original_bytes
    );

    std::fs::remove_file(&transcript_root).unwrap();
    std::fs::rename(&saved_root, &transcript_root).unwrap();
    assert!(service.cancel_task("cancel-retry").await.unwrap());
    assert_eq!(
        std::fs::read(transcript_root.join(&transcript_name)).unwrap(),
        b""
    );
    assert_eq!(
        store
            .task_events("cancel-retry")
            .iter()
            .filter(|event| event.kind == "run.cancelled")
            .count(),
        1
    );
    release.notify_waiters();
}
