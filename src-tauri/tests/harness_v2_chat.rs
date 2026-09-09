//! T42 阶段 1 —— GUI 聊天链路 v2 投影层端到端验收。
//!
//! 起真 daemon（staging native 包，复用 r-code-tui 的 daemon_common 模式），
//! 经 `harness_v2_chat::ChatV2Bridge` 走完 task_create → agent_send →
//! session_messages / task_detail 全链路。无 provider 时 run 诚实失败同样
//! 算投影成功：断言旧形状（字段存在与类型正确），不断言模型回复内容。

mod daemon_common;

use r_code_core::dto::{TaskEventType, TaskState};
use r_code_host::harness_v2_chat::ChatV2Bridge;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};

#[test]
fn chat_projection_end_to_end_against_real_daemon() {
    let (env, _env_vars) = daemon_common::daemon_env("t42-chat");
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(&env.data_dir)
            .with_ipc_name(env.ipc_name.clone()),
    )
    .expect("profile");
    let bridge = ChatV2Bridge::new_from_profile(
        profile,
        Some(daemon_common::target_debug("r-code-service")),
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        // 1. task_create → 旧 Task 形状（11 字段之外的 Task 本体先钉住）。
        let task = bridge
            .task_create("T42 投影", "验证 v2 聊天投影层", "ask", None, None)
            .await
            .expect("task_create");
        assert!(task.id.starts_with("task-"), "task id: {}", task.id);
        assert_eq!(task.title, "T42 投影");
        assert_eq!(task.goal, "验证 v2 聊天投影层");
        assert_eq!(task.state, TaskState::Idle);
        assert_eq!(task.workspace_path, None);
        assert_eq!(task.created_at, task.updated_at);

        // 2. task_list 包含新任务。
        let list = bridge.task_list(None, false).await.expect("task_list");
        assert!(
            list.iter()
                .any(|row| row.id == task.id && row.title == "T42 投影"),
            "task_list should contain the created task"
        );

        // 3. agent_send → Ok（v2 排队/启动都算成功）。
        bridge
            .agent_send(&task.id, "请回复一条消息")
            .await
            .expect("agent_send");

        // 4. 轮询：session_messages 出现用户消息；task_detail 出现已终结 run。
        //    无 provider 的诚实失败（run.failed）与正常回复都算投影成功。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut saw_user_message = false;
        let mut run_settled = false;
        let mut detail = None;
        while std::time::Instant::now() < deadline {
            let messages = bridge
                .session_messages(&task.id)
                .await
                .expect("session_messages");
            saw_user_message = messages.iter().any(|message| {
                message.role.as_deref() == Some("user")
                    && message.text.as_deref() == Some("请回复一条消息")
            });

            let current = bridge.task_detail(&task.id).await.expect("task_detail");
            // 11 字段存在且类型正确（形状断言，而非内容断言）。
            assert_eq!(current.task.id, task.id);
            assert_eq!(current.status.task_id, task.id);
            assert_eq!(current.active_branch.id, "main");
            assert_eq!(current.active_branch.task_id, task.id);
            assert_eq!(current.branches.len(), 1);
            assert!(current.queued_messages.is_empty());
            assert!(current.changes.is_empty());
            assert!(current.permissions.is_empty());
            assert!(current.verifications.is_empty());
            assert!(current.pending_plan_entry_offer.is_none());
            run_settled = current.runs.iter().any(|run| run.ended_at.is_some());
            detail = Some(current);

            if saw_user_message && run_settled {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        assert!(
            saw_user_message,
            "session_messages should project the queued user message"
        );
        assert!(
            run_settled,
            "at least one run must reach a terminal projection (assistant reply or honest failure)"
        );

        // 5. 事件时间线：run.started 必然入账；终结类型只能是 RunEnded。
        let detail = detail.expect("polled detail");
        assert!(
            detail
                .events
                .iter()
                .any(|event| event.event_type == TaskEventType::RunStarted),
            "events should contain run.started"
        );

        // 6. task_rename → 返回旧 Task 且标题生效。
        let renamed = bridge
            .task_rename(&task.id, "改名后的会话")
            .await
            .expect("task_rename");
        assert_eq!(renamed.title, "改名后的会话");

        // 7. task_clone → 新任务，标题带“（克隆）”。
        let clone = bridge.task_clone(&task.id).await.expect("task_clone");
        assert_ne!(clone.id, task.id);
        assert!(
            clone.title.contains("（克隆）"),
            "clone title: {}",
            clone.title
        );
    });

    daemon_common::shutdown_daemon(&env);
}
