//! IME 候选窗定位（PRD §4.1 R-TUI-04 / M8-03.A1）。
//!
//! 中文输入法的候选窗跟随**假光标**（终端绘图位置）而非硬件光标。定位链：
//! 输入框获得焦点（焦点容器树传播）→ 假光标行列计算 → 硬件光标同步移到
//! 假光标位置（crossterm MoveTo）→ IME 候选窗出现在正确位置。
//! 本模块是纯坐标/焦点逻辑（终端 IO 在壳层）。

/// 焦点容器树节点（容器焦点传播：焦点进入输入容器时 IME 才需要定位）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusNode {
    /// 输入框（IME 关心）。
    Input { id: &'static str },
    /// 普通容器（transcript/状态栏等）。
    Container {
        id: &'static str,
        focused_child: Option<Box<FocusNode>>,
    },
}

impl FocusNode {
    /// 输入框是否持有焦点（沿容器树传播查找）。
    pub fn input_has_focus(&self) -> bool {
        match self {
            Self::Input { .. } => true,
            Self::Container { focused_child, .. } => focused_child
                .as_ref()
                .is_some_and(|child| child.input_has_focus()),
        }
    }
}

/// 假光标位置（transcript 坐标系：列/行，0-based）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaretPosition {
    pub col: u16,
    pub row: u16,
}

/// 计算假光标位置：输入框 rect 内按文本长度推进列（含折行近似——每
/// `input_width` 列换一行）。
pub fn caret_after_text(
    input_rect: (u16, u16, u16, u16), // (x, y, w, h)
    text_cols: usize,
    input_width: u16,
) -> CaretPosition {
    let (x, y, w, _) = input_rect;
    let width = if input_width == 0 {
        w.max(1)
    } else {
        input_width.min(w)
    };
    let row = (text_cols as u16) / width.max(1);
    let col = (text_cols as u16) % width.max(1);
    CaretPosition {
        col: x + col,
        row: y + row,
    }
}

/// M5-03：inline 模式输入行光标位（贴底行 + 输入光标列；CJK=2 列）。
/// `prompt_cols`=提示符可见宽（> / steer > / !），`badge_cols`=模式徽章可见宽，
/// `text`=输入文本、`cursor`=char 光标位。返回 (col, row) 相对输入行起点。
pub fn inline_caret(
    prompt_cols: usize,
    badge_cols: usize,
    text: &str,
    cursor: usize,
    input_width: usize,
) -> CaretPosition {
    use unicode_width::UnicodeWidthStr;
    let before: String = text.chars().take(cursor).collect();
    let before_cols = UnicodeWidthStr::width(before.as_str());
    let col = prompt_cols + badge_cols + before_cols;
    // input_width 为 0 是防御性边界（终端宽度永远 ≥1 列）；除零不可达但
    // 保留安全回退。clippy 的 checked_div 建议不适用于除+取模成对场景。
    #[allow(clippy::manual_checked_ops)]
    let (row, col_in_row) = if input_width == 0 {
        (col, col)
    } else {
        (col / input_width, col % input_width)
    };
    CaretPosition {
        col: col_in_row as u16,
        row: row as u16,
    }
}

/// 硬件光标同步指令（壳层翻译为 crossterm MoveTo；IME 候选窗随之定位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareCursorSync {
    pub to_col: u16,
    pub to_row: u16,
    pub hidden: bool,
}

/// 输入框聚焦时的硬件光标同步决策：移到假光标并显示。
pub fn sync_for_ime(caret: CaretPosition) -> HardwareCursorSync {
    HardwareCursorSync {
        to_col: caret.col,
        to_row: caret.row,
        hidden: false,
    }
}

/// 失焦时不同步（候选窗不跟随；硬件光标留在原处）。
pub fn no_sync() -> HardwareCursorSync {
    HardwareCursorSync {
        to_col: 0,
        to_row: 0,
        hidden: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M5-03.A2：inline 光标位含 CJK 双宽（IME 候选窗跟随正确列）。
    #[test]
    fn inline_caret_accounts_for_cjk_double_width() {
        let caret = inline_caret(2, 0, "你好", 1, 80); // "你"=2 列，光标在 你|好
        assert_eq!(caret, CaretPosition { col: 4, row: 0 });
        let caret = inline_caret(2, 0, "你好", 2, 80); // 你好| 末尾
        assert_eq!(caret, CaretPosition { col: 6, row: 0 });
        // 窄宽折行：光标落到下一行。
        let caret = inline_caret(2, 0, "你好世界", 2, 4);
        assert_eq!(caret.col, 2, "4 列宽下 '你好' 占满，光标在 | 世界 前换行");
        assert_eq!(caret.row, 1);
    }

    /// M8-03.A1：焦点容器树传播——输入框聚焦可从任意深度判定。
    #[test]
    fn focus_propagates_to_input() {
        let tree = FocusNode::Container {
            id: "root",
            focused_child: Some(Box::new(FocusNode::Container {
                id: "main",
                focused_child: Some(Box::new(FocusNode::Input { id: "composer" })),
            })),
        };
        assert!(tree.input_has_focus());
        // 焦点在兄弟容器：输入框无焦点。
        let tree = FocusNode::Container {
            id: "root",
            focused_child: Some(Box::new(FocusNode::Container {
                id: "sidebar",
                focused_child: None,
            })),
        };
        assert!(!tree.input_has_focus());
    }

    /// M8-03.A1：假光标定位 + 硬件光标同步（IME 候选窗跟随正确位置）。
    #[test]
    fn caret_position_and_hardware_sync() {
        // 空输入：假光标在输入框起点。
        let caret = caret_after_text((10, 20, 40, 3), 0, 40);
        assert_eq!(caret, CaretPosition { col: 10, row: 20 });
        // 7 列文本：右移 7。
        let caret = caret_after_text((10, 20, 40, 3), 7, 40);
        assert_eq!(caret.col, 17);
        assert_eq!(caret.row, 20);
        // 折行：40 列恰好占满首行 → 假光标移到次行首（10, 21）。
        let caret = caret_after_text((10, 20, 40, 3), 40, 40);
        assert_eq!((caret.col, caret.row), (10, 21));
        // 同步指令：聚焦时 MoveTo 假光标并显示。
        let sync = sync_for_ime(caret);
        assert_eq!(
            sync,
            HardwareCursorSync {
                to_col: 10,
                to_row: 21,
                hidden: false
            }
        );
        // 失焦：隐藏不动。
        assert!(no_sync().hidden);
    }
}
