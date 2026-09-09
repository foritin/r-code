//! V2 persistence for verification definitions and evidence.

use crate::v2::V2Store;
use r_code_kernel::verification::CheckDefinition;
use rusqlite::params;

impl V2Store {
    /// Persist a check definition (host-owned; immutable per identity).
    pub fn save_check_definition(
        &self,
        definition: &CheckDefinition,
    ) -> Result<(), rusqlite::Error> {
        self.connection().execute(
            "INSERT OR REPLACE INTO evidence(
                evidence_id, task_id, check_id, candidate_digest, environment,
                passed, host_output_ref, provenance_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8)",
            params![
                format!("def:{}", definition.check_id),
                "<definition>",
                definition.check_id,
                definition.identity(),
                definition.toolchain,
                serde_json::to_string(&definition.entrypoint).unwrap_or_default(),
                serde_json::to_string(definition).unwrap_or_default(),
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// Record host-generated evidence.
    pub fn save_evidence(
        &self,
        evidence: &r_code_kernel::task::EvidenceRecord,
    ) -> Result<(), rusqlite::Error> {
        self.connection().execute(
            "INSERT OR REPLACE INTO evidence(
                evidence_id, task_id, check_id, candidate_digest, environment,
                passed, host_output_ref, provenance_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                evidence.evidence_id,
                format!("task:{}", evidence.check_id),
                evidence.check_id,
                evidence.candidate_digest,
                evidence.environment,
                evidence.passed as i64,
                evidence
                    .host_output
                    .as_ref()
                    .map(|artifact| artifact.blob_id.clone()),
                serde_json::to_string(&evidence.recorded_by).unwrap_or_default(),
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// All host-provenance passing evidence for one candidate digest.
    pub fn evidence_for_candidate(
        &self,
        candidate_digest: &str,
    ) -> Result<Vec<r_code_kernel::task::EvidenceRecord>, rusqlite::Error> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT evidence_id, check_id, environment, provenance_json FROM evidence
             WHERE candidate_digest = ?1 AND passed = 1 AND task_id != '<definition>'
             ORDER BY created_at_ms",
        )?;
        let rows = statement.query_map(params![candidate_digest], |row| {
            Ok(r_code_kernel::task::EvidenceRecord {
                evidence_id: row.get(0)?,
                check_id: row.get(1)?,
                candidate_digest: candidate_digest.to_string(),
                environment: row.get(2)?,
                passed: true,
                host_output: None,
                recorded_by: serde_json::from_str(&row.get::<_, String>(3)?)
                    .unwrap_or(r_code_harness_protocol::Provenance::Host),
            })
        })?;
        rows.collect()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
