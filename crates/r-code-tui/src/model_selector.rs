//! `/model` 模型选择器（M2-01 / R-MODEL-01；T35 起数据源 = 守护进程
//! `models.available`）。
//!
//! 纯逻辑层：条目投影（可用集 → 分组列表，不可用的仍列出但标注）+ fuzzy
//! 过滤 + 预选当前值 + 选中写回（`task.setPreferences` 的 model 选择，
//! 影响下一次 run）。渲染层（app.rs）只消费 `visible_rows`，键位路由见
//! `handle_key`。

use crate::engine::V2ChatClient;

/// 一条可选模型（provider 分组下的一员）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
    /// 来源说明（默认/已配置/缺鉴权），渲染为附注。
    pub source: String,
    /// 是否已鉴权（v2 has_credential）。不可用的仍列出但标注——功能入口
    /// 不因缺 key 消失，选择后发送即报可操作错误。
    pub available: bool,
}

/// v2 `models.available` → 选择器条目（provider 分组稳定序：先按
/// provider 名、再按 model 名排序，保证循环/滚动确定性）。
pub fn picker_entries(models: &[crate::engine::ModelRow]) -> Vec<ModelEntry> {
    let mut entries: Vec<ModelEntry> = models
        .iter()
        .map(|row| ModelEntry {
            provider: row.selection.clone(),
            model: row.model.clone(),
            source: if row.is_default {
                "默认".to_string()
            } else {
                "已配置".to_string()
            },
            available: row.has_credential,
        })
        .collect();
    entries.sort_by(|a, b| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)));
    entries
}

/// fuzzy 子串评分：查询按字符顺序出现在 `provider/model` 中即命中
/// （大小写不敏感）。返回是否命中。
pub fn fuzzy_matches(entry: &ModelEntry, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let haystack = format!("{}/{}", entry.provider, entry.model).to_lowercase();
    let mut cursor = 0;
    for ch in query.chars() {
        match haystack[cursor..].find(ch) {
            Some(offset) => cursor += offset + ch.len_utf8(),
            None => return false,
        }
    }
    true
}

/// footer 右侧模型标签：`(provider) model`。
pub fn model_label(provider: &str, model: &str) -> String {
    format!("({provider}) {model}")
}

/// 选择器状态（纯逻辑；渲染与键位由调用方驱动）。
#[derive(Debug, Default)]
pub struct ModelPicker {
    entries: Vec<ModelEntry>,
    /// 命中过滤的条目下标（entries 的索引）。
    filtered: Vec<usize>,
    /// filtered 中的选中位。
    selected: usize,
    query: String,
}

impl ModelPicker {
    pub fn new(entries: Vec<ModelEntry>, current_provider: Option<&str>) -> Self {
        let mut picker = Self {
            entries,
            filtered: Vec::new(),
            selected: 0,
            query: String::new(),
        };
        picker.refilter();
        // 预选当前 provider 的首个条目（codex `› high (current)` 语义）。
        if let Some(current) = current_provider.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(position) = picker
                .filtered
                .iter()
                .position(|&index| picker.entries[index].provider == current)
            {
                picker.selected = position;
            }
        }
        picker
    }

    fn refilter(&mut self) {
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| fuzzy_matches(entry, &self.query))
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

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }

    pub fn selection(&self) -> Option<&ModelEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|&index| self.entries.get(index))
    }

    /// 渲染行（provider 分组头 + 条目；选中位由调用方着色）。缺鉴权条目
    /// 尾注标注（仍可选——发送时会收到可操作的缺 key 错误）。
    pub fn visible_rows(&self) -> Vec<(Option<String>, String)> {
        let mut rows = Vec::new();
        let mut last_provider: Option<&str> = None;
        for (position, &index) in self.filtered.iter().enumerate() {
            let entry = &self.entries[index];
            if last_provider != Some(entry.provider.as_str()) {
                rows.push((Some(entry.provider.clone()), String::new()));
                last_provider = Some(entry.provider.as_str());
            }
            let availability = if entry.available {
                String::new()
            } else {
                "（缺鉴权）".to_string()
            };
            rows.push((
                None,
                format!("{}/{}{}", entry.provider, entry.model, availability),
            ));
            let _ = position;
        }
        rows
    }

    /// 选中行在 visible_rows 中的下标（供渲染层着色）。
    pub fn selected_row(&self) -> Option<usize> {
        let mut row = 0usize;
        let mut last_provider: Option<&str> = None;
        for (position, &index) in self.filtered.iter().enumerate() {
            let entry = &self.entries[index];
            if last_provider != Some(entry.provider.as_str()) {
                row += 1;
                last_provider = Some(entry.provider.as_str());
            }
            if position == self.selected {
                return Some(row);
            }
            row += 1;
        }
        None
    }
}

/// 选中写回：`task.setPreferences {model: selection}`（v2 语义：selection
/// 即模型服务选择 id，影响下一次 run）。返回 footer 联动标签。
pub async fn apply_model_selection(
    engine: &V2ChatClient,
    task_id: &str,
    entry: &ModelEntry,
) -> Result<String, String> {
    engine
        .set_preferences(task_id, Some(&entry.provider), None, None)
        .await?;
    Ok(model_label(&entry.provider, &entry.model))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        selection: &str,
        model: &str,
        has_credential: bool,
        is_default: bool,
    ) -> crate::engine::ModelRow {
        crate::engine::ModelRow {
            selection: selection.to_string(),
            model: model.to_string(),
            has_credential,
            is_default,
        }
    }

    /// M2-01.A1：可用集投影按 provider/model 稳定排序；缺鉴权条目保留并标注。
    #[test]
    fn picker_entries_project_available_set_grouped() {
        let entries = picker_entries(&[
            row("zeta", "z-1", true, false),
            row("alpha", "b-model", true, false),
            row("alpha", "a-model", true, true),
            row("ghost", "g-1", false, false), // 缺鉴权：仍列出但标注
        ]);
        let projected: Vec<(String, bool)> = entries
            .iter()
            .map(|item| (format!("{}/{}", item.provider, item.model), item.available))
            .collect();
        assert_eq!(
            projected,
            vec![
                ("alpha/a-model".to_string(), true),
                ("alpha/b-model".to_string(), true),
                ("ghost/g-1".to_string(), false),
                ("zeta/z-1".to_string(), true),
            ],
            "must group by provider with stable model order, keeping no-auth rows flagged"
        );
        // 渲染行带缺鉴权标注。
        let picker = ModelPicker::new(entries, None);
        let rows = picker.visible_rows();
        assert!(
            rows.iter()
                .any(|(_, text)| text.contains("ghost/g-1（缺鉴权）")),
            "no-auth rows must be annotated: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|(_, text)| text.contains("alpha/a-model") && !text.contains("缺鉴权")),
            "authed rows carry no annotation: {rows:?}"
        );
    }

    /// M2-01.A1：fuzzy 过滤（子序列，大小写不敏感；空查询全量）。
    #[test]
    fn fuzzy_filter_matches_subsequence() {
        let entries = picker_entries(&[
            row("deepseek", "deepseek-chat", true, false),
            row("anthropic", "claude-opus-4-5", true, false),
        ]);
        let mut picker = ModelPicker::new(entries, None);
        assert_eq!(
            picker.visible_rows().len(),
            4,
            "two group headers + two rows"
        );
        picker.set_query("dsc");
        let rows = picker.visible_rows();
        assert!(rows.len() >= 2, "deepseek entry must survive: {rows:?}");
        assert!(
            rows.iter()
                .any(|(_, text)| text.contains("deepseek/deepseek-chat")),
            "deepseek chat matches d-s-c subsequence: {rows:?}"
        );
        assert!(
            !rows.iter().any(|(_, text)| text.contains("claude")),
            "claude must be filtered out"
        );
    }

    /// M2-01.A3：预选当前 provider、上下移动不越界、selection 返回条目。
    #[test]
    fn picker_preselects_current_and_moves_within_bounds() {
        let entries = picker_entries(&[
            row("alpha", "a-model", true, false),
            row("beta", "b-model", true, false),
            row("beta", "b2-model", true, false),
        ]);
        let mut picker = ModelPicker::new(entries.clone(), Some("beta"));
        assert_eq!(
            picker.selection().map(|item| item.model.as_str()),
            Some("b-model"),
            "must preselect the current provider's first entry"
        );
        picker.move_up();
        assert_eq!(
            picker.selection().map(|item| item.provider.as_str()),
            Some("alpha"),
            "move_up crosses group boundary"
        );
        picker.move_up();
        assert_eq!(
            picker.selection().map(|item| item.provider.as_str()),
            Some("alpha"),
            "move_up clamps at the top"
        );
        picker.set_query("b2");
        assert_eq!(
            picker.selection().map(|item| item.model.as_str()),
            Some("b2-model")
        );
        picker.move_down();
        assert_eq!(
            picker.selection().map(|item| item.model.as_str()),
            Some("b2-model"),
            "move_down clamps at the filtered tail"
        );
    }

    /// M2-01.A2：选中写回 = task.setPreferences {model: selection}（JSON 口径）。
    #[tokio::test]
    async fn model_selection_writes_task_preferences() {
        // in-proc ApplicationService（r-code-runtime 直接组合；无需 daemon/
        // 插件进程——setPreferences 不驱动 run）。
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = r_code_runtime::RuntimeProfile::resolve(
            &r_code_runtime::LaunchOptions::new(r_code_runtime::ProfileFlavor::Development)
                .with_data_root(dir.path()),
        )
        .expect("profile");
        let models: std::sync::Arc<dyn r_code_kernel::ports::ModelService> =
            std::sync::Arc::new(r_code_kernel::testing::FakeModelService::default());
        let tools: std::sync::Arc<dyn r_code_kernel::ports::ToolService> =
            std::sync::Arc::new(r_code_kernel::testing::FakeToolService::default());
        let service =
            r_code_runtime::application::ApplicationService::compose(&profile, models, tools)
                .expect("compose");
        service
            .create_task(
                "task-model",
                "",
                r_code_kernel::task::TaskKind::Conversation,
                vec![],
            )
            .await
            .expect("create");

        let entry = ModelEntry {
            provider: "openai".to_string(),
            model: "gpt-5.6".to_string(),
            source: "已配置".to_string(),
            available: true,
        };
        // 写回语义与 V2ChatClient::set_preferences 相同的 params（engine 走
        // task.setPreferences RPC；此处直接调 service 同名能力验证读回）。
        service
            .set_task_preferences(
                "task-model",
                r_code_kernel::task::TaskPreferences {
                    model: Some(entry.provider.clone()),
                    inference: None,
                    mode: None,
                    require_desktop_confirm: false,
                },
            )
            .await
            .expect("set preferences");
        let detail = service.task_detail("task-model").await.expect("detail");
        assert_eq!(detail.model.as_deref(), Some("openai"));
    }
}
