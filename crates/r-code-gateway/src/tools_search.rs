//! 搜索工具 -- `search`（内容正则搜索）与 `glob`（文件名模式匹配）。
//!
//! ## 为什么不 shell 出去调 `rg`
//!
//! ripgrep 本身就是 Rust 写的，它的引擎以 crate 形式发布。直接内嵌意味着：
//!
//! - **无外部依赖**：用户机器上没装 `rg` 也能用，不需要 `which rg` 探测与兜底路径。
//! - **跨平台一致**：Windows / PowerShell 下不存在 `grep`、`find` 语义差异，
//!   同一份代码在三个平台行为完全相同。
//! - **无进程开销**：不 spawn、不做 stdout 解析、不受 shell 引号转义影响。
//! - **可精确控制**：命中上限、二进制跳过、超大文件跳过都是库参数，而非解析产物。
//!
//! 用到的 crate 即 ripgrep 自身的组件：
//! - [`ignore`]：`.gitignore` / `.ignore` 感知的目录遍历
//! - [`grep_regex`]：正则匹配器（含 smart-case）
//! - [`grep_searcher`]：行式搜索、上下文行、二进制探测
//! - [`globset`]：glob 模式匹配
//!
//! [doc-12 §Git 与搜索服务] [ADR-0003]

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use r_code_core::dto::RiskLevel;
use r_code_core::error::ProductError;
use r_code_core::security::path_for_display;
use serde::Serialize;

use crate::gateway::{PathBinding, Tool};

/// 单次调用默认返回的命中上限。
const DEFAULT_MAX_RESULTS: usize = 100;
/// 命中上限的硬顶，防止模型传入巨值把上下文撑爆。
const MAX_MAX_RESULTS: usize = 1_000;
/// 单行输出截断长度（压缩后的 minified 文件常有数万字符的单行）。
const MAX_LINE_CHARS: usize = 400;
/// 跳过超过此体积的文件（默认 4 MiB）。
const MAX_FILESIZE: u64 = 4 * 1024 * 1024;
/// 上下文行数上限。
const MAX_CONTEXT: usize = 10;

// ============================================================================
// 公共辅助
// ============================================================================

fn get_str<'a>(input: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(|v| v.as_str())
}

fn get_bool(input: &serde_json::Value, key: &str, default: bool) -> bool {
    input.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

fn get_usize(input: &serde_json::Value, key: &str, default: usize, cap: usize) -> usize {
    input
        .get(key)
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).min(cap))
        .unwrap_or(default)
}

/// 收集 `glob` 参数：既接受字符串也接受字符串数组。
fn get_globs(input: &serde_json::Value) -> Vec<String> {
    match input.get("glob") {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => vec![s.clone()],
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// 把 `text` 截断到 `MAX_LINE_CHARS` 个字符（按字符边界，不切碎 UTF-8）。
fn clip_line(text: &str) -> String {
    let trimmed = text.trim_end_matches(['\n', '\r']);
    if trimmed.chars().count() <= MAX_LINE_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(MAX_LINE_CHARS).collect();
    format!("{head}… [行已截断]")
}

/// 构造一个尊重忽略规则的目录遍历器。
///
/// `no_ignore = true` 时关闭全部忽略源（等价 `rg --no-ignore`）；
/// `include_hidden = true` 时不跳过隐藏文件（等价 `rg --hidden`）。
/// 无论如何都不跟随符号链接——避免遍历逃出工作区。
fn build_walker(
    root: &Path,
    globs: &[String],
    include_hidden: bool,
    no_ignore: bool,
    max_depth: Option<usize>,
) -> Result<ignore::Walk, ProductError> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!include_hidden)
        .ignore(!no_ignore)
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore)
        .parents(!no_ignore)
        // 工作区可能不是 git 仓库。ripgrep 默认只在仓库内应用 .gitignore；
        // 对 Agent 而言用户放了 .gitignore 就是明确表态"别看这里"，无论有没有 .git。
        .require_git(false)
        // 不跟随符号链接：软链是绕过 PathGuard 边界最省事的方式。
        .follow_links(false)
        .max_filesize(Some(MAX_FILESIZE))
        .max_depth(max_depth)
        // 确定性输出：同样的输入必须给模型同样的结果顺序。
        .sort_by_file_path(|a, b| a.cmp(b));

    if !globs.is_empty() {
        let mut overrides = OverrideBuilder::new(root);
        for glob in globs {
            overrides
                .add(glob)
                .map_err(|e| ProductError::Other(format!("invalid glob '{glob}': {e}")))?;
        }
        let overrides = overrides
            .build()
            .map_err(|e| ProductError::Other(format!("failed to build glob filter: {e}")))?;
        builder.overrides(overrides);
    }

    Ok(builder.build())
}

/// 手写正则元字符转义，等价 `regex::escape`，省掉一个依赖。
fn regex_escape(text: &str) -> String {
    const META: &str = r"\.+*?()|[]{}^$#&-~";
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        if META.contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

// ============================================================================
// search -- R0
// ============================================================================

/// 搜索命中结果。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct SearchHit {
    /// 绝对文件路径（可直接回传给 `read_file` / `edit`）。
    pub file: String,
    /// 1-based 行号。
    pub line: usize,
    /// 行文本（已去尾换行，超长会截断）。
    pub text: String,
    /// `"match"` 为命中行，`"context"` 为上下文行。
    ///
    /// 命中行占绝大多数，序列化时省略以省 token；反序列化时补回 `"match"`，
    /// 保证 round-trip 一致。
    #[serde(default = "default_hit_kind", skip_serializing_if = "is_match_kind")]
    pub kind: String,
}

fn default_hit_kind() -> String {
    "match".to_string()
}

/// serde 的 `skip_serializing_if` 必须收 `&FieldType`，所以这里只能是 `&String`。
#[allow(clippy::ptr_arg)]
fn is_match_kind(kind: &String) -> bool {
    kind == "match"
}

/// `search` 的结构化输出。
#[derive(Debug, Serialize)]
struct SearchOutput {
    /// `content` / `files` / `count`
    mode: String,
    /// 命中总数是否因上限被截断。
    truncated: bool,
    /// 实际打开搜索过的文件数。
    files_searched: usize,
    /// 含命中的文件数。
    files_matched: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    hits: Option<Vec<SearchHit>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_matches: Option<usize>,
    /// 结果被截断时给模型的下一步提示。
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

/// 把 `grep_searcher` 的回调收敛成命中列表。
///
/// `collect = false` 时只累加计数、不保存行文本，供 `files` / `count` 模式使用——
/// 这样大仓库下不会为了数一个总数而把所有命中行读进内存。
struct HitCollector<'a> {
    file: &'a str,
    hits: &'a mut Vec<SearchHit>,
    matches_in_file: usize,
    collect: bool,
    limit: usize,
    /// 本次搜索是否因触达上限而提前停止。
    limit_reached: bool,
}

impl HitCollector<'_> {
    fn push(&mut self, line_number: Option<u64>, bytes: &[u8], kind: &str) -> bool {
        if !self.collect {
            return true;
        }
        if self.hits.len() >= self.limit {
            self.limit_reached = true;
            return false;
        }
        self.hits.push(SearchHit {
            file: self.file.to_string(),
            line: line_number.unwrap_or(0) as usize,
            text: clip_line(&String::from_utf8_lossy(bytes)),
            kind: kind.to_string(),
        });
        true
    }
}

impl Sink for HitCollector<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> std::io::Result<bool> {
        // 一次回调可能带多行（匹配跨行时），逐行展开并递增行号。
        let mut line_number = mat.line_number();
        for line in mat.lines() {
            self.matches_in_file += 1;
            if !self.push(line_number, line, "match") {
                return Ok(false);
            }
            line_number = line_number.map(|n| n + 1);
        }
        Ok(true)
    }

    fn context(&mut self, _searcher: &Searcher, ctx: &SinkContext<'_>) -> std::io::Result<bool> {
        Ok(self.push(ctx.line_number(), ctx.bytes(), "context"))
    }
}

/// `search` 工具 -- 基于 ripgrep 引擎的内容搜索。
///
/// R0：只读。默认走正则；`literal = true` 时按字面量匹配。
pub struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }
    fn description(&self) -> &str {
        "Search local file contents with a regular expression, powered by the ripgrep engine. \
This is local file-content search, not web search; it requires `path` + `pattern`. \
Respects .gitignore, skips binary and oversized files. \
Set literal=true to match the pattern as plain text instead of a regex. \
Filter with glob (e.g. [\"*.rs\", \"!target/**\"]). \
Use output_mode=\"files\" to list only matching file paths, or \"count\" for a total. \
Returned paths are absolute and can be passed straight to read_file or edit."
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
                    "description": "Directory or single file to search."
                },
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for (Rust regex syntax)."
                },
                "literal": {
                    "type": "boolean",
                    "description": "Match the pattern as literal text instead of a regex. Default false."
                },
                "case": {
                    "type": "string",
                    "enum": ["smart", "sensitive", "insensitive"],
                    "description": "Case handling. 'smart' (default) is case-insensitive unless the pattern contains an uppercase letter."
                },
                "glob": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Glob filters, ripgrep -g semantics. Prefix with ! to exclude, e.g. [\"*.rs\", \"!**/tests/**\"]."
                },
                "context": {
                    "type": "integer",
                    "description": "Lines of context to include before and after each match. Default 0, max 10."
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files", "count"],
                    "description": "'content' (default) returns matching lines, 'files' only file paths, 'count' only totals."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum hits to return. Default 100, max 1000."
                },
                "include_hidden": {
                    "type": "boolean",
                    "description": "Also search dot-files and dot-directories. Default false."
                },
                "no_ignore": {
                    "type": "boolean",
                    "description": "Ignore .gitignore/.ignore rules. Default false."
                }
            },
            "required": ["path", "pattern"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let path = get_str(&input, "path")
            .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
        let pattern = get_str(&input, "pattern")
            .ok_or_else(|| ProductError::Other("missing 'pattern' parameter".to_string()))?;
        if pattern.is_empty() {
            return Err(ProductError::Other(
                "'pattern' must not be empty".to_string(),
            ));
        }

        let literal = get_bool(&input, "literal", false);
        let effective_pattern = if literal {
            regex_escape(pattern)
        } else {
            pattern.to_string()
        };

        let (case_insensitive, case_smart) = match get_str(&input, "case").unwrap_or("smart") {
            "insensitive" => (true, false),
            "sensitive" => (false, false),
            // smart：模式含大写才区分大小写
            _ => (false, true),
        };

        let context = get_usize(&input, "context", 0, MAX_CONTEXT);
        let max_results = get_usize(&input, "max_results", DEFAULT_MAX_RESULTS, MAX_MAX_RESULTS)
            .clamp(1, MAX_MAX_RESULTS);
        let output_mode = get_str(&input, "output_mode")
            .unwrap_or("content")
            .to_string();
        if !matches!(output_mode.as_str(), "content" | "files" | "count") {
            return Err(ProductError::Other(format!(
                "unknown output_mode '{output_mode}' (expected content | files | count)"
            )));
        }
        let include_hidden = get_bool(&input, "include_hidden", false);
        let no_ignore = get_bool(&input, "no_ignore", false);
        let globs = get_globs(&input);

        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(case_insensitive)
            .case_smart(case_smart)
            .line_terminator(Some(b'\n'))
            .build(&effective_pattern)
            .map_err(|e| ProductError::Other(format!("invalid pattern '{pattern}': {e}")))?;

        // files / count 模式不需要行文本，关掉行号与上下文可少走一大段拷贝。
        let want_lines = output_mode == "content";
        let mut searcher = SearcherBuilder::new()
            .line_number(want_lines)
            .before_context(if want_lines { context } else { 0 })
            .after_context(if want_lines { context } else { 0 })
            // 遇到 NUL 立即放弃该文件：等价 rg 默认的二进制跳过
            .binary_detection(BinaryDetection::quit(0))
            .build();

        let root = PathBuf::from(path);
        let root_is_file = root.is_file();

        let mut hits: Vec<SearchHit> = Vec::new();
        let mut matched_files: Vec<String> = Vec::new();
        let mut total_matches = 0usize;
        let mut files_searched = 0usize;
        let mut truncated = false;

        // 单文件目标：绕过遍历器，避免被 hidden/ignore 规则挡掉用户明确指定的文件。
        let candidates: Vec<PathBuf> = if root_is_file {
            vec![root]
        } else {
            let walker = build_walker(&root, &globs, include_hidden, no_ignore, None)?;
            let mut files = Vec::new();
            for entry in walker {
                let entry = match entry {
                    Ok(entry) => entry,
                    // 单个条目不可读（权限等）不应让整次搜索失败。
                    Err(err) => {
                        tracing::debug!(error = %err, "search: skipping unreadable entry");
                        continue;
                    }
                };
                if entry.file_type().is_some_and(|ft| ft.is_file()) {
                    files.push(entry.into_path());
                }
            }
            files
        };

        for file in candidates {
            if truncated {
                break;
            }
            let file_display = path_for_display(&file);
            files_searched += 1;

            // 内层作用域：collector 借走 hits，出了块才还，后面才能动 hits / file_display。
            let (matches_in_file, limit_reached, failure) = {
                let mut collector = HitCollector {
                    file: &file_display,
                    hits: &mut hits,
                    matches_in_file: 0,
                    collect: want_lines,
                    limit: max_results,
                    limit_reached: false,
                };
                let failure = searcher
                    .search_path(&matcher, &file, &mut collector)
                    .err()
                    .map(|e| e.to_string());
                (collector.matches_in_file, collector.limit_reached, failure)
            };

            // 单个文件读不动（权限、编码、竞态删除）不该让整次搜索失败。
            if let Some(err) = failure {
                tracing::debug!(file = %file_display, error = %err, "search: file skipped");
                continue;
            }
            if matches_in_file > 0 {
                total_matches += matches_in_file;
                matched_files.push(file_display);
            }
            if limit_reached || (!want_lines && matched_files.len() >= max_results) {
                truncated = true;
            }
        }

        let files_matched = matched_files.len();
        matched_files.truncate(max_results);

        let hint = if truncated {
            Some(format!(
                "结果已在 {max_results} 条处截断。请收窄 pattern、加 glob 过滤，\
或改用 output_mode=\"files\" 先定位文件。"
            ))
        } else {
            None
        };

        let output = match output_mode.as_str() {
            "files" => SearchOutput {
                mode: output_mode,
                truncated,
                files_searched,
                files_matched,
                hits: None,
                files: Some(matched_files),
                total_matches: Some(total_matches),
                hint,
            },
            "count" => SearchOutput {
                mode: output_mode,
                truncated,
                files_searched,
                files_matched,
                hits: None,
                files: None,
                total_matches: Some(total_matches),
                hint,
            },
            _ => SearchOutput {
                mode: output_mode,
                truncated,
                files_searched,
                files_matched,
                hits: Some(hits),
                files: None,
                total_matches: None,
                hint,
            },
        };

        serde_json::to_string(&output).map_err(|e| ProductError::Other(format!("JSON error: {e}")))
    }
}

// ============================================================================
// glob -- R0
// ============================================================================

#[derive(Debug, Serialize)]
struct GlobOutput {
    truncated: bool,
    /// 本次返回的文件数。
    returned: usize,
    /// 匹配到的文件总数（可能大于 `returned`）。
    matched: usize,
    files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

/// `glob` 工具 -- 按文件名模式列出文件。
///
/// R0：只读。补上 `search`（按内容）与 `list_files`（单层）之间的空档：
/// 「这个仓库里所有的 `**/*.rs` 在哪」。
pub struct GlobTool;

/// `path` 对模型是可选的；缺省值必须由会话的 `PathGuard` 注入，不能让
/// 执行器回落到 R-Code 进程的当前目录。
const GLOB_PATH_BINDINGS: &[PathBinding] = &[PathBinding::default_root("path")];

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }
    fn description(&self) -> &str {
        "Find files by name pattern (e.g. \"**/*.rs\", \"src/**/test_*.py\"), \
recursing from the given directory and respecting .gitignore. \
When path is omitted, search from the attached workspace root. \
A pattern without a slash matches against the file name only, like gitignore. \
Sort by \"path\" (default) or \"mtime\" to surface recently changed files. \
Returned paths are absolute."
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn path_bindings(&self) -> &'static [PathBinding] {
        GLOB_PATH_BINDINGS
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to search from. Optional; defaults to the attached workspace root."
                },
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern, e.g. '**/*.rs' or 'Cargo.toml'."
                },
                "sort": {
                    "type": "string",
                    "enum": ["path", "mtime"],
                    "description": "'path' (default) sorts alphabetically, 'mtime' newest first."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum files to return. Default 100, max 1000."
                },
                "include_hidden": {
                    "type": "boolean",
                    "description": "Also match dot-files and dot-directories. Default false."
                },
                "no_ignore": {
                    "type": "boolean",
                    "description": "Ignore .gitignore/.ignore rules. Default false."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let path = get_str(&input, "path")
            .ok_or_else(|| ProductError::Other("missing 'path' parameter".to_string()))?;
        let pattern = get_str(&input, "pattern")
            .ok_or_else(|| ProductError::Other("missing 'pattern' parameter".to_string()))?;
        if pattern.trim().is_empty() {
            return Err(ProductError::Other(
                "'pattern' must not be empty".to_string(),
            ));
        }

        let max_results = get_usize(&input, "max_results", DEFAULT_MAX_RESULTS, MAX_MAX_RESULTS)
            .clamp(1, MAX_MAX_RESULTS);
        let include_hidden = get_bool(&input, "include_hidden", false);
        let no_ignore = get_bool(&input, "no_ignore", false);
        let sort_by_mtime = get_str(&input, "sort").unwrap_or("path") == "mtime";

        // gitignore 语义：不含 `/` 的模式只匹配文件名，含 `/` 的匹配相对路径。
        let match_name_only = !pattern.contains('/');
        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(!match_name_only)
            .build()
            .map_err(|e| ProductError::Other(format!("invalid glob '{pattern}': {e}")))?
            .compile_matcher();

        let root = PathBuf::from(path);
        let walker = build_walker(&root, &[], include_hidden, no_ignore, None)?;

        let mut found: Vec<(PathBuf, Option<std::time::SystemTime>)> = Vec::new();
        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::debug!(error = %err, "glob: skipping unreadable entry");
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            let full = entry.path();
            let candidate: std::borrow::Cow<'_, str> = if match_name_only {
                full.file_name().unwrap_or_default().to_string_lossy()
            } else {
                // glob 模式一律用 `/`，Windows 路径先归一化。
                let relative = full.strip_prefix(&root).unwrap_or(full);
                std::borrow::Cow::Owned(relative.to_string_lossy().replace('\\', "/"))
            };
            if !glob.is_match(candidate.as_ref()) {
                continue;
            }
            let mtime = if sort_by_mtime {
                entry.metadata().ok().and_then(|m| m.modified().ok())
            } else {
                None
            };
            found.push((full.to_path_buf(), mtime));
        }

        if sort_by_mtime {
            // 新的在前；无 mtime 的排最后。
            found.sort_by(|a, b| match (a.1, b.1) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => a.0.cmp(&b.0),
            });
        }
        // path 模式下遍历器已按路径排好序，无需再排。

        let matched = found.len();
        let truncated = matched > max_results;
        let files: Vec<String> = found
            .into_iter()
            .take(max_results)
            .map(|(p, _)| path_for_display(p))
            .collect();

        let hint = if truncated {
            Some(format!(
                "共匹配 {matched} 个文件，仅返回前 {max_results} 个。请收窄 pattern 或提高 max_results。"
            ))
        } else {
            None
        };

        let output = GlobOutput {
            truncated,
            // `matched` 是匹配总数，`returned` 是本次实际返回数——分开报，
            // 否则模型会看到 "count: 5" 和 "共匹配 20 个" 自相矛盾。
            returned: files.len(),
            matched,
            files,
            hint,
        };
        serde_json::to_string(&output).map_err(|e| ProductError::Other(format!("JSON error: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn parse(json: &str) -> serde_json::Value {
        serde_json::from_str(json).expect("tool output is valid JSON")
    }

    /// 固定命中数的测试统一走 `no_ignore: true`。
    ///
    /// 否则遍历器会去读开发机的 global gitignore（`core.excludesFile`）和临时目录
    /// 祖先里的 `.gitignore`——那些内容因机器而异，会让断言随机失败。
    /// 专门验证忽略规则的用例不用这个 helper，自己显式构造输入。
    fn stable(dir: &TempDir, extra: serde_json::Value) -> serde_json::Value {
        let mut input = serde_json::json!({
            "path": dir.path().to_str().unwrap(),
            "no_ignore": true
        });
        let object = input.as_object_mut().unwrap();
        for (key, value) in extra.as_object().unwrap() {
            object.insert(key.clone(), value.clone());
        }
        input
    }

    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "foo bar\nbaz foo\n").unwrap();
        fs::write(dir.path().join("b.txt"), "no match here\n").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("c.txt"), "foo again\n").unwrap();
        fs::write(dir.path().join("sub").join("d.rs"), "fn Foo() {}\n").unwrap();
        dir
    }

    // ── search ────────────────────────────────────────────────

    #[tokio::test]
    async fn search_finds_matches_recursively() {
        let dir = fixture();
        let input = stable(
            &dir,
            serde_json::json!({"pattern": "foo", "case": "sensitive"}),
        );
        let out = SearchTool.execute(input).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["mode"], "content");
        assert_eq!(v["truncated"], false);
        let hits = v["hits"].as_array().unwrap();
        // a.txt 两行 + sub/c.txt 一行
        assert_eq!(hits.len(), 3);
        assert_eq!(v["files_matched"], 2);
        assert_eq!(SearchTool.risk_level(), RiskLevel::R0);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn search_and_glob_hide_windows_verbatim_prefixes_in_results() {
        let dir = fixture();
        let canonical_root = fs::canonicalize(dir.path()).unwrap();
        let raw_root = canonical_root.to_string_lossy().into_owned();
        assert!(
            raw_root.starts_with(r"\\?\"),
            "canonical test path was not verbatim: {raw_root}"
        );

        let search = SearchTool
            .execute(serde_json::json!({
                "path": raw_root,
                "pattern": "foo",
                "case": "sensitive",
                "no_ignore": true
            }))
            .await
            .unwrap();
        let search = parse(&search);
        let hit_paths: Vec<&str> = search["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["file"].as_str().unwrap())
            .collect();
        assert!(hit_paths
            .iter()
            .any(|path| *path == path_for_display(canonical_root.join("a.txt"))));
        assert!(hit_paths.iter().all(|path| !path.starts_with(r"\\?\")));

        let search_files = SearchTool
            .execute(serde_json::json!({
                "path": raw_root,
                "pattern": "foo",
                "case": "sensitive",
                "output_mode": "files",
                "no_ignore": true
            }))
            .await
            .unwrap();
        let search_files = parse(&search_files);
        assert!(search_files["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| !file.as_str().unwrap().starts_with(r"\\?\")));

        let glob = GlobTool
            .execute(serde_json::json!({
                "path": raw_root,
                "pattern": "*.rs",
                "no_ignore": true
            }))
            .await
            .unwrap();
        let glob = parse(&glob);
        assert_eq!(
            glob["files"][0],
            path_for_display(canonical_root.join("sub").join("d.rs"))
        );
        assert!(!glob["files"][0].as_str().unwrap().starts_with(r"\\?\"));
    }

    #[tokio::test]
    async fn search_no_matches_is_empty_not_error() {
        let dir = fixture();
        let out = SearchTool
            .execute(stable(&dir, serde_json::json!({"pattern": "nonexistent"})))
            .await
            .unwrap();
        let v = parse(&out);
        assert!(v["hits"].as_array().unwrap().is_empty());
        assert_eq!(v["files_matched"], 0);
    }

    #[tokio::test]
    async fn search_missing_params() {
        assert!(SearchTool
            .execute(serde_json::json!({ "path": "." }))
            .await
            .is_err());
        assert!(SearchTool
            .execute(serde_json::json!({ "pattern": "x" }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn search_supports_regex() {
        let dir = fixture();
        let out = SearchTool
            .execute(stable(&dir, serde_json::json!({"pattern": r"^fn\s+\w+\("})))
            .await
            .unwrap();
        let hits = parse(&out)["hits"].as_array().unwrap().clone();
        assert_eq!(hits.len(), 1);
        assert!(hits[0]["file"].as_str().unwrap().ends_with("d.rs"));
        assert_eq!(hits[0]["line"], 1);
    }

    #[tokio::test]
    async fn search_literal_mode_escapes_metacharacters() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("x.txt"), "a.c\nabc\n").unwrap();

        // 正则模式下 `a.c` 同时命中 "a.c" 与 "abc"
        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "a.c", "no_ignore": true
            }))
            .await
            .unwrap();
        assert_eq!(parse(&out)["hits"].as_array().unwrap().len(), 2);

        // 字面量模式只命中 "a.c"
        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "a.c",
                "literal": true, "no_ignore": true
            }))
            .await
            .unwrap();
        let hits = parse(&out)["hits"].as_array().unwrap().clone();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["text"], "a.c");
    }

    #[tokio::test]
    async fn search_invalid_regex_is_reported() {
        let dir = fixture();
        let err = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "a(b"
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid pattern"));
    }

    #[tokio::test]
    async fn search_smart_case_is_default() {
        let dir = fixture();
        // 全小写 pattern -> 不区分大小写，命中 "fn Foo()" 里的 Foo
        let out = SearchTool
            .execute(stable(&dir, serde_json::json!({"pattern": "foo"})))
            .await
            .unwrap();
        assert_eq!(parse(&out)["hits"].as_array().unwrap().len(), 4);

        // 含大写 -> 区分大小写，只命中 Foo
        let out = SearchTool
            .execute(stable(&dir, serde_json::json!({"pattern": "Foo"})))
            .await
            .unwrap();
        assert_eq!(parse(&out)["hits"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_glob_filter_narrows_files() {
        let dir = fixture();
        let input = stable(
            &dir,
            serde_json::json!({"pattern": "foo", "glob": ["*.rs"]}),
        );
        let out = SearchTool.execute(input).await.unwrap();
        let hits = parse(&out)["hits"].as_array().unwrap().clone();
        assert_eq!(hits.len(), 1);
        assert!(hits[0]["file"].as_str().unwrap().ends_with("d.rs"));
    }

    #[tokio::test]
    async fn search_glob_exclusion() {
        let dir = fixture();
        let input = stable(
            &dir,
            serde_json::json!({"pattern": "foo", "case": "sensitive", "glob": ["!sub/**"]}),
        );
        let out = SearchTool.execute(input).await.unwrap();
        assert_eq!(parse(&out)["hits"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn search_output_mode_files() {
        let dir = fixture();
        let input = stable(
            &dir,
            serde_json::json!({"pattern": "foo", "case": "sensitive", "output_mode": "files"}),
        );
        let out = SearchTool.execute(input).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["mode"], "files");
        assert_eq!(v["files"].as_array().unwrap().len(), 2);
        assert!(v["hits"].is_null());
    }

    #[tokio::test]
    async fn search_output_mode_count() {
        let dir = fixture();
        let input = stable(
            &dir,
            serde_json::json!({"pattern": "foo", "case": "sensitive", "output_mode": "count"}),
        );
        let out = SearchTool.execute(input).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["mode"], "count");
        assert_eq!(v["total_matches"], 3);
        assert!(v["files"].is_null());
        assert!(v["hits"].is_null());
    }

    #[tokio::test]
    async fn search_rejects_unknown_output_mode() {
        let dir = fixture();
        assert!(SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "foo", "output_mode": "bogus"
            }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn search_context_lines_are_labelled() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("x.txt"), "one\ntwo\nTARGET\nfour\nfive\n").unwrap();
        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "TARGET",
                "context": 1, "no_ignore": true
            }))
            .await
            .unwrap();
        let hits = parse(&out)["hits"].as_array().unwrap().clone();
        assert_eq!(hits.len(), 3);
        let kinds: Vec<&str> = hits
            .iter()
            .map(|h| h["kind"].as_str().unwrap_or("match"))
            .collect();
        assert_eq!(kinds, vec!["context", "match", "context"]);
        assert_eq!(hits[1]["line"], 3);
    }

    #[tokio::test]
    async fn search_max_results_truncates_with_hint() {
        let dir = TempDir::new().unwrap();
        let body: String = (0..50).map(|_| "needle\n").collect();
        fs::write(dir.path().join("x.txt"), body).unwrap();

        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "needle",
                "max_results": 5, "no_ignore": true
            }))
            .await
            .unwrap();
        let v = parse(&out);
        assert_eq!(v["hits"].as_array().unwrap().len(), 5);
        assert_eq!(v["truncated"], true);
        assert!(v["hint"].as_str().unwrap().contains('5'));
    }

    #[tokio::test]
    async fn search_respects_gitignore_and_can_override() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();
        fs::write(dir.path().join("kept.txt"), "needle\n").unwrap();

        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "needle"
            }))
            .await
            .unwrap();
        assert_eq!(parse(&out)["hits"].as_array().unwrap().len(), 1);

        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "needle", "no_ignore": true
            }))
            .await
            .unwrap();
        assert_eq!(parse(&out)["hits"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn search_skips_hidden_unless_asked() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".hidden")).unwrap();
        fs::write(dir.path().join(".hidden").join("x.txt"), "needle\n").unwrap();

        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "needle"
            }))
            .await
            .unwrap();
        assert!(parse(&out)["hits"].as_array().unwrap().is_empty());

        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "needle", "include_hidden": true
            }))
            .await
            .unwrap();
        assert_eq!(parse(&out)["hits"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_skips_binary_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("bin.dat"), b"needle\x00\x01\x02needle").unwrap();
        fs::write(dir.path().join("ok.txt"), "needle\n").unwrap();

        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "needle", "no_ignore": true
            }))
            .await
            .unwrap();
        let hits = parse(&out)["hits"].as_array().unwrap().clone();
        // 文本文件照常命中；二进制文件在 NUL 处放弃，不把乱码灌进上下文。
        assert!(hits
            .iter()
            .any(|h| h["file"].as_str().unwrap().ends_with("ok.txt")));
        assert!(!hits
            .iter()
            .any(|h| h["file"].as_str().unwrap().ends_with("bin.dat")));
    }

    #[tokio::test]
    async fn search_single_file_target() {
        let dir = fixture();
        let file = dir.path().join("a.txt");
        let out = SearchTool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(), "pattern": "foo",
                "case": "sensitive", "no_ignore": true
            }))
            .await
            .unwrap();
        let v = parse(&out);
        assert_eq!(v["hits"].as_array().unwrap().len(), 2);
        assert_eq!(v["files_searched"], 1);
    }

    #[tokio::test]
    async fn search_clips_very_long_lines() {
        let dir = TempDir::new().unwrap();
        let long = format!("needle{}", "x".repeat(MAX_LINE_CHARS * 2));
        fs::write(dir.path().join("x.txt"), format!("{long}\n")).unwrap();

        let out = SearchTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "needle", "no_ignore": true
            }))
            .await
            .unwrap();
        let text = parse(&out)["hits"][0]["text"].as_str().unwrap().to_string();
        assert!(text.contains("[行已截断]"));
        assert!(text.chars().count() < MAX_LINE_CHARS + 32);
    }

    // ── glob ──────────────────────────────────────────────────

    #[tokio::test]
    async fn glob_matches_recursive_pattern() {
        let dir = fixture();
        let out = GlobTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "**/*.txt", "no_ignore": true
            }))
            .await
            .unwrap();
        let v = parse(&out);
        assert_eq!(v["matched"], 3);
        assert_eq!(v["returned"], 3);
        assert_eq!(GlobTool.risk_level(), RiskLevel::R0);
    }

    #[test]
    fn glob_schema_requires_only_pattern_and_defaults_path_to_workspace_root() {
        let schema = GlobTool.input_schema();
        assert_eq!(schema["required"], serde_json::json!(["pattern"]));

        let bindings = GlobTool.path_bindings();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].key, "path");
        assert_eq!(bindings[0].arity, crate::gateway::PathArity::DefaultRoot);
    }

    #[tokio::test]
    async fn glob_bare_pattern_matches_file_name_anywhere() {
        let dir = fixture();
        // 不含 `/` 的模式按 gitignore 语义只比对文件名，因此能命中子目录
        let out = GlobTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "*.rs", "no_ignore": true
            }))
            .await
            .unwrap();
        let v = parse(&out);
        assert_eq!(v["matched"], 1);
        assert!(v["files"][0].as_str().unwrap().ends_with("d.rs"));
    }

    #[tokio::test]
    async fn glob_path_pattern_is_anchored_at_root() {
        let dir = fixture();
        let out = GlobTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "sub/*.txt", "no_ignore": true
            }))
            .await
            .unwrap();
        let v = parse(&out);
        assert_eq!(v["matched"], 1);
        assert!(v["files"][0].as_str().unwrap().ends_with("c.txt"));
    }

    #[tokio::test]
    async fn glob_exact_name() {
        let dir = fixture();
        let out = GlobTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "a.txt", "no_ignore": true
            }))
            .await
            .unwrap();
        assert_eq!(parse(&out)["matched"], 1);
    }

    #[tokio::test]
    async fn glob_respects_gitignore() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();
        fs::write(dir.path().join("a.log"), "x").unwrap();
        fs::write(dir.path().join("b.txt"), "x").unwrap();

        let out = GlobTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "*"
            }))
            .await
            .unwrap();
        let files: Vec<String> = parse(&out)["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(files.iter().any(|f| f.ends_with("b.txt")));
        assert!(!files.iter().any(|f| f.ends_with("a.log")));
    }

    #[tokio::test]
    async fn glob_truncates_with_hint() {
        let dir = TempDir::new().unwrap();
        for i in 0..20 {
            fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        let out = GlobTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "*.txt",
                "max_results": 5, "no_ignore": true
            }))
            .await
            .unwrap();
        let v = parse(&out);
        assert_eq!(v["returned"], 5);
        assert_eq!(v["matched"], 20);
        assert_eq!(v["truncated"], true);
        assert!(v["hint"].as_str().unwrap().contains("20"));
    }

    #[tokio::test]
    async fn glob_invalid_pattern_is_reported() {
        let dir = fixture();
        let err = GlobTool
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(), "pattern": "["
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid glob"));
    }

    #[tokio::test]
    async fn glob_missing_params() {
        assert!(GlobTool
            .execute(serde_json::json!({ "path": "." }))
            .await
            .is_err());
        assert!(GlobTool
            .execute(serde_json::json!({ "pattern": "*" }))
            .await
            .is_err());
    }

    #[test]
    fn regex_escape_neutralizes_metacharacters() {
        assert_eq!(regex_escape("a.c"), r"a\.c");
        assert_eq!(regex_escape("a+b*c?"), r"a\+b\*c\?");
        assert_eq!(regex_escape("x(y)[z]"), r"x\(y\)\[z\]");
        assert_eq!(regex_escape("plain"), "plain");
    }

    #[test]
    fn clip_line_preserves_short_lines() {
        assert_eq!(clip_line("hello\n"), "hello");
        assert_eq!(clip_line("hello\r\n"), "hello");
        assert!(clip_line(&"x".repeat(MAX_LINE_CHARS + 1)).contains("[行已截断]"));
    }

    #[test]
    fn get_globs_accepts_string_or_array() {
        assert_eq!(
            get_globs(&serde_json::json!({"glob": "*.rs"})),
            vec!["*.rs"]
        );
        assert_eq!(
            get_globs(&serde_json::json!({"glob": ["*.rs", "!x/**"]})),
            vec!["*.rs", "!x/**"]
        );
        assert!(get_globs(&serde_json::json!({})).is_empty());
        assert!(get_globs(&serde_json::json!({"glob": ""})).is_empty());
    }
}
