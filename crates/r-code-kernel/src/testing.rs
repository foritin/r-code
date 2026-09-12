//! In-memory fakes implementing the kernel ports. Test-only; production
//! adapters live in the runtime crate.

use crate::ports::*;
use crate::task::{OperationReceipt, TaskState};
use async_trait::async_trait;
use r_code_harness_protocol::{
    ArtifactRef, InputMessage, ModelStreamRequest, OperationKey, StreamEvent, StreamPayload,
    ToolCallReply, ToolCallRequest, ToolDescriptor,
};
use std::collections::HashMap;
use std::sync::Mutex;

/// In-memory journal with atomic aggregate+event saves.
#[derive(Default)]
pub struct MemoryJournal {
    inner: Mutex<MemoryJournalInner>,
}

struct MemoryJournalInner {
    tasks: HashMap<String, TaskState>,
    events: Vec<JournalEvent>,
    next_seq: u64,
    receipts: HashMap<(String, String), OperationReceipt>,
    checkpoints: HashMap<String, Vec<CheckpointRecord>>,
}

impl Default for MemoryJournalInner {
    fn default() -> Self {
        Self {
            tasks: HashMap::new(),
            events: Vec::new(),
            // Sequences start at 1 so `events.read(after_seq=0)` replays the
            // full journal.
            next_seq: 1,
            receipts: HashMap::new(),
            checkpoints: HashMap::new(),
        }
    }
}

impl MemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of events committed so far.
    pub fn event_count(&self) -> usize {
        self.inner.lock().unwrap().events.len()
    }
}

#[async_trait]
impl JournalStore for MemoryJournal {
    async fn load_task(&self, task_id: &str) -> Option<TaskState> {
        self.inner.lock().unwrap().tasks.get(task_id).cloned()
    }

    async fn save_task_and_events(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
    ) -> Result<(), ServiceError> {
        let mut inner = self.inner.lock().unwrap();
        let mut seq = inner.next_seq;
        let mut stamped = Vec::with_capacity(events.len());
        for mut event in events {
            event.seq = seq;
            seq += 1;
            stamped.push(event);
        }
        inner.next_seq = seq;
        inner.events.extend(stamped);
        inner
            .tasks
            .insert(task.contract.task_id.clone(), task.clone());
        Ok(())
    }

    async fn read_events(&self, after_seq: u64, limit: u32) -> Vec<JournalEvent> {
        self.inner
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| event.seq > after_seq)
            .take(limit as usize)
            .cloned()
            .collect()
    }

    async fn max_event_seq(&self) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .events
            .last()
            .map(|event| event.seq)
            .unwrap_or(0)
    }

    async fn save_receipt(&self, receipt: OperationReceipt) -> Result<(), ServiceError> {
        self.inner.lock().unwrap().receipts.insert(
            (receipt.attempt_id.clone(), receipt.operation_key.0.clone()),
            receipt,
        );
        Ok(())
    }

    async fn load_receipt(&self, attempt_id: &str, key: &OperationKey) -> Option<OperationReceipt> {
        self.inner
            .lock()
            .unwrap()
            .receipts
            .get(&(attempt_id.to_string(), key.0.clone()))
            .cloned()
    }

    async fn save_checkpoint(
        &self,
        attempt_id: &str,
        revision: u64,
        state: Vec<u8>,
        consumed_input_seq: u64,
    ) -> Result<ArtifactRef, ServiceError> {
        let artifact = ArtifactRef {
            schema: ArtifactRef::SCHEMA,
            blob_id: format!("blob:ckpt:{attempt_id}:{revision}"),
            bytes: state.len() as u64,
            sha256: format!("ckpt-{revision}"),
            media_type: Some("application/octet-stream".into()),
        };
        let record = CheckpointRecord {
            attempt_id: attempt_id.to_string(),
            revision,
            state,
            consumed_input_seq,
            artifact: artifact.clone(),
        };
        self.inner
            .lock()
            .unwrap()
            .checkpoints
            .entry(attempt_id.to_string())
            .or_default()
            .push(record);
        Ok(artifact)
    }

    async fn load_latest_checkpoint(&self, attempt_id: &str) -> Option<CheckpointRecord> {
        self.inner
            .lock()
            .unwrap()
            .checkpoints
            .get(attempt_id)
            .and_then(|list| list.last())
            .cloned()
    }
}

/// Collects stream events for assertions.
pub struct CollectSink {
    pub events: Vec<StreamEvent>,
}

impl CollectSink {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }
}

impl Default for CollectSink {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StreamSink for CollectSink {
    async fn send(&mut self, event: StreamEvent) -> Result<(), ServiceError> {
        self.events.push(event);
        Ok(())
    }
}

/// Scripted model service: emits one text delta per message and finishes.
#[derive(Default)]
pub struct FakeModelService {
    pub calls: Mutex<Vec<String>>,
}

#[async_trait]
impl ModelService for FakeModelService {
    async fn stream(
        &self,
        token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{}:{}", token.run_id, token.generation));
        let stream_id = format!("stream-{}", self.calls.lock().unwrap().len());
        for sequence in 0..request.messages.len() as u64 {
            sink.send(StreamEvent {
                stream_id: stream_id.clone(),
                sequence,
                payload: StreamPayload::TextDelta {
                    text: "delta".into(),
                },
                done: None,
            })
            .await?;
        }
        sink.send(StreamEvent {
            stream_id: stream_id.clone(),
            sequence: request.messages.len() as u64,
            payload: StreamPayload::Finish {
                reason: "stop".into(),
                usage: Default::default(),
            },
            done: Some(true),
        })
        .await?;
        Ok(ModelStreamOutcome {
            stream_id,
            finish_reason: Some("stop".into()),
            usage: Default::default(),
        })
    }
}

/// Scripted tool service recording authorized calls.
#[derive(Default)]
pub struct FakeToolService {
    pub calls: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl ToolService for FakeToolService {
    async fn list(&self, _token: GenerationToken) -> Result<Vec<ToolDescriptor>, ServiceError> {
        Ok(vec![ToolDescriptor {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }])
    }

    async fn call(
        &self,
        token: GenerationToken,
        call: ToolCallRequest,
    ) -> Result<ToolCallReply, ServiceError> {
        self.calls.lock().unwrap().push((
            format!("{}:{}", token.run_id, token.generation),
            call.tool.clone(),
        ));
        Ok(ToolCallReply {
            output: vec![r_code_harness_protocol::OutputBlock::Text { text: "ok".into() }],
            error: None,
        })
    }
}

/// In-memory process table with run-scoped handles.
#[derive(Default)]
pub struct FakeProcessService {
    pub open_count: Mutex<u64>,
    pub handles: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl ProcessService for FakeProcessService {
    async fn open(
        &self,
        token: GenerationToken,
        profile: &str,
        _arguments: Vec<String>,
        _cwd: Option<String>,
    ) -> Result<String, ServiceError> {
        let mut count = self.open_count.lock().unwrap();
        *count += 1;
        let handle = format!("handle-{}-{}", token.generation, *count);
        self.handles
            .lock()
            .unwrap()
            .insert(handle.clone(), profile.to_string());
        Ok(handle)
    }

    async fn write(
        &self,
        _token: GenerationToken,
        handle: &str,
        _data: Vec<u8>,
    ) -> Result<(), ServiceError> {
        match self.handles.lock().unwrap().get(handle) {
            Some(_) => Ok(()),
            None => Err(ServiceError::Failure("unknown handle".into())),
        }
    }

    async fn close(
        &self,
        _token: GenerationToken,
        handle: &str,
    ) -> Result<Option<i32>, ServiceError> {
        match self.handles.lock().unwrap().remove(handle) {
            Some(_) => Ok(Some(0)),
            None => Err(ServiceError::Failure("unknown handle".into())),
        }
    }
}

/// Scripted harness session recording lifecycle calls.
#[derive(Default)]
pub struct FakeHarnessSession {
    pub started: Mutex<Vec<String>>,
    pub resumed: Mutex<Vec<String>>,
    pub steered: Mutex<Vec<String>>,
    pub cancelled: Mutex<Vec<String>>,
}

#[async_trait]
impl HarnessSession for FakeHarnessSession {
    async fn start(
        &self,
        attempt: &crate::task::Attempt,
        _contract: &crate::task::TaskContract,
        first_input: &InputMessage,
    ) -> Result<(), ServiceError> {
        self.started
            .lock()
            .unwrap()
            .push(format!("{}:{}", attempt.attempt_id, first_input.message_id));
        Ok(())
    }

    async fn resume(
        &self,
        attempt: &crate::task::Attempt,
        checkpoint: &ArtifactRef,
        replay_inputs: &[InputMessage],
    ) -> Result<(), ServiceError> {
        self.resumed.lock().unwrap().push(format!(
            "{}:{}:{}",
            attempt.attempt_id,
            checkpoint.blob_id,
            replay_inputs.len()
        ));
        Ok(())
    }

    async fn steer(&self, input: &InputMessage) -> Result<(), ServiceError> {
        self.steered.lock().unwrap().push(input.message_id.clone());
        Ok(())
    }

    async fn cancel(&self, reason: &str) -> Result<(), ServiceError> {
        self.cancelled.lock().unwrap().push(reason.to_string());
        Ok(())
    }
}
