//! 配对统计（docs/pi-alignment PRD §4.1 R-EVL-03 / M2-03）。
//!
//! [`eval_harness_table`]：baseline / candidate 两个 [`Harness`] × 输入 × 重复
//! 轮次的矩阵；每行注入 `group_key = 输入标识（优先 input.id，否则规范化 JSON
//! SHA-256）+ 重复轮次`；产出 Pass Rate Lift、逐对差值（Token/耗时/成本——
//! **缺失跳过而非按 0 计**）与五类诊断（缺失/重复/harness 错/缺分/不可打分）。

use std::collections::BTreeMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::judge::{Judge, JudgeInput};
use crate::{EvalInput, EvalRunResult, Harness};

/// 单行：一次 (harness, input, repetition) 的运行与打分。
#[derive(Debug, Clone)]
pub struct EvalTableRow {
    pub harness: String,
    pub input_id: String,
    /// groupKey = 输入标识 + 重复轮次（配对依据）。
    pub group_key: String,
    pub repetition: u32,
    pub score: Option<f64>,
    pub rationale: Option<String>,
    /// pass = score >= 1 且 run 收敛（R-EVL-03 固定判据）。
    pub passed: bool,
    /// 运行错误（harness 级失败行没有结果）。
    pub run_error: Option<String>,
}

/// 一对 (baseline, candidate) 同 group 的差值。任一侧缺失该指标时该指标
/// **跳过**（None，不按 0 计）；整行只在两臂都运行成功时产生。
#[derive(Debug, Clone, PartialEq)]
pub struct PairedDiff {
    pub group_key: String,
    pub repetition: u32,
    /// candidate − baseline（usage 四桶 token 和；两侧都有 usage 才计）。
    pub token_delta: Option<i64>,
    /// candidate − baseline（wall_ms 恒有）。
    pub wall_ms_delta: Option<i64>,
    /// usage_json 的 cost_usd（M1-04 归因键）差值；两侧都有才计。
    pub cost_usd_delta: Option<f64>,
}

/// 五类诊断（单独列出，不与统计混算）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvalDiagnostics {
    /// 缺失：某 (group, repetition) 在某一臂没有运行行。
    pub missing: Vec<String>,
    /// 重复：同 (harness, group, repetition) 出现多行。
    pub duplicates: Vec<String>,
    /// harness 错：运行本身失败（run_error 非空）。
    pub harness_errors: Vec<String>,
    /// 缺分：行存在、无 harness 错，但 score 为 None。
    pub unscored: Vec<String>,
    /// 不可打分：score 非 [0,1]（NaN/越界）。
    pub unscorable: Vec<String>,
}

impl EvalDiagnostics {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty()
            && self.duplicates.is_empty()
            && self.harness_errors.is_empty()
            && self.unscored.is_empty()
            && self.unscorable.is_empty()
    }
}

/// 配对报告。
#[derive(Debug, Clone, Default)]
pub struct EvalTableReport {
    pub rows: Vec<EvalTableRow>,
    /// candidate 通过率 − baseline 通过率（分臂分母 = 各臂成功行数）。
    pub pass_rate_lift: f64,
    pub baseline_pass_rate: f64,
    pub candidate_pass_rate: f64,
    pub pairs: Vec<PairedDiff>,
    pub diagnostics: EvalDiagnostics,
}

/// 输入标识：优先 `input.id`，否则规范化 JSON 的 SHA-256（M2-03.A1）。
pub fn input_group_id(input: &EvalInput) -> String {
    if !input.id.trim().is_empty() {
        return input.id.trim().to_string();
    }
    // 规范化 JSON：键序稳定（serde_json Map 字母序）→ 同一输入同一摘要；
    // 路径分隔符归一，Windows/Unix 摘要一致。
    let canonical = serde_json::json!({
        "fixture": input
            .fixture
            .as_ref()
            .map(|path| path.to_string_lossy().replace('\\', "/")),
        "prompt": input.prompt,
    });
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string().as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn usage_tokens(usage_json: &str) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_str(usage_json).ok()?;
    let bucket = |key: &str| value.get(key).and_then(|item| item.as_i64());
    Some(
        bucket("input_tokens").unwrap_or(0)
            + bucket("output_tokens").unwrap_or(0)
            + bucket("cache_read_tokens").unwrap_or(0)
            + bucket("cache_write_tokens").unwrap_or(0),
    )
}

fn usage_cost_usd(usage_json: &str) -> Option<f64> {
    let value: serde_json::Value = serde_json::from_str(usage_json).ok()?;
    value.get("cost_usd").and_then(|item| item.as_f64())
}

/// 跑配对矩阵并统计。`repetitions` 每输入重复轮数（1 起）。
/// 打分对每行执行；未收敛（stopReason 非 stop）的行判不通过。
pub async fn eval_harness_table(
    baseline: Arc<dyn Harness>,
    candidate: Arc<dyn Harness>,
    inputs: &[EvalInput],
    repetitions: u32,
    judge: &Judge,
) -> EvalTableReport {
    let mut report = EvalTableReport::default();
    // 成功运行按 (harness, group, repetition) 存原始结果（配对差值用）。
    let mut outcomes: BTreeMap<(String, String, u32), EvalRunResult> = BTreeMap::new();
    let mut row_counts: BTreeMap<(String, String), usize> = BTreeMap::new();

    for (harness_name, harness) in [("baseline", baseline), ("candidate", candidate)] {
        for input in inputs {
            for repetition in 1..=repetitions {
                let group_key = format!("{}#{}", input_group_id(input), repetition);
                *row_counts
                    .entry((harness_name.to_string(), group_key.clone()))
                    .or_default() += 1;
                match harness.run(input).await {
                    Ok(result) => {
                        outcomes.insert(
                            (harness_name.to_string(), group_key.clone(), repetition),
                            result.clone(),
                        );
                        report.rows.push(scored_row(
                            harness_name,
                            &group_key,
                            repetition,
                            input,
                            &result,
                            judge,
                        ));
                    }
                    Err(error) => report.rows.push(EvalTableRow {
                        harness: harness_name.to_string(),
                        input_id: input.id.clone(),
                        group_key,
                        repetition,
                        score: None,
                        rationale: None,
                        passed: false,
                        run_error: Some(error),
                    }),
                }
            }
        }
    }

    // 诊断 1：重复（同臂同 group 多行）。
    for ((harness, group_key), count) in &row_counts {
        if *count > 1 {
            report
                .diagnostics
                .duplicates
                .push(format!("{harness}/{group_key} x{count}"));
        }
    }
    // 诊断 2：缺失（某臂某 group 一行都没有）。
    for input in inputs {
        for repetition in 1..=repetitions {
            let group_key = format!("{}#{}", input_group_id(input), repetition);
            for harness_name in ["baseline", "candidate"] {
                if !row_counts.contains_key(&(harness_name.to_string(), group_key.clone())) {
                    report
                        .diagnostics
                        .missing
                        .push(format!("{harness_name}/{group_key}"));
                }
            }
        }
    }
    // 诊断 3/4/5：harness 错 / 缺分 / 不可打分。
    for row in &report.rows {
        let label = format!("{}/{}", row.harness, row.group_key);
        if row.run_error.is_some() {
            report.diagnostics.harness_errors.push(label);
            continue;
        }
        match row.score {
            None => report.diagnostics.unscored.push(label),
            Some(score) if !score.is_finite() || !(0.0..=1.0).contains(&score) => {
                report.diagnostics.unscorable.push(label);
            }
            _ => {}
        }
    }

    // Pass Rate Lift（分臂分母 = 各臂成功行数；无行时 0）。
    let rate = |harness_name: &str| -> f64 {
        let rows: Vec<&EvalTableRow> = report
            .rows
            .iter()
            .filter(|row| row.harness == harness_name && row.run_error.is_none())
            .collect();
        if rows.is_empty() {
            return 0.0;
        }
        rows.iter().filter(|row| row.passed).count() as f64 / rows.len() as f64
    };
    report.baseline_pass_rate = rate("baseline");
    report.candidate_pass_rate = rate("candidate");
    report.pass_rate_lift = report.candidate_pass_rate - report.baseline_pass_rate;

    // 配对差值：两臂都成功的 (group, repetition) 才配；单指标缺失跳过（None）。
    let mut group_slots: BTreeMap<(String, u32), ()> = BTreeMap::new();
    for (harness, group_key, repetition) in outcomes.keys() {
        if harness == "baseline" {
            group_slots.insert((group_key.clone(), *repetition), ());
        }
    }
    for ((group_key, repetition), ()) in group_slots {
        let (Some(base), Some(cand)) = (
            outcomes.get(&("baseline".to_string(), group_key.clone(), repetition)),
            outcomes.get(&("candidate".to_string(), group_key.clone(), repetition)),
        ) else {
            continue;
        };
        let token_delta = match (&base.usage_json, &cand.usage_json) {
            (Some(before), Some(after)) => match (usage_tokens(before), usage_tokens(after)) {
                (Some(before), Some(after)) => Some(after - before),
                _ => None,
            },
            _ => None,
        };
        let cost_usd_delta = match (&base.usage_json, &cand.usage_json) {
            (Some(before), Some(after)) => match (usage_cost_usd(before), usage_cost_usd(after)) {
                (Some(before), Some(after)) => Some(after - before),
                _ => None,
            },
            _ => None,
        };
        report.pairs.push(PairedDiff {
            group_key,
            repetition,
            token_delta,
            wall_ms_delta: Some(cand.timings.wall_ms as i64 - base.timings.wall_ms as i64),
            cost_usd_delta,
        });
    }

    report
}

/// scored_row：把一次成功运行打成行（打分 + 未收敛强制不通过）。
fn scored_row(
    harness_name: &str,
    group_key: &str,
    repetition: u32,
    input: &EvalInput,
    result: &EvalRunResult,
    judge: &Judge,
) -> EvalTableRow {
    let verdict = judge.score(&JudgeInput {
        input,
        result,
        fixture: input.fixture.as_deref(),
    });
    let passed = result.stop_reason.is_settled() && verdict.score >= 1.0;
    EvalTableRow {
        harness: harness_name.to_string(),
        input_id: input.id.clone(),
        group_key: group_key.to_string(),
        repetition,
        score: Some(verdict.score),
        rationale: Some(verdict.rationale),
        passed,
        run_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::{create_judge, JudgeVerdict};
    use crate::{EvalStopReason, EvalTimings};
    use async_trait::async_trait;
    use std::path::PathBuf;

    struct ScriptedHarness {
        fail: bool,
        usage: Option<String>,
        wall_ms: u64,
    }

    #[async_trait]
    impl Harness for ScriptedHarness {
        fn name(&self) -> &str {
            "scripted"
        }

        async fn run(&self, input: &EvalInput) -> Result<EvalRunResult, String> {
            if self.fail {
                return Err("harness exploded".to_string());
            }
            Ok(EvalRunResult {
                harness: "scripted".to_string(),
                input_id: input.id.clone(),
                output: "ok".to_string(),
                usage_json: self.usage.clone(),
                timings: EvalTimings {
                    wall_ms: self.wall_ms,
                },
                events: Vec::new(),
                stop_reason: EvalStopReason::Settled,
                workspace: PathBuf::from("."),
            })
        }
    }

    /// M2-03.A1：groupKey——input.id 优先；无 id 时规范化 JSON SHA-256 稳定。
    #[test]
    fn group_key_prefers_id_else_stable_hash() {
        let with_id = EvalInput::new(" corpus-007 ", "do");
        assert_eq!(input_group_id(&with_id), "corpus-007");
        let no_id = EvalInput::new("", "same prompt");
        let no_id_again = EvalInput::new("", "same prompt");
        assert_eq!(input_group_id(&no_id), input_group_id(&no_id_again));
        assert!(input_group_id(&no_id).starts_with("sha256:"));
        let other = EvalInput::new("", "other prompt");
        assert_ne!(input_group_id(&no_id), input_group_id(&other));
    }

    /// M2-03.A2：Pass Rate Lift = candidate 通过率 − baseline 通过率。
    #[tokio::test]
    async fn pass_rate_lift_arithmetic() {
        let pass_judge = create_judge("pass", Arc::new(|_| JudgeVerdict::pass("always")));
        let baseline = Arc::new(ScriptedHarness {
            fail: false,
            usage: None,
            wall_ms: 100,
        });
        let candidate = Arc::new(ScriptedHarness {
            fail: false,
            usage: None,
            wall_ms: 120,
        });
        let inputs = vec![EvalInput::new("i1", "p"), EvalInput::new("i2", "p")];
        let report = eval_harness_table(baseline, candidate, &inputs, 2, &pass_judge).await;
        assert!((report.pass_rate_lift - 0.0).abs() < 1e-9);
        assert!((report.baseline_pass_rate - 1.0).abs() < 1e-9);
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        // 8 行（2 臂 × 2 输入 × 2 轮），4 对配对差值，wall_ms_delta = +20。
        assert_eq!(report.rows.len(), 8);
        assert_eq!(report.pairs.len(), 4);
        assert!(report
            .pairs
            .iter()
            .all(|pair| pair.wall_ms_delta == Some(20)));
        // 两侧都没有 usage：token/cost 差值跳过（None，不是 0）。
        assert!(report.pairs.iter().all(|pair| pair.token_delta.is_none()));
        assert!(report
            .pairs
            .iter()
            .all(|pair| pair.cost_usd_delta.is_none()));
    }

    /// M2-03.A3：配对差值逐对计算；指标缺失跳过非 0。
    #[test]
    fn paired_diffs_skip_missing_metrics() {
        let both = r#"{"input_tokens":100,"output_tokens":50,"cost_usd":0.01}"#;
        assert_eq!(usage_tokens(both), Some(150));
        assert_eq!(usage_cost_usd(both), Some(0.01));
        let no_cost = r#"{"input_tokens":100}"#;
        assert_eq!(usage_cost_usd(no_cost), None, "missing cost => skip");
        assert_eq!(usage_tokens("not-json"), None);
    }

    /// M2-03.A3（端到端）：单侧 usage 缺失 → 该对 token_delta 跳过。
    #[tokio::test]
    async fn one_sided_usage_skips_token_delta() {
        let pass_judge = create_judge("pass", Arc::new(|_| JudgeVerdict::pass("always")));
        let baseline = Arc::new(ScriptedHarness {
            fail: false,
            usage: None,
            wall_ms: 10,
        });
        let candidate = Arc::new(ScriptedHarness {
            fail: false,
            usage: Some(r#"{"input_tokens":100}"#.to_string()),
            wall_ms: 10,
        });
        let inputs = vec![EvalInput::new("i1", "p")];
        let report = eval_harness_table(baseline, candidate, &inputs, 1, &pass_judge).await;
        assert_eq!(report.pairs.len(), 1);
        assert_eq!(
            report.pairs[0].token_delta, None,
            "baseline 无 usage → 跳过"
        );
        assert_eq!(report.pairs[0].wall_ms_delta, Some(0));
    }

    /// M2-03.A4：五类诊断单独列出（harness 错/不可打分各归各类）。
    #[tokio::test]
    async fn five_diagnostics_are_separated() {
        let broken_judge = create_judge(
            "broken",
            Arc::new(|_| JudgeVerdict {
                score: 7.5,
                rationale: "out of range".to_string(),
                failures: Vec::new(),
            }),
        );
        let ok = Arc::new(ScriptedHarness {
            fail: false,
            usage: None,
            wall_ms: 1,
        });
        let broken = Arc::new(ScriptedHarness {
            fail: true,
            usage: None,
            wall_ms: 1,
        });
        let inputs = vec![EvalInput::new("i1", "p")];
        let report = eval_harness_table(ok, broken, &inputs, 1, &broken_judge).await;
        assert_eq!(report.diagnostics.harness_errors.len(), 1);
        assert!(report.diagnostics.harness_errors[0].contains("candidate/i1#1"));
        assert_eq!(report.diagnostics.unscorable.len(), 1);
        assert!(report.diagnostics.unscorable[0].contains("baseline/i1#1"));
        // harness 错行不进配对。
        assert!(report.pairs.is_empty());
    }
}
