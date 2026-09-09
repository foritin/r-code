//! harness-task-eval: paired coding-task evaluation runner (T40).

#[tokio::main]
async fn main() {
    let temp = tempfile::tempdir().expect("tempdir");
    let binary = r_code_evals::harness_tasks::repair_harness_binary();
    // Stage the package around the binary.
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
            "schema_version": "1",
            "id": "repair-harness.example",
            "version": "1.0.0",
            "apiMajor": 1, "apiMinor": 0,
            "displayName": "repair",
            "supportedPlatforms": [{"platform": platform, "executable": format!("bin/{executable}")}],
            "requestedHostServices": ["host.model.stream", "host.tools.list", "host.tools.call",
                "host.checkpoint.save", "host.completion.propose"],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");

    let fixtures = r_code_evals::harness_tasks::standard_fixtures();
    let mut baseline = Vec::new();
    let mut candidate = Vec::new();
    for fixture in &fixtures {
        // Same harness/model/config/task fixed across the pair; the two
        // sides differ only in their data roots (repeat-run variance).
        baseline.push(
            r_code_evals::harness_tasks::run_fixture(
                temp.path(),
                "baseline",
                fixture,
                &source,
                "repair-harness.example",
            )
            .await,
        );
        candidate.push(
            r_code_evals::harness_tasks::run_fixture(
                temp.path(),
                "candidate",
                fixture,
                &source,
                "repair-harness.example",
            )
            .await,
        );
    }
    println!(
        "{}",
        r_code_evals::harness_tasks::paired_report(&baseline, &candidate)
    );
    let faulted = baseline
        .iter()
        .chain(candidate.iter())
        .filter(|metrics| metrics.host_fault.is_some())
        .count();
    if faulted > 0 {
        eprintln!("{faulted} runs ended in host faults");
        std::process::exit(1);
    }
}
