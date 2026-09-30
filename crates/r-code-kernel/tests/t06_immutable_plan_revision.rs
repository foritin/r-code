//! T06 — immutable plan revision and approval domain contract.

use r_code_harness_protocol::services::{NetworkCeiling, WorkUnitEffectClass, WorkUnitWire};
use r_code_kernel::{
    PlanApprovalActor, PlanApprovalError, PlanRevision, PlanRevisionError, PlanRevisionMaterial,
    PlanRevisionRef, PLAN_APPROVE_SCOPE,
};

fn unit(id: &str, description: &str, dependencies: &[&str], acceptance: &[&str]) -> WorkUnitWire {
    WorkUnitWire {
        id: id.into(),
        description: description.into(),
        dependencies: dependencies.iter().map(|value| (*value).into()).collect(),
        acceptance: acceptance.iter().map(|value| (*value).into()).collect(),
        read_paths: vec![],
        write_paths: vec![],
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class: WorkUnitEffectClass::ReadOnly,
        network_ceiling: NetworkCeiling::Offline,
    }
}

fn material() -> PlanRevisionMaterial {
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
        required_checks: vec!["check:test".into(), "check:fmt".into()],
        work_units: vec![
            unit("compile", "compile the project", &[], &["check:fmt"]),
            unit("test", "run the tests", &["compile"], &["check:test"]),
        ],
    }
}

fn revision_ref(byte: char) -> PlanRevisionRef {
    PlanRevisionRef::parse(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("valid revision reference")
}

#[test]
fn canonical_identity_normalizes_sets_and_declares_work_unit_order_irrelevant() {
    let canonical = PlanRevision::new(material()).expect("canonical plan");

    let mut permuted = material();
    permuted.required_checks = vec!["check:test".into(), "check:fmt".into(), "check:test".into()];
    permuted.work_units.reverse();
    permuted.work_units[0].dependencies = vec!["compile".into(), "compile".into()];
    permuted.work_units[0].acceptance = vec!["check:test".into(), "check:test".into()];

    let normalized = PlanRevision::new(permuted).expect("normalized plan");
    assert_eq!(normalized.reference(), canonical.reference());
    assert_eq!(
        normalized.canonical_json().expect("normalized json"),
        canonical.canonical_json().expect("canonical json")
    );
    assert_eq!(
        normalized
            .material()
            .work_units
            .iter()
            .map(|work_unit| work_unit.id.as_str())
            .collect::<Vec<_>>(),
        vec!["compile", "test"],
        "WorkUnits use stable id ordering rather than proposal order"
    );
}

#[test]
fn every_material_plan_input_changes_the_content_address() {
    let base = material();
    let expected = PlanRevision::new(base.clone())
        .expect("base plan")
        .reference()
        .clone();

    macro_rules! assert_material_change {
        ($label:literal, $change:expr) => {{
            let mut changed = base.clone();
            ($change)(&mut changed);
            let actual = PlanRevision::new(changed)
                .unwrap_or_else(|error| panic!("{} must remain valid: {error}", $label));
            assert_ne!(actual.reference(), &expected, "{} was not hashed", $label);
        }};
    }

    assert_material_change!("task_id", |value: &mut PlanRevisionMaterial| {
        value.task_id = "task-2".into()
    });
    assert_material_change!("revision", |value: &mut PlanRevisionMaterial| {
        value.revision = 2
    });
    assert_material_change!("parent_revision", |value: &mut PlanRevisionMaterial| {
        value.parent_revision = Some(revision_ref('a'))
    });
    assert_material_change!("current_base_hash", |value: &mut PlanRevisionMaterial| {
        value.current_base_hash = "sha256:new-base".into()
    });
    assert_material_change!("workspace_baseline", |value: &mut PlanRevisionMaterial| {
        value.workspace_baseline = "sha256:new-workspace".into()
    });
    assert_material_change!("route_digest", |value: &mut PlanRevisionMaterial| {
        value.route_digest = "sha256:new-route".into()
    });
    assert_material_change!("prompt_digest", |value: &mut PlanRevisionMaterial| {
        value.prompt_digest = "sha256:new-prompt".into()
    });
    assert_material_change!("permission_digest", |value: &mut PlanRevisionMaterial| {
        value.permission_digest = "sha256:new-permission".into()
    });
    assert_material_change!("check_digest", |value: &mut PlanRevisionMaterial| {
        value.check_digest = "sha256:new-checks".into()
    });
    assert_material_change!("required_checks", |value: &mut PlanRevisionMaterial| {
        value.required_checks.push("check:clippy".into())
    });
    assert_material_change!("work_unit_id", |value: &mut PlanRevisionMaterial| {
        value.work_units[0].id = "build".into();
        value.work_units[1].dependencies = vec!["build".into()];
    });
    assert_material_change!(
        "work_unit_description",
        |value: &mut PlanRevisionMaterial| value.work_units[0].description = "build it".into()
    );
    assert_material_change!(
        "work_unit_dependencies",
        |value: &mut PlanRevisionMaterial| value.work_units[1].dependencies.clear()
    );
    assert_material_change!(
        "work_unit_acceptance",
        |value: &mut PlanRevisionMaterial| value.work_units[0]
            .acceptance
            .push("check:clippy".into())
    );
}

#[test]
fn invalid_plan_material_is_rejected_before_it_can_be_hashed() {
    let mut value = material();
    value.task_id = "  ".into();
    assert!(matches!(
        PlanRevision::new(value),
        Err(PlanRevisionError::EmptyField("task_id"))
    ));

    for field in [
        "current_base_hash",
        "workspace_baseline",
        "route_digest",
        "prompt_digest",
        "permission_digest",
        "check_digest",
    ] {
        let mut value = material();
        match field {
            "current_base_hash" => value.current_base_hash.clear(),
            "workspace_baseline" => value.workspace_baseline.clear(),
            "route_digest" => value.route_digest.clear(),
            "prompt_digest" => value.prompt_digest.clear(),
            "permission_digest" => value.permission_digest.clear(),
            "check_digest" => value.check_digest.clear(),
            _ => unreachable!(),
        }
        assert!(matches!(
            PlanRevision::new(value),
            Err(PlanRevisionError::EmptyField(actual)) if actual == field
        ));
    }

    let mut value = material();
    value.revision = 0;
    assert!(matches!(
        PlanRevision::new(value),
        Err(PlanRevisionError::ZeroRevision)
    ));

    let mut value = material();
    value.work_units.clear();
    assert!(matches!(
        PlanRevision::new(value),
        Err(PlanRevisionError::EmptyWorkUnits)
    ));

    let mut value = material();
    value.work_units[0].id = " ".into();
    assert!(matches!(
        PlanRevision::new(value),
        Err(PlanRevisionError::EmptyWorkUnitId)
    ));

    let mut value = material();
    value.work_units[1].id = "compile".into();
    assert!(matches!(
        PlanRevision::new(value),
        Err(PlanRevisionError::DuplicateWorkUnit(id)) if id == "compile"
    ));

    let mut value = material();
    value.work_units[1].dependencies = vec!["missing".into()];
    assert!(matches!(
        PlanRevision::new(value),
        Err(PlanRevisionError::UnknownDependency { unit_id, dependency })
            if unit_id == "test" && dependency == "missing"
    ));

    let mut value = material();
    value.work_units[1].dependencies = vec!["test".into()];
    assert!(matches!(
        PlanRevision::new(value),
        Err(PlanRevisionError::SelfDependency(id)) if id == "test"
    ));

    let mut value = material();
    value.work_units[0].dependencies = vec!["test".into()];
    assert!(matches!(
        PlanRevision::new(value),
        Err(PlanRevisionError::DependencyCycle)
    ));

    let mut value = material();
    value.parent_revision = Some(PlanRevisionRef("not-a-sha256-ref".into()));
    assert!(matches!(
        PlanRevision::new(value),
        Err(PlanRevisionError::InvalidRevisionRef(_))
    ));
}

#[test]
fn approval_actor_requires_authenticated_identity_session_and_exact_scope() {
    assert!(PlanApprovalActor::new("actor-1", "session-1", PLAN_APPROVE_SCOPE).is_ok());
    assert!(matches!(
        PlanApprovalActor::new(" ", "session-1", PLAN_APPROVE_SCOPE),
        Err(PlanApprovalError::EmptyField("actor_id"))
    ));
    assert!(matches!(
        PlanApprovalActor::new("actor-1", " ", PLAN_APPROVE_SCOPE),
        Err(PlanApprovalError::EmptyField("session_id"))
    ));
    assert!(matches!(
        PlanApprovalActor::new("actor-1", "session-1", "effect.approve"),
        Err(PlanApprovalError::InvalidScope(scope)) if scope == "effect.approve"
    ));
}
