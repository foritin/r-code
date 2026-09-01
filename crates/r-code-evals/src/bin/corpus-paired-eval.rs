//! 金集配对评估入口（M2-04）：`cargo run -p r-code-evals --bin corpus-paired-eval`。
//!
//! 在真实执行路径上跑金集（fast 层、当前平台），收集观察 → 安全红线硬断言
//! → baseline/candidate 二臂判定（repetitions 次重跑观察以验证确定性）→
//! 产物落盘 `artifacts/metrics/command-corpus/eval-paired-*.json`（逐行 JSONL
//! 可回放 + 汇总）。退出码：0 = 全部通过且红线未触发；1 = 红线违例或断言失败。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use r_code_core::dto::ProjectAccessMode;
use r_code_core::dto::RiskLevel as Level;
use r_code_evals::corpus::{
    assert_safety_redlines, corpus_path, load_corpus, paired_rows, summarize, CorpusEntry,
    CorpusObservation,
};
use r_code_gateway::{classify_shell_command, BashTool, PermissionEngine, Tool, ToolGateway};
use tempfile::TempDir;

const COMMAND_TIMEOUT_MS: u64 = 60_000;

/// 执行单条语料（对齐金集 runner：policy 走 gateway，其余走 BashTool）。
async fn execute_entry(
    entry: &CorpusEntry,
    cwd: &std::path::Path,
    gateway: &ToolGateway,
) -> CorpusObservation {
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
            Ok(outcome) => CorpusObservation {
                id: entry.id.clone(),
                blocked: false,
                error: outcome.is_error,
                exit_code: parse_exit_code(&outcome.content),
                output: outcome.content,
            },
            Err(err) => CorpusObservation {
                id: entry.id.clone(),
                blocked: true,
                error: false,
                exit_code: None,
                output: err.to_string(),
            },
        };
    }
    match BashTool.execute(input).await {
        Ok(text) => CorpusObservation {
            id: entry.id.clone(),
            blocked: false,
            error: false,
            exit_code: parse_exit_code(&text),
            output: text,
        },
        Err(err) => CorpusObservation {
            id: entry.id.clone(),
            blocked: false,
            error: true,
            exit_code: None,
            output: err.to_string(),
        },
    }
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

#[tokio::main]
async fn main() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let repetitions: usize = std::env::var("CORPUS_EVAL_REPETITIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);

    let entries = load_corpus(&corpus_path(&repo_root)).expect("load corpus");
    let platform = host_platform();
    let eligible: Vec<&CorpusEntry> = entries
        .iter()
        .filter(|entry| entry.platform == "both" || entry.platform == platform)
        .filter(|entry| entry.tier == "fast")
        .collect();

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

    let workspace = TempDir::new().expect("workspace");
    let engine = Arc::new(PermissionEngine::default());
    let gateway = ToolGateway::new(engine);

    // repetitions 轮观察（纯代码命令确定性：逐轮 met 必须一致）。
    let mut all_rows = Vec::new();
    let mut repetition_summaries = Vec::new();
    let mut final_observations: BTreeMap<String, CorpusObservation> = BTreeMap::new();
    for repetition in 1..=repetitions {
        let mut observations = BTreeMap::new();
        for entry in &eligible {
            let observation = execute_entry(entry, workspace.path(), &gateway).await;
            observations.insert(entry.id.clone(), observation);
        }
        // 安全红线硬断言：触发即整场失败（不是扣分）。
        if let Err(message) = assert_safety_redlines(
            &eligible
                .iter()
                .map(|entry| (*entry).clone())
                .collect::<Vec<_>>(),
            &observations,
        ) {
            eprintln!("corpus-paired-eval: {message}");
            std::process::exit(1);
        }
        let rows = paired_rows(
            &eligible
                .iter()
                .map(|entry| (*entry).clone())
                .collect::<Vec<_>>(),
            &observations,
        );
        repetition_summaries.push(summarize(&rows));
        if repetition == repetitions {
            final_observations = observations;
            all_rows = rows;
        }
    }

    // 确定性检查：每轮汇总一致（金集纯代码命令的评估必须可复现）。
    let deterministic = repetition_summaries.windows(2).all(|pair| {
        pair[0].baseline_met == pair[1].baseline_met
            && pair[0].candidate_met == pair[1].candidate_met
    });

    let summary = repetition_summaries
        .last()
        .expect("at least one repetition");
    let report = serde_json::json!({
        "schema": "r-code-corpus-paired-eval/v1",
        "platform": platform,
        "repetitions": repetitions,
        "deterministic_across_repetitions": deterministic,
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
        eprintln!("corpus-paired-eval: repetitions 不一致——评估不可复现");
        std::process::exit(1);
    }
}
