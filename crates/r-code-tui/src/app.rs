//! ratatui 渲染循环（R-TUI-01 阶段 1：消息流 + 流式 assistant + 工具卡折叠 +
//! 输入 + 发送/steer/abort）。
//!
//! 渲染只消费 `TuiState`（snapshot 权威：不在此累积领域状态副本），输入动作
//! 经 `input` 模块归一。滚动 = turn 级窗口化（`window` 模块）+ 视口偏移；
//! 状态栏展示运行态与提示键位。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event};

use crate::input::{map_key, InputBuffer, KeyAction};
use crate::model_selector::ModelPicker;
use crate::thinking::ThinkingPicker;
use crate::TuiState;

/// 底部插入式浮层（同一时刻至多一层；模型/思考/会话选择器/引导式配置）。
pub enum Overlay {
    Model(ModelPicker),
    Thinking(ThinkingPicker),
    Resume(crate::session_picker::SessionPicker),
    Setup(crate::setup_flow::SetupFlow),
    /// G8：/tree 分支树导航。
    Tree(crate::session_tree::BranchTree),
    /// G8：/fork 消息级分叉选择器。
    Fork(crate::session_tree::ForkPicker),
    /// G10：/login 账号登录。
    Login(crate::login_flow::LoginPicker),
}

/// G10：启动登录回调类型（"browser"/"device" → 状态行或错误）。
pub type StartLoginOp = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// 交互循环的宿主回调（发送/steer/abort 的真实语义由 main.rs 装配）。
#[derive(Clone)]
pub struct RunController {
    /// 发送/steer：`agent_send`（Auto 语义，运行中自动 steer）。
    pub send: Arc<dyn Fn(String) + Send + Sync>,
    /// 中止当前运行。
    pub abort: Arc<dyn Fn() + Send + Sync>,
    /// 打开 /model 选择器（可用集为空时返回 None）。
    pub open_model_picker: Arc<dyn Fn() -> Option<ModelPicker> + Send + Sync>,
    /// 选中模型写回（task_set_provider + task_set_model + footer 联动）。
    pub select_model: Arc<dyn Fn(crate::model_selector::ModelEntry) + Send + Sync>,
    /// 思考档位写回（task_set_inference + footer 联动；升降与弹层共用）。
    pub set_thinking: Arc<dyn Fn(&'static str) + Send + Sync>,
    /// 任务模式写回（Shift+Tab 循环）。
    pub set_mode: Arc<dyn Fn(&'static str) + Send + Sync>,
    /// 运行中排队发送（宿主 AgentSendMode::Queue）。
    pub queue_send: Arc<dyn Fn(String) + Send + Sync>,
    /// 审批决策落账（y/a/esc 三键契约；经宿主 PermissionEngine）。
    pub decide_approval: Arc<dyn Fn(crate::approval::ApprovalDecision) + Send + Sync>,
    /// /status 与 /usage 的数据装配（卡行 + 汇总行）。
    pub status_report: Arc<dyn Fn() -> (Vec<String>, String) + Send + Sync>,
    /// /session 会话统计卡装配（G9；ID/消息数/token/成本/会话文件行）。
    pub session_report: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// !command 直通执行（宿主 shell 链；输出进 Shell 行）。
    pub run_bang: Arc<dyn Fn(String) + Send + Sync>,
    /// 打开 /resume 列表（无会话时 None）。
    pub open_resume: Arc<dyn Fn() -> Option<crate::session_picker::SessionPicker> + Send + Sync>,
    /// 接续会话（task_id；JSONL 重建 transcript）。
    pub resume_session: Arc<dyn Fn(String) + Send + Sync>,
    /// 新建会话（/new）。
    pub new_session: Arc<dyn Fn() + Send + Sync>,
    /// 重命名会话（/rename <title>）。
    pub rename_session: Arc<dyn Fn(String) + Send + Sync>,
    /// 配置目录（/setup 引导流写回 config 与平台凭据）。
    pub config_dir: std::path::PathBuf,
    /// 模型选择器 Ctrl+S：选择并持久为全局默认（G2；写 config）。
    pub persist_default_model: Arc<dyn Fn(crate::model_selector::ModelEntry) + Send + Sync>,
    /// /compact [prompt]：显式压缩当前会话上下文（G5；宿主
    /// task_compact_context，focus=自定义指令）。返回状态行或错误。
    pub compact_context: Arc<dyn Fn(Option<String>) -> Result<String, String> + Send + Sync>,
    /// G8：/tree 打开分支树（None = 宿主侧不可用）。
    pub open_tree: Arc<dyn Fn() -> Option<crate::session_tree::BranchTree> + Send + Sync>,
    /// G8：切换活跃分支（同步；成功后 transcript 已由壳层重建，返回状态行）。
    pub switch_branch: Arc<dyn Fn(String) -> Result<String, String> + Send + Sync>,
    /// G8：/fork 打开 user 消息选择器（None = 还没有可分叉的消息）。
    pub open_fork: Arc<dyn Fn() -> Option<crate::session_tree::ForkPicker> + Send + Sync>,
    /// G8：从历史消息分叉重发（message_id + 改写文本 → agent_resend）。
    pub fork_send: Arc<dyn Fn(String, String) -> Result<String, String> + Send + Sync>,
    /// G8：克隆当前会话为新任务（返回状态行）。
    pub clone_session: Arc<dyn Fn() -> Result<String, String> + Send + Sync>,
    /// G10：/login 选择器快照（Codex 状态 + 选项）。
    pub open_login: Arc<dyn Fn() -> Option<crate::login_flow::LoginPicker> + Send + Sync>,
    /// G10：启动登录（"browser" / "device"；Codex 委托，新开系统终端）。
    pub start_login: StartLoginOp,
    /// G10：刷新登录状态（返回 transcript 行）。
    pub refresh_login: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// G6：带图片附件发送（文本， 图片， 运行中=排队）。
    pub send_attachments:
        Arc<dyn Fn(String, Vec<crate::image_attach::PendingImage>, bool) + Send + Sync>,
}

impl Default for RunController {
    fn default() -> Self {
        Self {
            send: Arc::new(|_| {}),
            abort: Arc::new(|| {}),
            open_model_picker: Arc::new(|| None),
            select_model: Arc::new(|_| {}),
            set_thinking: Arc::new(|_| {}),
            set_mode: Arc::new(|_| {}),
            queue_send: Arc::new(|_| {}),
            decide_approval: Arc::new(|_| {}),
            status_report: Arc::new(|| (Vec::new(), String::new())),
            session_report: Arc::new(Vec::new),
            run_bang: Arc::new(|_| {}),
            open_resume: Arc::new(|| None),
            resume_session: Arc::new(|_| {}),
            new_session: Arc::new(|| {}),
            rename_session: Arc::new(|_| {}),
            config_dir: std::path::PathBuf::new(),
            persist_default_model: Arc::new(|_| {}),
            compact_context: Arc::new(|_| Err("未装配压缩入口".to_string())),
            open_tree: Arc::new(|| None),
            switch_branch: Arc::new(|_| Err("未装配分支切换入口".to_string())),
            open_fork: Arc::new(|| None),
            fork_send: Arc::new(|_, _| Err("未装配分叉入口".to_string())),
            clone_session: Arc::new(|| Err("未装配克隆入口".to_string())),
            open_login: Arc::new(|| None),
            start_login: Arc::new(|_| Err("未装配登录入口".to_string())),
            refresh_login: Arc::new(Vec::new),
            send_attachments: Arc::new(|_, _, _| {}),
        }
    }
}

/// 交互循环结果（main 据以决定进程退出码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopOutcome {
    Quit,
}

/// 进入交互 TUI（备用屏 + 原始模式 + 渲染循环）。
///
/// `terminal` 由调用方用 stdout 构造；退出时由本函数恢复（raw mode 关闭、
/// 备用屏离开）。事件轮询非阻塞（100ms tick）以刷新流式 assistant。
pub async fn run_interactive(
    state: Arc<Mutex<TuiState>>,
    controller: RunController,
) -> LoopOutcome {
    let mut input = InputBuffer::new();
    let mut status: Option<String> = None;
    // M2-01/M2-02：底部插入式浮层（模型/思考选择器；打开期间独占键位）。
    let mut overlay: Option<Overlay> = None;
    // M4-02：折叠粘贴登记簿（发送时展开原文）。
    let mut pastes = crate::paste::PasteBuffer::new();
    // M4-03：斜杠菜单（输入以 / 起始时呈现；↑↓ 移动、Tab 补全）。
    let mut slash_menu: Option<crate::slash_menu::SlashMenu> = None;
    // M4-05：已发消息历史 + transcript 浮层（Ctrl+T）。
    let mut history = crate::history::History::new();
    let mut transcript_view = crate::transcript_view::TranscriptView::new();
    // G8：/fork 待发状态——Some(message_id) 时 Enter = 从该消息分叉重发
    //（pi 语义：选中文本回填编辑器，可改写；Esc 取消）。
    let mut pending_fork: Option<String> = None;
    // G6：粘贴图片缓冲（发送时随文本一起走宿主附件管线）。
    let mut attachments: Vec<crate::image_attach::PendingImage> = Vec::new();
    // G8：视图代际——transcript 整体重建（resume/分支切换/分叉）时清屏重排。
    let mut last_epoch: u64 = 0;

    // M5-02（2026-09-03 重构）：commit/live 双区渲染——历史行只打印一次进
    // scrollback（永不重写），live 区（流式预览/浮层/输入）每帧原位重绘。
    // 全量行差分在历史超过一屏后光标算术必然失准（终端滚动），已废弃。
    let mut renderer = crate::inline_render::InlineRenderer::new();
    let mut committed = 0usize;
    let mut last_live: Vec<String> = Vec::new();
    use std::io::Write;
    let mut stdout = std::io::stdout();
    // 诊断记录器：环境变量指定路径则 tee 全部渲染字节（见帧写入处注释）。
    let mut record: Option<std::fs::File> =
        std::env::var_os("R_CODE_TUI_RECORD").and_then(|path| {
            std::fs::File::create(std::path::PathBuf::from(path))
                .map_err(|e| eprintln!("r-code-tui: R_CODE_TUI_RECORD 打开失败：{e}"))
                .ok()
        });
    loop {
        let (epoch, commit, live, caret_col, rows_len) = {
            let (
                rows,
                running,
                model_selection,
                thinking,
                mode_badge,
                queue_block,
                approval,
                usage,
                streaming,
                epoch,
            ) = {
                let mut st = state.lock().unwrap();
                st.flush_streaming();
                (
                    st.rows().to_vec(),
                    st.is_running(),
                    st.model_selection().cloned(),
                    st.thinking().map(str::to_string),
                    crate::task_mode::mode_badge(st.task_mode()),
                    crate::queue_lines(st.queued()),
                    st.pending_approval().cloned(),
                    st.usage(),
                    st.streaming_preview().map(|s| s.to_string()),
                    st.view_epoch(),
                )
            };
            let model_label = model_selection
                .as_ref()
                .map(|(provider, model)| crate::model_selector::model_label(provider, model))
                .map(|label| crate::thinking::footer_label(&label, thinking.as_deref()));
            let approval_lines = approval
                .as_ref()
                .map(crate::approval_overlay::overlay_lines);
            let view = crate::display::DisplayInput {
                rows: rows.clone(),
                streaming,
                running,
                input: &input,
                status: status.clone(),
                queue_block,
                approval_lines,
                model_label,
                mode_badge,
                usage,
                overlay: overlay.as_ref(),
                slash_menu: slash_menu.as_ref(),
                transcript_view: &transcript_view,
            };
            let (width, height) = crossterm::terminal::size()
                .map(|(w, h)| (w as usize, h as usize))
                .unwrap_or((80, 24));
            // G8：视图代际变化 = transcript 已整体重建——全部行重印。
            if epoch != last_epoch {
                committed = 0;
            }
            let commit = if rows.len() > committed {
                crate::display::transcript_commit_lines(&rows[committed..])
            } else {
                Vec::new()
            };
            let mut live = crate::display::live_lines(&view, width);
            // live 块不得高于一屏减 2（保留一行余量），只保留底部最关键行。
            let max_live = height.saturating_sub(2).max(1);
            if live.len() > max_live {
                live = live.split_off(live.len() - max_live);
            }
            let caret_col = crate::display::input_caret_col(&view, width);
            let rows_len = rows.len();
            (epoch, commit, live, caret_col, rows_len)
        };

        // 无新历史且 live 未变：跳过重绘（省字节；终端无扰动）。
        if epoch != last_epoch || !commit.is_empty() || live != last_live {
            // G8：重建视图 = 清屏（含 scrollback）+ 渲染器几何重置。
            if epoch != last_epoch {
                let _ = stdout.write_all(b"\x1b[2J\x1b[3J\x1b[H");
                renderer.invalidate();
                last_live.clear();
                last_epoch = epoch;
            }
            let bytes = renderer.frame(&commit, &live);
            let _ = stdout.write_all(bytes.as_bytes());
            // 硬件光标放回输入位（IME 跟随）。
            let cursor_seq = renderer.cursor_to_live(0, caret_col as usize + 1);
            let _ = stdout.write_all(cursor_seq.as_bytes());
            let _ = stdout.flush();
            // 字节级输出记录（诊断/回归：ConPTY 会重合成输出流，真字节
            // 只有这里可取；R_CODE_TUI_RECORD=<file> 开启，帧边界以 2026 包
            // 裹为准）。
            if let Some(record) = record.as_mut() {
                let _ = record.write_all(bytes.as_bytes());
                let _ = record.write_all(cursor_seq.as_bytes());
            }
            last_live = live;
        }
        committed = rows_len;

        // 非阻塞轮询（tick 驱动流式刷新）；Ctrl-C 由 crossterm 默认捕获，这里
        // 通过 poll 收事件即可（未启用 raw 的 ctrl-c 时无需额外处理）。
        let event = tokio::task::block_in_place(|| {
            let mut got = None;
            for _ in 0..10 {
                if event::poll(Duration::from_millis(10)).unwrap_or(false) {
                    match event::read() {
                        Ok(Event::Key(key)) => {
                            got = Some(key);
                            break;
                        }
                        // M4-02：bracketed paste——超阈值折叠占位，小粘贴直插。
                        Ok(Event::Paste(text)) => {
                            if crate::paste::should_fold(&text) {
                                let placeholder = pastes.register(text);
                                input.insert_str(&placeholder);
                            } else {
                                input.insert_str(&text);
                            }
                        }
                        // M5-02：尺寸变化 → live 区几何失效，下一帧重起块。
                        Ok(Event::Resize(..)) => {
                            renderer.invalidate();
                            last_live.clear();
                        }
                        _ => {}
                    }
                }
            }
            got
        });

        if let Some(key) = event {
            // M4-05：transcript 浮层接管键位（q/esc 关闭、滚动；其余忽略）。
            if transcript_view.is_open() {
                match map_key(key) {
                    KeyAction::Quit => transcript_view.close(),
                    KeyAction::Insert(ch) if ch == 'q' || ch == 'Q' => transcript_view.close(),
                    KeyAction::HistoryPrev | KeyAction::CursorUp => {
                        let total = state.lock().unwrap().rows().len();
                        transcript_view.scroll_up(total);
                    }
                    KeyAction::HistoryNext | KeyAction::CursorDown => transcript_view.scroll_down(),
                    KeyAction::ScrollUp => {
                        let total = state.lock().unwrap().rows().len();
                        transcript_view.page_up(total, 20);
                    }
                    KeyAction::ScrollDown => transcript_view.page_down(20),
                    _ => {}
                }
                continue;
            }
            // M2-05：待审批请求接管键位（y/a/esc；必须决策，不可忽略关闭）。
            if overlay.is_none() && state.lock().unwrap().pending_approval().is_some() {
                if let Some(decision) = approval_decision_for_key(map_key(key)) {
                    (controller.decide_approval)(decision);
                }
                continue;
            }
            // 浮层打开期间独占键位（↑↓/enter/esc/字符过滤/backspace）。
            if overlay.is_some() {
                let action = map_key(key);
                // Windows/kitty 协议同一按键上报 Press+Release 双事件；Release
                // 归一为 Ignore——绝不能落进浮层的 `_ => close` 兜底，否则浮层
                // 被打开它的那枚按键的 Release 瞬间闪关（2026-09-03 /tree PTY
                // 取证：record 里浮层帧完整写出、ConPTY 流里不可见）。
                if matches!(action, KeyAction::Ignore) {
                    continue;
                }
                let mut close = false;
                match overlay.as_mut().expect("overlay") {
                    Overlay::Model(active) => match action {
                        KeyAction::ScrollUp | KeyAction::HistoryPrev | KeyAction::CursorUp => {
                            active.move_up()
                        }
                        KeyAction::ScrollDown | KeyAction::HistoryNext | KeyAction::CursorDown => {
                            active.move_down()
                        }
                        KeyAction::CursorLeft | KeyAction::CursorRight => {}
                        KeyAction::Send => {
                            // Enter = 本次会话生效（pi 对齐 G2）。
                            if let Some(entry) = active.selection().cloned() {
                                (controller.select_model)(entry);
                            }
                            close = true;
                        }
                        // Ctrl+S = 选择并持久为全局默认（写 config default_provider）。
                        KeyAction::PersistSelection => {
                            if let Some(entry) = active.selection().cloned() {
                                (controller.select_model)(entry.clone());
                                (controller.persist_default_model)(entry);
                            }
                            close = true;
                        }
                        KeyAction::Backspace => {
                            let mut query = active.query().to_string();
                            query.pop();
                            active.set_query(&query);
                        }
                        KeyAction::Insert(ch) => {
                            let mut query = active.query().to_string();
                            query.push(ch);
                            active.set_query(&query);
                        }
                        _ => close = true,
                    },
                    Overlay::Thinking(active) => match action {
                        KeyAction::ScrollUp | KeyAction::HistoryPrev | KeyAction::CursorUp => {
                            active.move_up()
                        }
                        KeyAction::ScrollDown | KeyAction::HistoryNext | KeyAction::CursorDown => {
                            active.move_down()
                        }
                        KeyAction::Send => {
                            let level = active.selection();
                            (controller.set_thinking)(level);
                            close = true;
                        }
                        _ => close = true,
                    },
                    Overlay::Resume(active) => match action {
                        KeyAction::ScrollUp | KeyAction::HistoryPrev | KeyAction::CursorUp => {
                            active.move_up()
                        }
                        KeyAction::ScrollDown | KeyAction::HistoryNext | KeyAction::CursorDown => {
                            active.move_down()
                        }
                        KeyAction::Send => {
                            if let Some(entry) = active.selection().cloned() {
                                (controller.resume_session)(entry.task_id);
                            }
                            close = true;
                        }
                        _ => close = true,
                    },
                    Overlay::Setup(active) => match action {
                        KeyAction::ScrollUp | KeyAction::HistoryPrev | KeyAction::CursorUp => {
                            active.move_up()
                        }
                        KeyAction::ScrollDown | KeyAction::HistoryNext | KeyAction::CursorDown => {
                            active.move_down()
                        }
                        KeyAction::Backspace => active.backspace(),
                        // G11：key 步 Tab 切换环境变量鉴权模式（PickProvider 态无效）。
                        KeyAction::ToggleSearch => active.toggle_env_mode(),
                        KeyAction::Insert(ch) => active.input_char(ch),
                        KeyAction::Send => {
                            if let Some(outcome) = active.advance() {
                                match outcome {
                                    crate::setup_flow::SubmitOutcome::EmptyKey => {
                                        status = Some("API key 为空，未保存".to_string());
                                    }
                                    crate::setup_flow::SubmitOutcome::Applied {
                                        provider,
                                        model,
                                    } => {
                                        let preset = crate::setup_flow::setup_presets()
                                            .into_iter()
                                            .find(|preset| preset.id == provider);
                                        match preset
                                            .map(|preset| {
                                                crate::setup_flow::apply(
                                                    &controller.config_dir,
                                                    preset,
                                                    match active.step() {
                                                        crate::setup_flow::Step::EnterKey {
                                                            key,
                                                            ..
                                                        } => key.as_str(),
                                                        _ => "",
                                                    },
                                                )
                                            })
                                            .unwrap_or_else(|| {
                                                Err("预设缺失，配置未保存".to_string())
                                            }) {
                                            Ok(()) => {
                                                status = Some(format!(
                                                    "已配置并设为默认：({provider}) {model}——/model 可切换"
                                                ));
                                                close = true;
                                            }
                                            Err(error) => {
                                                status = Some(error);
                                            }
                                        }
                                    }
                                    // G11：环境变量鉴权——空密钥落盘，不碰凭据后端。
                                    crate::setup_flow::SubmitOutcome::AppliedEnv {
                                        provider,
                                        model,
                                        env_vars,
                                    } => {
                                        let preset = crate::setup_flow::setup_presets()
                                            .into_iter()
                                            .find(|preset| preset.id == provider);
                                        let applied = preset.map(|preset| {
                                            crate::setup_flow::apply_env_mode(
                                                &controller.config_dir,
                                                preset,
                                            )
                                        });
                                        match applied.unwrap_or_else(|| {
                                            Err("预设缺失，配置未保存".to_string())
                                        }) {
                                            Ok(()) => {
                                                let any_set = env_vars.iter().any(|var| {
                                                    std::env::var(var)
                                                        .map(|value| !value.trim().is_empty())
                                                        .unwrap_or(false)
                                                });
                                                status = Some(if any_set {
                                                    format!(
                                                        "已配置并设为默认（环境变量鉴权）：({provider}) {model}——/model 可切换"
                                                    )
                                                } else {
                                                    format!(
                                                        "已设为默认（环境变量鉴权）：({provider}) {model}——但 {} 未设置，首次发送会失败",
                                                        env_vars.join(" / ")
                                                    )
                                                });
                                                close = true;
                                            }
                                            Err(error) => {
                                                status = Some(error);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        KeyAction::Quit => close = active.back(),
                        _ => {}
                    },
                    // G8：/tree——Enter 切换分支（宿主重建 transcript + 代际 +1）。
                    Overlay::Tree(active) => match action {
                        KeyAction::ScrollUp | KeyAction::HistoryPrev | KeyAction::CursorUp => {
                            active.move_up()
                        }
                        KeyAction::ScrollDown | KeyAction::HistoryNext | KeyAction::CursorDown => {
                            active.move_down()
                        }
                        KeyAction::Send => {
                            if let Some(node) = active.selection() {
                                let branch_id = node.id.clone();
                                match (controller.switch_branch)(branch_id) {
                                    Ok(line) => {
                                        state.lock().unwrap().push_system(line);
                                    }
                                    Err(error) => status = Some(error),
                                }
                            }
                            close = true;
                        }
                        _ => close = true,
                    },
                    // G8：/fork——选中消息回填编辑器（可改写），Enter 走分叉重发。
                    Overlay::Fork(active) => match action {
                        KeyAction::ScrollUp | KeyAction::HistoryPrev | KeyAction::CursorUp => {
                            active.move_up()
                        }
                        KeyAction::ScrollDown | KeyAction::HistoryNext | KeyAction::CursorDown => {
                            active.move_down()
                        }
                        KeyAction::Send => {
                            if let Some(entry) = active.selection().cloned() {
                                input.set_text(&entry.text);
                                pending_fork = Some(entry.message_id);
                                status = Some(
                                    "fork 模式：enter 从该消息分叉重发 · esc 取消".to_string(),
                                );
                            }
                            close = true;
                        }
                        _ => close = true,
                    },
                    // G10：/login——Codex 委托登录（浏览器/设备码）+ 状态刷新。
                    Overlay::Login(active) => match action {
                        KeyAction::ScrollUp | KeyAction::HistoryPrev | KeyAction::CursorUp => {
                            active.move_up()
                        }
                        KeyAction::ScrollDown | KeyAction::HistoryNext | KeyAction::CursorDown => {
                            active.move_down()
                        }
                        KeyAction::Send => {
                            if let Some(option) = active.selection() {
                                match option.key {
                                    "refresh" => {
                                        let lines = (controller.refresh_login)();
                                        let mut st = state.lock().unwrap();
                                        for line in lines {
                                            st.push_system(line);
                                        }
                                    }
                                    key => match (controller.start_login)(key) {
                                        Ok(line) => {
                                            state.lock().unwrap().push_system(line);
                                        }
                                        Err(error) => status = Some(error),
                                    },
                                }
                            }
                            close = true;
                        }
                        _ => close = true,
                    },
                }
                if close {
                    overlay = None;
                }
                continue;
            }
            let action_variant = map_key(key);
            // 斜杠菜单活动时 ↑↓ 优先归菜单（其余键仍进编辑器——菜单是被动浮层）。
            if slash_menu.is_some() {
                match action_variant {
                    KeyAction::ScrollUp | KeyAction::HistoryPrev | KeyAction::CursorUp => {
                        if let Some(menu) = slash_menu.as_mut() {
                            menu.move_up();
                        }
                        continue;
                    }
                    KeyAction::ScrollDown | KeyAction::HistoryNext | KeyAction::CursorDown => {
                        if let Some(menu) = slash_menu.as_mut() {
                            menu.move_down();
                        }
                        continue;
                    }
                    KeyAction::Send if input.text().trim() == "/model" => {}
                    KeyAction::Send if input.text().trim() == "/thinking" => {}
                    KeyAction::Send if input.text().trim() == "/status" => {}
                    KeyAction::Send if input.text().trim() == "/usage" => {}
                    KeyAction::Send if input.text().trim() == "/session" => {}
                    KeyAction::Send if input.text().trim() == "/export" => {}
                    KeyAction::Send if input.text().trim() == "/copy" => {}
                    KeyAction::Send if input.text().trim() == "/clear" => {}
                    KeyAction::Send if input.text().trim() == "/help" => {}
                    KeyAction::Send if input.text().trim() == "/quit" => {}
                    KeyAction::Send if input.text().trim() == "/tree" => {}
                    KeyAction::Send if input.text().trim() == "/fork" => {}
                    KeyAction::Send if input.text().trim() == "/clone" => {}
                    KeyAction::Send if input.text().trim() == "/login" => {}
                    KeyAction::Send if input.text().trim() == "?" => {}
                    // 非完整命令的回车 = 取菜单选中补全后再交给下方命令分派。
                    KeyAction::Send => {
                        if let Some(name) = slash_menu
                            .as_ref()
                            .and_then(|menu| menu.complete().map(str::to_string))
                        {
                            input.set_text(&name);
                        }
                    }
                    KeyAction::ToggleSearch => {
                        // Tab：补全选中命令名。
                        if let Some(name) = slash_menu
                            .as_ref()
                            .and_then(|menu| menu.complete().map(str::to_string))
                        {
                            input.set_text(&name);
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            match action_variant {
                KeyAction::Insert(ch) => input.insert(ch),
                KeyAction::Newline => input.newline(),
                KeyAction::Undo => {
                    input.undo();
                }
                KeyAction::Redo => {
                    input.redo();
                }
                KeyAction::WordLeft => input.move_word_left(),
                KeyAction::WordRight => input.move_word_right(),
                KeyAction::HistoryPrev => {
                    if let Some(text) = history.navigate_back(&input.text()) {
                        input.set_text(&text);
                    }
                }
                KeyAction::HistoryNext => {
                    if let Some(text) = history.navigate_forward() {
                        input.set_text(&text);
                    }
                }
                // pi 对齐 G3：多行编辑 ↑/↓ 先做垂直光标移动，只在首行/末行
                // 边界翻历史（Ctrl+P/N 无条件翻，保持 shell 惯例）。
                KeyAction::CursorUp => {
                    if !input.move_up() {
                        if let Some(text) = history.navigate_back(&input.text()) {
                            input.set_text(&text);
                        }
                    }
                }
                KeyAction::CursorDown => {
                    if !input.move_down() {
                        if let Some(text) = history.navigate_forward() {
                            input.set_text(&text);
                        }
                    }
                }
                // pi 对齐 G1：Ctrl+L 直开模型选择器（空集转配置向导，同 /model）。
                KeyAction::OpenModelPicker => {
                    overlay = (controller.open_model_picker)().map(Overlay::Model);
                    if overlay.is_none() {
                        overlay = Some(Overlay::Setup(crate::setup_flow::SetupFlow::new()));
                        status = Some("没有可用的模型服务——已进入配置向导（Esc 取消）".to_string());
                    }
                }
                // pi 对齐 G6：Ctrl+V 读系统剪贴板图片为附件（终端粘贴通道只有文本）。
                KeyAction::PasteImage => match crate::image_attach::read_clipboard_image() {
                    Ok(Some(pending)) => {
                        state.lock().unwrap().push_row(image_row(&pending));
                        attachments.push(pending);
                    }
                    Ok(None) => status = Some("剪贴板没有图片".to_string()),
                    Err(error) => status = Some(error),
                },
                KeyAction::ToggleTranscript => transcript_view.toggle(),
                KeyAction::ExternalEditor => {
                    // 临时退出 raw mode 给编辑器，回来后回填。
                    let draft = input.text();
                    let _ = crossterm::terminal::disable_raw_mode();
                    let outcome = crate::external_editor::run_external_editor(&draft).await;
                    let _ = crossterm::terminal::enable_raw_mode();
                    match outcome {
                        Ok(edited) => input.set_text(&edited),
                        Err(error) => {
                            state
                                .lock()
                                .unwrap()
                                .push_system(format!("外部编辑器：{error}"));
                        }
                    }
                    // inline：外编期间终端被外部改写，下一帧全量对齐。
                    renderer = crate::inline_render::InlineRenderer::new();
                }
                KeyAction::Backspace => input.backspace(),
                KeyAction::DeleteForward => input.delete_forward(),
                KeyAction::CursorLeft => input.move_left(),
                KeyAction::CursorRight => input.move_right(),
                KeyAction::CursorHome => input.move_home(),
                KeyAction::CursorEnd => input.move_end(),
                KeyAction::ScrollUp | KeyAction::ScrollDown => {}
                KeyAction::ToggleFullscreen | KeyAction::ToggleSearch => {}
                // Ctrl+S 只在模型浮层里有语义（见 Overlay::Model 臂）。
                KeyAction::PersistSelection => {}
                KeyAction::Send => {
                    let text = input.take();
                    let trimmed = text.trim();
                    if crate::bang_command::is_bang_command(&text) {
                        let command = crate::bang_command::command_body(&text).to_string();
                        {
                            let mut st = state.lock().unwrap();
                            st.push_row(crate::TranscriptRow::Shell(
                                crate::bang_command::ShellRow::Prompt {
                                    command: command.clone(),
                                },
                            ));
                        }
                        (controller.run_bang)(command);
                    } else if trimmed == "/model" {
                        overlay = (controller.open_model_picker)().map(Overlay::Model);
                        if overlay.is_none() {
                            // 症状3修复：死端改引导——无可用服务时直接进 /setup 流。
                            overlay = Some(Overlay::Setup(crate::setup_flow::SetupFlow::new()));
                            status =
                                Some("没有可用的模型服务——已进入配置向导（Esc 取消）".to_string());
                        }
                    } else if trimmed == "/setup" {
                        overlay = Some(Overlay::Setup(crate::setup_flow::SetupFlow::new()));
                    } else if trimmed == "/status" || trimmed == "/usage" {
                        let (card, summary) = (controller.status_report)();
                        let mut st = state.lock().unwrap();
                        if trimmed == "/status" {
                            for line in card {
                                st.push_system(line);
                            }
                        } else {
                            st.push_system(summary);
                        }
                    } else if trimmed == "/session" {
                        // G9：会话统计卡（ID/标题/消息数/token/成本/会话文件）。
                        let card = (controller.session_report)();
                        let mut st = state.lock().unwrap();
                        if card.is_empty() {
                            st.push_system("会话状态不可用".to_string());
                        } else {
                            for line in card {
                                st.push_system(line);
                            }
                        }
                    } else if trimmed == "/export" || trimmed.starts_with("/export ") {
                        // G7：导出当前 transcript（.md 默认 / .html / .jsonl）。
                        let arg = trimmed
                            .strip_prefix("/export")
                            .map(str::trim)
                            .filter(|text| !text.is_empty());
                        let now = chrono::Local::now();
                        let (rows, model_label) = {
                            let st = state.lock().unwrap();
                            (
                                st.rows().to_vec(),
                                st.model_selection().map(|(provider, model)| {
                                    crate::model_selector::model_label(provider, model)
                                }),
                            )
                        };
                        let meta = crate::export::ExportMeta {
                            model_label: model_label.unwrap_or_default(),
                            exported_at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
                        };
                        let path = crate::export::resolve_path(
                            arg,
                            &now.format("%Y%m%d-%H%M%S").to_string(),
                        );
                        match crate::export::write_export(&path, &rows, &meta) {
                            Ok(written) => {
                                state.lock().unwrap().push_system(format!(
                                    "已导出会话（{} 行）→ {written}",
                                    rows.len()
                                ));
                            }
                            Err(error) => status = Some(error),
                        }
                    } else if trimmed == "/copy" {
                        // G7：复制最后一条 assistant 回复（OSC 52 终端剪贴板）。
                        let last = {
                            let st = state.lock().unwrap();
                            crate::clipboard::last_assistant_text(st.rows()).map(str::to_string)
                        };
                        match last {
                            Some(text) => match crate::clipboard::copy_check(&text) {
                                Ok(()) => {
                                    let _ = stdout.write_all(
                                        crate::clipboard::osc52_sequence(&text).as_bytes(),
                                    );
                                    let _ = stdout.flush();
                                    state.lock().unwrap().push_system(
                                        "已复制最后一条回复（经终端剪贴板）".to_string(),
                                    );
                                }
                                Err(error) => status = Some(error),
                            },
                            None => status = Some("没有可复制的回复".to_string()),
                        }
                    } else if trimmed == "/clear" {
                        state.lock().unwrap().clear_transcript();
                    } else if trimmed == "/help" || trimmed == "?" {
                        let mut st = state.lock().unwrap();
                        for line in crate::slash_menu::help_panel_lines() {
                            st.push_system(line);
                        }
                    } else if trimmed == "/resume" {
                        overlay = (controller.open_resume)().map(Overlay::Resume);
                        if overlay.is_none() {
                            status = Some("没有可恢复的会话".to_string());
                        }
                    } else if trimmed == "/new" {
                        (controller.new_session)();
                    } else if trimmed.starts_with("/rename ") {
                        (controller.rename_session)(
                            trimmed
                                .strip_prefix("/rename ")
                                .unwrap_or_default()
                                .to_string(),
                        );
                    } else if trimmed == "/compact" || trimmed.starts_with("/compact ") {
                        // pi 对齐 G5：/compact [指令]——focus 为空走宿主默认摘要指令。
                        let focus = trimmed
                            .strip_prefix("/compact")
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                            .map(str::to_string);
                        match (controller.compact_context)(focus) {
                            Ok(line) => {
                                state.lock().unwrap().push_system(line);
                            }
                            Err(error) => status = Some(error),
                        }
                    } else if trimmed == "/quit" {
                        return LoopOutcome::Quit;
                    } else if trimmed == "/tree" {
                        // G8：分支树导航。
                        overlay = (controller.open_tree)().map(Overlay::Tree);
                        if overlay.is_none() {
                            status = Some("分支列表不可用".to_string());
                        }
                    } else if trimmed == "/fork" {
                        // G8：消息级分叉（pi /fork：选 user 消息 → 回填编辑器改写重发）。
                        overlay = (controller.open_fork)().map(Overlay::Fork);
                        if overlay.is_none() {
                            status = Some("还没有可分叉的消息（先发送一条）".to_string());
                        }
                    } else if trimmed == "/clone" {
                        // G8：克隆当前会话（新任务承载同历史；留在当前会话）。
                        match (controller.clone_session)() {
                            Ok(line) => {
                                state.lock().unwrap().push_system(line);
                            }
                            Err(error) => status = Some(error),
                        }
                    } else if trimmed == "/login" {
                        // G10：账号登录（Codex OAuth 委托；其余厂商引导 /setup）。
                        overlay = (controller.open_login)().map(Overlay::Login);
                    } else if trimmed == "/thinking" {
                        let current = state.lock().unwrap().thinking().map(str::to_string);
                        overlay = Some(Overlay::Thinking(ThinkingPicker::new(current.as_deref())));
                    } else if pending_fork.is_some() && !trimmed.is_empty() {
                        // G8：fork 模式发送——从选中的历史消息分叉重发（文本可改写）。
                        let Some(message_id) = pending_fork.take() else {
                            unreachable!("pending_fork 已判 Some");
                        };
                        let text = pastes.expand(&text);
                        history.record(&text);
                        match (controller.fork_send)(message_id, text) {
                            Ok(line) => {
                                state.lock().unwrap().push_system(line);
                            }
                            Err(error) => status = Some(error),
                        }
                    } else if !trimmed.is_empty() {
                        // M2-04：运行中 Enter = 排队（不打断当前 run），
                        // 空闲 = 正常发送。
                        // M4-02：折叠占位符在发送时展开（上下文拿完整原文）。
                        let text = pastes.expand(&text);
                        // M4-05：进历史栈（! 命令与斜杠命令同样可 ↑ 找回）。
                        history.record(&text);
                        // G6：@file 图片提及随发送读取加入附件（ctrl+V 粘贴的已在缓冲）。
                        let mut images = std::mem::take(&mut attachments);
                        if let Ok(cwd) = std::env::current_dir() {
                            for path in crate::image_attach::collect_image_mentions(&text, &cwd) {
                                match crate::image_attach::load_image_file(&path) {
                                    Ok(pending) => {
                                        let mut st = state.lock().unwrap();
                                        st.push_row(image_row(&pending));
                                        images.push(pending);
                                    }
                                    Err(error) => {
                                        state.lock().unwrap().push_system(error);
                                    }
                                }
                            }
                        }
                        let route = {
                            let mut st = state.lock().unwrap();
                            let route = crate::route_send(st.is_running());
                            st.push_user(text.clone());
                            if route == crate::SendRoute::Queue {
                                st.queue_message(text.clone());
                            }
                            route
                        };
                        if images.is_empty() {
                            match route {
                                crate::SendRoute::Queue => (controller.queue_send)(text),
                                crate::SendRoute::Send => (controller.send)(text),
                            }
                        } else {
                            // G6：带附件走宿主附件管线（校验/OCR 转换/排队持久化）。
                            (controller.send_attachments)(
                                text,
                                images,
                                route == crate::SendRoute::Queue,
                            );
                        }
                    }
                }
                KeyAction::Abort => {
                    let running = state.lock().unwrap().is_running();
                    if running {
                        (controller.abort)();
                        status = Some("已请求中止…".to_string());
                    } else {
                        return LoopOutcome::Quit;
                    }
                }
                KeyAction::Quit => {
                    if pending_fork.take().is_some() {
                        // G8：fork 模式下 Esc 优先取消分叉（不退出）。
                        status = Some("已取消分叉".to_string());
                    } else if !input.is_empty() {
                        input.take();
                    } else {
                        return LoopOutcome::Quit;
                    }
                }
                KeyAction::CycleMode => {
                    let current = state.lock().unwrap().task_mode().to_string();
                    let next = crate::task_mode::cycle_mode(&current);
                    (controller.set_mode)(next);
                }
                KeyAction::ToggleThinking => {
                    let current = state.lock().unwrap().thinking().map(str::to_string);
                    overlay = Some(Overlay::Thinking(ThinkingPicker::new(current.as_deref())));
                }
                KeyAction::ThinkingDown | KeyAction::ThinkingUp => {
                    let current = state.lock().unwrap().thinking().map(str::to_string);
                    let delta = if matches!(action_variant, KeyAction::ThinkingUp) {
                        1
                    } else {
                        -1
                    };
                    let level = crate::thinking::step_level(current.as_deref(), delta);
                    (controller.set_thinking)(level);
                }
                KeyAction::Ignore => {}
            }
            // M4-03：按输入文本同步斜杠菜单（/ 起始即呈现；编辑即过滤）。
            let text = input.text();
            if crate::slash_menu::should_show(&text) {
                match slash_menu.as_mut() {
                    Some(menu) => menu.set_query(text.trim()),
                    None => {
                        slash_menu = Some(crate::slash_menu::SlashMenu::new(text.trim()));
                    }
                }
            } else {
                slash_menu = None;
            }
        }
    }
}

/// M2-05：审批接管期的键位映射（y/a=决策，esc/ctrl-c=拒绝——审批不可忽略关闭）。
fn approval_decision_for_key(action: KeyAction) -> Option<crate::approval::ApprovalDecision> {
    match action {
        KeyAction::Insert(ch) => crate::approval_overlay::map_decision(ch),
        KeyAction::Quit | KeyAction::Abort => Some(crate::approval::ApprovalDecision::Deny),
        _ => None,
    }
}

/// G6：待发图片 → transcript 行（半块预览或占位）。
fn image_row(pending: &crate::image_attach::PendingImage) -> crate::TranscriptRow {
    crate::TranscriptRow::Image {
        name: pending.name.clone(),
        width: pending.width,
        height: pending.height,
        preview: pending.preview.clone(),
    }
}
