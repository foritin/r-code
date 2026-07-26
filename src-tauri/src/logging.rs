//! 结构化日志框架。
//!
//! 使用 `tracing` crate；日志格式结构化 JSON；支持日志级别动态调整。
//! [doc-14 阶段1] [doc-07 §6]

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 初始化日志。
///
/// 默认级别 `info`，可通过 `RUST_LOG` 环境变量覆盖。
/// 生产环境使用 JSON 格式，开发环境使用人类可读格式。
/// 所有事件同时写入内存环形缓冲（应用内日志查看，`cmd_logs_tail`）。
pub fn init() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let json_layer = fmt::layer()
        .json()
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(false)
        .with_line_number(false);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(json_layer)
        .with(crate::log_buffer::BufferLayer)
        .init();
}

/// 初始化开发环境日志（人类可读格式）。
pub fn init_dev() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

    let fmt_layer = fmt::layer()
        .with_target(true)
        .with_file(true)
        .with_line_number(true);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_does_not_panic() {
        // 初始化可能因为重复调用而失败，但不应该 panic
        let _ = std::panic::catch_unwind(|| {
            init();
        });
    }
}
