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
    "usage: r-code-tui [--mode tui|print|json] [--data-dir <path>] [--message <text>]\n\
     --mode print/json 需要预装脚本化场景（评估/演示线路）或真实 provider 配置"
}

fn parse_args(args: &[String]) -> Result<(String, Option<PathBuf>, Option<String>), String> {
    let mut mode = "tui".to_string();
    let mut data_dir: Option<PathBuf> = None;
    let mut message: Option<String> = None;
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
            other => return Err(format!("未知参数：{other}")),
        }
        index += 1;
    }
    Ok((mode, data_dir, message))
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

    let send_state = state.clone();
    let send_task = task_id.clone();
    let send_handle = handle.clone();
    let send: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text| {
        let state = send_state.clone();
        let task_id = send_task.clone();
        send_handle.spawn(async move {
            // agent_send 的 Auto 语义：运行中自动 steer，空闲时启动新 run。
            if let Err(error) = agent_send(&state, &task_id, &text).await {
                eprintln!("r-code-tui: 发送失败：{error}");
            }
        });
    });

    let abort_state = state.clone();
    let abort_task = task_id.clone();
    let abort_handle = handle.clone();
    let abort: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let state = abort_state.clone();
        let task_id = abort_task.clone();
        abort_handle.spawn(async move {
            if let Err(error) = agent_abort(&state, &task_id).await {
                eprintln!("r-code-tui: 中止失败：{error}");
            }
        });
    });

    let controller = RunController { send, abort };

    crossterm::terminal::enable_raw_mode().expect("raw mode");
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
    )
    .expect("enter alt screen");
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let terminal = ratatui::Terminal::new(backend).expect("terminal");

    run_interactive(terminal, tui_state, controller).await;

    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::cursor::Show,
        crossterm::terminal::LeaveAlternateScreen
    )
    .expect("leave alt screen");
    crossterm::terminal::disable_raw_mode().expect("disable raw mode");
}

#[tokio::main]
async fn main() {
    ensure_utf8_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, data_dir, message) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("r-code-tui: {error}\n{}", usage());
            std::process::exit(2);
        }
    };

    let state = Arc::new(match shared_state(data_dir.as_deref()) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("r-code-tui: {error}");
            std::process::exit(1);
        }
    });

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
    // 脚本化场景（无 provider 的确定性演示线路；真实 provider 配置存在时
    // 同样可跑——场景注入仅 Mock runtime 接受）。
    let reply = format!("[r-code-tui] 已收到：{message}");
    if !install_mock_scenario(&state, &task.id, constant_reply(&reply))
        .await
        .unwrap_or(false)
    {
        // 真实 runtime：直接发送（provider 配置来自共享 data-dir）。
    }
    agent_send(&state, &task.id, &message)
        .await
        .expect("agent_send");
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
