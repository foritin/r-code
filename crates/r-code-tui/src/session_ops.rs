//! 会话操作（M6-02 / R-SESS-02）：/new /rename /compact。
//!
//! 全部经宿主既有命令（`task_create` / `task_rename` / `task_delete`），不新造
//! 宿主 API。/compact 走宿主压缩入口（本期接线宿主既有压缩能力；若宿主无公开
//! 压缩命令则以 task_detail 触发——见决策记录）。

use r_code_host::commands::{task_create, task_rename, CommandState};

/// 新建空会话（复用 task_create；返回 task id 供 resume 接管）。
pub async fn new_session(state: &CommandState) -> Result<String, String> {
    let task = task_create(state, None, "新会话", "", "ask").await?;
    Ok(task.id)
}

/// 重命名会话（宿主校验空/超长/归档）。
pub async fn rename_session(
    state: &CommandState,
    task_id: &str,
    title: &str,
) -> Result<(), String> {
    task_rename(state, task_id, title).await.map(|_| ())
}

/// 触发宿主压缩（本期：压缩入口 = 宿主既有 automatic_compaction 由 run 结束
/// 触发，无显式公开命令；此处为占位接点——返回是否已支持。PRD 允许宿主无公开
/// 入口时记录数据缺口）。
pub fn compaction_supported() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_host::commands::task_detail;

    fn state() -> CommandState {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir config");
        std::fs::create_dir_all(dir.path().join("sessions")).expect("mkdir sessions");
        std::fs::create_dir_all(dir.path().join("blobs")).expect("mkdir blobs");
        let db = r_code_store::Database::open(dir.path().join("app.db")).expect("db");
        CommandState::new_with_planning_release_control(
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
                basis: "session ops test".to_string(),
            },
        )
    }

    /// M6-02.A1：/new 新建空会话（title 默认"新会话"）。
    #[tokio::test]
    async fn new_session_creates_task() {
        let state = state();
        let id = new_session(&state).await.expect("new");
        let detail = task_detail(&state, &id).await.expect("detail");
        assert_eq!(detail.task.title, "新会话");
        assert_eq!(detail.task.id, id);
    }

    /// M6-02.A2：/rename 持久化（task_detail 读回一致）。
    #[tokio::test]
    async fn rename_session_persists() {
        let state = state();
        let id = new_session(&state).await.expect("new");
        rename_session(&state, &id, "重命名后")
            .await
            .expect("rename");
        let detail = task_detail(&state, &id).await.expect("detail");
        assert_eq!(detail.task.title, "重命名后");
    }

    /// M6-02.A3：/compact 数据缺口如实暴露（宿主无公开压缩命令）。
    #[test]
    fn compaction_gap_is_reported() {
        assert!(
            !compaction_supported(),
            "宿主无公开压缩命令，接线方必须显式引导"
        );
    }
}
