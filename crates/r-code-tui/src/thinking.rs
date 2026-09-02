//! 思考级别控制（M2-02 / R-THINK-01）。
//!
//! 档位集合 = 宿主 `validated_inference` 接受的 `reasoning_effort` 全集
//!（比 pi 的七档多 `ultra`——以宿主契约为准，见 MC-3）。选中写回
//! `task_set_inference`（per-task 持久记忆）；thinking 字段随档位映射：
//! `none` → disabled，其余 → enabled。

use r_code_host::commands::{task_set_inference, CommandState};

/// 档位全集（升序；升降步进与 clamp 的序即此数组序）。
pub const EFFORT_LEVELS: [&str; 8] = [
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// 档位 → thinking 字段映射（MC-3：off 档关 thinking）。
pub fn thinking_flag(level: &str) -> &'static str {
    if level == "none" {
        "disabled"
    } else {
        "enabled"
    }
}

/// 步进一档（越界 clamp；level 未知时回落 medium）。
pub fn step_level(level: Option<&str>, delta: i32) -> &'static str {
    let current = level
        .and_then(|value| EFFORT_LEVELS.iter().position(|item| *item == value))
        .unwrap_or(3) as i32; // medium
    let next = (current + delta).clamp(0, EFFORT_LEVELS.len() as i32 - 1) as usize;
    EFFORT_LEVELS[next]
}

/// 选中写回：per-task 记忆（宿主持久化于任务；运行中会被拒绝并原样上抛）。
pub async fn apply_thinking(
    state: &CommandState,
    task_id: &str,
    level: &str,
) -> Result<(), String> {
    task_set_inference(
        state,
        task_id,
        agent_contract::InferenceOptions {
            thinking: Some(thinking_flag(level).to_string()),
            reasoning_effort: Some(level.to_string()),
            verbosity: None,
        },
    )
    .await
    .map(|_| ())
}

/// footer 标签：`(provider) model • thinking`；未设/不支持时省略 thinking 段。
pub fn footer_label(model_label: &str, thinking: Option<&str>) -> String {
    match thinking {
        Some(level) if !level.is_empty() => format!("{model_label} • {level}"),
        _ => model_label.to_string(),
    }
}

/// 选择器状态（与 model_selector 同款纯逻辑；档位固定小集合无需 fuzzy）。
#[derive(Debug, Default)]
pub struct ThinkingPicker {
    selected: usize,
}

impl ThinkingPicker {
    /// 预选当前档位（未知/未设回落 medium——与宿主默认一致）。
    pub fn new(current: Option<&str>) -> Self {
        Self {
            selected: current
                .and_then(|value| EFFORT_LEVELS.iter().position(|item| *item == value))
                .unwrap_or(3),
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < EFFORT_LEVELS.len() {
            self.selected += 1;
        }
    }

    pub fn selection(&self) -> &'static str {
        EFFORT_LEVELS[self.selected]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M2-02.A1：TUI 档位集合与宿主验证契约逐值一致（全档位写回通过）。
    #[tokio::test]
    async fn effort_levels_match_host_contract() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = r_code_store::Database::open(dir.path().join("app.db")).expect("db");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir");
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
                basis: "thinking test".to_string(),
            },
        );
        let task = r_code_host::commands::task_create(&state, None, "t", "goal", "ask")
            .await
            .expect("task");
        for level in EFFORT_LEVELS {
            apply_thinking(&state, &task.id, level)
                .await
                .unwrap_or_else(|error| panic!("level {level} must pass host validation: {error}"));
        }
        // 非法档位必须被宿主拒绝（验证我们真的在测契约边界）。
        assert!(apply_thinking(&state, &task.id, "extreme").await.is_err());
    }

    /// M2-02.A2：升降步进与 clamp（未知档位回落 medium）。
    #[test]
    fn step_levels_clamp_at_bounds() {
        assert_eq!(step_level(Some("none"), -1), "none", "下界 clamp");
        assert_eq!(step_level(Some("ultra"), 1), "ultra", "上界 clamp");
        assert_eq!(step_level(Some("medium"), 1), "high");
        assert_eq!(step_level(Some("high"), -1), "medium");
        assert_eq!(step_level(None, 0), "medium", "未设回落 medium");
        assert_eq!(step_level(Some("bogus"), 0), "medium", "未知回落 medium");
    }

    /// M2-02.A3：per-task 记忆——写回后 task_detail 读回一致（thinking 映射随档）。
    #[tokio::test]
    async fn thinking_persists_on_task() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = r_code_store::Database::open(dir.path().join("app.db")).expect("db");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir");
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
                basis: "thinking test".to_string(),
            },
        );
        let task = r_code_host::commands::task_create(&state, None, "t", "goal", "ask")
            .await
            .expect("task");
        apply_thinking(&state, &task.id, "xhigh")
            .await
            .expect("apply");
        let detail = r_code_host::commands::task_detail(&state, &task.id)
            .await
            .expect("detail");
        assert_eq!(
            detail.task.inference.reasoning_effort.as_deref(),
            Some("xhigh")
        );
        assert_eq!(detail.task.inference.thinking.as_deref(), Some("enabled"));
        // off 档映射 disabled。
        apply_thinking(&state, &task.id, "none")
            .await
            .expect("apply");
        let detail = r_code_host::commands::task_detail(&state, &task.id)
            .await
            .expect("detail");
        assert_eq!(detail.task.inference.thinking.as_deref(), Some("disabled"));
    }

    /// M2-02.A4：footer 联动（有档位拼 `• level`；未设/空省略段）。
    #[test]
    fn footer_label_appends_thinking_when_set() {
        assert_eq!(footer_label("(demo) m", Some("high")), "(demo) m • high");
        assert_eq!(footer_label("(demo) m", None), "(demo) m");
        assert_eq!(footer_label("(demo) m", Some("")), "(demo) m");
    }

    /// 弹层预选当前档位、移动 clamp。
    #[test]
    fn picker_preselects_and_clamps() {
        let mut picker = ThinkingPicker::new(Some("xhigh"));
        assert_eq!(picker.selection(), "xhigh");
        picker.move_up();
        assert_eq!(picker.selection(), "high");
        picker.move_down();
        picker.move_down();
        assert_eq!(picker.selection(), "max");
        let mut top = ThinkingPicker::new(Some("none"));
        top.move_up();
        assert_eq!(top.selection(), "none");
        let mut bottom = ThinkingPicker::new(Some("ultra"));
        bottom.move_down();
        assert_eq!(bottom.selection(), "ultra");
    }
}
