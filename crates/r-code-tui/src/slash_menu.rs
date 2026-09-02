//! 斜杠命令菜单与快捷键面板（M4-03 / R-CMD-01）。
//!
//! 菜单 = 输入以 `/` 起始时在输入区上方插入的过滤列表（codex 形态：`/名` +
//! dim 描述、选中 `› ` cyan bold、Tab 补全、no matches dim）。注册表只含
//! **已实现**命令（PRD 冻结约束：未实现命令不出现菜单；M6 任务落地后登记）。

/// 一条已实现命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub desc: &'static str,
}

/// 已实现命令注册表（渲染序）。
pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/model",
        desc: "切换模型服务（fuzzy 搜索）",
    },
    SlashCommand {
        name: "/setup",
        desc: "配置模型服务（选预设 + 输入 API key）",
    },
    SlashCommand {
        name: "/thinking",
        desc: "思考级别（alt+T 打开，alt+, / alt+. 升降）",
    },
    SlashCommand {
        name: "/status",
        desc: "会话与用量状态卡",
    },
    SlashCommand {
        name: "/usage",
        desc: "累计用量与成本",
    },
    SlashCommand {
        name: "/resume",
        desc: "恢复历史会话",
    },
    SlashCommand {
        name: "/new",
        desc: "新建空白会话",
    },
    SlashCommand {
        name: "/rename",
        desc: "重命名会话（/rename <名称>）",
    },
    SlashCommand {
        name: "/compact",
        desc: "压缩上下文（自动随 run 触发）",
    },
    SlashCommand {
        name: "/clear",
        desc: "清空当前 transcript 视图",
    },
    SlashCommand {
        name: "/help",
        desc: "快捷键面板",
    },
    SlashCommand {
        name: "/quit",
        desc: "退出 R-Code CLI",
    },
];

/// 计划中、尚未实现的命令（不出现在菜单；M6-01/M6-02 落地后移入 COMMANDS）。
pub const PENDING_COMMANDS: &[&str] = &[];

/// 查询是否命中命令（子串、大小写不敏感；空查询全量）。
fn matches(command: &SlashCommand, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let bare = query
        .strip_prefix('/')
        .unwrap_or(query.as_str())
        .to_string();
    command.name.contains(&format!("/{bare}"))
        || command.desc.to_lowercase().contains(bare.as_str())
        || command.desc.to_lowercase().contains(query.as_str())
}

/// 菜单状态（纯逻辑；渲染壳层据输入文本驱动 query）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SlashMenu {
    filtered: Vec<usize>,
    selected: usize,
    query: String,
}

impl SlashMenu {
    pub fn new(query: &str) -> Self {
        let mut menu = Self {
            filtered: Vec::new(),
            selected: 0,
            query: query.to_string(),
        };
        menu.refilter();
        menu
    }

    fn refilter(&mut self) {
        self.filtered = COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, command)| matches(command, &self.query))
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.refilter();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }

    pub fn selection(&self) -> Option<&SlashCommand> {
        self.filtered
            .get(self.selected)
            .and_then(|&index| COMMANDS.get(index))
    }

    /// 渲染行（(文本, 是否选中)；无命中返回空——由 no_matches_line 呈现）。
    pub fn visible_rows(&self) -> Vec<(String, bool)> {
        self.filtered
            .iter()
            .enumerate()
            .map(|(position, &index)| {
                let command = &COMMANDS[index];
                (
                    format!("{} {}", command.name, command.desc),
                    position == self.selected,
                )
            })
            .collect()
    }

    /// Tab 补全：选中命令名写回输入。
    pub fn complete(&self) -> Option<&'static str> {
        self.selection().map(|command| command.name)
    }
}

/// 无命中行（dim italic 形态由渲染层着色）。
pub fn no_matches_line() -> &'static str {
    "no matches"
}

/// 输入是否应呈现菜单（/ 起始且非完整命令提交路径由 Send 处理）。
pub fn should_show(text: &str) -> bool {
    text.trim_start().starts_with('/')
}

/// `?` 快捷键面板（两列、键名列定宽对齐、行尾补齐等宽）。
pub fn help_panel_lines() -> Vec<String> {
    const KEY_WIDTH: usize = 14;
    let key = |name: &str| format!("{name:<KEY_WIDTH$}");
    let rows = vec![
        format!(
            "{}发送（运行中=排队）  {}模式循环 ask→plan",
            key("enter"),
            key("shift+tab")
        ),
        format!(
            "{}思考级别选择器      {}思考降/升一档",
            key("alt+t"),
            key("alt+, / alt+.")
        ),
        format!(
            "{}撤销 / 重做          {}词左移 / 词右移",
            key("ctrl+z / y"),
            key("ctrl+← / →")
        ),
        format!(
            "{}多行换行             {}外部编辑器",
            key("shift+enter"),
            key("ctrl+g")
        ),
        format!(
            "{}历史上一条/下一条    {}transcript 滚动",
            key("↑ / ↓"),
            key("pgup / pgdn")
        ),
        format!(
            "{}中止运行 / 关闭浮层  {}清空输入（再按退出）",
            key("esc"),
            key("ctrl+c")
        ),
        format!("{}快捷键面板           {}命令菜单", key("?"), key("/")),
        format!(
            "{}模型选择器菜单        {}",
            key("/model"),
            key("/status · /usage")
        ),
    ];
    let width = rows
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0);
    rows.into_iter()
        .map(|row| format!("{row:<width$}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M4-03.A1：注册表 = 冻结的已实现命令集；计划中命令不在其中。
    /// 2026-09-03 增补 /setup（症状3：无配置时 /model 死端 → 引导式配置）。
    #[test]
    fn registry_matches_frozen_implemented_set() {
        let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            vec![
                "/model",
                "/setup",
                "/thinking",
                "/status",
                "/usage",
                "/resume",
                "/new",
                "/rename",
                "/compact",
                "/clear",
                "/help",
                "/quit"
            ],
            "已实现命令集（M6 收口 + /setup 引导配置）"
        );
        for command in COMMANDS {
            assert!(
                command.name.starts_with('/'),
                "命令必须 / 起始：{command:?}"
            );
            assert!(!command.desc.is_empty(), "描述非空：{command:?}");
        }
        // 计划中命令不得出现在已实现注册表。
        for pending in PENDING_COMMANDS {
            assert!(!names.contains(pending), "{pending} 未实现不得入菜单");
        }
    }

    /// M4-03.A2：fuzzy 过滤 + no matches。
    #[test]
    fn filter_matches_and_no_matches() {
        let mut menu = SlashMenu::new("/");
        assert_eq!(menu.visible_rows().len(), COMMANDS.len(), "空过滤全量");
        menu.set_query("/mo");
        let rows = menu.visible_rows();
        assert!(
            !rows.is_empty() && rows[0].0.starts_with("/model"),
            "{rows:?}"
        );
        assert!(
            rows.iter().all(|(text, _)| text.contains("/model")),
            "只留 model：{rows:?}"
        );
        menu.set_query("/zzz");
        assert!(menu.is_empty(), "无命中");
        assert_eq!(no_matches_line(), "no matches");
        // 描述也可命中（中文）。
        let by_desc = SlashMenu::new("/用量");
        assert!(
            by_desc
                .visible_rows()
                .iter()
                .any(|(text, _)| text.contains("/usage")),
            "描述模糊命中：{:?}",
            by_desc.visible_rows()
        );
    }

    /// M4-03.A3：Tab 补全与选中移动。
    #[test]
    fn tab_completion_returns_selected_name() {
        let mut menu = SlashMenu::new("/");
        assert_eq!(menu.complete(), Some("/model"), "默认选中第一条");
        menu.move_down();
        assert_eq!(menu.complete(), Some("/setup"));
        menu.move_up();
        assert_eq!(menu.complete(), Some("/model"));
        for _ in 0..20 {
            menu.move_down();
        }
        assert_eq!(
            menu.complete(),
            COMMANDS.last().map(|c| c.name),
            "下移钳在最后一条"
        );
        // 过滤后补全选中过滤集内条目。
        let filtered = SlashMenu::new("/sta");
        assert_eq!(filtered.complete(), Some("/status"));
        // /setup 已实现（前缀 /se 命中）。
        let setup = SlashMenu::new("/se");
        assert_eq!(setup.complete(), Some("/setup"));
    }

    /// M4-03.A4：? 面板两列渲染（键名列定宽、行宽一致）。
    #[test]
    fn help_panel_renders_two_aligned_columns() {
        let lines = help_panel_lines();
        assert!(lines.len() >= 8, "面板行数：{lines:?}");
        let widths: Vec<usize> = lines.iter().map(|line| line.chars().count()).collect();
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "两列定宽对齐（行宽一致）：{widths:?}"
        );
        let body = lines.join("\n");
        assert!(body.contains("shift+tab"), "{body}");
        assert!(body.contains("ctrl+g"), "{body}");
        assert!(body.contains("命令菜单"), "{body}");
    }
}
