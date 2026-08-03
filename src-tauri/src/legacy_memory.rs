//! 旧版项目记忆文件的只读状态探测。
//!
//! 本模块只检查固定路径 `.r-code/memory.md` 的文件元数据与 Git 索引状态，
//! 不打开或读取文件内容。

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use r_code_core::process::hide_background_console;
use serde::{Deserialize, Serialize};

const LEGACY_MEMORY_PATH: &str = ".r-code/memory.md";

/// 旧版记忆文件是否被 Git 跟踪。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyMemoryGitTracking {
    Tracked,
    Untracked,
    Unknown,
}

/// 旧版项目记忆文件的元数据状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyMemoryStatus {
    pub exists: bool,
    pub git_tracking: LegacyMemoryGitTracking,
}

/// 检查规范工作区根目录下固定的旧版记忆路径。
pub fn legacy_memory_status(canonical_root: &Path) -> io::Result<LegacyMemoryStatus> {
    legacy_memory_status_with_git(canonical_root, Path::new("git"))
}

fn legacy_memory_status_with_git(
    canonical_root: &Path,
    git_executable: &Path,
) -> io::Result<LegacyMemoryStatus> {
    let exists = match std::fs::symlink_metadata(canonical_root.join(LEGACY_MEMORY_PATH)) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };

    let git_tracking = git_tracking(canonical_root, git_executable);
    Ok(LegacyMemoryStatus {
        exists,
        git_tracking,
    })
}

fn git_tracking(canonical_root: &Path, git_executable: &Path) -> LegacyMemoryGitTracking {
    let mut command = Command::new(git_executable);
    command
        .arg("-C")
        .arg(canonical_root)
        .args(["ls-files", "--error-unmatch", "--", LEGACY_MEMORY_PATH])
        .stderr(Stdio::null());
    hide_background_console(&mut command);

    let Ok(output) = command.output() else {
        return LegacyMemoryGitTracking::Unknown;
    };

    if output.status.success() && tracked_stdout_matches(&output.stdout) {
        LegacyMemoryGitTracking::Tracked
    } else if output.status.code() == Some(1) && output.stdout.is_empty() {
        LegacyMemoryGitTracking::Untracked
    } else {
        LegacyMemoryGitTracking::Unknown
    }
}

fn tracked_stdout_matches(stdout: &[u8]) -> bool {
    let expected = LEGACY_MEMORY_PATH.as_bytes();
    stdout == expected
        || stdout.strip_suffix(b"\n") == Some(expected)
        || stdout.strip_suffix(b"\r\n") == Some(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Output;
    use tempfile::TempDir;

    fn git(root: &Path, args: &[&str]) -> Output {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("Git must be available for repository-bound legacy memory tests");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn init_repo(root: &Path) {
        git(root, &["init", "--quiet"]);
    }

    fn porcelain(root: &Path) -> Vec<u8> {
        git(
            root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )
        .stdout
    }

    #[test]
    fn reports_absent_untracked_tracked_non_repo_and_missing_git() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());

        assert_eq!(
            legacy_memory_status(repo.path()).unwrap(),
            LegacyMemoryStatus {
                exists: false,
                git_tracking: LegacyMemoryGitTracking::Untracked,
            }
        );

        fs::create_dir_all(repo.path().join(".r-code")).unwrap();
        fs::write(repo.path().join(".r-code/memory.md"), b"untracked sentinel").unwrap();
        fs::create_dir_all(repo.path().join("elsewhere")).unwrap();
        fs::write(repo.path().join("elsewhere/memory.md"), b"tracked decoy").unwrap();
        git(repo.path(), &["add", "--", "elsewhere/memory.md"]);
        assert_eq!(
            legacy_memory_status(repo.path()).unwrap(),
            LegacyMemoryStatus {
                exists: true,
                git_tracking: LegacyMemoryGitTracking::Untracked,
            },
            "tracking a different memory.md must not affect the fixed legacy path"
        );

        git(repo.path(), &["add", "--", ".r-code/memory.md"]);
        let tracked = legacy_memory_status(repo.path()).unwrap();
        assert_eq!(
            tracked,
            LegacyMemoryStatus {
                exists: true,
                git_tracking: LegacyMemoryGitTracking::Tracked,
            }
        );
        assert_eq!(
            serde_json::to_string(&tracked).unwrap(),
            r#"{"exists":true,"git_tracking":"tracked"}"#
        );

        let non_repo = TempDir::new().unwrap();
        assert_eq!(
            legacy_memory_status(non_repo.path()).unwrap().git_tracking,
            LegacyMemoryGitTracking::Unknown
        );

        let missing_git = repo.path().join("definitely-not-a-git-executable");
        assert_eq!(
            legacy_memory_status_with_git(repo.path(), &missing_git)
                .unwrap()
                .git_tracking,
            LegacyMemoryGitTracking::Unknown
        );
    }

    #[test]
    fn detector_preserves_user_files_and_git_status_without_leaking_content() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        fs::create_dir_all(repo.path().join(".r-code")).unwrap();

        let fixtures: [(&str, &[u8]); 3] = [
            (
                ".r-code/memory.md",
                b"MEMORY_SENTINEL_7e51f8\0\xff must never enter a response",
            ),
            (
                "AGENTS.md",
                b"AGENTS_SENTINEL_8a216d must remain byte-identical\r\n",
            ),
            (
                "CLAUDE.md",
                b"CLAUDE_SENTINEL_c39b44 must remain byte-identical\n",
            ),
        ];
        for (path, body) in fixtures {
            fs::write(repo.path().join(path), body).unwrap();
        }
        git(repo.path(), &["add", "--", ".r-code/memory.md"]);

        let before_files: Vec<_> = fixtures
            .iter()
            .map(|(path, _)| fs::read(repo.path().join(path)).unwrap())
            .collect();
        let before_status = porcelain(repo.path());

        let status = legacy_memory_status(repo.path()).unwrap();

        let after_files: Vec<_> = fixtures
            .iter()
            .map(|(path, _)| fs::read(repo.path().join(path)).unwrap())
            .collect();
        assert_eq!(after_files, before_files);
        assert_eq!(porcelain(repo.path()), before_status);

        let response = serde_json::to_string(&status).unwrap();
        assert_eq!(response, r#"{"exists":true,"git_tracking":"tracked"}"#);
        for forbidden in [
            "MEMORY_SENTINEL",
            "AGENTS_SENTINEL",
            "CLAUDE_SENTINEL",
            ".r-code/memory.md",
            "AGENTS.md",
            "CLAUDE.md",
            &repo.path().display().to_string(),
        ] {
            assert!(
                !response.contains(forbidden),
                "metadata response leaked {forbidden:?}: {response}"
            );
        }
    }

    #[test]
    fn dangling_symlink_is_detected_from_metadata_without_following_it() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        fs::create_dir_all(repo.path().join(".r-code")).unwrap();
        let link = repo.path().join(".r-code/memory.md");
        let missing_target = repo.path().join("target-that-does-not-exist");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&missing_target, &link).unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_file(&missing_target, &link) {
            if error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314)
            {
                return;
            }
            panic!("failed to create test symlink: {error}");
        }

        assert!(!missing_target.exists());
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            legacy_memory_status(repo.path()).unwrap(),
            LegacyMemoryStatus {
                exists: true,
                git_tracking: LegacyMemoryGitTracking::Untracked,
            }
        );
        assert!(!missing_target.exists());
    }
}
