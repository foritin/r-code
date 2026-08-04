//! 结构化诊断日志。
//!
//! `tracing` 事件同时写入控制台、进程内尾部缓冲和按日滚动的 JSONL 文件。
//! 文件固定保留最近 7 个自然日，且在落盘前完成密钥脱敏。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

static LOG_WRITER_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

/// 与 Tauri `app_data_dir()/r-code/logs` 对齐的启动期日志目录。
pub fn default_log_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("com.r-code.app")
        .join("r-code")
        .join("logs")
}

/// 从 CommandState 的 config 目录稳定推导同一日志目录。
pub fn log_dir_for_config(config_dir: &Path) -> PathBuf {
    config_dir.parent().unwrap_or(config_dir).join("logs")
}

fn persistent_writer() -> (
    Option<tracing_appender::non_blocking::NonBlocking>,
    Option<WorkerGuard>,
    Option<String>,
) {
    let log_dir = default_log_dir();
    if let Err(error) = std::fs::create_dir_all(&log_dir) {
        return (
            None,
            None,
            Some(format!(
                "could not create diagnostic log directory {}: {error}",
                log_dir.display()
            )),
        );
    }
    let mut warnings = Vec::new();
    if let Err(error) = crate::log_buffer::prune_expired_logs(&log_dir) {
        warnings.push(format!("could not prune expired diagnostic logs: {error}"));
    }
    if let Err(error) = crate::log_buffer::hydrate_from_persistence(&log_dir) {
        warnings.push(format!("could not load diagnostic log history: {error}"));
    }
    let appender = tracing_appender::rolling::daily(&log_dir, crate::log_buffer::LOG_FILE_PREFIX);
    let (writer, guard) = tracing_appender::non_blocking(appender);
    (
        Some(writer),
        Some(guard),
        (!warnings.is_empty()).then(|| warnings.join("; ")),
    )
}

/// 初始化生产日志。默认级别 `info`，可通过 `RUST_LOG` 环境变量覆盖。
pub fn init() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (writer, guard, warning) = persistent_writer();

    let console_layer = fmt::layer()
        .json()
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(false)
        .with_line_number(false);

    let initialized = tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(crate::log_buffer::BufferLayer::new(writer))
        .try_init()
        .is_ok();
    if initialized {
        if let Some(guard) = guard {
            let _ = LOG_WRITER_GUARD.set(guard);
        }
        if let Some(warning) = warning {
            tracing::warn!(%warning, "diagnostic log persistence degraded");
        }
    }
}

/// 初始化开发日志（人类可读控制台 + 同一份持久化诊断日志）。
pub fn init_dev() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));
    let (writer, guard, warning) = persistent_writer();
    let console_layer = fmt::layer()
        .with_target(true)
        .with_file(true)
        .with_line_number(true);

    let initialized = tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(crate::log_buffer::BufferLayer::new(writer))
        .try_init()
        .is_ok();
    if initialized {
        if let Some(guard) = guard {
            let _ = LOG_WRITER_GUARD.set(guard);
        }
        if let Some(warning) = warning {
            tracing::warn!(%warning, "diagnostic log persistence degraded");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_dir_is_sibling_of_config() {
        let config = PathBuf::from("root").join("r-code").join("config");
        assert_eq!(
            log_dir_for_config(&config),
            PathBuf::from("root").join("r-code").join("logs")
        );
    }

    #[test]
    fn default_log_dir_uses_the_tauri_bundle_identifier() {
        let path = default_log_dir();
        let rendered = path.to_string_lossy().replace('\\', "/");
        assert!(rendered.ends_with("com.r-code.app/r-code/logs"));
    }
}
