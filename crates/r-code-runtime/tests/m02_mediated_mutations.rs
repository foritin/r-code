use r_code_harness_protocol::rpc::{error_code, RpcRequest};
use r_code_harness_protocol::services::{
    ArtifactRef, NetworkCeiling, OutputBlock, ToolCallReply, ToolCallRequest, ToolDescriptor,
    WorkUnitEffectClass, WorkUnitWire,
};
use r_code_harness_protocol::{
    canonical_input_hash, HostService, OperationKey, RpcId, RunIdentity,
};
use r_code_kernel::plans::{
    PlanApprovalActor, PlanRevision, PlanRevisionMaterial, PLAN_APPROVE_SCOPE,
};
use r_code_kernel::ports::{GenerationToken, JournalStore, RunGuard, ServiceError, ToolService};
use r_code_kernel::task::{OperationReceipt, ReceiptOutcome, WorkUnit, WorkUnitStatus};
use r_code_kernel::testing::{
    FakeModelService, FakeProcessService, FakeToolService, MemoryJournal,
};
use r_code_runtime::application::ApplicationService;
use r_code_runtime::plugins::{HostRouter, IgnoreQuestions};
use r_code_runtime::services::artifacts::{sha256_hex, ArtifactStore};
use r_code_runtime::services::mutations::{
    MutationCheckpoint, MutationExecutionError, MutationExecutor, MutationFaultHook,
};
use r_code_runtime::services::tools::ExecutionToolService;
use r_code_runtime::services::workspaces::TaskWorkspaceBinding;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::{MutationState, V1Store};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

const TASK_ID: &str = "task-m02";
const ATTEMPT_ID: &str = "attempt-m02";

fn plan_and_unit(
    task_id: &str,
    revision: u64,
    parent_revision: Option<r_code_kernel::task::PlanRevisionRef>,
    read_paths: &[&str],
    write_paths: &[&str],
    repo_exclusive: bool,
) -> (PlanRevision, WorkUnit) {
    let plan = PlanRevision::new(PlanRevisionMaterial {
        task_id: task_id.into(),
        revision,
        parent_revision,
        current_base_hash: format!("sha256:base-{revision}"),
        workspace_baseline: format!("sha256:workspace-{revision}"),
        route_digest: "sha256:route".into(),
        prompt_digest: "sha256:prompt".into(),
        permission_digest: "sha256:permissions".into(),
        check_digest: "sha256:checks".into(),
        required_checks: vec!["check:test".into()],
        work_units: vec![WorkUnitWire {
            id: "unit-1".into(),
            description: "apply one approved mutation".into(),
            dependencies: vec![],
            acceptance: vec!["check:test".into()],
            read_paths: read_paths.iter().map(|path| (*path).into()).collect(),
            write_paths: write_paths.iter().map(|path| (*path).into()).collect(),
            repo_exclusive,
            ephemeral_roots: vec![],
            effect_class: WorkUnitEffectClass::ReadOnly,
            network_ceiling: NetworkCeiling::Offline,
        }],
    })
    .expect("valid plan");
    let wire = plan.material().work_units[0].clone();
    let unit = WorkUnit {
        id: wire.id,
        description: wire.description,
        dependencies: wire.dependencies,
        acceptance: wire.acceptance,
        read_paths: wire.read_paths,
        write_paths: wire.write_paths,
        repo_exclusive: wire.repo_exclusive,
        ephemeral_roots: wire.ephemeral_roots,
        effect_class: wire.effect_class,
        network_ceiling: wire.network_ceiling,
        status: WorkUnitStatus::Pending,
    };
    (plan, unit)
}

struct Fixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    database_path: PathBuf,
    artifacts_root: PathBuf,
    store: Arc<V1Store>,
    artifacts: Arc<ArtifactStore>,
    plan: PlanRevision,
    unit: WorkUnit,
}

impl Fixture {
    fn new(write_paths: &[&str]) -> Self {
        Self::with_scopes(&[], write_paths, false)
    }

    fn with_scopes(read_paths: &[&str], write_paths: &[&str], repo_exclusive: bool) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::create_dir_all(workspace.join("src2")).unwrap();
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::write(workspace.join("ignored.sentinel"), b"USER_SENTINEL").unwrap();
        let database_path = temp.path().join("store.db");
        let artifacts_root = temp.path().join("artifacts");
        let store = Arc::new(V1Store::open(&database_path).unwrap());
        let artifacts = Arc::new(ArtifactStore::for_task(&artifacts_root, TASK_ID));
        let (plan, unit) = plan_and_unit(TASK_ID, 1, None, read_paths, write_paths, repo_exclusive);
        Self {
            _temp: temp,
            workspace,
            database_path,
            artifacts_root,
            store,
            artifacts,
            plan,
            unit,
        }
    }

    fn binding(&self) -> TaskWorkspaceBinding {
        TaskWorkspaceBinding::bind_local(TASK_ID, &self.workspace, &[]).unwrap()
    }

    fn executor(&self, attempt_id: &str) -> MutationExecutor {
        MutationExecutor::new(
            &self.plan,
            &self.unit,
            self.binding(),
            self.store.clone(),
            self.artifacts.clone(),
            attempt_id,
        )
        .expect("mutation executor")
    }

    fn service(&self, attempt_id: &str) -> ExecutionToolService {
        ExecutionToolService::new(
            &self.plan,
            &self.unit,
            self.binding(),
            self.store.clone(),
            self.artifacts.clone(),
            attempt_id,
        )
        .expect("execution tools")
    }

    fn workspace_key(&self) -> String {
        let canonical = std::fs::canonicalize(&self.workspace).unwrap();
        format!(
            "sha256:{}",
            sha256_hex(canonical.to_string_lossy().as_bytes())
        )
    }
}

fn token() -> GenerationToken {
    GenerationToken::new("run-m02", 1)
}

async fn call(
    service: &ExecutionToolService,
    tool: &str,
    input: serde_json::Value,
    operation_key: Option<&str>,
) -> ToolCallReply {
    service
        .call(
            token(),
            ToolCallRequest {
                tool: tool.into(),
                input,
                operation_key: operation_key.map(OperationKey::new),
            },
        )
        .await
        .expect("tool transport")
}

fn assert_error_code(reply: &ToolCallReply, code: &str) {
    assert_eq!(
        reply.error.as_ref().map(|error| error.code.as_str()),
        Some(code)
    );
    assert!(reply.output.is_empty());
}

fn artifact_from_journal_ref(encoded: &str) -> ArtifactRef {
    serde_json::from_value(serde_json::from_str(encoded).unwrap()).unwrap()
}

#[cfg(windows)]
fn create_directory_link(target: &Path, link: &Path) {
    let link_text = link.to_string_lossy().replace('/', "\\");
    let target_text = target.to_string_lossy().replace('/', "\\");
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link_text)
        .arg(&target_text)
        .output()
        .expect("run mklink");
    assert!(
        output.status.success(),
        "create junction {} -> {} failed: {} {}",
        link.display(),
        target.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn create_directory_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

struct TestHook {
    point: MutationCheckpoint,
    fired: AtomicBool,
    inject: bool,
    action: Arc<dyn Fn() + Send + Sync>,
}

impl TestHook {
    fn inject(point: MutationCheckpoint) -> Arc<Self> {
        Arc::new(Self {
            point,
            fired: AtomicBool::new(false),
            inject: true,
            action: Arc::new(|| {}),
        })
    }

    fn mutate(point: MutationCheckpoint, action: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            point,
            fired: AtomicBool::new(false),
            inject: false,
            action: Arc::new(action),
        })
    }
}

impl MutationFaultHook for TestHook {
    fn checkpoint(
        &self,
        point: MutationCheckpoint,
        _operation_id: &str,
    ) -> Result<(), MutationExecutionError> {
        if point == self.point && !self.fired.swap(true, Ordering::SeqCst) {
            (self.action)();
            if self.inject {
                return Err(MutationExecutionError::Injected);
            }
        }
        Ok(())
    }
}

fn runtime_profile(root: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(root.join("profile"))
            .with_ipc_name(format!("m02-{}", uuid::Uuid::new_v4().simple())),
    )
    .unwrap()
}

fn publish_and_approve(store: &V1Store, plan: &PlanRevision, approval_id: &str) {
    store.publish_plan_revision(plan, None).unwrap();
    store
        .approve_plan_revision(
            &plan.material().task_id,
            plan.reference(),
            approval_id,
            PlanApprovalActor::new("actor", "session", PLAN_APPROVE_SCOPE).unwrap(),
        )
        .unwrap();
}

#[test]
fn execution_tools_require_exact_active_approval_unit_task_and_write_scope() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    let profile = runtime_profile(temp.path());
    let service = ApplicationService::compose(
        &profile,
        Arc::new(FakeModelService::default()),
        Arc::new(FakeToolService::default()),
    )
    .unwrap();
    let store = V1Store::open(&profile.database_path()).unwrap();
    let (plan, unit) = plan_and_unit("approved-task", 1, None, &[], &["src"], false);
    store.publish_plan_revision(&plan, None).unwrap();
    let binding = || TaskWorkspaceBinding::bind_local("approved-task", &workspace, &[]).unwrap();
    let workspace_key = format!(
        "sha256:{}",
        sha256_hex(
            std::fs::canonicalize(&workspace)
                .unwrap()
                .to_string_lossy()
                .as_bytes()
        )
    );

    assert!(service
        .runs()
        .execution_tools_for("approved-task", "attempt-no-approval", &unit, binding())
        .is_err());
    assert!(store.active_leases(&workspace_key).unwrap().is_empty());

    store
        .approve_plan_revision(
            "approved-task",
            plan.reference(),
            "approval-1",
            PlanApprovalActor::new("actor", "session", PLAN_APPROVE_SCOPE).unwrap(),
        )
        .unwrap();
    let tools = service
        .runs()
        .execution_tools_for("approved-task", "attempt-valid", &unit, binding())
        .expect("exact active approval");
    assert!(tools.release().unwrap());

    let mut wrong_unit = unit.clone();
    wrong_unit.id = "guessed-unit".into();
    assert!(service
        .runs()
        .execution_tools_for(
            "approved-task",
            "attempt-wrong-unit",
            &wrong_unit,
            binding()
        )
        .is_err());
    let wrong_binding = TaskWorkspaceBinding::bind_local("other-task", &workspace, &[]).unwrap();
    assert!(service
        .runs()
        .execution_tools_for("approved-task", "attempt-wrong-task", &unit, wrong_binding)
        .is_err());
    assert!(store.active_leases(&workspace_key).unwrap().is_empty());

    let (replacement, _) = plan_and_unit(
        "approved-task",
        2,
        Some(plan.reference().clone()),
        &[],
        &["src"],
        false,
    );
    store
        .publish_plan_revision(&replacement, Some(plan.reference()))
        .unwrap();
    assert!(service
        .runs()
        .execution_tools_for("approved-task", "attempt-superseded", &unit, binding())
        .is_err());
    assert!(store.active_leases(&workspace_key).unwrap().is_empty());

    let (read_only_plan, read_only_unit) =
        plan_and_unit("read-only-task", 1, None, &["src"], &[], false);
    publish_and_approve(&store, &read_only_plan, "approval-read-only");
    let read_only_binding =
        TaskWorkspaceBinding::bind_local("read-only-task", &workspace, &[]).unwrap();
    assert!(service
        .runs()
        .execution_tools_for(
            "read-only-task",
            "attempt-empty-writes",
            &read_only_unit,
            read_only_binding
        )
        .is_err());
    assert!(store.active_leases(&workspace_key).unwrap().is_empty());
}

#[tokio::test]
async fn discovery_and_guessed_calls_are_fail_closed_and_component_aware() {
    let fixture = Fixture::new(&["src"]);
    let service = fixture.service(ATTEMPT_ID);
    let mut names = service
        .list(token())
        .await
        .unwrap()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            "apply_patch",
            "create_file",
            "delete_file",
            "edit",
            "git_diff",
            "git_log",
            "git_status",
            "glob",
            "list_files",
            "read_file",
            "search"
        ]
    );
    assert!(names
        .iter()
        .all(|name| !matches!(name.as_str(), "bash" | "shell" | "git" | "process")));

    let empty = Fixture::with_scopes(&[], &[], false);
    let empty_service = empty.service("attempt-read-only");
    let mut read_names = empty_service
        .list(token())
        .await
        .unwrap()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    read_names.sort();
    assert_eq!(
        read_names,
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

    std::fs::write(
        fixture.workspace.join("src/approved.txt"),
        b"approved-before",
    )
    .unwrap();
    std::fs::write(
        fixture.workspace.join("src2/outside.txt"),
        b"outside-before",
    )
    .unwrap();
    std::fs::hard_link(
        fixture.workspace.join("src/approved.txt"),
        fixture.workspace.join("src2/hardlink.txt"),
    )
    .unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("escaped.txt"), b"outside-link-before").unwrap();
    create_directory_link(outside.path(), &fixture.workspace.join("src/escape"));

    let denied_calls = [
        (
            "bash",
            serde_json::json!({"path": "src/approved.txt", "content": "SECRET"}),
            Some("guess-bash"),
        ),
        (
            "create_file",
            serde_json::json!({"path": "src/no-key.txt", "content": "SECRET"}),
            None,
        ),
        (
            "apply_patch",
            serde_json::json!({"path": "src2/outside.txt", "content": "SECRET"}),
            Some("outside-prefix"),
        ),
        (
            "apply_patch",
            serde_json::json!({"path": ".GiT/HEAD", "content": "SECRET"}),
            Some("git"),
        ),
        (
            "apply_patch",
            serde_json::json!({"path": fixture.workspace.join("src/approved.txt"), "content": "SECRET"}),
            Some("absolute"),
        ),
        (
            "apply_patch",
            serde_json::json!({"path": "src2/hardlink.txt", "content": "SECRET"}),
            Some("hardlink-alias"),
        ),
        (
            "create_file",
            serde_json::json!({"path": "src/escape/new.txt", "content": "SECRET"}),
            Some("link-escape"),
        ),
    ];
    for (tool, input, key) in denied_calls {
        assert!(call(&service, tool, input, key).await.error.is_some());
    }
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/approved.txt")).unwrap(),
        b"approved-before"
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join("src2/outside.txt")).unwrap(),
        b"outside-before"
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join("src2/hardlink.txt")).unwrap(),
        b"approved-before"
    );
    assert_eq!(
        std::fs::read(outside.path().join("escaped.txt")).unwrap(),
        b"outside-link-before"
    );
    assert!(!outside.path().join("new.txt").exists());
    for key in [
        "guess-bash",
        "outside-prefix",
        "git",
        "absolute",
        "hardlink-alias",
        "link-escape",
    ] {
        assert!(fixture
            .store
            .load_mutation_operation(&format!("{ATTEMPT_ID}::{key}"))
            .unwrap()
            .is_none());
    }
    assert!(service.release().unwrap());
}

#[tokio::test]
async fn mediated_create_edit_patch_and_delete_have_strict_file_semantics() {
    let fixture = Fixture::new(&["src"]);
    std::fs::write(fixture.workspace.join("src/main.txt"), b"alpha beta").unwrap();
    std::fs::write(fixture.workspace.join("src/many.txt"), b"x x").unwrap();
    std::fs::write(fixture.workspace.join("src/binary.bin"), [0xff, 0xfe]).unwrap();
    let service = fixture.service(ATTEMPT_ID);

    let created = call(
        &service,
        "create_file",
        serde_json::json!({"path": "src/empty.txt", "content": ""}),
        Some("create-empty"),
    )
    .await;
    assert!(created.error.is_none());
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/empty.txt")).unwrap(),
        b""
    );
    let created_record = fixture
        .store
        .load_mutation_operation(&format!("{ATTEMPT_ID}::create-empty"))
        .unwrap()
        .unwrap();
    assert_eq!(created_record.state, MutationState::Receipted);
    assert!(created_record.files[0].before_sha256.is_none());
    assert!(created_record.files[0].before_cas_ref.is_none());
    assert!(created_record.files[0].after_sha256.is_some());
    assert!(created_record.files[0].after_cas_ref.is_some());

    assert_error_code(
        &call(
            &service,
            "create_file",
            serde_json::json!({"path": "src/main.txt", "content": "must-not-write"}),
            Some("create-existing"),
        )
        .await,
        "conflict",
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/main.txt")).unwrap(),
        b"alpha beta"
    );

    let patched = call(
        &service,
        "apply_patch",
        serde_json::json!({"path": "src/main.txt", "content": "one target two"}),
        Some("full-patch"),
    )
    .await;
    assert!(patched.error.is_none());
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/main.txt")).unwrap(),
        b"one target two"
    );

    let current = std::fs::read(fixture.workspace.join("src/main.txt")).unwrap();
    let revision = format!("blake3:{}", blake3::hash(&current).to_hex());
    let edited = call(
        &service,
        "edit",
        serde_json::json!({
            "path": "src/main.txt",
            "old_string": "target",
            "new_string": "changed",
            "expected_revision": revision
        }),
        Some("edit-unique"),
    )
    .await;
    assert!(edited.error.is_none());
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/main.txt")).unwrap(),
        b"one changed two"
    );

    assert_error_code(
        &call(
            &service,
            "edit",
            serde_json::json!({
                "path": "src/main.txt",
                "old_string": "changed",
                "new_string": "bad",
                "expected_revision": "blake3:stale"
            }),
            Some("edit-stale"),
        )
        .await,
        "conflict",
    );
    assert_error_code(
        &call(
            &service,
            "edit",
            serde_json::json!({"path": "src/many.txt", "old_string": "x", "new_string": "y"}),
            Some("edit-ambiguous"),
        )
        .await,
        "conflict",
    );
    let all = call(
        &service,
        "edit",
        serde_json::json!({
            "path": "src/many.txt",
            "old_string": "x",
            "new_string": "y",
            "replace_all": true
        }),
        Some("edit-all"),
    )
    .await;
    assert!(all.error.is_none());
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/many.txt")).unwrap(),
        b"y y"
    );

    assert_error_code(
        &call(
            &service,
            "edit",
            serde_json::json!({
                "path": "src/binary.bin",
                "old_string": "x",
                "new_string": "y"
            }),
            Some("edit-binary"),
        )
        .await,
        "invalid-input",
    );
    for (tool, input, key) in [
        (
            "create_file",
            serde_json::json!({"path": "src/missing-content.txt"}),
            "missing-create-content",
        ),
        (
            "apply_patch",
            serde_json::json!({"path": "src/main.txt"}),
            "missing-patch-content",
        ),
        (
            "edit",
            serde_json::json!({"path": "src/main.txt", "new_string": "x"}),
            "missing-edit-anchor",
        ),
        ("delete_file", serde_json::json!({}), "missing-delete-path"),
    ] {
        assert_error_code(
            &call(&service, tool, input, Some(key)).await,
            "invalid-input",
        );
    }

    let deleted = call(
        &service,
        "delete_file",
        serde_json::json!({"path": "src/empty.txt"}),
        Some("delete-empty"),
    )
    .await;
    assert!(deleted.error.is_none());
    assert!(!fixture.workspace.join("src/empty.txt").exists());
    let deleted_record = fixture
        .store
        .load_mutation_operation(&format!("{ATTEMPT_ID}::delete-empty"))
        .unwrap()
        .unwrap();
    assert!(deleted_record.files[0].before_sha256.is_some());
    assert!(deleted_record.files[0].after_sha256.is_none());
    assert!(deleted_record.files[0].after_cas_ref.is_none());
    assert!(service.release().unwrap());
}

#[test]
fn every_journal_fault_window_reconciles_without_repeating_the_effect() {
    for (index, point) in [
        MutationCheckpoint::AfterPrepare,
        MutationCheckpoint::BeforeEffect,
        MutationCheckpoint::AfterEffect,
        MutationCheckpoint::AfterApplied,
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = Fixture::new(&["src"]);
        let path = fixture.workspace.join("src/item.txt");
        std::fs::write(&path, b"before SECRET_CONTENT").unwrap();
        let operation_key = OperationKey::new(format!("fault-{index}"));
        let operation_id = format!("{ATTEMPT_ID}::{}", operation_key.0);
        let input = serde_json::json!({
            "path": "src/item.txt",
            "content": "after SECRET_CONTENT"
        });
        let executor = fixture
            .executor(ATTEMPT_ID)
            .with_fault_hook(TestHook::inject(point));

        assert_eq!(
            executor.execute("apply_patch", &input, &operation_key),
            Err(MutationExecutionError::Injected)
        );
        let interrupted = fixture
            .store
            .load_mutation_operation(&operation_id)
            .unwrap()
            .unwrap();
        let expected_state = if point == MutationCheckpoint::AfterApplied {
            MutationState::Applied
        } else {
            MutationState::Prepared
        };
        assert_eq!(interrupted.state, expected_state);
        let file = &interrupted.files[0];
        assert!(file.before_sha256.is_some() && file.after_sha256.is_some());
        assert!(file.before_cas_ref.is_some() && file.after_cas_ref.is_some());

        let reopened_artifacts = ArtifactStore::for_task(&fixture.artifacts_root, TASK_ID);
        let before_ref = artifact_from_journal_ref(file.before_cas_ref.as_deref().unwrap());
        let after_ref = artifact_from_journal_ref(file.after_cas_ref.as_deref().unwrap());
        assert_eq!(
            reopened_artifacts.read_all(&before_ref).unwrap(),
            b"before SECRET_CONTENT"
        );
        assert_eq!(
            reopened_artifacts.read_all(&after_ref).unwrap(),
            b"after SECRET_CONTENT"
        );

        let first_reply = executor
            .execute("apply_patch", &input, &operation_key)
            .expect("reconcile faulted operation");
        assert_eq!(first_reply.state, MutationState::Receipted);
        assert_eq!(std::fs::read(&path).unwrap(), b"after SECRET_CONTENT");
        let replay = executor
            .execute("apply_patch", &input, &operation_key)
            .expect("stable receipt replay");
        assert_eq!(replay, first_reply);
        let rendered = serde_json::to_string(&replay).unwrap();
        assert!(!rendered.contains("SECRET_CONTENT"));
        assert!(executor.release().unwrap());
        assert!(fixture
            .store
            .active_leases(&fixture.workspace_key())
            .unwrap()
            .is_empty());
    }
}

#[test]
fn restart_reconciles_prepared_after_effect_from_durable_cas() {
    let Fixture {
        _temp,
        workspace,
        database_path,
        artifacts_root,
        store,
        artifacts,
        plan,
        unit,
    } = Fixture::new(&["src"]);
    let path = workspace.join("src/restart.txt");
    std::fs::write(&path, b"before restart").unwrap();
    let input = serde_json::json!({"path": "src/restart.txt", "content": "after restart"});
    let key = OperationKey::new("restart-key");
    let binding = TaskWorkspaceBinding::bind_local(TASK_ID, &workspace, &[]).unwrap();
    let executor = MutationExecutor::new(
        &plan,
        &unit,
        binding,
        store.clone(),
        artifacts.clone(),
        ATTEMPT_ID,
    )
    .unwrap()
    .with_fault_hook(TestHook::inject(MutationCheckpoint::AfterEffect));
    assert_eq!(
        executor.execute("apply_patch", &input, &key),
        Err(MutationExecutionError::Injected)
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"after restart");
    drop(executor);
    drop(store);
    drop(artifacts);

    let restarted_store = Arc::new(V1Store::open(&database_path).unwrap());
    let restarted_artifacts = Arc::new(ArtifactStore::for_task(&artifacts_root, TASK_ID));
    let restarted = MutationExecutor::new(
        &plan,
        &unit,
        TaskWorkspaceBinding::bind_local(TASK_ID, &workspace, &[]).unwrap(),
        restarted_store.clone(),
        restarted_artifacts,
        ATTEMPT_ID,
    )
    .unwrap();
    let reply = restarted
        .execute("apply_patch", &input, &key)
        .expect("restart reconciliation");
    assert_eq!(reply.state, MutationState::Receipted);
    assert_eq!(
        restarted_store
            .load_mutation_operation(&format!("{ATTEMPT_ID}::restart-key"))
            .unwrap()
            .unwrap()
            .state,
        MutationState::Receipted
    );
    assert!(restarted.release().unwrap());
}

#[test]
fn changed_input_conflicts_and_receipted_retry_never_overwrites_user_bytes() {
    let fixture = Fixture::new(&["src"]);
    let path = fixture.workspace.join("src/item.txt");
    std::fs::write(&path, b"before").unwrap();
    let executor = fixture.executor(ATTEMPT_ID);
    let key = OperationKey::new("stable-key");
    let input = serde_json::json!({"path": "src/item.txt", "content": "agent-after"});
    let receipt = executor.execute("apply_patch", &input, &key).unwrap();
    assert_eq!(receipt.state, MutationState::Receipted);

    std::fs::write(&path, b"user-later-edit").unwrap();
    assert_eq!(
        executor.execute("apply_patch", &input, &key).unwrap(),
        receipt
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"user-later-edit");
    assert_eq!(
        executor.execute(
            "apply_patch",
            &serde_json::json!({"path": "src/item.txt", "content": "different input"}),
            &key,
        ),
        Err(MutationExecutionError::Conflict)
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"user-later-edit");
    assert!(executor.release().unwrap());
}

#[test]
fn prepared_operation_conflicts_when_current_is_neither_before_nor_after() {
    let fixture = Fixture::new(&["src"]);
    let path = fixture.workspace.join("src/item.txt");
    std::fs::write(&path, b"before").unwrap();
    let executor = fixture
        .executor(ATTEMPT_ID)
        .with_fault_hook(TestHook::inject(MutationCheckpoint::AfterPrepare));
    let key = OperationKey::new("external-edit");
    let input = serde_json::json!({"path": "src/item.txt", "content": "agent-after"});
    assert_eq!(
        executor.execute("apply_patch", &input, &key),
        Err(MutationExecutionError::Injected)
    );
    std::fs::write(&path, b"user-concurrent-edit").unwrap();
    assert_eq!(
        executor.execute("apply_patch", &input, &key),
        Err(MutationExecutionError::Conflict)
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"user-concurrent-edit");
    assert_eq!(
        fixture
            .store
            .load_mutation_operation(&format!("{ATTEMPT_ID}::external-edit"))
            .unwrap()
            .unwrap()
            .state,
        MutationState::Conflict
    );
    assert!(executor.release().unwrap());
}

#[test]
fn physical_target_parent_and_content_changes_are_revalidated_at_effect_time() {
    {
        let fixture = Fixture::new(&["src"]);
        let path = fixture.workspace.join("src/content.txt");
        std::fs::write(&path, b"before").unwrap();
        let action_path = path.clone();
        let executor = fixture
            .executor(ATTEMPT_ID)
            .with_fault_hook(TestHook::mutate(
                MutationCheckpoint::BeforeEffect,
                move || std::fs::write(&action_path, b"user-content").unwrap(),
            ));
        assert_eq!(
            executor.execute(
                "apply_patch",
                &serde_json::json!({"path": "src/content.txt", "content": "agent"}),
                &OperationKey::new("content-swap"),
            ),
            Err(MutationExecutionError::Conflict)
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"user-content");
        assert!(executor.release().unwrap());
    }

    {
        let fixture = Fixture::new(&["src"]);
        let path = fixture.workspace.join("src/target.txt");
        let held = fixture.workspace.join("src/held-target.txt");
        std::fs::write(&path, b"before").unwrap();
        let action_path = path.clone();
        let action_held = held.clone();
        let executor = fixture
            .executor(ATTEMPT_ID)
            .with_fault_hook(TestHook::mutate(
                MutationCheckpoint::BeforeEffect,
                move || {
                    std::fs::rename(&action_path, &action_held).unwrap();
                    std::fs::write(&action_path, b"replacement-target").unwrap();
                },
            ));
        assert_eq!(
            executor.execute(
                "apply_patch",
                &serde_json::json!({"path": "src/target.txt", "content": "agent"}),
                &OperationKey::new("target-swap"),
            ),
            Err(MutationExecutionError::Conflict)
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement-target");
        assert_eq!(std::fs::read(&held).unwrap(), b"before");
        assert!(executor.release().unwrap());
    }

    {
        let fixture = Fixture::new(&["src"]);
        let parent = fixture.workspace.join("src/parent");
        let held = fixture.workspace.join("src/parent-held");
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(parent.join("item.txt"), b"before").unwrap();
        std::fs::write(outside.path().join("item.txt"), b"outside-user").unwrap();
        let action_parent = parent.clone();
        let action_held = held.clone();
        let outside_path = outside.path().to_path_buf();
        let executor = fixture
            .executor(ATTEMPT_ID)
            .with_fault_hook(TestHook::mutate(
                MutationCheckpoint::BeforeEffect,
                move || {
                    std::fs::rename(&action_parent, &action_held).unwrap();
                    create_directory_link(&outside_path, &action_parent);
                },
            ));
        assert_eq!(
            executor.execute(
                "apply_patch",
                &serde_json::json!({"path": "src/parent/item.txt", "content": "agent"}),
                &OperationKey::new("parent-swap"),
            ),
            Err(MutationExecutionError::Conflict)
        );
        assert_eq!(
            std::fs::read(outside.path().join("item.txt")).unwrap(),
            b"outside-user"
        );
        assert_eq!(std::fs::read(held.join("item.txt")).unwrap(), b"before");
        assert!(executor.release().unwrap());
    }

    {
        let fixture = Fixture::new(&["src"]);
        let path = fixture.workspace.join("src/after-effect.txt");
        std::fs::write(&path, b"before").unwrap();
        let action_path = path.clone();
        let executor = fixture
            .executor(ATTEMPT_ID)
            .with_fault_hook(TestHook::mutate(
                MutationCheckpoint::AfterEffect,
                move || std::fs::write(&action_path, b"user-after-effect").unwrap(),
            ));
        assert_eq!(
            executor.execute(
                "apply_patch",
                &serde_json::json!({"path": "src/after-effect.txt", "content": "agent"}),
                &OperationKey::new("after-effect-user"),
            ),
            Err(MutationExecutionError::Conflict)
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"user-after-effect");
        assert!(executor.release().unwrap());
    }
}

fn tree_digest(root: &Path) -> BTreeMap<String, (u64, String)> {
    fn walk(root: &Path, current: &Path, out: &mut BTreeMap<String, (u64, String)>) {
        let mut entries = std::fs::read_dir(current)
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = std::fs::read(&path).unwrap();
                out.insert(relative, (metadata.len(), sha256_hex(&bytes)));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn mediated_execution_never_changes_git_metadata_or_unapproved_sentinels() {
    let fixture = Fixture::new(&["src"]);
    std::fs::write(fixture.workspace.join("src/item.txt"), b"before").unwrap();
    let git_before = tree_digest(&fixture.workspace.join(".git"));
    let sentinel_before = std::fs::read(fixture.workspace.join("ignored.sentinel")).unwrap();
    let executor = fixture.executor(ATTEMPT_ID);

    executor
        .execute(
            "create_file",
            &serde_json::json!({"path": "src/new.txt", "content": "new"}),
            &OperationKey::new("git-create"),
        )
        .unwrap();
    executor
        .execute(
            "apply_patch",
            &serde_json::json!({"path": "src/item.txt", "content": "after"}),
            &OperationKey::new("git-edit"),
        )
        .unwrap();
    executor
        .execute(
            "delete_file",
            &serde_json::json!({"path": "src/new.txt"}),
            &OperationKey::new("git-delete"),
        )
        .unwrap();

    assert_eq!(tree_digest(&fixture.workspace.join(".git")), git_before);
    assert_eq!(
        std::fs::read(fixture.workspace.join("ignored.sentinel")).unwrap(),
        sentinel_before
    );
    assert!(executor.release().unwrap());
}

#[derive(Default)]
struct FailOnceTools {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl ToolService for FailOnceTools {
    async fn list(&self, _token: GenerationToken) -> Result<Vec<ToolDescriptor>, ServiceError> {
        Ok(Vec::new())
    }

    async fn call(
        &self,
        _token: GenerationToken,
        _call: ToolCallRequest,
    ) -> Result<ToolCallReply, ServiceError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(ServiceError::Failure("transient tool interruption".into()))
        } else {
            Ok(ToolCallReply {
                output: vec![OutputBlock::Json {
                    value: serde_json::json!({"status": "reconciled"}),
                }],
                error: None,
            })
        }
    }
}

fn router_identity() -> RunIdentity {
    RunIdentity {
        task_id: "router-task".into(),
        branch_id: "router-branch".into(),
        run_id: "router-run".into(),
        attempt_id: "router-attempt".into(),
        generation: 1,
    }
}

fn rpc_request(method: &str, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: method.into(),
        params: Some(params),
    }
}

#[tokio::test]
async fn router_persists_rejected_then_retries_the_same_tool_key() {
    let tools = Arc::new(FailOnceTools::default());
    let processes = Arc::new(FakeProcessService::default());
    let store = Arc::new(MemoryJournal::new());
    let router = HostRouter::new(
        router_identity(),
        RunGuard::new("router-run", 1),
        vec![HostService::ToolsCall, HostService::ProcessOpen],
        tools.clone(),
        Arc::new(FakeModelService::default()),
        processes.clone(),
        store.clone(),
        Arc::new(IgnoreQuestions),
    );
    let secret = "CONTENT_MUST_NOT_APPEAR_IN_OBSERVATIONS";
    let params = serde_json::json!({
        "tool": "apply_patch",
        "input": {"path": "src/item.txt", "content": secret},
        "operation_key": "tool-reconcile"
    });

    let first_error = router
        .handle_request(rpc_request("host.tools.call", params.clone()))
        .await
        .expect_err("first tool call interrupted");
    assert!(!format!("{first_error:?}").contains(secret));
    let key = OperationKey::new("tool-reconcile");
    // A12：效果失败把 in-flight 的 Indeterminate 覆写为 Rejected 终态——
    // 键保持可重试，重试走同一执行臂而非 Reconcile 拒绝。
    let pending = store
        .load_receipt("router-attempt", &key)
        .await
        .expect("rejected receipt");
    assert!(matches!(pending.outcome, ReceiptOutcome::Rejected { .. }));
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);

    let reconciled = router
        .handle_request(rpc_request("host.tools.call", params))
        .await
        .expect("tool retry reconciles");
    assert_eq!(reconciled["output"][0]["value"]["status"], "reconciled");
    let completed = store
        .load_receipt("router-attempt", &key)
        .await
        .expect("completed receipt");
    assert!(matches!(
        completed.outcome,
        ReceiptOutcome::Completed { .. }
    ));
    assert_eq!(tools.calls.load(Ordering::SeqCst), 2);
    let observations =
        serde_json::to_string(&*router.host_observations.lock().expect("host observations"))
            .unwrap();
    assert!(!observations.contains(secret));
    assert!(observations.contains("<redacted>"));

    let process_key = OperationKey::new("process-pending");
    let process_params = serde_json::json!({
        "profile": "fixture",
        "arguments": [],
        "operation_key": process_key.0
    });
    store
        .save_receipt(OperationReceipt {
            attempt_id: "router-attempt".into(),
            operation_key: process_key,
            method: "host.process.open".into(),
            input_hash: canonical_input_hash(&process_params),
            outcome: ReceiptOutcome::Indeterminate {
                reason: "interrupted".into(),
            },
        })
        .await
        .unwrap();
    let process_error = router
        .handle_request(rpc_request("host.process.open", process_params))
        .await
        .expect_err("non-tool effects must not be retried blindly");
    assert_eq!(process_error.code, error_code::PROTOCOL_VIOLATION);
    assert_eq!(*processes.open_count.lock().unwrap(), 0);
    assert!(!format!("{process_error:?}").contains(secret));
}
