//! 绿灯 checkpoint：测试全绿后以 `git stash create` 生成可回滚的恢复点。
//!
//! `git stash create` 只创建 stash 提交对象：不动 HEAD、不动 index、不动工作区，
//! 因此它是循环内安全、可重复的“打点”方式。回滚时才执行 `git reset --hard`，
//! 且必须先确认 HEAD 未被外部移动；untracked 文件永远不会被回滚。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 一个与工作区根绑定的 git checkpoint 管理器。仅在 workspace 本身是 git
/// 仓库时可用（`discover` 返回 `None` 表示降级为无 checkpoint 模式）。
#[derive(Debug, Clone)]
pub struct GreenCheckpoint {
    root: PathBuf,
}

impl GreenCheckpoint {
    /// 探测 `root` 是否是 git 仓库的顶层目录；不是则返回 `None`。
    pub fn discover(root: &Path) -> Option<Self> {
        if !root.is_dir() {
            return None;
        }
        let output = git(root, &["rev-parse", "--show-toplevel"]).ok()?;
        if !output.status.success() {
            return None;
        }
        let top = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if top.is_empty() {
            return None;
        }
        let root_canonical = root.canonicalize().ok()?;
        let top_canonical = Path::new(&top).canonicalize().ok()?;
        if root_canonical != top_canonical {
            return None;
        }
        Some(Self {
            root: root_canonical,
        })
    }

    /// run 开始时的 HEAD SHA。后续回滚必须校验 HEAD 未被外部移动。
    pub fn head_sha(&self) -> Result<String, String> {
        let output = git(&self.root, &["rev-parse", "HEAD"]).map_err(git_error)?;
        if !output.status.success() {
            return Err(format!(
                "git rev-parse HEAD 失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            return Err("git rev-parse HEAD 返回空值".to_string());
        }
        Ok(sha)
    }

    /// 创建一次 checkpoint。工作区没有 tracked 变更时返回 `Ok(None)`。
    pub fn capture(&self) -> Result<Option<String>, String> {
        let output =
            git(&self.root, &["stash", "create", "r-code checkpoint"]).map_err(git_error)?;
        if !output.status.success() {
            return Err(format!(
                "git stash create 失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            return Ok(None);
        }
        Ok(Some(sha))
    }

    /// 回滚到 checkpoint。执行前校验当前 HEAD 仍等于 run 开始时记录的
    /// `expected_head`；untracked 文件不受影响。
    pub fn rollback(&self, expected_head: &str, checkpoint_sha: &str) -> Result<(), String> {
        let current_head = self.head_sha()?;
        if current_head != expected_head {
            return Err(format!(
                "外部已移动 HEAD（{} -> {}），拒绝 checkpoint 回滚",
                expected_head, current_head
            ));
        }
        let output = git(&self.root, &["reset", "--hard", checkpoint_sha]).map_err(git_error)?;
        if !output.status.success() {
            return Err(format!(
                "git reset --hard 失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output, std::io::Error> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root);
    command.args(args);
    command.output()
}

fn git_error(error: std::io::Error) -> String {
    format!("无法执行 git：{error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn init_repo(root: &Path) {
        for args in [
            &["init", "-q"][..],
            &["config", "user.name", "r-code-test"][..],
            &["config", "user.email", "r-code-test@example.invalid"][..],
            &["config", "core.autocrlf", "false"][..],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .status()
                .expect("git init/config failed");
            assert!(status.success());
        }
    }

    fn commit_all(root: &Path, message: &str) {
        let add = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "."])
            .status()
            .expect("git add failed");
        assert!(add.success());
        let commit = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["commit", "-q", "-m", message])
            .status()
            .expect("git commit failed");
        assert!(commit.success());
    }

    fn write(root: &Path, name: &str, content: &str) {
        fs::write(root.join(name), content).expect("write test file");
    }

    fn read(root: &Path, name: &str) -> String {
        fs::read_to_string(root.join(name))
            .expect("read test file")
            .replace("\r\n", "\n")
    }

    #[test]
    fn capture_and_rollback_restore_tracked_files_and_keep_untracked() {
        if !git_available() {
            eprintln!("git unavailable; skipping checkpoint test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        write(root, "a.txt", "base\n");
        commit_all(root, "baseline");

        let checkpoint = GreenCheckpoint::discover(root).expect("repo should be discovered");
        let head = checkpoint.head_sha().unwrap();
        write(root, "a.txt", "changed\n");
        let sha = checkpoint
            .capture()
            .unwrap()
            .expect("tracked change must produce a checkpoint");
        assert!(!sha.is_empty());
        assert_ne!(sha, head);
        write(root, "untracked.txt", "keep me\n");

        checkpoint.rollback(&head, &sha).unwrap();
        // checkpoint 快照的是打点时刻（绿灯）的工作区状态，因此回滚恢复到该状态。
        assert_eq!(read(root, "a.txt"), "changed\n");
        assert_eq!(read(root, "untracked.txt"), "keep me\n");
    }

    #[test]
    fn capture_without_changes_yields_none() {
        if !git_available() {
            eprintln!("git unavailable; skipping checkpoint test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        write(root, "a.txt", "base\n");
        commit_all(root, "baseline");

        let checkpoint = GreenCheckpoint::discover(root).unwrap();
        assert_eq!(checkpoint.capture().unwrap(), None);
    }

    #[test]
    fn rollback_rejects_external_head_movement() {
        if !git_available() {
            eprintln!("git unavailable; skipping checkpoint test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        write(root, "a.txt", "base\n");
        commit_all(root, "baseline");

        let checkpoint = GreenCheckpoint::discover(root).unwrap();
        let head = checkpoint.head_sha().unwrap();
        write(root, "a.txt", "changed\n");
        let sha = checkpoint.capture().unwrap().unwrap();
        commit_all(root, "external commit");

        let error = checkpoint.rollback(&head, &sha).unwrap_err();
        assert!(error.contains("拒绝 checkpoint 回滚"));
        assert_eq!(read(root, "a.txt"), "changed\n");
    }

    #[test]
    fn discover_returns_none_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert!(GreenCheckpoint::discover(dir.path()).is_none());
    }
}
