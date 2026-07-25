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

use r_code_core::dto::{FileChangeType, RiskLevel, TaskMode, TaskState};
use r_code_host::commands::{
    cmd_accept_task, cmd_agent_send, cmd_changes_list, cmd_permission_approve,
    cmd_permission_pending, cmd_recovery_data, cmd_rollback_file, cmd_task_create, cmd_task_detail,
    cmd_task_list, CommandState,
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

// ============================================================================
// R7-t1: E2E 创建任务 -- 选择项目 -> 输入目标 -> 提交 -> 任务创建成功
// ============================================================================

#[tokio::test]
async fn r7_t1_e2e_create_task() {
    let (_dir, state) = setup_state();

    // Step 1: 选择项目
    let project_id = "/home/user/myproject";

    // Step 2: 输入目标
    let title = "Fix login bug".to_string();
    let goal = "修复登录页面的认证 bug，确保用户可以使用正确的凭据登录".to_string();
    let mode = "edit".to_string();

    // Step 3: 提交 -> 任务创建
    let task = cmd_task_create(
        &state,
        project_id.to_string(),
        title.clone(),
        goal.clone(),
        mode.clone(),
    )
    .await
    .expect("task creation should succeed");

    // 验证任务已创建
    assert!(!task.id.is_empty(), "task ID should not be empty");
    assert_eq!(task.project_id, project_id);
    assert_eq!(task.title, title);
    assert_eq!(task.goal, goal);
    assert_eq!(task.mode, TaskMode::Edit);
    assert_eq!(task.state, TaskState::Idle);

    // 验证任务出现在列表中
    let tasks = cmd_task_list(&state, None, false).await.unwrap();
    assert_eq!(tasks.len(), 1, "exactly one task should exist");
    assert_eq!(tasks[0].id, task.id);

    // 验证按项目过滤
    let filtered = cmd_task_list(&state, Some(project_id.to_string()), false)
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);

    let other_project = cmd_task_list(&state, Some("/other".to_string()), false)
        .await
        .unwrap();
    assert!(other_project.is_empty());

    // 验证任务详情可获取
    let detail = cmd_task_detail(&state, task.id.clone()).await.unwrap();
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
    let task = cmd_task_create(
        &state,
        "/proj".into(),
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
    let pending = cmd_permission_pending(&state, task.id.clone())
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "should have 1 pending request");
    assert_eq!(pending[0].id, request.id);

    // 审批 -> allow（单次允许）
    cmd_permission_approve(&state, request.id.clone(), "allow".into())
        .await
        .expect("approval should succeed");

    // 验证待审批列表已清空
    let pending_after = cmd_permission_pending(&state, task.id.clone())
        .await
        .unwrap();
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

    cmd_permission_approve(&state, request2.id.clone(), "allow_always".into())
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

    cmd_permission_approve(&state, request3.id.clone(), "deny".into())
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
    let task = cmd_task_create(
        &state,
        dir.path().to_str().unwrap().to_string(),
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
    let changes = cmd_changes_list(&state, task.id.clone()).await.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].change_type, FileChangeType::Modify);

    // 验证文件已修改
    assert_eq!(
        std::fs::read_to_string(&file_path).unwrap(),
        modified_content
    );

    // 回滚
    let rel_path = "config.json";
    let result = cmd_rollback_file(&state, task.id.clone(), rel_path.into())
        .await
        .unwrap();

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

    let task = cmd_task_create(
        &state,
        dir.path().to_str().unwrap().to_string(),
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
    let results = r_code_host::commands::cmd_rollback_task(&state, task.id.clone())
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

    // 验证基本 HTML 结构
    assert!(html.contains("<!DOCTYPE html>"), "should have DOCTYPE");
    assert!(html.contains("lang=\"zh-CN\""), "should set Chinese locale");
    assert!(
        html.contains("<meta name=\"viewport\""),
        "should have viewport meta for responsive"
    );
    assert!(
        html.contains("<meta name=\"color-scheme\" content=\"dark\">"),
        "should declare dark color scheme"
    );

    // 验证四个视图存在
    assert!(html.contains("id=\"view-home\""), "should have Home view");
    assert!(
        html.contains("id=\"view-room\""),
        "should have Task Room view"
    );
    assert!(
        html.contains("id=\"view-editor\""),
        "should have Editor view"
    );
    assert!(
        html.contains("id=\"view-settings\""),
        "should have Settings view"
    );

    // 验证导航栏
    assert!(
        html.contains("class=\"sidebar\""),
        "should have sidebar nav"
    );
    assert!(
        html.contains("data-view=\"home\""),
        "should have home nav item"
    );
    assert!(
        html.contains("data-view=\"room\""),
        "should have room nav item"
    );
    assert!(
        html.contains("data-view=\"editor\""),
        "should have editor nav item"
    );
    assert!(
        html.contains("data-view=\"settings\""),
        "should have settings nav item"
    );

    // 验证无 console error 风险 -- 检查无未闭合的 script 标签
    let script_open = html.matches("<script").count();
    let script_close = html.matches("</script>").count();
    assert_eq!(script_open, script_close, "script tags should be balanced");

    // 验证引用了 CSS 和 JS
    assert!(
        html.contains("href=\"styles.css\""),
        "should link styles.css"
    );
    assert!(html.contains("src=\"app.js\""), "should load app.js");
}

#[test]
fn r7_t4_desktop_verification_narrow_viewport() {
    let css_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("styles.css");
    let css = std::fs::read_to_string(&css_path).expect("frontend/styles.css should exist");

    // 验证有窄视口响应式支持
    assert!(
        css.contains("@media (max-width:"),
        "should have responsive media queries for narrow viewports"
    );
    assert!(
        css.contains("max-width: 900px"),
        "should support narrow viewport (<=900px)"
    );
}

#[test]
fn r7_t4_desktop_verification_css_loaded() {
    let css_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("styles.css");
    let css = std::fs::read_to_string(&css_path).unwrap();

    // 验证 CSS 非空且包含核心样式
    assert!(!css.is_empty(), "CSS should not be empty");
    assert!(
        css.contains("--bg-primary"),
        "should have dark theme variables"
    );
    assert!(css.contains(".sidebar"), "should have sidebar styles");
    assert!(css.contains(".view"), "should have view styles");
    assert!(css.contains(".btn"), "should have button styles");
}

#[test]
fn r7_t4_desktop_verification_js_loaded() {
    let js_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("app.js");
    let js = std::fs::read_to_string(&js_path).unwrap();

    // 验证 JS 非空且包含核心逻辑
    assert!(!js.is_empty(), "JS should not be empty");
    assert!(js.contains("const IPC"), "should have IPC bridge");
    assert!(js.contains("const Nav"), "should have navigation");
    assert!(js.contains("const Zoom"), "should have zoom control");
    assert!(js.contains("const Replay"), "should have replay");
    assert!(js.contains("const DiffMode"), "should have diff mode");
    assert!(
        js.contains("const Context"),
        "should have context injection"
    );
}

// ============================================================================
// R7-t5: 可访问性 -- 缩放 80-200%、键盘流、aria-live
// ============================================================================

#[test]
fn r7_t5_accessibility_zoom_support() {
    let css_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("styles.css");
    let css = std::fs::read_to_string(&css_path).unwrap();

    // 验证缩放范围 80-200%
    assert!(css.contains("data-zoom=\"80\""), "should support 80% zoom");
    assert!(
        css.contains("data-zoom=\"200\""),
        "should support 200% zoom"
    );

    // 验证所有中间档位
    for level in [
        80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200,
    ] {
        assert!(
            css.contains(&format!("data-zoom=\"{level}\"")),
            "should support {level}% zoom"
        );
    }

    // 验证有 transform: scale 用于真窗口缩放
    assert!(
        css.contains("transform: scale"),
        "should use CSS transform for real window zoom"
    );
}

#[test]
fn r7_t5_accessibility_zoom_js_handlers() {
    let js_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("app.js");
    let js = std::fs::read_to_string(&js_path).unwrap();

    // 验证 JS 中有缩放控制
    assert!(js.contains("Zoom.set"), "should have Zoom.set function");
    assert!(js.contains("Zoom.in"), "should have Zoom.in function");
    assert!(js.contains("Zoom.out"), "should have Zoom.out function");
    assert!(js.contains("Zoom.reset"), "should have Zoom.reset function");

    // 验证缩放范围限制
    assert!(
        js.contains("min: 80") && js.contains("max: 200"),
        "should enforce 80-200% zoom range"
    );

    // 验证键盘快捷键
    assert!(
        js.contains("Cmd/Ctrl") || js.contains("metaKey") || js.contains("ctrlKey"),
        "should support Cmd/Ctrl keyboard shortcuts for zoom"
    );
}

#[test]
fn r7_t5_accessibility_aria_live() {
    let html_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("index.html");
    let html = std::fs::read_to_string(&html_path).unwrap();

    // 验证有 aria-live 区域
    assert!(
        html.contains("aria-live=\"polite\""),
        "should have aria-live polite regions"
    );
    assert!(
        html.contains("aria-live=\"assertive\""),
        "should have aria-live assertive regions"
    );

    // 验证有专门的 announcer
    assert!(
        html.contains("id=\"aria-announcer\""),
        "should have dedicated aria announcer"
    );
    assert!(
        html.contains("role=\"status\""),
        "announcer should have role=status"
    );

    // 验证 timeline 有 aria-live
    assert!(
        html.contains("role=\"log\""),
        "timeline should have role=log for live updates"
    );
}

#[test]
fn r7_t5_accessibility_keyboard_navigation() {
    let html_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("index.html");
    let html = std::fs::read_to_string(&html_path).unwrap();

    // 验证所有交互元素有 tabindex
    assert!(
        html.contains("tabindex=\"0\""),
        "interactive elements should have tabindex for keyboard access"
    );

    // 验证有 role 属性
    assert!(
        html.contains("role=\"button\""),
        "buttons should have role=button"
    );
    assert!(
        html.contains("role=\"navigation\""),
        "should have navigation role"
    );
    assert!(
        html.contains("role=\"region\""),
        "should have region roles for views"
    );

    let js_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("app.js");
    let js = std::fs::read_to_string(&js_path).unwrap();

    // 验证 JS 有键盘事件处理
    assert!(
        js.contains("addEventListener('keydown'"),
        "should have keyboard event listeners"
    );
    assert!(js.contains("e.key === 'Enter'"), "should handle Enter key");
    assert!(js.contains("Escape"), "should handle Escape key");
    assert!(
        js.contains("e.altKey"),
        "should support Alt+number navigation"
    );
}

#[test]
fn r7_t5_accessibility_diff_text_mode() {
    let js_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("app.js");
    let js = std::fs::read_to_string(&js_path).unwrap();

    // 验证有 F7/Shift+F7 处理
    assert!(
        js.contains("F7"),
        "should handle F7 for accessible diff navigation"
    );
    assert!(
        js.contains("e.shiftKey"),
        "should handle Shift+F7 for reverse navigation"
    );

    // 验证有 diff text mode 渲染
    assert!(
        js.contains("DiffMode.toggle"),
        "should have diff mode toggle"
    );
    assert!(
        js.contains("DiffMode.next"),
        "should have diff next navigation"
    );
    assert!(
        js.contains("DiffMode.prev"),
        "should have diff prev navigation"
    );
    assert!(
        js.contains("renderTextDiff"),
        "should render text diff for screen readers"
    );

    let html_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("index.html");
    let html = std::fs::read_to_string(&html_path).unwrap();

    // 验证 HTML 有 diff text mode 区域
    assert!(
        html.contains("diff-text-mode"),
        "should have diff text mode panel"
    );
    assert!(
        html.contains("aria-live=\"assertive\"") && html.contains("diff-text-mode"),
        "diff text mode should use aria-live assertive"
    );
}

#[test]
fn r7_t5_accessibility_replay_evidence_badges() {
    let css_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("styles.css");
    let css = std::fs::read_to_string(&css_path).unwrap();

    // 验证有证据级别 badge 样式
    assert!(
        css.contains(".evidence-badge"),
        "should have evidence badge styles"
    );
    assert!(
        css.contains(".verified"),
        "should have verified badge style"
    );
    assert!(
        css.contains(".recorded"),
        "should have recorded badge style"
    );
    assert!(
        css.contains(".observed"),
        "should have observed badge style"
    );
    assert!(
        css.contains(".inferred"),
        "should have inferred badge style"
    );
    assert!(css.contains(".missing"), "should have missing badge style");

    let js_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("app.js");
    let js = std::fs::read_to_string(&js_path).unwrap();

    // 验证 JS 有回放深度切换
    assert!(
        js.contains("Replay.setDepth"),
        "should have replay depth switching"
    );
    assert!(
        js.contains("recap") && js.contains("explore") && js.contains("verify"),
        "should support all three replay depths"
    );
    assert!(
        js.contains("evidenceForEvent"),
        "should determine evidence level per event"
    );
}

// ============================================================================
// R7-t5: 上下文投喂验证 [doc-04 §7]
// ============================================================================

#[test]
fn r7_t5_context_injection_features() {
    let js_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("app.js");
    let js = std::fs::read_to_string(&js_path).unwrap();

    // 验证有文件引用 @path
    assert!(
        js.contains("injectFileRef"),
        "should have file reference injection"
    );
    assert!(js.contains("@"), "should use @ syntax for file references");

    // 验证有选区引用
    assert!(
        js.contains("injectSelectionRef"),
        "should have selection reference injection"
    );

    // 验证有外部会话注入（bracketed paste，无回车）
    assert!(
        js.contains("injectExternalSession"),
        "should have external session injection"
    );
    assert!(
        js.contains("bracketed paste"),
        "should mention bracketed paste"
    );
    assert!(
        js.contains("\\x1b[200~"),
        "should use bracketed paste escape sequence"
    );
    assert!(
        js.contains("no trailing Enter") || js.contains("无回车") || js.contains("no 回车"),
        "should explicitly note no trailing Enter"
    );
}

// ============================================================================
// 附加测试: Agent 命令
// ============================================================================

#[tokio::test]
async fn r7_agent_send_and_abort() {
    let (_dir, state) = setup_state();

    let task = cmd_task_create(
        &state,
        "/proj".into(),
        "Agent test".into(),
        "Test agent".into(),
        "ask".into(),
    )
    .await
    .unwrap();

    // 发送消息
    cmd_agent_send(&state, task.id.clone(), "Hello agent".into())
        .await
        .unwrap();

    // 验证会话文件已创建
    let session_path = state.sessions_dir.join(format!("{}.jsonl", task.id));
    assert!(session_path.exists(), "session file should exist");

    // 中止
    TaskRepository::new(&state.db)
        .update_state(&task.id, TaskState::InProgress)
        .unwrap();

    r_code_host::commands::cmd_agent_abort(&state, task.id.clone())
        .await
        .unwrap();

    let detail = cmd_task_detail(&state, task.id).await.unwrap();
    assert_eq!(detail.task.state, TaskState::Idle);
}

// ============================================================================
// 附加测试: 恢复数据
// ============================================================================

#[tokio::test]
async fn r7_recovery_data() {
    let (_dir, state) = setup_state();

    // 空数据库 -> 无中断任务
    let data = cmd_recovery_data(&state).await.unwrap();
    assert!(data.interrupted_tasks.is_empty());
    assert_eq!(data.orphaned_permissions, 0);
}

// ============================================================================
// 附加测试: 接受任务
// ============================================================================

#[tokio::test]
async fn r7_accept_task() {
    let (_dir, state) = setup_state();

    let task = cmd_task_create(
        &state,
        "/proj".into(),
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
    cmd_accept_task(&state, task.id.clone()).await.unwrap();

    // 验证状态回到 Idle
    let detail = cmd_task_detail(&state, task.id.clone()).await.unwrap();
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
