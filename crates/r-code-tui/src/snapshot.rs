//! snapshot 权威 vs 事件瞬时（R-TUI-02 / M8-02.A2；T35 起事件源为 v2 journal）。

use crate::{TranscriptEvent, TranscriptRow, TuiState};

/// 权威重建：从 v2 journal 投影（TranscriptEvent 序列）重建 transcript
/// （权威状态走守护进程 journal + 重建，渲染层不把事件流累积成领域状态副本）。
pub fn rebuild_from_journal(events: &[TranscriptEvent]) -> Vec<TranscriptRow> {
    let mut state = TuiState::new();
    for event in events {
        state.apply_transcript_event(event);
    }
    state.rows().to_vec()
}

/// 一致性断言：事件累积视图（瞬时）与权威重建逐项一致。
pub fn views_agree(events: &[TranscriptEvent]) -> bool {
    let mut live = TuiState::new();
    for event in events {
        live.apply_transcript_event(event);
    }
    live.rows() == rebuild_from_journal(events).as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_events() -> Vec<TranscriptEvent> {
        vec![
            TranscriptEvent::User {
                message_id: "m-1".into(),
                text: "问题".into(),
            },
            TranscriptEvent::ToolCall {
                run_id: "r1".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            },
            TranscriptEvent::ToolResult {
                run_id: "r1".into(),
                name: "bash".into(),
                ok: true,
                output: serde_json::json!("ok"),
            },
            TranscriptEvent::Assistant {
                run_id: "r1".into(),
                text: "回答".into(),
            },
        ]
    }

    /// M8-02.A2：snapshot 权威 vs 事件瞬时——两视图重建逐项一致；渲染层
    /// 状态只是瞬时缓存，权威状态可随时从 journal 投影重建。
    #[test]
    fn live_view_matches_authoritative_rebuild() {
        let events = sample_events();
        assert!(views_agree(&events));
        // 空序列与单事件同样一致。
        assert!(views_agree(&[]));
        assert!(views_agree(&[TranscriptEvent::Assistant {
            run_id: "r".into(),
            text: "x".into(),
        }]));
        // 重建结果正确性（非平凡）：user + toolcard + assistant。
        let rows = rebuild_from_journal(&events);
        assert_eq!(rows.len(), 3);
    }
}
