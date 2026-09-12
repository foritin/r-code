//! Capability model (F5/F6): three tiers, granted per device at pairing
//! and enforced daemon-side on every remote command. The sensitive surface
//! below is *never* reachable from a remote transport, regardless of
//! capabilities.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The remote capability tiers (kebab-case on the wire and in storage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Read tasks/events (the default every paired device has).
    EventsRead,
    /// Create/send/cancel/rename/clone tasks.
    TasksWrite,
    /// Decide pending approvals (default OFF; granted per device).
    ApprovalsDecide,
}

impl Capability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Capability::EventsRead => "events-read",
            Capability::TasksWrite => "tasks-write",
            Capability::ApprovalsDecide => "approvals-decide",
        }
    }
}

/// A device's granted capabilities. `events:read` is implicit (every paired
/// device can observe); the set stores the *additional* grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CapabilitySet {
    #[serde(default)]
    tasks_write: bool,
    #[serde(default)]
    approvals_decide: bool,
}

impl CapabilitySet {
    /// The default grant for a freshly paired device: read-only.
    pub fn read_only() -> Self {
        Self::default()
    }

    pub fn with_tasks_write(mut self) -> Self {
        self.tasks_write = true;
        self
    }

    pub fn with_approvals_decide(mut self) -> Self {
        self.approvals_decide = true;
        self
    }

    pub fn has(&self, capability: Capability) -> bool {
        match capability {
            Capability::EventsRead => true,
            Capability::TasksWrite => self.tasks_write,
            Capability::ApprovalsDecide => self.approvals_decide,
        }
    }

    /// Wire labels (stability for storage/event payloads).
    pub fn labels(&self) -> Vec<&'static str> {
        let mut labels = vec![Capability::EventsRead.as_str()];
        if self.tasks_write {
            labels.push(Capability::TasksWrite.as_str());
        }
        if self.approvals_decide {
            labels.push(Capability::ApprovalsDecide.as_str());
        }
        labels
    }
}

/// Methods no remote transport may ever call, whatever the capabilities
/// (F5: settings/plugins management, device administration, service
/// control and the pairing/remote admin surface are local-only).
pub const FORBIDDEN_REMOTE_METHODS: &[&str] = &[
    // settings (read AND write: provider credentials live here)
    "settings.get",
    "settings.apply",
    "settings.setDefault",
    "settings.removeProvider",
    // plugin lifecycle (code installation is a host-console action)
    "plugins.install",
    "plugins.remove",
    "plugins.setEnabled",
    // device administration
    "device.list",
    "device.revoke",
    "device.updateCapabilities",
    "device.setListener",
    // pairing / remote admin
    "remote.pairingStart",
    "remote.setListener",
    // service lifecycle
    "service.shutdown",
];

/// Whether a method is unconditionally forbidden on remote transports.
pub fn is_forbidden_remote_method(method: &str) -> bool {
    FORBIDDEN_REMOTE_METHODS.contains(&method)
}

/// The capability a remotely callable method requires. `None` = forbidden
/// for remotes (see [`FORBIDDEN_REMOTE_METHODS`]).
pub fn required_capability(method: &str) -> Option<Capability> {
    if is_forbidden_remote_method(method) {
        return None;
    }
    match method {
        // read surface
        "ping" | "echo" | "task.list" | "task.detail" | "task.events" | "task.branches"
        | "models.available" | "plugins.list" | "approvals.list" | "codex.status" => {
            Some(Capability::EventsRead)
        }
        // write surface
        "task.create"
        | "task.sendMessage"
        | "task.cancel"
        | "task.rename"
        | "task.clone"
        | "task.selectHarness"
        | "task.setPreferences" => Some(Capability::TasksWrite),
        // approvals (the $remote suffix is injected by the listener's
        // gate for remote decisions — R12; unreachable from the wire
        // because the gate refuses unknown methods first)
        "approvals.decide" | "approvals.decide$remote" => Some(Capability::ApprovalsDecide),
        _ => None,
    }
}

/// Helper for capability-set round trips in tests and storage.
pub fn capability_set_from_labels(labels: &BTreeSet<String>) -> CapabilitySet {
    let mut set = CapabilitySet::read_only();
    for label in labels {
        match label.as_str() {
            "tasks-write" => set = set.with_tasks_write(),
            "approvals-decide" => set = set.with_approvals_decide(),
            _ => {}
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_read_only() {
        let set = CapabilitySet::read_only();
        assert!(set.has(Capability::EventsRead));
        assert!(!set.has(Capability::TasksWrite));
        assert!(!set.has(Capability::ApprovalsDecide));
        assert_eq!(set.labels(), vec!["events-read"]);
    }

    #[test]
    fn forbidden_methods_cover_the_sensitive_surface() {
        for method in [
            "settings.get",
            "settings.apply",
            "plugins.install",
            "plugins.setEnabled",
            "device.revoke",
            "remote.pairingStart",
            "service.shutdown",
        ] {
            assert!(
                is_forbidden_remote_method(method),
                "{method} must be forbidden"
            );
            assert!(required_capability(method).is_none());
        }
        // The readable plugin list stays reachable with events:read.
        assert_eq!(
            required_capability("plugins.list"),
            Some(Capability::EventsRead)
        );
        assert_eq!(
            required_capability("approvals.decide"),
            Some(Capability::ApprovalsDecide)
        );
    }
}
