//! Plan 双轨三臂评估器（docs/archive/implementation/plan-mode-dual-track-gate.md §16，M0-11a）。
//!
//! 用法：
//! ```text
//! plan_eval --corpus <dir> --out <dir> [--case <id>] [--arm direct_agent|plan_baseline|plan_dual_track] [--dry-run]
//! plan_eval routing --probes <file> --out <dir> [--dry-run]
//! ```
//!
//! 合同：
//! - eval-only 自动 accept/approve：能力实验里 harness 模拟用户进入并批准 Plan；
//!   路由实验只观测建议是否出现，绝不自动决定；
//! - 三臂能力实验关闭自动复杂度建议（`planning.suggest_complex_tasks = false`），
//!   防止 Direct Agent 自己调用 propose_plan_mode 污染控制组；
//! - 非 dry-run 评估 fail closed：只有 `provider_kind = deepseek` 的原生配置
//!   （允许的 model / 协议 / endpoint class）才允许运行；dry-run 记录显式标记
//!   `dry_run: true`，score.mjs 拒绝其为发布证据；
//! - 每个 (case, arm) 从冻结 fixture 建立全新 workspace / SQLite / session 目录；
//!   随机化的只是运行顺序，不是状态所有权。

#![recursion_limit = "256"]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use r_code_core::dto::{AgentRun, ProjectAccessMode, TaskMode, TaskState};
use r_code_core::plan_entry::PlanCatalogProfile;
use r_code_host::commands::{
    agent_send, plan_approve, plan_create, plan_get, request_audit_counters, task_create,
    task_detail, task_set_mode, workspace_open, workspace_set_access_mode, CommandState,
};
use r_code_host::plan_policy::{
    EndpointClass, PlanningReleaseControl, PlanningReleaseState, ProviderRouteContext,
    ELIGIBILITY_PROFILE_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const EVAL_ALLOWED_MODELS: &[&str] = &["deepseek-v4-flash", "deepseek-v4-pro"];
const EVAL_ALLOWED_PROTOCOLS: &[&str] = &["openai_chat", "openai_responses", "anthropic_messages"];
const EVAL_ALLOWED_ENDPOINT_CLASSES: &[&str] = &["official_api"];
const RAW_MANIFEST_SCHEMA: &str = "r-code-plan-raw-manifest/v1";
const CAPABILITY_ARTIFACT_SCHEMA: &str = "r-code-plan-capability-artifact/v1";
const ROUTING_ARTIFACT_SCHEMA: &str = "r-code-plan-routing-artifact/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PricingSchedule {
    currency: String,
    unit: String,
    input_usd_per_million: f64,
    cache_read_usd_per_million: f64,
    output_usd_per_million: f64,
}

impl PricingSchedule {
    fn from_env(dry_run: bool) -> Result<Option<Self>, String> {
        if dry_run {
            return Ok(None);
        }
        let input = required_non_negative_rate("PLAN_EVAL_INPUT_USD_PER_MILLION")?;
        let cache_read = required_non_negative_rate("PLAN_EVAL_CACHE_READ_USD_PER_MILLION")?;
        let output = required_non_negative_rate("PLAN_EVAL_OUTPUT_USD_PER_MILLION")?;
        if input == 0.0 || output == 0.0 {
            return Err(
                "PLAN_EVAL input/output prices must be positive frozen USD-per-million rates"
                    .to_string(),
            );
        }
        Ok(Some(Self {
            currency: "USD".to_string(),
            unit: "per_million_tokens".to_string(),
            input_usd_per_million: input,
            cache_read_usd_per_million: cache_read,
            output_usd_per_million: output,
        }))
    }

    fn cost_usd(&self, usage: &UsageTotals) -> f64 {
        let uncached_input = usage.input_tokens.saturating_sub(usage.cache_read_tokens);
        (uncached_input as f64 * self.input_usd_per_million
            + usage.cache_read_tokens as f64 * self.cache_read_usd_per_million
            + usage.output_tokens as f64 * self.output_usd_per_million)
            / 1_000_000.0
    }
}

#[derive(Debug, Clone)]
struct EvalMetadata {
    evidence_version: String,
    run_seed: String,
    commit: String,
    preregistration_sha256: String,
    corpus_lock_sha256: String,
    pricing: Option<PricingSchedule>,
    resolved_model: String,
    wire_protocol: String,
    endpoint_class: String,
    base_url_sha256: String,
    dry_run: bool,
}

impl EvalMetadata {
    fn load(repo_root: &Path, dry_run: bool) -> Result<Self, String> {
        let repository = repo_root.parent().and_then(Path::parent).ok_or_else(|| {
            format!(
                "cannot resolve repository root from {}",
                repo_root.display()
            )
        })?;
        let commit = git_stdout(repository, &["rev-parse", "HEAD"])?;
        if !is_git_commit(&commit) {
            return Err(format!(
                "git rev-parse HEAD returned an invalid commit: {commit}"
            ));
        }
        if !dry_run {
            let dirty = git_stdout(
                repository,
                &["status", "--porcelain", "--untracked-files=no"],
            )?;
            if !dirty.is_empty() {
                return Err(
                    "non-dry-run evidence requires a clean tracked worktree and clean submodules"
                        .to_string(),
                );
            }
        }
        let evidence_version =
            required_eval_identity("PLAN_EVAL_EVIDENCE_VERSION", dry_run, "dry-run-evidence")?;
        let run_seed = required_eval_identity("PLAN_EVAL_SEED", dry_run, "dry-run-seed-v1")?;
        let preregistration_sha256 = sha256_file(&repo_root.join("schema/preregistration.json"))?;
        let corpus_lock_sha256 = sha256_file(&repo_root.join("corpus-lock.json"))?;
        let resolved_model =
            std::env::var("PLAN_EVAL_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
        let wire_protocol =
            std::env::var("PLAN_EVAL_PROTOCOL").unwrap_or_else(|_| "openai_chat".to_string());
        let base_url = std::env::var("PLAN_EVAL_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com".to_string());
        let endpoint_class = EndpointClass::classify(&base_url).as_str().to_string();
        if !EVAL_ALLOWED_MODELS.contains(&resolved_model.as_str()) {
            return Err(format!(
                "PLAN_EVAL_MODEL is outside the frozen allowlist: {resolved_model}"
            ));
        }
        if !EVAL_ALLOWED_PROTOCOLS.contains(&wire_protocol.as_str()) {
            return Err(format!(
                "PLAN_EVAL_PROTOCOL is outside the frozen allowlist: {wire_protocol}"
            ));
        }
        if !dry_run && !EVAL_ALLOWED_ENDPOINT_CLASSES.contains(&endpoint_class.as_str()) {
            return Err("PLAN_EVAL_BASE_URL is not an approved official endpoint".to_string());
        }
        Ok(Self {
            evidence_version,
            run_seed,
            commit,
            preregistration_sha256,
            corpus_lock_sha256,
            pricing: PricingSchedule::from_env(dry_run)?,
            resolved_model,
            wire_protocol,
            endpoint_class,
            base_url_sha256: sha256_bytes(base_url.as_bytes()),
            dry_run,
        })
    }

    fn run_seed_sha256(&self) -> String {
        sha256_bytes(self.run_seed.as_bytes())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct UsageTotals {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    stream_retries: u64,
}

impl UsageTotals {
    fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RunUsageEvidence {
    run_id: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    stream_retries: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OriginRequestEvidence {
    request_key: String,
    operation_id: String,
    kind: String,
    parent_request_key: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct AuditHeaderEvidence {
    journal_id: String,
    request_header: serde_json::Value,
}

#[derive(Debug, Clone)]
struct AuditEvidence {
    headers: Vec<AuditHeaderEvidence>,
    sha256: String,
    mismatches: usize,
}

fn required_non_negative_rate(name: &str) -> Result<f64, String> {
    let raw = std::env::var(name)
        .map_err(|_| format!("{name} is required for non-dry-run evaluation"))?;
    let value = raw
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("{name} must be a finite non-negative number"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("{name} must be a finite non-negative number"));
    }
    Ok(value)
}

fn required_eval_identity(name: &str, dry_run: bool, dry_default: &str) -> Result<String, String> {
    if dry_run {
        return Ok(std::env::var(name).unwrap_or_else(|_| dry_default.to_string()));
    }
    let value = std::env::var(name)
        .map_err(|_| format!("{name} is required for non-dry-run evaluation"))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{name} cannot be blank"));
    }
    Ok(value.to_string())
}

fn git_stdout(repository: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .map_err(|error| format!("run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String, String> {
    fs::read(path)
        .map(|bytes| sha256_bytes(&bytes))
        .map_err(|error| format!("read {} for sha256: {error}", path.display()))
}

fn sha256_json<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_vec(value)
        .map(|bytes| sha256_bytes(&bytes))
        .map_err(|error| format!("serialize sha256 input: {error}"))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_git_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn hash_tree_entries(root: &Path, current: &Path, hasher: &mut Sha256) -> Result<(), String> {
    let mut entries = fs::read_dir(current)
        .map_err(|error| format!("read tree {}: {error}", current.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().into_owned());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            return Err(format!(
                "evaluation trees must not contain symlinks or junctions: {}",
                path.display()
            ));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if file_type.is_dir() {
            hasher.update(b"directory\0");
            hash_field(hasher, relative.as_bytes());
            hash_tree_entries(root, &path, hasher)?;
        } else if file_type.is_file() {
            hasher.update(b"file\0");
            hash_field(hasher, relative.as_bytes());
            let bytes = fs::read(&path)
                .map_err(|error| format!("read tree file {}: {error}", path.display()))?;
            hash_field(hasher, &bytes);
        } else {
            return Err(format!(
                "unsupported evaluation tree entry: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn tree_sha256(root: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"r-code-plan-tree-v1\0");
    hash_tree_entries(root, root, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn diff_sha256(fixture_sha256: &str, workspace_sha256: &str) -> String {
    sha256_bytes(format!("r-code-plan-diff-v1\0{fixture_sha256}\0{workspace_sha256}").as_bytes())
}

fn randomized_order_key(seed: &str, kind: &str, key: &str) -> String {
    sha256_bytes(format!("{seed}\0{kind}\0{key}").as_bytes())
}

fn usage_evidence(
    runs: &[AgentRun],
    dry_run: bool,
) -> Result<(UsageTotals, Vec<RunUsageEvidence>, Vec<String>), String> {
    let mut totals = UsageTotals::default();
    let mut by_run = Vec::new();
    let mut retry_reasons = Vec::new();
    for run in runs {
        let Some(raw) = run.usage_json.as_deref() else {
            if dry_run {
                continue;
            }
            return Err(format!("run {} is missing usage_json", run.id));
        };
        let value: serde_json::Value = serde_json::from_str(raw)
            .map_err(|error| format!("run {} has invalid usage_json: {error}", run.id))?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("run {} usage_json must be an object", run.id))?;
        let input_tokens = object
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("run {} usage_json missing input_tokens", run.id))?;
        let output_tokens = object
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("run {} usage_json missing output_tokens", run.id))?;
        if !dry_run && input_tokens.saturating_add(output_tokens) == 0 {
            return Err(format!("run {} reported zero provider tokens", run.id));
        }
        let cache_read_tokens = object
            .get("cache_read_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let cache_write_tokens = object
            .get("cache_write_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let stream_retries = object
            .get("stream_retries")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        totals.input_tokens = totals.input_tokens.saturating_add(input_tokens);
        totals.output_tokens = totals.output_tokens.saturating_add(output_tokens);
        totals.cache_read_tokens = totals.cache_read_tokens.saturating_add(cache_read_tokens);
        totals.cache_write_tokens = totals.cache_write_tokens.saturating_add(cache_write_tokens);
        totals.stream_retries = totals.stream_retries.saturating_add(stream_retries);
        retry_reasons.extend((0..stream_retries).map(|_| "stream_replay".to_string()));
        by_run.push(RunUsageEvidence {
            run_id: run.id.clone(),
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            stream_retries,
        });
    }
    if !dry_run && (runs.is_empty() || totals.total_tokens() == 0) {
        return Err("non-dry-run evaluation produced no auditable provider usage".to_string());
    }
    Ok((totals, by_run, retry_reasons))
}

fn origin_request_evidence(
    state: &CommandState,
    task_id: &str,
    dry_run: bool,
) -> Result<Vec<OriginRequestEvidence>, String> {
    let conn = state.db.conn().map_err(|error| error.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT request_key, operation_id, kind, parent_request_key, created_at \
             FROM origin_requests WHERE task_id = ?1 ORDER BY created_at, request_key",
        )
        .map_err(|error| error.to_string())?;
    let origins = statement
        .query_map([task_id], |row| {
            Ok(OriginRequestEvidence {
                request_key: row.get(0)?,
                operation_id: row.get(1)?,
                kind: row.get(2)?,
                parent_request_key: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if !dry_run && origins.is_empty() {
        return Err("non-dry-run task is missing origin request envelopes".to_string());
    }
    Ok(origins)
}

async fn collect_audit_evidence(
    state: &CommandState,
    task_id: &str,
    dry_run: bool,
) -> Result<AuditEvidence, String> {
    let audit_dir = state.sessions_dir.join("request-audit");
    let mut files = if audit_dir.is_dir() {
        fs::read_dir(&audit_dir)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
    } else {
        Vec::new()
    };
    files.sort_by_key(|entry| entry.file_name().to_string_lossy().into_owned());
    let mut headers = Vec::new();
    for entry in files {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let journal_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();
        let raw = fs::read_to_string(&path)
            .map_err(|error| format!("read request audit {}: {error}", path.display()))?;
        for (index, line) in raw.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
                format!(
                    "parse request audit {} line {}: {error}",
                    path.display(),
                    index + 1
                )
            })?;
            if let Some(header) = value.get("request_header") {
                headers.push(AuditHeaderEvidence {
                    journal_id: journal_id.clone(),
                    request_header: header.clone(),
                });
            }
        }
    }
    let counters = request_audit_counters(state, task_id).await?;
    let (counter_headers, mismatches) = counters.unwrap_or((0, 0));
    if !dry_run {
        if headers.is_empty() {
            return Err("request audit produced no RequestHeader records".to_string());
        }
        if counter_headers != headers.len() {
            return Err(format!(
                "request audit counter/header mismatch: counter={counter_headers}, artifacts={}",
                headers.len()
            ));
        }
        if mismatches != 0 {
            return Err(format!(
                "request audit rebuild self-check reported {mismatches} mismatches"
            ));
        }
    }
    Ok(AuditEvidence {
        sha256: sha256_json(&headers)?,
        headers,
        mismatches,
    })
}

fn safe_artifact_component(value: &str) -> Result<&str, String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("unsafe artifact path component: {value}"));
    }
    Ok(value)
}

fn write_redacted_artifact(
    out_root: &Path,
    dry_run: bool,
    kind: &str,
    components: &[&str],
    value: &serde_json::Value,
) -> Result<(String, String), String> {
    safe_artifact_component(kind)?;
    let mut relative = PathBuf::from(if dry_run { "raw-dry-run" } else { "raw" });
    relative.push(kind);
    for component in components {
        relative.push(safe_artifact_component(component)?);
    }
    relative.set_extension("json");
    let path = out_root.join(&relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    fs::write(&path, &bytes).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok((
        relative.to_string_lossy().replace('\\', "/"),
        sha256_bytes(&bytes),
    ))
}

/// 三臂评估必须能在 validated manifest 产生前启动，同时 baseline 臂不能受已经
/// 嵌入的旧 manifest 或调用进程环境变量污染。显式控制只注入隔离的 CommandState，
/// 不改变桌面进程的发布解析。
fn evaluation_release_control(dual_track_enabled: bool) -> PlanningReleaseControl {
    PlanningReleaseControl {
        provider_kind: "deepseek".to_string(),
        release_state: if dual_track_enabled {
            PlanningReleaseState::Open
        } else {
            PlanningReleaseState::Off
        },
        emergency_off: false,
        eligibility_profile_version: ELIGIBILITY_PROFILE_VERSION.to_string(),
        evidence_version: if dual_track_enabled {
            "plan-eval-bootstrap-v1".to_string()
        } else {
            String::new()
        },
        allowed_models: Vec::new(),
        allowed_protocols: Vec::new(),
        allowed_endpoint_classes: Vec::new(),
        basis: if dual_track_enabled {
            "isolated plan evaluator bootstrap; never used by desktop production state".to_string()
        } else {
            "isolated plan evaluator baseline control".to_string()
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Arm {
    DirectAgent,
    PlanBaseline,
    PlanDualTrack,
}

impl Arm {
    fn as_str(self) -> &'static str {
        match self {
            Self::DirectAgent => "direct_agent",
            Self::PlanBaseline => "plan_baseline",
            Self::PlanDualTrack => "plan_dual_track",
        }
    }

    fn uses_plan(self) -> bool {
        matches!(self, Self::PlanBaseline | Self::PlanDualTrack)
    }

    fn all() -> [Self; 3] {
        [Self::DirectAgent, Self::PlanBaseline, Self::PlanDualTrack]
    }
}

struct CaseSpec {
    id: String,
    category: String,
    task: String,
}

fn read_case(corpus: &Path, id: &str) -> Result<CaseSpec, String> {
    let meta_path = corpus.join(id).join("case.json");
    let raw = std::fs::read_to_string(&meta_path)
        .map_err(|error| format!("read {}: {error}", meta_path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("parse case.json: {error}"))?;
    Ok(CaseSpec {
        id: id.to_string(),
        category: value["category"].as_str().unwrap_or_default().to_string(),
        task: value["task"]
            .as_str()
            .ok_or_else(|| format!("case {id} missing task"))?
            .to_string(),
    })
}

fn list_cases(corpus: &Path) -> Result<Vec<CaseSpec>, String> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(corpus).map_err(|error| format!("read corpus: {error}"))? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.path().join("case.json").is_file() {
            ids.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    ids.sort();
    ids.iter().map(|id| read_case(corpus, id)).collect()
}

async fn register_eval_workspace(state: &CommandState, workspace: &Path) -> Result<String, String> {
    let opened = workspace_open(state, workspace).await?;
    workspace_set_access_mode(state, &opened.canonical_path, ProjectAccessMode::RiskBased)
        .await
        .map(|workspace| workspace.canonical_path)
}

/// 为一个 (case, arm) 建立完全隔离的环境并跑完整流程，返回原始记录。
async fn run_capability_arm(
    spec: &CaseSpec,
    arm: Arm,
    corpus: &Path,
    scratch_root: &Path,
    out_root: &Path,
    metadata: &EvalMetadata,
    order_index: usize,
) -> Result<serde_json::Value, String> {
    let dry_run = metadata.dry_run;
    let env_root = scratch_root
        .join("capability")
        .join(&spec.id)
        .join(arm.as_str());
    std::fs::create_dir_all(&env_root).map_err(|error| error.to_string())?;
    let workspace = env_root.join("workspace");
    let fixture = corpus.join(&spec.id).join("fixture");
    let fixture_sha256 = tree_sha256(&fixture)?;
    copy_dir(&fixture, &workspace)?;
    let initial_workspace_sha256 = tree_sha256(&workspace)?;
    if fixture_sha256 != initial_workspace_sha256 {
        return Err(format!(
            "fixture copy drift for case {}: fixture={} workspace={}",
            spec.id, fixture_sha256, initial_workspace_sha256
        ));
    }

    let state = build_isolated_state(&env_root, arm, dry_run).await?;
    ensure_route_matches_metadata(&state, metadata)?;
    let config_sha256 = sha256_file(&state.config_dir.join("config.toml"))?;
    let workspace_binding = register_eval_workspace(&state, &workspace).await?;
    // 环境指纹：三臂互不相同（隔离验证；score.mjs 拒绝共享状态）。
    let environment_fingerprint = format!(
        "{}:{}:{}:{}",
        env_root.to_string_lossy(),
        state
            .db_path
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        workspace
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        state.sessions_dir.to_string_lossy(),
    );

    let started = Instant::now();
    let task = task_create(
        &state,
        Some(&workspace_binding),
        &format!("eval {}", spec.id),
        &spec.task,
        "edit",
    )
    .await?;
    let mut profile_kind = "direct_agent".to_string();
    let mut profile_enabled = false;
    let mut runtime_profile = serde_json::Value::Null;
    let mut preapproval_workspace_sha256 = initial_workspace_sha256.clone();
    let mut unapproved_side_effects = false;

    // Plan 两臂：harness 模拟用户显式进入 Plan（§16.1 表），随后自动批准发布。
    if arm.uses_plan() {
        task_set_mode(&state, &task.id, TaskMode::Plan).await?;
        let view = plan_create(&state, &task.id).await?;
        let profile = view.plan.runtime_profile.as_ref().ok_or_else(|| {
            format!(
                "{} arm did not freeze an explicit runtime profile",
                arm.as_str()
            )
        })?;
        profile_enabled = profile.enabled;
        profile_kind = profile.catalog_profile.as_str().to_string();
        runtime_profile = serde_json::to_value(profile).map_err(|error| error.to_string())?;
        // 评估器显式注入互斥控制：双轨臂使用开放控制（release_state = open）；
        // baseline 臂注入关闭控制，与桌面进程的发布解析互不污染。
        match arm {
            Arm::PlanDualTrack => {
                if !profile.enabled || profile.catalog_profile != PlanCatalogProfile::PlanNativeV1 {
                    return Err("dual-track arm froze an unexpected runtime profile".to_string());
                }
            }
            Arm::PlanBaseline
                if profile.enabled || profile.catalog_profile != PlanCatalogProfile::Baseline =>
            {
                return Err("baseline arm was contaminated by a dual-track profile".to_string());
            }
            _ => {}
        }
        if !dry_run {
            agent_send(&state, &task.id, &spec.task).await?;
            wait_plan_terminal_or_ready(&state, &task.id).await?;
            preapproval_workspace_sha256 = tree_sha256(&workspace)?;
            unapproved_side_effects = preapproval_workspace_sha256 != initial_workspace_sha256;
            // eval-only 自动批准（用户批准的 harness 模拟）。
            if let Some(view) = plan_get(&state, &task.id).await? {
                if view.plan.state == r_code_core::plan::PlanState::Ready
                    && !unapproved_side_effects
                {
                    plan_approve(&state, &task.id, &view.plan.id, view.plan.revision)
                        .await
                        .map_err(|error| error.to_string())?;
                }
            }
        }
    } else if !dry_run {
        agent_send(&state, &task.id, &spec.task).await?;
    }

    if !dry_run {
        wait_task_settled(&state, &task.id).await?;
    }
    let wall_time_ms = started.elapsed().as_millis() as u64;

    // 验收：在 arm 工作区上运行冻结 verify.mjs。
    let verify_path = corpus.join(&spec.id).join("verify.mjs");
    let tests_passed = std::process::Command::new("node")
        .arg(verify_path.to_string_lossy().as_ref())
        .arg(workspace.to_string_lossy().as_ref())
        .current_dir(&workspace)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    let final_workspace_sha256 = tree_sha256(&workspace)?;
    let diff_digest = diff_sha256(&fixture_sha256, &final_workspace_sha256);
    let detail = task_detail(&state, &task.id).await?;
    let (usage, usage_by_run, retry_reasons) = usage_evidence(&detail.runs, dry_run)?;
    let origins = origin_request_evidence(&state, &task.id, dry_run)?;
    let audit = collect_audit_evidence(&state, &task.id, dry_run).await?;
    let request_ids = origins
        .iter()
        .map(|origin| origin.request_key.clone())
        .collect::<Vec<_>>();
    let operation_ids = origins
        .iter()
        .map(|origin| origin.operation_id.clone())
        .collect::<Vec<_>>();
    let run_ids = detail
        .runs
        .iter()
        .map(|run| run.id.clone())
        .collect::<Vec<_>>();
    let release_state = state.planning.release_control.release_state.as_str();
    let profile_sha256 = sha256_json(&serde_json::json!({
        "release_control": &state.planning.release_control,
        "runtime_profile": runtime_profile,
    }))?;
    let cost_usd = metadata
        .pricing
        .as_ref()
        .map(|pricing| pricing.cost_usd(&usage))
        .unwrap_or(0.0);
    if !cost_usd.is_finite() || cost_usd < 0.0 {
        return Err("computed evaluation cost is not finite and non-negative".to_string());
    }
    let recorded_at = chrono_now_rfc3339();
    let order_key_sha256 = randomized_order_key(
        &metadata.run_seed,
        "capability",
        &format!("{}:{}", spec.id, arm.as_str()),
    );
    let artifact = serde_json::json!({
        "schema": CAPABILITY_ARTIFACT_SCHEMA,
        "case_id": spec.id,
        "arm": arm.as_str(),
        "task_id": task.id,
        "origins": origins,
        "run_usage": usage_by_run,
        "request_headers": audit.headers,
        "request_audit_sha256": audit.sha256,
        "request_audit_mismatches": audit.mismatches,
        "hashes": {
            "fixture_sha256": fixture_sha256,
            "initial_workspace_sha256": initial_workspace_sha256,
            "preapproval_workspace_sha256": preapproval_workspace_sha256,
            "final_workspace_sha256": final_workspace_sha256,
            "diff_digest": diff_digest,
            "config_sha256": config_sha256,
            "profile_sha256": profile_sha256,
            "preregistration_sha256": metadata.preregistration_sha256,
            "corpus_lock_sha256": metadata.corpus_lock_sha256,
        },
        "usage": usage,
        "cost_usd": cost_usd,
        "recorded_at": recorded_at,
    });
    let (artifact_uri, artifact_sha256) = write_redacted_artifact(
        out_root,
        dry_run,
        "capability",
        &[&spec.id, arm.as_str()],
        &artifact,
    )?;

    Ok(serde_json::json!({
        "case_id": spec.id,
        "category": spec.category,
        "arm": arm.as_str(),
        "request_id": request_ids.first().cloned().unwrap_or_default(),
        "request_ids": request_ids,
        "operation_id": operation_ids.first().cloned().unwrap_or_default(),
        "operation_ids": operation_ids,
        "run_ids": run_ids,
        "provider_kind": provider_kind_of(&state),
        "resolved_model": resolved_model_of(&state),
        "wire_protocol": wire_protocol_of(&state),
        "endpoint_class": endpoint_class_of(&state),
        "release_state": release_state,
        "profile_kind": profile_kind,
        "profile_enabled": profile_enabled,
        "profile_sha256": profile_sha256,
        "config_sha256": config_sha256,
        "fixture_sha256": fixture_sha256,
        "initial_workspace_sha256": initial_workspace_sha256,
        "preapproval_workspace_sha256": preapproval_workspace_sha256,
        "final_workspace_sha256": final_workspace_sha256,
        "diff_digest": diff_digest,
        "preregistration_sha256": metadata.preregistration_sha256,
        "corpus_lock_sha256": metadata.corpus_lock_sha256,
        "run_seed_sha256": metadata.run_seed_sha256(),
        "order_index": order_index,
        "order_key_sha256": order_key_sha256,
        "dry_run": dry_run,
        "tests_passed": tests_passed,
        "unapproved_side_effects": unapproved_side_effects,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_tokens": usage.cache_read_tokens,
        "cache_write_tokens": usage.cache_write_tokens,
        "total_tokens": usage.total_tokens(),
        "rounds": audit.headers.len(),
        "wall_time_ms": wall_time_ms,
        "cost_usd": cost_usd,
        "retry_count": usage.stream_retries,
        "retry_reasons": retry_reasons,
        "request_audit_sha256": audit.sha256,
        "request_audit_mismatches": audit.mismatches,
        "artifact_uri": artifact_uri,
        "artifact_sha256": artifact_sha256,
        "environment_fingerprint": environment_fingerprint,
        "commit": metadata.commit,
        "recorded_at": recorded_at,
    }))
}

/// 路由实验（§16.3）：只观测建议是否出现；绝不自动决定。
fn observe_routing_offer(
    offer_id: String,
    seen_offer_ids: &mut HashSet<String>,
    suggested: &mut bool,
    repeat_prompts: &mut u32,
) {
    if !seen_offer_ids.insert(offer_id) {
        return;
    }
    if *suggested {
        *repeat_prompts = repeat_prompts.saturating_add(1);
    } else {
        *suggested = true;
    }
}

async fn run_routing_probe(
    probe_id: &str,
    label: &str,
    prompt: &str,
    scratch_root: &Path,
    out_root: &Path,
    metadata: &EvalMetadata,
    order_index: usize,
) -> Result<serde_json::Value, String> {
    let dry_run = metadata.dry_run;
    let env_root = scratch_root.join("routing").join(probe_id);
    std::fs::create_dir_all(&env_root).map_err(|error| error.to_string())?;
    let workspace = env_root.join("workspace");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let initial_workspace_sha256 = tree_sha256(&workspace)?;

    let state = build_isolated_routing_state(&env_root, dry_run).await?;
    ensure_route_matches_metadata(&state, metadata)?;
    let config_sha256 = sha256_file(&state.config_dir.join("config.toml"))?;
    let profile_sha256 = sha256_json(&state.planning.release_control)?;
    let workspace_binding = register_eval_workspace(&state, &workspace).await?;
    let task = task_create(
        &state,
        Some(&workspace_binding),
        &format!("routing {probe_id}"),
        prompt,
        "ask",
    )
    .await?;
    let started = Instant::now();
    if !dry_run {
        agent_send(&state, &task.id, prompt).await?;
    }
    let mut suggested = false;
    let mut repeat_prompts = 0u32;
    let mut seen_offer_ids = HashSet::new();
    let mut settled = dry_run;
    // 观测窗口：pending offer 出现即记 suggested；同 request 重复弹窗直接违规。
    for _ in 0..if dry_run { 0 } else { 120 } {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let detail = task_detail(&state, &task.id).await?;
        if let Some(offer) = detail.pending_plan_entry_offer {
            observe_routing_offer(
                offer.id,
                &mut seen_offer_ids,
                &mut suggested,
                &mut repeat_prompts,
            );
        }
        settled = matches!(detail.task.state, TaskState::Idle | TaskState::ReviewReady);
        if settled && (suggested || repeat_prompts > 0) {
            break;
        }
        if settled && !suggested {
            // 等一小段确认无建议后再结束。
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            break;
        }
    }
    if !settled {
        return Err(format!(
            "routing probe {probe_id} did not settle within 60 seconds"
        ));
    }
    let wall_time_ms = started.elapsed().as_millis() as u64;
    let final_workspace_sha256 = tree_sha256(&workspace)?;
    let routing_side_effects = final_workspace_sha256 != initial_workspace_sha256;
    let detail = task_detail(&state, &task.id).await?;
    let (usage, usage_by_run, retry_reasons) = usage_evidence(&detail.runs, dry_run)?;
    let origins = origin_request_evidence(&state, &task.id, dry_run)?;
    let audit = collect_audit_evidence(&state, &task.id, dry_run).await?;
    let request_ids = origins
        .iter()
        .map(|origin| origin.request_key.clone())
        .collect::<Vec<_>>();
    let operation_ids = origins
        .iter()
        .map(|origin| origin.operation_id.clone())
        .collect::<Vec<_>>();
    let run_ids = detail
        .runs
        .iter()
        .map(|run| run.id.clone())
        .collect::<Vec<_>>();
    let cost_usd = metadata
        .pricing
        .as_ref()
        .map(|pricing| pricing.cost_usd(&usage))
        .unwrap_or(0.0);
    if !cost_usd.is_finite() || cost_usd < 0.0 {
        return Err("computed routing cost is not finite and non-negative".to_string());
    }
    let recorded_at = chrono_now_rfc3339();
    let order_key_sha256 = randomized_order_key(&metadata.run_seed, "routing", probe_id);
    let environment_fingerprint = format!(
        "{}:{}:{}:{}",
        env_root.to_string_lossy(),
        state
            .db_path
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        workspace_binding,
        state.sessions_dir.to_string_lossy(),
    );
    let artifact = serde_json::json!({
        "schema": ROUTING_ARTIFACT_SCHEMA,
        "id": probe_id,
        "label": label,
        "task_id": task.id,
        "origins": origins,
        "run_usage": usage_by_run,
        "request_headers": audit.headers,
        "request_audit_sha256": audit.sha256,
        "request_audit_mismatches": audit.mismatches,
        "hashes": {
            "initial_workspace_sha256": initial_workspace_sha256,
            "final_workspace_sha256": final_workspace_sha256,
            "config_sha256": config_sha256,
            "profile_sha256": profile_sha256,
            "preregistration_sha256": metadata.preregistration_sha256,
            "corpus_lock_sha256": metadata.corpus_lock_sha256,
        },
        "usage": usage,
        "cost_usd": cost_usd,
        "recorded_at": recorded_at,
    });
    let (artifact_uri, artifact_sha256) =
        write_redacted_artifact(out_root, dry_run, "routing", &[probe_id], &artifact)?;
    let record = serde_json::json!({
        "id": probe_id,
        "label": label,
        "request_id": request_ids.first().cloned().unwrap_or_default(),
        "request_ids": request_ids,
        "operation_id": operation_ids.first().cloned().unwrap_or_default(),
        "operation_ids": operation_ids,
        "run_ids": run_ids,
        "provider_kind": provider_kind_of(&state),
        "resolved_model": resolved_model_of(&state),
        "wire_protocol": wire_protocol_of(&state),
        "endpoint_class": endpoint_class_of(&state),
        "release_state": state.planning.release_control.release_state.as_str(),
        "profile_kind": "routing_experiment",
        "profile_enabled": true,
        "profile_sha256": profile_sha256,
        "config_sha256": config_sha256,
        "initial_workspace_sha256": initial_workspace_sha256,
        "final_workspace_sha256": final_workspace_sha256,
        "routing_side_effects": routing_side_effects,
        "preregistration_sha256": metadata.preregistration_sha256,
        "corpus_lock_sha256": metadata.corpus_lock_sha256,
        "run_seed_sha256": metadata.run_seed_sha256(),
        "order_index": order_index,
        "order_key_sha256": order_key_sha256,
        "dry_run": dry_run,
        "suggested": suggested,
        "repeat_prompts": repeat_prompts,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_tokens": usage.cache_read_tokens,
        "cache_write_tokens": usage.cache_write_tokens,
        "total_tokens": usage.total_tokens(),
        "rounds": audit.headers.len(),
        "wall_time_ms": wall_time_ms,
        "cost_usd": cost_usd,
        "retry_count": usage.stream_retries,
        "retry_reasons": retry_reasons,
        "request_audit_sha256": audit.sha256,
        "request_audit_mismatches": audit.mismatches,
        "artifact_uri": artifact_uri,
        "artifact_sha256": artifact_sha256,
        "environment_fingerprint": environment_fingerprint,
        "commit": metadata.commit,
        "recorded_at": recorded_at,
    });
    // state（隔离环境）在此函数返回时统一释放；记录已序列化完成。
    Ok(record)
}

fn copy_dir(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|error| error.to_string())?;
    for entry in std::fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let destination = target.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), &destination).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// 写入隔离 config.toml：DeepSeek Provider（真实评估从环境变量取 key）+
/// `planning.suggest_complex_tasks = false`（能力实验关闭自动建议，§16.1）。
fn write_eval_config(config_dir: &Path, dry_run: bool) -> Result<(), String> {
    std::fs::create_dir_all(config_dir).map_err(|error| error.to_string())?;
    if !dry_run
        && std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err("DEEPSEEK_API_KEY is required for non-dry-run evaluation".to_string());
    }
    let model =
        std::env::var("PLAN_EVAL_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
    let base_url = std::env::var("PLAN_EVAL_BASE_URL")
        .unwrap_or_else(|_| "https://api.deepseek.com".to_string());
    let protocol =
        std::env::var("PLAN_EVAL_PROTOCOL").unwrap_or_else(|_| "openai_chat".to_string());
    let base_url = serde_json::to_string(&base_url).map_err(|error| error.to_string())?;
    let model = serde_json::to_string(&model).map_err(|error| error.to_string())?;
    let protocol = serde_json::to_string(&protocol).map_err(|error| error.to_string())?;
    let toml = format!(
        r#"
default_provider = "deepseek"

[planning]
suggest_complex_tasks = false
deepseek_plan_anchoring = false

[diagnostics]
request_audit = true

[providers.deepseek]
base_url = {base_url}
api_key = ""
provider_kind = "deepseek"
model = {model}
protocol = {protocol}
"#
    );
    std::fs::write(config_dir.join("config.toml"), toml).map_err(|error| error.to_string())?;
    // SettingsService 会从 DEEPSEEK_API_KEY 注入运行时凭据；评测器不把 key 写入
    // config、scratch、session 或 artifacts。dry-run 不构造任何 Provider 请求。
    Ok(())
}

async fn build_isolated_state(
    env_root: &Path,
    arm: Arm,
    dry_run: bool,
) -> Result<CommandState, String> {
    let config_dir = env_root.join("config");
    write_eval_config(&config_dir, dry_run)?;
    // 双轨臂要测的就是锚定轨迹：模拟已打开锚定滑钮的用户环境（§8.1：
    // 锚定是独立于 release gate 的用户开关，基线臂保持生产默认关闭）。
    if arm == Arm::PlanDualTrack {
        let dual_track_toml = std::fs::read_to_string(config_dir.join("config.toml"))
            .map_err(|error| error.to_string())?
            .replace(
                "deepseek_plan_anchoring = false",
                "deepseek_plan_anchoring = true",
            );
        std::fs::write(config_dir.join("config.toml"), dual_track_toml)
            .map_err(|error| error.to_string())?;
    }
    let state = CommandState::new_with_planning_release_control(
        Arc::new(open_isolated_db(&env_root.join("app.db"))?),
        env_root.join("blobs"),
        env_root.join("sessions"),
        config_dir,
        env_root.join("project"),
        Some(env_root.join("app.db")),
        evaluation_release_control(arm == Arm::PlanDualTrack),
    );
    state.agent.enable_real_mode();
    ensure_deepseek_fail_closed(&state, dry_run)?;
    Ok(state)
}

async fn build_isolated_routing_state(
    env_root: &Path,
    dry_run: bool,
) -> Result<CommandState, String> {
    let config_dir = env_root.join("config");
    // 路由实验必须开启客户开关才能观测建议（§16.1：与能力实验分离）。
    write_eval_config(&config_dir, dry_run)?;
    let routing_toml = std::fs::read_to_string(config_dir.join("config.toml"))
        .map_err(|error| error.to_string())?
        .replace(
            "suggest_complex_tasks = false",
            "suggest_complex_tasks = true",
        );
    std::fs::write(config_dir.join("config.toml"), routing_toml)
        .map_err(|error| error.to_string())?;
    let state = CommandState::new_with_planning_release_control(
        Arc::new(open_isolated_db(&env_root.join("app.db"))?),
        env_root.join("blobs"),
        env_root.join("sessions"),
        config_dir,
        env_root.join("project"),
        Some(env_root.join("app.db")),
        evaluation_release_control(true),
    );
    state.agent.enable_real_mode();
    ensure_deepseek_fail_closed(&state, dry_run)?;
    Ok(state)
}

fn open_isolated_db(path: &Path) -> Result<r_code_store::Database, String> {
    r_code_store::Database::open(path).map_err(|error| error.to_string())
}

/// 非 dry-run fail closed：只认冻结 provider_kind = deepseek 的原生配置。
fn ensure_deepseek_fail_closed(state: &CommandState, dry_run: bool) -> Result<(), String> {
    let settings = r_code_host::settings::SettingsService::new(state.config_dir.clone());
    let config = settings
        .load_global_unvalidated()
        .map_err(|error| error.to_string())?;
    let name = config.default_provider.clone();
    let provider = config
        .providers
        .get(&name)
        .ok_or_else(|| format!("provider {name} missing"))?;
    let route = ProviderRouteContext {
        provider_name: name.clone(),
        provider_kind: provider.provider_kind.clone().unwrap_or_default(),
        model: provider.model.clone(),
        wire_protocol: crate_host_protocol(&name, provider),
        endpoint_class: EndpointClass::classify(&provider.base_url),
    };
    if route.provider_kind != "deepseek" {
        return Err(format!(
            "fail closed: provider_kind {} is not deepseek; evidence must come from the native DeepSeek adapter",
            route.provider_kind
        ));
    }
    if !EVAL_ALLOWED_MODELS.contains(&route.model.as_str()) {
        return Err(format!(
            "fail closed: model {} is not in the frozen evaluation allowlist",
            route.model
        ));
    }
    if !EVAL_ALLOWED_PROTOCOLS.contains(&route.wire_protocol.as_str()) {
        return Err(format!(
            "fail closed: protocol {} is not in the frozen evaluation allowlist",
            route.wire_protocol
        ));
    }
    if !dry_run && route.endpoint_class != EndpointClass::OfficialApi {
        return Err(
            "fail closed: only the official DeepSeek endpoint class may produce release evidence"
                .to_string(),
        );
    }
    if !dry_run && provider.api_key.trim().is_empty() {
        return Err(
            "fail closed: DeepSeek API key did not load from the process environment".to_string(),
        );
    }
    Ok(())
}

/// bin 侧无法引用 crate 私有 helper：按持久化 protocol（缺失时按目录推断）。
fn crate_host_protocol(_name: &str, provider: &agent_config::ProviderConfig) -> String {
    if let Some(protocol) = provider.protocol.as_deref() {
        return protocol.to_string();
    }
    let lowered = provider.base_url.to_ascii_lowercase();
    if lowered.contains("/anthropic") {
        "anthropic_messages".to_string()
    } else {
        "openai_chat".to_string()
    }
}

fn provider_kind_of(state: &CommandState) -> String {
    let settings = r_code_host::settings::SettingsService::new(state.config_dir.clone());
    settings
        .load_global_unvalidated()
        .ok()
        .and_then(|config| {
            config
                .providers
                .get(&config.default_provider)
                .and_then(|provider| provider.provider_kind.clone())
        })
        .unwrap_or_default()
}

fn ensure_route_matches_metadata(
    state: &CommandState,
    metadata: &EvalMetadata,
) -> Result<(), String> {
    let model = resolved_model_of(state);
    let protocol = wire_protocol_of(state);
    let endpoint_class = endpoint_class_of(state);
    if model != metadata.resolved_model
        || protocol != metadata.wire_protocol
        || endpoint_class != metadata.endpoint_class
    {
        return Err(format!(
            "evaluation route changed after identity freeze: model={model}, protocol={protocol}, endpoint={endpoint_class}"
        ));
    }
    Ok(())
}

fn resolved_model_of(state: &CommandState) -> String {
    let settings = r_code_host::settings::SettingsService::new(state.config_dir.clone());
    settings
        .load_global_unvalidated()
        .ok()
        .and_then(|config| {
            config
                .providers
                .get(&config.default_provider)
                .map(|provider| provider.model.clone())
        })
        .unwrap_or_default()
}

fn wire_protocol_of(state: &CommandState) -> String {
    let settings = r_code_host::settings::SettingsService::new(state.config_dir.clone());
    settings
        .load_global_unvalidated()
        .ok()
        .and_then(|config| {
            let name = config.default_provider;
            config
                .providers
                .get(&name)
                .map(|provider| crate_host_protocol(&name, provider))
        })
        .unwrap_or_default()
}

fn endpoint_class_of(state: &CommandState) -> String {
    let settings = r_code_host::settings::SettingsService::new(state.config_dir.clone());
    settings
        .load_global_unvalidated()
        .ok()
        .and_then(|config| {
            config
                .providers
                .get(&config.default_provider)
                .map(|provider| EndpointClass::classify(&provider.base_url))
        })
        .map(|class| class.as_str().to_string())
        .unwrap_or_default()
}

fn chrono_now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

async fn wait_task_settled(state: &CommandState, task_id: &str) -> Result<(), String> {
    for _ in 0..720 {
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        let task = task_detail(state, task_id).await?;
        if matches!(task.task.state, TaskState::Idle | TaskState::ReviewReady) {
            return Ok(());
        }
    }
    Err("task did not settle within the evaluation budget".to_string())
}

async fn wait_plan_terminal_or_ready(state: &CommandState, task_id: &str) -> Result<(), String> {
    for _ in 0..720 {
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        let detail = task_detail(state, task_id).await?;
        if matches!(detail.task.state, TaskState::Idle | TaskState::ReviewReady) {
            return Ok(());
        }
    }
    Err("plan run did not settle within the evaluation budget".to_string())
}

fn append_jsonl(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    writeln!(file, "{value}").map_err(|error| error.to_string())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../eval/plan-eval");
    let result = match EvalMetadata::load(&repo_root, dry_run) {
        Ok(metadata) if args.iter().any(|arg| arg == "routing") => {
            run_routing_main(&repo_root, &args, &metadata).await
        }
        Ok(metadata) => run_capability_main(&repo_root, &args, &metadata).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        eprintln!("plan_eval: {error}");
        std::process::exit(1);
    }
}

async fn run_capability_main(
    repo_root: &Path,
    args: &[String],
    metadata: &EvalMetadata,
) -> Result<(), String> {
    let dry_run = metadata.dry_run;
    let corpus = PathBuf::from(
        arg_value(args, "--corpus")
            .unwrap_or_else(|| repo_root.join("corpus").to_string_lossy().into_owned()),
    );
    let out_root = PathBuf::from(
        arg_value(args, "--out")
            .unwrap_or_else(|| repo_root.join("artifacts").to_string_lossy().into_owned()),
    );
    std::fs::create_dir_all(&out_root).map_err(|error| error.to_string())?;
    let cases = list_cases(&corpus)?;
    validate_capability_corpus(&cases)?;
    let selected_case = arg_value(args, "--case");
    let selected_arm = arg_value(args, "--arm");
    if !dry_run && (selected_case.is_some() || selected_arm.is_some()) {
        return Err(
            "non-dry-run evidence must execute the complete 25x3 preregistered corpus; --case/--arm are dry-run diagnostics only"
                .to_string(),
        );
    }
    if let Some(selected) = selected_case.as_deref() {
        if !cases.iter().any(|spec| spec.id == selected) {
            return Err(format!("unknown capability case: {selected}"));
        }
    }
    let arms: Vec<Arm> = match selected_arm.as_deref() {
        Some("direct_agent") => vec![Arm::DirectAgent],
        Some("plan_baseline") => vec![Arm::PlanBaseline],
        Some("plan_dual_track") => vec![Arm::PlanDualTrack],
        Some(other) => return Err(format!("unknown capability arm: {other}")),
        None => Arm::all().to_vec(),
    };
    if !dry_run {
        ensure_existing_raw_manifest_compatible(&out_root, metadata)?;
    }
    let raw_path = if dry_run {
        out_root.join("raw-capability.dry-run.jsonl")
    } else {
        out_root.join("raw-capability.jsonl")
    };
    // 每次 invocation 都是一个独立样本集。append 会把上次运行残留变成重复记录，
    // 因此在写第一条前截断；运行中仍逐条 append，崩溃时可保留诊断进度。
    std::fs::File::create(&raw_path).map_err(|error| error.to_string())?;
    // 所有可变运行状态都在操作系统临时目录；API key 仅从进程环境读取。
    let scratch = tempfile::Builder::new()
        .prefix("r-code-plan-eval-capability-")
        .tempdir()
        .map_err(|error| error.to_string())?;

    let mut jobs = Vec::new();
    for spec in &cases {
        if selected_case
            .as_ref()
            .is_some_and(|selected| selected != &spec.id)
        {
            continue;
        }
        for arm in arms.iter().copied() {
            let key = format!("{}:{}", spec.id, arm.as_str());
            let order_key = randomized_order_key(&metadata.run_seed, "capability", &key);
            jobs.push((order_key, spec, arm, key));
        }
    }
    jobs.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.3.cmp(&right.3)));
    let mut written = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    for (order_index, (_, spec, arm, key)) in jobs.into_iter().enumerate() {
        if !seen.insert(key.clone()) {
            return Err(format!("duplicate run requested: {key}"));
        }
        println!("plan_eval: running {key}");
        let record = run_capability_arm(
            spec,
            arm,
            &corpus,
            scratch.path(),
            &out_root,
            metadata,
            order_index,
        )
        .await?;
        append_jsonl(&raw_path, &record)?;
        written += 1;
    }
    if !dry_run {
        if written != 75 {
            return Err(format!(
                "complete capability run must write 75 records, got {written}"
            ));
        }
        update_raw_manifest(&out_root, metadata)?;
    }
    println!(
        "plan_eval: wrote {written} capability records to {}",
        raw_path.display()
    );
    Ok(())
}

async fn run_routing_main(
    repo_root: &Path,
    args: &[String],
    metadata: &EvalMetadata,
) -> Result<(), String> {
    let dry_run = metadata.dry_run;
    let probes_path = PathBuf::from(arg_value(args, "--probes").unwrap_or_else(|| {
        repo_root
            .join("routing/probes.json")
            .to_string_lossy()
            .into_owned()
    }));
    let out_root = PathBuf::from(
        arg_value(args, "--out")
            .unwrap_or_else(|| repo_root.join("artifacts").to_string_lossy().into_owned()),
    );
    std::fs::create_dir_all(&out_root).map_err(|error| error.to_string())?;
    let probes: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&probes_path).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let probes = probes
        .as_array()
        .ok_or_else(|| "routing probes must be a JSON array".to_string())?;
    validate_routing_probes(probes)?;
    if !dry_run {
        ensure_existing_raw_manifest_compatible(&out_root, metadata)?;
    }
    let raw_path = if dry_run {
        out_root.join("raw-routing.dry-run.jsonl")
    } else {
        out_root.join("raw-routing.jsonl")
    };
    std::fs::File::create(&raw_path).map_err(|error| error.to_string())?;
    let scratch = tempfile::Builder::new()
        .prefix("r-code-plan-eval-routing-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    let mut jobs = probes
        .iter()
        .map(|probe| {
            let id = probe["id"].as_str().unwrap_or_default().to_string();
            let label = probe["label"].as_str().unwrap_or_default().to_string();
            let prompt = probe["prompt"].as_str().unwrap_or_default().to_string();
            let order_key = randomized_order_key(&metadata.run_seed, "routing", &id);
            (order_key, id, label, prompt)
        })
        .collect::<Vec<_>>();
    jobs.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let mut written = 0usize;
    for (order_index, (_, id, label, prompt)) in jobs.into_iter().enumerate() {
        println!("plan_eval routing: running {id}");
        let record = run_routing_probe(
            &id,
            &label,
            &prompt,
            scratch.path(),
            &out_root,
            metadata,
            order_index,
        )
        .await?;
        append_jsonl(&raw_path, &record)?;
        written += 1;
    }
    if !dry_run {
        if written != 40 {
            return Err(format!(
                "complete routing run must write 40 records, got {written}"
            ));
        }
        update_raw_manifest(&out_root, metadata)?;
    }
    println!(
        "plan_eval routing: wrote {written} records to {}",
        raw_path.display()
    );
    Ok(())
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn validate_capability_corpus(cases: &[CaseSpec]) -> Result<(), String> {
    if cases.len() != 25 {
        return Err(format!(
            "capability corpus must contain 25 cases, got {}",
            cases.len()
        ));
    }
    let mut categories = HashMap::new();
    let mut ids = HashSet::new();
    for spec in cases {
        if !ids.insert(spec.id.as_str()) {
            return Err(format!("duplicate capability case id: {}", spec.id));
        }
        *categories.entry(spec.category.as_str()).or_insert(0usize) += 1;
    }
    for category in ["bugfix", "feature", "migration", "performance", "safety"] {
        let count = categories.get(category).copied().unwrap_or(0);
        if count != 5 {
            return Err(format!(
                "capability category {category} must contain 5 cases, got {count}"
            ));
        }
    }
    Ok(())
}

fn validate_routing_probes(probes: &[serde_json::Value]) -> Result<(), String> {
    if probes.len() != 40 {
        return Err(format!(
            "routing corpus must contain 40 probes, got {}",
            probes.len()
        ));
    }
    let mut ids = HashSet::new();
    let mut labels = HashMap::new();
    for probe in probes {
        let id = probe["id"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "routing probe is missing a non-blank id".to_string())?;
        if !ids.insert(id) {
            return Err(format!("duplicate routing probe id: {id}"));
        }
        let label = probe["label"]
            .as_str()
            .filter(|value| matches!(*value, "simple" | "complex"))
            .ok_or_else(|| format!("routing probe {id} has invalid label"))?;
        if probe["prompt"]
            .as_str()
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(format!("routing probe {id} is missing a non-blank prompt"));
        }
        *labels.entry(label).or_insert(0usize) += 1;
    }
    if labels.get("simple").copied().unwrap_or(0) != 20
        || labels.get("complex").copied().unwrap_or(0) != 20
    {
        return Err(
            "routing probes must contain exactly 20 simple and 20 complex records".to_string(),
        );
    }
    Ok(())
}

fn raw_manifest_identity(metadata: &EvalMetadata) -> serde_json::Value {
    serde_json::json!({
        "evidence_version": metadata.evidence_version,
        "run_seed": metadata.run_seed,
        "commit": metadata.commit,
        "preregistration_sha256": metadata.preregistration_sha256,
        "corpus_lock_sha256": metadata.corpus_lock_sha256,
        "pricing": metadata.pricing,
        "resolved_model": metadata.resolved_model,
        "wire_protocol": metadata.wire_protocol,
        "endpoint_class": metadata.endpoint_class,
        "base_url_sha256": metadata.base_url_sha256,
        "allowed_models": EVAL_ALLOWED_MODELS,
        "allowed_protocols": EVAL_ALLOWED_PROTOCOLS,
        "allowed_endpoint_classes": EVAL_ALLOWED_ENDPOINT_CLASSES,
    })
}

fn ensure_existing_raw_manifest_compatible(
    out_root: &Path,
    metadata: &EvalMetadata,
) -> Result<(), String> {
    let path = out_root.join("raw-manifest.json");
    if !path.is_file() {
        return Ok(());
    }
    let existing: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("parse existing raw-manifest.json: {error}"))?;
    let expected_identity_sha256 = sha256_json(&raw_manifest_identity(metadata))?;
    if existing["schema"] != RAW_MANIFEST_SCHEMA
        || existing["identity_sha256"] != expected_identity_sha256
    {
        return Err(
            "existing raw-manifest.json belongs to a different evidence version, seed, commit, corpus, preregistration, or price schedule; use a fresh --out directory"
                .to_string(),
        );
    }
    Ok(())
}

fn raw_file_descriptor(out_root: &Path, name: &str) -> Result<serde_json::Value, String> {
    let path = out_root.join(name);
    if !path.is_file() {
        return Ok(serde_json::json!({
            "path": name,
            "records": 0,
            "sha256": sha256_bytes(&[]),
        }));
    }
    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| format!("{name} is not UTF-8 JSONL: {error}"))?;
    let mut records = 0usize;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        serde_json::from_str::<serde_json::Value>(line)
            .map_err(|error| format!("parse {name} line {}: {error}", index + 1))?;
        records += 1;
    }
    Ok(serde_json::json!({
        "path": name,
        "records": records,
        "sha256": sha256_bytes(&bytes),
    }))
}

fn reject_secret_artifacts(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let entry_path = entry.path();
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            reject_secret_artifacts(&entry_path)?;
        } else if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case("secrets.json")
        {
            return Err(format!(
                "secret material must never be published under artifacts: {}",
                entry_path.display()
            ));
        }
    }
    Ok(())
}

fn update_raw_manifest(out_root: &Path, metadata: &EvalMetadata) -> Result<(), String> {
    reject_secret_artifacts(out_root)?;
    let capability = raw_file_descriptor(out_root, "raw-capability.jsonl")?;
    let routing = raw_file_descriptor(out_root, "raw-routing.jsonl")?;
    let status = if capability["records"] == 75 && routing["records"] == 40 {
        "complete"
    } else {
        "partial"
    };
    let identity = raw_manifest_identity(metadata);
    let identity_sha256 = sha256_json(&identity)?;
    if !is_sha256(&identity_sha256) {
        return Err("raw manifest identity digest is invalid".to_string());
    }
    let manifest = serde_json::json!({
        "schema": RAW_MANIFEST_SCHEMA,
        "status": status,
        "identity_sha256": identity_sha256,
        "evidence_version": metadata.evidence_version,
        "run_seed": metadata.run_seed,
        "run_seed_sha256": metadata.run_seed_sha256(),
        "commit": metadata.commit,
        "preregistration_sha256": metadata.preregistration_sha256,
        "corpus_lock_sha256": metadata.corpus_lock_sha256,
        "pricing": metadata.pricing,
        "resolved_model": metadata.resolved_model,
        "wire_protocol": metadata.wire_protocol,
        "endpoint_class": metadata.endpoint_class,
        "base_url_sha256": metadata.base_url_sha256,
        "allowed_models": EVAL_ALLOWED_MODELS,
        "allowed_protocols": EVAL_ALLOWED_PROTOCOLS,
        "allowed_endpoint_classes": EVAL_ALLOWED_ENDPOINT_CLASSES,
        "capability": capability,
        "routing": routing,
        "artifacts_root": "raw",
        "updated_at": chrono_now_rfc3339(),
    });
    let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    fs::write(out_root.join("raw-manifest.json"), bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_evidence_sums_runs_and_fails_closed_on_missing_or_zero_usage() {
        let mut first = AgentRun::new("task", "model");
        first.usage_json = Some(
            serde_json::json!({
                "input_tokens": 100,
                "output_tokens": 20,
                "cache_read_tokens": 40,
                "cache_write_tokens": 60,
                "stream_retries": 1,
            })
            .to_string(),
        );
        let mut second = AgentRun::new("task", "model");
        second.usage_json = Some(
            serde_json::json!({
                "input_tokens": 50,
                "output_tokens": 10,
                "cache_read_tokens": 5,
                "cache_write_tokens": 45,
            })
            .to_string(),
        );
        let (usage, by_run, retries) = usage_evidence(&[first, second], false).unwrap();
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.cache_read_tokens, 45);
        assert_eq!(usage.cache_write_tokens, 105);
        assert_eq!(usage.stream_retries, 1);
        assert_eq!(usage.total_tokens(), 180);
        assert_eq!(by_run.len(), 2);
        assert_eq!(retries, vec!["stream_replay"]);

        assert!(usage_evidence(&[AgentRun::new("task", "model")], false).is_err());
        let mut zero = AgentRun::new("task", "model");
        zero.usage_json = Some(r#"{"input_tokens":0,"output_tokens":0}"#.to_string());
        assert!(usage_evidence(&[zero], false).is_err());
    }

    #[test]
    fn tree_digest_detects_files_and_empty_directory_side_effects() {
        let root = tempfile::TempDir::new().unwrap();
        let initial = tree_sha256(root.path()).unwrap();
        std::fs::create_dir(root.path().join("empty")).unwrap();
        let with_directory = tree_sha256(root.path()).unwrap();
        assert_ne!(initial, with_directory);
        std::fs::write(root.path().join("empty").join("change.txt"), b"changed").unwrap();
        let with_file = tree_sha256(root.path()).unwrap();
        assert_ne!(with_directory, with_file);
    }

    #[test]
    fn polling_the_same_pending_offer_is_not_counted_as_a_repeat() {
        let mut seen = HashSet::new();
        let mut suggested = false;
        let mut repeats = 0;
        observe_routing_offer(
            "offer-1".to_string(),
            &mut seen,
            &mut suggested,
            &mut repeats,
        );
        observe_routing_offer(
            "offer-1".to_string(),
            &mut seen,
            &mut suggested,
            &mut repeats,
        );
        assert!(suggested);
        assert_eq!(repeats, 0);
        observe_routing_offer(
            "offer-2".to_string(),
            &mut seen,
            &mut suggested,
            &mut repeats,
        );
        assert_eq!(repeats, 1);
    }

    #[tokio::test]
    async fn dry_run_collects_profile_without_starting_a_provider_run_or_leaking_secrets() {
        let root = tempfile::TempDir::new().unwrap();
        let corpus = root.path().join("corpus");
        let fixture = corpus.join("case-01").join("fixture");
        std::fs::create_dir_all(&fixture).unwrap();
        std::fs::write(fixture.join("input.txt"), b"fixture").unwrap();
        std::fs::write(
            corpus.join("case-01").join("verify.mjs"),
            b"process.exit(1);\n",
        )
        .unwrap();
        let scratch = root.path().join("scratch");
        let out = root.path().join("artifacts");
        let metadata = EvalMetadata {
            evidence_version: "dry-test".to_string(),
            run_seed: "dry-test-seed".to_string(),
            commit: "a".repeat(40),
            preregistration_sha256: sha256_bytes(b"preregister"),
            corpus_lock_sha256: sha256_bytes(b"corpus"),
            pricing: None,
            resolved_model: "deepseek-v4-flash".to_string(),
            wire_protocol: "openai_chat".to_string(),
            endpoint_class: "official_api".to_string(),
            base_url_sha256: sha256_bytes(b"https://api.deepseek.com"),
            dry_run: true,
        };
        let spec = CaseSpec {
            id: "case-01".to_string(),
            category: "bugfix".to_string(),
            task: "inspect only".to_string(),
        };
        let record = run_capability_arm(
            &spec,
            Arm::PlanDualTrack,
            &corpus,
            &scratch,
            &out,
            &metadata,
            0,
        )
        .await
        .unwrap();
        assert_eq!(record["dry_run"], true);
        assert_eq!(record["profile_kind"], "plan_native_v1");
        assert_eq!(record["total_tokens"], 0);
        assert_eq!(record["rounds"], 0);
        assert!(record["run_ids"].as_array().unwrap().is_empty());
        assert!(record["request_ids"].as_array().unwrap().is_empty());
        reject_secret_artifacts(&scratch).unwrap();
        reject_secret_artifacts(&out).unwrap();
        assert!(!out.join("raw-manifest.json").exists());
    }

    #[tokio::test]
    async fn evaluator_bootstraps_dual_track_without_contaminating_baseline() {
        let scratch = tempfile::TempDir::new().unwrap();
        for (arm, expected_release, expected_profile) in [
            (
                Arm::PlanBaseline,
                PlanningReleaseState::Off,
                PlanCatalogProfile::Baseline,
            ),
            (
                Arm::PlanDualTrack,
                PlanningReleaseState::Open,
                PlanCatalogProfile::PlanNativeV1,
            ),
        ] {
            let env_root = scratch.path().join(arm.as_str());
            let workspace = env_root.join("workspace");
            std::fs::create_dir_all(&workspace).unwrap();
            let state = build_isolated_state(&env_root, arm, true).await.unwrap();
            let workspace_binding = register_eval_workspace(&state, &workspace).await.unwrap();
            assert_eq!(
                state.planning.release_control.release_state,
                expected_release
            );

            let task = task_create(
                &state,
                Some(&workspace_binding),
                "eval bootstrap",
                "inspect the frozen profile",
                "edit",
            )
            .await
            .unwrap();
            task_set_mode(&state, &task.id, TaskMode::Plan)
                .await
                .unwrap();
            let plan = plan_create(&state, &task.id).await.unwrap().plan;
            let profile = plan.runtime_profile.unwrap();
            assert_eq!(profile.catalog_profile, expected_profile);
            assert_eq!(
                profile.enabled,
                expected_profile == PlanCatalogProfile::PlanNativeV1
            );
        }

        let routing_root = scratch.path().join("routing");
        let routing = build_isolated_routing_state(&routing_root, true)
            .await
            .unwrap();
        assert_eq!(
            routing.planning.release_control.release_state,
            PlanningReleaseState::Open
        );
    }
}
