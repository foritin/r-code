use r_code_core::dto::{SessionBranch, Task, TaskMode, TaskState};
use r_code_core::{TaskAttention, TaskDisplayState, TaskStatusView};
use r_code_host::commands::{DashboardChangeSummary, DashboardTaskSummary, TaskDetail};

fn task_and_status() -> (Task, TaskStatusView) {
    let mut task = Task::new(None, "contract", "contract", TaskMode::Edit);
    task.id = "status-contract-task".to_string();
    task.state = TaskState::ReviewReady;
    let status = TaskStatusView {
        task_id: task.id.clone(),
        persisted_state: TaskState::ReviewReady,
        display_state: TaskDisplayState::ReviewReady,
        attention: vec![TaskAttention::ReviewRequired],
        active_run_id: None,
        queue_depth: 2,
        unread_count: 4,
    };
    (task, status)
}

#[test]
fn task_detail_and_dashboard_summary_share_the_same_status_shape() {
    let (task, status) = task_and_status();
    let active_branch = SessionBranch::main(task.id.clone());
    let detail = TaskDetail {
        task: task.clone(),
        status: status.clone(),
        active_branch: active_branch.clone(),
        branches: vec![active_branch],
        runs: Vec::new(),
        events: Vec::new(),
        changes: Vec::new(),
        permissions: Vec::new(),
        verifications: Vec::new(),
        queued_messages: Vec::new(),
        pending_plan_entry_offer: None,
    };
    let dashboard = DashboardTaskSummary {
        task,
        status: status.clone(),
        activity: "review ready".to_string(),
        agent_label: "R-Code".to_string(),
        pending_permission_count: 0,
        active_run: None,
        change_summary: DashboardChangeSummary::default(),
        latest_verification: None,
    };

    let expected_status = serde_json::to_value(&status).expect("serialize status");
    let detail_json = serde_json::to_value(&detail).expect("serialize task detail");
    let dashboard_json = serde_json::to_value(&dashboard).expect("serialize dashboard summary");
    assert_eq!(detail_json["status"], expected_status);
    assert_eq!(dashboard_json["status"], expected_status);

    let restored_detail: TaskDetail =
        serde_json::from_value(detail_json).expect("deserialize task detail");
    let restored_dashboard: DashboardTaskSummary =
        serde_json::from_value(dashboard_json).expect("deserialize dashboard summary");
    assert_eq!(restored_detail.status, status);
    assert_eq!(restored_dashboard.status, status);
}
