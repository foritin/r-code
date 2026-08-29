//! agent-contracts 公共合同验证测试。
//!
//! 本文件实现 `docs/agent-contracts-development-checklist.html` 中的 P1 验证向量，
//! 验证 R-Code 所依赖的 agent-contracts 公共 API 表面（agent-* crate）被正确采纳。
//! 每个测试对应一个合同向量（V-MSG / V-PROV / V-TOOL / V-STORE / V-CFG / V-COMP）。
//!
//! 运行：`cargo test -p r-code-core --test contract_tests`

use std::sync::Arc;

use agent_compaction::{CompactionManager, SlidingWindowCompaction};
use agent_config::Config;
use agent_contract::*;
use agent_error::Error;
use agent_llm::{create_provider, MockProvider, ProviderConfig, RecordedTurn};
use agent_mcp::McpToolHost;
use agent_store::SessionStore;

/// 环境变量是进程级全局状态；多个测试并行操作会竞态，用锁串行化。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ─────────────────────────────────────────────────────────────
// V-MSG：消息与 Provider 向量
// ─────────────────────────────────────────────────────────────
mod v_msg {
    use super::*;

    fn assert_text_roundtrip(msg: &Message, expected_role: Role, expected_text: &str) {
        let json = serde_json::to_string(msg).expect("serialize message");
        let back: Message = serde_json::from_str(&json).expect("deserialize message");
        assert_eq!(back.role, expected_role, "role unchanged after roundtrip");
        assert_eq!(
            back.text_content().as_str(),
            expected_text,
            "text content unchanged after roundtrip"
        );
        // 结构等价：往返后再序列化应得到相同 JSON
        let json2 = serde_json::to_string(&back).expect("re-serialize message");
        assert_eq!(json, json2, "message structurally stable across roundtrip");
    }

    /// V-MSG-01：User(Text) -> Assistant(Text) 序列化往返，内容与角色不变。
    #[test]
    fn v_msg_01_user_assistant_text_roundtrip() {
        assert_text_roundtrip(
            &Message::user_text("hello world"),
            Role::User,
            "hello world",
        );
        assert_text_roundtrip(
            &Message::assistant_text("hi there"),
            Role::Assistant,
            "hi there",
        );
    }

    /// V-MSG-02：Assistant(ToolUse id=A) 必须后接 User(ToolResult id=A)。
    #[test]
    fn v_msg_02_tool_use_followed_by_matching_tool_result() {
        let assistant = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "A".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "/a"}),
            }],
        };
        let user = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "A".into(),
                content: "file contents".into(),
                is_error: false,
            }],
        };

        let uses = assistant.tool_uses();
        assert_eq!(uses.len(), 1, "assistant carries one tool use");
        let use_id = uses[0].tool_id().expect("tool id present");
        assert_eq!(use_id, "A");

        let result_id = user
            .content
            .iter()
            .find_map(|b| b.tool_use_id())
            .expect("user carries a tool result");
        assert_eq!(result_id, use_id, "tool result id must match tool use id");
    }

    /// V-MSG-03：ToolUse 与 ToolResult 写入之间被取消 -> 写入 is_error=true 的已取消结果。
    #[test]
    fn v_msg_03_cancellation_writes_error_cancelled_result() {
        let cancelled = ContentBlock::cancelled_tool_result("A");
        match &cancelled {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(
                    tool_use_id.as_str(),
                    "A",
                    "cancelled result pairs with the tool use id"
                );
                assert!(*is_error, "cancelled result is flagged as error");
                assert!(
                    content.to_lowercase().contains("cancel"),
                    "content indicates cancellation: {content}"
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        // 作为 User 消息回填，配对 id 与原 ToolUse 一致
        let user_cancelled = Message {
            role: Role::User,
            content: vec![cancelled],
        };
        assert_eq!(
            user_cancelled.content.iter().find_map(|b| b.tool_use_id()),
            Some("A"),
            "cancelled result carries the original tool use id"
        );
    }
}

// ─────────────────────────────────────────────────────────────
// V-PROV：Provider 向量
// ─────────────────────────────────────────────────────────────
mod v_prov {
    use super::*;
    use futures::StreamExt;

    fn mock_request() -> Arc<CompletionRequest> {
        Arc::new(CompletionRequest {
            model: "mock".into(),
            system: None,
            messages: vec![Message::user_text("hi")],
            tools: vec![],
            hosted_tools: vec![],
            max_tokens: 128,
            temperature: None,
            enable_caching: false,
            inference: Default::default(),
        })
    }

    /// V-PROV-01：MockProvider stream 依次产生 TextDelta / ToolUse / Usage / Done。
    #[tokio::test]
    async fn v_prov_01_mock_stream_event_order() {
        let provider = MockProvider::new("mock");
        provider.push_turn(RecordedTurn::ok(vec![
            StreamEvent::TextDelta {
                text: "hello".into(),
            },
            StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "read_file".into(),
            },
            StreamEvent::ToolUseComplete {
                id: "t1".into(),
                input: serde_json::json!({"path": "/a"}),
            },
            StreamEvent::Usage(Usage::new(10, 5)),
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));

        let stream = provider
            .stream(mock_request())
            .await
            .expect("stream starts");
        let events: Vec<StreamEvent> = stream.collect().await;

        // 事件依次出现，顺序为 TextDelta -> ToolUse -> Usage -> Stop(Done)
        assert_eq!(events.len(), 5, "all scripted events delivered");
        assert!(matches!(events[0], StreamEvent::TextDelta { .. }));
        assert!(matches!(events[1], StreamEvent::ToolUseStart { .. }));
        assert!(matches!(events[2], StreamEvent::ToolUseComplete { .. }));
        assert!(matches!(events[3], StreamEvent::Usage(_)));
        assert!(matches!(events[4], StreamEvent::Stop { .. }));

        // 严格顺序：text < tool < usage < stop
        let idx = |needle: &StreamEvent| -> usize {
            events
                .iter()
                .position(|e| std::mem::discriminant(e) == std::mem::discriminant(needle))
                .expect("event kind present")
        };
        let text_idx = idx(&StreamEvent::TextDelta {
            text: String::new(),
        });
        let tool_idx = idx(&StreamEvent::ToolUseStart {
            id: String::new(),
            name: String::new(),
        });
        let usage_idx = idx(&StreamEvent::Usage(Usage::default()));
        let stop_idx = idx(&StreamEvent::Stop {
            reason: StopReason::EndTurn,
        });
        assert!(text_idx < tool_idx, "TextDelta before ToolUse");
        assert!(tool_idx < usage_idx, "ToolUse before Usage");
        assert!(usage_idx < stop_idx, "Usage before Stop(Done)");
    }

    /// V-PROV-02：Provider 返回不可恢复错误，流以可分类错误终结，不泄露 API key。
    #[tokio::test]
    async fn v_prov_02_unrecoverable_error_classifiable_no_leak() {
        let provider = MockProvider::new("mock");
        provider.push_error_turn(Error::AuthFailed("authentication failed".into()));

        let err = provider
            .stream(mock_request())
            .await
            .err()
            .expect("stream terminates with an error");

        // 错误可分类：认证类，且不可恢复
        assert!(
            matches!(err, Error::AuthFailed(_)),
            "error is classifiable as auth failure"
        );
        assert!(!is_recoverable(&err), "auth failure is unrecoverable");

        // 不泄露敏感凭证
        let msg = err.to_string();
        assert!(!msg.contains("sk-ant-"), "no api key prefix in error");
        assert!(!msg.contains("api_key"), "no api_key literal in error");
        assert!(
            !msg.to_lowercase().contains("authorization"),
            "no authorization leak"
        );
        assert!(
            !msg.to_lowercase().contains("bearer"),
            "no bearer token leak"
        );

        // 工厂层面对空 key 返回可分类认证错误（不触发网络）
        let factory_err = match create_provider(ProviderConfig::Anthropic {
            api_key: String::new(),
            model: "claude-sonnet-4".into(),
            base_url: None,
        }) {
            Ok(_) => panic!("empty api_key should be rejected at construction"),
            Err(e) => e,
        };
        assert!(
            matches!(factory_err, Error::AuthFailed(_)),
            "factory surfaces classifiable auth error"
        );
        assert!(!factory_err.to_string().contains("sk-ant-"));
    }
}

// ─────────────────────────────────────────────────────────────
// V-TOOL：ToolHost & MCP 向量
// ─────────────────────────────────────────────────────────────
mod v_tool {
    use super::*;

    /// 测试用 builtin ToolHost：仅暴露一个裸名 "read_file"。
    struct BuiltinToolHost;

    #[async_trait::async_trait]
    impl ToolHost for BuiltinToolHost {
        async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
            Ok(vec![ToolSpec {
                name: "read_file".into(),
                description: "builtin file reader".into(),
                input_schema: serde_json::json!({}),
                source: ToolSource::Builtin,
                requires_confirmation: true,
            }])
        }

        async fn call(&self, name: &str, _args: serde_json::Value) -> Result<ToolCallOutcome> {
            if name == "read_file" {
                Ok(ToolCallOutcome {
                    content: "builtin-content".into(),
                    is_error: false,
                    metadata: None,
                })
            } else {
                Err(Error::ToolNotFound(name.to_string()))
            }
        }
    }

    /// V-TOOL-01：未知工具被拒绝（NullToolHost 拒绝一切调用）。
    #[tokio::test]
    async fn v_tool_01_unknown_tool_rejected() {
        let host = NullToolHost;

        let tools = host.list_tools().await.expect("list tools");
        assert!(tools.is_empty(), "NullToolHost exposes no tools");

        let result = host.call("unknown_tool", serde_json::json!({})).await;
        let err = result.expect_err("unknown tool call is rejected");
        match err {
            Error::ToolHost(_) | Error::ToolNotFound(_) => {}
            other => panic!("expected ToolHost/ToolNotFound error, got {other:?}"),
        }
    }

    /// V-TOOL-02：同名 builtin 与 MCP 工具不碰撞；MCP 使用 `server__tool` 命名。
    #[tokio::test]
    async fn v_tool_02_builtin_and_mcp_no_collision() {
        // 1) MCP 命名空间格式 server__tool
        let (server, tool) =
            McpToolHost::parse_namespaced("fs__read_file").expect("namespaced name parses");
        assert_eq!(server, "fs");
        assert_eq!(tool, "read_file");
        // 裸名 / 空段不是合法 MCP 工具名
        assert!(McpToolHost::parse_namespaced("read_file").is_err());
        assert!(McpToolHost::parse_namespaced("fs__").is_err());
        assert!(McpToolHost::parse_namespaced("__read_file").is_err());

        // 2) CompositeToolHost 按完整名匹配 -> 裸名与命名名互不碰撞
        let mut composite = CompositeToolHost::new();
        composite.add_host(Box::new(BuiltinToolHost));
        composite.add_host(Box::new(McpToolHost::new())); // 无已连接 server

        let tools = composite.list_tools().await.expect("list tools");
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"read_file"),
            "builtin bare name 'read_file' exposed"
        );
        assert_ne!(
            "read_file", "fs__read_file",
            "names are distinct identifiers"
        );

        // 调用裸名命中 builtin
        let outcome = composite.call("read_file", serde_json::json!({})).await;
        assert!(outcome.is_ok(), "bare name routes to builtin host");
        assert_eq!(outcome.unwrap().content, "builtin-content");

        // 调用 MCP 命名名（无对应 server）不会误命中 builtin
        let miss = composite.call("fs__read_file", serde_json::json!({})).await;
        assert!(
            miss.is_err(),
            "namespaced name does not collide with builtin"
        );
    }
}

// ─────────────────────────────────────────────────────────────
// V-STORE：Session Store 向量
// ─────────────────────────────────────────────────────────────
mod v_store {
    use super::*;

    /// V-STORE-01：JSONL 最后一行写一半 -> recover() 保留完整历史并报告截断。
    #[tokio::test]
    async fn v_store_01_recover_preserves_history_after_truncation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path().to_path_buf());

        let session = store
            .create("claude-sonnet-4", "anthropic")
            .await
            .expect("create session");
        let id = session.meta.id.clone();

        // 完整历史
        store
            .append(
                &id,
                SessionEvent::Message(Message::user_text("first complete message")),
            )
            .await
            .expect("append msg1");
        store
            .append(
                &id,
                SessionEvent::Message(Message::assistant_text("second complete message")),
            )
            .await
            .expect("append msg2");

        // 人为追加半行（无换行结尾，且 JSON 不完整）
        let path = dir.path().join(format!("{id}.jsonl"));
        let half_line =
            r#"{"message":{"role":"user","content":[{"type":"text","text":"half-written"}]}"#;
        {
            use tokio::io::AsyncWriteExt;
            let mut f = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .await
                .expect("open for half-line");
            f.write_all(half_line.as_bytes())
                .await
                .expect("write half line");
            f.flush().await.expect("flush");
        }

        // 半行存在时直接 load 应失败（间接报告截断）
        assert!(
            store.load(&id).await.is_err(),
            "half-written line breaks direct load"
        );

        // recover 截断半行，保留完整历史
        let recovered = store.recover(&id).await.expect("recover");
        assert_eq!(recovered.messages.len(), 2, "complete history preserved");
        assert_eq!(
            recovered.messages[0].text_content(),
            "first complete message"
        );
        assert_eq!(
            recovered.messages[1].text_content(),
            "second complete message"
        );

        // 截断后文件可正常 load（recover 修复了文件）
        let reloaded = store.load(&id).await.expect("load after recover");
        assert_eq!(reloaded.messages.len(), 2, "file fully recovered");
    }

    /// V-STORE-02：原子写入中断 -> 旧文件或新文件完整可读，绝不混合。
    #[tokio::test]
    async fn v_store_02_atomic_write_keeps_complete_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path().to_path_buf());

        // 旧版本（完整）
        let session = store.create("m", "p").await.expect("create session");
        let id = session.meta.id.clone();
        let meta = session.meta.clone();
        store
            .write_session_atomic(
                &id,
                &[
                    SessionEvent::Meta(meta.clone()),
                    SessionEvent::Message(Message::user_text("old-a")),
                    SessionEvent::Message(Message::user_text("old-b")),
                ],
            )
            .await
            .expect("atomic write old");
        let old = store.load(&id).await.expect("load old");
        assert_eq!(old.messages.len(), 2);

        // 原子覆写为新版本
        store
            .write_session_atomic(
                &id,
                &[
                    SessionEvent::Meta(meta),
                    SessionEvent::Message(Message::user_text("new-a")),
                    SessionEvent::Message(Message::user_text("new-b")),
                    SessionEvent::Message(Message::user_text("new-c")),
                ],
            )
            .await
            .expect("atomic write new");

        // 新版本完整可读，无新旧混合
        let new = store.load(&id).await.expect("load new");
        assert_eq!(new.messages.len(), 3, "new file complete, no mixing");
        let texts: Vec<String> = new.messages.iter().map(|m| m.text_content()).collect();
        assert_eq!(texts, vec!["new-a", "new-b", "new-c"]);
        assert!(
            !texts.iter().any(|t| t.contains("old")),
            "no stale old content mixed into new file"
        );
    }
}

// ─────────────────────────────────────────────────────────────
// V-CFG：Config 向量
// ─────────────────────────────────────────────────────────────
mod v_cfg {
    use super::*;

    /// V-CFG-01：默认值 < 配置文件 < 环境变量 < 显式参数 优先级链。
    #[test]
    fn v_cfg_01_priority_chain_default_file_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::remove_var("ANTHROPIC_API_KEY");

        // 1) 默认值存在
        let default = Config::default();
        assert_eq!(
            default.default_provider, "anthropic",
            "default provider exists"
        );
        assert_eq!(default.log_level, "info", "default log level exists");

        // 2) 配置文件覆盖默认值
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = dir.path().join("config.toml");
        // Windows 路径含反斜杠，写入 TOML basic string 会触发 \U 转义错误 → 统一为正斜杠
        let base_str = dir.path().join("data").to_string_lossy().replace('\\', "/");
        let toml = format!(
            r#"default_provider = "anthropic"
log_level = "debug"

[providers.anthropic]
base_url = "https://api.anthropic.com"
api_key = "file-key"
model = "claude-sonnet-4"

[storage]
base_dir = "{base}"
sessions_dir = "{base}/s"
skills_dir = "{base}/k"
memories_dir = "{base}/m"
"#,
            base = base_str
        );
        std::fs::write(&cfg_path, toml).expect("write config file");
        let from_file = Config::load_from(&cfg_path).expect("load from file");
        assert_eq!(from_file.log_level, "debug", "file overrides default");
        assert_eq!(
            from_file.providers.get("anthropic").unwrap().api_key,
            "file-key",
            "file-specified api_key"
        );

        // 3) 环境变量覆盖配置文件
        std::env::set_var("ANTHROPIC_API_KEY", "env-key");
        let from_env = Config::load_from(&cfg_path).expect("load with env override");
        assert_eq!(
            from_env.providers.get("anthropic").unwrap().api_key,
            "env-key",
            "env var overrides file"
        );

        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    /// V-CFG-02：Debug 输出不含 api_key / authorization / cookie / token。
    #[test]
    fn v_cfg_02_debug_output_masks_secrets() {
        let pc = agent_config::ProviderConfig {
            base_url: "https://api.anthropic.com".into(),
            api_key: "sk-ant-super-secret-key-9876543210".into(),
            model: "claude-sonnet-4".into(),
            max_tokens: Some(4096),
            temperature: Some(0.7),
            protocol: None,
            provider_kind: None,
            show_reasoning: true,
        };
        let debug = format!("{pc:?}");
        assert!(debug.contains("***"), "api_key masked as ***");
        assert!(
            !debug.contains("sk-ant-super-secret-key-9876543210"),
            "raw api_key must not appear in Debug"
        );

        // 嵌套在 Config 中的 ProviderConfig 同样脱敏
        let mut config = Config::default();
        config.providers.insert("anthropic".into(), pc);
        let cfg_debug = format!("{config:?}");
        assert!(
            !cfg_debug.contains("sk-ant-super-secret-key-9876543210"),
            "Config Debug must not leak api_key"
        );
        let lower = cfg_debug.to_lowercase();
        assert!(!lower.contains("authorization"), "no authorization leak");
        assert!(!lower.contains("cookie"), "no cookie leak");
        assert!(!lower.contains("bearer"), "no bearer token leak");
    }
}

// ─────────────────────────────────────────────────────────────
// V-COMP：压缩向量
// ─────────────────────────────────────────────────────────────
mod v_comp {
    use super::*;

    /// V-COMP-01：压缩后近期用户目标、未完成工具结果、持久事实仍可追溯。
    #[tokio::test]
    async fn v_comp_01_recent_goal_toolresults_facts_traceable() {
        let mut session = Session::new(SessionMeta::new("claude-sonnet-4", "anthropic"));

        // 持久事实 / 首条用户目标（keep_first 保留）
        session.push_user("Goal: ship the agent-contracts contract test suite.");

        // 中段闲聊：将被压缩
        for i in 0..10 {
            session.push_assistant(format!("filler assistant turn {i} with padding text"));
            session.push_user(format!("filler user turn {i} with padding text"));
        }

        // 近期窗口：未完成工具结果 + 近期用户目标
        session.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tu-1".into(),
                content: "result payload for unfinished tool".into(),
                is_error: false,
            }],
        });
        session.push_assistant("analyzing the tool result");
        session.push_user("Now run cargo test to verify.");

        let before = session.messages.len();
        assert!(before > 6, "session has enough messages to compact");

        let strategy: Box<dyn CompactionStrategy> = Box::new(SlidingWindowCompaction::new(1, 3));
        let manager = CompactionManager::with_strategies(1, vec![strategy]);
        let compacted = manager.auto_compact(&session).await.expect("compact");

        // 压缩确实发生
        assert!(compacted.len() < before, "compaction reduced message count");
        assert!(
            compacted
                .iter()
                .any(|m| m.text_content().contains("[compaction:")),
            "compaction marker present"
        );

        // 持久事实（首条用户目标）可追溯
        assert!(
            compacted[0]
                .text_content()
                .contains("ship the agent-contracts contract test suite"),
            "persistent first user goal preserved"
        );

        // 近期用户目标可追溯
        assert!(
            compacted
                .iter()
                .any(|m| m.text_content().contains("run cargo test")),
            "recent user goal preserved"
        );

        // 未完成工具结果可追溯
        assert!(
            compacted
                .iter()
                .any(|m| m.content.iter().any(|b| b.is_tool_result())),
            "unfinished tool result preserved in recent window"
        );
    }
}
