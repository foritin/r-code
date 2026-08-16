use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use agent_contract::ToolCallOutcome;
use r_code_mcp::{
    McpClientError, McpClientSession, McpConnector, McpServerConfig, McpServerSource,
    McpServerState, McpSupervisor, McpToolDescriptor, McpTransportConfig,
};
use serde_json::{json, Value};

struct FakeSession {
    closed: AtomicBool,
    close_count: Arc<AtomicUsize>,
}

#[async_trait]
impl McpClientSession for FakeSession {
    async fn list_tools(&self, server_id: &str) -> Result<Vec<McpToolDescriptor>, McpClientError> {
        tokio::task::yield_now().await;
        Ok(vec![McpToolDescriptor {
            server_id: server_id.to_string(),
            name: "lookup".to_string(),
            description: "Lookup a value".to_string(),
            input_schema: json!({"type": "object"}),
            read_only: true,
        }])
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<ToolCallOutcome, McpClientError> {
        Ok(ToolCallOutcome {
            content: format!("{name}:{args}"),
            is_error: false,
            metadata: None,
        })
    }

    async fn close(&self) -> Result<(), McpClientError> {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.close_count.fetch_add(1, Ordering::AcqRel);
        }
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

struct FakeConnector {
    connect_count: AtomicUsize,
    close_count: Arc<AtomicUsize>,
    fail: AtomicBool,
}

impl FakeConnector {
    fn new() -> Self {
        Self {
            connect_count: AtomicUsize::new(0),
            close_count: Arc::new(AtomicUsize::new(0)),
            fail: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl McpConnector for FakeConnector {
    async fn connect(
        &self,
        _config: &McpServerConfig,
    ) -> Result<Arc<dyn McpClientSession>, McpClientError> {
        self.connect_count.fetch_add(1, Ordering::AcqRel);
        tokio::task::yield_now().await;
        if self.fail.load(Ordering::Acquire) {
            return Err(McpClientError::Initialize("fixture failure".to_string()));
        }
        Ok(Arc::new(FakeSession {
            closed: AtomicBool::new(false),
            close_count: self.close_count.clone(),
        }))
    }
}

fn config(enabled: bool) -> McpServerConfig {
    McpServerConfig {
        id: "fixture".to_string(),
        display_name: "Fixture".to_string(),
        description: String::new(),
        enabled,
        source: McpServerSource::User,
        transport: McpTransportConfig::Stdio {
            command: "fixture".to_string(),
            args: Vec::new(),
            env: Default::default(),
        },
        approved_launch_fingerprint: None,
    }
}

#[tokio::test]
async fn concurrent_first_use_connects_only_once() {
    let connector = Arc::new(FakeConnector::new());
    let supervisor = Arc::new(McpSupervisor::new(connector.clone(), vec![config(true)]).unwrap());
    let mut tasks = Vec::new();
    for _ in 0..24 {
        let supervisor = supervisor.clone();
        tasks.push(tokio::spawn(async move {
            supervisor.list_tools("fixture").await.unwrap()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().len(), 1);
    }

    assert_eq!(connector.connect_count.load(Ordering::Acquire), 1);
    assert_eq!(supervisor.status_snapshot()[0].tool_count, 1);
}

#[tokio::test]
async fn disabled_call_is_an_ordinary_outcome_and_starts_nothing() {
    let connector = Arc::new(FakeConnector::new());
    let supervisor = McpSupervisor::new(connector.clone(), vec![config(false)]).unwrap();

    let outcome = supervisor
        .call_tool("fixture", "lookup", json!({}))
        .await
        .unwrap();

    assert!(outcome.is_error);
    assert_eq!(outcome.metadata.unwrap()["reason"], "disabled");
    assert_eq!(connector.connect_count.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn toggle_off_closes_session_and_next_enable_reconnects() {
    let connector = Arc::new(FakeConnector::new());
    let supervisor = McpSupervisor::new(connector.clone(), vec![config(true)]).unwrap();
    supervisor.list_tools("fixture").await.unwrap();

    supervisor.set_enabled("fixture", false).await.unwrap();
    assert_eq!(connector.close_count.load(Ordering::Acquire), 1);
    assert_eq!(
        supervisor.status_snapshot()[0].state,
        McpServerState::Disabled
    );
    let disabled = supervisor
        .call_tool("fixture", "lookup", json!({}))
        .await
        .unwrap();
    assert!(disabled.is_error);

    supervisor.set_enabled("fixture", true).await.unwrap();
    supervisor.list_tools("fixture").await.unwrap();
    assert_eq!(connector.connect_count.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn shutdown_closes_sessions_but_preserves_enabled_configuration() {
    let connector = Arc::new(FakeConnector::new());
    let supervisor = McpSupervisor::new(connector.clone(), vec![config(true)]).unwrap();
    supervisor.list_tools("fixture").await.unwrap();

    supervisor.shutdown().await.unwrap();

    assert_eq!(connector.close_count.load(Ordering::Acquire), 1);
    assert!(supervisor.config_snapshot().await[0].enabled);
    assert!(supervisor.list_tools("fixture").await.is_err());
}

#[tokio::test]
async fn failed_connect_is_reported_without_leaking_detail_into_status() {
    let connector = Arc::new(FakeConnector::new());
    connector.fail.store(true, Ordering::Release);
    let supervisor = McpSupervisor::new(connector, vec![config(true)]).unwrap();

    assert!(supervisor.list_tools("fixture").await.is_err());

    let status = &supervisor.status_snapshot()[0];
    assert_eq!(status.state, McpServerState::Error);
    assert_eq!(status.error_code.as_deref(), Some("connect_failed"));
}

#[tokio::test]
async fn remove_reclaims_a_connected_session() {
    let connector = Arc::new(FakeConnector::new());
    let supervisor = McpSupervisor::new(connector.clone(), vec![config(true)]).unwrap();
    supervisor.list_tools("fixture").await.unwrap();

    let removed = supervisor.remove("fixture").await.unwrap();

    assert_eq!(removed.id, "fixture");
    assert_eq!(connector.close_count.load(Ordering::Acquire), 1);
    assert!(supervisor.status_snapshot().is_empty());
}
