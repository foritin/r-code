//! r-code-tui 入口（pi-alignment PRD §4.1 R-TUI-01 / M8-01）。
//!
//! 独立 `[[bin]]`：复用 Host 编排（r_code_host::commands 同源工厂）但不启动
//! WebView/Tauri。默认 `--mode tui`（交互终端）；`--mode print`（单轮后退出）
//! 与 `--mode json`（事件 JSONL 到 stdout）供脚本/管道消费。
//! 默认 data-dir = 桌面应用同一 AppData（会话互通：GUI 可 resume TUI 会话）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use r_code_core::dto::{AgentEvent, TaskState};
use r_code_host::commands::{
    agent_abort, agent_send, install_mock_scenario, task_create, task_detail, task_set_inference,
    CommandState,
};
use r_code_tui::{EventBridge, TuiState};

/// Windows 控制台默认 GBK（代码页 936），而 TUI 的模型回复/IME 中文都是
/// UTF-8——不切换会整屏乱码。设置输入/输出代码页为 UTF-8（65001）。
/// 失败静默（不因环境限制阻断启动）；交互备用屏由 ratatui 自管。
#[cfg(windows)]
fn ensure_utf8_console() {
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleOutputCP(65001);
        windows_sys::Win32::System::Console::SetConsoleCP(65001);
    }
}

#[cfg(not(windows))]
fn ensure_utf8_console() {}

fn usage() -> &'static str {
    "usage: r-code-tui [--mode tui|print|json] [--data-dir <path>] [--message <text>] [--mock]\n\
     --mode print/json 默认走真实 provider（无配置时输出引导并以 exit 2 退出）\n\
     --mock 仅限 --mode print|json：注入确定性演示场景（评估/演示线路），交互模式不可用"
}

fn parse_args(args: &[String]) -> Result<(String, Option<PathBuf>, Option<String>, bool), String> {
    let mut mode = "tui".to_string();
    let mut data_dir: Option<PathBuf> = None;
    let mut message: Option<String> = None;
    let mut mock = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--mode" => {
                index += 1;
                mode = args.get(index).ok_or("--mode 缺值")?.clone();
                if !matches!(mode.as_str(), "tui" | "print" | "json") {
                    return Err(format!("未知 --mode：{mode}"));
                }
            }
            "--data-dir" => {
                index += 1;
                data_dir = Some(PathBuf::from(args.get(index).ok_or("--data-dir 缺值")?));
            }
            "--message" => {
                index += 1;
                message = Some(args.get(index).ok_or("--message 缺值")?.clone());
            }
            // 红线 R1：mock 只允许出现在非交互评估线路；交互模式必须真实 provider。
            "--mock" => mock = true,
            other => return Err(format!("未知参数：{other}")),
        }
        index += 1;
    }
    if mock && mode == "tui" {
        return Err(
            "--mock 仅支持 --mode print|json（评估/演示线路）；交互模式必须使用真实 provider"
                .to_string(),
        );
    }
    Ok((mode, data_dir, message, mock))
}

/// 装配共享 data-dir 的宿主状态（与桌面同一 AppData 根；config/sessions/db
/// 与 GUI 互通——TUI 会话 GUI 可 resume）。
fn shared_state(data_dir: Option<&std::path::Path>) -> Result<CommandState, String> {
    let root = data_dir
        .map(PathBuf::from)
        .or_else(r_code_host::app_paths::default_data_dir)
        .ok_or_else(|| "无法解析默认 AppData 根，请显式传 --data-dir".to_string())?;
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).map_err(|e| format!("建 config 目录：{e}"))?;
    for dir in ["sessions", "blobs"] {
        std::fs::create_dir_all(root.join(dir)).map_err(|e| format!("建 {dir} 目录：{e}"))?;
    }
    let db = r_code_store::Database::open(root.join("app.db"))
        .map_err(|error| format!("open db：{error}"))?;
    // 与桌面同一装配入口（显式 Off 的发布控制；无桌面进程时不读其内存态）。
    let state = CommandState::new_with_planning_release_control(
        Arc::new(db),
        root.join("blobs"),
        root.join("sessions"),
        config_dir,
        root.join("project"),
        Some(root.join("app.db")),
        r_code_host::plan_policy::PlanningReleaseControl {
            provider_kind: "tui".to_string(),
            release_state: r_code_host::plan_policy::PlanningReleaseState::Off,
            emergency_off: false,
            eligibility_profile_version: String::new(),
            evidence_version: String::new(),
            allowed_models: Vec::new(),
            allowed_protocols: Vec::new(),
            allowed_endpoint_classes: Vec::new(),
            basis: "r-code-tui bootstrap".to_string(),
        },
    );
    Ok(state)
}

fn constant_reply(text: &str) -> Vec<AgentEvent> {
    vec![AgentEvent::Message {
        text: text.to_string(),
        delta: false,
    }]
}

/// 交互 TUI：进入备用屏 + 渲染循环；发送（运行中 = steer）、Ctrl-C 中止、
/// Esc 退出。会话复用共享 state 的同一 task。
async fn run_interactive_tui(state: Arc<CommandState>, tui_state: Arc<Mutex<TuiState>>) {
    use r_code_tui::app::{run_interactive, RunController};

    // M1-04：无配置首屏引导（已配置时为空，首屏不出现）。
    for line in r_code_tui::onboarding_lines(&state.config_dir) {
        if let Ok(mut st) = tui_state.lock() {
            st.push_system(line);
        }
    }

    // 会话 task：TUI 打开即一个会话，首个发送 = 新 run。
    let task = task_create(&state, None, "tui", "", "ask")
        .await
        .expect("task_create");
    task_set_inference(
        &state,
        &task.id,
        agent_contract::InferenceOptions {
            thinking: Some("disabled".to_string()),
            reasoning_effort: None,
            verbosity: None,
        },
    )
    .await
    .expect("thinking off");
    let task_id = task.id.clone();

    let handle = tokio::runtime::Handle::current();

    // M1-03：交互路径错误一律进 transcript（alt-screen 下 eprintln 不可见）。
    let send_state = state.clone();
    let send_task = task_id.clone();
    let send_handle = handle.clone();
    let send_tui = tui_state.clone();
    let send_config = state.config_dir.clone();
    let send: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text| {
        let state = send_state.clone();
        let task_id = send_task.clone();
        let tui = send_tui.clone();
        let config_dir = send_config.clone();
        send_handle.spawn(async move {
            // agent_send 的 Auto 语义：运行中自动 steer，空闲时启动新 run。
            if let Err(error) = agent_send(&state, &task_id, &text).await {
                if let Ok(mut st) = tui.lock() {
                    st.push_system(format!(
                        "发送失败：{}",
                        r_code_tui::provider_error_guidance(&error, &config_dir)
                    ));
                }
            }
        });
    });

    let abort_state = state.clone();
    let abort_task = task_id.clone();
    let abort_handle = handle.clone();
    let abort_tui = tui_state.clone();
    let abort: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let state = abort_state.clone();
        let task_id = abort_task.clone();
        let tui = abort_tui.clone();
        abort_handle.spawn(async move {
            if let Err(error) = agent_abort(&state, &task_id).await {
                if let Ok(mut st) = tui.lock() {
                    st.push_system(format!("中止失败：{error}"));
                }
            }
        });
    });

    // 初始模型投影（footer 右侧）：任务绑定优先，回落全局默认。
    {
        let settings = r_code_host::settings::SettingsService::new(state.config_dir.clone());
        if let Ok(config) = settings.load_global_unvalidated() {
            let detail = task_detail(&state, &task_id).await.ok();
            let provider = detail
                .as_ref()
                .and_then(|item| item.task.provider_name.clone())
                .unwrap_or_else(|| config.default_provider.clone());
            let model = detail
                .as_ref()
                .and_then(|item| item.task.model.clone())
                .or_else(|| config.providers.get(&provider).map(|p| p.model.clone()));
            if let Some(model) = model {
                tui_state
                    .lock()
                    .unwrap()
                    .set_model_selection(provider, model);
            }
            // M2-02/M2-03：初始思考档位与任务模式投影。
            if let Some(effort) = detail
                .as_ref()
                .and_then(|item| item.task.inference.reasoning_effort.clone())
            {
                tui_state.lock().unwrap().set_thinking(Some(effort));
            }
            if let Some(mode) = detail.as_ref().map(|item| item.task.mode.to_string()) {
                tui_state.lock().unwrap().set_task_mode(mode);
            }
        }
    }

    // M2-01：/model 选择器数据源（可用集 = model_availability 的 available，
    // 与设置页/`--list-models` 同一口径；快照构建是同步纯函数）。
    let picker_state = state.clone();
    let picker_tui = tui_state.clone();
    let open_model_picker: Arc<
        dyn Fn() -> Option<r_code_tui::model_selector::ModelPicker> + Send + Sync,
    > = Arc::new(move || {
        let settings = r_code_host::settings::SettingsService::new(picker_state.config_dir.clone());
        let config = settings.load_global_unvalidated().ok()?;
        let decls =
            r_code_host::provider_decl::load_decls(&picker_state.config_dir).unwrap_or_default();
        let snapshot = r_code_host::model_availability::build_snapshot(
            &config,
            &decls,
            None,
            &r_code_host::model_availability::runtime_has_auth,
        );
        let entries = r_code_tui::model_selector::picker_entries(&snapshot.available);
        if entries.is_empty() {
            return None;
        }
        let current = picker_tui
            .lock()
            .ok()
            .and_then(|st| st.model_selection().map(|(provider, _)| provider.clone()));
        Some(r_code_tui::model_selector::ModelPicker::new(
            entries,
            current.as_deref(),
        ))
    });

    let select_state = state.clone();
    let select_task = task_id.clone();
    let select_handle = handle.clone();
    let select_tui = tui_state.clone();
    let select_model: Arc<dyn Fn(r_code_tui::model_selector::ModelEntry) + Send + Sync> =
        Arc::new(move |entry| {
            let state = select_state.clone();
            let task_id = select_task.clone();
            let tui = select_tui.clone();
            select_handle.spawn(async move {
                match r_code_tui::model_selector::apply_model_selection(&state, &task_id, &entry)
                    .await
                {
                    Ok(label) => {
                        if let Ok(mut st) = tui.lock() {
                            st.set_model_selection(entry.provider.clone(), entry.model.clone());
                            st.push_system(format!("已切换模型：{label}"));
                        }
                    }
                    Err(error) => {
                        if let Ok(mut st) = tui.lock() {
                            st.push_system(format!("切换模型失败：{error}"));
                        }
                    }
                }
            });
        });

    // M2-02：思考档位写回（alt+T 弹层与 alt+,/alt+. 升降共用；per-task 持久）。
    let think_state = state.clone();
    let think_task = task_id.clone();
    let think_handle = handle.clone();
    let think_tui = tui_state.clone();
    let set_thinking: Arc<dyn Fn(&'static str) + Send + Sync> = Arc::new(move |level| {
        let state = think_state.clone();
        let task_id = think_task.clone();
        let tui = think_tui.clone();
        think_handle.spawn(async move {
            match r_code_tui::thinking::apply_thinking(&state, &task_id, level).await {
                Ok(()) => {
                    if let Ok(mut st) = tui.lock() {
                        st.set_thinking(Some(level.to_string()));
                        st.push_system(format!("思考级别：{level}"));
                    }
                }
                Err(error) => {
                    if let Ok(mut st) = tui.lock() {
                        st.push_system(format!("设置思考级别失败：{error}"));
                    }
                }
            }
        });
    });

    // M2-03：Shift+Tab 模式循环写回（运行中/Plan+Codex 由宿主拒绝）。
    let mode_state = state.clone();
    let mode_task = task_id.clone();
    let mode_handle = handle.clone();
    let mode_tui = tui_state.clone();
    let set_mode: Arc<dyn Fn(&'static str) + Send + Sync> = Arc::new(move |mode| {
        let state = mode_state.clone();
        let task_id = mode_task.clone();
        let tui = mode_tui.clone();
        mode_handle.spawn(async move {
            match r_code_tui::task_mode::apply_mode(&state, &task_id, mode).await {
                Ok(()) => {
                    if let Ok(mut st) = tui.lock() {
                        st.set_task_mode(mode.to_string());
                        st.push_system(format!("模式：{mode}"));
                    }
                }
                Err(error) => {
                    if let Ok(mut st) = tui.lock() {
                        st.push_system(format!("切换模式失败：{error}"));
                    }
                }
            }
        });
    });

    // M2-04：运行中排队（宿主 AgentSendMode::Queue；run 结束自动派发）。
    let queue_state = state.clone();
    let queue_task = task_id.clone();
    let queue_handle = handle.clone();
    let queue_tui = tui_state.clone();
    let queue_send: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text| {
        let state = queue_state.clone();
        let task_id = queue_task.clone();
        let tui = queue_tui.clone();
        queue_handle.spawn(async move {
            use r_code_core::dto::AgentSendMode;
            if let Err(error) = r_code_host::commands::agent_send_with_mode(
                &state,
                &task_id,
                &text,
                AgentSendMode::Queue,
            )
            .await
            {
                if let Ok(mut st) = tui.lock() {
                    st.push_system(format!("排队失败：{error}"));
                }
            }
        });
    });

    // M2-05：审批请求订阅泵（PermissionEngine broadcast → TuiState 投影）。
    {
        let engine = state.permission_engine.clone();
        let tui = tui_state.clone();
        let mut requests = engine.subscribe_requests();
        handle.spawn(async move {
            loop {
                match requests.recv().await {
                    Ok(request) => {
                        if let Ok(mut st) = tui.lock() {
                            st.set_pending_approval(
                                r_code_tui::approval_overlay::PendingApproval::from_request(
                                    &request,
                                ),
                            );
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }

    // M2-05：y/a/esc 决策落账（意图经宿主 PermissionEngine；不绕过安全边界）。
    let approval_state = state.clone();
    let approval_handle = handle.clone();
    let approval_tui = tui_state.clone();
    let decide_approval: Arc<dyn Fn(r_code_tui::approval::ApprovalDecision) + Send + Sync> =
        Arc::new(move |decision| {
            let Some(pending) = approval_tui
                .lock()
                .ok()
                .and_then(|mut st| st.take_pending_approval())
            else {
                return;
            };
            let engine = approval_state.permission_engine.clone();
            let tui = approval_tui.clone();
            approval_handle.spawn(async move {
                let host_decision = r_code_tui::approval_overlay::to_host_decision(decision);
                let result = engine.decide(&pending.request_id, host_decision).await;
                match result {
                    Ok(()) => {
                        if let Ok(mut st) = tui.lock() {
                            st.push_system(r_code_tui::approval_overlay::resolution_note(
                                &pending, decision,
                            ));
                        }
                    }
                    Err(error) => {
                        if let Ok(mut st) = tui.lock() {
                            st.push_system(format!("审批落账失败：{error}"));
                        }
                    }
                }
            });
        });

    // M3-01：用量刷新泵（持久化投影：task_detail.runs.usage_json 累加；
    // resume 后仍准确。上下文窗口数据源暂缺——未知窗口按 codex 形态回退 used）。
    {
        let refresh_state = state.clone();
        let refresh_task = task_id.clone();
        let refresh_tui = tui_state.clone();
        handle.spawn(async move {
            loop {
                if let Ok(detail) = task_detail(&refresh_state, &refresh_task).await {
                    let stats = r_code_tui::status_bar::accumulate_usage(&detail.runs);
                    if let Ok(mut st) = refresh_tui.lock() {
                        st.set_usage(stats);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }

    // M3-02：/status 与 /usage 数据装配（模型标签 + 目录缩写 + 用量/成本）。
    let status_state = state.clone();
    let status_task = task_id.clone();
    let status_tui = tui_state.clone();
    let status_report: Arc<dyn Fn() -> (Vec<String>, String) + Send + Sync> = Arc::new(move || {
        let model_label = status_tui
            .lock()
            .ok()
            .and_then(|st| {
                st.model_selection().map(|(provider, model)| {
                    r_code_tui::model_selector::model_label(provider, model)
                })
            })
            .unwrap_or_else(|| "未选择".to_string());
        let directory = std::env::current_dir()
            .map(|path| {
                let text = path.display().to_string();
                match std::env::var_os("HOME") {
                    Some(home) => text.replace(home.to_string_lossy().as_ref(), "~"),
                    None => text,
                }
            })
            .unwrap_or_else(|_| "?".to_string());
        let Ok(detail) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(task_detail(&status_state, &status_task))
        }) else {
            return (Vec::new(), "状态不可用".to_string());
        };
        let stats = r_code_tui::status_bar::accumulate_usage(&detail.runs);
        let cost = r_code_tui::status_bar::accumulate_cost(&detail.runs);
        let card =
            r_code_tui::status_bar::status_card_lines(&model_label, &directory, &stats, None);
        let summary = r_code_tui::status_bar::usage_summary(&stats, cost);
        (card, summary)
    });

    // M4-04：! 直通（宿主 LocalShellBackend 五级 shell 链；cwd = 当前目录）。
    let bang_tui = tui_state.clone();
    let run_bang: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |command| {
        let tui = bang_tui.clone();
        tokio::runtime::Handle::current().spawn(async move {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let (output, exit_code) = r_code_tui::bang_exec::run_bang(&command, &cwd).await;
            if let Ok(mut st) = tui.lock() {
                st.push_row(r_code_tui::TranscriptRow::Shell(
                    r_code_tui::bang_command::ShellRow::Output {
                        text: output,
                        exit_code,
                    },
                ));
            }
        });
    });

    let controller = RunController {
        send,
        abort,
        open_model_picker,
        select_model,
        set_thinking,
        set_mode,
        queue_send,
        decide_approval,
        status_report,
        run_bang,
    };

    // M5-02：inline 模式——只进 raw + bracketed paste，不进备用屏
    //（历史进终端 scrollback、退出保留；行差分渲染见 app.rs）。
    crossterm::terminal::enable_raw_mode().expect("raw mode");
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste)
        .expect("enable bracketed paste");

    run_interactive(tui_state, controller).await;

    crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste)
        .expect("disable bracketed paste");
    crossterm::terminal::disable_raw_mode().expect("disable raw mode");
}

#[tokio::main]
async fn main() {
    ensure_utf8_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, data_dir, message, mock) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("r-code-tui: {error}\n{}", usage());
            std::process::exit(2);
        }
    };

    let data_root = data_dir
        .clone()
        .or_else(r_code_host::app_paths::default_data_dir);
    let state = Arc::new(match shared_state(data_dir.as_deref()) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("r-code-tui: {error}");
            std::process::exit(1);
        }
    });

    // R1 真实化：除显式 --mock 评估线路外，装配即真实 provider；
    // 配置缺失/无效由宿主 ensure_real_runtime 直接报错，不降级到演示回放。
    if !mock {
        state.enable_real_agent_mode().await;
    }

    // 事件桥：宿主 sink → 共享 TUI 状态。
    let tui_state = Arc::new(Mutex::new(TuiState::new()));
    let bridge = Arc::new(EventBridge::new(tui_state.clone()));
    let sink_target = bridge.clone();
    state.set_agent_event_sink(Arc::new(move |task_id, event| {
        sink_target.forward(task_id, event);
    }));

    if mode == "tui" {
        run_interactive_tui(state, tui_state).await;
        return;
    }

    // 非交互（脚本/管道）：单轮执行后退出。
    let Some(message) = message else {
        eprintln!("r-code-tui: {} 模式需要 --message", mode);
        std::process::exit(2);
    };

    // R2 显式引导：无 provider 配置时报出两条可操作途径并 exit 2，
    // 不进入任何演示回放。
    if !mock {
        let config_dir = data_root
            .as_deref()
            .map(|root| root.join("config"))
            .unwrap_or_else(|| std::path::PathBuf::from("config"));
        if let Some(guidance) = r_code_tui::provider_config_guidance(&config_dir) {
            eprintln!("r-code-tui: {guidance}");
            std::process::exit(2);
        }
    }

    let task = task_create(&state, None, "tui", &message, "ask")
        .await
        .expect("task_create");
    task_set_inference(
        &state,
        &task.id,
        agent_contract::InferenceOptions {
            thinking: Some("disabled".to_string()),
            reasoning_effort: None,
            verbosity: None,
        },
    )
    .await
    .expect("thinking off");
    if mock {
        // 评估/演示线路：确定性场景回放（仅 Mock runtime 接受注入）。
        let reply = format!("[r-code-tui] 已收到：{message}");
        install_mock_scenario(&state, &task.id, constant_reply(&reply))
            .await
            .unwrap_or(false);
    }
    if let Err(error) = agent_send(&state, &task.id, &message).await {
        eprintln!("r-code-tui: 发送失败：{error}");
        std::process::exit(1);
    }
    // 等待收敛。
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let detail = task_detail(&state, &task.id).await.expect("detail");
        if matches!(detail.task.state, TaskState::Idle | TaskState::ReviewReady) {
            break;
        }
    }
    let mut final_state = tui_state.lock().unwrap();
    final_state.flush_streaming();
    let rows = final_state.rows().to_vec();
    if mode == "json" {
        for row in &rows {
            println!("{}", serde_json::to_string(row).unwrap_or_default());
        }
    } else {
        for row in &rows {
            match row {
                r_code_tui::TranscriptRow::User { text } => println!("你 > {text}"),
                r_code_tui::TranscriptRow::Assistant { text, .. } => {
                    println!("R-Code > {text}")
                }
                r_code_tui::TranscriptRow::ToolCard { name, is_error, .. } => {
                    println!("  [tool] {name}{}", if *is_error { " (失败)" } else { "" })
                }
                r_code_tui::TranscriptRow::System { text } => println!("· {text}"),
                r_code_tui::TranscriptRow::Shell(shell) => match shell {
                    r_code_tui::bang_command::ShellRow::Prompt { command } => {
                        println!("$ {command}")
                    }
                    r_code_tui::bang_command::ShellRow::Output { exit_code, .. } => {
                        println!("  (shell 退出码 {exit_code:?})")
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod m1_tests {
    use super::*;

    fn arg_list(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn mock_flag_is_rejected_for_interactive_mode() {
        // 默认 tui 模式 + --mock：拒绝（红线 R1）。
        let error = parse_args(&arg_list(&["--mock"])).expect_err("must reject");
        assert!(error.contains("交互模式"), "unexpected: {error}");
        assert!(error.contains("--mock"), "unexpected: {error}");
        // 显式 tui + --mock：同样拒绝。
        assert!(parse_args(&arg_list(&["--mode", "tui", "--mock"])).is_err());
        // print/json + --mock：接受。
        for mode in ["print", "json"] {
            let parsed = parse_args(&arg_list(&["--mode", mode, "--mock", "--message", "x"]))
                .expect("eval line must accept --mock");
            assert!(parsed.3, "mock flag must parse for {mode}");
        }
    }

    #[test]
    fn mock_flag_defaults_to_false_for_plain_print() {
        let (_, _, _, mock) =
            parse_args(&arg_list(&["--mode", "print", "--message", "x"])).expect("parse");
        assert!(!mock, "print without --mock must be real mode");
    }
}
