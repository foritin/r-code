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
    agent_abort, agent_resend, agent_send, agent_send_with_mode_and_attachments,
    codex_integration_status, codex_start_device_login, codex_start_login, install_mock_scenario,
    session_branch_list, session_messages, task_clone, task_create, task_detail,
    task_set_inference, task_switch_branch, AttachmentInput, CommandState,
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
       r-code-tui auth check [--data-dir <path>]\n\
     --mode print/json 默认走真实 provider（无配置时输出引导并以 exit 2 退出）\n\
     --mock 仅限 --mode print|json：注入确定性演示场景（评估/演示线路），交互模式不可用\n\
     auth check 打印各 provider 认证状态（默认服务已认证 = exit 0，否则 exit 1）"
}

/// `auth check` 报告（G4，pi `pi auth check` 对齐；脚本/CI 消费）。
/// 口径与 /model 选择器同源（load_global_unvalidated + decls +
/// build_snapshot(runtime_has_auth)）。
fn auth_check_report(config_dir: &std::path::Path) -> i32 {
    let settings = r_code_host::settings::SettingsService::new(config_dir.to_path_buf());
    let config = match settings.load_global_unvalidated() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("auth check: 配置加载失败：{error}");
            return 1;
        }
    };
    let decls = r_code_host::provider_decl::load_decls(config_dir).unwrap_or_default();
    let snapshot = r_code_host::model_availability::build_snapshot(
        &config,
        &decls,
        None,
        &r_code_host::model_availability::runtime_has_auth,
    );
    println!("default provider: {}", config.default_provider);
    // provider 去重序（all 保序；available 子集判定认证态）。
    let mut providers: Vec<String> = Vec::new();
    for entry in &snapshot.all {
        if !providers.contains(&entry.provider) {
            providers.push(entry.provider.clone());
        }
    }
    if providers.is_empty() {
        println!("no providers configured（/setup 或桌面设置页完成配置）");
        return 1;
    }
    for provider in &providers {
        let authed_models = snapshot
            .available
            .iter()
            .filter(|entry| &entry.provider == provider)
            .count();
        if authed_models > 0 {
            println!("ok      {provider} ({authed_models} models)");
        } else {
            println!("no-auth {provider}（配置存在但缺鉴权，/setup 重配或检查凭据）");
        }
    }
    let default_ok = snapshot
        .available
        .iter()
        .any(|entry| entry.provider == config.default_provider);
    if default_ok {
        0
    } else {
        1
    }
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

/// 新任务默认推理参数（与启动任务一致：显式关闭 thinking）。
fn disabled_inference() -> agent_contract::InferenceOptions {
    agent_contract::InferenceOptions {
        thinking: Some("disabled".to_string()),
        reasoning_effort: None,
        verbosity: None,
    }
}

/// footer 投影刷新：任务绑定优先，回落全局默认（启动 / resume / 分支切换共用）。
async fn refresh_projection(state: &CommandState, tui: &Arc<Mutex<TuiState>>, task_id: &str) {
    let settings = r_code_host::settings::SettingsService::new(state.config_dir.clone());
    let Ok(config) = settings.load_global_unvalidated() else {
        return;
    };
    let Ok(detail) = task_detail(state, task_id).await else {
        return;
    };
    let provider = detail
        .task
        .provider_name
        .clone()
        .unwrap_or_else(|| config.default_provider.clone());
    let model = detail
        .task
        .model
        .clone()
        .or_else(|| config.providers.get(&provider).map(|p| p.model.clone()));
    if let Ok(mut st) = tui.lock() {
        if let Some(model) = model {
            st.set_model_selection(provider, model);
        }
        st.set_thinking(detail.task.inference.reasoning_effort.clone());
        st.set_task_mode(detail.task.mode.to_string());
    }
}

/// 装入某任务为当前会话（G8：/resume 接续、/new 切换、克隆后打开）：
/// 切 current_task 句柄 + 事件过滤口径 → 活跃分支 JSONL 重建 transcript →
/// 刷新 footer 投影。顺序有意先切句柄再重建——重建期间流入的事件归新任务。
async fn adopt_task(
    state: &CommandState,
    tui: &Arc<Mutex<TuiState>>,
    current_task: &Arc<Mutex<String>>,
    task_id: &str,
    note: String,
) {
    if let Ok(mut id) = current_task.lock() {
        *id = task_id.to_string();
    }
    if let Ok(mut st) = tui.lock() {
        st.set_task_id(task_id);
    }
    let messages = session_messages(state, task_id).await.unwrap_or_default();
    if let Ok(mut st) = tui.lock() {
        st.rebuild_from_session(&messages);
        st.push_system(note);
    }
    refresh_projection(state, tui, task_id).await;
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
    task_set_inference(&state, &task.id, disabled_inference())
        .await
        .expect("thinking off");
    // G8：当前任务句柄——/new /resume /clone 切换后更新；所有会话操作
    // 闭包在**调用时**读取（不再克隆固定值，修复 /resume 后发送仍进旧会话）。
    let current_task: Arc<Mutex<String>> = Arc::new(Mutex::new(task.id.clone()));
    tui_state.lock().unwrap().set_task_id(task.id.clone());

    let handle = tokio::runtime::Handle::current();

    // M1-03：交互路径错误一律进 transcript（alt-screen 下 eprintln 不可见）。
    let send_state = state.clone();
    let send_current = current_task.clone();
    let send_handle = handle.clone();
    let send_tui = tui_state.clone();
    let send_config = state.config_dir.clone();
    let send: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text| {
        let state = send_state.clone();
        let task_id = send_current.lock().unwrap().clone();
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
    let abort_current = current_task.clone();
    let abort_handle = handle.clone();
    let abort_tui = tui_state.clone();
    let abort: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let state = abort_state.clone();
        let task_id = abort_current.lock().unwrap().clone();
        let tui = abort_tui.clone();
        abort_handle.spawn(async move {
            if let Err(error) = agent_abort(&state, &task_id).await {
                if let Ok(mut st) = tui.lock() {
                    st.push_system(format!("中止失败：{error}"));
                }
            }
        });
    });

    // 初始模型投影（footer 右侧）：任务绑定优先，回落全局默认
    //（G8 起 /resume //new 切换共用 refresh_projection）。
    refresh_projection(&state, &tui_state, &task.id).await;

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
    let select_current = current_task.clone();
    let select_handle = handle.clone();
    let select_tui = tui_state.clone();
    let select_model: Arc<dyn Fn(r_code_tui::model_selector::ModelEntry) + Send + Sync> =
        Arc::new(move |entry| {
            let state = select_state.clone();
            let task_id = select_current.lock().unwrap().clone();
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
    let think_current = current_task.clone();
    let think_handle = handle.clone();
    let think_tui = tui_state.clone();
    let set_thinking: Arc<dyn Fn(&'static str) + Send + Sync> = Arc::new(move |level| {
        let state = think_state.clone();
        let task_id = think_current.lock().unwrap().clone();
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
    let mode_current = current_task.clone();
    let mode_handle = handle.clone();
    let mode_tui = tui_state.clone();
    let set_mode: Arc<dyn Fn(&'static str) + Send + Sync> = Arc::new(move |mode| {
        let state = mode_state.clone();
        let task_id = mode_current.lock().unwrap().clone();
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
    let queue_current = current_task.clone();
    let queue_handle = handle.clone();
    let queue_tui = tui_state.clone();
    let queue_send: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text| {
        let state = queue_state.clone();
        let task_id = queue_current.lock().unwrap().clone();
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
        let refresh_current = current_task.clone();
        let refresh_tui = tui_state.clone();
        handle.spawn(async move {
            loop {
                let task_id = refresh_current.lock().unwrap().clone();
                if let Ok(detail) = task_detail(&refresh_state, &task_id).await {
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
    let status_current = current_task.clone();
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
        let status_task = status_current.lock().unwrap().clone();
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

    // G9：/session 会话统计卡装配（TuiState 消息计数 + task_detail 会话维度 +
    // 最近 run 的 JSONL 会话文件路径）。
    let session_card_state = state.clone();
    let session_card_current = current_task.clone();
    let session_card_tui = tui_state.clone();
    let session_report: Arc<dyn Fn() -> Vec<String> + Send + Sync> = Arc::new(move || {
        let session_card_task = session_card_current.lock().unwrap().clone();
        let (rows, model_label) = session_card_tui
            .lock()
            .ok()
            .and_then(|st| {
                st.model_selection().map(|(provider, model)| {
                    (
                        st.rows().to_vec(),
                        r_code_tui::model_selector::model_label(provider, model),
                    )
                })
            })
            .unwrap_or_default();
        let model_label = if model_label.is_empty() {
            "未选择".to_string()
        } else {
            model_label
        };
        let Ok(detail) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(task_detail(&session_card_state, &session_card_task))
        }) else {
            return Vec::new();
        };
        let stats = r_code_tui::status_bar::accumulate_usage(&detail.runs);
        let cost = r_code_tui::status_bar::accumulate_cost(&detail.runs);
        let session_file = detail.runs.iter().rev().find_map(|run| {
            run.external_session_id.as_deref().map(|session_id| {
                session_card_state
                    .sessions_dir
                    .join(format!("{session_id}.jsonl"))
                    .display()
                    .to_string()
            })
        });
        let input = r_code_tui::status_bar::SessionCardInput {
            task_id: detail.task.id.clone(),
            title: detail.task.title.clone(),
            model_label,
            created_at: detail
                .task
                .created_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            messages: r_code_tui::status_bar::count_messages(&rows),
            runs: detail.runs.len(),
            stats,
            cost,
            session_file,
        };
        r_code_tui::status_bar::session_card_lines(&input)
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

    // M6-01/M6-02：会话操作（task_list 列表、resume 接管、new/rename）。
    let resume_state = state.clone();
    let open_resume: Arc<
        dyn Fn() -> Option<r_code_tui::session_picker::SessionPicker> + Send + Sync,
    > = Arc::new(move || {
        let state = resume_state.clone();
        let tasks = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(r_code_host::commands::task_list(&state, None, false))
        })
        .unwrap_or_default();
        let entries: Vec<r_code_tui::session_picker::SessionEntry> = tasks
            .iter()
            .map(r_code_tui::session_picker::entry_from_task)
            .collect();
        if entries.is_empty() {
            None
        } else {
            Some(r_code_tui::session_picker::SessionPicker::new(entries))
        }
    });

    let resume_session_state = state.clone();
    let resume_session_tui = tui_state.clone();
    let resume_session_current = current_task.clone();
    let resume_session: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |task_id| {
        let state = resume_session_state.clone();
        let tui = resume_session_tui.clone();
        let current = resume_session_current.clone();
        tokio::runtime::Handle::current().spawn(async move {
            match r_code_host::commands::task_detail(&state, &task_id).await {
                Ok(detail) => {
                    // G8：真正接续——切换当前任务 + JSONL 重建 transcript + 投影刷新
                    //（此前只提示不切换，发送仍进旧会话）。
                    adopt_task(
                        &state,
                        &tui,
                        &current,
                        &task_id,
                        format!("已接续会话：{}", detail.task.title),
                    )
                    .await;
                }
                Err(error) => {
                    if let Ok(mut st) = tui.lock() {
                        st.push_system(format!("接续会话失败：{error}"));
                    }
                }
            }
        });
    });

    let new_session_state = state.clone();
    let new_session_tui = tui_state.clone();
    let new_session_current = current_task.clone();
    let new_session: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let state = new_session_state.clone();
        let tui = new_session_tui.clone();
        let current = new_session_current.clone();
        tokio::runtime::Handle::current().spawn(async move {
            match r_code_tui::session_ops::new_session(&state).await {
                Ok(new_id) => {
                    let _ = task_set_inference(&state, &new_id, disabled_inference()).await;
                    // G8：/new 真正切换到新会话（此前只建任务，发送仍进旧会话）。
                    adopt_task(
                        &state,
                        &tui,
                        &current,
                        &new_id,
                        "已新建空白会话".to_string(),
                    )
                    .await;
                }
                Err(error) => {
                    if let Ok(mut st) = tui.lock() {
                        st.push_system(format!("新建会话失败：{error}"));
                    }
                }
            }
        });
    });

    let rename_session_state = state.clone();
    let rename_session_tui = tui_state.clone();
    let rename_session_current = current_task.clone();
    let rename_session: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |title| {
        let state = rename_session_state.clone();
        let task_id = rename_session_current.lock().unwrap().clone();
        let tui = rename_session_tui.clone();
        tokio::runtime::Handle::current().spawn(async move {
            match r_code_tui::session_ops::rename_session(&state, &task_id, &title).await {
                Ok(()) => {
                    if let Ok(mut st) = tui.lock() {
                        st.push_system(format!("已重命名会话：{title}"));
                    }
                }
                Err(error) => {
                    if let Ok(mut st) = tui.lock() {
                        st.push_system(format!("重命名失败：{error}"));
                    }
                }
            }
        });
    });

    // G2：模型选择器 Ctrl+S —— 选择并持久为全局默认（写 config，同 /setup
    // 的写回口径：load → 改 default_provider + 该服务默认模型 → save_global）。
    let persist_state = state.clone();
    let persist_tui = tui_state.clone();
    let persist_default_model: Arc<dyn Fn(r_code_tui::model_selector::ModelEntry) + Send + Sync> =
        Arc::new(move |entry| {
            let settings =
                r_code_host::settings::SettingsService::new(persist_state.config_dir.clone());
            let mut config = match settings.load_global_unvalidated() {
                Ok(config) => config,
                Err(error) => {
                    if let Ok(mut st) = persist_tui.lock() {
                        st.push_system(format!("持久化默认模型失败：{error}"));
                    }
                    return;
                }
            };
            config.default_provider = entry.provider.clone();
            if let Some(provider) = config.providers.get_mut(&entry.provider) {
                provider.model = entry.model.clone();
            }
            match settings.save_global(&config) {
                Ok(()) => {
                    if let Ok(mut st) = persist_tui.lock() {
                        st.push_system(format!(
                            "已设为全局默认：({}) {}（新会话沿用）",
                            entry.provider, entry.model
                        ));
                    }
                }
                Err(error) => {
                    if let Ok(mut st) = persist_tui.lock() {
                        st.push_system(format!("持久化默认模型失败：{error}"));
                    }
                }
            }
        });

    // G5：/compact [prompt] —— 宿主显式压缩（focus=自定义指令）。
    let compact_state = state.clone();
    let compact_current = current_task.clone();
    let compact_handle = handle.clone();
    let compact_context: Arc<dyn Fn(Option<String>) -> Result<String, String> + Send + Sync> =
        Arc::new(move |focus| {
            let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
            let state = compact_state.clone();
            let task_id = compact_current.lock().unwrap().clone();
            compact_handle.spawn(async move {
                let focus = focus.as_deref();
                let outcome = match r_code_host::commands::task_compact_context(
                    &state, &task_id, focus,
                )
                .await
                {
                    Ok(result) => {
                        if result.compacted {
                            Ok(format!(
                                "上下文已压缩：{} → {} 条消息",
                                result.before_messages, result.after_messages
                            ))
                        } else {
                            Ok(format!(
                                "无需压缩（当前 {} 条消息，低于阈值）",
                                result.before_messages
                            ))
                        }
                    }
                    Err(error) => Err(error),
                };
                let _ = tx.send(outcome);
            });
            rx.recv_timeout(std::time::Duration::from_secs(120))
                .unwrap_or_else(|_| Err("压缩超时（120s）".to_string()))
        });

    // G8：/tree 分支树（同步读取 SQLite 分支元数据）。
    let tree_state = state.clone();
    let tree_current = current_task.clone();
    let open_tree: Arc<dyn Fn() -> Option<r_code_tui::session_tree::BranchTree> + Send + Sync> =
        Arc::new(move || {
            let state = tree_state.clone();
            let task_id = tree_current.lock().unwrap().clone();
            let branches = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(session_branch_list(&state, &task_id))
            })
            .unwrap_or_default();
            Some(r_code_tui::session_tree::BranchTree::new(&branches))
        });

    // G8：切换分支（宿主激活目标分支 + JSONL 重放语义；成功即重建 transcript）。
    let switch_state = state.clone();
    let switch_current = current_task.clone();
    let switch_tui = tui_state.clone();
    let switch_handle = handle.clone();
    let switch_branch: Arc<dyn Fn(String) -> Result<String, String> + Send + Sync> =
        Arc::new(move |branch_id| {
            let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
            let state = switch_state.clone();
            let task_id = switch_current.lock().unwrap().clone();
            let tui = switch_tui.clone();
            switch_handle.spawn(async move {
                let outcome = match task_switch_branch(&state, &task_id, &branch_id).await {
                    Ok(branch) => {
                        let messages = session_messages(&state, &task_id).await.unwrap_or_default();
                        if let Ok(mut st) = tui.lock() {
                            st.rebuild_from_session(&messages);
                        }
                        Ok(format!(
                            "已切换到分支 {}",
                            r_code_tui::session_tree::short_id(&branch.id)
                        ))
                    }
                    Err(error) => Err(error),
                };
                let _ = tx.send(outcome);
            });
            rx.recv_timeout(std::time::Duration::from_secs(30))
                .unwrap_or_else(|_| Err("切换分支超时（30s）".to_string()))
        });

    // G8：/fork 消息选择器（活跃分支 JSONL 的 user 消息投影）。
    let fork_state = state.clone();
    let fork_current = current_task.clone();
    let open_fork: Arc<dyn Fn() -> Option<r_code_tui::session_tree::ForkPicker> + Send + Sync> =
        Arc::new(move || {
            let state = fork_state.clone();
            let task_id = fork_current.lock().unwrap().clone();
            let messages = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(session_messages(&state, &task_id))
            })
            .unwrap_or_default();
            let entries = r_code_tui::session_tree::fork_entries(&messages);
            if entries.is_empty() {
                None
            } else {
                Some(r_code_tui::session_tree::ForkPicker::new(entries))
            }
        });

    // G8：分叉重发（agent_resend = 前缀复制 + 新分支激活 + 改写消息重发）。
    let fork_send_state = state.clone();
    let fork_send_current = current_task.clone();
    let fork_send_tui = tui_state.clone();
    let fork_send_handle = handle.clone();
    let fork_send: Arc<dyn Fn(String, String) -> Result<String, String> + Send + Sync> =
        Arc::new(move |message_id, text| {
            let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
            let state = fork_send_state.clone();
            let task_id = fork_send_current.lock().unwrap().clone();
            let tui = fork_send_tui.clone();
            fork_send_handle.spawn(async move {
                let outcome = match agent_resend(&state, &task_id, &message_id, &text).await {
                    Ok(()) => {
                        // 分叉成功：重建视图（新分支前缀 + 刚重发的消息）；
                        // run 事件经事件桥继续追加。
                        let messages = session_messages(&state, &task_id).await.unwrap_or_default();
                        if let Ok(mut st) = tui.lock() {
                            st.rebuild_from_session(&messages);
                        }
                        Ok(format!(
                            "已从消息 {} 分叉并重发（原分支保留，/tree 可切回）",
                            message_id
                                .rsplit_once(':')
                                .map(|(_, line)| format!("#{line}"))
                                .unwrap_or_else(|| message_id.clone())
                        ))
                    }
                    Err(error) => Err(error),
                };
                let _ = tx.send(outcome);
            });
            rx.recv_timeout(std::time::Duration::from_secs(30))
                .unwrap_or_else(|_| Err("分叉超时（30s）".to_string()))
        });

    // G8：/clone 克隆当前会话（新任务承载同历史；留在当前会话）。
    let clone_state = state.clone();
    let clone_current = current_task.clone();
    let clone_handle = handle.clone();
    let clone_session: Arc<dyn Fn() -> Result<String, String> + Send + Sync> =
        Arc::new(move || {
            let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
            let state = clone_state.clone();
            let task_id = clone_current.lock().unwrap().clone();
            clone_handle.spawn(async move {
                let outcome = task_clone(&state, &task_id)
                    .await
                    .map(|clone| format!("已克隆会话：{}（/resume 可打开）", clone.title));
                let _ = tx.send(outcome);
            });
            rx.recv_timeout(std::time::Duration::from_secs(30))
                .unwrap_or_else(|_| Err("克隆超时（30s）".to_string()))
        });

    // G10：/login 选择器快照（Codex CLI 可用性 + 登录态；宿主全局探测，无 state）。
    let open_login: Arc<dyn Fn() -> Option<r_code_tui::login_flow::LoginPicker> + Send + Sync> =
        Arc::new(move || {
            let status = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(codex_integration_status())
            })
            .ok()?;
            let available = status
                .get("cli_available")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let authenticated = status
                .get("authenticated")
                .and_then(|value| value.as_bool());
            let method = status
                .get("auth_method")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            Some(r_code_tui::login_flow::LoginPicker::new(
                available,
                authenticated,
                method,
            ))
        });

    // G10：启动 Codex 登录（宿主委托：新开系统终端窗口跑 OAuth，不读输出）。
    let login_start_tui = tui_state.clone();
    let login_start_handle = handle.clone();
    let start_login: r_code_tui::app::StartLoginOp = Arc::new(move |mode| {
        let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
        let mode = mode.to_string();
        let tui = login_start_tui.clone();
        login_start_handle.spawn(async move {
            let started = if mode == "device" {
                codex_start_device_login().await
            } else {
                codex_start_login().await
            };
            let outcome = started.map(|()| {
                "已在新终端窗口启动 Codex 登录——完成浏览器/设备码步骤后，这里会自动确认".to_string()
            });
            // 后台轮询确认（5s 间隔，最长 5 分钟；只读状态，不碰凭据）。
            if outcome.is_ok() {
                let tui = tui.clone();
                tokio::spawn(async move {
                    for _ in 0..60 {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        if let Ok(status) = codex_integration_status().await {
                            if status
                                .get("authenticated")
                                .and_then(|value| value.as_bool())
                                == Some(true)
                            {
                                if let Ok(mut st) = tui.lock() {
                                    st.push_system("Codex 登录完成 ✓（/model 可切换使用）");
                                }
                                return;
                            }
                        }
                    }
                });
            }
            let _ = tx.send(outcome);
        });
        rx.recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|_| Err("启动登录超时（60s）".to_string()))
    });

    // G10：刷新登录状态。
    let refresh_login: Arc<dyn Fn() -> Vec<String> + Send + Sync> = Arc::new(move || {
        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(codex_integration_status())
        }) {
            Ok(status) => {
                let available = status
                    .get("cli_available")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let line = if !available {
                    "未检测到 Codex CLI（ChatGPT 账号登录需要它；桌面设置页可安装）".to_string()
                } else {
                    match status
                        .get("authenticated")
                        .and_then(|value| value.as_bool())
                    {
                        Some(true) => format!(
                            "Codex 已登录（{}）",
                            status
                                .get("auth_method")
                                .and_then(|value| value.as_str())
                                .unwrap_or("方式未知")
                        ),
                        Some(false) => "Codex 未登录（/login 走浏览器或设备码登录）".to_string(),
                        None => "Codex 登录状态未知（稍后重试）".to_string(),
                    }
                };
                vec![
                    line,
                    "其余模型服务为 API key 鉴权：/setup（Tab 可切环境变量模式）".to_string(),
                ]
            }
            Err(error) => vec![format!("登录状态查询失败：{error}")],
        }
    });

    // G6：带图片附件发送（宿主附件管线：魔数校验 / OCR 转换 / 排队持久化）。
    let attach_state = state.clone();
    let attach_current = current_task.clone();
    let attach_tui = tui_state.clone();
    let attach_config = state.config_dir.clone();
    let attach_handle = handle.clone();
    let send_attachments: Arc<
        dyn Fn(String, Vec<r_code_tui::image_attach::PendingImage>, bool) + Send + Sync,
    > = Arc::new(move |text, images, queue| {
        use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
        use base64::Engine as _;
        use r_code_core::dto::AgentSendMode;
        let attachments: Vec<AttachmentInput> = images
            .iter()
            .map(|pending| AttachmentInput {
                name: pending.name.clone(),
                media_type: pending.media_type.to_string(),
                data: BASE64_STANDARD.encode(&pending.data),
                native_ocr: false,
            })
            .collect();
        let state = attach_state.clone();
        let task_id = attach_current.lock().unwrap().clone();
        let tui = attach_tui.clone();
        let config_dir = attach_config.clone();
        attach_handle.spawn(async move {
            let mode = if queue {
                AgentSendMode::Queue
            } else {
                AgentSendMode::Auto
            };
            if let Err(error) =
                agent_send_with_mode_and_attachments(&state, &task_id, &text, mode, &attachments)
                    .await
            {
                if let Ok(mut st) = tui.lock() {
                    st.push_system(format!(
                        "发送失败：{}",
                        r_code_tui::provider_error_guidance(&error, &config_dir)
                    ));
                }
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
        session_report,
        run_bang,
        open_resume,
        resume_session,
        new_session,
        rename_session,
        config_dir: state.config_dir.clone(),
        persist_default_model,
        compact_context,
        open_tree,
        switch_branch,
        open_fork,
        fork_send,
        clone_session,
        open_login,
        start_login,
        refresh_login,
        send_attachments,
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
    // G4：`auth check` 子命令（脚本/CI 用，pi `pi auth check` 对齐）——
    // 打印各 provider 认证状态；默认服务已认证 = exit 0，否则 exit 1。
    if args.first().map(String::as_str) == Some("auth") {
        let sub = args.get(1).map(String::as_str).unwrap_or("");
        if sub != "check" {
            eprintln!("r-code-tui: 未知 auth 子命令：{sub:?}（仅支持 auth check）");
            std::process::exit(2);
        }
        let data_root = args
            .iter()
            .position(|arg| arg == "--data-dir")
            .and_then(|index| args.get(index + 1))
            .map(std::path::PathBuf::from)
            .or_else(r_code_host::app_paths::default_data_dir);
        let config_dir = data_root
            .as_ref()
            .map(|root| root.join("config"))
            .unwrap_or_else(|| std::path::PathBuf::from("config"));
        let code = auth_check_report(&config_dir);
        std::process::exit(code);
    }
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
                r_code_tui::TranscriptRow::Image {
                    name,
                    width,
                    height,
                    ..
                } => println!(
                    "🖼 {}",
                    r_code_tui::image_attach::placeholder_line(name, *width, *height)
                ),
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
