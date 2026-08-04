//! 支持包生成器 -- 创建诊断包用于 bug 报告 [doc-18 M10-04]。
//!
//! 收集：应用版本、平台信息、脱敏日志、配置摘要（无密钥）、数据库统计。
//! 所有敏感数据（API key、token、密码）经 [`redact_text`] 处理后抹除。
//!
//! [doc-18 M10-04] [doc-07 §6]

use std::path::{Path, PathBuf};

use r_code_core::error::ProductError;
use r_code_core::secret::redact_text;
use rusqlite::Connection;

/// 支持包生成器 -- 创建诊断包用于 bug 报告。
///
/// All sensitive data (API keys, tokens, passwords) is redacted.
pub struct SupportBundle {
    output_dir: PathBuf,
    log_dir: PathBuf,
    mcp_servers: Vec<McpServerSupportSummary>,
}

/// 支持包内容。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BundleContents {
    /// 应用版本
    pub version: String,
    /// 平台信息
    pub platform: String,
    /// 生成时间（RFC 3339）
    pub generated_at: String,
    /// 脱敏后的最近 warning/error 日志
    pub logs: Vec<LogEntry>,
    /// 配置摘要（无密钥）
    pub config_summary: ConfigSummary,
    /// 数据库统计
    pub db_stats: DbStats,
}

/// 一条日志条目（已脱敏）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    /// 日志级别
    pub level: String,
    /// 日志消息（已脱敏）
    pub message: String,
    /// 时间戳
    pub timestamp: String,
    /// 产生日志的模块
    pub target: String,
}

/// 配置摘要 -- 仅包含非敏感信息 [doc-07 §6]。
///
/// No API keys or tokens!
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigSummary {
    /// 默认 provider 名称（不含 API key）
    pub default_provider: String,
    /// 已配置 MCP 的最小诊断状态；不包含命令、参数、URL、请求头或凭据引用。
    pub mcp_servers: Vec<McpServerSupportSummary>,
    /// 存储目录路径
    pub storage_dir: String,
}

/// MCP 支持信息的严格白名单。字段只能描述状态，不能承载启动配置或秘密。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct McpServerSupportSummary {
    pub id: String,
    pub transport_kind: String,
    pub enabled: bool,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
}

/// 数据库统计。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DbStats {
    /// 任务总数
    pub task_count: u64,
    /// Agent Run 总数
    pub run_count: u64,
    /// Tool Call 总数
    pub tool_call_count: u64,
}

/// 读取的最近日志行数上限。
const MAX_LOG_LINES: usize = 200;

impl SupportBundle {
    /// 创建支持包生成器。输出目录与诊断日志目录必须显式分离，避免把用户选择的
    /// 导出位置误当作应用日志来源。
    pub fn new(output_dir: PathBuf, log_dir: PathBuf) -> Self {
        Self {
            output_dir,
            log_dir,
            mcp_servers: Vec::new(),
        }
    }

    /// Attach an already-redacted MCP status snapshot. This API intentionally accepts only the
    /// whitelist DTO above, making it impossible for launch arguments or credential values to
    /// enter a support bundle through the MCP configuration object.
    pub fn with_mcp_servers(mut self, servers: Vec<McpServerSupportSummary>) -> Self {
        self.mcp_servers = servers;
        self
    }

    /// 生成支持包，写入 JSON 文件。
    ///
    /// Collects: app version, platform info, redacted logs, config summary, db stats.
    /// All sensitive data is redacted using `redact_text`.
    /// 返回生成的 JSON 文件路径。
    pub async fn generate(&self, db_path: &Path) -> Result<PathBuf, ProductError> {
        let contents = self.collect(db_path).await?;
        std::fs::create_dir_all(&self.output_dir)?;

        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let file_name = format!("r-code-support-bundle-{timestamp}.json");
        let out_path = self.output_dir.join(file_name);

        let json = serde_json::to_string_pretty(&contents)?;
        std::fs::write(&out_path, json)?;
        Ok(out_path)
    }

    /// 预览支持包内容（不写文件，供用户确认后再导出）。
    ///
    /// Preview the bundle contents (for user confirmation before export).
    pub async fn preview(&self, db_path: &Path) -> Result<BundleContents, ProductError> {
        self.collect(db_path).await
    }

    /// 收集全部内容。
    async fn collect(&self, db_path: &Path) -> Result<BundleContents, ProductError> {
        let generated_at = chrono::Utc::now().to_rfc3339();
        let logs = self.collect_logs()?;
        let config_summary = self.collect_config(db_path);
        let db_stats = self.collect_db_stats(db_path);

        Ok(BundleContents {
            version: env!("CARGO_PKG_VERSION").to_string(),
            platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            generated_at,
            logs,
            config_summary,
            db_stats,
        })
    }

    /// 读取近 7 天最近的 warning/error，并保留事件原始时间戳与模块。
    fn collect_logs(&self) -> Result<Vec<LogEntry>, ProductError> {
        let logs = crate::log_buffer::tail_levels_with_persistence(
            &self.log_dir,
            MAX_LOG_LINES,
            &["WARN", "ERROR"],
        )?;
        Ok(logs
            .into_iter()
            .map(|entry| LogEntry {
                level: entry.level.to_ascii_lowercase(),
                message: redact_text(&entry.message),
                timestamp: entry.timestamp,
                target: entry.target,
            })
            .collect())
    }

    /// 收集配置摘要（无密钥）。
    ///
    /// ConfigSummary should only include non-sensitive config info
    /// (provider name, server names, directory paths).
    fn collect_config(&self, db_path: &Path) -> ConfigSummary {
        let default_provider = detect_default_provider();
        let storage_dir = db_path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        ConfigSummary {
            default_provider,
            mcp_servers: self.mcp_servers.clone(),
            storage_dir,
        }
    }

    /// 收集数据库统计。表缺失 / DB 不存在时返回 0（降级）。
    fn collect_db_stats(&self, db_path: &Path) -> DbStats {
        let conn = match Connection::open(db_path) {
            Ok(c) => c,
            Err(_) => {
                return DbStats {
                    task_count: 0,
                    run_count: 0,
                    tool_call_count: 0,
                }
            }
        };
        DbStats {
            task_count: count_rows(&conn, "tasks"),
            run_count: count_rows(&conn, "agent_runs"),
            tool_call_count: count_rows(&conn, "tool_calls"),
        }
    }
}

/// 检测默认 provider（仅返回名称，绝不返回 API key）。
fn detect_default_provider() -> String {
    if std::env::var_os("OPENAI_API_KEY").is_some() {
        "openai".to_string()
    } else {
        // 默认 provider 为 anthropic（无论 ANTHROPIC_API_KEY 是否设置）
        "anthropic".to_string()
    }
}

/// 统计表行数，失败返回 0。
fn count_rows(conn: &Connection, table: &str) -> u64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
        .map(|n| n.max(0) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::{Task, TaskMode};
    use r_code_store::{Database, TaskRepository};
    use tempfile::TempDir;

    fn setup_db_with_data() -> (TempDir, PathBuf, Database) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        // 插入 2 个任务
        let t1 = Task::new(Some("/proj".into()), "t1", "g1", TaskMode::Ask);
        let t2 = Task::new(Some("/proj".into()), "t2", "g2", TaskMode::Edit);
        TaskRepository::new(&db).create(&t1).unwrap();
        TaskRepository::new(&db).create(&t2).unwrap();
        (dir, db_path, db)
    }

    // ── generate / preview ────────────────────────────────────────

    #[tokio::test]
    async fn generate_creates_json_file() {
        let (dir, db_path, _db) = setup_db_with_data();
        let bundle = SupportBundle::new(dir.path().to_path_buf(), dir.path().join("logs"));
        let path = bundle.generate(&db_path).await.unwrap();
        assert!(path.exists());
        assert_eq!(path.extension().and_then(|s| s.to_str()), Some("json"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("version"));
        assert!(content.contains("platform"));
        assert!(content.contains("db_stats"));
        assert!(content.contains("task_count"));
        assert!(content.contains("\"task_count\": 2"));
    }

    #[tokio::test]
    async fn preview_returns_contents_without_writing() {
        let (dir, db_path, _db) = setup_db_with_data();
        // 输出目录使用嵌套路径，验证不写文件时不创建目录
        let bundle = SupportBundle::new(
            dir.path().join("nested").join("out"),
            dir.path().join("logs"),
        );
        let contents = bundle.preview(&db_path).await.unwrap();
        assert_eq!(contents.version, env!("CARGO_PKG_VERSION"));
        assert!(!contents.platform.is_empty());
        assert_eq!(contents.db_stats.task_count, 2);
        assert_eq!(contents.db_stats.run_count, 0);
        assert_eq!(contents.db_stats.tool_call_count, 0);
        // preview 不应创建输出目录
        assert!(!dir.path().join("nested").exists());
    }

    #[tokio::test]
    async fn db_stats_counts_runs_and_tool_calls() {
        let (dir, db_path, db) = setup_db_with_data();
        let conn = db.conn().unwrap();
        let task_id: String = conn
            .query_row("SELECT id FROM tasks LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO agent_runs (id, task_id, model, review_state, started_at) \
             VALUES ('r1', ?1, 'm', 'pending', ?2)",
            rusqlite::params![task_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tool_calls (id, run_id, task_id, tool_name, input_json, risk_level, status, started_at) \
             VALUES ('tc1', 'r1', ?1, 't', '{}', 'r0', 'running', ?2)",
            rusqlite::params![task_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();

        let bundle = SupportBundle::new(dir.path().to_path_buf(), dir.path().join("logs"));
        let contents = bundle.preview(&db_path).await.unwrap();
        assert_eq!(contents.db_stats.task_count, 2);
        assert_eq!(contents.db_stats.run_count, 1);
        assert_eq!(contents.db_stats.tool_call_count, 1);
    }

    // ── 日志脱敏 ──────────────────────────────────────────────────

    #[tokio::test]
    async fn logs_redacted_in_bundle() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join(format!(
            "{}.{}",
            crate::log_buffer::LOG_FILE_PREFIX,
            chrono::Utc::now().format("%Y-%m-%d")
        ));
        let entries = [
            crate::log_buffer::LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "WARN".into(),
                target: "r_code_agent_worker".into(),
                message: "Authorization: Bearer sk-secret123456".into(),
            },
            crate::log_buffer::LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "ERROR".into(),
                target: "r_code_gateway".into(),
                message: "token=abc456 model request failed".into(),
            },
            crate::log_buffer::LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "INFO".into(),
                target: "r_code_host".into(),
                message: "normal line".into(),
            },
        ];
        let log_content = entries
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(log_path, format!("{log_content}\n")).unwrap();

        let db_path = dir.path().join("test.db");
        let _db = Database::open(&db_path).unwrap();

        let bundle = SupportBundle::new(dir.path().to_path_buf(), log_dir);
        let contents = bundle.preview(&db_path).await.unwrap();

        // 所有日志条目不得包含原始敏感片段
        for entry in &contents.logs {
            assert!(
                !entry.message.contains("sk-secret123456"),
                "leak: {}",
                entry.message
            );
            assert!(!entry.message.contains("abc456"), "leak: {}", entry.message);
        }
        // 脱敏标记应存在
        let combined: String = contents.logs.iter().map(|e| e.message.as_str()).collect();
        assert!(
            combined.contains("Bearer ***") || combined.contains("Authorization: ***"),
            "expected redaction marker, got: {combined}"
        );
        assert!(combined.contains("token=***"));
        // 支持包只导出 warning/error，普通 info 不应混入。
        assert!(!combined.contains("normal line"));
        assert!(contents.logs.iter().all(|entry| {
            matches!(entry.level.as_str(), "warn" | "error")
                && !entry.timestamp.is_empty()
                && !entry.target.is_empty()
        }));
    }

    #[tokio::test]
    async fn preview_without_log_file_only_uses_safe_current_process_tail() {
        let (dir, db_path, _db) = setup_db_with_data();
        let bundle = SupportBundle::new(dir.path().to_path_buf(), dir.path().join("logs"));
        let contents = bundle.preview(&db_path).await.unwrap();
        assert!(contents
            .logs
            .iter()
            .all(|entry| matches!(entry.level.as_str(), "warn" | "error")));
    }

    // ── 配置摘要（无密钥）────────────────────────────────────────

    #[tokio::test]
    async fn config_summary_has_no_secrets() {
        let (dir, db_path, _db) = setup_db_with_data();
        let bundle = SupportBundle::new(dir.path().to_path_buf(), dir.path().join("logs"));
        let contents = bundle.preview(&db_path).await.unwrap();
        let cs = &contents.config_summary;
        assert!(!cs.default_provider.is_empty());
        // 配置摘要不得包含 API key 模式
        assert!(!cs.default_provider.contains("sk-"));
        assert!(cs.mcp_servers.is_empty());
        assert!(!cs.storage_dir.is_empty());
    }

    #[tokio::test]
    async fn mcp_support_summary_is_status_only() {
        let (dir, db_path, _db) = setup_db_with_data();
        let bundle = SupportBundle::new(dir.path().to_path_buf(), dir.path().join("logs"))
            .with_mcp_servers(vec![McpServerSupportSummary {
                id: "sample".to_string(),
                transport_kind: "stdio".to_string(),
                enabled: true,
                state: "error".to_string(),
                error_class: Some("connect_failed".to_string()),
            }]);
        let contents = bundle.preview(&db_path).await.unwrap();
        let json = serde_json::to_string(&contents).unwrap();

        assert!(json.contains("sample"));
        assert!(json.contains("connect_failed"));
        assert!(!json.contains("executable"));
        assert!(!json.contains("environment"));
        assert!(!json.contains("credential"));
    }

    // ── 降级 ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn generate_missing_db_returns_zero_stats() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("nonexistent.db");
        let bundle = SupportBundle::new(dir.path().to_path_buf(), dir.path().join("logs"));
        // 不存在的 DB 不应 panic，统计归零
        let contents = bundle.preview(&db_path).await.unwrap();
        assert_eq!(contents.db_stats.task_count, 0);
        assert_eq!(contents.db_stats.run_count, 0);
        assert_eq!(contents.db_stats.tool_call_count, 0);
    }

    #[tokio::test]
    async fn generate_creates_output_dir_if_missing() {
        let (dir, db_path, _db) = setup_db_with_data();
        let out_dir = dir.path().join("deep").join("nested").join("bundle");
        let bundle = SupportBundle::new(out_dir.clone(), dir.path().join("logs"));
        let path = bundle.generate(&db_path).await.unwrap();
        assert!(path.exists());
        assert!(out_dir.exists());
    }

    // ── 单元测试：纯函数 ──────────────────────────────────────────

    #[test]
    fn redact_text_helper_is_used() {
        // 验证 collect_logs 路径所依赖的 redact_text 行为
        let redacted = redact_text("key=sk-abc123");
        assert_eq!(redacted, "key=sk-***");
    }

    #[test]
    fn bundle_contents_is_serializable() {
        let contents = BundleContents {
            version: env!("CARGO_PKG_VERSION").into(),
            platform: "linux/x86_64".into(),
            generated_at: "2026-07-25T00:00:00Z".into(),
            logs: vec![LogEntry {
                level: "info".into(),
                message: "hello".into(),
                timestamp: "2026-07-25T00:00:00Z".into(),
                target: "r_code_host".into(),
            }],
            config_summary: ConfigSummary {
                default_provider: "anthropic".into(),
                mcp_servers: vec![],
                storage_dir: "/tmp".into(),
            },
            db_stats: DbStats {
                task_count: 1,
                run_count: 0,
                tool_call_count: 0,
            },
        };
        let json = serde_json::to_string(&contents).unwrap();
        assert!(json.contains("config_summary"));
        assert!(json.contains("db_stats"));
    }
}
