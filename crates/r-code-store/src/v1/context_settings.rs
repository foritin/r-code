//! Per-workspace instruction-injection settings (M1a-06, FR-1 / PRD §8).
//! Stored as validated JSON keyed by the same workspace_key the lease
//! tables use. The daemon resolves these at run-freeze time; the desktop
//! settings surface mutates them over the `context.settings.update` RPC.

use crate::v1::journal::{V1Store, V1StoreError};
use rusqlite::{params, OptionalExtension};

/// The stored wire shape (validated by the runtime's engine settings).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSettingsRecord {
    pub injection_enabled: bool,
    pub total_budget_bytes: u64,
    pub jit_allowance_bytes: u64,
    #[serde(default)]
    pub fallback_names: Vec<String>,
}

impl ContextSettingsRecord {
    /// Sensible defaults mirror the engine defaults (PRD 8).
    pub fn defaults() -> Self {
        Self {
            injection_enabled: true,
            total_budget_bytes: 32 * 1024,
            jit_allowance_bytes: 8 * 1024,
            fallback_names: vec!["CLAUDE.md".into()],
        }
    }
}

impl V1Store {
    /// Persist settings for one workspace key (upsert).
    pub fn save_context_settings(
        &self,
        workspace_key: &str,
        record: &ContextSettingsRecord,
    ) -> Result<(), V1StoreError> {
        let settings_json = serde_json::to_string(record)
            .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
        let mut connection = self.connection();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO context_settings(workspace_key, settings_json, updated_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(workspace_key) DO UPDATE SET
                settings_json = excluded.settings_json,
                updated_at_ms = excluded.updated_at_ms",
            params![workspace_key, settings_json, now_ms()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Load the stored settings for one workspace key, if any.
    pub fn context_settings(
        &self,
        workspace_key: &str,
    ) -> Result<Option<ContextSettingsRecord>, V1StoreError> {
        let settings_json: Option<String> = self
            .connection()
            .query_row(
                "SELECT settings_json FROM context_settings WHERE workspace_key = ?1",
                params![workspace_key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(settings_json) = settings_json else {
            return Ok(None);
        };
        serde_json::from_str(&settings_json)
            .map(Some)
            .map_err(|error| V1StoreError::Serialization(error.to_string()))
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
