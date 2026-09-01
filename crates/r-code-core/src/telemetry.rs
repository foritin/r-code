//! TelemetryContext 契约（docs/pi-alignment PRD §4.1 R-TEL-01 / M6-01）。
//!
//! 统一双引擎（原生 LlmAgentRuntime + Codex 适配层）的观测合同：
//! [`TelemetryContext`] 提供开 Span 的唯一入口；Span 携带 start/end
//! attributes、属性/事件/状态。默认 [`NOOP`]（零开销——所有方法是空体，
//! 内联后无成本）；[`InMemory`] 供测试与一致性基准（M6-03）断言。
//!
//! 设计约束：
//! - **零依赖**：不引 opentelemetry/tracing-opentelemetry——契约自持，未来
//!   接 OTLP 时实现一个导出型 Context 即可，消费方（双引擎打点，M6-02）
//!   不感知；
//! - **默认 NOOP**：宿主未显式安装 Context 时一切打点退化为空操作（PRD
//!   冻结决策 §2.7：遥测默认 NOOP）；
//! - **同步 API**：Span 开始/结束在请求路径上，异步导出留给实现方。

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Span 状态（OpenTelemetry 语义子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanStatus {
    #[default]
    Unset,
    Ok,
    Error,
}

/// 单个事件（时间点 + 名 + 属性）。
#[derive(Debug, Clone, PartialEq)]
pub struct SpanEvent {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub timestamp_unix_ms: u128,
}

/// 单个 Span 的完整记录（InMemory 断言用）。
#[derive(Debug, Clone, PartialEq)]
pub struct SpanRecord {
    pub name: String,
    /// start 时刻携带的 attributes。
    pub start_attributes: Vec<(String, String)>,
    /// end 时补充的 attributes（与 start 合并语义：后写覆盖同键）。
    pub end_attributes: Vec<(String, String)>,
    pub attributes: Vec<(String, String)>,
    pub events: Vec<SpanEvent>,
    pub status: SpanStatus,
    pub started_at_unix_ms: u128,
    pub ended_at_unix_ms: Option<u128>,
}

impl SpanRecord {
    /// 按 key 取属性（end 覆盖动态、动态覆盖 start）。
    pub fn attribute(&self, key: &str) -> Option<&str> {
        fn lookup<'a>(list: &'a [(String, String)], key: &str) -> Option<&'a str> {
            list.iter()
                .rev()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        }
        lookup(&self.end_attributes, key)
            .or_else(|| lookup(&self.attributes, key))
            .or_else(|| lookup(&self.start_attributes, key))
    }
}

/// 属性值（保持字符串化——契约不做类型系统，序列化边界由实现方定）。
pub fn attr(value: impl ToString) -> String {
    value.to_string()
}

/// Span 句柄：drop 不自动 end（调用方必须显式 `end`——请求路径上漏 end
/// 是 bug，静默吞掉会让 InMemory 断言失败而不是默默丢数据）。
pub struct Span {
    context_kind: ContextKind,
    record: Option<SpanRecord>,
}

enum ContextKind {
    Noop,
    InMemory { sink: Arc<Mutex<Vec<SpanRecord>>> },
}

impl Span {
    /// 追加属性（Span 打开期间）。
    pub fn set_attribute(&mut self, key: &str, value: impl ToString) {
        if let Some(record) = &mut self.record {
            record.attributes.push((key.to_string(), value.to_string()));
        }
    }

    /// 记录事件。
    pub fn add_event(&mut self, name: &str, attributes: Vec<(String, String)>) {
        if let Some(record) = &mut self.record {
            record.events.push(SpanEvent {
                name: name.to_string(),
                attributes,
                timestamp_unix_ms: now_unix_ms(),
            });
        }
    }

    /// 设置状态。
    pub fn set_status(&mut self, status: SpanStatus) {
        if let Some(record) = &mut self.record {
            record.status = status;
        }
    }

    /// 结束 Span（补 end attributes；重复 end 是 no-op）。
    pub fn end_with(&mut self, end_attributes: Vec<(String, String)>) {
        if let Some(mut record) = self.record.take() {
            record.ended_at_unix_ms = Some(now_unix_ms());
            record.end_attributes = end_attributes;
            if let ContextKind::InMemory { sink } = &self.context_kind {
                if let Ok(mut sink) = sink.lock() {
                    sink.push(record);
                }
            }
        }
    }

    pub fn end(&mut self) {
        self.end_with(Vec::new());
    }
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// 遥测上下文（trait object 形态：宿主可安装导出实现；默认 NOOP）。
pub struct TelemetryContext {
    kind: ContextKind,
}

impl Default for TelemetryContext {
    /// 默认 NOOP（零开销）。
    fn default() -> Self {
        NOOP.clone()
    }
}

impl Clone for TelemetryContext {
    fn clone(&self) -> Self {
        match &self.kind {
            ContextKind::Noop => TelemetryContext {
                kind: ContextKind::Noop,
            },
            ContextKind::InMemory { sink } => TelemetryContext {
                kind: ContextKind::InMemory { sink: sink.clone() },
            },
        }
    }
}

impl TelemetryContext {
    /// 开始一个 Span。
    pub fn span(&self, name: &str, start_attributes: Vec<(String, String)>) -> Span {
        match &self.kind {
            ContextKind::Noop => Span {
                context_kind: ContextKind::Noop,
                record: None,
            },
            ContextKind::InMemory { sink } => Span {
                context_kind: ContextKind::InMemory { sink: sink.clone() },
                record: Some(SpanRecord {
                    name: name.to_string(),
                    start_attributes,
                    attributes: Vec::new(),
                    end_attributes: Vec::new(),
                    events: Vec::new(),
                    status: SpanStatus::Unset,
                    started_at_unix_ms: now_unix_ms(),
                    ended_at_unix_ms: None,
                }),
            },
        }
    }

    /// 是否为 NOOP（调用方可据此完全跳过属性构造成本）。
    pub fn is_noop(&self) -> bool {
        matches!(self.kind, ContextKind::Noop)
    }
}

/// 全局默认 NOOP 上下文（零开销：span() 返回空 Span，一切方法 no-op）。
pub static NOOP: TelemetryContext = TelemetryContext {
    kind: ContextKind::Noop,
};

/// InMemory 实现（测试参考 + M6-03 一致性基准）。
pub fn in_memory() -> (TelemetryContext, InMemoryHandle) {
    let sink: Arc<Mutex<Vec<SpanRecord>>> = Arc::new(Mutex::new(Vec::new()));
    (
        TelemetryContext {
            kind: ContextKind::InMemory { sink: sink.clone() },
        },
        InMemoryHandle { sink },
    )
}

/// InMemory 上下文的读取端。
pub struct InMemoryHandle {
    sink: Arc<Mutex<Vec<SpanRecord>>>,
}

impl InMemoryHandle {
    /// 已结束的 Span 记录（按结束顺序）。
    pub fn records(&self) -> Vec<SpanRecord> {
        self.sink
            .lock()
            .map(|sink| sink.clone())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.sink.lock().map(|sink| sink.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 双引擎统一打点（PRD §4.1 R-TEL-02 / M6-02）：原生 LlmAgentRuntime 与
/// Codex 适配层共用两条 Span——`r_code.ai.request`（一次模型请求）与
/// `r_code.harness.run`（一次 agent 运行）。同构字段：`engine`
/// （native|codex）、`provider`、`model`；usage 归因键（input/output/
/// cache_read/cache_write/cost_usd tokens）从 usage_json 提取为 end attrs。
pub const SPAN_AI_REQUEST: &str = "r_code.ai.request";
pub const SPAN_HARNESS_RUN: &str = "r_code.harness.run";

/// 引擎标识（同构 span 的第一属性）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Native,
    Codex,
}

impl EngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Codex => "codex",
        }
    }
}

/// 开一条 ai.request Span（双引擎同构 start attrs）。
pub fn ai_request_span(
    context: &TelemetryContext,
    engine: EngineKind,
    provider: &str,
    model: &str,
) -> Span {
    context.span(
        SPAN_AI_REQUEST,
        vec![
            ("engine".to_string(), engine.as_str().to_string()),
            ("provider".to_string(), provider.to_string()),
            ("model".to_string(), model.to_string()),
        ],
    )
}

/// 开一条 harness.run Span。
pub fn harness_run_span(context: &TelemetryContext, engine: EngineKind, run_id: &str) -> Span {
    context.span(
        SPAN_HARNESS_RUN,
        vec![
            ("engine".to_string(), engine.as_str().to_string()),
            ("run_id".to_string(), run_id.to_string()),
        ],
    )
}

/// usage_json → Span end attributes（M6-02.A2：归因从 Span 提取）。
/// 提取 input/output/cache_read/cache_write tokens 与 cost_usd；缺键跳过。
pub fn usage_end_attributes(usage_json: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(usage_json) else {
        return Vec::new();
    };
    let mut attributes = Vec::new();
    for key in [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
    ] {
        if let Some(number) = value.get(key).and_then(|item| item.as_u64()) {
            attributes.push((key.to_string(), number.to_string()));
        }
    }
    if let Some(cost) = value.get("cost_usd").and_then(|item| item.as_f64()) {
        attributes.push(("cost_usd".to_string(), format!("{cost:.6}")));
    }
    attributes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M6-01.A1：契约完整——Span + start/end attributes + 属性/事件/状态。
    #[test]
    fn span_carries_full_contract_surface() {
        let (context, handle) = in_memory();
        let mut span = context.span(
            "r_code.ai.request",
            vec![("provider".to_string(), "deepseek".to_string())],
        );
        span.set_attribute("model", "deepseek-v4");
        span.add_event("stream.started", vec![]);
        span.set_status(SpanStatus::Ok);
        span.end_with(vec![("usage_tokens".to_string(), "150".to_string())]);

        let records = handle.records();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.name, "r_code.ai.request");
        assert_eq!(record.attribute("provider"), Some("deepseek"));
        assert_eq!(record.attribute("model"), Some("deepseek-v4"));
        // end 覆盖语义：end 后属性仍可读。
        assert_eq!(record.attribute("usage_tokens"), Some("150"));
        assert_eq!(record.events.len(), 1);
        assert_eq!(record.status, SpanStatus::Ok);
        assert!(record.ended_at_unix_ms.is_some());
    }

    /// M6-01.A3：NOOP 默认零开销——不产生记录、is_noop 可判、方法全 no-op。
    #[test]
    fn noop_default_is_silent_and_detectable() {
        let context = TelemetryContext::default();
        assert!(context.is_noop());
        let mut span = context.span("anything", vec![]);
        span.set_attribute("k", "v");
        span.add_event("e", vec![]);
        span.set_status(SpanStatus::Error);
        span.end();
        // NOOP 没有任何输出面——静态保证：Span.record 为 None（无分配路径）。
        // 用行为验证：同一 Context 再开 Span 也不累积任何可见状态。
        let another = context.span("x", vec![]);
        drop(another);
        assert!(context.is_noop());
    }

    /// M6-01.A2：InMemory 可断言——end 顺序即记录顺序；重复 end 不重复入账。
    #[test]
    fn in_memory_records_in_end_order() {
        let (context, handle) = in_memory();
        let mut first = context.span("first", vec![]);
        let mut second = context.span("second", vec![]);
        second.end();
        first.end();
        first.end(); // 重复 end：no-op。
        let records = handle.records();
        assert_eq!(
            records.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["second", "first"],
            "记录按结束顺序"
        );
        assert_eq!(handle.len(), 2);
    }

    /// Clone 语义：InMemory clone 共享 sink；NOOP clone 保持 NOOP。
    #[test]
    fn clone_shares_sink_or_stays_noop() {
        let (context, handle) = in_memory();
        let cloned = context.clone();
        let mut span = cloned.span("s", vec![]);
        span.end();
        assert_eq!(handle.len(), 1);
        assert!(NOOP.clone().is_noop());
    }
}

#[cfg(test)]
mod engine_span_tests {
    use super::*;

    /// M6-02.A1：两条 Span 双引擎同构——native/codex 打同名 Span、同构字段。
    #[test]
    fn both_engines_emit_isomorphic_spans() {
        let (context, handle) = in_memory();
        let mut native = ai_request_span(&context, EngineKind::Native, "deepseek", "deepseek-v4");
        let mut codex = ai_request_span(&context, EngineKind::Codex, "codex", "gpt-5");
        native.end();
        codex.end();
        let records = handle.records();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| record.name == SPAN_AI_REQUEST));
        assert_eq!(records[0].attribute("engine"), Some("native"));
        assert_eq!(records[1].attribute("engine"), Some("codex"));
        // 同构字段面：engine/provider/model 三键齐全。
        for record in &records {
            for key in ["engine", "provider", "model"] {
                assert!(record.attribute(key).is_some(), "缺 {key}");
            }
        }
        let mut run_native = harness_run_span(&context, EngineKind::Native, "run-1");
        run_native.end();
        let records = handle.records();
        let run_record = records.last().unwrap();
        assert_eq!(run_record.name, SPAN_HARNESS_RUN);
        assert_eq!(run_record.attribute("engine"), Some("native"));
        assert_eq!(run_record.attribute("run_id"), Some("run-1"));
    }

    /// M6-02.A2：usage_json 从 Span 提取（end attrs）。
    #[test]
    fn usage_attributes_extract_from_usage_json() {
        let attrs = usage_end_attributes(
            r#"{"input_tokens":100,"output_tokens":50,"cache_read_tokens":80,"cache_write_tokens":40,"cost_usd":0.0123456}"#,
        );
        let lookup = |key: &str| {
            attrs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(lookup("input_tokens"), Some("100"));
        assert_eq!(lookup("output_tokens"), Some("50"));
        assert_eq!(lookup("cache_read_tokens"), Some("80"));
        assert_eq!(lookup("cost_usd"), Some("0.012346"));
        // 缺键跳过（tool_calls 等非归因键不进 Span）。
        assert!(lookup("tool_calls").is_none());
        assert!(usage_end_attributes("not-json").is_empty());
    }
}

#[cfg(test)]
mod adapter_consistency_tests {
    use super::*;

    /// M6-03.A1-原子性：Span 记录要么完整（有 start/end/状态）要么不存在——
    /// 未 end 的 Span 不入账（半写记录不可见）。
    #[test]
    fn atomicity_unended_span_is_never_visible() {
        let (context, handle) = in_memory();
        let mut ended = context.span("ended", vec![("a".into(), "1".into())]);
        ended.end();
        // 打开但未 end：不入账。
        let _unended = context.span("unended", vec![]);
        assert_eq!(handle.len(), 1);
        let records = handle.records();
        assert_eq!(records[0].name, "ended");
        assert!(records[0].ended_at_unix_ms.is_some());
    }

    /// M6-03.A1-状态合并：end attrs 与运行中 attrs 合并语义（end 覆盖同键），
    /// 单条记录呈现合并后的完整视图。
    #[test]
    fn state_merge_end_overrides_runtime_attributes() {
        let (context, handle) = in_memory();
        let mut span = context.span("merge", vec![("phase".into(), "start".into())]);
        span.set_attribute("phase", "running");
        span.set_attribute("tokens", "10");
        span.end_with(vec![("phase".into(), "done".into())]);
        let record = &handle.records()[0];
        assert_eq!(record.attribute("phase"), Some("done"), "end 覆盖");
        assert_eq!(record.attribute("tokens"), Some("10"), "运行中属性保留");
        assert_eq!(record.attribute("nonexistent"), None);
    }

    /// M6-03.A1-嵌套父子：父 Span end 前子 Span 先完成——记录顺序反映真实
    /// 完成序（子先父后），父子各自完整（可作未来 Adapter 挂 parent id 的基准）。
    #[test]
    fn nested_parent_child_spans_complete_independently() {
        let (context, handle) = in_memory();
        let mut parent = context.span("parent.run", vec![("run_id".into(), "r1".into())]);
        {
            let mut child = context.span("child.tool", vec![("tool".into(), "bash".into())]);
            child.set_status(SpanStatus::Ok);
            child.end(); // 子先完成
        }
        parent.set_status(SpanStatus::Ok);
        parent.end(); // 父后完成
        let records = handle.records();
        assert_eq!(
            records.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["child.tool", "parent.run"],
            "完成序：子先父后"
        );
        assert!(records.iter().all(|r| r.ended_at_unix_ms.is_some()));
    }
}
