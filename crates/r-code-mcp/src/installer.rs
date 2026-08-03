use std::{collections::BTreeMap, sync::Mutex, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{McpServerConfig, McpTransportConfig};

pub const DEFAULT_APPROVAL_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LaunchApprovalError {
    #[error("built-in MCP services do not require a launch approval")]
    Builtin,
    #[error("MCP launch approval token is unknown or has already been used")]
    UnknownToken,
    #[error("MCP launch approval token expired")]
    Expired,
    #[error("MCP launch plan changed after it was reviewed")]
    PlanChanged,
    #[error("MCP server id does not match the reviewed launch plan")]
    ServerMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaunchPreviewTransport {
    Stdio {
        executable: String,
        args: Vec<String>,
        environment_names: Vec<String>,
    },
    StreamableHttp {
        url: String,
        header_names: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPreview {
    pub token: String,
    pub server_id: String,
    pub fingerprint: String,
    pub transport: LaunchPreviewTransport,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone)]
struct PendingApproval {
    server_id: String,
    fingerprint: String,
    expires_at: DateTime<Utc>,
}

/// Keeps short-lived, single-use launch approvals in memory. Tokens deliberately never persist.
pub struct LaunchApprovalService {
    ttl: Duration,
    pending: Mutex<BTreeMap<String, PendingApproval>>,
}

impl Default for LaunchApprovalService {
    fn default() -> Self {
        Self::new(DEFAULT_APPROVAL_TTL)
    }
}

impl LaunchApprovalService {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            pending: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn issue(
        &self,
        config: &McpServerConfig,
        now: DateTime<Utc>,
    ) -> Result<LaunchPreview, LaunchApprovalError> {
        let fingerprint = launch_fingerprint(config).ok_or(LaunchApprovalError::Builtin)?;
        let transport = preview_transport(&config.transport).ok_or(LaunchApprovalError::Builtin)?;
        let expires_at = now
            + chrono::Duration::from_std(self.ttl).unwrap_or_else(|_| chrono::Duration::minutes(5));
        let token = Uuid::new_v4().to_string();
        let pending = PendingApproval {
            server_id: config.id.clone(),
            fingerprint: fingerprint.clone(),
            expires_at,
        };
        let mut approvals = self.pending.lock().expect("launch approval lock poisoned");
        approvals.retain(|_, approval| approval.expires_at > now);
        approvals.insert(token.clone(), pending);
        Ok(LaunchPreview {
            token,
            server_id: config.id.clone(),
            fingerprint,
            transport,
            expires_at,
        })
    }

    /// Consumes a token and records the approved immutable launch fingerprint on the config.
    pub fn confirm(
        &self,
        token: &str,
        config: &mut McpServerConfig,
        now: DateTime<Utc>,
    ) -> Result<(), LaunchApprovalError> {
        let approval = self
            .pending
            .lock()
            .expect("launch approval lock poisoned")
            .remove(token)
            .ok_or(LaunchApprovalError::UnknownToken)?;
        if approval.expires_at <= now {
            return Err(LaunchApprovalError::Expired);
        }
        if approval.server_id != config.id {
            return Err(LaunchApprovalError::ServerMismatch);
        }
        let current = launch_fingerprint(config).ok_or(LaunchApprovalError::Builtin)?;
        if current != approval.fingerprint {
            return Err(LaunchApprovalError::PlanChanged);
        }
        config.approved_launch_fingerprint = Some(current);
        Ok(())
    }

    pub fn is_approved(config: &McpServerConfig) -> bool {
        match launch_fingerprint(config) {
            None => true,
            Some(fingerprint) => {
                config.approved_launch_fingerprint.as_deref() == Some(&fingerprint)
            }
        }
    }
}

pub fn launch_fingerprint(config: &McpServerConfig) -> Option<String> {
    config
        .transport
        .launch_fingerprint_material()
        .map(|material| {
            let source = serde_json::to_string(&config.source).unwrap_or_default();
            blake3::hash(format!("{}\0{}\0{}", config.id, source, material).as_bytes())
                .to_hex()
                .to_string()
        })
}

fn preview_transport(transport: &McpTransportConfig) -> Option<LaunchPreviewTransport> {
    match transport {
        McpTransportConfig::Builtin { .. } => None,
        McpTransportConfig::Stdio { command, args, env } => Some(LaunchPreviewTransport::Stdio {
            executable: command.clone(),
            args: args.clone(),
            environment_names: env.keys().cloned().collect(),
        }),
        McpTransportConfig::StreamableHttp { url, headers } => {
            Some(LaunchPreviewTransport::StreamableHttp {
                url: url.clone(),
                header_names: headers.keys().cloned().collect(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{McpServerSource, SecretRef};

    use super::*;

    fn config() -> McpServerConfig {
        McpServerConfig {
            id: "sample".to_string(),
            display_name: "Sample".to_string(),
            description: String::new(),
            enabled: false,
            source: McpServerSource::User,
            transport: McpTransportConfig::Stdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "sample@1.2.3".to_string()],
                env: BTreeMap::from([(
                    "TOKEN".to_string(),
                    SecretRef::new("mcp/sample/env/TOKEN").unwrap(),
                )]),
            },
            approved_launch_fingerprint: None,
        }
    }

    #[test]
    fn confirmation_is_exact_single_use_and_contains_no_values() {
        let service = LaunchApprovalService::default();
        let now = Utc::now();
        let mut config = config();
        let preview = service.issue(&config, now).unwrap();
        assert_eq!(
            preview.transport,
            LaunchPreviewTransport::Stdio {
                executable: "npx".to_string(),
                args: vec!["-y".to_string(), "sample@1.2.3".to_string()],
                environment_names: vec!["TOKEN".to_string()],
            }
        );
        service.confirm(&preview.token, &mut config, now).unwrap();
        assert!(LaunchApprovalService::is_approved(&config));
        assert_eq!(
            service.confirm(&preview.token, &mut config, now),
            Err(LaunchApprovalError::UnknownToken)
        );
    }

    #[test]
    fn changed_and_expired_plans_are_rejected() {
        let service = LaunchApprovalService::new(Duration::from_secs(1));
        let now = Utc::now();
        let mut changed = config();
        let preview = service.issue(&changed, now).unwrap();
        if let McpTransportConfig::Stdio { args, .. } = &mut changed.transport {
            args.push("--changed".to_string());
        }
        assert_eq!(
            service.confirm(&preview.token, &mut changed, now),
            Err(LaunchApprovalError::PlanChanged)
        );

        let mut expired = config();
        let preview = service.issue(&expired, now).unwrap();
        assert_eq!(
            service.confirm(
                &preview.token,
                &mut expired,
                now + chrono::Duration::seconds(2)
            ),
            Err(LaunchApprovalError::Expired)
        );
    }

    #[test]
    fn edited_config_invalidates_stored_fingerprint() {
        let service = LaunchApprovalService::default();
        let now = Utc::now();
        let mut config = config();
        let preview = service.issue(&config, now).unwrap();
        service.confirm(&preview.token, &mut config, now).unwrap();
        if let McpTransportConfig::Stdio { command, .. } = &mut config.transport {
            *command = "other".to_string();
        }
        assert!(!LaunchApprovalService::is_approved(&config));
    }
}
