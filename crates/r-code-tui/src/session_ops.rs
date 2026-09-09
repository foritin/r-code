//! 会话操作（M6-02 / R-SESS-02）：/new /rename /compact。
//!
//! T35 起全部经守护进程 v2 RPC（`task.create` / `task.rename`）。
//! /compact 是 v2 首版的诚实降级：上下文压缩能力尚未在 v2 会话引擎提供，
//! 返回明确提示而非报错（功能入口保留）。

use crate::engine::V2ChatClient;

/// 新建空会话（task.create + 命名"新会话"；返回 task id 供 resume 接管）。
pub async fn new_session(engine: &V2ChatClient) -> Result<String, String> {
    engine.create_session("新会话").await
}

/// 重命名会话（守护进程校验）。
pub async fn rename_session(
    engine: &V2ChatClient,
    task_id: &str,
    title: &str,
) -> Result<(), String> {
    engine.rename_task(task_id, title).await
}

/// /compact 提示文案（v2 首版：压缩能力未上，诚实降级不报错）。
pub fn compact_unavailable_note() -> &'static str {
    "v2 上下文压缩将在后续版本提供"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M6-02.A3（T35 更新）：v2 首版 /compact 诚实降级——返回提示不报错。
    #[test]
    fn compaction_reports_honest_deferral() {
        let note = compact_unavailable_note();
        assert!(note.contains("后续版本"), "降级文案：{note}");
        assert!(!note.to_lowercase().contains("error"));
    }

    /// M6-02.A1/A2：new/rename 走 in-proc ApplicationService（task.create +
    /// rename 读回一致；与 V2ChatClient 同名 RPC 的服务端语义）。
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
