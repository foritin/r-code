//! T17a — frozen verifier and dependency inputs.
//!
//! Real fixtures cover: npm test changed to noop (control file wins),
//! deleted verifier entry, legitimate dependency addition, submodules /
//! undeclared inputs, dirty/new files and concurrent external edits.

use r_code_kernel::verification::{CheckDefinition, CheckEntrypoint, ControlFile};
use r_code_runtime::services::artifacts::sha256_hex;
use r_code_runtime::services::verification_inputs::*;
use r_code_runtime::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use std::path::Path;

fn control_package_json(script: &str) -> Vec<u8> {
    format!(r#"{{"name":"fixture","scripts":{{"test":"{script}"}}}}"#).into_bytes()
}

fn setup_workspace(root: &Path, verifier_body: &str) -> ControlFile {
    std::fs::create_dir_all(root).expect("dirs");
    // Candidate material: a package.json whose test script the candidate
    // could redefine to a noop.
    std::fs::write(
        root.join("package.json"),
        br#"{"name":"fixture","scripts":{"test":"node verify.js"}}"#,
    )
    .expect("candidate package.json");
    std::fs::write(root.join("verify.js"), verifier_body).expect("verify.js");
    std::fs::write(root.join("package-lock.json"), b"{}").expect("lockfile");

    // Frozen control copy of package.json pins the test entry.
    let bytes = control_package_json("node verify.js");
    ControlFile {
        path: "package.json".into(),
        sha256: sha256_hex(&bytes),
    }
}

fn definition(control: ControlFile) -> CheckDefinition {
    CheckDefinition {
        check_id: "check:npm-test".into(),
        entrypoint: CheckEntrypoint::Command {
            program: "npm".into(),
            argv: vec!["test".into(), "--".into()],
        },
        control_files: vec![control],
        source_roots: vec![".".into()],
        dependency_locks: vec!["package-lock.json".into()],
        toolchain: "node 22".into(),
        declared_external_inputs: vec![],
        entrypoint_bytes: None,
    }
}

fn materialize_into(
    temp: &Path,
    root: &Path,
    control: &ControlFile,
    def: &CheckDefinition,
) -> Result<PreparedVerificationDir, MaterializeError> {
    let control_store = FrozenControlStore::new(temp.join("frozen"));
    control_store
        .store(control, &control_package_json("node verify.js"))
        .expect("store control");
    let binding = TaskWorkspaceBinding::bind_local("task-1", root, &[]).expect("bind");
    let manifest = CandidateManifest::capture(&binding).expect("capture");
    materialize(
        &binding,
        &manifest,
        &control_store,
        def,
        &temp.join("verify-dir"),
    )
}

#[test]
fn noop_script_redefinition_is_defeated_by_frozen_controls() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    let control = setup_workspace(&root, "process.exit(0);");
    let def = definition(control.clone());

    // The candidate redefines "npm test" to a noop — a classic fake-pass.
    std::fs::write(
        root.join("package.json"),
        br#"{"name":"fixture","scripts":{"test":"node -e ''"}}"#,
    )
    .expect("noop redefinition");

    let prepared = materialize_into(temp.path(), &root, &control, &def).expect("materialize");
    let materialized = std::fs::read_to_string(prepared.dir.join("package.json")).expect("read");
    // The FROZEN control package.json won: the test entry still runs the
    // verifier, not the candidate's noop.
    assert!(
        materialized.contains("node verify.js"),
        "control file must override the candidate noop: {materialized}"
    );
    // Candidate lockfile preserved from the candidate bytes.
    assert!(prepared.dir.join("package-lock.json").is_file());
    assert!(prepared
        .preserved_lockfiles
        .contains(&"package-lock.json".to_string()));
}

#[test]
fn deleted_verifier_entry_makes_the_check_unavailable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    let control = setup_workspace(&root, "process.exit(0);");
    // Delete the verifier the frozen entry invokes.
    std::fs::remove_file(root.join("verify.js")).expect("delete verifier");
    let def = definition(control.clone());

    // Materialization succeeds (files copied); the verifier absence is
    // visible in the materialized dir — the runner (T19) turns this into
    // an unavailable outcome rather than a pass.
    let prepared = materialize_into(temp.path(), &root, &control, &def).expect("materialize");
    assert!(
        !prepared.dir.join("verify.js").exists(),
        "deleted entry stays deleted"
    );
}

#[test]
fn legitimate_dependency_additions_change_the_candidate_not_the_controls() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    let control = setup_workspace(&root, "process.exit(0);");

    // A legitimate new dependency changes the lockfile and cache identity.
    std::fs::write(
        root.join("package-lock.json"),
        br#"{"deps":{"left-pad":"1.0.0"}}"#,
    )
    .expect("dep add");
    let def = definition(control.clone());
    let prepared = materialize_into(temp.path(), &root, &control, &def).expect("materialize");

    let mut second_root = temp.path().join("project2");
    std::fs::create_dir_all(&second_root).expect("dirs");
    std::fs::write(
        second_root.join("package.json"),
        br#"{"name":"fixture","scripts":{"test":"node verify.js"}}"#,
    )
    .expect("pkg");
    std::fs::write(second_root.join("verify.js"), b"x").expect("verify");
    std::fs::write(
        second_root.join("package-lock.json"),
        br#"{"deps":{"left-pad":"1.0.0"}}"#,
    )
    .expect("lock");
    let control_store = FrozenControlStore::new(temp.path().join("frozen2"));
    control_store
        .store(&control, &control_package_json("node verify.js"))
        .expect("store");
    let binding2 = TaskWorkspaceBinding::bind_local("task-2", &second_root, &[]).expect("bind");
    let manifest2 = CandidateManifest::capture(&binding2).expect("capture");
    let prepared2 = materialize(
        &binding2,
        &manifest2,
        &control_store,
        &def,
        &temp.path().join("verify-dir2"),
    )
    .expect("materialize 2");
    // Same lockfile + toolchain → same (reusable) dependency cache identity.
    assert_eq!(prepared.cache_identity, prepared2.cache_identity);

    // A different lockfile → different cache identity (no stale reuse).
    std::fs::write(
        second_root.join("package-lock.json"),
        br#"{"deps":{"right-pad":"2.0.0"}}"#,
    )
    .expect("lock2");
    let manifest3 = CandidateManifest::capture(&binding2).expect("capture 3");
    let prepared3 = materialize(
        &binding2,
        &manifest3,
        &control_store,
        &def,
        &temp.path().join("verify-dir3"),
    )
    .expect("materialize 3");
    assert_ne!(prepared.cache_identity, prepared3.cache_identity);
    let _ = &mut second_root;
}

#[test]
fn undeclared_external_inputs_refuse_materialization() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    let control = setup_workspace(&root, "process.exit(0);");
    let mut def = definition(control.clone());
    // A required external input that does not exist.
    def.declared_external_inputs = vec!["submodules/vendor".into()];
    let error = materialize_into(temp.path(), &root, &control, &def).expect_err("undeclared");
    assert!(
        matches!(error, MaterializeError::UndeclaredInput(input) if input == "submodules/vendor")
    );

    // Declaring an existing external input materializes fine.
    std::fs::create_dir_all(root.join("submodules").join("vendor")).expect("submodule");
    std::fs::write(root.join("submodules/vendor/thing.txt"), b"vendored").expect("vendor");
    materialize_into(temp.path(), &root, &control, &def).expect("declared input ok");
}

#[test]
fn concurrent_external_edits_refuse_materialization() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    let control = setup_workspace(&root, "process.exit(0);");
    let def = definition(control.clone());

    let control_store = FrozenControlStore::new(temp.path().join("frozen3"));
    control_store
        .store(&control, &control_package_json("node verify.js"))
        .expect("store");
    let binding = TaskWorkspaceBinding::bind_local("task-1", &root, &[]).expect("bind");
    let manifest = CandidateManifest::capture(&binding).expect("capture");

    // External edit after capture: materialization refuses the stale state.
    std::fs::write(
        root.join("verify.js"),
        b"process.exit(0); // edited concurrently",
    )
    .expect("edit");
    let error = materialize(
        &binding,
        &manifest,
        &control_store,
        &def,
        &temp.path().join("verify-dir4"),
    )
    .expect_err("stale");
    assert!(matches!(error, MaterializeError::StaleCapture(_)));
    // Nothing was materialized.
    assert!(!temp.path().join("verify-dir4/package.json").exists());
}

#[test]
fn dirty_and_new_files_travel_with_the_candidate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    let control = setup_workspace(&root, "process.exit(0);");
    // Pre-existing dirty tracked file and a new untracked one.
    std::fs::write(root.join("dirty.txt"), b"user-was-here").expect("dirty");
    std::fs::create_dir_all(root.join("new-dir")).expect("new dir");
    std::fs::write(root.join("new-dir/new.txt"), b"brand-new").expect("new file");

    let def = definition(control.clone());
    let prepared = materialize_into(temp.path(), &root, &control, &def).expect("materialize");
    assert_eq!(
        std::fs::read(prepared.dir.join("dirty.txt")).unwrap(),
        b"user-was-here"
    );
    assert_eq!(
        std::fs::read(prepared.dir.join("new-dir/new.txt")).unwrap(),
        b"brand-new"
    );
}
