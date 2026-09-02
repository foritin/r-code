//! TaskMode 循环（M2-03 / R-MODE-01）。
//!
//! `Shift+Tab` 循环 ask→edit→auto→plan（宿主 `TaskMode` 枚举序，PRD §2.6）；
//! 写回经 `task_set_mode`（运行中被拒、Plan 需 R-Code 引擎——宿主语义不动）。
//! 输入区模式态：非 ask 档显示 `[mode]` 前缀，plan 用 magenta（色彩语义 §2.7）。

use r_code_core::dto::TaskMode;
use r_code_host::commands::{task_set_mode, CommandState};

/// 循环序（= 宿主枚举序）。
pub const MODE_CYCLE: [&str; 4] = ["ask", "edit", "auto", "plan"];

/// 下一档（未知值回落 ask，避免脏状态卡死循环）。
pub fn cycle_mode(current: &str) -> &'static str {
    let next = MODE_CYCLE
        .iter()
        .position(|item| *item == current)
        .map(|index| (index + 1) % MODE_CYCLE.len())
        .unwrap_or(0);
    MODE_CYCLE[next]
}

/// 模式徽章语义色（lib 层不依赖 ratatui；app.rs 映射到终端色）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeColor {
    Cyan,
    Yellow,
    Magenta,
}

/// 非 ask 档的输入区前缀徽章；ask（默认）无徽章。
pub fn mode_badge(mode: &str) -> Option<(&'static str, BadgeColor)> {
    match mode {
        "edit" => Some(("[edit]", BadgeColor::Cyan)),
        "auto" => Some(("[auto]", BadgeColor::Yellow)),
        "plan" => Some(("[plan]", BadgeColor::Magenta)),
        _ => None,
    }
}

/// 写回任务模式（运行中/引擎不支持由宿主拒绝并原样上抛）。
pub async fn apply_mode(state: &CommandState, task_id: &str, mode: &str) -> Result<(), String> {
    let parsed = TaskMode::try_from_str(mode).ok_or_else(|| format!("未知模式：{mode}"))?;
    task_set_mode(state, task_id, parsed).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M2-03.A1：循环序 ask→edit→auto→plan→ask；未知值回落 ask。
    #[test]
    fn cycle_follows_host_enum_order() {
        assert_eq!(cycle_mode("ask"), "edit");
        assert_eq!(cycle_mode("edit"), "auto");
        assert_eq!(cycle_mode("auto"), "plan");
        assert_eq!(cycle_mode("plan"), "ask", "循环回到起点");
        assert_eq!(cycle_mode("bogus"), "ask", "未知值安全回落");
    }

    /// M2-03.A2：模式写回任务（读回一致）。
    #[tokio::test]
    async fn mode_persists_on_task() {
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
                basis: "task_mode test".to_string(),
            },
        );
        let task = r_code_host::commands::task_create(&state, None, "t", "goal", "ask")
            .await
            .expect("task");
        apply_mode(&state, &task.id, "plan")
            .await
            .expect("apply plan");
        let detail = r_code_host::commands::task_detail(&state, &task.id)
            .await
            .expect("detail");
        assert_eq!(detail.task.mode, TaskMode::Plan);
        apply_mode(&state, &task.id, "auto")
            .await
            .expect("apply auto");
        let detail = r_code_host::commands::task_detail(&state, &task.id)
            .await
            .expect("detail");
        assert_eq!(detail.task.mode, TaskMode::Auto);
    }

    /// M2-03.A3：plan 态 magenta 语义色；ask 无徽章。
    #[test]
    fn plan_badge_uses_magenta_semantic() {
        assert_eq!(
            mode_badge("plan"),
            Some(("[plan]", BadgeColor::Magenta)),
            "Plan 态必须是 magenta 语义（色彩契约 §2.7）"
        );
        assert_eq!(mode_badge("ask"), None, "默认 ask 不显示徽章");
        assert_eq!(mode_badge("edit"), Some(("[edit]", BadgeColor::Cyan)));
        assert_eq!(mode_badge("auto"), Some(("[auto]", BadgeColor::Yellow)));
    }
}
