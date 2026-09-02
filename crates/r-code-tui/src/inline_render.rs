//! 自研 inline 行差分渲染核心（M5-01 PoC / M5-02 落地基座）。
//!
//! 渲染单位 = 行数组（pi 同款前提）。每次 update 产出一段 ANSI 序列：
//! 整体包在 CSI ?2026 同步输出里防闪烁；仅重写 first_changed..last_changed；
//! append-only 尾部直接续写（历史行自然滚入终端 scrollback——退出即保留）。
//! 纯逻辑无终端 IO：单测与基准共用同一差分引擎。

/// 行差分渲染器。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InlineRenderer {
    prev: Vec<String>,
}

/// CSI ?2026 同步输出包裹。
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

impl InlineRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 上一帧行数（光标复位基准；测试与基准用）。
    pub fn previous_height(&self) -> usize {
        self.prev.len()
    }

    /// 产出下一帧的 ANSI 写入序列（写完光标停在新块首行行首）。
    pub fn update(&mut self, next: &[String]) -> String {
        let mut out = String::from(SYNC_BEGIN);
        // 公共前后缀求差分区间。
        let prefix = self
            .prev
            .iter()
            .zip(next.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self
            .prev
            .iter()
            .rev()
            .zip(next.iter().rev())
            .take_while(|(a, b)| a == b)
            .count()
            .min(self.prev.len().saturating_sub(prefix))
            .min(next.len().saturating_sub(prefix));
        let prev_len = self.prev.len();
        let first_changed = prefix;
        let last_changed = next.len().saturating_sub(suffix);

        if next.len() >= prev_len && prefix >= prev_len {
            // append-only：光标已在块尾，直接续写新行。
            for line in &next[prev_len..] {
                out.push_str(line);
                out.push_str("\r\n");
            }
        } else if next.is_empty() {
            // 清空块：上移整块并清除。
            if prev_len > 0 {
                out.push_str(&cursor_up(prev_len));
            }
            out.push_str("\x1b[J");
        } else {
            // 行内变化：上移到块首，重写差分区间，其余行跳过（↓）。
            out.push_str(&cursor_up(prev_len));
            for (index, line) in next.iter().enumerate() {
                if index < first_changed || index >= last_changed {
                    out.push_str("\x1b[1B"); // 未变行：下移跳过（不重写）
                } else {
                    out.push_str("\x1b[2K"); // 清行后重写
                    out.push_str(line);
                    out.push_str("\x1b[1B\r");
                }
            }
            // 回到块首（下一帧基准）。
            if !next.is_empty() {
                out.push_str(&cursor_up(next.len()));
            }
        }
        out.push_str(SYNC_END);
        self.prev = next.to_vec();
        out
    }
}

fn cursor_up(lines: usize) -> String {
    if lines == 0 {
        String::new()
    } else {
        format!("\x1b[{lines}A\r")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 首帧全量；CSI ?2026 全程包裹。
    #[test]
    fn first_frame_writes_all_wrapped_in_sync() {
        let mut renderer = InlineRenderer::new();
        let out = renderer.update(&["a".into(), "b".into()]);
        assert!(out.starts_with("\x1b[?2026h"), "{out:?}");
        assert!(out.ends_with("\x1b[?2026l"), "{out:?}");
        assert!(out.contains("a") && out.contains("b"));
        assert!(!out.contains("\x1b[1A"), "首帧无光标上移：{out:?}");
    }

    /// append-only：只写新增行（历史滚入 scrollback），无整块重绘。
    #[test]
    fn append_only_writes_new_lines_without_repaint() {
        let mut renderer = InlineRenderer::new();
        renderer.update(&["h1".into(), "spinner".into()]);
        let out = renderer.update(&["h1".into(), "spinner".into(), "h2".into()]);
        assert!(out.contains("h2"), "{out:?}");
        assert!(!out.contains("h1"), "未变历史行不得重写：{out:?}");
        assert!(!out.contains("\x1b[1A"), "append-only 无光标上移：{out:?}");
        assert!(out.contains("\x1b[?2026h"), "同步输出包裹：{out:?}");
    }

    /// 行内变化（spinner）：仅重写该行（清行 + 内容），未变行只跳过。
    #[test]
    fn spinner_change_rewrites_only_that_line() {
        let mut renderer = InlineRenderer::new();
        renderer.update(&["h1".into(), "h2".into(), "⠋ working".into(), "input".into()]);
        let out = renderer.update(&["h1".into(), "h2".into(), "⠙ working".into(), "input".into()]);
        assert!(out.contains("⠙"), "{out:?}");
        assert!(
            !out.contains("h1") && !out.contains("input\r"),
            "未变行不重写：{out:?}"
        );
        assert!(out.contains("\x1b[4A"), "上移整块 4 行：{out:?}");
        assert!(out.contains("\x1b[2K"), "清行重写：{out:?}");
    }

    /// 块清空：上移 + ED 清除。
    #[test]
    fn clearing_wipes_block() {
        let mut renderer = InlineRenderer::new();
        renderer.update(&["a".into(), "b".into()]);
        let out = renderer.update(&[]);
        assert!(out.contains("\x1b[2A") && out.contains("\x1b[J"), "{out:?}");
        assert_eq!(renderer.previous_height(), 0);
    }
}
