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
                    if let Ok(Event::Key(key)) = event::read() {
                        got = Some(key);
                        break;
                    }
                }
            }
            got
        });

        if let Some(key) = event {
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
                    } else if trimmed == "/thinking" {
                        let current = state.lock().unwrap().thinking().map(str::to_string);
                        overlay = Some(Overlay::Thinking(ThinkingPicker::new(current.as_deref())));
                    } else if !trimmed.is_empty() {
                        // M2-04：运行中 Enter = 排队（不打断当前 run），
                        // 空闲 = 正常发送。
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
    let (running, model_selection, thinking, mode_badge, queue_block) = {
        let st = state.lock().unwrap();
        (
            st.is_running(),
            st.model_selection().cloned(),
            st.thinking().map(str::to_string),
            crate::task_mode::mode_badge(st.task_mode()),
            crate::queue_lines(st.queued()),
        )
    };

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
    overlay: Option<&Overlay>,
) {
    // 浮层打开时：底部预留插入式列表（≤8 行 + 查询行），列表在输入区上方；
    // 排队块（M2-04）在其上。
    let overlay_height = overlay.map(|_| 9).unwrap_or(0);
    let queue_height = queue_block.len();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Min(3),
                Constraint::Length(queue_height as u16),
                Constraint::Length(overlay_height),
                Constraint::Length(3),
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

    if let (Some(overlay), true) = (overlay, overlay_height > 0) {
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
    // footer 右侧标签 `(provider) model • thinking`（M2-01/M2-02 联动；未定时不占位）。
    if let Some(label) = model_label {
        let used: usize = prompt.chars().count() + input_text.chars().count();
        let width = area.width as usize;
        if let Some(padding) = (width.saturating_sub(used + label.chars().count() + 1))
            .checked_sub(1)
            .filter(|pad| *pad > 0)
        {
            input_line.push(Span::raw(" ".repeat(padding)));
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
    );
}
