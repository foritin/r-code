//! E08 — one registry owning every in-flight supervised tree.
//!
//! Registration is explicit at the seams where tree ids are BORN: the plugin
//! session's [`PluginProcess`](crate::plugins::transport::PluginProcess)
//! (surfaced by `PluginSession::supervised_tree_id` for the owning run) and
//! the profiled run's `SupervisedRun` record (surfaced alongside the
//! `open_profiled` operation token by
//! [`ManagedProcessService::tree_of`](crate::services::processes::ManagedProcessService)).
//! The registry itself is only a registry: no journal interception, no second
//! state machine — cancellation and proof ride each handle's own supervised
//! vocabulary, and a set-wide "all dead" is claimed only from per-tree
//! proofs.

use crate::plugins::transport::PluginProcess;
use crate::services::process_supervisor::{ProcessTreeBackend, RunningTree};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the sweep waits for one tree's own death proof.
const PROOF_TIMEOUT: Duration = Duration::from_secs(5);

/// What the registry consumes from one supervised tree: its identity and its
/// own cancel-and-prove primitive. The handle's vocabulary (phases, proofs)
/// stays with the handle — the registry never re-derives it.
#[async_trait::async_trait]
pub trait SupervisedChild: Send + Sync {
    /// The supervised tree this handle owns (the durable tree id).
    fn tree_id(&self) -> &str;

    /// Cancel this tree and return its OWN death proof. An already-settled
    /// tree proves trivially — it must never block a sweep.
    async fn cancel_and_prove(&self) -> bool;
}

#[async_trait::async_trait]
impl SupervisedChild for PluginProcess {
    fn tree_id(&self) -> &str {
        PluginProcess::tree_id(self)
    }

    async fn cancel_and_prove(&self) -> bool {
        if !self.is_alive() {
            return true;
        }
        // A busy harness may not service the graceful cancel; the confirmed
        // kill that follows is the proof.
        let _ = self.cancel("child-supervisor sweep").await;
        self.kill_confirmed().await
    }
}

/// One profiled run's tree: the backend holds the containment, so the sweep
/// terminates through it and demands the same `wait_and_prove` record the
/// close path accepts.
pub struct ProfiledTreeChild {
    tree_id: String,
    backend: Arc<dyn ProcessTreeBackend>,
    tree: RunningTree,
}

impl ProfiledTreeChild {
    pub fn new(
        tree_id: impl Into<String>,
        backend: Arc<dyn ProcessTreeBackend>,
        tree: RunningTree,
    ) -> Self {
        Self {
            tree_id: tree_id.into(),
            backend,
            tree,
        }
    }
}

#[async_trait::async_trait]
impl SupervisedChild for ProfiledTreeChild {
    fn tree_id(&self) -> &str {
        &self.tree_id
    }

    async fn cancel_and_prove(&self) -> bool {
        if self.backend.terminate(&self.tree).await.is_err() {
            return false;
        }
        self.backend
            .wait_and_prove(&self.tree, PROOF_TIMEOUT)
            .await
            .is_ok()
    }
}

/// Registry failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SupervisedTreeError {
    #[error("tree {tree_id} is already owned by attempt {owner}")]
    AlreadyOwned { tree_id: String, owner: String },
    #[error("tree {tree_id} is not registered under attempt {attempt_id}")]
    ForeignTree { tree_id: String, attempt_id: String },
}

struct Registration {
    attempt_id: String,
    child: Arc<dyn SupervisedChild>,
}

/// The registry: every in-flight supervised tree keyed by its tree id with
/// exactly one owning attempt (E08.1).
#[derive(Default)]
pub struct ChildSupervisor {
    entries: Mutex<HashMap<String, Registration>>,
}

/// What one cancel-and-prove-all sweep learned, per tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorSweep {
    /// Trees whose own death proof landed (and deregistered).
    pub proven: Vec<(String, String)>,
    /// Trees that could NOT prove death; they stay registered and the set is
    /// never reported swept.
    pub unproven: Vec<(String, String)>,
}

impl SupervisorSweep {
    /// True only with per-tree proofs for every registered tree — an
    /// unprovable tree keeps this false forever.
    pub fn all_dead(&self) -> bool {
        self.unproven.is_empty()
    }
}

impl ChildSupervisor {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register one tree under its owning attempt. Re-registering the same
    /// (attempt, tree) converges; a tree already owned by ANOTHER attempt
    /// refuses — every registered tree has exactly one owner attempt.
    pub fn register(
        &self,
        attempt_id: &str,
        child: Arc<dyn SupervisedChild>,
    ) -> Result<(), SupervisedTreeError> {
        let tree_id = child.tree_id().to_string();
        let mut entries = self.lock();
        match entries.get(&tree_id) {
            Some(existing) if existing.attempt_id == attempt_id => Ok(()),
            Some(existing) => Err(SupervisedTreeError::AlreadyOwned {
                tree_id,
                owner: existing.attempt_id.clone(),
            }),
            None => {
                entries.insert(
                    tree_id,
                    Registration {
                        attempt_id: attempt_id.to_string(),
                        child,
                    },
                );
                Ok(())
            }
        }
    }

    /// Settle one tree (it settled on its own): deregister it. A tree not in
    /// the registry — or owned by a different attempt — is refused: a proof
    /// naming a tree the registry does not hold proves nothing.
    pub fn settle(&self, attempt_id: &str, tree_id: &str) -> Result<(), SupervisedTreeError> {
        let mut entries = self.lock();
        match entries.get(tree_id) {
            Some(existing) if existing.attempt_id == attempt_id => {
                entries.remove(tree_id);
                Ok(())
            }
            Some(existing) => Err(SupervisedTreeError::ForeignTree {
                tree_id: tree_id.to_string(),
                attempt_id: existing.attempt_id.clone(),
            }),
            None => Err(SupervisedTreeError::ForeignTree {
                tree_id: tree_id.to_string(),
                attempt_id: attempt_id.to_string(),
            }),
        }
    }

    /// Cancel exactly one registered tree with its own proof; a proven tree
    /// deregisters. Unknown trees refuse (`None`).
    pub async fn cancel_one(&self, attempt_id: &str, tree_id: &str) -> Option<bool> {
        let child = {
            let entries = self.lock();
            let registration = entries.get(tree_id)?;
            if registration.attempt_id != attempt_id {
                return Some(false);
            }
            registration.child.clone()
        };
        let proven = child.cancel_and_prove().await;
        if proven {
            let _ = self.settle(attempt_id, tree_id);
        }
        Some(proven)
    }

    /// Sweep every registered tree with its per-tree death proof (E08.2).
    /// Proven trees deregister; unprovable ones stay registered and the
    /// result never claims the set swept while any remains.
    pub async fn cancel_and_prove_all(&self) -> SupervisorSweep {
        let snapshot: Vec<(String, Arc<dyn SupervisedChild>)> = {
            let entries = self.lock();
            let mut ids: Vec<&String> = entries.keys().collect();
            ids.sort();
            ids.into_iter()
                .map(|tree_id| {
                    let registration = &entries[tree_id];
                    (tree_id.clone(), registration.child.clone())
                })
                .collect()
        };
        let mut proven = Vec::new();
        let mut unproven = Vec::new();
        for (tree_id, child) in snapshot {
            let attempt_id = {
                let entries = self.lock();
                entries
                    .get(&tree_id)
                    .map(|registration| registration.attempt_id.clone())
            };
            let Some(attempt_id) = attempt_id else {
                continue;
            };
            if child.cancel_and_prove().await {
                let _ = self.settle(&attempt_id, &tree_id);
                proven.push((attempt_id, tree_id));
            } else {
                unproven.push((attempt_id, tree_id));
            }
        }
        SupervisorSweep { proven, unproven }
    }

    /// The registered (attempt, tree) pairs, sorted — the supervisor's
    /// observable census for diagnostics.
    pub fn registered(&self) -> Vec<(String, String)> {
        let entries = self.lock();
        let mut pairs: Vec<(String, String)> = entries
            .iter()
            .map(|(tree_id, registration)| (registration.attempt_id.clone(), tree_id.clone()))
            .collect();
        pairs.sort();
        pairs
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Registration>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
