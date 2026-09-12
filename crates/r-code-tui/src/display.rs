//! inline 显示模型（commit/live 双区）。
//!
//! - [`transcript_commit_lines`]：transcript 行 → scrollback 区行（含 `\n` 的
//!   拆物理行；**不截断**——超宽自然折行，打印一次永不回访，折行无害）。
//! - [`live_lines`]：底部活动块行（流式预览/浮层/状态/输入），**每行 ANSI
//!   感知截断到终端宽**（live 行必须恰占一物理行，光标算术才成立）。
//!
//! 行内 ANSI 语义色（§2.7）：cyan=强调/选中、green=助手、yellow=警告、
//! red=失败、magenta=模式/命令、dim=辅助、light-red=bash 态提示符。

use crate::app::Overlay;
use crate::input::InputBuffer;
use crate::model_selector::ModelPicker;
use crate::thinking::ThinkingPicker;
use crate::transcript_view::TranscriptView;

fn fg(text: &str, code: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

/// 显示快照输入（渲染所需的全部状态；由 app 循环装配）。
pub struct DisplayInput<'a> {
    pub rows: Vec<crate::TranscriptRow>,
    /// 流式 assistant 预览（未收口内容；live 区呈现，收口后整行 commit）。
    pub streaming: Option<String>,
    pub running: bool,
    pub input: &'a InputBuffer,
    pub status: Option<String>,
    pub queue_block: Vec<String>,
    pub approval_lines: Option<Vec<crate::approval_overlay::OverlayLine>>,
    pub model_label: Option<String>,
    pub mode_badge: Option<(&'static str, crate::task_mode::BadgeColor)>,
    pub usage: Option<crate::status_bar::UsageStats>,
    pub overlay: Option<&'a Overlay>,
    pub slash_menu: Option<&'a crate::slash_menu::SlashMenu>,
    pub transcript_view: &'a TranscriptView,
}

/// scrollback 区：transcript 行拆物理行（含 `\n`），不截断。
pub fn transcript_commit_lines(rows: &[crate::TranscriptRow]) -> Vec<String> {
    let mut lines = Vec::new();
    for row in rows {
        for segment in transcript_row_line(row).split('\n') {
            lines.push(segment.to_string());
        }
    }
    lines
}

/// live 区：流式预览 + 排队 + 审批 + 浮层/菜单 + 状态 + 输入行。
/// 每行截断到 `width` 可视列（ANSI 感知、CJK 双宽）。
pub fn live_lines(view: &DisplayInput<'_>, width: usize) -> Vec<String> {
    // transcript 浮层：占满 live 区（唯一"全屏"语义载体）。
    if view.transcript_view.is_open() {
        let header = fg(&crate::transcript_view::header_line(width), "2");
        // 滚动窗口：scroll = 距底部行偏移——先移除底部 scroll 行，再交由
        // 调用方截底部一屏（scroll=0 即尾锚定，↑/pgup 后窗口随之上移）。
        let mut body = crate::transcript_view::render_rows(&view.rows);
        let visible_end = body.len().saturating_sub(view.transcript_view.scroll());
        body.truncate(visible_end);
        let body = body.into_iter().map(|line| fg(&line, "2"));
        let hints = fg(crate::transcript_view::hints_line(), "2");
        let mut lines: Vec<String> = std::iter::once(header)
            .chain(body)
            .chain(std::iter::once(hints))
            .collect();
        return truncate_live_block(&mut lines, width);
    }

    let mut lines = Vec::new();
    // 流式预览（未收口 assistant；收口后整行进 commit 区）。
    if let Some(text) = &view.streaming {
        if !text.is_empty() {
            lines.push(fg(&format!("R-Code > {text}"), "32;3"));
        }
    }
    // 排队块。
    for line in &view.queue_block {
        lines.push(fg(line, "2"));
    }
    // 审批浮层（最高优先级带面）。
    if let Some(approval) = &view.approval_lines {
        for line in approval {
            let styled = match line.kind {
                crate::approval_overlay::LineKind::Title => fg(&line.text, "1"),
                crate::approval_overlay::LineKind::Command => fg(&line.text, "35"),
                crate::approval_overlay::LineKind::Option => line.text.clone(),
                crate::approval_overlay::LineKind::Hint => fg(&line.text, "2"),
            };
            lines.push(styled);
        }
    }
    // 模型/思考/会话选择浮层或斜杠菜单。
    match view.overlay {
        Some(Overlay::Model(picker)) => lines.extend(model_picker_lines(picker)),
        Some(Overlay::Thinking(picker)) => lines.extend(thinking_picker_lines(picker)),
        Some(Overlay::Resume(picker)) => {
            lines.extend(picker.visible_rows().into_iter().map(|row| fg(&row, "2")))
        }
        Some(Overlay::Setup(flow)) => {
            lines.extend(flow.render_rows().into_iter().map(|row| fg(&row, "2")))
        }
        Some(Overlay::Tree(tree)) => {
            lines.extend(tree.visible_rows().into_iter().map(|row| fg(&row, "2")))
        }
        Some(Overlay::Fork(picker)) => {
            lines.extend(picker.visible_rows().into_iter().map(|row| fg(&row, "2")))
        }
        Some(Overlay::Login(picker)) => {
            lines.extend(picker.visible_rows().into_iter().map(|row| fg(&row, "2")))
        }
        None => {
            if let Some(menu) = view.slash_menu {
                lines.extend(slash_menu_lines(menu));
            }
        }
    }
    // 状态行（瞬态提示）。
    if let Some(status) = &view.status {
        lines.push(fg(status, "33"));
    }
    // 输入区（贴底：prompt + 徽章 + 输入 + 右侧统计/模型标签）。多行/折行
    // 输入产出多条 live 行——每行恰占一物理行，live 块高度随之增长。
    lines.extend(input_lines(view, width));
    truncate_live_block(&mut lines, width)
}

/// live 块截断：每行 ANSI 感知截断到 width；超出一屏的只保留底部
/// `max_rows` 行（浮层/输入贴底最关键）。
fn truncate_live_block(lines: &mut Vec<String>, width: usize) -> Vec<String> {
    for line in lines.iter_mut() {
        *line = truncate_live(line, width);
    }
    std::mem::take(lines)
}

fn transcript_row_line(row: &crate::TranscriptRow) -> String {
    use crate::TranscriptRow;
    match row {
        TranscriptRow::User { text } => fg(&format!("你 > {text}"), "36"),
        TranscriptRow::Assistant { text, complete } => {
            if *complete {
                fg(&format!("R-Code > {text}"), "32")
            } else {
                fg(&format!("R-Code > {text}"), "32;3")
            }
        }
        TranscriptRow::ToolCard {
            name,
            summary,
            is_error,
        } => {
            let line = format!(
                "  [tool] {name} · {summary}{}",
                if *is_error { "（失败）" } else { "" }
            );
            if *is_error {
                fg(&line, "31")
            } else {
                fg(&line, "33")
            }
        }
        TranscriptRow::System { text } => fg(&format!("· {text}"), "2"),
        TranscriptRow::Shell(shell) => match shell {
            crate::bang_command::ShellRow::Prompt { command } => fg(&format!("$ {command}"), "35"),
            crate::bang_command::ShellRow::Output { text, exit_code } => {
                fg(&format!("  {text} (exit {exit_code:?})"), "2")
            }
        },
        TranscriptRow::Image {
            name,
            width,
            height,
            preview,
        } => {
            // G6：头部占位行 + 半块 ANSI 预览块（多物理行由 commit 拆行处理——
            // 预览行自身不含 \n）。
            let mut lines = vec![fg(
                &crate::image_attach::placeholder_line(name, *width, *height),
                "2",
            )];
            lines.extend(preview.iter().cloned());
            lines.join("\n")
        }
    }
}

fn model_picker_lines(picker: &ModelPicker) -> Vec<String> {
    let mut lines = vec![
        // G2 双语义（pi 对齐）：Enter=本次会话，Ctrl+S=设为全局默认。
        fg("enter=本次会话 · ctrl+s=设为默认 · esc=取消", "2"),
        fg(&format!("/model {}", picker.query()), "2"),
    ];
    let selected_row = picker.selected_row().unwrap_or(0);
    let mut row_index = 1usize;
    let mut last_provider: Option<&str> = None;
    for (group, text) in picker.visible_rows().iter() {
        if let Some(provider) = group {
            if last_provider != Some(provider.as_str()) {
                lines.push(fg(provider, "2"));
                last_provider = Some(provider.as_str());
            }
            continue;
        }
        if row_index == selected_row {
            lines.push(fg(&format!("› {text}"), "36;1"));
        } else {
            lines.push(format!("  {text}"));
        }
        row_index += 1;
    }
    lines
}

fn thinking_picker_lines(picker: &ThinkingPicker) -> Vec<String> {
    let selected = picker.selection();
    let mut lines = vec![fg("/thinking（alt+T 打开，alt+, / alt+. 升降）", "2")];
    for level in crate::thinking::EFFORT_LEVELS {
        if level == selected {
            lines.push(fg(&format!("› {level}"), "36;1"));
        } else {
            lines.push(format!("  {level}"));
        }
    }
    lines
}

fn slash_menu_lines(menu: &crate::slash_menu::SlashMenu) -> Vec<String> {
    if menu.is_empty() {
        return vec![fg(crate::slash_menu::no_matches_line(), "2;3")];
    }
    menu.visible_rows()
        .into_iter()
        .map(|(text, selected)| {
            if selected {
                fg(&format!("› {text}"), "36;1")
            } else {
                format!("  {text}")
            }
        })
        .collect()
}

/// 输入行框架件（prompt/徽章/右侧统计与模型标签——统计块只占首物理行）。
struct InputFrame {
    prompt: String,
    prompt_color: &'static str,
    badge: String,
    badge_cols: usize,
    stats_text: String,
    stats_color: &'static str,
    model: String,
}

fn input_frame(view: &DisplayInput<'_>) -> InputFrame {
    let prompt = if view.running {
        "⏳ steer > ".to_string()
    } else {
        "> ".to_string()
    };
    let prompt_color = if crate::bang_command::prompt_semantic(&view.input.text())
        == crate::bang_command::PromptSemantic::Bang
    {
        "91"
    } else {
        "36"
    };
    let badge = view
        .mode_badge
        .map(|(text, color)| {
            let code = match color {
                crate::task_mode::BadgeColor::Cyan => "36",
                crate::task_mode::BadgeColor::Yellow => "33",
                crate::task_mode::BadgeColor::Magenta => "35",
            };
            fg(&format!("{text} "), code)
        })
        .unwrap_or_default();
    let badge_cols = visible_width(&badge);
    // 统计 + 模型标签只拼一次（右侧段）。
    let (stats_text, stats_color) = view
        .usage
        .map(|stats| {
            let (text, threshold) = crate::status_bar::footer_stats_line(&stats, None, false);
            let code = match threshold {
                crate::status_bar::Threshold::Normal => "2",
                crate::status_bar::Threshold::Warning => "33",
                crate::status_bar::Threshold::Error => "31",
            };
            (format!("{text}  "), code)
        })
        .unwrap_or_default();
    InputFrame {
        prompt,
        prompt_color,
        badge,
        badge_cols,
        stats_text,
        stats_color,
        model: view.model_label.clone().unwrap_or_default(),
    }
}

/// 输入文本净化（live 区几何安全）：`\t`/`\r` 折成空格（控制字符宽度不可
/// 控、`\r` 还会被终端解释回行首），`\n` 保留由 wrap_lines 拆行。逐字符
/// 1:1 映射——光标的 char 索引在净化前后保持一致。
fn sanitize_input_text(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\t' | '\r' => ' ',
            other => other,
        })
        .collect()
}

/// 输入区可折行宽度（首行余量：prompt+徽章在左、统计+模型在右，各留 1 列松弛）。
fn input_wrap_width(frame: &InputFrame, width: usize) -> usize {
    let left_cols = visible_width(&frame.prompt) + frame.badge_cols;
    let right_cols = visible_width(&frame.stats_text) + visible_width(&frame.model);
    width.saturating_sub(left_cols + right_cols + 1).max(1)
}

/// 输入区行（多行安全）：净化 + wrap_lines 折行，一条 live 行 = 一个物理行。
/// 首行带 prompt/徽章 + 右侧统计；续行仅文本。
pub fn input_lines(view: &DisplayInput<'_>, width: usize) -> Vec<String> {
    let frame = input_frame(view);
    let wrap_width = input_wrap_width(&frame, width);
    let wrapped = crate::input::wrap_lines(&sanitize_input_text(&view.input.text()), wrap_width);
    let right_cols = visible_width(&frame.stats_text) + visible_width(&frame.model);
    let mut lines = Vec::with_capacity(wrapped.len());
    for (index, segment) in wrapped.iter().enumerate() {
        if index == 0 {
            let left = format!(
                "{}{}{}",
                fg(&frame.prompt, frame.prompt_color),
                frame.badge,
                segment
            );
            let padding = width.saturating_sub(visible_width(&left) + right_cols + 1);
            lines.push(format!(
                "{}{}{}{}",
                left,
                " ".repeat(padding),
                fg(&frame.stats_text, frame.stats_color),
                fg(&frame.model, "2")
            ));
        } else {
            lines.push(segment.clone());
        }
    }
    lines
}

/// 输入光标位（live 区坐标，多行/折行安全）：返回 `(row_from_bottom, col)`
/// ——输入块贴 live 块尾部，`row_from_bottom` 相对块尾（0 = 最后一行）。
/// 折行与 [`input_lines`] 同口径（贪心 wrap 前缀行 == 全文行到光标处）。
pub fn input_caret_pos(view: &DisplayInput<'_>, width: usize) -> (usize, u16) {
    let frame = input_frame(view);
    let left_cols = visible_width(&frame.prompt) + frame.badge_cols;
    let wrap_width = input_wrap_width(&frame, width);
    let text = sanitize_input_text(&view.input.text());
    let before: String = text.chars().take(view.input.cursor()).collect();
    let before_rows = crate::input::wrap_lines(&before, wrap_width);
    let total_rows = crate::input::wrap_lines(&text, wrap_width).len();
    let row_index = before_rows.len() - 1;
    let row_from_bottom = total_rows - 1 - row_index;
    let mut col = before_rows
        .last()
        .map(|row| visible_width(row))
        .unwrap_or(0);
    if row_index == 0 {
        col += left_cols;
    }
    (row_from_bottom, col.min(width.saturating_sub(1)) as u16)
}

/// 兼容入口：仅取光标列（单行输入与旧口径等价）。
pub fn input_caret_col(view: &DisplayInput<'_>, width: usize) -> u16 {
    input_caret_pos(view, width).1
}

/// ANSI 感知截断到 `width` 可视列（超宽尾部加 `…`）。live 行专用。
pub fn truncate_live(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if visible_width(text) <= width {
        // 仅去尾随空白（padding 类）。
        return text.trim_end().to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            out.push(ch);
            if let Some(&'[') = chars.peek() {
                out.push(chars.next().unwrap());
                for inner in chars.by_ref() {
                    out.push(inner);
                    if inner.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        let w = cjk_width(ch);
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// 去 ANSI 后的可视宽度（CJK 按宽表计）。
pub fn visible_width(text: &str) -> usize {
    strip_ansi(text).chars().map(cjk_width).sum()
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for inner in chars.by_ref() {
                    if inner.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn cjk_width(ch: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    ch.width().unwrap_or(0).max(if ch == ' ' { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_view<'a>(
        input: &'a InputBuffer,
        transcript_view: &'a TranscriptView,
    ) -> DisplayInput<'a> {
        DisplayInput {
            rows: vec![],
            streaming: None,
            running: false,
            input,
            status: None,
            queue_block: vec![],
            approval_lines: None,
            model_label: None,
            mode_badge: None,
            usage: None,
            overlay: None,
            slash_menu: None,
            transcript_view,
        }
    }

    /// G6：图片行 = dim 占位头 + 半块预览块逐行原样输出（ANSI 不截断）。
    #[test]
    fn image_row_renders_placeholder_header_and_preview() {
        let row = crate::TranscriptRow::Image {
            name: "shot.png".to_string(),
            width: 100,
            height: 50,
            preview: vec!["\x1b[38;2;1;2;3m\x1b[48;2;4;5;6m▀\x1b[0m".to_string()],
        };
        let lines = transcript_commit_lines(std::slice::from_ref(&row));
        assert_eq!(lines.len(), 2, "头行 + 1 预览行：{lines:?}");
        assert!(lines[0].contains("[图片 shot.png 100x50]"), "{lines:?}");
        assert!(lines[1].contains('▀'), "{lines:?}");
        // 空预览 = 仅占位头（重建历史的元数据形态）。
        let empty = crate::TranscriptRow::Image {
            name: "shot.png".to_string(),
            width: 0,
            height: 0,
            preview: Vec::new(),
        };
        assert_eq!(transcript_commit_lines(&[empty]).len(), 1);
    }

    /// commit 区：含 \n 的行拆物理行；超宽**不截断**（wrap 无害，打印一次）。
    #[test]
    fn commit_lines_split_newlines_and_keep_long_text() {
        let rows = vec![crate::TranscriptRow::System {
            text: "两种配置途径：\n  1) 桌面端设置\n  2) 直接编辑 config.toml".into(),
        }];
        let lines = transcript_commit_lines(&rows);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[2].contains("config.toml"));
        // 超宽保留（scrollback 自然折行，无回访所以无害）。
        let long = crate::TranscriptRow::Assistant {
            text: "x".repeat(500),
            complete: true,
        };
        let lines = transcript_commit_lines(&[long]);
        assert_eq!(lines.len(), 1);
        assert!(visible_width(&lines[0]) > 400, "长文不得截断");
    }

    /// live 区：超宽行 ANSI 感知截断（每行恰占一物理行）。
    #[test]
    fn live_lines_truncated_to_width() {
        let input = InputBuffer::new();
        let transcript_view = TranscriptView::new();
        let mut view = base_view(&input, &transcript_view);
        view.status = Some(format!("很长的状态 {}", "字".repeat(80)));
        let lines = live_lines(&view, 40);
        assert_eq!(lines.len(), 2, "状态 + 输入：{lines:?}");
        for line in &lines {
            assert!(visible_width(line) <= 40, "行宽必须 ≤ 40：{line:?}");
        }
    }

    /// 输入行统计只出现一次（实测截图 "used ↑0 ↓0 used" 重复回归）。
    #[test]
    fn input_line_stats_not_duplicated() {
        let input = InputBuffer::new();
        let transcript_view = TranscriptView::new();
        let mut view = base_view(&input, &transcript_view);
        view.usage = Some(crate::status_bar::UsageStats {
            input_tokens: 1000,
            output_tokens: 900,
            ..Default::default()
        });
        let text = strip_ansi(&input_lines(&view, 80).join("\n"));
        assert_eq!(text.matches("↑1.0K").count(), 1, "token 段唯一：{text}");
        assert_eq!(text.matches("1.9K used").count(), 1, "占用段唯一：{text}");
    }

    /// 多行/折行输入：一行 live 行 = 一个物理行（\t/\r 净化成空格、\n 拆行），
    /// 光标行号随 wrap 行定位（live 块高度与光标算术的几何前提）。
    #[test]
    fn multi_line_input_wraps_into_one_live_row_per_physical_row() {
        let mut input = InputBuffer::new();
        input.insert_str("第一行");
        input.newline();
        input.insert_str("第二行\t尾");
        let transcript_view = TranscriptView::new();
        let view = base_view(&input, &transcript_view);
        let lines = input_lines(&view, 40);
        assert_eq!(lines.len(), 2, "两个逻辑行 = 两条 live 行：{lines:?}");
        assert!(lines[0].contains("第一行"), "{lines:?}");
        assert!(
            lines[1].contains("第二行") && lines[1].contains("尾"),
            "{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains('\n') || line.contains('\t')),
            "live 行不得内嵌 \\n/\\t：{lines:?}"
        );
        // 光标在末行（"第二行 尾"之后）→ row_from_bottom=0，列=该行宽（无 prompt）。
        assert_eq!(
            input_caret_pos(&view, 40),
            (0, 9),
            "第二行=6 + 空格=1 + 尾=2"
        );
        // 光标回到第一行 → row_from_bottom=1，列=prompt(2)+第一行(6)。
        drop(view);
        input.move_home(); // 当前行行首（第二行）
        input.move_left(); // 越过 \n 到第一行行尾
        let view = base_view(&input, &transcript_view);
        assert_eq!(
            input_caret_pos(&view, 40),
            (1, 2 + 6),
            "{:?}/{}",
            input.cursor(),
            input.text()
        );
        // \r 净化：不破坏行数（不产生额外物理行）。
        let mut cr_input = InputBuffer::new();
        cr_input.insert_str("a\r");
        cr_input.insert('b');
        let view = base_view(&cr_input, &transcript_view);
        assert_eq!(input_lines(&view, 20).len(), 1, "\\r 折成空格不拆行");
    }

    /// 长行软折行：超出首行余量的文本折成多条 live 行，光标随行推进。
    #[test]
    fn long_input_soft_wraps_and_caret_follows() {
        let mut input = InputBuffer::new();
        input.insert_str(&"x".repeat(40));
        let transcript_view = TranscriptView::new();
        let view = base_view(&input, &transcript_view);
        // width=20：wrap_width = 20 - 2(prompt) - 1 = 17 → 40 字符折 3 行。
        let lines = input_lines(&view, 20);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            input_caret_pos(&view, 20),
            (0, 40 - 2 * 17),
            "光标在末 wrap 行"
        );
    }

    /// transcript 浮层滚动：scroll>0 时窗口上移（不再永久尾锚定）。
    #[test]
    fn transcript_overlay_windows_body_by_scroll() {
        let input = InputBuffer::new();
        let rows: Vec<crate::TranscriptRow> = (0..30)
            .map(|index| crate::TranscriptRow::System {
                text: format!("行{index}"),
            })
            .collect();
        // 尾锚定（scroll=0）：最新行可见。
        let mut anchored = TranscriptView::new();
        anchored.open();
        let mut view = base_view(&input, &anchored);
        view.rows = rows.clone();
        let lines = live_lines(&view, 80);
        assert!(
            lines.iter().any(|line| line.contains("行29")),
            "尾锚定必须包含最新行：{lines:?}"
        );
        // 上滚 2 行：底部 2 行移出窗口，顶部行仍在。
        let mut scrolled = TranscriptView::new();
        scrolled.open();
        scrolled.scroll_up(rows.len());
        scrolled.scroll_up(rows.len());
        let mut view = base_view(&input, &scrolled);
        view.rows = rows;
        let lines = live_lines(&view, 80);
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("行29") || line.contains("行28")),
            "滚动后底部行必须移出窗口：{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("行0")),
            "顶部行仍可见：{lines:?}"
        );
    }

    /// 流式预览进 live 区；commit 行不重复它。
    #[test]
    fn streaming_preview_in_live_block() {
        let input = InputBuffer::new();
        let transcript_view = TranscriptView::new();
        let mut view = base_view(&input, &transcript_view);
        view.streaming = Some("部分回答".into());
        view.running = true;
        let lines = live_lines(&view, 80);
        assert!(
            lines.iter().any(|l| l.contains("R-Code > 部分回答")),
            "{lines:?}"
        );
    }

    /// 输入光标列 = prompt + 徽章 + 光标前文本可见宽（CJK 双宽），钳在宽度内。
    #[test]
    fn input_caret_col_accounts_for_cjk() {
        let mut input = InputBuffer::new();
        input.insert_str("你好");
        let transcript_view = TranscriptView::new();
        let mut view = base_view(&input, &transcript_view);
        assert_eq!(input_caret_col(&view, 80), 2 + 4); // "> "=2 + 你好=4
        view.mode_badge = Some(("[plan]", crate::task_mode::BadgeColor::Magenta));
        assert_eq!(input_caret_col(&view, 80), 2 + 7 + 4); // [plan]=6 + 空格=1
    }

    /// M5-02.A4：M1-M4 组件面在行模型下仍投影（审批带/排队/模式徽章/菜单）。
    #[test]
    fn display_assembly_covers_all_milestone_surfaces() {
        let input = InputBuffer::new();
        let transcript_view = TranscriptView::new();
        let mut view = base_view(&input, &transcript_view);
        view.rows = vec![crate::TranscriptRow::System {
            text: "错误进 transcript".into(),
        }];
        view.running = true;
        view.status = Some("已请求中止…".into());
        view.queue_block = vec!["• Queued follow-up inputs".into()];
        view.approval_lines = Some(crate::approval_overlay::overlay_lines(
            &crate::approval_overlay::PendingApproval {
                request_id: "r".into(),
                tool_name: "bash".into(),
                command: "cargo test".into(),
                risk: r_code_core::dto::RiskLevel::R2,
                op_id: None,
            },
        ));
        view.mode_badge = Some(("[plan]", crate::task_mode::BadgeColor::Magenta));
        let menu = crate::slash_menu::SlashMenu::new("/zzz");
        view.slash_menu = Some(&menu);
        let commit = transcript_commit_lines(&view.rows);
        let lines = live_lines(&view, 100);
        let body = format!("{}\n{}", commit.join("\n"), lines.join("\n"));
        assert!(body.contains("· 错误进 transcript"), "{body}");
        assert!(body.contains("• Queued follow-up inputs"), "{body}");
        assert!(body.contains("是否允许执行以下命令？"), "{body}");
        assert!(body.contains("[plan]"), "{body}");
        assert!(body.contains("no matches"), "{body}");
    }
}
