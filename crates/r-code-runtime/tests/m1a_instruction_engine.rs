//! M1a-05 (FR-1.1-1.4, 1.7): the project-instruction engine core — repo-root
//! discovery (plain/linked-worktree/no-git), layer collection with the
//! fallback list, conflict/dedup rules, oversize skip, budget trim order,
//! and the JIT allowance.

use r_code_runtime::services::project_instructions::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn candidate(layer: InstructionLayer, path: &str, content: &str) -> CandidateFile {
    CandidateFile {
        layer,
        path: path.into(),
        content: content.into(),
    }
}

fn status_of(bundle: &InstructionBundle, path: &str) -> InstructionStatus {
    bundle
        .entries
        .iter()
        .find(|e| e.path == path)
        .map(|e| e.status)
        .unwrap_or_else(|| panic!("entry {path} missing"))
}

// -- repo-root discovery -------------------------------------------------

#[test]
fn discovery_finds_plain_repo_root_from_nested_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join("crates/deep")).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let discovery = discover_repo_roots(&repo.join("crates/deep"));
    assert_eq!(discovery.roots, vec![repo.clone()]);
    assert!(!discovery.no_git);
}

#[test]
fn discovery_resolves_linked_worktree_to_both_roots_nearest_first() {
    let temp = tempfile::tempdir().unwrap();
    let main = temp.path().join("main");
    let worktree = temp.path().join("wt");
    let main_git = main.join(".git");
    std::fs::create_dir_all(main_git.join("worktrees/wt-1")).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    // The .git file points at the linked worktree gitdir (relative form).
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}", main_git.join("worktrees/wt-1").display()),
    )
    .unwrap();

    let discovery = discover_repo_roots(&worktree);
    assert_eq!(discovery.roots, vec![worktree.clone(), main.clone()]);
    assert!(!discovery.no_git);
}

#[test]
fn discovery_without_git_falls_back_to_workspace_root_flagged() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("plain");
    std::fs::create_dir_all(&ws).unwrap();
    let discovery = discover_repo_roots(&ws);
    assert_eq!(discovery.roots, vec![ws.clone()]);
    assert!(discovery.no_git);
}

// -- layer collection ----------------------------------------------------

#[test]
fn frozen_collection_follows_agents_then_fallback_and_skips_local() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    // Only CLAUDE.md present: fallback applies.
    std::fs::write(repo.join("CLAUDE.md"), "fallback rules").unwrap();
    let settings = InstructionSettings::default();
    let (candidates, _) = collect_frozen_candidates(&discover_repo_roots(&repo), None, &settings);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].layer, InstructionLayer::RepoForeign);
    assert!(candidates[0].path.to_lowercase().ends_with("claude.md"));

    // AGENTS.md present: it wins over the fallback; CLAUDE.local.md never
    // eligible even if configured... (it is rejected in validate, and the
    // primary name takes precedence).
    std::fs::write(repo.join("AGENTS.md"), "primary rules").unwrap();
    std::fs::create_dir_all(repo.join(".r-code")).unwrap();
    std::fs::write(repo.join(".r-code/context.md"), "own rules").unwrap();
    let (candidates, _) = collect_frozen_candidates(&discover_repo_roots(&repo), None, &settings);
    let layers: Vec<InstructionLayer> = candidates.iter().map(|c| c.layer).collect();
    assert_eq!(
        layers,
        vec![InstructionLayer::RepoForeign, InstructionLayer::RepoOwn]
    );
    assert!(candidates[0].path.to_lowercase().ends_with("agents.md"));
}

#[test]
fn collection_matches_names_case_insensitively_and_normalizes_content() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join("agents.MD"), "a\r\nb\r\n").unwrap();
    let (candidates, _) = collect_frozen_candidates(
        &discover_repo_roots(&repo),
        None,
        &InstructionSettings::default(),
    );
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].content, "a\nb\n", "CRLF folded to LF");
    let mut raw_bom = b"\xEF\xBB\xBFblocked".to_vec();
    raw_bom.extend_from_slice(b"\n");
    std::fs::write(repo.join("CLAUDE.MD"), &raw_bom).unwrap();
    let normalized = normalize_content(&raw_bom);
    assert!(normalized.starts_with("blocked"), "BOM stripped");
}

#[test]
fn settings_validation_rejects_local_and_out_of_range() {
    let local = InstructionSettings {
        fallback_names: vec!["CLAUDE.local.md".into()],
        ..InstructionSettings::default()
    };
    assert!(local.validate().is_err());
    let gemini = InstructionSettings {
        fallback_names: vec!["GEMINI.md".into()],
        ..InstructionSettings::default()
    };
    assert!(gemini.validate().is_ok());
    let tiny_budget = InstructionSettings {
        total_budget_bytes: 4 * 1024,
        ..InstructionSettings::default()
    };
    assert!(tiny_budget.validate().is_err());
    let huge_jit = InstructionSettings {
        jit_allowance_bytes: 64 * 1024,
        ..InstructionSettings::default()
    };
    assert!(huge_jit.validate().is_err());
}

// -- planning: dedup, conflicts, budget -----------------------------------

#[test]
fn equivalent_entries_dedup_keeping_the_later_layer() {
    let settings = InstructionSettings::default();
    let candidates = vec![
        candidate(InstructionLayer::RepoForeign, "repo/AGENTS.md", "same body"),
        candidate(
            InstructionLayer::RepoOwn,
            "repo/.r-code/context.md",
            "same body",
        ),
        candidate(InstructionLayer::Global, "home/context.md", "global body"),
    ];
    let bundle = plan_bundle(&candidates, &[], &settings);
    assert_eq!(
        status_of(&bundle, "repo/AGENTS.md"),
        InstructionStatus::DroppedBudget
    );
    assert_eq!(
        status_of(&bundle, "repo/.r-code/context.md"),
        InstructionStatus::Injected
    );
    assert_eq!(
        status_of(&bundle, "home/context.md"),
        InstructionStatus::Injected
    );
    // Rendered carries the surviving instance exactly once.
    assert_eq!(bundle.rendered.matches("same body").count(), 1);
}

#[test]
fn oversize_files_are_skipped_with_a_label() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let big = vec![b'x'; MAX_INSTRUCTION_FILE_BYTES + 1];
    std::fs::write(repo.join("AGENTS.md"), &big).unwrap();
    let (candidates, skipped) = collect_frozen_candidates(
        &discover_repo_roots(&repo),
        None,
        &InstructionSettings::default(),
    );
    assert!(candidates.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].status, InstructionStatus::SkippedOversize);
    let bundle = plan_bundle(&candidates, &skipped, &InstructionSettings::default());
    assert_eq!(
        status_of(&bundle, skipped[0].path.as_str()),
        InstructionStatus::SkippedOversize
    );
}

#[test]
fn budget_overflow_trims_global_first_and_keeps_higher_layers() {
    let settings = InstructionSettings {
        total_budget_bytes: 200,
        ..InstructionSettings::default()
    };
    let body = 120;
    let candidates = vec![
        candidate(
            InstructionLayer::Global,
            "home/context.md",
            &"g".repeat(body),
        ),
        candidate(
            InstructionLayer::RepoForeign,
            "repo/AGENTS.md",
            &"f".repeat(body),
        ),
        candidate(
            InstructionLayer::RepoOwn,
            "repo/.r-code/context.md",
            &"o".repeat(body),
        ),
    ];
    let bundle = plan_bundle(&candidates, &[], &settings);
    // 3 x 120 > 200: the global layer sheds first, then foreign if needed.
    assert_eq!(
        status_of(&bundle, "home/context.md"),
        InstructionStatus::Trimmed
    );
    let injected_total: usize = bundle
        .entries
        .iter()
        .filter(|e| e.status == InstructionStatus::Injected)
        .map(|e| e.bytes)
        .sum();
    assert!(injected_total <= settings.total_budget_bytes);
}

#[test]
fn disabled_injection_drops_everything_with_the_disabled_label() {
    let settings = InstructionSettings {
        injection_enabled: false,
        ..InstructionSettings::default()
    };
    let candidates = vec![candidate(
        InstructionLayer::RepoForeign,
        "repo/AGENTS.md",
        "rules",
    )];
    let bundle = plan_bundle(&candidates, &[], &settings);
    assert_eq!(
        status_of(&bundle, "repo/AGENTS.md"),
        InstructionStatus::DroppedDisabled
    );
    assert!(bundle.rendered.is_empty());
}

// -- JIT ------------------------------------------------------------------

#[test]
fn jit_collects_ancestor_chain_once_and_dedups_against_frozen() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let nested = repo.join("crates/deep");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(repo.join("AGENTS.md"), "root rules").unwrap();
    std::fs::write(nested.join("AGENTS.md"), "deep rules").unwrap();

    let mut injected_paths = BTreeSet::new();
    injected_paths.insert(repo.join("AGENTS.md").to_string_lossy().to_string());
    let mut injected_hashes = BTreeSet::new();
    injected_hashes.insert(content_hash_of("root rules"));

    let candidates = collect_jit_candidates(
        std::slice::from_ref(&nested),
        &injected_paths,
        &injected_hashes,
        std::slice::from_ref(&repo),
    );
    assert_eq!(candidates.len(), 1, "root AGENTS.md already injected");
    assert!(
        candidates[0].path.ends_with("deep\\AGENTS.md")
            || candidates[0].path.ends_with("deep/AGENTS.md")
    );
    assert_eq!(candidates[0].content, "deep rules");
}

fn content_hash_of(content: &str) -> String {
    // Mirror the planner's hashing through a tiny round-trip: plan a
    // single-entry bundle and read its entry hash.
    let bundle = plan_bundle(
        &[candidate(InstructionLayer::RepoForeign, "x", content)],
        &[],
        &InstructionSettings::default(),
    );
    bundle.entries[0].sha256.clone()
}

#[test]
fn jit_over_allowance_abandons_the_whole_batch() {
    let settings = InstructionSettings {
        jit_allowance_bytes: 50,
        ..InstructionSettings::default()
    };
    let candidates = vec![candidate(
        InstructionLayer::Subdir,
        "repo/sub/AGENTS.md",
        &"y".repeat(80),
    )];
    let bundle = plan_jit_bundle(&candidates, &settings);
    assert!(bundle.rendered.is_empty());
    assert_eq!(
        status_of(&bundle, "repo/sub/AGENTS.md"),
        InstructionStatus::DroppedBudget
    );
}

#[test]
fn jit_within_allowance_injects_the_batch() {
    let settings = InstructionSettings::default();
    let candidates = vec![candidate(
        InstructionLayer::Subdir,
        "repo/sub/AGENTS.md",
        "small rules",
    )];
    let bundle = plan_jit_bundle(&candidates, &settings);
    assert!(bundle.rendered.contains("small rules"));
    assert_eq!(
        status_of(&bundle, "repo/sub/AGENTS.md"),
        InstructionStatus::Injected
    );
}

// -- identity --------------------------------------------------------------

#[test]
fn bundle_digest_is_order_independent_and_content_addressed() {
    let settings = InstructionSettings::default();
    let a = plan_bundle(
        &[
            candidate(InstructionLayer::RepoForeign, "repo/AGENTS.md", "one"),
            candidate(InstructionLayer::RepoOwn, "repo/.r-code/context.md", "two"),
        ],
        &[],
        &settings,
    );
    let b = plan_bundle(
        &[
            candidate(InstructionLayer::RepoOwn, "repo/.r-code/context.md", "two"),
            candidate(InstructionLayer::RepoForeign, "repo/AGENTS.md", "one"),
        ],
        &[],
        &settings,
    );
    assert_eq!(
        a.digest, b.digest,
        "layer order fixes identity, input order does not"
    );
    let c = plan_bundle(
        &[candidate(
            InstructionLayer::RepoForeign,
            "repo/AGENTS.md",
            "changed",
        )],
        &[],
        &settings,
    );
    assert_ne!(a.digest, c.digest);
}

#[test]
fn renderer_carries_the_untrusted_preamble_and_layer_labels() {
    let settings = InstructionSettings::default();
    let bundle = plan_bundle(
        &[candidate(
            InstructionLayer::RepoOwn,
            "repo/.r-code/context.md",
            "own body",
        )],
        &[],
        &settings,
    );
    assert!(bundle.rendered.contains("project documentation"));
    assert!(bundle.rendered.contains("[repo-own]"));
    assert!(bundle.rendered.contains("own body"));
    let _: Option<PathBuf> = None; // keep PathBuf import meaningful on all platforms
    let _: Option<&Path> = None;
}
