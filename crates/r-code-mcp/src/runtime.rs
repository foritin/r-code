use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use hermes_core::ToolCallOutcome;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::{watch, Mutex, RwLock};

use crate::client::{MCP_INITIALIZE_TIMEOUT, MCP_LIST_TOOLS_TIMEOUT};
use crate::{
    McpClientError, McpClientSession, McpConfigError, McpConnector, McpServerConfig,
    McpServerState, McpServerStatus, McpToolDescriptor,
};

const MCP_ABORT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
const MCP_CONNECT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(MCP_INITIALIZE_TIMEOUT.as_secs() + 5);
const MCP_SUPERVISOR_LIST_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(MCP_LIST_TOOLS_TIMEOUT.as_secs() + 1);
const MCP_SUPERVISOR_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum McpRuntimeError {
    #[error(transparent)]
    InvalidConfig(#[from] McpConfigError),
    #[error("duplicate MCP server id: {0}")]
    DuplicateServer(String),
    #[error("unknown MCP server: {0}")]
    UnknownServer(String),
    #[error("MCP runtime is shutting down")]
    ShuttingDown,
    #[error(transparent)]
    Client(#[from] McpClientError),
}

struct ServerSlot {
    config: RwLock<McpServerConfig>,
    session: RwLock<Option<Arc<dyn McpClientSession>>>,
    connect_gate: Mutex<()>,
}

impl ServerSlot {
    fn new(config: McpServerConfig) -> Self {
        Self {
            config: RwLock::new(config),
            session: RwLock::new(None),
            connect_gate: Mutex::new(()),
        }
    }
}

pub struct McpSupervisor {
    slots: RwLock<BTreeMap<String, Arc<ServerSlot>>>,
    connector: Arc<dyn McpConnector>,
    statuses: watch::Sender<BTreeMap<String, McpServerStatus>>,
    mutation: Mutex<()>,
    shutting_down: AtomicBool,
}

impl McpSupervisor {
    pub fn new(
        connector: Arc<dyn McpConnector>,
        configs: Vec<McpServerConfig>,
    ) -> Result<Self, McpRuntimeError> {
        let mut ids = BTreeSet::new();
        let mut slots = BTreeMap::new();
        let mut statuses = BTreeMap::new();
        for config in configs {
            config.validate()?;
            if !ids.insert(config.id.clone()) {
                return Err(McpRuntimeError::DuplicateServer(config.id));
            }
            let state = if config.enabled {
                McpServerState::Stopped
            } else {
                McpServerState::Disabled
            };
            statuses.insert(
                config.id.clone(),
                McpServerStatus {
                    id: config.id.clone(),
                    state,
                    tool_count: 0,
                    error_code: None,
                },
            );
            slots.insert(config.id.clone(), Arc::new(ServerSlot::new(config)));
        }
        let (status_tx, _) = watch::channel(statuses);
        Ok(Self {
            slots: RwLock::new(slots),
            connector,
            statuses: status_tx,
            mutation: Mutex::new(()),
            shutting_down: AtomicBool::new(false),
        })
    }

    pub fn subscribe_statuses(&self) -> watch::Receiver<BTreeMap<String, McpServerStatus>> {
        self.statuses.subscribe()
    }

    pub fn status_snapshot(&self) -> Vec<McpServerStatus> {
        self.statuses.borrow().values().cloned().collect()
    }

    pub async fn config_snapshot(&self) -> Vec<McpServerConfig> {
        let slots = self
            .slots
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut configs = Vec::with_capacity(slots.len());
        for slot in slots {
            configs.push(slot.config.read().await.clone());
        }
        configs.sort_by(|left, right| left.id.cmp(&right.id));
        configs
    }

    pub async fn upsert(&self, config: McpServerConfig) -> Result<(), McpRuntimeError> {
        config.validate()?;
        let _mutation = self.mutation.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(McpRuntimeError::ShuttingDown);
        }
        let existing = self.slots.read().await.get(&config.id).cloned();
        if let Some(slot) = existing {
            let _gate = slot.connect_gate.lock().await;
            let previous = slot.config.read().await.clone();
            if previous.transport != config.transport || !config.enabled {
                self.close_slot_session(&slot).await?;
            }
            *slot.config.write().await = config.clone();
            self.publish_status(
                &config.id,
                if config.enabled {
                    McpServerState::Stopped
                } else {
                    McpServerState::Disabled
                },
                0,
                None,
            );
            return Ok(());
        }

        let state = if config.enabled {
            McpServerState::Stopped
        } else {
            McpServerState::Disabled
        };
        self.slots
            .write()
            .await
            .insert(config.id.clone(), Arc::new(ServerSlot::new(config.clone())));
        self.publish_status(&config.id, state, 0, None);
        Ok(())
    }

    pub async fn set_enabled(&self, server_id: &str, enabled: bool) -> Result<(), McpRuntimeError> {
        let _mutation = self.mutation.lock().await;
        let slot = self.slot(server_id).await?;
        let _gate = slot.connect_gate.lock().await;
        if !enabled {
            self.close_slot_session(&slot).await?;
        }
        slot.config.write().await.enabled = enabled;
        self.publish_status(
            server_id,
            if enabled {
                McpServerState::Stopped
            } else {
                McpServerState::Disabled
            },
            0,
            None,
        );
        Ok(())
    }

    pub async fn remove(&self, server_id: &str) -> Result<McpServerConfig, McpRuntimeError> {
        let _mutation = self.mutation.lock().await;
        let slot = self.slot(server_id).await?;
        let _gate = slot.connect_gate.lock().await;
        self.close_slot_session(&slot).await?;
        self.slots
            .write()
            .await
            .remove(server_id)
            .ok_or_else(|| McpRuntimeError::UnknownServer(server_id.to_string()))?;
        self.statuses.send_modify(|statuses| {
            statuses.remove(server_id);
        });
        let config = slot.config.read().await.clone();
        Ok(config)
    }

    pub async fn list_tools(
        &self,
        server_id: &str,
    ) -> Result<Vec<McpToolDescriptor>, McpRuntimeError> {
        let slot = self.slot(server_id).await?;
        let session = self.ensure_session(server_id, &slot).await?;
        let discovery =
            tokio::time::timeout(MCP_SUPERVISOR_LIST_TIMEOUT, session.list_tools(server_id))
                .await
                .map_err(|_| {
                    McpClientError::ListToolsTimeout(MCP_SUPERVISOR_LIST_TIMEOUT.as_secs())
                });
        match discovery.and_then(|result| result) {
            Ok(tools) => {
                self.publish_status(server_id, McpServerState::Running, tools.len(), None);
                Ok(tools)
            }
            Err(error) => {
                tracing::warn!(server_id, %error, "MCP tool discovery failed");
                self.publish_status(
                    server_id,
                    McpServerState::Error,
                    0,
                    Some("list_tools_failed".to_string()),
                );
                Err(error.into())
            }
        }
    }

    pub async fn call_tool(
        &self,
        server_id: &str,
        tool_name: &str,
        args: Value,
    ) -> Result<ToolCallOutcome, McpRuntimeError> {
        self.call_tool_with_abort(server_id, tool_name, args, None)
            .await
    }

    pub async fn call_tool_with_abort(
        &self,
        server_id: &str,
        tool_name: &str,
        args: Value,
        abort: Option<Arc<AtomicBool>>,
    ) -> Result<ToolCallOutcome, McpRuntimeError> {
        if abort
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(McpClientError::Cancelled.into());
        }
        let slot = match self.slot(server_id).await {
            Ok(slot) => slot,
            Err(McpRuntimeError::UnknownServer(_)) => {
                return Ok(unavailable_outcome(server_id, "not_installed"));
            }
            Err(error) => return Err(error),
        };
        if !slot.config.read().await.enabled {
            return Ok(unavailable_outcome(server_id, "disabled"));
        }
        let connect = self.ensure_session(server_id, &slot);
        tokio::pin!(connect);
        let session_result = loop {
            if abort
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                break Err(McpRuntimeError::Client(McpClientError::Cancelled));
            }
            if abort.is_none() {
                break connect.await;
            }
            tokio::select! {
                result = &mut connect => break result,
                _ = tokio::time::sleep(MCP_ABORT_POLL_INTERVAL) => {}
            }
        };
        let session = match session_result {
            Ok(session) => session,
            Err(McpRuntimeError::Client(McpClientError::Cancelled)) => {
                return Err(McpClientError::Cancelled.into());
            }
            Err(McpRuntimeError::Client(_)) => {
                return Ok(unavailable_outcome(server_id, "connection_failed"));
            }
            Err(error) => return Err(error),
        };
        match session
            .call_tool_with_abort(tool_name, args, abort.clone())
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(McpClientError::Cancelled) => Err(McpClientError::Cancelled.into()),
            Err(error) => {
                tracing::warn!(server_id, tool_name, %error, "MCP tool call failed");
                self.publish_status(
                    server_id,
                    McpServerState::Error,
                    0,
                    Some("call_failed".to_string()),
                );
                Ok(ToolCallOutcome {
                    content: format!("MCP service '{server_id}' could not complete the tool call."),
                    is_error: true,
                    metadata: Some(json!({
                        "server_id": server_id,
                        "reason": "call_failed",
                    })),
                })
            }
        }
    }

    pub async fn shutdown(&self) -> Result<(), McpRuntimeError> {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let _mutation = self.mutation.lock().await;
        let slots = self
            .slots
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for slot in slots {
            let _gate = slot.connect_gate.lock().await;
            let id = slot.config.read().await.id.clone();
            if let Err(error) = self.close_slot_session(&slot).await {
                tracing::warn!(server_id = %id, %error, "MCP session shutdown failed");
                self.publish_status(
                    &id,
                    McpServerState::Error,
                    0,
                    Some("shutdown_failed".to_string()),
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            } else {
                self.publish_status(&id, McpServerState::Stopped, 0, None);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn slot(&self, server_id: &str) -> Result<Arc<ServerSlot>, McpRuntimeError> {
        self.slots
            .read()
            .await
            .get(server_id)
            .cloned()
            .ok_or_else(|| McpRuntimeError::UnknownServer(server_id.to_string()))
    }

    async fn ensure_session(
        &self,
        server_id: &str,
        slot: &Arc<ServerSlot>,
    ) -> Result<Arc<dyn McpClientSession>, McpRuntimeError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(McpRuntimeError::ShuttingDown);
        }
        if !slot.config.read().await.enabled {
            return Err(McpRuntimeError::UnknownServer(server_id.to_string()));
        }
        if let Some(session) = slot.session.read().await.clone() {
            if !session.is_closed() {
                return Ok(session);
            }
        }

        let _gate = slot.connect_gate.lock().await;
        let config = slot.config.read().await.clone();
        if !config.enabled {
            return Err(McpRuntimeError::UnknownServer(server_id.to_string()));
        }
        if let Some(session) = slot.session.read().await.clone() {
            if !session.is_closed() {
                return Ok(session);
            }
        }
        self.publish_status(server_id, McpServerState::Starting, 0, None);
        let connection = tokio::time::timeout(MCP_CONNECT_TIMEOUT, self.connector.connect(&config))
            .await
            .map_err(|_| McpClientError::ConnectionTimeout(MCP_CONNECT_TIMEOUT.as_secs()));
        match connection.and_then(|result| result) {
            Ok(session) => {
                *slot.session.write().await = Some(session.clone());
                self.publish_status(server_id, McpServerState::Running, 0, None);
                Ok(session)
            }
            Err(error) => {
                tracing::warn!(server_id, %error, "MCP connection failed");
                self.publish_status(
                    server_id,
                    McpServerState::Error,
                    0,
                    Some("connect_failed".to_string()),
                );
                Err(error.into())
            }
        }
    }

    async fn close_slot_session(&self, slot: &ServerSlot) -> Result<(), McpRuntimeError> {
        let Some(session) = slot.session.write().await.take() else {
            return Ok(());
        };
        tokio::time::timeout(MCP_SUPERVISOR_CLOSE_TIMEOUT, session.close())
            .await
            .map_err(|_| {
                McpClientError::Shutdown(format!(
                    "MCP session did not close within {} seconds",
                    MCP_SUPERVISOR_CLOSE_TIMEOUT.as_secs()
                ))
            })??;
        Ok(())
    }

    fn publish_status(
        &self,
        server_id: &str,
        state: McpServerState,
        tool_count: usize,
        error_code: Option<String>,
    ) {
        self.statuses.send_modify(|statuses| {
            statuses.insert(
                server_id.to_string(),
                McpServerStatus {
                    id: server_id.to_string(),
                    state,
                    tool_count,
                    error_code,
                },
            );
        });
    }
}

fn unavailable_outcome(server_id: &str, reason: &str) -> ToolCallOutcome {
    ToolCallOutcome {
        content: format!(
            "MCP service '{server_id}' is {reason}. Continue without it or ask the user to enable it."
        ),
        is_error: true,
        metadata: Some(json!({"server_id": server_id, "reason": reason})),
    }
}
