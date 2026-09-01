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
use crate::window::windowed;
use crate::{TranscriptRow, TuiState};

/// 交互循环的宿主回调（发送/steer/abort 的真实语义由 main.rs 装配）。
#[derive(Clone)]
pub struct RunController {
    /// 发送/steer：`agent_send`（Auto 语义，运行中自动 steer）。
    pub send: Arc<dyn Fn(String) + Send + Sync>,
    /// 中止当前运行。
    pub abort: Arc<dyn Fn() + Send + Sync>,
}

impl Default for RunController {
    fn default() -> Self {
        Self {
            send: Arc::new(|_| {}),
            abort: Arc::new(|| {}),
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
            match map_key(key) {
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
                    if !text.trim().is_empty() {
                        {
                            let mut st = state.lock().unwrap();
                            st.push_user(text.clone());
                        }
                        (controller.send)(text);
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
) {
    // snapshot 权威：渲染只读快照（不累积副本）。
    let rows = {
        let mut st = state.lock().unwrap();
        st.flush_streaming();
        st.rows().to_vec()
    };
    let running = state.lock().unwrap().is_running();

    let area = frame.area();
    if screen.mode == crate::fullscreen::ScreenMode::Fullscreen {
        render_fullscreen(frame, area, &rows, input, running, status);
    } else {
        render_regular(frame, area, &rows, input, running, status, scroll_offset);
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

fn render_regular(
    frame: &mut Frame<'_>,
    area: Rect,
    rows: &[TranscriptRow],
    input: &InputBuffer,
    running: bool,
    status: Option<&str>,
    _scroll_offset: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)].as_ref())
        .split(area);

    let transcript = Paragraph::new(transcript_lines(rows))
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false });
    frame.render_widget(transcript, chunks[0]);

    let input_text = input.text();
    let prompt = if running { "⏳ steer > " } else { "> " };
    let input_paragraph = Paragraph::new(Line::from(vec![
        Span::styled(prompt, Style::default().fg(Color::Blue)),
        Span::raw(input_text),
    ]))
    .block(Block::default().borders(Borders::TOP));
    frame.render_widget(input_paragraph, chunks[1]);

    if let Some(status) = status {
        let status_line = Paragraph::new(Span::styled(status, Style::default().fg(Color::Yellow)));
        let bottom = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)].as_ref())
            .split(area);
        frame.render_widget(status_line, bottom[1]);
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
    render_regular(frame, area, &owned, input, running, status, 0);
}
