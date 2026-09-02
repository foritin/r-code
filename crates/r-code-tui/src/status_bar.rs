//! footer 统计投影（M3-01 / R-STAT-01）。
//!
//! 数据源 = `TaskDetail.runs` 的 `usage_json` 累加（持久化投影，非 runtime
//! 私有态——resume 后仍准确，pi 同款原则）。上下文窗口未知时按 codex 形态
//! 回退为 `{x.xK} used`。阈值变色：占用 >70% warning、>90% error（§2.7）。

use r_code_core::dto::AgentRun;

/// 会话累计用量（tokens）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub runs: usize,
}

/// 从持久化 run 列表累加 usage（usage_json 缺失/损坏的 run 按零计并跳过）。
pub fn accumulate_usage(runs: &[AgentRun]) -> UsageStats {
    let mut stats = UsageStats::default();
    for run in runs {
        stats.runs += 1;
        let Some(raw) = run.usage_json.as_deref() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        let get = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        stats.input_tokens += get("input_tokens");
        stats.output_tokens += get("output_tokens");
        stats.cache_read_tokens += get("cache_read_tokens");
        stats.cache_write_tokens += get("cache_write_tokens");
    }
    stats
}

impl UsageStats {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

/// 紧凑数字（codex 风格：`900` / `1.9K` / `12.3K` / `4.56M`）。
pub fn format_compact(value: u64) -> String {
    if value < 1000 {
        value.to_string()
    } else if value < 1_000_000 {
        format!("{:.1}K", value as f64 / 1000.0)
    } else {
        format!("{:.2}M", value as f64 / 1_000_000.0)
    }
}

/// 占用阈值（>90% error、>70% warning；以**占用**计，呈现为余量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Threshold {
    Normal,
    Warning,
    Error,
}

pub fn threshold_for_percent_used(percent_used: u32) -> Threshold {
    if percent_used > 90 {
        Threshold::Error
    } else if percent_used > 70 {
        Threshold::Warning
    } else {
        Threshold::Normal
    }
}

/// 自动压缩标记（宿主暂无可观察的 compaction 事件/公开状态——数据缺口记录
/// 于任务证据；接线方传 false 时省略，后续接宿主状态后置 true）。
pub fn compaction_marker(auto_compaction: bool) -> &'static str {
    if auto_compaction {
        " (auto)"
    } else {
        ""
    }
}

/// footer 统计行（codex 形态：`↑in ↓out [N% context left | xK used]( (auto))`）。
/// 返回 (文本, 阈值色)。
pub fn footer_stats_line(
    stats: &UsageStats,
    context_window: Option<u64>,
    auto_compaction: bool,
) -> (String, Threshold) {
    let tokens = format!(
        "↑{} ↓{}",
        format_compact(stats.input_tokens + stats.cache_read_tokens),
        format_compact(stats.output_tokens)
    );
    let (occupancy, threshold) = match context_window {
        Some(window) if window > 0 => {
            let percent_used = ((stats.total_tokens() * 100) / window).min(100) as u32;
            (
                format!(
                    "{}% context left",
                    100u64.saturating_sub(percent_used as u64)
                ),
                threshold_for_percent_used(percent_used),
            )
        }
        _ => (
            format!("{} used", format_compact(stats.total_tokens())),
            Threshold::Normal,
        ),
    };
    (
        format!("{tokens} {occupancy}{}", compaction_marker(auto_compaction)),
        threshold,
    )
}

/// 从持久化 run 列表累计成本（宿主已把 cost_usd 归因进 usage_json；
/// 无任何成本数据返回 None）。
pub fn accumulate_cost(runs: &[AgentRun]) -> Option<f64> {
    let mut total = 0.0;
    let mut seen = false;
    for run in runs {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(run.usage_json.as_deref()?)
        else {
            continue;
        };
        if let Some(cost) = value.get("cost_usd").and_then(serde_json::Value::as_f64) {
            total += cost;
            seen = true;
        }
    }
    seen.then_some((total * 1e6).round() / 1e6)
}

/// `/usage` 汇总行（成本段仅在有定价数据时出现）。
pub fn usage_summary(stats: &UsageStats, cost: Option<f64>) -> String {
    match cost {
        Some(cost) => format!(
            "累计用量：↑{} ↓{} · 成本 ${cost:.4}",
            format_compact(stats.input_tokens + stats.cache_read_tokens),
            format_compact(stats.output_tokens)
        ),
        None => format!(
            "累计用量：↑{} ↓{}（无定价数据）",
            format_compact(stats.input_tokens + stats.cache_read_tokens),
            format_compact(stats.output_tokens)
        ),
    }
}

/// codex /status 状态卡（圆角框 ≤56 内宽；标签 padEnd(18) 对齐）。
pub fn status_card_lines(
    model_label: &str,
    directory: &str,
    stats: &UsageStats,
    context_window: Option<u64>,
) -> Vec<String> {
    let label = |name: &str| format!("{name:<18}");
    let context = match context_window {
        Some(window) if window > 0 => {
            let percent_used = ((stats.total_tokens() * 100) / window).min(100);
            format!(
                "{}% left ({} used / {})",
                100 - percent_used,
                format_compact(stats.total_tokens()),
                format_compact(window),
            )
        }
        _ => format!("{} used", format_compact(stats.total_tokens())),
    };
    let rows = vec![
        format!(" >_ R-Code CLI"),
        String::new(),
        format!("{}{}", label("model:"), model_label),
        format!("{}{}", label("directory:"), directory),
        format!(
            "{}{} total ({} input + {} output)",
            label("Token usage:"),
            format_compact(stats.total_tokens()),
            format_compact(stats.input_tokens + stats.cache_read_tokens),
            format_compact(stats.output_tokens),
        ),
        format!("{}{}", label("Context window:"), context),
    ];
    let width = rows
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0)
        .min(56);
    let mut lines = vec![format!("╭{}╮", "─".repeat(width + 2))];
    for row in rows {
        let pad = width.saturating_sub(row.chars().count());
        lines.push(format!("│ {}{} │", row, " ".repeat(pad)));
    }
    lines.push(format!("╰{}╯", "─".repeat(width + 2)));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn run_with_usage(json: &str) -> AgentRun {
        AgentRun {
            usage_json: Some(json.to_string()),
            ..fixture_run()
        }
    }

    fn fixture_run() -> AgentRun {
        AgentRun {
            id: "r".to_string(),
            task_id: "t".to_string(),
            branch_id: "b".to_string(),
            parent_run_id: None,
            agent_kind: r_code_core::dto::AgentKind::default(),
            agent_label: None,
            summary: None,
            delegated_by_tool_call_id: None,
            model: "demo".to_string(),
            runtime_kind: r_code_core::dto::AgentRunRuntimeKind::default(),
            access_mode: r_code_core::dto::SubagentAccessMode::default(),
            require_approval: false,
            routing_reason: None,
            external_session_id: None,
            review_state: r_code_core::dto::ReviewState::default(),
            started_at: Utc::now(),
            ended_at: None,
            usage_json: None,
            guard_trip: None,
            checkpoint_sha: None,
            checkpoint_base_head: None,
        }
    }

    /// M3-01.A1：累加 + 紧凑格式（K/M 边界、缓存并流）。
    #[test]
    fn accumulates_and_formats_usage() {
        let runs = vec![
            run_with_usage(
                r#"{"input_tokens":900,"output_tokens":100,"cache_read_tokens":0,"cache_write_tokens":0}"#,
            ),
            run_with_usage(
                r#"{"input_tokens":1000,"output_tokens":800,"cache_read_tokens":5000,"cache_write_tokens":300}"#,
            ),
            fixture_run(), // 无 usage 的 run 按零计
        ];
        let stats = accumulate_usage(&runs);
        assert_eq!(stats.runs, 3);
        assert_eq!(stats.input_tokens, 1900);
        assert_eq!(stats.output_tokens, 900);
        assert_eq!(stats.cache_read_tokens, 5000);
        assert_eq!(format_compact(900), "900");
        assert_eq!(format_compact(1900), "1.9K");
        assert_eq!(format_compact(45_600), "45.6K");
        assert_eq!(format_compact(4_560_000), "4.56M");
        // 相同输入恒等输出（持久化投影，resume 后一致）。
        assert_eq!(accumulate_usage(&runs), stats);
    }

    /// M3-01.A2：阈值变色契约（>70% warning、>90% error；余量呈现）。
    #[test]
    fn thresholds_change_at_contract_boundaries() {
        assert_eq!(threshold_for_percent_used(70), Threshold::Normal);
        assert_eq!(threshold_for_percent_used(71), Threshold::Warning);
        assert_eq!(threshold_for_percent_used(90), Threshold::Warning);
        assert_eq!(threshold_for_percent_used(91), Threshold::Error);
        // 窗口已知：低占用 → Normal + 余量呈现。
        let stats = UsageStats {
            input_tokens: 1000,
            output_tokens: 1000,
            ..UsageStats::default()
        };
        let (text, threshold) = footer_stats_line(&stats, Some(200_000), false);
        assert_eq!(threshold, Threshold::Normal);
        assert!(text.contains("% context left"), "codex 余量形态：{text}");
        // 高占用 → Error。
        let hot = UsageStats {
            input_tokens: 95_000,
            ..UsageStats::default()
        };
        let (_, threshold) = footer_stats_line(&hot, Some(100_000), false);
        assert_eq!(threshold, Threshold::Error);
        // 窗口未知 → used 回退（codex 同款）。
        let (text, threshold) = footer_stats_line(&stats, None, false);
        assert_eq!(threshold, Threshold::Normal);
        assert!(text.contains("2.0K used"), "未知窗口回退 used 形态：{text}");
    }

    /// M3-02.A1：/status 卡行快照（圆角框、>_ 头、标签 padEnd(18)、两行用量）。
    #[test]
    fn status_card_matches_codex_shape() {
        let stats = UsageStats {
            input_tokens: 1000,
            output_tokens: 900,
            ..UsageStats::default()
        };
        let lines = status_card_lines("(demo) m", "~/dev/r-code", &stats, Some(272_000));
        assert!(lines.first().is_some_and(|line| line.starts_with("╭")));
        assert!(lines.last().is_some_and(|line| line.starts_with("╰")));
        let body = lines.join("\n");
        assert!(body.contains(" >_ R-Code CLI"), "codex 头行：{body}");
        assert!(
            body.contains("model:            (demo) m"),
            "标签 padEnd(18) 对齐：{body}"
        );
        assert!(
            body.contains("Token usage:      1.9K total (1.0K input + 900 output)"),
            "Token usage 行：{body}"
        );
        assert!(
            body.contains("Context window:   100% left (1.9K used / 272.0K)"),
            "Context window 行（窗口已知→余量形态）：{body}"
        );
    }

    /// M3-02.A2：成本累加与 /usage 汇总（无定价数据省略成本段）。
    #[test]
    fn usage_summary_reports_cost_when_priced() {
        let priced =
            run_with_usage(r#"{"input_tokens":1000,"output_tokens":900,"cost_usd":0.4123}"#);
        let stats = accumulate_usage(std::slice::from_ref(&priced));
        let cost = accumulate_cost(std::slice::from_ref(&priced)).expect("cost attributed");
        assert!((cost - 0.4123).abs() < 1e-9);
        let summary = usage_summary(&stats, Some(cost));
        assert!(summary.contains("成本 $0.4123"), "{summary}");

        let unpriced = run_with_usage(r#"{"input_tokens":10,"output_tokens":5}"#);
        assert!(accumulate_cost(std::slice::from_ref(&unpriced)).is_none());
        let summary = usage_summary(&accumulate_usage(std::slice::from_ref(&unpriced)), None);
        assert!(summary.contains("无定价数据"), "{summary}");
    }

    /// M3-02.A3：卡内 context 行与 footer 形态一致（未知窗口回退 used）。
    #[test]
    fn status_card_context_row_matches_footer_format() {
        let stats = UsageStats {
            input_tokens: 2300,
            ..UsageStats::default()
        };
        let lines = status_card_lines("(demo) m", "~", &stats, None);
        let body = lines.join("\n");
        assert!(
            body.contains("Context window:   2.3K used"),
            "未知窗口回退 used 形态：{body}"
        );
    }

    /// M3-01.A3：compaction 标记（(auto) / 空）。
    #[test]
    fn compaction_marker_toggles() {
        assert_eq!(compaction_marker(true), " (auto)");
        assert_eq!(compaction_marker(false), "");
        let (text, _) = footer_stats_line(&UsageStats::default(), Some(1000), true);
        assert!(text.contains("(auto)"), "auto 压缩标记随行：{text}");
    }
}
