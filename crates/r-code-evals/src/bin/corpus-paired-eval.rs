//! 金集配对评估入口（M2-04）：`cargo run -p r-code-evals --bin corpus-paired-eval`。
//!
//! 在真实执行路径上跑金集（fast 层、当前平台），收集观察 → 安全红线硬断言
//! → baseline/candidate 二臂判定（repetitions 次重跑观察以验证确定性）→
//! 产物落盘 `artifacts/metrics/command-corpus/eval-paired-*.json`（逐行 JSONL
//! 可回放 + 汇总）。退出码：0 = 全部通过且红线未触发；1 = 红线违例或断言失败。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use r_code_core::dto::ProjectAccessMode;
use r_code_core::dto::RiskLevel as Level;
use r_code_evals::corpus::{
    assert_safety_redlines, corpus_path, load_corpus, paired_rows, summarize, CorpusEntry,
    CorpusObservation, CorpusObservationPair,
};
use r_code_gateway::tools_command::execute_bash_for_evaluation;
use r_code_gateway::{classify_shell_command, BashTool, PermissionEngine, ToolGateway};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const COMMAND_TIMEOUT_MS: u64 = 60_000;

/// 执行单条语料（对齐金集 runner：policy 走 gateway，其余走 BashTool）。
async fn execute_entry(
    entry: &CorpusEntry,
    cwd: &Path,
    gateway: &ToolGateway,
) -> CorpusObservationPair {
    let input = serde_json::json!({
        "command": entry.cmd,
        "cwd": cwd.to_string_lossy(),
        "timeout_ms": COMMAND_TIMEOUT_MS,
    });
    if entry.category == "policy" {
        return match gateway
            .execute_call_with_access_mode_and_workspace_guard(
                "corpus-eval-task",
                "corpus-eval-run",
                "bash",
                input,
                None,
                ProjectAccessMode::FullAccess,
                None,
            )
            .await
        {
            Ok(outcome) => {
                // 允许执行的 policy smoke 命令必须成功且不会触发诊断，因此 gateway
                // 返回值可同时作为两臂观察。危险 policy 会在 spawn 前走 Err 分支。
                let observation = CorpusObservation {
                    id: entry.id.clone(),
                    blocked: false,
                    error: outcome.is_error,
                    exit_code: parse_exit_code(&outcome.content),
                    timed_out: outcome.content.contains("[超时]"),
                    output: outcome.content,
                };
                CorpusObservationPair {
                    baseline: observation.clone(),
                    candidate: observation,
                }
            }
            Err(err) => {
                // Gateway 的产品路径已追加诊断。baseline 截取追加前的原始拒绝，
                // candidate 保留真实返回值，避免再追加一次相同提示。
                let diagnosed_output = err.to_string();
                let raw_output = diagnosed_output
                    .split_once("\n\n[诊断]")
                    .map_or_else(|| diagnosed_output.clone(), |(raw, _)| raw.to_string());
                let blocked = raw_output.contains("risk level R4: pre-rejected by policy")
                    || raw_output.contains("denied by standing rule");
                CorpusObservationPair {
                    baseline: CorpusObservation {
                        id: entry.id.clone(),
                        blocked,
                        error: !blocked,
                        exit_code: None,
                        timed_out: false,
                        output: raw_output,
                    },
                    candidate: CorpusObservation {
                        id: entry.id.clone(),
                        blocked,
                        error: !blocked,
                        exit_code: None,
                        timed_out: false,
                        output: diagnosed_output,
                    },
                }
            }
        };
    }
    match execute_bash_for_evaluation(input).await {
        Ok(observation) => CorpusObservationPair {
            baseline: CorpusObservation {
                id: entry.id.clone(),
                blocked: false,
                error: false,
                exit_code: observation.exit_code,
                timed_out: observation.timed_out,
                output: observation.raw_output,
            },
            candidate: CorpusObservation {
                id: entry.id.clone(),
                blocked: false,
                error: false,
                exit_code: observation.exit_code,
                timed_out: observation.timed_out,
                output: observation.diagnosed_output,
            },
        },
        Err(err) => {
            let output = err.to_string();
            let observation = CorpusObservation {
                id: entry.id.clone(),
                blocked: false,
                error: true,
                exit_code: None,
                timed_out: false,
                output,
            };
            CorpusObservationPair {
                baseline: observation.clone(),
                candidate: observation,
            }
        }
    }
}

fn normalize_workspace_paths(output: &str, workspace: &Path) -> String {
    let native = workspace.to_string_lossy().into_owned();
    let slash = native.replace('\\', "/");
    let mut variants = vec![native, slash.clone()];
    #[cfg(windows)]
    if let Some((drive, rest)) = slash.split_once(":/") {
        variants.push(format!("/{}/{}", drive.to_ascii_lowercase(), rest));
        variants.push(format!("/{}/{}", drive.to_ascii_uppercase(), rest));
    }
    #[cfg(windows)]
    if let Some(name) = workspace.file_name().and_then(|name| name.to_str()) {
        // Git Bash maps Windows' temporary directory to its stable `/tmp` mount.
        variants.push(format!("/tmp/{name}"));
    }
    variants.sort_by_key(|value| std::cmp::Reverse(value.len()));
    variants.dedup();

    let mut normalized = output.to_string();
    for variant in variants {
        normalized = normalized.replace(&variant, "<WORKSPACE>");
    }
    normalized
}

fn normalize_pair(mut pair: CorpusObservationPair, workspace: &Path) -> CorpusObservationPair {
    pair.baseline.output = normalize_workspace_paths(&pair.baseline.output, workspace);
    pair.candidate.output = normalize_workspace_paths(&pair.candidate.output, workspace);
    pair
}

fn observation_digest(observations: &BTreeMap<String, CorpusObservationPair>) -> String {
    let bytes = serde_json::to_vec(observations).expect("serialize deterministic observations");
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn parse_repetitions(value: Option<&str>) -> Result<usize, String> {
    let Some(value) = value else {
        return Ok(3);
    };
    let repetitions = value
        .parse::<usize>()
        .map_err(|error| format!("CORPUS_EVAL_REPETITIONS must be a positive integer: {error}"))?;
    if repetitions == 0 {
        return Err("CORPUS_EVAL_REPETITIONS must be greater than zero".to_string());
    }
    Ok(repetitions)
}

fn parse_exit_code(output: &str) -> Option<i32> {
    for line in output.lines() {
        let Some(rest) = line.trim().strip_prefix("exit:") else {
            continue;
        };
        let token = rest.trim().split(['（', ' ']).next().unwrap_or("");
        return token.parse::<i32>().ok();
    }
    None
}

fn host_platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// 平台匹配：金集 schema 用 `macos` 标注（corpus.rs 文档），宿主侧历史上称
/// `darwin`——两者视为等价，避免正确标注的条目在 macOS 上静默不跑。
fn platform_matches(entry_platform: &str, host: &str) -> bool {
    if entry_platform == host {
        return true;
    }
    matches!(
        (entry_platform, host),
        ("macos", "darwin") | ("darwin", "macos")
    )
}

#[tokio::main]
async fn main() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let repetitions_value = std::env::var("CORPUS_EVAL_REPETITIONS").ok();
    let repetitions = match parse_repetitions(repetitions_value.as_deref()) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("corpus-paired-eval: {message}");
            std::process::exit(1);
        }
    };

    let entries = load_corpus(&corpus_path(&repo_root)).expect("load corpus");
    let platform = host_platform();
    let eligible: Vec<&CorpusEntry> = entries
        .iter()
        .filter(|entry| entry.platform == "both" || platform_matches(&entry.platform, platform))
        .filter(|entry| entry.tier == "fast")
        .collect();
    // 空合格集是断言失败而非静默空转（否则 0 条目、lift 0 会以退出码 0 假通过）。
    if eligible.is_empty() {
        eprintln!(
            "corpus-paired-eval: eligible set is empty (platform={platform}, tier=fast)——请检查金集 platform/tier 标注"
        );
        std::process::exit(1);
    }

    // 安全前置：policy 条目分类器定级必须 ≥R3（对齐金集 runner preflight）。
    for entry in eligible.iter().filter(|entry| entry.category == "policy") {
        let level = classify_shell_command(&entry.cmd).level;
        if entry.expect != "ok" && (level as u8) < (Level::R3 as u8) {
            eprintln!(
                "corpus-paired-eval: policy {} 分类器定级 {:?} < R3",
                entry.id, level
            );
            std::process::exit(1);
        }
    }

    let engine = Arc::new(PermissionEngine::default());
    let mut gateway = ToolGateway::new(engine);
    gateway.register(Box::new(BashTool));

    let eligible_entries = eligible
        .iter()
        .map(|entry| (*entry).clone())
        .collect::<Vec<_>>();

    // 每条命令使用独立 workspace，避免语料之间以及 repetitions 之间共享状态。
    // 对路径归一化后的完整观察做摘要，任何输出/元数据漂移都会使确定性检查失败。
    let mut all_rows = Vec::new();
    let mut repetition_summaries = Vec::new();
    let mut repetition_hashes = Vec::new();
    let mut reference_observations: Option<BTreeMap<String, CorpusObservationPair>> = None;
    let mut nondeterministic_ids = BTreeSet::new();
    let mut final_observations: BTreeMap<String, CorpusObservationPair> = BTreeMap::new();
    for repetition in 1..=repetitions {
        let mut observations = BTreeMap::new();
        for entry in &eligible {
            let workspace = TempDir::new().expect("workspace");
            let observation = execute_entry(entry, workspace.path(), &gateway).await;
            observations.insert(
                entry.id.clone(),
                normalize_pair(observation, workspace.path()),
            );
        }
        let baseline_observations = observations
            .iter()
            .map(|(id, pair)| (id.clone(), pair.baseline.clone()))
            .collect::<BTreeMap<_, _>>();
        let candidate_observations = observations
            .iter()
            .map(|(id, pair)| (id.clone(), pair.candidate.clone()))
            .collect::<BTreeMap<_, _>>();
        // 安全红线硬断言：触发即整场失败（不是扣分）。
        for arm in [&baseline_observations, &candidate_observations] {
            if let Err(message) = assert_safety_redlines(&eligible_entries, arm) {
                eprintln!("corpus-paired-eval: {message}");
                std::process::exit(1);
            }
        }
        let rows = paired_rows(
            &eligible_entries,
            &baseline_observations,
            &candidate_observations,
        );
        repetition_summaries.push(summarize(&rows));
        repetition_hashes.push(observation_digest(&observations));
        if let Some(reference) = &reference_observations {
            for (id, observation) in &observations {
                if reference.get(id) != Some(observation) {
                    nondeterministic_ids.insert(id.clone());
                }
            }
        } else {
            reference_observations = Some(observations.clone());
        }
        if repetition == repetitions {
            final_observations = observations;
            all_rows = rows;
        }
    }

    let deterministic = repetition_hashes.windows(2).all(|pair| pair[0] == pair[1]);

    let summary = repetition_summaries
        .last()
        .expect("at least one repetition");
    let report = serde_json::json!({
        "schema": "r-code-corpus-paired-eval/v2",
        "platform": platform,
        "repetitions": repetitions,
        "deterministic_across_repetitions": deterministic,
        "repetition_hashes": repetition_hashes,
        "nondeterministic_ids": nondeterministic_ids,
        "summary": summary,
        "rows": all_rows.iter().map(|row| serde_json::json!({
            "id": row.id,
            "baseline_met": row.baseline_met,
            "candidate_met": row.candidate_met,
        })).collect::<Vec<_>>(),
        "observations": final_observations.values().collect::<Vec<_>>(),
    });

    let out_dir = repo_root
        .join("artifacts")
        .join("metrics")
        .join("command-corpus");
    std::fs::create_dir_all(&out_dir).expect("create metrics dir");
    let rev = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&repo_root)
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let out_path = out_dir.join(format!("eval-paired-{rev}-{platform}.json"));
    std::fs::write(
        &out_path,
        serde_json::to_vec_pretty(&report).expect("serialize"),
    )
    .expect("write report");

    println!(
        "corpus-paired-eval: {} entries, baseline {:.1}% candidate {:.1}% lift {:+.1}% ({} reps, deterministic={})",
        summary.entries,
        summary.baseline_rate * 100.0,
        summary.candidate_rate * 100.0,
        summary.pass_rate_lift * 100.0,
        repetitions,
        deterministic
    );
    println!("report: {}", out_path.display());
    if !deterministic {
        eprintln!(
            "corpus-paired-eval: repetitions 不一致——评估不可复现；漂移条目: {:?}",
            nondeterministic_ids
        );
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repetitions_must_be_positive() {
        assert_eq!(parse_repetitions(None).unwrap(), 3);
        assert_eq!(parse_repetitions(Some("2")).unwrap(), 2);
        assert!(parse_repetitions(Some("0")).is_err());
        assert!(parse_repetitions(Some("invalid")).is_err());
    }

    #[test]
    fn platform_matches_treats_macos_and_darwin_as_equivalent() {
        // 金集 schema 标注 `macos`、宿主侧称 `darwin`：必须互相匹配。
        assert!(platform_matches("macos", "darwin"));
        assert!(platform_matches("darwin", "macos"));
        assert!(platform_matches("windows", "windows"));
        assert!(platform_matches("linux", "linux"));
        assert!(!platform_matches("windows", "darwin"));
        assert!(!platform_matches("macos", "windows"));
    }

    #[test]
    fn workspace_paths_are_normalized_before_hashing() {
        let workspace = if cfg!(windows) {
            Path::new(r"C:\Users\example\Temp\.tmp123")
        } else {
            Path::new("/tmp/.tmp123")
        };
        let native = workspace.to_string_lossy();
        let output = format!("cwd={native}");
        assert_eq!(
            normalize_workspace_paths(&output, workspace),
            "cwd=<WORKSPACE>"
        );
    }
}
