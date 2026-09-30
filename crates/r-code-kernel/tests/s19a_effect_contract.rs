//! S19A — the frozen Shell effect/network contract at API v1.2.

use r_code_harness_protocol::manifest::{
    ApiVersion, HarnessManifest, HarnessManifestBuilder, HostService, ManifestError, Platform,
};
use r_code_harness_protocol::rpc::HOST_API_VERSION;
use r_code_harness_protocol::services::{
    work_unit_payload_hash, NetworkCeiling, WorkUnitEffectClass, WorkUnitWire,
};
use r_code_kernel::plans::{PlanRevision, PlanRevisionError, PlanRevisionMaterial};
use serde::Serialize;

/// The pre-P19A `WorkUnitWire` document: exactly the fields an API 1.0/1.1
/// plan could carry, and nothing else.
const LEGACY_UNIT_JSON: &str = r#"{"id":"unit-1","description":"implement the reader","dependencies":[],"acceptance":["check:test"],"write_paths":["src"]}"#;

/// The frozen pre-P19A wire shape, replicated so the additive-field claim is
/// a byte comparison rather than a hand-typed expectation.
#[derive(Debug, Serialize)]
struct LegacyWorkUnitWire {
    id: String,
    description: String,
    dependencies: Vec<String>,
    acceptance: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    read_paths: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    write_paths: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    repo_exclusive: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ephemeral_roots: Vec<String>,
}

fn unit(effect_class: WorkUnitEffectClass, network_ceiling: NetworkCeiling) -> WorkUnitWire {
    WorkUnitWire {
        id: "unit-1".into(),
        description: "implement the reader".into(),
        dependencies: vec![],
        acceptance: vec!["check:test".into()],
        read_paths: vec![],
        write_paths: vec!["src".into()],
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class,
        network_ceiling,
    }
}

fn material(units: Vec<WorkUnitWire>) -> PlanRevisionMaterial {
    PlanRevisionMaterial {
        task_id: "task-effect".into(),
        revision: 1,
        parent_revision: None,
        current_base_hash: "sha256:base".into(),
        workspace_baseline: "sha256:workspace".into(),
        route_digest: "sha256:route".into(),
        prompt_digest: "sha256:prompt".into(),
        permission_digest: "sha256:permission".into(),
        check_digest: "sha256:checks".into(),
        required_checks: vec!["check:test".into()],
        work_units: units,
    }
}

#[test]
fn effect_and_network_enums_freeze_their_kebab_wire_values() {
    let classes = [
        (WorkUnitEffectClass::ReadOnly, "read-only"),
        (WorkUnitEffectClass::WorkspaceMutation, "workspace-mutation"),
        (
            WorkUnitEffectClass::DependencyPreparation,
            "dependency-preparation",
        ),
    ];
    for (value, wire) in classes {
        assert_eq!(value.as_str(), wire);
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            format!("\"{wire}\"")
        );
        assert_eq!(
            serde_json::from_str::<WorkUnitEffectClass>(&format!("\"{wire}\"")).unwrap(),
            value
        );
        assert!(serde_json::from_str::<WorkUnitEffectClass>("\"Read-Only\"").is_err());
        assert!(serde_json::from_str::<WorkUnitEffectClass>("\"read_only\"").is_err());
    }

    let ceilings = [
        (NetworkCeiling::Offline, "offline"),
        (
            NetworkCeiling::PublicInternetClient,
            "public-internet-client",
        ),
        (NetworkCeiling::HostNetwork, "host-network"),
    ];
    for (value, wire) in ceilings {
        assert_eq!(value.as_str(), wire);
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            format!("\"{wire}\"")
        );
        assert_eq!(
            serde_json::from_str::<NetworkCeiling>(&format!("\"{wire}\"")).unwrap(),
            value
        );
        assert!(serde_json::from_str::<NetworkCeiling>("\"public\"").is_err());
    }

    assert_eq!(
        WorkUnitEffectClass::default(),
        WorkUnitEffectClass::ReadOnly
    );
    assert_eq!(NetworkCeiling::default(), NetworkCeiling::Offline);
    assert!(WorkUnitEffectClass::ReadOnly.is_read_only());
    assert!(!WorkUnitEffectClass::WorkspaceMutation.is_read_only());
    assert!(!WorkUnitEffectClass::DependencyPreparation.is_read_only());
    assert!(NetworkCeiling::Offline.is_offline());
    assert!(!NetworkCeiling::PublicInternetClient.is_offline());
    assert!(!NetworkCeiling::HostNetwork.is_offline());

    assert!(WorkUnitEffectClass::ReadOnly < WorkUnitEffectClass::WorkspaceMutation);
    assert!(WorkUnitEffectClass::WorkspaceMutation < WorkUnitEffectClass::DependencyPreparation);
}

#[test]
fn a_pre_p19a_unit_document_parses_to_the_conservative_floor() {
    let parsed: WorkUnitWire = serde_json::from_str(LEGACY_UNIT_JSON).expect("legacy document");
    assert_eq!(parsed.effect_class, WorkUnitEffectClass::ReadOnly);
    assert_eq!(parsed.network_ceiling, NetworkCeiling::Offline);
    assert_eq!(parsed.id, "unit-1");
    assert!(parsed.dependencies.is_empty());
    assert_eq!(parsed.acceptance, vec!["check:test".to_string()]);
    assert_eq!(parsed.write_paths, vec!["src".to_string()]);
    assert!(parsed.read_paths.is_empty());
    assert!(!parsed.repo_exclusive);

    let declared: WorkUnitWire = serde_json::from_str(
        r#"{"id":"u","description":"d","effect_class":"workspace-mutation","network_ceiling":"public-internet-client"}"#,
    )
    .expect("v1.2 document");
    assert_eq!(
        declared.effect_class,
        WorkUnitEffectClass::WorkspaceMutation
    );
    assert_eq!(
        declared.network_ceiling,
        NetworkCeiling::PublicInternetClient
    );

    assert!(serde_json::from_str::<WorkUnitWire>(
        r#"{"id":"u","description":"d","effect_class":"host_network"}"#
    )
    .is_err());
}

#[test]
fn defaulted_units_serialize_byte_identically_to_the_pre_p19a_shape() {
    let current = unit(WorkUnitEffectClass::ReadOnly, NetworkCeiling::Offline);
    let legacy = LegacyWorkUnitWire {
        id: current.id.clone(),
        description: current.description.clone(),
        dependencies: current.dependencies.clone(),
        acceptance: current.acceptance.clone(),
        read_paths: current.read_paths.clone(),
        write_paths: current.write_paths.clone(),
        repo_exclusive: current.repo_exclusive,
        ephemeral_roots: current.ephemeral_roots.clone(),
    };
    assert_eq!(
        serde_json::to_string(&current).unwrap(),
        serde_json::to_string(&legacy).unwrap(),
        "the additive fields must not perturb an old plan's bytes"
    );
    assert_eq!(serde_json::to_string(&current).unwrap(), LEGACY_UNIT_JSON);

    let legacy_value: serde_json::Value = serde_json::from_str(LEGACY_UNIT_JSON).unwrap();
    let keys: Vec<&str> = legacy_value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        !keys
            .iter()
            .any(|key| key.contains("effect") || key.contains("network")),
        "legacy key set: {keys:?}"
    );

    let round_tripped: WorkUnitWire = serde_json::from_str(LEGACY_UNIT_JSON).unwrap();
    assert_eq!(
        serde_json::to_string(&round_tripped).unwrap(),
        LEGACY_UNIT_JSON
    );

    let escalated = serde_json::to_value(unit(
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::PublicInternetClient,
    ))
    .unwrap();
    assert_eq!(escalated["effect_class"], "workspace-mutation");
    assert_eq!(escalated["network_ceiling"], "public-internet-client");
}

#[test]
fn payload_hash_commits_the_authority_keys_and_ignores_description() {
    let base = unit(WorkUnitEffectClass::ReadOnly, NetworkCeiling::Offline);
    let hash = work_unit_payload_hash(&base);
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(
        hash.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "{hash}"
    );
    assert_eq!(hash, work_unit_payload_hash(&base));

    let mut reworded = base.clone();
    reworded.description = "a completely different description".into();
    assert_eq!(
        work_unit_payload_hash(&reworded),
        hash,
        "copy edits must not invalidate an effect approval"
    );

    let mut id_changed = base.clone();
    id_changed.id = "unit-2".into();
    let mut deps_changed = base.clone();
    deps_changed.dependencies = vec!["unit-9".into()];
    let mut reads_changed = base.clone();
    reads_changed.read_paths = vec!["docs".into()];
    let mut writes_changed = base.clone();
    writes_changed.write_paths = vec!["crates".into()];
    let mut exclusive_changed = base.clone();
    exclusive_changed.repo_exclusive = true;
    let mut ephemeral_changed = base.clone();
    ephemeral_changed.ephemeral_roots = vec!["tmp".into()];
    let mut class_changed = base.clone();
    class_changed.effect_class = WorkUnitEffectClass::WorkspaceMutation;
    let mut network_changed = base.clone();
    network_changed.network_ceiling = NetworkCeiling::PublicInternetClient;

    for (name, candidate) in [
        ("id", &id_changed),
        ("dependencies", &deps_changed),
        ("read_paths", &reads_changed),
        ("write_paths", &writes_changed),
        ("repo_exclusive", &exclusive_changed),
        ("ephemeral_roots", &ephemeral_changed),
        ("effect_class", &class_changed),
        ("network_ceiling", &network_changed),
    ] {
        assert_ne!(
            work_unit_payload_hash(candidate),
            hash,
            "{name} must be inside the payload hash"
        );
    }

    let legacy: WorkUnitWire = serde_json::from_str(LEGACY_UNIT_JSON).unwrap();
    let mut explicit = legacy.clone();
    explicit.effect_class = WorkUnitEffectClass::ReadOnly;
    explicit.network_ceiling = NetworkCeiling::Offline;
    assert_eq!(
        work_unit_payload_hash(&legacy),
        work_unit_payload_hash(&explicit)
    );
}

#[test]
fn host_network_plans_are_rejected_while_weaker_ceilings_publish() {
    let refused = PlanRevision::new(material(vec![unit(
        WorkUnitEffectClass::ReadOnly,
        NetworkCeiling::HostNetwork,
    )]))
    .expect_err("host-network must be refused");
    assert!(
        matches!(&refused, PlanRevisionError::HostNetworkUnsupported(id) if id == "unit-1"),
        "{refused:?}"
    );
    assert!(
        refused.to_string().contains("platform-incompatible"),
        "{refused}"
    );

    let mut mixed = vec![unit(WorkUnitEffectClass::ReadOnly, NetworkCeiling::Offline)];
    let mut offender = unit(
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::HostNetwork,
    );
    offender.id = "unit-offender".into();
    mixed.push(offender);
    assert!(matches!(
        PlanRevision::new(material(mixed)),
        Err(PlanRevisionError::HostNetworkUnsupported(id)) if id == "unit-offender"
    ));

    let internet = PlanRevision::new(material(vec![unit(
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::PublicInternetClient,
    )]))
    .expect("public-internet-client publishes");
    assert_eq!(
        internet.material().work_units[0].network_ceiling,
        NetworkCeiling::PublicInternetClient
    );
    assert_eq!(
        internet.material().work_units[0].effect_class,
        WorkUnitEffectClass::WorkspaceMutation
    );
}

#[test]
fn effect_fields_are_inside_the_plan_revision_hash() {
    let baseline = PlanRevision::new(material(vec![unit(
        WorkUnitEffectClass::ReadOnly,
        NetworkCeiling::Offline,
    )]))
    .expect("floor plan");
    let mutated = PlanRevision::new(material(vec![unit(
        WorkUnitEffectClass::WorkspaceMutation,
        NetworkCeiling::Offline,
    )]))
    .expect("mutation plan");
    let networked = PlanRevision::new(material(vec![unit(
        WorkUnitEffectClass::ReadOnly,
        NetworkCeiling::PublicInternetClient,
    )]))
    .expect("network plan");

    assert_ne!(baseline.reference(), mutated.reference());
    assert_ne!(baseline.reference(), networked.reference());
    assert_ne!(mutated.reference(), networked.reference());
    mutated
        .validate_identity()
        .expect("canonical with effect fields");

    let baseline_json = baseline.canonical_json().expect("canonical json");
    assert!(
        !baseline_json.contains("effect_class") && !baseline_json.contains("network_ceiling"),
        "a floor plan keeps its pre-P19A bytes: {baseline_json}"
    );
    let mutated_json = mutated.canonical_json().expect("canonical json");
    assert!(mutated_json.contains("\"effect_class\":\"workspace-mutation\""));
    let networked_json = networked.canonical_json().expect("canonical json");
    assert!(networked_json.contains("\"network_ceiling\":\"public-internet-client\""));

    let legacy_document: WorkUnitWire = serde_json::from_str(LEGACY_UNIT_JSON).unwrap();
    let from_legacy = PlanRevision::new(material(vec![legacy_document])).expect("legacy plan");
    assert_eq!(
        from_legacy.reference(),
        baseline.reference(),
        "a plan that omits the fields keeps its pre-P19A revision hash"
    );
    assert_eq!(
        from_legacy.canonical_json().unwrap(),
        baseline_json,
        "canonical bytes are unchanged for floor plans"
    );
}

#[test]
fn requires_effect_fields_demands_api_minor_two() {
    for (major, minor, flag, expected) in [
        (1u32, 0u32, false, Ok(())),
        (1, 1, false, Ok(())),
        (1, 2, false, Ok(())),
        (1, 2, true, Ok(())),
        (1, 3, true, Ok(())),
        (1, 0, true, Err((1, 0))),
        (1, 1, true, Err((1, 1))),
        (2, 0, true, Err((2, 0))),
    ] {
        let mut builder = HarnessManifestBuilder::new("harness.effect", "1.0.0")
            .api(major, minor)
            .entrypoint(Platform::WindowsX64, "bin/harness", &[]);
        if flag {
            builder = builder.requires_effect_fields();
        }
        let manifest = builder.build();
        match (manifest.validate(), expected) {
            (Ok(()), Ok(())) => {}
            (Err(ManifestError::EffectFieldsNeedNewerApi { declared }), Err(wanted)) => {
                assert_eq!(declared, wanted, "api {major}.{minor}")
            }
            (other, wanted) => panic!("api {major}.{minor} flag {flag}: {other:?} vs {wanted:?}"),
        }
    }

    let plain = HarnessManifestBuilder::new("harness.effect", "1.0.0").build();
    let plain_json = serde_json::to_value(&plain).unwrap();
    assert!(
        plain_json.get("requiresEffectFields").is_none(),
        "the flag must stay out of an old manifest's bytes"
    );

    let flagged = HarnessManifestBuilder::new("harness.effect", "1.0.0")
        .api(1, 2)
        .requires_effect_fields()
        .build();
    assert_eq!(
        serde_json::to_value(&flagged).unwrap()["requiresEffectFields"],
        serde_json::json!(true)
    );
    let decoded: HarnessManifest =
        serde_json::from_str(&serde_json::to_string(&flagged).unwrap()).unwrap();
    assert!(decoded.requires_effect_fields);
    let legacy_decoded: HarnessManifest =
        serde_json::from_str(&serde_json::to_string(&plain).unwrap()).unwrap();
    assert!(!legacy_decoded.requires_effect_fields);
}

#[test]
fn legacy_one_x_packages_still_negotiate_against_host_v1_2() {
    assert_eq!(HOST_API_VERSION, ApiVersion::new(1, 2));
    assert_eq!(HOST_API_VERSION.major, 1, "INV-01: no v2 brand");

    for (major, minor) in [(1u32, 0u32), (1, 1), (1, 2)] {
        let manifest = HarnessManifestBuilder::new("harness.legacy", "1.0.0")
            .api(major, minor)
            .entrypoint(Platform::WindowsX64, "bin/harness", &[])
            .build();
        let negotiated = manifest
            .negotiate(HOST_API_VERSION, Platform::WindowsX64, HostService::ALL)
            .unwrap_or_else(|error| panic!("api {major}.{minor} refused: {error:?}"));
        assert_eq!(negotiated.plugin_api, ApiVersion::new(major, minor));
        assert_eq!(negotiated.host_api, HOST_API_VERSION);
    }

    let too_new = HarnessManifestBuilder::new("harness.future", "1.0.0")
        .api(1, 3)
        .entrypoint(Platform::WindowsX64, "bin/harness", &[])
        .build();
    assert!(matches!(
        too_new.negotiate(HOST_API_VERSION, Platform::WindowsX64, HostService::ALL),
        Err(ManifestError::IncompatibleApi {
            required_minor: 3,
            host_minor: 2,
            ..
        })
    ));

    let flagged_old = HarnessManifestBuilder::new("harness.flagged", "1.0.0")
        .api(1, 1)
        .requires_effect_fields()
        .entrypoint(Platform::WindowsX64, "bin/harness", &[])
        .build();
    assert!(
        matches!(
            flagged_old.negotiate(HOST_API_VERSION, Platform::WindowsX64, HostService::ALL),
            Err(ManifestError::EffectFieldsNeedNewerApi { declared: (1, 1) })
        ),
        "negotiation must refuse before any spawn"
    );
}

#[test]
fn the_native_package_declares_v1_3_and_requires_single_process() {
    let document = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/native/harness.json"
    ));
    let manifest: HarnessManifest = serde_json::from_str(document).expect("native manifest");
    assert_eq!(manifest.id.as_str(), "native.r-code");
    assert_eq!(manifest.api_major, 1);
    assert_eq!(manifest.api_minor, 3);
    assert!(manifest.requires_effect_fields);
    assert!(manifest.requires_single_process);
    manifest.validate().expect("native manifest validates");

    // The protocol crate's frozen host constant is still the transitional 1.2
    // (rpc.rs sits outside P23's declared files), so the 1.3 package
    // negotiating against it is refused until that constant advances — the
    // runtime catalog already advertises the Wave 3 minor that serves it.
    assert!(matches!(
        manifest.negotiate(HOST_API_VERSION, Platform::current(), HostService::ALL),
        Err(ManifestError::IncompatibleApi {
            required_minor: 3,
            host_minor: 2,
            ..
        })
    ));
}
