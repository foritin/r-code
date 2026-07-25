//! 内置工具 -- R0/R1 只读工具集 + R2 写入工具。 [doc-02 §5] [doc-18 M8-02]
//!
//! 实现以下工具：
//! - `read_file`（R1）：读取文件内容
//! - `list_files`（R0）：列出目录内容
//! - `search`（R0）：递归搜索文本
//! - `git_status`（R0）：获取 git 状态
//! - `load_skill`（R0）：加载 SKILL.md，路径穿越拒绝 [doc-02 §5]
//! - `apply_patch`（R2）：原子化应用补丁（全文件替换）
//! - `create_file`（R2）：创建新文件
//! - `delete_file`（R2）：删除文件

use std::path::{Component, Path};

use async_trait::async_trait;
use r_code_core::dto::RiskLevel;
use r_code_core::error::ProductError;
use serde::Serialize;

use crate::gateway::Tool;

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
        "Read the full contents of a text file."
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
                }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
        std::fs::read_to_string(path)
            .map_err(|e| ProductError::Other(format!("failed to read {path}: {e}")))
    }
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
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
        let entries = std::fs::read_dir(path)
            .map_err(|e| ProductError::Other(format!("failed to list {path}: {e}")))?;
        let mut names: Vec<String> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| ProductError::Other(format!("dir entry error: {e}")))?;
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();
        serde_json::to_string(&names).map_err(|e| ProductError::Other(format!("JSON error: {e}")))
    }
}

// ============================================================================
// search  [doc-02 §5] -- R0
// ============================================================================

/// 搜索命中结果。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct SearchHit {
    file: String,
    line: usize,
    text: String,
}

/// `search` 工具 -- 递归搜索文件中的文本。
///
/// R0：只读。最多返回 100 条命中。
pub struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }
    fn description(&self) -> &str {
        "Search for a text pattern in files under a directory."
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
                    "description": "Directory to search in."
                },
                "pattern": {
                    "type": "string",
                    "description": "Text pattern to search for."
                }
            },
            "required": ["path", "pattern"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
        let pattern = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'pattern' parameter".to_string()))?;

        let mut hits: Vec<SearchHit> = Vec::new();
        search_recursive(Path::new(path), pattern, &mut hits, 100)?;
        serde_json::to_string(&hits).map_err(|e| ProductError::Other(format!("JSON error: {e}")))
    }
}

/// 递归搜索目录中的文本匹配。
fn search_recursive(
    dir: &Path,
    pattern: &str,
    results: &mut Vec<SearchHit>,
    max_results: usize,
) -> Result<(), ProductError> {
    if results.len() >= max_results {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)
        .map_err(|e| ProductError::Other(format!("failed to read dir {}: {e}", dir.display())))?
    {
        if results.len() >= max_results {
            return Ok(());
        }
        let entry = entry.map_err(|e| ProductError::Other(format!("dir entry error: {e}")))?;
        let path = entry.path();
        if path.is_dir() {
            // 跳过隐藏目录
            if let Some(name) = path.file_name() {
                if name.to_string_lossy().starts_with('.') {
                    continue;
                }
            }
            search_recursive(&path, pattern, results, max_results)?;
        } else if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for (idx, line) in content.lines().enumerate() {
                    if results.len() >= max_results {
                        return Ok(());
                    }
                    if line.contains(pattern) {
                        results.push(SearchHit {
                            file: path.to_string_lossy().to_string(),
                            line: idx + 1,
                            text: line.to_string(),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

// ============================================================================
// git_status  [doc-02 §5] -- R0
// ============================================================================

/// `git_status` 工具 -- 获取仓库的 git 状态。
///
/// R0：只读。使用 `git status --porcelain=v1`。
pub struct GitStatusTool;

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
        let output = std::process::Command::new("git")
            .args(["-C", path, "status", "--porcelain=v1"])
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

        std::fs::read_to_string(p)
            .map_err(|e| ProductError::Other(format!("failed to read skill {path}: {e}")))
    }
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
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'content' parameter".to_string()))?;

        let file_path = Path::new(path);
        atomic_write(file_path, content.as_bytes())?;
        Ok(format!("patched {path}"))
    }
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
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'content' parameter".to_string()))?;

        let file_path = Path::new(path);
        // create_new=true 保证原子性：文件已存在则失败
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file_path)
            .map_err(|e| ProductError::Other(format!("failed to create {path}: {e}")))?;
        file.write_all(content.as_bytes())
            .map_err(|e| ProductError::Other(format!("failed to write {path}: {e}")))?;
        Ok(format!("created {path}"))
    }
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
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;

        std::fs::remove_file(path)
            .map_err(|e| ProductError::Other(format!("failed to delete {path}: {e}")))?;
        Ok(format!("deleted {path}"))
    }
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
    async fn search_finds_matches() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "foo bar\nbaz foo").unwrap();
        fs::write(dir.path().join("b.txt"), "no match here").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("c.txt"), "foo again").unwrap();

        let tool = SearchTool;
        let result = tool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(),
                "pattern": "foo"
            }))
            .await
            .unwrap();
        let hits: Vec<SearchHit> = serde_json::from_str(&result).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(tool.risk_level(), RiskLevel::R0);
    }

    #[tokio::test]
    async fn search_no_matches() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "hello world").unwrap();

        let tool = SearchTool;
        let result = tool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(),
                "pattern": "nonexistent"
            }))
            .await
            .unwrap();
        let hits: Vec<SearchHit> = serde_json::from_str(&result).unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn search_missing_params() {
        let tool = SearchTool;
        assert!(tool
            .execute(serde_json::json!({ "path": "." }))
            .await
            .is_err());
        assert!(tool
            .execute(serde_json::json!({ "pattern": "x" }))
            .await
            .is_err());
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

    #[test]
    fn tool_names_and_descriptions() {
        assert_eq!(ReadFileTool.name(), "read_file");
        assert!(!ReadFileTool.description().is_empty());
        assert_eq!(ListFilesTool.name(), "list_files");
        assert_eq!(SearchTool.name(), "search");
        assert_eq!(GitStatusTool.name(), "git_status");
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
            SearchTool.input_schema(),
            GitStatusTool.input_schema(),
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
