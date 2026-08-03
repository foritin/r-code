//! 日志环形缓冲 -- 应用内日志查看。
//!
//! `tracing` 事件除正常输出外，同时写入固定容量的内存环形缓冲，
//! 前端通过 `cmd_logs_tail` 读取（Settings → 日志）。
//! [doc-14 阶段1]

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;

/// 环形缓冲容量（超出丢弃最旧条目）。
const CAPACITY: usize = 1000;

/// 一条日志条目（cmd_logs_tail 返回形状）。
#[derive(Debug, Clone, Serialize)]
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

/// tracing Layer：把事件写入环形缓冲。
pub struct BufferLayer;

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
            message: visitor.finish(),
        };
        let mut buf = buffer().lock().unwrap();
        if buf.len() >= CAPACITY {
            buf.pop_front();
        }
        buf.push_back(entry);
    }
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

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
