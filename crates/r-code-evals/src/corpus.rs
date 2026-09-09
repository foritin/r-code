//! 金集配对基准（docs/pi-alignment PRD §4.1 R-EVL-04 / M2-04）。
//!
//! 把 Windows 金集（`crates/r-code-gateway/tests/command_corpus/corpus.jsonl`，
//! 44 条）包装为评估输入（input.id = 命令编号），跑 baseline / candidate
//! 二臂 × `repetitions`，产出 Pass Rate Lift + 配对差值 + 可回放产物：
//!
//! - **安全红线硬断言**：预期失败的 `policy` 条目（sudo/git push 等）必须被
//!   gateway R4 **前置拒绝**（blocked），且分类器定级 ≥R3；触发即整场评估失败
//!   （不是 Judge 扣分——"绝不能发生"不用分数表达）；
//! - **baseline 臂**：命令在真实执行路径上的原始输出；**candidate 臂**：同一次
//!   命令执行经产品诊断器追加提示后的输出。两臂共享执行结果但保留真实干预差异；
//! - **产物**：`artifacts/metrics/command-corpus/eval-paired-<rev>-<platform>.json`
//!   含逐行 JSONL（可回放）与汇总（lift/配对差值/红线检查）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 金集条目（corpus.jsonl 行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusEntry {
    pub id: String,
    pub cmd: String,
    /// `windows` | `macos` | `both`
    pub platform: String,
    /// `fast` | `slow`
    pub tier: String,
    pub category: String,
    /// `ok` | `fail` | `fail-with-hint`
    pub expect: String,
}

/// 从金集文件加载全部条目。
pub fn load_corpus(path: &Path) -> Result<Vec<CorpusEntry>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("read corpus: {error}"))?;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: CorpusEntry = serde_json::from_str(line)
            .map_err(|error| format!("corpus line {}: {error}", index + 1))?;
        // expect 枚举在加载期校验：手改语料的非法值在此报错，
        // 而不是留到 compute_met/paired_rows 的 unreachable! 里 panic。
        match entry.expect.as_str() {
            "ok" | "fail" | "fail-with-hint" => {}
            other => {
                return Err(format!(
                    "corpus line {}: invalid expect {:?} (expected one of: ok, fail, fail-with-hint)",
                    index + 1, other
                ));
            }
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// 单条命令的观察结果（真实执行路径产出；由宿主侧 runner 提供）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorpusObservation {
    pub id: String,
    /// gateway 前置拒绝（未 spawn）。
    pub blocked: bool,
    /// 工具层错误（spawn 失败等）。
    pub error: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// 输出文本（诊断重判与回放用；不落密钥——金集命令无密钥材料）。
    pub output: String,
}

/// 同一次命令执行在诊断干预前后的两份观察。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorpusObservationPair {
    pub baseline: CorpusObservation,
    pub candidate: CorpusObservation,
}

/// met 结算（对齐金集 runner compute_met；hint 由调用方判定后传入）。
pub fn compute_met(entry: &CorpusEntry, observation: &CorpusObservation, hint: bool) -> bool {
    // Harness/执行器故障不是被测命令的预期失败，所有 expect 下都必须计失败。
    if observation.error || observation.timed_out {
        return false;
    }
    match entry.expect.as_str() {
        "ok" => !observation.blocked && observation.exit_code == Some(0),
        "fail" => observation.blocked || observation.exit_code != Some(0),
        "fail-with-hint" => (observation.blocked || observation.exit_code != Some(0)) && hint,
        other => unreachable!("corpus schema expect 枚举已校验，非法值 {other}"),
    }
}

/// 诊断提示标记（对齐金集 runner HINT_MARKER）。
pub const HINT_MARKER: &str = "[诊断]";

/// 安全红线硬断言（R-EVL-04）：预期失败的 policy 条目必须被前置拒绝
///（blocked），触发即返回 Err 整场失败——不进 Judge、不进统计。
pub fn assert_safety_redlines(
    entries: &[CorpusEntry],
    observations: &BTreeMap<String, CorpusObservation>,
) -> Result<(), String> {
    for entry in entries
        .iter()
        .filter(|entry| entry.category == "policy" && entry.expect != "ok")
    {
        let Some(observation) = observations.get(&entry.id) else {
            return Err(format!(
                "safety redline missing observation for policy entry {}",
                entry.id
            ));
        };
        if !observation.blocked || observation.error || observation.timed_out {
            return Err(format!(
                "SAFETY REDLINE VIOLATION: policy command {:?} ({}) was not cleanly blocked by the gateway",
                entry.cmd, entry.id,
            ));
        }
    }
    Ok(())
}

/// 二臂逐条判定并配对。baseline 与 candidate 必须来自同一次命令执行的
/// 原始/诊断后输出；本函数只按各自真实输出中的提示标记结算。
pub struct PairedCorpusRow {
    pub id: String,
    pub baseline_met: bool,
    pub candidate_met: bool,
}

pub fn paired_rows(
    entries: &[CorpusEntry],
    baseline_observations: &BTreeMap<String, CorpusObservation>,
    candidate_observations: &BTreeMap<String, CorpusObservation>,
) -> Vec<PairedCorpusRow> {
    entries
        .iter()
        .map(|entry| {
            let baseline = baseline_observations
                .get(&entry.id)
                .expect("caller collects baseline observations for every eligible entry");
            let candidate = candidate_observations
                .get(&entry.id)
                .expect("caller collects candidate observations for every eligible entry");
            let baseline_met = compute_met(entry, baseline, baseline.output.contains(HINT_MARKER));
            let candidate_met =
                compute_met(entry, candidate, candidate.output.contains(HINT_MARKER));
            PairedCorpusRow {
                id: entry.id.clone(),
                baseline_met,
                candidate_met,
            }
        })
        .collect()
}

/// 汇总（Pass Rate Lift：candidate 符合率 − baseline 符合率）。
#[derive(Debug, Clone, Serialize)]
pub struct PairedCorpusSummary {
    pub entries: usize,
    pub baseline_met: usize,
    pub candidate_met: usize,
    pub baseline_rate: f64,
    pub candidate_rate: f64,
    pub pass_rate_lift: f64,
}

pub fn summarize(rows: &[PairedCorpusRow]) -> PairedCorpusSummary {
    let entries = rows.len();
    let baseline_met = rows.iter().filter(|row| row.baseline_met).count();
    let candidate_met = rows.iter().filter(|row| row.candidate_met).count();
    let rate = |met: usize| {
        if entries == 0 {
            0.0
        } else {
            met as f64 / entries as f64
        }
    };
    let baseline_rate = rate(baseline_met);
    let candidate_rate = rate(candidate_met);
    PairedCorpusSummary {
        entries,
        baseline_met,
        candidate_met,
        baseline_rate,
        candidate_rate,
        pass_rate_lift: candidate_rate - baseline_rate,
    }
}

/// 金集路径解析（repo_root 指向仓库根）。
pub fn corpus_path(repo_root: &Path) -> PathBuf {
    repo_root
        .join("crates")
        .join("r-code-gateway")
        .join("tests")
        .join("command_corpus")
        .join("corpus.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, expect: &str, category: &str) -> CorpusEntry {
        CorpusEntry {
            id: id.to_string(),
            cmd: format!("cmd-{id}"),
            platform: "both".to_string(),
            tier: "fast".to_string(),
            category: category.to_string(),
            expect: expect.to_string(),
        }
    }

    fn observation(
        id: &str,
        blocked: bool,
        error: bool,
        exit: Option<i32>,
        output: &str,
    ) -> CorpusEntryObservation {
        CorpusEntryObservation {
            id: id.to_string(),
            blocked,
            error,
            exit_code: exit,
            timed_out: false,
            output: output.to_string(),
        }
    }
    type CorpusEntryObservation = CorpusObservation;

    /// 金集加载：44 条、id 唯一、expect 枚举合法。
    #[test]
    fn corpus_loads_forty_four_unique_entries() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let path = corpus_path(&repo_root);
        let entries = load_corpus(&path).unwrap();
        assert_eq!(entries.len(), 44, "金集基线 44 条");
        let mut ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), entries.len(), "input.id（命令编号）必须唯一");
        for entry in &entries {
            assert!(matches!(
                entry.expect.as_str(),
                "ok" | "fail" | "fail-with-hint"
            ));
        }
    }

    /// 手改语料的非法 expect 必须在加载期报 Err（而不是后续 unreachable! panic）。
    #[test]
    fn load_corpus_rejects_invalid_expect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corpus.jsonl");
        let valid = concat!(
            r#"{"id":"a","cmd":"echo hi","platform":"both","tier":"fast","#,
            r#""category":"path","expect":"ok"}"#
        );
        let invalid = concat!(
            r#"{"id":"b","cmd":"sudo rm -rf /","platform":"both","tier":"fast","#,
            r#""category":"policy","expect":"maybe"}"#
        );
        std::fs::write(&path, format!("{valid}\n{invalid}\n")).unwrap();
        let error = load_corpus(&path).unwrap_err();
        assert!(error.contains("corpus line 2"), "{error}");
        assert!(error.contains("expect"), "{error}");
        assert!(error.contains("maybe"), "{error}");
        // 合法行单独可加载。
        std::fs::write(&path, valid).unwrap();
        assert_eq!(load_corpus(&path).unwrap().len(), 1);
    }

    /// M2-04.A2：安全红线硬断言——policy 未被拦截即 Err（fail 整场）。
    #[test]
    fn safety_redline_blocks_unblocked_policy_command() {
        let entries = vec![
            entry("allowed", "ok", "policy"),
            entry("sudo", "fail-with-hint", "policy"),
            entry("push", "fail-with-hint", "policy"),
        ];
        let mut observations = BTreeMap::new();
        observations.insert(
            "allowed".to_string(),
            observation("allowed", false, false, Some(0), "done"),
        );
        observations.insert(
            "sudo".to_string(),
            observation("sudo", true, false, None, "blocked"),
        );
        // 红线触发：push 未被拦截。
        observations.insert(
            "push".to_string(),
            observation("push", false, false, Some(0), "pushed!"),
        );
        let error = assert_safety_redlines(&entries, &observations).unwrap_err();
        assert!(error.contains("SAFETY REDLINE VIOLATION"));
        assert!(error.contains("push"));
        // 全部拦截：通过。
        observations.insert(
            "push".to_string(),
            observation("push", true, false, None, "blocked"),
        );
        assert!(assert_safety_redlines(&entries, &observations).is_ok());
        // 观察缺失：也是红线违例（不能静默跳过 policy 条目）。
        observations.remove("push");
        assert!(assert_safety_redlines(&entries, &observations).is_err());
    }

    /// 二臂判定 + Pass Rate Lift：candidate 必须携带真实追加的诊断提示。
    #[test]
    fn paired_rows_lift_arithmetic() {
        let entries = vec![
            entry("ok-1", "ok", "path"),
            entry("dialect-1", "fail-with-hint", "quoting"),
            entry("hard-1", "fail", "exit-code"),
        ];
        let mut baseline = BTreeMap::new();
        baseline.insert(
            "ok-1".to_string(),
            observation("ok-1", false, false, Some(0), "done"),
        );
        baseline.insert(
            "dialect-1".to_string(),
            observation(
                "dialect-1",
                false,
                false,
                Some(1),
                "bash: foo: command not found",
            ),
        );
        // 非零退出：两臂都 met（expect=fail）。
        baseline.insert(
            "hard-1".to_string(),
            observation("hard-1", false, false, Some(42), "exit: 42"),
        );
        let mut candidate = baseline.clone();
        candidate.get_mut("dialect-1").unwrap().output =
            "bash: foo: command not found\n\n[诊断] 请检查命令与当前 shell 方言。".to_string();
        let rows = paired_rows(&entries, &baseline, &candidate);
        assert_eq!(rows.len(), 3);
        let summary = summarize(&rows);
        assert_eq!(summary.baseline_met, 2);
        assert_eq!(summary.candidate_met, 3);
        assert!((summary.pass_rate_lift - 1.0 / 3.0).abs() < 1e-9);
    }

    /// compute_met 语义快照（对齐金集 runner）。
    #[test]
    fn compute_met_matches_corpus_semantics() {
        let ok_entry = entry("e", "ok", "path");
        assert!(compute_met(
            &ok_entry,
            &observation("e", false, false, Some(0), ""),
            false
        ));
        assert!(!compute_met(
            &ok_entry,
            &observation("e", false, false, Some(1), ""),
            false
        ));
        let fail_entry = entry("f", "fail", "exit-code");
        assert!(compute_met(
            &fail_entry,
            &observation("f", false, false, Some(42), ""),
            false
        ));
        let hint_entry = entry("h", "fail-with-hint", "policy");
        assert!(compute_met(
            &hint_entry,
            &observation("h", true, false, None, "[诊断] blocked"),
            true
        ));
        assert!(!compute_met(
            &hint_entry,
            &observation("h", true, false, None, "blocked"),
            false
        ));
        let mut harness_error = observation("f", false, true, None, "spawn failed");
        assert!(!compute_met(&fail_entry, &harness_error, false));
        harness_error.error = false;
        harness_error.timed_out = true;
        assert!(!compute_met(&fail_entry, &harness_error, false));
    }
}
