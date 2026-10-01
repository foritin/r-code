//! P-GATE — task kinds select planning versus conversation semantics.

mod p_gate_support;

use p_gate_support::{
    compose_with_builtin, profile, snapshot_id, stage_native, system_text, tool_names,
    wait_for_kind, write_workspace, ScriptedModel, STRICT_PLAN,
};
use r_code_kernel::ports::JournalStore;
use r_code_kernel::task::{RunSnapshotPhase, TaskExecution, TaskKind, TaskPreferences};
use r_code_store::v1::V1Store;

#[tokio::test]
async fn implementation_repair_and_plan_draft_plan_while_conversation_stays_a_reply() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("task-kinds", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let model = ScriptedModel::fixed(STRICT_PLAN);
    let service = compose_with_builtin(&profile, &package, model.clone());
    let store = V1Store::open(&profile.database_path()).expect("store");

    for (task_id, kind) in [
        ("implementation", TaskKind::Implementation),
        ("repair", TaskKind::Repair),
        ("plan-draft", TaskKind::PlanDraft),
    ] {
        service
            .create_task(task_id, task_id, kind, vec![])
            .await
            .expect("create planning task");
        service
            .set_task_preferences(
                task_id,
                TaskPreferences {
                    workspace_path: Some(workspace.display().to_string()),
                    system_prompt: Some(format!("prompt-{task_id}")),
                    ..TaskPreferences::default()
                },
            )
            .await
            .expect("planning preferences");
        service
            .send_message(task_id, "plan")
            .await
            .expect("plan run");
        let events = wait_for_kind(&service, task_id, "plan.awaiting-approval").await;
        assert!(matches!(
            store.load_task(task_id).await.expect("task").execution,
            TaskExecution::AwaitingPlanApproval { .. }
        ));
        assert_eq!(
            store
                .load_run_snapshot(snapshot_id(&events, task_id))
                .expect("snapshot query")
                .expect("snapshot")
                .phase(),
            &RunSnapshotPhase::Planning
        );
    }

    service
        .create_task("conversation", "chat", TaskKind::Conversation, vec![])
        .await
        .expect("create conversation");
    service
        .set_task_preferences(
            "conversation",
            TaskPreferences {
                mode: Some("edit".into()),
                system_prompt: Some("conversation-own-mode".into()),
                workspace_path: Some(workspace.display().to_string()),
                ..TaskPreferences::default()
            },
        )
        .await
        .expect("conversation preferences");
    service
        .send_message("conversation", "answer only")
        .await
        .expect("conversation run");
    wait_for_kind(&service, "conversation", "run.completed").await;
    assert!(matches!(
        store
            .load_task("conversation")
            .await
            .expect("conversation")
            .execution,
        TaskExecution::ReviewReady { .. }
    ));
    assert!(store
        .current_plan_revision("conversation")
        .expect("plan query")
        .is_none());
    let requests = model.requests.lock().expect("requests");
    assert!(requests[..3]
        .iter()
        .all(|request| system_text(request).contains("Plan 模式")));
    assert_eq!(system_text(&requests[3]), "conversation-own-mode");
    // M1a-11 (D9): the parent planning catalog carries the children tools.
    assert!(requests.iter().all(|request| tool_names(&request.tools)
        == [
            "children_close",
            "children_spawn",
            "children_wait",
            "git_diff",
            "git_log",
            "git_status",
            "glob",
            "list_files",
            "read_file",
            "search",
        ]));
}
