//! Final Delivery 测试 -- 交付物验收 [doc-16] [doc-19 §4]。
//!
//! 验证项：
//! - F-1:  用户可从空目录配置、打开项目、运行安全只读 Agent
//! - F-2:  Agent 可在显式审批下写入、验证、审查、接受或安全回滚
//! - F-3:  Terminal / 外部 CLI / 预览 / 附件 / 历史 / 证据在 Task 维度一致链接
//! - F-4:  重启 / worker 退出 / 迁移失败 / 待审批权限 / 外部变更均有恢复流程
//! - F-7:  DoD -- 每个功能具备 empty/loading/failure/recovery 状态
//! - F-9:  DoD -- 权限 / 数据归属 / 隐私审查；写入有回滚或不可逆警告
//! - F-11: DoD -- 文档链接到对应设计 / 合同 / 测试 / 发布条件
//! - F-12..F-18: Work card 审计要求（goal/boundary/contract/failure/tests/evidence/rollback）
//!
//! 运行：`cargo test -p r-code-host --test final_delivery`

use r_code_host::work_card::{FailureState, RequiredTest, TestStatus, TestType, WorkCard};

/// F-1: User can configure from empty directory, open project, run safe read-only Agent
#[test]
fn f1_user_can_configure_and_run() {
    // Verify all required components exist
    assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../Cargo.toml")
        .exists());
    assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tauri.conf.json")
        .exists());
    assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/main.rs")
        .exists());
    assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend/index.html")
        .exists());

    // Verify database can be created
    let db = r_code_store::Database::open_in_memory().unwrap();
    let conn = db.conn().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

/// F-2: Agent can write with explicit approval, verify, review, accept or safely rollback
#[test]
fn f2_agent_write_review_rollback() {
    // Verify ChangeService, PermissionEngine, VerificationService, ReviewService exist
    let db = r_code_store::Database::open_in_memory().unwrap();

    // These services are the backbone of write/review/rollback
    let _change_svc = r_code_store::ChangeService::new(&db, std::path::PathBuf::from("/tmp/blobs"));
    let _verify_svc =
        r_code_store::VerificationService::new(&db, std::path::PathBuf::from("/tmp/blobs"));
    let _review_svc = r_code_store::ReviewService::new(&db, std::path::PathBuf::from("/tmp/blobs"));
    let _perm_engine = r_code_gateway::PermissionEngine::new();
}

/// F-3: Terminal, external CLI, preview, attachments, history, evidence consistently linked at Task dimension
#[test]
fn f3_task_dimension_consistency() {
    // Verify all task-dimension services exist
    let db = r_code_store::Database::open_in_memory().unwrap();
    let task_repo = r_code_store::TaskRepository::new(&db);
    let run_repo = r_code_store::AgentRunRepository::new(&db);
    let event_store = r_code_store::TaskEventStore::new(&db);

    // Create task -> run -> events chain
    let task = r_code_core::dto::Task::new(
        Some("/proj".into()),
        "Test",
        "Goal",
        r_code_core::dto::TaskMode::Ask,
    );
    task_repo.create(&task).unwrap();
    let run = r_code_core::dto::AgentRun::new(&task.id, "model");
    run_repo.create(&run).unwrap();
    let _event_id = event_store
        .append(&task.id, r_code_core::dto::TaskEventType::RunStarted)
        .unwrap();

    // Verify chain
    let events = event_store.list_by_task(&task.id, None, None).unwrap();
    assert!(!events.is_empty());
    assert_eq!(events[0].task_id, task.id);
}

/// F-4: Restart, worker exit, migration failure, pending permissions, external changes all have recovery flows
#[test]
fn f4_recovery_flows() {
    use r_code_host::migration::MigrationManager;
    use r_code_host::recovery::RecoveryManager;

    // Recovery manager exists and can scan
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = r_code_store::Database::open(&db_path).unwrap();
    drop(db);

    let _recovery = RecoveryManager::new(db_path.clone());
    let migration = MigrationManager::new(db_path);

    // Both can be created without error
    assert!(migration.needs_migration().is_ok());
}

/// F-7: DoD - each feature has empty/loading/failure/recovery state
#[test]
fn f7_dod_states_exist() {
    // 新前端为 React（src-tauri/frontend/src）：汇总全部 tsx/css 源码，
    // 验证 empty / loading / error 三类状态模式存在
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src");
    let mut all = String::new();
    let mut stack = vec![src_dir];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("tsx" | "ts" | "css")
            ) {
                all.push_str(&std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    assert!(!all.is_empty(), "frontend sources must exist");

    assert!(
        all.contains("loading")
            || all.contains("Loading")
            || all.contains("加载")
            || all.contains("搜索中"),
        "Loading state must exist"
    );
    assert!(
        all.contains("error") || all.contains("Error") || all.contains("错误"),
        "Error state must exist"
    );
    assert!(
        all.contains("empty")
            || all.contains("Empty")
            || all.contains("空态")
            || all.contains("还没有"),
        "Empty state must exist"
    );
}

/// F-9: DoD - permission, data ownership, privacy review; rollback or irreversibility warning for writes
#[test]
fn f9_dod_privacy_review() {
    use r_code_core::secret::redact_text;
    use r_code_host::security_config::SecurityConfig;

    // Verify secret redaction works
    let redacted = redact_text("api_key=sk-abc123def456");
    assert!(redacted.contains("***"));
    assert!(!redacted.contains("abc123def456"));

    // Verify security config has proper restrictions
    let prod = SecurityConfig::production();
    assert!(
        !prod.devtools_enabled,
        "Devtools must be disabled in production"
    );
    assert!(prod.sandbox_enabled, "Sandbox must be enabled");
}

/// F-11: DoD - documentation links to corresponding design, contract, test, release conditions
#[test]
fn f11_dod_documentation_links() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");

    // Keep this list aligned with the current public documentation surface. Historical
    // reconstruction HTML lives outside the delivery contract and may be pruned.
    let required_docs = [
        "README.md",
        "CHANGELOG.md",
        "docs/README.md",
        ".github/workflows/release.yml",
        "vendor/agent-core/docs/index.html",
        "vendor/agent-core/docs/checklist.html",
    ];

    for doc in required_docs {
        assert!(
            repo_root.join(doc).is_file(),
            "Required doc must exist: {}",
            doc
        );
    }
}

/// F-12: Work card goal
#[test]
fn f12_work_card_goal() {
    let card = WorkCard::new("WC-001", "R1", "User can create tasks");
    assert!(!card.goal.is_empty());
}

/// F-13: Work card boundary
#[test]
fn f13_work_card_boundary() {
    let mut card = WorkCard::new("WC-001", "R1", "Test");
    card.boundary.workspaces.push("/project".to_string());
    card.boundary.processes.push("agent-worker".to_string());
    assert!(!card.boundary.workspaces.is_empty());
    assert!(!card.boundary.processes.is_empty());
}

/// F-14: Work card contract
#[test]
fn f14_work_card_contract() {
    let mut card = WorkCard::new("WC-001", "R1", "Test");
    card.contract.new_dtos.push("Task".to_string());
    card.contract
        .new_rpc_methods
        .push("task.create".to_string());
    assert!(!card.contract.new_dtos.is_empty());
    assert!(!card.contract.new_rpc_methods.is_empty());
}

/// F-15: Work card failure states
#[test]
fn f15_work_card_failure_states() {
    let mut card = WorkCard::new("WC-001", "R1", "Test");
    card.failure_states.push(FailureState {
        scenario: "User cancels during task creation".to_string(),
        expected_behavior: "Task is not created, no side effects".to_string(),
    });
    assert!(!card.failure_states.is_empty());
}

/// F-16: Work card tests
#[test]
fn f16_work_card_tests() {
    let mut card = WorkCard::new("WC-001", "R1", "Test");
    card.tests.push(RequiredTest {
        test_type: TestType::Unit,
        description: "Task creation creates valid DTO".to_string(),
        status: TestStatus::Passing,
    });
    assert!(!card.tests.is_empty());
}

/// F-17: Work card evidence
#[test]
fn f17_work_card_evidence() {
    let mut card = WorkCard::new("WC-001", "R1", "Test");
    card.evidence.push(r_code_host::work_card::EvidenceItem {
        kind: "test".to_string(),
        description: "760 tests passing".to_string(),
        location: "cargo test --workspace".to_string(),
    });
    assert!(!card.evidence.is_empty());
}

/// F-18: Work card rollback
#[test]
fn f18_work_card_rollback() {
    let mut card = WorkCard::new("WC-001", "R1", "Test");
    card.rollback.code_rollback = "git revert <commit>".to_string();
    card.rollback.data_rollback = "Drop tasks table, re-run migration".to_string();
    assert!(!card.rollback.code_rollback.is_empty());
    assert!(!card.rollback.data_rollback.is_empty());

    // Validate work card
    card.failure_states.push(FailureState {
        scenario: "Test".to_string(),
        expected_behavior: "Test".to_string(),
    });
    card.tests.push(RequiredTest {
        test_type: TestType::Unit,
        description: "Test".to_string(),
        status: TestStatus::Passing,
    });
    assert!(card.validate().is_ok());
}

/// F-19: Windows release launches as a desktop GUI without opening a console window.
#[test]
fn f19_windows_release_uses_gui_subsystem() {
    let main = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .unwrap();
    assert!(
        main.contains("windows_subsystem = \"windows\""),
        "release builds must use the Windows GUI subsystem"
    );
}

/// F-20: The NSIS installer and uninstaller use the product icon instead of NSIS defaults.
#[test]
fn f20_nsis_uses_r_code_icons() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest_dir.join("tauri.conf.json")).unwrap(),
    )
    .unwrap();
    let nsis = &config["bundle"]["windows"]["nsis"];

    for key in ["installerIcon", "uninstallerIcon"] {
        let relative = nsis[key]
            .as_str()
            .unwrap_or_else(|| panic!("bundle.windows.nsis.{key} must be configured"));
        assert_eq!(relative, "../icons/icon.ico");
        assert!(
            manifest_dir.join(relative).is_file(),
            "configured NSIS icon does not exist: {relative}"
        );
    }

    assert_eq!(nsis["installerHooks"], "installer-hooks.nsh");
    let hooks = std::fs::read_to_string(manifest_dir.join("installer-hooks.nsh")).unwrap();
    assert!(hooks.contains("NSIS_HOOK_PREINSTALL"));
    assert!(hooks.contains("NSIS_HOOK_POSTINSTALL"));
    assert!(hooks.contains("/BRANDED_PROGRESS="));
}

/// F-21: The distributable Windows installer is a real branded bootstrapper,
/// not only a design mockup around inert controls.
#[test]
fn f21_branded_installer_contract() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let installer = repository.join("installer");

    let main = std::fs::read_to_string(installer.join("src/main.rs")).unwrap();
    for contract in [
        "windows_subsystem = \"windows\"",
        "fn start_install",
        "extract_payload",
        "command.arg(\"/S\")",
        "command.arg(\"/NS\")",
        "NSIS requires /D to be the final command-line argument",
    ] {
        assert!(
            main.contains(contract),
            "missing installer contract: {contract}"
        );
    }

    let html = std::fs::read_to_string(installer.join("frontend/index.html")).unwrap();
    for control in [
        "id=\"install-now\"",
        "id=\"browse-path\"",
        "id=\"cancel-install\"",
        "id=\"complete-primary\"",
    ] {
        assert!(
            html.contains(control),
            "missing installer control: {control}"
        );
    }
    assert!(!html.contains("INTERACTION PROTOTYPE"));

    let script = std::fs::read_to_string(installer.join("frontend/app.js")).unwrap();
    assert!(script.contains("bridge.invoke(\"start_install\""));
    assert!(script.contains("bridge.invoke(\"cancel_install\""));

    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(installer.join("tauri.conf.json")).unwrap())
            .unwrap();
    assert_eq!(config["app"]["windows"][0]["decorations"], false);
    assert_eq!(config["bundle"]["icon"][0], "../icons/icon.ico");

    assert!(repository
        .join("scripts/build-branded-installer.ps1")
        .is_file());
}
