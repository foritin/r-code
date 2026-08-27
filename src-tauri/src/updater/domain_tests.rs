use std::sync::atomic::{AtomicI64, AtomicUsize};
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;

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
