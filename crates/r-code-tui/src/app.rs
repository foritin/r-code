//! ratatui 渲染循环（R-TUI-01 阶段 1：消息流 + 流式 assistant + 工具卡折叠 +
//! 输入 + 发送/steer/abort）。
//!
//! 渲染只消费 `TuiState`（snapshot 权威：不在此累积领域状态副本），输入动作
//! 经 `input` 模块归一。滚动 = turn 级窗口化（`window` 模块）+ 视口偏移；
//! 状态栏展示运行态与提示键位。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::fullscreen::ScreenState;
use crate::input::{map_key, InputBuffer, KeyAction};
use crate::model_selector::ModelPicker;
use crate::thinking::ThinkingPicker;
use crate::window::windowed;
use crate::{TranscriptRow, TuiState};

/// 底部插入式浮层（同一时刻至多一层；模型/思考选择器）。
pub enum Overlay {
    Model(ModelPicker),
    Thinking(ThinkingPicker),
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
    mut terminal: Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    state: Arc<Mutex<TuiState>>,
    controller: RunController,
) -> LoopOutcome {
    let mut input = InputBuffer::new();
    let mut screen = ScreenState::default();
    let mut scroll_offset: usize = 0;
    let mut status: Option<String> = None;
    // M2-01/M2-02：底部插入式浮层（模型/思考选择器；打开期间独占键位）。
    let mut overlay: Option<Overlay> = None;
    // M4-02：折叠粘贴登记簿（发送时展开原文）。
    let mut pastes = crate::paste::PasteBuffer::new();

    loop {
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &state,
                    &input,
                    &screen,
                    scroll_offset,
                    status.as_deref(),
                    overlay.as_ref(),
                );
            })
            .expect("terminal draw");

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
                        _ => {}
                    }
                }
            }
            got
        });

        if let Some(key) = event {
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
                }
                if close {
                    overlay = None;
                }
                continue;
            }
            let action_variant = map_key(key);
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
                KeyAction::ExternalEditor => {
                    // 临时退出 raw/alt-screen 给编辑器，回来后回填。
                    let draft = input.text();
                    let _ = crossterm::terminal::disable_raw_mode();
                    let mut stdout = std::io::stdout();
                    let _ = crossterm::execute!(
                        stdout,
                        crossterm::cursor::Show,
                        crossterm::terminal::LeaveAlternateScreen
                    );
                    let outcome = crate::external_editor::run_external_editor(&draft).await;
                    let _ = crossterm::terminal::enable_raw_mode();
                    let _ = crossterm::execute!(
                        stdout,
                        crossterm::terminal::EnterAlternateScreen,
                        crossterm::cursor::Hide
                    );
                    match outcome {
                        Ok(edited) => input.set_text(&edited),
                        Err(error) => {
                            state
                                .lock()
                                .unwrap()
                                .push_system(format!("外部编辑器：{error}"));
                        }
                    }
                    terminal.clear().expect("terminal clear");
                }
                KeyAction::Backspace => input.backspace(),
                KeyAction::DeleteForward => input.delete_forward(),
                KeyAction::CursorLeft => input.move_left(),
                KeyAction::CursorRight => input.move_right(),
                KeyAction::CursorHome => input.move_home(),
                KeyAction::CursorEnd => input.move_end(),
                KeyAction::ScrollUp => scroll_offset = scroll_offset.saturating_add(1),
                KeyAction::ScrollDown => scroll_offset = scroll_offset.saturating_sub(1),
                KeyAction::ToggleFullscreen => screen.toggle(),
                KeyAction::ToggleSearch => screen.toggle_search(),
                KeyAction::Send => {
                    let text = input.take();
                    let trimmed = text.trim();
                    if trimmed == "/model" {
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
                    } else if trimmed == "/thinking" {
                        let current = state.lock().unwrap().thinking().map(str::to_string);
                        overlay = Some(Overlay::Thinking(ThinkingPicker::new(current.as_deref())));
                    } else if !trimmed.is_empty() {
                        // M2-04：运行中 Enter = 排队（不打断当前 run），
                        // 空闲 = 正常发送。
                        // M4-02：折叠占位符在发送时展开（上下文拿完整原文）。
                        let text = pastes.expand(&text);
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
        }
    }
}

fn render(
    frame: &mut Frame<'_>,
    state: &Arc<Mutex<TuiState>>,
    input: &InputBuffer,
    screen: &ScreenState,
    scroll_offset: usize,
    status: Option<&str>,
    overlay: Option<&Overlay>,
) {
    // snapshot 权威：渲染只读快照（不累积副本）。
    let rows = {
        let mut st = state.lock().unwrap();
        st.flush_streaming();
        st.rows().to_vec()
    };
    let (running, model_selection, thinking, mode_badge, queue_block, approval, usage) = {
        let st = state.lock().unwrap();
        (
            st.is_running(),
            st.model_selection().cloned(),
            st.thinking().map(str::to_string),
            crate::task_mode::mode_badge(st.task_mode()),
            crate::queue_lines(st.queued()),
            st.pending_approval().cloned(),
            st.usage(),
        )
    };
    let approval_lines = approval
        .as_ref()
        .map(crate::approval_overlay::overlay_lines);

    let area = frame.area();
    if screen.mode == crate::fullscreen::ScreenMode::Fullscreen {
        render_fullscreen(frame, area, &rows, input, running, status);
    } else {
        let label = model_selection
            .as_ref()
            .map(|(provider, model)| crate::model_selector::model_label(provider, model))
            .map(|label| crate::thinking::footer_label(&label, thinking.as_deref()));
        render_regular(
            frame,
            area,
            &rows,
            input,
            running,
            status,
            scroll_offset,
            label,
            mode_badge,
            queue_block,
            approval_lines,
            usage,
            overlay,
        );
    }
}

fn transcript_lines(rows: &[TranscriptRow]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in rows {
        match row {
            TranscriptRow::User { text } => {
                lines.push(Line::from(Span::styled(
                    format!("你 > {text}"),
                    Style::default().fg(Color::Cyan),
                )));
            }
            TranscriptRow::Assistant { text, complete } => {
                let style = if *complete {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::ITALIC)
                };
                lines.push(Line::from(Span::styled(format!("R-Code > {text}"), style)));
            }
            TranscriptRow::ToolCard {
                name,
                summary,
                is_error,
            } => {
                let style = if *is_error {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                lines.push(Line::from(Span::styled(
                    format!("  [tool] {name} · {summary}"),
                    style,
                )));
            }
            TranscriptRow::System { text } => {
                lines.push(Line::from(Span::styled(
                    format!("· {text}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            TranscriptRow::Shell(shell) => match shell {
                crate::bang_command::ShellRow::Prompt { command } => {
                    lines.push(Line::from(Span::styled(
                        format!("$ {command}"),
                        Style::default().fg(Color::Magenta),
                    )));
                }
                crate::bang_command::ShellRow::Output { exit_code, .. } => {
                    lines.push(Line::from(Span::styled(
                        format!("  (shell 退出码 {exit_code:?})"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            },
        }
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn render_regular(
    frame: &mut Frame<'_>,
    area: Rect,
    rows: &[TranscriptRow],
    input: &InputBuffer,
    running: bool,
    status: Option<&str>,
    _scroll_offset: usize,
    model_label: Option<String>,
    mode_badge: Option<(&'static str, crate::task_mode::BadgeColor)>,
    queue_block: Vec<String>,
    approval_lines: Option<Vec<crate::approval_overlay::OverlayLine>>,
    usage: Option<crate::status_bar::UsageStats>,
    overlay: Option<&Overlay>,
) {
    // 浮层打开时：底部预留插入式列表（≤8 行 + 查询行），列表在输入区上方；
    // 排队块（M2-04）在其上。
    let overlay_height = overlay.map(|_| 9).unwrap_or(0);
    let queue_height = queue_block.len();
    // 输入区自动增高（M4-01：折行行数 + 边框，上限 10 行）。
    let input_height = {
        let lines = crate::input::wrap_lines(&input.text(), area.width.saturating_sub(2) as usize);
        (lines.len().max(1) + 2).min(10) as u16
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Min(3),
                Constraint::Length(queue_height as u16),
                Constraint::Length(overlay_height),
                Constraint::Length(input_height),
            ]
            .as_ref(),
        )
        .split(area);

    let transcript = Paragraph::new(transcript_lines(rows))
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false });
    frame.render_widget(transcript, chunks[0]);

    if !queue_block.is_empty() {
        let queue_paragraph = Paragraph::new(
            queue_block
                .iter()
                .map(|line| {
                    Line::from(Span::styled(
                        line.clone(),
                        Style::default().fg(Color::DarkGray),
                    ))
                })
                .collect::<Vec<_>>(),
        );
        frame.render_widget(queue_paragraph, chunks[1]);
    }

    // M2-05：审批浮层（带面语义：内联底部面板，Title bold / Command magenta / Hint dim）。
    if let Some(lines) = approval_lines {
        let styled: Vec<Line<'static>> = lines
            .iter()
            .map(|line| {
                let style = match line.kind {
                    crate::approval_overlay::LineKind::Title => {
                        Style::default().add_modifier(Modifier::BOLD)
                    }
                    crate::approval_overlay::LineKind::Command => {
                        Style::default().fg(Color::Magenta)
                    }
                    crate::approval_overlay::LineKind::Option => Style::default(),
                    crate::approval_overlay::LineKind::Hint => Style::default().fg(Color::DarkGray),
                };
                Line::from(Span::styled(line.text.clone(), style))
            })
            .collect();
        let height = styled.len() as u16;
        let band = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(height), Constraint::Min(0)].as_ref())
            .split(chunks[2]);
        frame.render_widget(
            Paragraph::new(styled).block(Block::default().borders(Borders::TOP)),
            band[0],
        );
        // 其余浮层（若同时存在）渲染在审批带之下。
        if let (Some(overlay), true) = (overlay, overlay_height > 0) {
            match overlay {
                Overlay::Model(picker) => render_model_picker(frame, band[1], picker),
                Overlay::Thinking(picker) => render_thinking_picker(frame, band[1], picker),
            }
        }
    } else if let (Some(overlay), true) = (overlay, overlay_height > 0) {
        match overlay {
            Overlay::Model(picker) => render_model_picker(frame, chunks[2], picker),
            Overlay::Thinking(picker) => render_thinking_picker(frame, chunks[2], picker),
        }
    }

    let input_text = input.text();
    let prompt = if running { "⏳ steer > " } else { "> " };
    let mut input_line = vec![Span::styled(prompt, Style::default().fg(Color::Blue))];
    // M2-03：模式徽章（plan=magenta / edit=cyan / auto=yellow；ask 无徽章）。
    if let Some((badge, color)) = mode_badge {
        let semantic = match color {
            crate::task_mode::BadgeColor::Cyan => Color::Cyan,
            crate::task_mode::BadgeColor::Yellow => Color::Yellow,
            crate::task_mode::BadgeColor::Magenta => Color::Magenta,
        };
        input_line.push(Span::styled(
            format!("{badge} "),
            Style::default().fg(semantic),
        ));
    }
    input_line.push(Span::raw(input_text.clone()));
    // footer 右侧：统计行（M3-01，阈值变色）+ 模型标签
    // `↑in ↓out N% context left (provider) model • thinking`。
    let (stats_text, stats_color) = usage
        .map(|stats| {
            let (text, threshold) = crate::status_bar::footer_stats_line(&stats, None, false);
            let color = match threshold {
                crate::status_bar::Threshold::Normal => Color::DarkGray,
                crate::status_bar::Threshold::Warning => Color::Yellow,
                crate::status_bar::Threshold::Error => Color::Red,
            };
            (format!("{text}  "), color)
        })
        .unwrap_or_default();
    if let Some(label) = model_label {
        let used: usize = prompt.chars().count() + input_text.chars().count();
        let tail = format!("{stats_text}{label}");
        let width = area.width as usize;
        if let Some(padding) = (width.saturating_sub(used + tail.chars().count() + 1))
            .checked_sub(1)
            .filter(|pad| *pad > 0)
        {
            input_line.push(Span::raw(" ".repeat(padding)));
            input_line.push(Span::styled(stats_text, Style::default().fg(stats_color)));
            input_line.push(Span::styled(label, Style::default().fg(Color::DarkGray)));
        }
    }
    let input_paragraph =
        Paragraph::new(Line::from(input_line)).block(Block::default().borders(Borders::TOP));
    frame.render_widget(input_paragraph, chunks[2]);

    if let Some(status) = status {
        let status_line = Paragraph::new(Span::styled(status, Style::default().fg(Color::Yellow)));
        let bottom = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)].as_ref())
            .split(area);
        frame.render_widget(status_line, bottom[1]);
    }
}

/// /model 选择器：查询行 + 分组条目（选中行 `› ` + cyan bold；组头 dim）。
fn render_model_picker(frame: &mut Frame<'_>, area: Rect, picker: &ModelPicker) {
    let selected_row = picker.selected_row().unwrap_or(0);
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
        format!("/model {}", picker.query()),
        Style::default().fg(Color::DarkGray),
    ))];
    for (index, (group, text)) in picker.visible_rows().iter().take(8).enumerate() {
        let row_index = index + 1;
        if let Some(provider) = group {
            lines.push(Line::from(Span::styled(
                provider.clone(),
                Style::default().fg(Color::DarkGray),
            )));
        } else if row_index == selected_row {
            lines.push(Line::from(Span::styled(
                format!("› {text}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::raw(format!("  {text}"))));
        }
    }
    let picker_paragraph = Paragraph::new(lines).block(Block::default().borders(Borders::TOP));
    frame.render_widget(picker_paragraph, area);
}

/// 思考档位选择器：选中行 `› ` + cyan bold（与模型选择器同视觉语义）。
fn render_thinking_picker(frame: &mut Frame<'_>, area: Rect, picker: &ThinkingPicker) {
    let selected = picker.selection();
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
        "/thinking（alt+T 打开，alt+, / alt+. 升降）",
        Style::default().fg(Color::DarkGray),
    ))];
    for level in crate::thinking::EFFORT_LEVELS {
        if level == selected {
            lines.push(Line::from(Span::styled(
                format!("› {level}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::raw(format!("  {level}"))));
        }
    }
    let picker_paragraph = Paragraph::new(lines).block(Block::default().borders(Borders::TOP));
    frame.render_widget(picker_paragraph, area);
}

/// M2-05：审批接管期的键位映射（y/a=决策，esc/ctrl-c=拒绝——审批不可忽略关闭）。
fn approval_decision_for_key(action: KeyAction) -> Option<crate::approval::ApprovalDecision> {
    match action {
        KeyAction::Insert(ch) => crate::approval_overlay::map_decision(ch),
        KeyAction::Quit | KeyAction::Abort => Some(crate::approval::ApprovalDecision::Deny),
        _ => None,
    }
}

fn render_fullscreen(
    frame: &mut Frame<'_>,
    area: Rect,
    rows: &[TranscriptRow],
    input: &InputBuffer,
    running: bool,
    status: Option<&str>,
) {
    // 窗口化：turn 级（PRD R-TUI-05），窗口 = 最近 50 turn。
    let windowed = windowed(rows, 50);
    let owned: Vec<TranscriptRow> = windowed.iter().map(|row| (*row).clone()).collect();
    render_regular(
        frame,
        area,
        &owned,
        input,
        running,
        status,
        0,
        None,
        None,
        Vec::new(),
        None,
        None,
        None,
    );
}
