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

/// 底部插入式浮层（同一时刻至多一层；模型/思考/会话选择器）。
pub enum Overlay {
    Model(ModelPicker),
    Thinking(ThinkingPicker),
    Resume(crate::session_picker::SessionPicker),
}

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
            run_bang: Arc::new(|_| {}),
            open_resume: Arc::new(|| None),
            resume_session: Arc::new(|_| {}),
            new_session: Arc::new(|| {}),
            rename_session: Arc::new(|_| {}),
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

    // M5-02（2026-09-03 重构）：commit/live 双区渲染——历史行只打印一次进
    // scrollback（永不重写），live 区（流式预览/浮层/输入）每帧原位重绘。
    // 全量行差分在历史超过一屏后光标算术必然失准（终端滚动），已废弃。
    let mut renderer = crate::inline_render::InlineRenderer::new();
    let mut committed = 0usize;
    let mut last_live: Vec<String> = Vec::new();
    use std::io::Write;
    let mut stdout = std::io::stdout();
    loop {
        let (commit, live, caret_col, rows_len) = {
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
            (commit, live, caret_col, rows_len)
        };

        // 无新历史且 live 未变：跳过重绘（省字节；终端无扰动）。
        if !commit.is_empty() || live != last_live {
            let bytes = renderer.frame(&commit, &live);
            let _ = stdout.write_all(bytes.as_bytes());
            // 硬件光标放回输入位（IME 跟随）。
            let cursor_seq = renderer.cursor_to_live(0, caret_col as usize + 1);
            let _ = stdout.write_all(cursor_seq.as_bytes());
            let _ = stdout.flush();
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
                    KeyAction::HistoryPrev => {
                        let total = state.lock().unwrap().rows().len();
                        transcript_view.scroll_up(total);
                    }
                    KeyAction::HistoryNext => transcript_view.scroll_down(),
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
                let mut close = false;
                match overlay.as_mut().expect("overlay") {
                    Overlay::Model(active) => match action {
                        KeyAction::ScrollUp => active.move_up(),
                        KeyAction::ScrollDown => active.move_down(),
                        KeyAction::CursorLeft | KeyAction::CursorRight => {}
                        KeyAction::Send => {
                            if let Some(entry) = active.selection().cloned() {
                                (controller.select_model)(entry);
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
                        KeyAction::ScrollUp => active.move_up(),
                        KeyAction::ScrollDown => active.move_down(),
                        KeyAction::Send => {
                            let level = active.selection();
                            (controller.set_thinking)(level);
                            close = true;
                        }
                        _ => close = true,
                    },
                    Overlay::Resume(active) => match action {
                        KeyAction::ScrollUp | KeyAction::HistoryPrev => active.move_up(),
                        KeyAction::ScrollDown | KeyAction::HistoryNext => active.move_down(),
                        KeyAction::Send => {
                            if let Some(entry) = active.selection().cloned() {
                                (controller.resume_session)(entry.task_id);
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
                    KeyAction::ScrollUp | KeyAction::HistoryPrev => {
                        if let Some(menu) = slash_menu.as_mut() {
                            menu.move_up();
                        }
                        continue;
                    }
                    KeyAction::ScrollDown | KeyAction::HistoryNext => {
                        if let Some(menu) = slash_menu.as_mut() {
                            menu.move_down();
                        }
                        continue;
                    }
                    KeyAction::Send if input.text().trim() == "/model" => {}
                    KeyAction::Send if input.text().trim() == "/thinking" => {}
                    KeyAction::Send if input.text().trim() == "/status" => {}
                    KeyAction::Send if input.text().trim() == "/usage" => {}
                    KeyAction::Send if input.text().trim() == "/clear" => {}
                    KeyAction::Send if input.text().trim() == "/help" => {}
                    KeyAction::Send if input.text().trim() == "/quit" => {}
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
                            status = Some("没有可用的模型服务（先完成 provider 配置）".to_string());
                        }
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
                    } else if trimmed == "/compact" {
                        if !crate::session_ops::compaction_supported() {
                            state.lock().unwrap().push_system(
                                "自动压缩随 run 结束触发，暂无可显式调用的压缩入口".to_string(),
                            );
                        }
                    } else if trimmed == "/quit" {
                        return LoopOutcome::Quit;
                    } else if trimmed == "/thinking" {
                        let current = state.lock().unwrap().thinking().map(str::to_string);
                        overlay = Some(Overlay::Thinking(ThinkingPicker::new(current.as_deref())));
                    } else if !trimmed.is_empty() {
                        // M2-04：运行中 Enter = 排队（不打断当前 run），
                        // 空闲 = 正常发送。
                        // M4-02：折叠占位符在发送时展开（上下文拿完整原文）。
                        let text = pastes.expand(&text);
                        // M4-05：进历史栈（! 命令与斜杠命令同样可 ↑ 找回）。
                        history.record(&text);
                        let route = {
                            let mut st = state.lock().unwrap();
                            let route = crate::route_send(st.is_running());
                            st.push_user(text.clone());
                            if route == crate::SendRoute::Queue {
                                st.queue_message(text.clone());
                            }
                            route
                        };
                        match route {
                            crate::SendRoute::Queue => (controller.queue_send)(text),
                            crate::SendRoute::Send => (controller.send)(text),
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
                    if !input.is_empty() {
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
