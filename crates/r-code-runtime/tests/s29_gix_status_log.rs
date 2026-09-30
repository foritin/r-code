//! S29 — the restricted gix status/log reader.
//!
//! Proves the P29 acceptance on real repositories: only the canonical
//! `.git` directory opens (no discovery), status/log parity against a real
//! `git` oracle run on a separate copy with GIT_OPTIONAL_LOCKS=0, a
//! malicious process filter configured in the repository's OWN config is
//! never executed by our reader (with a control arm proving real git does
//! execute it), reads leave the live `.git` metadata byte-identical, and a
//! hostile environment cannot inject configuration.

use r_code_runtime::services::git_read::{
    open_read_only, ChangeKind, GitReadError, Stage, MAX_LOG_COMMITS,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=s29@test.local", "-c", "user.name=s29"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {} {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A fixture repository: two commits, one staged modification, one
/// unstaged modification, one untracked file — and optionally a malicious
/// process filter configured in the repository's OWN local config.
fn fixture_repo(temp: &Path, name: &str, malicious_filter: bool) -> PathBuf {
    let repo = temp.join(name);
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-q", "--initial-branch=main"]);
    std::fs::write(repo.join("committed.txt"), "first\n").expect("write");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    std::fs::write(repo.join("second.txt"), "second\n").expect("write");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "second"]);
    // Staged modification (HEAD -> index).
    std::fs::write(repo.join("committed.txt"), "staged-change\n").expect("write");
    git(&repo, &["add", "committed.txt"]);
    // Unstaged modification (index -> worktree).
    std::fs::write(repo.join("second.txt"), "worktree-change\n").expect("write");
    // Untracked file.
    std::fs::write(repo.join("untracked.txt"), "untracked\n").expect("write");
    if malicious_filter {
        std::fs::write(repo.join("payload.data"), "payload\n").expect("write");
        git(&repo, &["add", "payload.data"]);
        std::fs::write(repo.join("payload.data"), "payload-modified\n").expect("write");
        std::fs::write(repo.join(".gitattributes"), "*.data filter=evil\n").expect("attributes");
        git(&repo, &["add", ".gitattributes"]);
        let marker = temp.join(format!("{name}-pwned-marker.txt"));
        let _ = std::fs::remove_file(&marker);
        // Forward slashes: the filter command runs through git's bundled sh,
        // which eats backslashes (the probe round proved a backslash path
        // becomes a mangled untracked file inside the repo instead).
        let marker_unix = marker.to_string_lossy().replace('\\', "/");
        let command = if cfg!(windows) {
            format!("cmd /c echo pwned> {marker_unix}")
        } else {
            format!("echo pwned > {marker_unix}")
        };
        git(
            &repo,
            &["config", "--local", "filter.evil.clean", command.as_str()],
        );
        git(
            &repo,
            &["config", "--local", "filter.evil.smudge", command.as_str()],
        );
        git(
            &repo,
            &["config", "--local", "filter.evil.required", "true"],
        );
    }
    repo
}

fn git_dir(repo: &Path) -> PathBuf {
    repo.join(".git")
}

/// Recursive copy of a directory tree (the oracle fixture copy).
fn copy_dir(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("copy dir");
    for entry in std::fs::read_dir(source).expect("read dir") {
        let entry = entry.expect("entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

/// Fingerprint the live git metadata: every file under `.git` with size
/// and mtime. Our reads must leave this map byte-identical.
fn fingerprint_git_dir(git_dir: &Path) -> BTreeMap<String, (u64, std::time::SystemTime)> {
    let mut map = BTreeMap::new();
    fn walk(dir: &Path, prefix: &str, map: &mut BTreeMap<String, (u64, std::time::SystemTime)>) {
        for entry in std::fs::read_dir(dir).expect("read git dir") {
            let entry = entry.expect("entry");
            let relative = format!("{prefix}/{}", entry.file_name().to_string_lossy());
            if entry.file_type().expect("type").is_dir() {
                walk(&entry.path(), &relative, map);
            } else {
                let metadata = entry.metadata().expect("metadata");
                map.insert(
                    relative,
                    (metadata.len(), metadata.modified().expect("modified")),
                );
            }
        }
    }
    walk(git_dir, "", &mut map);
    map
}

/// Normalize one porcelain line to (path, stage, kind) so gix and git
/// compare as sets, not strings.
fn porcelain_to_tuples(porcelain: &str) -> BTreeMap<(String, &'static str), &'static str> {
    let mut tuples = BTreeMap::new();
    for line in porcelain.lines() {
        if line.len() < 4 {
            continue;
        }
        let xy: Vec<char> = line.chars().take(2).collect();
        let path: String = line[3..].trim().to_string();
        for (index, letter, stage) in [(0, xy[0], "head-to-index"), (1, xy[1], "index-to-worktree")]
        {
            // `??` is a single untracked fact in the worktree column only —
            // porcelain duplicates it across both columns.
            if letter == '?' && index == 0 {
                continue;
            }
            let kind = match letter {
                'M' | 'T' | 'U' | 'A' if stage == "head-to-index" => "modified-or-added",
                'D' if stage == "head-to-index" => "deleted",
                'M' | 'T' | 'U' if stage == "index-to-worktree" => "modified",
                'D' if stage == "index-to-worktree" => "deleted",
                '?' => "untracked",
                _ => continue,
            };
            tuples.insert((path.clone(), stage), kind);
        }
    }
    tuples
}

/// Normalize our reader's report the same way.
fn report_to_tuples(
    entries: &[r_code_runtime::services::git_read::StatusEntry],
) -> BTreeMap<(String, &'static str), &'static str> {
    let mut tuples = BTreeMap::new();
    for entry in entries {
        let stage = match entry.stage {
            Stage::HeadToIndex => "head-to-index",
            Stage::IndexToWorktree => "index-to-worktree",
        };
        let kind = match (entry.change, entry.stage) {
            (ChangeKind::Added, Stage::HeadToIndex) => "modified-or-added",
            (ChangeKind::Modified, Stage::HeadToIndex) => "modified-or-added",
            (ChangeKind::Deleted, _) => "deleted",
            (ChangeKind::Modified, Stage::IndexToWorktree) => "modified",
            (ChangeKind::Added, Stage::IndexToWorktree) => "modified",
            (ChangeKind::Renamed, _) => continue,
            (ChangeKind::Untracked, _) => "untracked",
        };
        tuples.insert((entry.path.clone(), stage), kind);
    }
    tuples
}

#[test]
fn opens_only_the_canonical_git_dir_and_serves_plain_data() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = fixture_repo(temp.path(), "open", false);

    let reader = open_read_only(&git_dir(&repo)).expect("the canonical git dir opens");
    let report = reader.status(100).expect("status");
    assert!(!report.entries.is_empty());
    assert!(!report.truncated);
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.change == ChangeKind::Untracked),
        "untracked files are reported: {:?}",
        report.entries
    );

    // No discovery: the worktree root and a foreign directory both refuse.
    assert!(matches!(
        open_read_only(&repo),
        Err(GitReadError::Open(_, _))
    ));
    assert!(matches!(
        open_read_only(&temp.path().join("not-a-repo")),
        Err(GitReadError::Open(_, _))
    ));

    // Bounded: a limit below the entry count truncates; zero refuses.
    let bounded = reader.status(1).expect("bounded status");
    assert_eq!(bounded.entries.len(), 1);
    assert!(bounded.truncated);
    assert_eq!(reader.status(0).unwrap_err(), GitReadError::ZeroBound);

    let log = reader.log(MAX_LOG_COMMITS).expect("log");
    assert_eq!(log.commits.len(), 2, "two commits in the fixture");
    assert!(!log.truncated);
    assert_eq!(reader.log(0).unwrap_err(), GitReadError::ZeroBound);
}

#[test]
fn status_and_log_parity_against_the_git_oracle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = fixture_repo(temp.path(), "parity", false);
    let oracle = temp.path().join("oracle-copy");
    copy_dir(&repo, &oracle);

    // The oracle: real git on the separate copy, optional locks off.
    let porcelain = git_output(&oracle, &["status", "--porcelain"]);
    let oracle_tuples = porcelain_to_tuples(&porcelain);
    assert!(oracle_tuples.len() >= 3, "fixture has changes: {porcelain}");

    let reader = open_read_only(&git_dir(&repo)).expect("open");
    let report = reader.status(1_000).expect("status");
    let ours = report_to_tuples(&report.entries);
    assert_eq!(
        ours, oracle_tuples,
        "our status must match the git oracle exactly"
    );

    let oracle_log: Vec<String> = git_output(&oracle, &["log", "--format=%H"])
        .lines()
        .map(str::to_string)
        .collect();
    let mut our_log = reader.log(1_000).expect("log").commits;
    our_log.sort();
    let mut oracle_sorted = oracle_log;
    oracle_sorted.sort();
    assert_eq!(our_log, oracle_sorted, "log ids must match the oracle");
}

#[test]
fn malicious_process_filter_is_never_executed_by_the_reader() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = fixture_repo(temp.path(), "malicious", true);
    let marker = temp.path().join("malicious-pwned-marker.txt");

    // Control arm: real git DOES execute the locally-configured filter —
    // proving the fixture is genuinely malicious.
    let control = temp.path().join("control-copy");
    copy_dir(&repo, &control);
    let control_marker = temp.path().join("control-pwned-marker.txt");
    let _ = std::fs::remove_file(&control_marker);
    let control_command = if cfg!(windows) {
        format!(
            "cmd /c echo pwned> {}",
            control_marker.to_string_lossy().replace('\\', "/")
        )
    } else {
        format!(
            "echo pwned > {}",
            control_marker.to_string_lossy().replace('\\', "/")
        )
    };
    git(
        &control,
        &[
            "config",
            "--local",
            "filter.evil.clean",
            control_command.as_str(),
        ],
    );
    git(&control, &["add", "payload.data"]);
    assert!(
        control_marker.is_file(),
        "control arm: real git executed the malicious filter"
    );

    // Our reader: status reads the modified filtered file, but the driver
    // is untrusted, so nothing ever spawns.
    let reader = open_read_only(&git_dir(&repo)).expect("open");
    let report = reader.status(1_000).expect("status");
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.path == "payload.data"),
        "the filtered file is still diffed: {:?}",
        report.entries
    );
    assert!(
        !marker.is_file(),
        "the malicious process filter must never execute through the reader"
    );
    let _ = reader.log(100).expect("log");
    assert!(!marker.is_file(), "log must not execute filters either");
}

#[test]
fn reads_leave_the_live_git_metadata_byte_identical() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = fixture_repo(temp.path(), "metadata", true);
    let reader = open_read_only(&git_dir(&repo)).expect("open");

    let before = fingerprint_git_dir(&git_dir(&repo));
    let status = reader.status(1_000).expect("status");
    let log = reader.log(100).expect("log");
    assert!(!status.entries.is_empty());
    assert!(!log.commits.is_empty());
    let after = fingerprint_git_dir(&git_dir(&repo));
    assert_eq!(
        before, after,
        "our reads must make zero metadata changes (no index write-back, no locks)"
    );
}

#[test]
fn hostile_environment_cannot_inject_configuration() {
    // The hostile environment lives in a RE-EXECUTED CHILD of this very
    // test binary: mutating the process environment in place would leak
    // GIT_CONFIG_* into sibling tests running in parallel (their `git`
    // oracle calls read it too — the first full-suite run caught exactly
    // that race).
    let child_repo = std::env::var("R29_HOSTILE_CHILD_REPO").ok();
    if let Some(repo) = child_repo {
        let report = open_read_only(std::path::Path::new(&repo))
            .expect("open under hostile env")
            .status(1_000)
            .expect("status under hostile env");
        for entry in &report.entries {
            println!(
                "ENTRY {:?}|{:?}|{:?}",
                entry.path, entry.change, entry.stage
            );
        }
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let repo = fixture_repo(temp.path(), "hostile", false);
    let reader = open_read_only(&git_dir(&repo)).expect("open");
    let clean = reader.status(1_000).expect("status without hostile env");
    assert!(!clean.entries.is_empty());

    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "hostile_environment_cannot_inject_configuration",
            "--nocapture",
        ])
        .env("R29_HOSTILE_CHILD_REPO", git_dir(&repo))
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "filter.evil.clean")
        .env("GIT_CONFIG_VALUE_0", "definitely-a-command")
        .env("GIT_CONFIG_KEY_1", "status.showUntrackedFiles")
        .env("GIT_CONFIG_VALUE_1", "no")
        .output()
        .expect("re-execute the hostile child");
    assert!(
        output.status.success(),
        "hostile child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut hostile_lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("ENTRY "))
        .collect();
    hostile_lines.sort_unstable();
    let mut clean_lines: Vec<String> = clean
        .entries
        .iter()
        .map(|entry| {
            format!(
                "ENTRY {:?}|{:?}|{:?}",
                entry.path, entry.change, entry.stage
            )
        })
        .collect();
    clean_lines.sort_unstable();
    let hostile_owned: Vec<String> = hostile_lines.iter().map(|line| line.to_string()).collect();
    assert_eq!(
        clean_lines, hostile_owned,
        "environment-injected configuration must not change what the reader sees"
    );
}

/// The repository object is not plugin-visible: the crate root never
/// re-exports the reader or any gix type, and the module lives under
/// `services` (internal composition), not the plugin surface.
#[test]
fn the_repository_object_is_not_plugin_visible() {
    let lib = include_str!("../src/lib.rs");
    assert!(
        !lib.contains("pub use gix") && !lib.contains("pub use crate::services::git_read"),
        "the crate root must never re-export the reader or gix types"
    );
    let services = include_str!("../src/services/mod.rs");
    assert!(services.contains("pub mod git_read;"));
    let router = include_str!("../src/plugins/router.rs");
    assert!(
        !router.contains("git_read"),
        "the plugin router must not surface the git reader"
    );
}
