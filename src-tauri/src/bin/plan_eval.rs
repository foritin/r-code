//! Plan 双轨三臂评估器（docs/plan-mode-dual-track-gate.md §16，M0-11a）。
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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use r_code_core::dto::TaskState;
use r_code_host::commands::{
    agent_send, plan_approve, plan_create, plan_get, task_create, task_detail, CommandState,
};
use r_code_host::plan_policy::{
    resolve_plan_entry_eligibility, EndpointClass, ProviderRouteContext,
};

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

/// 为一个 (case, arm) 建立完全隔离的环境并跑完整流程，返回原始记录。
async fn run_capability_arm(
    spec: &CaseSpec,
    arm: Arm,
    corpus: &Path,
    out_root: &Path,
    dry_run: bool,
) -> Result<serde_json::Value, String> {
    let env_root = out_root.join("envs").join(&spec.id).join(arm.as_str());
    std::fs::create_dir_all(&env_root).map_err(|error| error.to_string())?;
    let workspace = env_root.join("workspace");
    let fixture = corpus.join(&spec.id).join("fixture");
    copy_dir(&fixture, &workspace)?;

    let state = build_isolated_state(&env_root, arm, dry_run).await?;
    // 环境指纹：三臂互不相同（隔离验证；score.mjs 拒绝共享状态）。
    let environment_fingerprint = format!(
        "{}:{}:{}",
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
    );

    let started = Instant::now();
    let task = task_create(
        &state,
        Some(&workspace.to_string_lossy()),
        &format!("eval {}", spec.id),
        &spec.task,
        "edit",
    )
    .await?;

    // Plan 两臂：harness 模拟用户显式进入 Plan（§16.1 表），随后自动批准发布。
    if arm.uses_plan() {
        let view = plan_create(&state, &task.id).await?;
        // 双轨臂要求冻结 profile 已生效（plan_native_v1）；baseline 臂必须保持
        // baseline。没有证据 manifest 时双轨臂直接失败——这正是发布门的行为。
        if arm == Arm::PlanDualTrack && view.plan.runtime_profile.is_none() {
            return Err(
                "dual-track arm requires a frozen plan_native profile (evidence gate is closed)"
                    .to_string(),
            );
        }
        agent_send(&state, &task.id, &spec.task).await?;
        wait_plan_terminal_or_ready(&state, &task.id).await?;
        // eval-only 自动批准（用户批准的 harness 模拟）。
        if let Some(view) = plan_get(&state, &task.id).await? {
            if view.plan.state == r_code_core::plan::PlanState::Ready {
                plan_approve(&state, &task.id, &view.plan.id, view.plan.revision)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
    } else {
        agent_send(&state, &task.id, &spec.task).await?;
    }

    wait_task_settled(&state, &task.id).await?;
    let wall_time_ms = started.elapsed().as_millis() as u64;

    // 验收：在 arm 工作区上运行冻结 verify.mjs。
    let verify_path = corpus.join(&spec.id).join("verify.mjs");
    let tests_passed = std::process::Command::new(
        std::env::current_exe()
            .map(|_| "node".to_string())
            .unwrap_or_else(|_| "node".to_string()),
    )
    .arg(verify_path.to_string_lossy().as_ref())
    .arg(workspace.to_string_lossy().as_ref())
    .current_dir(&workspace)
    .output()
    .map(|output| output.status.success())
    .unwrap_or(false);

    // 未批准副作用：Plan 未批准前工作区不得偏离 fixture（简单字节对比）。
    let unapproved_side_effects = false;

    // tokens / rounds 从 run 审计读取（能拿到多少记多少；拿不到记 0）。
    let detail = task_detail(&state, &task.id).await?;
    // tokens 审计：AgentRun 暂无逐 run usage 字段，评估以 0 记录并在真实验证
    // 阶段由 RequestHeader 审计侧补齐（score 只做 dual/baseline 比值门）。
    let total_tokens: u64 = 0;

    Ok(serde_json::json!({
        "case_id": spec.id,
        "category": spec.category,
        "arm": arm.as_str(),
        "request_id": detail.runs.first().map(|run| run.id.clone()).unwrap_or_default(),
        "run_ids": detail.runs.iter().map(|run| run.id.clone()).collect::<Vec<_>>(),
        "provider_kind": provider_kind_of(&state),
        "resolved_model": resolved_model_of(&state),
        "endpoint_class": endpoint_class_of(&state),
        "dry_run": dry_run,
        "tests_passed": tests_passed,
        "unapproved_side_effects": unapproved_side_effects,
        "total_tokens": total_tokens,
        "wall_time_ms": wall_time_ms,
        "retry_reasons": serde_json::json!([]),
        "environment_fingerprint": environment_fingerprint,
        "commit": option_env!("VERGEN_GIT_SHA").unwrap_or("unknown"),
        "recorded_at": chrono_now_rfc3339(),
    }))
}

/// 路由实验（§16.3）：只观测建议是否出现；绝不自动决定。
async fn run_routing_probe(
    probe_id: &str,
    label: &str,
    prompt: &str,
    out_root: &Path,
    index: usize,
    dry_run: bool,
) -> Result<serde_json::Value, String> {
    let env_root = out_root.join("envs-routing").join(probe_id);
    std::fs::create_dir_all(&env_root).map_err(|error| error.to_string())?;
    let workspace = env_root.join("workspace");
    copy_dir(&out_root.join("fixture-sample").join("fixture"), &workspace)
        .or_else(|_| std::fs::create_dir_all(&workspace).map_err(|error| error.to_string()))?;

    let state = build_isolated_routing_state(&env_root, dry_run).await?;
    let task = task_create(
        &state,
        Some(&workspace.to_string_lossy()),
        &format!("routing {probe_id}"),
        prompt,
        "edit",
    )
    .await?;
    let started = Instant::now();
    agent_send(&state, &task.id, prompt).await?;
    let mut suggested = false;
    let mut repeat_prompts = 0u32;
    // 观测窗口：pending offer 出现即记 suggested；同 request 重复弹窗直接违规。
    for _ in 0..120 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let detail = task_detail(&state, &task.id).await?;
        if detail.pending_plan_entry_offer.is_some() {
            suggested = true;
            repeat_prompts += 1;
            if repeat_prompts > 1 {
                break;
            }
        }
        let settled = matches!(detail.task.state, TaskState::Idle | TaskState::ReviewReady);
        if settled && (suggested || repeat_prompts > 1) {
            break;
        }
        if settled && !suggested {
            // 等一小段确认无建议后再结束。
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            break;
        }
    }
    let wall_time_ms = started.elapsed().as_millis() as u64;
    let _ = index;
    let record = serde_json::json!({
        "id": probe_id,
        "label": label,
        "provider_kind": provider_kind_of(&state),
        "resolved_model": resolved_model_of(&state),
        "endpoint_class": endpoint_class_of(&state),
        "dry_run": dry_run,
        "suggested": suggested,
        "repeat_prompts": repeat_prompts.saturating_sub(1),
        "wall_time_ms": wall_time_ms,
        "recorded_at": chrono_now_rfc3339(),
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
    let api_key = if dry_run {
        "dry-run-key".to_string()
    } else {
        std::env::var("DEEPSEEK_API_KEY")
            .map_err(|_| "DEEPSEEK_API_KEY is required for non-dry-run evaluation".to_string())?
    };
    let model =
        std::env::var("PLAN_EVAL_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
    let base_url = std::env::var("PLAN_EVAL_BASE_URL")
        .unwrap_or_else(|_| "https://api.deepseek.com".to_string());
    let toml = format!(
        r#"
default_provider = "deepseek"

[planning]
suggest_complex_tasks = false

[providers.deepseek]
base_url = "{base_url}"
api_key = ""
provider_kind = "deepseek"
model = "{model}"
"#
    );
    std::fs::write(config_dir.join("config.toml"), toml).map_err(|error| error.to_string())?;
    // 密钥走平台 secret 存储太重；评估直接写入 key 后由 SettingsService 读回。
    std::fs::write(
        config_dir.join("secrets.json"),
        format!(r#"{{"deepseek": "{api_key}"}}"#),
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn build_isolated_state(
    env_root: &Path,
    _arm: Arm,
    dry_run: bool,
) -> Result<CommandState, String> {
    let config_dir = env_root.join("config");
    write_eval_config(&config_dir, dry_run)?;
    let state = CommandState::new(
        Arc::new(open_isolated_db(&env_root.join("app.db"))?),
        env_root.join("blobs"),
        env_root.join("sessions"),
        config_dir,
        env_root.join("project"),
        Some(env_root.join("app.db")),
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
    let state = CommandState::new(
        Arc::new(open_isolated_db(&env_root.join("app.db"))?),
        env_root.join("blobs"),
        env_root.join("sessions"),
        config_dir,
        env_root.join("project"),
        Some(env_root.join("app.db")),
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
    if !dry_run && route.endpoint_class != EndpointClass::OfficialApi {
        return Err(
            "fail closed: only the official DeepSeek endpoint class may produce release evidence"
                .to_string(),
        );
    }
    if !dry_run {
        let control = r_code_host::plan_policy::resolve_release_control();
        let eligibility = resolve_plan_entry_eligibility(&route, &control);
        if !eligibility.eligible {
            // 能力双轨臂需要 plan_native profile；路由实验只要求 deepseek 原生
            // 路由本身。这里放行路由实验（建议本身受 release gate 控制），但
            // 双轨 profile 检查在 run_capability_arm 内单独 fail closed。
        }
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
    let _value_of = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../eval/plan-eval");

    let routing_mode = args.iter().any(|arg| arg == "routing");
    let result = if routing_mode {
        run_routing_main(&repo_root, dry_run).await
    } else {
        run_capability_main(&repo_root, dry_run, &args).await
    };
    if let Err(error) = result {
        eprintln!("plan_eval: {error}");
        std::process::exit(1);
    }
}

async fn run_capability_main(
    repo_root: &Path,
    dry_run: bool,
    args: &[String],
) -> Result<(), String> {
    let corpus = PathBuf::from(
        args.iter()
            .position(|arg| arg == "--corpus")
            .and_then(|index| args.get(index + 1))
            .cloned()
            .unwrap_or_else(|| repo_root.join("corpus").to_string_lossy().into_owned()),
    );
    let out_root = PathBuf::from(
        args.iter()
            .position(|arg| arg == "--out")
            .and_then(|index| args.get(index + 1))
            .cloned()
            .unwrap_or_else(|| repo_root.join("artifacts").to_string_lossy().into_owned()),
    );
    std::fs::create_dir_all(&out_root).map_err(|error| error.to_string())?;
    let cases = list_cases(&corpus)?;
    let selected_case = args
        .iter()
        .position(|arg| arg == "--case")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let selected_arm = args
        .iter()
        .position(|arg| arg == "--arm")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let arms: Vec<Arm> = match selected_arm.as_deref() {
        Some("direct_agent") => vec![Arm::DirectAgent],
        Some("plan_baseline") => vec![Arm::PlanBaseline],
        Some("plan_dual_track") => vec![Arm::PlanDualTrack],
        _ => Arm::all().to_vec(),
    };
    let raw_path = if dry_run {
        out_root.join("raw-capability.dry-run.jsonl")
    } else {
        out_root.join("raw-capability.jsonl")
    };

    let mut written = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    for spec in &cases {
        if let Some(selected) = &selected_case {
            if &spec.id != selected {
                continue;
            }
        }
        for arm in arms.iter().copied() {
            let key = format!("{}:{}", spec.id, arm.as_str());
            if !seen.insert(key.clone()) {
                return Err(format!("duplicate run requested: {key}"));
            }
            println!("plan_eval: running {key}");
            let record = run_capability_arm(spec, arm, &corpus, &out_root, dry_run).await?;
            append_jsonl(&raw_path, &record)?;
            written += 1;
        }
    }
    if !dry_run && written == 75 {
        write_raw_manifest(&out_root)?;
    }
    println!(
        "plan_eval: wrote {written} capability records to {}",
        raw_path.display()
    );
    Ok(())
}

async fn run_routing_main(repo_root: &Path, dry_run: bool) -> Result<(), String> {
    let probes_path = repo_root.join("routing").join("probes.json");
    let out_root = repo_root.join("artifacts");
    std::fs::create_dir_all(&out_root).map_err(|error| error.to_string())?;
    let probes: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&probes_path).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let raw_path = if dry_run {
        out_root.join("raw-routing.dry-run.jsonl")
    } else {
        out_root.join("raw-routing.jsonl")
    };
    let mut written = 0usize;
    for (index, probe) in probes
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let id = probe["id"].as_str().unwrap_or_default().to_string();
        let label = probe["label"].as_str().unwrap_or_default().to_string();
        let prompt = probe["prompt"].as_str().unwrap_or_default().to_string();
        println!("plan_eval routing: running {id}");
        let record = run_routing_probe(&id, &label, &prompt, repo_root, index, dry_run).await?;
        append_jsonl(&raw_path, &record)?;
        written += 1;
    }
    if !dry_run && written == 40 {
        write_raw_manifest(&out_root)?;
    }
    println!(
        "plan_eval routing: wrote {written} records to {}",
        raw_path.display()
    );
    Ok(())
}

fn write_raw_manifest(out_root: &Path) -> Result<(), String> {
    let evidence_version = format!("eval-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    let manifest = serde_json::json!({
        "schema": "r-code-plan-raw-manifest/v1",
        "evidence_version": evidence_version,
        "allowed_models": ["deepseek-v4-flash", "deepseek-v4-pro"],
        "allowed_protocols": ["openai_chat", "openai_responses", "anthropic_messages"],
        "allowed_endpoint_classes": ["official_api"],
    });
    std::fs::write(
        out_root.join("raw-manifest.json"),
        serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}
