use r_code_harness_protocol::canonical_input_hash;
use r_code_runtime::services::artifacts::{sha256_hex, ArtifactStore};
use r_code_runtime::services::review::{DurableReviewService, ReviewError};
use r_code_runtime::services::workspaces::TaskWorkspaceBinding;
use r_code_store::v1::{LeaseRequest, MutationFile, MutationOperation, MutationState, V1Store};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct RollbackFixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    artifacts_root: PathBuf,
    store: Arc<V1Store>,
    artifacts: Arc<ArtifactStore>,
    files: Vec<MutationFile>,
    workspace_key: String,
}

impl RollbackFixture {
    fn new(count: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        let store = Arc::new(V1Store::open(&temp.path().join("store.db")).unwrap());
        let artifacts_root = temp.path().join("artifacts");
        let artifacts = Arc::new(ArtifactStore::for_task(&artifacts_root, "task-rollback"));
        let canonical = std::fs::canonicalize(&workspace).unwrap();
        let workspace_key = format!(
            "sha256:{}",
            sha256_hex(canonical.to_string_lossy().as_bytes())
        );
        let paths = (0..count)
            .map(|index| format!("src/file-{index:04}.txt"))
            .collect::<Vec<_>>();
        let lease = store
            .acquire_lease(LeaseRequest {
                workspace_key: workspace_key.clone(),
                operation_id: "forward-lease".into(),
                owner_id: "attempt-forward".into(),
                read_paths: vec![],
                write_paths: paths.clone(),
                repo_exclusive: false,
            })
            .unwrap();
        let mut files = Vec::new();
        for (index, logical_path) in paths.into_iter().enumerate() {
            let before = format!("before-{index:04}").into_bytes();
            let after = format!("after-{index:04}").into_bytes();
            std::fs::write(workspace.join(&logical_path), &after).unwrap();
            let before_ref = artifacts.put_bytes(&before, None).unwrap();
            let after_ref = artifacts.put_bytes(&after, None).unwrap();
            let file = MutationFile {
                logical_path: logical_path.clone(),
                before_sha256: Some(before_ref.sha256.clone()),
                after_sha256: Some(after_ref.sha256.clone()),
                before_cas_ref: Some(serde_json::to_string(&before_ref).unwrap()),
                after_cas_ref: Some(serde_json::to_string(&after_ref).unwrap()),
            };
            let operation = MutationOperation {
                operation_id: format!("forward-{index}"),
                workspace_key: workspace_key.clone(),
                lease_id: lease.lease_id.clone(),
                owner_id: "attempt-forward".into(),
                fencing_epoch: lease.fencing_epoch,
                input_hash: format!("input-{index}"),
                state: MutationState::Prepared,
                files: vec![file.clone()],
            };
            store.prepare_effect_operation(&operation).unwrap();
            store
                .mark_applied(
                    &operation.operation_id,
                    "attempt-forward",
                    lease.fencing_epoch,
                    vec![file.clone()],
                )
                .unwrap();
            store
                .mark_receipted(
                    &operation.operation_id,
                    "attempt-forward",
                    lease.fencing_epoch,
                )
                .unwrap();
            files.push(file);
        }
        store
            .release_lease(&lease.lease_id, "attempt-forward", lease.fencing_epoch)
            .unwrap();
        Self {
            _temp: temp,
            workspace,
            artifacts_root,
            store,
            artifacts,
            files,
            workspace_key,
        }
    }

    fn service(&self) -> DurableReviewService {
        DurableReviewService::new(
            self.store.clone(),
            TaskWorkspaceBinding::bind_local("task-rollback", &self.workspace, &[]).unwrap(),
            self.artifacts.clone(),
            "task-rollback",
            "attempt-forward",
            "unit-1",
            "candidate",
        )
        .unwrap()
    }

    fn seed_inverse(&self, action_id: &str, state: MutationState) -> (String, String) {
        let owner = format!("review:task-rollback:{action_id}");
        let file = self.files[0].clone();
        let lease = self
            .store
            .acquire_lease(LeaseRequest {
                workspace_key: self.workspace_key.clone(),
                operation_id: format!("review-rollback:{action_id}"),
                owner_id: owner.clone(),
                read_paths: vec![],
                write_paths: vec![file.logical_path.clone()],
                repo_exclusive: false,
            })
            .unwrap();
        let inverse = MutationFile {
            logical_path: file.logical_path.clone(),
            before_sha256: file.after_sha256.clone(),
            after_sha256: file.before_sha256.clone(),
            before_cas_ref: file.after_cas_ref.clone(),
            after_cas_ref: file.before_cas_ref.clone(),
        };
        let operation_id = format!("review:{action_id}:0:forward-0");
        let operation = MutationOperation {
            operation_id: operation_id.clone(),
            workspace_key: self.workspace_key.clone(),
            lease_id: lease.lease_id.clone(),
            owner_id: owner.clone(),
            fencing_epoch: lease.fencing_epoch,
            input_hash: canonical_input_hash(&serde_json::json!({
                "forward": "forward-0",
                "path": inverse.logical_path,
                "before": inverse.before_sha256,
                "after": inverse.after_sha256,
            })),
            state: MutationState::Prepared,
            files: vec![inverse.clone()],
        };
        self.store.prepare_effect_operation(&operation).unwrap();
        if matches!(state, MutationState::Applied | MutationState::Receipted) {
            let before_ref: r_code_harness_protocol::ArtifactRef =
                serde_json::from_str(file.before_cas_ref.as_deref().unwrap()).unwrap();
            let before = self.artifacts.read_all(&before_ref).unwrap();
            std::fs::write(self.workspace.join(&file.logical_path), before).unwrap();
            self.store
                .mark_applied(&operation_id, &owner, lease.fencing_epoch, vec![inverse])
                .unwrap();
        }
        if state == MutationState::Receipted {
            self.store
                .mark_receipted(&operation_id, &owner, lease.fencing_epoch)
                .unwrap();
            self.store
                .release_lease(&lease.lease_id, &owner, lease.fencing_epoch)
                .unwrap();
        }
        (owner, operation_id)
    }
}

#[test]
fn reject_revalidates_every_path_after_acquiring_its_complete_lease() {
    let fixture = RollbackFixture::new(256);
    let store = fixture.store.clone();
    let workspace = fixture.workspace.clone();
    let workspace_key = fixture.workspace_key.clone();
    let writer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if store
                .active_leases(&workspace_key)
                .unwrap()
                .iter()
                .any(|lease| lease.request.owner_id.starts_with("review:"))
            {
                std::fs::write(workspace.join("src/file-0000.txt"), b"external-user").unwrap();
                return;
            }
            assert!(Instant::now() < deadline, "review lease was never acquired");
            std::thread::yield_now();
        }
    });
    let result = fixture.service().rollback("lease-race");
    writer.join().unwrap();
    assert!(matches!(result, Err(ReviewError::Conflict(_))));
    assert_eq!(
        std::fs::read(fixture.workspace.join("src/file-0000.txt")).unwrap(),
        b"external-user"
    );
    for index in 1..256 {
        assert_eq!(
            std::fs::read(fixture.workspace.join(format!("src/file-{index:04}.txt"))).unwrap(),
            format!("after-{index:04}").as_bytes(),
            "rollback partially wrote before discovering the post-lease conflict"
        );
    }
}

#[test]
fn applied_inverse_retry_receipts_and_releases_instead_of_leaking_active_lease() {
    let fixture = RollbackFixture::new(1);
    let action_id = "applied-retry";
    let owner = format!("review:task-rollback:{action_id}");
    let file = fixture.files[0].clone();
    let lease = fixture
        .store
        .acquire_lease(LeaseRequest {
            workspace_key: fixture.workspace_key.clone(),
            operation_id: format!("review-rollback:{action_id}"),
            owner_id: owner.clone(),
            read_paths: vec![],
            write_paths: vec![file.logical_path.clone()],
            repo_exclusive: false,
        })
        .unwrap();
    let inverse = MutationFile {
        logical_path: file.logical_path.clone(),
        before_sha256: file.after_sha256.clone(),
        after_sha256: file.before_sha256.clone(),
        before_cas_ref: file.after_cas_ref.clone(),
        after_cas_ref: file.before_cas_ref.clone(),
    };
    let operation_id = format!("review:{action_id}:0:forward-0");
    let operation = MutationOperation {
        operation_id: operation_id.clone(),
        workspace_key: fixture.workspace_key.clone(),
        lease_id: lease.lease_id.clone(),
        owner_id: owner.clone(),
        fencing_epoch: lease.fencing_epoch,
        input_hash: canonical_input_hash(&serde_json::json!({
            "forward": "forward-0",
            "path": inverse.logical_path,
            "before": inverse.before_sha256,
            "after": inverse.after_sha256,
        })),
        state: MutationState::Prepared,
        files: vec![inverse.clone()],
    };
    fixture.store.prepare_effect_operation(&operation).unwrap();
    let before_ref: r_code_harness_protocol::ArtifactRef =
        serde_json::from_str(file.before_cas_ref.as_deref().unwrap()).unwrap();
    let before = fixture.artifacts.read_all(&before_ref).unwrap();
    std::fs::write(fixture.workspace.join(&file.logical_path), before).unwrap();
    fixture
        .store
        .mark_applied(&operation_id, &owner, lease.fencing_epoch, vec![inverse])
        .unwrap();

    fixture.service().rollback(action_id).unwrap();
    assert_eq!(
        fixture
            .store
            .load_mutation_operation(&operation_id)
            .unwrap()
            .unwrap()
            .state,
        MutationState::Receipted
    );
    assert!(fixture
        .store
        .active_leases(&fixture.workspace_key)
        .unwrap()
        .is_empty());
}

#[test]
fn corrupt_inverse_cas_records_conflict_instead_of_stranding_prepared() {
    let fixture = RollbackFixture::new(1);
    let before_ref: r_code_harness_protocol::ArtifactRef =
        serde_json::from_str(fixture.files[0].before_cas_ref.as_deref().unwrap()).unwrap();
    std::fs::remove_file(
        fixture
            .artifacts_root
            .join(format!("{}.blob", before_ref.sha256)),
    )
    .unwrap();
    assert!(fixture.service().rollback("corrupt-cas").is_err());
    let inverse = fixture
        .store
        .mutation_operations_for_owner("review:task-rollback:corrupt-cas")
        .unwrap();
    assert_eq!(inverse.len(), 1);
    assert_eq!(
        inverse[0].state,
        MutationState::Conflict,
        "a prepared inverse must not remain silently ambiguous"
    );
}

#[test]
fn prepared_and_receipted_inverse_retries_converge_without_duplicate_writes() {
    let prepared = RollbackFixture::new(1);
    let (_, prepared_id) = prepared.seed_inverse("prepared-retry", MutationState::Prepared);
    prepared.service().rollback("prepared-retry").unwrap();
    assert_eq!(
        prepared
            .store
            .load_mutation_operation(&prepared_id)
            .unwrap()
            .unwrap()
            .state,
        MutationState::Receipted
    );
    assert_eq!(
        std::fs::read(prepared.workspace.join("src/file-0000.txt")).unwrap(),
        b"before-0000"
    );
    assert!(prepared
        .store
        .active_leases(&prepared.workspace_key)
        .unwrap()
        .is_empty());

    let receipted = RollbackFixture::new(1);
    let (_, receipted_id) = receipted.seed_inverse("receipted-retry", MutationState::Receipted);
    let before = std::fs::read(receipted.workspace.join("src/file-0000.txt")).unwrap();
    let outcome = receipted.service().rollback("receipted-retry").unwrap();
    assert!(outcome.restored_paths.is_empty());
    assert_eq!(
        receipted
            .store
            .load_mutation_operation(&receipted_id)
            .unwrap()
            .unwrap()
            .state,
        MutationState::Receipted
    );
    assert_eq!(
        std::fs::read(receipted.workspace.join("src/file-0000.txt")).unwrap(),
        before
    );
    assert!(receipted
        .store
        .active_leases(&receipted.workspace_key)
        .unwrap()
        .is_empty());
}
