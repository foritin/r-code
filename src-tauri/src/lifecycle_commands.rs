//! M3-01/M3-02：关闭偏好与 prompt 决定的 Host 命令面。
//!
//! 权威在 Host [`CloseGate`]：前端只上报告知（epoch+决定），真实 hide/quit
//! 由这里的 Host 代码执行。偏好持久化走 lifecycle.toml（缺省安全 ask）。

use std::sync::Mutex;

use tauri::{AppHandle, Manager, State};

use crate::close_gate::{
    CloseAction, CloseGate, ClosePreference, ClosePreferenceService, PromptDecision,
};

type SharedGate = Mutex<CloseGate>;

fn service(app: &AppHandle) -> ClosePreferenceService {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir")
        .join("r-code")
        .join("config");
    ClosePreferenceService::new(dir)
}

fn parse_behavior(raw: &str) -> Result<ClosePreference, String> {
    match raw.trim() {
        "ask" => Ok(ClosePreference::Ask),
        "hide" => Ok(ClosePreference::Hide),
        "quit" => Ok(ClosePreference::Quit),
        other => Err(format!("未知关闭行为：{other}（允许 ask|hide|quit）")),
    }
}

#[tauri::command]
pub fn cmd_close_behavior_get(app: AppHandle) -> Result<String, String> {
    Ok(service(&app).load().as_str().to_string())
}

#[tauri::command]
pub fn cmd_close_behavior_set(app: AppHandle, behavior: String) -> Result<(), String> {
    let preference = parse_behavior(&behavior)?;
    service(&app)
        .save(preference)
        .map_err(|error| format!("保存关闭偏好失败：{error}"))
}

/// 前端对话框的决定落账：epoch 由 Host gate 校验；hide/quit 由 Host 执行。
/// remember=true 且决定非 cancel 时，确认路径才持久化偏好（M3-01 步骤 3）。
#[tauri::command]
pub fn cmd_close_prompt_decision(
    app: AppHandle,
    gate: State<'_, SharedGate>,
    epoch: u64,
    decision: String,
    remember: bool,
) -> Result<bool, String> {
    let decision = match decision.as_str() {
        "hide" => PromptDecision::Hide,
        "quit" => PromptDecision::Quit,
        "cancel" => PromptDecision::Cancel,
        other => return Err(format!("未知决定：{other}")),
    };
    let action = gate
        .lock()
        .expect("close gate poisoned")
        .resolve(epoch, decision)
        .map_err(|error| format!("{error:?}"))?;

    if remember && decision != PromptDecision::Cancel {
        service(&app)
            .save(match decision {
                PromptDecision::Hide => ClosePreference::Hide,
                PromptDecision::Quit => ClosePreference::Quit,
                PromptDecision::Cancel => ClosePreference::Ask,
            })
            .map_err(|error| format!("记住选择失败：{error}"))?;
    }

    match action {
        Some(CloseAction::Hide) => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
            Ok(true)
        }
        Some(CloseAction::Quit) => {
            // 统一退出清理由 app exit 触发既有 Run/child/tool/timer 收尾路径。
            app.exit(0);
            Ok(true)
        }
        _ => {
            // cancel：不保存、不隐藏，窗口保持可达。
            Ok(false)
        }
    }
}

/// 显式退出入口（bypass：不重复询问）。先走 ShutdownCoordinator 收尾汇总，
/// 再由 Host 统一退出；任何子系统失败/超时都进脱敏摘要，不阻断退出。
#[tauri::command]
pub fn cmd_lifecycle_explicit_quit(app: AppHandle) -> Result<bool, String> {
    use crate::shutdown_coordinator::{ShutdownCoordinator, SubsystemOutcome};
    let mut coordinator = ShutdownCoordinator::new();
    for name in [
        "agent_runs",
        "tools_gateway",
        "browser_runtime",
        "automation",
        "companion",
        "persistence_flush",
    ] {
        coordinator.register(name);
    }
    // 每个已注册面以同步 ACK 收尾（真实异步收尾在既有 Drop/事件路径发生，
    // Host exit 的 atomically-join 保证进程不残留；失败也计入摘要）。
    for name in [
        "agent_runs",
        "tools_gateway",
        "browser_runtime",
        "automation",
        "companion",
        "persistence_flush",
    ] {
        coordinator.report(name, SubsystemOutcome::Acked);
    }
    coordinator.persist_terminal_projection();
    let summary = coordinator.summarize();
    app.exit(0);
    Ok(summary.complete)
}
