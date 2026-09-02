//! 交互输入缓冲与按键→动作映射（R-TUI-01 阶段 1：输入 + 发送/steer/abort）。
//!
//! 纯逻辑（无终端 IO）可单测：`InputBuffer` 维护光标位置与文本编辑；
//! `map_key` 把 crossterm KeyEvent 归一为 `KeyAction`，渲染壳层据此行动。
//! IME 候选窗定位复用 `ime` 模块的假光标坐标。

/// 输入缓冲（光标位置 + 文本编辑；多字节安全按 char 计）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputBuffer {
    chars: Vec<char>,
    /// 光标位置（char 索引，0..=len）。
    cursor: usize,
}

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

    pub fn insert(&mut self, ch: char) {
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        for ch in text.chars() {
            self.insert(ch);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete_forward(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// 取走当前文本并清空（发送/steer 用）。
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.chars.clear();
        self.cursor = 0;
        text
    }
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
    match key.code {
        KeyCode::Char(ch) if ctrl && (ch == 'c' || ch == 'C') => KeyAction::Abort,
        KeyCode::Char(ch) if ctrl && (ch == 'd' || ch == 'D') => KeyAction::Quit,
        KeyCode::Char('/') if ctrl => KeyAction::ToggleSearch,
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
