//! P0 健康检查测试 [doc-19 §2 退出条件]。
//!
//! 验证 IPC server 的 `ping`/`task.create` 处理器端到端可用：
//! - `ping` 响应 `{ "pong": true }`
//! - `task.create` 返回带 `id`/`mode`/`state` 的新任务
//!
//! 运行：`cargo test -p r-code-host --test health_check`

use std::sync::Arc;

use hermes_ipc::{IpcClient, IpcServer};

#[tokio::test]
async fn ping_pong_health_check() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("health.sock");
    let mut server = IpcServer::bind(socket).unwrap();
    server.register("ping", Arc::new(r_code_host::ipc::PingHandler));

    let path = server.socket_path().clone();
    tokio::spawn(async move {
        let _ = server.serve().await;
    });

    // 等待 server 启动
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = IpcClient::connect(&path).await.unwrap();
    let result = client.call("ping", serde_json::Value::Null).await.unwrap();
    assert_eq!(result["pong"], true);
}

#[tokio::test]
async fn task_create_via_ipc() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("task.sock");
    let mut server = IpcServer::bind(socket).unwrap();
    server.register("task.create", Arc::new(r_code_host::ipc::TaskCreateHandler));

    let path = server.socket_path().clone();
    tokio::spawn(async move {
        let _ = server.serve().await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = IpcClient::connect(&path).await.unwrap();
    let result = client
        .call(
            "task.create",
            serde_json::json!({
                "projectId": "/test/project",
                "goal": "Test task",
                "mode": "ask"
            }),
        )
        .await
        .unwrap();

    assert!(result["id"].is_string());
    assert_eq!(result["mode"], "ask");
    assert_eq!(result["state"], "idle");
}
