//! Product-owned application updater orchestration.
//!
//! The signed endpoint and public key remain owned by Tauri configuration. This module adds the
//! product state machine, cross-restart automatic-check throttling, explicit user actions, stable
//! errors, and one observable DTO shared by Settings and startup coordination.

mod domain;
mod tauri_backend;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use r_code_core::UserFacingError;
use tauri::{AppHandle, Emitter};

pub use domain::{
    ApplicationUpdater, DownloadedUpdate, FileUpdaterStateStore, SystemUpdaterClock,
    UpdaterBackend, UpdaterBackendError, UpdaterBackendErrorKind, UpdaterClock,
    UpdaterDownloadProgress, UpdaterOperation, UpdaterPhase, UpdaterProgressCallback,
    UpdaterRelease, UpdaterSnapshot, UpdaterStateStore, AUTO_CHECK_INTERVAL,
};
pub use tauri_backend::TauriUpdaterBackend;

pub const UPDATER_STATE_EVENT: &str = "application-updater-state";
const AUTO_CHECK_FAILURE_RETRY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct ApplicationUpdaterState {
    updater: Arc<ApplicationUpdater>,
}

impl ApplicationUpdaterState {
    pub fn new(app: AppHandle, config_dir: PathBuf) -> Self {
        let current_version = app.package_info().version.to_string();
        let updater = ApplicationUpdater::new(
            current_version,
            Arc::new(TauriUpdaterBackend::new(app)),
            Arc::new(SystemUpdaterClock),
            Arc::new(FileUpdaterStateStore::new(config_dir)),
        );
        Self {
            updater: Arc::new(updater),
        }
    }

    pub fn status(&self) -> UpdaterSnapshot {
        self.updater.status()
    }

    pub async fn check(&self, force: bool) -> Result<UpdaterSnapshot, UserFacingError> {
        self.updater.check(force).await
    }

    pub async fn download(&self) -> Result<UpdaterSnapshot, UserFacingError> {
        self.updater.download().await
    }

    pub async fn install(&self) -> Result<UpdaterSnapshot, UserFacingError> {
        self.updater.install().await
    }

    pub async fn restart(&self) -> Result<(), UserFacingError> {
        self.updater.restart().await
    }
}

/// Start the only automatic updater path. It performs a rate-limited metadata check and never
/// calls download, install, or restart. `AppHandle::updater()` therefore continues to use the
/// flavor-specific endpoint already applied to Tauri configuration before Builder startup.
pub fn start_application_updater(app: AppHandle, state: ApplicationUpdaterState) {
    let mut updates = state.updater.subscribe();
    let event_app = app.clone();
    tauri::async_runtime::spawn(async move {
        while updates.changed().await.is_ok() {
            let snapshot = updates.borrow_and_update().clone();
            if let Err(error) = event_app.emit(UPDATER_STATE_EVENT, snapshot) {
                tracing::warn!(%error, "failed to emit application updater state");
            }
        }
    });

    tauri::async_runtime::spawn(async move {
        loop {
            let delay = state.updater.auto_check_delay();
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if let Err(error) = state.check(false).await {
                tracing::warn!(code = %error.code, "automatic application update check failed");
                tokio::time::sleep(AUTO_CHECK_FAILURE_RETRY).await;
            }
        }
    });
}
