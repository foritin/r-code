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

use std::io::Read;
use std::path::{Component, Path};

use async_trait::async_trait;
use r_code_core::dto::RiskLevel;
use r_code_core::error::ProductError;
use r_code_core::process::hide_background_console;
use r_code_core::security::{PathGuard, WorkspaceFileAccess};

use crate::gateway::{PathBinding, Tool, ToolExecutionContext, ToolExecutionResult};

/// `read_file` 不带 limit 时，单次最多返回的行数。
///
/// 没有这个上限，模型读一个几万行的生成文件就能把整个上下文撑爆，
/// 之后的对话全部失效。宁可截断并告诉它怎么翻页。
const DEFAULT_READ_LINE_LIMIT: usize = 2_000;
/// `read_file` 单次返回的字符上限（约 100 KiB）。
const MAX_READ_CHARS: usize = 100_000;

/// Read text through the host-owned workspace capability when one is available.
///
/// Unscoped calls retain the historic behaviour for standalone tools and their unit tests. Agent
/// runs always provide a guard, so their actual file handle is opened under the fixed workspace
/// directory rather than by a model-supplied ambient path.
fn read_text_file(path: &str, workspace_guard: Option<&PathGuard>) -> Result<String, ProductError> {
    match workspace_guard {
        Some(guard) => {
            let (_, mut file) =
                guard.open_existing_file(Path::new(path), WorkspaceFileAccess::Read)?;
            let mut content = String::new();
            file.read_to_string(&mut content)
                .map_err(|e| ProductError::Other(format!("failed to read {path}: {e}")))?;
            Ok(content)
        }
        None => std::fs::read_to_string(path)
            .map_err(|e| ProductError::Other(format!("failed to read {path}: {e}"))),
    }
}

fn list_directory_names(
    path: &str,
    workspace_guard: Option<&PathGuard>,
) -> Result<Vec<String>, ProductError> {
    let mut names = match workspace_guard {
        Some(guard) => guard
            .list_existing_directory(Path::new(path))?
            .1
            .into_iter()
            .map(|entry| entry.name.to_string_lossy().to_string())
            .collect(),
        None => {
            let entries = std::fs::read_dir(path)
                .map_err(|e| ProductError::Other(format!("cannot list {path}: {e}")))?;
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
        Some(guard) => guard.atomic_write_file(path, content).map(|_| ()),
        None => atomic_write(path, content),
    }
}

fn create_new_file_scoped(
    path: &Path,
    content: &[u8],
    workspace_guard: Option<&PathGuard>,
) -> Result<(), ProductError> {
    match workspace_guard {
        Some(guard) => guard.create_new_file(path, content).map(|_| ()),
        None => {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| {
                    ProductError::Other(format!("failed to create {}: {e}", path.display()))
                })?;
            file.write_all(content).map_err(|e| {
                ProductError::Other(format!("failed to write {}: {e}", path.display()))
            })
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
                    "path does not exist: {path:?}"
                )))
            }
        }
        None => std::fs::remove_file(path)
            .map_err(|e| ProductError::Other(format!("failed to delete {}: {e}", path.display()))),
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
Output is truncated with a note if it would be too large; read the note and page instead."
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
                    "description": "Maximum number of lines to return. Defaults to 2000."
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
    let content = read_text_file(path, workspace_guard)?;

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
    let paging_requested = input.get("offset").is_some() || input.get("limit").is_some();

    // 未分页且文件不大：原样返回，与历史行为完全一致。
    if !paging_requested
        && content.len() <= MAX_READ_CHARS
        && content.lines().count() <= DEFAULT_READ_LINE_LIMIT
    {
        return Ok(content);
    }

    let all_lines: Vec<&str> = content.lines().collect();
    let total = all_lines.len();
    let start = offset.saturating_sub(1);
    if start >= total && total > 0 {
        return Err(ProductError::Other(format!(
            "offset {offset} is past the end of {path} ({total} lines)"
        )));
    }

    let mut body = String::new();
    let mut emitted = 0usize;
    let mut char_capped = false;
    for line in all_lines.iter().skip(start).take(limit) {
        if body.len() + line.len() + 1 > MAX_READ_CHARS {
            char_capped = true;
            break;
        }
        body.push_str(line);
        body.push('\n');
        emitted += 1;
    }

    let last = start + emitted;
    let has_more = last < total;
    if has_more || start > 0 {
        let mut note = format!(
            "\n[{path} 共 {total} 行；本次返回第 {}–{last} 行",
            start + 1
        );
        if has_more {
            let reason = if char_capped {
                format!("已达单次 {MAX_READ_CHARS} 字符上限")
            } else {
                format!("已达单次上限 {limit} 行")
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
/// 2. **自带并发保护**：`old_string` 必须在当前磁盘内容里唯一命中。若文件在模型
///    读取之后被别人改过，命中数会变成 0 或多于 1，替换直接失败而不是把别人的
///    修改静默覆盖掉。这比对比哈希更好用——它同时校验了"改的是我以为的那段"。
pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Replace an exact literal snippet in a file. This is the preferred way to modify files. \
old_string must appear exactly once, so include enough surrounding lines to make it unique \
(indentation must match the file byte for byte). \
Set replace_all=true to replace every occurrence instead — useful for renaming a symbol. \
The uniqueness check also guards against overwriting concurrent edits: if the file changed \
since you read it, the edit fails instead of clobbering."
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

    let occurrences = content.matches(old_string).count();
    if occurrences == 0 {
        return Err(ProductError::Other(format!(
            "'old_string' was not found in {path}. \
Re-read the file: it may have changed, or the indentation / line endings may differ."
        )));
    }
    if occurrences > 1 && !replace_all {
        let lines = match_line_numbers(&content, old_string);
        let shown: Vec<String> = lines.iter().take(10).map(usize::to_string).collect();
        return Err(ProductError::Other(format!(
            "'old_string' matches {occurrences} places in {path} (lines {}). \
Add surrounding context to make it unique, or set replace_all=true.",
            shown.join(", ")
        )));
    }

    let updated = if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    };
    atomic_write_scoped(Path::new(path), updated.as_bytes(), workspace_guard)?;

    let first_line = match_line_numbers(&content, old_string)
        .first()
        .copied()
        .unwrap_or(0);
    if replace_all {
        Ok(format!(
            "edited {path}: replaced {occurrences} occurrence(s)"
        ))
    } else {
        Ok(format!("edited {path} at line {first_line}"))
    }
}

/// 找出 `needle` 每次出现所在的 1-based 行号。
fn match_line_numbers(content: &str, needle: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    let mut cursor = 0usize;
    while let Some(found) = content[cursor..].find(needle) {
        let absolute = cursor + found;
        // 出现位置之前的换行数 + 1 即行号。
        lines.push(content[..absolute].matches('\n').count() + 1);
        cursor = absolute + needle.len().max(1);
        if cursor >= content.len() {
            break;
        }
    }
    lines
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
    Ok(format!("patched {path}"))
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
    Ok(format!("created {path}"))
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
    Ok(format!("deleted {path}"))
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
            tmp_path.display()
        ))
    })?;

    std::fs::rename(&tmp_path, path).map_err(|e| {
        // 清理残留临时文件
        let _ = std::fs::remove_file(&tmp_path);
        ProductError::Other(format!(
            "failed to rename temp file to {}: {e}",
            path.display()
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
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "message was: {err}");
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
    fn match_line_numbers_locates_every_occurrence() {
        let content = "a\nneedle\nb\nneedle\nc\n";
        assert_eq!(match_line_numbers(content, "needle"), vec![2, 4]);
        assert_eq!(match_line_numbers(content, "a"), vec![1]);
        assert!(match_line_numbers(content, "zzz").is_empty());
        // 首行命中
        assert_eq!(match_line_numbers("x\ny\n", "x"), vec![1]);
        // 跨行片段按起始行计
        assert_eq!(match_line_numbers("a\nb\nc\n", "b\nc"), vec![2]);
    }

    #[test]
    fn tool_names_and_descriptions() {
        assert_eq!(ReadFileTool.name(), "read_file");
        assert!(!ReadFileTool.description().is_empty());
        assert_eq!(ListFilesTool.name(), "list_files");
        assert_eq!(GitStatusTool.name(), "git_status");
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
