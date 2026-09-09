//! /resume 会话列表（M6-01 / R-SESS-01；T35 起数据源 = 守护进程
//! `task.list`，与桌面 GUI 共享同一 v2 任务库）。
//!
//! codex 形态：`❯` 光标（区别于列表 `›`）、列头（Updated at / State /
//! Conversation）、底行 hints `enter to resume   esc to start new`。
//! enter 接续 = 关闭列表，把选中 task 交壳层 resume（journal 事件重放重建
//! transcript）。

use crate::engine::TaskSummary;

/// 一行会话（展示投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub task_id: String,
    pub title: String,
    pub state: String,
    pub updated: String,
}

/// 守护进程 TaskSummary → 展示投影（时间截断到分钟，缺省回落占位）。
pub fn entry_from_summary(task: &TaskSummary) -> SessionEntry {
    let stamp = chrono::DateTime::from_timestamp_millis(task.updated_at_ms)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".to_string());
    SessionEntry {
        task_id: task.task_id.clone(),
        title: if task.title.trim().is_empty() {
            "（未命名会话）".to_string()
        } else {
            task.title.clone()
        },
        state: if task.running {
            "running".to_string()
        } else {
            task.state.clone()
        },
        updated: stamp,
    }
}

/// 选择器状态（纯逻辑）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SessionPicker {
    entries: Vec<SessionEntry>,
    selected: usize,
}

impl SessionPicker {
    pub fn new(entries: Vec<SessionEntry>) -> Self {
        Self {
            entries,
            selected: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    pub fn selection(&self) -> Option<&SessionEntry> {
        self.entries.get(self.selected)
    }

    /// 渲染行（`❯` 光标 + 列头 + 底行 hints）。
    pub fn visible_rows(&self) -> Vec<String> {
        if self.entries.is_empty() {
            return vec!["（没有可恢复的会话）".to_string()];
        }
        let mut rows = vec!["  Updated at        State           Conversation".to_string()];
        for (index, entry) in self.entries.iter().enumerate() {
            let cursor = if index == self.selected { "❯" } else { " " };
            rows.push(format!(
                "{cursor} {}  {:<15}  {}",
                entry.updated, entry.state, entry.title
            ));
        }
        rows.push("  enter to resume     esc to start new     ↑/↓ to move".to_string());
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, title: &str, updated_ms: i64) -> TaskSummary {
        TaskSummary {
            task_id: id.to_string(),
            title: title.to_string(),
            kind: "conversation".to_string(),
            state: "pending".to_string(),
            running: false,
            updated_at_ms: updated_ms,
        }
    }

    /// M6-01.A1：列表投影（列头/❯ 光标 + 上下移动钳位）。
    #[test]
    fn session_picker_projects_and_clamps() {
        let now = chrono::Utc::now().timestamp_millis();
        let mut picker = SessionPicker::new(vec![
            entry_from_summary(&summary("t1", "第一个", now)),
            entry_from_summary(&summary("t2", "第二个", now)),
        ]);
        let rows = picker.visible_rows();
        assert_eq!(rows[0], "  Updated at        State           Conversation");
        assert!(rows[1].starts_with("❯"), "默认选中第一条：{rows:?}");
        assert!(rows[2].starts_with(' '), "未选中项无 ❯：{rows:?}");
        assert!(rows.last().unwrap().contains("enter to resume"), "{rows:?}");
        assert_eq!(picker.selection().unwrap().task_id, "t1");
        picker.move_down();
        assert_eq!(picker.selection().unwrap().task_id, "t2");
        for _ in 0..5 {
            picker.move_down();
        }
        assert_eq!(picker.selection().unwrap().task_id, "t2", "下移钳位");
        for _ in 0..5 {
            picker.move_up();
        }
        assert_eq!(picker.selection().unwrap().task_id, "t1", "上移钳位");
    }

    /// M6-01.A1：空标题回退占位；空列表渲染空态。
    #[test]
    fn empty_picker_renders_empty_state() {
        let picker = SessionPicker::new(vec![]);
        assert!(picker.is_empty());
        let rows = picker.visible_rows();
        assert_eq!(rows, vec!["（没有可恢复的会话）".to_string()]);
        let entry = entry_from_summary(&summary("t", "   ", 0));
        assert_eq!(entry.title, "（未命名会话）");
    }
}
