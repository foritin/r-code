//! SearchService -- Quick Open 与全局搜索。 [doc-12 §2]
//!
//! 提供文件名模糊匹配（Quick Open）和文件内容搜索（Global Search）。
//! 支持取消令牌和替换预览。
//!
//! ## 功能
//! - **Quick Open**：模糊匹配文件路径，按相关度排序
//! - **Global Search**：递归搜索文件内容，支持取消
//! - **Replace Preview**：生成替换前后的文本对比（不实际写入）
//!
//! 跳过 `.git`、`node_modules`、`target` 等目录与隐藏目录。
//!
//! [doc-12 §2] [doc-09]

use std::path::{Path, PathBuf};

use r_code_core::error::ProductError;
use tokio_util::sync::CancellationToken;

/// SearchService -- Quick Open 与全局搜索。
pub struct SearchService {
    project_root: PathBuf,
}

/// 搜索命中结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// 文件路径（相对于 project_root）
    pub path: String,
    /// 行号（1-based）
    pub line: usize,
    /// 列号（1-based，字节偏移）
    pub column: usize,
    /// 整行文本
    pub line_text: String,
    /// 匹配起始字节偏移（0-based）
    pub match_start: usize,
    /// 匹配结束字节偏移（exclusive）
    pub match_end: usize,
}

/// 替换预览条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacePreview {
    /// 文件路径（相对于 project_root）
    pub path: String,
    /// 行号（1-based）
    pub line: usize,
    /// 替换前文本
    pub old_text: String,
    /// 替换后文本
    pub new_text: String,
}

impl SearchService {
    /// 创建 SearchService。
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }

    /// Quick Open：模糊匹配文件路径。
    ///
    /// 返回最多 `limit` 条匹配，按模糊评分降序排序。
    /// 跳过 `.git`、`node_modules`、`target` 等目录。
    pub async fn quick_open(&self, query: &str, limit: usize) -> Result<Vec<String>, ProductError> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        collect_files(&self.project_root, &self.project_root, &mut files);

        // 模糊匹配并评分
        let mut scored: Vec<(i32, String)> = files
            .into_iter()
            .filter_map(|path| fuzzy_score(query, &path).map(|score| (score, path)))
            .collect();

        // 按分数降序、路径升序排序
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        Ok(scored.into_iter().take(limit).map(|(_, p)| p).collect())
    }

    /// 全局搜索：搜索文件内容。
    ///
    /// 返回最多 `limit` 条匹配。可通过 `cancel` 令牌取消（返回已收集的部分结果）。
    pub async fn global_search(
        &self,
        query: &str,
        limit: usize,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchMatch>, ProductError> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let mut matches = Vec::new();
        let mut files = Vec::new();
        collect_files(&self.project_root, &self.project_root, &mut files);

        for rel_path in files {
            // 取消检查
            if cancel.is_cancelled() || matches.len() >= limit {
                break;
            }

            let full_path = self.project_root.join(&rel_path);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => continue, // 跳过二进制文件或无法读取的文件
            };

            for (line_idx, line) in content.lines().enumerate() {
                if matches.len() >= limit {
                    break;
                }
                if let Some(pos) = line.find(query) {
                    matches.push(SearchMatch {
                        path: rel_path.clone(),
                        line: line_idx + 1,
                        column: pos + 1,
                        line_text: line.to_string(),
                        match_start: pos,
                        match_end: pos + query.len(),
                    });
                }
            }
        }

        Ok(matches)
    }

    /// 替换预览：生成替换前后的文本对比（不实际写入磁盘）。
    ///
    /// 对 `paths` 中每个文件的每一行，将所有 `query` 出现替换为 `replacement`，
    /// 生成 `ReplacePreview` 条目。
    pub async fn replace_preview(
        &self,
        query: &str,
        replacement: &str,
        paths: &[String],
    ) -> Result<Vec<ReplacePreview>, ProductError> {
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let mut previews = Vec::new();

        for path_str in paths {
            let full_path = self.project_root.join(path_str);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            for (line_idx, line) in content.lines().enumerate() {
                if line.contains(query) {
                    let new_text = line.replace(query, replacement);
                    previews.push(ReplacePreview {
                        path: path_str.clone(),
                        line: line_idx + 1,
                        old_text: line.to_string(),
                        new_text,
                    });
                }
            }
        }

        Ok(previews)
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 需要跳过的目录名。
const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".next",
    ".venv",
    "__pycache__",
    "dist",
    "build",
];

/// 递归收集目录下的所有文件路径（相对于 `root`）。
///
/// 跳过 `IGNORED_DIRS` 中的目录以及任何以 `.` 开头的隐藏目录。
fn collect_files(root: &Path, dir: &Path, results: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();

        if path.is_dir() {
            if let Some(file_name) = path.file_name() {
                let dir_name = file_name.to_string_lossy();
                let is_ignored =
                    dir_name.starts_with('.') || IGNORED_DIRS.iter().any(|d| dir_name == *d);
                if is_ignored {
                    continue;
                }
            }
            collect_files(root, &path, results);
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                results.push(rel.to_string_lossy().to_string());
            }
        }
    }
}

/// 模糊匹配评分：返回 `Some(score)` 表示匹配成功，`None` 表示不匹配。
///
/// 评分规则：
/// - 查询字符必须按顺序出现在路径中（子序列匹配）
/// - 每个匹配字符 +1 分
/// - 连续匹配 +1 分（额外）
/// - 路径分隔符后或开头的匹配 +2 分（额外，词边界奖励）
fn fuzzy_score(query: &str, path: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }

    let query_chars: Vec<char> = query.chars().collect();
    let path_chars: Vec<char> = path.chars().collect();

    if query_chars.len() > path_chars.len() {
        return None;
    }

    let mut qi = 0;
    let mut score = 0;
    let mut last_match_idx: Option<usize> = None;

    for (i, &c) in path_chars.iter().enumerate() {
        if qi >= query_chars.len() {
            break;
        }

        if c.eq_ignore_ascii_case(&query_chars[qi]) {
            // 连续匹配奖励
            if let Some(last) = last_match_idx {
                if i == last + 1 {
                    score += 1;
                }
            }

            // 词边界奖励（开头、路径分隔符后）
            if i == 0 || path_chars[i - 1] == '/' || path_chars[i - 1] == '\\' {
                score += 2;
            }

            score += 1;
            last_match_idx = Some(i);
            qi += 1;
        }
    }

    if qi == query_chars.len() {
        Some(score)
    } else {
        None
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    // ── quick_open ─────────────────────────────────────────────────

    #[tokio::test]
    async fn quick_open_match() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "").unwrap();
        std::fs::write(dir.path().join("lib.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let results = svc.quick_open("main", 10).await.unwrap();

        assert!(results.contains(&"main.rs".to_string()));
        assert!(!results.contains(&"lib.rs".to_string()));
    }

    #[tokio::test]
    async fn quick_open_subsequence_match() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        // "srcmain" 是 "src/main.rs" 的子序列：s(0) r(1) c(2) m(4) a(5) i(6) n(7)
        let results = svc.quick_open("srcmain", 10).await.unwrap();
        assert!(results.contains(&"src/main.rs".to_string()));
    }

    #[tokio::test]
    async fn quick_open_no_match() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let results = svc.quick_open("xyzabc", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn quick_open_limit() {
        let dir = TempDir::new().unwrap();
        // 创建 5 个匹配的文件
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("main{i}.rs")), "").unwrap();
        }

        let svc = SearchService::new(dir.path().to_path_buf());
        let results = svc.quick_open("main", 3).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn quick_open_empty_query() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let results = svc.quick_open("", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn quick_open_skips_ignored_dirs() {
        let dir = TempDir::new().unwrap();
        // 根目录文件 - 应被找到
        std::fs::write(dir.path().join("main.rs"), "").unwrap();
        // .git 目录 - 应跳过
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git").join("main.rs"), "").unwrap();
        // node_modules 目录 - 应跳过
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join("node_modules").join("main.js"), "").unwrap();
        // target 目录 - 应跳过
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target").join("main.txt"), "").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let results = svc.quick_open("main", 50).await.unwrap();

        assert!(results.contains(&"main.rs".to_string()));
        // 不应包含被跳过目录中的文件
        assert!(!results.iter().any(|p| p.contains(".git")));
        assert!(!results.iter().any(|p| p.contains("node_modules")));
        assert!(!results.iter().any(|p| p.contains("target")));
    }

    #[tokio::test]
    async fn quick_open_case_insensitive() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Main.rs"), "").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let results = svc.quick_open("main", 10).await.unwrap();
        assert!(results.contains(&"Main.rs".to_string()));
    }

    // ── global_search ─────────────────────────────────────────────

    #[tokio::test]
    async fn global_search_match() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\nfoo bar\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello rust\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let cancel = CancellationToken::new();
        let results = svc.global_search("hello", 100, cancel).await.unwrap();

        assert_eq!(results.len(), 2);
        // a.txt 第 1 行
        assert!(results.iter().any(|m| m.path == "a.txt" && m.line == 1));
        // b.txt 第 1 行
        assert!(results.iter().any(|m| m.path == "b.txt" && m.line == 1));
    }

    #[tokio::test]
    async fn global_search_match_offsets() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let cancel = CancellationToken::new();
        let results = svc.global_search("world", 100, cancel).await.unwrap();

        assert_eq!(results.len(), 1);
        let m = &results[0];
        assert_eq!(m.line, 1);
        assert_eq!(m.column, 7); // "hello " is 6 bytes, "world" starts at offset 6 (1-based: 7)
        assert_eq!(m.match_start, 6);
        assert_eq!(m.match_end, 11);
        assert_eq!(m.line_text, "hello world");
    }

    #[tokio::test]
    async fn global_search_no_match() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let cancel = CancellationToken::new();
        let results = svc.global_search("nonexistent", 100, cancel).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn global_search_cancelled() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let cancel = CancellationToken::new();
        cancel.cancel(); // 预先取消

        let results = svc.global_search("hello", 100, cancel).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn global_search_limit() {
        let dir = TempDir::new().unwrap();
        // 创建多个包含 "hello" 的文件
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("file{i}.txt")), "hello\n").unwrap();
        }

        let svc = SearchService::new(dir.path().to_path_buf());
        let cancel = CancellationToken::new();
        let results = svc.global_search("hello", 3, cancel).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn global_search_empty_query() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let cancel = CancellationToken::new();
        let results = svc.global_search("", 100, cancel).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn global_search_multiple_matches_per_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\nhello\nhello\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let cancel = CancellationToken::new();
        let results = svc.global_search("hello", 100, cancel).await.unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].line, 1);
        assert_eq!(results[1].line, 2);
        assert_eq!(results[2].line, 3);
    }

    // ── replace_preview ───────────────────────────────────────────

    #[tokio::test]
    async fn replace_preview_generates_preview() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\nhello rust\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let previews = svc
            .replace_preview("hello", "hi", &["a.txt".to_string()])
            .await
            .unwrap();

        assert_eq!(previews.len(), 2);
        assert_eq!(previews[0].line, 1);
        assert_eq!(previews[0].old_text, "hello world");
        assert_eq!(previews[0].new_text, "hi world");
        assert_eq!(previews[1].line, 2);
        assert_eq!(previews[1].old_text, "hello rust");
        assert_eq!(previews[1].new_text, "hi rust");
    }

    #[tokio::test]
    async fn replace_preview_no_match() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "foo bar\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let previews = svc
            .replace_preview("hello", "hi", &["a.txt".to_string()])
            .await
            .unwrap();
        assert!(previews.is_empty());
    }

    #[tokio::test]
    async fn replace_preview_multiple_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let previews = svc
            .replace_preview("hello", "hi", &["a.txt".to_string(), "b.txt".to_string()])
            .await
            .unwrap();
        assert_eq!(previews.len(), 2);
        assert_eq!(previews[0].path, "a.txt");
        assert_eq!(previews[1].path, "b.txt");
    }

    #[tokio::test]
    async fn replace_preview_missing_file() {
        let dir = TempDir::new().unwrap();
        let svc = SearchService::new(dir.path().to_path_buf());
        let previews = svc
            .replace_preview("hello", "hi", &["nonexistent.txt".to_string()])
            .await
            .unwrap();
        assert!(previews.is_empty());
    }

    #[tokio::test]
    async fn replace_preview_multiple_occurrences_per_line() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello hello hello\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let previews = svc
            .replace_preview("hello", "hi", &["a.txt".to_string()])
            .await
            .unwrap();
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].old_text, "hello hello hello");
        assert_eq!(previews[0].new_text, "hi hi hi");
    }

    #[tokio::test]
    async fn replace_preview_empty_query() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();

        let svc = SearchService::new(dir.path().to_path_buf());
        let previews = svc
            .replace_preview("", "hi", &["a.txt".to_string()])
            .await
            .unwrap();
        assert!(previews.is_empty());
    }

    // ── fuzzy_score 单元测试 ──────────────────────────────────────

    #[test]
    fn fuzzy_score_exact_match() {
        let score = fuzzy_score("main", "main.rs").unwrap();
        assert!(score > 0);
    }

    #[test]
    fn fuzzy_score_subsequence() {
        assert!(fuzzy_score("mrs", "main.rs").is_some());
    }

    #[test]
    fn fuzzy_score_no_match() {
        assert!(fuzzy_score("xyz", "main.rs").is_none());
    }

    #[test]
    fn fuzzy_score_empty_query() {
        assert_eq!(fuzzy_score("", "main.rs"), Some(0));
    }

    #[test]
    fn fuzzy_score_query_longer_than_path() {
        assert!(fuzzy_score("verylongquery", "a").is_none());
    }
}
