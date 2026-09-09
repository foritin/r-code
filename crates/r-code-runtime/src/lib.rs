//! # r-code-runtime
//!
//! Tauri-free composition of the concrete host services: plugin management,
//! process transport, storage adapters, daemon ownership and the shared
//! ApplicationService. Frontends (GUI/TUI/MCP) reach it through
//! `r-code-client`; nothing here links Tauri.

pub mod application;
pub mod application_receipts;
pub mod daemon;
pub mod ipc;
pub mod legacy;
pub mod plugins;
pub mod process_guard;
pub mod profile;
pub mod providers;
pub mod run_manager;
pub mod services;
pub mod settings;
pub mod workspace_locks;

pub use profile::*;
