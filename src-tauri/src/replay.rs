//! Replay Service -- 三层深度会话回放。 [doc-01 §8] [doc-18 M11-04]
//!
//! 提供任务会话的三层深度回放：Recap / Explore / Verify。
//! 三层共享同一份事件与证据记录；切换深度时 playhead/过滤器/证据选择保持稳定。
//!
//! ## 核心原则
//! - **永不展示隐藏 chain-of-thought**：`ContentBlock::Thinking` 内容被过滤。
//! - **证据不确定时必须明确说明**：返回 "records cannot confirm" 而非猜测。
//! - **证据级别**：Verified > Recorded > Observed > Inferred > Missing。
//!
//! ## 证据级别定义
//! | 级别 | 说明 |
//! |------|------|
//! | Verified | 工具调用且存储了结果 |
//! | Recorded | 结构化记录（消息） |
//! | Observed | 观察（系统事件 / 普通 TUI） |
//! | Inferred | 推断（无直接记录） |
//! | Missing | 缺失（预期事件未找到） |
//!
//! [doc-01 §8] [doc-18 M11-05]

use std::path::PathBuf;

use agent_contract::{ContentBlock, Message, Role, SessionEvent, SessionMeta};
use r_code_core::error::ProductError;
use serde::{Deserialize, Serialize};

/// 回放深度级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayDepth {
    /// 概览视图 -- 高层摘要
    Recap,
    /// 详细视图 -- 浏览所有事件
    Explore,
    /// 证据视图 -- 用源数据验证
    Verify,
}

impl std::fmt::Display for ReplayDepth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recap => write!(f, "recap"),
            Self::Explore => write!(f, "explore"),
            Self::Verify => write!(f, "verify"),
        }
    }
}

impl ReplayDepth {
    /// 从字符串解析深度。
    pub fn try_from_str(s: &str) -> Option<Self> {
        match s {
            "recap" => Some(Self::Recap),
            "explore" => Some(Self::Explore),
            "verify" => Some(Self::Verify),
            _ => None,
        }
    }
}

/// 证据级别 -- 回放条目的可信度分级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    /// 已验证 -- 工具调用且存储了结果
    Verified,
    /// 结构化记录 -- 消息等结构化数据
    Recorded,
    /// 观察 -- 系统事件 / 普通 TUI 输出
    Observed,
    /// 推断 -- 无直接记录，基于上下文推断
    Inferred,
    /// 缺失 -- 预期事件未找到
    Missing,
}

impl std::fmt::Display for EvidenceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Verified => write!(f, "verified"),
            Self::Recorded => write!(f, "recorded"),
            Self::Observed => write!(f, "observed"),
            Self::Inferred => write!(f, "inferred"),
            Self::Missing => write!(f, "missing"),
        }
    }
}

impl EvidenceLevel {
    /// 从字符串解析证据级别。
    pub fn try_from_str(s: &str) -> Option<Self> {
        match s {
            "verified" => Some(Self::Verified),
            "recorded" => Some(Self::Recorded),
            "observed" => Some(Self::Observed),
            "inferred" => Some(Self::Inferred),
            "missing" => Some(Self::Missing),
            _ => None,
        }
    }
}

/// 回放条目 -- 回放时间线中的单个项目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayEntry {
    /// 事件类型（如 "meta", "message", "tool_call", "tool_result", "system"）
    pub event_type: String,
    /// 时间戳（RFC 3339）
    pub timestamp: String,
    /// 人类可读摘要
    pub summary: String,
    /// 证据级别
    pub evidence_level: EvidenceLevel,
    /// 详细信息（Verify 深度时填充；Recap/Explore 可能为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Replay 服务 -- 提供任务会话的三层深度回放。
///
/// 永不展示隐藏 chain-of-thought。证据不确定时明确说明 "records cannot confirm"。
pub struct ReplayService {
    sessions_dir: PathBuf,
}

impl ReplayService {
    /// 创建 ReplayService。
    ///
    /// `sessions_dir` 是 JSONL 会话文件所在目录（与 `SessionStore` 的 base_dir 一致）。
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    /// 获取指定深度的回放数据。
    ///
    /// 三层共享同一份事件与证据记录。切换深度时 playhead/过滤器/证据选择保持稳定。
    /// 永不包含 thinking/reasoning 内容。
    pub async fn get_replay(
        &self,
        session_id: &str,
        depth: ReplayDepth,
    ) -> Result<Vec<ReplayEntry>, ProductError> {
        let events = self.read_events(session_id).await?;

        // 收集是否有任何 ToolResult（用于判定 ToolCall 的证据级别）
        let has_any_result = events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolResult { .. }));

        // 统计信息（用于 Recap 摘要）
        let message_count = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::Message(_)))
            .count();
        let tool_call_count = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::ToolCall { .. }))
            .count();

        let mut entries = Vec::new();

        for (index, event) in events.iter().enumerate() {
            let entry = match event {
                SessionEvent::Meta(meta) => {
                    // Meta 只在 Recap 首条和 Explore/Verify 中包含
                    Some(ReplayEntry {
                        event_type: "meta".into(),
                        timestamp: meta.created_at.to_rfc3339(),
                        summary: format!(
                            "Session started (model: {}, provider: {})",
                            meta.model, meta.provider
                        ),
                        evidence_level: EvidenceLevel::Recorded,
                        details: if depth == ReplayDepth::Verify {
                            Some(serde_json::to_value(meta).unwrap_or_default())
                        } else {
                            None
                        },
                    })
                }

                // RequestHeader 是派发自检快照，回放不可见。
                SessionEvent::RequestHeader { .. } => None,

                SessionEvent::Message(msg) => {
                    // Recap: 只包含首条和末条消息
                    if depth == ReplayDepth::Recap {
                        let is_first = entries.is_empty()
                            || entries.iter().all(|e: &ReplayEntry| e.event_type == "meta");
                        let is_last_message = (index + 1..events.len())
                            .all(|i| !matches!(events[i], SessionEvent::Message(_)));
                        if !is_first && !is_last_message {
                            None
                        } else {
                            Some(self.message_to_entry(msg, depth, index))
                        }
                    } else {
                        Some(self.message_to_entry(msg, depth, index))
                    }
                }

                SessionEvent::ToolCall { name, input } => {
                    // Recap: 只统计，不逐条列出
                    if depth == ReplayDepth::Recap {
                        None
                    } else {
                        Some(ReplayEntry {
                            event_type: "tool_call".into(),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            summary: format!("Tool call: {name}"),
                            evidence_level: if has_any_result {
                                EvidenceLevel::Verified
                            } else {
                                EvidenceLevel::Observed
                            },
                            details: if depth == ReplayDepth::Verify {
                                Some(serde_json::json!({
                                    "name": name,
                                    "input": input,
                                }))
                            } else {
                                None
                            },
                        })
                    }
                }

                SessionEvent::ToolResult {
                    call_id,
                    output,
                    is_error,
                } => {
                    // Recap: 只统计，不逐条列出
                    if depth == ReplayDepth::Recap {
                        None
                    } else {
                        Some(ReplayEntry {
                            event_type: "tool_result".into(),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            summary: if *is_error {
                                format!("Tool result (error) for {call_id}")
                            } else {
                                format!("Tool result for {call_id}")
                            },
                            evidence_level: EvidenceLevel::Verified,
                            details: if depth == ReplayDepth::Verify {
                                Some(serde_json::json!({
                                    "call_id": call_id,
                                    "output": output,
                                    "is_error": is_error,
                                }))
                            } else {
                                None
                            },
                        })
                    }
                }

                SessionEvent::Usage(_) => {
                    // Usage 事件不展示在回放中（内部记账）
                    None
                }

                // 快照服务于 provider 上下文恢复，正文已经由原始 Message /
                // ToolCall / ToolResult 事件表达，不能在回放中重复展示。
                SessionEvent::HistorySnapshot { .. } => None,
                SessionEvent::ModelProjection { .. } => None,

                SessionEvent::System { event, data } => {
                    // Recap: 包含系统事件（状态变更）
                    Some(ReplayEntry {
                        event_type: "system".into(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        summary: format!("System: {event}"),
                        evidence_level: EvidenceLevel::Observed,
                        details: if depth == ReplayDepth::Verify {
                            Some(serde_json::json!({
                                "event": event,
                                "data": data,
                            }))
                        } else {
                            None
                        },
                    })
                }
            };

            if let Some(e) = entry {
                entries.push(e);
            }
        }

        // Recap: 追加统计摘要条目
        if depth == ReplayDepth::Recap {
            let recap_summary =
                format!("Recap: {message_count} message(s), {tool_call_count} tool call(s)");
            entries.push(ReplayEntry {
                event_type: "recap_summary".into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                summary: recap_summary,
                evidence_level: if message_count > 0 || tool_call_count > 0 {
                    EvidenceLevel::Recorded
                } else {
                    EvidenceLevel::Missing
                },
                details: None,
            });
        }

        // 若事件列表为空，返回一条 "records cannot confirm" 条目
        if entries.is_empty() {
            entries.push(ReplayEntry {
                event_type: "missing".into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                summary: "records cannot confirm: no events found for this session".into(),
                evidence_level: EvidenceLevel::Missing,
                details: None,
            });
        }

        Ok(entries)
    }

    /// 获取指定回放条目的证据。
    ///
    /// 返回该条目的详细证据数据。若证据不可用，返回 "records cannot confirm"。
    pub async fn get_evidence(
        &self,
        session_id: &str,
        entry_index: usize,
    ) -> Result<serde_json::Value, ProductError> {
        let entries = self.get_replay(session_id, ReplayDepth::Verify).await?;

        if entry_index >= entries.len() {
            return Ok(serde_json::json!({
                "available": false,
                "message": "records cannot confirm: entry index out of range"
            }));
        }

        let entry = &entries[entry_index];
        Ok(serde_json::json!({
            "available": entry.evidence_level != EvidenceLevel::Missing,
            "evidence_level": entry.evidence_level.to_string(),
            "event_type": entry.event_type,
            "summary": entry.summary,
            "details": entry.details,
            "note": if entry.evidence_level == EvidenceLevel::Missing {
                "records cannot confirm this event"
            } else {
                "evidence available"
            }
        }))
    }

    /// 读取会话的原始事件列表。
    ///
    /// 直接读取 JSONL 文件并逐行解析，保留所有事件类型。
    async fn read_events(&self, session_id: &str) -> Result<Vec<SessionEvent>, ProductError> {
        let path = self.sessions_dir.join(format!("{session_id}.jsonl"));
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|_| ProductError::Other(format!("session not found: {session_id}")))?;

        let mut events = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionEvent>(line) {
                Ok(event) => events.push(event),
                Err(_) => {
                    // 跳过无法解析的行（崩溃恢复场景）
                    tracing::warn!(session_id, "skipping unparseable session event line");
                }
            }
        }
        Ok(events)
    }

    /// 将 Message 转换为 ReplayEntry，过滤 Thinking 内容。
    fn message_to_entry(&self, msg: &Message, depth: ReplayDepth, index: usize) -> ReplayEntry {
        let role = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };

        // 过滤 Thinking 块，只保留非隐藏内容
        let visible_text: String = msg
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ToolUse { name, .. } => Some(format!("[tool: {name}]")),
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    if *is_error {
                        Some(format!("[error: {content}]"))
                    } else {
                        Some(format!("[result: {content}]"))
                    }
                }
                ContentBlock::Image { .. } => Some("[image]".to_string()),
                ContentBlock::File { source } => Some(format!("[file: {}]", source.name)),
                ContentBlock::Attachment { source } => {
                    Some(format!("[attachment: {}]", source.name))
                }
                ContentBlock::Custom { type_name, .. } => Some(format!("[{type_name}]")),
                // Thinking 块被过滤 -- 永不展示 chain-of-thought
                ContentBlock::Thinking { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        // 检查是否有被过滤的 Thinking 内容。DeepSeek 工具调用轮可能回传空明文
        // reasoning（仅为满足下一轮协议要求），这种空块没有需要隐藏的思维链，
        // 不应打出误导性的 [reasoning hidden] 标记；签名块则始终代表被隐藏内容。
        let has_hidden_thinking = msg.content.iter().any(|b| match b {
            ContentBlock::Thinking {
                thinking,
                signature,
            } => !thinking.trim().is_empty() || signature.is_some(),
            _ => false,
        });

        let summary = if has_hidden_thinking {
            format!("{role}: {visible_text} [reasoning hidden]")
        } else {
            format!("{role}: {visible_text}")
        };

        // 截断过长的摘要
        let summary = if summary.len() > 200 {
            format!("{}...", &summary[..197])
        } else {
            summary
        };

        ReplayEntry {
            event_type: "message".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            summary,
            evidence_level: EvidenceLevel::Recorded,
            details: if depth == ReplayDepth::Verify {
                Some(serde_json::json!({
                    "role": role.to_lowercase(),
                    "index": index,
                    "has_hidden_thinking": has_hidden_thinking,
                    "visible_content": visible_text,
                }))
            } else {
                None
            },
        }
    }
}

/// 从 SessionMeta 构造 ReplayEntry（用于会话起始标记）。
#[allow(dead_code)]
fn meta_to_summary(meta: &SessionMeta) -> String {
    match &meta.title {
        Some(title) => format!("Session: {title}"),
        None => format!("Session started (model: {})", meta.model),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_contract::{Message, SessionEvent, SessionMeta};
    use tempfile::TempDir;

    /// 创建临时 ReplayService 并写入测试事件。
    async fn setup_with_events(events: Vec<SessionEvent>) -> (TempDir, ReplayService, String) {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let svc = ReplayService::new(sessions_dir.clone());

        let session_id = "test-session-001".to_string();
        let path = sessions_dir.join(format!("{session_id}.jsonl"));
        let mut content = String::new();
        for event in &events {
            content.push_str(&serde_json::to_string(event).unwrap());
            content.push('\n');
        }
        std::fs::write(&path, content).unwrap();

        (dir, svc, session_id)
    }

    fn sample_meta() -> SessionMeta {
        SessionMeta {
            id: "test-session-001".to_string(),
            created_at: chrono::Utc::now(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            title: Some("Test Session".to_string()),
        }
    }

    #[tokio::test]
    async fn get_replay_recap_includes_summary() {
        let events = vec![
            SessionEvent::Meta(sample_meta()),
            SessionEvent::Message(Message::user_text("Hello")),
            SessionEvent::Message(Message::assistant_text("Hi there")),
        ];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let entries = svc.get_replay(&sid, ReplayDepth::Recap).await.unwrap();
        // Recap 应包含 meta + 首末消息 + 统计摘要
        assert!(entries.len() >= 2);
        assert!(entries.iter().any(|e| e.event_type == "recap_summary"));
    }

    #[tokio::test]
    async fn get_replay_explore_includes_all_events() {
        let events = vec![
            SessionEvent::Meta(sample_meta()),
            SessionEvent::Message(Message::user_text("Hello")),
            SessionEvent::Message(Message::assistant_text("Hi")),
            SessionEvent::ToolCall {
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "/a"}),
            },
        ];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let entries = svc.get_replay(&sid, ReplayDepth::Explore).await.unwrap();
        // Explore 应包含所有事件（不含 recap_summary）
        assert!(entries.iter().any(|e| e.event_type == "meta"));
        assert!(entries.iter().any(|e| e.event_type == "message"));
        assert!(entries.iter().any(|e| e.event_type == "tool_call"));
        assert!(!entries.iter().any(|e| e.event_type == "recap_summary"));
    }

    #[tokio::test]
    async fn get_replay_verify_includes_details() {
        let events = vec![
            SessionEvent::Meta(sample_meta()),
            SessionEvent::ToolCall {
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "/a"}),
            },
            SessionEvent::ToolResult {
                call_id: "c1".to_string(),
                output: serde_json::json!({"content": "hello"}),
                is_error: false,
            },
        ];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let entries = svc.get_replay(&sid, ReplayDepth::Verify).await.unwrap();
        // Verify 深度应有 details
        let tool_call_entry = entries
            .iter()
            .find(|e| e.event_type == "tool_call")
            .unwrap();
        assert!(tool_call_entry.details.is_some());
        let tool_result_entry = entries
            .iter()
            .find(|e| e.event_type == "tool_result")
            .unwrap();
        assert!(tool_result_entry.details.is_some());
    }

    #[tokio::test]
    async fn tool_result_is_verified() {
        let events = vec![
            SessionEvent::Meta(sample_meta()),
            SessionEvent::ToolCall {
                name: "read_file".to_string(),
                input: serde_json::json!({}),
            },
            SessionEvent::ToolResult {
                call_id: "c1".to_string(),
                output: serde_json::json!({"content": "data"}),
                is_error: false,
            },
        ];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let entries = svc.get_replay(&sid, ReplayDepth::Explore).await.unwrap();
        let tool_result = entries
            .iter()
            .find(|e| e.event_type == "tool_result")
            .unwrap();
        assert_eq!(tool_result.evidence_level, EvidenceLevel::Verified);
    }

    #[tokio::test]
    async fn message_is_recorded() {
        let events = vec![
            SessionEvent::Meta(sample_meta()),
            SessionEvent::Message(Message::user_text("Hello")),
        ];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let entries = svc.get_replay(&sid, ReplayDepth::Explore).await.unwrap();
        let msg = entries.iter().find(|e| e.event_type == "message").unwrap();
        assert_eq!(msg.evidence_level, EvidenceLevel::Recorded);
    }

    #[tokio::test]
    async fn thinking_content_is_filtered() {
        use agent_contract::{ContentBlock, Message, Role};
        let events = vec![
            SessionEvent::Meta(sample_meta()),
            SessionEvent::Message(Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "secret reasoning".to_string(),
                        signature: None,
                    },
                    ContentBlock::Text {
                        text: "Visible response".to_string(),
                    },
                ],
            }),
        ];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let entries = svc.get_replay(&sid, ReplayDepth::Explore).await.unwrap();
        let msg = entries.iter().find(|e| e.event_type == "message").unwrap();
        // 摘要不应包含 "secret reasoning"
        assert!(!msg.summary.contains("secret reasoning"));
        // 摘要应包含 "Visible response"
        assert!(msg.summary.contains("Visible response"));
        // 应标记 [reasoning hidden]
        assert!(msg.summary.contains("[reasoning hidden]"));
    }

    #[tokio::test]
    async fn empty_plaintext_reasoning_does_not_mark_reasoning_hidden() {
        use agent_contract::{ContentBlock, Message, Role};
        // DeepSeek 工具调用轮可能回传空 reasoning_text（仅为满足协议要求），
        // 它没有可隐藏的思维链，回放摘要不应打出误导性的 [reasoning hidden]。
        let events = vec![
            SessionEvent::Meta(sample_meta()),
            SessionEvent::Message(Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: String::new(),
                        signature: None,
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({"path": "/a"}),
                    },
                ],
            }),
        ];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let entries = svc.get_replay(&sid, ReplayDepth::Explore).await.unwrap();
        let msg = entries.iter().find(|e| e.event_type == "message").unwrap();
        assert!(!msg.summary.contains("[reasoning hidden]"));
    }

    #[tokio::test]
    async fn empty_session_returns_missing() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let svc = ReplayService::new(sessions_dir);

        // 不存在的会话 -> 应返回错误
        let result = svc.get_replay("nonexistent", ReplayDepth::Explore).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_evidence_returns_details() {
        let events = vec![
            SessionEvent::Meta(sample_meta()),
            SessionEvent::Message(Message::user_text("Hello")),
        ];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let evidence = svc.get_evidence(&sid, 1).await.unwrap();
        assert_eq!(evidence["available"], true);
        assert_eq!(evidence["evidence_level"], "recorded");
    }

    #[tokio::test]
    async fn get_evidence_out_of_range() {
        let events = vec![SessionEvent::Meta(sample_meta())];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let evidence = svc.get_evidence(&sid, 999).await.unwrap();
        assert_eq!(evidence["available"], false);
        assert!(evidence["message"]
            .as_str()
            .unwrap()
            .contains("cannot confirm"));
    }

    #[tokio::test]
    async fn usage_events_are_excluded() {
        use agent_contract::Usage;
        let events = vec![
            SessionEvent::Meta(sample_meta()),
            SessionEvent::Usage(Usage::new(100, 50)),
            SessionEvent::Message(Message::user_text("Hello")),
        ];
        let (_dir, svc, sid) = setup_with_events(events).await;

        let entries = svc.get_replay(&sid, ReplayDepth::Explore).await.unwrap();
        // Usage 事件不应出现在回放中
        assert!(!entries.iter().any(|e| e.event_type == "usage"));
    }

    #[test]
    fn replay_depth_parse() {
        assert_eq!(ReplayDepth::try_from_str("recap"), Some(ReplayDepth::Recap));
        assert_eq!(
            ReplayDepth::try_from_str("explore"),
            Some(ReplayDepth::Explore)
        );
        assert_eq!(
            ReplayDepth::try_from_str("verify"),
            Some(ReplayDepth::Verify)
        );
        assert_eq!(ReplayDepth::try_from_str("invalid"), None);
    }

    #[test]
    fn evidence_level_parse() {
        assert_eq!(
            EvidenceLevel::try_from_str("verified"),
            Some(EvidenceLevel::Verified)
        );
        assert_eq!(
            EvidenceLevel::try_from_str("missing"),
            Some(EvidenceLevel::Missing)
        );
        assert_eq!(EvidenceLevel::try_from_str("invalid"), None);
    }
}
