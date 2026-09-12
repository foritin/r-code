//! TaskMode 循环（M2-03 / R-MODE-01；T35 起写回走守护进程
//! `task.setPreferences` 的 mode）。
//!
//! `Shift+Tab` 循环 ask→edit→auto→plan；写回 per-task 偏好（影响下一次
//! run）。输入区模式态：非 ask 档显示 `[mode]` 前缀，plan 用 magenta
//!（色彩语义 §2.7）。

use crate::engine::V2ChatClient;

/// 循环序（= 模式枚举序）。
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

/// 写回任务模式（守护进程校验未知模式；影响下一次 run）。
pub async fn apply_mode(engine: &V2ChatClient, task_id: &str, mode: &str) -> Result<(), String> {
    if !MODE_CYCLE.contains(&mode) {
        return Err(format!("未知模式：{mode}"));
    }
    engine
        .set_preferences(task_id, None, None, Some(mode))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M2-03.A1：循环序 ask→edit→auto→plan→ask；未知值回落 ask。
    #[test]
    fn cycle_follows_enum_order() {
        assert_eq!(cycle_mode("ask"), "edit");
        assert_eq!(cycle_mode("edit"), "auto");
        assert_eq!(cycle_mode("auto"), "plan");
        assert_eq!(cycle_mode("plan"), "ask", "循环回到起点");
        assert_eq!(cycle_mode("bogus"), "ask", "未知值安全回落");
    }

    /// M2-03.A2：模式写回任务（task.detail 读回一致；非法模式客户端拒绝）。
    #[tokio::test]
    async fn mode_persists_on_task() {
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
                "task-mode",
                "",
                r_code_kernel::task::TaskKind::Conversation,
                vec![],
            )
            .await
            .expect("create");
        // 与 V2ChatClient::set_preferences 同参数形状（mode 字符串）。
        service
            .set_task_preferences(
                "task-mode",
                r_code_kernel::task::TaskPreferences {
                    model: None,
                    inference: None,
                    mode: Some("plan".to_string()),
                    require_desktop_confirm: false,
                },
            )
            .await
            .expect("apply plan");
        let detail = service.task_detail("task-mode").await.expect("detail");
        assert_eq!(detail.mode.as_deref(), Some("plan"));
        // 非法模式：TUI 层拒绝（不连 daemon）。
        let engine = crate::engine::V2ChatClient::from_profile(profile, None);
        assert!(apply_mode(&engine, "task-mode", "bogus").await.is_err());
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
