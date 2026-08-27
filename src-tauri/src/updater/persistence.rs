use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const AUTO_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const PERSISTED_STATE_SCHEMA_VERSION: u32 = 1;

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
