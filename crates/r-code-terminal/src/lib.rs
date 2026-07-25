//! R-Code Terminal -- PTY 终端系统。
//!
//! 提供 PTY 管理、Shell 集成注入和 OSC 133 块模型解析三个子系统。
//!
//! ## 模块结构
//! - [`manager`]: 多终端 PTY 管理器（创建/发送/读取/终止/调整大小）
//! - [`shell_integration`]: Shell 集成纯函数（zsh/bash/fish OSC 133 注入）
//! - [`block`]: OSC 133 块解析器（命令块/turn 块）
//! - [`cli_detector`]: 外部 CLI 检测（claude/codex 进程名匹配）
//! - [`replay_parser`]: 外部 CLI JSONL transcript 增量解析

pub mod block;
pub mod cli_detector;
pub mod control_service;
pub mod manager;
pub mod replay_parser;
pub mod shell_integration;

pub use block::{Block, BlockParser, BlockType};
pub use cli_detector::{CliDetector, ExternalCli};
pub use control_service::{
    SendOptions, TerminalControlService, TerminalInfo, WaitMode, WaitResult,
};
pub use manager::{TerminalHandle, TerminalId, TerminalManager, TerminalState};
pub use replay_parser::{
    detect_format, parse_claude_line, parse_codex_line, ReplayEvent, ReplayParser,
};
pub use shell_integration::{
    shell_integration_spawn, ShellIntegrationConfig, ShellIntegrationResult,
};
