//! Canonical transcript and context projections.
//!
//! The host owns the canonical transcript; harnesses select context
//! strategy. Reads replay by cursor and never split a tool call from its
//! result; attachment ownership and frozen-memory identity are part of the
//! context projection. Exactly one transcript writer per task is admitted —
//! duplicate writers are refused instead of racing.

use crate::services::artifacts::sha256_hex;
use r_code_harness_protocol::services::{ContentBlock, ContextPage, ModelMessage, TranscriptEntry};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The largest transcript page exposed by `host.context.read`.
pub const MAX_TRANSCRIPT_PAGE_LIMIT: u32 = 256;

/// Errors from the context service.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextError {
    #[error("a writer already owns the transcript for task {0}")]
    DuplicateWriter(String),
    #[error("the persisted transcript is invalid")]
    InvalidTranscript,
    #[error("the model request diverges from the canonical transcript")]
    HistoryFork,
    #[error("io failure: {0}")]
    Io(String),
}

/// The canonical transcript store for one task (host-owned).
pub struct TranscriptWriter {
    task_id: String,
    state: Mutex<TranscriptState>,
    path: Option<PathBuf>,
}

struct TranscriptState {
    entries: Vec<TranscriptEntry>,
    #[allow(dead_code)]
    active_writers: usize,
}

/// Registry admitting at most one writer per task.
#[derive(Default)]
pub struct ContextRegistry {
    writers: Mutex<std::collections::HashSet<String>>,
}

impl ContextRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the (single) transcript writer for a task.
    pub fn open_transcript(
        &self,
        task_id: &str,
        path: Option<PathBuf>,
    ) -> Result<TranscriptWriter, ContextError> {
        let mut writers = self.writers.lock().expect("writers");
        if writers.contains(task_id) {
            return Err(ContextError::DuplicateWriter(task_id.to_string()));
        }
        let entries = load_entries(path.as_deref())?;
        writers.insert(task_id.to_string());
        Ok(TranscriptWriter {
            task_id: task_id.to_string(),
            state: Mutex::new(TranscriptState {
                entries,
                active_writers: 1,
            }),
            path,
        })
    }
}

impl TranscriptWriter {
    /// Append one entry; the next dense sequence is assigned host-side.
    pub fn append(
        &self,
        role: r_code_harness_protocol::services::ModelRole,
        blocks: Vec<ContentBlock>,
    ) -> Result<u64, ContextError> {
        let mut state = self.state.lock().expect("transcript");
        let seq = next_sequence(&state.entries)?;
        let mut entries = state.entries.clone();
        entries.push(TranscriptEntry { seq, role, blocks });
        self.persist(&entries)?;
        state.entries = entries;
        Ok(seq)
    }

    /// Synchronize a provider request with the host-owned transcript.
    ///
    /// A request may replay the exact canonical prefix and append new
    /// messages. It may not truncate, rewrite or branch history.
    pub fn sync_messages(&self, messages: &[ModelMessage]) -> Result<(), ContextError> {
        let mut state = self.state.lock().expect("transcript");
        if messages.len() < state.entries.len()
            || state.entries.iter().zip(messages).any(|(entry, message)| {
                entry.role != message.role || entry.blocks != message.content
            })
        {
            return Err(ContextError::HistoryFork);
        }

        let mut entries = state.entries.clone();
        for message in &messages[entries.len()..] {
            let seq = next_sequence(&entries)?;
            entries.push(TranscriptEntry {
                seq,
                role: message.role,
                blocks: message.content.clone(),
            });
        }
        if entries.len() != state.entries.len() {
            self.persist(&entries)?;
            state.entries = entries;
        }
        Ok(())
    }

    /// Current durable prefix length, used as a run rollback point.
    pub fn position(&self) -> u64 {
        self.state.lock().expect("transcript").entries.len() as u64
    }

    /// Restore a previously observed prefix after a cancelled/failed run.
    pub fn truncate_to(&self, position: u64) -> Result<(), ContextError> {
        let mut state = self.state.lock().expect("transcript");
        let position = usize::try_from(position).map_err(|_| ContextError::InvalidTranscript)?;
        if position > state.entries.len() {
            return Err(ContextError::InvalidTranscript);
        }
        if position == state.entries.len() {
            return Ok(());
        }
        let entries = state.entries[..position].to_vec();
        self.persist(&entries)?;
        state.entries = entries;
        Ok(())
    }

    fn persist(&self, entries: &[TranscriptEntry]) -> Result<(), ContextError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ContextError::Io(e.to_string()))?;
        }
        let mut text = String::new();
        for entry in entries {
            let row = serde_json::to_string(entry)
                .map_err(|error| ContextError::Io(error.to_string()))?;
            text.push_str(&row);
            text.push('\n');
        }
        let temporary = path.with_extension("jsonl.tmp");
        std::fs::write(&temporary, text).map_err(|e| ContextError::Io(e.to_string()))?;
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| ContextError::Io(e.to_string()))?;
        }
        std::fs::rename(&temporary, path).map_err(|e| ContextError::Io(e.to_string()))?;
        Ok(())
    }

    /// Replay from a cursor. The page never splits a ToolCall from its
    /// ToolResult: if the limit would cut between them, the result travels
    /// with its call.
    pub fn read_page(&self, cursor: u64, limit: u32) -> ContextPage {
        let state = self.state.lock().expect("transcript");
        let limit = limit.clamp(1, MAX_TRANSCRIPT_PAGE_LIMIT);
        let entries: VecDeque<TranscriptEntry> = state
            .entries
            .iter()
            .filter(|entry| entry.seq > cursor)
            .cloned()
            .collect();
        let mut page: Vec<TranscriptEntry> = Vec::new();
        for entry in entries {
            if page.len() >= limit as usize {
                // Boundary check: the last appended entry opens a tool pair
                // whose result would be severed — pull the result along.
                if opens_tool_pair(page.last()) {
                    if let Some(restored) = state.entries.iter().find(|candidate| {
                        candidate.seq == page.last().expect("non-empty").seq + 1
                            && completes_tool_pair(candidate)
                    }) {
                        page.push(restored.clone());
                    }
                }
                break;
            }
            page.push(entry);
        }
        let next_cursor = page.last().map(|entry| entry.seq);
        ContextPage {
            entries: page,
            next_cursor,
        }
    }

    /// The newest complete tail (retained-tail projection).
    pub fn read_tail(&self, limit: u32) -> ContextPage {
        let state = self.state.lock().expect("transcript");
        let limit = limit.clamp(1, MAX_TRANSCRIPT_PAGE_LIMIT);
        let total = state.entries.len() as u64;
        let cursor = total.saturating_sub(limit as u64);
        let page: Vec<TranscriptEntry> = state
            .entries
            .iter()
            .filter(|entry| entry.seq > cursor)
            .cloned()
            .collect();
        let next_cursor = page.last().map(|entry| entry.seq);
        ContextPage {
            entries: page,
            next_cursor,
        }
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }
}

fn load_entries(path: Option<&Path>) -> Result<Vec<TranscriptEntry>, ContextError> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(ContextError::Io(error.to_string())),
    };
    let mut entries = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let entry = serde_json::from_str::<TranscriptEntry>(line)
            .map_err(|_| ContextError::InvalidTranscript)?;
        let expected = entries.len() as u64 + 1;
        if entry.seq != expected {
            return Err(ContextError::InvalidTranscript);
        }
        entries.push(entry);
    }
    Ok(entries)
}

fn next_sequence(entries: &[TranscriptEntry]) -> Result<u64, ContextError> {
    u64::try_from(entries.len())
        .ok()
        .and_then(|len| len.checked_add(1))
        .ok_or(ContextError::InvalidTranscript)
}

fn opens_tool_pair(entry: Option<&TranscriptEntry>) -> bool {
    entry
        .map(|entry| {
            entry
                .blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall { .. }))
        })
        .unwrap_or(false)
}

fn completes_tool_pair(entry: &TranscriptEntry) -> bool {
    entry
        .blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

/// Frozen memory context: explicitly configured files, identity-frozen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenMemory {
    pub entries: Vec<FrozenMemoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenMemoryEntry {
    pub source: String,
    pub content_sha256: String,
    pub bytes: Vec<u8>,
}

impl FrozenMemory {
    /// Freeze memory files at task creation: contents are captured and
    /// hashed; the identity is stable for the task's lifetime.
    pub fn freeze(files: &[(String, Vec<u8>)]) -> Self {
        Self {
            entries: files
                .iter()
                .map(|(source, bytes)| FrozenMemoryEntry {
                    source: source.clone(),
                    content_sha256: sha256_hex(bytes),
                    bytes: bytes.clone(),
                })
                .collect(),
        }
    }

    /// The frozen context identity: changes only when the captured inputs
    /// change (never on re-freeze of identical content).
    pub fn identity(&self) -> String {
        let mut material = String::new();
        for entry in &self.entries {
            material.push_str(&entry.source);
            material.push('\u{1}');
            material.push_str(&entry.content_sha256);
            material.push('\u{1}');
        }
        sha256_hex(material.as_bytes())
    }
}

/// Catalogs exposed through the context service (instructions, skills, MCP
/// servers): versioned snapshots a harness requests explicitly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct CatalogSnapshot {
    pub instructions: Vec<String>,
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
}

impl CatalogSnapshot {
    /// FR-1 (5.3.1): fill the reserved instructions catalog from a frozen
    /// instruction set — injected file paths, audit view.
    pub fn from_instructions(set: &r_code_kernel::task::InstructionSetRef) -> Self {
        Self {
            instructions: set
                .entries
                .iter()
                .filter(|entry| entry.status == "injected")
                .map(|entry| entry.path.clone())
                .collect(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_memory_identity_is_content_addressed() {
        let a = FrozenMemory::freeze(&[("m1.md".into(), b"same".to_vec())]);
        let b = FrozenMemory::freeze(&[("m1.md".into(), b"same".to_vec())]);
        assert_eq!(a.identity(), b.identity());
        let c = FrozenMemory::freeze(&[("m1.md".into(), b"different".to_vec())]);
        assert_ne!(a.identity(), c.identity());
        // Order matters as part of the frozen identity.
        let d = FrozenMemory::freeze(&[
            ("m1.md".into(), b"same".to_vec()),
            ("m2.md".into(), b"x".to_vec()),
        ]);
        let e = FrozenMemory::freeze(&[
            ("m2.md".into(), b"x".to_vec()),
            ("m1.md".into(), b"same".to_vec()),
        ]);
        assert_ne!(d.identity(), e.identity());
    }
}
