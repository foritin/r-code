//! V2 persistence for durable questions.

use crate::v2::V2Store;
use rusqlite::params;

impl V2Store {
    /// Persist a question (before suspension).
    pub fn save_question(
        &self,
        question_id: &str,
        task_id: &str,
        run_id: &str,
        text: &str,
        options: &[String],
        blocking: bool,
    ) -> Result<(), rusqlite::Error> {
        self.connection().execute(
            "INSERT OR REPLACE INTO questions(
                question_id, task_id, run_id, text, options_json, state, answer, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', NULL, ?6)",
            params![
                question_id,
                task_id,
                run_id,
                text,
                serde_json::to_string(options).unwrap_or_else(|_| "[]".into()),
                now_ms(),
            ],
        )?;
        let _ = blocking;
        Ok(())
    }

    /// Record the answer (resume-once bookkeeping).
    pub fn answer_question(
        &self,
        question_id: &str,
        answer: &str,
    ) -> Result<bool, rusqlite::Error> {
        let changed = self.connection().execute(
            "UPDATE questions
             SET state = 'answered', answer = ?2, answered_at_ms = ?3
             WHERE question_id = ?1 AND state = 'open'",
            params![question_id, answer, now_ms()],
        )?;
        Ok(changed > 0)
    }

    /// Expire an open question.
    pub fn expire_question(&self, question_id: &str) -> Result<bool, rusqlite::Error> {
        let changed = self.connection().execute(
            "UPDATE questions SET state = 'expired' WHERE question_id = ?1 AND state = 'open'",
            params![question_id],
        )?;
        Ok(changed > 0)
    }

    /// The open blocking question for a task, if any.
    pub fn open_blocking_question(&self, task_id: &str) -> Result<Option<String>, rusqlite::Error> {
        use rusqlite::OptionalExtension;
        self.connection()
            .query_row(
                "SELECT question_id FROM questions
                 WHERE task_id = ?1 AND state = 'open' ORDER BY created_at_ms LIMIT 1",
                params![task_id],
                |row| row.get(0),
            )
            .optional()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
