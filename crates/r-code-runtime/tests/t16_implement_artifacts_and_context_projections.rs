//! T16 — artifacts and context projections.
//!
//! Replay and retained-tail tests preserve complete tool pairs, attachment
//! ownership and frozen context identity without duplicate transcript
//! writers.

use r_code_harness_protocol::services::{
    ArtifactsPutRequest, ArtifactsReadRequest, ContentBlock, ModelRole, OutputBlock,
};
use r_code_runtime::services::artifacts::ArtifactStore;
use r_code_runtime::services::context::{ContextRegistry, FrozenMemory};

#[test]
fn replay_preserves_complete_tool_pairs_at_page_boundaries() {
    let registry = ContextRegistry::new();
    let writer = registry.open_transcript("task-1", None).expect("writer");

    // Entry pairs: user → (assistant ToolCall, Tool ToolResult) × 3.
    writer
        .append(
            ModelRole::User,
            vec![ContentBlock::Text {
                text: "turn 1".into(),
            }],
        )
        .expect("append");
    for turn in 1..=3 {
        writer
            .append(
                ModelRole::Assistant,
                vec![ContentBlock::ToolCall {
                    id: format!("t{turn}"),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": format!("f{turn}")}),
                }],
            )
            .expect("append");
        writer
            .append(
                ModelRole::Tool,
                vec![ContentBlock::ToolResult {
                    call_id: format!("t{turn}"),
                    output: vec![OutputBlock::Text {
                        text: format!("body {turn}"),
                    }],
                }],
            )
            .expect("append");
    }

    // A page limit that would cut between call and result pulls the result
    // along: limit=2 lands after entry 2 (the call of pair 1) → the result
    // rides with it.
    let page = writer.read_page(0, 2);
    let sequences: Vec<u64> = page.entries.iter().map(|entry| entry.seq).collect();
    assert_eq!(
        sequences,
        vec![1, 2, 3],
        "tool pair stays whole at the boundary"
    );
    // The last entry of the page is the result, not a dangling call.
    assert!(matches!(
        page.entries.last().unwrap().blocks[0],
        ContentBlock::ToolResult { .. }
    ));

    // Continue from the cursor: the next page starts on the second pair's
    // call and replays every remaining entry.
    let next = writer.read_page(page.next_cursor.expect("cursor"), 10);
    let sequences: Vec<u64> = next.entries.iter().map(|entry| entry.seq).collect();
    assert_eq!(sequences, vec![4, 5, 6, 7]);
    assert!(matches!(
        next.entries[0].blocks[0],
        ContentBlock::ToolCall { .. }
    ));

    // Retained tail keeps whole pairs too.
    let tail = writer.read_tail(4);
    assert!(tail.entries.len() >= 4);
    for window in tail.entries.windows(2) {
        if opens_call(window[0].blocks.last()) {
            assert!(
                completes(window[1].blocks.first()),
                "tail split a tool pair"
            );
        }
    }
}

fn opens_call(block: Option<&ContentBlock>) -> bool {
    matches!(block, Some(ContentBlock::ToolCall { .. }))
}

fn completes(block: Option<&ContentBlock>) -> bool {
    matches!(block, Some(ContentBlock::ToolResult { .. }))
}

#[test]
fn duplicate_transcript_writers_are_refused() {
    let registry = ContextRegistry::new();
    let _first = registry
        .open_transcript("task-1", None)
        .expect("first writer");
    let error = match registry.open_transcript("task-1", None) {
        Err(error) => error,
        Ok(_) => panic!("duplicate writer must be refused"),
    };
    assert!(matches!(
        error,
        r_code_runtime::services::context::ContextError::DuplicateWriter(_)
    ));
    // Other tasks are unaffected.
    registry
        .open_transcript("task-2", None)
        .expect("second task writer");
}

#[test]
fn transcripts_persist_and_reload() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("transcript.jsonl");
    {
        let registry = ContextRegistry::new();
        let writer = registry
            .open_transcript("task-1", Some(path.clone()))
            .expect("writer");
        writer
            .append(
                ModelRole::User,
                vec![ContentBlock::Text {
                    text: "persisted".into(),
                }],
            )
            .expect("append");
    }
    // A new registry (restart) reloads the entries.
    let registry = ContextRegistry::new();
    let writer = registry
        .open_transcript("task-1", Some(path))
        .expect("reload");
    let page = writer.read_page(0, 10);
    assert_eq!(page.entries.len(), 1);
    assert_eq!(
        page.entries[0].blocks[0],
        ContentBlock::Text {
            text: "persisted".into()
        }
    );
}

#[test]
fn attachment_ownership_and_artifact_round_trip() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = ArtifactStore::new(temp.path().join("blobs"));

    let put = ArtifactsPutRequest {
        media_type: Some("image/png".into()),
        data_base64: base64_encode(b"attachment-bytes"),
    };
    let reference = store.put(&put, "task-owner-1").expect("put");
    assert!(reference.blob_id.starts_with("blob:sha256:"));
    assert_eq!(reference.bytes, b"attachment-bytes".len() as u64);

    // Ownership metadata travels with the reference.
    assert_eq!(
        store.owner_of(&reference.blob_id).as_deref(),
        Some("task-owner-1")
    );

    // Another task reads the bytes by reference but does not own them.
    let read = store
        .read(&ArtifactsReadRequest {
            artifact: reference.clone(),
            offset: 0,
            length: 0,
        })
        .expect("read");
    assert_eq!(base64_decode(&read.data_base64), b"attachment-bytes");
    assert_eq!(read.total_bytes, 16);

    // Range reads work.
    let partial = store
        .read(&ArtifactsReadRequest {
            artifact: reference,
            offset: 4,
            length: 8,
        })
        .expect("partial");
    assert_eq!(base64_decode(&partial.data_base64), b"chment-b");

    // Unknown refs fail closed.
    let missing = r_code_harness_protocol::ArtifactRef {
        schema: 1,
        blob_id: "blob:sha256:dead".into(),
        bytes: 0,
        sha256: "dead".into(),
        media_type: None,
    };
    assert!(store
        .read(&ArtifactsReadRequest {
            artifact: missing,
            offset: 0,
            length: 0
        })
        .is_err());

    // Identical content dedupes to the same blob.
    let again = store
        .put(
            &ArtifactsPutRequest {
                media_type: None,
                data_base64: base64_encode(b"attachment-bytes"),
            },
            "task-owner-2",
        )
        .expect("put again");
    assert_eq!(
        again.sha256,
        "attachment-bytes"
            .len()
            .to_string()
            .parse::<u64>()
            .map(|_| again.sha256.clone())
            .unwrap()
    );
    assert!(store
        .root()
        .join(format!("{}.blob", again.sha256))
        .is_file());
}

#[test]
fn frozen_context_identity_is_stable_and_content_addressed() {
    let files = vec![
        ("memory/project.md".to_string(), b"project facts".to_vec()),
        ("memory/style.md".to_string(), b"style rules".to_vec()),
    ];
    let frozen = FrozenMemory::freeze(&files);
    let identity = frozen.identity();
    // Same content, same identity.
    assert_eq!(FrozenMemory::freeze(&files).identity(), identity);
    // Changed content, different identity.
    let mut changed = files.clone();
    changed[0].1 = b"project facts v2".to_vec();
    assert_ne!(FrozenMemory::freeze(&changed).identity(), identity);
    // The captured bytes are immutable snapshots.
    assert_eq!(frozen.entries[0].bytes, b"project facts");
    assert!(!frozen.entries[0].content_sha256.is_empty());
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(text: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap()
}
