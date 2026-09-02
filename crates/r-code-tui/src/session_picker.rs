//! /resume 会话列表（M6-01 / R-SESS-01）。
//!
//! 数据源 = 宿主 `task_list`（共享 data-dir 的 tasks，与桌面互通）。codex 形态：
//! `❯` 光标（区别于列表 `›`）、双行行目（Created at / Updated at）、底行 hints
//! `enter to resume   esc to start new`。enter 接续 = 关闭列表，把选中 task 交
//! 壳层 resume（JSONL 重建 transcript）。

use r_code_core::dto::Task;

/// 一行会话（展示投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub task_id: String,
    pub title: String,
    pub mode: String,
    pub created: String,
    pub updated: String,
}

/// Task → 展示投影（时间截断到分钟，缺省回落占位）。
pub fn entry_from_task(task: &Task) -> SessionEntry {
    let stamp = |value: &str| {
        if value.len() > 16 {
            value[..16].to_string()
        } else {
            value.to_string()
        }
    };
    SessionEntry {
        task_id: task.id.clone(),
        title: if task.title.trim().is_empty() {
            "（未命名会话）".to_string()
        } else {
            task.title.clone()
        },
        mode: task.mode.to_string(),
        created: stamp(&task.created_at.to_rfc3339()),
        updated: stamp(&task.updated_at.to_rfc3339()),
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

    /// 渲染行（`❯` 光标 + 双行行目 + 列头 + 底行 hints）。
    pub fn visible_rows(&self) -> Vec<String> {
        if self.entries.is_empty() {
            return vec!["（没有可恢复的会话）".to_string()];
        }
        let mut rows =
            vec!["  Created at        Updated at        Mode    Conversation".to_string()];
        for (index, entry) in self.entries.iter().enumerate() {
            let cursor = if index == self.selected { "❯" } else { " " };
            rows.push(format!(
                "{cursor} {}  {}  {:>6}  {}",
                entry.created, entry.updated, entry.mode, entry.title
            ));
        }
        rows.push("  enter to resume     esc to start new     ↑/↓ to move".to_string());
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, title: &str) -> Task {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "title": title,
            "goal": "",
            "mode": "ask",
            "state": "idle",
            "created_at": chrono::Utc::now().to_rfc3339(),
            "updated_at": chrono::Utc::now().to_rfc3339(),
        }))
        .expect("task from json")
    }

    /// M6-01.A1：列表投影（列头/双行行目/❯ 光标/排序 + 上下移动钳位）。
    #[test]
    fn session_picker_projects_and_clamps() {
        let mut picker = SessionPicker::new(vec![
            entry_from_task(&task("t1", "第一个")),
            entry_from_task(&task("t2", "第二个")),
        ]);
        let rows = picker.visible_rows();
        assert_eq!(
            rows[0],
            "  Created at        Updated at        Mode    Conversation"
        );
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

    /// M6-01.A1：空列表渲染空态。
    #[test]
    fn empty_picker_renders_empty_state() {
        let picker = SessionPicker::new(vec![]);
        assert!(picker.is_empty());
        let rows = picker.visible_rows();
        assert_eq!(rows, vec!["（没有可恢复的会话）".to_string()]);
    }
}
