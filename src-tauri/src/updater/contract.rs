use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type UpdaterProgressCallback = Arc<dyn Fn(usize, Option<u64>) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdaterPhase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Downloaded,
    Installing,
    RestartPending,
    Failed,
}

impl UpdaterPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Checking => "checking",
            Self::UpToDate => "up_to_date",
            Self::Available => "available",
            Self::Downloading => "downloading",
            Self::Downloaded => "downloaded",
            Self::Installing => "installing",
            Self::RestartPending => "restart_pending",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdaterOperation {
    Check,
    Download,
    Install,
    Restart,
}

impl UpdaterOperation {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Download => "download",
            Self::Install => "install",
            Self::Restart => "restart",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdaterRelease {
    pub version: String,
    pub notes: Option<String>,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdaterDownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percent: Option<u8>,
}

impl UpdaterDownloadProgress {
    pub(super) fn from_download(downloaded_bytes: u64, total_bytes: Option<u64>) -> Self {
        let percent = total_bytes
            .filter(|total| *total > 0)
            .map(|total| ((downloaded_bytes.saturating_mul(100) / total).min(100)) as u8);
        Self {
            downloaded_bytes,
            total_bytes,
            percent,
        }
    }

    pub(super) fn completed(downloaded_bytes: u64, reported_total: Option<u64>) -> Self {
        Self {
            downloaded_bytes,
            total_bytes: reported_total.or(Some(downloaded_bytes)),
            percent: Some(100),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdaterSnapshot {
    pub current_version: String,
    pub state: UpdaterPhase,
    pub release: Option<UpdaterRelease>,
    pub progress: UpdaterDownloadProgress,
    pub last_check_at: Option<DateTime<Utc>>,
    pub next_auto_check_at: Option<DateTime<Utc>>,
    pub error_code: Option<String>,
    #[serde(default)]
    pub error_args: BTreeMap<String, Value>,
    pub failed_operation: Option<UpdaterOperation>,
}

impl UpdaterSnapshot {
    pub(super) fn new(current_version: String, last_check_at: Option<DateTime<Utc>>) -> Self {
        Self {
            current_version,
            state: UpdaterPhase::Idle,
            release: None,
            progress: UpdaterDownloadProgress::default(),
            last_check_at,
            next_auto_check_at: None,
            error_code: None,
            error_args: BTreeMap::new(),
            failed_operation: None,
        }
    }

    pub(super) fn clear_error(&mut self) {
        self.error_code = None;
        self.error_args.clear();
        self.failed_operation = None;
    }

    pub(super) fn can_check(&self) -> bool {
        matches!(
            self.state,
            UpdaterPhase::Idle
                | UpdaterPhase::UpToDate
                | UpdaterPhase::Available
                | UpdaterPhase::Failed
        )
    }

    pub(super) fn can_download(&self) -> bool {
        self.state == UpdaterPhase::Available
            || (self.state == UpdaterPhase::Failed
                && self.failed_operation == Some(UpdaterOperation::Download)
                && self.release.is_some())
    }

    pub(super) fn can_install(&self) -> bool {
        self.state == UpdaterPhase::Downloaded
            || (self.state == UpdaterPhase::Failed
                && self.failed_operation == Some(UpdaterOperation::Install)
                && self.release.is_some()
                && self.progress.percent == Some(100))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdaterBackendErrorKind {
    SignatureInvalid,
    Network,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterBackendError {
    pub kind: UpdaterBackendErrorKind,
    pub detail: String,
}

impl UpdaterBackendError {
    pub fn new(kind: UpdaterBackendErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadedUpdate {
    pub bytes: u64,
}

#[async_trait]
pub trait UpdaterBackend: Send + Sync {
    async fn check(&self) -> Result<Option<UpdaterRelease>, UpdaterBackendError>;

    async fn download(
        &self,
        progress: UpdaterProgressCallback,
    ) -> Result<DownloadedUpdate, UpdaterBackendError>;

    async fn install(&self) -> Result<(), UpdaterBackendError>;

    async fn restart(&self) -> Result<(), UpdaterBackendError>;
}
