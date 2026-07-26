//! ProjectMemory - 项目记忆管理 [doc-04 §8]
//!
//! 单源三投影的项目记忆系统。记忆文件存储在 `<project_root>/.r-code/memory.md`，
//! 投影到三个表面：
//!
//! 1. **preamble** - 系统提示注入（运行时）
//! 2. **CLAUDE.md** - 托管区块（供 Claude CLI 读取）
//! 3. **AGENTS.md** - 托管区块（供 Codex CLI 读取）
//!
//! 私有记忆只管理不合并。
//!
//! ## 托管区块 [doc-04 §8.1]
//! CLAUDE.md 和 AGENTS.md 中的托管区块由 `MANAGED_START` / `MANAGED_END`
//! 标记包裹。R-Code 只写入标记之间的内容，标记外的内容永不被触碰。

use std::path::{Path, PathBuf};

use r_code_core::error::ProductError;

/// ProjectMemory - 单源三投影的项目记忆管理器。
///
/// 1. Preamble（系统提示注入）
/// 2. CLAUDE.md（Claude CLI 托管区块）
/// 3. AGENTS.md（Codex CLI 托管区块）
///
/// 私有记忆只管理不合并。
pub struct ProjectMemory {
    project_root: PathBuf,
}

impl ProjectMemory {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }

    /// 获取记忆文件路径：`<project_root>/.r-code/memory.md`
    pub fn memory_path(&self) -> PathBuf {
        self.project_root.join(".r-code").join("memory.md")
    }

    /// 加载项目记忆内容。
    ///
    /// 文件不存在时返回空字符串。
    pub fn load(&self) -> Result<String, ProductError> {
        match std::fs::read_to_string(self.memory_path()) {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// 保存项目记忆内容。
    ///
    /// 自动创建 `.r-code` 目录。
    pub fn save(&self, content: &str) -> Result<(), ProductError> {
        let path = self.memory_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// 生成系统提示注入用的 preamble 文本。
    ///
    /// 记忆为空时返回空字符串；否则返回 `<project_memory>` 包裹的内容。
    pub fn generate_preamble(&self) -> Result<String, ProductError> {
        let memory = self.load()?;
        if memory.trim().is_empty() {
            return Ok(String::new());
        }
        Ok(format!(
            "<project_memory>\n{}\n</project_memory>",
            memory.trim()
        ))
    }

    /// 将项目记忆同步到 CLAUDE.md 的托管区块。
    ///
    /// 写入 `<project_root>/CLAUDE.md`，使用托管标记。
    /// 标记外的内容保持不变。
    pub fn sync_to_claude(&self) -> Result<(), ProductError> {
        let memory = self.load()?;
        let target = self.project_root.join("CLAUDE.md");
        sync_managed_block(&target, &memory)
    }

    /// 将项目记忆同步到 AGENTS.md 的托管区块。
    ///
    /// 写入 `<project_root>/AGENTS.md`，使用托管标记。
    /// 标记外的内容保持不变。
    pub fn sync_to_agents(&self) -> Result<(), ProductError> {
        let memory = self.load()?;
        let target = self.project_root.join("AGENTS.md");
        sync_managed_block(&target, &memory)
    }

    /// 托管区块起始标记。
    pub const MANAGED_START: &'static str = "<!-- r-code:managed-start -->";

    /// 托管区块结束标记。
    pub const MANAGED_END: &'static str = "<!-- r-code:managed-end -->";
}

/// 同步托管区块到目标文件。
///
/// - 若文件存在且包含标记：替换标记间的内容。
/// - 若文件存在但无标记：在末尾追加托管区块。
/// - 若文件不存在：创建新文件并写入托管区块。
///
/// 标记外的内容永不被触碰。原子写入（temp + rename）。
fn sync_managed_block(path: &Path, content: &str) -> Result<(), ProductError> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();

    let new_content = replace_managed_block(&existing, content);

    atomic_write(path, &new_content)?;
    Ok(())
}

/// 在已有内容中替换或插入托管区块。
fn replace_managed_block(existing: &str, content: &str) -> String {
    let start_marker = ProjectMemory::MANAGED_START;
    let end_marker = ProjectMemory::MANAGED_END;

    let block = format!("{start_marker}\n{content}\n{end_marker}");

    if let Some(start_idx) = existing.find(start_marker) {
        // 找到起始标记，查找结束标记
        let search_from = start_idx + start_marker.len();
        if let Some(end_idx) = existing[search_from..].find(end_marker) {
            let abs_end = search_from + end_idx + end_marker.len();
            // 替换标记间的内容（含标记）
            let before = &existing[..start_idx];
            let after = &existing[abs_end..];
            return format!("{before}{block}{after}");
        } else {
            // 只有起始标记没有结束标记 - 从起始标记处替换到末尾
            let before = &existing[..start_idx];
            return format!("{before}{block}");
        }
    }

    // 无标记 - 追加
    if existing.trim().is_empty() {
        block
    } else {
        format!("{}\n\n{block}", existing.trim_end())
    }
}

/// 原子写入文件（写入临时文件，然后 rename）。
fn atomic_write(path: &Path, content: &str) -> Result<(), ProductError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn project_memory(tmp: &TempDir) -> ProjectMemory {
        ProjectMemory::new(tmp.path().to_path_buf())
    }

    // ── memory_path ─────────────────────────────────────────────────

    #[test]
    fn memory_path_under_r_code_dir() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        assert_eq!(
            pm.memory_path(),
            tmp.path().join(".r-code").join("memory.md")
        );
    }

    // ── load / save ─────────────────────────────────────────────────

    #[test]
    fn load_returns_empty_when_not_exists() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        assert_eq!(pm.load().unwrap(), "");
    }

    #[test]
    fn save_creates_dir_and_file() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);

        pm.save("# Project Rules\n\n- Use 2-space indent\n")
            .unwrap();

        let content = pm.load().unwrap();
        assert_eq!(content, "# Project Rules\n\n- Use 2-space indent\n");
        assert!(tmp.path().join(".r-code/memory.md").exists());
    }

    #[test]
    fn save_overwrites_existing() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);

        pm.save("old content").unwrap();
        pm.save("new content").unwrap();

        assert_eq!(pm.load().unwrap(), "new content");
    }

    #[test]
    fn save_empty_string() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);

        pm.save("").unwrap();
        assert_eq!(pm.load().unwrap(), "");
    }

    // ── generate_preamble ──────────────────────────────────────────

    #[test]
    fn preamble_empty_when_no_memory() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        assert_eq!(pm.generate_preamble().unwrap(), "");
    }

    #[test]
    fn preamble_empty_when_whitespace_only() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        pm.save("   \n\n  \t  \n").unwrap();
        assert_eq!(pm.generate_preamble().unwrap(), "");
    }

    #[test]
    fn preamble_wraps_content_in_tags() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        pm.save("Always use TypeScript\n").unwrap();

        let preamble = pm.generate_preamble().unwrap();
        assert!(preamble.contains("<project_memory>"));
        assert!(preamble.contains("</project_memory>"));
        assert!(preamble.contains("Always use TypeScript"));
    }

    #[test]
    fn preamble_trims_content() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        pm.save("\n\n  some rules  \n\n").unwrap();

        let preamble = pm.generate_preamble().unwrap();
        // trim 后不应有前导/尾随空白
        assert!(preamble.starts_with("<project_memory>\n"));
        assert!(preamble.ends_with("\n</project_memory>"));
        assert!(preamble.contains("some rules"));
    }

    // ── sync_to_claude ──────────────────────────────────────────────

    #[test]
    fn sync_to_claude_creates_file() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        pm.save("rule 1\n").unwrap();

        pm.sync_to_claude().unwrap();

        let content = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains(ProjectMemory::MANAGED_START));
        assert!(content.contains(ProjectMemory::MANAGED_END));
        assert!(content.contains("rule 1"));
    }

    #[test]
    fn sync_to_claude_appends_to_existing() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);

        // 预存用户内容
        std::fs::write(tmp.path().join("CLAUDE.md"), "# My Notes\n\nuser content\n").unwrap();

        pm.save("injected rule\n").unwrap();
        pm.sync_to_claude().unwrap();

        let content = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        // 用户内容保留
        assert!(content.contains("# My Notes"));
        assert!(content.contains("user content"));
        // 托管区块追加
        assert!(content.contains(ProjectMemory::MANAGED_START));
        assert!(content.contains(ProjectMemory::MANAGED_END));
        assert!(content.contains("injected rule"));
    }

    #[test]
    fn sync_to_claude_replaces_managed_block() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);

        // 预存含托管区块的文件
        let existing = format!(
            "# Header\n\nuser content\n\n{}\nold rule\n{}\n\n# Footer\n",
            ProjectMemory::MANAGED_START,
            ProjectMemory::MANAGED_END
        );
        std::fs::write(tmp.path().join("CLAUDE.md"), &existing).unwrap();

        pm.save("new rule\n").unwrap();
        pm.sync_to_claude().unwrap();

        let content = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        // 标记外内容保留
        assert!(content.contains("# Header"));
        assert!(content.contains("user content"));
        assert!(content.contains("# Footer"));
        // 旧内容被替换
        assert!(!content.contains("old rule"));
        // 新内容存在
        assert!(content.contains("new rule"));
        // 标记仍存在
        assert!(content.contains(ProjectMemory::MANAGED_START));
        assert!(content.contains(ProjectMemory::MANAGED_END));
    }

    #[test]
    fn sync_to_claude_preserves_outside_content() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);

        let existing = format!(
            "line before\n\n{}old{}\nline after\n",
            ProjectMemory::MANAGED_START,
            ProjectMemory::MANAGED_END
        );
        std::fs::write(tmp.path().join("CLAUDE.md"), &existing).unwrap();

        pm.save("updated\n").unwrap();
        pm.sync_to_claude().unwrap();

        let content = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert!(content.starts_with("line before"));
        assert!(content.contains("line after"));
        assert!(content.contains("updated"));
    }

    #[test]
    fn sync_to_claude_idempotent() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        pm.save("same rule\n").unwrap();

        pm.sync_to_claude().unwrap();
        let first = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();

        pm.sync_to_claude().unwrap();
        let second = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn sync_to_claude_empty_memory() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        // 不 save，memory 为空

        pm.sync_to_claude().unwrap();

        let content = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains(ProjectMemory::MANAGED_START));
        assert!(content.contains(ProjectMemory::MANAGED_END));
        // 标记间应为空行
        let block = content
            .split(ProjectMemory::MANAGED_START)
            .nth(1)
            .unwrap()
            .split(ProjectMemory::MANAGED_END)
            .next()
            .unwrap();
        assert_eq!(block.trim(), "");
    }

    // ── sync_to_agents ─────────────────────────────────────────────

    #[test]
    fn sync_to_agents_creates_file() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        pm.save("agent rule\n").unwrap();

        pm.sync_to_agents().unwrap();

        let content = std::fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();
        assert!(content.contains(ProjectMemory::MANAGED_START));
        assert!(content.contains(ProjectMemory::MANAGED_END));
        assert!(content.contains("agent rule"));
    }

    #[test]
    fn sync_to_agents_replaces_managed_block() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);

        let existing = format!(
            "# Agents\n\n{}\nold\n{}\n",
            ProjectMemory::MANAGED_START,
            ProjectMemory::MANAGED_END
        );
        std::fs::write(tmp.path().join("AGENTS.md"), &existing).unwrap();

        pm.save("fresh\n").unwrap();
        pm.sync_to_agents().unwrap();

        let content = std::fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();
        assert!(content.contains("# Agents"));
        assert!(!content.contains("\nold\n"));
        assert!(content.contains("fresh"));
    }

    #[test]
    fn sync_both_claude_and_agents() {
        let tmp = TempDir::new().unwrap();
        let pm = project_memory(&tmp);
        pm.save("shared rule\n").unwrap();

        pm.sync_to_claude().unwrap();
        pm.sync_to_agents().unwrap();

        let claude = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        let agents = std::fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();

        assert!(claude.contains("shared rule"));
        assert!(agents.contains("shared rule"));
    }

    // ── replace_managed_block 纯函数 ────────────────────────────────

    #[test]
    fn replace_block_inserts_when_no_markers() {
        let result = replace_managed_block("hello\n", "world");
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
        assert!(result.contains(ProjectMemory::MANAGED_START));
        assert!(result.contains(ProjectMemory::MANAGED_END));
    }

    #[test]
    fn replace_block_creates_when_empty() {
        let result = replace_managed_block("", "content");
        assert_eq!(
            result,
            format!(
                "{}\ncontent\n{}",
                ProjectMemory::MANAGED_START,
                ProjectMemory::MANAGED_END
            )
        );
    }

    #[test]
    fn replace_block_preserves_content_between_multiple_syncs() {
        let existing = "# Title\n\nsome text\n";
        let step1 = replace_managed_block(existing, "v1");
        let step2 = replace_managed_block(&step1, "v2");

        assert!(step2.contains("# Title"));
        assert!(step2.contains("some text"));
        assert!(!step2.contains("v1"));
        assert!(step2.contains("v2"));
    }
}
