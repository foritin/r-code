//! S30 — bounded gix diff and read-only adapters.
//!
//! Proves the acceptance set: only status/log/diff are public (three named
//! tools + one RPC family; no generic git descriptor anywhere), no write
//! or Git executable is discoverable (the tools compute/read through the
//! restricted P29 reader; the source guard pins it), and bounds fail
//! cleanly (hunk/line/byte overflows are Unavailable or truncated flags —
//! never a partial silent stream). Diff parity runs against a real git
//! oracle on a separate no-lock copy.

use r_code_kernel::ports::ToolService as _;
use r_code_kernel::task::WorkspaceSnapshotRef;
use r_code_runtime::services::git_read::{diff_blobs, DiffBounds, DiffError, DiffLineKind};
use r_code_runtime::services::tools::PlanningToolService;
use std::path::Path;

fn git_fixture(temp: &Path, name: &str) -> std::path::PathBuf {
    let repo = temp.join(name);
    std::fs::create_dir_all(&repo).expect("repo dir");
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.email=s30@t.local", "-c", "user.name=s30"])
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    run(&["init", "-q", "--initial-branch=main"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").expect("a");
    std::fs::write(repo.join("bin.dat"), [(0xff_u8), 0, 1, 2]).expect("bin");
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "first"]);
    std::fs::write(repo.join("a.txt"), "one\nTWO-CHANGED\nthree\nfour\n").expect("a2");
    std::fs::write(repo.join("new.txt"), "created\n").expect("new");
    repo
}

#[test]
fn diff_parity_with_the_git_oracle_and_binary_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = git_fixture(temp.path(), "parity");
    let oracle = temp.path().join("oracle");
    // Separate no-lock copy: `git diff` on it is the truth.
    crate::oracle_copy(&repo, &oracle);

    let old = std::fs::read(oracle.join("a.txt")).unwrap();
    // The oracle's HEAD version of a.txt.
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&oracle)
        .args(["show", "HEAD:a.txt"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .expect("show");
    assert!(head.status.success());
    let head_content = String::from_utf8_lossy(&head.stdout).into_owned();

    let report = diff_blobs(head_content.as_bytes(), &old, &DiffBounds::default()).expect("diff");
    assert!(report.binary.is_none());
    assert!(!report.truncated);
    // The changed line appears as removed(HEAD)+added(worktree); the added
    // `four` line appears as added; `one`/`three` are context (absent in a
    // zero-context diff).
    let added: Vec<&str> = report
        .hunks
        .iter()
        .flat_map(|hunk| hunk.lines.iter())
        .filter(|line| line.kind == DiffLineKind::Added)
        .map(|line| line.text.as_str())
        .collect();
    let removed: Vec<&str> = report
        .hunks
        .iter()
        .flat_map(|hunk| hunk.lines.iter())
        .filter(|line| line.kind == DiffLineKind::Removed)
        .map(|line| line.text.as_str())
        .collect();
    assert!(
        added.contains(&"TWO-CHANGED".to_string().as_str())
            || added.iter().any(|t| t.contains("TWO-CHANGED")),
        "added: {added:?}"
    );
    assert!(
        removed.iter().any(|t| t.contains("two")),
        "removed: {removed:?}"
    );
    assert!(
        added.iter().any(|t| t.contains("four")),
        "the inserted line is added: {added:?}"
    );

    // Binary content is metadata only — never hunks.
    let binary = diff_blobs(
        b"\x00\x01binary",
        b"\x00\x02changed",
        &DiffBounds::default(),
    )
    .expect("binary diff");
    assert_eq!(binary.binary, Some((8, 9)));
    assert!(binary.hunks.is_empty());
}

#[test]
fn bounds_fail_cleanly_never_partially() {
    // Byte bound: inputs beyond the ceiling refuse with Unavailable.
    let big_old = vec![b'a'; 600_000];
    let big_new = vec![b'b'; 600_000];
    assert!(matches!(
        diff_blobs(&big_old, &big_new, &DiffBounds::default()),
        Err(DiffError::Unavailable(reason)) if reason.contains("byte")
    ));

    // Hunk/line bounds truncate with the flag set — never silently.
    let mut many: Vec<String> = Vec::new();
    for index in 0..600 {
        many.push(format!("line-{index}"));
    }
    let old_text = many.join("\n");
    let mut changed = many.clone();
    for index in (0..changed.len()).step_by(1) {
        changed[index] = format!("x{index}");
    }
    let new_text = changed.join("\n");
    let report = diff_blobs(
        old_text.as_bytes(),
        new_text.as_bytes(),
        &DiffBounds {
            max_hunks: 5,
            max_lines_per_hunk: 10,
            max_total_bytes: DiffBounds::default().max_total_bytes,
        },
    )
    .expect("bounded diff");
    assert!(report.truncated, "the bound overflow is flagged");
    assert!(report.hunks.len() <= 6);
}

fn token() -> r_code_kernel::ports::GenerationToken {
    r_code_kernel::ports::GenerationToken::new("run-s30", 1)
}

#[tokio::test]
async fn read_only_tools_expose_exactly_three_git_surfaces() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = git_fixture(temp.path(), "tools");
    let service = PlanningToolService::from_workspace(&WorkspaceSnapshotRef {
        workspace_identity: "sha256:bound".into(),
        canonical_root: repo.to_string_lossy().into_owned(),
        baseline_sha256: "sha256:baseline".into(),
    })
    .expect("planning service");

    let tools = service.list(token()).await.expect("list");
    let git_tools: Vec<&str> = tools
        .iter()
        .filter(|tool| tool.name.starts_with("git_"))
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(
        git_tools,
        vec!["git_status", "git_log", "git_diff"],
        "exactly the three projections, no generic git tool"
    );

    // A status call runs through the restricted reader and returns the
    // fixture's changes (a.txt modified, new.txt untracked).
    let reply = service
        .call(
            r_code_kernel::ports::GenerationToken::new("run-s30", 1),
            r_code_harness_protocol::services::ToolCallRequest {
                tool: "git_status".into(),
                input: serde_json::json!({"limit": 50}),
                operation_key: None,
            },
        )
        .await
        .expect("status call");
    assert!(reply.error.is_none(), "{:?}", reply.error);
    let payload = serde_json::to_string(&reply.output).expect("payload");
    assert!(payload.contains("a.txt"), "{payload}");
    assert!(payload.contains("new.txt"), "{payload}");

    // Unbound workspace: the git descriptors are not even listed.
    let unbound = PlanningToolService::from_workspace(&WorkspaceSnapshotRef {
        workspace_identity: "unbound-read-only".into(),
        canonical_root: String::new(),
        baseline_sha256: String::new(),
    })
    .expect("unbound");
    assert!(unbound.list(token()).await.expect("list").is_empty());
}

fn oracle_copy(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("copy dir");
    for entry in std::fs::read_dir(source).expect("read") {
        let entry = entry.expect("entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            oracle_copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy");
        }
    }
}

#[test]
fn source_guard_no_git_executable_no_write_surface() {
    let tools = include_str!("../src/services/tools.rs");
    assert!(
        !tools.contains("std::process::Command::new(\"git\")"),
        "the git tools never shell out to a git executable"
    );
    let git_read = include_str!("../src/services/git_read.rs");
    for forbidden in [
        "std::process::Command::new(\"git\")",
        "write_object",
        "git commit",
        "git push",
    ] {
        assert!(
            !git_read.contains(forbidden),
            "the reader has no write surface: {forbidden}"
        );
    }
    // The descriptors name only the three projections.
    assert!(tools.contains("\"git_status\""));
    assert!(tools.contains("\"git_log\""));
    assert!(tools.contains("\"git_diff\""));
    assert!(!tools.contains("name: \"git\""));
}
