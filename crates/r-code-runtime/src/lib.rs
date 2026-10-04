//! # r-code-runtime
//!
//! Tauri-free composition of the concrete host services: plugin management,
//! process transport, storage adapters, daemon ownership and the shared
//! ApplicationService. Frontends (GUI/TUI/MCP) reach it through
//! `r-code-client`; nothing here links Tauri.

// clippy 1.99 对 async_trait 展开的 boxing 方法报 double_must_use（方法与其返回
// 的 BoxFuture 同时标 must_use）——宏输出不可控，crate 级豁免。
#![allow(clippy::double_must_use)]

pub mod application;
pub mod application_receipts;
pub mod child_supervisor;
pub mod daemon;
pub mod ipc;
pub mod legacy;
pub mod plugins;
pub mod process_guard;
pub mod profile;
pub mod providers;
pub mod remote;
pub mod run_drive;
pub mod run_manager;
pub mod services;
pub mod settings;
pub mod workspace_locks;

pub use profile::*;
