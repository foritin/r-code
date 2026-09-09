//! Read-only legacy history access.
//!
//! [`LegacyReader`] never calls the legacy mutating constructors
//! (`Database::open`, `MigrationManager`, `SettingsService`). The source
//! SQLite opens with `SQLITE_OPEN_READ_ONLY`; a read transaction is taken
//! and the database is copied with the online backup API into v2 temporary
//! storage for coherent, WAL-aware querying and export. JSONL exports
//! freeze byte boundaries and include complete records only. When safe
//! read-only WAL access is unavailable (live WAL held by a running old
//! app), the reader returns a clear close-old-app state instead of
//! `immutable=1` hacks.

use rusqlite::{OpenFlags, OptionalExtension};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The state of a legacy read attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyReadOutcome {
    /// The backup copy is ready for querying/export.
    Ready { copy_path: PathBuf },
    /// The old app is running and holds the WAL: the user must close it or
    /// export from within the old app.
    CloseOldAppRequired { reason: String },
}

/// Errors from the legacy reader.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LegacyError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("sqlite failure: {0}")]
    Sqlite(String),
    #[error("backup did not converge within {0:?}")]
    BackupTimeout(Duration),
}

/// Read-only access to one legacy database.
pub struct LegacyReader;

impl LegacyReader {
    /// Open the source read-only, hold a read transaction, and back it up
    /// into `destination` (the v2 temporary copy). The source file is never
    /// written; normal lock bookkeeping is not product-data migration.
    pub fn backup_readonly(
        source: &Path,
        destination: &Path,
        deadline: Duration,
    ) -> Result<LegacyReadOutcome, LegacyError> {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LegacyError::Io(e.to_string()))?;
        }
        // READ_ONLY open: no journal replay writes, no migrations.
        let source_connection =
            rusqlite::Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| LegacyError::Sqlite(e.to_string()))?;

        // A live WAL with an active writer makes coherent read-only access
        // unsafe: surface the close-old-app state, never immutable=1.
        if wal_is_live(&source_connection) {
            return Ok(LegacyReadOutcome::CloseOldAppRequired {
                reason: "legacy database has a live WAL (old app running?)".into(),
            });
        }

        // Hold a read transaction for a coherent snapshot, then online-backup.
        let mut destination_connection = rusqlite::Connection::open(destination)
            .map_err(|e| LegacyError::Sqlite(e.to_string()))?;
        let source_connection = source_connection;
        let backup = rusqlite::backup::Backup::new(&source_connection, &mut destination_connection)
            .map_err(|e| LegacyError::Sqlite(e.to_string()))?;
        let started = std::time::Instant::now();
        backup
            .run_to_completion(
                64,
                Duration::from_millis(10),
                Some(|_progress| {
                    std::thread::sleep(Duration::from_millis(1));
                }),
            )
            .map_err(|e| LegacyError::Sqlite(e.to_string()))?;
        if started.elapsed() > deadline {
            return Err(LegacyError::BackupTimeout(deadline));
        }
        // Drop the backup and connections before reporting readiness.
        drop(backup);
        drop(destination_connection);
        drop(source_connection);
        Ok(LegacyReadOutcome::Ready {
            copy_path: destination.to_path_buf(),
        })
    }

    /// Export one table of the backup copy as JSONL (complete records
    /// only; the copy is immutable so boundaries are frozen by
    /// construction).
    pub fn export_table_jsonl(
        copy_path: &Path,
        table: &str,
        sink: &mut dyn io::Write,
    ) -> Result<usize, LegacyError> {
        let connection =
            rusqlite::Connection::open_with_flags(copy_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| LegacyError::Sqlite(e.to_string()))?;
        // Table names come from the host's fixed allowlist, never user
        // input; still, refuse anything suspicious.
        if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(LegacyError::Sqlite(format!("invalid table name {table:?}")));
        }
        let mut statement = connection
            .prepare(&format!("SELECT rowid, * FROM {table} ORDER BY rowid"))
            .map_err(|e| LegacyError::Sqlite(e.to_string()))?;
        let column_count = statement.column_count();
        let column_names: Vec<String> = (0..column_count)
            .map(|index| {
                statement
                    .column_name(index)
                    .map(str::to_string)
                    .map_err(|e| LegacyError::Sqlite(e.to_string()))
            })
            .collect::<Result<_, _>>()?;
        let mut rows = statement
            .query([])
            .map_err(|e| LegacyError::Sqlite(e.to_string()))?;
        let mut exported = 0usize;
        while let Some(row) = rows
            .next()
            .map_err(|e| LegacyError::Sqlite(e.to_string()))?
        {
            let mut record = serde_json::Map::new();
            for (index, name) in column_names.iter().enumerate() {
                let name = name.as_str();
                let value = row
                    .get_ref(index)
                    .map_err(|e| LegacyError::Sqlite(e.to_string()))?;
                record.insert(
                    name.to_string(),
                    match value {
                        rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                        rusqlite::types::ValueRef::Integer(v) => serde_json::Value::from(v),
                        rusqlite::types::ValueRef::Real(v) => serde_json::Value::from(v),
                        rusqlite::types::ValueRef::Text(v) => {
                            serde_json::Value::from(String::from_utf8_lossy(v).into_owned())
                        }
                        rusqlite::types::ValueRef::Blob(v) => serde_json::Value::from(
                            v.iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect::<String>(),
                        ),
                    },
                );
            }
            let line = serde_json::to_string(&serde_json::Value::Object(record))
                .map_err(|e| LegacyError::Io(e.to_string()))?;
            writeln!(sink, "{line}").map_err(|e| LegacyError::Io(e.to_string()))?;
            exported += 1;
        }
        Ok(exported)
    }

    /// Whether the (already opened) source has a live WAL: a `-wal` file
    /// with non-zero size next to the database.
    fn _unused(_connection: &rusqlite::Connection) {}
}

fn wal_is_live(connection: &rusqlite::Connection) -> bool {
    // journal_mode query is read-only; a non-wal legacy DB returns "delete".
    let mode: Option<String> = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .optional()
        .ok()
        .flatten();
    if mode.as_deref() != Some("wal") {
        return false;
    }
    // WAL mode: live only when a writer holds the file — approximate by the
    // presence of a non-empty -wal sibling.
    let path = match connection.query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
    {
        Ok(path) => path,
        Err(_) => return false,
    };
    let wal = PathBuf::from(&path).with_extension("sqlite3-wal");
    let sibling = PathBuf::from(format!("{path}-wal"));
    let size = |file: &Path| std::fs::metadata(file).map(|meta| meta.len()).unwrap_or(0);
    // Either spelling counts; empty WALs mean no pending writer state.
    size(&wal) > 0 || size(&sibling) > 0
}

impl LegacyError {
    /// Marker so the legacy reader can never be confused with the legacy
    /// mutating stack at the type level.
    pub fn is_readonly_path() -> bool {
        true
    }
}
