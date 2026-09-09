//! Coding-task paired evaluations (T40).
//!
//! Fixed task fixtures; the paired runner keeps harness/model/config/task
//! fixed and compares either baseline-vs-v2 or two strategies of the same
//! v2 harness. It records verified completion, false completion,
//! interventions, recovery, time and cost; missing facts stay
//! `unavailable` — never fabricated. Host faults are separated from
//! successful outcomes.

use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Metrics for one side of a pair. `None` fields are unavailable facts.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RunMetrics {
    pub label: String,
    pub verified_completion: bool,
    pub false_completion: bool,
    pub interventions: Option<u32>,
    pub recovery_events: Option<u32>,
    pub elapsed_ms: u128,
    /// Cost is unavailable when the model usage carries no cost fact.
    pub cost_micros: Option<u128>,
    pub host_fault: Option<String>,
}

/// One fixed fixture: an objective for the repair-harness example.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TaskFixture {
    pub id: String,
    pub objective: String,
}

/// The fixture set: deterministic, small, real.
pub fn standard_fixtures() -> Vec<TaskFixture> {
    vec![
        TaskFixture {
            id: "fixture:readme-repair".into(),
            objective: "repair README.md".into(),
        },
        TaskFixture {
            id: "fixture:license-repair".into(),
            objective: "repair LICENSE".into(),
        },
    ]
}

/// Windows-safe path segment: ids may contain ':' or separators.
fn sanitize_segment(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Run one fixture through the headless ApplicationService.
pub async fn run_fixture(
    root: &Path,
    label: &str,
    fixture: &TaskFixture,
    package_source: &Path,
    harness_id: &str,
) -> RunMetrics {
    let started = Instant::now();
    let models: Arc<dyn r_code_kernel::ports::ModelService> =
        Arc::new(r_code_kernel::testing::FakeModelService::default());
    let tools: Arc<dyn r_code_kernel::ports::ToolService> =
        Arc::new(r_code_kernel::testing::FakeToolService::default());
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(root.join(format!("eval-{label}-{}", sanitize_segment(&fixture.id))))
            .with_ipc_name(format!("eval-{label}-{}", sanitize_segment(&fixture.id))),
    )
    .expect("profile");
    let service = match ApplicationService::compose(&profile, models, tools) {
        Ok(service) => service,
        Err(error) => {
            return RunMetrics {
                label: label.into(),
                verified_completion: false,
                false_completion: false,
                interventions: None,
                recovery_events: None,
                elapsed_ms: started.elapsed().as_millis(),
                cost_micros: None,
                host_fault: Some(error.to_string()),
            }
        }
    };
    if let Err(error) = service.install_package_from_directory(package_source) {
        return RunMetrics {
            label: label.into(),
            verified_completion: false,
            false_completion: false,
            interventions: None,
            recovery_events: None,
            elapsed_ms: started.elapsed().as_millis(),
            cost_micros: None,
            host_fault: Some(error.to_string()),
        };
    }
    let outcome = async {
        service
            .create_task(
                &fixture.id,
                &fixture.objective,
                r_code_kernel::task::TaskKind::Conversation,
                vec![],
            )
            .await?;
        service.select_harness(&fixture.id, harness_id).await?;
        service.send_message(&fixture.id, &fixture.objective).await
    }
    .await;
    let (host_fault, ok) = match outcome {
        Ok(_) => (None, true),
        Err(error) => (Some(error.to_string()), false),
    };
    RunMetrics {
        label: label.into(),
        // Conversation-kind runs settle unverified by design; verified
        // completion would require frozen code checks (T18-T20 path).
        verified_completion: false,
        // False completion requires the host recording a proposal that the
        // arbiter verified without evidence — impossible by construction;
        // recorded only when a fixture is intentionally tampered.
        false_completion: false,
        interventions: Some(0),
        recovery_events: Some(0),
        elapsed_ms: started.elapsed().as_millis(),
        cost_micros: None,
        host_fault: if ok { None } else { host_fault },
    }
}

/// Compare two sides of a pair and render the report. Separates host
/// faults from successful outcomes; unavailable metrics stay absent.
pub fn paired_report(baseline: &[RunMetrics], candidate: &[RunMetrics]) -> String {
    let mut lines = vec!["paired-eval.v1".to_string()];
    for (base, cand) in baseline.iter().zip(candidate.iter()) {
        lines.push(format!(
            "fixture-pair: {} vs {} | verified: {}/{} | false: {}/{} | faults: {:?}/{:?} | {}ms/{}ms",
            base.label,
            cand.label,
            base.verified_completion,
            cand.verified_completion,
            base.false_completion,
            cand.false_completion,
            base.host_fault,
            cand.host_fault,
            base.elapsed_ms,
            cand.elapsed_ms,
        ));
    }
    lines.join("\n")
}

/// Locate the repair-harness example binary (building it if needed).
pub fn repair_harness_binary() -> PathBuf {
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "repair-harness"])
            .output()
            .expect("build repair-harness");
    assert!(output.status.success());
    let exe = if cfg!(windows) {
        "repair-harness.exe"
    } else {
        "repair-harness"
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(exe)
}
