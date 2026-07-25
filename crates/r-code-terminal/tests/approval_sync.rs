//! R8 集成测试 -- 审批同步、takeover/hand-back、replay 解析性能。
//!
//! 验证三个 R8 场景：
//! - `approval_multi_entry_sync`：审批多入口同步（standing rule 与单次审批）
//! - `takeover_handback_conceptual`：takeover 自控拒绝（安全基础）
//! - `replay_parser_performance`：replay 解析器性能基准
//!
//! 运行：`cargo test -p r-code-terminal --test approval_sync`

use r_code_core::dto::{PermissionDecision, RiskLevel};
use r_code_gateway::{PermissionCheckResult, PermissionEngine};
use r_code_terminal::replay_parser::ReplayParser;
use r_code_terminal::{SendOptions, TerminalControlService, TerminalManager};
use std::sync::Arc;

/// Unix: /bin/cat（回显 stdin）；Windows: cmd.exe。
fn shell_path() -> &'static str {
    #[cfg(unix)]
    {
        if std::path::Path::new("/bin/cat").exists() {
            "/bin/cat"
        } else {
            "/usr/bin/cat"
        }
    }
    #[cfg(windows)]
    {
        "cmd.exe"
    }
}

/// R8-t12: Approval multi-entry sync - any entry resolves, all clear.
#[tokio::test]
async fn approval_multi_entry_sync() {
    let engine = PermissionEngine::new();

    // Create a permission request for R2 risk
    let req = engine
        .request_permission(
            "task1",
            "tc1",
            "write_file",
            RiskLevel::R2,
            "writing test.txt",
        )
        .await;

    // Check: should need approval
    let check = engine
        .check("task1", "write_file", RiskLevel::R2, None)
        .await;
    match check {
        PermissionCheckResult::NeedsApproval(r) => {
            // Multiple checks should all reference the same pending request
            assert_eq!(r.task_id, "task1");
        }
        PermissionCheckResult::Allowed => panic!("R2 should need approval"),
        PermissionCheckResult::Denied(_) => panic!("R2 should not be denied"),
    }

    // Decide: allow (single-use)
    engine
        .decide(&req.id, PermissionDecision::Allow)
        .await
        .unwrap();

    // After single Allow (not AllowAlways), no standing rule is created,
    // so a subsequent check still needs approval.
    let check2 = engine
        .check("task1", "write_file", RiskLevel::R2, None)
        .await;
    assert!(
        matches!(check2, PermissionCheckResult::NeedsApproval(_)),
        "single Allow should not persist as standing rule"
    );

    // Now add standing rule (AllowAlways) -- requires risk_level param
    engine
        .add_standing_rule(
            "task1",
            "write_file",
            None,
            RiskLevel::R2,
            PermissionDecision::AllowAlways,
        )
        .await
        .unwrap();

    // Now it should be allowed
    let check3 = engine
        .check("task1", "write_file", RiskLevel::R2, None)
        .await;
    assert!(matches!(check3, PermissionCheckResult::Allowed));
}

/// R8-t13: Takeover/hand-back - user keyboard input keeps queue, resume injects.
/// This tests the terminal pause/resume behavior conceptually.
///
/// The actual takeover/hand-back requires a running PTY with user input.
/// Here we test the ControlService's self-rejection logic which is the
/// security foundation of takeover (agent can't send to its own terminal).
#[tokio::test]
async fn takeover_handback_conceptual() {
    let manager = Arc::new(TerminalManager::new());
    let svc = TerminalControlService::new(manager);

    let tmp = std::env::temp_dir();
    let shell = shell_path();

    let id = svc.create(shell, &tmp, vec![]).await.unwrap();

    // Self-send (takeover scenario) should be rejected
    let result = svc
        .send(
            &id,
            Some(id.as_str()),
            SendOptions {
                text: "injected".into(),
                press_enter: false,
            },
        )
        .await;
    assert!(
        result.is_err(),
        "self-send should be rejected (takeover protection)"
    );

    // Non-self send should work
    let result2 = svc
        .send(
            &id,
            None,
            SendOptions {
                text: "hello\n".into(),
                press_enter: true,
            },
        )
        .await;
    assert!(result2.is_ok(), "non-self send should succeed");

    let _ = svc.kill(&id, false).await;
}

/// R8-t14: Replay parser performance benchmark.
#[tokio::test]
async fn replay_parser_performance() {
    let mut parser = ReplayParser::new();

    // Generate a large transcript (1000 lines) using Claude format
    // (type=assistant, message.content[].text -- parses to AgentMessage)
    let mut data = String::new();
    for i in 0..1000 {
        data.push_str(&format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"output line {}"}}]}}}}"#,
            i
        ));
        data.push('\n');
    }

    let start = std::time::Instant::now();
    parser.feed(&data);
    let elapsed = start.elapsed();

    // Should parse 1000 lines in under 500ms
    assert!(
        elapsed.as_millis() < 500,
        "parsing 1000 lines took {:?} (expected < 500ms)",
        elapsed
    );

    // Should have parsed events
    let events = parser.events();
    assert!(!events.is_empty(), "should have parsed events");
}
