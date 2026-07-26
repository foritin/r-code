//! R6 E2E 测试 [doc-18 M9-06, M8-07]。
//!
//! 验证端到端流程：
//! - R6-t6：验证失败 -> 修复 -> 通过，两条记录均保留，旧的被取代
//! - R6-t7：跨文件任务 -- 3 文件补丁 + 验证 + 接受 -> ACCEPTED
//!
//! 运行：`cargo test -p r-code-store --test e2e_tests`

use r_code_core::dto::{FileChangeType, ReviewState, TaskMode, VerificationStatus};
use r_code_store::{
    AgentRunRepository, ChangeService, Database, TaskRepository, VerificationConfig,
    VerificationService,
};

fn successful_command() -> &'static str {
    #[cfg(windows)]
    {
        "exit /B 0"
    }
    #[cfg(not(windows))]
    {
        "true"
    }
}

fn failing_command() -> &'static str {
    #[cfg(windows)]
    {
        "exit /B 7"
    }
    #[cfg(not(windows))]
    {
        "false"
    }
}

/// R6-t6：E2E 验证失败 -> 修复 -> 通过，两条记录均保留，旧的被取代。
#[tokio::test]
async fn e2e_verification_fail_then_pass() {
    let db = Database::open_in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let blobs_dir = dir.path().join("blobs");
    std::fs::create_dir_all(&blobs_dir).unwrap();

    // 创建 task 与 run
    let task = r_code_core::dto::Task::new(Some("/test".into()), "Test", "Fix bug", TaskMode::Auto);
    let run = r_code_core::dto::AgentRun::new(&task.id, "test-model");

    let task_repo = TaskRepository::new(&db);
    task_repo.create(&task).unwrap();
    let run_repo = AgentRunRepository::new(&db);
    run_repo.create(&run).unwrap();

    // 第一次验证：FAIL
    let vs = VerificationService::new(&db, blobs_dir.clone());
    let config = VerificationConfig {
        command: failing_command().to_string(),
        timeout_secs: 5,
    };
    let result1 = vs
        .run_verification(&task.id, &run.id, &config, dir.path())
        .await
        .unwrap();
    assert_eq!(result1.status, VerificationStatus::Failed);

    // 第二次验证：PASS
    let config2 = VerificationConfig {
        command: successful_command().to_string(),
        timeout_secs: 5,
    };
    let result2 = vs
        .run_verification(&task.id, &run.id, &config2, dir.path())
        .await
        .unwrap();
    assert_eq!(result2.status, VerificationStatus::Passed);

    // 两条记录均存在
    let all = vs.list_for_task(&task.id).await.unwrap();
    assert_eq!(all.len(), 2);

    // 最新记录为 Passed
    let latest = vs.latest_for_task(&task.id).await.unwrap().unwrap();
    assert_eq!(latest.status, VerificationStatus::Passed);
}

/// R6-t7：E2E 跨文件任务 -- 3 文件补丁 + 验证 + 接受 -> ACCEPTED。
#[tokio::test]
async fn e2e_cross_file_task() {
    let db = Database::open_in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let blobs_dir = dir.path().join("blobs");
    std::fs::create_dir_all(&blobs_dir).unwrap();

    // 创建 task 与 run
    let task = r_code_core::dto::Task::new(
        Some("/test".into()),
        "Test",
        "Create 3 files",
        TaskMode::Edit,
    );
    let run = r_code_core::dto::AgentRun::new(&task.id, "test-model");

    let task_repo = TaskRepository::new(&db);
    task_repo.create(&task).unwrap();
    let run_repo = AgentRunRepository::new(&db);
    run_repo.create(&run).unwrap();

    // 创建 3 个文件并记录变更
    let cs = ChangeService::new(&db, blobs_dir.clone());
    for i in 0..3 {
        let path = dir.path().join(format!("file{}.txt", i));
        let content = format!("content {}", i);
        std::fs::write(&path, &content).unwrap();
        cs.record_change(
            &task.id,
            &path,
            FileChangeType::Create,
            None,
            None,
            Some(content.as_bytes()),
            None,
        )
        .await
        .unwrap();
    }

    // 验证变更已记录
    let changes = cs.list_changes(&task.id).await.unwrap();
    assert_eq!(changes.len(), 3);

    // 计算变更集
    let change_set = cs.compute_change_set(&task.id).await.unwrap();
    assert_eq!(change_set.entries.len(), 3);

    // 验证并接受
    let vs = VerificationService::new(&db, blobs_dir.clone());
    let config = VerificationConfig {
        command: successful_command().to_string(),
        timeout_secs: 5,
    };
    let result = vs
        .run_verification(&task.id, &run.id, &config, dir.path())
        .await
        .unwrap();
    assert_eq!(result.status, VerificationStatus::Passed);

    // 接受 run
    run_repo
        .update_review_state(&run.id, ReviewState::Accepted)
        .unwrap();

    // 验证已接受
    let updated_run = run_repo.get(&run.id).unwrap().unwrap();
    assert_eq!(updated_run.review_state, ReviewState::Accepted);
}
