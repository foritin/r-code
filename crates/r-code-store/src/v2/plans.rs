//! V2 persistence for plans.

use crate::v2::V2Store;
use rusqlite::params;

impl V2Store {
    /// Persist the current plan revision for a task (JSON snapshot).
    pub fn save_plan(
        &self,
        task_id: &str,
        revision: u64,
        plan_json: &str,
    ) -> Result<(), rusqlite::Error> {
        self.connection().execute(
            "INSERT INTO reviews(task_id, disposition, notes, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(task_id) DO UPDATE SET
                disposition = excluded.disposition,
                notes = excluded.notes,
                updated_at_ms = excluded.updated_at_ms",
            params![
                format!("plan:{task_id}"),
                format!("revision:{revision}"),
                plan_json,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Load the persisted plan revision for a task.
    pub fn load_plan(&self, task_id: &str) -> Result<Option<(u64, String)>, rusqlite::Error> {
        use rusqlite::OptionalExtension;
        let row: Option<(String, Option<String>)> = self
            .connection()
            .query_row(
                "SELECT disposition, notes FROM reviews WHERE task_id = ?1",
                params![format!("plan:{task_id}")],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(disposition, notes)| {
            let revision = disposition.trim_start_matches("revision:").parse().ok()?;
            Some((revision, notes.unwrap_or_default()))
        }))
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
