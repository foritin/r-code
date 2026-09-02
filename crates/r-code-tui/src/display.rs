//! inline 显示模型（M5-02）：把 TuiState/输入/浮层组装成行数组，
//! 交 `inline_render::InlineRenderer` 行差分输出。
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

/// 组装完整显示行（inline 模式唯一渲染入口）。
pub fn display_lines(view: &DisplayInput<'_>, width: usize) -> Vec<String> {
    // transcript 浮层：占满整屏（唯一"全屏"语义载体）。
    if view.transcript_view.is_open() {
        let mut lines = vec![fg(&crate::transcript_view::header_line(width), "2")];
        lines.extend(
            crate::transcript_view::render_rows(&view.rows)
                .into_iter()
                .map(|line| fg(&line, "2")),
        );
        lines.push(fg(crate::transcript_view::hints_line(), "2"));
        return lines;
    }

    let mut lines = Vec::new();
    // 历史区（进 scrollback 后不再重写——append-only 差分天然满足）。
    for row in &view.rows {
        lines.push(transcript_row_line(row));
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
    // 输入行（贴底：prompt + 徽章 + 输入 + 右侧统计/模型标签）。
    lines.push(input_line(view, width));
    // M6-03 修复：所有行拆物理行 + 截断（diff 引擎要求"行=物理行"）。
    normalize_lines(&lines, width)
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
    }
}

fn model_picker_lines(picker: &ModelPicker) -> Vec<String> {
    let mut lines = vec![fg(&format!("/model {}", picker.query()), "2")];
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

fn input_line(view: &DisplayInput<'_>, width: usize) -> String {
    let prompt = if view.running { "⏳ steer > " } else { "> " };
    let prompt_color = if crate::bang_command::prompt_semantic(&view.input.text())
        == crate::bang_command::PromptSemantic::Bang
    {
        "91"
    } else {
        "36"
    };
    let badge = view.mode_badge.map(|(text, color)| {
        let code = match color {
            crate::task_mode::BadgeColor::Cyan => "36",
            crate::task_mode::BadgeColor::Yellow => "33",
            crate::task_mode::BadgeColor::Magenta => "35",
        };
        fg(&format!("{text} "), code)
    });
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
    let left = format!(
        "{}{}{}",
        fg(prompt, prompt_color),
        badge.unwrap_or_default(),
        view.input.text()
    );
    // right = 模型标签（stats 已作为独立右段拼一次，避免 "used ↑0 ↓0 used" 重复）。
    let right = view.model_label.clone().unwrap_or_default();
    let left_len = plain_len(&left);
    let right_len = plain_len(&right) + plain_len(&stats_text);
    let padding = width
        .saturating_sub(left_len + right_len + 1)
        .saturating_sub(1);
    format!(
        "{}{}{}{}",
        left,
        " ".repeat(padding),
        fg(&stats_text, stats_color),
        fg(&right, "2")
    )
}

/// M6-03 修复：把含 `\n` 的显示行拆成多物理行、超宽行截断、去尾随空白。
/// 差分引擎假设"一行 = 一个终端物理行"，任何多物理行/自动折行都会让
/// `\x1b[{n}A` / `\x1b[1B` 光标跳行错位（截图撕裂根因）。
pub fn normalize_lines(lines: &[String], width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines.iter() {
        for segment in split_physical(line, width) {
            out.push(segment);
        }
    }
    out
}

/// 拆 `\n` 并逐段截断到 width（CJK 双宽 + ANSI 感知；超宽截断加省略号）。
pub fn split_physical(line: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for segment in line.split('\n') {
        let mut current = segment.to_string();
        // 去尾随空白（消费多余的 padding）。
        loop {
            let stripped = strip_ansi(&current);
            if stripped.len() <= width {
                break;
            }
            if let Some(ch) = current.pop() {
                let _ = ch;
            } else {
                break;
            }
        }
        if plot_width(&current) > width {
            current = truncate_styled(&current, width);
        }
        result.push(current);
    }
    result
}

/// 可视宽度（ANSI 感知 + CJK 双宽）。
fn plot_width(text: &str) -> usize {
    strip_ansi(text).chars().map(cjk_width).sum()
}

/// 保留 ANSI 序列前提下截断到 width 可视列（尾部加 `…`）。
fn truncate_styled(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            out.push(ch);
            // 原样复制 ANSI 转义序列。
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
        if used + w > width.saturating_sub(1) && used > 0 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// 去 ANSI 后的可视宽度（CJK 按宽表计）。
fn plain_len(text: &str) -> usize {
    let stripped = strip_ansi(text);
    stripped.chars().map(cjk_width).sum()
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

    /// M6-03 复现：输入行统计只能出现一次（截图 "used ↑0 ↓0 used" 重复是 stats 双拼）。
    #[test]
    fn input_line_stats_not_duplicated() {
        let input = InputBuffer::new();
        let view = DisplayInput {
            rows: vec![],
            running: false,
            input: &input,
            status: None,
            queue_block: vec![],
            approval_lines: None,
            model_label: None,
            mode_badge: None,
            usage: Some(crate::status_bar::UsageStats {
                input_tokens: 1000,
                output_tokens: 900,
                ..Default::default()
            }),
            overlay: None,
            slash_menu: None,
            transcript_view: &TranscriptView::new(),
        };
        let line = input_line(&view, 80);
        let text = strip_ansi(&line);
        // footer_stats_line 一次调用产出 tokens 段 + 占用段，二者都应各出现一次。
        let token_count = text.matches("↑1.0K").count();
        let used_count = text.matches("1.9K used").count();
        assert_eq!(token_count, 1, "token 段不重复：{text}");
        assert_eq!(used_count, 1, "占用段不重复：{text}");
    }

    /// M6-03 复现：含 \n 的引导行必须拆成多物理行（否则差分光标错位）。
    #[test]
    fn embedded_newlines_split_into_physical_lines() {
        let lines = vec![fg(
            "R-Code CLI 尚未配置模型服务\n  1) 桌面端设置\n  2) 直接编辑 config.toml",
            "2",
        )];
        let normalized = normalize_lines(&lines, 80);
        assert_eq!(
            normalized.len(),
            3,
            "引导行含 2 个换行应拆成 3 行：{normalized:?}"
        );
    }

    /// M6-03 复现：超宽行必须截断（否则终端自动折行 → 差分引擎光标跳行错位）。
    #[test]
    fn overwide_lines_are_truncated() {
        let long = "x".repeat(200);
        let lines = vec![fg(&long, "36")];
        let normalized = normalize_lines(&lines.clone(), 60);
        assert_eq!(normalized.len(), 1, "单行超宽截断为 1 物理行");
        let text = strip_ansi(&normalized[0]);
        assert!(
            text.chars().count() <= 61,
            "截断后含省略号 ≤ 宽+1：{text:?} (len={})",
            text.chars().count()
        );
    }

    /// M5-02.A3：resize 稳定——输入行贴底且宽度自适应（窄宽不越界、统计/标签右对齐）。
    #[test]
    fn input_line_stays_bottom_and_fits_width() {
        let mut input = InputBuffer::new();
        input.insert_str("hello");
        let view = DisplayInput {
            rows: vec![],
            running: false,
            input: &input,
            status: None,
            queue_block: vec![],
            approval_lines: None,
            model_label: Some("(demo) m".into()),
            mode_badge: None,
            usage: None,
            overlay: None,
            slash_menu: None,
            transcript_view: &TranscriptView::new(),
        };
        let line = input_line(&view, 40);
        assert!(line.contains("hello"), "输入贴底：{line}");
        // 窄宽不 panic（宽度小于内容时 padding 钳 0）。
        let narrow = input_line(&view, 5);
        assert!(!narrow.contains("  "), "窄宽不产生负 padding 异常");
    }

    /// M5-02.A4：M1-M4 组件面在 inline 行模型下仍投影（语义色 + 审批带 + 菜单 no matches）。
    #[test]
    fn display_assembly_covers_all_milestone_surfaces() {
        let input = InputBuffer::new();
        let view = DisplayInput {
            rows: vec![crate::TranscriptRow::System {
                text: "错误进 transcript".into(),
            }],
            running: true,
            input: &input,
            status: Some("已请求中止…".into()),
            queue_block: vec!["• Queued follow-up inputs".into()],
            approval_lines: Some(crate::approval_overlay::overlay_lines(
                &crate::approval_overlay::PendingApproval {
                    request_id: "r".into(),
                    tool_name: "bash".into(),
                    command: "cargo test".into(),
                    risk: r_code_core::dto::RiskLevel::R2,
                },
            )),
            model_label: Some("(demo) m • high".into()),
            mode_badge: Some(("[plan]", crate::task_mode::BadgeColor::Magenta)),
            usage: Some(crate::status_bar::UsageStats {
                input_tokens: 1000,
                output_tokens: 900,
                ..Default::default()
            }),
            overlay: None,
            slash_menu: Some(&crate::slash_menu::SlashMenu::new("/zzz")),
            transcript_view: &TranscriptView::new(),
        };
        let lines = display_lines(&view, 100);
        let body = lines.join("\n");
        assert!(body.contains("· 错误进 transcript"), "{body}");
        assert!(body.contains("• Queued follow-up inputs"), "{body}");
        assert!(body.contains("是否允许执行以下命令？"), "{body}");
        assert!(body.contains("[plan]"), "{body}");
        assert!(body.contains("no matches"), "斜杠菜单无命中行：{body}");
        // ANSI 语义色存在（cyan/green/magenta）。
        assert!(
            body.contains("\x1b[35m") || body.contains("\x1b[36m"),
            "语义色存在：{body:?}"
        );
    }
}
