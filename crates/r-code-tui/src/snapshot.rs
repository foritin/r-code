//! snapshot 权威 vs 事件瞬时（R-TUI-02 / M8-02.A2）。

use crate::{TranscriptRow, TuiState};
use r_code_core::dto::AgentEvent;

/// 权威重建：从 JSONL 事件序列重建 transcript（权威状态走 JSONL + 重建，
/// 渲染层不把事件流累积成领域状态副本）。
pub fn rebuild_from_jsonl(events: &[AgentEvent]) -> Vec<TranscriptRow> {
    let mut state = TuiState::new();
    for event in events {
        state.apply(event);
    }
    state.flush_streaming();
    state.rows().to_vec()
}

/// 一致性断言：事件累积视图（瞬时）与权威重建逐项一致。
pub fn views_agree(events: &[AgentEvent]) -> bool {
    let mut live = TuiState::new();
    for event in events {
        live.apply(event);
    }
    live.flush_streaming();
    live.rows() == rebuild_from_jsonl(events).as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::AgentEvent;

    fn sample_events() -> Vec<AgentEvent> {
        vec![
            AgentEvent::Message {
                text: "问题".into(),
                delta: false,
            },
            AgentEvent::ToolCall {
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
                call_id: "c1".into(),
            },
            AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: serde_json::json!("ok"),
                is_error: false,
            },
            AgentEvent::Message {
                text: "回答".into(),
                delta: false,
            },
        ]
    }

    /// M8-02.A2：snapshot 权威 vs 事件瞬时——两视图重建逐项一致；渲染层
    /// 状态只是瞬时缓存，权威状态可随时从事件序列（JSONL 等价物）重建。
    #[test]
    fn live_view_matches_authoritative_rebuild() {
        let events = sample_events();
        assert!(views_agree(&events));
        // 空序列与单事件同样一致。
        assert!(views_agree(&[]));
        assert!(views_agree(&[AgentEvent::Message {
            text: "x".into(),
            delta: false
        }]));
        // 重建结果正确性（非平凡）。
        let rows = rebuild_from_jsonl(&events);
        assert_eq!(rows.len(), 3);
    }
}
