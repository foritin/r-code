//! 会话附件 JSONL 原子迁移器（docs/support/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §7.3）。
//!
//! 启动后的后台迁移器按单个 `storage_id` 串行处理：
//! 1. 计算源 JSONL SHA-256，在 `session_attachment_migrations` 写 pending；
//! 2. 完整解析事件；遇到一处损坏即把该会话标为 failed，源文件保持不变；
//! 3. 对每个二进制 Base64 解码、校验并 stage Blob（相同内容复用同一物理
//!    Blob；同 task 下同 hash 的既有 staged 行直接复用，崩溃重执行不重复
//!    递增 refcount）；
//! 4. 把 Message / HistorySnapshot / ModelProjection 中的二进制块改为
//!    `AttachmentRefV1`；`r_code_attachment_image` 预览事件的 `preview_id`
//!    同步改写为 staged 原图附件 id（预览改由 BlobStore 提供）；
//! 5. 原目录写临时 JSONL、flush、`sync_all`，重新解析验证（消息数、tool
//!    pairing、附件可读性、无 Base64）；
//! 6. 同目录原子 rename 替换活动 JSONL；
//! 7. `commit_many()`、记录 target SHA-256、状态置 committed。
//!
//! 崩溃恢复（§7.3）：`pending` 且活动文件仍为 source hash → 重新执行（幂等）；
//! `pending` 且活动文件为可验证 target（完整解析 + 无二进制 Base64 + 引用全部
//! 可解析）→ 补 commit；两者都不匹配 → failed，禁止自动覆盖。
//!
//! 任一步失败都不删除旧数据；只有任务的全部会话迁移 committed 且预览事件全部
//! 改写成功后，才清理旧 `{app_data}/attachments/{task_id}` 预览目录。

use std::collections::HashMap;
use std::path::Path;

use agent_contract::{AttachmentRefV1, ContentBlock, Message, SessionEvent};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use r_code_store::{AttachmentStore, Database, StageAttachment};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

/// 一次运行的汇总（诊断/日志用）。
#[derive(Debug, Default, Clone)]
pub struct AttachmentMigrationReport {
    pub scanned: usize,
    pub committed: usize,
    pub skipped: usize,
    pub failed: usize,
    /// (storage_id, 原因) 明细。
    pub failures: Vec<(String, String)>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn media_type_extension(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

fn task_for_storage(db: &Database, storage_id: &str) -> Option<String> {
    let conn = db.conn().ok()?;
    conn.query_row(
        "SELECT task_id FROM session_branches WHERE storage_id = ?1",
        params![storage_id],
        |row| row.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

fn task_state(db: &Database, task_id: &str) -> Option<String> {
    let conn = db.conn().ok()?;
    conn.query_row(
        "SELECT state FROM tasks WHERE id = ?1",
        params![task_id],
        |row| row.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// 迁移路径的 stage：同 task + 同 blob_hash 的既有 staged 行直接复用——
/// 崩溃后重执行（活动文件仍是 source）不会重复递增 blobs.ref_count。
fn stage_or_reuse(
    store: &AttachmentStore<'_>,
    db: &Database,
    task_id: &str,
    metadata: &StageAttachment,
    bytes: &[u8],
) -> Result<AttachmentRefV1, String> {
    let hash = blake3::hash(bytes).to_hex().to_string();
    let conn = db.conn().map_err(|error| error.to_string())?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM attachments \
             WHERE task_id = ?1 AND blob_hash = ?2 AND state = 'staged' LIMIT 1",
            params![task_id, hash],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some(attachment_id) = existing {
        let record = store
            .get_owned(task_id, &attachment_id)
            .map_err(|error| error.to_string())?;
        return Ok(record.to_ref_v1(agent_contract::AttachmentPurpose::NativeInput));
    }
    store
        .stage(task_id, metadata, bytes)
        .map_err(|error| error.to_string())
}

fn block_is_binary(block: &ContentBlock) -> bool {
    match block {
        ContentBlock::Image { .. } => true,
        ContentBlock::File { source } => source.data.is_some(),
        _ => false,
    }
}

fn message_has_binary(message: &Message) -> bool {
    message.content.iter().any(block_is_binary)
}

fn event_has_binary(event: &SessionEvent) -> bool {
    match event {
        SessionEvent::Message(message) => message_has_binary(message),
        SessionEvent::HistorySnapshot { messages } => messages.iter().any(message_has_binary),
        SessionEvent::ModelProjection {
            messages: Some(messages),
        } => messages.iter().any(message_has_binary),
        _ => false,
    }
}

enum MessageRewrite {
    Unchanged,
    Rewritten(Message),
}

/// 重写一条消息中的二进制块为附件引用。
fn rewrite_message(
    message: &Message,
    store: &AttachmentStore<'_>,
    db: &Database,
    task_id: &str,
    name_seq: &mut usize,
    staged_ids: &mut Vec<String>,
) -> Result<MessageRewrite, String> {
    if !message_has_binary(message) {
        return Ok(MessageRewrite::Unchanged);
    }
    let mut content = Vec::with_capacity(message.content.len());
    for block in &message.content {
        match block {
            ContentBlock::Image { source } => {
                let bytes = BASE64_STANDARD
                    .decode(source.data.as_bytes())
                    .map_err(|error| format!("图片 Base64 解码失败：{error}"))?;
                *name_seq += 1;
                let name = format!(
                    "image-{seq}.{ext}",
                    seq = *name_seq,
                    ext = media_type_extension(&source.media_type)
                );
                let reference = stage_or_reuse(
                    store,
                    db,
                    task_id,
                    &StageAttachment {
                        name,
                        media_type: source.media_type.clone(),
                    },
                    &bytes,
                )?;
                staged_ids.push(reference.attachment_id.clone());
                content.push(ContentBlock::Attachment { source: reference });
            }
            ContentBlock::File { source } if source.data.is_some() => {
                let bytes = BASE64_STANDARD
                    .decode(source.data.as_deref().unwrap_or_default().as_bytes())
                    .map_err(|error| format!("{} Base64 解码失败：{error}", source.name))?;
                let reference = stage_or_reuse(
                    store,
                    db,
                    task_id,
                    &StageAttachment {
                        name: source.name.clone(),
                        media_type: source.media_type.clone(),
                    },
                    &bytes,
                )?;
                staged_ids.push(reference.attachment_id.clone());
                content.push(ContentBlock::Attachment { source: reference });
            }
            other => content.push(other.clone()),
        }
    }
    Ok(MessageRewrite::Rewritten(Message {
        role: message.role,
        content,
    }))
}

/// 重写 `r_code_attachment_image` 预览事件：preview_id → staged 原图附件 id。
/// 预览文件缺失时保留原 preview_id（任务预览目录清理对该任务跳过）。
fn rewrite_preview_event(
    store: &AttachmentStore<'_>,
    db: &Database,
    task_id: &str,
    previews_root: &Path,
    data: &serde_json::Value,
    staged_ids: &mut Vec<String>,
    unresolved: &mut usize,
) -> serde_json::Value {
    let Some(entries) = data.as_array() else {
        return data.clone();
    };
    let mut next = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut map = match entry.as_object() {
            Some(map) => map.clone(),
            None => {
                next.push(entry.clone());
                continue;
            }
        };
        let preview_id = map.get("preview_id").and_then(|v| v.as_str()).unwrap_or("");
        let media_type = map
            .get("media_type")
            .and_then(|v| v.as_str())
            .unwrap_or("image/png");
        let name = map.get("name").and_then(|v| v.as_str()).unwrap_or("image");
        let path = previews_root.join(task_id).join(format!(
            "{preview_id}.{ext}",
            ext = media_type_extension(media_type)
        ));
        let staged = std::fs::read(&path).ok().and_then(|bytes| {
            stage_or_reuse(
                store,
                db,
                task_id,
                &StageAttachment {
                    name: name.to_string(),
                    media_type: media_type.to_string(),
                },
                &bytes,
            )
            .ok()
        });
        match staged {
            Some(reference) => {
                staged_ids.push(reference.attachment_id.clone());
                map.insert(
                    "preview_id".to_string(),
                    serde_json::Value::String(reference.attachment_id),
                );
            }
            None => *unresolved += 1,
        }
        next.push(serde_json::Value::Object(map));
    }
    serde_json::Value::Array(next)
}

struct RewritePlan {
    events: Vec<SessionEvent>,
    staged_ids: Vec<String>,
    unresolved_previews: usize,
}

/// 逐事件重写（步骤 3~4）。输入是已完整解析的事件，输出可序列化的改写结果。
fn rewrite_events(
    events: Vec<SessionEvent>,
    store: &AttachmentStore<'_>,
    db: &Database,
    task_id: &str,
    previews_root: &Path,
) -> Result<RewritePlan, String> {
    let mut out = Vec::with_capacity(events.len());
    let mut staged_ids = Vec::new();
    let mut unresolved_previews = 0usize;
    let mut name_seq = 0usize;
    for mut event in events {
        match &mut event {
            SessionEvent::Message(message) => {
                if let MessageRewrite::Rewritten(rewritten) =
                    rewrite_message(message, store, db, task_id, &mut name_seq, &mut staged_ids)?
                {
                    *message = rewritten;
                }
            }
            SessionEvent::HistorySnapshot { messages } => {
                let mut next = Vec::with_capacity(messages.len());
                for message in messages.iter() {
                    match rewrite_message(
                        message,
                        store,
                        db,
                        task_id,
                        &mut name_seq,
                        &mut staged_ids,
                    )? {
                        MessageRewrite::Rewritten(rewritten) => next.push(rewritten),
                        MessageRewrite::Unchanged => next.push(message.clone()),
                    }
                }
                *messages = next;
            }
            SessionEvent::ModelProjection {
                messages: Some(projected),
            } => {
                let mut next = Vec::with_capacity(projected.len());
                for message in projected.iter() {
                    match rewrite_message(
                        message,
                        store,
                        db,
                        task_id,
                        &mut name_seq,
                        &mut staged_ids,
                    )? {
                        MessageRewrite::Rewritten(rewritten) => next.push(rewritten),
                        MessageRewrite::Unchanged => next.push(message.clone()),
                    }
                }
                *projected = next;
            }
            SessionEvent::System { event, data } if event == "r_code_attachment_image" => {
                let next = rewrite_preview_event(
                    store,
                    db,
                    task_id,
                    previews_root,
                    data,
                    &mut staged_ids,
                    &mut unresolved_previews,
                );
                if next != *data {
                    *data = next;
                }
            }
            _ => {}
        }
        out.push(event);
    }
    Ok(RewritePlan {
        events: out,
        staged_ids,
        unresolved_previews,
    })
}

/// 事件统计（迁移前后必须一致的消息数 / tool pairing）。
#[derive(Debug, Default, PartialEq, Eq)]
struct EventStats {
    events: usize,
    messages: usize,
    tool_uses: usize,
    tool_results: usize,
}

fn count_message(message: &Message, stats: &mut EventStats) {
    stats.messages += 1;
    for block in &message.content {
        match block {
            ContentBlock::ToolUse { .. } => stats.tool_uses += 1,
            ContentBlock::ToolResult { .. } => stats.tool_results += 1,
            _ => {}
        }
    }
}

fn stats_of(events: &[SessionEvent]) -> EventStats {
    let mut stats = EventStats::default();
    for event in events {
        stats.events += 1;
        match event {
            SessionEvent::Message(message) => count_message(message, &mut stats),
            SessionEvent::HistorySnapshot { messages } => {
                for message in messages {
                    count_message(message, &mut stats);
                }
            }
            SessionEvent::ModelProjection {
                messages: Some(messages),
            } => {
                for message in messages {
                    count_message(message, &mut stats);
                }
            }
            _ => {}
        }
    }
    stats
}

fn refs_of_message<'a>(message: &'a Message, out: &mut Vec<&'a AttachmentRefV1>) {
    for block in &message.content {
        if let ContentBlock::Attachment { source } = block {
            out.push(source);
        }
    }
}

fn collect_refs(event: &SessionEvent) -> Vec<&AttachmentRefV1> {
    let mut out = Vec::new();
    match event {
        SessionEvent::Message(message) => refs_of_message(message, &mut out),
        SessionEvent::HistorySnapshot { messages } => {
            for message in messages {
                refs_of_message(message, &mut out);
            }
        }
        SessionEvent::ModelProjection {
            messages: Some(messages),
        } => {
            for message in messages {
                refs_of_message(message, &mut out);
            }
        }
        _ => {}
    }
    out
}

/// 步骤 5 的验证半边：重写文本可完整解析、统计一致、无二进制 Base64、引用
/// 全部可解析。
fn verify_rewritten(
    rewritten: &str,
    expected: &EventStats,
    store: &AttachmentStore<'_>,
    task_id: &str,
) -> Result<(), String> {
    let events = parse_events(rewritten)?;
    let actual = stats_of(&events);
    if actual != *expected {
        return Err(format!(
            "重写后统计漂移：events {}/{} messages {}/{} tool_uses {}/{} tool_results {}/{}",
            actual.events,
            expected.events,
            actual.messages,
            expected.messages,
            actual.tool_uses,
            expected.tool_uses,
            actual.tool_results,
            expected.tool_results
        ));
    }
    if events.iter().any(event_has_binary) {
        return Err("重写后仍存在二进制 Base64 块".to_string());
    }
    for event in &events {
        for reference in collect_refs(event) {
            store
                .get_owned(task_id, &reference.attachment_id)
                .map_err(|error| format!("附件 {} 不可解析：{error}", reference.attachment_id))?;
        }
    }
    Ok(())
}

fn parse_events(source: &str) -> Result<Vec<SessionEvent>, String> {
    let mut events = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: SessionEvent = serde_json::from_str(line)
            .map_err(|error| format!("第 {} 行解析失败：{error}", index + 1))?;
        events.push(event);
    }
    Ok(events)
}

enum StorageOutcome {
    Skipped,
    Committed {
        task_id: String,
        unresolved_previews: usize,
    },
}

fn mark_failed(store: &AttachmentStore<'_>, storage_id: &str, reason: &str) -> String {
    if let Err(error) = store.migration_mark_failed(storage_id, reason) {
        return format!("{reason}（标记 failed 也失败：{error}）");
    }
    reason.to_string()
}

/// 单个 storage 的迁移（含崩溃恢复判定）。
fn migrate_storage(
    db: &Database,
    blobs_dir: &Path,
    sessions_dir: &Path,
    storage_id: &str,
) -> Result<StorageOutcome, String> {
    let store = AttachmentStore::new(db, blobs_dir.to_path_buf());
    let path = sessions_dir.join(format!("{storage_id}.jsonl"));
    let source =
        std::fs::read_to_string(&path).map_err(|error| format!("读取会话文件失败：{error}"))?;
    let source_hash = sha256_hex(source.as_bytes());

    // 终态行直接跳过（failed 禁止自动覆盖，诊断页介入）。
    if let Some((state, row_source, _, error)) = store
        .migration_state(storage_id)
        .map_err(|e| e.to_string())?
    {
        match state.as_str() {
            "committed" => return Ok(StorageOutcome::Skipped),
            "failed" => {
                tracing::debug!(
                    storage_id,
                    prior_error = error.as_deref().unwrap_or_default(),
                    "session attachment migration previously failed; leaving file untouched"
                );
                return Ok(StorageOutcome::Skipped);
            }
            // pending：崩溃恢复判定（§7.3）。
            _ => {
                if source_hash != row_source {
                    return recover_pending_target(&store, db, &source, storage_id);
                }
                // 活动 == source：重新执行（stage_or_reuse 幂等，不重复 refcount）。
            }
        }
    }

    // 步骤 2：完整解析；一处损坏即 failed，源文件保持不变。
    let events = match parse_events(&source) {
        Ok(events) => events,
        Err(error) => {
            store
                .migration_mark_pending(storage_id, &source_hash)
                .map_err(|e| e.to_string())?;
            return Err(mark_failed(&store, storage_id, &error));
        }
    };

    let fail_with = |store: &AttachmentStore<'_>, reason: &str| -> String {
        store.migration_mark_pending(storage_id, &source_hash).ok();
        mark_failed(store, storage_id, reason)
    };

    let Some(task_id) = task_for_storage(db, storage_id) else {
        return Err(fail_with(&store, "session branch not found"));
    };
    match task_state(db, &task_id).as_deref() {
        None => return Err(fail_with(&store, "task no longer exists")),
        Some("archived") => return Err(fail_with(&store, "task archived")),
        Some(_) => {}
    }

    let previews_root = sessions_dir
        .parent()
        .map(|base| base.join("attachments"))
        .unwrap_or_else(|| std::path::PathBuf::from("attachments"));

    // 步骤 1：pending 行（改写/staging 之前落盘）。
    store
        .migration_mark_pending(storage_id, &source_hash)
        .map_err(|e| e.to_string())?;

    // 无二进制 Base64 且无预览事件：直接以 source 为 target 标 committed。
    let needs_work = events.iter().any(event_has_binary)
        || events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::System { event, .. } if event == "r_code_attachment_image"
            )
        });
    if !needs_work {
        store
            .migration_mark_committed(storage_id, &source_hash)
            .map_err(|e| e.to_string())?;
        return Ok(StorageOutcome::Committed {
            task_id,
            unresolved_previews: 0,
        });
    }

    // 步骤 3~4：stage + 改写。
    let plan = rewrite_events(events, &store, db, &task_id, &previews_root)?;
    let expected = stats_of(&parse_events(&source)?);
    let mut rewritten = String::with_capacity(source.len());
    for event in &plan.events {
        rewritten.push_str(
            &serde_json::to_string(event).map_err(|error| format!("序列化事件失败：{error}"))?,
        );
        rewritten.push('\n');
    }
    verify_rewritten(&rewritten, &expected, &store, &task_id)?;

    // 步骤 5：同目录临时文件 + flush + sync_all。
    let temp_path = sessions_dir.join(format!("{storage_id}.jsonl.migrate-tmp"));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp_path)
            .map_err(|error| format!("创建迁移临时文件失败：{error}"))?;
        file.write_all(rewritten.as_bytes())
            .map_err(|error| format!("写迁移临时文件失败：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync 迁移临时文件失败：{error}"))?;
    }
    // 步骤 6：同目录原子 rename。
    std::fs::rename(&temp_path, &path).map_err(|error| format!("原子替换会话文件失败：{error}"))?;
    // 步骤 7：commit + 记录 target + committed。
    let target_hash = sha256_hex(rewritten.as_bytes());
    store
        .commit_many(&task_id, &plan.staged_ids)
        .map_err(|e| e.to_string())?;
    store
        .migration_mark_committed(storage_id, &target_hash)
        .map_err(|e| e.to_string())?;
    Ok(StorageOutcome::Committed {
        task_id,
        unresolved_previews: plan.unresolved_previews,
    })
}

/// pending 崩溃恢复：活动文件不是 source hash 时，验证它是可迁移 target 则补
/// commit；否则标 failed（禁止自动覆盖）。
fn recover_pending_target(
    store: &AttachmentStore<'_>,
    db: &Database,
    source: &str,
    storage_id: &str,
) -> Result<StorageOutcome, String> {
    let Some(task_id) = task_for_storage(db, storage_id) else {
        return Err(mark_failed(
            store,
            storage_id,
            "recovery: session branch not found",
        ));
    };
    let events = match parse_events(source) {
        Ok(events) => events,
        Err(error) => {
            return Err(mark_failed(
                store,
                storage_id,
                &format!("recovery: {error}"),
            ));
        }
    };
    if events.iter().any(event_has_binary) {
        return Err(mark_failed(
            store,
            storage_id,
            "recovery: file is neither source nor a clean target",
        ));
    }
    // 可验证 target：引用全部可解析 → 补 commit（§7.3 恢复分支 2）。
    let mut staged_ids = Vec::new();
    for event in &events {
        for reference in collect_refs(event) {
            if store.get_owned(&task_id, &reference.attachment_id).is_err() {
                return Err(mark_failed(
                    store,
                    storage_id,
                    "recovery: attachment reference does not resolve",
                ));
            }
            staged_ids.push(reference.attachment_id.clone());
        }
    }
    store
        .commit_many(&task_id, &staged_ids)
        .map_err(|e| e.to_string())?;
    store
        .migration_mark_committed(storage_id, &sha256_hex(source.as_bytes()))
        .map_err(|e| e.to_string())?;
    Ok(StorageOutcome::Committed {
        task_id,
        unresolved_previews: 0,
    })
}

/// 入口：扫描 sessions 目录并逐个迁移。同步阻塞（文件 + SQLite IO），由宿主
/// 在 `spawn_blocking` 中调用。
pub fn run_session_attachment_migrations(
    db: &Database,
    blobs_dir: &Path,
    sessions_dir: &Path,
) -> AttachmentMigrationReport {
    let mut report = AttachmentMigrationReport::default();
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return report;
    };
    let mut storage_ids = Vec::new();
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.ends_with(".migrate-tmp") {
            // 上一轮 rename 前崩溃的残留：重执行会重写，直接安全删除。
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let Some(storage_id) = name.strip_suffix(".jsonl") else {
            continue;
        };
        storage_ids.push(storage_id.to_string());
    }
    storage_ids.sort();
    // task → (committed, failed, unresolved_previews)；全部 committed 且无未解析
    // 预览时才清理该任务的预览目录。
    let mut task_outcomes: HashMap<String, (usize, usize, usize)> = HashMap::new();
    for storage_id in &storage_ids {
        report.scanned += 1;
        match migrate_storage(db, blobs_dir, sessions_dir, storage_id) {
            Ok(StorageOutcome::Skipped) => report.skipped += 1,
            Ok(StorageOutcome::Committed {
                task_id,
                unresolved_previews,
            }) => {
                report.committed += 1;
                let entry = task_outcomes.entry(task_id).or_insert((0, 0, 0));
                entry.0 += 1;
                entry.2 += unresolved_previews;
            }
            Err(error) => {
                report.failed += 1;
                if let Some(task_id) = task_for_storage(db, storage_id) {
                    task_outcomes.entry(task_id).or_insert((0, 0, 0)).1 += 1;
                }
                tracing::warn!(storage_id, "session attachment migration failed: {error}");
                report.failures.push((storage_id.clone(), error));
            }
        }
    }
    if let Some(previews_root) = sessions_dir.parent().map(|base| base.join("attachments")) {
        for (task_id, (committed, failed, unresolved)) in &task_outcomes {
            if *failed == 0 && *unresolved == 0 && *committed > 0 {
                let dir = previews_root.join(task_id);
                if dir.is_dir() {
                    if let Err(error) = std::fs::remove_dir_all(&dir) {
                        tracing::warn!(task_id, "预览目录清理失败（下轮重试）：{error}");
                    }
                }
            }
        }
    }
    report
}

/// 启动 GC（docs §4.3/§7.5）：
/// 1. 回收过期 staged 附件——删除前用 `collect_referenced_attachment_ids`
///    扫描活动 JSONL 与排队载荷，防止「消息已落盘但 commit 标记未写」的崩溃
///    窗口误删附件；
/// 2. 清理孤儿预览目录（`{app_data}/attachments/{task_id}` 中任务已不存在者）。
///
/// 必须在 `run_session_attachment_migrations` 之后调用（迁移会补 commit 引用，
/// 避免把 pending 恢复窗口内的附件当过期回收）。
pub fn run_startup_attachment_gc(db: &Database, blobs_dir: &Path, sessions_dir: &Path) {
    let referenced = collect_referenced_attachment_ids(db, sessions_dir);
    let store = AttachmentStore::new(db, blobs_dir.to_path_buf());
    match store.gc_expired_staged(chrono::Utc::now(), &referenced) {
        Ok(collected) if !collected.is_empty() => {
            tracing::info!(
                count = collected.len(),
                "garbage-collected expired staged attachments"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "attachment staged GC failed; retry at next startup"),
    }

    // 孤儿预览目录：任务已删除但目录残留（任务删除的磁盘清理是最佳努力）。
    let Some(previews_root) = sessions_dir.parent().map(|base| base.join("attachments")) else {
        return;
    };
    let Ok(conn) = db.conn() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&previews_root) else {
        return;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(task_id) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM tasks WHERE id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if exists.is_none() {
            if let Err(error) = std::fs::remove_dir_all(&dir) {
                tracing::warn!(task_id, "孤儿预览目录清理失败（下轮重试）：{error}");
            }
        }
    }
}

/// 引用扫描（启动 GC 用，§4.3）：活动 JSONL + 排队消息中仍被引用的附件 id。
/// JSONL 侧用字符串匹配 `"attachment_id":"..."`（引用块/预览事件共用该键），
/// 只需要"是否仍被引用"这一布尔结论，无需完整解析。
pub fn collect_referenced_attachment_ids(db: &Database, sessions_dir: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sessions_dir) {
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            ids.extend(extract_attachment_ids(&content));
        }
    }
    if let Ok(conn) = db.conn() {
        if let Ok(mut statement) = conn.prepare("SELECT attachments_json FROM queued_messages") {
            if let Ok(rows) = statement.query_map([], |row| row.get::<_, Option<String>>(0)) {
                for json in rows.filter_map(|row| row.ok()).flatten() {
                    ids.extend(extract_attachment_ids(&json));
                }
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// 从任意 JSON 文本提取 `"attachment_id":"<value>"` 值（GC 引用扫描专用）。
fn extract_attachment_ids(content: &str) -> Vec<String> {
    const NEEDLE: &str = "\"attachment_id\":\"";
    let mut ids = Vec::new();
    let mut cursor = 0usize;
    while let Some(start) = content[cursor..].find(NEEDLE) {
        let value_start = cursor + start + NEEDLE.len();
        let Some(len) = content[value_start..].find('"') else {
            break;
        };
        ids.push(content[value_start..value_start + len].to_string());
        cursor = value_start + len;
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_contract::Role;
    use r_code_store::migrations::run_migrations;
    use rusqlite::Connection;

    fn setup() -> (r_code_store::Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = r_code_store::Database::open(dir.path().join("test.db")).unwrap();
        run_migrations(&db.conn().unwrap()).unwrap();
        (db, dir)
    }

    fn seed_task(conn: &Connection, task_id: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, goal, state, mode, agent_engine, created_at, updated_at) \
             VALUES (?1, 't', 'g', 'active', 'auto', 'r_code', ?2, ?2)",
            params![task_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_branches (id, task_id, storage_id, is_active, created_at) \
             VALUES ('b1', ?1, 's1', 1, ?2)",
            params![task_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();
    }

    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
            0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xde,
        ]
    }

    fn legacy_session_jsonl() -> String {
        let png_b64 = BASE64_STANDARD.encode(tiny_png());
        let user = Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "看这张图".to_string(),
                },
                ContentBlock::Image {
                    source: agent_contract::ImageSource {
                        kind: "base64".to_string(),
                        media_type: "image/png".to_string(),
                        data: png_b64,
                    },
                },
            ],
        };
        let assistant = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "t1".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "a"}),
            }],
        };
        let tool_result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".to_string(),
                content: "ok".to_string(),
                is_error: false,
            }],
        };
        format!(
            "{}\n{}\n",
            serde_json::to_string(&SessionEvent::Message(user)).unwrap(),
            serde_json::to_string(&SessionEvent::HistorySnapshot {
                messages: vec![assistant, tool_result]
            })
            .unwrap(),
        )
    }

    /// §7.3 全流程：Base64 → 引用；迁移后无 Base64、统计一致、引用可解析、
    /// 状态 committed；再次运行幂等（skipped）。
    #[test]
    fn migrates_legacy_jsonl_to_attachment_refs_atomically() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(sessions_dir.join("s1.jsonl"), legacy_session_jsonl()).unwrap();

        let report =
            run_session_attachment_migrations(&db, &dir.path().join("blobs"), &sessions_dir);
        assert_eq!(report.committed, 1);
        assert!(report.failures.is_empty(), "{:?}", report.failures);

        let migrated = std::fs::read_to_string(sessions_dir.join("s1.jsonl")).unwrap();
        assert!(!migrated.contains(&BASE64_STANDARD.encode(tiny_png())));
        assert!(migrated.contains("\"type\":\"attachment\""));
        let events = parse_events(&migrated).unwrap();
        let stats = stats_of(&events);
        assert_eq!(stats.messages, 3);
        assert_eq!(stats.tool_uses, 1);
        assert_eq!(stats.tool_results, 1);

        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        let (state, _, _, _) = store.migration_state("s1").unwrap().unwrap();
        assert_eq!(state, "committed");
        for reference in events.iter().flat_map(collect_refs) {
            assert!(store.get_owned("task-1", &reference.attachment_id).is_ok());
        }

        // 幂等：再次运行 → skipped（无重复 refcount / 重复 Blob）。
        let report =
            run_session_attachment_migrations(&db, &dir.path().join("blobs"), &sessions_dir);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.committed, 0);
    }

    /// 同一 Base64 图片出现在两条消息：stage_or_reuse 复用同一 staged 行，
    /// blobs.ref_count 只按 stage 次数计入，物理 Blob 仍只有一份。
    #[test]
    fn crash_reexecution_is_refcount_neutral_via_stage_reuse() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        // 两条消息携带相同图片 + 一个 pending 行（模拟 rename 前崩溃：活动文件
        // 仍是 source，重执行必须幂等）。
        std::fs::write(sessions_dir.join("s1.jsonl"), legacy_session_jsonl()).unwrap();
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        // 预置一个"崩溃残留"的 staged 行（内容与文件中的图片相同）。
        let leftover = store
            .stage(
                "task-1",
                &StageAttachment {
                    name: "image-1.png".to_string(),
                    media_type: "image/png".to_string(),
                },
                &tiny_png(),
            )
            .unwrap();
        store
            .migration_mark_pending("s1", &sha256_hex(legacy_session_jsonl().as_bytes()))
            .unwrap();

        let report =
            run_session_attachment_migrations(&db, &dir.path().join("blobs"), &sessions_dir);
        assert_eq!(report.committed, 1, "{:?}", report.failures);
        // 重执行复用残留行：不新增逻辑行，ref_count 不重复递增。
        let record = store.get_owned("task-1", &leftover.attachment_id).unwrap();
        assert_eq!(
            record.state,
            r_code_store::AttachmentState::Committed,
            "残留 staged 行被复用并补 commit"
        );
        let rows: i64 = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM attachments WHERE task_id = 'task-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1, "同内容同 task 复用一行，不产生重复逻辑引用");
    }

    /// 损坏行 → failed，源文件保持不变（禁止自动覆盖）。
    #[test]
    fn corrupted_jsonl_fails_without_touching_source() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let mut broken = legacy_session_jsonl();
        broken.push_str("{ this is not json\n");
        std::fs::write(sessions_dir.join("s1.jsonl"), broken.clone()).unwrap();

        let report =
            run_session_attachment_migrations(&db, &dir.path().join("blobs"), &sessions_dir);
        assert_eq!(report.failed, 1);
        let content = std::fs::read_to_string(sessions_dir.join("s1.jsonl")).unwrap();
        assert_eq!(content, broken, "failed 会话的源文件必须保持不变");
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        let (state, _, _, error) = store.migration_state("s1").unwrap().unwrap();
        assert_eq!(state, "failed");
        assert!(error.unwrap().contains("解析失败"));

        // 再次运行：failed 行不再自动覆盖。
        let report =
            run_session_attachment_migrations(&db, &dir.path().join("blobs"), &sessions_dir);
        assert_eq!(report.skipped, 1);
    }

    /// 崩溃恢复（§7.3）：pending 行 + 活动文件已是可验证 target → 补 commit。
    #[test]
    fn pending_row_with_migrated_target_recovers_to_committed() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        let reference = store
            .stage(
                "task-1",
                &StageAttachment {
                    name: "img.png".to_string(),
                    media_type: "image/png".to_string(),
                },
                &tiny_png(),
            )
            .unwrap();
        let user = Message {
            role: Role::User,
            content: vec![ContentBlock::Attachment {
                source: reference.clone(),
            }],
        };
        let migrated = serde_json::to_string(&SessionEvent::Message(user)).unwrap() + "\n";
        std::fs::write(sessions_dir.join("s1.jsonl"), migrated).unwrap();
        store
            .migration_mark_pending("s1", "0000-not-the-current-hash")
            .unwrap();

        let report =
            run_session_attachment_migrations(&db, &dir.path().join("blobs"), &sessions_dir);
        assert_eq!(report.committed, 1, "{:?}", report.failures);
        let record = store.get_owned("task-1", &reference.attachment_id).unwrap();
        assert_eq!(
            record.state,
            r_code_store::AttachmentState::Committed,
            "恢复路径必须补 commit 引用"
        );
        let (state, _, _, _) = store.migration_state("s1").unwrap().unwrap();
        assert_eq!(state, "committed");
    }

    /// 崩溃恢复：pending + 文件既不是 source 也不是干净 target → failed。
    #[test]
    fn pending_row_with_unrecognized_file_marks_failed() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(sessions_dir.join("s1.jsonl"), legacy_session_jsonl()).unwrap();
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        store
            .migration_mark_pending("s1", "definitely-not-current-hash")
            .unwrap();
        let report =
            run_session_attachment_migrations(&db, &dir.path().join("blobs"), &sessions_dir);
        assert_eq!(report.failed, 1);
        let (state, _, _, _) = store.migration_state("s1").unwrap().unwrap();
        assert_eq!(state, "failed");
    }

    /// 预览事件改写：preview 文件 stage 后 preview_id 换成附件 id；任务全部
    /// committed 且无未解析预览时清理预览目录（§7.5）。
    #[test]
    fn preview_events_are_staged_and_dirs_cleaned_after_migration() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let sessions_dir = dir.path().join("sessions");
        let previews_root = dir.path().join("attachments").join("task-1");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::create_dir_all(&previews_root).unwrap();
        std::fs::write(previews_root.join("prev-1.png"), tiny_png()).unwrap();

        let event = SessionEvent::System {
            event: "r_code_attachment_image".to_string(),
            data: serde_json::json!([{
                "preview_id": "prev-1",
                "name": "shot.png",
                "media_type": "image/png",
                "ocr_name": "shot.png.ocr.txt",
            }]),
        };
        std::fs::write(
            sessions_dir.join("s1.jsonl"),
            serde_json::to_string(&event).unwrap() + "\n",
        )
        .unwrap();

        let report =
            run_session_attachment_migrations(&db, &dir.path().join("blobs"), &sessions_dir);
        assert_eq!(report.committed, 1, "{:?}", report.failures);
        let migrated = std::fs::read_to_string(sessions_dir.join("s1.jsonl")).unwrap();
        assert!(!migrated.contains("\"preview_id\":\"prev-1\""));
        // preview_id 的值被改写为 staged 附件 id（UUID），且该附件可按 task 解析。
        let rewritten_id = {
            let needle = "\"preview_id\":\"";
            let start = migrated.find(needle).expect("preview entry kept") + needle.len();
            let end = migrated[start..].find('"').expect("closing quote") + start;
            migrated[start..end].to_string()
        };
        assert_ne!(rewritten_id, "prev-1");
        let store = r_code_store::AttachmentStore::new(&db, dir.path().join("blobs"));
        assert!(store.get_owned("task-1", &rewritten_id).is_ok());
        assert!(!previews_root.exists(), "迁移完成后预览目录必须删除");
    }

    /// 预览文件缺失：事件保留原 preview_id，预览目录不清理。
    #[test]
    fn missing_preview_file_keeps_reference_and_dir() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let sessions_dir = dir.path().join("sessions");
        let previews_root = dir.path().join("attachments").join("task-1");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::create_dir_all(&previews_root).unwrap();
        // 不写 prev-9.png：文件缺失。
        let event = SessionEvent::System {
            event: "r_code_attachment_image".to_string(),
            data: serde_json::json!([{
                "preview_id": "prev-9",
                "name": "gone.png",
                "media_type": "image/png",
                "ocr_name": "gone.png.ocr.txt",
            }]),
        };
        std::fs::write(
            sessions_dir.join("s1.jsonl"),
            serde_json::to_string(&event).unwrap() + "\n",
        )
        .unwrap();
        let report =
            run_session_attachment_migrations(&db, &dir.path().join("blobs"), &sessions_dir);
        assert_eq!(report.committed, 1, "{:?}", report.failures);
        let migrated = std::fs::read_to_string(sessions_dir.join("s1.jsonl")).unwrap();
        assert!(migrated.contains("\"preview_id\":\"prev-9\""));
        assert!(previews_root.exists(), "未解析预览的任务保留目录");
    }

    /// GC 引用扫描：JSONL 与排队载荷中的 attachment_id 都被提取。
    #[test]
    fn collect_referenced_ids_scans_sessions_and_queue() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "INSERT INTO tasks (id, title, goal, state, mode, agent_engine, created_at, updated_at) \
                 VALUES ('t1', 't', 'g', 'active', 'auto', 'r_code', '2026-01-01', '2026-01-01')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO queued_messages \
                 (id, task_id, branch_id, message, priority, state, created_at, updated_at, attachments_json) \
                 VALUES ('q1', 't1', 'b1', 'hi', 0, 'pending', '2026-01-01', '2026-01-01', ?1)",
                params![r#"{"version":2,"attachments":[{"version":1,"attachment_id":"queued-att-1","name":"a","media_type":"image/png","kind":"image","byte_len":8,"purpose":"native_input"}],"route":"native_main_vision"}"#],
            )
            .unwrap();
        }
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("s9.jsonl"),
            "{\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"attachment\",\"source\":{\"version\":1,\"attachment_id\":\"jsonl-att-9\",\"name\":\"x\",\"media_type\":\"image/png\",\"kind\":\"image\",\"byte_len\":8,\"purpose\":\"display_only\"}}]}}\n",
        )
        .unwrap();
        let ids = collect_referenced_attachment_ids(&db, &sessions_dir);
        assert!(ids.contains(&"queued-att-1".to_string()));
        assert!(ids.contains(&"jsonl-att-9".to_string()));
    }
}
