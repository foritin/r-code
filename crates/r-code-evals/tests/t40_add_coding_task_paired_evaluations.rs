//! T40 — coding-task paired evaluations.
//!
//! Repeated paired runs keep model/config/task fixed and separate host
//! faults/unknown external metrics from successful outcomes.

use r_code_evals::harness_tasks::{paired_report, standard_fixtures};

#[tokio::test]
async fn paired_runs_separate_faults_from_outcomes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let binary = r_code_evals::harness_tasks::repair_harness_binary();

    // Stage the package.
    let source = temp.path().join("pkg");
    std::fs::create_dir_all(source.join("bin")).expect("dirs");
    std::fs::copy(
        &binary,
        source.join("bin").join(binary.file_name().unwrap()),
    )
    .expect("copy");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    let executable = binary.file_name().unwrap().to_string_lossy();
    let executable = if cfg!(windows) {
        executable.trim_end_matches(".exe")
    } else {
        executable.as_ref()
    };
    std::fs::write(
        source.join("harness.json"),
        serde_json::json!({
            "schema_version": "1", "id": "repair-harness.example", "version": "1.0.0",
            "apiMajor": 1, "apiMinor": 0, "displayName": "repair",
            "supportedPlatforms": [{"platform": platform, "executable": format!("bin/{executable}")}],
            "requestedHostServices": ["host.model.stream", "host.tools.list", "host.tools.call",
                "host.checkpoint.save", "host.completion.propose"],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");

    // Repeated paired runs: same harness/model/config/task both sides.
    let fixtures = standard_fixtures();
    for fixture in &fixtures {
        let baseline = r_code_evals::harness_tasks::run_fixture(
            temp.path(),
            "baseline",
            fixture,
            &source,
            "repair-harness.example",
        )
        .await;
        let candidate = r_code_evals::harness_tasks::run_fixture(
            temp.path(),
            "candidate",
            fixture,
            &source,
            "repair-harness.example",
        )
        .await;
        // Neither side fabricated a false completion.
        assert!(!baseline.false_completion && !candidate.false_completion);
        // Host faults are separated from outcomes (none expected here).
        assert!(
            baseline.host_fault.is_none(),
            "baseline fault: {:?}",
            baseline.host_fault
        );
        assert!(
            candidate.host_fault.is_none(),
            "candidate fault: {:?}",
            candidate.host_fault
        );
        // Unavailable metrics stay unavailable (cost has no fact here).
        assert!(baseline.cost_micros.is_none() && candidate.cost_micros.is_none());
        // Both sides ran and were timed.
        assert!(baseline.elapsed_ms > 0 && candidate.elapsed_ms > 0);
    }

    // The report renders with the pair structure.
    let report = paired_report(&standard_metrics(), &standard_metrics());
    assert!(report.starts_with("paired-eval.v1"));
    assert!(report.contains("fixture-pair:"));
    assert!(report.contains("verified:"));
}

fn standard_metrics() -> Vec<r_code_evals::harness_tasks::RunMetrics> {
    use r_code_evals::harness_tasks::RunMetrics;
    standard_fixtures()
        .into_iter()
        .map(|fixture| RunMetrics {
            label: fixture.id.clone(),
            verified_completion: false,
            false_completion: false,
            interventions: Some(0),
            recovery_events: Some(0),
            elapsed_ms: 1,
            cost_micros: None,
            host_fault: None,
        })
        .collect()
}
