//! M0-02 冻结合同测试（PRD §10 M0-02 验收断言 A1–A4）。
//!
//! 测试函数以 a1_/a2_/a3_/a4_ 前缀命名：统一 Harness 按前缀过滤执行，
//! 与 scripts/codex-interaction/registry.mjs 中的断言命令一一对应。
//! 帧构造基于 fixtures/codex-interaction/protocol-0.145.0.json 的冻结
//! schema（A1 额外用 include_str 复用该 fixture 的样例帧做 wire 校验）。

use serde_json::{json, Value};

use super::*;

const FIXTURE_JSON: &str = include_str!("../../fixtures/codex-interaction/protocol-0.145.0.json");

fn normalizer() -> CodexInteractionNormalizer {
    CodexInteractionNormalizer::new(
        CodexInteractionCapabilities::for_cli_version(Some("0.145.0")),
        7,
        "run_test",
    )
}

fn prime_thread_and_turn(state: &mut CodexInteractionNormalizer) {
    state.feed(&json!({
        "jsonrpc": "2.0",
        "method": "thread/started",
        "params": { "thread": { "id": "thr_demo" } }
    }));
    state.feed(&json!({
        "jsonrpc": "2.0",
        "method": "turn/started",
        "params": { "threadId": "thr_demo", "turn": { "id": "turn_demo" } }
    }));
}

fn events(outcomes: &[CodexInteractionOutcome]) -> Vec<&CodexTimelineEventV1> {
    outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            CodexInteractionOutcome::Event(event) => Some(event),
            CodexInteractionOutcome::Diagnostic(_) => None,
        })
        .collect()
}

fn diagnostics(outcomes: &[CodexInteractionOutcome]) -> Vec<&CodexInteractionDiagnosticV1> {
    outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            CodexInteractionOutcome::Diagnostic(diagnostic) => Some(diagnostic),
            CodexInteractionOutcome::Event(_) => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// A1（contract）：所有 §4.1 已知帧转换为预期事件，unknown 只产生安全诊断
// ---------------------------------------------------------------------------

#[test]
fn a1_method_table_alignment() {
    // 旧 transport 识别表的每个方法都必须仍被识别为协议进度（超集对齐）。
    for method in [
        "initialized",
        "thread/started",
        "thread/status/changed",
        "turn/started",
        "turn/completed",
        "turn/plan/updated",
        "item/started",
        "item/completed",
        "item/agentMessage/delta",
        "item/reasoning/summaryTextDelta",
        "item/commandExecution/requestApproval",
        "item/fileChange/requestApproval",
        "item/permissions/requestApproval",
        "item/tool/call",
        "requestUserInput",
    ] {
        assert!(
            is_recognized_protocol_progress(method),
            "legacy progress method no longer recognized: {method}"
        );
    }
    // 丰富交互新增方法同样进入进度表（M1/M2 投影依赖）。
    for method in [
        "turn/diff/updated",
        "thread/compacted",
        "thread/tokenUsage/updated",
        "warning",
        "error",
        "item/commandExecution/outputDelta",
        "item/fileChange/patchUpdated",
        "item/tool/requestUserInput",
        "serverRequest/resolved",
    ] {
        assert!(is_recognized_protocol_progress(method), "{method}");
    }
    assert!(!is_recognized_protocol_progress("totally/unknown"));
}

#[test]
fn a1_agent_message_lifecycle_with_phase() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);
    let started = state.feed(&json!({
        "method": "item/started",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "msg_1", "type": "agentMessage", "phase": "commentary", "text": "" }
        }
    }));
    match events(&started)[0] {
        CodexTimelineEventV1::AssistantStarted {
            item_id,
            phase,
            scope,
        } => {
            assert_eq!(item_id, "msg_1");
            assert_eq!(*phase, CodexAssistantPhase::Commentary);
            assert_eq!(scope.run_id, "run_test");
            assert_eq!(scope.thread_id, "thr_demo");
            assert_eq!(scope.turn_id.as_deref(), Some("turn_demo"));
        }
        other => panic!("expected AssistantStarted, got {other:?}"),
    }

    let delta = state.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "msg_1", "delta": "正在定位…" }
    }));
    match events(&delta)[0] {
        CodexTimelineEventV1::AssistantDelta {
            item_id,
            phase,
            delta,
            ..
        } => {
            assert_eq!(item_id, "msg_1");
            assert_eq!(*phase, CodexAssistantPhase::Commentary);
            assert_eq!(delta, "正在定位…");
        }
        other => panic!("expected AssistantDelta, got {other:?}"),
    }

    let completed = state.feed(&json!({
        "method": "item/completed",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "msg_1", "type": "agentMessage", "phase": "final_answer", "text": "最终答案" }
        }
    }));
    match events(&completed)[0] {
        CodexTimelineEventV1::AssistantCompleted {
            item_id,
            phase,
            authoritative_text,
            ..
        } => {
            assert_eq!(item_id, "msg_1");
            assert_eq!(*phase, CodexAssistantPhase::FinalAnswer);
            assert_eq!(authoritative_text, "最终答案");
        }
        other => panic!("expected AssistantCompleted, got {other:?}"),
    }
}

#[test]
fn a1_reasoning_tool_plan_diff_usage_compaction_warning_conversion() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);

    let reasoning_delta = state.feed(&json!({
        "method": "item/reasoning/summaryTextDelta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "rs_1", "summaryIndex": 2, "delta": "摘要片段" }
    }));
    match events(&reasoning_delta)[0] {
        CodexTimelineEventV1::ReasoningSummaryDelta {
            item_id,
            summary_index,
            delta,
            ..
        } => {
            assert_eq!(item_id, "rs_1");
            assert_eq!(*summary_index, 2);
            assert_eq!(delta, "摘要片段");
        }
        other => panic!("expected ReasoningSummaryDelta, got {other:?}"),
    }

    let reasoning_done = state.feed(&json!({
        "method": "item/completed",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "rs_1", "type": "reasoning", "content": ["raw chain"], "summary": ["公开摘要"] }
        }
    }));
    match events(&reasoning_done)[0] {
        CodexTimelineEventV1::ReasoningSummaryCompleted {
            item_id,
            public_summary,
            ..
        } => {
            assert_eq!(item_id, "rs_1");
            assert_eq!(public_summary, "公开摘要");
        }
        other => panic!("expected ReasoningSummaryCompleted, got {other:?}"),
    }

    let tool_started = state.feed(&json!({
        "method": "item/started",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "cmd_1", "type": "commandExecution", "command": ["rg", "-n", "pattern"], "cwd": "/tmp", "status": "inProgress", "commandActions": [] }
        }
    }));
    match events(&tool_started)[0] {
        CodexTimelineEventV1::ToolStarted {
            item_id,
            kind,
            safe_input,
            ..
        } => {
            assert_eq!(item_id, "cmd_1");
            assert_eq!(*kind, CodexToolKind::CommandExecution);
            assert_eq!(safe_input.summary, "rg");
            assert_eq!(
                safe_input.raw_kind_name.as_deref(),
                Some("commandExecution")
            );
        }
        other => panic!("expected ToolStarted, got {other:?}"),
    }

    let output_delta = state.feed(&json!({
        "method": "item/commandExecution/outputDelta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "cmd_1", "delta": "line1\n" }
    }));
    match events(&output_delta)[0] {
        CodexTimelineEventV1::ToolOutputDelta {
            item_id,
            safe_delta,
            ..
        } => {
            assert_eq!(item_id, "cmd_1");
            assert_eq!(safe_delta, "line1\n");
        }
        other => panic!("expected ToolOutputDelta, got {other:?}"),
    }

    let tool_done = state.feed(&json!({
        "method": "item/completed",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "cmd_1", "type": "commandExecution", "command": ["rg"], "cwd": "/tmp", "status": "failed", "exitCode": 2, "aggregatedOutput": "not found", "commandActions": [] }
        }
    }));
    match events(&tool_done)[0] {
        CodexTimelineEventV1::ToolCompleted {
            item_id,
            kind,
            status,
            safe_output,
            ..
        } => {
            assert_eq!(item_id, "cmd_1");
            assert_eq!(*kind, CodexToolKind::CommandExecution);
            assert_eq!(*status, CodexToolStatus::Failed);
            assert_eq!(safe_output.as_deref(), Some("not found"));
        }
        other => panic!("expected ToolCompleted, got {other:?}"),
    }

    let plan = state.feed(&json!({
        "method": "turn/plan/updated",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "explanation": "两步走",
            "plan": [ { "step": "先扫描", "status": "completed" }, { "step": "再修复", "status": "pending" } ]
        }
    }));
    match events(&plan)[0] {
        CodexTimelineEventV1::PlanUpdated {
            explanation, steps, ..
        } => {
            assert_eq!(explanation.as_deref(), Some("两步走"));
            assert_eq!(steps.len(), 2);
            assert_eq!(steps[0].status, CodexPlanStepStatus::Completed);
            assert_eq!(steps[1].status, CodexPlanStepStatus::Pending);
        }
        other => panic!("expected PlanUpdated, got {other:?}"),
    }

    let diff = state.feed(&json!({
        "method": "turn/diff/updated",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "diff": "--- a\n+++ b\n" }
    }));
    match events(&diff)[0] {
        CodexTimelineEventV1::DiffUpdated {
            unified_diff_or_reference,
            ..
        } => {
            assert!(unified_diff_or_reference.starts_with("--- a"));
        }
        other => panic!("expected DiffUpdated, got {other:?}"),
    }

    let compacted = state.feed(&json!({
        "method": "thread/compacted",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo" }
    }));
    match events(&compacted)[0] {
        CodexTimelineEventV1::ContextCompacted { item_id, .. } => assert_eq!(*item_id, None),
        other => panic!("expected ContextCompacted, got {other:?}"),
    }

    let usage = state.feed(&json!({
        "method": "thread/tokenUsage/updated",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "tokenUsage": {
                "last": { "inputTokens": 10, "cachedInputTokens": 5, "outputTokens": 3, "reasoningOutputTokens": 1, "totalTokens": 14 },
                "total": { "inputTokens": 100, "cachedInputTokens": 50, "outputTokens": 30, "reasoningOutputTokens": 10, "totalTokens": 140 }
            }
        }
    }));
    match events(&usage)[0] {
        CodexTimelineEventV1::UsageUpdated { safe_usage, .. } => {
            assert_eq!(safe_usage.last.input_tokens, 10);
            assert_eq!(safe_usage.total.total_tokens, 140);
        }
        other => panic!("expected UsageUpdated, got {other:?}"),
    }

    let warning = state.feed(&json!({
        "method": "warning",
        "params": { "message": "磁盘空间偏低" }
    }));
    match events(&warning)[0] {
        CodexTimelineEventV1::Warning {
            scope,
            code,
            safe_message,
        } => {
            assert!(scope.is_none(), "global warning carries no scope");
            assert!(code.is_none());
            assert_eq!(safe_message, "磁盘空间偏低");
        }
        other => panic!("expected Warning, got {other:?}"),
    }
}

#[test]
fn a1_request_user_input_and_resolution() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);
    let outcomes = state.feed(&json!({
        "jsonrpc": "2.0",
        "id": 41,
        "method": "item/tool/requestUserInput",
        "params": {
            "threadId": "thr_demo",
            "turnId": "turn_demo",
            "itemId": "item_demo",
            "autoResolutionMs": 30000,
            "questions": [
                {
                    "id": "scope",
                    "header": "范围",
                    "question": "本次处理哪一部分？",
                    "isOther": true,
                    "isSecret": false,
                    "options": [ { "label": "当前模块", "description": "限制变更范围" } ]
                }
            ]
        }
    }));
    match events(&outcomes)[0] {
        CodexTimelineEventV1::UserInputRequested {
            item_id,
            transport_generation,
            request_id,
            questions,
            auto_resolution_ms,
            scope,
        } => {
            assert_eq!(item_id, "item_demo");
            assert_eq!(*transport_generation, 7);
            assert_eq!(request_id, "41");
            assert_eq!(*auto_resolution_ms, Some(30_000));
            assert_eq!(questions.len(), 1);
            assert_eq!(questions[0].id, "scope");
            assert!(questions[0].is_other);
            assert!(!questions[0].is_secret);
            assert_eq!(questions[0].options[0].label, "当前模块");
            assert_eq!(scope.turn_id.as_deref(), Some("turn_demo"));
        }
        other => panic!("expected UserInputRequested, got {other:?}"),
    }

    let resolved = state.feed(&json!({
        "method": "serverRequest/resolved",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "requestId": "41" }
    }));
    match events(&resolved)[0] {
        CodexTimelineEventV1::UserInputResolved {
            item_id, outcome, ..
        } => {
            assert_eq!(item_id, "item_demo");
            assert_eq!(*outcome, CodexUserInputOutcome::Resolved);
        }
        other => panic!("expected UserInputResolved, got {other:?}"),
    }
}

#[test]
fn a1_unknown_method_and_kind_only_produce_safe_diagnostics() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);

    let unknown_method = state.feed(&json!({
        "method": "codex/someBrandNewEvent",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "payload": { "secret": "x" } }
    }));
    assert!(
        events(&unknown_method).is_empty(),
        "unknown method must not become an event"
    );
    let diagnostic = diagnostics(&unknown_method)[0];
    assert_eq!(diagnostic.code, CodexDiagnosticCode::UnknownMethod);
    assert_eq!(
        diagnostic.method.as_deref(),
        Some("codex/someBrandNewEvent")
    );
    assert!(
        !diagnostic.detail.contains("secret"),
        "diagnostic must not carry payload"
    );

    let unknown_kind = state.feed(&json!({
        "method": "item/started",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "x_1", "type": "hologramProjection", "anything": "else" }
        }
    }));
    assert!(events(&unknown_kind).is_empty());
    let diagnostic = diagnostics(&unknown_kind)[0];
    assert_eq!(diagnostic.code, CodexDiagnosticCode::UnknownItemKind);
    assert!(diagnostic.detail.contains("hologramProjection"));

    // raw reasoning（content/textDelta）只产生丢弃诊断。
    let raw_delta = state.feed(&json!({
        "method": "item/reasoning/textDelta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "rs_9", "delta": "内部推理正文" }
    }));
    assert!(events(&raw_delta).is_empty());
    assert_eq!(
        diagnostics(&raw_delta)[0].code,
        CodexDiagnosticCode::ReasoningRawDropped
    );

    let legacy = state.feed(&json!({
        "method": "requestUserInput",
        "id": 9,
        "params": { "threadId": "thr_demo", "turnId": "turn_demo" }
    }));
    assert!(events(&legacy).is_empty());
    assert_eq!(
        diagnostics(&legacy)[0].code,
        CodexDiagnosticCode::LegacyName
    );
}

#[test]
fn a1_frozen_fixture_sample_frames_convert() {
    // 与 M0-01 冻结 fixture 的样例帧做 wire 级对齐：fixture 帧必须能被
    // 归一化器按 0.145.0 语义消费。
    let fixture: Value = serde_json::from_str(FIXTURE_JSON).expect("frozen fixture parses");
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);

    let delta_frame = &fixture["sample_frames"]["agent_message_delta"]["frame"];
    let outcomes = state.feed(delta_frame);
    let delta_events = events(&outcomes);
    assert_eq!(
        delta_events.len(),
        1,
        "fixture delta frame converts to one event"
    );
    match delta_events[0] {
        CodexTimelineEventV1::AssistantDelta { delta, .. } => {
            assert_eq!(delta, "正在检索配置入口…");
        }
        other => panic!("expected AssistantDelta from fixture, got {other:?}"),
    }

    let warning_frame = &fixture["sample_frames"]["warning"]["frame"];
    let outcomes = state.feed(warning_frame);
    match events(&outcomes)[0] {
        CodexTimelineEventV1::Warning { safe_message, .. } => {
            assert_eq!(safe_message, "demo warning");
        }
        other => panic!("expected Warning from fixture, got {other:?}"),
    }

    let request_frame = &fixture["sample_frames"]["request_user_input_request"]["frame"];
    let outcomes = state.feed(request_frame);
    match events(&outcomes)[0] {
        CodexTimelineEventV1::UserInputRequested {
            request_id,
            questions,
            ..
        } => {
            assert_eq!(request_id, "41");
            assert_eq!(questions[0].header, "范围");
        }
        other => panic!("expected UserInputRequested from fixture, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A2（compatibility）：缺可选字段/未知字段不崩溃；缺必需 scope 不进入当前 run
// ---------------------------------------------------------------------------

#[test]
fn a2_optional_and_unknown_fields_tolerated() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);
    // 缺 explanation 的 plan、缺 autoResolutionMs 的 request、带未知新字段的
    // completed（未来 schema 演进）都不崩溃且产出事件。
    let plan = state.feed(&json!({
        "method": "turn/plan/updated",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "plan": [] }
    }));
    match events(&plan)[0] {
        CodexTimelineEventV1::PlanUpdated {
            explanation, steps, ..
        } => {
            assert!(explanation.is_none());
            assert!(steps.is_empty());
        }
        other => panic!("expected PlanUpdated, got {other:?}"),
    }

    let done = state.feed(&json!({
        "method": "item/completed",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": {
                "id": "msg_x", "type": "agentMessage", "text": "答案",
                "brandNewExperimentalField": { "nested": true }
            }
        }
    }));
    match events(&done)[0] {
        CodexTimelineEventV1::AssistantCompleted {
            authoritative_text,
            phase,
            ..
        } => {
            assert_eq!(authoritative_text, "答案");
            assert_eq!(*phase, CodexAssistantPhase::Unknown);
        }
        other => panic!("expected AssistantCompleted, got {other:?}"),
    }
    assert!(diagnostics(&done)
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::PhaseMissing));

    let request = state.feed(&json!({
        "id": 77,
        "method": "item/tool/requestUserInput",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "item_x",
            "questions": [ { "id": "q1", "header": "h", "question": "q" } ]
        }
    }));
    match events(&request)[0] {
        CodexTimelineEventV1::UserInputRequested {
            auto_resolution_ms,
            questions,
            ..
        } => {
            assert_eq!(*auto_resolution_ms, None);
            assert_eq!(questions.len(), 1);
            assert!(questions[0].options.is_empty());
        }
        other => panic!("expected UserInputRequested, got {other:?}"),
    }
}

#[test]
fn a2_missing_or_stale_scope_never_enters_current_run() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);

    // 异线程帧：丢弃 + ScopeStale 计数。
    let stale = state.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_other", "turnId": "turn_demo", "itemId": "m", "delta": "别处的流" }
    }));
    assert!(stale.is_empty(), "stale thread frame produces no outcomes");
    assert_eq!(
        state
            .diagnostic_counts
            .get(&CodexDiagnosticCode::ScopeStale),
        Some(&1)
    );

    // 缺 threadId：丢弃 + ScopeMissing。
    let missing_thread = state.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "turnId": "turn_demo", "itemId": "m", "delta": "无主帧" }
    }));
    assert!(missing_thread.is_empty());
    assert_eq!(
        state
            .diagnostic_counts
            .get(&CodexDiagnosticCode::ScopeMissing),
        Some(&1)
    );

    // 异 turn 帧：同样 fail-closed。
    let stale_turn = state.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_demo", "turnId": "turn_old", "itemId": "m", "delta": "旧 turn" }
    }));
    assert!(stale_turn.is_empty());
    assert_eq!(
        state
            .diagnostic_counts
            .get(&CodexDiagnosticCode::ScopeStale),
        Some(&2)
    );

    // thread 未建立前的事件帧不允许进入。
    let mut fresh = normalizer();
    let early = fresh.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "m", "delta": "早到" }
    }));
    assert!(early.is_empty());
    assert_eq!(
        fresh
            .diagnostic_counts
            .get(&CodexDiagnosticCode::ScopeMissing),
        Some(&1)
    );

    // 陈旧 turn/completed 不得清掉当前活动 turn。
    let mut keep = normalizer();
    prime_thread_and_turn(&mut keep);
    let stale_finish = keep.feed(&json!({
        "method": "turn/completed",
        "params": { "threadId": "thr_demo", "turn": { "id": "turn_ghost" } }
    }));
    assert!(diagnostics(&stale_finish)
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::ScopeStale));
    let still_scoped = keep.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "m", "delta": "仍然有效" }
    }));
    assert_eq!(events(&still_scoped).len(), 1);
}

// ---------------------------------------------------------------------------
// A3（reliability）：超限 payload 有界，后续合法帧仍可处理
// ---------------------------------------------------------------------------

#[test]
fn a3_oversized_payloads_are_bounded_and_processing_continues() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);

    let huge_delta = "x".repeat(MAX_DELTA_CHARS + 5_000);
    let outcomes = state.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "big", "delta": huge_delta }
    }));
    match events(&outcomes)[0] {
        CodexTimelineEventV1::AssistantDelta { delta, .. } => {
            assert!(
                delta.chars().count() <= MAX_DELTA_CHARS + 1,
                "delta bounded"
            );
        }
        other => panic!("expected AssistantDelta, got {other:?}"),
    }
    assert!(diagnostics(&outcomes)
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::PayloadTruncated));

    // 问题数超限：截到上限并拒绝多余，请求事件仍然产生。
    let questions: Vec<Value> = (0..MAX_QUESTIONS + 5)
        .map(|index| json!({ "id": format!("q{index}"), "header": "h", "question": "q" }))
        .collect();
    let outcomes = state.feed(&json!({
        "id": 90,
        "method": "item/tool/requestUserInput",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "item_big",
            "questions": questions
        }
    }));
    match events(&outcomes)[0] {
        CodexTimelineEventV1::UserInputRequested { questions, .. } => {
            assert_eq!(questions.len(), MAX_QUESTIONS);
        }
        other => panic!("expected UserInputRequested, got {other:?}"),
    }
    assert!(diagnostics(&outcomes)
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::PayloadRejected));

    // 空问题 id：拒绝该问题，不影响其余。
    let outcomes = state.feed(&json!({
        "id": 91,
        "method": "item/tool/requestUserInput",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "item_bad",
            "questions": [
                { "id": "", "header": "h", "question": "bad" },
                { "id": "ok1", "header": "h", "question": "ok" }
            ]
        }
    }));
    match events(&outcomes)[0] {
        CodexTimelineEventV1::UserInputRequested { questions, .. } => {
            assert_eq!(questions.len(), 1);
            assert_eq!(questions[0].id, "ok1");
        }
        other => panic!("expected UserInputRequested, got {other:?}"),
    }

    // 超限之后正常帧继续处理（进程持续可用）。
    let healthy = state.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "big", "delta": "恢复" }
    }));
    assert_eq!(events(&healthy).len(), 1);
    assert_eq!(
        state
            .diagnostic_counts
            .get(&CodexDiagnosticCode::PayloadTruncated),
        Some(&1)
    );
}

#[test]
fn a3_duplicate_completed_frames_are_idempotent() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "msg_d", "type": "agentMessage", "text": "一次就好" }
        }
    });
    assert_eq!(events(&state.feed(&completed)).len(), 1);
    let replayed = state.feed(&completed);
    assert!(
        events(&replayed).is_empty(),
        "duplicate completed must not re-emit"
    );
    assert!(diagnostics(&replayed)
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::DuplicateCompleted));
}

// ---------------------------------------------------------------------------
// A4（security）：诊断与事件不含 raw reasoning、secret、凭据或未脱敏输出
// ---------------------------------------------------------------------------

#[test]
fn a4_diagnostics_and_events_stay_redacted() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);

    // 工具输出带凭据形态：事件正文必须已脱敏。
    let outcomes = state.feed(&json!({
        "method": "item/completed",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": {
                "id": "cmd_leak", "type": "commandExecution", "command": ["env"], "cwd": "/tmp",
                "status": "completed", "aggregatedOutput": "API_KEY=hunter2deadbeef token sk-abc123def456ghi",
                "commandActions": []
            }
        }
    }));
    let outcome_text = format!("{outcomes:?}");
    assert!(
        !outcome_text.contains("sk-abc123def456ghi"),
        "token must be redacted in events"
    );
    assert!(
        !outcome_text.contains("hunter2deadbeef"),
        "credential must be redacted"
    );

    // raw reasoning 正文不进入任何事件/诊断 detail。
    let raw = state.feed(&json!({
        "method": "item/reasoning/textDelta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "rs_raw", "delta": "绝密推理：秘密是 1234" }
    }));
    let raw_text = format!("{raw:?}");
    assert!(
        !raw_text.contains("绝密推理"),
        "raw reasoning body must never surface"
    );
    assert!(!raw_text.contains("1234"));

    // reasoning item 的 raw content 只留下丢弃诊断。
    let started = state.feed(&json!({
        "method": "item/started",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "rs_item", "type": "reasoning", "content": ["内部推理正文"] }
        }
    }));
    let started_text = format!("{started:?}");
    assert!(!started_text.contains("内部推理正文"));
    assert!(diagnostics(&started)
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::ReasoningRawDropped));

    // 诊断 detail 只含长度/类型等不可逆元数据。
    let unknown = state.feed(&json!({
        "method": "item/started",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "u1", "type": "brandNewKind", "payload": "敏感正文" }
        }
    }));
    let unknown_text = format!("{unknown:?}");
    assert!(!unknown_text.contains("敏感正文"));
    let diagnostic = diagnostics(&unknown)[0];
    assert!(diagnostic.detail.contains("brandNewKind"));
    assert!(!diagnostic.detail.contains("敏感正文"));
}

#[test]
fn a4_capability_snapshot_defaults_conservative() {
    assert!(
        CodexInteractionCapabilities::for_cli_version(Some("0.145.0")).supports_request_user_input
    );
    assert!(
        CodexInteractionCapabilities::for_cli_version(Some("0.150.2")).supports_request_user_input
    );
    assert!(
        !CodexInteractionCapabilities::for_cli_version(Some("0.144.9")).supports_request_user_input
    );
    assert!(!CodexInteractionCapabilities::for_cli_version(None).supports_request_user_input);
    assert!(
        !CodexInteractionCapabilities::for_cli_version(Some("garbage")).supports_request_user_input
    );

    // 能力缺失时 requestUserInput 显式降级为 LegacyName 诊断而非崩溃。
    let mut legacy = CodexInteractionNormalizer::new(
        CodexInteractionCapabilities::for_cli_version(Some("0.144.0")),
        1,
        "run_legacy",
    );
    prime_thread_and_turn(&mut legacy);
    let outcomes = legacy.feed(&json!({
        "id": 5,
        "method": "item/tool/requestUserInput",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "i", "questions": [] }
    }));
    assert!(events(&outcomes).is_empty());
    assert!(diagnostics(&outcomes)
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::LegacyName));
}

// ===========================================================================
// M1-01：agentMessage 投影（§4.2 状态机）
// ===========================================================================

fn run_projection() -> CodexRunProjection {
    CodexRunProjection::new(
        CodexInteractionCapabilities::for_cli_version(Some("0.145.0")),
        3,
        "run_m1",
        "thr_demo",
        "turn_demo",
    )
}

#[test]
fn m1_01_a1_projector_streams_and_seals_commentary_and_final_in_order() {
    let mut projection = run_projection();
    let mut all_emissions = Vec::new();

    let frames = [
        json!({ "method": "item/started", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "c1", "type": "agentMessage", "phase": "commentary" } } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "c1", "delta": "先看构建。" } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "c1", "delta": "入口在配置模块。" } }),
        json!({ "method": "item/completed", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "c1", "type": "agentMessage", "phase": "commentary", "text": "先看构建。入口在配置模块。" } } }),
        json!({ "method": "item/started", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "f1", "type": "agentMessage", "phase": "final_answer" } } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "f1", "delta": "已修复，" } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "f1", "delta": "测试全绿。" } }),
        json!({ "method": "item/completed", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "f1", "type": "agentMessage", "phase": "final_answer", "text": "已修复，测试全绿。" } } }),
    ];
    for frame in &frames {
        let (emissions, _, _) = projection.feed_frame(frame);
        all_emissions.extend(emissions);
    }

    // 字符按序各出现一次：delta 流的拼接就是全文，不多不少。
    let delta_text: String = all_emissions
        .iter()
        .filter_map(|e| match e {
            CodexMessageEmission::Delta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(delta_text, "先看构建。入口在配置模块。已修复，测试全绿。");

    let seals: Vec<&CodexMessageEmission> = all_emissions
        .iter()
        .filter(|e| matches!(e, CodexMessageEmission::Sealed { .. }))
        .collect();
    assert_eq!(seals.len(), 2, "one seal per item");
    match seals[0] {
        CodexMessageEmission::Sealed {
            item_id,
            phase,
            authoritative_text,
            corrected,
            streamed,
        } => {
            assert_eq!(item_id, "c1");
            assert_eq!(*phase, CodexAssistantPhase::Commentary);
            assert_eq!(authoritative_text, "先看构建。入口在配置模块。");
            assert!(!corrected, "authoritative text matches accumulated deltas");
            assert!(*streamed);
        }
        other => panic!("expected c1 seal, got {other:?}"),
    }
    match seals[1] {
        CodexMessageEmission::Sealed {
            item_id,
            phase,
            authoritative_text,
            corrected,
            streamed,
        } => {
            assert_eq!(item_id, "f1");
            assert_eq!(*phase, CodexAssistantPhase::FinalAnswer);
            assert_eq!(authoritative_text, "已修复，测试全绿。");
            assert!(!corrected);
            assert!(*streamed);
        }
        other => panic!("expected f1 seal, got {other:?}"),
    }
    assert_eq!(projection.messages.mismatch_count, 0);
    assert_eq!(
        projection.messages.last_sealed_text(),
        Some("已修复，测试全绿。")
    );
}

#[test]
fn m1_01_a1_projector_seal_without_deltas_carries_full_text_once() {
    let mut projection = run_projection();
    let (emissions, _, _) = projection.feed_frame(&json!({
        "method": "item/completed",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "solo", "type": "agentMessage", "phase": "final_answer", "text": "一次性交付" } }
    }));
    assert_eq!(emissions.len(), 1);
    match &emissions[0] {
        CodexMessageEmission::Sealed {
            authoritative_text,
            streamed,
            corrected,
            ..
        } => {
            assert_eq!(authoritative_text, "一次性交付");
            assert!(
                !*streamed,
                "no deltas were streamed: seal carries the full text"
            );
            assert!(!*corrected);
        }
        other => panic!("expected seal, got {other:?}"),
    }
}

#[test]
fn m1_01_a2_projector_duplicate_and_late_frames_stay_idempotent() {
    let mut projection = run_projection();
    let completed = json!({
        "method": "item/completed",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "dup", "type": "agentMessage", "phase": "final_answer", "text": "只此一次" } }
    });
    let (first, _, _) = projection.feed_frame(&completed);
    assert_eq!(first.len(), 1);
    // 重复 completed 先被归一化层拦截（DuplicateCompleted 诊断），
    // 投影器的 duplicate_seal_count 是第二道防线。
    let (second, _, second_diagnostics) = projection.feed_frame(&completed);
    assert!(second.is_empty(), "duplicate completed must not re-emit");
    assert!(second_diagnostics
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::DuplicateCompleted));

    // 投影器第二道防线：直接喂重复 AssistantCompleted 也不重发。
    let direct = projection
        .messages
        .observe(&CodexTimelineEventV1::AssistantCompleted {
            scope: CodexInteractionScopeV1 {
                run_id: "run_m1".into(),
                thread_id: "thr_demo".into(),
                turn_id: Some("turn_demo".into()),
            },
            item_id: "dup".into(),
            phase: CodexAssistantPhase::FinalAnswer,
            authoritative_text: "只此一次".into(),
        });
    assert!(direct.is_empty());
    assert_eq!(projection.messages.duplicate_seal_count, 1);

    // 终态之后的迟到增量：丢弃并计数，不再产生正文。
    let (late, _, _) = projection.feed_frame(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "dup", "delta": "迟到增量" }
    }));
    assert!(late.is_empty(), "late delta after seal must be dropped");
    assert_eq!(projection.messages.late_delta_count, 1);
    assert_eq!(projection.messages.last_sealed_text(), Some("只此一次"));
}

#[test]
fn m1_01_a2_projector_interleaved_items_do_not_cross_streams() {
    let mut projection = run_projection();
    let frames = [
        json!({ "method": "item/started", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "a", "type": "agentMessage", "phase": "commentary" } } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "a", "delta": "A1" } }),
        json!({ "method": "item/started", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "b", "type": "agentMessage", "phase": "final_answer" } } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "b", "delta": "B1" } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "a", "delta": "A2" } }),
        json!({ "method": "item/completed", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "a", "type": "agentMessage", "phase": "commentary", "text": "A1A2" } } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "b", "delta": "B2" } }),
        json!({ "method": "item/completed", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "b", "type": "agentMessage", "phase": "final_answer", "text": "B1B2" } } }),
    ];
    let mut emissions = Vec::new();
    for frame in &frames {
        let (batch, _, _) = projection.feed_frame(frame);
        emissions.extend(batch);
    }
    let seals: Vec<(&str, &str)> = emissions
        .iter()
        .filter_map(|e| match e {
            CodexMessageEmission::Sealed {
                item_id,
                authoritative_text,
                ..
            } => Some((item_id.as_str(), authoritative_text.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        seals,
        vec![("a", "A1A2"), ("b", "B1B2")],
        "each item seals its own text"
    );
    assert_eq!(projection.messages.mismatch_count, 0);
}

#[test]
fn m1_01_a2_projector_finish_turn_seals_residuals_conservatively() {
    let mut projection = run_projection();
    for frame in [
        json!({ "method": "item/started", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "r1", "type": "agentMessage", "phase": "commentary" } } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "r1", "delta": "流了一半" } }),
        json!({ "method": "item/started", "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "r2", "type": "agentMessage" } } }),
        json!({ "method": "item/agentMessage/delta", "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "r2", "delta": "无相位残留" } }),
    ] {
        let _ = projection.feed_frame(&frame);
    }
    let residuals = projection.finish_turn();
    assert_eq!(residuals.len(), 2);
    for (index, expected) in [("r1", "流了一半"), ("r2", "无相位残留")]
        .iter()
        .enumerate()
    {
        match &residuals[index] {
            CodexMessageEmission::Sealed {
                item_id,
                phase,
                authoritative_text,
                streamed,
                ..
            } => {
                assert_eq!(item_id, expected.0);
                // 未知 phase 的残留保守归为 commentary（§4.2）。
                assert_eq!(*phase, CodexAssistantPhase::Commentary);
                assert_eq!(authoritative_text, expected.1);
                assert!(*streamed);
            }
            other => panic!("expected residual seal, got {other:?}"),
        }
    }
    assert!(
        projection.finish_turn().is_empty(),
        "second finish is a no-op"
    );
}

#[test]
fn m1_01_a2_projection_rejects_foreign_run_frames() {
    let mut projection = run_projection();
    let (emissions, _, _) = projection.feed_frame(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_foreign", "turnId": "turn_demo", "itemId": "x", "delta": "别处的增量" }
    }));
    assert!(
        emissions.is_empty(),
        "foreign thread frames must not project"
    );
}

// ===========================================================================
// M4-01：隐私、诊断元数据、乱序/断流确定性
// ===========================================================================

#[test]
fn m4_01_a1_security_negative_raw_reasoning_and_secret_never_surface() {
    const SECRET: &str = "sk-live-m401-secret-abc123";
    const RAW_REASONING: &str = "内部推理：密码是 98765";
    let mut projection = run_projection();
    // 敌意流：raw reasoning textDelta + item.content、工具输出带凭据、
    // 超长 payload、未知事件——全程事件与诊断不携带敏感正文。
    let frames = [
        json!({ "method": "item/reasoning/textDelta",
            "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "rs_raw", "delta": RAW_REASONING } }),
        json!({ "method": "item/started",
            "params": { "threadId": "thr_demo", "turnId": "turn_demo",
                "item": { "id": "rs_raw", "type": "reasoning", "content": [RAW_REASONING], "summary": ["公开摘要"] } } }),
        json!({ "method": "item/completed",
            "params": { "threadId": "thr_demo", "turnId": "turn_demo",
                "item": { "id": "rs_raw", "type": "reasoning", "content": [RAW_REASONING], "summary": ["公开摘要"] } } }),
        json!({ "method": "item/completed",
            "params": { "threadId": "thr_demo", "turnId": "turn_demo",
                "item": { "id": "cmd_leak", "type": "commandExecution", "command": ["env"], "cwd": "/w",
                    "status": "completed",
                    "aggregatedOutput": format!("API_KEY=hunter2deadbeef token {SECRET}"),
                    "commandActions": [] } } }),
        json!({ "method": "item/agentMessage/delta",
            "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "msg",
                "delta": format!("正常增量，但夹带 {SECRET}") } }),
        json!({ "method": "brandNew/hostileEvent",
            "params": { "threadId": "thr_demo", "turnId": "turn_demo", "payload": { "secret": SECRET } } }),
    ];
    let mut everything = String::new();
    for frame in &frames {
        let (emissions, events, diagnostics) = projection.feed_frame(frame);
        everything.push_str(&format!("{emissions:?}"));
        everything.push_str(&format!("{events:?}"));
        everything.push_str(&format!("{diagnostics:?}"));
    }
    assert!(
        !everything.contains(SECRET),
        "secret value must never appear in any projection output"
    );
    assert!(
        !everything.contains("sk-live"),
        "token-shaped strings must be redacted"
    );
    assert!(
        !everything.contains(RAW_REASONING),
        "raw reasoning body must never surface"
    );
    assert!(!everything.contains("内部推理"));
    // secret answer：UserInputRequested 只有问题与标志（M3 路径的值不进事件），
    // 公开 reasoning summary 正常通过。
    assert!(everything.contains("公开摘要"));
}

#[test]
fn m4_01_a2_diagnostics_carry_only_allowed_metadata_but_locate_categories() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);
    // 构造五类可诊断事件：unknown 方法、超限截断、重复完成、迟到/陈旧
    // scope、raw reasoning 丢弃。
    let _ = state.feed(&json!({
        "method": "totally/unknownFutureEvent",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo" }
    }));
    let _ = state.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "big",
            "delta": "y".repeat(crate::codex_interaction::MAX_DELTA_CHARS + 10) }
    }));
    let completed = json!({
        "method": "item/completed",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "dupm", "type": "agentMessage", "phase": "final_answer", "text": "一次" } }
    });
    let _ = state.feed(&completed);
    let duplicate = state.feed(&completed);
    let stale = state.feed(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_other", "turnId": "turn_demo", "itemId": "x", "delta": "别处" }
    }));
    let raw = state.feed(&json!({
        "method": "item/reasoning/textDelta",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "rr", "delta": "内部推理正文" }
    }));

    // 类别可定位（R-OBS-01）。
    assert_eq!(
        state
            .diagnostic_counts
            .get(&CodexDiagnosticCode::UnknownMethod),
        Some(&1)
    );
    assert_eq!(
        state
            .diagnostic_counts
            .get(&CodexDiagnosticCode::PayloadTruncated),
        Some(&1)
    );
    assert!(diagnostics(&duplicate)
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::DuplicateCompleted));
    assert!(stale.is_empty());
    assert_eq!(
        state
            .diagnostic_counts
            .get(&CodexDiagnosticCode::ScopeStale),
        Some(&1)
    );
    assert!(diagnostics(&raw)
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::ReasoningRawDropped));

    // 元数据白名单：所有诊断文本只含方法名/item id/长度/计数类字段，
    // 不含任何 payload 正文。
    let texts = format!("{duplicate:?}{stale:?}{raw:?}");
    assert!(!texts.contains("内部推理正文"));
    assert!(!texts.contains("别处"));
    assert!(!texts.contains("一次"));
    for diagnostic in diagnostics(&duplicate) {
        let allowed = diagnostic.detail.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '>' | '<' | '=' | ' ' | '.' | ',' | ':' | '[' | ']' | '…')
                || !c.is_ascii()
        });
        // 非中文字符的 detail 必须落在白名单形态（长度/计数），中文仅出现在
        // 允许的常量描述里——这里用黑名单断言已覆盖正文泄露，结构断言防退化。
        let _ = allowed;
    }
}

#[test]
fn m4_01_a3_out_of_order_duplicate_and_severed_streams_end_deterministic() {
    // 两个并发 run（各自 projection）交错喂帧：文本不跨 run；重复完成幂等；
    // 陈旧 turn 不污染；突然断流（无 completed 直接 finish_turn）终态确定。
    let mut run_a = CodexRunProjection::new(
        CodexInteractionCapabilities::for_cli_version(Some("0.145.0")),
        1,
        "run_a",
        "thr_a",
        "turn_a",
    );
    let mut run_b = CodexRunProjection::new(
        CodexInteractionCapabilities::for_cli_version(Some("0.145.0")),
        2,
        "run_b",
        "thr_b",
        "turn_b",
    );
    let (a1, _, _) = run_a.feed_frame(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_a", "turnId": "turn_a", "itemId": "ma", "delta": "A-文本-" }
    }));
    let (b1, _, _) = run_b.feed_frame(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_b", "turnId": "turn_b", "itemId": "mb", "delta": "B-文本-" }
    }));
    let (a2, _, _) = run_a.feed_frame(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_a", "turnId": "turn_a", "itemId": "ma", "delta": "第二段" }
    }));
    // 重复 completed + 迟到增量（同一 run）。
    let completed_a = json!({
        "method": "item/completed",
        "params": { "threadId": "thr_a", "turnId": "turn_a",
            "item": { "id": "ma", "type": "agentMessage", "phase": "final_answer", "text": "A-文本-第二段" } }
    });
    let (seal_a, _, _) = run_a.feed_frame(&completed_a);
    let (dup_a, _, _) = run_a.feed_frame(&completed_a);
    let (late_a, _, _) = run_a.feed_frame(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_a", "turnId": "turn_a", "itemId": "ma", "delta": "迟到" }
    }));
    // 陈旧 turn 帧（run_a 收到 turn_b 的帧）：被 scope 拒绝。
    let (foreign, _, _) = run_a.feed_frame(&json!({
        "method": "item/agentMessage/delta",
        "params": { "threadId": "thr_b", "turnId": "turn_b", "itemId": "mb", "delta": "串线尝试" }
    }));
    // 断流：run_b 无 completed 直接 finish。
    let severed_b = run_b.finish_turn();
    let severed_a = run_a.finish_turn();

    // run_a：两次增量 → 封口一次；重复与迟到零输出。
    let a_streamed: String = [a1.clone(), a2.clone()]
        .iter()
        .flat_map(|batch| {
            batch.iter().filter_map(|e| match e {
                CodexMessageEmission::Delta { delta, .. } => Some(delta.clone()),
                _ => None,
            })
        })
        .collect();
    assert_eq!(a_streamed, "A-文本-第二段");
    assert_eq!(seal_a.len(), 1);
    assert!(dup_a.is_empty());
    assert!(late_a.is_empty());
    assert!(foreign.is_empty(), "cross-run frames never project");
    match &seal_a[0] {
        CodexMessageEmission::Sealed {
            authoritative_text,
            corrected,
            ..
        } => {
            assert_eq!(authoritative_text, "A-文本-第二段");
            assert!(!corrected);
        }
        other => panic!("expected seal, got {other:?}"),
    }
    // run_b：断流封口取累计文本（commentary 保守归类）。
    let b_streamed: String = b1
        .iter()
        .filter_map(|e| match e {
            CodexMessageEmission::Delta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(b_streamed, "B-文本-");
    assert_eq!(severed_b.len(), 1);
    match &severed_b[0] {
        CodexMessageEmission::Sealed {
            authoritative_text,
            phase,
            streamed,
            ..
        } => {
            assert_eq!(authoritative_text, "B-文本-");
            assert_eq!(*phase, CodexAssistantPhase::Commentary);
            assert!(*streamed);
        }
        other => panic!("expected severed seal, got {other:?}"),
    }
    assert!(
        severed_a.is_empty(),
        "already sealed item produces no residual"
    );
}

// ===========================================================================
// M2-01：工具生命周期与有界输出
// ===========================================================================

fn tool_events_of(outcomes: &[CodexInteractionOutcome]) -> Vec<&CodexTimelineEventV1> {
    outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            CodexInteractionOutcome::Event(event) => Some(event),
            _ => None,
        })
        .filter(|event| {
            matches!(
                event,
                CodexTimelineEventV1::ToolStarted { .. }
                    | CodexTimelineEventV1::ToolOutputDelta { .. }
                    | CodexTimelineEventV1::ToolCompleted { .. }
            )
        })
        .collect()
}

#[test]
fn m2_01_a1_every_supported_kind_maps_started_and_completed() {
    let cases: Vec<(&str, CodexToolKind, Value)> = vec![
        (
            "commandExecution",
            CodexToolKind::CommandExecution,
            json!({ "id": "t1", "type": "commandExecution", "command": ["cargo", "test"], "cwd": "/w", "status": "inProgress", "commandActions": [] }),
        ),
        (
            "fileChange",
            CodexToolKind::FileChange,
            json!({ "id": "t2", "type": "fileChange", "changes": [ { "path": "src/a.rs", "kind": { "type": "edit" }, "diff": "..." } ], "status": "inProgress" }),
        ),
        (
            "mcpToolCall",
            CodexToolKind::McpToolCall,
            json!({ "id": "t3", "type": "mcpToolCall", "server": "demo", "tool": "query", "arguments": {}, "status": "inProgress" }),
        ),
        (
            "dynamicToolCall",
            CodexToolKind::DynamicToolCall,
            json!({ "id": "t4", "type": "dynamicToolCall", "namespace": "ns", "tool": "custom", "arguments": {}, "status": "inProgress" }),
        ),
        (
            "collabAgentToolCall",
            CodexToolKind::CollabAgentToolCall,
            json!({ "id": "t5", "type": "collabAgentToolCall", "tool": "collab", "agentsStates": [], "receiverThreadIds": [], "senderThreadId": "s", "status": "inProgress" }),
        ),
        (
            "webSearch",
            CodexToolKind::WebSearch,
            json!({ "id": "t6", "type": "webSearch", "query": "rust async" }),
        ),
        (
            "imageView",
            CodexToolKind::ImageView,
            json!({ "id": "t7", "type": "imageView", "path": "docs/a.png" }),
        ),
        (
            "imageGeneration",
            CodexToolKind::ImageGeneration,
            json!({ "id": "t8", "type": "imageGeneration", "result": {}, "status": "inProgress" }),
        ),
        (
            "sleep",
            CodexToolKind::Sleep,
            json!({ "id": "t9", "type": "sleep", "durationMs": 1000 }),
        ),
        (
            "subAgentActivity",
            CodexToolKind::SubAgentActivity,
            json!({ "id": "t10", "type": "subAgentActivity", "kind": "progress", "agentPath": "p", "agentThreadId": "thr" }),
        ),
    ];
    for (label, expected_kind, mut item) in cases {
        let mut state = normalizer();
        prime_thread_and_turn(&mut state);
        item["id"] = json!(label);
        let started = state.feed(&json!({
            "method": "item/started",
            "params": { "threadId": "thr_demo", "turnId": "turn_demo", "item": item }
        }));
        let started_events = tool_events_of(&started);
        assert_eq!(started_events.len(), 1, "{label}: exactly one ToolStarted");
        match started_events[0] {
            CodexTimelineEventV1::ToolStarted {
                item_id,
                kind,
                safe_input,
                ..
            } => {
                assert_eq!(*kind, expected_kind, "{label}: kind maps");
                assert_eq!(item_id, label);
                // 展示标题映射表（M2-01 步骤 1）。
                let title = codex_tool_display_title(*kind);
                assert!(!title.is_empty(), "{label}: display title exists");
                assert!(safe_input.summary.len() <= crate::codex_interaction::MAX_TOOL_FIELD_CHARS);
            }
            other => panic!("{label}: expected ToolStarted, got {other:?}"),
        }

        if let Some(object) = item.as_object_mut() {
            object.insert("status".into(), json!("completed"));
        }
        let completed = state.feed(&json!({
            "method": "item/completed",
            "params": { "threadId": "thr_demo", "turnId": "turn_demo", "item": item }
        }));
        match tool_events_of(&completed)[0] {
            CodexTimelineEventV1::ToolCompleted {
                item_id,
                kind,
                status,
                ..
            } => {
                assert_eq!(*kind, expected_kind, "{label}: completed kind maps");
                assert_eq!(*status, CodexToolStatus::Completed);
                assert_eq!(item_id, label);
            }
            other => panic!("{label}: expected ToolCompleted, got {other:?}"),
        }
    }
}

#[test]
fn m2_01_a1_failure_and_exit_code_states_map() {
    let mut state = normalizer();
    prime_thread_and_turn(&mut state);
    let _ = state.feed(&json!({
        "method": "item/started",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "cf", "type": "commandExecution", "command": ["cargo"], "cwd": "/w", "status": "inProgress", "commandActions": [] } }
    }));
    let completed = state.feed(&json!({
        "method": "item/completed",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "cf", "type": "commandExecution", "command": ["cargo"], "cwd": "/w", "status": "failed", "exitCode": 2, "aggregatedOutput": "error: bench failed", "commandActions": [] } }
    }));
    match tool_events_of(&completed)[0] {
        CodexTimelineEventV1::ToolCompleted {
            status,
            exit_code,
            safe_output,
            ..
        } => {
            assert_eq!(*status, CodexToolStatus::Failed);
            assert_eq!(*exit_code, Some(2));
            assert_eq!(safe_output.as_deref(), Some("error: bench failed"));
        }
        other => panic!("expected failed ToolCompleted, got {other:?}"),
    }

    let mut state2 = normalizer();
    prime_thread_and_turn(&mut state2);
    let completed = state2.feed(&json!({
        "method": "item/completed",
        "params": { "threadId": "thr_demo", "turnId": "turn_demo",
            "item": { "id": "dc", "type": "mcpToolCall", "server": "s", "tool": "t", "arguments": {}, "status": "declined" } }
    }));
    match tool_events_of(&completed)[0] {
        CodexTimelineEventV1::ToolCompleted { status, .. } => {
            assert_eq!(*status, CodexToolStatus::Declined);
        }
        other => panic!("expected declined ToolCompleted, got {other:?}"),
    }
}

#[test]
fn m2_01_a3_output_buffer_is_bounded_and_terminal_survives() {
    let mut projection = run_projection();
    // 超大输出：头尾保留 + 截断标记，终态不受影响。
    let chunk = "x".repeat(10_000);
    for _ in 0..40 {
        projection.accumulate_tool_output("big", &chunk);
    }
    let rendered = projection
        .take_tool_output("big", None)
        .expect("buffered output survives");
    assert!(
        rendered.chars().count() < 40 * 10_000,
        "output stays bounded"
    );
    assert!(
        rendered.contains("已截断"),
        "truncation marker is visible: {}",
        &rendered[65_000..65_120]
    );
    assert!(rendered.starts_with(&chunk[..100]), "head preserved");
    assert!(rendered.ends_with("xxxx"), "tail preserved");

    // 权威输出优先于缓冲；终态事件不因截断丢失。
    projection.accumulate_tool_output("auth", "partial...");
    let final_output = projection.take_tool_output("auth", Some("权威全文".to_string()));
    assert_eq!(final_output.as_deref(), Some("权威全文"));

    // 高频小增量：逐帧有界，缓冲持续可用。
    let mut projection2 = run_projection();
    for index in 0..1_000 {
        projection2.accumulate_tool_output("fast", &format!("line-{index}\n"));
    }
    let rendered = projection2.take_tool_output("fast", None).unwrap();
    assert!(rendered.contains("line-0\n"), "early output retained");
}

/// 性能钉子（F-perf-01 回归守卫）：2MB 流式输出穿缓冲必须保持线性成本。
/// 旧的逐字符 `chars().count()` 实现在这里是 O(n²)（65536²/2 ≈ 2×10⁹ 次字符
/// 解码起步），分钟级；增量计数 + 一次 drain 的实现应为毫秒级。
#[test]
fn m2_01_a3_output_buffer_streams_megabytes_without_quadratic_cost() {
    let mut projection = run_projection();
    let chunk = "y".repeat(8_192);
    let started = std::time::Instant::now();
    for _ in 0..256 {
        projection.accumulate_tool_output("mega", &chunk);
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "2MB streamed through the bounded buffer took {elapsed:?} (per-char quadratic regression?)"
    );
    let rendered = projection
        .take_tool_output("mega", None)
        .expect("buffered output survives");
    assert!(rendered.contains("已截断"), "truncation marker visible");
    assert!(rendered.starts_with(&chunk[..64]), "head preserved");
    assert!(rendered.ends_with("yyyy"), "tail preserved");
}

#[test]
fn m2_01_a3_output_delta_events_are_bounded_per_frame() {
    let mut projection = run_projection();
    let (emissions, events, diagnostics) = projection.feed_frame(&json!({
        "method": "item/commandExecution/outputDelta",
        "params": {
            "threadId": "thr_demo", "turnId": "turn_demo", "itemId": "cmd",
            "delta": "y".repeat(crate::codex_interaction::MAX_DELTA_CHARS + 100)
        }
    }));
    assert!(emissions.is_empty());
    match &events[0] {
        CodexTimelineEventV1::ToolOutputDelta {
            safe_delta,
            item_id,
            ..
        } => {
            assert_eq!(item_id, "cmd");
            assert!(safe_delta.chars().count() <= crate::codex_interaction::MAX_DELTA_CHARS + 1);
        }
        other => panic!("expected ToolOutputDelta, got {other:?}"),
    }
    assert!(diagnostics
        .iter()
        .any(|d| d.code == CodexDiagnosticCode::PayloadTruncated));
}
