//! Codex 链路重放评估 runner（M4-02.A3）。
//!
//! 读取离线取证样本（fixtures/windows-reliability/codex-replay-samples.jsonl，
//! 已脱敏），经**同源** `append_diagnosis`（codex 投影挂载的同一实现）重放，
//! 输出结构化 JSON 供 scripts/windows-reliability/replay-eval.mjs 校验。
//! 由 `CORPUS_REPLAY=1` 显式启用（普通 cargo test 跳过）。
//!
//! 真实 Codex 账号复测（≥92% 链路成功率）属外部放行：在装有已登录 codex CLI
//! 的机器上运行真实委派负载并按同 schema 收集——见 PRD §11.3。

use r_code_gateway::{append_diagnosis, classify_failure, codex_shell_dialect};

#[derive(Debug, serde::Deserialize)]
struct ReplaySample {
    id: String,
    output: String,
    exit_code: Option<i32>,
    /// 期望命中的诊断类别（null = 不应有提示）。
    expect_hint: Option<String>,
}

#[test]
fn replay_eval_offline() {
    if std::env::var("CORPUS_REPLAY").ok().as_deref() != Some("1") {
        println!("replay eval skipped: set CORPUS_REPLAY=1 to run the offline fixture replay");
        return;
    }
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_path = manifest_dir
        .join("..")
        .join("..")
        .join("fixtures")
        .join("windows-reliability")
        .join("codex-replay-samples.jsonl");
    let text = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", fixture_path.display()));

    let dialect = codex_shell_dialect();
    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut mismatches: Vec<serde_json::Value> = Vec::new();
    let mut hint_hits = 0usize;
    let mut total = 0usize;

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let sample: ReplaySample =
            serde_json::from_str(line).unwrap_or_else(|error| panic!("fixture parse: {error}"));
        total += 1;
        let annotated = append_diagnosis(&sample.output, sample.exit_code, dialect);
        let hit = classify_failure(&sample.output).map(|kind| kind.label().to_string());
        let expected = sample.expect_hint.clone();
        let matched = hit == expected;
        if hit.is_some() {
            hint_hits += 1;
        }
        let annotated_leaks_original = annotated.starts_with(sample.output.trim_end());
        if !matched || !annotated_leaks_original {
            mismatches.push(serde_json::json!({
                "id": sample.id,
                "hit": hit,
                "expected": expected,
                "original_preserved": annotated_leaks_original,
            }));
        }
        results.push(serde_json::json!({
            "id": sample.id,
            "hit": hit,
            "expected": expected,
            "matched": matched,
            "original_preserved": annotated_leaks_original,
        }));
    }

    let report = serde_json::json!({
        "schema_version": "codex-replay-eval.v1",
        "mode": "offline-fixture",
        "dialect": dialect.label(),
        "total": total,
        "hint_hits": hint_hits,
        "mismatch_count": mismatches.len(),
        "mismatches": mismatches,
        "samples": results,
    });
    println!("REPLAY_EVAL_JSON_BEGIN");
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    println!("REPLAY_EVAL_JSON_END");
    assert!(
        mismatches.is_empty(),
        "offline replay must match the sanitized forensic expectations"
    );
    assert!(
        total >= 8,
        "fixture corpus must cover the forensic categories"
    );
}
