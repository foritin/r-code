//! Background execution for the local evolving-memory reviewer.
//!
//! Main runs only enqueue sanitized turns.  This worker owns the deliberately small,
//! non-streaming provider request and commits validated proposals through `MemoryStore`.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use agent_contract::{CompletionRequest, InferenceOptions, Message};
use r_code_core::MemoryReviewOutput;
use r_code_store::{Database, MemoryReviewClaim, MemoryStore};

use crate::provider_support::{build_provider_config, provider_readiness_error};
use crate::settings::SettingsService;

const REVIEW_MAX_TOKENS: u32 = 2_048;

fn reviewer_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Wake the process-wide reviewer. Multiple calls coalesce behind one serial drain, which keeps
/// lightweight review work from competing with itself while never blocking the main answer.
pub fn spawn_memory_review_worker(db: Arc<Database>, config_dir: PathBuf) {
    tokio::spawn(async move {
        let _guard = reviewer_lock().lock().await;
        loop {
            let claim = match MemoryStore::new(&db).claim_next_job() {
                Ok(Some(claim)) => claim,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!("cannot claim memory review job: {error}");
                    break;
                }
            };
            if let Err((code, _detail)) = execute_claim(&db, &config_dir, &claim).await {
                tracing::warn!(
                    job_id = %claim.job_id,
                    error_code = code,
                    "memory review failed"
                );
                if let Err(error) =
                    MemoryStore::new(&db).mark_failed(&claim.job_id, claim.attempt, code)
                {
                    tracing::warn!(job_id = %claim.job_id, "cannot persist memory review failure: {error}");
                }
            }
        }
    });
}

async fn execute_claim(
    db: &Database,
    config_dir: &std::path::Path,
    claim: &MemoryReviewClaim,
) -> Result<(), (&'static str, String)> {
    let config = SettingsService::new(config_dir.to_path_buf())
        .load_global_unvalidated()
        .map_err(|error| ("provider_unavailable", error.to_string()))?;
    let provider_config = config
        .providers
        .get(&claim.reviewer.provider_name)
        .ok_or_else(|| {
            (
                "provider_unavailable",
                format!(
                    "reviewer provider '{}' no longer exists",
                    claim.reviewer.provider_name
                ),
            )
        })?;
    if let Some(problem) = provider_readiness_error(&claim.reviewer.provider_name, provider_config)
    {
        return Err(("provider_unavailable", problem));
    }
    let provider = agent_llm::create_provider(build_provider_config(
        &claim.reviewer.provider_name,
        provider_config,
    ))
    .map_err(|error| ("provider_unavailable", error.to_string()))?;
    let input = serde_json::to_string(&claim.assembly.wire)
        .map_err(|error| ("invalid_review_output", error.to_string()))?;
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        provider.complete(CompletionRequest {
            model: claim.reviewer.model.clone(),
            system: Some(reviewer_system_prompt().to_string()),
            messages: vec![Message::user_text(format!(
                "Review this sanitized memory envelope and return only the required JSON object:\n{input}"
            ))],
            tools: Vec::new(),
            hosted_tools: Vec::new(),
            max_tokens: REVIEW_MAX_TOKENS,
            temperature: Some(0.1),
            enable_caching: false,
            inference: InferenceOptions::default(),
        }),
    )
    .await
    .map_err(|_| ("provider_request_failed", "memory review timed out".to_string()))?
    .map_err(|error| ("provider_request_failed", error.to_string()))?;
    let output =
        parse_review_output(&response.text()).map_err(|error| ("invalid_review_output", error))?;
    MemoryStore::new(db)
        .commit_success(claim, &output)
        .map_err(|error| ("invalid_review_output", error.to_string()))
}

fn reviewer_system_prompt() -> &'static str {
    "You are R-Code's bounded memory reviewer. The input is already sanitized. Propose only \
durable, reusable information supported by the supplied evidence: explicit user preferences or \
constraints, stable project conventions/decisions, and verified recurring pitfalls. Never retain \
credentials, tokens, private keys, personal data, raw logs, local absolute paths, temporary task \
state, guesses, or assistant-only claims. Use global scope only for cross-project user preferences; \
use project scope for repository-specific facts. A project proposal must use basis=explicit_user and \
may cite only turns whose explicit_remember field is true; assistant text never grants permission. \
Do not emit project proposals with basis=verified_result because this envelope has no structured \
host-result evidence. Global proposals still require user approval. Prefer no proposal over a weak \
one. Return exactly one JSON object with this shape and no prose: \
{\"proposals\":[{\"scope\":\"global|project|skip\",\"kind\":\"preference|constraint|convention|decision|pitfall\",\"operation\":\"add|replace|noop\",\"target_memory_ordinal\":null,\"target_version\":null,\"content\":null,\"reason\":\"\",\"basis\":\"explicit_user|verified_result\",\"evidence_ordinals\":[1],\"confidence\":0.0}]}"
}

fn parse_review_output(text: &str) -> Result<MemoryReviewOutput, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("reviewer returned an empty response".to_string());
    }
    let json = if trimmed.starts_with("```") {
        let mut lines = trimmed.lines();
        let opening = lines.next().unwrap_or_default().trim();
        if opening != "```json" && opening != "```JSON" {
            return Err("reviewer fence must be explicitly marked json".to_string());
        }
        let mut body = lines.collect::<Vec<_>>();
        if body.last().map(|line| line.trim()) != Some("```") {
            return Err("reviewer JSON fence is not closed".to_string());
        }
        body.pop();
        if body.iter().any(|line| line.trim_start().starts_with("```")) {
            return Err("reviewer returned multiple fenced blocks".to_string());
        }
        body.join("\n")
    } else {
        trimmed.to_string()
    };
    if !json.trim_start().starts_with('{') || !json.trim_end().ends_with('}') {
        return Err("reviewer must return one JSON object without prose".to_string());
    }
    serde_json::from_str(json.trim()).map_err(|error| format!("invalid reviewer JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_plain_or_one_json_fence() {
        let plain = r#"{"proposals":[]}"#;
        assert!(parse_review_output(plain).is_ok());
        assert!(parse_review_output(&format!("```json\n{plain}\n```")).is_ok());
    }

    #[test]
    fn parser_rejects_prose_and_unknown_fields() {
        assert!(parse_review_output("result: {\"proposals\":[]}").is_err());
        assert!(parse_review_output(r#"{"proposals":[],"extra":true}"#).is_err());
    }
}
