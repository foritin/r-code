//! Plugin system: package installation (T07), catalog and version
//! lifecycle (T08), process transport (T09) and host RPC routing (T10).

pub mod approval_store;
pub mod catalog;
pub mod package;
pub mod router;
pub mod session;
pub mod transport;

pub use approval_store::{
    ApprovalStore, DecideError, DecisionRecord, PendingView, DEFAULT_DECISION_TIMEOUT,
};
pub use catalog::{
    Availability, CatalogEntry, CatalogError, PluginCatalog, UnavailableReason, HOST_API,
};
pub use package::{InstallError, InstalledPackage, PackageInstaller};
pub use router::{
    service_for_method, HostRouter, IgnoreQuestions, QuestionSink, RaisedQuestion, RouterStreamSink,
};
pub use session::{PluginSession, SessionError};
pub use transport::{
    spawn_plugin, DenyCallbacks, PluginCallbacks, PluginProcess, TransportError, TransportLimits,
    STDERR_TAIL_BYTES,
};
