//! Immutable host check definitions and candidate-bound evidence.

use crate::v1::V1Store;
use r_code_harness_protocol::{ArtifactRef, Provenance};
use r_code_kernel::task::EvidenceRecord;
use r_code_kernel::verification::CheckDefinition;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, thiserror::Error)]
pub enum VerificationStoreError {
    #[error("sqlite failure: {0}")]
    Sqlite(String),
    #[error("verification record serialization failed")]
    Serialization,
    #[error("immutable verification identity conflicts with stored content")]
    Conflict,
    #[error("check definition is invalid")]
    InvalidDefinition,
    #[error("evidence identity is incomplete or foreign")]
    InvalidEvidence,
}

impl From<rusqlite::Error> for VerificationStoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

impl V1Store {
    pub fn save_check_definition(
        &self,
        definition: &CheckDefinition,
    ) -> Result<(), VerificationStoreError> {
        definition
            .validate()
            .map_err(|_| VerificationStoreError::InvalidDefinition)?;
        let identity = definition.identity();
        let json =
            serde_json::to_string(definition).map_err(|_| VerificationStoreError::Serialization)?;
        let existing: Option<(String, String)> = self
            .connection()
            .query_row(
                "SELECT identity, definition_json FROM check_definitions WHERE check_id = ?1",
                params![definition.check_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((stored_identity, stored_json)) = existing {
            return if stored_identity == identity && stored_json == json {
                Ok(())
            } else {
                Err(VerificationStoreError::Conflict)
            };
        }
        self.connection().execute(
            "INSERT INTO check_definitions(check_id, identity, definition_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![definition.check_id, identity, json, now_ms()],
        )?;
        Ok(())
    }

    pub fn load_check_definition(
        &self,
        check_id: &str,
    ) -> Result<Option<CheckDefinition>, VerificationStoreError> {
        let row: Option<(String, String)> = self
            .connection()
            .query_row(
                "SELECT identity, definition_json FROM check_definitions WHERE check_id = ?1",
                params![check_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((identity, json)) = row else {
            return Ok(None);
        };
        let definition: CheckDefinition =
            serde_json::from_str(&json).map_err(|_| VerificationStoreError::Serialization)?;
        definition
            .validate()
            .map_err(|_| VerificationStoreError::InvalidDefinition)?;
        if definition.check_id != check_id || definition.identity() != identity {
            return Err(VerificationStoreError::Conflict);
        }
        Ok(Some(definition))
    }

    pub fn save_evidence(&self, evidence: &EvidenceRecord) -> Result<(), VerificationStoreError> {
        if evidence.evidence_id.trim().is_empty()
            || evidence.task_id.trim().is_empty()
            || evidence.definition_identity.trim().is_empty()
            || evidence.environment_fingerprint.trim().is_empty()
            || !matches!(evidence.recorded_by, Provenance::Host)
            || evidence
                .host_output
                .as_ref()
                .is_some_and(|artifact| !valid_artifact_ref(artifact))
        {
            return Err(VerificationStoreError::InvalidEvidence);
        }
        let definition = self
            .load_check_definition(&evidence.check_id)?
            .ok_or(VerificationStoreError::InvalidEvidence)?;
        if definition.identity() != evidence.definition_identity {
            return Err(VerificationStoreError::InvalidEvidence);
        }
        if let Some(existing) = load_evidence_by_id(self, &evidence.evidence_id)? {
            return if existing == *evidence {
                Ok(())
            } else {
                Err(VerificationStoreError::Conflict)
            };
        }
        let host_output = evidence
            .host_output
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| VerificationStoreError::Serialization)?;
        let provenance = serde_json::to_string(&evidence.recorded_by)
            .map_err(|_| VerificationStoreError::Serialization)?;
        let inserted = self.connection().execute(
            "INSERT OR IGNORE INTO evidence(
                evidence_id, task_id, check_id, definition_identity, candidate_digest,
                environment, environment_fingerprint, passed, host_output_ref,
                provenance_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                evidence.evidence_id,
                evidence.task_id,
                evidence.check_id,
                evidence.definition_identity,
                evidence.candidate_digest,
                evidence.environment,
                evidence.environment_fingerprint,
                evidence.passed as i64,
                host_output,
                provenance,
                now_ms(),
            ],
        )?;
        if inserted == 1 {
            Ok(())
        } else {
            Err(VerificationStoreError::Conflict)
        }
    }

    pub fn evidence_for_candidate(
        &self,
        candidate_digest: &str,
    ) -> Result<Vec<EvidenceRecord>, VerificationStoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT evidence_id, task_id, check_id, definition_identity, environment,
                    environment_fingerprint, host_output_ref, provenance_json
             FROM evidence WHERE candidate_digest = ?1 AND passed = 1
             ORDER BY created_at_ms, evidence_id",
        )?;
        let rows = statement.query_map(params![candidate_digest], |row| {
            decode_evidence_row(row, candidate_digest, true)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

fn load_evidence_by_id(
    store: &V1Store,
    evidence_id: &str,
) -> Result<Option<EvidenceRecord>, VerificationStoreError> {
    store
        .connection()
        .query_row(
            "SELECT evidence_id, task_id, check_id, definition_identity, environment,
                    environment_fingerprint, host_output_ref, provenance_json,
                    candidate_digest, passed
             FROM evidence WHERE evidence_id = ?1",
            params![evidence_id],
            |row| {
                let candidate: String = row.get(8)?;
                let passed: i64 = row.get(9)?;
                decode_evidence_row(row, &candidate, passed == 1)
            },
        )
        .optional()
        .map_err(Into::into)
}

fn decode_evidence_row(
    row: &rusqlite::Row<'_>,
    candidate_digest: &str,
    passed: bool,
) -> Result<EvidenceRecord, rusqlite::Error> {
    let host_output_json: Option<String> = row.get(6)?;
    let provenance_json: String = row.get(7)?;
    let host_output = host_output_json
        .map(|json| decode_json::<ArtifactRef>(&json, 6))
        .transpose()?;
    if host_output
        .as_ref()
        .is_some_and(|artifact| !valid_artifact_ref(artifact))
    {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid evidence artifact reference",
            )),
        ));
    }
    let recorded_by = decode_json::<Provenance>(&provenance_json, 7)?;
    Ok(EvidenceRecord {
        evidence_id: row.get(0)?,
        task_id: row.get(1)?,
        check_id: row.get(2)?,
        definition_identity: row.get(3)?,
        candidate_digest: candidate_digest.to_string(),
        environment: row.get(4)?,
        environment_fingerprint: row.get(5)?,
        passed,
        host_output,
        recorded_by,
    })
}

fn valid_artifact_ref(artifact: &ArtifactRef) -> bool {
    artifact.schema == ArtifactRef::SCHEMA
        && artifact.sha256.len() == 64
        && artifact.blob_id == format!("blob:sha256:{}", artifact.sha256)
        && artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_json<T: serde::de::DeserializeOwned>(
    value: &str,
    column: usize,
) -> Result<T, rusqlite::Error> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
