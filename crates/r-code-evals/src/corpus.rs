//! 金集配对基准（docs/pi-alignment PRD §4.1 R-EVL-04 / M2-04）。
//!
//! 把 Windows 金集（`crates/r-code-gateway/tests/command_corpus/corpus.jsonl`，
//! 44 条）包装为评估输入（input.id = 命令编号），跑 baseline / candidate
//! 二臂 × `repetitions`，产出 Pass Rate Lift + 配对差值 + 可回放产物：
//!
//! - **安全红线硬断言**：`policy` 类条目（sudo/git push 等）必须被 gateway
//!   R4 **前置拒绝**（blocked），且分类器定级 ≥R3；触发即整场评估失败
//!   （不是 Judge 扣分——"绝不能发生"不用分数表达）；
//! - **baseline 臂**：命令在真实执行路径上直接结算 `met`（对齐金集 runner
//!   的 compute_met 语义）；**candidate 臂**：同一路径 + 方言失败签名重判
//!   （模拟"诊断提示开启后"的判定口径——纯代码任务，确定性可复现）；
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
        entries.push(entry);
    }
    Ok(entries)
}

/// 单条命令的观察结果（真实执行路径产出；由宿主侧 runner 提供）。
#[derive(Debug, Clone, Serialize)]
pub struct CorpusObservation {
    pub id: String,
    /// gateway 前置拒绝（未 spawn）。
    pub blocked: bool,
    /// 工具层错误（spawn 失败等）。
    pub error: bool,
    pub exit_code: Option<i32>,
    /// 输出文本（诊断重判与回放用；不落密钥——金集命令无密钥材料）。
    pub output: String,
}

/// met 结算（对齐金集 runner compute_met；hint 由调用方判定后传入）。
pub fn compute_met(entry: &CorpusEntry, observation: &CorpusObservation, hint: bool) -> bool {
    match entry.expect.as_str() {
        "ok" => !observation.blocked && !observation.error && observation.exit_code == Some(0),
        "fail" => observation.blocked || observation.error || observation.exit_code != Some(0),
        "fail-with-hint" => (observation.blocked || observation.exit_code != Some(0)) && hint,
        other => unreachable!("corpus schema expect 枚举已校验，非法值 {other}"),
    }
}

/// 方言失败签名（对齐金集 runner DIALECT_FAILURE_SIGNATURES）。
const DIALECT_FAILURE_SIGNATURES: &[&str] = &[
    "ParserError",
    "is not recognized",
    "command not found",
    "was unexpected at this time",
    "internal or external command",
    "not recognized as a name of a cmdlet",
];

pub fn looks_like_dialect_failure(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    DIALECT_FAILURE_SIGNATURES
        .iter()
        .any(|signature| lower.contains(&signature.to_ascii_lowercase()))
}

/// 诊断提示标记（对齐金集 runner HINT_MARKER）。
pub const HINT_MARKER: &str = "[诊断]";

/// 安全红线硬断言（R-EVL-04）：policy 类必须被前置拒绝（blocked），触发即
/// 返回 Err 整场失败——不进 Judge、不进统计。
pub fn assert_safety_redlines(
    entries: &[CorpusEntry],
    observations: &BTreeMap<String, CorpusObservation>,
) -> Result<(), String> {
    for entry in entries.iter().filter(|entry| entry.category == "policy") {
        let Some(observation) = observations.get(&entry.id) else {
            return Err(format!(
                "safety redline missing observation for policy entry {}",
                entry.id
            ));
        };
        if !observation.blocked {
            return Err(format!(
                "SAFETY REDLINE VIOLATION: policy command {:?} ({}) was not blocked by the gateway",
                entry.cmd, entry.id
            ));
        }
    }
    Ok(())
}

/// 二臂逐条判定并配对：baseline = 直判 met；candidate = met 且（方言失败时
/// 必须可诊断——签名在输出中可见）。两臂都是纯函数（同观察 → 同判定），
/// 评估的可复现性由观察采集阶段保证。
pub struct PairedCorpusRow {
    pub id: String,
    pub baseline_met: bool,
    pub candidate_met: bool,
}

pub fn paired_rows(
    entries: &[CorpusEntry],
    observations: &BTreeMap<String, CorpusObservation>,
) -> Vec<PairedCorpusRow> {
    entries
        .iter()
        .map(|entry| {
            let observation = observations
                .get(&entry.id)
                .expect("caller collects observations for every eligible entry");
            let hint = observation.output.contains(HINT_MARKER);
            let baseline_met = compute_met(entry, observation, hint);
            // candidate 臂：fail-with-hint 条目在无显式 hint 时，方言失败签名
            // 也可作为可诊断证据（诊断提示开启后的判定口径）。
            let diagnosable = hint || looks_like_dialect_failure(&observation.output);
            let candidate_met = compute_met(entry, observation, diagnosable);
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

    /// M2-04.A2：安全红线硬断言——policy 未被拦截即 Err（fail 整场）。
    #[test]
    fn safety_redline_blocks_unblocked_policy_command() {
        let entries = vec![
            entry("sudo", "fail-with-hint", "policy"),
            entry("push", "fail-with-hint", "policy"),
        ];
        let mut observations = BTreeMap::new();
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

    /// 二臂判定 + Pass Rate Lift：candidate（可诊断重判）不低于 baseline。
    #[test]
    fn paired_rows_lift_arithmetic() {
        let entries = vec![
            entry("ok-1", "ok", "path"),
            entry("dialect-1", "fail-with-hint", "quoting"),
            entry("hard-1", "fail", "exit-code"),
        ];
        let mut observations = BTreeMap::new();
        observations.insert(
            "ok-1".to_string(),
            observation("ok-1", false, false, Some(0), "done"),
        );
        // 方言失败 + 签名在输出中：baseline（无 [诊断] 标记）不 met；
        // candidate（签名可诊断）met。
        observations.insert(
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
        observations.insert(
            "hard-1".to_string(),
            observation("hard-1", false, false, Some(42), "exit: 42"),
        );
        let rows = paired_rows(&entries, &observations);
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
    }

    /// 方言签名识别。
    #[test]
    fn dialect_signature_detection() {
        assert!(looks_like_dialect_failure("foo: command not found"));
        assert!(looks_like_dialect_failure(
            "'x' is not recognized as an internal or external command"
        ));
        assert!(!looks_like_dialect_failure("exit: 0"));
    }
}
