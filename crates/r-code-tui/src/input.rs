//! 交互输入缓冲与按键→动作映射（R-TUI-01 阶段 1：输入 + 发送/steer/abort）。
//!
//! 纯逻辑（无终端 IO）可单测：`InputBuffer` 维护光标位置与文本编辑；
//! `map_key` 把 crossterm KeyEvent 归一为 `KeyAction`，渲染壳层据此行动。
//! IME 候选窗定位复用 `ime` 模块的假光标坐标。

/// 编辑快照（undo/redo 栈元素）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    chars: Vec<char>,
    cursor: usize,
}

/// 多行编辑器内核（M4-01：显式换行、undo/redo、词导航、grapheme 边界安全）。
///
/// 存储按 char；退格/前删以 grapheme 簇为原子（不撕开组合字符）；折行宽度
/// 核算 CJK=2 列（MC-8）。纯逻辑无终端 IO。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputBuffer {
    chars: Vec<char>,
    /// 光标位置（char 索引，0..=len）。
    cursor: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

/// 单个快照上限（防长会话内存膨胀）。
const UNDO_LIMIT: usize = 200;

impl InputBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// 行数（空缓冲 = 1 行）。
    pub fn line_count(&self) -> usize {
        1 + self.chars.iter().filter(|ch| **ch == '\n').count()
    }

    fn snapshot(&mut self) {
        self.redo.clear();
        self.undo.push(Snapshot {
            chars: self.chars.clone(),
            cursor: self.cursor,
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    pub fn insert(&mut self, ch: char) {
        self.snapshot();
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        self.snapshot();
        for ch in text.chars() {
            self.chars.insert(self.cursor, ch);
            self.cursor += 1;
        }
    }

    /// 显式换行（Shift+Enter / Ctrl+J）。
    pub fn newline(&mut self) {
        self.insert('\n');
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.snapshot();
        // grapheme 原子删除：找到光标前一个 grapheme 簇的起点。
        let start = self.prev_grapheme_start();
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn delete_forward(&mut self) {
        if self.cursor >= self.chars.len() {
            return;
        }
        self.snapshot();
        let end = self.next_grapheme_end();
        self.chars.drain(self.cursor..end);
    }

    /// 光标前一个 grapheme 的起点（char 索引；grapheme_indices 返回字节索引，
    /// 必须经 chars().count() 换算，不能与 char 光标直接比较）。
    fn prev_grapheme_start(&self) -> usize {
        use unicode_segmentation::UnicodeSegmentation;
        let text = self.text();
        let mut boundary = 0;
        for (byte_index, _) in text.grapheme_indices(true) {
            let char_index = text[..byte_index].chars().count();
            if char_index < self.cursor {
                boundary = char_index;
            } else {
                break;
            }
        }
        boundary
    }

    /// 光标所在 grapheme 的终点（char 索引）。
    fn next_grapheme_end(&self) -> usize {
        self.next_grapheme_end_from(self.cursor)
    }

    fn next_grapheme_end_from(&self, from: usize) -> usize {
        use unicode_segmentation::UnicodeSegmentation;
        let text = self.text();
        for (byte_index, grapheme) in text.grapheme_indices(true) {
            let char_index = text[..byte_index].chars().count();
            if char_index >= from {
                return char_index + grapheme.chars().count();
            }
        }
        self.chars.len()
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        self.redo.push(Snapshot {
            chars: std::mem::take(&mut self.chars),
            cursor: self.cursor,
        });
        self.chars = snapshot.chars;
        self.cursor = snapshot.cursor;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.undo.push(Snapshot {
            chars: std::mem::take(&mut self.chars),
            cursor: self.cursor,
        });
        self.chars = snapshot.chars;
        self.cursor = snapshot.cursor;
        true
    }

    /// 词左移：跳过空白后到词首（词 = 字母数字/下划线/CJK 连续段）。
    pub fn move_word_left(&mut self) {
        let mut index = self.cursor;
        while index > 0 && self.chars[index - 1].is_whitespace() {
            index -= 1;
        }
        while index > 0 && self.is_word_char(self.chars[index - 1]) {
            index -= 1;
        }
        self.cursor = index;
    }

    /// 词右移：跳过当前词尾与空白后到下一词首。
    pub fn move_word_right(&mut self) {
        let len = self.chars.len();
        let mut index = self.cursor;
        while index < len && self.is_word_char(self.chars[index]) {
            index += 1;
        }
        while index < len && self.chars[index].is_whitespace() {
            index += 1;
        }
        self.cursor = index;
    }

    fn is_word_char(&self, ch: char) -> bool {
        ch.is_alphanumeric() || ch == '_'
    }

    pub fn move_left(&mut self) {
        self.cursor = self.prev_grapheme_start();
    }

    pub fn move_right(&mut self) {
        // 右移到下一个 grapheme 边界（越过整个簇，不撕开组合字符）。
        self.cursor = self.next_grapheme_end().min(self.chars.len());
    }

    /// 行首（当前行；单行缓冲 = 0）。
    pub fn move_home(&mut self) {
        let mut index = self.cursor;
        while index > 0 && self.chars[index - 1] != '\n' {
            index -= 1;
        }
        self.cursor = index;
    }

    /// 行尾（当前行；单行缓冲 = len）。
    pub fn move_end(&mut self) {
        let len = self.chars.len();
        let mut index = self.cursor;
        while index < len && self.chars[index] != '\n' {
            index += 1;
        }
        self.cursor = index;
    }

    /// 取走当前文本并清空（发送/steer 用；可 undo 找回）。
    pub fn take(&mut self) -> String {
        self.snapshot();
        let text = self.text();
        self.chars.clear();
        self.cursor = 0;
        text
    }
}

/// CJK 感知折行（MC-8：宽度一律按 unicode-width，CJK=2 列；纯逻辑可单测）。
/// 超过宽度的单个 grapheme 独占一行（不截断）。
pub fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        if grapheme == "\n" {
            lines.push(std::mem::take(&mut current));
            used = 0;
            continue;
        }
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if used + grapheme_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push_str(grapheme);
        used += grapheme_width;
    }
    lines.push(current);
    lines
}

/// 按键归一化后的动作（渲染壳层消费）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// 可打印字符（含中文等多字节）→ 插入缓冲。
    Insert(char),
    /// 退格。
    Backspace,
    /// Delete。
    DeleteForward,
    /// 光标左/右/行首/行尾。
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
    /// 发送（Enter）。运行中 = steer，空闲 = 新 run。
    Send,
    /// 中止当前运行（Ctrl-C）；空闲时 = 退出。
    Abort,
    /// 退出（Esc / Ctrl-D 空缓冲）。
    Quit,
    /// 滚动 transcript。
    ScrollUp,
    ScrollDown,
    /// fullscreen/regular 切换。
    ToggleFullscreen,
    /// 打开/关闭全文搜索（fullscreen 态）。
    ToggleSearch,
    /// 打开思考级别选择器（alt+T）。
    ToggleThinking,
    /// 思考降一档（alt+,）。
    ThinkingDown,
    /// 思考升一档（alt+.）。
    ThinkingUp,
    /// TaskMode 循环（Shift+Tab）。
    CycleMode,
    /// 显式换行（Shift+Enter / Ctrl+J）。
    Newline,
    /// 撤销（Ctrl+Z）。
    Undo,
    /// 重做（Ctrl+Y）。
    Redo,
    /// 词左移（Ctrl+Left）。
    WordLeft,
    /// 词右移（Ctrl+Right）。
    WordRight,
    /// 忽略（未映射键）。
    Ignore,
}

/// crossterm KeyEvent → KeyAction（键位与桌面 App 惯用一致）。
pub fn map_key(key: crossterm::event::KeyEvent) -> KeyAction {
    use crossterm::event::{KeyCode, KeyModifiers};
    // Windows/kitty 协议下同一按键会上报 Press 与 Release 两个事件
    //（v1 未过滤导致每键双写，调研报告 §4 差距 #6）。仅 Press 与 Repeat
    // 产生动作；Release 一律忽略——Repeat 保留长按重发语义（退格连删）。
    match key.kind {
        crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat => {}
        crossterm::event::KeyEventKind::Release => return KeyAction::Ignore,
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char(ch) if ctrl && (ch == 'c' || ch == 'C') => KeyAction::Abort,
        KeyCode::Char(ch) if ctrl && (ch == 'd' || ch == 'D') => KeyAction::Quit,
        KeyCode::Char('/') if ctrl => KeyAction::ToggleSearch,
        KeyCode::Enter if shift => KeyAction::Newline,
        KeyCode::Char('j') | KeyCode::Char('J') if ctrl => KeyAction::Newline,
        KeyCode::Char('z') | KeyCode::Char('Z') if ctrl => KeyAction::Undo,
        KeyCode::Char('y') | KeyCode::Char('Y') if ctrl => KeyAction::Redo,
        KeyCode::Left if ctrl => KeyAction::WordLeft,
        KeyCode::Right if ctrl => KeyAction::WordRight,
        KeyCode::Char('t') | KeyCode::Char('T') if alt => KeyAction::ToggleThinking,
        KeyCode::Char(',') if alt => KeyAction::ThinkingDown,
        KeyCode::Char('.') if alt => KeyAction::ThinkingUp,
        KeyCode::F(10) => KeyAction::ToggleFullscreen,
        KeyCode::Char(ch) => KeyAction::Insert(ch),
        KeyCode::BackTab => KeyAction::CycleMode,
        KeyCode::Enter => KeyAction::Send,
        KeyCode::Backspace => KeyAction::Backspace,
        KeyCode::Delete => KeyAction::DeleteForward,
        KeyCode::Left => KeyAction::CursorLeft,
        KeyCode::Right => KeyAction::CursorRight,
        KeyCode::Home => KeyAction::CursorHome,
        KeyCode::End => KeyAction::CursorEnd,
        KeyCode::Up => KeyAction::ScrollUp,
        KeyCode::Down => KeyAction::ScrollDown,
        KeyCode::Esc => KeyAction::Quit,
        _ => KeyAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// 输入缓冲：插入/退格/删除/光标移动/取走（多字节安全）。
    #[test]
    fn input_buffer_editing_is_multibyte_safe() {
        let mut buf = InputBuffer::new();
        buf.insert_str("你好a");
        assert_eq!(buf.text(), "你好a");
        assert_eq!(buf.cursor(), 3);
        buf.backspace();
        assert_eq!(buf.text(), "你好");
        buf.move_home();
        buf.insert('前');
        assert_eq!(buf.text(), "前你好");
        buf.move_end();
        buf.delete_forward();
        assert_eq!(buf.text(), "前你好");
        assert_eq!(buf.take(), "前你好");
        assert!(buf.is_empty());
        assert_eq!(buf.cursor(), 0);
    }

    /// 按键映射：Enter 发送、Ctrl-C abort、Esc 退出、可打印插入、F10 切全屏。
    #[test]
    fn key_mapping_actions() {
        let key = |code| map_key(KeyEvent::new(code, KeyModifiers::NONE));
        let ctrl = |code| map_key(KeyEvent::new(code, KeyModifiers::CONTROL));
        assert_eq!(key(KeyCode::Enter), KeyAction::Send);
        assert_eq!(key(KeyCode::Esc), KeyAction::Quit);
        assert_eq!(key(KeyCode::Up), KeyAction::ScrollUp);
        assert_eq!(key(KeyCode::F(10)), KeyAction::ToggleFullscreen);
        assert_eq!(ctrl(KeyCode::Char('c')), KeyAction::Abort);
        assert_eq!(ctrl(KeyCode::Char('d')), KeyAction::Quit);
        assert_eq!(ctrl(KeyCode::Char('/')), KeyAction::ToggleSearch);
        assert_eq!(key(KeyCode::Char('你')), KeyAction::Insert('你'));
        assert_eq!(key(KeyCode::Backspace), KeyAction::Backspace);
        assert_eq!(key(KeyCode::Delete), KeyAction::DeleteForward);
        assert_eq!(key(KeyCode::Left), KeyAction::CursorLeft);
        assert_eq!(key(KeyCode::Right), KeyAction::CursorRight);
        assert_eq!(key(KeyCode::Home), KeyAction::CursorHome);
        assert_eq!(key(KeyCode::End), KeyAction::CursorEnd);
    }

    /// M4-01.A1：多行编辑与显式换行（newline/backspace 跨行合并/take 全文）。
    #[test]
    fn multi_line_editing_with_explicit_newline() {
        let mut buf = InputBuffer::new();
        buf.insert_str("第一行");
        buf.newline();
        buf.insert_str("第二行");
        assert_eq!(buf.text(), "第一行\n第二行");
        assert_eq!(buf.line_count(), 2);
        // 行首/行尾是当前行语义。
        buf.move_home();
        assert_eq!(buf.cursor(), "第一行\n".chars().count(), "行首=当前行起点");
        buf.move_end();
        assert_eq!(buf.cursor(), buf.text().chars().count());
        // 退格跨行合并。
        let mut joiner = InputBuffer::new();
        joiner.insert_str("a\nb");
        joiner.move_home();
        joiner.backspace(); // 删掉 \n → 两行合并
        assert_eq!(joiner.text(), "ab");
        assert_eq!(joiner.line_count(), 1);
        // take 返回完整多行文本。
        let mut taker = InputBuffer::new();
        taker.insert_str("x\ny");
        assert_eq!(taker.take(), "x\ny");
    }

    /// M4-01.A2：undo/redo（编辑序列回退/重做；栈底/栈顶安全）。
    #[test]
    fn undo_redo_roundtrip() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.insert('b');
        buf.newline();
        buf.insert('c');
        assert_eq!(buf.text(), "ab\nc");
        assert!(buf.undo(), "undo 1: 回到 ab\n");
        assert_eq!(buf.text(), "ab\n");
        assert!(buf.undo(), "undo 2: 回到 ab");
        assert_eq!(buf.text(), "ab");
        assert!(buf.undo(), "undo 3: 回到 a");
        assert_eq!(buf.text(), "a");
        assert!(buf.undo(), "undo 4: 回到空");
        assert!(buf.is_empty());
        assert!(!buf.undo(), "栈底再 undo = no-op");
        assert!(buf.redo(), "redo: a");
        assert_eq!(buf.text(), "a");
        assert!(buf.redo(), "redo: ab");
        assert_eq!(buf.text(), "ab");
        assert!(!buf.redo() || buf.text() == "ab\n", "栈顶边界安全");
        // take 后 undo 找回。
        let mut sent = InputBuffer::new();
        sent.insert_str("draft");
        sent.take();
        assert!(sent.undo());
        assert_eq!(sent.text(), "draft", "发送后可 undo 找回草稿");
    }

    /// M4-01.A3：词导航 + CJK/grapheme 折行边界。
    #[test]
    fn word_navigation_and_cjk_wrap_boundaries() {
        let mut buf = InputBuffer::new();
        buf.insert_str("hello 世界 word");
        let end = buf.cursor();
        buf.move_word_left();
        assert_eq!(
            buf.cursor(),
            "hello 世界 ".chars().count(),
            "词左移到 word 词首"
        );
        buf.move_word_left();
        assert_eq!(buf.cursor(), "hello ".chars().count(), "CJK 连续段是一个词");
        buf.move_word_right();
        assert_eq!(buf.cursor(), "hello 世界 ".chars().count());
        buf.move_word_right();
        assert_eq!(buf.cursor(), end, "词右移到文本尾");

        // grapheme 原子退格：组合字符（é = e + U+0301）一次删完。
        let mut combined = InputBuffer::new();
        combined.insert_str("e\u{0301}x");
        combined.backspace(); // 删 x
        combined.backspace(); // 删整个 é（e+组合符）
        assert_eq!(combined.text(), "", "组合字符不撕裂：{:?}", combined.text());

        // CJK 折行：宽度按 2 列计，超宽簇独占一行。
        let lines = wrap_lines("你好world", 6);
        assert_eq!(
            lines,
            vec!["你好wo".to_string(), "rld".to_string()],
            "你好=4 列 + w/o 各 1 列恰满 6；rld 换行"
        );
        let wide = wrap_lines("ab\n你好", 1);
        assert_eq!(
            wide,
            vec!["a", "b", "你", "好"],
            "显式换行分片 + 宽 1 时超宽 CJK 逐字独占行（贪心不截断）"
        );
    }

    /// M4-01.A4：光标移动/编辑不越界。
    #[test]
    fn cursor_never_escapes_bounds() {
        let mut buf = InputBuffer::new();
        buf.move_left();
        assert_eq!(buf.cursor(), 0, "空缓冲左移不越界");
        buf.move_right();
        assert_eq!(buf.cursor(), 0, "空缓冲右移不越界");
        buf.backspace();
        buf.delete_forward();
        assert!(buf.is_empty(), "空缓冲编辑 no-op");
        buf.insert_str("a\nb");
        buf.move_home();
        for _ in 0..10 {
            buf.move_left();
        }
        assert_eq!(buf.cursor(), 0, "连按左移钳在 0");
        buf.move_end();
        for _ in 0..10 {
            buf.move_right();
        }
        assert_eq!(buf.cursor(), buf.text().chars().count(), "连按右移钳在行尾");
        // 光标在行 1 尾再右移 = 跨过换行符到行 2 首。
        buf.move_left(); // 3 → 2（行 2 首）
        buf.move_left(); // 2 → 1（\n 上）
        buf.move_left(); // 1 → 0（行 1 首）
        buf.move_end(); // → 行 1 尾（cursor=1）
        buf.move_right(); // 越过 \n → 行 2 首
        assert_eq!(buf.cursor(), 2, "a\n|b——越过换行符");
    }

    /// M1-02.A1：Release 事件不产生任何动作（Windows/kitty 双写根因）。
    #[test]
    fn release_events_do_not_produce_actions() {
        use crossterm::event::{KeyEventKind, KeyEventState};
        let release = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };
        assert_eq!(map_key(release(KeyCode::Char('a'))), KeyAction::Ignore);
        assert_eq!(map_key(release(KeyCode::Enter)), KeyAction::Ignore);
        assert_eq!(map_key(release(KeyCode::Backspace)), KeyAction::Ignore);
        assert_eq!(map_key(release(KeyCode::Esc)), KeyAction::Ignore);
    }

    /// M1-02.A2：一次物理按键（Press+Release 成对）恰好映射一个动作；
    /// Repeat 保留（长按重发）。
    #[test]
    fn press_events_produce_exactly_one_action() {
        use crossterm::event::{KeyEventKind, KeyEventState};
        let press = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        let repeat = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Repeat,
            state: KeyEventState::NONE,
        };
        let release = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };
        for code in [KeyCode::Char('a'), KeyCode::Enter, KeyCode::Backspace] {
            let actions = [map_key(press(code)), map_key(release(code))]
                .into_iter()
                .filter(|action| !matches!(action, KeyAction::Ignore))
                .count();
            assert_eq!(
                actions, 1,
                "press+release pair must yield one action for {code:?}"
            );
        }
        // Repeat 与 Press 同语义（退格长按连删）。
        assert_eq!(map_key(repeat(KeyCode::Backspace)), KeyAction::Backspace);
    }
}
