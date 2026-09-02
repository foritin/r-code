//! inline 渲染器（commit/live 双区模型，2026-09-03 重构）。
//!
//! 架构（pi / codex cli 同款）：
//! - **scrollback 区**：transcript 历史行**只打印一次**（append-only），自然滚入
//!   终端 scrollback，永不重写、不参与任何光标算术——退出终端后历史保留。
//! - **live 区**：屏幕底部活动块（流式预览/浮层/状态/输入行），每帧原位重绘：
//!   cursor-up 到块首 → 逐行清写 → `ED` 清残留。
//!
//! 为什么不用全量行差分：历史超过一屏后，写入 `\r\n` 使终端滚动、块顶滚出
//! 屏幕，`\x1b[{n}A` 的"光标在块尾"前提失效——光标落到任意位置重写，整屏
//! 撕裂（2026-09-03 用户实测截图根因）。commit/live 分区从结构上消除该类
//! 错误：commit 行永不回访，live 区高度恒小于一屏且由调用方截断保证每行
//! 恰占一物理行。

/// CSI ?2026 同步输出包裹（防闪烁）。
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

/// commit/live 双区渲染器。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InlineRenderer {
    /// 上一帧 live 区占用的物理行数（0 = 尚未绘制/已失效）。
    live_height: usize,
}

impl InlineRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 渲染一帧。
    ///
    /// - `commit`：新增 transcript 行——在 live 区上方打印一次（进入
    ///   scrollback，永不重写）。允许自然折行（超宽由终端 wrap，无回访）。
    /// - `live`：底部活动块行——原位重绘。调用方必须保证每行不含 `\n` 且
    ///   可视宽度 ≤ 终端宽（每行恰占一物理行，光标算术才成立）。
    pub fn frame(&mut self, commit: &[String], live: &[String]) -> String {
        let mut out = String::from(SYNC_BEGIN);
        // 1) 光标回到 live 区块首（上一帧画完后光标停在块尾下一行）。
        if self.live_height > 0 {
            out.push_str(&format!("\x1b[{}A\r", self.live_height));
        }
        // 2) 新历史行打印一次。写在旧 live 区顶部：旧 live 是 stale 内容，
        //    下面的 live 重绘 + ED 会清理。终端按需滚动，行进 scrollback。
        for line in commit {
            out.push_str(line);
            out.push_str("\r\n");
        }
        // 3) live 区原位重写 + ED 清掉收缩残留。
        for line in live {
            out.push_str("\x1b[2K");
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.push_str("\x1b[J");
        self.live_height = live.len();
        out.push_str(SYNC_END);
        out
    }

    /// 硬件光标放回 live 区行（`row_from_bottom`：0 = 最后一行）的 `col` 列，
    /// 供 IME 候选窗跟随/输入定位。
    pub fn cursor_to_live(&self, row_from_bottom: usize, col: usize) -> String {
        // 画完帧后光标在块尾下一行：上移 row_from_bottom + 1 行到达目标行。
        let up = row_from_bottom + 1;
        format!("\x1b[{up}A\x1b[{col}G")
    }

    /// 终端尺寸变化：live 区几何失效（旧行位置不可信）。下一帧从当前光标处
    /// 起一块新 live 区，旧内容随滚动自然让位。
    pub fn invalidate(&mut self) {
        self.live_height = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 首帧：无光标上移；commit 行 + live 行按序写入；CSI 2026 包裹。
    #[test]
    fn first_frame_prints_commit_then_live() {
        let mut r = InlineRenderer::new();
        let out = r.frame(&["h1".into()], &["› input".into()]);
        assert!(out.starts_with("\x1b[?2026h"), "{out:?}");
        assert!(out.contains("h1\r\n"), "{out:?}");
        assert!(out.contains("\x1b[2K› input\r\n"), "live 行清写：{out:?}");
        assert!(out.ends_with("\x1b[J\x1b[?2026l"), "{out:?}");
    }

    /// 第二帧无新历史：只重绘 live 区（cursor-up = 上帧 live 高度）。
    #[test]
    fn live_only_frame_moves_up_by_previous_height() {
        let mut r = InlineRenderer::new();
        r.frame(&["h1".into()], &["a".into(), "b".into()]);
        let out = r.frame(&[], &["a".into(), "b2".into()]);
        assert!(out.contains("\x1b[2A\r"), "上移 2 行到块首：{out:?}");
        assert!(out.contains("\x1b[2Kb2"), "变化行清写：{out:?}");
        assert!(!out.contains("h1"), "历史行永不重写：{out:?}");
    }

    /// live 收缩：ED 清掉残留尾行。
    #[test]
    fn shrink_clears_tail() {
        let mut r = InlineRenderer::new();
        r.frame(&[], &["a".into(), "b".into(), "c".into()]);
        let out = r.frame(&[], &["a".into()]);
        assert!(out.contains("\x1b[3A\r"), "{out:?}");
        assert!(out.ends_with("\x1b[J\x1b[?2026l"), "ED 清残留：{out:?}");
    }

    /// 历史提交：commit 行打印一次后不再出现在后续帧。
    #[test]
    fn committed_lines_never_rewritten() {
        let mut r = InlineRenderer::new();
        r.frame(&["h1".into(), "h2".into()], &["› x".into()]);
        let out = r.frame(&["h3".into()], &["› xy".into()]);
        // 上移 1 行（live 高度）→ 打印 h3 → live 重绘。
        assert!(out.contains("\x1b[1A\r"), "{out:?}");
        assert!(out.contains("h3\r\n"), "{out:?}");
        assert!(
            !out.contains("h1") && !out.contains("h2"),
            "旧历史不重写：{out:?}"
        );
    }

    /// 光标定位：画完 3 行 live 块后，row_from_bottom=0 → 上移 1 行。
    #[test]
    fn cursor_to_live_rows_from_bottom() {
        let mut r = InlineRenderer::new();
        r.frame(&[], &["a".into(), "b".into(), "c".into()]);
        assert_eq!(r.cursor_to_live(0, 5), "\x1b[1A\x1b[5G");
        assert_eq!(r.cursor_to_live(2, 0), "\x1b[3A\x1b[0G");
    }

    /// invalidate：live 高度归零，下一帧无光标上移。
    #[test]
    fn invalidate_resets_live_height() {
        let mut r = InlineRenderer::new();
        r.frame(&[], &["a".into(), "b".into()]);
        r.invalidate();
        let out = r.frame(&[], &["a".into()]);
        assert!(!out.contains("\x1b[2A"), "失效后不上移：{out:?}");
    }
}
