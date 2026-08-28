//! 命令金集 runner（PRD §4.4 / M0-01）。
//!
//! 通过环境变量 `CORPUS_RUN=fast|slow|all` 显式启用（普通 `cargo test` 跳过），
//! 由 `scripts/windows-reliability/corpus-run.mjs` 编排调用。每条语料在临时
//! 工作区内经**真实产品执行路径**执行：
//! - `policy` 类经 `ToolGateway`（R4 前置拒绝是产品权限链路的一部分）；
//! - 其余类直接经 `BashTool::execute`（与 gateway Allowed 分支同一实现）。
//!
//! 产出 `artifacts/metrics/command-corpus/report-<git-sha>-<platform>.json`。
//!
//! `ok` 的语义是「结果符合 expect」（含预期失败与预期失败+诊断提示），
//! `fail` 是「结果不符合 expect」——金集的成功率指标按符合率结算。

use std::path::{Path, PathBuf};
use std::time::Instant;

use r_code_core::dto::{ProjectAccessMode, RiskLevel};
use r_code_gateway::{
    classify_shell_command, current_shell_dialect_label, BashTool, PermissionEngine, Tool,
    ToolGateway,
};
use tempfile::TempDir;

/// M2-02 诊断提示的稳定标记；提示引擎落成前该标记不会出现（hint_hits=0）。
const HINT_MARKER: &str = "[诊断]";
/// 判定为「方言类失败」的错误签名子串（对齐 PRD §4.3 模式表）。
const DIALECT_FAILURE_SIGNATURES: &[&str] = &[
    "ParserError",
    "is not recognized",
    "command not found",
    "was unexpected at this time",
    "internal or external command",
    "not recognized as a name of a cmdlet",
];
/// 单条命令的超时上限（PRD §5：金集单条超时 60s）。
const CORPUS_COMMAND_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CorpusEntry {
    id: String,
    cmd: String,
    /// `windows` | `macos` | `both`
    platform: String,
    /// `fast` | `slow`
    tier: String,
    category: String,
    /// `ok` | `fail` | `fail-with-hint`
    expect: String,
}

#[derive(Debug, serde::Serialize)]
struct CommandOutcome {
    id: String,
    category: String,
    tier: String,
    expect: String,
    /// policy 类经 gateway 被前置拒绝（未 spawn）。
    blocked: bool,
    /// 工具层错误（spawn 失败等），非命令退出。
    error: bool,
    exit_code: Option<i32>,
    met: bool,
    dialect_failure: bool,
    hint_present: bool,
    utf8_loss: bool,
    duration_ms: u128,
}

fn load_corpus() -> Result<Vec<CorpusEntry>, String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let corpus_path = manifest_dir
        .join("tests")
        .join("command_corpus")
        .join("corpus.jsonl");
    let text = std::fs::read_to_string(&corpus_path)
        .map_err(|e| format!("failed to read corpus {}: {e}", corpus_path.display()))?;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("corpus line {}: {e}", index + 1))?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("corpus line {}: missing field {name}", index + 1))
        };
        entries.push(CorpusEntry {
            id: field("id")?,
            cmd: field("cmd")?,
            platform: field("platform")?,
            tier: field("tier")?,
            category: field("category")?,
            expect: field("expect")?,
        });
    }
    Ok(entries)
}

/// 从 `render_output` 文本解析退出码（`exit: 0` / `exit: 42（非零…）`）。
fn parse_exit_code(output: &str) -> Option<i32> {
    for line in output.lines() {
        let Some(rest) = line.trim().strip_prefix("exit:") else {
            continue;
        };
        let token = rest
            .trim()
            .split(['（', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        return token.parse::<i32>().ok();
    }
    None
}

fn looks_like_dialect_failure(text: &str) -> bool {
    DIALECT_FAILURE_SIGNATURES.iter().any(|signature| {
        text.to_ascii_lowercase()
            .contains(&signature.to_ascii_lowercase())
    })
}

/// 执行单条语料，返回 (文本输出, blocked, error)。
async fn execute_entry(
    entry: &CorpusEntry,
    cwd: &Path,
    gateway: &ToolGateway,
) -> (String, bool, bool) {
    let input = serde_json::json!({
        "command": entry.cmd,
        "cwd": cwd.to_string_lossy(),
        "timeout_ms": CORPUS_COMMAND_TIMEOUT_MS,
    });
    if entry.category == "policy" {
        // policy 类走真实 gateway 权限链路：R4 前置拒绝的文本会被 M2-02 的
        // 诊断提示包裹，fail-with-hint 断言据此判定。
        return match gateway
            .execute_call_with_access_mode_and_workspace_guard(
                "corpus-task",
                "corpus-run",
                "bash",
                input,
                None,
                ProjectAccessMode::FullAccess,
                None,
            )
            .await
        {
            Ok(outcome) => (outcome.content, false, outcome.is_error),
            Err(err) => (err.to_string(), true, false),
        };
    }
    match BashTool.execute(input).await {
        Ok(text) => (text, false, false),
        Err(err) => (err.to_string(), false, true),
    }
}

fn entry_eligible(entry: &CorpusEntry, host_platform: &str, tiers: &[&str]) -> bool {
    let platform_ok = entry.platform == "both" || entry.platform == host_platform;
    let tier_ok = tiers.contains(&entry.tier.as_str());
    platform_ok && tier_ok
}

fn compute_met(
    entry: &CorpusEntry,
    blocked: bool,
    error: bool,
    exit_code: Option<i32>,
    hint: bool,
) -> bool {
    match entry.expect.as_str() {
        "ok" => !blocked && !error && exit_code == Some(0),
        "fail" => blocked || error || exit_code != Some(0),
        "fail-with-hint" => (blocked || exit_code != Some(0)) && hint,
        other => unreachable!("corpus schema 已校验 expect 枚举，遇非法值 {other}"),
    }
}

/// policy 类语料的静态前置条件：分类器定级必须达到「需要审批」档（≥R3），
/// 否则该条语料与预期语义不符（runner 直接报告错误而不是静默执行）。
fn policy_preflight(entry: &CorpusEntry) -> Result<(), String> {
    let level = classify_shell_command(&entry.cmd).level;
    let expects_blocked = entry.expect != "ok";
    if expects_blocked && (level as u8) < (RiskLevel::R3 as u8) {
        return Err(format!(
            "policy 语料 {} 预期被拦截，但分类器仅定级 {:?}",
            entry.id, level
        ));
    }
    if !expects_blocked && (level as u8) > (RiskLevel::R2 as u8) {
        return Err(format!(
            "policy 语料 {} 预期放行，但分类器定级 {:?}",
            entry.id, level
        ));
    }
    Ok(())
}

fn report_path(git_sha: &str, platform: &str, tag: &str) -> PathBuf {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let name = if tag.is_empty() {
        format!("report-{git_sha}-{platform}.json")
    } else {
        format!("report-{git_sha}-{platform}-{tag}.json")
    };
    repo_root
        .join("artifacts")
        .join("metrics")
        .join("command-corpus")
        .join(name)
}

fn host_platform() -> Option<&'static str> {
    if cfg!(windows) {
        Some("windows")
    } else if cfg!(target_os = "macos") {
        Some("darwin")
    } else {
        None
    }
}

// 金集属显式选择执行：默认 `cargo test` 报 ignored（而非"静默 pass"掩盖金集
// 0 执行）；经 scripts/windows-reliability/corpus-run.mjs（CI Windows 门禁）或
// `cargo test --test command_corpus_runner -- --ignored` + CORPUS_RUN 环境变量执行。
#[tokio::test]
#[ignore = "golden corpus 需显式执行：CORPUS_RUN=fast|slow|all + windows/darwin 平台（corpus-run.mjs 或 -- --ignored）"]
async fn command_corpus_run() {
    let Some(tier_selection) = std::env::var("CORPUS_RUN").ok() else {
        panic!("CORPUS_RUN=fast|slow|all 未设置：--ignored 显式执行金集时必须选择档位");
    };
    let tiers: Vec<&str> = match tier_selection.as_str() {
        "fast" => vec!["fast"],
        "slow" => vec!["slow"],
        "all" => vec!["fast", "slow"],
        other => panic!("CORPUS_RUN must be fast|slow|all, got {other}"),
    };
    let Some(platform) = host_platform() else {
        panic!("golden corpus 仅支持 windows/darwin 宿主平台");
    };

    let entries = load_corpus().expect("corpus.jsonl must load");
    let workspace = TempDir::new().expect("corpus workspace tempdir");
    let mut gateway = ToolGateway::new(std::sync::Arc::new(PermissionEngine::new()));
    gateway.register(Box::new(BashTool));

    let mut outcomes: Vec<CommandOutcome> = Vec::new();
    for entry in &entries {
        if !entry_eligible(entry, platform, &tiers) {
            continue;
        }
        if entry.category == "policy" {
            policy_preflight(entry).expect("policy corpus entry must match classifier semantics");
        }
        let started = Instant::now();
        let (text, blocked, error) = execute_entry(entry, workspace.path(), &gateway).await;
        let exit_code = if blocked {
            None
        } else {
            parse_exit_code(&text)
        };
        let hint_present = text.contains(HINT_MARKER);
        let met = compute_met(entry, blocked, error, exit_code, hint_present);
        let dialect_failure = !met && !error && looks_like_dialect_failure(&text);
        outcomes.push(CommandOutcome {
            id: entry.id.clone(),
            category: entry.category.clone(),
            tier: entry.tier.clone(),
            expect: entry.expect.clone(),
            blocked,
            error,
            exit_code,
            met,
            dialect_failure,
            hint_present,
            utf8_loss: text.contains('\u{FFFD}'),
            duration_ms: started.elapsed().as_millis(),
        });
    }

    let total = outcomes.len();
    let ok = outcomes.iter().filter(|o| o.met).count();
    let fail = total - ok;
    let dialect_failures = outcomes.iter().filter(|o| o.dialect_failure).count();
    let hint_hits = outcomes.iter().filter(|o| o.hint_present).count();
    let git_sha = std::env::var("CORPUS_GIT_SHA").unwrap_or_else(|_| "unknown".to_string());

    let report = serde_json::json!({
        "schema_version": "command-corpus-report.v1",
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "git_sha": git_sha,
        "platform": platform,
        "dialect": current_shell_dialect_label(),
        "tiers_run": tiers,
        "total": total,
        "ok": ok,
        "fail": fail,
        "dialect_failures": dialect_failures,
        "hint_hits": hint_hits,
        "commands": outcomes,
    });

    let tag = std::env::var("CORPUS_REPORT_TAG").unwrap_or_default();
    let path = report_path(&git_sha, platform, &tag);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create metrics dir");
    }
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&report).unwrap()),
    )
    .unwrap_or_else(|e| panic!("failed to write corpus report {}: {e}", path.display()));

    println!("corpus-report: {}", path.display());
    println!(
        "corpus-summary: platform={platform} dialect={} total={total} ok={ok} fail={fail} \
dialect_failures={dialect_failures} hint_hits={hint_hits}",
        current_shell_dialect_label()
    );

    // 执行条数为 0 说明筛选配置错误（例如在 windows 上选了只有 macos 条目的档）。
    assert!(
        total > 0,
        "corpus run executed zero commands — check CORPUS_RUN/platform filter"
    );
}
