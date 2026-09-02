//! 会话树（G8：/tree 分支导航 + /fork 消息级分叉选择器）。
//!
//! 数据源全走宿主：分支列表 = `session_branch_list`（SQLite 元数据），user
//! 消息列表 = `session_messages`（活跃分支 JSONL 投影，id 即 `{storage}:{line}`
//! 稳定标识——`agent_resend` 直接消费）。纯逻辑无终端 IO。
//!
//! pi 语义映射：pi 的树是 session 文件内 entry 树（leaf 指针切换）；本仓是
//! 分支树（每分支独立 JSONL，活跃指针切换，宿主发送链路从 JSONL 重放前缀）。
//! 节点粒度更粗（分支 = 一次 fork），对用户呈现的"回到某条消息重试"语义由
//! /fork 的消息级选择补齐。

use r_code_core::dto::SessionBranch;

/// 分支展示投影 + 树深度（父分支不在列表 = 根；main 恒为根）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchNode {
    pub id: String,
    pub parent_branch_id: Option<String>,
    /// 分叉自哪条消息（`{storage}:{line}`；main 无）。
    pub forked_from_message_id: Option<String>,
    /// 创建时间（MM-DD HH:mm）。
    pub created: String,
    pub is_active: bool,
    pub depth: usize,
}

/// `SessionBranch` → 树节点（按 parent 链算深度；孤儿分支按根处理）。
pub fn branch_nodes(branches: &[SessionBranch]) -> Vec<BranchNode> {
    let by_id: std::collections::HashMap<&str, &SessionBranch> = branches
        .iter()
        .map(|branch| (branch.id.as_str(), branch))
        .collect();
    let depth_of = |id: &str, seen: &mut Vec<String>| -> usize {
        let mut depth = 0usize;
        let mut current = id.to_string();
        while let Some(branch) = by_id.get(current.as_str()) {
            let Some(parent) = branch.parent_branch_id.clone() else {
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
            parent_branch_id: branch.parent_branch_id.clone(),
            forked_from_message_id: branch.forked_from_message_id.clone(),
            created: branch
                .created_at
                .with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string(),
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
    pub fn new(branches: &[SessionBranch]) -> Self {
        // 列表按创建时间倒序（仓库序）；树形展示按时间正序（旧分支在上）。
        let mut ordered: Vec<SessionBranch> = branches.to_vec();
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

    /// 选中分支 id（Enter 语义：切到该分支；已活跃则宿主 no-op）。
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

/// `{storage}:{line}` → `分叉自 #line`（行号即消息序位提示）。
fn fork_line_of(message_id: &str) -> Option<String> {
    let (_, line) = message_id.rsplit_once(':')?;
    let line: usize = line.parse().ok()?;
    Some(format!("分叉自 #{line}"))
}

/// main 原样；UUID 分支取前 8 位。
pub fn short_id(id: &str) -> &str {
    if id.len() <= 8 {
        id
    } else {
        &id[..8]
    }
}

/// /fork 的 user 消息条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkEntry {
    /// `{storage}:{line}` 稳定标识（agent_resend 直接消费）。
    pub message_id: String,
    /// 消息原文（选中后回填编辑器可改写）。
    pub text: String,
}

/// 宿主 `session_messages` → user 消息条目（顺序保留）。
pub fn fork_entries(messages: &[r_code_host::commands::SessionMessage]) -> Vec<ForkEntry> {
    messages
        .iter()
        .filter(|message| message.kind == "message" && message.role.as_deref() == Some("user"))
        .filter_map(|message| {
            let id = message.id.clone()?;
            let text = message.text.clone().unwrap_or_default();
            if text.trim().is_empty() {
                return None;
            }
            Some(ForkEntry {
                message_id: id,
                text,
            })
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

    /// 渲染行（`#序号` = JSONL 行号锚点提示；预览截断 56 字符）。
    pub fn visible_rows(&self) -> Vec<String> {
        if self.entries.is_empty() {
            return vec!["（还没有可分叉的消息——先发送一条）".to_string()];
        }
        let mut rows = vec!["  选择要分叉的消息（选中后回填编辑器，可改写再发）".to_string()];
        for (index, entry) in self.entries.iter().enumerate() {
            let cursor = if index == self.selected { "❯" } else { " " };
            let line = entry
                .message_id
                .rsplit_once(':')
                .map(|(_, line)| line.to_string())
                .unwrap_or_default();
            let preview = preview_text(&entry.text, 56);
            rows.push(format!("{cursor} #{line:<5} {preview}"));
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

    fn branch(id: &str, parent: Option<&str>, forked: Option<&str>, active: bool) -> SessionBranch {
        SessionBranch {
            id: id.to_string(),
            task_id: "t".to_string(),
            parent_branch_id: parent.map(str::to_string),
            forked_from_message_id: forked.map(str::to_string),
            storage_id: format!("t--{id}"),
            is_active: active,
            created_at: chrono::Utc::now(),
        }
    }

    /// G8.A1：分支深度按 parent 链计算；孤儿分支按根。
    #[test]
    fn branch_nodes_compute_depth() {
        let branches = vec![
            branch("main", None, None, false),
            branch("aaaabbbb-1", Some("main"), Some("t:5"), true),
            branch("aaaabbbb-2", Some("aaaabbbb-1"), Some("t--x:9"), false),
        ];
        let nodes = branch_nodes(&branches);
        assert_eq!(nodes[0].depth, 0, "main 是根");
        assert_eq!(nodes[1].depth, 1);
        assert_eq!(nodes[2].depth, 2, "孙分支深度 2");
        // 父分支缺失（悬空）→ 按根处理，不 panic。
        let orphan = vec![branch("zzzz", Some("ghost"), None, true)];
        assert_eq!(branch_nodes(&orphan)[0].depth, 0);
    }

    /// G8.A2：树渲染——时间正序、❯ 落在活跃分支、分叉注记、hints 行。
    #[test]
    fn branch_tree_renders_and_selects_active() {
        // 仓库序 = 创建时间倒序（新分支在前）。
        let branches = vec![
            branch("aaaabbbb-2", Some("aaaabbbb-1"), Some("t--x:9"), false),
            branch("aaaabbbb-1", Some("main"), Some("t:5"), true),
            branch("main", None, None, false),
        ];
        let mut tree = BranchTree::new(&branches);
        let rows = tree.visible_rows();
        // 展示序 = 时间正序：main → aaaabbbb-1 → aaaabbbb-2。
        assert!(rows[1].contains("main"), "{rows:?}");
        assert!(rows[2].contains("└"), "子分支有分叉连接符：{rows:?}");
        assert!(rows[2].contains("分叉自 #5"), "{rows:?}");
        assert!(rows[2].contains("← 当前"), "活跃标记：{rows:?}");
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
        let message = |id: &str, kind: &str, role: Option<&str>, text: &str| {
            r_code_host::commands::SessionMessage {
                id: Some(id.to_string()),
                branch_id: "main".to_string(),
                kind: kind.to_string(),
                role: role.map(str::to_string),
                text: Some(text.to_string()),
                image_count: None,
                image_media_types: None,
                attachments: None,
                tool_name: None,
                call_id: None,
                input_json: None,
                output_json: None,
                is_error: None,
                timestamp: None,
            }
        };
        let messages = vec![
            message("t:2", "message", Some("user"), "第一条"),
            message("t:3", "message", Some("assistant"), "回答"),
            message("t:4", "tool_call", None, ""),
            message("t:6", "message", Some("user"), "第二条"),
            message("t:8", "message", Some("user"), "   "),
        ];
        let entries = fork_entries(&messages);
        assert_eq!(entries.len(), 2, "空文本/非 user 不入选择器");
        assert_eq!(entries[0].message_id, "t:2");
        assert_eq!(entries[1].text, "第二条");

        let mut picker = ForkPicker::new(entries);
        assert_eq!(
            picker.selection().unwrap().message_id,
            "t:6",
            "默认选中最后一条（改写刚才的消息）"
        );
        let rows = picker.visible_rows();
        assert!(rows[1].contains("#2"), "{rows:?}");
        assert!(rows[2].contains("第二条"), "{rows:?}");
        picker.move_up();
        assert_eq!(picker.selection().unwrap().message_id, "t:2");
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
