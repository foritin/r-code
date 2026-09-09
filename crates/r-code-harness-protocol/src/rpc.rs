//! JSON-RPC 2.0 envelopes and NDJSON framing limits for the plugin protocol.
//!
//! stdout carries protocol frames only (one JSON document per newline); stderr
//! is bounded diagnostics. Frame and queue budgets are enforced by the
//! transport (T09); this module owns the wire shapes and the shared limits.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Maximum size of a single protocol frame (bytes of the JSON document).
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Default aggregate bound for all pending outbound frames per direction.
pub const MAX_QUEUE_BYTES: usize = 16 * 1024 * 1024;
/// Default time the host waits for `initialize` responses.
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);
/// Grace period after `harness.cancel` before the process tree is killed.
pub const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// Host-to-plugin lifecycle methods.
pub const HOST_TO_PLUGIN_METHODS: &[&str] = &[
    "initialize",
    "harness.start",
    "harness.resume",
    "harness.steer",
    "harness.cancel",
    "shutdown",
];

/// Plugin-to-host request/notification methods.
pub const PLUGIN_TO_HOST_METHODS: &[&str] = &[
    "host.model.stream",
    "host.tools.list",
    "host.tools.call",
    "host.process.open",
    "host.process.write",
    "host.process.close",
    "host.context.read",
    "host.artifacts.put",
    "host.artifacts.read",
    "host.plan.publish",
    "host.plan.update",
    "host.questions.ask",
    "host.approvals.request",
    "host.children.spawn",
    "host.children.wait",
    "host.children.cancel",
    "host.verification.run",
    "host.checkpoint.save",
    "host.completion.propose",
    "harness.event",
    "stream.event",
];

/// True when `method` is part of the protocol in either direction.
pub fn is_known_method(method: &str) -> bool {
    HOST_TO_PLUGIN_METHODS.contains(&method) || PLUGIN_TO_HOST_METHODS.contains(&method)
}

/// JSON-RPC request/response id: number or string, never null.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Number(i64),
    Text(String),
}

impl RpcId {
    pub fn next_number(seq: i64) -> Self {
        RpcId::Number(seq)
    }
}

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: RpcId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 notification (no id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// A structured JSON-RPC error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Protocol-specific error codes in the JSON-RPC reserved -32000 range.
pub mod error_code {
    pub const FRAME_TOO_LARGE: i64 = -32001;
    pub const QUEUE_OVERFLOW: i64 = -32002;
    pub const PROTOCOL_VIOLATION: i64 = -32003;
    pub const INITIALIZE_TIMEOUT: i64 = -32004;
    pub const GENERATION_REVOKED: i64 = -32005;
    pub const RUN_MISMATCH: i64 = -32006;
    pub const UNKNOWN_METHOD: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL: i64 = -32603;
}

impl RpcError {
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: error_code::UNKNOWN_METHOD,
            message: format!("unknown method {method}"),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: error_code::INVALID_PARAMS,
            message: message.into(),
            data: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: error_code::INTERNAL,
            message: message.into(),
            data: None,
        }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self {
            code: error_code::PROTOCOL_VIOLATION,
            message: message.into(),
            data: None,
        }
    }
}

/// A JSON-RPC 2.0 response. Exactly one of `result`/`error` is present; the
/// custom deserializer rejects envelopes with neither or both.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: RpcId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl<'de> Deserialize<'de> for RpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            jsonrpc: String,
            id: RpcId,
            #[serde(default)]
            result: Option<serde_json::Value>,
            #[serde(default)]
            error: Option<RpcError>,
        }
        let raw = Raw::deserialize(deserializer)?;
        match (raw.result, raw.error) {
            (Some(_), Some(_)) | (None, None) => Err(serde::de::Error::custom(
                "rpc response must carry exactly one of result or error",
            )),
            (result, error) => Ok(RpcResponse {
                jsonrpc: raw.jsonrpc,
                id: raw.id,
                result,
                error,
            }),
        }
    }
}

/// One decoded protocol frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcMessage {
    Request(RpcRequest),
    Notification(RpcNotification),
    Response(RpcResponse),
}

/// Errors produced while encoding or decoding frames.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {size} bytes exceeds the {max} byte limit")]
    TooLarge { size: usize, max: usize },
    #[error("frame is not valid UTF-8")]
    InvalidUtf8,
    #[error("frame is not valid JSON: {0}")]
    MalformedJson(String),
    #[error("frame is not a valid JSON-RPC 2.0 message")]
    InvalidRpcShape,
}

/// Serialize a message into one NDJSON frame, enforcing the frame budget.
pub fn encode_frame(message: &RpcMessage) -> Result<Vec<u8>, FrameError> {
    let mut line =
        serde_json::to_vec(message).map_err(|e| FrameError::MalformedJson(e.to_string()))?;
    line.push(b'\n');
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            size: line.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    Ok(line)
}

/// Decode one NDJSON frame (without the trailing newline).
pub fn decode_frame(line: &[u8]) -> Result<RpcMessage, FrameError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            size: line.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    let text = std::str::from_utf8(line).map_err(|_| FrameError::InvalidUtf8)?;
    let message: RpcMessage =
        serde_json::from_str(text).map_err(|e| FrameError::MalformedJson(e.to_string()))?;
    Ok(message)
}

/// True when the method is not part of the protocol; used to fail closed
/// before any routing decision.
pub fn reject_unknown_method(method: &str) -> Option<RpcError> {
    if is_known_method(method) {
        None
    } else {
        Some(RpcError::method_not_found(method))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_requires_exactly_one_of_result_or_error() {
        let ok: RpcMessage = decode_frame(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#).unwrap();
        assert!(matches!(&ok, RpcMessage::Response(r) if r.result.is_some()));
        let err = decode_frame(br#"{"jsonrpc":"2.0","id":1}"#);
        assert!(matches!(err, Err(FrameError::MalformedJson(_))));
        let both = decode_frame(
            br#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-1,"message":"x"}}"#,
        );
        assert!(matches!(both, Err(FrameError::MalformedJson(_))));
    }
}
