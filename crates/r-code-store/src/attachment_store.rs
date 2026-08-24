//! 附件引用存储（docs/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §4.3）。
//!
//! 二进制附件只在 BlobStore 保存一份物理副本；`attachments` 表是逻辑引用账本
//! （同一内容可在多条消息中各有一条记录）。staged 记录带 24 小时租约，草稿
//! 删除立即 `discard_staged()`，WebView 崩溃残留由 `gc_expired_staged()` 回收。
//!
//! `stage()` 顺序固定（§4.3）：校验 task → 校验元数据/魔数/尺寸 → BLAKE3 →
//! 临时文件 + fsync + 原子 rename → IMMEDIATE 事务（blobs.ref_count + 行插入）
//! → 返回引用。第 4~5 步成功但事务失败时允许留下无 ledger 的物理文件，由
//! `prune_unreferenced_files()` 回收；绝无「数据库已提交但 Blob 未安装」。

use std::path::PathBuf;

use agent_contract::{AttachmentKind, AttachmentPurpose, AttachmentRefV1};
use chrono::{DateTime, Duration, Utc};
use r_code_core::error::ProductError;
#[cfg(test)]
use rusqlite::Connection;
use rusqlite::{params, OptionalExtension, TransactionBehavior};

use crate::database::Database;
use crate::repositories::BlobStore;

/// 与 repositories::db_err 同构的私有包装（保持 crate 内错误映射一致）。
fn db_err(error: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(error.to_string())
}

/// 迁移行的当前状态：`(state, source_sha256, target_sha256, error)`。
pub type MigrationStateRow = (String, String, Option<String>, Option<String>);

/// staged 租约时长：24 小时。
const STAGED_LEASE_HOURS: i64 = 24;
/// 单附件上限与既有 IPC 校验一致（commands.rs 的 MAX_* 常量）。
const MAX_IMAGE_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TEXT_ATTACHMENT_BYTES: u64 = 1024 * 1024;
const MAX_PDF_ATTACHMENT_BYTES: u64 = 16 * 1024 * 1024;

fn invalid(message: impl Into<String>) -> ProductError {
    ProductError::BlobError(message.into())
}

/// 附件的数据库权威记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRecord {
    pub attachment_id: String,
    pub task_id: String,
    pub blob_hash: String,
    pub name: String,
    pub media_type: String,
    pub kind: AttachmentKind,
    pub byte_len: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub state: AttachmentState,
    pub created_at: DateTime<Utc>,
    pub committed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentState {
    Staged,
    Committed,
}

impl AttachmentRecord {
    /// 无 IO 的引用投影（消息与预算预检使用；读取时以数据库元数据为权威）。
    pub fn to_ref_v1(&self, purpose: AttachmentPurpose) -> AttachmentRefV1 {
        AttachmentRefV1 {
            version: 1,
            attachment_id: self.attachment_id.clone(),
            name: self.name.clone(),
            media_type: self.media_type.clone(),
            kind: self.kind,
            byte_len: self.byte_len,
            width: self.width,
            height: self.height,
            purpose,
        }
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttachmentRecord> {
    let kind: String = row.get("kind")?;
    let state: String = row.get("state")?;
    Ok(AttachmentRecord {
        attachment_id: row.get("id")?,
        task_id: row.get("task_id")?,
        blob_hash: row.get("blob_hash")?,
        name: row.get("name")?,
        media_type: row.get("media_type")?,
        kind: match kind.as_str() {
            "image" => AttachmentKind::Image,
            "text" => AttachmentKind::Text,
            "pdf" => AttachmentKind::Pdf,
            _ => AttachmentKind::Text,
        },
        byte_len: row.get::<_, i64>("byte_len")? as u64,
        width: row.get::<_, Option<i64>>("width")?.map(|v| v as u32),
        height: row.get::<_, Option<i64>>("height")?.map(|v| v as u32),
        state: match state.as_str() {
            "committed" => AttachmentState::Committed,
            _ => AttachmentState::Staged,
        },
        created_at: DateTime::parse_from_rfc3339(
            &row.get::<_, String>("created_at")?.replace(' ', "T"),
        )
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_default(),
        committed_at: row
            .get::<_, Option<String>>("committed_at")?
            .and_then(|value| DateTime::parse_from_rfc3339(&value.replace(' ', "T")).ok())
            .map(|t| t.with_timezone(&Utc)),
    })
}

/// 纯 Rust 的图片尺寸解析（PNG / JPEG / GIF / WebP）。无外部解码依赖，
/// 只读头部元数据；解析失败返回 Err（staging 拒绝无法核算预算的图片）。
pub fn image_dimensions(bytes: &[u8], media_type: &str) -> Result<(u32, u32), ProductError> {
    let dimensions = match media_type {
        "image/png" => png_dimensions(bytes)?,
        "image/jpeg" => jpeg_dimensions(bytes)?,
        "image/gif" => gif_dimensions(bytes)?,
        "image/webp" => webp_dimensions(bytes)?,
        other => return Err(invalid(format!("unsupported image media type: {other}"))),
    };
    Ok(dimensions)
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), ProductError> {
    if bytes.len() < 24 || !bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Err(invalid("not a valid PNG header"));
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Ok((width, height))
}

fn gif_dimensions(bytes: &[u8]) -> Result<(u32, u32), ProductError> {
    if bytes.len() < 10 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Err(invalid("not a valid GIF header"));
    }
    let width = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
    let height = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
    Ok((width, height))
}

fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32), ProductError> {
    if bytes.len() < 4 || !bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Err(invalid("not a valid JPEG header"));
    }
    let mut index = 2usize;
    while index + 9 < bytes.len() {
        if bytes[index] != 0xff {
            index += 1;
            continue;
        }
        let marker = bytes[index + 1];
        // SOF0..SOF15（除 DHT/JPG/DAC）携带尺寸。
        if (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc {
            let height = u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32;
            let width = u16::from_be_bytes([bytes[index + 7], bytes[index + 8]]) as u32;
            return Ok((width, height));
        }
        let segment_length = u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]) as usize;
        if segment_length < 2 {
            return Err(invalid("corrupt JPEG segment"));
        }
        index += 2 + segment_length;
    }
    Err(invalid("JPEG SOF marker not found"))
}

fn webp_dimensions(bytes: &[u8]) -> Result<(u32, u32), ProductError> {
    if bytes.len() < 30 || !bytes.starts_with(b"RIFF") || &bytes[8..12] != b"WEBP" {
        return Err(invalid("not a valid WebP header"));
    }
    match &bytes[12..16] {
        b"VP8 " => {
            // Lossy: frame tag + sync code 后的 14 字节处。
            if bytes.len() < 30 {
                return Err(invalid("truncated WebP VP8"));
            }
            let width = (u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3fff) as u32;
            let height = (u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3fff) as u32;
            Ok((width.max(1), height.max(1)))
        }
        b"VP8L" => {
            if bytes.len() < 25 {
                return Err(invalid("truncated WebP VP8L"));
            }
            let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            let width = (bits & 0x3fff) + 1;
            let height = ((bits >> 14) & 0x3fff) + 1;
            Ok((width, height))
        }
        b"VP8X" => {
            if bytes.len() < 30 {
                return Err(invalid("truncated WebP VP8X"));
            }
            let width =
                (u32::from(bytes[24]) | (u32::from(bytes[25]) << 8) | (u32::from(bytes[26]) << 16))
                    + 1;
            let height =
                (u32::from(bytes[27]) | (u32::from(bytes[28]) << 8) | (u32::from(bytes[29]) << 16))
                    + 1;
            Ok((width, height))
        }
        _ => Err(invalid("unknown WebP chunk")),
    }
}

/// staging 请求元数据（Base64 解码后的权威校验在 `stage()` 内完成）。
#[derive(Debug, Clone)]
pub struct StageAttachment {
    pub name: String,
    pub media_type: String,
}

/// 附件引用存储。
pub struct AttachmentStore<'a> {
    db: &'a Database,
    blobs: BlobStore<'a>,
}

impl<'a> AttachmentStore<'a> {
    pub fn new(db: &'a Database, blobs_dir: PathBuf) -> Self {
        Self {
            db,
            blobs: BlobStore::new(db, blobs_dir),
        }
    }

    /// staging 入口（§4.3 顺序固定）。成功返回 staged 引用（24h 租约）。
    pub fn stage(
        &self,
        task_id: &str,
        metadata: &StageAttachment,
        bytes: &[u8],
    ) -> Result<AttachmentRefV1, ProductError> {
        // 1. 校验 task 存在且未归档。作用域内即还连接：后续 IMMEDIATE 事务
        // 需要再取连接，池较小时并发持有两个连接会自我饥饿。
        let task_state: Option<String> = {
            let conn = self.db.conn()?;
            conn.query_row(
                "SELECT state FROM tasks WHERE id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?
        };
        match task_state.as_deref() {
            None => {
                return Err(ProductError::AttachmentNotFound {
                    attachment_id: format!("task:{task_id}"),
                })
            }
            Some("archived") => return Err(invalid("会话已归档，不能附加文件")),
            Some(_) => {}
        }

        // 2. 校验名称、MIME、魔数、字节数和图片尺寸。
        let name = metadata.name.trim();
        if name.is_empty() || name.contains('\0') || name.len() > 255 {
            return Err(invalid("附件名称为空或超过 255 字符"));
        }
        let media_type = metadata.media_type.trim().to_ascii_lowercase();
        let byte_len = bytes.len() as u64;
        if byte_len == 0 {
            return Err(invalid(format!("{name} 的文件内容为空")));
        }
        let (kind, width, height) = if media_type.starts_with("image/") {
            if byte_len > MAX_IMAGE_ATTACHMENT_BYTES {
                return Err(invalid(format!("{name} 超过 8 MiB")));
            }
            let (width, height) = image_dimensions(bytes, &media_type)
                .map_err(|error| invalid(format!("{name} 无法解析图片尺寸：{error}")))?;
            if width == 0 || height == 0 {
                return Err(invalid(format!("{name} 的图片尺寸非法")));
            }
            (AttachmentKind::Image, Some(width), Some(height))
        } else if media_type == "application/pdf" {
            if byte_len > MAX_PDF_ATTACHMENT_BYTES {
                return Err(invalid(format!("{name} 超过 16 MiB")));
            }
            if !bytes.starts_with(b"%PDF-") {
                return Err(invalid(format!("{name} 不是有效的 PDF 文件")));
            }
            (AttachmentKind::Pdf, None, None)
        } else {
            if byte_len > MAX_TEXT_ATTACHMENT_BYTES {
                return Err(invalid(format!("{name} 超过 1 MiB")));
            }
            if std::str::from_utf8(bytes).is_err() {
                return Err(invalid(format!("{name} 不是 UTF-8 文本文件")));
            }
            (AttachmentKind::Text, None, None)
        };

        // 3~5. BLAKE3 + 原子安装（BlobStore::put 已实现 temp+fsync+rename）。
        let blob_hash = self.blobs.put(bytes)?;

        // 6. IMMEDIATE 事务：insert/递增 blobs.ref_count，插入 staged 行。
        let attachment_id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let lease_expires = now + Duration::hours(STAGED_LEASE_HOURS);
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        tx.execute(
            "INSERT INTO blobs (hash, ref_count, created_at) VALUES (?1, 1, ?2) \
             ON CONFLICT(hash) DO UPDATE SET ref_count = ref_count + 1",
            params![blob_hash, now.to_rfc3339()],
        )
        .map_err(db_err)?;
        tx.execute(
            "INSERT INTO attachments \
             (id, task_id, blob_hash, name, media_type, kind, byte_len, width, height, \
              state, lease_expires_at, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'staged', ?10, ?11)",
            params![
                attachment_id,
                task_id,
                blob_hash,
                name,
                media_type,
                match kind {
                    AttachmentKind::Image => "image",
                    AttachmentKind::Text => "text",
                    AttachmentKind::Pdf => "pdf",
                },
                byte_len as i64,
                width.map(u64::from).map(|v| v as i64),
                height.map(u64::from).map(|v| v as i64),
                lease_expires.to_rfc3339(),
                now.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        // 7. 事务提交后返回引用。
        tx.commit().map_err(db_err)?;

        Ok(AttachmentRefV1 {
            version: 1,
            attachment_id,
            name: name.to_string(),
            media_type,
            kind,
            byte_len,
            width,
            height,
            purpose: AttachmentPurpose::NativeInput,
        })
    }

    /// 读取本任务拥有的附件记录（所有权检查的权威入口）。
    pub fn get_owned(
        &self,
        task_id: &str,
        attachment_id: &str,
    ) -> Result<AttachmentRecord, ProductError> {
        let conn = self.db.conn()?;
        let record = conn
            .query_row(
                "SELECT * FROM attachments WHERE id = ?1",
                params![attachment_id],
                row_to_record,
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| ProductError::AttachmentNotFound {
                attachment_id: attachment_id.to_string(),
            })?;
        if record.task_id != task_id {
            return Err(ProductError::AttachmentOwnershipMismatch {
                attachment_id: attachment_id.to_string(),
            });
        }
        Ok(record)
    }

    /// 读取本任务拥有的附件字节（BlobStore → 物化边界的唯一读取路径）。
    pub fn read_owned(&self, task_id: &str, attachment_id: &str) -> Result<Vec<u8>, ProductError> {
        let record = self.get_owned(task_id, attachment_id)?;
        self.blobs
            .get(&record.blob_hash)?
            .ok_or_else(|| ProductError::AttachmentNotFound {
                attachment_id: attachment_id.to_string(),
            })
    }

    /// 批量把 staged 记录标记 committed（JSONL append 成功后调用）。
    /// 幂等：已 committed 的行跳过；不存在的 id 返回 Err。
    pub fn commit_many(
        &self,
        task_id: &str,
        attachment_ids: &[String],
    ) -> Result<(), ProductError> {
        if attachment_ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        for attachment_id in attachment_ids {
            let record = tx
                .query_row(
                    "SELECT * FROM attachments WHERE id = ?1",
                    params![attachment_id],
                    row_to_record,
                )
                .optional()
                .map_err(db_err)?
                .ok_or_else(|| ProductError::AttachmentNotFound {
                    attachment_id: attachment_id.clone(),
                })?;
            if record.task_id != task_id {
                return Err(ProductError::AttachmentOwnershipMismatch {
                    attachment_id: attachment_id.clone(),
                });
            }
            if record.state == AttachmentState::Committed {
                continue;
            }
            tx.execute(
                "UPDATE attachments SET state = 'committed', committed_at = ?2, \
                 lease_expires_at = NULL WHERE id = ?1",
                params![attachment_id, Utc::now().to_rfc3339()],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }

    /// 删除草稿附件（UI remove）：staged 行删除 + BlobStore 递减。
    pub fn discard_staged(&self, task_id: &str, attachment_id: &str) -> Result<(), ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let record = tx
            .query_row(
                "SELECT * FROM attachments WHERE id = ?1",
                params![attachment_id],
                row_to_record,
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| ProductError::AttachmentNotFound {
                attachment_id: attachment_id.to_string(),
            })?;
        if record.task_id != task_id {
            return Err(ProductError::AttachmentOwnershipMismatch {
                attachment_id: attachment_id.to_string(),
            });
        }
        if record.state == AttachmentState::Committed {
            return Err(invalid(format!(
                "附件 {attachment_id} 已提交到会话，不能按草稿删除"
            )));
        }
        tx.execute(
            "DELETE FROM attachments WHERE id = ?1",
            params![attachment_id],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        self.blobs.decrement_ref(&record.blob_hash)
    }

    /// 启动恢复：把 JSONL 引用到的 staged 记录补为 committed（§4.4 步骤 6）。
    /// 只处理本 storage 归属 task 的附件；返回补提交的 id 清单。
    pub fn reconcile_session_refs(
        &self,
        storage_id: &str,
        refs: &[AttachmentRefV1],
    ) -> Result<Vec<String>, ProductError> {
        // storage_id → task：会话分支归属任务由 session_branches 记录。
        let conn = self.db.conn()?;
        let task_id: Option<String> = conn
            .query_row(
                "SELECT task_id FROM session_branches WHERE storage_id = ?1",
                params![storage_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let task_id = match task_id {
            Some(task_id) => task_id,
            None => {
                // 分支不存在（已清理）：无可补提交。
                return Ok(Vec::new());
            }
        };
        let mut reconciled = Vec::new();
        for reference in refs {
            let record = self.get_owned(&task_id, &reference.attachment_id);
            match record {
                Ok(record) if record.state == AttachmentState::Staged => {
                    self.commit_many(&task_id, std::slice::from_ref(&reference.attachment_id))?;
                    reconciled.push(reference.attachment_id.clone());
                }
                _ => {}
            }
        }
        Ok(reconciled)
    }

    /// 回收过期 staged 记录。`referenced_ids` 是调用方扫描活动 JSONL 与 queued
    /// message 得到的仍被引用的 id（防止「消息已落盘但 commit 标记未写」的
    /// 崩溃窗口误删附件，§4.3）。返回回收的 id 清单。
    pub fn gc_expired_staged(
        &self,
        now: DateTime<Utc>,
        referenced_ids: &[String],
    ) -> Result<Vec<String>, ProductError> {
        let conn = self.db.conn()?;
        let expired: Vec<AttachmentRecord> = {
            let mut statement = conn
                .prepare(
                    "SELECT * FROM attachments WHERE state = 'staged' \
                     AND lease_expires_at IS NOT NULL AND lease_expires_at < ?1",
                )
                .map_err(db_err)?;
            let rows = statement
                .query_map(params![now.to_rfc3339()], row_to_record)
                .map_err(db_err)?;
            rows.filter_map(|row| row.ok()).collect()
        };
        let mut collected = Vec::new();
        for record in expired {
            if referenced_ids.contains(&record.attachment_id) {
                // 仍被活动引用：延后一轮回收（保守）。
                continue;
            }
            if self
                .discard_staged(&record.task_id, &record.attachment_id)
                .is_ok()
            {
                collected.push(record.attachment_id);
            }
        }
        Ok(collected)
    }

    // ── JSONL 迁移状态机（docs/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §7.3）─────────────────

    /// 迁移行的当前状态，见 [`MigrationStateRow`]。
    pub fn migration_state(
        &self,
        storage_id: &str,
    ) -> Result<Option<MigrationStateRow>, ProductError> {
        let conn = self.db.conn()?;
        conn.query_row(
            "SELECT state, source_sha256, target_sha256, error              FROM session_attachment_migrations WHERE storage_id = ?1",
            params![storage_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            },
        )
        .optional()
        .map_err(db_err)
    }

    /// 步骤 1：计算源 SHA-256 后写 pending。重复执行（重扫）幂等——已
    /// committed/failed 的行不覆盖；pending 保持原 source hash（崩溃恢复按它
    /// 判定重执行/补 commit）。
    pub fn migration_mark_pending(
        &self,
        storage_id: &str,
        source_sha256: &str,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO session_attachment_migrations              (storage_id, source_sha256, state, updated_at) VALUES (?1, ?2, 'pending', ?3)              ON CONFLICT(storage_id) DO UPDATE SET                source_sha256 = excluded.source_sha256,                state = 'pending',                target_sha256 = NULL,                error = NULL,                updated_at = excluded.updated_at              WHERE session_attachment_migrations.state = 'pending'",
            params![storage_id, source_sha256, Utc::now().to_rfc3339()],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 步骤 7：记录 target SHA-256 并置 committed。
    pub fn migration_mark_committed(
        &self,
        storage_id: &str,
        target_sha256: &str,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE session_attachment_migrations              SET state = 'committed', target_sha256 = ?2, error = NULL, updated_at = ?3              WHERE storage_id = ?1",
            params![storage_id, target_sha256, Utc::now().to_rfc3339()],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 损坏/不可迁移：置 failed 并保留可读错误（源文件保持不变）。
    pub fn migration_mark_failed(&self, storage_id: &str, error: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE session_attachment_migrations              SET state = 'failed', error = ?2, updated_at = ?3 WHERE storage_id = ?1",
            params![storage_id, error, Utc::now().to_rfc3339()],
        )
        .map_err(db_err)?;
        Ok(())
    }
    /// 任务删除前：列出其全部 blob hash（含重复计数），供事务后按引用数递减。
    pub fn list_hashes_for_task(&self, task_id: &str) -> Result<Vec<String>, ProductError> {
        let conn = self.db.conn()?;
        let mut statement = conn
            .prepare("SELECT blob_hash FROM attachments WHERE task_id = ?1")
            .map_err(db_err)?;
        let rows = statement
            .query_map(params![task_id], |row| row.get::<_, String>(0))
            .map_err(db_err)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::run_migrations;

    fn setup() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("test.db")).unwrap();
        run_migrations(&db.conn().unwrap()).unwrap();
        (db, dir)
    }

    fn seed_task(conn: &Connection, task_id: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, goal, state, mode, agent_engine, created_at, updated_at) \
             VALUES (?1, 't', 'test goal', 'active', 'auto', 'r_code', ?2, ?2)",
            params![task_id, Utc::now().to_rfc3339()],
        )
        .unwrap();
    }

    /// 最小合法 PNG（1×1 白色像素）。
    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
            0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xde,
        ]
    }

    fn stage_png(store: &AttachmentStore<'_>, task_id: &str, name: &str) -> AttachmentRefV1 {
        store
            .stage(
                task_id,
                &StageAttachment {
                    name: name.to_string(),
                    media_type: "image/png".to_string(),
                },
                &tiny_png(),
            )
            .unwrap()
    }

    #[test]
    fn stage_get_read_roundtrip() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        let reference = stage_png(&store, "task-1", "shot.png");
        assert_eq!(reference.version, 1);
        assert_eq!(reference.width, Some(1));
        assert_eq!(reference.height, Some(1));
        assert_eq!(reference.kind, AttachmentKind::Image);
        let record = store.get_owned("task-1", &reference.attachment_id).unwrap();
        assert_eq!(record.state, AttachmentState::Staged);
        assert_eq!(
            store
                .read_owned("task-1", &reference.attachment_id)
                .unwrap(),
            tiny_png()
        );
    }

    /// §10 阶段 B 完成条件：相同图片 stage 两次只有一个物理 Blob；逻辑引用数正确。
    #[test]
    fn same_content_stages_deduplicate_into_one_physical_blob() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let blobs_dir = dir.path().join("blobs");
        let store = AttachmentStore::new(&db, blobs_dir.clone());
        let first = stage_png(&store, "task-1", "a.png");
        let second = stage_png(&store, "task-1", "b.png");
        assert_ne!(first.attachment_id, second.attachment_id);
        // 只统计内容 hash 文件（忽略 .tmp- 残留）。
        let physical: Vec<_> = std::fs::read_dir(&blobs_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .map(|name| !name.starts_with(".tmp-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(physical.len(), 1, "同一内容物理 Blob 只存一份");
        let count: i64 = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT ref_count FROM blobs WHERE hash = (SELECT blob_hash FROM attachments WHERE id = ?1)",
                params![first.attachment_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "逻辑引用计数为 2");
        // 第二个任务再引用一次：仍是一个物理文件，ref_count = 3。
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-2");
        }
        stage_png(&store, "task-2", "c.png");
        let count: i64 = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT ref_count FROM blobs WHERE hash = (SELECT blob_hash FROM attachments WHERE id = ?1)",
                params![first.attachment_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
    }

    /// 任一逻辑引用删除不会误删仍在使用的 Blob；最后一个引用删除后 ledger 归零。
    #[test]
    fn discard_keeps_blob_while_other_references_exist() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
            seed_task(&conn, "task-2");
        }
        let blobs_dir = dir.path().join("blobs");
        let store = AttachmentStore::new(&db, blobs_dir.clone());
        let a = stage_png(&store, "task-1", "a.png");
        let b = stage_png(&store, "task-1", "b.png");
        let c = stage_png(&store, "task-2", "c.png");
        assert_eq!(a.blob_hash_ref(&store), b.blob_hash_ref(&store));
        store.discard_staged("task-1", &a.attachment_id).unwrap();
        // 另一逻辑引用仍在：物理 Blob 保留、可读。
        assert_eq!(
            store.read_owned("task-1", &b.attachment_id).unwrap(),
            tiny_png()
        );
        assert_eq!(
            store.read_owned("task-2", &c.attachment_id).unwrap(),
            tiny_png()
        );
        store.discard_staged("task-1", &b.attachment_id).unwrap();
        store.discard_staged("task-2", &c.attachment_id).unwrap();
        let remaining: i64 = db
            .conn()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0, "最后一个引用删除后 ledger 归零");
        let files = std::fs::read_dir(&blobs_dir).unwrap().count();
        assert_eq!(files, 0, "物理文件随之清理");
    }

    #[test]
    fn ownership_mismatch_rejected() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
            seed_task(&conn, "task-2");
        }
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        let reference = stage_png(&store, "task-1", "a.png");
        let error = store
            .get_owned("task-2", &reference.attachment_id)
            .unwrap_err();
        assert!(matches!(
            error,
            ProductError::AttachmentOwnershipMismatch { .. }
        ));
    }

    #[test]
    fn committed_attachments_survive_gc_and_staged_expire() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        let committed = stage_png(&store, "task-1", "keep.png");
        store
            .commit_many("task-1", std::slice::from_ref(&committed.attachment_id))
            .unwrap();
        // 直接把 staged 租约拨到过去（模拟 24h 过期）。
        let stale = stage_png(&store, "task-1", "stale.png");
        db.conn()
            .unwrap()
            .execute(
                "UPDATE attachments SET lease_expires_at = ?1 WHERE id = ?2",
                params![
                    (Utc::now() - Duration::hours(25)).to_rfc3339(),
                    stale.attachment_id
                ],
            )
            .unwrap();
        let collected = store.gc_expired_staged(Utc::now(), &[]).unwrap();
        assert_eq!(collected, vec![stale.attachment_id.clone()]);
        // committed 记录不受影响。
        assert_eq!(
            store
                .get_owned("task-1", &committed.attachment_id)
                .unwrap()
                .state,
            AttachmentState::Committed
        );
        // 仍被 JSONL 引用的 staged 记录不回收（崩溃窗口保护）。
        let referenced = stage_png(&store, "task-1", "ref.png");
        db.conn()
            .unwrap()
            .execute(
                "UPDATE attachments SET lease_expires_at = ?1 WHERE id = ?2",
                params![
                    (Utc::now() - Duration::hours(25)).to_rfc3339(),
                    referenced.attachment_id
                ],
            )
            .unwrap();
        let collected = store
            .gc_expired_staged(Utc::now(), std::slice::from_ref(&referenced.attachment_id))
            .unwrap();
        assert!(collected.is_empty(), "被引用的 staged 记录不回收");
    }

    #[test]
    fn stage_rejects_archived_and_missing_tasks() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
            seed_task(&conn, "task-arch");
            conn.execute(
                "UPDATE tasks SET state = 'archived' WHERE id = 'task-arch'",
                [],
            )
            .unwrap();
        }
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        assert!(store
            .stage(
                "missing",
                &StageAttachment {
                    name: "a.png".into(),
                    media_type: "image/png".into()
                },
                &tiny_png(),
            )
            .is_err());
        assert!(store
            .stage(
                "task-arch",
                &StageAttachment {
                    name: "a.png".into(),
                    media_type: "image/png".into()
                },
                &tiny_png(),
            )
            .is_err());
    }

    /// §7.5：删除任务按 attachments 逻辑引用递减 Blob refcount；同一 Blob 被
    /// 其他任务引用时物理文件保留，另一任务仍可读取。
    #[test]
    fn task_purge_decrements_attachment_refs_and_keeps_shared_blobs() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
            seed_task(&conn, "task-2");
        }
        let blobs_dir = dir.path().join("blobs");
        let store = AttachmentStore::new(&db, blobs_dir.clone());
        let a = stage_png(&store, "task-1", "a.png");
        let b = stage_png(&store, "task-2", "b.png");
        assert_eq!(
            store
                .get_owned("task-1", &a.attachment_id)
                .unwrap()
                .blob_hash,
            store
                .get_owned("task-2", &b.attachment_id)
                .unwrap()
                .blob_hash,
            "同内容跨任务共享同一物理 Blob"
        );

        // 删除 task-1：其逻辑引用释放，task-2 的引用与物理 Blob 保留。
        let projection_root = dir.path().join("projections");
        std::fs::create_dir_all(&projection_root).unwrap();
        crate::repositories::TaskRepository::new(&db)
            .delete("task-1", &blobs_dir, &projection_root)
            .unwrap();
        assert!(store.get_owned("task-2", &b.attachment_id).is_ok());
        assert_eq!(
            store.read_owned("task-2", &b.attachment_id).unwrap(),
            tiny_png()
        );
        let ref_count: i64 = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT ref_count FROM blobs WHERE hash = (SELECT blob_hash FROM attachments WHERE id = ?1)",
                rusqlite::params![b.attachment_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ref_count, 1, "删除 task-1 后剩余 1 个逻辑引用");

        // 删除最后一个引用任务：ledger 归零、物理文件清理。
        crate::repositories::TaskRepository::new(&db)
            .delete("task-2", &blobs_dir, &projection_root)
            .unwrap();
        let remaining: i64 = db
            .conn()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn corrupted_image_bytes_rejected_at_stage() {
        let (db, dir) = setup();
        {
            let conn = db.conn().unwrap();
            seed_task(&conn, "task-1");
        }
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));
        let error = store
            .stage(
                "task-1",
                &StageAttachment {
                    name: "fake.png".into(),
                    media_type: "image/png".into(),
                },
                b"not a png at all",
            )
            .unwrap_err();
        assert!(error.to_string().contains("无法解析图片尺寸"));
    }

    /// §12 r-code-store 测试项：JSONL source/target hash 状态机。pending 可
    /// 重入、committed/failed 不被 pending 覆盖、崩溃恢复三分支的数据侧基础。
    #[test]
    fn migration_state_machine_transitions() {
        let (db, dir) = setup();
        let store = AttachmentStore::new(&db, dir.path().join("blobs"));

        // 无行 → 未迁移。
        assert!(store.migration_state("s1").unwrap().is_none());

        // pending 写入 + 幂等重入（同 source 更新保持 pending）。
        store.migration_mark_pending("s1", "sha-a").unwrap();
        store.migration_mark_pending("s1", "sha-a").unwrap();
        assert_eq!(
            store.migration_state("s1").unwrap().unwrap(),
            ("pending".to_string(), "sha-a".to_string(), None, None)
        );

        // committed 终态：后续 pending 重扫不得回退。
        store.migration_mark_committed("s1", "sha-b").unwrap();
        store.migration_mark_pending("s1", "sha-c").unwrap();
        let (state, source, target, error) = store.migration_state("s1").unwrap().unwrap();
        assert_eq!(state, "committed");
        assert_eq!(source, "sha-a");
        assert_eq!(target.as_deref(), Some("sha-b"));
        assert!(error.is_none());

        // failed 终态：保留可读错误，不被 pending 覆盖。
        store.migration_mark_pending("s2", "sha-x").unwrap();
        store.migration_mark_failed("s2", "corrupt line 3").unwrap();
        store.migration_mark_pending("s2", "sha-x").unwrap();
        let (state, _, _, error) = store.migration_state("s2").unwrap().unwrap();
        assert_eq!(state, "failed");
        assert_eq!(error.as_deref(), Some("corrupt line 3"));
    }

    #[test]
    fn image_dimensions_parses_regression_fixture_shape() {
        // 尺寸解析器单测（1818×1026 回归样本的头部形状）。
        let mut png = tiny_png();
        png[16..20].copy_from_slice(&1818u32.to_be_bytes());
        png[20..24].copy_from_slice(&1026u32.to_be_bytes());
        assert_eq!(image_dimensions(&png, "image/png").unwrap(), (1818, 1026));
    }

    /// 辅助：引用 → blob hash（测试内部用）。
    trait BlobHashRef {
        fn blob_hash_ref(&self, store: &AttachmentStore<'_>) -> String;
    }
    impl BlobHashRef for AttachmentRefV1 {
        fn blob_hash_ref(&self, store: &AttachmentStore<'_>) -> String {
            // 测试里 task 固定；直接从记录读。
            store
                .get_owned("task-1", &self.attachment_id)
                .unwrap()
                .blob_hash
        }
    }
}
