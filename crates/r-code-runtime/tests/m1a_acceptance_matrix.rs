//! M1a acceptance matrix (M1a-12): each PRD acceptance letter maps to the
//! test that pins it. This file adds the two letters not covered elsewhere
//! (FR-1 b conflict ordering, FR-1 e trim order end-to-end composition) and
//! documents the mapping.
//!
//! FR-1 (PRD §4):
//!   a) /context shows entries/bytes/sources + skipped/trimmed labels + memory summary
//!      → m1a_instruction_freeze (snapshot/ledger/event), m1a_context_ui (frontend rows),
//!        tui session_ops::context_view_tests, context_view (TaskMemoryView projection)
//!   b) conflicts resolve to .r-code/ → THIS FILE (order pin) + engine dedup tests
//!   c) JIT injects with a timeline event + token accounting
//!      → m1a_jit_injection (e2e: request-visible, journal-clean, context.jit event,
//!        jit ledger row; token accounting rides model.usage which the JIT block joins)
//!   d) toggle off → next run injects nothing
//!      → m1a_instruction_freeze::context_settings_persist_and_the_toggle_reaches_the_engine
//!        + empty-set identity test (settings read at freeze time, per run)
//!   e) 32 KiB trim order global→foreign→own; JIT own allowance
//!      → m1a_instruction_engine::budget_overflow_trims_global_first... + THIS FILE
//!   f) JIT over allowance abandons the batch with a trace
//!      → m1a_instruction_engine + m1a_jit_injection (tracker + audit reason)
//!
//! FR-7 (PRD §4):
//!   a) PromptSnapshot carries the memory segment; ledger row exists
//!      → m1a_memory_injection (builder + e2e conversation run)
//!   b) same snapshot hash for the main agent and its children
//!      → m1a_children_executor (child contract + child-run ledger row same hash)
//!   c) memory off → no injection → m1a_memory_modes (desktop) + engine disabled label
//!   d) Codex delegation prompt carries the segment → task.detail projection
//!      (m1a_codex_memory) + codex_delegation_memory_context wiring (build arm pre-pinned)
//!
//! FR-8 step one (PRD §4, M1a slice):
//!   a) spawn read-only scout, receive report → m1a_children_executor e2e #1
//!   b) whitelist + caller=subagent audit → structural catalog fence (e2e #1 group 4)
//!      + gateway house tests (subagent gate family)
//!   c) 7th spawn queues, runs after a close frees the slot
//!      → m1a_children_executor e2e #2 + kernel gate tests
//!   d) nested spawn refused → kernel NestingLimit test + structural absence
//!   e) wait never busy-polls → kernel condvar test (m1a_children_gates #4)
//!   g) catalog digest carries the tools (children tools in parent catalog,
//!      absent in child) → e2e #1 group 4 + house digest determinism

use r_code_runtime::services::project_instructions::*;

fn candidate(layer: InstructionLayer, path: &str, content: &str) -> CandidateFile {
    CandidateFile {
        layer,
        path: path.into(),
        content: content.into(),
    }
}

/// FR-1 b: different-content conflicts keep BOTH texts but order the
/// repo-own layer AFTER the foreign file — the later block is the one a
/// model reads as current ("以 .r-code/ 为准" via 拼接顺序).
#[test]
fn fr1_b_own_layer_orders_after_foreign_on_content_conflicts() {
    let bundle = plan_bundle(
        &[
            candidate(
                InstructionLayer::RepoForeign,
                "repo/AGENTS.md",
                "use tabs for yaml",
            ),
            candidate(
                InstructionLayer::RepoOwn,
                "repo/.r-code/context.md",
                "use spaces for yaml",
            ),
        ],
        &[],
        &InstructionSettings::default(),
    );
    let tabs = bundle.rendered.find("use tabs").expect("foreign present");
    let spaces = bundle.rendered.find("use spaces").expect("own present");
    assert!(
        spaces > tabs,
        "the .r-code/ block must compose after the foreign block (later wins)"
    );
    // The untrusted preamble precedes both.
    let preamble = bundle.rendered.find("project documentation").unwrap();
    assert!(preamble < tabs);
}

/// FR-1 e: the full frozen stack composes global → foreign → own and the
/// JIT allowance sits on top of the same budget family.
#[test]
fn fr1_e_frozen_stack_orders_global_foreign_own() {
    let bundle = plan_bundle(
        &[
            candidate(InstructionLayer::RepoOwn, "repo/.r-code/context.md", "OWN"),
            candidate(InstructionLayer::RepoForeign, "repo/AGENTS.md", "FOREIGN"),
            candidate(InstructionLayer::Global, "home/context.md", "GLOBAL"),
        ],
        &[],
        &InstructionSettings::default(),
    );
    let global = bundle.rendered.find("GLOBAL").unwrap();
    let foreign = bundle.rendered.find("FOREIGN").unwrap();
    let own = bundle.rendered.find("OWN").unwrap();
    assert!(global < foreign && foreign < own);
}
