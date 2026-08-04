//! 持久化诊断日志与应用内尾部缓冲。
//!
//! `tracing` 事件除正常输出外，同时写入按日滚动的 JSONL 文件和固定容量的
//! 内存环形缓冲；启动时从最近七天的磁盘日志水合尾部，前端通过
//! `cmd_logs_tail` 读取（Settings → 诊断）。
//! [doc-14 阶段1]

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use r_code_core::secret::redact_text;
use serde::{Deserialize, Serialize};
use tracing::field::{Field, Visit};
use tracing_appender::non_blocking::NonBlocking;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::Layer;

/// 环形缓冲容量（超出丢弃最旧条目）。
const CAPACITY: usize = 1000;

/// 日志文件名前缀。`tracing-appender` 会追加 UTC 日期后缀。
pub const LOG_FILE_PREFIX: &str = "r-code.jsonl";

/// 固定保留天数。该策略是产品安全边界，不暴露为用户设置。
pub const LOG_RETENTION_DAYS: i64 = 7;

/// 一条日志条目（cmd_logs_tail 返回形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// RFC3339 时间戳
    pub timestamp: String,
    /// 级别（ERROR/WARN/INFO/DEBUG/TRACE）
    pub level: String,
    /// 模块路径
    pub target: String,
    /// 消息文本（含结构化字段 k=v）
    pub message: String,
}

static BUFFER: OnceLock<Mutex<VecDeque<LogEntry>>> = OnceLock::new();

fn buffer() -> &'static Mutex<VecDeque<LogEntry>> {
    BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

/// 读取最近 `limit` 条日志；`level` 非空时按级别精确过滤（大小写不敏感）。
pub fn tail(limit: usize, level: Option<&str>) -> Vec<LogEntry> {
    let buf = buffer().lock().unwrap();
    let level = level.map(|l| l.to_ascii_uppercase());
    buf.iter()
        .rev()
        .filter(|e| level.as_ref().is_none_or(|l| e.level == *l))
        .take(limit)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// tracing Layer：把事件同时写入环形缓冲和按日滚动的 JSONL 文件。
pub struct BufferLayer {
    writer: Option<NonBlocking>,
}

impl BufferLayer {
    pub fn new(writer: Option<NonBlocking>) -> Self {
        Self { writer }
    }
}

impl<S: tracing::Subscriber> Layer<S> for BufferLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        let entry = LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: meta.level().to_string(),
            target: meta.target().to_string(),
            // 日志在落盘前即脱敏，支持包导出时还会再做一次防御性脱敏。
            message: redact_text(&visitor.finish()),
        };
        let mut buf = buffer().lock().unwrap();
        if buf.len() >= CAPACITY {
            buf.pop_front();
        }
        buf.push_back(entry.clone());
        drop(buf);

        if let Some(writer) = &self.writer {
            let mut writer = writer.make_writer();
            if serde_json::to_writer(&mut writer, &entry).is_ok() {
                let _ = writer.write_all(b"\n");
            }
        }
    }
}

/// 合并最近的指定级别日志。支持包用它只收集 warning/error，而不会因为尾部 info
/// 过多而挤掉真正需要诊断的错误。
pub fn tail_levels_with_persistence(
    log_dir: &Path,
    limit: usize,
    levels: &[&str],
) -> std::io::Result<Vec<LogEntry>> {
    collect_with_persistence(log_dir, limit, |entry| {
        levels
            .iter()
            .any(|expected| entry.level.eq_ignore_ascii_case(expected))
    })
}

/// 启动时把近 7 天的磁盘尾部装入内存。诊断页之后只读取固定容量队列，避免每 1.5 秒
/// 重扫日志文件；新事件仍由 [`BufferLayer`] 实时追加。
pub fn hydrate_from_persistence(log_dir: &Path) -> std::io::Result<usize> {
    let cutoff = Utc::now() - Duration::days(LOG_RETENTION_DAYS);
    let mut entries = read_persisted_entries(log_dir)?;
    entries.retain(|entry| {
        DateTime::parse_from_rfc3339(&entry.timestamp)
            .map(|timestamp| timestamp.with_timezone(&Utc) >= cutoff)
            .unwrap_or(false)
    });
    entries.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
    if entries.len() > CAPACITY {
        entries.drain(..entries.len() - CAPACITY);
    }

    let count = entries.len();
    let mut buf = buffer().lock().unwrap();
    buf.clear();
    buf.extend(entries);
    Ok(count)
}

fn collect_with_persistence(
    log_dir: &Path,
    limit: usize,
    matches: impl Fn(&LogEntry) -> bool,
) -> std::io::Result<Vec<LogEntry>> {
    prune_expired_logs(log_dir)?;
    let cutoff = Utc::now() - Duration::days(LOG_RETENTION_DAYS);
    let mut entries = read_persisted_entries(log_dir)?;
    entries.extend(tail(CAPACITY, None));
    entries.retain(|entry| {
        matches(entry)
            && DateTime::parse_from_rfc3339(&entry.timestamp)
                .map(|timestamp| timestamp.with_timezone(&Utc) >= cutoff)
                // 当前进程测试或极早期日志若没有标准时间戳，仍允许从内存查看。
                .unwrap_or(true)
    });
    entries.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));

    let mut seen = HashSet::new();
    entries.retain(|entry| {
        seen.insert((
            entry.timestamp.clone(),
            entry.level.clone(),
            entry.target.clone(),
            entry.message.clone(),
        ))
    });
    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
    }
    Ok(entries)
}

fn read_persisted_entries(log_dir: &Path) -> std::io::Result<Vec<LogEntry>> {
    let mut paths = match std::fs::read_dir(log_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| is_log_file(path))
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    paths.sort();

    let mut entries = Vec::new();
    for path in paths {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(mut entry) = serde_json::from_str::<LogEntry>(&line) {
                // Older files may predate a newly added redaction rule. Treat disk as untrusted
                // diagnostic input and sanitize it again before hydration, preview, or export.
                entry.message = redact_text(&entry.message);
                entries.push(entry);
            }
        }
    }
    Ok(entries)
}

fn is_log_file(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name == LOG_FILE_PREFIX || name.starts_with(&format!("{LOG_FILE_PREFIX}."))
            })
}

/// 删除七个自然日之前的滚动文件。只会处理由 R-Code 创建的固定前缀文件。
pub fn prune_expired_logs(log_dir: &Path) -> std::io::Result<usize> {
    prune_expired_logs_at(log_dir, Utc::now().date_naive())
}

fn prune_expired_logs_at(log_dir: &Path, today: NaiveDate) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let oldest_kept_day = today - Duration::days(LOG_RETENTION_DAYS - 1);
    let mut removed = 0;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !is_log_file(&path) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(date) = name
            .strip_prefix(&format!("{LOG_FILE_PREFIX}."))
            .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        else {
            continue;
        };
        if date < oldest_kept_day {
            std::fs::remove_file(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// 提取事件字段：`message` 为主文本，其余结构化字段以 `k=v` 追加。
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<String>,
}

impl MessageVisitor {
    fn finish(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.fields.join(" ")
        } else {
            format!("{} {}", self.message, self.fields.join(" "))
        }
    }
}

fn sensitive_log_field(name: &str) -> bool {
    let normalized = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "password"
            | "passwd"
            | "pwd"
            | "token"
            | "apikey"
            | "xapikey"
            | "clientsecret"
            | "privatekey"
            | "credential"
            | "credentials"
            | "awsaccesskeyid"
            | "awssecretaccesskey"
            | "awssessiontoken"
    ) || normalized.ends_with("accesstoken")
        || normalized.ends_with("refreshtoken")
        || normalized.ends_with("idtoken")
        || normalized.ends_with("sessiontoken")
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else if sensitive_log_field(field.name()) {
            self.fields.push(format!("{}=***", field.name()));
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else if sensitive_log_field(field.name()) {
            self.fields.push(format!("{}=***", field.name()));
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tracing_subscriber::prelude::*;

    #[test]
    fn tail_filters_by_level() {
        {
            let mut buf = buffer().lock().unwrap();
            buf.push_back(LogEntry {
                timestamp: "t1".into(),
                level: "INFO".into(),
                target: "a".into(),
                message: "hello".into(),
            });
            buf.push_back(LogEntry {
                timestamp: "t2".into(),
                level: "ERROR".into(),
                target: "b".into(),
                message: "boom".into(),
            });
        }
        let errors = tail(10, Some("error"));
        assert!(errors.iter().all(|e| e.level == "ERROR"));
        assert!(errors.iter().any(|e| e.message == "boom"));
        let all = tail(10, None);
        assert!(all.len() >= 2);
    }

    #[test]
    fn persisted_tail_preserves_structured_fields_and_filters_levels() {
        let dir = TempDir::new().unwrap();
        let today = Utc::now().date_naive();
        let path = dir
            .path()
            .join(format!("{LOG_FILE_PREFIX}.{}", today.format("%Y-%m-%d")));
        let entries = [
            LogEntry {
                timestamp: Utc::now().to_rfc3339(),
                level: "INFO".into(),
                target: "r_code_host::startup".into(),
                message: "ready".into(),
            },
            LogEntry {
                timestamp: Utc::now().to_rfc3339(),
                level: "ERROR".into(),
                target: "r_code_agent_worker".into(),
                message: "provider failed".into(),
            },
        ];
        let content = entries
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, format!("{content}\n")).unwrap();

        let errors = tail_levels_with_persistence(dir.path(), 10, &["warn", "error"]).unwrap();
        assert!(errors.iter().any(|entry| {
            entry.level == "ERROR"
                && entry.target == "r_code_agent_worker"
                && entry.message == "provider failed"
        }));
        assert!(!errors.iter().any(|entry| entry.message == "ready"));
    }

    #[test]
    fn tracing_layer_writes_redacted_structured_jsonl() {
        let dir = TempDir::new().unwrap();
        let appender = tracing_appender::rolling::daily(dir.path(), LOG_FILE_PREFIX);
        let (writer, guard) = tracing_appender::non_blocking(appender);
        let subscriber = tracing_subscriber::registry().with(BufferLayer::new(Some(writer)));

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                task_id = "task-1",
                token = "secret-value",
                api_key = "plain-api-key",
                password = "plain-password",
                client_secret = "plain-client-secret",
                aws_secret_access_key = "plain-aws-secret",
                "provider request failed"
            );
        });
        // Dropping the guard flushes the non-blocking worker before the file is inspected.
        drop(guard);

        let entries = read_persisted_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.level, "WARN");
        assert!(entry.message.contains("provider request failed"));
        assert!(entry.message.contains("task_id=task-1"));
        assert!(entry.message.contains("token=***"));
        for secret in [
            "secret-value",
            "plain-api-key",
            "plain-password",
            "plain-client-secret",
            "plain-aws-secret",
        ] {
            assert!(
                !entry.message.contains(secret),
                "credential leaked: {secret}"
            );
        }
        for field in [
            "api_key=***",
            "password=***",
            "client_secret=***",
            "aws_secret_access_key=***",
        ] {
            assert!(entry.message.contains(field), "missing redaction: {field}");
        }
        assert!(!entry.timestamp.is_empty());
        assert!(!entry.target.is_empty());
    }

    #[test]
    fn persisted_legacy_logs_are_redacted_again_when_read() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(format!(
            "{LOG_FILE_PREFIX}.{}",
            Utc::now().date_naive().format("%Y-%m-%d")
        ));
        let entry = LogEntry {
            timestamp: Utc::now().to_rfc3339(),
            level: "ERROR".into(),
            target: "legacy".into(),
            message: "api_key=legacy-secret password=legacy-password github_pat_abcdefghijklmnopqrstuvwxyz123456".into(),
        };
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();

        let entries = read_persisted_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].message.contains("legacy-secret"));
        assert!(!entries[0].message.contains("legacy-password"));
        assert!(!entries[0]
            .message
            .contains("github_pat_abcdefghijklmnopqrstuvwxyz123456"));
        assert!(entries[0].message.contains("api_key=***"));
    }

    #[test]
    fn retention_prunes_only_expired_r_code_logs() {
        let dir = TempDir::new().unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 8, 4).unwrap();
        let expired = dir.path().join(format!("{LOG_FILE_PREFIX}.2026-07-28"));
        let kept = dir.path().join(format!("{LOG_FILE_PREFIX}.2026-07-29"));
        let unrelated = dir.path().join("application.2026-01-01.log");
        std::fs::write(&expired, "old").unwrap();
        std::fs::write(&kept, "kept").unwrap();
        std::fs::write(&unrelated, "unrelated").unwrap();

        assert_eq!(prune_expired_logs_at(dir.path(), today).unwrap(), 1);
        assert!(!expired.exists());
        assert!(kept.exists());
        assert!(unrelated.exists());
    }
}
