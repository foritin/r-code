use r_code_harness_protocol::services::{
    NetworkCeiling, WorkUnitEffectClass, WorkUnitWire, WorkspacePathError,
};
use r_code_kernel::{PlanRevision, PlanRevisionError, PlanRevisionMaterial};

fn unit() -> WorkUnitWire {
    WorkUnitWire {
        id: "unit-1".into(),
        description: "exercise the approved scope".into(),
        dependencies: vec![],
        acceptance: vec!["check:test".into()],
        read_paths: vec![],
        write_paths: vec![],
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class: WorkUnitEffectClass::ReadOnly,
        network_ceiling: NetworkCeiling::Offline,
    }
}

fn material(work_unit: WorkUnitWire) -> PlanRevisionMaterial {
    PlanRevisionMaterial {
        task_id: "task-1".into(),
        revision: 1,
        parent_revision: None,
        current_base_hash: "sha256:base".into(),
        workspace_baseline: "sha256:workspace".into(),
        route_digest: "sha256:route".into(),
        prompt_digest: "sha256:prompt".into(),
        permission_digest: "sha256:permission".into(),
        check_digest: "sha256:checks".into(),
        required_checks: vec!["check:test".into()],
        work_units: vec![work_unit],
    }
}

#[test]
fn effect_scope_aliases_sort_and_deduplicate_to_one_revision() {
    let mut aliased = unit();
    aliased.read_paths = vec![
        "tests".into(),
        "./src//lib.rs".into(),
        r"src\lib.rs".into(),
        "tests/./".into(),
    ];
    aliased.write_paths = vec!["generated\\".into(), "generated".into()];
    aliased.ephemeral_roots = vec!["generated/./cache".into(), "generated/cache".into()];

    let mut canonical = unit();
    canonical.read_paths = vec!["src/lib.rs".into(), "tests".into()];
    canonical.write_paths = vec!["generated".into()];
    canonical.ephemeral_roots = vec!["generated/cache".into()];

    let aliased = PlanRevision::new(material(aliased)).expect("aliases are safe");
    let canonical = PlanRevision::new(material(canonical)).expect("canonical scope is safe");
    assert_eq!(aliased.reference(), canonical.reference());
    assert_eq!(
        aliased.canonical_json().unwrap(),
        canonical.canonical_json().unwrap()
    );
    assert_eq!(
        aliased.material().work_units[0].read_paths,
        ["src/lib.rs", "tests"]
    );
}

#[cfg(windows)]
#[test]
fn windows_scope_case_aliases_have_one_identity() {
    let mut mixed_case = unit();
    mixed_case.write_paths = vec!["SRC/Lib.RS".into(), "src/lib.rs".into()];
    let mut lower_case = unit();
    lower_case.write_paths = vec!["src/lib.rs".into()];

    let mixed_case = PlanRevision::new(material(mixed_case)).expect("Windows aliases are safe");
    let lower_case = PlanRevision::new(material(lower_case)).expect("canonical scope is safe");
    assert_eq!(mixed_case.reference(), lower_case.reference());
    assert_eq!(
        mixed_case.material().work_units[0].write_paths,
        ["src/lib.rs"]
    );
}

#[cfg(not(windows))]
#[test]
fn unix_scope_case_is_part_of_identity() {
    let mut mixed_case = unit();
    mixed_case.write_paths = vec!["SRC/Lib.RS".into()];
    let mut lower_case = unit();
    lower_case.write_paths = vec!["src/lib.rs".into()];

    let mixed_case = PlanRevision::new(material(mixed_case)).unwrap();
    let lower_case = PlanRevision::new(material(lower_case)).unwrap();
    assert_ne!(mixed_case.reference(), lower_case.reference());
}

#[test]
fn unsafe_effect_paths_fail_closed_without_echoing_input() {
    let unsafe_paths = [
        ("/absolute/secret-a", WorkspacePathError::Absolute),
        (r"C:\secret-b", WorkspacePathError::Absolute),
        (r"\\server\share\secret-c", WorkspacePathError::Absolute),
        ("src/../secret-d", WorkspacePathError::ParentTraversal),
        ("src/.GiT/config-secret-e", WorkspacePathError::GitMetadata),
        ("src/secret-f\0tail", WorkspacePathError::Nul),
    ];

    for (path, expected_reason) in unsafe_paths {
        let mut scoped = unit();
        scoped.write_paths = vec![path.into()];
        let error = PlanRevision::new(material(scoped)).expect_err("unsafe path must fail");
        assert!(matches!(
            error,
            PlanRevisionError::InvalidEffectPath {
                field: "write_paths",
                reason,
                ..
            } if reason == expected_reason
        ));
        let rendered = format!("{error:?} {error}");
        assert!(
            !rendered.contains(path),
            "validation leaked input: {rendered}"
        );
        assert!(
            !rendered.contains("secret-"),
            "validation leaked secret: {rendered}"
        );
    }
}

#[test]
fn any_read_write_overlap_is_rejected() {
    for (read, write) in [("src", "src"), ("src", "src/lib.rs"), ("src/lib.rs", "src")] {
        let mut scoped = unit();
        scoped.read_paths = vec![read.into()];
        scoped.write_paths = vec![write.into()];
        assert!(matches!(
            PlanRevision::new(material(scoped)),
            Err(PlanRevisionError::ConflictingEffectPath { .. })
        ));
    }
}

#[test]
fn ephemeral_roots_use_component_prefixes_not_string_prefixes() {
    let mut valid = unit();
    valid.write_paths = vec!["src".into()];
    valid.ephemeral_roots = vec!["src/generated".into()];
    PlanRevision::new(material(valid)).expect("descendant is covered");

    let mut sibling = unit();
    sibling.write_paths = vec!["src".into()];
    sibling.ephemeral_roots = vec!["src2/generated".into()];
    assert!(matches!(
        PlanRevision::new(material(sibling)),
        Err(PlanRevisionError::EphemeralOutsideWriteScope { .. })
    ));
}

#[test]
fn repository_exclusivity_is_revision_material() {
    let normal = PlanRevision::new(material(unit())).unwrap();
    let mut exclusive = unit();
    exclusive.repo_exclusive = true;
    let exclusive = PlanRevision::new(material(exclusive)).unwrap();
    assert_ne!(normal.reference(), exclusive.reference());
    assert!(exclusive.material().work_units[0].repo_exclusive);
}
