//! The multi-turn model/tool loop.
//!
//! Pure request-shaping: project the conversation, call the model, execute
//! returned tool calls through host tools with attempt-stable operation
//! keys, checkpoint after every turn, and propose completion when the
//! model stops calling tools. Cancellation is cooperative between turns.

use crate::request_projection::{
    project_request, push_assistant, push_tool_result, push_user, ConversationState, WireBlock,
};
use r_code_harness_protocol::services::{ProposalKind, ToolDescriptor};
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
    pub max_turns: u32,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            system_prompt: "You are a careful coding agent. Use the provided tools.".into(),
            model_selection: None,
            inference: None,
            task_mode: None,
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
            Some("ask") => format!(
                "{}\n\n当前为 Ask 模式：优先回答与解释，必要时用只读工具查证；不做文件修改。",
                if self.system_prompt.is_empty() {
                    "You are a careful coding agent."
                } else {
                    &self.system_prompt
                }
            ),
            _ => self.system_prompt.clone(),
        }
    }
}

/// Errors surfaced by the loop.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LoopError {
    #[error("sdk failure: {0}")]
    Sdk(String),
    #[error("turn limit reached ({0} turns)")]
    TurnLimit(u32),
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
        );
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

    // Propose completion for host arbitration.
    let decision = handle
        .propose_completion(
            r_code_harness_protocol::services::CompletionProposalRequest {
                kind: ProposalKind::Reply,
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
