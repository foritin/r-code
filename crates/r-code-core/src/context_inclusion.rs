//! SessionEvent 上下文正交标记（docs/pi-alignment PRD §4.1 R-SES-01 / M5-01）。
//!
//! "进入 LLM 上下文"与"仅持久化"在 R-Code 的类型层已由 agent-store 回放的
//! **穷举 match + 显式 no-op** 固化（新变体不归类就无法编译）。本模块把该
//! 归类提升为显式类型面 [`ContextInclusion`]：单一函数 `classify` 对每个
//! [`SessionEvent`] 变体给出归类，持久化层/上下文构建层共用同一判定——
//! 编译期语义（穷举）+ 运行期可断言（分类清单单测固化）。
//!
//! 归类语义（与 agent-store 回放行为逐变体对齐）：
//! - `Context`：回放时进 `Session.messages`（Message / 快照 / 投影 / 持久化
//!   user 消息事件）；
//! - `AuditOnly`：只持久化（审计/重建自检），回放 no-op（RequestHeader /
//!   Usage / ToolCall / ToolResult 计数 / 一般 System 事件 / Meta 首行）。

use agent_contract::session::SessionEvent;

/// 单个 SessionEvent 变体的上下文归类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextInclusion {
    /// 进入 LLM 上下文（回放重建 messages/projection）。
    Context,
    /// 仅持久化（审计/统计；回放 no-op，绝不进模型请求）。
    AuditOnly,
}

/// 变体归类唯一实现（上下文构建与审计过滤共用；新增变体必须在此归类，
/// 否则无法编译——与 agent-store 回放穷举同一约束强度）。
pub fn classify(event: &SessionEvent) -> ContextInclusion {
    match event {
        // 正文消息：进上下文。
        SessionEvent::Message(_) => ContextInclusion::Context,
        // 无损工作集快照与模型投影：上下文重建的权威来源。
        SessionEvent::HistorySnapshot { .. } => ContextInclusion::Context,
        SessionEvent::ModelProjection { .. } => ContextInclusion::Context,
        // 审计信封：只持久化（哈希指纹不进请求）。
        SessionEvent::RequestHeader { .. } => ContextInclusion::AuditOnly,
        // 用量统计：计数，不进上下文。
        SessionEvent::Usage(_) => ContextInclusion::AuditOnly,
        // 工具调用双记录（正文经 HistorySnapshot 的 ToolUse/ToolResult 配对进
        // 上下文；这两条独立审计副本只计数）。
        SessionEvent::ToolCall { .. } => ContextInclusion::AuditOnly,
        SessionEvent::ToolResult { .. } => ContextInclusion::AuditOnly,
        // System：仅 durable user message 语义事件回放为消息（agent-store
        // is_durable_user_message_event 白名单）；分类面按"潜在 Context"标记，
        // 由 classify_system 按事件名细分。
        SessionEvent::System { .. } => ContextInclusion::AuditOnly,
        // 首行元数据：不是事件正文。
        SessionEvent::Meta(_) => ContextInclusion::AuditOnly,
    }
}

/// System 事件的细分：事件名命中持久化用户消息白名单时进上下文。
/// （与 agent-store 的 is_durable_user_message_event 同一判定面。）
pub fn classify_system(event_name: &str) -> ContextInclusion {
    match event_name {
        // agent-store 的 durable user message 物化事件（含 legacy 名）。
        "durable_user_message" | "r_code_durable_user_message" => ContextInclusion::Context,
        _ => ContextInclusion::AuditOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_contract::message::Message;
    use agent_contract::session::SessionMeta;
    use agent_contract::usage::Usage;

    fn meta() -> SessionMeta {
        SessionMeta::new("m", "p")
    }

    /// M5-01.A1：类型层区分显式——全变体穷举归类（新增变体未归类即编译失败）。
    #[test]
    fn every_variant_has_explicit_classification() {
        let samples: Vec<SessionEvent> = vec![
            SessionEvent::Meta(meta()),
            SessionEvent::Message(Message::user_text("hi")),
            SessionEvent::Usage(Usage::new(1, 2)),
            SessionEvent::ToolCall {
                name: "bash".to_string(),
                input: serde_json::json!({}),
            },
            SessionEvent::ToolResult {
                call_id: "c".to_string(),
                output: serde_json::json!("ok"),
                is_error: false,
            },
            SessionEvent::HistorySnapshot {
                messages: vec![Message::user_text("s")],
            },
            SessionEvent::ModelProjection { messages: None },
            SessionEvent::RequestHeader {
                system_sha256: "x".to_string(),
                tools_sha256: "y".to_string(),
                messages_sha256: "z".to_string(),
                reason: "initial".to_string(),
                excluded_tails: Vec::new(),
                tool_names: Vec::new(),
                hosted_tool_names: Vec::new(),
                max_tokens: 0,
                provider_name: None,
                provider_kind: None,
                model: None,
                protocol: None,
                context_window_tokens: 0,
                text_tokens: 0,
                image_tokens: 0,
                document_tokens: 0,
                tool_schema_tokens: 0,
                estimated_input_tokens: 0,
                requested_output_tokens: 0,
                reserve_tokens: 0,
                materialized_wire_bytes: 0,
                attachment_count: 0,
                anchoring_phase: None,
                context_profile: None,
                attachment_ids: Vec::new(),
            },
            SessionEvent::System {
                event: "whatever".to_string(),
                data: serde_json::json!({}),
            },
        ];
        // 逐条归类不 panic 且二值合法；正文/快照/投影 = Context，其余 AuditOnly。
        for event in &samples {
            let inclusion = classify(event);
            assert!(matches!(
                inclusion,
                ContextInclusion::Context | ContextInclusion::AuditOnly
            ));
        }
        assert_eq!(classify(&samples[1]), ContextInclusion::Context);
        assert_eq!(classify(&samples[5]), ContextInclusion::Context);
        assert_eq!(classify(&samples[6]), ContextInclusion::Context);
        for index in [0usize, 2, 3, 4, 7, 8] {
            assert_eq!(
                classify(&samples[index]),
                ContextInclusion::AuditOnly,
                "index {index} 必须是仅持久化"
            );
        }
    }

    /// M5-01.A2：纯 UI/审计 entry 不进上下文——RequestHeader/Usage/独立工具
    /// 审计副本/一般 System 全部 AuditOnly（单测杜绝误发）。
    #[test]
    fn audit_only_entries_never_reach_context() {
        let audit_samples: Vec<SessionEvent> = vec![
            SessionEvent::Usage(Usage::new(10, 5)),
            SessionEvent::ToolCall {
                name: "read".to_string(),
                input: serde_json::json!({}),
            },
            SessionEvent::ToolResult {
                call_id: "c".to_string(),
                output: serde_json::json!(null),
                is_error: true,
            },
            SessionEvent::System {
                event: "ui.local-scroll".to_string(),
                data: serde_json::json!({"px": 120}),
            },
        ];
        assert!(audit_samples
            .iter()
            .all(|event| classify(event) == ContextInclusion::AuditOnly));
        // durable user message 语义事件例外进上下文。
        assert_eq!(
            classify_system("durable_user_message"),
            ContextInclusion::Context
        );
        assert_eq!(
            classify_system("r_code_durable_user_message"),
            ContextInclusion::Context
        );
        assert_eq!(
            classify_system("ui.local-scroll"),
            ContextInclusion::AuditOnly
        );
    }
}
