use std::sync::Arc;

use async_trait::async_trait;
use tauri::AppHandle;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

use super::domain::{
    DownloadedUpdate, UpdaterBackend, UpdaterBackendError, UpdaterBackendErrorKind,
    UpdaterProgressCallback, UpdaterRelease,
};

pub struct TauriUpdaterBackend {
    app: AppHandle,
    checked_update: Mutex<Option<Update>>,
    downloaded: Mutex<Option<Arc<[u8]>>>,
}

impl TauriUpdaterBackend {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            checked_update: Mutex::new(None),
            downloaded: Mutex::new(None),
        }
    }
}

#[async_trait]
impl UpdaterBackend for TauriUpdaterBackend {
    async fn check(&self) -> Result<Option<UpdaterRelease>, UpdaterBackendError> {
        let updater = self.app.updater().map_err(map_tauri_updater_error)?;
        let update = updater.check().await.map_err(map_tauri_updater_error)?;
        let release = update.as_ref().map(|update| UpdaterRelease {
            version: update.version.clone(),
            notes: update.body.clone(),
            published_at: update
                .raw_json
                .get("pub_date")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| update.date.map(|date| date.to_string())),
        });
        *self.checked_update.lock().await = update;
        *self.downloaded.lock().await = None;
        Ok(release)
    }

    async fn download(
        &self,
        progress: UpdaterProgressCallback,
    ) -> Result<DownloadedUpdate, UpdaterBackendError> {
        let update = self
            .checked_update
            .lock()
            .await
            .clone()
            .ok_or_else(|| backend_state_error("no checked update is available"))?;
        let bytes = update
            .download(move |chunk, total| progress(chunk, total), || {})
            .await
            .map_err(map_tauri_updater_error)?;
        let bytes: Arc<[u8]> = bytes.into();
        let downloaded = DownloadedUpdate {
            bytes: bytes.len() as u64,
        };
        *self.downloaded.lock().await = Some(bytes);
        Ok(downloaded)
    }

    async fn install(&self) -> Result<(), UpdaterBackendError> {
        let update = self
            .checked_update
            .lock()
            .await
            .clone()
            .ok_or_else(|| backend_state_error("no checked update is available"))?;
        let bytes = self
            .downloaded
            .lock()
            .await
            .clone()
            .ok_or_else(|| backend_state_error("no verified updater payload is available"))?;
        tauri::async_runtime::spawn_blocking(move || update.install(bytes.as_ref()))
            .await
            .map_err(|error| {
                UpdaterBackendError::new(
                    UpdaterBackendErrorKind::Other,
                    format!("updater installer task failed: {error}"),
                )
            })?
            .map_err(map_tauri_updater_error)
    }

    async fn restart(&self) -> Result<(), UpdaterBackendError> {
        self.app.restart()
    }
}

fn backend_state_error(detail: impl Into<String>) -> UpdaterBackendError {
    UpdaterBackendError::new(UpdaterBackendErrorKind::Other, detail)
}

fn map_tauri_updater_error(error: tauri_plugin_updater::Error) -> UpdaterBackendError {
    let kind = match &error {
        tauri_plugin_updater::Error::Minisign(_)
        | tauri_plugin_updater::Error::Base64(_)
        | tauri_plugin_updater::Error::SignatureUtf8(_) => {
            UpdaterBackendErrorKind::SignatureInvalid
        }
        tauri_plugin_updater::Error::Reqwest(_)
        | tauri_plugin_updater::Error::Network(_)
        | tauri_plugin_updater::Error::ReleaseNotFound => UpdaterBackendErrorKind::Network,
        _ => UpdaterBackendErrorKind::Other,
    };
    UpdaterBackendError::new(kind, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_decoding_errors_are_classified_separately_from_network_errors() {
        let signature = map_tauri_updater_error(tauri_plugin_updater::Error::SignatureUtf8(
            "not utf-8".to_string(),
        ));
        assert_eq!(signature.kind, UpdaterBackendErrorKind::SignatureInvalid);

        let network = map_tauri_updater_error(tauri_plugin_updater::Error::Network(
            "connection reset".to_string(),
        ));
        assert_eq!(network.kind, UpdaterBackendErrorKind::Network);

        let install = map_tauri_updater_error(tauri_plugin_updater::Error::PackageInstallFailed);
        assert_eq!(install.kind, UpdaterBackendErrorKind::Other);
    }
}
