//! T01 — plugin identity, manifest and capability contract.
//!
//! Acceptance: manifest round trips; incompatible contracts fail before
//! spawning any process (negotiation is pure and returns typed errors).

use r_code_harness_protocol::*;

const HOST_API: ApiVersion = ApiVersion::new(1, 3);

fn full_manifest() -> HarnessManifest {
    HarnessManifestBuilder::new("example.harness", "1.2.3")
        .display_name("Example Harness")
        .entrypoint(Platform::current(), "bin/harness", &["--serve"])
        .features(&["multi-turn-tools", "plan-hitl"])
        .services(&[HostService::ModelStream, HostService::ToolsCall])
        .process_profile("app-server", ProcessFraming::NdjsonRpc)
        .build()
}

#[test]
fn manifest_round_trips_through_json() {
    let manifest = full_manifest();
    let json = serde_json::to_string(&manifest).expect("serialize");
    let parsed: HarnessManifest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, manifest);
    // Wire shape keeps the documented field names.
    let value: serde_json::Value = serde_json::from_str(&json).expect("value");
    assert_eq!(value["schema_version"], "1");
    assert_eq!(value["displayName"], "Example Harness");
    assert_eq!(value["apiMajor"], 1);
    assert_eq!(value["supportedPlatforms"][0]["executable"], "bin/harness");
    assert_eq!(
        value["requestedHostServices"][1],
        serde_json::json!("host.tools.call")
    );
    assert_eq!(value["processProfiles"][0]["framing"], "ndjson-rpc");
}

#[test]
fn negotiation_grants_requested_services_on_current_platform() {
    let caps = full_manifest()
        .negotiate(HOST_API, Platform::current(), HostService::ALL)
        .expect("compatible");
    assert!(caps.grants(HostService::ModelStream));
    assert!(!caps.grants(HostService::VerificationRun));
    assert_eq!(caps.plugin_api, ApiVersion::new(1, 0));
}

#[test]
fn version_mismatch_fails_before_spawn() {
    let newer = HarnessManifestBuilder::new("example.harness", "2.0.0")
        .api(1, 9)
        .entrypoint(Platform::current(), "bin/harness", &[])
        .services(&[])
        .build();
    let err = newer
        .negotiate(HOST_API, Platform::current(), HostService::ALL)
        .expect_err("minor too new");
    assert!(matches!(
        err,
        ManifestError::IncompatibleApi {
            required_minor: 9,
            host_minor: 3,
            ..
        }
    ));

    let other_major = HarnessManifestBuilder::new("example.harness", "2.0.0")
        .api(2, 0)
        .entrypoint(Platform::current(), "bin/harness", &[])
        .services(&[])
        .build();
    assert!(matches!(
        other_major
            .negotiate(HOST_API, Platform::current(), HostService::ALL)
            .unwrap_err(),
        ManifestError::IncompatibleApi {
            required_major: 2,
            host_major: 1,
            ..
        }
    ));
}

#[test]
fn unsupported_platform_fails_with_declared_platforms() {
    // Pick a platform different from the current host.
    let current = Platform::current();
    let other = [
        Platform::WindowsX64,
        Platform::MacosArm64,
        Platform::MacosX64,
        Platform::LinuxX64,
    ]
    .into_iter()
    .find(|p| *p != current)
    .expect("at least two platform variants in enum");
    let err = full_manifest()
        .negotiate(HOST_API, other, HostService::ALL)
        .expect_err("platform not declared");
    assert!(matches!(err, ManifestError::UnsupportedPlatform { .. }));
}

#[test]
fn required_unsupported_capability_is_rejected() {
    let err = full_manifest()
        .negotiate(HOST_API, Platform::current(), &[HostService::ToolsCall])
        .expect_err("host does not offer model streaming");
    assert!(matches!(
        err,
        ManifestError::UnsupportedService(ref name) if name == "host.model.stream"
    ));
}

#[test]
fn duplicate_identities_and_duplicate_services_are_rejected() {
    let a = full_manifest();
    let mut b = full_manifest();
    b.version = semver::Version::new(9, 9, 9);
    assert!(matches!(
        ensure_unique_ids(&[a.clone(), b]),
        Err(ManifestError::DuplicateHarnessId(_))
    ));

    let mut dup_services = full_manifest();
    dup_services
        .requested_host_services
        .push(HostService::ModelStream);
    assert!(matches!(
        dup_services.validate(),
        Err(ManifestError::DuplicateService(_))
    ));
}

#[test]
fn entrypoint_escapes_and_bad_ids_fail_validation() {
    let escaping = HarnessManifestBuilder::new("bad.path", "1.0.0")
        .entrypoint(Platform::current(), "../outside/bin", &[])
        .build();
    assert!(matches!(
        escaping.validate(),
        Err(ManifestError::EntrypointEscapesPackage(_))
    ));

    let absolute = HarnessManifestBuilder::new("bad.path", "1.0.0")
        .entrypoint(Platform::current(), "/usr/bin/evil", &[])
        .build();
    assert!(matches!(
        absolute.validate(),
        Err(ManifestError::EntrypointEscapesPackage(_))
    ));

    let bad_id = HarnessManifestBuilder::new("bad id!", "1.0.0").build();
    assert!(matches!(
        bad_id.validate(),
        Err(ManifestError::InvalidHarnessId(_))
    ));
}

#[test]
fn schema_document_covers_all_wire_service_names() {
    let schema_text = include_str!("../schema/harness-v1.schema.json");
    let schema: serde_json::Value = serde_json::from_str(schema_text).expect("schema parses");
    let enum_values = &schema["properties"]["requestedHostServices"]["items"]["enum"];
    let listed = enum_values
        .as_array()
        .expect("enum array")
        .iter()
        .map(|v| v.as_str().expect("string").to_string())
        .collect::<Vec<_>>();
    for service in HostService::ALL {
        assert!(
            listed.contains(&service.wire_name().to_string()),
            "schema missing {}",
            service.wire_name()
        );
    }
}
