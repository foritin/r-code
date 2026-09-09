//! 会话树（G8：/tree 分支导航 + /fork 消息级分叉选择器；T35 起数据源 =
//! 守护进程 `task.branches` + journal 投影）。
//!
//! v2 语义：pi 的树是 session 文件内 entry 树；本仓 v2 的分支 = **分支任务**
//!（`task.clone` 派生的子任务，`parent_task_id` 谱系）。当前任务自身即
//! main 分支；其子任务为子分支。Enter 切换到 main = 幂等（提示"已切换到
//! 分支 main"）；子分支切换 = 接管对应任务（事件重放重建 transcript）。
//! /fork 的消息级"回到某条消息重试"由 TranscriptEvent 的 user 消息选择补齐
//!（选消息 → 新分支任务 + 改写文本发进去）。

/// 分支展示投影 + 树深度（父分支不在列表 = 根；main 恒为根）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchNode {
    pub id: String,
    pub parent_branch_id: Option<String>,
    /// 分叉自哪条消息（`{message_id}`；main 无）。
    pub forked_from_message_id: Option<String>,
    /// 创建时间（MM-DD HH:mm）。
    pub created: String,
    pub is_active: bool,
    pub depth: usize,
}

/// v2 分支行（`task.branches` + 当前任务派生；由壳层装配）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    pub id: String,
    pub parent_id: Option<String>,
    pub is_active: bool,
    pub created: String,
}

/// `BranchInfo` → 树节点（按 parent 链算深度；孤儿分支按根处理）。
pub fn branch_nodes(branches: &[BranchInfo]) -> Vec<BranchNode> {
    let by_id: std::collections::HashMap<&str, &BranchInfo> = branches
        .iter()
        .map(|branch| (branch.id.as_str(), branch))
        .collect();
    let depth_of = |id: &str, seen: &mut Vec<String>| -> usize {
        let mut depth = 0usize;
        let mut current = id.to_string();
        while let Some(branch) = by_id.get(current.as_str()) {
            let Some(parent) = branch.parent_id.clone() else {
                break;
            };
            if seen.contains(&parent) || !by_id.contains_key(parent.as_str()) {
                break;
            }
            seen.push(parent.clone());
            current = parent;
            depth += 1;
        }
        depth
    };
    branches
        .iter()
        .map(|branch| BranchNode {
            id: branch.id.clone(),
            parent_branch_id: branch.parent_id.clone(),
            forked_from_message_id: None,
            created: branch.created.clone(),
            is_active: branch.is_active,
            depth: depth_of(&branch.id, &mut Vec::new()),
        })
        .collect()
}

/// /tree 分支树选择器（❯ 光标 + 树形缩进 + 活跃标记）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BranchTree {
    nodes: Vec<BranchNode>,
    selected: usize,
}

impl BranchTree {
    pub fn new(branches: &[BranchInfo]) -> Self {
        // 列表按创建时间倒序（装配序）；树形展示按时间正序（旧分支在上）。
        let mut ordered: Vec<BranchInfo> = branches.to_vec();
        ordered.reverse();
        let nodes = branch_nodes(&ordered);
        let selected = nodes
            .iter()
            .position(|node| node.is_active)
            .unwrap_or(0)
            .min(nodes.len().saturating_sub(1));
        Self { nodes, selected }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.nodes.len() {
            self.selected += 1;
        }
    }

    /// 选中分支 id（Enter 语义：切到该分支任务；已活跃则幂等提示）。
    pub fn selection(&self) -> Option<&BranchNode> {
        self.nodes.get(self.selected)
    }

    /// 渲染行（树形：深度 × "  " 缩进 + `└` 分叉标记；`❯` 光标 + `*` 活跃）。
    pub fn visible_rows(&self) -> Vec<String> {
        if self.nodes.is_empty() {
            return vec!["（当前会话还没有分支）".to_string()];
        }
        let mut rows = vec!["  分支               创建        说明".to_string()];
        for (index, node) in self.nodes.iter().enumerate() {
            let cursor = if index == self.selected { "❯" } else { " " };
            let id = short_id(&node.id);
            let fork_note = node
                .forked_from_message_id
                .as_deref()
                .and_then(fork_line_of)
                .unwrap_or_else(|| "主线".to_string());
            let active = if node.is_active { "  ← 当前" } else { "" };
            let indent = "  ".repeat(node.depth);
            let connector = if node.depth > 0 { "└ " } else { "" };
            rows.push(format!(
                "{cursor} {indent}{connector}{id:<16}  {}  {fork_note}{active}",
                node.created
            ));
        }
        rows.push("  enter 切换分支 · esc 取消 · /fork 可从某条消息创建分支".to_string());
        rows
    }
}

/// `{message_id}` → `分叉自 #id`（消息序位提示）。
fn fork_line_of(message_id: &str) -> Option<String> {
    if message_id.is_empty() {
        return None;
    }
    Some(format!("分叉自 {message_id}"))
}

/// main 原样；超长分支取前 8 位（chars 截取——多字节边界安全，同
/// status_bar 的 id 缩短口径）。
pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// /fork 的 user 消息条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkEntry {
    /// journal `input.queued` 的 message_id（稳定标识）。
    pub message_id: String,
    /// 消息原文（选中后回填编辑器可改写）。
    pub text: String,
}

/// journal 投影 → user 消息条目（顺序保留；跳过空文本）。
pub fn fork_entries(events: &[crate::TranscriptEvent]) -> Vec<ForkEntry> {
    events
        .iter()
        .filter_map(|event| match event {
            crate::TranscriptEvent::User { message_id, text } => {
                if text.trim().is_empty() {
                    None
                } else {
                    Some(ForkEntry {
                        message_id: message_id.clone(),
                        text: text.clone(),
                    })
                }
            }
            _ => None,
        })
        .collect()
}

/// /fork 消息选择器（❯ 光标 + 单行预览 + 序号即分叉点）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ForkPicker {
    entries: Vec<ForkEntry>,
    selected: usize,
}

impl ForkPicker {
    pub fn new(entries: Vec<ForkEntry>) -> Self {
        // 默认选中最后一条（最常见的"改写刚才那条重试"）。
        let selected = entries.len().saturating_sub(1);
        Self { entries, selected }
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

    pub fn selection(&self) -> Option<&ForkEntry> {
        self.entries.get(self.selected)
    }

    /// 渲染行（`#序号` = 消息序位锚点提示；预览截断 56 字符）。
    pub fn visible_rows(&self) -> Vec<String> {
        if self.entries.is_empty() {
            return vec!["（还没有可分叉的消息——先发送一条）".to_string()];
        }
        let mut rows = vec!["  选择要分叉的消息（选中后回填编辑器，可改写再发）".to_string()];
        for (index, entry) in self.entries.iter().enumerate() {
            let cursor = if index == self.selected { "❯" } else { " " };
            let preview = preview_text(&entry.text, 56);
            rows.push(format!("{cursor} #{index:<5} {preview}"));
        }
        rows.push("  enter 选中 · esc 取消".to_string());
        rows
    }
}

fn preview_text(text: &str, max: usize) -> String {
    let flattened = text.replace('\n', " ⏎ ");
    if flattened.chars().count() <= max {
        flattened
    } else {
        let cut: String = flattened.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, parent: Option<&str>, active: bool) -> BranchInfo {
        BranchInfo {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            is_active: active,
            created: "09-09 12:00".to_string(),
        }
    }

    /// G8.A1：分支深度按 parent 链计算；孤儿分支按根。
    #[test]
    fn branch_nodes_compute_depth() {
        let branches = vec![
            info("main", None, false),
            info("aaaabbbb-1", Some("main"), true),
            info("aaaabbbb-2", Some("aaaabbbb-1"), false),
        ];
        let nodes = branch_nodes(&branches);
        assert_eq!(nodes[0].depth, 0, "main 是根");
        assert_eq!(nodes[1].depth, 1);
        assert_eq!(nodes[2].depth, 2, "孙分支深度 2");
        // 父分支缺失（悬空）→ 按根处理，不 panic。
        let orphan = vec![info("zzzz", Some("ghost"), true)];
        assert_eq!(branch_nodes(&orphan)[0].depth, 0);
    }

    /// G8.A2：树渲染——时间正序、❯ 落在活跃分支、hints 行。
    #[test]
    fn branch_tree_renders_and_selects_active() {
        // 装配序 = 创建时间倒序（新分支在前）。
        let branches = vec![
            info("aaaabbbb-2", Some("aaaabbbb-1"), false),
            info("aaaabbbb-1", Some("main"), true),
            info("main", None, false),
        ];
        let mut tree = BranchTree::new(&branches);
        let rows = tree.visible_rows();
        // 展示序 = 时间正序：main → aaaabbbb-1 → aaaabbbb-2。
        assert!(rows[1].contains("main"), "{rows:?}");
        assert!(rows[2].contains("└"), "子分支有分叉连接符：{rows:?}");
        assert!(rows[1].starts_with(' '), "未选中行无 ❯：{rows:?}");
        assert_eq!(
            tree.selection().unwrap().id,
            "aaaabbbb-1",
            "默认选中活跃分支"
        );
        assert!(rows.last().unwrap().contains("enter 切换分支"));
        // 光标移动钳位。
        tree.move_up();
        assert_eq!(tree.selection().unwrap().id, "main");
        for _ in 0..5 {
            tree.move_up();
        }
        assert_eq!(tree.selection().unwrap().id, "main", "上移钳位");
        for _ in 0..5 {
            tree.move_down();
        }
        assert_eq!(tree.selection().unwrap().id, "aaaabbbb-2", "下移钳位");
    }

    /// G8.A3：fork 条目投影（只取 user 消息、跳过空文本、保序）。
    #[test]
    fn fork_entries_project_user_messages() {
        let event = |variant: crate::TranscriptEvent| variant;
        let events = vec![
            event(crate::TranscriptEvent::User {
                message_id: "m-2".into(),
                text: "第一条".into(),
            }),
            crate::TranscriptEvent::Assistant {
                run_id: "r".into(),
                text: "回答".into(),
            },
            crate::TranscriptEvent::ToolCall {
                run_id: "r".into(),
                name: "bash".into(),
                input: serde_json::json!({}),
            },
            crate::TranscriptEvent::User {
                message_id: "m-6".into(),
                text: "第二条".into(),
            },
            crate::TranscriptEvent::User {
                message_id: "m-8".into(),
                text: "   ".into(),
            },
        ];
        let entries = fork_entries(&events);
        assert_eq!(entries.len(), 2, "空文本/非 user 不入选择器");
        assert_eq!(entries[0].message_id, "m-2");
        assert_eq!(entries[1].text, "第二条");

        let mut picker = ForkPicker::new(entries);
        assert_eq!(
            picker.selection().unwrap().message_id,
            "m-6",
            "默认选中最后一条（改写刚才的消息）"
        );
        let rows = picker.visible_rows();
        assert!(rows[1].contains("#0"), "{rows:?}");
        assert!(rows[2].contains("第二条"), "{rows:?}");
        picker.move_up();
        assert_eq!(picker.selection().unwrap().message_id, "m-2");
        // 空态。
        assert!(ForkPicker::new(Vec::new()).visible_rows()[0].contains("还没有"));
    }

    /// UUID 缩写：main 原样，长 id 取前 8 位。
    #[test]
    fn short_id_truncates_uuids() {
        assert_eq!(short_id("main"), "main");
        assert_eq!(short_id("12345678-90ab-cdef"), "12345678");
    }
}
