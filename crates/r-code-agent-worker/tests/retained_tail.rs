//! retained_tail 自包含检查点与自包含恢复（PRD §4.1 R-SES-02/03 / M5-02/M5-03）。
//!
//! R-Code 的压缩检查点形态是 `SessionEvent::ModelProjection { messages }`——
//! **物化的保留消息**（retained_tail），不是 Pi 式 `firstKeptEntryId` 指针。
//! 这两个测试验证三件事：
//!
//! 1. **M5-02.A1**：新压缩安装投影后 JSONL 含物化 `model_projection` 行
//!    （`messages: Some`，自包含——恢复不需要回读更早的行）；
//! 2. **M5-02.A2**：旧格式兼容读——没有投影行的 JSONL（旧版本写入）加载后
//!    `model_projection == None`（等价"用 canonical 全量"，不报错）；
//! 3. **M5-03.A1**：自包含恢复一致性——从最后一个 `ModelProjection` 恢复的
//!    provider 上下文，与整段 JSONL 回溯重建（HistorySnapshot canonical 路径）
//!    的结果在投影覆盖范围内**逐字节一致**（serde_json 规范化字节比较）。

use agent_contract::message::Message;
use agent_contract::session::{SessionEvent, SessionMeta};
use agent_store::SessionStore;

fn canonical_bytes(messages: &[Message]) -> Vec<u8> {
    serde_json::to_vec(messages).expect("canonical serialize")
}

/// M5-02.A1：新压缩写物化 retained_tail（ModelProjection.messages = Some）。
#[tokio::test]
async fn new_compaction_writes_materialized_retained_tail() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let session_id = "m5-02-materialized";
    store
        .append(session_id, SessionEvent::Meta(SessionMeta::new("m", "p")))
        .await
        .unwrap();
    // 压缩前的 canonical 历史（快照物化）。
    let history = vec![
        Message::user_text("old-1"),
        Message::user_text("old-2"),
        Message::user_text("kept-tail"),
    ];
    store
        .append(
            session_id,
            SessionEvent::HistorySnapshot {
                messages: history.clone(),
            },
        )
        .await
        .unwrap();
    // 压缩安装投影：物化保留消息（retained_tail），无任何指针字段。
    let retained_tail = vec![Message::user_text("kept-tail")];
    store
        .append(
            session_id,
            SessionEvent::ModelProjection {
                messages: Some(retained_tail.clone()),
            },
        )
        .await
        .unwrap();

    let session = store.load(session_id).await.unwrap();
    // 投影物化就位（自包含：Some 且内容逐字节等于压缩时的保留消息）。
    let projection = session
        .model_projection
        .expect("projection must be materialized");
    assert_eq!(
        canonical_bytes(&projection),
        canonical_bytes(&retained_tail)
    );
    // canonical 全量不受压缩影响（证据不丢）。
    assert_eq!(
        canonical_bytes(&session.messages),
        canonical_bytes(&history)
    );

    // JSONL 行本身含物化 messages 数组（wire 证据）。
    let raw = std::fs::read_to_string(dir.path().join(format!("{session_id}.jsonl"))).unwrap();
    let projection_line = raw
        .lines()
        .find(|line| line.contains("model_projection"))
        .expect("model_projection line must exist");
    assert!(projection_line.contains("kept-tail"), "投影行物化保留消息");
    assert!(
        !projection_line.contains("firstKeptEntryId") && !projection_line.contains("first_kept"),
        "投影行不得是指针格式"
    );
}

/// M5-02.A2：旧格式兼容读——无投影行的 JSONL 加载后投影为 None（canonical 兜底）。
#[tokio::test]
async fn legacy_pointer_free_format_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let session_id = "m5-02-legacy";
    store
        .append(session_id, SessionEvent::Meta(SessionMeta::new("m", "p")))
        .await
        .unwrap();
    store
        .append(
            session_id,
            SessionEvent::HistorySnapshot {
                messages: vec![Message::user_text("only-history")],
            },
        )
        .await
        .unwrap();
    // 旧版本写入的会话没有 ModelProjection 行——加载不报错、投影 None
    // （等价"模型直接使用完整历史"，即加载时迁移为 canonical 路径）。
    let session = store.load(session_id).await.unwrap();
    assert!(session.model_projection.is_none());
    assert_eq!(session.messages.len(), 1);
    // 投影清除语义（新版本写 None = 回退 canonical）同样合法。
    store
        .append(session_id, SessionEvent::ModelProjection { messages: None })
        .await
        .unwrap();
    let session = store.load(session_id).await.unwrap();
    assert!(session.model_projection.is_none());
}

/// M5-03.A1：自包含恢复与回溯逐字节一致——从最后投影恢复的上下文与
/// canonical 回溯重建在投影范围内 serde_json 字节相同。
#[tokio::test]
async fn self_contained_recovery_matches_full_replay_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let session_id = "m5-03-recovery";

    // 构造带两轮压缩的长会话：canonical 8 条消息，最终投影保留 3 条尾部。
    let mut canonical = Vec::new();
    for index in 0..8 {
        canonical.push(Message::user_text(format!("msg-{index}")));
    }
    let first_projection = canonical[4..].to_vec(); // 第一次压缩保留 4..8
    let final_projection = canonical[5..].to_vec(); // 第二次压缩保留 5..8

    store
        .append(session_id, SessionEvent::Meta(SessionMeta::new("m", "p")))
        .await
        .unwrap();
    store
        .append(
            session_id,
            SessionEvent::HistorySnapshot {
                messages: canonical.clone(),
            },
        )
        .await
        .unwrap();
    store
        .append(
            session_id,
            SessionEvent::ModelProjection {
                messages: Some(first_projection),
            },
        )
        .await
        .unwrap();
    store
        .append(
            session_id,
            SessionEvent::ModelProjection {
                messages: Some(final_projection.clone()),
            },
        )
        .await
        .unwrap();

    // 恢复路径 A（自包含）：加载后直接取最后一个 ModelProjection。
    let session = store.load(session_id).await.unwrap();
    let recovered = session
        .model_projection
        .expect("final projection must exist");
    // 恢复路径 B（回溯）：从 canonical 全量按最终保留窗口重建。
    let replayed = &canonical[canonical.len() - recovered.len()..];
    // 逐字节一致（serde_json 规范化字节，非结构等价）。
    assert_eq!(
        canonical_bytes(&recovered),
        canonical_bytes(replayed),
        "自包含恢复与回溯重建必须逐字节一致"
    );
    // 恢复内容正确（最终投影生效，非首轮投影）。
    assert_eq!(recovered.len(), 3);
    assert_eq!(
        canonical_bytes(&recovered),
        canonical_bytes(&final_projection)
    );
}
