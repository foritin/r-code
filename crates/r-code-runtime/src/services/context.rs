//! Canonical transcript and context projections.
//!
//! The host owns the canonical transcript; harnesses select context
//! strategy. Reads replay by cursor and never split a tool call from its
//! result; attachment ownership and frozen-memory identity are part of the
//! context projection. Exactly one transcript writer per task is admitted —
//! duplicate writers are refused instead of racing.

use crate::services::artifacts::sha256_hex;
use r_code_harness_protocol::services::{ContentBlock, ContextPage, TranscriptEntry};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

/// Errors from the context service.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextError {
    #[error("a writer already owns the transcript for task {0}")]
    DuplicateWriter(String),
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
        if !writers.insert(task_id.to_string()) {
            return Err(ContextError::DuplicateWriter(task_id.to_string()));
        }
        // Reload persisted entries when the transcript file exists.
        let entries = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .filter_map(|line| serde_json::from_str::<TranscriptEntry>(line).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
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
        let seq = state.entries.len() as u64 + 1;
        state.entries.push(TranscriptEntry { seq, role, blocks });
        self.persist(&state)?;
        Ok(seq)
    }

    fn persist(&self, state: &TranscriptState) -> Result<(), ContextError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut text = String::new();
        for entry in &state.entries {
            text.push_str(&serde_json::to_string(entry).unwrap_or_default());
            text.push('\n');
        }
        std::fs::write(path, text).map_err(|e| ContextError::Io(e.to_string()))?;
        Ok(())
    }

    /// Replay from a cursor. The page never splits a ToolCall from its
    /// ToolResult: if the limit would cut between them, the result travels
    /// with its call.
    pub fn read_page(&self, cursor: u64, limit: u32) -> ContextPage {
        let state = self.state.lock().expect("transcript");
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
