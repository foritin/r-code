//! T42 阶段 1 —— GUI 聊天链路 v1 投影层端到端验收。
//!
//! 起真 daemon（staging native 包，复用 r-code-tui 的 daemon_common 模式），
//! 经 `harness_v1_chat::ChatV1Bridge` 走完 task_create → agent_send →
//! session_messages / task_detail 全链路。无 provider 时 run 诚实失败同样
//! 算投影成功：断言旧形状（字段存在与类型正确），不断言模型回复内容。

mod daemon_common;

use r_code_core::dto::{TaskEventType, TaskState};
use r_code_host::harness_v1_chat::ChatV1Bridge;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const STRICT_PLAN: &str = r#"{"work_units":[{"id":"inspect","description":"Inspect the current checkout","dependencies":[],"acceptance":["read-only"]},{"id":"implement","description":"Implement the approved PRD","dependencies":["inspect"],"acceptance":["check:test"]}]}"#;

#[test]
fn v1_projection_source_contains_no_v2_state_compatibility_alias() {
    let source = include_str!("../src/harness_v1_chat.rs");
    assert!(
        !source.contains("v2_state_to_task_state"),
        "the retained v2 state alias is not a legacy migration boundary and must be removed"
    );
}

#[test]
fn tauri_create_command_forwards_the_complete_initial_selection() {
    let source = include_str!("../src/tauri_commands.rs");
    let start = source
        .find("pub async fn cmd_task_create(")
        .expect("task create command");
    let end = source[start..]
        .find("pub async fn cmd_project_conversation_create(")
        .map(|offset| start + offset)
        .expect("next command boundary");
    let command = &source[start..end];
    assert!(command.contains(".task_create_with_route("));
    for forwarded in [
        "provider_name.as_deref()",
        "agent_engine.as_deref()",
        "model.as_deref()",
        "inference.as_ref()",
    ] {
        assert!(
            command.contains(forwarded),
            "missing create field: {forwarded}"
        );
    }
}

fn command_body<'a>(source: &'a str, command: &str, next_command: &str) -> &'a str {
    let start = source
        .find(&format!("pub async fn {command}("))
        .unwrap_or_else(|| panic!("missing {command}"));
    let end = source[start..]
        .find(&format!("pub async fn {next_command}("))
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("missing {next_command}"));
    &source[start..end]
}

#[test]
fn desktop_plan_commands_use_only_the_daemon_bridge_and_exact_hash_contract() {
    let source = include_str!("../src/tauri_commands.rs");
    let get = command_body(source, "cmd_plan_get", "cmd_plan_create");
    let create = command_body(source, "cmd_plan_create", "cmd_plan_answer");
    let approve = command_body(source, "cmd_plan_approve", "cmd_plan_retry_implementation");

    for (name, body) in [("get", get), ("create", create), ("approve", approve)] {
        assert!(body.contains("chat_v1"), "{name} bypasses ChatV1Bridge");
        assert!(
            !body.contains("r_code_host::commands::plan_"),
            "{name} still invokes the legacy PlanStore command"
        );
        for forbidden in ["plan_store", "stage_plan", "implementation_queue"] {
            assert!(!body.contains(forbidden), "{name} uses legacy {forbidden}");
        }
    }

    assert!(approve.contains("let revision_hash = revision_hash"));
    assert!(approve.contains(".or(plan_id)"));
    assert!(approve.contains("revision_hash.trim().is_empty()"));
    assert!(approve.contains(".plan_approve(&task_id, &revision_hash, expected_revision)"));
}

#[test]
fn frontend_plan_entry_sends_once_and_approves_the_exact_revision_hash() {
    let ipc = include_str!("../frontend/src/lib/ipc.ts");
    let home = include_str!("../frontend/src/components/scenes/HomeScene.tsx");
    let approve_start = ipc.find("export const planApprove").expect("planApprove");
    let approve = &ipc[approve_start..approve_start + 260];
    assert!(approve.contains("revisionHash: planId"));
    assert!(!approve.contains("{ taskId, planId, expectedRevision }"));
    assert!(ipc.contains("{ ...args, planId: args.revisionHash }"));

    let launch_start = home
        .find("const launchConversation = async")
        .expect("Home launch");
    let launch_end = home[launch_start..]
        .find("const setGoalComposerMode")
        .map(|offset| launch_start + offset)
        .expect("Home launch boundary");
    let launch = &home[launch_start..launch_end];
    assert_eq!(launch.matches("await agentSend(").count(), 1);
    assert!(!launch.contains("planCreate("));
    assert!(!launch.contains("创建计划"));
    assert!(launch.contains("stage = \"发送消息\""));
}

async fn read_http_request(socket: &mut tokio::net::TcpStream) {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4_096];
    let header_end = loop {
        let read = socket
            .read(&mut chunk)
            .await
            .expect("read provider request");
        assert!(read > 0, "provider request ended before headers");
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().expect("content length"))
            })
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let read = socket.read(&mut chunk).await.expect("read provider body");
        assert!(read > 0, "provider request ended before body");
        bytes.extend_from_slice(&chunk[..read]);
    }
    assert!(headers
        .lines()
        .next()
        .is_some_and(|line| { line.starts_with("POST ") && line.contains("/chat/completions") }));
}

async fn spawn_plan_provider(response_count: usize) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind plan provider");
    let origin = format!(
        "http://{}",
        listener.local_addr().expect("provider address")
    );
    let server = tokio::spawn(async move {
        for _ in 0..response_count {
            let (mut socket, _) = listener.accept().await.expect("accept provider request");
            read_http_request(&mut socket).await;
            let frame = serde_json::json!({
                "choices": [{"delta": {"content": STRICT_PLAN}}]
            });
            let body = format!("data: {frame}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write provider response");
        }
    });
    (origin, server)
}

async fn wait_for_plan(
    bridge: &ChatV1Bridge,
    task_id: &str,
    previous: Option<&str>,
) -> r_code_core::plan::PlanView {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match bridge.plan_get(task_id).await {
            Ok(Some(plan)) if previous.is_none_or(|revision| plan.plan.id != revision) => {
                return plan;
            }
            Ok(_) => {}
            Err(error)
                if error
                    .to_string()
                    .contains("unsupported daemon plan state running") => {}
            Err(error) => panic!("read daemon plan: {error}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for plan"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn task_event_kinds(client: &mut r_code_client::DaemonClient, task_id: &str) -> Vec<String> {
    client
        .call(
            "task.events",
            serde_json::json!({"afterSeq": 0, "limit": 1_000}),
        )
        .await
        .expect("read task events")
        .as_array()
        .expect("event array")
        .iter()
        .filter(|event| event["task_id"] == task_id)
        .filter_map(|event| event["payload"]["journalKind"].as_str().map(str::to_string))
        .collect()
}

fn stage_codex_harness(root: &Path) -> PathBuf {
    let package = root.join("codex-package");
    let bin = package.join("bin");
    std::fs::create_dir_all(&bin).expect("codex package directories");
    let executable = if cfg!(windows) {
        "codex-fixture.exe"
    } else {
        "codex-fixture"
    };
    std::fs::copy(
        std::env::current_exe().expect("current test executable"),
        bin.join(executable),
    )
    .expect("stage codex executable");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    std::fs::write(
        package.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": "codex.r-code",
            "version": "1.0.0",
            "apiMajor": 1,
            "apiMinor": 0,
            "displayName": "Codex",
            "supportedPlatforms": [{
                "platform": platform,
                "executable": "bin/codex-fixture"
            }],
            "requestedHostServices": [
                "host.tools.list",
                "host.tools.call",
                "host.checkpoint.save",
                "host.completion.propose"
            ],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("write codex manifest");
    package
}

async fn raw_client(profile: &RuntimeProfile) -> r_code_client::DaemonClient {
    let owner = (0..100)
        .find_map(|_| {
            let owner = r_code_client::read_owner_token(&profile.harness_v1_root());
            if owner.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            owner
        })
        .expect("daemon owner");
    r_code_client::DaemonClient::connect(
        &profile.ipc_endpoint(),
        &profile.profile_id(),
        &owner.token,
        "t04-route-inspector",
    )
    .await
    .expect("connect raw client")
}

#[test]
fn chat_bridge_maps_r_code_and_codex_to_their_owned_routes() {
    let (env, _env_vars) = daemon_common::daemon_env("t04-routes");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(&env.data_dir)
            .with_ipc_name(env.ipc_name.clone()),
    )
    .expect("profile");
    let bridge = ChatV1Bridge::new_from_profile(
        profile.clone(),
        Some(daemon_common::target_debug("r-code-service")),
    );
    let provider_env = format!("R_CODE_T04_BRIDGE_KEY_{}", std::process::id());
    std::env::set_var(&provider_env, "bridge-secret");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        bridge
            .task_create("bootstrap", "start daemon", "ask", None, "prompt", None)
            .await
            .expect("bootstrap task");
        let mut client = raw_client(&profile).await;
        client
            .call(
                "settings.apply",
                serde_json::json!({
                    "expectedRevision": 0,
                    "selection": "deepseek",
                    "model": "deepseek-default",
                    "envVar": provider_env,
                }),
            )
            .await
            .expect("configure DeepSeek");
        let codex = stage_codex_harness(&env.data_dir);
        client
            .call(
                "plugins.install",
                serde_json::json!({"path": codex.display().to_string()}),
            )
            .await
            .expect("install Codex harness");

        let r_code = bridge
            .task_create_with_route(
                "R-Code route",
                "route through DeepSeek",
                "ask",
                None,
                "prompt",
                Some("deepseek"),
                Some("r_code"),
                Some("deepseek-concrete"),
                Some(&agent_contract::InferenceOptions::default()),
                None,
            )
            .await
            .expect("create R-Code route");
        assert_eq!(r_code.provider_name.as_deref(), Some("deepseek"));
        assert_eq!(r_code.model.as_deref(), Some("deepseek-concrete"));
        assert_eq!(r_code.agent_engine, r_code_core::dto::AgentEngine::RCode);
        let detail = client
            .call("task.detail", serde_json::json!({"taskId": r_code.id}))
            .await
            .expect("R-Code detail");
        assert_eq!(detail["harness_id"], "native.r-code");
        assert_eq!(detail["model_route"]["kind"], "host-provider");
        assert_eq!(detail["model_route"]["providerId"], "deepseek");

        let codex = bridge
            .task_create_with_route(
                "Codex route",
                "route through Codex",
                "ask",
                None,
                "prompt",
                None,
                Some("codex"),
                Some("gpt-codex"),
                None,
                None,
            )
            .await
            .expect("create Codex route");
        assert_eq!(codex.provider_name, None);
        assert_eq!(codex.model.as_deref(), Some("gpt-codex"));
        assert_eq!(codex.agent_engine, r_code_core::dto::AgentEngine::Codex);
        let detail = client
            .call("task.detail", serde_json::json!({"taskId": codex.id}))
            .await
            .expect("Codex detail");
        assert_eq!(detail["harness_id"], "codex.r-code");
        assert_eq!(detail["model_route"]["kind"], "harness-managed");
        assert_eq!(detail["model_route"]["harnessId"], "codex.r-code");
    });
    std::env::remove_var(provider_env);
    daemon_common::shutdown_daemon(&env);
}

#[test]
fn desktop_p_gate_projects_approves_and_replays_the_exact_daemon_plan() {
    let (env, _env_vars) = daemon_common::daemon_env("t08b-plan");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(&env.data_dir)
            .with_ipc_name(env.ipc_name.clone()),
    )
    .expect("profile");
    let service_binary = daemon_common::target_debug("r-code-service");
    let bridge = ChatV1Bridge::new_from_profile(profile.clone(), Some(service_binary.clone()));
    let workspace = env.data_dir.join("checkout");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(workspace.join("prd.md"), "Build the complete feature\n")
        .expect("workspace fixture");
    let workspace_text = workspace.to_string_lossy().into_owned();
    let provider_env = format!("R_CODE_T08B_BRIDGE_KEY_{}", std::process::id());
    let provider_secret = "T08B_PROVIDER_SECRET_MUST_NOT_LEAK";
    std::env::set_var(&provider_env, provider_secret);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (task_id, final_revision_hash, final_items) = runtime.block_on(async {
        let (origin, provider) = spawn_plan_provider(2).await;
        bridge
            .task_create("bootstrap", "start daemon", "ask", None, "prompt", None)
            .await
            .expect("bootstrap task");
        let mut client = raw_client(&profile).await;
        client
            .call(
                "settings.apply",
                serde_json::json!({
                    "expectedRevision": 0,
                    "selection": "deepseek",
                    "model": "deepseek-chat",
                    "baseUrl": format!("{origin}/v1"),
                    "protocol": "openai_chat",
                    "envVar": provider_env,
                }),
            )
            .await
            .expect("configure loopback provider");

        let task = bridge
            .task_create_with_route(
                "Desktop P-GATE",
                "Deliver the complete PRD",
                "plan",
                Some(&workspace_text),
                "test system prompt",
                Some("deepseek"),
                Some("r_code"),
                Some("deepseek-chat"),
                None,
                None,
            )
            .await
            .expect("create plan task");
        let missing = bridge
            .plan_create(&task.id)
            .await
            .expect_err("plan_create must not synthesize an old-store plan");
        assert!(missing.to_string().contains("send the first message"));

        bridge
            .agent_send(
                &task.id,
                "Inspect the checkout and prepare the PRD plan",
                Some(&workspace_text),
                "test system prompt",
            )
            .await
            .expect("start planning");
        let first = wait_for_plan(&bridge, &task.id, None).await;
        assert_eq!(first.plan.id.len(), "sha256:".len() + 64);
        assert_eq!(first.plan.task_id, task.id);
        assert_eq!(first.plan.revision, 1);
        assert_eq!(first.plan.state, r_code_core::plan::PlanState::Ready);
        assert_eq!(first.goal.task_id, task.id);
        assert_eq!(first.goal.goal, "Deliver the complete PRD");
        assert_eq!(
            first
                .items
                .iter()
                .map(|item| (item.id.as_str(), item.ordinal, item.depends_on.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("implement", 0, vec!["inspect".to_string()]),
                ("inspect", 1, Vec::new()),
            ],
            "runtime canonicalizes WorkUnits by id and the bridge must preserve that order"
        );
        let projection = serde_json::to_string(&first).expect("serialize projected plan");
        assert!(!projection.contains(provider_secret));
        for digest_name in [
            "route_digest",
            "prompt_digest",
            "permission_digest",
            "workspace_baseline",
        ] {
            assert!(!projection.contains(digest_name), "displayed {digest_name}");
        }
        let replay = bridge
            .plan_create(&task.id)
            .await
            .expect("existing daemon plan is an idempotent create result");
        assert_eq!(replay.plan.id, first.plan.id);
        assert_eq!(replay.plan.revision, first.plan.revision);
        assert_eq!(replay.plan.state, first.plan.state);
        assert_eq!(replay.goal.goal, first.goal.goal);
        assert_eq!(
            replay
                .items
                .iter()
                .map(|item| (&item.id, item.ordinal, &item.depends_on))
                .collect::<Vec<_>>(),
            first
                .items
                .iter()
                .map(|item| (&item.id, item.ordinal, &item.depends_on))
                .collect::<Vec<_>>()
        );

        assert!(bridge
            .plan_approve(&task.id, "", first.plan.revision)
            .await
            .is_err());
        assert!(bridge
            .plan_approve(&task.id, &first.plan.id, 0)
            .await
            .expect_err("wrong expected revision")
            .to_string()
            .contains("stale plan revision"));
        let wrong_hash = format!("sha256:{}", "0".repeat(64));
        assert!(bridge
            .plan_approve(&task.id, &wrong_hash, first.plan.revision)
            .await
            .expect_err("wrong revision hash")
            .to_string()
            .contains("stale plan identity"));
        let other = bridge
            .task_create("Other task", "Other task", "plan", None, "prompt", None)
            .await
            .expect("other task");
        assert!(bridge
            .plan_approve(&other.id, &first.plan.id, first.plan.revision)
            .await
            .expect_err("cross-task approval")
            .to_string()
            .contains("has no daemon Plan"));

        let approved_first = bridge
            .plan_approve(&task.id, &first.plan.id, first.plan.revision)
            .await
            .expect("approve exact first revision");
        assert_eq!(
            approved_first.plan.state,
            r_code_core::plan::PlanState::Approved
        );
        assert_eq!(approved_first.plan.approved_revision, Some(1));
        assert_eq!(
            approved_first.plan.implementation_dispatch_state,
            r_code_core::plan::PlanImplementationDispatchState::NotRequested
        );
        assert!(approved_first
            .plan
            .implementation_queue_message_id
            .is_none());
        let detail = client
            .call("task.detail", serde_json::json!({"taskId": task.id}))
            .await
            .expect("ready task detail");
        assert_eq!(detail["state"], "ready");

        client
            .call_with_id(
                "plan.revise",
                serde_json::json!({"taskId": task.id, "reason": "requirements changed"}),
                "desktop-revise",
            )
            .await
            .expect("revise approved plan");
        bridge
            .agent_send(
                &task.id,
                "Regenerate the plan for the changed requirements",
                Some(&workspace_text),
                "test system prompt",
            )
            .await
            .expect("start revised planning");
        let revised = wait_for_plan(&bridge, &task.id, Some(&first.plan.id)).await;
        assert_eq!(revised.plan.revision, 2);
        assert!(bridge
            .plan_approve(&task.id, &first.plan.id, revised.plan.revision)
            .await
            .expect_err("stale displayed hash")
            .to_string()
            .contains("stale plan identity"));

        let events_before = task_event_kinds(&mut client, &task.id).await;
        let approved = bridge
            .plan_approve(&task.id, &revised.plan.id, revised.plan.revision)
            .await
            .expect("approve exact revised hash");
        assert_eq!(approved.plan.state, r_code_core::plan::PlanState::Approved);
        let events_after = task_event_kinds(&mut client, &task.id).await;
        for kind in ["input.queued", "run.started"] {
            assert_eq!(
                events_after
                    .iter()
                    .filter(|value| value.as_str() == kind)
                    .count(),
                events_before
                    .iter()
                    .filter(|value| value.as_str() == kind)
                    .count(),
                "approval must not enqueue or start implementation via {kind}"
            );
        }
        assert_eq!(
            events_after
                .iter()
                .filter(|value| value.as_str() == "plan.approved")
                .count(),
            2
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), provider)
            .await
            .expect("plan provider did not receive both planning requests")
            .expect("plan provider completed");
        (
            task.id.clone(),
            revised.plan.id.clone(),
            revised
                .items
                .iter()
                .map(|item| (item.id.clone(), item.ordinal, item.depends_on.clone()))
                .collect::<Vec<_>>(),
        )
    });

    daemon_common::shutdown_daemon(&env);
    let restarted = ChatV1Bridge::new_from_profile(profile.clone(), Some(service_binary));
    runtime.block_on(async {
        let restored = restarted
            .plan_get(&task_id)
            .await
            .unwrap_or_else(|error| {
                // CI 重启失败的唯一线索：client 落盘的 daemon stderr。
                let stderr_tail =
                    std::fs::read_to_string(profile.harness_v1_root().join("daemon.last.stderr"))
                        .unwrap_or_else(|_| "(no daemon stderr log)".into());
                panic!("read plan after daemon restart: {error}; daemon stderr: {stderr_tail}")
            })
            .expect("persisted plan after daemon restart");
        assert_eq!(restored.plan.id, final_revision_hash);
        assert_eq!(restored.plan.revision, 2);
        assert_eq!(restored.plan.state, r_code_core::plan::PlanState::Approved);
        assert_eq!(restored.plan.approved_revision, Some(2));
        assert_eq!(
            restored
                .items
                .iter()
                .map(|item| (item.id.clone(), item.ordinal, item.depends_on.clone()))
                .collect::<Vec<_>>(),
            final_items
        );
        let mut client = raw_client(restarted.profile()).await;
        assert_eq!(
            client
                .call("task.detail", serde_json::json!({"taskId": task_id}))
                .await
                .expect("task detail after restart")["state"],
            "ready"
        );
    });
    std::env::remove_var(provider_env);
    daemon_common::shutdown_daemon(&env);
}

#[test]
fn chat_projection_end_to_end_against_real_daemon() {
    let (env, _env_vars) = daemon_common::daemon_env("t42-chat");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(&env.data_dir)
            .with_ipc_name(env.ipc_name.clone()),
    )
    .expect("profile");
    let bridge = ChatV1Bridge::new_from_profile(
        profile,
        Some(daemon_common::target_debug("r-code-service")),
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        // 1. task_create → 旧 Task 形状（11 字段之外的 Task 本体先钉住）。
        let task = bridge
            .task_create(
                "T42 投影",
                "验证 v1 聊天投影层",
                "ask",
                None,
                "test system prompt",
                None,
            )
            .await
            .expect("task_create");
        assert!(task.id.starts_with("task-"), "task id: {}", task.id);
        assert_eq!(task.title, "T42 投影");
        assert_eq!(task.goal, "验证 v1 聊天投影层");
        assert_eq!(task.state, TaskState::Idle);
        assert_eq!(task.workspace_path, None);
        assert_eq!(task.created_at, task.updated_at);

        // 2. task_list 包含新任务。
        let list = bridge.task_list(None, false).await.expect("task_list");
        assert!(
            list.iter()
                .any(|row| row.id == task.id && row.title == "T42 投影"),
            "task_list should contain the created task"
        );

        // 3. agent_send → Ok（v1 排队/启动都算成功）。
        bridge
            .agent_send(&task.id, "请回复一条消息", None, "test system prompt")
            .await
            .expect("agent_send");

        // 4. 轮询：session_messages 出现用户消息；task_detail 出现已终结 run。
        //    无 provider 的诚实失败（run.failed）与正常回复都算投影成功。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut saw_user_message = false;
        let mut run_settled = false;
        let mut detail = None;
        while std::time::Instant::now() < deadline {
            let messages = bridge
                .session_messages(&task.id)
                .await
                .expect("session_messages");
            saw_user_message = messages.iter().any(|message| {
                message.role.as_deref() == Some("user")
                    && message.text.as_deref() == Some("请回复一条消息")
            });

            let current = bridge.task_detail(&task.id).await.expect("task_detail");
            // 11 字段存在且类型正确（形状断言，而非内容断言）。
            assert_eq!(current.task.id, task.id);
            assert_eq!(current.status.task_id, task.id);
            assert_eq!(current.active_branch.id, "main");
            assert_eq!(current.active_branch.task_id, task.id);
            assert_eq!(current.branches.len(), 1);
            assert!(current.queued_messages.is_empty());
            assert!(current.changes.is_empty());
            assert!(current.permissions.is_empty());
            assert!(current.verifications.is_empty());
            assert!(current.pending_plan_entry_offer.is_none());
            run_settled = current.runs.iter().any(|run| run.ended_at.is_some());
            detail = Some(current);

            if saw_user_message && run_settled {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        assert!(
            saw_user_message,
            "session_messages should project the queued user message"
        );
        assert!(
            run_settled,
            "at least one run must reach a terminal projection (assistant reply or honest failure)"
        );

        // 5. 无 Provider 的严格 daemon 会在快照前失败，因此只要求诚实的终结事件；
        // 配置完整的成功路径由下方路由测试覆盖。
        let detail = detail.expect("polled detail");
        assert!(
            detail
                .events
                .iter()
                .any(|event| event.event_type == TaskEventType::RunEnded),
            "events should contain the fail-closed terminal projection"
        );

        // 6. task_rename → 返回旧 Task 且标题生效。
        let renamed = bridge
            .task_rename(&task.id, "改名后的会话")
            .await
            .expect("task_rename");
        assert_eq!(renamed.title, "改名后的会话");

        // 7. task_clone → 新任务，标题带“（克隆）”。
        let clone = bridge.task_clone(&task.id).await.expect("task_clone");
        assert_ne!(clone.id, task.id);
        assert!(
            clone.title.contains("（克隆）"),
            "clone title: {}",
            clone.title
        );
    });

    daemon_common::shutdown_daemon(&env);
}
