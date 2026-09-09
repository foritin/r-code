//! Persistent client outbox: commands are recorded *before* first
//! transmission and retained until acknowledgement, so a lost reply or a
//! reconnect replays the same `(client_id, command_id)` instead of minting a
//! duplicate effect. Never store plaintext secrets here — sensitive settings
//! travel as credential-broker references.

use std::io;
use std::path::{Path, PathBuf};

/// One outbox record.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OutboxEntry {
    pub command_id: String,
    pub method: String,
    pub params: serde_json::Value,
    pub acked: bool,
}

/// Durable outbox backed by a JSON file.
pub struct Outbox {
    path: PathBuf,
    client_id: String,
    entries: Vec<OutboxEntry>,
}

impl Outbox {
    /// Load (or create) the outbox for one client beneath the profile root.
    pub fn open(harness_v2_root: &Path, client_id: &str) -> io::Result<Self> {
        let dir = harness_v2_root.join("client-outbox");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{client_id}.json"));
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Ok(Self {
            path,
            client_id: client_id.to_string(),
            entries,
        })
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    fn persist(&self) -> io::Result<()> {
        let text = serde_json::to_string_pretty(&self.entries)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(&self.path, text)
    }

    /// Record a command before its first transmission. Re-preparing an
    /// existing id returns the original entry unchanged.
    pub fn prepare(
        &mut self,
        command_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> io::Result<OutboxEntry> {
        if let Some(existing) = self
            .entries
            .iter()
            .find(|entry| entry.command_id == command_id)
        {
            return Ok(existing.clone());
        }
        let entry = OutboxEntry {
            command_id: command_id.to_string(),
            method: method.to_string(),
            params,
            acked: false,
        };
        self.entries.push(entry.clone());
        self.persist()?;
        Ok(entry)
    }

    /// Mark a command acknowledged (it may now be compacted later).
    pub fn mark_acked(&mut self, command_id: &str) -> io::Result<bool> {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.command_id == command_id)
        else {
            return Ok(false);
        };
        if entry.acked {
            return Ok(true);
        }
        entry.acked = true;
        self.persist()?;
        Ok(true)
    }

    /// Commands still awaiting acknowledgement, oldest first.
    pub fn pending(&self) -> Vec<OutboxEntry> {
        self.entries
            .iter()
            .filter(|entry| !entry.acked)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_prepares_before_send_and_survives_reopen() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("harness-v2");
        let mut outbox = Outbox::open(&root, "client-a").expect("open");
        let entry = outbox
            .prepare("cmd-1", "task.create", serde_json::json!({"title": "fix"}))
            .expect("prepare");
        assert!(!entry.acked);

        // Re-preparing the same id is idempotent.
        let again = outbox
            .prepare("cmd-1", "task.create", serde_json::json!({"title": "fix"}))
            .expect("re-prepare");
        assert_eq!(again, entry);

        // A fresh process (new Outbox handle) sees the pending entry.
        let reopened = Outbox::open(&root, "client-a").expect("reopen");
        assert_eq!(reopened.pending().len(), 1);
        assert_eq!(reopened.pending()[0].command_id, "cmd-1");

        // Acknowledge and reopen: nothing pending.
        let mut outbox = Outbox::open(&root, "client-a").expect("reopen mutable");
        assert!(outbox.mark_acked("cmd-1").expect("ack"));
        assert!(outbox.pending().is_empty());
        assert!(Outbox::open(&root, "client-a")
            .expect("again")
            .pending()
            .is_empty());
        // Unknown ids report false rather than inventing success.
        assert!(!outbox.mark_acked("cmd-404").expect("unknown"));
    }
}
