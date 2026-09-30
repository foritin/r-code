//! The multi-turn model/tool loop.
//!
//! Pure request-shaping: project the conversation, call the model, execute
//! returned tool calls through host tools with attempt-stable operation
//! keys, checkpoint after every turn, and propose completion when the
//! model stops calling tools. Cancellation is cooperative between turns.

use crate::request_projection::{
    project_request, push_assistant, push_tool_result, push_user, ConversationState,
    ProjectionError, WireBlock,
};
use r_code_harness_protocol::services::{
    PlanPublishRequest, ProposalKind, ToolDescriptor, WorkUnitWire,
};
use r_code_harness_sdk::{SdkError, SdkHandle};

/// Loop configuration (strategy defaults preserved from the Native
/// runtime; budgets stay host-owned). The host seeds per-task values
/// (model selection, inference knobs, task mode) through the initialize
/// handshake's `harness_config`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoopConfig {
    pub system_prompt: String,
    pub model_selection: Option<String>,
    /// Inference knobs (thinking level / effort) forwarded verbatim to the
    /// host model broker.
    #[serde(default)]
    pub inference: Option<serde_json::Value>,
    /// Task mode label ("ask" / "edit" / "auto" / "plan") selecting the
    /// system prompt posture.
    #[serde(default)]
    pub task_mode: Option<String>,
    /// Next task-owned immutable plan revision supplied by the host. It is
    /// required in plan mode and never guessed by the harness.
    #[serde(default)]
    pub plan_revision: Option<u64>,
    pub max_turns: u32,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            system_prompt: agent_config::DEFAULT_MAIN_AGENT_PROMPT.to_string(),
            model_selection: None,
            inference: None,
            task_mode: None,
            plan_revision: None,
            max_turns: 25,
        }
    }
}

impl LoopConfig {
    /// Build the config from the host-provided harness config document.
    pub fn from_harness_config(harness_config: &serde_json::Value) -> Self {
        let mut config = Self::default();
        if let Some(selection) = harness_config
            .get("modelSelection")
            .and_then(|v| v.as_str())
        {
            if !selection.is_empty() {
                config.model_selection = Some(selection.to_string());
            }
        }
        if config.model_selection.is_none() {
            if let Some(default) = harness_config
                .get("defaultModelSelection")
                .and_then(|v| v.as_str())
            {
                if !default.is_empty() {
                    config.model_selection = Some(default.to_string());
                }
            }
        }
        config.inference = harness_config
            .get("inference")
            .cloned()
            .filter(|v| !v.is_null());
        config.task_mode = harness_config
            .get("taskMode")
            .and_then(|v| v.as_str())
            .filter(|mode| !mode.is_empty())
            .map(str::to_string);
        config.plan_revision = harness_config
            .get("planRevision")
            .and_then(|value| value.as_u64())
            .filter(|revision| *revision > 0);
        if let Some(system_prompt) = harness_config
            .get("systemPrompt")
            .and_then(|value| value.as_str())
        {
            config.system_prompt = system_prompt.to_string();
        }
        if let Some(max_turns) = harness_config
            .get("maxTurns")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
        {
            config.max_turns = max_turns;
        }
        config
    }

    /// The effective system prompt for the configured task mode.
    pub fn effective_system_prompt(&self) -> String {
        match self.task_mode.as_deref() {
            Some("plan") => format!(
                "{}\n\n当前为 Plan 模式：先调查再规划。产出分步实施计划（目标、步骤、\
                 涉及文件、风险），未经用户确认不要直接修改文件。",
                if self.system_prompt.is_empty() {
                    "You are a careful coding agent."
                } else {
                    &self.system_prompt
                }
            ),
            // Ask is enforced by the host's read-only tool capability. Keep
            // the resolved prompt byte-for-byte so task prompt snapshots and
            // model requests have the same identity.
            Some("ask") => self.system_prompt.clone(),
            _ => self.system_prompt.clone(),
        }
    }
}

/// Errors surfaced by the loop.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LoopError {
    #[error("sdk failure: {0}")]
    Sdk(String),
    #[error(transparent)]
    Projection(#[from] ProjectionError),
    #[error("turn limit reached ({0} turns)")]
    TurnLimit(u32),
    #[error("plan mode requires a host-provided plan revision")]
    MissingPlanRevision,
}

impl From<SdkError> for LoopError {
    fn from(error: SdkError) -> Self {
        LoopError::Sdk(error.to_string())
    }
}

/// The loop result.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoopResult {
    pub final_text: String,
    pub turns: u32,
    pub tool_calls_executed: u32,
    pub proposal_accepted: bool,
}

/// Run the loop for one user input over the host services.
pub async fn run_loop(
    handle: &SdkHandle,
    config: &LoopConfig,
    state: &mut ConversationState,
    user_input: &str,
) -> Result<LoopResult, LoopError> {
    push_user(state, user_input);
    let tools: Vec<ToolDescriptor> = handle
        .tools_list()
        .await
        .map_err(|e| LoopError::Sdk(e.to_string()))?
        .tools;
    let system_prompt = config.effective_system_prompt();
    let mut turns = 0u32;
    let mut executed = 0u32;
    let mut final_text = String::new();
    loop {
        if handle.is_cancelled() {
            break;
        }
        turns += 1;
        if turns > config.max_turns {
            return Err(LoopError::TurnLimit(config.max_turns));
        }
        let request = project_request(
            state,
            &system_prompt,
            &tools,
            config.model_selection.as_deref(),
            config.inference.clone(),
        )?;
        let turn = handle.model_stream(request).await?;
        let text = turn.text();
        let calls = turn.tool_calls();
        let wire_calls: Vec<WireBlock> = calls
            .iter()
            .map(|(id, name, input)| WireBlock::ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            })
            .collect();
        push_assistant(state, &text, &wire_calls);

        if calls.is_empty() {
            final_text = text;
            // Text-only finishes checkpoint too: the conversation state is
            // the crash-resume point regardless of how the turn ended.
            let payload = serde_json::to_vec(state).unwrap_or_default();
            let _ = handle.save_checkpoint(&payload, turns as u64).await;
            break;
        }
        // Execute every tool call through the host with stable keys.
        for (id, name, input) in &calls {
            let operation_key = format!("turn-{turns}-{id}");
            let reply = handle
                .tools_call(name, input.clone(), Some(&operation_key))
                .await
                .map_err(|e| LoopError::Sdk(e.to_string()))?;
            let output = reply
                .output
                .iter()
                .map(|block| match block {
                    r_code_harness_protocol::services::OutputBlock::Text { text } => text.clone(),
                    r_code_harness_protocol::services::OutputBlock::Json { value } => {
                        value.to_string()
                    }
                    r_code_harness_protocol::services::OutputBlock::Image { .. } => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            let output = match reply.error {
                Some(error) => format!("error: {}", error.message),
                None => output,
            };
            push_tool_result(state, id, &output);
            executed += 1;
        }
        // Checkpoint the conversation after every turn (crash-resume point).
        let payload = serde_json::to_vec(state).unwrap_or_default();
        let _ = handle
            .save_checkpoint(&payload, turns as u64)
            .await
            .map_err(|e| LoopError::Sdk(e.to_string()))?;
    }

    // A plan becomes a completion proposal only after the host validates and
    // durably publishes its exact revision. A publish failure exits here and
    // therefore cannot be mistaken for completion.
    let proposal_kind = match config.task_mode.as_deref() {
        Some("plan") => {
            let revision = config.plan_revision.ok_or(LoopError::MissingPlanRevision)?;
            handle
                .plan_publish(PlanPublishRequest {
                    revision,
                    work_units: plan_work_units(&final_text),
                })
                .await
                .map_err(|error| LoopError::Sdk(error.to_string()))?;
            ProposalKind::PlanDraft
        }
        Some("execution" | "edit" | "auto") => ProposalKind::Implementation,
        _ => ProposalKind::Reply,
    };

    // Propose completion for host arbitration.
    let decision = handle
        .propose_completion(
            r_code_harness_protocol::services::CompletionProposalRequest {
                kind: proposal_kind,
                summary: if final_text.is_empty() {
                    "run stopped".into()
                } else {
                    final_text.clone()
                },
                candidate_digest: None,
                work_unit_statuses: vec![],
            },
        )
        .await
        .map_err(|e| LoopError::Sdk(e.to_string()))?;
    Ok(LoopResult {
        final_text,
        turns,
        tool_calls_executed: executed,
        proposal_accepted: decision.accepted,
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictPlanDocument {
    work_units: Vec<StrictWorkUnit>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictWorkUnit {
    id: String,
    description: String,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    acceptance: Vec<String>,
    #[serde(default)]
    read_paths: Vec<String>,
    #[serde(default)]
    write_paths: Vec<String>,
    #[serde(default)]
    repo_exclusive: bool,
    #[serde(default)]
    ephemeral_roots: Vec<String>,
    /// P19A (API v1.2): effect authority carried verbatim from the
    /// strict plan document; absent fields stay at the conservative
    /// ReadOnly/Offline defaults.
    #[serde(default)]
    effect_class: r_code_harness_protocol::services::WorkUnitEffectClass,
    #[serde(default)]
    network_ceiling: r_code_harness_protocol::services::NetworkCeiling,
}

/// Parse the documented strict JSON shape when present. Semantic validation
/// (empty/duplicate ids and dependency integrity) stays host-owned. Free-form
/// model output becomes one stable unit so identical text produces identical
/// plan material across retries.
fn plan_work_units(final_text: &str) -> Vec<WorkUnitWire> {
    if let Ok(document) = serde_json::from_str::<StrictPlanDocument>(final_text) {
        return document
            .work_units
            .into_iter()
            .map(|unit| WorkUnitWire {
                id: unit.id,
                description: unit.description,
                dependencies: unit.dependencies,
                acceptance: unit.acceptance,
                read_paths: unit.read_paths,
                write_paths: unit.write_paths,
                repo_exclusive: unit.repo_exclusive,
                ephemeral_roots: unit.ephemeral_roots,
                effect_class: unit.effect_class,
                network_ceiling: unit.network_ceiling,
            })
            .collect();
    }
    let digest = r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
        "finalText": final_text,
    }));
    vec![WorkUnitWire {
        id: format!("plan-{}", &digest[..16]),
        description: final_text.to_string(),
        dependencies: Vec::new(),
        acceptance: Vec::new(),
        read_paths: Vec::new(),
        write_paths: Vec::new(),
        repo_exclusive: false,
        ephemeral_roots: Vec::new(),
        effect_class: r_code_harness_protocol::services::WorkUnitEffectClass::ReadOnly,
        network_ceiling: r_code_harness_protocol::services::NetworkCeiling::Offline,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_the_product_owned_main_agent_prompt() {
        let config = LoopConfig::default();
        assert_eq!(
            config.system_prompt,
            agent_config::DEFAULT_MAIN_AGENT_PROMPT
        );
        assert!(config.system_prompt.contains("You are R-Code"));
    }

    #[test]
    fn harness_config_can_override_prompt_and_turn_budget() {
        let config = LoopConfig::from_harness_config(&serde_json::json!({
            "systemPrompt": "project prompt",
            "maxTurns": 41,
            "taskMode": "edit",
        }));
        assert_eq!(config.system_prompt, "project prompt");
        assert_eq!(config.max_turns, 41);
        assert_eq!(config.task_mode.as_deref(), Some("edit"));
    }
}
