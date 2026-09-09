//! Built-in Native harness plugin (library).
//!
//! The loop is pure request-shaping over public host services: model turns
//! via `host.model.stream`, tool calls via `host.tools.call` with
//! attempt-stable operation keys, checkpoints after every turn, plan
//! publication and completion proposals. No direct provider, tool, store
//! or Tauri access — the host owns all of it.

pub mod loop_engine;
pub mod orchestration;
pub mod request_projection;
pub mod session;

pub use loop_engine::{LoopConfig, LoopResult};
pub use session::NativeSession;
