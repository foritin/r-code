//! transcript 浮层（M4-05 / R-HIST-01，Ctrl+T）。
//!
//! 唯一"全屏"语义载体（PRD §2.4：无独立 fullscreen 模式）。codex 形态：
//! 顶行 `/ T R A N S C R I P T / ...` dim + 底部 hints；内容 = transcript
//! 全量（工具卡展开含错误态）。q/esc 关闭，↑↓/pgup/pgdn 滚动。

use crate::TranscriptRow;

/// 浮层视图状态（scroll = 距底部的行偏移；0 = 锚定最新）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TranscriptView {
    open: bool,
    scroll: usize,
}

impl TranscriptView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
        self.scroll = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// ↑（向旧滚动）。
    pub fn scroll_up(&mut self, total_lines: usize) {
        if self.scroll < total_lines {
            self.scroll += 1;
        }
    }

    /// ↓（向新滚动；0 = 底部锚定）。
    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }

    /// 整页滚动。
    pub fn page_up(&mut self, total_lines: usize, page: usize) {
        self.scroll = (self.scroll + page).min(total_lines);
    }

    pub fn page_down(&mut self, page: usize) {
        self.scroll = self.scroll.saturating_sub(page);
    }
}

/// 顶行（codex 快照形态：`/ T R A N S C R I P T / / / / …` 按宽度铺满）。
pub fn header_line(width: usize) -> String {
    let title = "/ T R A N S C R I P T /";
    if width <= title.chars().count() {
        return title.to_string();
    }
    let fill = " /".repeat((width - title.chars().count()) / 2);
    format!("{title}{fill}")
}

/// 底部 hints 行。
pub fn hints_line() -> &'static str {
    "↑/↓ to scroll   pgup/pgdn to page   q to quit   esc to edit prev"
}

/// 全量展开渲染行（工具卡含摘要与错误态；shell 段原样）。
pub fn render_rows(rows: &[TranscriptRow]) -> Vec<String> {
    let mut lines = Vec::new();
    for row in rows {
        match row {
            TranscriptRow::User { text } => lines.push(format!("› {text}")),
            TranscriptRow::Assistant { text, .. } => lines.push(format!("• {text}")),
            TranscriptRow::ToolCard {
                name,
                summary,
                is_error,
            } => lines.push(format!(
                "  ⏺ {name} · {summary}{}",
                if *is_error { "（失败）" } else { "" }
            )),
            TranscriptRow::System { text } => lines.push(format!("· {text}")),
            TranscriptRow::Image {
                name,
                width,
                height,
                ..
            } => lines.push(format!(
                "🖼 {}",
                crate::image_attach::placeholder_line(name, *width, *height)
            )),
            TranscriptRow::Shell(shell) => match shell {
                crate::bang_command::ShellRow::Prompt { command } => {
                    lines.push(format!("$ {command}"))
                }
                crate::bang_command::ShellRow::Output { text, exit_code } => {
                    lines.push(format!("  {text} (exit {exit_code:?})"))
                }
            },
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M4-05.A2：浮层开合与滚动钳位（↑ 顶部钳、↓ 底部锚定 0、翻页钳位）。
    #[test]
    fn transcript_view_open_close_and_scroll_clamp() {
        let mut view = TranscriptView::new();
        assert!(!view.is_open());
        view.open();
        assert!(view.is_open());
        assert_eq!(view.scroll(), 0, "打开即锚定底部");
        view.scroll_up(10);
        view.scroll_up(10);
        assert_eq!(view.scroll(), 2);
        for _ in 0..20 {
            view.scroll_up(10);
        }
        assert_eq!(view.scroll(), 10, "顶部钳在总行数");
        view.page_up(10, 5);
        assert_eq!(view.scroll(), 10, "翻页同钳位");
        view.page_down(5);
        assert_eq!(view.scroll(), 5);
        view.page_down(99);
        assert_eq!(view.scroll(), 0, "底部锚定 0");
        view.close();
        assert!(!view.is_open());
        view.toggle();
        assert!(view.is_open());
        view.toggle();
        assert!(!view.is_open());
    }

    /// M4-05.A3：顶行/hints 行快照（codex 形态）。
    #[test]
    fn header_and_hints_match_codex_shape() {
        let header = header_line(60);
        assert!(
            header.starts_with("/ T R A N S C R I P T /"),
            "顶行标题：{header}"
        );
        assert!(header.ends_with("/"), "铺满斜线收尾：{header}");
        assert_eq!(header_line(10), "/ T R A N S C R I P T /", "窄宽不截断");
        assert_eq!(
            hints_line(),
            "↑/↓ to scroll   pgup/pgdn to page   q to quit   esc to edit prev"
        );
    }

    /// M4-05.A4：浮层内容 = 全量展开（工具卡含错误态、shell 退出码、用户/助手前缀）。
    #[test]
    fn render_rows_expand_tools_and_shell() {
        let rows = vec![
            TranscriptRow::User {
                text: "跑一下".into(),
            },
            TranscriptRow::ToolCard {
                name: "bash".into(),
                summary: "cargo test".into(),
                is_error: false,
            },
            TranscriptRow::ToolCard {
                name: "edit".into(),
                summary: "lib.rs".into(),
                is_error: true,
            },
            TranscriptRow::Shell(crate::bang_command::ShellRow::Output {
                text: "ok".into(),
                exit_code: Some(0),
            }),
            TranscriptRow::Assistant {
                text: "完成".into(),
                complete: true,
            },
        ];
        let lines = render_rows(&rows);
        let body = lines.join("\n");
        assert!(body.contains("⏺ bash · cargo test"), "{body}");
        assert!(
            body.contains("⏺ edit · lib.rs（失败）"),
            "错误态必须可见：{body}"
        );
        assert!(
            body.contains("$ ") || body.contains("(exit Some(0))"),
            "{body}"
        );
        assert!(
            body.contains("› 跑一下") && body.contains("• 完成"),
            "{body}"
        );
    }
}
