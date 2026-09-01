//! Judge 抽象与确定性 Judge（docs/pi-alignment PRD §4.1 R-EVL-02 / M2-02）。
//!
//! [`Judge`] = `scoringFn -> { score ∈ [0,1], rationale }`。失败原因**累积**
//! （rationale 汇总全部 failures，不折叠成单一布尔）；确定性规则优先，LLM
//! Judge 以同签名评分函数注入（[`create_judge`] 不关心函数由谁实现）。

use std::path::Path;
use std::sync::Arc;

use crate::{EvalInput, EvalRunResult};

/// 一次打分结论。
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeVerdict {
    /// [0, 1]；1 = 完全通过（配对统计的 pass 判据 = score >= 1）。
    pub score: f64,
    /// 人可读依据（汇总失败原因）。
    pub rationale: String,
    /// 累积的失败原因（空 = 无失败）。
    pub failures: Vec<String>,
}

impl JudgeVerdict {
    /// 全通过。
    pub fn pass(rationale: impl Into<String>) -> Self {
        Self {
            score: 1.0,
            rationale: rationale.into(),
            failures: Vec::new(),
        }
    }

    /// 从累积失败原因合成结论：每条失败扣一档（n 条失败 → max(0, 1-n/权重)）。
    pub fn from_failures(failures: Vec<String>, total_checks: usize) -> Self {
        let total = total_checks.max(failures.len());
        let score = if total == 0 {
            0.0
        } else {
            (total - failures.len()) as f64 / total as f64
        };
        let rationale = if failures.is_empty() {
            format!("{total}/{total} checks passed")
        } else {
            format!(
                "{}/{} checks passed; failures: {}",
                total - failures.len(),
                total,
                failures.join("; ")
            )
        };
        Self {
            score,
            rationale,
            failures,
        }
    }
}

/// 打分函数入参：输入、结果、（可选）fixture 原貌（改动面对比基准）。
pub struct JudgeInput<'a> {
    pub input: &'a EvalInput,
    pub result: &'a EvalRunResult,
    /// fixture 目录（输入未带 fixture 时为 None）。
    pub fixture: Option<&'a Path>,
}

/// 评分函数（确定性规则或 LLM 评注器共用同一签名——LLM 扩展点）。
pub type ScoringFn = Arc<dyn Fn(&JudgeInput<'_>) -> JudgeVerdict + Send + Sync>;

/// Judge：命名 + 评分函数。
pub struct Judge {
    pub name: String,
    scoring: ScoringFn,
}

/// 构造 Judge（LLM Judge 扩展点 = 传入自实现的 ScoringFn，签名即合同）。
pub fn create_judge(name: impl Into<String>, scoring: ScoringFn) -> Judge {
    Judge {
        name: name.into(),
        scoring,
    }
}

impl Judge {
    /// 打分（每个 Judge 内部确定性可复现）。
    pub fn score(&self, input: &JudgeInput<'_>) -> JudgeVerdict {
        (self.scoring)(input)
    }
}

/// 内置确定性 Judge #1：测试通过率——在工作区运行 `verify.mjs`
/// （plan_eval 金集约定：每个 case 携带冻结验证脚本），exit 0 = 通过。
/// 工作区没有 verify.mjs 视为不可打分（score 0 + 失败原因，不臆造通过）。
pub fn test_pass_judge() -> Judge {
    create_judge(
        "test-pass",
        Arc::new(|input| {
            let mut failures = Vec::new();
            let verify = input.result.workspace.join("verify.mjs");
            if !verify.exists() {
                failures.push(format!(
                    "workspace has no verify.mjs (looked at {})",
                    verify.display()
                ));
            } else {
                let output = std::process::Command::new("node")
                    .arg("verify.mjs")
                    .current_dir(&input.result.workspace)
                    .output();
                match output {
                    Ok(finished) if finished.status.success() => {}
                    Ok(finished) => failures.push(format!(
                        "verify.mjs exited with {}: {}",
                        finished.status,
                        String::from_utf8_lossy(&finished.stderr)
                            .chars()
                            .take(300)
                            .collect::<String>()
                    )),
                    Err(error) => failures.push(format!("spawn node verify.mjs: {error}")),
                }
            }
            // 未收敛的 run 直接判失败（stopReason 非 stop 不得计通过）。
            if !input.result.stop_reason.is_settled() {
                failures.push(format!(
                    "run did not settle: {:?}",
                    input.result.stop_reason
                ));
            }
            JudgeVerdict::from_failures(failures, 2)
        }),
    )
}

/// 内置确定性 Judge #2：改动面——相对 fixture，只允许改声明过的路径
/// （`allowed_changes`）；越界路径逐条累积。
pub fn focus_judge(allowed_changes: Vec<String>) -> Judge {
    create_judge(
        "focus",
        Arc::new(move |input| {
            let mut failures = Vec::new();
            let Some(fixture) = input.fixture else {
                return JudgeVerdict::from_failures(
                    vec!["focus judge requires a fixture baseline".to_string()],
                    1,
                );
            };
            for changed in changed_paths(fixture, &input.result.workspace) {
                if !allowed_changes.iter().any(|allowed| {
                    changed == *allowed || changed.starts_with(&format!("{allowed}/"))
                }) {
                    failures.push(format!("unexpected workspace change: {changed}"));
                }
            }
            let total = failures.len() + 1;
            JudgeVerdict::from_failures(failures, total)
        }),
    )
}

/// 内置确定性 Judge #3：测试文件完整性——fixture 里的测试文件（verify.mjs /
/// *.test.* / tests/ 目录）在结果工作区必须原样存在（不被删除或改写）。
pub fn integrity_judge() -> Judge {
    create_judge(
        "integrity",
        Arc::new(|input| {
            let mut failures = Vec::new();
            let Some(fixture) = input.fixture else {
                return JudgeVerdict::from_failures(
                    vec!["integrity judge requires a fixture baseline".to_string()],
                    1,
                );
            };
            let is_test_file = |path: &Path| -> bool {
                let text = path.to_string_lossy().replace('\\', "/");
                text.ends_with("verify.mjs")
                    || text.contains(".test.")
                    || text.starts_with("tests/")
                    || text.contains("/tests/")
            };
            let mut checked = 0usize;
            for relative in test_files(fixture, &is_test_file) {
                checked += 1;
                let before = std::fs::read(fixture.join(&relative));
                let after = std::fs::read(input.result.workspace.join(&relative));
                match (before, after) {
                    (Ok(before), Ok(after)) if before == after => {}
                    (Ok(_), Ok(_)) => {
                        failures.push(format!("test file modified: {relative}"));
                    }
                    (_, Err(_)) => {
                        failures.push(format!("test file missing: {relative}"));
                    }
                    (Err(_), Ok(_)) => {
                        failures.push(format!("test file unreadable in fixture: {relative}"));
                    }
                }
            }
            if checked == 0 {
                failures.push("fixture contains no test files to protect".to_string());
            }
            JudgeVerdict::from_failures(failures, checked.max(1))
        }),
    )
}

/// 相对 fixture 的改动路径集合（相对路径，`/` 分隔；仅在结果侧存在的文件也算改动）。
fn changed_paths(fixture: &Path, workspace: &Path) -> Vec<String> {
    let mut changed = Vec::new();
    let walk = |root: &Path, prefix: &str, sink: &mut dyn FnMut(String, u64)| {
        fn visit(dir: &Path, prefix: &str, sink: &mut dyn FnMut(String, u64)) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let relative = if prefix.is_empty() {
                    entry.file_name().to_string_lossy().into_owned()
                } else {
                    format!("{prefix}/{}", entry.file_name().to_string_lossy())
                };
                if path.is_dir() {
                    visit(&path, &relative, sink);
                } else {
                    let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                    sink(relative, size);
                }
            }
        }
        visit(root, prefix, sink);
    };
    let mut before: std::collections::BTreeMap<String, u64> = Default::default();
    walk(fixture, "", &mut |relative, digest| {
        before.insert(relative, digest);
    });
    let mut after: std::collections::BTreeMap<String, u64> = Default::default();
    walk(workspace, "", &mut |relative, digest| {
        after.insert(relative, digest);
    });
    for (relative, digest) in &after {
        match before.get(relative) {
            Some(before_digest) if before_digest == digest => {}
            _ => changed.push(relative.clone()),
        }
    }
    for relative in before.keys() {
        if !after.contains_key(relative) {
            changed.push(relative.clone());
        }
    }
    changed
}

/// fixture 内的测试文件相对路径集合。
fn test_files(fixture: &Path, is_test_file: &dyn Fn(&Path) -> bool) -> Vec<String> {
    fn visit(
        dir: &Path,
        prefix: &str,
        is_test_file: &dyn Fn(&Path) -> bool,
        out: &mut Vec<String>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = if prefix.is_empty() {
                entry.file_name().to_string_lossy().into_owned()
            } else {
                format!("{prefix}/{}", entry.file_name().to_string_lossy())
            };
            if path.is_dir() {
                visit(&path, &relative, is_test_file, out);
            } else if is_test_file(Path::new(&relative)) {
                out.push(relative);
            }
        }
    }
    let mut out = Vec::new();
    visit(fixture, "", is_test_file, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvalRunResult, EvalStopReason, EvalTimings};
    use std::path::PathBuf;

    fn result_with(workspace: &std::path::Path, settled: bool) -> EvalRunResult {
        EvalRunResult {
            harness: "r-code".to_string(),
            input_id: "case".to_string(),
            output: String::new(),
            usage_json: None,
            timings: EvalTimings { wall_ms: 10 },
            events: Vec::new(),
            stop_reason: if settled {
                EvalStopReason::Settled
            } else {
                EvalStopReason::NotSettled("budget".to_string())
            },
            workspace: workspace.to_path_buf(),
        }
    }

    /// M2-02.A1：score ∈ [0,1] + rationale + 失败累积（不折叠单布尔）。
    #[test]
    fn verdicts_accumulate_failures() {
        let no_fail = JudgeVerdict::from_failures(vec![], 3);
        assert_eq!(no_fail.score, 1.0);
        assert!(no_fail.rationale.contains("3/3"));
        let two_fails =
            JudgeVerdict::from_failures(vec!["a broke".to_string(), "b broke".to_string()], 3);
        assert!(
            (two_fails.score - 1.0 / 3.0).abs() < 1e-9,
            "score={}",
            two_fails.score
        );
        assert!(two_fails.rationale.contains("a broke"));
        assert!(
            two_fails.rationale.contains("b broke"),
            "failures accumulate"
        );
        assert_eq!(two_fails.failures.len(), 2);
        // 越界钳制。
        let over = JudgeVerdict::from_failures(vec!["x".to_string()], 0);
        assert_eq!(over.score, 0.0);
    }

    /// M2-02.A2：TestPassJudge 确定性可复现——同一工作区两次打分逐字节一致。
    #[test]
    fn test_pass_judge_is_deterministic() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("verify.mjs"), "process.exit(0);\n").unwrap();
        let input = EvalInput::new("case", "do");
        let result = result_with(workspace.path(), true);
        let judge = test_pass_judge();
        let first = judge.score(&JudgeInput {
            input: &input,
            result: &result,
            fixture: None,
        });
        let second = judge.score(&JudgeInput {
            input: &input,
            result: &result,
            fixture: None,
        });
        assert_eq!(first, second);
        assert_eq!(first.score, 1.0, "passing verify + settled => 1.0");
        // 失败腿：verify 失败 + 未收敛 = 两条失败原因累积。
        std::fs::write(workspace.path().join("verify.mjs"), "process.exit(1);\n").unwrap();
        let failed_result = result_with(workspace.path(), false);
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &failed_result,
            fixture: None,
        });
        assert_eq!(verdict.failures.len(), 2);
        assert!(verdict.score < 1.0);
    }

    /// M2-02.A2（续）：FocusJudge——越界改动逐条累积，允许清单内不罚。
    #[test]
    fn focus_judge_flags_out_of_scope_changes() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("keep.txt"), "base").unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("keep.txt"), "base").unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src").join("lib.ts"), "new").unwrap();
        std::fs::write(workspace.path().join("stray.txt"), "oops").unwrap();

        let input = EvalInput::new("case", "do");
        let result = result_with(workspace.path(), true);
        let judge = focus_judge(vec!["src".to_string()]);
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &result,
            fixture: Some(fixture.path()),
        });
        assert_eq!(
            verdict.failures,
            vec!["unexpected workspace change: stray.txt".to_string()]
        );
        assert!(verdict.score < 1.0);
    }

    /// M2-02.A2（续）：IntegrityJudge——测试文件被删/被改判失败，原样保留通过。
    #[test]
    fn integrity_judge_protects_test_files() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fixture.path().join("tests")).unwrap();
        std::fs::write(fixture.path().join("tests").join("a.test.ts"), "1").unwrap();
        std::fs::write(fixture.path().join("verify.mjs"), "ok").unwrap();
        std::fs::write(fixture.path().join("note.md"), "x").unwrap();

        // 完整保留：通过。
        let intact = tempfile::tempdir().unwrap();
        for relative in ["tests/a.test.ts", "verify.mjs"] {
            let target = intact.path().join(relative);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(fixture.path().join(relative), &target).unwrap();
        }
        let input = EvalInput::new("case", "do");
        let judge = integrity_judge();
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &result_with(intact.path(), true),
            fixture: Some(fixture.path()),
        });
        assert_eq!(verdict.score, 1.0, "{:?}", verdict.failures);

        // 改写测试文件：失败且指名道姓。
        std::fs::write(intact.path().join("verify.mjs"), "tampered").unwrap();
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &result_with(intact.path(), true),
            fixture: Some(fixture.path()),
        });
        assert!(verdict
            .failures
            .iter()
            .any(|failure| failure.contains("verify.mjs")));
    }

    /// M2-02.A3：LLM Judge 扩展点——create_judge 接受任意同签名函数。
    #[test]
    fn llm_judge_extension_point_signature() {
        let judge = create_judge(
            "llm",
            Arc::new(|_input: &JudgeInput<'_>| JudgeVerdict {
                score: 0.75,
                rationale: "model-graded".to_string(),
                failures: vec!["partial".to_string()],
            }),
        );
        let input = EvalInput::new("case", "do");
        let workspace = PathBuf::from(".");
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &result_with(&workspace, true),
            fixture: None,
        });
        assert_eq!(verdict.score, 0.75);
        assert_eq!(judge.name, "llm");
    }
}
