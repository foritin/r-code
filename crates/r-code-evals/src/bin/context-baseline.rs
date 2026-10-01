//! Appendix-A acceptance driver (project-context PRD, M1a baseline).
//!
//! Runs ONE standard task end-to-end against the shared r-code-service
//! daemon over the same RPC surface the TUI uses, then prints a single
//! JSON result object on stdout (all diagnostics go to stderr):
//!
//! ```text
//! cargo run -p r-code-evals --bin context-baseline -- \
//!   --data-root <dir> --workspace <repo> --mode <ask|edit|plan> \
//!   --objective-file <file> [--timeout-secs 900] [--auto-approve on]
//!   [--events-out <file>] [--context-injection on|off]
//! ```
//!
//! Baseline policy (kept identical across both arms so the comparison only
//! measures the context features under test):
//! - the objective is sent once as the opening user message;
//! - pending approvals are auto-granted (`approvals.decide`, audited as this
//!   connection's client id) so unattended modify-tasks can proceed;
//! - terminal = the first `run.completed|run.failed|run.cancelled` journal
//!   event for the task; token totals are summed from `model.usage` events
//!   and cross-checked against `task.detail`.
//!
//! Exit codes: 0 settled, 2 timed out, 1 harness/protocol failure.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use r_code_client::DaemonClient;
use r_code_runtime::profile::{LaunchOptions, ProfileFlavor, RuntimeProfile};

const TERMINAL_KINDS: [&str; 3] = ["run.completed", "run.failed", "run.cancelled"];

#[tokio::main]
async fn main() {
    match run().await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("context-baseline: {error}");
            std::process::exit(1);
        }
    }
}

struct Args {
    data_root: PathBuf,
    flavor: ProfileFlavor,
    ipc_name: Option<String>,
    workspace: PathBuf,
    mode: String,
    objective: String,
    title: Option<String>,
    timeout: Duration,
    auto_approve: bool,
    events_out: Option<PathBuf>,
    context_injection: Option<bool>,
}

fn parse_flag_value(args: &mut std::vec::IntoIter<String>, name: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("--{name} requires a value"))
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut data_root: Option<PathBuf> = None;
    let mut flavor = ProfileFlavor::Development;
    let mut ipc_name: Option<String> = None;
    let mut workspace: Option<PathBuf> = None;
    let mut mode: Option<String> = None;
    let mut objective: Option<String> = None;
    let mut objective_file: Option<PathBuf> = None;
    let mut title: Option<String> = None;
    let mut timeout_secs: u64 = 900;
    let mut auto_approve = true;
    let mut events_out: Option<PathBuf> = None;
    let mut context_injection: Option<bool> = None;

    let mut it = raw.into_iter();
    while let Some(arg) = it.next() {
        let value = |it: &mut _, name: &str| parse_flag_value(it, name);
        match arg.as_str() {
            "--data-root" => data_root = Some(PathBuf::from(value(&mut it, "data-root")?)),
            "--profile" => {
                flavor = ProfileFlavor::parse(&value(&mut it, "profile")?)
                    .ok_or_else(|| "unknown --profile (development|production)".to_string())?;
            }
            "--ipc-name" => ipc_name = Some(value(&mut it, "ipc-name")?),
            "--workspace" => workspace = Some(PathBuf::from(value(&mut it, "workspace")?)),
            "--mode" => mode = Some(value(&mut it, "mode")?),
            "--objective" => objective = Some(value(&mut it, "objective")?),
            "--objective-file" => {
                objective_file = Some(PathBuf::from(value(&mut it, "objective-file")?))
            }
            "--title" => title = Some(value(&mut it, "title")?),
            "--timeout-secs" => {
                timeout_secs = value(&mut it, "timeout-secs")?
                    .parse()
                    .map_err(|_| "invalid --timeout-secs".to_string())?;
            }
            "--auto-approve" => {
                let v = value(&mut it, "auto-approve")?;
                auto_approve = matches!(v.as_str(), "on" | "true" | "1");
            }
            "--events-out" => events_out = Some(PathBuf::from(value(&mut it, "events-out")?)),
            "--context-injection" => {
                let v = value(&mut it, "context-injection")?;
                context_injection = Some(matches!(v.as_str(), "on" | "true" | "1"));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }

    let workspace = workspace.ok_or("--workspace is required")?;
    let mode = mode.unwrap_or_else(|| "ask".to_string());
    if !matches!(mode.as_str(), "ask" | "edit" | "auto" | "plan") {
        return Err(format!("unsupported --mode {mode} (ask|edit|auto|plan)"));
    }
    if (objective.is_none()) == (objective_file.is_none()) {
        return Err("exactly one of --objective / --objective-file is required".to_string());
    }
    let objective = match objective {
        Some(text) => text,
        None => std::fs::read_to_string(objective_file.expect("checked above"))
            .map_err(|e| format!("reading --objective-file: {e}"))?,
    };
    let data_root = data_root.ok_or("--data-root is required")?;

    Ok(Args {
        data_root,
        flavor,
        ipc_name,
        workspace,
        mode,
        objective,
        title,
        timeout: Duration::from_secs(timeout_secs.max(30)),
        auto_approve,
        events_out,
        context_injection,
    })
}

/// r-code-service binary resolution: explicit env override, else beside the
/// current exe (dev layout puts cargo output in the same target dir).
fn service_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("R_CODE_SERVICE_BIN") {
        return Some(PathBuf::from(path));
    }
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "r-code-service.exe"
    } else {
        "r-code-service"
    };
    let beside = exe.parent()?.join(name);
    beside.is_file().then_some(beside)
}

#[derive(Default)]
struct UsageSum {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
}

impl UsageSum {
    fn add_payload(&mut self, usage: &serde_json::Value) {
        let field = |snake: &str, camel: &str| {
            usage
                .get(snake)
                .or_else(|| usage.get(camel))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        self.input_tokens += field("input_tokens", "inputTokens");
        self.output_tokens += field("output_tokens", "outputTokens");
        self.cache_read_tokens += field("cache_read_tokens", "cacheReadTokens");
        self.cache_write_tokens += field("cache_write_tokens", "cacheWriteTokens");
    }

    fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

async fn call(
    client: &mut DaemonClient,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    client.call(method, params).await.map_err(|e| e.to_string())
}

async fn run() -> Result<i32, String> {
    let args = parse_args()?;
    let started = Instant::now();

    let mut options = LaunchOptions::new(args.flavor).with_data_root(&args.data_root);
    if let Some(name) = &args.ipc_name {
        options = options.with_ipc_name(name.clone());
    }
    let profile = RuntimeProfile::resolve(&options).map_err(|e| e.to_string())?;

    r_code_client::ensure_daemon(
        &profile.harness_v1_root(),
        &profile.ipc_endpoint(),
        &profile.profile_id(),
        service_binary().as_deref(),
    )
    .await
    .map_err(|e| format!("ensuring daemon: {e}"))?;

    let mut client = DaemonClient::connect(
        &profile.ipc_endpoint(),
        &profile.profile_id(),
        &r_code_client::read_owner_token(&profile.harness_v1_root())
            .ok_or("daemon owner token missing")?
            .token,
        "context-baseline",
    )
    .await
    .map_err(|e| format!("connect: {e}"))?;

    let workspace = std::fs::canonicalize(&args.workspace)
        .map_err(|e| format!("canonicalizing workspace: {e}"))?;
    let workspace_str = workspace
        .to_str()
        .ok_or("workspace path is not valid UTF-8")?
        .to_string();

    // Baseline lever for the injection arm. The RPC is part of the M1a
    // acceptance contract (FR-1 config surface); failing here means the
    // feature under test is not implemented yet.
    if let Some(enabled) = args.context_injection {
        call(
            &mut client,
            "context.settings.update",
            serde_json::json!({
                "workspacePath": workspace_str,
                "injectionEnabled": enabled,
            }),
        )
        .await
        .map_err(|e| {
            format!("context.settings.update failed (FR-1 injection switch not implemented?): {e}")
        })?;
    }

    let task_id = format!("task-baseline-{}", uuid::Uuid::new_v4().simple());
    let title = args
        .title
        .clone()
        .unwrap_or_else(|| format!("baseline-{}", &task_id[..20.min(task_id.len())]));
    let created = call(
        &mut client,
        "task.create",
        serde_json::json!({
            "taskId": task_id,
            "objective": args.objective,
            "title": title,
            "mode": args.mode,
            "workspacePath": workspace_str,
        }),
    )
    .await?;
    let task_id = created["taskId"]
        .as_str()
        .unwrap_or(task_id.as_str())
        .to_string();
    eprintln!(
        "context-baseline: task {task_id} created (mode {})",
        args.mode
    );

    call(
        &mut client,
        "task.sendMessage",
        serde_json::json!({"taskId": task_id, "text": args.objective}),
    )
    .await
    .map_err(|e| format!("task.sendMessage: {e}"))?;

    let mut cursor: u64 = 0;
    let mut usage = UsageSum::default();
    let mut assistant_chars: usize = 0;
    let mut tool_calls: usize = 0;
    let mut approvals: Vec<serde_json::Value> = Vec::new();
    let mut decided: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut event_dump: Vec<String> = Vec::new();
    let mut terminal: Option<String> = None;

    let deadline = Instant::now() + args.timeout;
    while terminal.is_none() {
        if Instant::now() >= deadline {
            break;
        }
        let events = call(
            &mut client,
            "task.events",
            serde_json::json!({"afterSeq": cursor, "limit": 500}),
        )
        .await?;
        let events = events
            .as_array()
            .cloned()
            .ok_or("task.events returned a non-array")?;
        for event in &events {
            if let Some(seq) = event.get("seq").and_then(|v| v.as_u64()) {
                cursor = cursor.max(seq);
            }
            if event.get("task_id").and_then(|v| v.as_str()) != Some(task_id.as_str()) {
                continue;
            }
            if let Ok(line) = serde_json::to_string(event) {
                event_dump.push(line);
            }
            let payload = event
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let kind = payload
                .get("journalKind")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match kind {
                "model.usage" => {
                    if let Some(u) = payload.get("usage") {
                        usage.add_payload(u);
                    }
                }
                "assistant.message" => {
                    assistant_chars += payload
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(str::len)
                        .unwrap_or(0);
                }
                "tool.call" => tool_calls += 1,
                _ => {}
            }
            if TERMINAL_KINDS.contains(&kind) {
                terminal = Some(kind.to_string());
            }
        }

        if args.auto_approve {
            let pending = call(&mut client, "approvals.list", serde_json::json!({})).await?;
            for entry in pending.as_array().cloned().unwrap_or_default() {
                if entry.get("taskId").and_then(|v| v.as_str()) != Some(task_id.as_str()) {
                    continue;
                }
                let op = entry["operationId"]
                    .as_str()
                    .or_else(|| entry["operation_id"].as_str())
                    .map(str::to_string);
                let Some(op) = op else { continue };
                if !decided.insert(op.clone()) {
                    continue;
                }
                call(
                    &mut client,
                    "approvals.decide",
                    serde_json::json!({"operationId": op, "decision": "granted"}),
                )
                .await?;
                approvals.push(serde_json::json!({"operationId": op, "decision": "granted"}));
                eprintln!("context-baseline: auto-granted approval {op}");
            }
        }

        if terminal.is_none() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    let detail = call(
        &mut client,
        "task.detail",
        serde_json::json!({"taskId": task_id}),
    )
    .await
    .unwrap_or(serde_json::Value::Null);
    let run_count = detail["runs"].as_array().map(Vec::len).unwrap_or(0);

    if let Some(out) = &args.events_out {
        if let Some(parent) = Path::new(out).parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("creating events-out dir: {e}"))?;
        }
        std::fs::write(out, event_dump.join("\n") + "\n")
            .map_err(|e| format!("writing events-out: {e}"))?;
    }

    let result = serde_json::json!({
        "ok": terminal.is_some(),
        "taskId": task_id,
        "mode": args.mode,
        "settled": terminal.is_some(),
        "terminalJournalKind": terminal,
        "wallMs": started.elapsed().as_millis() as u64,
        "usage": {
            "inputTokens": usage.input_tokens,
            "outputTokens": usage.output_tokens,
            "cacheReadTokens": usage.cache_read_tokens,
            "cacheWriteTokens": usage.cache_write_tokens,
            "totalTokens": usage.total(),
        },
        "detailUsage": {
            "inputTokens": detail["usage"]["input_tokens"].as_u64().unwrap_or(0),
            "outputTokens": detail["usage"]["output_tokens"].as_u64().unwrap_or(0),
        },
        "runCount": run_count,
        "toolCalls": tool_calls,
        "assistantChars": assistant_chars,
        "approvalsGranted": approvals.len(),
        "approvals": approvals,
        "contextInjection": args.context_injection,
    });
    println!("{result}");
    Ok(if terminal.is_some() { 0 } else { 2 })
}
