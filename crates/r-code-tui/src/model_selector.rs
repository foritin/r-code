//! `/model` 模型选择器（M2-01 / R-MODEL-01）。
//!
//! 纯逻辑层：条目投影（可用集 → 分组列表）+ fuzzy 过滤 + 预选当前值 +
//! 选中写回（`task_set_provider` + `task_set_model`，空闲会话语义由宿主保证）。
//! 渲染层（app.rs）只消费 `visible_rows`，键位路由见 `handle_key`。

use r_code_core::dto::ModelAvailabilityEntry;
use r_code_host::commands::{task_set_model, task_set_provider, CommandState};

/// 一条可选模型（provider 分组下的一员）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
    /// 来源说明（decl / catalog / config），渲染为 dim 附注。
    pub source: String,
}

/// 可用集 → 选择器条目（只收 `available`——与设置页/`--list-models` 同一口径：
/// "配置解析但缺鉴权"的模型不进选择面）。
pub fn picker_entries(available: &[ModelAvailabilityEntry]) -> Vec<ModelEntry> {
    let mut entries: Vec<ModelEntry> = available
        .iter()
        .filter(|entry| entry.has_auth)
        .map(|entry| ModelEntry {
            provider: entry.provider.clone(),
            model: entry.model.clone(),
            source: entry.source.clone(),
        })
        .collect();
    // provider 分组稳定序：先按 provider 名、再按 model 名排序，保证循环/滚动确定性。
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

    /// 渲染行（provider 分组头 + 条目；选中位由调用方着色）。
    pub fn visible_rows(&self) -> Vec<(Option<String>, String)> {
        let mut rows = Vec::new();
        let mut last_provider: Option<&str> = None;
        for (position, &index) in self.filtered.iter().enumerate() {
            let entry = &self.entries[index];
            if last_provider != Some(entry.provider.as_str()) {
                rows.push((Some(entry.provider.clone()), String::new()));
                last_provider = Some(entry.provider.as_str());
            }
            rows.push((None, format!("{}/{}", entry.provider, entry.model)));
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

/// 选中写回：切 provider → 设 model（顺序即宿主语义：换服务清旧模型覆盖）。
/// 返回 footer 联动标签。运行中会话会被宿主拒绝（错误原样上抛）。
pub async fn apply_model_selection(
    state: &CommandState,
    task_id: &str,
    entry: &ModelEntry,
) -> Result<String, String> {
    task_set_provider(state, task_id, &entry.provider).await?;
    task_set_model(state, task_id, Some(&entry.model)).await?;
    Ok(model_label(&entry.provider, &entry.model))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(provider: &str, model: &str, has_auth: bool) -> ModelAvailabilityEntry {
        ModelAvailabilityEntry {
            provider: provider.to_string(),
            model: model.to_string(),
            source: "config".to_string(),
            has_auth,
        }
    }

    /// M2-01.A1：可用集投影只收 has_auth 条目、按 provider/model 稳定排序。
    #[test]
    fn picker_entries_project_available_set_grouped() {
        let entries = picker_entries(&[
            entry("zeta", "z-1", true),
            entry("alpha", "b-model", true),
            entry("alpha", "a-model", true),
            entry("ghost", "g-1", false), // 缺鉴权：不进选择面
        ]);
        let projected: Vec<String> = entries
            .iter()
            .map(|item| format!("{}/{}", item.provider, item.model))
            .collect();
        assert_eq!(
            projected,
            vec!["alpha/a-model", "alpha/b-model", "zeta/z-1"],
            "must group by provider with stable model order, excluding no-auth"
        );
    }

    /// M2-01.A1：fuzzy 过滤（子序列，大小写不敏感；空查询全量）。
    #[test]
    fn fuzzy_filter_matches_subsequence() {
        let entries = picker_entries(&[
            entry("deepseek", "deepseek-chat", true),
            entry("anthropic", "claude-opus-4-5", true),
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
            entry("alpha", "a-model", true),
            entry("beta", "b-model", true),
            entry("beta", "b2-model", true),
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

    /// M2-01.A2：选中写回任务（provider + model 落库可读回），返回 footer 标签。
    #[tokio::test]
    async fn model_selection_writes_task_and_returns_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir config");
        std::fs::write(
            config_dir.join("config.toml"),
            "default_provider = \"demo\"\n\n[providers.demo]\nbase_url = \"https://example.invalid/v1\"\napi_key = \"test-key\"\nmodel = \"demo-model\"\n",
        )
        .expect("write config");
        let db = r_code_store::Database::open(dir.path().join("app.db")).expect("db");
        let state = r_code_host::commands::CommandState::new_with_planning_release_control(
            std::sync::Arc::new(db),
            dir.path().join("blobs"),
            dir.path().join("sessions"),
            config_dir,
            dir.path().join("project"),
            Some(dir.path().join("app.db")),
            r_code_host::plan_policy::PlanningReleaseControl {
                provider_kind: "tui-test".to_string(),
                release_state: r_code_host::plan_policy::PlanningReleaseState::Off,
                emergency_off: false,
                eligibility_profile_version: String::new(),
                evidence_version: String::new(),
                allowed_models: Vec::new(),
                allowed_protocols: Vec::new(),
                allowed_endpoint_classes: Vec::new(),
                basis: "model_selector test".to_string(),
            },
        );
        let task = r_code_host::commands::task_create(&state, None, "t", "goal", "ask")
            .await
            .expect("task");
        let entry = ModelEntry {
            provider: "demo".to_string(),
            model: "demo-model-v2".to_string(),
            source: "config".to_string(),
        };
        let label = apply_model_selection(&state, &task.id, &entry)
            .await
            .expect("selection applies");
        assert_eq!(label, "(demo) demo-model-v2");
        let detail = r_code_host::commands::task_detail(&state, &task.id)
            .await
            .expect("detail");
        assert_eq!(detail.task.provider_name.as_deref(), Some("demo"));
        assert_eq!(detail.task.model.as_deref(), Some("demo-model-v2"));
    }
}
