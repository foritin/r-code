//! FR-1 (M1a-08): the /context query view — what the latest run injected.
//! Reads the durable surfaces only: the task contract (frozen memory), the
//! latest run snapshot (frozen instructions), and the injection ledger
//! (memory/instruction rows plus JIT batches). No recomputation.

use crate::application::{ApplicationError, ApplicationService};
use r_code_kernel::ports::JournalStore as _;

/// One instruction file fact for the view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextInstructionEntry {
    pub layer: String,
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub status: String,
}

/// One JIT batch recorded during the run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextJitBatch {
    pub applied: u64,
    pub bytes: u64,
    pub paths: Vec<String>,
}

/// The full /context payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCurrentView {
    pub task_id: String,
    pub run_id: Option<String>,
    pub injection_enabled: bool,
    pub memory: Option<ContextMemorySummary>,
    pub instructions: Vec<ContextInstructionEntry>,
    pub instructions_digest: String,
    pub instructions_bytes: u64,
    pub total_budget_bytes: u64,
    pub jit_allowance_bytes: u64,
    pub jit_batches: Vec<ContextJitBatch>,
}

/// Frozen memory summary for the view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextMemorySummary {
    pub snapshot_hash: String,
    pub entry_count: usize,
    pub chars: u64,
}

impl ApplicationService {
    /// FR-1 /context: the latest run's injection state for one task.
    pub async fn context_current(
        &self,
        task_id: &str,
    ) -> Result<ContextCurrentView, ApplicationError> {
        let state = self
            .store
            .load_task(task_id)
            .await
            .ok_or_else(|| ApplicationError::Task(format!("task {task_id} not found")))?;

        // Latest run + its frozen snapshot come from the journal.
        let events = self.store.task_events(task_id);
        let latest_run = events.iter().rev().find_map(|event| {
            if event.kind != "run.started" {
                return None;
            }
            event
                .payload
                .get("runId")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
        let latest_snapshot = events.iter().rev().find_map(|event| {
            if event.kind != "run.started" {
                return None;
            }
            event
                .payload
                .get("snapshotId")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });

        let mut instructions = Vec::new();
        let mut instructions_digest = String::new();
        let mut instructions_bytes = 0u64;
        if let Some(snapshot_id) = &latest_snapshot {
            if let Ok(Some(snapshot)) = self.store.load_run_snapshot(snapshot_id) {
                let set = &snapshot.material().instructions;
                instructions_digest = set.digest.clone();
                instructions_bytes = set.rendered.len() as u64;
                for entry in &set.entries {
                    instructions.push(ContextInstructionEntry {
                        layer: entry.layer.clone(),
                        path: entry.path.clone(),
                        sha256: entry.sha256.chars().take(12).collect(),
                        bytes: entry.bytes,
                        status: entry.status.clone(),
                    });
                }
            }
        }

        // JIT batches: the context.jit journal events carry the audit
        // detail (applied count, paths, bytes) for this task.
        let jit_batches: Vec<ContextJitBatch> = events
            .iter()
            .filter(|event| event.kind == "context.jit")
            .filter_map(|event| {
                let applied = event.payload.get("applied")?.as_u64()?;
                let bytes = event
                    .payload
                    .get("bytes")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let paths = event
                    .payload
                    .get("paths")
                    .and_then(|v| v.as_array())
                    .map(|list| {
                        list.iter()
                            .filter_map(|p| p.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(ContextJitBatch {
                    applied,
                    paths,
                    bytes,
                })
            })
            .collect();

        // Effective settings for this workspace (PRD 8 view).
        let workspace_root = state.preferences.workspace_path.clone();
        let settings = workspace_root
            .as_deref()
            .map(|root| {
                let canonical = std::fs::canonicalize(root)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| root.to_string());
                crate::services::project_instructions::resolve_settings(&self.store, &canonical)
            })
            .unwrap_or_default();

        let memory = state
            .contract
            .memory
            .as_ref()
            .map(|memory| ContextMemorySummary {
                snapshot_hash: memory.snapshot_hash.clone(),
                entry_count: memory.entry_ids.len(),
                chars: memory.rendered.chars().count() as u64,
            });

        Ok(ContextCurrentView {
            task_id: task_id.to_string(),
            run_id: latest_run,
            injection_enabled: settings.injection_enabled,
            memory,
            instructions,
            instructions_digest: instructions_digest.chars().take(12).collect(),
            instructions_bytes,
            total_budget_bytes: settings.total_budget_bytes as u64,
            jit_allowance_bytes: settings.jit_allowance_bytes as u64,
            jit_batches,
        })
    }
}
