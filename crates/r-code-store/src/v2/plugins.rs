//! V2 persistence for the plugin catalog and run pins.

use crate::v2::V2Store;
use rusqlite::params;

/// One catalog row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCatalogRecord {
    pub id: String,
    pub version: String,
    pub content_digest: String,
    pub enabled: bool,
    pub granted_services: Vec<String>,
    pub config: String,
    pub manifest_json: String,
    pub install_dir: String,
}

/// One run pin row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPinRecord {
    pub attempt_id: String,
    pub task_id: String,
    pub id: String,
    pub version: String,
    pub content_digest: String,
}

impl V2Store {
    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Register (or refresh) an installed package.
    pub fn register_plugin(&self, record: &PluginCatalogRecord) -> Result<(), rusqlite::Error> {
        self.connection().execute(
            "INSERT INTO plugin_catalog(
                id, version, content_digest, enabled, granted_services, config,
                manifest_json, install_dir, installed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id, version, content_digest) DO UPDATE SET
                granted_services = excluded.granted_services,
                config = excluded.config,
                manifest_json = excluded.manifest_json,
                install_dir = excluded.install_dir",
            params![
                record.id,
                record.version,
                record.content_digest,
                record.enabled as i64,
                serde_json::to_string(&record.granted_services).unwrap_or_else(|_| "[]".into()),
                record.config,
                record.manifest_json,
                record.install_dir,
                Self::now_ms(),
            ],
        )?;
        Ok(())
    }

    /// All catalog rows ordered by identity.
    pub fn list_plugins(&self) -> Result<Vec<PluginCatalogRecord>, rusqlite::Error> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, version, content_digest, enabled, granted_services, config,
                    manifest_json, install_dir
             FROM plugin_catalog ORDER BY id, version, content_digest",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(PluginCatalogRecord {
                id: row.get(0)?,
                version: row.get(1)?,
                content_digest: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                granted_services: serde_json::from_str(&row.get::<_, String>(4)?)
                    .unwrap_or_default(),
                config: row.get(5)?,
                manifest_json: row.get(6)?,
                install_dir: row.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// Enable or disable one installed package.
    pub fn set_plugin_enabled(
        &self,
        id: &str,
        content_digest: &str,
        enabled: bool,
    ) -> Result<bool, rusqlite::Error> {
        let changed = self.connection().execute(
            "UPDATE plugin_catalog SET enabled = ?3 WHERE id = ?1 AND content_digest = ?2",
            params![id, content_digest, enabled as i64],
        )?;
        Ok(changed > 0)
    }

    /// Remove a catalog row; callers must refuse while run pins reference it.
    pub fn remove_plugin(&self, id: &str, content_digest: &str) -> Result<bool, rusqlite::Error> {
        let changed = self.connection().execute(
            "DELETE FROM plugin_catalog WHERE id = ?1 AND content_digest = ?2",
            params![id, content_digest],
        )?;
        Ok(changed > 0)
    }

    /// Pin the exact package bytes an attempt runs with.
    pub fn pin_plugin(
        &self,
        attempt_id: &str,
        task_id: &str,
        id: &str,
        version: &str,
        content_digest: &str,
    ) -> Result<(), rusqlite::Error> {
        self.connection().execute(
            "INSERT OR REPLACE INTO plugin_pins(
                attempt_id, task_id, id, version, content_digest, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                attempt_id,
                task_id,
                id,
                version,
                content_digest,
                Self::now_ms()
            ],
        )?;
        Ok(())
    }

    /// All pins referencing one package identity.
    pub fn plugin_pins_for(
        &self,
        id: &str,
        content_digest: &str,
    ) -> Result<Vec<PluginPinRecord>, rusqlite::Error> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT attempt_id, task_id, id, version, content_digest FROM plugin_pins
             WHERE id = ?1 AND content_digest = ?2",
        )?;
        let rows = statement.query_map(params![id, content_digest], |row| {
            Ok(PluginPinRecord {
                attempt_id: row.get(0)?,
                task_id: row.get(1)?,
                id: row.get(2)?,
                version: row.get(3)?,
                content_digest: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// The pin recorded for one attempt.
    pub fn plugin_pin_for_attempt(
        &self,
        attempt_id: &str,
    ) -> Result<Option<PluginPinRecord>, rusqlite::Error> {
        use rusqlite::OptionalExtension;
        self.connection()
            .query_row(
                "SELECT attempt_id, task_id, id, version, content_digest FROM plugin_pins
                 WHERE attempt_id = ?1",
                params![attempt_id],
                |row| {
                    Ok(PluginPinRecord {
                        attempt_id: row.get(0)?,
                        task_id: row.get(1)?,
                        id: row.get(2)?,
                        version: row.get(3)?,
                        content_digest: row.get(4)?,
                    })
                },
            )
            .optional()
    }
}
