//! 外部编辑器（M4-02 / R-EDIT-02，Ctrl+G）。
//!
//! `$VISUAL` 优先、`$EDITOR` 次之、`vi` 兜底。当前输入写入临时文件 →
//! 阻塞运行编辑器（调用方负责临时退出 raw/alt-screen）→ 读回替换输入。
//! 编辑器非零退出 = 取消（输入内容不变，返回 Err）。

use std::path::PathBuf;

/// 解析编辑器命令（VISUAL > EDITOR > vi）。返回 (程序, 参数前缀)。
pub fn editor_command(visual: Option<&str>, editor: Option<&str>) -> (String, Vec<String>) {
    let raw = visual
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| editor.map(str::trim).filter(|value| !value.is_empty()))
        .unwrap_or("vi");
    let mut parts = raw.split_whitespace();
    let program = parts.next().unwrap_or("vi").to_string();
    let args = parts.map(str::to_string).collect();
    (program, args)
}

/// 运行外部编辑器编辑 `text`，成功返回编辑后的全文。
/// 非零退出/启动失败返回 Err（输入不被破坏）。
pub async fn run_external_editor(text: &str) -> Result<String, String> {
    let text = text.to_string();
    tokio::task::spawn_blocking(move || {
        let (program, args) = editor_command(
            std::env::var("VISUAL").ok().as_deref(),
            std::env::var("EDITOR").ok().as_deref(),
        );
        let path: PathBuf =
            std::env::temp_dir().join(format!("r-code-tui-editor-{}.md", std::process::id()));
        std::fs::write(&path, &text).map_err(|error| format!("写临时文件失败：{error}"))?;
        let status = std::process::Command::new(&program)
            .args(&args)
            .arg(&path)
            .status()
            .map_err(|error| format!("启动编辑器 {program} 失败：{error}"))?;
        if !status.success() {
            // 清理痕迹但不吞掉真实错误。
            let _ = std::fs::remove_file(&path);
            return Err(format!("编辑器退出码非零（{status}），已取消编辑"));
        }
        let edited =
            std::fs::read_to_string(&path).map_err(|error| format!("读回编辑结果失败：{error}"))?;
        let _ = std::fs::remove_file(&path);
        Ok(edited)
    })
    .await
    .map_err(|error| format!("编辑器任务失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).expect("stat").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod");
    }

    /// VISUAL > EDITOR > vi 的解析顺序。
    #[test]
    fn editor_command_prefers_visual_then_editor() {
        assert_eq!(
            editor_command(Some("code --wait"), Some("nano")),
            ("code".to_string(), vec!["--wait".to_string()])
        );
        assert_eq!(
            editor_command(None, Some("nano")),
            ("nano".to_string(), Vec::new())
        );
        assert_eq!(editor_command(None, None), ("vi".to_string(), Vec::new()));
        // 空串视为未设置。
        assert_eq!(
            editor_command(Some("  "), Some("nano")),
            ("nano".to_string(), Vec::new())
        );
    }

    /// M4-02.A2：真实 run_external_editor + fixture EDITOR——成功回填、
    /// 非零退出取消（单测试内串行改 env，避免并行竞态）。
    /// fixture 是 `#!/bin/sh` 脚本 + chmod，仅 Unix 可跑。
    #[cfg(unix)]
    #[tokio::test]
    async fn external_editor_roundtrip_with_fixture_editor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ok_script = dir.path().join("fixture-editor.sh");
        std::fs::write(
            &ok_script,
            "#!/bin/sh\nprintf 'edited by fixture\\nline2' > \"$1\"\n",
        )
        .expect("write ok script");
        make_executable(&ok_script);
        let fail_script = dir.path().join("fail-editor.sh");
        std::fs::write(&fail_script, "#!/bin/sh\nexit 3\n").expect("write fail script");
        make_executable(&fail_script);

        let previous_visual = std::env::var("VISUAL").ok();
        // 成功路径：VISUAL 优先。
        std::env::set_var("VISUAL", &ok_script);
        let ok = run_external_editor("原始草稿").await;
        // 失败路径：非零退出 = 取消。
        std::env::set_var("VISUAL", &fail_script);
        let failed = run_external_editor("草稿").await;
        match previous_visual {
            Some(value) => std::env::set_var("VISUAL", value),
            None => std::env::remove_var("VISUAL"),
        }

        assert_eq!(
            ok.expect("editor roundtrip"),
            "edited by fixture\nline2",
            "编辑器写入的内容必须完整读回"
        );
        let error = failed.expect_err("非零退出必须取消");
        assert!(error.contains("退出码非零"), "{error}");
    }
}
