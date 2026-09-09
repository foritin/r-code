//! Provider-neutral request projection.
//!
//! Shapes the wire-stable model request: system prompt, conversation
//! blocks and tool descriptors. Provider-specific quirks live host-side in
//! the model broker; this module only produces the neutral projection.

use r_code_harness_protocol::services::{
    ContentBlock, ModelMessage, ModelRole, ModelStreamRequest, ToolDescriptor,
};

/// The projected conversation state carried across turns (also the shape
/// persisted into checkpoints).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct ConversationState {
    pub messages: Vec<WireMessage>,
}

/// A serializable mirror of [`ModelMessage`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireMessage {
    pub role: String,
    pub blocks: Vec<WireBlock>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum WireBlock {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        output_text: String,
    },
}

/// Project the conversation into a model request.
pub fn project_request(
    state: &ConversationState,
    system_prompt: &str,
    tools: &[ToolDescriptor],
    selection: Option<&str>,
    inference: Option<serde_json::Value>,
) -> ModelStreamRequest {
    let mut messages = Vec::new();
    if !system_prompt.trim().is_empty() {
        messages.push(ModelMessage {
            role: ModelRole::System,
            content: vec![ContentBlock::Text {
                text: system_prompt.to_string(),
            }],
        });
    }
    for message in &state.messages {
        let role = match message.role.as_str() {
            "user" => ModelRole::User,
            "tool" => ModelRole::Tool,
            _ => ModelRole::Assistant,
        };
        let blocks = message
            .blocks
            .iter()
            .map(|block| match block {
                WireBlock::Text { text } => ContentBlock::Text { text: text.clone() },
                WireBlock::ToolCall { id, name, input } => ContentBlock::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                },
                WireBlock::ToolResult {
                    call_id,
                    output_text,
                } => ContentBlock::ToolResult {
                    call_id: call_id.clone(),
                    output: vec![r_code_harness_protocol::services::OutputBlock::Text {
                        text: output_text.clone(),
                    }],
                },
            })
            .collect();
        messages.push(ModelMessage {
            role,
            content: blocks,
        });
    }
    ModelStreamRequest {
        selection: selection.map(str::to_string),
        messages,
        tools: tools.to_vec(),
        inference,
        deadline_ms: None,
    }
}

/// Append an assistant turn (text + pending tool calls) to the state.
pub fn push_assistant(state: &mut ConversationState, text: &str, calls: &[WireBlock]) {
    let mut blocks = Vec::new();
    if !text.is_empty() {
        blocks.push(WireBlock::Text {
            text: text.to_string(),
        });
    }
    blocks.extend(calls.iter().cloned());
    if !blocks.is_empty() {
        state.messages.push(WireMessage {
            role: "assistant".into(),
            blocks,
        });
    }
}

/// Append a tool result.
pub fn push_tool_result(state: &mut ConversationState, call_id: &str, output: &str) {
    state.messages.push(WireMessage {
        role: "tool".into(),
        blocks: vec![WireBlock::ToolResult {
            call_id: call_id.into(),
            output_text: output.into(),
        }],
    });
}

/// Push the initial user objective.
pub fn push_user(state: &mut ConversationState, text: &str) {
    state.messages.push(WireMessage {
        role: "user".into(),
        blocks: vec![WireBlock::Text { text: text.into() }],
    });
}
