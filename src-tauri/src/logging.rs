//! 结构化诊断日志。
//!
//! `tracing` 事件同时写入控制台、进程内尾部缓冲和按日滚动的 JSONL 文件。
//! 文件固定保留最近 7 个自然日，且在落盘前完成密钥脱敏。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_appender::non_blocking::WorkerGuard;
#[cfg(test)]
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::{format::Writer as FmtWriter, FormatEvent, FormatFields};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 控制台事件格式器（F-sec-01）：消息经 [`crate::log_buffer::redacted_event_message`]
/// 与落盘/环形缓冲同源脱敏——此前控制台 fmt 层独立格式化字段，启动期 stderr
/// 可以输出未脱敏内容。`json` 对应生产控制台（单行 JSON），否则为开发可读行。
struct RedactedEventFormat {
    json: bool,
}

impl<S, N> FormatEvent<S, N> for RedactedEventFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: FmtWriter<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let meta = event.metadata();
        let message = crate::log_buffer::redacted_event_message(event);
        let timestamp = chrono::Utc::now().to_rfc3339();
        if self.json {
            write!(
                writer,
                "{{\"timestamp\":{},\"level\":\"{}\",\"target\":{},\"message\":{}}}",
                serde_json::to_string(&timestamp).map_err(|_| std::fmt::Error)?,
                meta.level(),
                serde_json::to_string(meta.target()).map_err(|_| std::fmt::Error)?,
                serde_json::to_string(&message).map_err(|_| std::fmt::Error)?,
            )
        } else {
            let location = match (meta.file(), meta.line()) {
                (Some(file), Some(line)) => format!(" {file}:{line}"),
                _ => String::new(),
            };
            write!(
                writer,
                "{timestamp} {} {}{location}: {message}",
                meta.level(),
                meta.target()
            )
        }
    }
}

static LOG_WRITER_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

/// 与 Tauri `app_data_dir()/r-code/logs` 对齐的启动期日志目录。
pub fn default_log_dir() -> PathBuf {
    crate::app_paths::default_data_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("r-code"))
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
/// `tauri_plugin_updater` 内部日志静音：更新端点未发布 release 属预期
/// external-pending 状态，检查失败由 updater 域日志与「更新」设置页状态呈现。
pub fn init() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tauri_plugin_updater=off"));
    let (writer, guard, warning) = persistent_writer();

    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .event_format(RedactedEventFormat { json: true });

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
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("debug,tauri_plugin_updater=off"));
    let (writer, guard, warning) = persistent_writer();
    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .event_format(RedactedEventFormat { json: false });

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

    /// 控制台格式器必须与落盘同一脱敏规则（F-sec-01 钉子）。
    #[test]
    fn console_formatter_redacts_sensitive_fields() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct SharedBuf(Arc<Mutex<Vec<u8>>>);
        impl Write for SharedBuf {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl MakeWriter<'_> for SharedBuf {
            type Writer = Self;
            fn make_writer(&self) -> Self {
                self.clone()
            }
        }

        let buffer = SharedBuf(Arc::new(Mutex::new(Vec::new())));
        let layer = fmt::layer()
            .with_writer(buffer.clone())
            .event_format(RedactedEventFormat { json: false });
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                token = "console-secret-value",
                api_key = "console-api-key",
                "console redaction probe"
            );
        });
        let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert!(text.contains("console redaction probe"));
        assert!(text.contains("token=***"));
        assert!(text.contains("api_key=***"));
        assert!(!text.contains("console-secret-value"));
        assert!(!text.contains("console-api-key"));
    }

    #[test]
    fn default_log_dir_uses_the_tauri_bundle_identifier() {
        let path = default_log_dir();
        let rendered = path.to_string_lossy().replace('\\', "/");
        assert!(rendered.ends_with(&format!(
            "{}/r-code/logs",
            crate::app_paths::bundle_identifier()
        )));
    }
}
