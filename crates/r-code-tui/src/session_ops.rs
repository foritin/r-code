//! 会话操作（M6-02 / R-SESS-02）：/new /rename /compact。
//!
//! T35 起全部经守护进程 v1 RPC（`task.create` / `task.rename`）。
//! /compact 是 v1 首版的诚实降级：上下文压缩能力尚未在 v1 会话引擎提供，
//! 返回明确提示而非报错（功能入口保留）。

use crate::engine::V1ChatClient;

/// 新建空会话（task.create + 命名"新会话"；返回 task id 供 resume 接管）。
pub async fn new_session(engine: &V1ChatClient) -> Result<String, String> {
    engine.create_session("新会话").await
}

/// 重命名会话（守护进程校验）。
pub async fn rename_session(
    engine: &V1ChatClient,
    task_id: &str,
    title: &str,
) -> Result<(), String> {
    engine.rename_task(task_id, title).await
}

/// /compact 提示文案（v1 首版：压缩能力未上，诚实降级不报错）。
pub fn compact_unavailable_note() -> &'static str {
    "v1 上下文压缩将在后续版本提供"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M6-02.A3（T35 更新）：v1 首版 /compact 诚实降级——返回提示不报错。
    #[test]
    fn compaction_reports_honest_deferral() {
        let note = compact_unavailable_note();
        assert!(note.contains("后续版本"), "降级文案：{note}");
        assert!(!note.to_lowercase().contains("error"));
    }

    /// M6-02.A1/A2：new/rename 走 in-proc ApplicationService（task.create +
    /// rename 读回一致；与 V1ChatClient 同名 RPC 的服务端语义）。
    #[tokio::test]
    async fn session_ops_round_trip_in_proc() {
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
                "task-sess",
                "",
                r_code_kernel::task::TaskKind::Conversation,
                vec![],
            )
            .await
            .expect("create");
        service
            .rename_task("task-sess", "新会话")
            .await
            .expect("rename");
        let detail = service.task_detail("task-sess").await.expect("detail");
        assert_eq!(detail.title, "新会话");
        service
            .rename_task("task-sess", "重命名后")
            .await
            .expect("rename 2");
        let detail = service.task_detail("task-sess").await.expect("detail 2");
        assert_eq!(detail.title, "重命名后");
    }
}

/// FR-1 (M1a-08)：/context 的多行状态文本（与 GUI 面板同一数据源）。
pub fn render_context_view(view: &serde_json::Value) -> String {
    let enabled = view["injectionEnabled"].as_bool().unwrap_or(false);
    let entries = view["instructions"].as_array().cloned().unwrap_or_default();
    let injected = entries
        .iter()
        .filter(|entry| entry["status"].as_str() == Some("injected"))
        .count();
    let bytes = view["instructionsBytes"].as_u64().unwrap_or(0);
    let budget = view["totalBudgetBytes"].as_u64().unwrap_or(0);
    let mut lines = Vec::new();
    if !enabled {
        lines.push("指令注入已对本项目关闭（新 run 不再注入）".to_string());
    } else {
        lines.push(format!(
            "指令注入已开启 · 注入 {injected} 条 · {bytes}/{budget} 字节"
        ));
    }
    if let Some(memory) = view["memory"].as_object() {
        lines.push(format!(
            "记忆快照 {} · {} 条 · {} 字符",
            memory["snapshotHash"]
                .as_str()
                .unwrap_or("?")
                .chars()
                .take(8)
                .collect::<String>(),
            memory["entryCount"].as_u64().unwrap_or(0),
            memory["chars"].as_u64().unwrap_or(0),
        ));
    } else {
        lines.push("无记忆注入".to_string());
    }
    let layer_label = |layer: &str| -> String {
        match layer {
            "global" => "全局".to_string(),
            "repo-foreign" => "外来".to_string(),
            "repo-own" => "自有".to_string(),
            "subdir" => "子目录".to_string(),
            other => other.to_string(),
        }
    };
    let status_label = |status: &str| -> String {
        match status {
            "injected" => String::new(),
            "skipped_oversize" => " · 跳过（>4MiB）".to_string(),
            "trimmed" => " · 被裁剪（预算）".to_string(),
            "dropped_budget" => " · 去重/预算淘汰".to_string(),
            "dropped_disabled" => " · 注入已关闭".to_string(),
            other => other.to_string(),
        }
    };
    for entry in &entries {
        let path = entry["path"].as_str().unwrap_or("?");
        let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
        lines.push(format!(
            "[{}] {} · {} B{}",
            layer_label(entry["layer"].as_str().unwrap_or("?")),
            file,
            entry["bytes"].as_u64().unwrap_or(0),
            status_label(entry["status"].as_str().unwrap_or("")),
        ));
    }
    let jit = view["jitBatches"].as_array().cloned().unwrap_or_default();
    if !jit.is_empty() {
        lines.push(format!("运行中 JIT 注入 {} 批", jit.len()));
    }
    lines.join("\n")
}

#[cfg(test)]
mod context_view_tests {
    #[test]
    fn render_context_view_lists_entries_with_layer_and_status_labels() {
        let view = serde_json::json!({
            "injectionEnabled": true,
            "memory": {"snapshotHash": "abcdef1234", "entryCount": 2, "chars": 120},
            "instructions": [
                {"layer": "repo-foreign", "path": "D:/repo/AGENTS.md", "sha256": "x", "bytes": 40, "status": "injected"},
                {"layer": "repo-own", "path": "D:/repo/.r-code/context.md", "sha256": "y", "bytes": 60, "status": "injected"},
                {"layer": "global", "path": "C:/home/.r-code/context.md", "sha256": "z", "bytes": 99, "status": "trimmed"},
            ],
            "instructionsBytes": 100,
            "totalBudgetBytes": 32768,
            "jitBatches": [],
        });
        let rendered = super::render_context_view(&view);
        assert!(rendered.contains("注入 2 条"), "{rendered}");
        assert!(rendered.contains("记忆快照 abcdef12 · 2 条 · 120 字符"));
        assert!(rendered.contains("[外来] AGENTS.md · 40 B"));
        assert!(rendered.contains("[自有] context.md · 60 B"));
        assert!(rendered.contains("[全局] context.md · 99 B · 被裁剪（预算）"));
        assert!(!rendered.contains("JIT"));
    }
}
