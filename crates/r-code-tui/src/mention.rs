//! `@` 文件提及补全（M4-04 / R-SHELL-01）。
//!
//! 输入中的 `@token`（最近一段空白后的 @ 起始段）触发工作区文件补全；
//! 扫描当前目录一层（跳过隐藏项与 .git），前缀/包含过滤，上限 20 条。

use std::path::Path;

/// 从输入提取 @ 查询（无 @ 或 @ 后有空白 = 无活动提及）。
pub fn mention_query(text: &str) -> Option<String> {
    let last = text.split_whitespace().next_back()?;
    let token = last.strip_prefix('@')?;
    if token.contains('/') || last.len() == 1 {
        // 恰好 "@"：也算活动提及（空查询列全部）。
        return Some(String::new());
    }
    Some(token.to_string())
}

/// 候选文件（一层目录、排序、上限 limit）。
pub fn complete_files(root: &Path, query: &str, limit: usize) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Some(if is_dir { format!("{name}/") } else { name })
        })
        .filter(|name| query.is_empty() || name.to_lowercase().contains(&query.to_lowercase()))
        .collect();
    names.sort();
    names.truncate(limit);
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("alpha.rs"), "").expect("a");
        std::fs::write(root.join("beta.rs"), "").expect("b");
        std::fs::write(root.join("README.md"), "").expect("r");
        std::fs::create_dir(root.join("src")).expect("d");
        std::fs::create_dir(root.join(".git")).expect("hidden dir");
        std::fs::write(root.join(".hidden"), "").expect("hidden file");
        dir
    }

    /// M4-04.A2：@ 查询提取与补全过滤。
    #[test]
    fn mention_query_extracts_active_token() {
        assert_eq!(mention_query("看下 @al"), Some("al".to_string()));
        assert_eq!(mention_query("@"), Some(String::new()));
        assert_eq!(mention_query("没有提及"), None);
        assert_eq!(mention_query("邮箱 a@b 不触发"), None, "@ 前有字母不是提及");
    }

    /// M4-04.A2：补全列表过滤（隐藏项排除、目录带 /、上限）。
    #[test]
    fn completion_filters_and_skips_hidden() {
        let dir = fixture();
        let all = complete_files(dir.path(), "", 20);
        assert_eq!(
            all,
            vec!["README.md", "alpha.rs", "beta.rs", "src/"],
            "{all:?}"
        );
        let filtered = complete_files(dir.path(), "alp", 20);
        assert_eq!(filtered, vec!["alpha.rs"]);
        let limited = complete_files(dir.path(), "", 2);
        assert_eq!(limited.len(), 2, "上限截断");
        let none = complete_files(dir.path(), "zzz", 20);
        assert!(none.is_empty());
    }
}
