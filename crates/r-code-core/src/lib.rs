//! R-Code 核心产品类型：DTO、状态机、错误类型。
//!
//! 本 crate 定义 R-Code 私有的产品类型，不修改 agent-core 公共枚举。
//! 产品专属块（file_ref, selection_ref）走 `ContentBlock::Custom` 透传。
//!
//! ## 模块结构
//! - [`dto`]: 冻结的产品 DTO（Task, AgentRun, ToolCall, PermissionRequest 等）
//! - [`state`]: 任务状态机与转换合法性校验
//! - [`error`]: R-Code 专属错误类型（WorktreeError, PathEscape, BlobError 等）
//! - [`ids`]: ID 生成工具

#![allow(clippy::doc_nested_refdefs)]

pub mod dto;
pub mod error;
pub mod ids;
mod memory;
pub mod plan;
pub mod process;
pub mod secret;
pub mod security;
pub mod state;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

// 重新导出公共合同类型，方便产品 crate 统一引用
pub use agent_contract::{
    ContentBlock, FileSource, ImageSource, InferenceOptions, Message, Role, Session, SessionEvent,
    SessionMeta, SessionStatus, ToolCallOutcome, ToolHost, ToolSource, ToolSpec, Usage,
};
pub use agent_error::{Error, Result, ResultExt};
pub use memory::*;
