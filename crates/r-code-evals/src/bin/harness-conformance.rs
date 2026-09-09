//! harness-conformance: publish the SDK conformance command (T39).

#[tokio::main]
async fn main() {
    let results = r_code_evals::harness_conformance::run_conformance_suite().await;
    println!("{}", r_code_evals::harness_conformance::render(&results));
    let failed = results.iter().filter(|result| !result.passed).count();
    if failed > 0 {
        eprintln!("{failed} conformance checks failed");
        std::process::exit(1);
    }
}
