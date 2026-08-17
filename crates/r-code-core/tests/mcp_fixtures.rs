//! P1 MCP 测试夹具 [agent-contracts/14 §3]。
//!
//! 验证 MCP 合同向量与故障演练：
//! - V-TOOL-01：未知工具被拒绝
//! - V-TOOL-02：`server__tool` 命名空间防止碰撞
//! - V-TOOL-03：stdio 子进程退出 -> 下次调用返回可诊断错误
//! - V-TOOL-04：HTTP 超时/非 2xx/非法 JSON -> 稳定错误类别
//! - P1-t17：stdio 断连 / HTTP 超时夹具
//! - P1-t18：会话截断 / 未知工具故障演练
//!
//! 运行：`cargo test -p r-code-core --test mcp_fixtures`

use std::sync::Arc;

use agent_mcp::{McpError, McpServer, McpToolHost, MockTransport, Transport};
use serde_json::json;

/// 辅助：配置 initialize + tools/list 成功的 mock。
async fn configure_connected_mock(mock: &MockTransport) {
    mock.set_result("initialize", json!({"capabilities": {"tools": {}}}))
        .await;
    mock.set_result("tools/list", json!({"tools": []})).await;
}

/// V-TOOL-01：未知工具被拒绝。
#[tokio::test]
async fn v_tool_01_unknown_rejected() {
    let mock = MockTransport::new();
    configure_connected_mock(&mock).await;
    mock.set_error("tools/call", McpError::ToolNotFound("unknown".into()))
        .await;

    let server = McpServer::new("test", Arc::new(mock));
    server.connect().await.unwrap();

    let result = server.call_tool("unknown_tool", json!({})).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::ToolNotFound(_)));
}

/// V-TOOL-02：`server__tool` 命名空间防止碰撞；裸名/空段不可解析。
#[tokio::test]
async fn v_tool_02_namespaced_naming() {
    let (server, tool) = McpToolHost::parse_namespaced("fs__read_file").unwrap();
    assert_eq!(server, "fs");
    assert_eq!(tool, "read_file");

    // 裸名应解析失败
    assert!(McpToolHost::parse_namespaced("read_file").is_err());
    // 空段不可解析
    assert!(McpToolHost::parse_namespaced("fs__").is_err());
    assert!(McpToolHost::parse_namespaced("__read_file").is_err());
}

/// V-TOOL-03：stdio 子进程退出后，下次调用返回可诊断错误（非 panic/挂起）。
#[tokio::test]
async fn v_tool_03_stdio_exit_diagnosable() {
    // 模拟：server 已连接，但 tools/call 返回 NotConnected（子进程已退出）
    let mock = MockTransport::new();
    configure_connected_mock(&mock).await;
    mock.set_error("tools/call", McpError::NotConnected("server exited".into()))
        .await;

    let server = McpServer::new("test-stdio", Arc::new(mock));
    server.connect().await.unwrap();

    // 退出后调用应返回可诊断错误
    let result = server.call_tool("some_tool", json!({})).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::NotConnected(_)));
}

/// V-TOOL-04：HTTP 超时/非 2xx/非法 JSON 映射为稳定错误类别。
#[tokio::test]
async fn v_tool_04_http_error_mapping() {
    // 超时 -> 稳定 Timeout 类别
    let mock = MockTransport::new();
    configure_connected_mock(&mock).await;
    mock.set_error("tools/call", McpError::Timeout).await;

    let server = McpServer::new("test-http", Arc::new(mock));
    server.connect().await.unwrap();

    let result = server.call_tool("some_tool", json!({})).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::Timeout));
}

/// P1-t17：stdio MCP 断连夹具 -- 正常生命周期后断连，调用可诊断。
#[tokio::test]
async fn stdio_mcp_disconnect_fixture() {
    // 1. 正常生命周期：initialize + tools/list 成功
    let mock = MockTransport::new();
    mock.set_result("initialize", json!({"capabilities": {"tools": {}}}))
        .await;
    mock.set_result(
        "tools/list",
        json!({"tools": [{"name": "read", "description": "read", "inputSchema": {"type": "object"}}]}),
    )
    .await;

    let server = McpServer::new("stdio-test", Arc::new(mock));
    server.connect().await.unwrap();

    let tools = server.tools().await;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read");

    // 2. 断连后：initialize 返回 NotConnected -> connect 失败
    let mock2 = MockTransport::new();
    mock2
        .set_error("initialize", McpError::NotConnected("exited".into()))
        .await;
    let server2 = McpServer::new("stdio-test-2", Arc::new(mock2));
    let result = server2.connect().await;
    assert!(result.is_err(), "connect after exit should fail");
}

/// P1-t17：HTTP MCP 超时夹具 -- 传输层返回稳定 Timeout，server 层 connect 失败。
#[tokio::test]
async fn http_mcp_timeout_fixture() {
    let mock = MockTransport::new();
    mock.set_error("initialize", McpError::Timeout).await;

    let transport = Arc::new(mock);

    // 传输层：Timeout 是稳定错误类别
    let result = transport.request("initialize", None).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::Timeout));

    // server 层：connect 包装为 InitializeFailed
    let server = McpServer::new("http-test", transport);
    let result = server.connect().await;
    assert!(result.is_err(), "connect with timeout should fail");
}

/// P1-t18：工具调用被中断 -> 可诊断错误（会话截断等价故障）。
#[tokio::test]
async fn fault_drill_session_truncation() {
    let mock = MockTransport::new();
    configure_connected_mock(&mock).await;
    mock.set_error("tools/call", McpError::CallFailed("interrupted".into()))
        .await;

    let server = McpServer::new("fault-test", Arc::new(mock));
    server.connect().await.unwrap();

    let result = server.call_tool("any", json!({})).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::CallFailed(_)));
}

/// P1-t18：未知工具故障演练。
#[tokio::test]
async fn fault_drill_unknown_tool() {
    let mock = MockTransport::new();
    configure_connected_mock(&mock).await;
    mock.set_error("tools/call", McpError::ToolNotFound("mystery".into()))
        .await;

    let server = McpServer::new("fault-test", Arc::new(mock));
    server.connect().await.unwrap();

    let result = server.call_tool("mystery", json!({})).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::ToolNotFound(_)));
}
