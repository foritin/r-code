//! T39 — plugin conformance and failure suite.
//!
//! A deliberately broken Harness fails for false completion, stale
//! evidence, cross-run handles, duplicate side effects or hanging
//! cancellation.

#[tokio::test]
async fn broken_harness_behaviours_fail_the_conformance_suite() {
    let results = r_code_evals::harness_conformance::run_conformance_suite().await;
    assert!(!results.is_empty());
    for result in &results {
        assert!(
            result.passed,
            "conformance check {} failed: {}",
            result.name, result.detail
        );
    }
    // The suite covers exactly the broken behaviours from the acceptance.
    let names: Vec<&str> = results.iter().map(|result| result.name).collect();
    assert!(names.contains(&"false-completion-refused"));
    assert!(names.contains(&"stale-evidence-refused"));
    assert!(names.contains(&"cross-run-handle-refused"));
    assert!(names.contains(&"duplicate-side-effect-replayed"));
    assert!(names.contains(&"hanging-cancellation-fenced"));
}
