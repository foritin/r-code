//! P-GATE — daemon approval, invalidation and restart semantics.
//! macOS：daemon→native harness 链路依赖平台安全激活报告（P13），本 wave
//! 报告后端固定 none-this-wave/Unsupported——链路在 macOS 按设计拒绝启动；
//! 端到端用例由 linux/windows 腿运行，P13 报告落地后移除此门。
#![cfg(not(target_os = "macos"))]

mod p_gate_support;

use p_gate_support::daemon::Daemon;
use p_gate_support::{profile, stage_native, wait_for_kind, ScriptedModel, STRICT_PLAN};
use r_code_kernel::ports::JournalStore;
use r_code_kernel::task::{ModelRoute, TaskExecution, TaskKind, TaskPreferences};
use r_code_runtime::application::ApplicationService;
use r_code_runtime::services::settings_store::{ProviderEntry, SettingsStore, V1Settings};
use r_code_store::v1::V1Store;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

fn workspace(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).expect("workspace");
    std::fs::write(path.join("fixture.txt"), format!("fixture-{name}")).expect("workspace fixture");
    path
}

fn row_count(path: &Path, table: &str) -> i64 {
    Connection::open(path)
        .expect("database")
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("row count")
}

async fn planned_service(
    profile: &r_code_runtime::RuntimeProfile,
    package: &Path,
) -> ApplicationService {
    planned_service_with_plan(profile, package, STRICT_PLAN).await
}

async fn planned_service_with_plan(
    profile: &r_code_runtime::RuntimeProfile,
    package: &Path,
    plan: &str,
) -> ApplicationService {
    let service = ApplicationService::compose(
        profile,
        ScriptedModel::fixed(plan),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .expect("compose service");
    service.ensure_builtin(package).expect("built-in package");
    service
}

/// E06 semantic rebase of STRICT_PLAN for the explicit-dispatch pin: one
/// explicit send now drives the WHOLE two-unit DAG, so the implement unit's
/// `check:test` acceptance — never exercised pre-E06 (only the first unit
/// ran) — would land RepairRequired here (the sandbox gate is honestly
/// closed, so even a defined check:test is CheckUnavailable). The check
/// acceptance is dropped from this fixture's plan so review-ready stays
/// reachable and the pin keeps its "dispatch only after the explicit send"
/// core under the E06 one-send-whole-dag semantics.
const EXECUTION_PLAN: &str = r#"{"work_units":[{"id":"inspect","description":"inspect current checkout","dependencies":[],"acceptance":["read-only"],"write_paths":["src"]},{"id":"implement","description":"implement after approval","dependencies":["inspect"],"acceptance":["read-only"],"write_paths":["src"]}]}"#;

async fn publish(
    service: &ApplicationService,
    task_id: &str,
    workspace: &Path,
) -> r_code_runtime::application::PlanView {
    publish_kind(service, task_id, workspace, TaskKind::PlanDraft).await
}

async fn publish_kind(
    service: &ApplicationService,
    task_id: &str,
    workspace: &Path,
    kind: TaskKind,
) -> r_code_runtime::application::PlanView {
    service
        .create_task(task_id, "secret-free plan", kind, vec![])
        .await
        .expect("create task");
    service
        .set_task_preferences(
            task_id,
            TaskPreferences {
                workspace_path: Some(workspace.display().to_string()),
                ..TaskPreferences::default()
            },
        )
        .await
        .expect("workspace preference");
    service
        .send_message(task_id, "plan")
        .await
        .expect("plan run");
    wait_for_kind(service, task_id, "plan.awaiting-approval").await;
    service.plan(task_id).await.expect("plan view")
}

#[tokio::test]
async fn ready_implementation_dispatches_only_after_explicit_send() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile("explicit-execution", temp.path());
    let checkout = workspace(temp.path(), "checkout");
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let service = planned_service_with_plan(&profile, &package, EXECUTION_PLAN).await;
    let plan = publish_kind(&service, "explicit", &checkout, TaskKind::Implementation).await;
    service
        .approve_plan(
            "explicit",
            &plan.revision_hash,
            "approval-explicit",
            "local-user",
            "session-explicit",
        )
        .await
        .unwrap();
    let store = V1Store::open(&profile.database_path()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(matches!(
        store.load_task("explicit").await.unwrap().execution,
        TaskExecution::Ready { .. }
    ));
    // The approval gate is unchanged: approval alone never dispatches.
    assert!(!store
        .task_events("explicit")
        .iter()
        .any(|event| event.kind == "execution.started"));

    service
        .send_message("explicit", "execute approved unit")
        .await
        .unwrap();
    wait_for_kind(&service, "explicit", "review-ready").await;
    // E06: the one explicit send drives the WHOLE two-unit DAG — both units
    // run, each exactly once.
    assert_eq!(
        store
            .task_events("explicit")
            .iter()
            .filter(|event| event.kind == "execution.started")
            .count(),
        2
    );
    let started_units = store
        .task_events("explicit")
        .into_iter()
        .filter(|event| event.kind == "execution.started")
        .filter_map(|event| {
            event
                .payload
                .get("workUnitId")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        started_units,
        vec!["inspect".to_string(), "implement".to_string()],
        "the dependency-ready unit dispatches first"
    );
}

#[tokio::test]
async fn daemon_exact_approval_is_atomic_idempotent_secret_free_and_restart_safe() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile("daemon-approval", temp.path());
    let checkout = workspace(temp.path(), "checkout");
    let package = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let service = planned_service(&profile, &package).await;
    let task_a = publish(&service, "task-a", &checkout).await;
    let task_b = publish(&service, "task-b", &checkout).await;
    drop(service);

    let secret_name = format!("R_CODE_P_GATE_SECRET_{}", std::process::id());
    let secret_value = "PROVIDER_SECRET_MUST_NOT_LEAK";
    SettingsStore::for_profile(&profile)
        .compare_and_swap(
            0,
            V1Settings {
                revision: 0,
                providers: vec![ProviderEntry {
                    selection: "deepseek".into(),
                    model: "deepseek-chat".into(),
                    base_url: None,
                    protocol: None,
                    env_var: Some(secret_name.clone()),
                }],
                default_selection: Some("deepseek".into()),
            },
        )
        .expect("provider settings");

    let daemon = Daemon::start(&profile, Some((&secret_name, secret_value)));
    let mut client = daemon.connect(&profile, "p-gate-client").await;
    let fetched = client
        .call("plan.get", serde_json::json!({"taskId": "task-a"}))
        .await
        .expect("plan.get");
    assert_eq!(fetched["revisionHash"], task_a.revision_hash);
    assert!(!fetched.to_string().contains(secret_value));

    let wrong_hash = format!("sha256:{}", "0".repeat(64));
    client
        .call_with_id(
            "plan.approve",
            serde_json::json!({"taskId": "task-a", "revisionHash": wrong_hash}),
            "approve-wrong-hash",
        )
        .await
        .expect_err("wrong revision must fail");
    client
        .call_with_id(
            "plan.approve",
            serde_json::json!({"taskId": "task-b", "revisionHash": task_a.revision_hash}),
            "approve-other-task",
        )
        .await
        .expect_err("cross-task revision must fail");
    assert_eq!(row_count(&profile.database_path(), "plan_approvals"), 0);

    let params = serde_json::json!({
        "taskId": "task-a",
        "revisionHash": task_a.revision_hash,
    });
    let approved = client
        .call_with_id("plan.approve", params.clone(), "approve-exact")
        .await
        .expect("approve exact hash");
    assert_eq!(approved["state"], "ready");
    assert_eq!(
        client
            .call_with_id("plan.approve", params, "approve-exact")
            .await
            .expect("idempotent command replay"),
        approved
    );
    client
        .call_with_id(
            "plan.approve",
            serde_json::json!({
                "taskId": "task-a",
                "revisionHash": task_a.revision_hash,
            }),
            "approve-conflict",
        )
        .await
        .expect_err("a second approval command conflicts");

    let store = V1Store::open(&profile.database_path()).expect("store");
    assert_eq!(row_count(&profile.database_path(), "plan_approvals"), 1);
    assert_eq!(
        store
            .task_events("task-a")
            .iter()
            .filter(|event| event.kind == "plan.approved")
            .count(),
        1
    );
    let approval = store
        .load_active_plan_approval("task-a")
        .expect("approval query")
        .expect("active approval");
    assert_eq!(approval.actor.actor_id, "p-gate-client");
    assert_eq!(approval.actor.session_id, "approve-exact");
    assert_eq!(approval.actor.scope, "plan.approve");
    assert_eq!(approval.plan_revision.as_str(), task_a.revision_hash);
    assert!(matches!(
        store.load_task("task-a").await.expect("task-a").execution,
        TaskExecution::Ready { .. }
    ));
    drop(client);
    drop(daemon);

    let restarted = Daemon::start(&profile, Some((&secret_name, secret_value)));
    let mut client = restarted.connect(&profile, "p-gate-after-restart").await;
    let ready = client
        .call("plan.get", serde_json::json!({"taskId": "task-a"}))
        .await
        .expect("ready after restart");
    assert_eq!(ready["state"], "ready");
    assert_eq!(ready["approval"]["approval_id"], "approve-exact");
    let executions_before = store
        .task_events("task-a")
        .iter()
        .filter(|event| event.kind == "execution.started")
        .count();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        store
            .task_events("task-a")
            .iter()
            .filter(|event| event.kind == "execution.started")
            .count(),
        executions_before,
        "approval alone must never auto-dispatch execution"
    );

    let revised = client
        .call_with_id(
            "plan.revise",
            serde_json::json!({"taskId": "task-a", "reason": "requirements changed"}),
            "revise-exact",
        )
        .await
        .expect("explicit revise");
    assert_eq!(revised["state"], "pending");
    assert!(store
        .load_active_plan_approval("task-a")
        .expect("approval query")
        .is_none());
    assert!(matches!(
        store.load_task("task-a").await.expect("task-a").execution,
        TaskExecution::Pending
    ));
    assert_eq!(task_b.revision, 1);
}

#[tokio::test]
async fn every_frozen_run_material_change_supersedes_approval_atomically() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile("material-invalidation", temp.path());
    let checkout = workspace(temp.path(), "checkout");
    let other_checkout = workspace(temp.path(), "other-checkout");
    let native = stage_native(temp.path(), "native.r-code", "1.0.0", false);
    let alternate = stage_native(temp.path(), "alternate.r-code", "1.0.0", true);
    let service = planned_service(&profile, &native).await;
    service
        .install_package_from_directory(&alternate)
        .expect("alternate harness");

    for task_id in ["route", "prompt", "workspace", "inference", "harness"] {
        let plan = publish(&service, task_id, &checkout).await;
        service
            .approve_plan(
                task_id,
                &plan.revision_hash,
                &format!("approval-{task_id}"),
                "local-user",
                &format!("session-{task_id}"),
            )
            .await
            .expect("approve task");
    }

    let route_env = format!("R_CODE_P_GATE_ROUTE_{}", std::process::id());
    std::env::set_var(&route_env, "test-only-provider-secret");
    SettingsStore::for_profile(&profile)
        .compare_and_swap(
            0,
            V1Settings {
                revision: 0,
                providers: vec![ProviderEntry {
                    selection: "deepseek".into(),
                    model: "deepseek-chat".into(),
                    base_url: None,
                    protocol: None,
                    env_var: Some(route_env.clone()),
                }],
                default_selection: Some("deepseek".into()),
            },
        )
        .expect("route provider settings");
    let mut route = service
        .task_preferences("route")
        .await
        .expect("route prefs");
    route.model_route = Some(ModelRoute::HostProvider {
        provider_id: "deepseek".into(),
        model_id: Some("deepseek-chat".into()),
    });
    service
        .set_task_preferences("route", route)
        .await
        .expect("route change");

    let mut prompt = service
        .task_preferences("prompt")
        .await
        .expect("prompt prefs");
    prompt.system_prompt = Some("changed prompt".into());
    service
        .set_task_preferences("prompt", prompt)
        .await
        .expect("prompt change");

    let mut workspace_prefs = service
        .task_preferences("workspace")
        .await
        .expect("workspace prefs");
    workspace_prefs.workspace_path = Some(other_checkout.display().to_string());
    service
        .set_task_preferences("workspace", workspace_prefs)
        .await
        .expect("workspace change");

    let mut inference = service
        .task_preferences("inference")
        .await
        .expect("inference prefs");
    inference.inference = Some(serde_json::json!({"temperature": 0.2}));
    service
        .set_task_preferences("inference", inference)
        .await
        .expect("inference change");
    service
        .select_harness("harness", "alternate.r-code")
        .await
        .expect("harness change");

    let store = V1Store::open(&profile.database_path()).expect("store");
    for task_id in ["route", "prompt", "workspace", "inference", "harness"] {
        assert!(matches!(
            store.load_task(task_id).await.expect("task").execution,
            TaskExecution::Pending
        ));
        assert!(store
            .load_active_plan_approval(task_id)
            .expect("approval query")
            .is_none());
        assert_eq!(
            store
                .task_events(task_id)
                .iter()
                .filter(|event| event.kind == "plan.invalidated")
                .count(),
            1,
            "{task_id} must atomically record one invalidation"
        );
    }
    std::env::remove_var(route_env);
}
