use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use r_code_core::UserFacingError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{watch, Mutex};

pub const AUTO_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const PERSISTED_STATE_SCHEMA_VERSION: u32 = 1;

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
    const fn as_str(self) -> &'static str {
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
    fn from_download(downloaded_bytes: u64, total_bytes: Option<u64>) -> Self {
        let percent = total_bytes
            .filter(|total| *total > 0)
            .map(|total| ((downloaded_bytes.saturating_mul(100) / total).min(100)) as u8);
        Self {
            downloaded_bytes,
            total_bytes,
            percent,
        }
    }

    fn completed(downloaded_bytes: u64, reported_total: Option<u64>) -> Self {
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
    fn new(current_version: String, last_check_at: Option<DateTime<Utc>>) -> Self {
        Self {
            current_version,
            state: UpdaterPhase::Idle,
            release: None,
            progress: UpdaterDownloadProgress::default(),
            last_check_at,
            next_auto_check_at: last_check_at.map(next_check_at),
            error_code: None,
            error_args: BTreeMap::new(),
            failed_operation: None,
        }
    }

    fn clear_error(&mut self) {
        self.error_code = None;
        self.error_args.clear();
        self.failed_operation = None;
    }

    fn can_check(&self) -> bool {
        matches!(
            self.state,
            UpdaterPhase::Idle
                | UpdaterPhase::UpToDate
                | UpdaterPhase::Available
                | UpdaterPhase::Failed
        )
    }

    fn can_download(&self) -> bool {
        self.state == UpdaterPhase::Available
            || (self.state == UpdaterPhase::Failed
                && self.failed_operation == Some(UpdaterOperation::Download)
                && self.release.is_some())
    }

    fn can_install(&self) -> bool {
        self.state == UpdaterPhase::Downloaded
            || (self.state == UpdaterPhase::Failed
                && self.failed_operation == Some(UpdaterOperation::Install)
                && self.release.is_some()
                && self.progress.percent == Some(100))
    }
}

pub trait UpdaterClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default)]
pub struct SystemUpdaterClock;

impl UpdaterClock for SystemUpdaterClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

pub trait UpdaterStateStore: Send + Sync {
    fn load_last_check(&self) -> Result<Option<DateTime<Utc>>, String>;
    fn save_last_check(&self, checked_at: DateTime<Utc>) -> Result<(), String>;
}

#[derive(Debug, Clone)]
pub struct FileUpdaterStateStore {
    path: PathBuf,
}

impl FileUpdaterStateStore {
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            path: config_dir.into().join("application-updater.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedUpdaterState {
    schema_version: u32,
    last_check_at: DateTime<Utc>,
}

impl UpdaterStateStore for FileUpdaterStateStore {
    fn load_last_check(&self) -> Result<Option<DateTime<Utc>>, String> {
        if !self.path.exists() {
            return Ok(None);
        }
        let content = std::fs::read(&self.path)
            .map_err(|error| format!("read {}: {error}", self.path.display()))?;
        let persisted: PersistedUpdaterState = serde_json::from_slice(&content)
            .map_err(|error| format!("parse {}: {error}", self.path.display()))?;
        if persisted.schema_version != PERSISTED_STATE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported updater state schema {}",
                persisted.schema_version
            ));
        }
        Ok(Some(persisted.last_check_at))
    }

    fn save_last_check(&self, checked_at: DateTime<Utc>) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "updater state path has no parent directory".to_string())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
        let payload = serde_json::to_vec_pretty(&PersistedUpdaterState {
            schema_version: PERSISTED_STATE_SCHEMA_VERSION,
            last_check_at: checked_at,
        })
        .map_err(|error| format!("serialize updater state: {error}"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| format!("create updater state temp file: {error}"))?;
        temporary
            .write_all(&payload)
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|error| format!("write updater state: {error}"))?;
        temporary
            .persist(&self.path)
            .map_err(|error| format!("persist {}: {}", self.path.display(), error.error))?;
        Ok(())
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

pub struct ApplicationUpdater {
    backend: Arc<dyn UpdaterBackend>,
    clock: Arc<dyn UpdaterClock>,
    store: Arc<dyn UpdaterStateStore>,
    snapshot: Arc<RwLock<UpdaterSnapshot>>,
    updates: watch::Sender<UpdaterSnapshot>,
    operation: Mutex<()>,
}

impl ApplicationUpdater {
    pub fn new(
        current_version: impl Into<String>,
        backend: Arc<dyn UpdaterBackend>,
        clock: Arc<dyn UpdaterClock>,
        store: Arc<dyn UpdaterStateStore>,
    ) -> Self {
        let mut snapshot = match store.load_last_check() {
            Ok(last_check_at) => UpdaterSnapshot::new(current_version.into(), last_check_at),
            Err(detail) => {
                tracing::warn!(%detail, "failed to load persisted updater rate limit");
                let mut snapshot = UpdaterSnapshot::new(current_version.into(), None);
                snapshot.state = UpdaterPhase::Failed;
                snapshot.error_code = Some("updater.persistence_failed".to_string());
                snapshot.failed_operation = Some(UpdaterOperation::Check);
                snapshot
            }
        };
        snapshot.next_auto_check_at = snapshot.last_check_at.map(next_check_at);
        let (updates, _) = watch::channel(snapshot.clone());
        Self {
            backend,
            clock,
            store,
            snapshot: Arc::new(RwLock::new(snapshot)),
            updates,
            operation: Mutex::new(()),
        }
    }

    pub fn status(&self) -> UpdaterSnapshot {
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<UpdaterSnapshot> {
        self.updates.subscribe()
    }

    pub fn auto_check_delay(&self) -> Duration {
        let snapshot = self.status();
        if !snapshot.can_check() {
            return AUTO_CHECK_INTERVAL;
        }
        let Some(last_check_at) = snapshot.last_check_at else {
            return Duration::ZERO;
        };
        let now = self.clock.now();
        if last_check_at > now {
            return Duration::ZERO;
        }
        let remaining = next_check_at(last_check_at).signed_duration_since(now);
        remaining.to_std().unwrap_or(Duration::ZERO)
    }

    pub async fn check(&self, force: bool) -> Result<UpdaterSnapshot, UserFacingError> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| operation_in_progress())?;
        let current = self.status();
        if !current.can_check() {
            return Err(invalid_state(UpdaterOperation::Check, current.state));
        }

        let checked_at = self.clock.now();
        if !force && automatic_check_is_limited(current.last_check_at, checked_at) {
            return Ok(current);
        }

        self.mutate(|snapshot| {
            snapshot.state = UpdaterPhase::Checking;
            snapshot.clear_error();
        });

        if let Err(detail) = self.store.save_last_check(checked_at) {
            let error =
                UserFacingError::new("updater.persistence_failed").with_debug_detail(detail);
            self.publish_failure(UpdaterOperation::Check, &error);
            return Err(error);
        }
        self.mutate(|snapshot| {
            snapshot.last_check_at = Some(checked_at);
            snapshot.next_auto_check_at = Some(next_check_at(checked_at));
        });

        match self.backend.check().await {
            Ok(Some(release)) => Ok(self.mutate(|snapshot| {
                snapshot.state = UpdaterPhase::Available;
                snapshot.release = Some(release);
                snapshot.progress = UpdaterDownloadProgress::default();
                snapshot.clear_error();
            })),
            Ok(None) => Ok(self.mutate(|snapshot| {
                snapshot.state = UpdaterPhase::UpToDate;
                snapshot.release = None;
                snapshot.progress = UpdaterDownloadProgress::default();
                snapshot.clear_error();
            })),
            Err(backend_error) => {
                let error = user_error(UpdaterOperation::Check, backend_error);
                self.publish_failure(UpdaterOperation::Check, &error);
                Err(error)
            }
        }
    }

    pub async fn download(&self) -> Result<UpdaterSnapshot, UserFacingError> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| operation_in_progress())?;
        let current = self.status();
        if !current.can_download() {
            return Err(invalid_state(UpdaterOperation::Download, current.state));
        }
        let version = current
            .release
            .as_ref()
            .map(|release| release.version.clone());
        self.mutate(|snapshot| {
            snapshot.state = UpdaterPhase::Downloading;
            snapshot.progress = UpdaterDownloadProgress::default();
            snapshot.clear_error();
        });

        let downloaded = Arc::new(AtomicU64::new(0));
        let progress_snapshot = self.snapshot.clone();
        let progress_updates = self.updates.clone();
        let progress_downloaded = downloaded.clone();
        let progress: UpdaterProgressCallback = Arc::new(move |chunk, total| {
            let downloaded_bytes = progress_downloaded
                .fetch_add(chunk as u64, Ordering::Relaxed)
                .saturating_add(chunk as u64);
            mutate_and_broadcast(&progress_snapshot, &progress_updates, |snapshot| {
                snapshot.progress = UpdaterDownloadProgress::from_download(downloaded_bytes, total);
            });
        });

        match self.backend.download(progress).await {
            Ok(artifact) => Ok(self.mutate(|snapshot| {
                snapshot.state = UpdaterPhase::Downloaded;
                snapshot.progress = UpdaterDownloadProgress::completed(
                    artifact.bytes,
                    snapshot.progress.total_bytes,
                );
                snapshot.clear_error();
            })),
            Err(backend_error) => {
                let mut error = user_error(UpdaterOperation::Download, backend_error);
                if let Some(version) = version {
                    error.args.insert("version".to_string(), version.into());
                }
                self.publish_failure(UpdaterOperation::Download, &error);
                Err(error)
            }
        }
    }

    pub async fn install(&self) -> Result<UpdaterSnapshot, UserFacingError> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| operation_in_progress())?;
        let current = self.status();
        if !current.can_install() {
            return Err(invalid_state(UpdaterOperation::Install, current.state));
        }
        let version = current
            .release
            .as_ref()
            .map(|release| release.version.clone());
        self.mutate(|snapshot| {
            snapshot.state = UpdaterPhase::Installing;
            snapshot.clear_error();
        });

        match self.backend.install().await {
            Ok(()) => Ok(self.mutate(|snapshot| {
                snapshot.state = UpdaterPhase::RestartPending;
                snapshot.clear_error();
            })),
            Err(backend_error) => {
                let mut error = user_error(UpdaterOperation::Install, backend_error);
                if let Some(version) = version {
                    error.args.insert("version".to_string(), version.into());
                }
                self.publish_failure(UpdaterOperation::Install, &error);
                Err(error)
            }
        }
    }

    pub async fn restart(&self) -> Result<(), UserFacingError> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| operation_in_progress())?;
        let current = self.status();
        if current.state != UpdaterPhase::RestartPending
            && !(current.state == UpdaterPhase::Failed
                && current.failed_operation == Some(UpdaterOperation::Restart))
        {
            return Err(invalid_state(UpdaterOperation::Restart, current.state));
        }
        if let Err(backend_error) = self.backend.restart().await {
            let error = user_error(UpdaterOperation::Restart, backend_error);
            self.publish_failure(UpdaterOperation::Restart, &error);
            return Err(error);
        }
        Ok(())
    }

    fn mutate(&self, update: impl FnOnce(&mut UpdaterSnapshot)) -> UpdaterSnapshot {
        mutate_and_broadcast(&self.snapshot, &self.updates, update)
    }

    fn publish_failure(&self, operation: UpdaterOperation, error: &UserFacingError) {
        self.mutate(|snapshot| {
            snapshot.state = UpdaterPhase::Failed;
            snapshot.error_code = Some(error.code.clone());
            snapshot.error_args = error.args.clone();
            snapshot.failed_operation = Some(operation);
        });
    }
}

fn mutate_and_broadcast(
    snapshot: &Arc<RwLock<UpdaterSnapshot>>,
    updates: &watch::Sender<UpdaterSnapshot>,
    update: impl FnOnce(&mut UpdaterSnapshot),
) -> UpdaterSnapshot {
    let next = {
        let mut snapshot = snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut snapshot);
        snapshot.clone()
    };
    updates.send_replace(next.clone());
    next
}

fn automatic_check_is_limited(last_check_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    let Some(last_check_at) = last_check_at else {
        return false;
    };
    last_check_at <= now && now < next_check_at(last_check_at)
}

fn next_check_at(checked_at: DateTime<Utc>) -> DateTime<Utc> {
    checked_at
        + chrono::Duration::from_std(AUTO_CHECK_INTERVAL)
            .expect("six-hour updater interval must fit chrono::Duration")
}

fn operation_in_progress() -> UserFacingError {
    UserFacingError::new("updater.operation_in_progress")
}

fn invalid_state(operation: UpdaterOperation, state: UpdaterPhase) -> UserFacingError {
    UserFacingError::new("updater.invalid_state")
        .with_arg("operation", operation.as_str())
        .with_arg("state", state.as_str())
}

fn user_error(operation: UpdaterOperation, backend_error: UpdaterBackendError) -> UserFacingError {
    let code = if backend_error.kind == UpdaterBackendErrorKind::SignatureInvalid {
        "updater.signature_invalid"
    } else {
        match operation {
            UpdaterOperation::Check => "updater.check_failed",
            UpdaterOperation::Download => "updater.download_failed",
            UpdaterOperation::Install => "updater.install_failed",
            UpdaterOperation::Restart => "updater.restart_failed",
        }
    };
    UserFacingError::new(code)
        .with_arg("operation", operation.as_str())
        .with_debug_detail(backend_error.detail)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, AtomicUsize};
    use std::sync::Mutex as StdMutex;

    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).expect("valid test timestamp")
    }

    #[derive(Default)]
    struct MemoryStore {
        last_check_at: StdMutex<Option<DateTime<Utc>>>,
    }

    impl UpdaterStateStore for MemoryStore {
        fn load_last_check(&self) -> Result<Option<DateTime<Utc>>, String> {
            Ok(*self.last_check_at.lock().expect("memory store poisoned"))
        }

        fn save_last_check(&self, checked_at: DateTime<Utc>) -> Result<(), String> {
            *self.last_check_at.lock().expect("memory store poisoned") = Some(checked_at);
            Ok(())
        }
    }

    struct FakeClock(AtomicI64);

    impl FakeClock {
        fn new(now: DateTime<Utc>) -> Self {
            Self(AtomicI64::new(now.timestamp()))
        }

        fn advance(&self, duration: Duration) {
            self.0
                .fetch_add(duration.as_secs() as i64, Ordering::Relaxed);
        }
    }

    impl UpdaterClock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            at(self.0.load(Ordering::Relaxed))
        }
    }

    struct FakeBackend {
        check_result: StdMutex<Result<Option<UpdaterRelease>, UpdaterBackendError>>,
        download_result: StdMutex<Result<DownloadedUpdate, UpdaterBackendError>>,
        install_result: StdMutex<Result<(), UpdaterBackendError>>,
        restart_result: StdMutex<Result<(), UpdaterBackendError>>,
        checks: AtomicUsize,
        downloads: AtomicUsize,
        installs: AtomicUsize,
        restarts: AtomicUsize,
    }

    impl FakeBackend {
        fn new(check_result: Result<Option<UpdaterRelease>, UpdaterBackendError>) -> Self {
            Self {
                check_result: StdMutex::new(check_result),
                download_result: StdMutex::new(Ok(DownloadedUpdate { bytes: 100 })),
                install_result: StdMutex::new(Ok(())),
                restart_result: StdMutex::new(Ok(())),
                checks: AtomicUsize::new(0),
                downloads: AtomicUsize::new(0),
                installs: AtomicUsize::new(0),
                restarts: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl UpdaterBackend for FakeBackend {
        async fn check(&self) -> Result<Option<UpdaterRelease>, UpdaterBackendError> {
            self.checks.fetch_add(1, Ordering::Relaxed);
            self.check_result
                .lock()
                .expect("fake check result poisoned")
                .clone()
        }

        async fn download(
            &self,
            progress: UpdaterProgressCallback,
        ) -> Result<DownloadedUpdate, UpdaterBackendError> {
            self.downloads.fetch_add(1, Ordering::Relaxed);
            let result = self
                .download_result
                .lock()
                .expect("fake download result poisoned")
                .clone();
            if result.is_ok() {
                progress(40, Some(100));
                progress(60, Some(100));
            }
            result
        }

        async fn install(&self) -> Result<(), UpdaterBackendError> {
            self.installs.fetch_add(1, Ordering::Relaxed);
            self.install_result
                .lock()
                .expect("fake install result poisoned")
                .clone()
        }

        async fn restart(&self) -> Result<(), UpdaterBackendError> {
            self.restarts.fetch_add(1, Ordering::Relaxed);
            self.restart_result
                .lock()
                .expect("fake restart result poisoned")
                .clone()
        }
    }

    fn release() -> UpdaterRelease {
        UpdaterRelease {
            version: "1.1.0".to_string(),
            notes: Some("Safer updates".to_string()),
            published_at: Some("2026-08-26T00:00:00Z".to_string()),
        }
    }

    fn updater(
        backend: Arc<FakeBackend>,
        clock: Arc<FakeClock>,
        store: Arc<MemoryStore>,
    ) -> ApplicationUpdater {
        ApplicationUpdater::new("1.0.0", backend, clock, store)
    }

    #[tokio::test]
    async fn no_update_and_available_results_are_distinct_stable_states() {
        let clock = Arc::new(FakeClock::new(at(1_800_000_000)));
        let no_update_backend = Arc::new(FakeBackend::new(Ok(None)));
        let no_update = updater(
            no_update_backend.clone(),
            clock.clone(),
            Arc::new(MemoryStore::default()),
        );
        let snapshot = no_update.check(false).await.expect("check with no update");
        assert_eq!(snapshot.state, UpdaterPhase::UpToDate);
        assert!(snapshot.release.is_none());
        assert_eq!(no_update_backend.checks.load(Ordering::Relaxed), 1);

        let available_backend = Arc::new(FakeBackend::new(Ok(Some(release()))));
        let available = updater(available_backend, clock, Arc::new(MemoryStore::default()));
        let snapshot = available.check(false).await.expect("check with update");
        assert_eq!(snapshot.state, UpdaterPhase::Available);
        assert_eq!(snapshot.release, Some(release()));
    }

    #[tokio::test]
    async fn automatic_checks_are_rate_limited_across_service_restarts() {
        let clock = Arc::new(FakeClock::new(at(1_800_000_000)));
        let store = Arc::new(MemoryStore::default());
        let first_backend = Arc::new(FakeBackend::new(Ok(None)));
        updater(first_backend.clone(), clock.clone(), store.clone())
            .check(false)
            .await
            .expect("first automatic check");
        assert_eq!(first_backend.checks.load(Ordering::Relaxed), 1);

        let restarted_backend = Arc::new(FakeBackend::new(Ok(None)));
        let restarted = updater(restarted_backend.clone(), clock.clone(), store);
        restarted
            .check(false)
            .await
            .expect("rate-limited startup check");
        assert_eq!(restarted_backend.checks.load(Ordering::Relaxed), 0);

        clock.advance(AUTO_CHECK_INTERVAL);
        restarted
            .check(false)
            .await
            .expect("automatic check after six hours");
        assert_eq!(restarted_backend.checks.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn forced_manual_check_bypasses_the_persisted_rate_limit() {
        let now = at(1_800_000_000);
        let clock = Arc::new(FakeClock::new(now));
        let store = Arc::new(MemoryStore {
            last_check_at: StdMutex::new(Some(now)),
        });
        let backend = Arc::new(FakeBackend::new(Ok(None)));
        let updater = updater(backend.clone(), clock, store);

        updater.check(false).await.expect("limited automatic check");
        assert_eq!(backend.checks.load(Ordering::Relaxed), 0);
        updater.check(true).await.expect("forced manual check");
        assert_eq!(backend.checks.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn network_and_signature_errors_use_stable_user_facing_codes() {
        let network_backend = Arc::new(FakeBackend::new(Err(UpdaterBackendError::new(
            UpdaterBackendErrorKind::Network,
            "connection reset",
        ))));
        let network = updater(
            network_backend,
            Arc::new(FakeClock::new(at(1_800_000_000))),
            Arc::new(MemoryStore::default()),
        );
        let error = network.check(true).await.expect_err("network check fails");
        assert_eq!(error.code, "updater.check_failed");
        assert_eq!(network.status().state, UpdaterPhase::Failed);

        let signature_backend = Arc::new(FakeBackend::new(Ok(Some(release()))));
        *signature_backend
            .download_result
            .lock()
            .expect("fake download result poisoned") = Err(UpdaterBackendError::new(
            UpdaterBackendErrorKind::SignatureInvalid,
            "invalid minisign payload",
        ));
        let signature = updater(
            signature_backend,
            Arc::new(FakeClock::new(at(1_800_000_000))),
            Arc::new(MemoryStore::default()),
        );
        signature.check(true).await.expect("available update");
        let error = signature
            .download()
            .await
            .expect_err("signature verification must fail closed");
        assert_eq!(error.code, "updater.signature_invalid");
        assert!(!error.to_string().contains("minisign"));
        assert_eq!(
            signature.status().error_code.as_deref(),
            Some(error.code.as_str())
        );
    }

    #[tokio::test]
    async fn download_install_and_restart_remain_three_explicit_actions() {
        let backend = Arc::new(FakeBackend::new(Ok(Some(release()))));
        let updater = updater(
            backend.clone(),
            Arc::new(FakeClock::new(at(1_800_000_000))),
            Arc::new(MemoryStore::default()),
        );
        updater.check(true).await.expect("available update");

        let downloaded = updater.download().await.expect("download update");
        assert_eq!(downloaded.state, UpdaterPhase::Downloaded);
        assert_eq!(downloaded.progress.downloaded_bytes, 100);
        assert_eq!(downloaded.progress.total_bytes, Some(100));
        assert_eq!(downloaded.progress.percent, Some(100));
        assert_eq!(backend.installs.load(Ordering::Relaxed), 0);
        assert_eq!(backend.restarts.load(Ordering::Relaxed), 0);

        let installed = updater.install().await.expect("install update");
        assert_eq!(installed.state, UpdaterPhase::RestartPending);
        assert_eq!(backend.installs.load(Ordering::Relaxed), 1);
        assert_eq!(backend.restarts.load(Ordering::Relaxed), 0);

        updater.restart().await.expect("explicit restart");
        assert_eq!(backend.restarts.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn install_failure_preserves_download_for_an_explicit_retry() {
        let backend = Arc::new(FakeBackend::new(Ok(Some(release()))));
        *backend
            .install_result
            .lock()
            .expect("fake install result poisoned") = Err(UpdaterBackendError::new(
            UpdaterBackendErrorKind::Other,
            "installer was cancelled",
        ));
        let updater = updater(
            backend.clone(),
            Arc::new(FakeClock::new(at(1_800_000_000))),
            Arc::new(MemoryStore::default()),
        );
        updater.check(true).await.expect("available update");
        updater.download().await.expect("download update");
        let error = updater.install().await.expect_err("install should fail");
        assert_eq!(error.code, "updater.install_failed");
        assert_eq!(updater.status().progress.percent, Some(100));

        *backend
            .install_result
            .lock()
            .expect("fake install result poisoned") = Ok(());
        let retried = updater.install().await.expect("retry preserved download");
        assert_eq!(retried.state, UpdaterPhase::RestartPending);
    }

    #[test]
    fn file_store_round_trips_the_cross_restart_timestamp() {
        let directory = tempfile::tempdir().expect("create updater store temp directory");
        let store = FileUpdaterStateStore::new(directory.path());
        let checked_at = at(1_800_000_000);

        assert_eq!(store.load_last_check().expect("load empty store"), None);
        store
            .save_last_check(checked_at)
            .expect("persist updater timestamp");
        assert_eq!(
            FileUpdaterStateStore::new(directory.path())
                .load_last_check()
                .expect("reload updater timestamp"),
            Some(checked_at)
        );
    }
}
