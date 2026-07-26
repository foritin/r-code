//! R7 测试 -- UI 完整面验证 [doc-09] [doc-11] [doc-18 M11-04/05]
//!
//! 验证项：
//! - R7-t1: 创建任务 E2E
//! - R7-t2: 审批流程 E2E
//! - R7-t3: 回滚 E2E
//! - R7-t4: 桌面验证 -- HTML 结构有效
//! - R7-t5: 可访问性 -- 缩放/键盘/aria-live
//!
//! 运行：`cargo test -p r-code-host --test r7_tests`

use r_code_core::dto::{FileChangeType, ProjectAccessMode, RiskLevel, TaskMode, TaskState};
use r_code_host::commands::{
    accept_task, agent_send, changes_list, permission_approve, permission_pending, recovery_data,
    rollback_file, task_create, task_detail, task_list, workspace_open, workspace_set_access_mode,
    CommandState,
};
use r_code_store::{AgentRunRepository, ChangeService, TaskRepository};
use std::path::Path;
use tempfile::TempDir;

// ============================================================================
// Test Helpers
// ============================================================================

/// 创建测试状态：内存数据库 + 临时目录。
fn setup_state() -> (TempDir, CommandState) {
    let dir = TempDir::new().unwrap();
    let state = CommandState::in_memory(dir.path()).unwrap();
    (dir, state)
}

async fn scoped_workspace(state: &CommandState) -> String {
    let workspace = workspace_open(state, &state.project_root).await.unwrap();
    workspace_set_access_mode(
        state,
        &workspace.canonical_path,
        ProjectAccessMode::RiskBased,
    )
    .await
    .unwrap();
    workspace.canonical_path
}

// ============================================================================
// R7-t1: E2E 创建任务 -- 选择项目 -> 输入目标 -> 提交 -> 任务创建成功
// ============================================================================

#[tokio::test]
async fn r7_t1_e2e_create_task() {
    let (_dir, state) = setup_state();

    // Step 1: 用户从真实目录中选择工作区
    let workspace_path = scoped_workspace(&state).await;

    // Step 2: 输入目标
    let title = "Fix login bug".to_string();
    let goal = "修复登录页面的认证 bug，确保用户可以使用正确的凭据登录".to_string();
    let mode = "edit".to_string();

    // Step 3: 提交 -> 任务创建
    let task = task_create(&state, Some(&workspace_path), &title, &goal, &mode)
        .await
        .expect("task creation should succeed");

    // 验证任务已创建
    assert!(!task.id.is_empty(), "task ID should not be empty");
    assert_eq!(
        task.workspace_path.as_deref(),
        Some(workspace_path.as_str())
    );
    assert_eq!(task.title, title);
    assert_eq!(task.goal, goal);
    assert_eq!(task.mode, TaskMode::Edit);
    assert_eq!(task.state, TaskState::Idle);

    // 验证任务出现在列表中
    let tasks = task_list(&state, None, false).await.unwrap();
    assert_eq!(tasks.len(), 1, "exactly one task should exist");
    assert_eq!(tasks[0].id, task.id);

    // 验证按项目过滤
    let filtered = task_list(&state, Some(&workspace_path), false)
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);

    let other_project = task_list(&state, Some("/other"), false).await.unwrap();
    assert!(other_project.is_empty());

    // 验证任务详情可获取
    let detail = task_detail(&state, &task.id).await.unwrap();
    assert_eq!(detail.task.id, task.id);
    assert!(!detail.events.is_empty(), "should have TaskCreated event");
}

// ============================================================================
// R7-t2: E2E 审批流程 -- 触发 R3 -> 审批 -> 执行
// ============================================================================

#[tokio::test]
async fn r7_t2_e2e_approval_flow() {
    let (_dir, state) = setup_state();

    // 创建任务
    let task = task_create(
        &state,
        None,
        "Risk task".into(),
        "Do something risky".into(),
        "edit".into(),
    )
    .await
    .unwrap();

    // 触发 R3 权限请求（模拟高风险工具调用）
    let request = state
        .permission_engine
        .request_permission(
            &task.id,
            "tc-001",
            "kill_process",
            RiskLevel::R3,
            "killing process PID 1234",
        )
        .await;

    assert_eq!(request.risk_level, RiskLevel::R3);
    assert_eq!(request.tool_name, "kill_process");

    // 验证待审批列表包含该请求
    let pending = permission_pending(&state, &task.id).await.unwrap();
    assert_eq!(pending.len(), 1, "should have 1 pending request");
    assert_eq!(pending[0].id, request.id);

    // 审批 -> allow（单次允许）
    permission_approve(&state, &request.id, "allow")
        .await
        .expect("approval should succeed");

    // 验证待审批列表已清空
    let pending_after = permission_pending(&state, &task.id).await.unwrap();
    assert!(
        pending_after.is_empty(),
        "pending list should be empty after approval"
    );

    // 测试 allow_always 审批
    let request2 = state
        .permission_engine
        .request_permission(
            &task.id,
            "tc-002",
            "write_file",
            RiskLevel::R2,
            "writing to src/main.rs",
        )
        .await;

    permission_approve(&state, &request2.id, "allow_always")
        .await
        .expect("allow_always should succeed for R2");

    // 测试 deny 审批
    let request3 = state
        .permission_engine
        .request_permission(
            &task.id,
            "tc-003",
            "delete_file",
            RiskLevel::R3,
            "deleting important file",
        )
        .await;

    permission_approve(&state, &request3.id, "deny")
        .await
        .expect("deny should succeed");
}

// ============================================================================
// R7-t3: E2E 回滚 -- 修改文件 -> 回滚 -> 文件恢复
// ============================================================================

#[tokio::test]
async fn r7_t3_e2e_rollback() {
    let (dir, state) = setup_state();

    // 创建任务
    let workspace_path = scoped_workspace(&state).await;
    let task = task_create(
        &state,
        Some(&workspace_path),
        "Rollback test".into(),
        "Modify and rollback".into(),
        "edit".into(),
    )
    .await
    .unwrap();

    // 创建文件并捕获基线
    let file_path = dir.path().join("config.json");
    let original_content = r#"{"version": "1.0", "name": "myapp"}"#;
    std::fs::write(&file_path, original_content).unwrap();

    let cs = ChangeService::new(&state.db, state.blobs_dir.clone());
    cs.capture_baseline(&task.id, &file_path).await.unwrap();

    // 修改文件
    let modified_content = r#"{"version": "2.0", "name": "myapp", "debug": true}"#;
    std::fs::write(&file_path, modified_content).unwrap();

    // 记录变更
    cs.record_change(
        &task.id,
        &file_path,
        FileChangeType::Modify,
        None,
        Some(original_content.as_bytes()),
        Some(modified_content.as_bytes()),
        None,
    )
    .await
    .unwrap();

    // 验证变更已记录
    let changes = changes_list(&state, &task.id).await.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].change_type, FileChangeType::Modify);

    // 验证文件已修改
    assert_eq!(
        std::fs::read_to_string(&file_path).unwrap(),
        modified_content
    );

    // 回滚
    let rel_path = "config.json";
    let result = rollback_file(&state, &task.id, rel_path).await.unwrap();

    // 验证回滚成功
    assert!(
        result.contains("Restored"),
        "rollback should restore file, got: {result}"
    );

    // 验证文件已恢复到原始内容
    assert_eq!(
        std::fs::read_to_string(&file_path).unwrap(),
        original_content,
        "file should be restored to original content"
    );
}

// ============================================================================
// R7-t3b: E2E 回滚全部变更
// ============================================================================

#[tokio::test]
async fn r7_t3b_e2e_rollback_all() {
    let (dir, state) = setup_state();

    let workspace_path = scoped_workspace(&state).await;
    let task = task_create(
        &state,
        Some(&workspace_path),
        "Multi rollback".into(),
        "Modify multiple files".into(),
        "edit".into(),
    )
    .await
    .unwrap();

    let cs = ChangeService::new(&state.db, state.blobs_dir.clone());

    // 创建并修改两个文件
    for i in 0..2 {
        let path = dir.path().join(format!("file{i}.txt"));
        let original = format!("original {i}");
        std::fs::write(&path, &original).unwrap();
        cs.capture_baseline(&task.id, &path).await.unwrap();

        let modified = format!("modified {i}");
        std::fs::write(&path, &modified).unwrap();
        cs.record_change(
            &task.id,
            &path,
            FileChangeType::Modify,
            None,
            Some(original.as_bytes()),
            Some(modified.as_bytes()),
            None,
        )
        .await
        .unwrap();
    }

    // 回滚所有
    let results = r_code_host::commands::rollback_task(&state, &task.id)
        .await
        .unwrap();

    assert_eq!(results.len(), 2, "should rollback 2 files");

    // 验证两个文件都恢复了
    for i in 0..2 {
        let path = dir.path().join(format!("file{i}.txt"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, format!("original {i}"));
    }
}

// ============================================================================
// R7-t4: 桌面验证 -- HTML 结构有效 + 窄视口支持
// ============================================================================

#[test]
fn r7_t4_desktop_verification_html_structure() {
    let html_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("index.html");
    let html = std::fs::read_to_string(&html_path).expect("frontend/index.html should exist");

    // 验证基本 HTML 结构（新前端：React + Vite 挂载点）
    assert!(html.contains("<!DOCTYPE html>"), "should have DOCTYPE");
    assert!(html.contains("lang=\"zh-CN\""), "should set Chinese locale");
    assert!(
        html.contains("<meta name=\"viewport\""),
        "should have viewport meta for responsive"
    );
    assert!(
        html.contains("name=\"color-scheme\" content=\"dark\""),
        "should declare dark color scheme"
    );
    assert!(
        html.contains("id=\"root\""),
        "should have React mount point"
    );

    // 验证场景装配（App.tsx 挂载全部场景）
    let app =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/App.tsx"))
            .unwrap();
    for scene in [
        "HomeScene",
        "DeckScene",
        "RoomScene",
        "InboxScene",
        "ProjectsScene",
        "EditorScene",
        "SettingsScene",
        "SearchOverlay",
    ] {
        assert!(app.contains(scene), "App should mount {scene}");
    }

    // 验证场景文件存在
    for scene in [
        "HomeScene",
        "DeckScene",
        "RoomScene",
        "InboxScene",
        "ProjectsScene",
        "EditorScene",
        "SettingsScene",
    ] {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("frontend/src/components/scenes")
            .join(format!("{scene}.tsx"));
        assert!(p.exists(), "scene file should exist: {scene}");
    }
}

#[test]
fn r7_t4_desktop_verification_motion_safety() {
    // 动画预算红线：base.css 必须有 prefers-reduced-motion 全停规则
    let base = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/styles/base.css"),
    )
    .unwrap();
    assert!(
        base.contains("prefers-reduced-motion"),
        "should have reduced-motion guard"
    );
    assert!(
        base.contains("animation: none !important"),
        "reduced-motion should stop all animations"
    );

    // shell 栅格：紧凑标题栏 / 单一会话侧栏 / 主工作区
    let shell = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/styles/shell.css"),
    )
    .unwrap();
    assert!(shell.contains("grid-template-rows: 34px"));
    assert!(shell.contains("grid-template-columns: 252px"));
}

#[test]
fn r7_t4_desktop_verification_tokens() {
    // 设计 token 体系：obsidian（暗，默认）+ studio-light（亮）双主题列
    let tokens = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/styles/tokens.css"),
    )
    .unwrap();
    assert!(tokens.contains("[data-theme='obsidian']"), "obsidian theme");
    assert!(
        tokens.contains("[data-theme='studio-light']"),
        "studio-light theme"
    );
    for tok in [
        "--bg-app",
        "--fg-muted",
        "--accent",
        "--fx-glow-accent",
        "--radius-card",
    ] {
        assert!(tokens.contains(tok), "token should exist: {tok}");
    }

    // 共享原语
    let base = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/styles/base.css"),
    )
    .unwrap();
    for cls in [
        ".pane",
        ".btn",
        ".chip",
        ".lamp",
        ".gseg",
        ".diffstat",
        ".zone-head",
    ] {
        assert!(base.contains(cls), "primitive should exist: {cls}");
    }
}

#[test]
fn r7_t4_desktop_verification_app_wiring() {
    // 入口与状态层
    let main = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/main.tsx"),
    )
    .unwrap();
    assert!(main.contains("createRoot"), "should mount React root");
    assert!(main.contains("tokens.css"), "should load token css");

    // IPC 层覆盖后端命令（抽查关键命令）
    let ipc = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/lib/ipc.ts"),
    )
    .unwrap();
    for cmd in [
        "cmd_task_create",
        "cmd_agent_send",
        "cmd_agent_abort",
        "cmd_permission_approve",
        "cmd_changes_list",
        "cmd_change_diff",
        "cmd_run_verification",
        "cmd_terminal_create",
        "cmd_replay",
        "cmd_session_messages",
        "cmd_memory_get",
        "cmd_recovery_cleanup",
        "cmd_support_preview",
        "cmd_logs_tail",
        "cmd_settings_get",
        "cmd_settings_save_provider",
        "cmd_codex_integration_status",
    ] {
        assert!(ipc.contains(cmd), "ipc wrapper should exist: {cmd}");
    }
}

// ============================================================================
// R7-t5: 可访问性 -- 缩放 80-200%、键盘流、aria-live
// ============================================================================

#[test]
fn r7_t5_accessibility_zoom_support() {
    // 缩放由 store/app.ts 实现（zoomLevel 80-200 + setZoom/zoomIn/zoomOut/zoomReset）
    let store = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/store/app.ts"),
    )
    .unwrap();
    for api in ["setZoom", "zoomIn", "zoomOut", "zoomReset"] {
        assert!(store.contains(api), "zoom API should exist: {api}");
    }
    assert!(store.contains("Math.max(80"), "zoom lower bound 80");
    assert!(store.contains("Math.min(200"), "zoom upper bound 200");

    // 设置页有缩放滑块与复位
    let settings = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("frontend/src/components/scenes/SettingsScene.tsx"),
    )
    .unwrap();
    assert!(
        settings.contains("setZoom"),
        "settings should expose zoom slider"
    );
    assert!(
        settings.contains("zoomReset"),
        "settings should expose zoom reset"
    );
}

#[test]
fn r7_t5_accessibility_aria_roles() {
    // 新前端用 role 语义：role=main / alert / status（错误条与恢复横幅）
    let read = |rel: &str| {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
    };
    let app = read("frontend/src/App.tsx");
    assert!(app.contains("role=\"main\""), "main region role");

    let home = read("frontend/src/components/scenes/HomeScene.tsx");
    assert!(
        home.contains("role=\"status\""),
        "status region for recovery banner"
    );

    let deck = read("frontend/src/components/scenes/DeckScene.tsx");
    assert!(deck.contains("role=\"alert\""), "alert role for error bar");
}

#[test]
fn r7_t5_accessibility_keyboard_navigation() {
    // 全局快捷键（Ctrl K / Ctrl E，Mac 同时兼容 Command）+ 输入控件过滤
    let keys = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/lib/keys.ts"),
    )
    .unwrap();
    assert!(
        keys.contains("metaKey") && keys.contains("ctrlKey"),
        "should support Cmd/Ctrl shortcuts"
    );
    assert!(
        keys.contains("isTypingTarget"),
        "should ignore typing targets"
    );

    // 场景级键盘导航（Deck rows j/k/x/esc）
    let rows = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/components/deck/FleetRows.tsx"),
    )
    .unwrap();
    assert!(rows.contains("keydown"), "deck rows should bind keydown");
}

#[test]
fn r7_t5_accessibility_diff_text_mode() {
    // accessible diff mode：store 标志 + 设置页开关（F7 导航为后续增强）
    let store = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/store/app.ts"),
    )
    .unwrap();
    assert!(store.contains("accessibleDiffMode"));
    assert!(store.contains("toggleDiffMode"));

    let settings = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("frontend/src/components/scenes/SettingsScene.tsx"),
    )
    .unwrap();
    assert!(settings.contains("accessibleDiffMode"), "settings toggle");
    assert!(settings.contains("F7"), "F7 hint text");
}

#[test]
fn r7_t5_replay_three_depths_and_evidence_levels() {
    // 回放三层深度 + 证据分级（types.ts 与后端 serde 形状对齐）
    let types = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/lib/types.ts"),
    )
    .unwrap();
    for depth in ["recap", "explore", "verify"] {
        assert!(types.contains(depth), "replay depth: {depth}");
    }
    for level in ["verified", "recorded", "observed", "inferred", "missing"] {
        assert!(types.contains(level), "evidence level: {level}");
    }

    let ipc = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/lib/ipc.ts"),
    )
    .unwrap();
    assert!(ipc.contains("cmd_replay"), "replay ipc wrapper");

    // 回放能力保留在 IPC 与时间线数据模型中，不再强制挂载底部胶片控件。
    let model = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/components/room/model.ts"),
    )
    .unwrap();
    assert!(
        model.contains("buildTimeline"),
        "timeline model should exist"
    );
}

// ============================================================================
// R7-t5: 上下文投喂验证 [doc-04 §7]
// ============================================================================

#[test]
fn r7_t5_context_injection_features() {
    // 后端：文件引用 / 选区引用 / 外部会话注入（bracketed paste，无回车）
    let commands =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands.rs"))
            .unwrap();
    assert!(commands.contains("create_file_ref"), "file ref creation");
    assert!(
        commands.contains("create_selection_ref"),
        "selection ref creation"
    );
    assert!(
        commands.contains("create_external_session_injection"),
        "external session injection"
    );
    assert!(commands.contains("\\x1b[200~"), "bracketed paste start");
    assert!(commands.contains("\\x1b[201~"), "bracketed paste end");
    assert!(
        commands.contains("刻意不追加"),
        "should explicitly note no trailing Enter"
    );

    // 前端：Composer 的 @ 文件 attach（quickOpen 下拉）
    let composer = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/components/room/Composer.tsx"),
    )
    .unwrap();
    assert!(
        composer.contains("quickOpen"),
        "composer @ attach via quickOpen"
    );
}

// ============================================================================
// 附加测试: Agent 命令
// ============================================================================

#[tokio::test]
async fn r7_agent_send_and_abort() {
    let (_dir, state) = setup_state();

    let task = task_create(
        &state,
        None,
        "Agent test".into(),
        "Test agent".into(),
        "ask".into(),
    )
    .await
    .unwrap();

    // 发送消息
    agent_send(&state, &task.id, "Hello agent").await.unwrap();

    // 验证会话文件已创建
    let session_path = state.sessions_dir.join(format!("{}.jsonl", task.id));
    assert!(session_path.exists(), "session file should exist");

    // 中止
    TaskRepository::new(&state.db)
        .update_state(&task.id, TaskState::InProgress)
        .unwrap();

    r_code_host::commands::agent_abort(&state, &task.id)
        .await
        .unwrap();

    let detail = task_detail(&state, &task.id).await.unwrap();
    assert_eq!(detail.task.state, TaskState::Interrupted);
}

// ============================================================================
// 附加测试: 恢复数据
// ============================================================================

#[tokio::test]
async fn r7_recovery_data() {
    let (_dir, state) = setup_state();

    // 空数据库 -> 无中断任务
    let data = recovery_data(&state).await.unwrap();
    assert!(data.interrupted_tasks.is_empty());
    assert_eq!(data.orphaned_permissions, 0);
}

// ============================================================================
// 附加测试: 接受任务
// ============================================================================

#[tokio::test]
async fn r7_accept_task() {
    let (_dir, state) = setup_state();

    let task = task_create(
        &state,
        None,
        "Accept test".into(),
        "Test accept".into(),
        "edit".into(),
    )
    .await
    .unwrap();

    // 创建活跃 Agent Run（接受操作需要活跃 run）
    let run = r_code_core::dto::AgentRun::new(&task.id, "test-model");
    AgentRunRepository::new(&state.db).create(&run).unwrap();

    // 设置为 ReviewReady
    TaskRepository::new(&state.db)
        .update_state(&task.id, TaskState::ReviewReady)
        .unwrap();

    // 接受任务
    accept_task(&state, &task.id).await.unwrap();

    // 验证状态回到 Idle
    let detail = task_detail(&state, &task.id).await.unwrap();
    assert_eq!(detail.task.state, TaskState::Idle);
}

// ============================================================================
// 附加测试: Replay 服务
// ============================================================================

#[tokio::test]
async fn r7_replay_service_three_depths() {
    use hermes_core::{Message, SessionEvent, SessionMeta};
    use r_code_host::replay::{EvidenceLevel, ReplayDepth, ReplayService};

    let dir = TempDir::new().unwrap();
    let sessions_dir = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let svc = ReplayService::new(sessions_dir.clone());

    let session_id = "replay-test-001";
    let path = sessions_dir.join(format!("{session_id}.jsonl"));

    // 写入测试事件
    let meta = SessionMeta {
        id: session_id.to_string(),
        created_at: chrono::Utc::now(),
        model: "test-model".to_string(),
        provider: "test".to_string(),
        title: Some("Replay Test".to_string()),
    };
    let mut content = serde_json::to_string(&SessionEvent::Meta(meta)).unwrap();
    content.push('\n');
    content.push_str(
        &serde_json::to_string(&SessionEvent::Message(Message::user_text("Hello"))).unwrap(),
    );
    content.push('\n');
    content.push_str(
        &serde_json::to_string(&SessionEvent::ToolCall {
            name: "read_file".to_string(),
            input: serde_json::json!({"path": "/test"}),
        })
        .unwrap(),
    );
    content.push('\n');
    content.push_str(
        &serde_json::to_string(&SessionEvent::ToolResult {
            call_id: "c1".to_string(),
            output: serde_json::json!({"content": "data"}),
            is_error: false,
        })
        .unwrap(),
    );
    content.push('\n');
    std::fs::write(&path, content).unwrap();

    // Recap 深度
    let recap = svc
        .get_replay(session_id, ReplayDepth::Recap)
        .await
        .unwrap();
    assert!(
        recap.iter().any(|e| e.event_type == "recap_summary"),
        "recap should include summary"
    );

    // Explore 深度
    let explore = svc
        .get_replay(session_id, ReplayDepth::Explore)
        .await
        .unwrap();
    assert!(
        explore.iter().any(|e| e.event_type == "tool_call"),
        "explore should include tool calls"
    );
    assert!(
        explore.iter().any(|e| e.event_type == "tool_result"),
        "explore should include tool results"
    );

    // Verify 深度
    let verify = svc
        .get_replay(session_id, ReplayDepth::Verify)
        .await
        .unwrap();
    let tool_result = verify
        .iter()
        .find(|e| e.event_type == "tool_result")
        .unwrap();
    assert!(tool_result.details.is_some(), "verify should have details");
    assert_eq!(
        tool_result.evidence_level,
        EvidenceLevel::Verified,
        "tool result should be verified"
    );

    // 获取证据
    let evidence = svc.get_evidence(session_id, 0).await.unwrap();
    assert_eq!(evidence["available"], true);
}

#[test]
fn r7_replay_never_shows_thinking() {
    use hermes_core::{ContentBlock, Message, Role, SessionEvent, SessionMeta};
    use r_code_host::replay::{ReplayDepth, ReplayService};

    let dir = TempDir::new().unwrap();
    let sessions_dir = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let svc = ReplayService::new(sessions_dir.clone());
    let session_id = "thinking-test";

    let meta = SessionMeta {
        id: session_id.to_string(),
        created_at: chrono::Utc::now(),
        model: "m".to_string(),
        provider: "p".to_string(),
        title: None,
    };

    let msg = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "secret chain of thought".to_string(),
                signature: None,
            },
            ContentBlock::Text {
                text: "Visible answer".to_string(),
            },
        ],
    };

    let mut content = serde_json::to_string(&SessionEvent::Meta(meta)).unwrap();
    content.push('\n');
    content.push_str(&serde_json::to_string(&SessionEvent::Message(msg)).unwrap());
    content.push('\n');
    std::fs::write(sessions_dir.join(format!("{session_id}.jsonl")), content).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let entries = rt
        .block_on(svc.get_replay(session_id, ReplayDepth::Explore))
        .unwrap();

    let msg_entry = entries.iter().find(|e| e.event_type == "message").unwrap();
    // Thinking 内容绝不能出现在摘要中
    assert!(
        !msg_entry.summary.contains("secret chain of thought"),
        "thinking content must never be shown in replay"
    );
    assert!(
        msg_entry.summary.contains("Visible answer"),
        "visible text should be shown"
    );
    assert!(
        msg_entry.summary.contains("[reasoning hidden]"),
        "should indicate reasoning was hidden"
    );
}
