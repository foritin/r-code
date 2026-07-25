//! R8 终端控制服务集成测试 [doc-03 §8]。
//!
//! 验证 TerminalControlService 六个原语的端到端行为：
//! - `managed_driver_create_send_wait_read_kill`：完整生命周期
//! - `agent_kill_rejected`：agent 调用 kill 被拒绝
//! - `self_send_rejected`：自控 send 被拒绝
//!
//! 运行：`cargo test -p r-code-terminal --test control_integration`

use std::sync::Arc;

use r_code_terminal::{SendOptions, TerminalControlService, TerminalManager};

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

#[tokio::test]
async fn managed_driver_create_send_wait_read_kill() {
    let manager = Arc::new(TerminalManager::new());
    let svc = TerminalControlService::new(manager);

    let tmp = std::env::temp_dir();
    let shell = shell_path();

    // create
    let id = svc.create(shell, &tmp, vec![]).await.unwrap();
    assert!(!id.is_empty());

    // send
    svc.send(
        &id,
        None,
        SendOptions {
            text: "test123".to_string(),
            press_enter: true,
        },
    )
    .await
    .unwrap();

    // read（轮询直到看到回显）
    let mut found = false;
    for _ in 0..25 {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        let output = svc.read(&id).await.unwrap();
        if output.contains("test123") {
            found = true;
            break;
        }
    }
    assert!(found, "read should contain echoed text");

    // list
    let list = svc.list().await.unwrap();
    assert_eq!(list.len(), 1);

    // kill
    svc.kill(&id, false).await.unwrap();
}

#[tokio::test]
async fn agent_kill_rejected() {
    let manager = Arc::new(TerminalManager::new());
    let svc = TerminalControlService::new(manager);

    let tmp = std::env::temp_dir();
    let shell = shell_path();

    let id = svc.create(shell, &tmp, vec![]).await.unwrap();

    // agent kill 应被拒绝
    let result = svc.kill(&id, true).await;
    assert!(result.is_err());

    // 清理
    let _ = svc.kill(&id, false).await;
}

#[tokio::test]
async fn self_send_rejected() {
    let manager = Arc::new(TerminalManager::new());
    let svc = TerminalControlService::new(manager);

    let tmp = std::env::temp_dir();
    let shell = shell_path();

    let id = svc.create(shell, &tmp, vec![]).await.unwrap();

    // 自控应被拒绝
    let result = svc
        .send(
            &id,
            Some(id.as_str()),
            SendOptions {
                text: "test".to_string(),
                press_enter: false,
            },
        )
        .await;
    assert!(result.is_err());

    // 清理
    let _ = svc.kill(&id, false).await;
}
