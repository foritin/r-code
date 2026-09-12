//! r-code-tui 入口（pi-alignment PRD §4.1 R-TUI-01 / M8-01；T35 v2 化）。
//!
//! 会话引擎全部经共享 r-code-service 守护进程（v2 Harness 协议，
//! [`r_code_tui::engine::V2ChatClient`]）——不依赖 r-code-host/Tauri，也不
//! 再在进程内装配 agent runtime。默认 `--mode tui`（交互终端）；
//! `--mode print`（单轮后退出）与 `--mode json`（事件 JSONL 到 stdout）供
//! 脚本/管道消费。`--data-dir` 是 v2 数据根（与桌面 GUI 共享同一 profile
//! 的任务库，GUI 可 resume TUI 会话）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use r_code_tui::engine::{project_events, transcript_events_of, V2ChatClient};
use r_code_tui::TuiState;

struct TerminalSession {
    raw_mode: bool,
    bracketed_paste: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self, String> {
        let mut session = Self {
            raw_mode: false,
            bracketed_paste: false,
        };
        crossterm::terminal::enable_raw_mode()
            .map_err(|error| format!("enable raw mode: {error}"))?;
        session.raw_mode = true;
        crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste)
            .map_err(|error| format!("enable bracketed paste: {error}"))?;
        session.bracketed_paste = true;
        Ok(session)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.bracketed_paste {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
            self.bracketed_paste = false;
        }
        if self.raw_mode {
            let _ = crossterm::terminal::disable_raw_mode();
            self.raw_mode = false;
        }
    }
}

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
    "usage: r-code-tui [--mode tui|print|json] [--data-dir <path>] [--ipc-name <name>] [--message <text>] [--mock]\n\
       r-code-tui auth check [--data-dir <path>]\n\
     --data-dir 指定 v2 数据根（默认使用该 profile 的平台数据目录）\n\
     --ipc-name 守护进程端点名覆盖（默认随 profile；并行多实例/测试隔离用）\n\
     --mode print/json 单轮执行（经共享 r-code-service；无配置时输出引导并以 exit 2 退出）\n\
     --mock 仅限 --mode print|json：本地确定性回显（评估/演示线路，不连 daemon）\n\
     auth check 打印各模型服务认证状态（默认服务已认证 = exit 0，否则 exit 1）"
}

/// `auth check` 报告（G4，pi `pi auth check` 对齐；脚本/CI 消费）。
/// 口径与 /model 选择器同源：v2 SettingsStore 的 availability()
///（settings.json + 平台凭据感知，本地读取，无需 daemon）。
fn auth_check_report(profile_root: &std::path::Path) -> i32 {
    use r_code_runtime::services::settings_store::SettingsStore;
    let store = SettingsStore::new(profile_root.to_path_buf());
    let settings = store.load();
    let availability = store.availability();
    let default = settings.default_selection.clone().unwrap_or_default();
    println!("default provider: {default}");
    if availability.is_empty() {
        println!("no providers configured（/setup 或桌面设置页完成配置）");
        return 1;
    }
    for row in &availability {
        if row.has_credential {
            println!("ok      {} ({} models)", row.selection, 1);
        } else {
            println!(
                "no-auth {}（配置存在但缺鉴权，/setup 重配或检查凭据）",
                row.selection
            );
        }
    }
    let default_ok = availability
        .iter()
        .any(|row| row.selection == default && row.has_credential);
    if default_ok {
        0
    } else {
        1
    }
}

/// 解析结果：`(mode, data_dir, message, mock, profile, ipc_name)`。
type ParsedArgs = (
    String,
    Option<PathBuf>,
    Option<String>,
    bool,
    Option<String>,
    Option<String>,
);

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut mode = "tui".to_string();
    let mut data_dir: Option<PathBuf> = None;
    let mut message: Option<String> = None;
    let mut mock = false;
    let mut profile: Option<String> = None;
    let mut ipc_name: Option<String> = None;
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
            "--ipc-name" => {
                index += 1;
                ipc_name = Some(args.get(index).ok_or("--ipc-name 缺值")?.clone());
            }
            "--message" => {
                index += 1;
                message = Some(args.get(index).ok_or("--message 缺值")?.clone());
            }
            // 显式 v2 Profile（development|production；绝不从环境推断）。
            "--profile" => {
                index += 1;
                let value = args.get(index).ok_or("--profile 缺值")?.clone();
                if !matches!(
                    value.as_str(),
                    "development" | "dev" | "production" | "prod"
                ) {
                    return Err(format!("未知 --profile：{value}"));
                }
                profile = Some(value);
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
    Ok((mode, data_dir, message, mock, profile, ipc_name))
}

/// v2 数据根：显式 --data-dir 优先；缺省回落 profile 平台数据目录
///（与桌面 GUI 同根，任务库互通）。
fn resolve_data_root(data_dir: Option<&std::path::Path>) -> Result<PathBuf, String> {
    if let Some(root) = data_dir {
        return Ok(root.to_path_buf());
    }
    r_code_runtime::ProfileFlavor::Development
        .default_data_root()
        .ok_or_else(|| "无法解析默认数据根，请显式传 --data-dir".to_string())
}

/// footer 投影刷新：任务绑定优先，回落全局默认（启动 / resume / 分支切换
/// 共用）。v2 口径：task.detail 的 model/推理/模式。
async fn refresh_projection(engine: &V2ChatClient, tui: &Arc<Mutex<TuiState>>, task_id: &str) {
    let Ok(detail) = engine.task_detail(task_id).await else {
        return;
    };
    if let Ok(mut st) = tui.lock() {
        if let Some(model) = detail.model.as_deref() {
            // selection id 即 provider 标签；model 行由 settings 命名空间决定，
            // footer 展示 (selection) selection。
            st.set_model_selection(model.to_string(), model.to_string());
        }
        let effort = detail
            .inference
            .as_ref()
            .and_then(|value| value.get("reasoning_effort"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        st.set_thinking(effort);
        st.set_task_mode(detail.mode.clone().unwrap_or_else(|| "ask".to_string()));
    }
}

/// 装入某任务为当前会话（G8：/resume 接续、/new 切换、克隆/分叉后打开）：
/// 切 current_task 句柄 + journal 事件重放重建 transcript + 刷新 footer
/// 投影。事件重放会把共享游标推进到最新（实时泵不会重投旧事件）。
async fn adopt_task(
    engine: &V2ChatClient,
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
    let events = engine.events_from_start().await.unwrap_or_default();
    let transcript = transcript_events_of(&events, task_id);
    if let Ok(mut st) = tui.lock() {
        st.rebuild_from_events(&transcript);
        st.push_system(note);
    }
    refresh_projection(engine, tui, task_id).await;
}

/// 交互 TUI：进入 raw + bracketed paste；发送（运行中 = 排队）、Ctrl-C
/// 中止、Esc 退出。会话 = 守护进程上的一个 conversation 任务。
async fn run_interactive_tui(
    engine: V2ChatClient,
    tui_state: Arc<Mutex<TuiState>>,
    harness_profile: r_code_runtime::ProfileFlavor,
    harness_data_root: Option<std::path::PathBuf>,
    harness_ipc_name: Option<String>,
) -> Result<(), String> {
    use r_code_tui::app::{run_interactive, RunController};

    let profile_root = engine.profile().harness_v2_root();

    // M1-04：无配置首屏引导（本地 settings.json 快读；已配置时首屏不出现）。
    for line in r_code_tui::onboarding_lines(&profile_root) {
        if let Ok(mut st) = tui_state.lock() {
            st.push_system(line);
        }
    }

    // 会话 task：TUI 打开即一个会话，首个发送 = 新 run。
    let task_id = engine
        .ensure_session("tui")
        .await
        .map_err(|error| format!("创建会话失败：{error}"))?;
    // G8：当前任务句柄——/new /resume /clone 切换后更新；所有会话操作
    // 闭包在**调用时**读取（不再克隆固定值）。
    let current_task: Arc<Mutex<String>> = Arc::new(Mutex::new(task_id.clone()));
    tui_state.lock().unwrap().set_task_id(task_id.clone());

    // 初始模型投影（footer 右侧）：任务绑定优先，回落全局默认。
    refresh_projection(&engine, &tui_state, &task_id).await;

    let handle = tokio::runtime::Handle::current();

    // 事件泵：250ms 轮询 task.events → 投影进 TuiState（assistant 整轮文本/
    // 工具卡/run 态/失败引导）。连接失败静默重试（每 tick 新建连接）。
    {
        let pump_engine = engine.clone();
        let pump_tui = tui_state.clone();
        handle.spawn(async move {
            loop {
                if let Ok(events) = pump_engine.poll_events().await {
                    if !events.is_empty() {
                        if let Ok(mut st) = pump_tui.lock() {
                            project_events(&mut st, &events);
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        });
    }

    // M1-03：交互路径错误一律进 transcript（alt-screen 下 eprintln 不可见）。
    let send_engine = engine.clone();
    let send_current = current_task.clone();
    let send_tui = tui_state.clone();
    let send_root = profile_root.clone();
    let send: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text| {
        let engine = send_engine.clone();
        let task_id = send_current.lock().unwrap().clone();
        let tui = send_tui.clone();
        let root = send_root.clone();
        tokio::runtime::Handle::current().spawn(async move {
            // 守护进程语义：空闲 = 启动新 run；运行中 = 自动排队（run 结束
            // 后派发）。
            if let Err(error) = engine.send(&task_id, &text).await {
                if let Ok(mut st) = tui.lock() {
                    st.push_system(format!(
                        "发送失败：{}",
                        r_code_tui::provider_error_guidance(&error, &root)
                    ));
                }
            }
        });
    });

    let abort_engine = engine.clone();
    let abort_current = current_task.clone();
    let abort_tui = tui_state.clone();
    let abort: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let engine = abort_engine.clone();
        let task_id = abort_current.lock().unwrap().clone();
        let tui = abort_tui.clone();
        tokio::runtime::Handle::current().spawn(async move {
            if let Err(error) = engine.abort(&task_id).await {
                if let Ok(mut st) = tui.lock() {
                    st.push_system(format!("中止失败：{error}"));
                }
            }
        });
    });

    // M2-01：/model 选择器数据源（models.available；不可用的仍列出但标注）。
    let picker_engine = engine.clone();
    let picker_tui = tui_state.clone();
    let open_model_picker: Arc<
        dyn Fn() -> Option<r_code_tui::model_selector::ModelPicker> + Send + Sync,
    > = Arc::new(move || {
        let engine = picker_engine.clone();
        let rows = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(engine.models_available())
        })
        .unwrap_or_default();
        let entries = r_code_tui::model_selector::picker_entries(&rows);
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

    let select_engine = engine.clone();
    let select_current = current_task.clone();
    let select_tui = tui_state.clone();
    let select_model: Arc<dyn Fn(r_code_tui::model_selector::ModelEntry) + Send + Sync> =
        Arc::new(move |entry| {
            let engine = select_engine.clone();
            let task_id = select_current.lock().unwrap().clone();
            let tui = select_tui.clone();
            tokio::runtime::Handle::current().spawn(async move {
                match r_code_tui::model_selector::apply_model_selection(&engine, &task_id, &entry)
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
    let think_engine = engine.clone();
    let think_current = current_task.clone();
    let think_tui = tui_state.clone();
    let set_thinking: Arc<dyn Fn(&'static str) + Send + Sync> = Arc::new(move |level| {
        let engine = think_engine.clone();
        let task_id = think_current.lock().unwrap().clone();
        let tui = think_tui.clone();
        tokio::runtime::Handle::current().spawn(async move {
            match r_code_tui::thinking::apply_thinking(&engine, &task_id, level).await {
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

    // M2-03：Shift+Tab 模式循环写回（task.setPreferences；影响下一次 run）。
    let mode_engine = engine.clone();
    let mode_current = current_task.clone();
    let mode_tui = tui_state.clone();
    let set_mode: Arc<dyn Fn(&'static str) + Send + Sync> = Arc::new(move |mode| {
        let engine = mode_engine.clone();
        let task_id = mode_current.lock().unwrap().clone();
        let tui = mode_tui.clone();
        tokio::runtime::Handle::current().spawn(async move {
            match r_code_tui::task_mode::apply_mode(&engine, &task_id, mode).await {
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

    // M2-04：运行中排队（守护进程同一 send 语义：运行中自动入队）。
    let queue_engine = engine.clone();
    let queue_current = current_task.clone();
    let queue_tui = tui_state.clone();
    let queue_send: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text| {
        let engine = queue_engine.clone();
        let task_id = queue_current.lock().unwrap().clone();
        let tui = queue_tui.clone();
        tokio::runtime::Handle::current().spawn(async move {
            if let Err(error) = engine.send(&task_id, &text).await {
                if let Ok(mut st) = tui.lock() {
                    st.push_system(format!("排队失败：{error}"));
                }
            }
        });
    });

    // M2-05 / RA3：审批决策。daemon 审批（approval.requested 投影出的
    // 浮层）经 approvals.decide 落账；本地 PermissionEngine 流（op_id
    // 为空）维持既有语义——意图消费在宿主侧，这里只处理 daemon 来源。
    let decide_engine = engine.clone();
    let decide_tui = tui_state.clone();
    let decide_approval: Arc<dyn Fn(r_code_tui::approval::ApprovalDecision) + Send + Sync> =
        Arc::new(move |decision| {
            let engine = decide_engine.clone();
            let tui = decide_tui.clone();
            tokio::runtime::Handle::current().spawn(async move {
                let pending = tui
                    .lock()
                    .map(|st| st.pending_approval().cloned())
                    .ok()
                    .flatten();
                let Some(pending) = pending.filter(|p| p.is_daemon()) else {
                    return;
                };
                let Some(op_id) = pending.op_id else {
                    return;
                };
                let approve = !matches!(decision, r_code_tui::approval::ApprovalDecision::Deny);
                if let Err(error) = engine.decide_approval(&op_id, approve).await {
                    if let Ok(mut st) = tui.lock() {
                        st.push_system(format!("审批决策失败：{error}"));
                    }
                }
                // decided 事件到达时浮层由 project_events 收起；这里先收
                // 起避免事件延迟期间重复决策。
                if let Ok(mut st) = tui.lock() {
                    st.take_pending_approval();
                }
            });
        });

    // M3-01：用量刷新泵（task.detail.usage 聚合投影；resume 后仍准确）。
    {
        let refresh_engine = engine.clone();
        let refresh_current = current_task.clone();
        let refresh_tui = tui_state.clone();
        handle.spawn(async move {
            loop {
                let task_id = refresh_current.lock().unwrap().clone();
                if let Ok(detail) = refresh_engine.task_detail(&task_id).await {
                    let stats = r_code_tui::status_bar::UsageStats {
                        input_tokens: detail.usage.input_tokens,
                        output_tokens: detail.usage.output_tokens,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        runs: detail.runs.len(),
                    };
                    if let Ok(mut st) = refresh_tui.lock() {
                        st.set_usage(stats);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }

    // M3-02：/status 与 /usage 数据装配（模型标签 + 目录缩写 + 用量）。
    let status_engine = engine.clone();
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
        let task_id = status_current.lock().unwrap().clone();
        let Ok(detail) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(status_engine.task_detail(&task_id))
        }) else {
            return (Vec::new(), "状态不可用".to_string());
        };
        let stats = r_code_tui::status_bar::UsageStats {
            input_tokens: detail.usage.input_tokens,
            output_tokens: detail.usage.output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            runs: detail.runs.len(),
        };
        let card =
            r_code_tui::status_bar::status_card_lines(&model_label, &directory, &stats, None);
        let summary = r_code_tui::status_bar::usage_summary(&stats, None);
        (card, summary)
    });

    // G9：/session 会话统计卡装配（TuiState 消息计数 + task.detail 会话维度 +
    // v2 数据库路径——无 JSONL 会话文件）。
    let session_card_engine = engine.clone();
    let session_card_current = current_task.clone();
    let session_card_tui = tui_state.clone();
    let session_report: Arc<dyn Fn() -> Vec<String> + Send + Sync> = Arc::new(move || {
        let task_id = session_card_current.lock().unwrap().clone();
        // rows 与 model_selection 独立收集：无模型投影时消息计数仍要真实。
        let (rows, model_label) = session_card_tui
            .lock()
            .ok()
            .map(|st| {
                let model_label = st.model_selection().map(|(provider, model)| {
                    r_code_tui::model_selector::model_label(provider, model)
                });
                (st.rows().to_vec(), model_label)
            })
            .unwrap_or_default();
        let model_label = model_label.unwrap_or_else(|| "未选择".to_string());
        let Ok(detail) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(session_card_engine.task_detail(&task_id))
        }) else {
            return Vec::new();
        };
        let stats = r_code_tui::status_bar::UsageStats {
            input_tokens: detail.usage.input_tokens,
            output_tokens: detail.usage.output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            runs: detail.runs.len(),
        };
        // 无 JSONL 会话文件：runs 为 0 显示"未落盘"，否则显示 v2 数据库路径。
        let session_file = (!detail.runs.is_empty()).then(|| {
            session_card_engine
                .profile()
                .database_path()
                .display()
                .to_string()
        });
        let input = r_code_tui::status_bar::SessionCardInput {
            task_id: detail.task_id.clone(),
            title: detail.title.clone(),
            model_label,
            created_at: String::new(),
            messages: r_code_tui::status_bar::count_messages(&rows),
            runs: detail.runs.len(),
            stats,
            cost: None,
            session_file,
        };
        r_code_tui::status_bar::session_card_lines(&input)
    });

    // M4-04：! 直通（本地 shell 链；cwd = 当前目录）。
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

    // M6-01/M6-02：会话操作（task.list 列表、resume 接管、new/rename）。
    let resume_engine = engine.clone();
    let open_resume: Arc<
        dyn Fn() -> Option<r_code_tui::session_picker::SessionPicker> + Send + Sync,
    > = Arc::new(move || {
        let engine = resume_engine.clone();
        let tasks = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(engine.list_sessions())
        })
        .unwrap_or_default();
        let entries: Vec<r_code_tui::session_picker::SessionEntry> = tasks
            .iter()
            .map(r_code_tui::session_picker::entry_from_summary)
            .collect();
        if entries.is_empty() {
            None
        } else {
            Some(r_code_tui::session_picker::SessionPicker::new(entries))
        }
    });

    let resume_session_engine = engine.clone();
    let resume_session_tui = tui_state.clone();
    let resume_session_current = current_task.clone();
    let resume_session: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |task_id| {
        let engine = resume_session_engine.clone();
        let tui = resume_session_tui.clone();
        let current = resume_session_current.clone();
        tokio::runtime::Handle::current().spawn(async move {
            match engine.task_detail(&task_id).await {
                Ok(detail) => {
                    // G8：真正接续——切换当前任务 + journal 重放重建 transcript。
                    adopt_task(
                        &engine,
                        &tui,
                        &current,
                        &task_id,
                        format!("已接续会话：{}", detail.title),
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

    let new_session_engine = engine.clone();
    let new_session_tui = tui_state.clone();
    let new_session_current = current_task.clone();
    let new_session: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let engine = new_session_engine.clone();
        let tui = new_session_tui.clone();
        let current = new_session_current.clone();
        tokio::runtime::Handle::current().spawn(async move {
            match r_code_tui::session_ops::new_session(&engine).await {
                Ok(new_id) => {
                    // G8：/new 真正切换到新会话（发送进新任务）。
                    adopt_task(
                        &engine,
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

    let rename_session_engine = engine.clone();
    let rename_session_current = current_task.clone();
    let rename_session_tui = tui_state.clone();
    let rename_session: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |title| {
        let engine = rename_session_engine.clone();
        let task_id = rename_session_current.lock().unwrap().clone();
        let tui = rename_session_tui.clone();
        tokio::runtime::Handle::current().spawn(async move {
            match r_code_tui::session_ops::rename_session(&engine, &task_id, &title).await {
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

    // G2：模型选择器 Ctrl+S —— 选择并持久为全局默认（settings.setDefault）。
    let persist_engine = engine.clone();
    let persist_tui = tui_state.clone();
    let persist_default_model: Arc<dyn Fn(r_code_tui::model_selector::ModelEntry) + Send + Sync> =
        Arc::new(move |entry| {
            let engine = persist_engine.clone();
            let tui = persist_tui.clone();
            tokio::runtime::Handle::current().spawn(async move {
                match engine.set_default_model(&entry.provider).await {
                    Ok(()) => {
                        if let Ok(mut st) = tui.lock() {
                            st.push_system(format!(
                                "已设为全局默认：({}) {}（新会话沿用）",
                                entry.provider, entry.model
                            ));
                        }
                    }
                    Err(error) => {
                        if let Ok(mut st) = tui.lock() {
                            st.push_system(format!("持久化默认模型失败：{error}"));
                        }
                    }
                }
            });
        });

    // G5：/compact [prompt] —— v2 首版诚实降级（压缩能力未上，提示不报错）。
    let compact_context: Arc<dyn Fn(Option<String>) -> Result<String, String> + Send + Sync> =
        Arc::new(move |_focus| Ok(r_code_tui::session_ops::compact_unavailable_note().to_string()));

    // G8：/tree 分支树（task.branches + 当前任务 = main 分支）。
    let tree_engine = engine.clone();
    let tree_current = current_task.clone();
    let open_tree: Arc<dyn Fn() -> Option<r_code_tui::session_tree::BranchTree> + Send + Sync> =
        Arc::new(move || {
            let engine = tree_engine.clone();
            let task_id = tree_current.lock().unwrap().clone();
            let branches = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(engine.branches())
            })
            .unwrap_or_default();
            // main-only 树：当前任务自身 = main 分支（活跃）；branch.created
            // 派生的子任务（parent 指向当前）为子分支。
            let mut infos = vec![r_code_tui::session_tree::BranchInfo {
                id: "main".to_string(),
                parent_id: None,
                is_active: true,
                created: chrono::Local::now().format("%m-%d %H:%M").to_string(),
            }];
            for row in &branches {
                if row.parent_task_id.as_deref() == Some(task_id.as_str()) {
                    infos.push(r_code_tui::session_tree::BranchInfo {
                        id: row.task_id.clone(),
                        parent_id: Some("main".to_string()),
                        is_active: false,
                        created: chrono::Local::now().format("%m-%d %H:%M").to_string(),
                    });
                }
            }
            Some(r_code_tui::session_tree::BranchTree::new(&infos))
        });

    // G8：切换分支（main = 幂等；子分支 = 接管对应任务并重建 transcript）。
    let switch_engine = engine.clone();
    let switch_current = current_task.clone();
    let switch_tui = tui_state.clone();
    let switch_branch: Arc<dyn Fn(String) -> Result<String, String> + Send + Sync> =
        Arc::new(move |branch_id| {
            let engine = switch_engine.clone();
            let tui = switch_tui.clone();
            let current = switch_current.clone();
            let branch_id = branch_id.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    if branch_id == "main" {
                        // 切回 main = 幂等（当前会话即 main）。
                        return Ok("已切换到分支 main".to_string());
                    }
                    // 子分支存在性校验（不存在时守护进程报错）。
                    engine.task_detail(&branch_id).await?;
                    let line = format!(
                        "已切换到分支 {}",
                        r_code_tui::session_tree::short_id(&branch_id)
                    );
                    adopt_task(&engine, &tui, &current, &branch_id, line.clone()).await;
                    Ok(line)
                })
            })
            // block_in_place：同步等待不冻结其余 runtime 任务（事件泵/用量泵）。
        });

    // G8：/fork 消息选择器（journal input.queued 的 user 消息投影）。
    let fork_engine = engine.clone();
    let fork_current = current_task.clone();
    let open_fork: Arc<dyn Fn() -> Option<r_code_tui::session_tree::ForkPicker> + Send + Sync> =
        Arc::new(move || {
            let engine = fork_engine.clone();
            let task_id = fork_current.lock().unwrap().clone();
            let events = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(engine.events_from_start())
            })
            .unwrap_or_default();
            let entries =
                r_code_tui::session_tree::fork_entries(&transcript_events_of(&events, &task_id));
            if entries.is_empty() {
                None
            } else {
                Some(r_code_tui::session_tree::ForkPicker::new(entries))
            }
        });

    // G8：分叉重发（v2 语义 = 新分支任务 + 改写文本发进去；原任务保留）。
    let fork_send_engine = engine.clone();
    let fork_send_current = current_task.clone();
    let fork_send_tui = tui_state.clone();
    let fork_send: Arc<dyn Fn(String, String) -> Result<String, String> + Send + Sync> =
        Arc::new(move |message_id, text| {
            let engine = fork_send_engine.clone();
            let current = fork_send_current.clone();
            let tui = fork_send_tui.clone();
            let note = "已从消息分叉并重发（原分支保留，/tree 可切回）".to_string();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let task_id = current.lock().unwrap().clone();
                    let title = engine
                        .task_detail(&task_id)
                        .await
                        .map(|detail| detail.title)
                        .unwrap_or_else(|_| "tui".to_string());
                    let branch = engine
                        .clone_task(&task_id, &format!("{title}（分叉）"))
                        .await?;
                    engine.send(&branch, &text).await?;
                    adopt_task(&engine, &tui, &current, &branch, note).await;
                    let _ = message_id;
                    Ok("已从消息分叉并重发（原分支保留，/tree 可切回）".to_string())
                })
            })
            // block_in_place：同步等待不冻结其余 runtime 任务（事件泵/用量泵）。
        });

    // G8：/clone 克隆当前会话（新分支任务承载；留在当前会话）。
    let clone_engine = engine.clone();
    let clone_current = current_task.clone();
    let clone_session: Arc<dyn Fn() -> Result<String, String> + Send + Sync> =
        Arc::new(move || {
            let engine = clone_engine.clone();
            let task_id = clone_current.lock().unwrap().clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let title = engine
                        .task_detail(&task_id)
                        .await
                        .map(|detail| detail.title)
                        .unwrap_or_else(|_| "tui".to_string());
                    let new_title = format!("{title}（克隆）");
                    engine.clone_task(&task_id, &new_title).await?;
                    Ok(format!("已克隆会话：{new_title}（/resume 可打开）"))
                })
            })
            // block_in_place：同步等待不冻结其余 runtime 任务（事件泵/用量泵）。
        });

    // G10：/login 选择器快照（Codex CLI 可用性 + 登录态；经守护进程）。
    let login_engine = engine.clone();
    let open_login: Arc<dyn Fn() -> Option<r_code_tui::login_flow::LoginPicker> + Send + Sync> =
        Arc::new(move || {
            let engine = login_engine.clone();
            let status = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(engine.codex_status())
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

    // G10：启动 Codex 登录（守护进程委托：新开系统终端窗口跑 OAuth）。
    let login_start_engine = engine.clone();
    let login_start_tui = tui_state.clone();
    let start_login: r_code_tui::app::StartLoginOp = Arc::new(move |mode| {
        let engine = login_start_engine.clone();
        let tui = login_start_tui.clone();
        let mode = mode.to_string();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                engine
                    .codex_start_login(&mode)
                    .await
                    .map(|()| {
                        "已在新终端窗口启动 Codex 登录——完成浏览器/设备码步骤后，这里会自动确认"
                            .to_string()
                    })
                    .inspect(|_line| {
                        // 后台轮询确认（5s 间隔，最长 5 分钟；只读状态）。
                        let engine = engine.clone();
                        let tui = tui.clone();
                        tokio::runtime::Handle::current().spawn(async move {
                            for _ in 0..60 {
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                if let Ok(status) = engine.codex_status().await {
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
                    })
            })
        })
    });

    // G10：刷新登录状态。
    let refresh_login_engine = engine.clone();
    let refresh_login: Arc<dyn Fn() -> Vec<String> + Send + Sync> = Arc::new(move || {
        let engine = refresh_login_engine.clone();
        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(engine.codex_status())
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

    // G6：带图片附件发送——v2 首版不接图片，诚实降级（保留附件 UI 入口）。
    let attach_tui = tui_state.clone();
    let send_attachments: Arc<
        dyn Fn(String, Vec<r_code_tui::image_attach::PendingImage>, bool) + Send + Sync,
    > = Arc::new(move |text, _images, _queue| {
        let tui = attach_tui.clone();
        tokio::runtime::Handle::current().spawn(async move {
            if let Ok(mut st) = tui.lock() {
                st.push_system(format!("已收到「{text}」——图片附件将在 v2 后续版本支持"));
            }
        });
    });

    // /plugins 子命令：v2 桥不可用时置 None（TUI 主体不受影响）。
    let harness_tui = tui_state.clone();
    let run_harness_command: Option<Arc<dyn Fn(String) + Send + Sync>> =
        r_code_tui::harness_client::HarnessTuiClient::from_args(
            harness_profile,
            harness_data_root.clone(),
            harness_ipc_name.clone(),
        )
        .ok()
        .map(|client| {
            Arc::new(move |arg: String| {
                let client = client.clone();
                let tui = harness_tui.clone();
                tokio::runtime::Handle::current().spawn(async move {
                    let outcome = client.execute(&arg).await;
                    if let Ok(mut st) = tui.lock() {
                        for line in outcome.lines {
                            st.push_system(line);
                        }
                    }
                });
            }) as Arc<dyn Fn(String) + Send + Sync>
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
        run_harness_command,
        open_resume,
        resume_session,
        new_session,
        rename_session,
        config_dir: profile_root,
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
    let _terminal_session = TerminalSession::enter()?;
    run_interactive(tui_state, controller).await?;
    Ok(())
}

/// 非交互 print/json：单轮经共享服务执行后退出。
///
/// 事件轮询至 run.completed/failed/cancelled（超时 120s），从 TuiState rows
/// 输出（json = JSONL 行；print = 现有行渲染）。run.cancelled → 输出后
/// exit 130；run.failed → 输出后 exit 1；超时 → exit 1。
async fn run_print_mode(
    engine: &V2ChatClient,
    tui_state: &Arc<Mutex<TuiState>>,
    message: &str,
    json: bool,
) -> ! {
    let task_id = match engine.create_session("tui").await {
        Ok(task_id) => task_id,
        Err(error) => {
            eprintln!("r-code-tui: 创建会话失败：{error}");
            std::process::exit(1);
        }
    };
    tui_state.lock().unwrap().set_task_id(task_id.clone());
    tui_state.lock().unwrap().push_user(message);
    if let Err(error) = engine.send(&task_id, message).await {
        eprintln!(
            "r-code-tui: 发送失败：{}",
            r_code_tui::provider_error_guidance(&error, &engine.profile().harness_v2_root(),)
        );
        std::process::exit(1);
    }
    let mut outcome: Option<&'static str> = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while outcome.is_none() {
        if std::time::Instant::now() >= deadline {
            eprintln!("r-code-tui: 等待回复超时（120s）");
            std::process::exit(1);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let events = match engine.poll_events().await {
            Ok(events) => events,
            Err(error) => {
                eprintln!("r-code-tui: 读取事件失败：{error}");
                std::process::exit(1);
            }
        };
        let settled = events.iter().any(|event| {
            matches!(
                event.payload.get("journalKind").and_then(|v| v.as_str()),
                Some("run.completed") | Some("run.failed") | Some("run.cancelled")
            ) && event.task_id == task_id
        });
        if let Ok(mut st) = tui_state.lock() {
            project_events(&mut st, &events);
        }
        if settled {
            outcome = Some("settled");
        }
    }
    let mut final_state = tui_state.lock().unwrap();
    final_state.flush_streaming();
    let rows = final_state.rows().to_vec();
    let failed = rows.iter().any(|row| {
        matches!(row, r_code_tui::TranscriptRow::System { text } if text.contains("发送失败"))
    });
    if json {
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
    if failed {
        // 真实失败路径（无 provider 等）：输出已含引导行，非零退出。
        std::process::exit(1);
    }
    std::process::exit(0);
}

#[tokio::main]
async fn main() {
    ensure_utf8_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // G4：`auth check` 子命令（脚本/CI 用，pi `pi auth check` 对齐）——
    // 打印各模型服务认证状态；默认服务已认证 = exit 0，否则 exit 1。
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
            .or_else(|| r_code_runtime::ProfileFlavor::Development.default_data_root());
        let profile_root = data_root
            .as_ref()
            .map(|root| root.join("harness-v2"))
            .unwrap_or_else(|| std::path::PathBuf::from("harness-v2"));
        let code = auth_check_report(&profile_root);
        std::process::exit(code);
    }
    let (mode, data_dir, message, mock, profile, ipc_name) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("r-code-tui: {error}\n{}", usage());
            std::process::exit(2);
        }
    };

    // 显式 v2 Profile（脚本传 --profile；缺省 development 开发工具链）。
    let harness_flavor = match profile.as_deref() {
        Some("production") | Some("prod") => r_code_runtime::ProfileFlavor::Production,
        _ => r_code_runtime::ProfileFlavor::Development,
    };
    let data_root = match resolve_data_root(data_dir.as_deref()) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("r-code-tui: {error}");
            std::process::exit(1);
        }
    };

    // --mock 评估线路：本地确定性回显，不连 daemon（红线 R1/R2：不冒充
    // 真实链路；真实模式必须走共享服务）。
    if mock {
        let Some(message) = message else {
            eprintln!("r-code-tui: {} 模式需要 --message", mode);
            std::process::exit(2);
        };
        let mut state = TuiState::new();
        state.push_user(message.clone());
        state.push_system(format!("[r-code-tui] 已收到：{message}"));
        let rows = state.rows().to_vec();
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
                    r_code_tui::TranscriptRow::System { text } => println!("· {text}"),
                    _ => {}
                }
            }
        }
        return;
    }

    // v2 引擎：ensure_daemon 自举共享 r-code-service（真进程、真 ModelBroker）。
    let engine =
        match V2ChatClient::from_args(harness_flavor, Some(data_root.clone()), ipc_name.clone()) {
            Ok(engine) => engine,
            Err(error) => {
                eprintln!("r-code-tui: {error}");
                std::process::exit(1);
            }
        };

    let tui_state = Arc::new(Mutex::new(TuiState::new()));

    if mode == "tui" {
        if let Err(error) = run_interactive_tui(
            engine,
            tui_state,
            harness_flavor,
            Some(data_root.clone()),
            ipc_name.clone(),
        )
        .await
        {
            eprintln!("r-code-tui: {error}");
            std::process::exit(1);
        }
        return;
    }

    // 非交互（脚本/管道）：单轮执行后退出。
    let Some(message) = message else {
        eprintln!("r-code-tui: {} 模式需要 --message", mode);
        std::process::exit(2);
    };

    // R2 显式引导：无 provider 配置时报出可操作途径并 exit 2，
    // 不进入任何演示回放。
    let profile_root = engine.profile().harness_v2_root();
    if let Some(guidance) = r_code_tui::provider_config_guidance(&profile_root) {
        eprintln!("r-code-tui: {guidance}");
        std::process::exit(2);
    }

    run_print_mode(&engine, &tui_state, &message, mode == "json").await;
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
        let parsed = parse_args(&arg_list(&["--mode", "print", "--message", "x"])).expect("parse");
        assert!(!parsed.3, "print without --mock must be real mode");
    }
}
