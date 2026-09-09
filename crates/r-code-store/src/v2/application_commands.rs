//! V2 persistence for application-level command receipts.
//!
//! `(profile_id, client_id, command_id)` identity with canonical
//! method+payload hashes. Independent of the plugin-attempt operation keys:
//! this layer deduplicates *frontend* commands (task creation, message
//! submission, plugin install, approvals, stop) across reconnects and daemon
//! restarts. Acceptance and result commit atomically.

use crate::v2::V2Store;
use rusqlite::params;

/// What a repeated command finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandReceiptState {
    /// No prior record; the intent row was just persisted.
    Fresh,
    /// Same identity and hash, already completed: replay this result.
    Completed { result: serde_json::Value },
    /// Same identity and hash, accepted but not finished: wait/retry the
    /// query path (never re-execute blindly).
    Accepted,
    /// Same identity, different canonical input.
    Conflict {
        recorded_hash: String,
        incoming_hash: String,
    },
}

impl V2Store {
    /// Record (or inspect) the intent for a command. The hash covers the
    /// canonical serialization of method + params.
    pub fn application_command_intent(
        &self,
        profile_id: &str,
        client_id: &str,
        command_id: &str,
        method: &str,
        payload_hash: &str,
    ) -> Result<CommandReceiptState, rusqlite::Error> {
        let connection = self.connection();
        let existing: Option<(String, String, Option<String>)> = connection
            .query_row(
                "SELECT payload_hash, state, result_json FROM application_commands
                 WHERE profile_id = ?1 AND client_id = ?2 AND command_id = ?3",
                params![profile_id, client_id, command_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        if let Some((recorded_hash, state, result_json)) = existing {
            if recorded_hash != payload_hash {
                return Ok(CommandReceiptState::Conflict {
                    recorded_hash,
                    incoming_hash: payload_hash.to_string(),
                });
            }
            return match state.as_str() {
                "completed" => Ok(CommandReceiptState::Completed {
                    result: result_json
                        .and_then(|text| serde_json::from_str(&text).ok())
                        .unwrap_or(serde_json::Value::Null),
                }),
                _ => Ok(CommandReceiptState::Accepted),
            };
        }
        connection.execute(
            "INSERT INTO application_commands(
                profile_id, client_id, command_id, method, payload_hash, state,
                result_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 'accepted', NULL, ?6)",
            params![
                profile_id,
                client_id,
                command_id,
                method,
                payload_hash,
                now_ms()
            ],
        )?;
        Ok(CommandReceiptState::Fresh)
    }

    /// Atomically attach the result to an accepted command.
    pub fn complete_application_command(
        &self,
        profile_id: &str,
        client_id: &str,
        command_id: &str,
        result: &serde_json::Value,
    ) -> Result<(), rusqlite::Error> {
        self.connection().execute(
            "UPDATE application_commands
             SET state = 'completed', result_json = ?4
             WHERE profile_id = ?1 AND client_id = ?2 AND command_id = ?3",
            params![
                profile_id,
                client_id,
                command_id,
                serde_json::to_string(result).unwrap_or_else(|_| "null".into())
            ],
        )?;
        Ok(())
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
