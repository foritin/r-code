//! P-GATE — deterministic Native planning over the current checkout.

mod p_gate_support;

use p_gate_support::{
    compose_with_builtin, escape_link, profile, snapshot_id, stage_native, tool_names, tree,
    wait_for_event, wait_for_kind, write_workspace, ScriptedModel, STRICT_PLAN,
};
use r_code_harness_protocol::services::ToolCallRequest;
use r_code_harness_protocol::{canonical_input_hash, HostService};
use r_code_kernel::ports::{GenerationToken, JournalStore, ToolService};
use r_code_kernel::task::{RunSnapshotPhase, TaskExecution, TaskKind, TaskPreferences};
use r_code_runtime::services::tools::PlanningToolService;
use r_code_runtime::services::workspaces::TaskWorkspaceBinding;
use r_code_store::v1::V1Store;
use std::path::PathBuf;

#[tokio::test]
async fn native_prd_planning_reads_only_the_bound_checkout_and_awaits_approval() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let outside = temp.path().join("outside-secret.txt");
    std::fs::write(&outside, "OUTSIDE_SECRET_SENTINEL\n").expect("outside file");
    let linked = escape_link(&workspace.join("escape.txt"), &outside);
    let profile = profile("read-only-e2e", temp.path());
    std::fs::create_dir_all(profile.workspaces_root()).expect("legacy workspace root");
    let profile_decoy = profile.workspaces_root().join("profile-secret.txt");
    std::fs::write(&profile_decoy, "PROFILE_SECRET_SENTINEL\n").expect("profile decoy");

    let mut calls = vec![
        (
            "read_file".into(),
            serde_json::json!({"path": "tracked.txt"}),
        ),
        ("list_files".into(), serde_json::json!({"path": "."})),
        (
            "search".into(),
            serde_json::json!({"path": ".", "pattern": "TRACKED_SENTINEL", "literal": true}),
        ),
        ("glob".into(), serde_json::json!({"pattern": "**/*.txt"})),
        (
            "read_file".into(),
            serde_json::json!({"path": profile_decoy}),
        ),
        ("read_file".into(), serde_json::json!({"path": outside})),
    ];
    if linked {
        calls.push((
            "read_file".into(),
            serde_json::json!({"path": "escape.txt"}),
        ));
    }
    let model = ScriptedModel::with_calls(calls, STRICT_PLAN);
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let service = compose_with_builtin(&profile, &package, model.clone());
    service
        .create_task("prd", "implement the PRD", TaskKind::Implementation, vec![])
        .await
        .expect("create task");
    service
        .set_task_preferences(
            "prd",
            TaskPreferences {
                workspace_path: Some(workspace.display().to_string()),
                ..TaskPreferences::default()
            },
        )
        .await
        .expect("bind current checkout");

    let before = tree(&workspace);
    service
        .send_message("prd", "inspect and plan; do not write")
        .await
        .expect("start planning");
    let events = wait_for_kind(&service, "prd", "plan.awaiting-approval").await;
    assert_eq!(tree(&workspace), before, "planning changed the checkout");

    let requests = model.requests.lock().expect("model requests").clone();
    assert_eq!(requests.len(), 2, "one tool turn and one plan turn");
    assert_eq!(
        tool_names(&requests[0].tools),
        [
            "children_close",
            "children_spawn",
            "children_wait",
            "git_diff",
            "git_log",
            "git_status",
            "glob",
            "list_files",
            "read_file",
            "search"
        ]
    );
    let observations = serde_json::to_string(&requests[1].messages).expect("messages");
    assert!(observations.contains("TRACKED_SENTINEL"));
    assert!(observations.contains("untracked.txt"));
    for secret in [
        "WORKSPACE_SECRET_SENTINEL",
        "PROFILE_SECRET_SENTINEL",
        "OUTSIDE_SECRET_SENTINEL",
    ] {
        assert!(!observations.contains(secret), "leaked {secret}");
    }

    let store = V1Store::open(&profile.database_path()).expect("open store");
    let state = store.load_task("prd").await.expect("task state");
    assert!(matches!(
        state.execution,
        TaskExecution::AwaitingPlanApproval { .. }
    ));
    assert!(!events.iter().any(|event| {
        event.task_id == "prd"
            && matches!(
                event.payload["journalKind"].as_str(),
                Some("review.ready" | "task.terminal")
            )
    }));
    let snapshot = store
        .load_run_snapshot(snapshot_id(&events, "prd"))
        .expect("load snapshot")
        .expect("snapshot");
    assert_eq!(snapshot.phase(), &RunSnapshotPhase::Planning);
    assert_eq!(
        PathBuf::from(&snapshot.material().workspace.canonical_root),
        std::fs::canonicalize(&workspace).expect("canonical workspace")
    );
    let mut catalog = requests[0].tools.clone();
    catalog.sort_by_key(|tool| serde_json::to_string(tool).unwrap_or_default());
    assert_eq!(
        snapshot.material().tool_catalog_sha256,
        canonical_input_hash(&serde_json::to_value(catalog).expect("catalog"))
    );
    let plan = service.plan("prd").await.expect("published plan");
    assert_eq!(plan.revision, 1);
    assert_eq!(plan.work_units.len(), 2);
    assert_eq!(
        plan.work_units
            .iter()
            .find(|unit| unit.id == "implement")
            .expect("implementation unit")
            .dependencies,
        ["inspect"]
    );
    assert!(plan.approval.is_none());
}

#[tokio::test]
async fn planning_tool_catalog_and_execution_share_one_read_only_allowlist() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let snapshot = TaskWorkspaceBinding::bind_local("bound", &workspace, &[])
        .and_then(|binding| binding.snapshot_ref())
        .expect("workspace snapshot");
    let tools = PlanningToolService::from_workspace(&snapshot).expect("bound tools");
    let token = GenerationToken::new("planning", 1);
    // Bare service (no child controls attached): children tools stay
    // hidden — only run-wired services expose them (D9).
    assert_eq!(
        tool_names(&tools.list(token.clone()).await.expect("tool list")),
        [
            "git_diff",
            "git_log",
            "git_status",
            "glob",
            "list_files",
            "read_file",
            "search"
        ]
    );
    let before = tree(&workspace);
    for guessed in [
        "create_file",
        "edit",
        "delete_file",
        "apply_patch",
        "bash",
        "git_status",
        "process",
        "children",
        "verification",
        "plan.update",
    ] {
        let reply = tools
            .call(
                token.clone(),
                ToolCallRequest {
                    tool: guessed.into(),
                    operation_key: None,
                    input: serde_json::json!({"path": "owned.txt", "content": "bad"}),
                },
            )
            .await
            .expect("denial reply");
        assert_eq!(reply.error.expect("denied").code, "denied", "{guessed}");
    }
    assert_eq!(tree(&workspace), before, "guessed effects changed files");

    let unbound =
        PlanningToolService::from_workspace(&TaskWorkspaceBinding::unbound_read_only_snapshot())
            .expect("unbound tools");
    assert!(unbound
        .list(token.clone())
        .await
        .expect("unbound list")
        .is_empty());
    let denied = unbound
        .call(
            token,
            ToolCallRequest {
                tool: "read_file".into(),
                operation_key: None,
                input: serde_json::json!({"path": "tracked.txt"}),
            },
        )
        .await
        .expect("unbound denial");
    assert_eq!(denied.error.expect("denied").code, "denied");
}

#[tokio::test]
async fn free_form_fallback_is_stable_and_ready_never_auto_dispatches() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("fallback-revise", temp.path());
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let service = compose_with_builtin(
        &profile,
        &package,
        ScriptedModel::fixed("same free-form plan"),
    );
    service
        .create_task("fallback", "plan", TaskKind::PlanDraft, vec![])
        .await
        .expect("create task");
    service
        .set_task_preferences(
            "fallback",
            TaskPreferences {
                workspace_path: Some(workspace.display().to_string()),
                ..TaskPreferences::default()
            },
        )
        .await
        .expect("workspace");
    service
        .send_message("fallback", "first")
        .await
        .expect("first run");
    wait_for_kind(&service, "fallback", "plan.awaiting-approval").await;
    let first = service.plan("fallback").await.expect("first plan");

    service
        .send_message("fallback", "revise explicitly")
        .await
        .expect("revision run");
    let events = wait_for_event(&service, |event| {
        event.task_id == "fallback"
            && event.payload.get("journalKind")
                == Some(&serde_json::json!("plan.awaiting-approval"))
            && event.payload.get("revision") == Some(&serde_json::json!(2))
    })
    .await;
    let second = service.plan("fallback").await.expect("second plan");
    assert_eq!(second.revision, 2);
    assert_eq!(first.work_units[0].id, second.work_units[0].id);
    assert!(events.iter().any(|event| {
        event.task_id == "fallback"
            && event.payload.get("journalKind") == Some(&serde_json::json!("plan.invalidated"))
    }));

    service
        .approve_plan(
            "fallback",
            &second.revision_hash,
            "approval-fallback",
            "local-user",
            "session-fallback",
        )
        .await
        .expect("approve exact head");
    let executions_before = service
        .events_after(0, 1_000)
        .await
        .iter()
        .filter(|event| {
            event.task_id == "fallback"
                && event.payload.get("journalKind") == Some(&serde_json::json!("execution.started"))
        })
        .count();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(matches!(
        V1Store::open(&profile.database_path())
            .expect("store")
            .load_task("fallback")
            .await
            .expect("task"),
        state if matches!(state.execution, TaskExecution::Ready { .. })
    ));
    assert_eq!(
        service
            .events_after(0, 1_000)
            .await
            .iter()
            .filter(|event| {
                event.task_id == "fallback"
                    && event.payload.get("journalKind")
                        == Some(&serde_json::json!("execution.started"))
            })
            .count(),
        executions_before
    );
}

#[tokio::test]
async fn invalid_plan_material_fails_closed_before_awaiting_approval() {
    let invalid = [
        r#"{"work_units":[]}"#,
        r#"{"work_units":[{"id":"x","description":"one"},{"id":"x","description":"two"}]}"#,
        r#"{"work_units":[{"id":"x","description":"bad dependency","dependencies":["missing"]}]}"#,
        r#"{"work_units":[{"id":"","description":"empty id"}]}"#,
    ];
    for (index, output) in invalid.into_iter().enumerate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("checkout");
        write_workspace(&workspace);
        let profile = profile(&format!("invalid-{index}"), temp.path());
        let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
        let service = compose_with_builtin(&profile, &package, ScriptedModel::fixed(output));
        let task_id = format!("invalid-{index}");
        service
            .create_task(&task_id, "invalid", TaskKind::Implementation, vec![])
            .await
            .expect("create invalid task");
        service
            .set_task_preferences(
                &task_id,
                TaskPreferences {
                    workspace_path: Some(workspace.display().to_string()),
                    ..TaskPreferences::default()
                },
            )
            .await
            .expect("workspace");
        service
            .send_message(&task_id, "plan")
            .await
            .expect("dispatch");
        let events = wait_for_kind(&service, &task_id, "run.failed").await;
        let store = V1Store::open(&profile.database_path()).expect("store");
        assert!(matches!(
            store.load_task(&task_id).await.expect("task").execution,
            TaskExecution::Pending
        ));
        assert!(store
            .current_plan_revision(&task_id)
            .expect("plan query")
            .is_none());
        assert!(!events.iter().any(|event| {
            event.task_id == task_id
                && event.payload.get("journalKind")
                    == Some(&serde_json::json!("plan.awaiting-approval"))
        }));
    }
}

#[tokio::test]
async fn third_party_same_id_cannot_inherit_pre_v1_native_plan_publish_compatibility() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("checkout");
    write_workspace(&workspace);
    let profile = profile("untrusted-native-id", temp.path());
    let trusted = stage_native(temp.path(), "native.r-code", "1.0.0", true);
    let attacker = stage_native(temp.path(), "native.r-code", "9.0.0", false);
    let service = compose_with_builtin(&profile, &trusted, ScriptedModel::fixed(STRICT_PLAN));
    let installed = service
        .install_package_from_directory(&attacker)
        .expect("install same-id third-party package");
    service
        .create_task(
            "spoofed",
            "must fail closed",
            TaskKind::Implementation,
            vec![],
        )
        .await
        .expect("create task");
    service
        .set_task_preferences(
            "spoofed",
            TaskPreferences {
                workspace_path: Some(workspace.display().to_string()),
                ..TaskPreferences::default()
            },
        )
        .await
        .expect("workspace");
    service
        .send_message("spoofed", "plan")
        .await
        .expect("dispatch");
    let events = wait_for_kind(&service, "spoofed", "run.failed").await;
    let store = V1Store::open(&profile.database_path()).expect("store");
    let snapshot = store
        .load_run_snapshot(snapshot_id(&events, "spoofed"))
        .expect("snapshot query")
        .expect("snapshot");
    assert_eq!(
        snapshot.material().harness_package.content_digest,
        installed.package_ref.content_digest
    );
    assert!(!snapshot
        .material()
        .permissions
        .capabilities
        .contains(&HostService::PlanPublish.wire_name().to_string()));
    assert!(store
        .current_plan_revision("spoofed")
        .expect("plan query")
        .is_none());
}
