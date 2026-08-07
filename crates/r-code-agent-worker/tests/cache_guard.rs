//! P2-H（docs/deepseek-prefix-cache.md §5 P2-H）：缓存命中守卫测试（release 门禁）。
//!
//! 多轮工具循环场景 tail_avg（末 3 轮）命中率 ≥90%，对齐 Reasonix
//! `TestReleaseCacheHitGuard`（`cachehit_e2e_test.go:378-477`）：
//! - 默认 skip：`R_CODE_CACHE_GUARD != "1"` 时仅提示并返回（release 门禁定位，
//!   避免"CI 里根本不跑"的落差）；
//! - 非严格模式（无 `R_CODE_CACHE_GUARD_STRICT`）：命中率不足只打印警告；
//! - 严格模式（`R_CODE_CACHE_GUARD_STRICT` 非空）：命中率不足直接 panic。
//!
//! mock 按**字节级公共前缀**模拟命中（对齐 `cachehit_e2e_test.go:59-74` 的
//! mockDeepSeek）：记录上一轮请求的 messages 逐条序列化字节，本轮推导公共前缀
//! 长度并生成 `Usage { cache_read_tokens, cache_write_tokens }`——报告出的命中率
//! 就是客户端把请求前缀保持得有多稳定的直接度量。
//!
//! 压缩轮不计入 tail_avg：P2-G 压缩已接入 llm_runtime 主 run 循环，但本守卫
//! 场景驱动 `run_agent_loop_iteration` 单轮路径且上下文低于压缩阈值，全部轮次
//! 均为 append-only 工具轮；未来守卫场景覆盖压缩后需按轮标记并排除压缩轮
//! （PRD P2-H 方案 2）。
//!
//! 有效性验证：`R_CODE_CACHE_GUARD_DRIFT=1` 时向 system 注入逐轮漂移（模拟
//! P0-A 未落地的时间戳），守卫应归因 [`CacheChangeCause::System`] 并拉低命中率
//! ——严格模式下断言变红，证明守卫确实在检测前缀字节稳定性（PRD P2-H 验收）。

use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::BoxStream;
use hermes_core::{
    Capabilities, CompletionRequest, CompletionResponse, LlmProvider, Message, StopReason,
    StreamEvent, ToolCallOutcome, ToolHost, ToolSource, ToolSpec, Usage,
};
use hermes_error::{Error, Result};
use r_code_agent_worker::cache_shape::{capture, compare, CacheChangeCause};
use r_code_agent_worker::run_agent_loop_iteration;
use r_code_core::dto::AgentEvent;

/// tail_avg 阈值（对齐 Reasonix 默认 90）。
const TAIL_AVG_THRESHOLD_PCT: f64 = 90.0;
/// tail 窗口：末 3 轮。
const TAIL_WINDOW: usize = 3;
/// 工具调用轮数：4 轮工具调用 + 1 轮最终回答 = 5 次 provider 请求。
const TOOL_ROUNDS: u32 = 4;
/// 初始 user 消息的重复句数（每句约 140 字节）：建立历史基数，
/// 保证末 3 轮命中率远离阈值（余量约 6 个百分点）。
const INITIAL_PROMPT_REPEATS: usize = 40;
/// 循环上限（防回归导致死循环）。
const MAX_ROUNDS: usize = 12;

/// 固定 system 文本：守卫场景中必须逐字节稳定（PRD §4 原则 1，P0-A）。
const SYSTEM_PROMPT: &str = "You are r-code, a coding agent. Be concise and follow project \
conventions. This system prompt is the cacheable head of every request and must never change \
between turns.";

/// 每轮记录的命中数据。
#[derive(Debug, Clone, Copy)]
struct RoundUsage {
    /// 缓存读（命中）token。
    hit: u32,
    /// 总输入 token。
    input: u32,
}

impl RoundUsage {
    /// 本轮命中率（0-100）。对齐 Reasonix `hitRate`：hit / (hit + miss)，
    /// 本 mock 保证 miss = input - hit。
    fn rate_pct(&self) -> f64 {
        if self.input > 0 {
            100.0 * self.hit as f64 / self.input as f64
        } else {
            0.0
        }
    }
}

/// Mock provider 内部状态。
struct MockState {
    /// 上一轮请求的 system 字节（公共前缀推导基准的一部分：
    /// DeepSeek 前缀 = system + messages + tools 的整体字节）。
    prev_system: Vec<u8>,
    /// 上一轮请求的 messages 逐条序列化字节（公共前缀推导基准）。
    prev_messages: Vec<Vec<u8>>,
    /// 剩余工具轮数。
    tool_rounds_left: u32,
    /// 每轮 usage 记录。
    rounds: Vec<RoundUsage>,
}

/// 按字节级公共前缀模拟 DeepSeek 前缀缓存的 provider。
///
/// 每轮 `stream` 时：把本轮 `request.messages` 逐条序列化，与上一轮逐条比较
/// （`bytes ==` 才计数，第一条不同即停），公共字节数 / 4 即 cache_read_tokens；
/// 总字节 / 4 为 input_tokens，差值即 cache_write_tokens。
struct PrefixCacheMockProvider {
    state: Mutex<MockState>,
}

impl PrefixCacheMockProvider {
    fn new(tool_rounds: u32) -> Self {
        Self {
            state: Mutex::new(MockState {
                prev_system: Vec::new(),
                prev_messages: Vec::new(),
                tool_rounds_left: tool_rounds,
                rounds: Vec::new(),
            }),
        }
    }

    fn rounds(&self) -> Vec<RoundUsage> {
        self.state.lock().unwrap().rounds.clone()
    }
}

#[async_trait]
impl LlmProvider for PrefixCacheMockProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
        Err(Error::Internal(
            "PrefixCacheMockProvider only supports stream".to_string(),
        ))
    }

    async fn stream(&self, request: CompletionRequest) -> Result<BoxStream<'static, StreamEvent>> {
        let mut state = self.state.lock().unwrap();

        // 公共前缀推导（对齐 Reasonix commonPrefixMsgs + charsOf，
        // cachehit_e2e_test.go:57-74）：system 字节须与上一轮完全一致，
        // 然后逐条 bytes 相等计数 messages，前缀即字节稳定区。
        let system_bytes = request.system.as_deref().unwrap_or("").as_bytes().to_vec();
        let current: Vec<Vec<u8>> = request
            .messages
            .iter()
            .map(|m| serde_json::to_vec(m).unwrap_or_default())
            .collect();
        let common_bytes: usize = if state.prev_system == system_bytes {
            let message_prefix: usize = state
                .prev_messages
                .iter()
                .zip(current.iter())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a.len())
                .sum();
            system_bytes.len() + message_prefix
        } else {
            0
        };
        let total_bytes: usize = current.iter().map(|b| b.len()).sum();

        // 可缓存输入 = system + messages 的整体字节（DeepSeek prompt 前缀）。
        let input_tokens = ((system_bytes.len() + total_bytes) / 4) as u32;
        let hit_tokens = (common_bytes / 4) as u32;

        state.prev_system = system_bytes;
        state.prev_messages = current;
        let write_tokens = input_tokens.saturating_sub(hit_tokens);
        state.rounds.push(RoundUsage {
            hit: hit_tokens,
            input: input_tokens,
        });

        let emit_tool = state.tool_rounds_left > 0;
        if emit_tool {
            state.tool_rounds_left -= 1;
        }

        let mut events = Vec::new();
        if emit_tool {
            let idx = state.rounds.len();
            events.push(StreamEvent::ToolUseStart {
                id: format!("call_{idx}"),
                name: "echo".to_string(),
            });
            events.push(StreamEvent::ToolUseComplete {
                id: format!("call_{idx}"),
                input: serde_json::json!({ "text": format!("round-{idx}") }),
            });
        } else {
            events.push(StreamEvent::TextDelta {
                text: "Done.".to_string(),
            });
        }
        // Usage 必须位于 Stop 之前：agent_loop 在 Stop 处跳出事件循环
        // （agent_loop.rs:538-590），Stop 之后的事件不会被累计。
        events.push(StreamEvent::Usage(Usage {
            input_tokens,
            output_tokens: 50,
            cache_read_tokens: Some(hit_tokens),
            cache_write_tokens: Some(write_tokens),
        }));
        let stop_reason = if emit_tool {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        };
        events.push(StreamEvent::Stop {
            reason: stop_reason,
        });
        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            supports_streaming: true,
            supports_tool_use: true,
            supports_vision: false,
            supports_prompt_caching: true,
            max_context_tokens: 200_000,
        }
    }

    fn name(&self) -> &str {
        "prefix-cache-mock"
    }
}

/// 回显工具宿主：调用返回固定短文本 "ok"，
/// 保证每轮新增消息（assistant tool_use + user tool_result）字节小且确定。
struct EchoToolHost {
    tools: Vec<ToolSpec>,
}

impl EchoToolHost {
    fn new() -> Self {
        Self {
            tools: vec![ToolSpec {
                name: "echo".to_string(),
                description: "echo back the given text".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                source: ToolSource::Builtin,
                requires_confirmation: false,
            }],
        }
    }
}

#[async_trait]
impl ToolHost for EchoToolHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(self.tools.clone())
    }

    async fn call(&self, name: &str, _args: serde_json::Value) -> Result<ToolCallOutcome> {
        if name == "echo" {
            Ok(ToolCallOutcome {
                content: "ok".to_string(),
                is_error: false,
                metadata: None,
            })
        } else {
            Err(Error::ToolNotFound(name.to_string()))
        }
    }
}

/// 末 n 轮命中率均值（对齐 Reasonix `tailAverage`，cachehit_e2e_test.go:447-459）。
fn tail_average(rounds: &[RoundUsage], n: usize) -> f64 {
    if rounds.is_empty() {
        return 0.0;
    }
    let n = n.min(rounds.len());
    let window = &rounds[rounds.len() - n..];
    window.iter().map(RoundUsage::rate_pct).sum::<f64>() / window.len() as f64
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn cache_guard_tool_loop_tail_avg() {
    // ── 环境门禁：默认 skip（release 门禁定位）──
    if std::env::var("R_CODE_CACHE_GUARD").as_deref() != Ok("1") {
        eprintln!(
            "[cache_guard] 跳过：设置 R_CODE_CACHE_GUARD=1 启用 release 缓存守卫 \
             （R_CODE_CACHE_GUARD_STRICT=1 时命中率不足才失败）"
        );
        return;
    }
    let strict = std::env::var("R_CODE_CACHE_GUARD_STRICT").is_ok_and(|v| !v.is_empty());
    let drift = std::env::var("R_CODE_CACHE_GUARD_DRIFT").is_ok_and(|v| !v.is_empty());
    if drift {
        eprintln!(
            "[cache_guard] 已注入逐轮 system 漂移（有效性验证）：\
             归因应为 System 且命中率塌陷"
        );
    }

    let provider = PrefixCacheMockProvider::new(TOOL_ROUNDS);
    let tool_host = EchoToolHost::new();
    let tools = tool_host.tools.clone();

    // 初始 user 消息做历史基数（模拟真实长指令），保证末 3 轮命中率远离阈值。
    let mut messages = vec![Message::user_text(format!(
        "Run several tool calls and then give a final answer. {}",
        "Keep the request prefix byte-stable across turns: this is a deliberately long \
initial instruction that builds cacheable history. "
            .repeat(INITIAL_PROMPT_REPEATS)
    ))];

    // ── 归因：每轮请求前捕获并比对 PrefixShape（drift 模式必须报 System，
    //    正常模式必须 None——system/tools/版本逐轮稳定）──
    let mut shape = capture(SYSTEM_PROMPT, &tools, 1, 0);
    let mut completed = false;

    for round in 0..MAX_ROUNDS {
        let system = if drift {
            format!("{SYSTEM_PROMPT}\n[drift] local time: 2026-01-01T00:00:0{round}")
        } else {
            SYSTEM_PROMPT.to_string()
        };
        let next = capture(&system, &tools, 1, 0);
        let cause = compare(&shape, &next);
        if drift {
            assert_eq!(
                cause,
                CacheChangeCause::System,
                "drift 注入应归因 System（第 {round} 轮）"
            );
        } else {
            assert_eq!(
                cause,
                CacheChangeCause::None,
                "正常模式下 system/tools/版本必须逐轮稳定（第 {round} 轮归因 {cause}）"
            );
        }
        shape = next;

        // request.messages / request.tools 会被 run_agent_loop_iteration
        // 以工作集（messages / tools 参数）覆盖（agent_loop.rs:362-363）。
        let request = CompletionRequest {
            model: "deepseek-mock".to_string(),
            system: Some(system),
            messages: Vec::new(),
            tools: Vec::new(),
            hosted_tools: Vec::new(),
            max_tokens: 512,
            temperature: None,
            enable_caching: false,
            inference: Default::default(),
        };

        let events =
            run_agent_loop_iteration(&provider, &tool_host, request, &mut messages, &tools)
                .await
                .expect("工具循环迭代必须成功");

        // 每轮 usage 由 agent_loop 聚合发出（mock 总是报告非零 usage）。
        let usage_json = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Usage { usage_json } => Some(usage_json.as_str()),
                _ => None,
            })
            .expect("每轮必须发出 Usage 事件");
        let _usage: Usage = serde_json::from_str(usage_json).expect("usage_json 必须可解析");

        let had_tool_call = events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { .. }));
        if !had_tool_call {
            completed = true;
            break;
        }
    }
    assert!(
        completed,
        "工具循环应在 {MAX_ROUNDS} 轮内以最终回答结束（append-only 未中断）"
    );

    // 命中率曲线（对齐 Reasonix CACHE_GUARD_RESULT 日志）
    let rounds = provider.rounds();
    for (i, r) in rounds.iter().enumerate() {
        eprintln!(
            "[cache_guard] round {i}: input={} hit={} → {:.1}%",
            r.input,
            r.hit,
            r.rate_pct()
        );
    }

    // 压缩轮不计入 tail_avg：当前无压缩路径（P2-G 未接入），全部轮次均为
    // append-only 工具轮；未来接入压缩后需按轮标记并排除压缩轮（PRD P2-H 方案 2）。
    let tail = tail_average(&rounds, TAIL_WINDOW);
    eprintln!(
        "[cache_guard] tail_avg({TAIL_WINDOW}) = {tail:.1}% 阈值 {TAIL_AVG_THRESHOLD_PCT}% \
         strict={strict} drift={drift}"
    );

    if tail < TAIL_AVG_THRESHOLD_PCT {
        let msg = format!(
            "[cache_guard] tail_avg 命中率 {tail:.1}% < {TAIL_AVG_THRESHOLD_PCT}%：\
             请求前缀字节不稳定（P0-A/P1-C 未落地或回归；drift 注入={drift}）"
        );
        eprintln!("{msg}");
        if strict {
            panic!("{msg}");
        }
    }
}
