//! M1a-03 (FR-7.3): task.detail projects the task-frozen memory handoff so
//! desktop Codex delegations can reuse the frozen snapshot instead of
//! recomputing one (the desktop `codex_delegation_memory_context` helper
//! consumes this projection for the exec/MCP delegation prompts).

//! macOS：daemon→native harness 链路依赖 P13 安全激活报告，本 wave 固定
//! Unsupported——按设计拒绝启动；用例由 linux/windows 腿运行，P13 落地后移除。
#![cfg(not(target_os = "macos"))]

mod p_gate_support;

use p_gate_support::{compose_with_builtin, stage_native};
use r_code_kernel::task::{FrozenMemoryHandoff, TaskKind};
use r_code_kernel::testing::FakeModelService;
use r_code_runtime::application::CreateTaskInput;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::sync::Arc;

#[tokio::test]
async fn task_detail_projects_the_frozen_memory_handoff() {
    let directory = tempfile::tempdir().unwrap();
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(directory.path().join("data"))
            .with_ipc_name("m1a-03-codex-memory"),
    )
    .unwrap();
    let package = stage_native(directory.path(), "native.r-code", "1.0.0", true);
    let service = compose_with_builtin(&profile, &package, Arc::new(FakeModelService::default()));

    let handoff = FrozenMemoryHandoff {
        rendered: "<r_code_memory_snapshot>codex delegation source</r_code_memory_snapshot>".into(),
        entry_ids: vec!["entry-9".into()],
        snapshot_hash: "hash-m1a-03".into(),
    };
    service
        .create_task_legacy_default(CreateTaskInput {
            task_id: "codex-memory-task".into(),
            objective: "delegate".into(),
            title: None,
            kind: TaskKind::Conversation,
            required_checks: vec![],
            memory: Some(handoff),
            preferences: Default::default(),
            harness_id: None,
        })
        .await
        .expect("create task");

    let detail = service
        .task_detail("codex-memory-task")
        .await
        .expect("detail");
    let memory = detail.memory.as_ref().expect("memory projection present");
    assert_eq!(memory.snapshot_hash, "hash-m1a-03");
    assert_eq!(memory.entry_ids, vec!["entry-9".to_string()]);
    assert!(memory.rendered.contains("codex delegation source"));

    service
        .create_task_legacy_default(CreateTaskInput {
            task_id: "memoryless-task".into(),
            objective: "delegate".into(),
            title: None,
            kind: TaskKind::Conversation,
            required_checks: vec![],
            memory: None,
            preferences: Default::default(),
            harness_id: None,
        })
        .await
        .expect("create memoryless");
    let plain = service
        .task_detail("memoryless-task")
        .await
        .expect("detail memoryless");
    assert!(plain.memory.is_none(), "no handoff, no projection");
}
