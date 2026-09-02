//! `/export [path]` 会话导出（pi 对齐 G7）。
//!
//! 数据源 = 当前 transcript 视图（`TuiState.rows`）——与用户所见一致（含
//! 系统行与 `!` shell 行），纯函数渲染 + 写盘，不经宿主。格式按扩展名分派：
//! `.md`（默认）/ `.html`（pi 同款单文件网页）/ `.jsonl`（TranscriptRow
//! 原生序列化，机器可读）。默认文件名落在当前目录：
//! `r-code-session-YYYYMMDD-HHMMSS.md`。

use crate::TranscriptRow;

/// 导出元信息（卡头几行；model label 空时省略该行）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExportMeta {
    /// 形如 `(openai) gpt-5.6`（model_selector::model_label 口径）。
    pub model_label: String,
    /// 导出时间（本地时区，人类可读）。
    pub exported_at: String,
}

/// 解析 `/export` 参数为目标路径。
///
/// - 无参 → `<cwd>/r-code-session-<时间戳>.md`；
/// - 有参但无扩展名 → 追加 `.md`（默认 markdown）；
/// - `.html` / `.jsonl` 原样保留，其余扩展名按 markdown 渲染（文件名不动）。
pub fn resolve_path(arg: Option<&str>, now: &str) -> std::path::PathBuf {
    let Some(arg) = arg.map(str::trim).filter(|text| !text.is_empty()) else {
        return std::path::PathBuf::from(format!("r-code-session-{now}.md"));
    };
    let path = std::path::PathBuf::from(arg);
    if path.extension().is_none() {
        let mut owned = path.into_os_string();
        owned.push(".md");
        return std::path::PathBuf::from(owned);
    }
    path
}

/// 按扩展名渲染内容（未知扩展名按 markdown）。
pub fn render(path: &std::path::Path, rows: &[TranscriptRow], meta: &ExportMeta) -> String {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") | Some("htm") => render_html(rows, meta),
        Some("jsonl") => render_jsonl(rows),
        _ => render_markdown(rows, meta),
    }
}

/// 写盘（父目录缺失即报错，不代建——导出目标由用户指定）。
/// 返回写入的绝对/原样路径字符串（供 transcript 状态行展示）。
pub fn write_export(
    path: &std::path::Path,
    rows: &[TranscriptRow],
    meta: &ExportMeta,
) -> Result<String, String> {
    if rows.is_empty() {
        return Err("当前会话为空，没有可导出的内容".to_string());
    }
    let content = render(path, rows, meta);
    std::fs::write(path, content).map_err(|error| format!("写入失败：{error}"))?;
    Ok(path.display().to_string())
}

/// Markdown 渲染。
pub fn render_markdown(rows: &[TranscriptRow], meta: &ExportMeta) -> String {
    let mut out = String::from("# R-Code CLI 会话导出\n\n");
    let mut header = vec![format!("- 导出时间：{}", meta.exported_at)];
    if !meta.model_label.is_empty() {
        header.insert(0, format!("- 模型：{}", meta.model_label));
    }
    out.push_str(&header.join("\n"));
    out.push_str(&format!("\n- 行数：{}\n\n---\n", rows.len()));
    for row in rows {
        match row {
            TranscriptRow::User { text } => {
                out.push_str(&format!("\n## 你\n\n{text}\n"));
            }
            TranscriptRow::Assistant { text, .. } => {
                out.push_str(&format!("\n## R-Code\n\n{text}\n"));
            }
            TranscriptRow::ToolCard {
                name,
                summary,
                is_error,
            } => {
                let flag = if *is_error { "（失败）" } else { "" };
                out.push_str(&format!("\n- `⏺ {name}` {summary}{flag}\n"));
            }
            TranscriptRow::System { text } => {
                out.push_str(&format!("\n> · {text}\n"));
            }
            TranscriptRow::Shell(shell) => match shell {
                crate::bang_command::ShellRow::Prompt { command } => {
                    out.push_str(&format!("\n```bash\n$ {command}\n"));
                }
                crate::bang_command::ShellRow::Output { text, exit_code } => {
                    out.push_str(&format!(
                        "{}\n```\n\n（exit {}）\n",
                        indent(text),
                        exit_code
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "?".to_string())
                    ));
                }
            },
        }
    }
    out
}

/// HTML 渲染（单文件、零外部资源；pi 同款自包含形态）。
pub fn render_html(rows: &[TranscriptRow], meta: &ExportMeta) -> String {
    let mut body = String::new();
    for row in rows {
        let (role, html) = match row {
            TranscriptRow::User { text } => ("user", html_escape(text)),
            TranscriptRow::Assistant { text, .. } => ("assistant", html_escape(text)),
            TranscriptRow::ToolCard {
                name,
                summary,
                is_error,
            } => {
                let flag = if *is_error { "（失败）" } else { "" };
                (
                    "tool",
                    format!(
                        "<code>{}</code> {}{}",
                        html_escape(name),
                        html_escape(summary),
                        flag
                    ),
                )
            }
            TranscriptRow::System { text } => ("system", html_escape(text)),
            TranscriptRow::Shell(shell) => match shell {
                crate::bang_command::ShellRow::Prompt { command } => {
                    ("shell", format!("<code>$ {}</code>", html_escape(command)))
                }
                crate::bang_command::ShellRow::Output { text, exit_code } => (
                    "shell",
                    format!(
                        "<pre>{}</pre><small>exit {}</small>",
                        html_escape(text),
                        exit_code
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "?".to_string())
                    ),
                ),
            },
        };
        body.push_str(&format!(
            "<div class=\"msg {role}\"><span class=\"role\">{role}</span><div class=\"body\">{html}</div></div>\n"
        ));
    }
    let model_line = if meta.model_label.is_empty() {
        String::new()
    } else {
        format!(
            "<meta name=\"model\" content=\"{}\">",
            html_escape(&meta.model_label)
        )
    };
    format!(
        "<!DOCTYPE html>\n<html lang=\"zh\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>R-Code CLI 会话导出</title>\n{model_line}\n<style>\n\
         body{{font-family:system-ui,sans-serif;max-width:52em;margin:2rem auto;padding:0 1rem;color:#24292f}}\n\
         h1{{font-size:1.3rem}}\n\
         .msg{{margin:.8rem 0;padding:.6rem .9rem;border-radius:8px;background:#f6f8fa}}\n\
         .msg.assistant{{background:#eefdf2}}\n\
         .msg.system{{background:#fff8e6;color:#57600f}}\n\
         .msg.tool,.msg.shell{{background:#f0f0f4;font-family:ui-monospace,monospace;font-size:.9em}}\n\
         .role{{display:block;font-size:.72rem;text-transform:uppercase;letter-spacing:.08em;color:#656d76;margin-bottom:.3rem}}\n\
         .body{{white-space:pre-wrap;word-break:break-word}}\n\
         pre{{margin:.3rem 0;white-space:pre-wrap}}\n\
         </style>\n</head>\n<body>\n\
         <h1>R-Code CLI 会话导出</h1>\n\
         <p><small>导出于 {exported} · {count} 行</small></p>\n\
         {body}</body>\n</html>\n",
        exported = html_escape(&meta.exported_at),
        count = rows.len(),
    )
}

/// JSONL 渲染（TranscriptRow 原生 serde 形态，一行一条）。
pub fn render_jsonl(rows: &[TranscriptRow]) -> String {
    let mut out = String::new();
    for row in rows {
        out.push_str(&serde_json::to_string(row).unwrap_or_default());
        out.push('\n');
    }
    out
}

/// 多行 shell 输出缩进两格（markdown 代码块内可读性）。
fn indent(text: &str) -> String {
    text.lines().collect::<Vec<_>>().join("\n  ")
}

/// HTML 转义（内容全部来自模型/用户输入，必须转义）。
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rows() -> Vec<TranscriptRow> {
        vec![
            TranscriptRow::User {
                text: "你好 <script>".to_string(),
            },
            TranscriptRow::Assistant {
                text: "回答 & 解释".to_string(),
                complete: true,
            },
            TranscriptRow::ToolCard {
                name: "bash".to_string(),
                summary: "cargo test".to_string(),
                is_error: false,
            },
            TranscriptRow::ToolCard {
                name: "edit".to_string(),
                summary: "src/lib.rs".to_string(),
                is_error: true,
            },
            TranscriptRow::System {
                text: "已切换模型".to_string(),
            },
            TranscriptRow::Shell(crate::bang_command::ShellRow::Prompt {
                command: "cargo build".to_string(),
            }),
            TranscriptRow::Shell(crate::bang_command::ShellRow::Output {
                text: "ok\nline2".to_string(),
                exit_code: Some(0),
            }),
        ]
    }

    fn meta() -> ExportMeta {
        ExportMeta {
            model_label: "(demo) m".to_string(),
            exported_at: "2026-09-02 10:00:00".to_string(),
        }
    }

    /// G7.A1：路径解析——默认名 / 补 .md / 保留 .html/.jsonl。
    #[test]
    fn resolve_path_dispatches_by_extension() {
        assert_eq!(
            resolve_path(None, "20260902-100000"),
            std::path::PathBuf::from("r-code-session-20260902-100000.md")
        );
        assert_eq!(
            resolve_path(Some("notes"), "x"),
            std::path::PathBuf::from("notes.md")
        );
        assert_eq!(
            resolve_path(Some("a.html"), "x"),
            std::path::PathBuf::from("a.html")
        );
        // 空白参数按无参处理。
        assert_eq!(
            resolve_path(Some("   "), "20260902-100000")
                .display()
                .to_string(),
            "r-code-session-20260902-100000.md"
        );
    }

    /// G7.A2：markdown 全行型映射 + 头部元信息。
    #[test]
    fn markdown_maps_every_row_kind() {
        let md = render_markdown(&sample_rows(), &meta());
        assert!(md.contains("# R-Code CLI 会话导出"), "{md}");
        assert!(md.contains("- 模型：(demo) m"), "{md}");
        assert!(md.contains("## 你\n\n你好 <script>"), "{md}");
        assert!(md.contains("## R-Code\n\n回答 & 解释"), "{md}");
        assert!(md.contains("- `⏺ bash` cargo test"), "{md}");
        assert!(md.contains("- `⏺ edit` src/lib.rs（失败）"), "{md}");
        assert!(md.contains("> · 已切换模型"), "{md}");
        assert!(md.contains("```bash\n$ cargo build"), "{md}");
        assert!(md.contains("（exit 0）"), "{md}");
    }

    /// G7.A3：HTML 转义（用户/模型内容不可注入标记）+ 单文件形态。
    #[test]
    fn html_escapes_user_content() {
        let html = render_html(&sample_rows(), &meta());
        assert!(html.starts_with("<!DOCTYPE html>"), "自包含单文件");
        assert!(html.contains("&lt;script&gt;"), "尖括号必须转义：{html}");
        assert!(html.contains("回答 &amp; 解释"), "{html}");
        assert!(!html.contains("<script>"), "未转义原文不得出现：{html}");
        assert!(html.contains("class=\"msg tool\""), "{html}");
        // 模型标签进 meta（已转义空格无需求，但引号内容必须安全）。
        assert!(html.contains("content=\"(demo) m\""), "{html}");
    }

    /// G7.A4：jsonl 一行一条 + 可反序列化回 TranscriptRow。
    #[test]
    fn jsonl_roundtrips_rows() {
        let jsonl = render_jsonl(&sample_rows());
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), sample_rows().len());
        let back: Vec<TranscriptRow> = lines
            .iter()
            .map(|line| serde_json::from_str(line).expect("valid row json"))
            .collect();
        assert_eq!(back, sample_rows(), "serde 往返无损");
    }

    /// G7.A5：写盘 + 空会话拒绝。
    #[test]
    fn write_export_roundtrip_and_empty_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let md = dir.path().join("out.md");
        let path = write_export(&md, &sample_rows(), &meta()).expect("write");
        assert_eq!(path, md.display().to_string());
        let content = std::fs::read_to_string(&md).expect("read");
        assert!(content.contains("# R-Code CLI 会话导出"));
        // 扩展名分派同函数内生效。
        let html = dir.path().join("out.html");
        write_export(&html, &sample_rows(), &meta()).expect("write html");
        assert!(std::fs::read_to_string(&html)
            .unwrap()
            .contains("<!DOCTYPE html>"));
        // 空会话是用户错误，不产出空文件。
        let err = write_export(&dir.path().join("e.md"), &[], &meta()).expect_err("empty");
        assert!(err.contains("没有可导出的内容"), "{err}");
    }
}
