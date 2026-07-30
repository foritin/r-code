//! SkillManager - 外部 CLI Skill 安装 [doc-05 §7] [doc-10 §7]
//!
//! 管理 R-Code 协作 SKILL.md 文件的安装。
//! 安装到 Claude 和 Codex 的 skills 目录，指导外部 CLI 如何安全使用
//! R-Code MCP 的只读委派边界。
//!
//! ## 安装路径 [doc-10 §7.1]
//! - `~/.claude/skills/r-code-terminal/SKILL.md`（历史稳定路径）
//! - `~/.codex/skills/r-code-terminal/SKILL.md`（历史稳定路径）
//!
//! ## 原则
//! - 原子写入（temp + rename）。
//! - 仅用户显式请求时写入。
//! - 新鲜度检查：逐字节比较。

use std::path::{Path, PathBuf};

use r_code_core::error::ProductError;

/// SKILL.md 内容 - 指导外部 CLI 如何安全使用 R-Code MCP。
const SKILL_CONTENT: &str = include_str!("../assets/SKILL.md");

/// Skill 安装状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillStatus {
    /// 未安装
    NotInstalled,
    /// 已安装且为最新
    UpToDate,
    /// 已安装但已过期（用户编辑过）
    UpdateAvailable,
    /// 检查时出错
    Error(String),
}

/// SkillManager - 安装和管理 r-code-terminal SKILL.md 文件。
///
/// 将 SKILL.md 安装到 Claude 和 Codex 的 skills 目录。
/// 原子写入（temp + rename），仅用户显式请求时写入。
pub struct SkillManager {
    claude_skills_dir: PathBuf,
    codex_skills_dir: PathBuf,
}

impl SkillManager {
    /// 创建 SkillManager，路径设为 `~/.claude/skills/r-code-terminal`
    /// 和 `~/.codex/skills/r-code-terminal`。
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::with_home(home)
    }

    /// 使用指定的 home 目录创建 SkillManager（用于测试）。
    pub fn with_home(home: PathBuf) -> Self {
        // 保留既有目录名，确保旧版安装会被“更新协作 Skill”原地替换，而不会让
        // 过时的 ControlDoor 说明和新 MCP 指南同时出现在 Codex 上下文里。
        Self {
            claude_skills_dir: home.join(".claude").join("skills").join("r-code-terminal"),
            codex_skills_dir: home.join(".codex").join("skills").join("r-code-terminal"),
        }
    }

    /// 获取 SKILL.md 内容。
    pub fn skill_content() -> &'static str {
        SKILL_CONTENT
    }

    /// 将 skill 安装到 Claude 和 Codex 的 skills 目录。
    ///
    /// 原子写入（temp + rename）。仅当用户显式请求时调用。
    pub fn install(&self) -> Result<(), ProductError> {
        for dir in [&self.claude_skills_dir, &self.codex_skills_dir] {
            std::fs::create_dir_all(dir)?;
            let target = dir.join("SKILL.md");
            atomic_write(&target, SKILL_CONTENT)?;
        }
        Ok(())
    }

    /// 仅安装到 Codex。设置页中用户明确连接 Codex 时使用，绝不顺带改写 Claude。
    pub fn install_codex(&self) -> Result<(), ProductError> {
        std::fs::create_dir_all(&self.codex_skills_dir)?;
        atomic_write(&self.codex_skills_dir.join("SKILL.md"), SKILL_CONTENT)
    }

    /// 检查已安装 skill 的新鲜度（逐字节比较）。
    ///
    /// 检查两个安装路径：
    /// - 均未安装 -> `NotInstalled`
    /// - 全部已安装且匹配 -> `UpToDate`
    /// - 任一已安装但不匹配 -> `UpdateAvailable`
    pub fn check_freshness(&self) -> SkillStatus {
        let paths = [
            self.claude_skills_dir.join("SKILL.md"),
            self.codex_skills_dir.join("SKILL.md"),
        ];

        let mut installed = 0;
        let mut matching = 0;

        for path in &paths {
            match std::fs::read(path) {
                Ok(content) => {
                    installed += 1;
                    if content == SKILL_CONTENT.as_bytes() {
                        matching += 1;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // 该路径未安装 - 继续
                }
                Err(e) => {
                    return SkillStatus::Error(e.to_string());
                }
            }
        }

        if installed == 0 {
            SkillStatus::NotInstalled
        } else if installed == matching {
            SkillStatus::UpToDate
        } else {
            SkillStatus::UpdateAvailable
        }
    }

    /// 获取预期的安装路径（SKILL.md 完整文件路径）。
    pub fn install_paths(&self) -> Vec<PathBuf> {
        vec![
            self.claude_skills_dir.join("SKILL.md"),
            self.codex_skills_dir.join("SKILL.md"),
        ]
    }
}

impl Default for SkillManager {
    fn default() -> Self {
        Self::new()
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

    #[test]
    fn skill_content_not_empty() {
        let content = SkillManager::skill_content();
        assert!(!content.is_empty());
        assert!(content.contains("R-Code Collaboration"));
        assert!(content.contains("r_code_delegate"));
        assert!(content.contains("read-only task by default"));
        assert!(content.contains("full_access"));
    }

    #[test]
    fn install_creates_both_files() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        mgr.install().unwrap();

        let claude_skill = tmp.path().join(".claude/skills/r-code-terminal/SKILL.md");
        let codex_skill = tmp.path().join(".codex/skills/r-code-terminal/SKILL.md");

        assert!(claude_skill.exists(), "Claude SKILL.md should exist");
        assert!(codex_skill.exists(), "Codex SKILL.md should exist");

        let claude_content = std::fs::read_to_string(&claude_skill).unwrap();
        let codex_content = std::fs::read_to_string(&codex_skill).unwrap();
        assert_eq!(claude_content, SKILL_CONTENT);
        assert_eq!(codex_content, SKILL_CONTENT);
    }

    #[test]
    fn install_codex_only_leaves_claude_untouched() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        mgr.install_codex().unwrap();

        assert!(tmp
            .path()
            .join(".codex/skills/r-code-terminal/SKILL.md")
            .exists());
        assert!(!tmp
            .path()
            .join(".claude/skills/r-code-terminal/SKILL.md")
            .exists());
    }

    #[test]
    fn check_freshness_not_installed() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        assert_eq!(mgr.check_freshness(), SkillStatus::NotInstalled);
    }

    #[test]
    fn check_freshness_up_to_date() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        mgr.install().unwrap();
        assert_eq!(mgr.check_freshness(), SkillStatus::UpToDate);
    }

    #[test]
    fn check_freshness_update_available() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        mgr.install().unwrap();

        // 用户编辑了 Claude 的 SKILL.md
        let claude_skill = tmp.path().join(".claude/skills/r-code-terminal/SKILL.md");
        std::fs::write(&claude_skill, "# Modified by user\n").unwrap();

        assert_eq!(mgr.check_freshness(), SkillStatus::UpdateAvailable);
    }

    #[test]
    fn check_freshness_one_installed_one_not() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        // 只安装到 Claude，不安装到 Codex
        let claude_dir = tmp.path().join(".claude/skills/r-code-terminal");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("SKILL.md"), SKILL_CONTENT).unwrap();

        // 一个已安装且匹配，另一个未安装 -> UpToDate
        assert_eq!(mgr.check_freshness(), SkillStatus::UpToDate);
    }

    #[test]
    fn check_freshness_one_installed_one_outdated() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        // Claude: 已安装且匹配
        let claude_dir = tmp.path().join(".claude/skills/r-code-terminal");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("SKILL.md"), SKILL_CONTENT).unwrap();

        // Codex: 已安装但不匹配
        let codex_dir = tmp.path().join(".codex/skills/r-code-terminal");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("SKILL.md"), "# outdated\n").unwrap();

        assert_eq!(mgr.check_freshness(), SkillStatus::UpdateAvailable);
    }

    #[test]
    fn install_overwrites_existing() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        // 预先写入旧内容
        let claude_dir = tmp.path().join(".claude/skills/r-code-terminal");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("SKILL.md"), "# old content\n").unwrap();

        // 安装覆盖
        mgr.install().unwrap();

        let content = std::fs::read_to_string(claude_dir.join("SKILL.md")).unwrap();
        assert_eq!(content, SKILL_CONTENT);
        assert_eq!(mgr.check_freshness(), SkillStatus::UpToDate);
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        mgr.install().unwrap();
        mgr.install().unwrap();

        assert_eq!(mgr.check_freshness(), SkillStatus::UpToDate);
    }

    #[test]
    fn install_paths_returns_both() {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_home(tmp.path().to_path_buf());

        let paths = mgr.install_paths();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with(".claude/skills/r-code-terminal/SKILL.md"));
        assert!(paths[1].ends_with(".codex/skills/r-code-terminal/SKILL.md"));
    }

    #[test]
    fn default_equals_new() {
        // default 和 new 都使用真实 home，路径结构应一致
        let a = SkillManager::new();
        let b = SkillManager::default();
        assert_eq!(a.install_paths(), b.install_paths());
    }
}
