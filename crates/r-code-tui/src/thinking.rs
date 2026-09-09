//! 思考级别控制（M2-02 / R-THINK-01；T35 起写回走守护进程
//! `task.setPreferences` 的 inference）。
//!
//! 档位集合 = v2 harness config `inference` 载荷接受的 `reasoning_effort`
//! 全集（比 pi 的七档多 `ultra`——以引擎契约为准，见 MC-3）。选中写回
//! per-task 偏好（影响下一次 run）；thinking 字段随档位映射：
//! `none` → disabled，其余 → enabled。

use crate::engine::V2ChatClient;

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

/// 选中写回：per-task inference 偏好（守护进程持久化于任务；影响下一次
/// run——运行中的 run 不受影响，无需拒绝路径）。
pub async fn apply_thinking(
    engine: &V2ChatClient,
    task_id: &str,
    level: &str,
) -> Result<(), String> {
    let position = EFFORT_LEVELS
        .iter()
        .position(|item| *item == level)
        .ok_or_else(|| format!("未知思考档位：{level}"))?;
    let _ = position;
    engine
        .set_preferences(
            task_id,
            None,
            Some(serde_json::json!({
                "thinking": thinking_flag(level),
                "reasoning_effort": level,
            })),
            None,
        )
        .await
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
    /// 预选当前档位（未知/未设回落 medium——与引擎默认一致）。
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

    /// M2-02.A1：档位全集写回（inference 载荷读回一致；非法档位客户端拒绝）。
    #[tokio::test]
    async fn effort_levels_write_task_preferences() {
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
                "task-think",
                "",
                r_code_kernel::task::TaskKind::Conversation,
                vec![],
            )
            .await
            .expect("create");
        for level in EFFORT_LEVELS {
            let inference = serde_json::json!({
                "thinking": thinking_flag(level),
                "reasoning_effort": level,
            });
            service
                .set_task_preferences(
                    "task-think",
                    r_code_kernel::task::TaskPreferences {
                        model: None,
                        inference: Some(inference),
                        mode: None,
                    },
                )
                .await
                .unwrap_or_else(|error| panic!("level {level} must apply: {error}"));
        }
        // 非法档位在 TUI 层拒绝（不进 daemon）。
        let engine = crate::engine::V2ChatClient::from_profile(profile, None);
        assert!(
            apply_thinking(&engine, "task-think", "extreme")
                .await
                .is_err(),
            "invalid level must be rejected client-side"
        );
        // 读回：最后一档 ultra 落在任务偏好上。
        let detail = service.task_detail("task-think").await.expect("detail");
        assert_eq!(
            detail
                .inference
                .as_ref()
                .and_then(|value| value.get("reasoning_effort")),
            Some(&serde_json::json!("ultra"))
        );
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
