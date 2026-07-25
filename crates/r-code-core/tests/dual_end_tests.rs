//! P0-t8: 双端测试规则 -- 公共合同变更触发 R-Code + Tiny Hermes 双端测试。
//!
//! 验证 `contract-lock.json` 存在且结构正确：
//! - publicContract / commit / validatedBy / qualityGates / versionPolicy
//! - validatedBy 必须包含 agentCore / r-code / tinyHermes 三个消费者
//!
//! 运行：`cargo test -p r-code-core --test dual_end_tests`

/// P0-t8: Dual-end test rules - public contract changes trigger both R-Code + Tiny Hermes tests
#[test]
fn dual_end_test_rules_verified() {
    // Verify contract-lock.json exists and has correct structure
    let lock_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/agent-core/contract-lock.json");
    assert!(lock_path.exists(), "contract-lock.json must exist");

    let content = std::fs::read_to_string(&lock_path).unwrap();
    let lock: serde_json::Value = serde_json::from_str(&content).unwrap();

    // Verify structure
    assert!(lock["publicContract"].is_string());
    assert!(lock["commit"].is_string());
    assert!(
        lock["commit"].as_str().unwrap().len() >= 40,
        "commit must be a full SHA"
    );

    // Verify validatedBy includes both consumers
    let validated_by = &lock["validatedBy"];
    assert!(validated_by["agentCore"].is_string());
    assert!(validated_by["r-code"].is_string());
    assert!(validated_by["tinyHermes"].is_string());

    // Verify quality gates
    let gates = &lock["qualityGates"];
    assert!(gates["fmt"].is_string());
    assert!(gates["clippy"].is_string());
    assert!(gates["test"].is_string());

    // Verify version policy
    assert!(lock["versionPolicy"].is_string());

    println!("\u{2705} Dual-end test rules verified: contract-lock.json is valid");
    println!("  Public contract: {}", lock["publicContract"]);
    println!(
        "  Commit: {}",
        lock["commit"]
            .as_str()
            .unwrap()
            .chars()
            .take(8)
            .collect::<String>()
    );
    println!("  R-Code test command: {}", validated_by["r-code"]);
}
