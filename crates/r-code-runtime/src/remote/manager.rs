//! Remote device management (R11): the local-console surface over the
//! device registry and the live listener. One implementation shared by the
//! service daemon and the management tests — revoke/capability narrowing
//! drop live sockets immediately (R11.A2/A3 semantics live here).

use crate::application::CommandSource;
use crate::daemon::ApplicationHandler;
use crate::remote::capabilities::CapabilitySet;
use crate::remote::fanout::FanoutHub;
use crate::remote::listener::{listen_pairing, ListenerHandle};
use crate::remote::pairing::{PairingSessions, PairingStartReply};
use crate::remote::registry::{DeviceRegistry, ListenerConfig};
use crate::remote::tls::Identity;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::Mutex;

/// The remote-control manager owned by the daemon (and exercised directly
/// by R11's tests).
pub struct RemoteManager {
    pub registry: Arc<DeviceRegistry>,
    pub pairing: Arc<PairingSessions>,
    pub identity: Identity,
    pub hub: Arc<FanoutHub>,
    pub app_dir: Option<std::path::PathBuf>,
    pub bind_ip: IpAddr,
    /// The dedup-wrapped handler remote connections share with the local
    /// pipe (F1), injected once the daemon is composed.
    handler: Mutex<Option<Arc<dyn ApplicationHandler>>>,
    listener: Mutex<Option<ListenerHandle>>,
}

/// Errors from the management surface (structured codes for the console).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagerError {
    #[error("unknown device {0}")]
    UnknownDevice(String),
    #[error("remote handler not wired yet")]
    NotWired,
    #[error("{0}")]
    Failure(String),
}

impl RemoteManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: Arc<DeviceRegistry>,
        pairing: Arc<PairingSessions>,
        identity: Identity,
        hub: Arc<FanoutHub>,
        app_dir: Option<std::path::PathBuf>,
        bind_ip: IpAddr,
    ) -> Self {
        Self {
            registry,
            pairing,
            identity,
            hub,
            app_dir,
            bind_ip,
            handler: Mutex::new(None),
            listener: Mutex::new(None),
        }
    }

    /// Inject the dedup-wrapped handler remote connections serve through.
    pub async fn wire_handler(&self, handler: Arc<dyn ApplicationHandler>) {
        *self.handler.lock().await = Some(handler);
    }

    /// Ensure the pairing listener is up; return its address. The listener
    /// closes itself when the pairing window lapses with no device (R04
    /// supervision), so re-entry restarts it.
    pub async fn ensure_listener(&self) -> Result<SocketAddr, ManagerError> {
        let mut listener = self.listener.lock().await;
        if let Some(handle) = listener.as_ref() {
            return Ok(handle.local_addr);
        }
        let handler = self
            .handler
            .lock()
            .await
            .clone()
            .ok_or(ManagerError::NotWired)?;
        let handle = listen_pairing(
            self.bind_ip,
            0,
            self.registry.clone(),
            self.identity.duplicate(),
            handler,
            self.hub.clone(),
            self.app_dir.clone(),
            Some(self.pairing.clone()),
        )
        .await
        .map_err(|e| ManagerError::Failure(e.to_string()))?;
        let addr = handle.local_addr;
        *listener = Some(handle);
        Ok(addr)
    }

    /// `remote.pairingStart` (local console only, F4/F5): open the listener
    /// surface and start a one-shot session.
    pub async fn pairing_start(&self) -> Result<PairingStartReply, ManagerError> {
        // F2: the listener surface opens with pairing; it closes itself
        // when the window lapses with no device. The durable switch stays
        // owned by the management console (setListener).
        if !self.registry.listener().enabled {
            self.registry
                .set_listener(ListenerConfig {
                    enabled: true,
                    bind: vec!["loopback".into()],
                    port: None,
                })
                .map_err(|e| ManagerError::Failure(e.to_string()))?;
        }
        let addr = self.ensure_listener().await?;
        let endpoint = format!("wss://{}:{}", self.bind_ip, addr.port());
        self.pairing
            .start(
                CommandSource::Local,
                &self.identity.fingerprint,
                vec![endpoint],
            )
            .map_err(|e| ManagerError::Failure(e.to_string()))
    }

    /// The current pairing listener port (when listening).
    pub async fn listening_port(&self) -> Option<u16> {
        self.listener
            .lock()
            .await
            .as_ref()
            .map(|handle| handle.local_addr.port())
    }

    /// `device.list`: every device row (revoked included) for the console.
    pub fn list_devices(&self) -> Vec<serde_json::Value> {
        self.registry
            .list()
            .into_iter()
            .map(|device| {
                serde_json::json!({
                    "deviceId": device.id,
                    "name": device.name,
                    "platform": device.platform,
                    "capabilities": device.capabilities.labels(),
                    "pairedAt": device.paired_at,
                    "lastSeenAt": device.last_seen_at,
                    "revoked": device.revoked,
                })
            })
            .collect()
    }

    /// `device.revoke`: revoke + drop the device's live sockets. The
    /// listener closes itself once the last live device is gone (F2).
    pub async fn revoke(&self, device_id: &str) -> Result<(), ManagerError> {
        self.registry
            .revoke(device_id)
            .map_err(|_| ManagerError::UnknownDevice(device_id.into()))?;
        if let Some(handle) = self.listener.lock().await.as_ref() {
            handle.disconnect_device(device_id);
        }
        Ok(())
    }

    /// `device.updateCapabilities`: narrow/widen grants; the device's live
    /// connections drop so the new set applies on reconnect.
    pub async fn update_capabilities(
        &self,
        device_id: &str,
        labels: &[&str],
    ) -> Result<Vec<&'static str>, ManagerError> {
        let mut set = CapabilitySet::read_only();
        for label in labels {
            match *label {
                "tasks-write" => set = set.with_tasks_write(),
                "approvals-decide" => set = set.with_approvals_decide(),
                "events-read" => {}
                _ => return Err(ManagerError::Failure(format!("unknown capability {label}"))),
            }
        }
        let record = self
            .registry
            .update_capabilities(device_id, set)
            .map_err(|_| ManagerError::UnknownDevice(device_id.into()))?;
        if let Some(handle) = self.listener.lock().await.as_ref() {
            handle.disconnect_device(device_id);
        }
        Ok(record.capabilities.labels())
    }

    /// `device.setListener`: the durable switch. Off drops every
    /// connection but keeps device records; on (with a live device or
    /// pairing intent) restarts the listener.
    pub async fn set_listener(&self, enabled: bool) -> Result<(), ManagerError> {
        self.registry
            .set_listener(ListenerConfig {
                enabled,
                bind: vec![],
                port: None,
            })
            .map_err(|e| ManagerError::Failure(e.to_string()))?;
        if enabled {
            let _ = self.ensure_listener().await?;
        } else if let Some(handle) = self.listener.lock().await.take() {
            handle.stop();
        }
        Ok(())
    }
}
