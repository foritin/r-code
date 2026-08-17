//! 内置工具 -- R0/R1 只读工具集 + R2 写入工具。 [doc-02 §5] [doc-18 M8-02]
//!
//! 实现以下工具：
//! - `read_file`（R1）：读取文件内容，支持 offset / limit 分页
//! - `list_files`（R0）：列出目录内容
//! - `git_status`（R0）：获取 git 状态
//! - `load_skill`（R0）：加载 SKILL.md，路径穿越拒绝 [doc-02 §5]
//! - `edit`（R2）：按字面量精确替换片段（首选的修改方式）
//! - `apply_patch`（R2）：原子化应用补丁（全文件替换）
//! - `create_file`（R2）：创建新文件
//! - `delete_file`（R2）：删除文件
//!
//! 内容搜索（`search`）与文件名匹配（`glob`）在 [`crate::tools_search`]；
//! shell 命令执行（`bash`）在 [`crate::tools_command`]。

use std::io::{BufReader, Read};
use std::ops::Range;
use std::path::{Component, Path};

use async_trait::async_trait;
use r_code_core::dto::RiskLevel;
use r_code_core::error::ProductError;
use r_code_core::process::hide_background_console;
use r_code_core::security::{path_for_display, PathGuard, WorkspaceFileAccess};

use crate::gateway::{PathBinding, Tool, ToolExecutionContext, ToolExecutionResult};

/// `read_file` 不带 limit 时，单次最多返回的行数。
///
/// 没有这个上限，模型读一个几万行的生成文件就能把整个上下文撑爆，
/// 之后的对话全部失效。宁可截断并告诉它怎么翻页。
const DEFAULT_READ_LINE_LIMIT: usize = 2_000;
/// `read_file` 单次返回的正文 UTF-8 字节上限（约 100 KiB）。
const MAX_READ_BYTES: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadStopReason {
    LineLimit,
    ByteLimitAfterLine,
    ByteLimitWithinLine {
        line: usize,
        next_byte_offset: usize,
    },
}

struct ReadFilePager {
    offset: usize,
    limit: usize,
    first_line_byte_offset: usize,
    current_line: usize,
    current_line_bytes: usize,
    current_line_has_content: bool,
    pending_char: Option<char>,
    total_lines: usize,
    body: String,
    returned_last_line: Option<usize>,
    stop_reason: Option<ReadStopReason>,
}

impl ReadFilePager {
    fn new(offset: usize, limit: usize, first_line_byte_offset: usize) -> Self {
        Self {
            offset,
            limit,
            first_line_byte_offset,
            current_line: 1,
            current_line_bytes: 0,
            current_line_has_content: false,
            pending_char: None,
            total_lines: 0,
            body: String::new(),
            returned_last_line: None,
            stop_reason: None,
        }
    }

    fn process_text(&mut self, text: &str) -> Result<(), ProductError> {
        for ch in text.chars() {
            if ch == '\n' {
                if self.pending_char == Some('\r') {
                    // `str::lines()` treats CRLF as one terminator and does not expose CR.
                    self.pending_char = None;
                } else {
                    self.flush_pending_char()?;
                }
                self.finish_current_line()?;
            } else {
                self.flush_pending_char()?;
                self.pending_char = Some(ch);
                self.current_line_has_content = true;
            }
        }
        Ok(())
    }

    fn finish_eof(&mut self) -> Result<(), ProductError> {
        self.flush_pending_char()?;
        if self.current_line_has_content {
            self.finish_current_line()?;
        }
        Ok(())
    }

    fn flush_pending_char(&mut self) -> Result<(), ProductError> {
        let Some(ch) = self.pending_char.take() else {
            return Ok(());
        };
        let char_start = self.current_line_bytes;
        let char_end = char_start + ch.len_utf8();

        if self.current_line == self.offset
            && self.first_line_byte_offset > char_start
            && self.first_line_byte_offset < char_end
        {
            return Err(ProductError::Other(format!(
                "line_byte_offset {} is not on a UTF-8 character boundary in line {}",
                self.first_line_byte_offset, self.offset
            )));
        }

        if self.line_is_selected()
            && self.stop_reason.is_none()
            && char_start >= self.byte_offset_for_current_line()
        {
            if self.body.len() + ch.len_utf8() > MAX_READ_BYTES {
                self.stop_reason = Some(ReadStopReason::ByteLimitWithinLine {
                    line: self.current_line,
                    next_byte_offset: char_start,
                });
            } else {
                self.body.push(ch);
            }
        }
        self.current_line_bytes = char_end;
        Ok(())
    }

    fn finish_current_line(&mut self) -> Result<(), ProductError> {
        self.total_lines += 1;

        if self.current_line == self.offset && self.first_line_byte_offset > self.current_line_bytes
        {
            return Err(ProductError::Other(format!(
                "line_byte_offset {} is past the end of line {} ({} UTF-8 bytes)",
                self.first_line_byte_offset, self.offset, self.current_line_bytes
            )));
        }

        if self.line_is_selected() && self.stop_reason.is_none() {
            self.returned_last_line = Some(self.current_line);
            if self.body.len() < MAX_READ_BYTES {
                self.body.push('\n');
            } else {
                self.stop_reason = Some(ReadStopReason::ByteLimitAfterLine);
            }

            if self.current_line - self.offset + 1 >= self.limit && self.stop_reason.is_none() {
                self.stop_reason = Some(ReadStopReason::LineLimit);
            }
        }

        self.current_line += 1;
        self.current_line_bytes = 0;
        self.current_line_has_content = false;
        self.pending_char = None;
        Ok(())
    }

    fn line_is_selected(&self) -> bool {
        self.current_line >= self.offset && self.current_line - self.offset < self.limit
    }

    fn byte_offset_for_current_line(&self) -> usize {
        if self.current_line == self.offset {
            self.first_line_byte_offset
        } else {
            0
        }
    }
}

fn open_text_file(
    path: &str,
    workspace_guard: Option<&PathGuard>,
) -> Result<std::fs::File, ProductError> {
    let display_path = path_for_display(path);
    match workspace_guard {
        Some(guard) => guard
            .open_file(Path::new(path), WorkspaceFileAccess::Read)
            .map(|handle| handle.into_file()),
        None => std::fs::File::open(path)
            .map_err(|e| ProductError::Other(format!("failed to read {display_path}: {e}"))),
    }
}

/// Read text through the host-owned workspace capability when one is available.
///
/// Unscoped calls retain the historic behaviour for standalone tools and their unit tests. Agent
/// runs always provide a guard, so their actual file handle is opened under the fixed workspace
/// directory rather than by a model-supplied ambient path.
fn read_text_file(path: &str, workspace_guard: Option<&PathGuard>) -> Result<String, ProductError> {
    let mut file = open_text_file(path, workspace_guard)?;
    let mut content = String::new();
    let display_path = path_for_display(path);
    file.read_to_string(&mut content)
        .map_err(|e| ProductError::Other(format!("failed to read {display_path}: {e}")))?;
    Ok(content)
}

fn list_directory_names(
    path: &str,
    workspace_guard: Option<&PathGuard>,
) -> Result<Vec<String>, ProductError> {
    let display_path = path_for_display(path);
    let mut names = match workspace_guard {
        Some(guard) => guard
            .list_directory(Path::new(path))?
            .into_entries()
            .into_iter()
            .map(|entry| entry.name.to_string_lossy().to_string())
            .collect(),
        None => {
            let entries = std::fs::read_dir(path)
                .map_err(|e| ProductError::Other(format!("cannot list {display_path}: {e}")))?;
            let mut names = Vec::new();
            for entry in entries {
                let entry =
                    entry.map_err(|e| ProductError::Other(format!("dir entry error: {e}")))?;
                names.push(entry.file_name().to_string_lossy().to_string());
            }
            names
        }
    };
    names.sort();
    Ok(names)
}

fn atomic_write_scoped(
    path: &Path,
    content: &[u8],
    workspace_guard: Option<&PathGuard>,
) -> Result<(), ProductError> {
    match workspace_guard {
        Some(guard) => guard.atomic_write_path(path, content).map(|_| ()),
        None => atomic_write(path, content),
    }
}

fn create_new_file_scoped(
    path: &Path,
    content: &[u8],
    workspace_guard: Option<&PathGuard>,
) -> Result<(), ProductError> {
    match workspace_guard {
        Some(guard) => guard.create_new_path(path, content).map(|_| ()),
        None => {
            use std::io::Write;
            let display_path = path_for_display(path);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| {
                    ProductError::Other(format!("failed to create {display_path}: {e}"))
                })?;
            file.write_all(content)
                .map_err(|e| ProductError::Other(format!("failed to write {display_path}: {e}")))
        }
    }
}

fn remove_file_scoped(
    path: &Path,
    workspace_guard: Option<&PathGuard>,
) -> Result<(), ProductError> {
    match workspace_guard {
        Some(guard) => {
            if guard.remove_file_if_exists(path)? {
                Ok(())
            } else {
                Err(ProductError::PathNotFound(format!(
                    "path does not exist: {}",
                    path_for_display(path)
                )))
            }
        }
        None => std::fs::remove_file(path).map_err(|e| {
            ProductError::Other(format!("failed to delete {}: {e}", path_for_display(path)))
        }),
    }
}

fn reject_if_cancelled(
    tool_name: &str,
    abort_flag: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), ProductError> {
    if abort_flag.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
        Err(ProductError::Other(format!(
            "tool {tool_name} cancelled before execution completed"
        )))
    } else {
        Ok(())
    }
}

// ============================================================================
// read_file  [doc-02 §5] -- R1
// ============================================================================

/// `read_file` 工具 -- 读取文件内容。
///
/// R1：低风险，可能泄露信息。
pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read the contents of a text file. \
For large files, page through with offset (1-based line number) and limit. \
Output is truncated with a note if it would be too large; follow its next offset, \
or its line_byte_offset when one individual line is exceptionally long."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R1
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file."
                },
                "offset": {
                    "type": "integer",
                    "description": "1-based line number to start reading from. Defaults to 1."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return. Defaults to 2000. When scanning a long file, use a large window (500-2000 lines) and page with offset; use a small window only when re-reading a narrow region around an edit anchor."
                },
                "line_byte_offset": {
                    "type": "integer",
                    "description": "UTF-8 byte offset within the first requested line. Defaults to 0. Use only with offset when a previous response asks you to continue a very long line."
                }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        execute_read_file(&input, None)
    }

    async fn execute_with_context_and_abort_with_workspace(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        reject_if_cancelled(self.name(), abort_flag)?;
        execute_read_file(&input, workspace_guard).map(ToolExecutionResult::from)
    }
}

fn execute_read_file(
    input: &serde_json::Value,
    workspace_guard: Option<&PathGuard>,
) -> Result<String, ProductError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
    let display_path = path_for_display(path);

    let offset = input
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|n| n.max(1) as usize)
        .unwrap_or(1);
    let limit = input
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n.max(1) as usize)
        .unwrap_or(DEFAULT_READ_LINE_LIMIT);
    let line_byte_offset = input
        .get("line_byte_offset")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(0);
    let paging_requested = input.get("offset").is_some()
        || input.get("limit").is_some()
        || input.get("line_byte_offset").is_some();

    let file = open_text_file(path, workspace_guard)?;
    let mut reader = BufReader::new(file);
    let mut pager = ReadFilePager::new(offset, limit, line_byte_offset);
    let mut original = (!paging_requested).then(String::new);
    let mut bytes = [0_u8; 8 * 1024];
    let mut utf8_pending = Vec::with_capacity(bytes.len() + 4);

    loop {
        let count = reader
            .read(&mut bytes)
            .map_err(|e| ProductError::Other(format!("failed to read {display_path}: {e}")))?;
        if count == 0 {
            break;
        }
        utf8_pending.extend_from_slice(&bytes[..count]);

        loop {
            match std::str::from_utf8(&utf8_pending) {
                Ok(text) => {
                    if let Some(content) = original.as_mut() {
                        content.push_str(text);
                    }
                    if original
                        .as_ref()
                        .is_some_and(|content| content.len() > MAX_READ_BYTES)
                    {
                        original = None;
                    }
                    pager.process_text(text)?;
                    utf8_pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        let text = std::str::from_utf8(&utf8_pending[..valid])
                            .expect("valid_up_to must identify valid UTF-8");
                        if let Some(content) = original.as_mut() {
                            content.push_str(text);
                        }
                        if original
                            .as_ref()
                            .is_some_and(|content| content.len() > MAX_READ_BYTES)
                        {
                            original = None;
                        }
                        pager.process_text(text)?;
                        utf8_pending.drain(..valid);
                        continue;
                    }
                    if error.error_len().is_some() {
                        return Err(ProductError::Other(format!(
                            "failed to read {display_path}: file is not valid UTF-8"
                        )));
                    }
                    // A multi-byte scalar was split across read buffers. Keep at most
                    // three trailing bytes and complete it with the next read.
                    break;
                }
            }
        }
    }

    if !utf8_pending.is_empty() {
        return Err(ProductError::Other(format!(
            "failed to read {display_path}: file ends with incomplete UTF-8"
        )));
    }
    pager.finish_eof()?;

    // 未分页且文件不大：原样返回，与历史行为完全一致（包括换行符风格）。
    if let Some(content) = original {
        if pager.total_lines <= DEFAULT_READ_LINE_LIMIT {
            return Ok(content);
        }
    }

    let total = pager.total_lines;
    if total == 0 && line_byte_offset > 0 {
        return Err(ProductError::Other(format!(
            "line_byte_offset {line_byte_offset} is invalid because {display_path} is empty"
        )));
    }
    if total == 0 && offset > 1 {
        return Err(ProductError::Other(format!(
            "offset {offset} is past the end of {display_path} (0 lines)"
        )));
    }
    if offset.saturating_sub(1) >= total && total > 0 {
        return Err(ProductError::Other(format!(
            "offset {offset} is past the end of {display_path} ({total} lines)"
        )));
    }

    let mut body = pager.body;
    match pager.stop_reason {
        Some(ReadStopReason::ByteLimitWithinLine {
            line,
            next_byte_offset,
        }) => {
            let returned_line_byte_offset = if line == offset { line_byte_offset } else { 0 };
            body.push_str(&format!("\n[{display_path} 共 {total} 行；"));
            if let Some(last_complete) = pager.returned_last_line {
                body.push_str(&format!(
                    "本次完整返回第 {offset}–{last_complete} 行，并返回"
                ));
            } else {
                body.push_str("本次返回");
            }
            body.push_str(&format!(
                "第 {line} 行的 UTF-8 字节 {returned_line_byte_offset}..{next_byte_offset}（已达单次 {MAX_READ_BYTES} UTF-8 字节正文上限）。继续读取请调用 read_file 并设 offset={line}、line_byte_offset={next_byte_offset}]\n"
            ));
        }
        _ => {
            let last = pager.returned_last_line.unwrap_or(offset.saturating_sub(1));
            let has_more = last < total;
            if has_more || offset > 1 || line_byte_offset > 0 {
                let mut note =
                    format!("\n[{display_path} 共 {total} 行；本次返回第 {offset}–{last} 行");
                if line_byte_offset > 0 {
                    note.push_str(&format!(
                        "（第 {offset} 行从 UTF-8 字节 {line_byte_offset} 开始）"
                    ));
                }
                if has_more {
                    let reason = match pager.stop_reason {
                        Some(ReadStopReason::ByteLimitAfterLine) => {
                            format!("已达单次 {MAX_READ_BYTES} UTF-8 字节正文上限")
                        }
                        _ => format!("已达单次上限 {limit} 行"),
                    };
                    note.push_str(&format!(
                        "（{reason}）。继续读取请调用 read_file 并设 offset={}]\n",
                        last + 1
                    ));
                } else {
                    note.push_str("，已到文件末尾]\n");
                }
                body.push_str(&note);
            }
        }
    }
    Ok(body)
}

// ============================================================================
// list_files  [doc-02 §5] -- R0
// ============================================================================

/// `list_files` 工具 -- 列出目录中的文件与子目录。
///
/// R0：只读，无风险。
pub struct ListFilesTool;

#[async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }
    fn description(&self) -> &str {
        "List files and subdirectories in a directory."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path to list."
                }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        execute_list_files(&input, None)
    }

    async fn execute_with_context_and_abort_with_workspace(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        reject_if_cancelled(self.name(), abort_flag)?;
        execute_list_files(&input, workspace_guard).map(ToolExecutionResult::from)
    }
}

fn execute_list_files(
    input: &serde_json::Value,
    workspace_guard: Option<&PathGuard>,
) -> Result<String, ProductError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
    let names = match list_directory_names(path, workspace_guard) {
        Ok(names) => names,
        Err(error) => return Ok(format!("Error: cannot list {path}: {error}")),
    };
    serde_json::to_string(&names).map_err(|e| ProductError::Other(format!("JSON error: {e}")))
}

// ============================================================================
// git_status  [doc-02 §5] -- R0
// ============================================================================

/// `git_status` 工具 -- 获取仓库的 git 状态。
///
/// R0：只读。使用 `git status --porcelain=v1`。
pub struct GitStatusTool;

/// `path` 在模型契约中是可选的；缺省时必须由会话的 `PathGuard`
/// 注入已校验的工作区根，不能让执行器回落到 R-Code 进程 CWD。
const GIT_STATUS_PATH_BINDINGS: &[PathBinding] = &[PathBinding::default_root("path")];

#[async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }
    fn description(&self) -> &str {
        "Get the git status of a repository (porcelain format)."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn path_bindings(&self) -> &'static [PathBinding] {
        GIT_STATUS_PATH_BINDINGS
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Repository path (defaults to current working directory)."
                }
            }
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let mut command = std::process::Command::new("git");
        command.args(["-C", path, "status", "--porcelain=v1"]);
        hide_background_console(&mut command);
        let output = command
            .output()
            .map_err(|e| ProductError::GitError(format!("failed to run git: {e}")))?;
        if !output.status.success() {
            return Err(ProductError::GitError(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

// ============================================================================
// git_diff_stat -- R0
// ============================================================================

/// `git_diff_stat` 工具 -- 报告未提交变更的逐文件增删行数。
///
/// R0：只读。内部运行 `git diff --numstat`，汇总成与审查面板一致的
/// `+X −Y` 行数统计，让模型在编辑后无需再 shell 调用 `git diff`。
pub struct GitDiffStatTool;

/// `path` 在模型契约中是可选的；缺省时由会话的 `PathGuard` 注入工作区根。
const GIT_DIFF_STAT_PATH_BINDINGS: &[PathBinding] = &[PathBinding::default_root("path")];

#[async_trait]
impl Tool for GitDiffStatTool {
    fn name(&self) -> &str {
        "git_diff_stat"
    }
    fn description(&self) -> &str {
        "Report per-file insertions/deletions and totals for uncommitted changes in a git \
repository (git diff --numstat). Use it after editing files to report line-level change stats. \
Path defaults to the workspace root."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn path_bindings(&self) -> &'static [PathBinding] {
        GIT_DIFF_STAT_PATH_BINDINGS
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Repository path (defaults to current working directory)."
                }
            }
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let numstat = run_git(path, &["diff", "--numstat"])?;

        let mut files: Vec<(String, i64, i64)> = Vec::new();
        let mut binary_files: Vec<String> = Vec::new();
        let mut total_added: i64 = 0;
        let mut total_deleted: i64 = 0;

        for line in numstat.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.split('\t');
            let added_s = parts.next().unwrap_or("");
            let deleted_s = parts.next().unwrap_or("");
            let file = parts.next().unwrap_or("");
            if file.is_empty() {
                continue;
            }
            if added_s == "-" || deleted_s == "-" {
                binary_files.push(file.to_string());
                continue;
            }
            let added = added_s.parse::<i64>().unwrap_or(0);
            let deleted = deleted_s.parse::<i64>().unwrap_or(0);
            // 纯重命名在 numstat 里是 0/0，无行数变化，不计入统计。
            if added == 0 && deleted == 0 {
                continue;
            }
            total_added += added;
            total_deleted += deleted;
            files.push((file.to_string(), added, deleted));
        }

        if files.is_empty() && binary_files.is_empty() {
            return Ok("(无未提交变更)".to_string());
        }

        let mut out = format!(
            "{} 个文件变更，+{} −{}\n",
            files.len() + binary_files.len(),
            total_added,
            total_deleted
        );
        for (file, added, deleted) in files {
            out.push_str(&format!("{file} +{added} −{deleted}\n"));
        }
        for file in binary_files {
            out.push_str(&format!("{file} (二进制)\n"));
        }
        Ok(out)
    }
}

fn run_git(path: &str, args: &[&str]) -> Result<String, ProductError> {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(path).args(args);
    hide_background_console(&mut command);
    let output = command
        .output()
        .map_err(|e| ProductError::GitError(format!("failed to run git: {e}")))?;
    if !output.status.success() {
        return Err(ProductError::GitError(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ============================================================================
// load_skill  [doc-02 §5] -- R0, 路径穿越拒绝
// ============================================================================

/// `load_skill` 工具 -- 加载 SKILL.md 文件。
///
/// R0 只读，各模式可用。路径穿越（`..`）被拒绝。 [doc-02 §5]
pub struct LoadSkillTool;

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }
    fn description(&self) -> &str {
        "Load a SKILL.md file from the managed skill library."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the SKILL.md file."
                }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        execute_load_skill(&input, None)
    }

    async fn execute_with_context_and_abort_with_workspace(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        reject_if_cancelled(self.name(), abort_flag)?;
        execute_load_skill(&input, workspace_guard).map(ToolExecutionResult::from)
    }
}

fn execute_load_skill(
    input: &serde_json::Value,
    workspace_guard: Option<&PathGuard>,
) -> Result<String, ProductError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;

    // 路径穿越拒绝 [doc-02 §5]
    let p = Path::new(path);
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(ProductError::PathEscape(format!(
            "path traversal rejected: {path}"
        )));
    }

    read_text_file(path, workspace_guard)
        .map_err(|e| ProductError::Other(format!("failed to read skill {path}: {e}")))
}

// ============================================================================
// edit -- R2, 字面量精确替换
// ============================================================================

/// `edit` 工具 -- 用 `new_string` 替换文件中的 `old_string`。
///
/// R2：中风险。相比 `apply_patch` 的全文件覆盖，本工具有两个关键优势：
///
/// 1. **省 token**：改一行不必重发整个文件，长文件上差别是数量级的。
/// 2. **局部前置条件**：`old_string` 必须在当前磁盘内容里唯一命中，因此目标片段
///    已消失或变得歧义时会拒绝写入。可选 `expected_revision` 还能发现本次读取时已经
///    可见的整文件变化；它不是跨进程文件锁或严格 CAS，不能覆盖读取后的竞态窗口。
pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Replace an exact literal snippet in a file. This is the preferred way to modify files. \
old_string must appear exactly once, so include enough surrounding lines to make it unique \
(leading indentation must match). CRLF/LF and trailing line whitespace are handled conservatively. \
Set replace_all=true to replace every occurrence instead — useful for renaming a symbol. \
After a stale-anchor error, re-read the file and never retry unchanged arguments. Pass the returned \
current_revision as expected_revision for an additional best-effort stale check before the recovery \
write; old_string uniqueness remains the primary precondition. Optional postcondition literals \
can turn an already-completed retry into a verified no-op. \
These checks reduce stale writes but are not a cross-process lock or strict compare-and-swap."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R2
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact literal text to replace, including indentation."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text. Use an empty string to delete the snippet."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence instead of requiring exactly one. Default false."
                },
                "expected_revision": {
                    "type": "string",
                    "description": "Optional current_revision from a preceding edit error. Detects a change already visible when this edit starts and rejects it as stale_read; this is not a cross-process filesystem lock."
                },
                "postcondition": {
                    "type": "object",
                    "description": "Optional explicit invariant used only when old_string is already absent. If every must_contain literal is present and every must_not_contain literal is absent, the edit returns an already_satisfied no-op without writing.",
                    "properties": {
                        "must_contain": {
                            "type": "array",
                            "items": { "type": "string" },
                            "default": []
                        },
                        "must_not_contain": {
                            "type": "array",
                            "items": { "type": "string" },
                            "default": []
                        }
                    },
                    "additionalProperties": false
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        execute_edit(&input, None)
    }

    async fn execute_with_context_and_abort_with_workspace(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        reject_if_cancelled(self.name(), abort_flag)?;
        execute_edit(&input, workspace_guard).map(ToolExecutionResult::from)
    }
}

fn execute_edit(
    input: &serde_json::Value,
    workspace_guard: Option<&PathGuard>,
) -> Result<String, ProductError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
    let old_string = input
        .get("old_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'old_string' parameter".to_string()))?;
    let new_string = input
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'new_string' parameter".to_string()))?;
    let replace_all = input
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let expected_revision = input
        .get("expected_revision")
        .map(|value| {
            value.as_str().ok_or_else(|| {
                ProductError::Other("'expected_revision' must be a string".to_string())
            })
        })
        .transpose()?;
    let postcondition = parse_edit_postcondition(input.get("postcondition"))?;

    if old_string.is_empty() {
        return Err(ProductError::Other(
            "'old_string' must not be empty; use create_file to write a new file".to_string(),
        ));
    }
    if old_string == new_string {
        return Err(ProductError::Other(
            "'old_string' and 'new_string' are identical; nothing to do".to_string(),
        ));
    }

    let content = read_text_file(path, workspace_guard)?;
    let revision = edit_content_revision(&content);
    let display_path = path_for_display(path);
    let (matches, match_kind) = find_edit_matches(&content, old_string);

    if matches.is_empty() {
        if postcondition
            .as_ref()
            .is_some_and(|condition| edit_postcondition_is_satisfied(&content, condition))
        {
            return Ok(serde_json::json!({
                "status": "already_satisfied",
                "path": display_path,
                "revision": revision,
                "written": false,
                "message": "old_string is absent and the explicit postcondition is satisfied; no file write was needed"
            })
            .to_string());
        }
        return Err(recoverable_edit_error(
            "old_string_not_found",
            format!("'old_string' was not found in {display_path}"),
            serde_json::json!({
                "path": display_path,
                "current_revision": revision,
                "closest_line_numbers": closest_edit_line_numbers(&content, old_string),
                "required_action": "Re-read the smallest relevant region, check whether the intended postcondition is already satisfied, and otherwise build a new unique old_string from the latest content.",
                "retry_policy": "Do not retry the same path/old_string/new_string arguments unchanged. Use current_revision as expected_revision on a recovered write."
            }),
        ));
    }

    if let Some(expected) = expected_revision {
        if expected != revision {
            return Err(recoverable_edit_error(
                "stale_read",
                format!("{display_path} changed after the edit recovery context was captured"),
                serde_json::json!({
                    "path": display_path,
                    "expected_revision": expected,
                    "current_revision": revision,
                    "required_action": "Re-read the relevant region and rebuild the edit from the latest file."
                }),
            ));
        }
    }

    let occurrences = matches.len();
    if occurrences > 1 && !replace_all {
        let lines = match_ranges_line_numbers(&content, &matches);
        let shown = lines
            .iter()
            .take(10)
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(recoverable_edit_error(
            "old_string_not_unique",
            format!("'old_string' matches {occurrences} places in {display_path} (lines {shown})"),
            serde_json::json!({
                "path": display_path,
                "current_revision": revision,
                "occurrences": occurrences,
                "lines": lines.iter().take(10).copied().collect::<Vec<_>>(),
                "required_action": "Add the smallest amount of stable surrounding context needed to make old_string unique, or use replace_all only when every occurrence is intentionally targeted."
            }),
        ));
    }
    if replace_all
        && matches
            .windows(2)
            .any(|window| window[0].end > window[1].start)
    {
        return Err(recoverable_edit_error(
            "overlapping_equivalent_matches",
            format!(
                "newline/whitespace-equivalent matches overlap in {display_path}; replace_all is unsafe"
            ),
            serde_json::json!({
                "path": display_path,
                "current_revision": revision,
                "required_action": "Use a more specific non-overlapping anchor."
            }),
        ));
    }

    let replacement = replacement_with_file_line_endings(&content, new_string);
    let selected_matches = if replace_all {
        &matches[..]
    } else {
        &matches[..1]
    };
    let updated = replace_edit_ranges(&content, selected_matches, &replacement);
    atomic_write_scoped(Path::new(path), updated.as_bytes(), workspace_guard)?;

    let first_line = content[..matches[0].start].matches('\n').count() + 1;
    let compatibility_note = match match_kind {
        EditMatchKind::Exact => "",
        EditMatchKind::LineEndings => " (matched after normalizing CRLF/LF)",
        EditMatchKind::TrailingWhitespace => {
            " (matched after normalizing CRLF/LF and trailing line whitespace)"
        }
    };
    if replace_all {
        Ok(format!(
            "edited {display_path}: replaced {occurrences} occurrence(s){compatibility_note}"
        ))
    } else {
        Ok(format!(
            "edited {display_path} at line {first_line}{compatibility_note}"
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditMatchKind {
    Exact,
    LineEndings,
    TrailingWhitespace,
}

#[derive(Debug, Default)]
struct EditPostcondition {
    must_contain: Vec<String>,
    must_not_contain: Vec<String>,
}

fn parse_edit_postcondition(
    value: Option<&serde_json::Value>,
) -> Result<Option<EditPostcondition>, ProductError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| ProductError::Other("'postcondition' must be an object".to_string()))?;
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "must_contain" | "must_not_contain"))
    {
        return Err(ProductError::Other(format!(
            "postcondition contains unknown field '{key}'"
        )));
    }
    let parse_literals = |key: &str| -> Result<Vec<String>, ProductError> {
        let Some(value) = object.get(key) else {
            return Ok(Vec::new());
        };
        let values = value.as_array().ok_or_else(|| {
            ProductError::Other(format!("postcondition.{key} must be an array of strings"))
        })?;
        values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|literal| !literal.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        ProductError::Other(format!(
                            "postcondition.{key} must contain only non-empty strings"
                        ))
                    })
            })
            .collect()
    };
    let condition = EditPostcondition {
        must_contain: parse_literals("must_contain")?,
        must_not_contain: parse_literals("must_not_contain")?,
    };
    if condition.must_contain.is_empty() && condition.must_not_contain.is_empty() {
        return Err(ProductError::Other(
            "'postcondition' must declare at least one literal invariant".to_string(),
        ));
    }
    if condition
        .must_contain
        .iter()
        .any(|literal| condition.must_not_contain.contains(literal))
    {
        return Err(ProductError::Other(
            "postcondition must not place the same literal in both must_contain and must_not_contain"
                .to_string(),
        ));
    }
    Ok(Some(condition))
}

fn edit_postcondition_is_satisfied(content: &str, condition: &EditPostcondition) -> bool {
    condition
        .must_contain
        .iter()
        .all(|literal| !find_edit_matches(content, literal).0.is_empty())
        && condition
            .must_not_contain
            .iter()
            .all(|literal| find_edit_matches(content, literal).0.is_empty())
}

fn recoverable_edit_error(
    code: impl Into<String>,
    message: impl Into<String>,
    details: serde_json::Value,
) -> ProductError {
    ProductError::RecoverableToolError {
        tool: "edit".to_string(),
        code: code.into(),
        message: message.into(),
        details,
    }
}

fn edit_content_revision(content: &str) -> String {
    format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())
}

fn exact_match_ranges(content: &str, needle: &str) -> Vec<Range<usize>> {
    content
        .match_indices(needle)
        .map(|(start, matched)| start..start + matched.len())
        .collect()
}

fn find_edit_matches(content: &str, needle: &str) -> (Vec<Range<usize>>, EditMatchKind) {
    let mut matches = exact_match_ranges(content, needle);
    let mut match_kind = EditMatchKind::Exact;

    let normalized = needle.replace("\r\n", "\n");
    let crlf = normalized.replace('\n', "\r\n");
    for candidate in [normalized.as_str(), crlf.as_str()] {
        if candidate == needle || candidate.is_empty() {
            continue;
        }
        for candidate_match in exact_match_ranges(content, candidate) {
            if !matches
                .iter()
                .any(|existing| existing.start == candidate_match.start)
            {
                matches.push(candidate_match);
                if match_kind == EditMatchKind::Exact {
                    match_kind = EditMatchKind::LineEndings;
                }
            }
        }
    }

    for candidate_match in line_whitespace_match_ranges(content, needle) {
        if !matches
            .iter()
            .any(|existing| existing.start == candidate_match.start)
        {
            matches.push(candidate_match);
            match_kind = EditMatchKind::TrailingWhitespace;
        }
    }
    matches.sort_unstable_by_key(|range| range.start);
    (matches, match_kind)
}

#[derive(Debug)]
struct TextLine<'a> {
    start: usize,
    body_end: usize,
    end: usize,
    body: &'a str,
    has_line_ending: bool,
}

fn text_lines(text: &str) -> Vec<TextLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for segment in text.split_inclusive('\n') {
        let end = start + segment.len();
        let body_end = if segment.ends_with("\r\n") {
            end - 2
        } else if segment.ends_with('\n') {
            end - 1
        } else {
            end
        };
        lines.push(TextLine {
            start,
            body_end,
            end,
            body: &text[start..body_end],
            has_line_ending: end > body_end,
        });
        start = end;
    }
    lines
}

fn line_whitespace_match_ranges(content: &str, needle: &str) -> Vec<Range<usize>> {
    let content_lines = text_lines(content);
    let needle_lines = text_lines(needle);
    if needle_lines.is_empty()
        || needle_lines
            .iter()
            .all(|line| line.body.trim_matches([' ', '\t']).is_empty())
    {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut index = 0usize;
    while index + needle_lines.len() <= content_lines.len() {
        let window = &content_lines[index..index + needle_lines.len()];
        let bodies_match = window.iter().zip(&needle_lines).all(|(current, expected)| {
            current.body.trim_end_matches([' ', '\t'])
                == expected.body.trim_end_matches([' ', '\t'])
                && (!expected.has_line_ending || current.has_line_ending)
        });
        if bodies_match {
            let last_current = window.last().expect("non-empty line window");
            let last_expected = needle_lines.last().expect("non-empty needle lines");
            let end = if last_expected.has_line_ending {
                last_current.end
            } else {
                last_current.body_end
            };
            matches.push(window[0].start..end);
            index += needle_lines.len();
        } else {
            index += 1;
        }
    }
    matches
}

fn replacement_with_file_line_endings(content: &str, replacement: &str) -> String {
    let crlf_count = content.match_indices("\r\n").count();
    let lf_count = content.bytes().filter(|byte| *byte == b'\n').count();
    let lone_lf_count = lf_count.saturating_sub(crlf_count);
    let normalized = replacement.replace("\r\n", "\n");
    if crlf_count > 0 && lone_lf_count == 0 {
        normalized.replace('\n', "\r\n")
    } else if lone_lf_count > 0 && crlf_count == 0 {
        normalized
    } else {
        replacement.to_string()
    }
}

fn replace_edit_ranges(content: &str, matches: &[Range<usize>], replacement: &str) -> String {
    let mut updated = content.to_string();
    for range in matches.iter().rev() {
        updated.replace_range(range.clone(), replacement);
    }
    updated
}

fn match_ranges_line_numbers(content: &str, matches: &[Range<usize>]) -> Vec<usize> {
    matches
        .iter()
        .map(|range| content[..range.start].matches('\n').count() + 1)
        .collect()
}

fn closest_edit_line_numbers(content: &str, needle: &str) -> Vec<usize> {
    let mut anchors = needle
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    anchors.sort_unstable();
    anchors.dedup();

    content
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim();
            anchors
                .iter()
                .any(|anchor| trimmed == *anchor || trimmed.contains(anchor))
                .then_some(index + 1)
        })
        .take(8)
        .collect()
}

// ============================================================================
// apply_patch  [doc-18 M8-02] -- R2, 原子化全文件替换
// ============================================================================

/// `apply_patch` 工具 -- 原子化应用补丁（全文件替换）。
///
/// R2：中风险，可能修改状态。需要用户确认。
/// 使用临时文件 + rename 策略保证原子性。
pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }
    fn description(&self) -> &str {
        "Apply a full-file patch: atomically replace the contents of a file with new content."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R2
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to patch."
                },
                "content": {
                    "type": "string",
                    "description": "New file content."
                }
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        execute_apply_patch(&input, None)
    }

    async fn execute_with_context_and_abort_with_workspace(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        reject_if_cancelled(self.name(), abort_flag)?;
        execute_apply_patch(&input, workspace_guard).map(ToolExecutionResult::from)
    }
}

fn execute_apply_patch(
    input: &serde_json::Value,
    workspace_guard: Option<&PathGuard>,
) -> Result<String, ProductError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
    let content = input
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'content' parameter".to_string()))?;

    atomic_write_scoped(Path::new(path), content.as_bytes(), workspace_guard)?;
    Ok(format!("patched {}", path_for_display(path)))
}

// ============================================================================
// create_file  [doc-18 M8-02] -- R2, 创建新文件
// ============================================================================

/// `create_file` 工具 -- 创建新文件。
///
/// R2：中风险。若文件已存在则失败（不覆盖）。
pub struct CreateFileTool;

#[async_trait]
impl Tool for CreateFileTool {
    fn name(&self) -> &str {
        "create_file"
    }
    fn description(&self) -> &str {
        "Create a new file with the given content. Fails if the file already exists."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R2
    }
    fn requires_existing_path(&self) -> bool {
        false
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the new file."
                },
                "content": {
                    "type": "string",
                    "description": "File content."
                }
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        execute_create_file(&input, None)
    }

    async fn execute_with_context_and_abort_with_workspace(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        reject_if_cancelled(self.name(), abort_flag)?;
        execute_create_file(&input, workspace_guard).map(ToolExecutionResult::from)
    }
}

fn execute_create_file(
    input: &serde_json::Value,
    workspace_guard: Option<&PathGuard>,
) -> Result<String, ProductError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
    let content = input
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'content' parameter".to_string()))?;

    create_new_file_scoped(Path::new(path), content.as_bytes(), workspace_guard)?;
    Ok(format!("created {}", path_for_display(path)))
}

// ============================================================================
// delete_file  [doc-18 M8-02] -- R2, 删除文件
// ============================================================================

/// `delete_file` 工具 -- 删除文件。
///
/// R2：中风险，可能不可逆。若文件不存在则失败。
pub struct DeleteFileTool;

#[async_trait]
impl Tool for DeleteFileTool {
    fn name(&self) -> &str {
        "delete_file"
    }
    fn description(&self) -> &str {
        "Delete a file. Fails if the file does not exist."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R2
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to delete."
                }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        execute_delete_file(&input, None)
    }

    async fn execute_with_context_and_abort_with_workspace(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
        abort_flag: Option<&std::sync::atomic::AtomicBool>,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        reject_if_cancelled(self.name(), abort_flag)?;
        execute_delete_file(&input, workspace_guard).map(ToolExecutionResult::from)
    }
}

fn execute_delete_file(
    input: &serde_json::Value,
    workspace_guard: Option<&PathGuard>,
) -> Result<String, ProductError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;

    remove_file_scoped(Path::new(path), workspace_guard)?;
    Ok(format!("deleted {}", path_for_display(path)))
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 原子写入：先写同目录临时文件，再 rename 覆盖目标。
fn atomic_write(path: &Path, content: &[u8]) -> Result<(), ProductError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp_name = format!(".{file_name}.r-code-tmp-{}", uuid::Uuid::new_v4());
    let tmp_path = dir.join(tmp_name);

    std::fs::write(&tmp_path, content).map_err(|e| {
        ProductError::Other(format!(
            "failed to write temp file {}: {e}",
            path_for_display(&tmp_path)
        ))
    })?;

    std::fs::rename(&tmp_path, path).map_err(|e| {
        // 清理残留临时文件
        let _ = std::fs::remove_file(&tmp_path);
        ProductError::Other(format!(
            "failed to rename temp file to {}: {e}",
            path_for_display(path)
        ))
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn read_file_basic() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();

        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();
        assert_eq!(result, "hello world");
        assert_eq!(tool.risk_level(), RiskLevel::R1);
    }

    #[tokio::test]
    async fn read_file_missing_path() {
        let tool = ReadFileTool;
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_file_nonexistent() {
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({ "path": "/nonexistent/file.txt" }))
            .await;
        assert!(result.is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_paths_are_hidden_only_at_file_tool_output_boundaries() {
        let dir = TempDir::new().unwrap();
        let canonical_root = fs::canonicalize(dir.path()).unwrap();
        let raw_root = canonical_root.to_string_lossy();
        assert!(
            raw_root.starts_with(r"\\?\"),
            "canonical test path was not verbatim: {raw_root}"
        );

        let existing = canonical_root.join("existing.txt");
        fs::write(&existing, "one\ntwo\n").unwrap();
        let raw_existing = existing.to_string_lossy().into_owned();
        let display_existing = path_for_display(&existing);

        let read = execute_read_file(
            &serde_json::json!({ "path": raw_existing, "limit": 1 }),
            None,
        )
        .unwrap();
        assert!(read.contains(&display_existing), "output was: {read}");
        assert!(!read.contains(r"\\?\"), "output was: {read}");

        let patched = execute_apply_patch(
            &serde_json::json!({ "path": raw_existing, "content": "patched\n" }),
            None,
        )
        .unwrap();
        assert_eq!(patched, format!("patched {display_existing}"));
        assert_eq!(fs::read_to_string(&existing).unwrap(), "patched\n");

        let created = canonical_root.join("created.txt");
        let raw_created = created.to_string_lossy().into_owned();
        let display_created = path_for_display(&created);
        let created_output = execute_create_file(
            &serde_json::json!({ "path": raw_created, "content": "new\n" }),
            None,
        )
        .unwrap();
        assert_eq!(created_output, format!("created {display_created}"));
        assert_eq!(fs::read_to_string(&created).unwrap(), "new\n");

        let duplicate_error = execute_create_file(
            &serde_json::json!({ "path": raw_created, "content": "duplicate\n" }),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(
            duplicate_error.contains(&display_created),
            "error was: {duplicate_error}"
        );
        assert!(
            !duplicate_error.contains(r"\\?\"),
            "error was: {duplicate_error}"
        );

        let deleted =
            execute_delete_file(&serde_json::json!({ "path": raw_created }), None).unwrap();
        assert_eq!(deleted, format!("deleted {display_created}"));
        assert!(!created.exists());

        let missing = canonical_root.join("missing.txt");
        let raw_missing = missing.to_string_lossy().into_owned();
        let display_missing = path_for_display(&missing);
        let missing_error = execute_read_file(&serde_json::json!({ "path": raw_missing }), None)
            .unwrap_err()
            .to_string();
        assert!(
            missing_error.contains(&display_missing),
            "error was: {missing_error}"
        );
        assert!(
            !missing_error.contains(r"\\?\"),
            "error was: {missing_error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_guarded_file_tools_reject_symlink_escape() {
        use std::os::unix::fs::symlink;

        let outer = TempDir::new().unwrap();
        let root = outer.path().join("workspace");
        let outside_dir = outer.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside_dir).unwrap();
        let outside_file = outside_dir.join("secret.txt");
        fs::write(&outside_file, "secret").unwrap();
        let guard = PathGuard::new(root.clone()).unwrap();

        let escape_file = root.join("escape-file");
        let escape_dir = root.join("escape-dir");
        symlink(&outside_file, &escape_file).unwrap();
        symlink(&outside_dir, &escape_dir).unwrap();
        let escape_file = escape_file.to_string_lossy().to_string();
        let escape_dir = escape_dir.to_string_lossy().to_string();

        assert!(
            execute_read_file(&serde_json::json!({ "path": escape_file }), Some(&guard)).is_err()
        );
        assert!(
            execute_load_skill(&serde_json::json!({ "path": escape_file }), Some(&guard)).is_err()
        );
        let listed =
            execute_list_files(&serde_json::json!({ "path": escape_dir }), Some(&guard)).unwrap();
        assert!(listed.contains("path escape"), "listing was: {listed}");
        assert!(execute_edit(
            &serde_json::json!({
                "path": escape_file,
                "old_string": "secret",
                "new_string": "changed"
            }),
            Some(&guard)
        )
        .is_err());
        assert!(execute_apply_patch(
            &serde_json::json!({ "path": escape_file, "content": "changed" }),
            Some(&guard)
        )
        .is_err());
        assert!(execute_create_file(
            &serde_json::json!({ "path": format!("{escape_dir}/created.txt"), "content": "new" }),
            Some(&guard)
        )
        .is_err());
        assert!(
            execute_delete_file(&serde_json::json!({ "path": escape_file }), Some(&guard)).is_err()
        );

        assert_eq!(fs::read_to_string(&outside_file).unwrap(), "secret");
        assert!(!outside_dir.join("created.txt").exists());
    }

    #[tokio::test]
    async fn list_files_basic() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(dir.path().join("b.txt"), "b").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        let tool = ListFilesTool;
        let result = tool
            .execute(serde_json::json!({ "path": dir.path().to_str().unwrap() }))
            .await
            .unwrap();
        let names: Vec<String> = serde_json::from_str(&result).unwrap();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"b.txt".to_string()));
        assert!(names.contains(&"sub".to_string()));
        assert_eq!(tool.risk_level(), RiskLevel::R0);
    }

    #[tokio::test]
    async fn list_files_missing_path() {
        let tool = ListFilesTool;
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn git_status_runs() {
        // 只在 git 可用时测试
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = TempDir::new().unwrap();
        // 初始化一个临时 git 仓库
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join("new.txt"), "content").unwrap();

        let tool = GitStatusTool;
        let result = tool
            .execute(serde_json::json!({ "path": dir.path().to_str().unwrap() }))
            .await
            .unwrap();
        assert!(result.contains("new.txt"));
        assert_eq!(tool.risk_level(), RiskLevel::R0);
    }

    #[tokio::test]
    async fn git_status_default_path() {
        let tool = GitStatusTool;
        // 不传 path，使用 CWD（可能是 git 仓库也可能不是，只要不 panic 即可）
        let _ = tool.execute(serde_json::json!({})).await;
    }

    #[test]
    fn git_status_declares_workspace_root_as_its_missing_path_default() {
        let bindings = GitStatusTool.path_bindings();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].key, "path");
        assert_eq!(bindings[0].arity, crate::gateway::PathArity::DefaultRoot);
    }

    #[tokio::test]
    async fn load_skill_reads_file() {
        let dir = TempDir::new().unwrap();
        let skill = dir.path().join("SKILL.md");
        fs::write(&skill, "# Skill\n\nInstructions here").unwrap();

        let tool = LoadSkillTool;
        let result = tool
            .execute(serde_json::json!({ "path": skill.to_str().unwrap() }))
            .await
            .unwrap();
        assert!(result.contains("# Skill"));
        assert_eq!(tool.risk_level(), RiskLevel::R0);
    }

    #[tokio::test]
    async fn load_skill_rejects_traversal() {
        let tool = LoadSkillTool;
        let result = tool
            .execute(serde_json::json!({ "path": "../../../etc/passwd" }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)));
    }

    #[tokio::test]
    async fn load_skill_rejects_traversal_in_middle() {
        let tool = LoadSkillTool;
        let result = tool
            .execute(serde_json::json!({ "path": "skills/../etc/passwd" }))
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProductError::PathEscape(_)));
    }

    #[tokio::test]
    async fn load_skill_missing_path() {
        let tool = LoadSkillTool;
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn load_skill_nonexistent_file() {
        let tool = LoadSkillTool;
        let result = tool
            .execute(serde_json::json!({ "path": "/nonexistent/SKILL.md" }))
            .await;
        assert!(result.is_err());
    }

    // ── read_file 分页 ────────────────────────────────────────

    #[tokio::test]
    async fn read_file_small_file_is_returned_verbatim() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("s.txt");
        fs::write(&file, "line1\nline2\n").unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();
        // 不分页、不超限时行为与改造前完全一致（含末尾换行）
        assert_eq!(out, "line1\nline2\n");
    }

    #[tokio::test]
    async fn read_file_small_utf8_file_preserves_original_bytes() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("mixed-newlines.txt");
        let content = "第一行\r\nsecond\rthird\n最后一行";
        fs::write(&file, content.as_bytes()).unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();
        assert_eq!(out.as_bytes(), content.as_bytes());
    }

    #[tokio::test]
    async fn read_file_offset_and_limit() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("m.txt");
        let body: String = (1..=100).map(|i| format!("line{i}\n")).collect();
        fs::write(&file, body).unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 10, "limit": 3
            }))
            .await
            .unwrap();
        assert!(out.starts_with("line10\nline11\nline12\n"));
        assert!(out.contains("共 100 行"));
        assert!(out.contains("offset=13"));
    }

    #[tokio::test]
    async fn read_file_last_page_says_end_of_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("m.txt");
        fs::write(&file, "a\nb\nc\n").unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 3
            }))
            .await
            .unwrap();
        assert!(out.contains('c'));
        assert!(out.contains("已到文件末尾"));
        assert!(!out.contains("offset="));
    }

    #[tokio::test]
    async fn read_file_offset_past_end_is_an_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("m.txt");
        fs::write(&file, "a\nb\n").unwrap();

        let result = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 99
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_file_truncates_huge_file_without_paging() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("big.txt");
        let body: String = (0..DEFAULT_READ_LINE_LIMIT + 500)
            .map(|i| format!("l{i}\n"))
            .collect();
        fs::write(&file, body).unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();
        assert!(out.contains("已达单次上限"));
        assert!(out.contains(&format!("offset={}", DEFAULT_READ_LINE_LIMIT + 1)));
    }

    #[tokio::test]
    async fn read_file_long_single_line_advances_with_line_byte_offset() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("long-line.txt");
        let body = "x".repeat(MAX_READ_BYTES * 2 + 37);
        fs::write(&file, &body).unwrap();

        let first = ReadFileTool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();
        assert!(first.starts_with(&"x".repeat(MAX_READ_BYTES)));
        assert!(first.contains("UTF-8 字节"));
        assert!(first.contains(&format!("line_byte_offset={MAX_READ_BYTES}")));

        let second = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "offset": 1,
                "line_byte_offset": MAX_READ_BYTES
            }))
            .await
            .unwrap();
        assert!(second.starts_with(&"x".repeat(MAX_READ_BYTES)));
        assert!(second.contains(&format!("line_byte_offset={}", MAX_READ_BYTES * 2)));

        let third = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "offset": 1,
                "line_byte_offset": MAX_READ_BYTES * 2
            }))
            .await
            .unwrap();
        assert!(third.starts_with(&"x".repeat(37)));
        assert!(third.contains("已到文件末尾"));
        assert!(!third.contains("line_byte_offset="));
    }

    #[tokio::test]
    async fn read_file_long_cjk_line_uses_utf8_byte_boundaries() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("long-cjk-line.txt");
        let body = "界".repeat(MAX_READ_BYTES / "界".len() + 5);
        fs::write(&file, &body).unwrap();
        let expected_next_byte = MAX_READ_BYTES - (MAX_READ_BYTES % "界".len());

        let first = ReadFileTool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();
        assert!(first.starts_with(&body[..expected_next_byte]));
        assert!(first.contains(&format!("line_byte_offset={expected_next_byte}")));

        let second = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "offset": 1,
                "line_byte_offset": expected_next_byte
            }))
            .await
            .unwrap();
        assert!(second.starts_with(&body[expected_next_byte..]));
        assert!(second.contains("已到文件末尾"));
    }

    #[tokio::test]
    async fn read_file_rejects_line_byte_offset_inside_utf8_scalar() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("cjk.txt");
        fs::write(&file, "界tail").unwrap();

        let result = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 1, "line_byte_offset": 1
            }))
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("UTF-8 character boundary"));
    }

    #[tokio::test]
    async fn read_file_rejects_line_byte_offset_past_line_end() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("short.txt");
        fs::write(&file, "short\nnext\n").unwrap();

        let result = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 1, "line_byte_offset": 6
            }))
            .await;
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("past the end of line 1"),
            "message was: {error}"
        );
    }

    #[tokio::test]
    async fn read_file_crlf_paging_matches_existing_line_semantics() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("crlf.txt");
        fs::write(&file, "first\r\nsecond\r\nthird\r\n").unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 2, "limit": 1
            }))
            .await
            .unwrap();
        assert!(out.starts_with("second\n"));
        assert!(!out.starts_with("second\r\n"));
        assert!(out.contains("共 3 行"));
        assert!(out.contains("offset=3"));
    }

    #[tokio::test]
    async fn read_file_cjk_normal_page_reports_utf8_bytes_consistently() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("cjk-lines.txt");
        let first_line = "界".repeat(MAX_READ_BYTES / "界".len());
        fs::write(&file, format!("{first_line}\n下一行\n")).unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();
        assert!(out.starts_with(&first_line));
        assert!(out.contains("UTF-8 字节正文上限"));
        assert!(out.contains("offset=2"));
        assert!(!out.contains("字符上限"));
    }

    #[tokio::test]
    async fn read_file_byte_cap_on_later_line_reports_that_line_from_byte_zero() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("later-long-line.txt");
        let long_line = "x".repeat(MAX_READ_BYTES);
        fs::write(&file, format!("a\n{long_line}\nlast\n")).unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();
        assert!(out.starts_with("a\n"));
        assert!(out.contains(&format!("第 2 行的 UTF-8 字节 0..{}", MAX_READ_BYTES - 2)));
        assert!(out.contains(&format!(
            "offset=2、line_byte_offset={}",
            MAX_READ_BYTES - 2
        )));
    }

    #[tokio::test]
    async fn read_file_paged_last_line_without_trailing_newline_reaches_eof() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("no-trailing-newline.txt");
        fs::write(&file, "first\nlast").unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 2, "limit": 1
            }))
            .await
            .unwrap();
        assert!(out.starts_with("last\n"));
        assert!(out.contains("已到文件末尾"));
        assert!(!out.contains("offset="));
    }

    #[tokio::test]
    async fn read_file_empty_file_remains_empty_when_paged() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("empty.txt");
        fs::write(&file, "").unwrap();

        let out = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 1, "limit": 10
            }))
            .await
            .unwrap();
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn read_file_empty_file_rejects_nonzero_line_byte_offset() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("empty.txt");
        fs::write(&file, "").unwrap();

        let result = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 1, "line_byte_offset": 1
            }))
            .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("is empty"), "message was: {error}");
    }

    #[tokio::test]
    async fn read_file_empty_file_rejects_offset_past_first_line() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("empty.txt");
        fs::write(&file, "").unwrap();

        let result = ReadFileTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "offset": 2
            }))
            .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("0 lines"), "message was: {error}");
    }

    // ── edit ──────────────────────────────────────────────────

    #[tokio::test]
    async fn edit_replaces_unique_snippet() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.rs");
        fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();

        let out = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "let x = 1;",
                "new_string": "let x = 42;"
            }))
            .await
            .unwrap();
        assert!(out.contains("line 2"), "message was: {out}");
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "fn main() {\n    let x = 42;\n}\n"
        );
        assert_eq!(EditTool.risk_level(), RiskLevel::R2);
    }

    #[tokio::test]
    async fn edit_rejects_ambiguous_match() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.rs");
        fs::write(&file, "let x = 1;\nlet y = 2;\nlet x = 1;\n").unwrap();

        let err = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "let x = 1;",
                "new_string": "let x = 9;"
            }))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("matches 2 places"), "message was: {err}");
        assert!(err.contains("lines 1, 3"), "message was: {err}");
        // 失败必须不落盘
        assert!(fs::read_to_string(&file)
            .unwrap()
            .contains("let x = 1;\nlet y"));
    }

    #[tokio::test]
    async fn edit_replace_all_rewrites_every_occurrence() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.rs");
        fs::write(&file, "old\nkeep\nold\n").unwrap();

        let out = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "old",
                "new_string": "new",
                "replace_all": true
            }))
            .await
            .unwrap();
        assert!(out.contains('2'), "message was: {out}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "new\nkeep\nnew\n");
    }

    #[tokio::test]
    async fn edit_reports_missing_snippet() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.rs");
        fs::write(&file, "hello\n").unwrap();

        let err = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "goodbye",
                "new_string": "hi"
            }))
            .await
            .unwrap_err();
        match err {
            ProductError::RecoverableToolError {
                tool,
                code,
                message,
                details,
            } => {
                assert_eq!(tool, "edit");
                assert_eq!(code, "old_string_not_found");
                assert!(message.contains("not found"), "message was: {message}");
                assert!(details["current_revision"]
                    .as_str()
                    .is_some_and(|revision| revision.starts_with("blake3:")));
                assert!(details["retry_policy"].as_str().is_some());
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn edit_matches_lf_anchor_in_crlf_file_and_preserves_crlf() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("windows.rs");
        fs::write(&file, "enum Origin {\r\n    Agent,\r\n    Undo,\r\n}\r\n").unwrap();

        let out = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "    Agent,\n    Undo,",
                "new_string": "    Agent,\n    Reviewed,"
            }))
            .await
            .unwrap();

        assert!(out.contains("normalizing CRLF/LF"), "message was: {out}");
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "enum Origin {\r\n    Agent,\r\n    Reviewed,\r\n}\r\n"
        );
    }

    #[tokio::test]
    async fn edit_matches_crlf_anchor_in_lf_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("unix.rs");
        fs::write(&file, "Agent,\nUndo,\n").unwrap();

        EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "Agent,\r\nUndo,",
                "new_string": "Agent,\r\nReviewed,"
            }))
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "Agent,\nReviewed,\n");
    }

    #[tokio::test]
    async fn edit_treats_exact_and_crlf_equivalent_matches_as_ambiguous() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("mixed.txt");
        fs::write(&file, "old\nold\r\n").unwrap();

        let err = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "old\n",
                "new_string": "new\n"
            }))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProductError::RecoverableToolError { ref code, .. }
                if code == "old_string_not_unique"
        ));

        EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "old\n",
                "new_string": "new\n",
                "replace_all": true
            }))
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new\nnew\n");
    }

    #[tokio::test]
    async fn edit_replace_all_rejects_overlapping_equivalent_matches() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("overlapping.txt");
        fs::write(&file, "a\r\na\na").unwrap();

        let err = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "a\r\na",
                "new_string": "replaced",
                "replace_all": true
            }))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProductError::RecoverableToolError { ref code, .. }
                if code == "overlapping_equivalent_matches"
        ));
        assert_eq!(fs::read_to_string(&file).unwrap(), "a\r\na\na");
    }

    #[tokio::test]
    async fn edit_uses_utf8_byte_ranges_without_shifting_the_match() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("unicode.txt");
        fs::write(&file, "界面\r\nold\r\nkeep\r\n").unwrap();

        EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "old\n",
                "new_string": "新值\n"
            }))
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "界面\r\n新值\r\nkeep\r\n"
        );
    }

    #[tokio::test]
    async fn edit_conservatively_ignores_only_trailing_line_whitespace() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("whitespace.txt");
        fs::write(&file, "alpha  \r\nbeta\t\r\nkeep\r\n").unwrap();

        let out = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "alpha\nbeta\n",
                "new_string": "done\n"
            }))
            .await
            .unwrap();
        assert!(out.contains("trailing line whitespace"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "done\r\nkeep\r\n");

        fs::write(&file, "    alpha  \r\n").unwrap();
        let err = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "alpha \n",
                "new_string": "done\n"
            }))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProductError::RecoverableToolError { ref code, .. }
                if code == "old_string_not_found"
        ));
        assert_eq!(fs::read_to_string(&file).unwrap(), "    alpha  \r\n");
    }

    #[tokio::test]
    async fn edit_postcondition_can_verify_an_already_completed_noop() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("memory.rs");
        let current = "enum Origin {\r\n    Agent,\r\n    Undo,\r\n}\r\n";
        fs::write(&file, current).unwrap();

        let out = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "    Undo,\n    Undo,",
                "new_string": "    Undo,",
                "postcondition": {
                    "must_contain": ["    Undo,"],
                    "must_not_contain": ["    Undo,\n    Undo,"]
                }
            }))
            .await
            .unwrap();
        let outcome: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(outcome["status"], "already_satisfied");
        assert_eq!(outcome["written"], false);
        assert_eq!(fs::read_to_string(&file).unwrap(), current);
    }

    #[tokio::test]
    async fn edit_postcondition_rejects_unknown_or_contradictory_invariants() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("memory.rs");
        fs::write(&file, "Undo,\n").unwrap();
        let path = file.to_str().unwrap();

        let unknown = EditTool
            .execute(serde_json::json!({
                "path": path,
                "old_string": "Undo,\nUndo,",
                "new_string": "Undo,",
                "postcondition": {
                    "must_contain": ["Undo,"],
                    "must_not_contians": ["Undo,\nUndo,"]
                }
            }))
            .await
            .unwrap_err();
        assert!(unknown.to_string().contains("unknown field"));

        let contradictory = EditTool
            .execute(serde_json::json!({
                "path": path,
                "old_string": "Undo,\nUndo,",
                "new_string": "Undo,",
                "postcondition": {
                    "must_contain": ["Undo,"],
                    "must_not_contain": ["Undo,"]
                }
            }))
            .await
            .unwrap_err();
        assert!(contradictory.to_string().contains("both must_contain"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "Undo,\n");
    }

    #[tokio::test]
    async fn edit_postcondition_contradiction_does_not_echo_literals() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("memory.rs");
        fs::write(&file, "current\n").unwrap();
        let secret = "DO_NOT_ECHO_POSTCONDITION_LITERAL";

        let message = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "missing",
                "new_string": "replacement",
                "postcondition": {
                    "must_contain": [secret],
                    "must_not_contain": [secret]
                }
            }))
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("both must_contain"));
        assert!(
            !message.contains(secret),
            "postcondition validation error leaked the literal"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "current\n");
    }

    #[tokio::test]
    async fn edit_expected_revision_rejects_a_second_concurrent_change() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("stale.rs");
        fs::write(&file, "let value = 1;\n").unwrap();
        let revision = edit_content_revision("let value = 1;\n");
        fs::write(&file, "let value = 2;\n").unwrap();

        let err = EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "let value = 2;",
                "new_string": "let value = 3;",
                "expected_revision": revision
            }))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProductError::RecoverableToolError { ref code, .. } if code == "stale_read"
        ));
        assert_eq!(fs::read_to_string(&file).unwrap(), "let value = 2;\n");
    }

    #[tokio::test]
    async fn edit_can_delete_a_snippet() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.rs");
        fs::write(&file, "keep\nDROP ME\nkeep2\n").unwrap();

        EditTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "DROP ME\n",
                "new_string": ""
            }))
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "keep\nkeep2\n");
    }

    #[tokio::test]
    async fn edit_rejects_degenerate_input() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.rs");
        fs::write(&file, "x\n").unwrap();
        let path = file.to_str().unwrap();

        // 空 old_string
        assert!(EditTool
            .execute(serde_json::json!({"path": path, "old_string": "", "new_string": "y"}))
            .await
            .is_err());
        // 新旧相同
        assert!(EditTool
            .execute(serde_json::json!({"path": path, "old_string": "x", "new_string": "x"}))
            .await
            .is_err());
        // 缺参数
        assert!(EditTool
            .execute(serde_json::json!({"path": path, "old_string": "x"}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn edit_missing_file_is_an_error() {
        let err = EditTool
            .execute(serde_json::json!({
                "path": "/nonexistent/a.rs", "old_string": "a", "new_string": "b"
            }))
            .await;
        assert!(err.is_err());
    }

    #[test]
    fn tool_names_and_descriptions() {
        assert_eq!(ReadFileTool.name(), "read_file");
        assert!(!ReadFileTool.description().is_empty());
        assert_eq!(ListFilesTool.name(), "list_files");
        assert_eq!(GitStatusTool.name(), "git_status");
        assert_eq!(GitDiffStatTool.name(), "git_diff_stat");
        assert!(!GitDiffStatTool.description().is_empty());
        assert_eq!(EditTool.name(), "edit");
        assert!(!EditTool.description().is_empty());
        assert_eq!(LoadSkillTool.name(), "load_skill");
        assert_eq!(ApplyPatchTool.name(), "apply_patch");
        assert!(!ApplyPatchTool.description().is_empty());
        assert_eq!(CreateFileTool.name(), "create_file");
        assert_eq!(DeleteFileTool.name(), "delete_file");
    }

    #[test]
    fn tool_input_schemas_are_objects() {
        for schema in [
            ReadFileTool.input_schema(),
            ListFilesTool.input_schema(),
            GitStatusTool.input_schema(),
            GitDiffStatTool.input_schema(),
            EditTool.input_schema(),
            LoadSkillTool.input_schema(),
            ApplyPatchTool.input_schema(),
            CreateFileTool.input_schema(),
            DeleteFileTool.input_schema(),
        ] {
            assert_eq!(schema["type"], "object");
        }
    }

    #[test]
    fn write_tools_are_r2() {
        assert_eq!(ApplyPatchTool.risk_level(), RiskLevel::R2);
        assert_eq!(CreateFileTool.risk_level(), RiskLevel::R2);
        assert_eq!(DeleteFileTool.risk_level(), RiskLevel::R2);
    }

    // --------------------------------------------------------------------------
    // apply_patch 工具测试
    // --------------------------------------------------------------------------

    #[tokio::test]
    async fn apply_patch_creates_new_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("patched.txt");

        let tool = ApplyPatchTool;
        let result = tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "new content"
            }))
            .await
            .unwrap();
        assert!(result.contains("patched"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "new content");
    }

    #[tokio::test]
    async fn apply_patch_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("existing.txt");
        fs::write(&file, "old content").unwrap();

        let tool = ApplyPatchTool;
        tool.execute(serde_json::json!({
            "path": file.to_str().unwrap(),
            "content": "new content"
        }))
        .await
        .unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "new content");
    }

    #[tokio::test]
    async fn apply_patch_missing_params() {
        let tool = ApplyPatchTool;
        assert!(tool
            .execute(serde_json::json!({ "path": "/tmp/x" }))
            .await
            .is_err());
        assert!(tool
            .execute(serde_json::json!({ "content": "x" }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn apply_patch_no_temp_left_behind() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("clean.txt");

        let tool = ApplyPatchTool;
        tool.execute(serde_json::json!({
            "path": file.to_str().unwrap(),
            "content": "content"
        }))
        .await
        .unwrap();

        // 不应有 .r-code-tmp 文件残留
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["clean.txt".to_string()]);
    }

    // --------------------------------------------------------------------------
    // create_file 工具测试
    // --------------------------------------------------------------------------

    #[tokio::test]
    async fn create_file_success() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("new.txt");

        let tool = CreateFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "hello"
            }))
            .await
            .unwrap();
        assert!(result.contains("created"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello");
    }

    #[tokio::test]
    async fn create_file_fails_if_exists() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("exists.txt");
        fs::write(&file, "original").unwrap();

        let tool = CreateFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "new"
            }))
            .await;
        assert!(result.is_err());
        // 原文件内容不应被修改
        assert_eq!(fs::read_to_string(&file).unwrap(), "original");
    }

    #[tokio::test]
    async fn create_file_missing_params() {
        let tool = CreateFileTool;
        assert!(tool.execute(serde_json::json!({})).await.is_err());
        assert!(tool
            .execute(serde_json::json!({ "path": "/tmp/x" }))
            .await
            .is_err());
    }

    // --------------------------------------------------------------------------
    // delete_file 工具测试
    // --------------------------------------------------------------------------

    #[tokio::test]
    async fn delete_file_success() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("deleteme.txt");
        fs::write(&file, "content").unwrap();

        let tool = DeleteFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap()
            }))
            .await
            .unwrap();
        assert!(result.contains("deleted"));
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn delete_file_fails_if_missing() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("nonexistent.txt");

        let tool = DeleteFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap()
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_file_missing_param() {
        let tool = DeleteFileTool;
        assert!(tool.execute(serde_json::json!({})).await.is_err());
    }
}
