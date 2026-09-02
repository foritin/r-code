//! 已发消息历史（M4-05 / R-HIST-01）。
//!
//! ↑/↓ 与 Ctrl+P/N 翻已发消息；浏览时保留未发送草稿（回到最新时还原）。
//! 相邻重复去重（重复发送同一命令不堆栈）。

/// 历史栈（纯逻辑；发送路径 record，导航取草稿替换）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct History {
    entries: Vec<String>,
    /// 当前浏览位（None = 停在最新/草稿态）。
    position: Option<usize>,
    /// 进入历史浏览时的草稿（回到最新时还原）。
    draft: Option<String>,
}

/// 历史上限（防长会话内存膨胀）。
const HISTORY_LIMIT: usize = 500;

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一条已发送消息（空串与相邻重复不入栈）。
    pub fn record(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.entries.last().is_some_and(|last| last == text) {
            self.position = None;
            self.draft = None;
            return;
        }
        self.entries.push(text.to_string());
        if self.entries.len() > HISTORY_LIMIT {
            self.entries.remove(0);
        }
        self.position = None;
        self.draft = None;
    }

    /// ↑：向旧翻一条；首次进入时保存草稿。返回替换输入的文本。
    pub fn navigate_back(&mut self, current_draft: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.position {
            None => self.entries.len().saturating_sub(1),
            Some(0) => return None,
            Some(position) => position - 1,
        };
        if self.position.is_none() && !current_draft.trim().is_empty() {
            self.draft = Some(current_draft.to_string());
        }
        self.position = Some(next);
        Some(self.entries[next].clone())
    }

    /// ↓：向新翻一条；越过最新回到草稿。
    pub fn navigate_forward(&mut self) -> Option<String> {
        let position = self.position?;
        let newer = position + 1;
        if newer >= self.entries.len() {
            self.position = None;
            return self.draft.take();
        }
        self.position = Some(newer);
        Some(self.entries[newer].clone())
    }

    /// 当前浏览位（测试/诊断用）。
    pub fn position(&self) -> Option<usize> {
        self.position
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl History {
        fn entries_len_for_test(&self) -> usize {
            self.entries.len()
        }
    }

    /// M4-05.A1：↑/↓ 历史导航（草稿保留、相邻去重、空历史 no-op）。
    #[test]
    fn history_navigation_preserves_draft() {
        let mut history = History::new();
        assert_eq!(history.navigate_back("草稿"), None, "空历史 no-op");
        history.record("第一条");
        history.record("第二条");
        history.record("第三条");

        // 进入历史浏览：保存草稿。
        assert_eq!(history.navigate_back("未发草稿"), Some("第三条".into()));
        assert_eq!(history.navigate_back(""), Some("第二条".into()));
        assert_eq!(history.navigate_back(""), Some("第一条".into()));
        assert_eq!(history.navigate_back(""), None, "最旧一条再 ↑ = no-op");
        // ↓ 回新。
        assert_eq!(history.navigate_forward(), Some("第二条".into()));
        assert_eq!(history.navigate_forward(), Some("第三条".into()));
        // 越过最新 → 还原草稿。
        assert_eq!(history.navigate_forward(), Some("未发草稿".into()));
        assert_eq!(history.position(), None, "回到草稿态");

        // 相邻重复去重（草稿已在上面 next() 越过最新时被消费）。
        history.record("same");
        history.record("same");
        assert_eq!(history.navigate_back(""), Some("same".into()));
        assert_eq!(
            history.navigate_back(""),
            Some("第三条".into()),
            "重复不入栈"
        );
        // 空消息不入栈。
        history.record("   ");
        assert_eq!(
            history.entries_len_for_test(),
            4,
            "[一,二,三,same]（去重+空白不入栈）"
        );
    }
}
